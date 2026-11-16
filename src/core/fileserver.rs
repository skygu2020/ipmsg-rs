
use std::sync::{Mutex, Arc};
use std::net::{TcpStream, TcpListener};
use std::thread;
use std::io::{Read, Write, BufWriter, Seek, SeekFrom};
use std::path::PathBuf;
use std::fs::{self, File, Metadata};
use encoding::{Encoding, DecoderTrap};
use encoding::all::GB18030;
use chrono::prelude::*;
use log::{info, debug};
use combine::parser::Parser;
use crate::models::model::ShareInfo;
use crate::{constants::protocol, util};
use crate::constants::protocol::{IPMSG_PACKET_DELIMITER, REPARENT_PATH};
use crate::util::packet_parser;

#[derive(Clone, Debug)]
pub struct FileServer {
    pub file_pool: Arc<Mutex<Vec<ShareInfo>>>
}

impl FileServer {

    pub fn new(file_pool: Arc<Mutex<Vec<ShareInfo>>>) -> FileServer {
        FileServer{
            file_pool
        }
    }

    pub fn run(&self) {
        let pool_tmp = self.file_pool.clone();
        thread::spawn(move || {
            let tcp_listener: TcpListener = TcpListener::bind(protocol::ADDR.as_str()).unwrap();
            let pool_tmp = pool_tmp.clone();
            info!("tcp server start listening! {:?}", protocol::ADDR.as_str());
            for stream in tcp_listener.incoming() {
                let base_stream = stream.unwrap().try_clone().unwrap();
                //let search_arc = search_arc_tmp.clone();
                let pool_tmp = pool_tmp.clone();
                let src = base_stream.peer_addr().unwrap();
                thread::spawn(move || {
                    let mut stream_echo = base_stream;
                    let mut request = Vec::with_capacity(256);
                    let mut byte = [0u8; 1];

                    // TCP is a byte stream: do not assume one read contains
                    // the complete NUL-terminated IPMSG request.
                    let read_result = loop {
                        if request.len() >= 8192 {
                            break Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "file request exceeds 8192 bytes",
                            ));
                        }

                        match stream_echo.read_exact(&mut byte) {
                            Ok(()) => {
                                request.push(byte[0]);
                                if byte[0] == 0 {
                                    break Ok(());
                                }
                            }
                            Err(err) => break Err(err),
                        }
                    };

                    if let Err(err) = read_result {
                        info!("failed to read complete TCP file request from {:?}: {}", src, err);
                        return;
                    }

                    let tmp_str = match GB18030.decode(&request, DecoderTrap::Strict) {
                        Ok(value) => value,
                        Err(err) => {
                            info!("invalid GB18030 TCP file request from {:?}: {:?}", src, err);
                            return;
                        }
                    };
                    info!("file_processer receive raw str {:?}", tmp_str);
                    let result = packet_parser().parse(tmp_str.as_str());
                    match result {
                        Ok((mut packet, _)) => {
                            packet.ip = src.ip().to_string();
                            let cmd = protocol::get_mode(packet.command_no);
                            if packet.additional_section.is_some() {
                                if cmd == protocol::IPMSG_GETFILEDATA {
                                        //文件请求
                                    FileServer::process_file(&pool_tmp, &mut stream_echo, packet.additional_section.unwrap())
                                }else if cmd == protocol::IPMSG_GETDIRFILES {
                                    FileServer::process_dir(pool_tmp, stream_echo, packet.additional_section.unwrap())
                                }else {
                                    info!("Invalid packet tcp file cmd {:?} !", tmp_str);
                                }
                            }else{
                                info!("Invalid packet additional_section is none {:?} !", tmp_str);
                            }
                        }
                        Err(_) => {
                            info!("Invalid packet tcp file cmd {:?} !", tmp_str);
                        }
                    }
                });
            }
        });
    }

    fn process_dir(pool_tmp: Arc<Mutex<Vec<ShareInfo>>>, stream_echo: TcpStream, ext_str: String) -> () {
        let file_attr = ext_str.splitn(3, |c| c == ':').into_iter().filter(|x: &&str| !x.is_empty()).collect::<Vec<&str>>();
        info!("file dir packet parse {:?}", file_attr);
        if file_attr.len() < 2 {
            info!("invalid directory request: expected packet and file ids");
            return;
        }

        let packet_id = match u32::from_str_radix(file_attr[0], 16) {
            Ok(value) => value,
            Err(err) => {
                info!("invalid directory request packet id {:?}: {}", file_attr[0], err);
                return;
            }
        };
        let file_id = match u32::from_str_radix(file_attr[1], 16) {
            Ok(value) => value,
            Err(err) => {
                info!("invalid directory request file id {:?}: {}", file_attr[1], err);
                return;
            }
        };
        let search_result = {
            let search = match pool_tmp.lock() {
                Ok(search) => search,
                Err(err) => {
                    info!("file share pool lock poisoned: {}", err);
                    return;
                }
            };
            search.iter().find(|s| s.packet_no == packet_id).cloned()
        };
        if let Some(result_share_file) = search_result {
            let file_info = result_share_file.file_info.iter().find(|f| f.file_id == file_id);
                if let Some(file_info) = file_info {
                    let root_path = &file_info.file_name;
                    let mut buffer = match stream_echo.try_clone() {
                        Ok(stream) => BufWriter::new(stream),
                        Err(err) => {
                            info!("failed to clone directory transfer stream: {}", err);
                            return;
                        }
                    };
                    if let Err(err) = send_dir(root_path, &mut buffer) {
                        info!("failed to send shared directory {:?}: {}", root_path, err);
                        return;
                    }
                    if let Err(err) = buffer.flush() {
                        info!("failed to flush shared directory {:?}: {}", root_path, err);
                    }
                }
            }
        }
    }

    fn process_file(pool_tmp: &Arc<Mutex<Vec<ShareInfo>>>, stream_echo: &mut TcpStream, ext_str: String) -> () {
        let file_attr = ext_str.splitn(4, |c| c == ':').into_iter().filter(|x: &&str| !x.is_empty()).collect::<Vec<&str>>();
        info!("file packet parse {:?}", file_attr);
        if file_attr.len() >= 3 {
            let packet_id = match u32::from_str_radix(file_attr[0], 16) {
                Ok(value) => value,
                Err(err) => {
                    info!("invalid file request packet id {:?}: {}", file_attr[0], err);
                    return;
                }
            };
            let file_id = match u32::from_str_radix(file_attr[1], 16) {
                Ok(value) => value,
                Err(err) => {
                    info!("invalid file request file id {:?}: {}", file_attr[1], err);
                    return;
                }
            };
            // The offset is a decimal byte count in GETFILEDATA requests.
            // Use u64 so files larger than 4 GiB can be resumed.
            let offset = match file_attr[2].parse::<u64>() {
                Ok(value) => value,
                Err(err) => {
                    info!("invalid file request offset {:?}: {}", file_attr[2], err);
                    return;
                }
            };
            let search_result = {
                let search = pool_tmp.lock().unwrap();
                search.iter().find(|s| s.packet_no == packet_id).cloned()
            };
            if let Some(result_share_file) = search_result {
                let file_info = result_share_file.file_info.iter().find(|f| f.file_id == file_id);
                if let Some(file_info) = file_info {
                    // IPMSG_GETFILEDATA carries the byte offset in the third
                    // field. Honour it so clients can resume an interrupted transfer.
                    let mut f = match File::open(&file_info.file_name) {
                        Ok(file) => file,
                        Err(err) => {
                            info!("failed to open shared file {:?}: {}", file_info.file_name, err);
                            return;
                        }
                    };
                    if let Err(err) = f.seek(SeekFrom::Start(offset)) {
                        info!("failed to seek shared file {:?} to {}: {}", file_info.file_name, offset, err);
                        return;
                    }

                    let mut buf = [0; 16 * 1024];
                    let mut buffer = BufWriter::new(stream_echo);
                    loop {
                        match f.read(&mut buf) {
                            Ok(0) => break,
                            Ok(bytes_read) => {
                                if let Err(err) = buffer.write_all(&buf[..bytes_read]) {
                                    info!("failed to send shared file {:?}: {}", file_info.file_name, err);
                                    return;
                                }
                            }
                            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(err) => {
                                info!("failed to read shared file {:?}: {}", file_info.file_name, err);
                                return;
                            }
                        }
                    }
                    if let Err(err) = buffer.flush() {
                        info!("failed to flush shared file {:?}: {}", file_info.file_name, err);
                    }
                }
            }
        }
    }
}

