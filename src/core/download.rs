use std::io::prelude::*;
use std::net::TcpStream;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::fs::{self, File};
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use encoding::{Encoding, DecoderTrap};
use encoding::all::GB18030;
use log::{info, debug};
use anyhow::{Result, anyhow};
use crate::constants::protocol::{self, IPMSG_SENDMSG, IPMSG_GETFILEDATA, IPMSG_GETDIRFILES, IPMSG_FILE_DIR, IPMSG_FILE_REGULAR, IPMSG_FILE_RETPARENT, IPMSG_PACKET_DELIMITER};
use crate::core::GLOBLE_SENDER;
use crate::models::event::ModelEvent;
use crate::models::model::{Packet, ReceivedSimpleFileInfo};

#[derive(Clone, Debug)]
pub struct ManagerPool {
    pub file_pool: Arc<Mutex<HashMap<u32, PoolFile>>>,
}

impl ManagerPool {

    pub fn new(file_pool: Arc<Mutex<HashMap< u32, PoolFile>>>) -> ManagerPool {
        ManagerPool { file_pool}
    }

    pub fn run(self, file_info: ReceivedSimpleFileInfo, save_path: PathBuf, download_ip: String) {
        let tmp = self.clone();
        {
            let mut lock = tmp.file_pool.lock().unwrap();
            let file = lock.get(&file_info.file_id);
            if let Some(p_file) = file {
                if p_file.status == 1 {
                    //下载中
                    GLOBLE_SENDER.send(ModelEvent::DownloadIsBusy{ file: file_info }).expect("send DownloadIsBusy fail!");
                    return;
                }
            }else {
                lock.insert(file_info.file_id, PoolFile { status: 1, file_info: file_info.clone() });
            }
        }
        let tmp = self.clone();
        thread::spawn(move || {
            let download_url = format!("{}:{}", download_ip, protocol::IPMSG_DEFAULT_PORT);
            let is_ok = download(download_url, save_path, file_info.clone()).is_ok();
            {
                let mut lock = tmp.file_pool.lock().unwrap();
                let mut file = lock.get(&file_info.file_id);
                if let Some(p_file) = file.take() {
                    if is_ok {
                        lock.remove(&file_info.file_id);
                        GLOBLE_SENDER.send(ModelEvent::RemoveDownloadTaskInPool{ packet_id: file_info.packet_id, file_id: file_info.file_id, download_ip }).expect("send RemoveDownloadTaskInPool fail!");
                    }else{
                        let mut tmp_file = p_file.clone();
                        tmp_file.status = 0;
                        lock.insert(tmp_file.file_info.file_id, tmp_file);
                    }

                }
            }

        });
    }

}

#[derive(Clone, Debug)]
pub struct PoolFile {
    pub status: u8, //0 初始 1 下载中
    pub file_info: ReceivedSimpleFileInfo,
}

pub fn download<A: ToSocketAddrs, S: AsRef<Path>>(addr: A, to_path: S, r_file: ReceivedSimpleFileInfo) -> Result<()> {
    info!("start download file");
    let file_type = r_file.attr as u32;
    let mut stream = TcpStream::connect(addr)?;
    let packet = Packet::new(IPMSG_SENDMSG| if file_type == IPMSG_FILE_DIR { IPMSG_GETDIRFILES } else { IPMSG_GETFILEDATA }, Some(format!("{:x}:{:x}:0:\u{0}", r_file.packet_id, r_file.file_id)));
    stream.write(packet.to_string().as_bytes())?;
    debug!("filetype {}", file_type);
    if file_type == IPMSG_FILE_REGULAR {
        let mut file_location = to_path.as_ref().to_path_buf();
        file_location.push(r_file.name);
        let file_size = r_file.size;
        let mut buffer = BufReader::new(stream);
        read_bytes_to_file(&mut buffer, file_size, &file_location)?;
    }else if file_type == IPMSG_FILE_DIR {
        let mut next_path = to_path.as_ref().to_path_buf();
        let mut directory_depth = 0usize;
        let mut buffer = BufReader::new(stream);
        loop {
            let header_size_str = match read_delimiter(&mut buffer)? {
                Some(value) => value,
                None => break,
            };
            let header_size = u64::from_str_radix(&header_size_str, 16)?;
            info!("header_size {:?}", header_size);

            // The size includes the four-byte length prefix and its delimiter.
            let prefix_len = u64::try_from(header_size_str.len())?;
            let context_len = header_size
                .checked_sub(prefix_len + 1)
                .ok_or_else(|| anyhow!("invalid directory header length: {}", header_size))?;
            let header_context_str = read_bytes(&mut buffer, context_len)?;
            let v: Vec<&str> = header_context_str.splitn(4, |c| c == ':').collect();
            if v.len() < 3 {
                return Err(anyhow!("invalid directory file header: {:?}", header_context_str));
            }
            let file_name = v[0];
            let file_size = u64::from_str_radix(v[1], 16)?;
            let file_attr = u32::from_str_radix(v[2], 16)?;
            let cmd = protocol::get_mode(file_attr);
            info!("header context {:?}", v);
            if cmd == IPMSG_FILE_DIR {
                validate_transfer_name(file_name)?;
                next_path.push(file_name);
                match fs::symlink_metadata(&next_path) {
                    Ok(metadata) if metadata.file_type().is_dir() => {}
                    Ok(_) => return Err(anyhow!("directory destination is not a real directory: {:?}", next_path)),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&next_path)?,
                    Err(err) => return Err(err.into()),
                }
                directory_depth += 1;
                info!("created directory {:?}", next_path);
            }else if cmd == IPMSG_FILE_REGULAR {
                validate_transfer_name(file_name)?;
                next_path.push(file_name);
                if let Ok(metadata) = fs::symlink_metadata(&next_path) {
                    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                        return Err(anyhow!("file destination is not a regular file: {:?}", next_path));
                    }
                }
                info!("receiving file {:?}", next_path);
                let file_result = read_bytes_to_file(&mut buffer, file_size, &next_path);
                next_path.pop();
                file_result?;
            }else if cmd == IPMSG_FILE_RETPARENT {
                if directory_depth == 0 {
                    return Err(anyhow!("directory transfer attempted to leave its destination root"));
                }
                next_path.pop();
                directory_depth -= 1;
                info!("back to parent {:?}", next_path);
            }else {
                return Err(anyhow!("unsupported directory entry type: {:#x}", file_attr));
            }
        }
    }
    info!("download end!");
    Ok(())
}

fn validate_transfer_name(name: &str) -> Result<()> {
    // Filenames are a single IPMSG header field and must never become paths.
    // Reject both platform separators so a packet cannot escape the chosen
    // download directory on either Unix or Windows.
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\\\', ':', '\\0'])
        || Path::new(name).components().any(|component| {
            !matches!(component, Component::Normal(_))
        })
    {
        return Err(anyhow!("unsafe filename in directory transfer: {:?}", name));
    }
    Ok(())
}

fn read_delimiter(stream: &mut BufReader<TcpStream>) -> Result<Option<String>> {
    let mut buffer = Vec::new();
    let len = stream.read_until(u8::try_from(IPMSG_PACKET_DELIMITER)?, &mut buffer)?;
    if len == 0 {
        return Ok(None);
    }
    if len > 200 {
        return Err(anyhow!("directory header length prefix exceeds 200 bytes"));
    }
    if buffer.last().copied() != Some(u8::try_from(IPMSG_PACKET_DELIMITER)?) {
        return Err(anyhow!("connection closed in the middle of a directory header"));
    }

    buffer.pop();
    Ok(Some(String::from_utf8(buffer)?))
}

fn read_bytes(stream: &mut BufReader<TcpStream>, len: u64) -> Result<String> {
    let len = usize::try_from(len)?;
    let mut buffer = vec![0; len];
    stream.read_exact(&mut buffer)?;
    Ok(GB18030.decode(&buffer, DecoderTrap::Strict)
        .map_err(|err| anyhow!("invalid GB18030 directory header: {:?}", err))?)
}

fn read_bytes_to_file(stream: &mut BufReader<TcpStream>, len: u64, file_path: &PathBuf) -> Result<()> {
    let mut file = File::create(file_path)?;
    info!("file len {:?}", len);

    // A TCP read may return fewer bytes than requested. Copy exactly the
    // advertised payload length and report an early EOF as a failed download.
    let mut limited_stream = stream.take(len);
    let copied = std::io::copy(&mut limited_stream, &mut file)?;
    if copied != len {
        return Err(anyhow!(
            "incomplete file transfer for {:?}: expected {} bytes, received {}",
            file_path,
            len,
            copied
        ));
    }

    file.flush()?;
    Ok(())
}

///
/// unkown filesize can use follow
/// 可以不需要文件长度的读
///
fn read_bytes_to_file_unsize(stream : &mut BufReader<TcpStream>, file_path: &PathBuf) -> Result<()> {
    let mut file: File = File::create(file_path).unwrap();
    loop {
        let mut buffer = [0; 2048];
        let num = stream.read(&mut buffer[..])?;
        if num == 0 {
            break;
        }
        file.write(&buffer[0..num])?;
    }
    Ok(())
}