//send dir
pub fn send_dir(root_path: &PathBuf, buffer: &mut BufWriter<TcpStream>) -> std::io::Result<()> {
    let root_metadata = fs::symlink_metadata(root_path)?;
    if !root_metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("shared directory is not a real directory: {:?}", root_path),
        ));
    }

    let root_header = make_header(root_path, false)?;
    buffer.write_all(util::utf8_to_gb18030(&root_header).as_slice())?;
    debug!("{:?}", root_header);

    for entry in fs::read_dir(root_path)? {
        let entry = entry?;
        let sub = entry.path();
        let file_type = entry.file_type()?;

        // Do not follow symbolic links: they may point outside the shared tree.
        if file_type.is_symlink() {
            info!("skip symbolic link in shared directory: {:?}", sub);
            continue;
        }

        if file_type.is_file() {
            let header = make_header(&sub, false)?;
            buffer.write_all(util::utf8_to_gb18030(&header).as_slice())?;
            info!("{:?}", header);

            let mut file = File::open(&sub)?;
            let mut buf = [0; 16 * 1024];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(bytes_read) => buffer.write_all(&buf[..bytes_read])?,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => return Err(err),
                }
            }
        } else if file_type.is_dir() {
            send_dir(&sub, buffer)?;
        } else {
            info!("skip unsupported filesystem entry in shared directory: {:?}", sub);
        }
    }

    let ret_parent = make_header(root_path, true)?;
    buffer.write_all(util::utf8_to_gb18030(&ret_parent).as_slice())?;
    debug!("{ret_parent:?}");
    Ok(())
}

///
/// 转换报文
pub fn make_header(path: &PathBuf, ret_parent: bool) -> std::io::Result<String> {
    let mut header = String::new();
    header.push(IPMSG_PACKET_DELIMITER);

    let (file_size, file_attr) = if ret_parent {
        header.push(REPARENT_PATH);
        (0, protocol::IPMSG_FILE_RETPARENT)
    } else {
        let path_metadata: Metadata = fs::symlink_metadata(path)?;
        let file_name = path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("shared filename is not valid UTF-8: {:?}", path),
            ))?;
        header.push_str(file_name);
        let file_attr = if path_metadata.file_type().is_dir() {
            protocol::IPMSG_FILE_DIR
        } else if path_metadata.file_type().is_file() {
            protocol::IPMSG_FILE_REGULAR
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported shared filesystem entry: {:?}", path),
            ));
        };
        (path_metadata.len(), file_attr)
    };

    header.push(IPMSG_PACKET_DELIMITER);
    header.push_str(format!("{:x}", file_size).as_str());
    header.push(IPMSG_PACKET_DELIMITER);
    header.push_str(format!("{:x}", file_attr).as_str());
    let timestamp_now = Local::now().timestamp();
    header.push_str(format!(":{:x}={:x}:{:x}={:x}:", protocol::IPMSG_FILE_CREATETIME, timestamp_now, protocol::IPMSG_FILE_MTIME, timestamp_now).as_str());
    let mut length = util::utf8_to_gb18030(&header).len();
    length += format!("{:0>4x}", length).len();
    header.insert_str(0, format!("{:0>4x}", length).as_str());
    Ok(header)
}