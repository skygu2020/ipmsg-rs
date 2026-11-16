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
    pub file_pool: Arc<Mutex<HashMap<(String, u32, u32), PoolFile>>>,
}

impl ManagerPool {

    pub fn new(file_pool: Arc<Mutex<HashMap<(String, u32, u32), PoolFile>>>) -> ManagerPool {
        ManagerPool { file_pool}
    }

    pub fn run(self, file_info: ReceivedSimpleFileInfo, save_path: PathBuf, download_ip: String) {
        let task_key = (download_ip.clone(), file_info.packet_id, file_info.file_id);
        let tmp = self.clone();
        {
            let mut lock = match tmp.file_pool.lock() {
                Ok(lock) => lock,
                Err(err) => {
                    log::error!("download task pool lock poisoned: {}", err);
                    if let Err(send_err) = GLOBLE_SENDER.send(ModelEvent::DownloadIsBusy {
                        file: file_info,
                    }) {
                        log::error!("failed to report unavailable download task: {}", send_err);
                    }
                    return;
                }
            };
            if let Some(p_file) = lock.get(&task_key) {
                if p_file.status == 1 {
                    // Download already in progress.
                    if let Err(err) = GLOBLE_SENDER.send(ModelEvent::DownloadIsBusy { file: file_info }) {
                        log::error!("failed to report busy download: {}", err);
                    }
                    return;
                }
                // A previous attempt failed; mark the existing task active again.
                lock.insert(task_key.clone(), PoolFile {
                    status: 1,
                    file_info: file_info.clone(),
                });
            } else {
                lock.insert(task_key.clone(), PoolFile {
                    status: 1,
                    file_info: file_info.clone(),
                });
            }
        }
        let initial_total = if file_info.attr as u32 == IPMSG_FILE_REGULAR {
            file_info.size
        } else {
            0
        };
        if let Err(err) = GLOBLE_SENDER.send(ModelEvent::DownloadProgress {
            packet_id: file_info.packet_id,
            file_id: file_info.file_id,
            download_ip: download_ip.clone(),
            transferred: 0,
            total: initial_total,
        }) {
            log::warn!("failed to initialize download progress: {}", err);
        }

        let tmp = self.clone();
        thread::spawn(move || {
            let download_url = format!("{}:{}", download_ip, protocol::IPMSG_DEFAULT_PORT);
            let progress_file = file_info.clone();
            let progress_ip = download_ip.clone();
            let is_ok = download_with_progress(download_url, save_path, file_info.clone(), move |transferred, total| {
                if let Err(err) = GLOBLE_SENDER.send(ModelEvent::DownloadProgress {
                    packet_id: progress_file.packet_id,
                    file_id: progress_file.file_id,
                    download_ip: progress_ip.clone(),
                    transferred,
                    total,
                }) {
                    log::warn!("failed to report download progress: {}", err);
                }
            }).is_ok();
            {
                let mut lock = match tmp.file_pool.lock() {
                    Ok(lock) => lock,
                    Err(err) => {
                        log::error!("download task pool lock poisoned while finishing transfer: {}", err);
                        return;
                    }
                };
                let file = lock.get(&task_key).cloned();
                if let Some(p_file) = file {
                    if is_ok {
                        lock.remove(&task_key);
                        if let Err(err) = GLOBLE_SENDER.send(ModelEvent::RemoveDownloadTaskInPool {
                            packet_id: file_info.packet_id,
                            file_id: file_info.file_id,
                            download_ip,
                        }) {
                            log::error!("failed to report completed download: {}", err);
                        }
                    }else{
                        let mut tmp_file = p_file.clone();
                        tmp_file.status = 0;
                        lock.insert(task_key.clone(), tmp_file);
                        if let Err(err) = GLOBLE_SENDER.send(ModelEvent::DownloadFailed {
                            packet_id: file_info.packet_id,
                            file_id: file_info.file_id,
                            download_ip,
                        }) {
                            log::warn!("failed to report download failure: {}", err);
                        }
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
    download_with_progress(addr, to_path, r_file, |_, _| {})
}

fn download_with_progress<A, S, F>(
    addr: A,
    to_path: S,
    r_file: ReceivedSimpleFileInfo,
    mut progress: F,
) -> Result<()>
where
    A: ToSocketAddrs,
    S: AsRef<Path>,
    F: FnMut(u64, u64),
{
    info!("start download file");
    let file_type = r_file.attr as u32;
    if file_type == IPMSG_FILE_REGULAR {
        // The offered filename comes from the remote peer just like directory
        // entry names, so validate it before joining it to the chosen folder.
        validate_transfer_name(&r_file.name)?;
    } else if file_type != IPMSG_FILE_DIR {
        return Err(anyhow!("unsupported download file type: {:#x}", file_type));
    }
    let mut stream = connect_with_timeout(addr, std::time::Duration::from_secs(15))?;
    // Do not let a stalled peer block the download worker indefinitely.
    // The longer read timeout accommodates slower directory and file transfers.
    stream.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(30)))?;
    let packet = Packet::new(IPMSG_SENDMSG| if file_type == IPMSG_FILE_DIR { IPMSG_GETDIRFILES } else { IPMSG_GETFILEDATA }, Some(format!("{:x}:{:x}:0:\u{0}", r_file.packet_id, r_file.file_id)));
    // TCP writes may be partial; ensure the complete request reaches the peer.
    stream.write_all(packet.to_string().as_bytes())?;
    debug!("filetype {}", file_type);
    if file_type == IPMSG_FILE_REGULAR {
        let mut file_location = to_path.as_ref().to_path_buf();
        file_location.push(r_file.name);
        let file_size = r_file.size;
        let mut buffer = BufReader::new(stream);
        read_bytes_to_file(&mut buffer, file_size, &file_location, &mut progress)?;
    }else if file_type == IPMSG_FILE_DIR {
        // The user-selected destination root is trusted, but every directory
        // created below it must remain a real directory, not a symlink.
        let destination_root = to_path.as_ref().to_path_buf();
        let mut next_path = destination_root.clone();
        let mut directory_depth = 0usize;
        let mut directory_transferred = 0u64;
        let mut root_completed = false;
        let mut buffer = BufReader::new(stream);
        loop {
            let header_size_str = match read_delimiter(&mut buffer)? {
                Some(value) => value,
                None if root_completed => break,
                None => return Err(anyhow!("directory transfer ended before its root directory terminator")),
            };
            let header_size = u64::from_str_radix(&header_size_str, 16)?;
            info!("header_size {:?}", header_size);

            // The size includes the four-byte length prefix and its delimiter.
            let prefix_len = u64::try_from(header_size_str.len())?;
            let context_len = header_size
                .checked_sub(prefix_len + 1)
                .ok_or_else(|| anyhow!("invalid directory header length: {}", header_size))?;
            if context_len > MAX_DIRECTORY_HEADER_CONTEXT {
                return Err(anyhow!(
                    "directory header context exceeds {} bytes: {}",
                    MAX_DIRECTORY_HEADER_CONTEXT,
                    context_len
                ));
            }
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
                validate_transfer_directories(&destination_root, &next_path)?;
                next_path.push(file_name);
                match fs::symlink_metadata(&next_path) {
                    Ok(metadata) if metadata.file_type().is_dir() => {}
                    Ok(_) => return Err(anyhow!("directory destination is not a real directory: {:?}", next_path)),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        fs::create_dir(&next_path)?;
                        let metadata = fs::symlink_metadata(&next_path)?;
                        if !metadata.file_type().is_dir() {
                            return Err(anyhow!("created directory was replaced: {:?}", next_path));
                        }
                    }
                    Err(err) => return Err(err.into()),
                }
                directory_depth += 1;
                info!("created directory {:?}", next_path);
            }else if cmd == IPMSG_FILE_REGULAR {
                validate_transfer_name(file_name)?;
                validate_transfer_directories(&destination_root, &next_path)?;
                next_path.push(file_name);
                if let Ok(metadata) = fs::symlink_metadata(&next_path) {
                    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                        return Err(anyhow!("file destination is not a regular file: {:?}", next_path));
                    }
                }
                info!("receiving file {:?}", next_path);
                let file_result = read_bytes_to_file(
                    &mut buffer,
                    file_size,
                    &next_path,
                    &mut |transferred, _| progress(directory_transferred.saturating_add(transferred), 0),
                );
                next_path.pop();
                file_result?;
                directory_transferred = directory_transferred.saturating_add(file_size);
            }else if cmd == IPMSG_FILE_RETPARENT {
                if directory_depth == 0 {
                    return Err(anyhow!("directory transfer attempted to leave its destination root"));
                }
                next_path.pop();
                directory_depth -= 1;
                if directory_depth == 0 {
                    root_completed = true;
                }
                info!("back to parent {:?}", next_path);
            }else {
                return Err(anyhow!("unsupported directory entry type: {:#x}", file_attr));
            }
        }
    }
    info!("download end!");
    Ok(())
}

// Resolve all candidate addresses and bound each TCP connection attempt.
fn connect_with_timeout<A: ToSocketAddrs>(
    addr: A,
    timeout: std::time::Duration,
) -> Result<TcpStream> {
    let mut last_error = None;
    for address in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
    }

    match last_error {
        Some(err) => Err(err.into()),
        None => Err(anyhow!("download address resolved to no socket addresses")),
    }
}

// Check every directory below the user-selected root before writing into it.
// The root itself may intentionally be a symlink chosen by the user; descendants
// created or traversed from the transfer stream must not be symlinks.
fn validate_transfer_directories(root: &Path, current: &Path) -> Result<()> {
    let relative = current.strip_prefix(root)
        .map_err(|_| anyhow!("directory transfer path escaped destination root"))?;
    let mut checked = root.to_path_buf();

    for component in relative.components() {
        if let Component::Normal(name) = component {
            checked.push(name);
            let metadata = fs::symlink_metadata(&checked)?;
            if !metadata.file_type().is_dir() {
                return Err(anyhow!(
                    "directory transfer path contains a non-directory or symlink: {:?}",
                    checked
                ));
            }
        } else {
            return Err(anyhow!("invalid directory component in transfer path: {:?}", component));
        }
    }

    Ok(())
}

fn validate_transfer_name(name: &str) -> Result<()> {
    // Filenames are a single IPMSG header field and must never become paths.
    // Reject both platform separators so a packet cannot escape the chosen
    // download directory on either Unix or Windows.
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':', '\0'])
        || Path::new(name).components().any(|component| {
            !matches!(component, Component::Normal(_))
        })
    {
        return Err(anyhow!("unsafe filename in directory transfer: {:?}", name));
    }
    Ok(())
}

const MAX_DIRECTORY_HEADER_PREFIX: usize = 200;
const MAX_DIRECTORY_HEADER_CONTEXT: u64 = 64 * 1024;

fn read_delimiter(stream: &mut BufReader<TcpStream>) -> Result<Option<String>> {
    let delimiter = u8::try_from(IPMSG_PACKET_DELIMITER)?;
    let mut buffer = Vec::with_capacity(8);
    let mut byte = [0u8; 1];

    loop {
        match stream.read(&mut byte)? {
            0 if buffer.is_empty() => return Ok(None),
            0 => return Err(anyhow!("connection closed in the middle of a directory header length")),
            _ if byte[0] == delimiter => break,
            _ => {
                if buffer.len() >= MAX_DIRECTORY_HEADER_PREFIX {
                    return Err(anyhow!(
                        "directory header length prefix exceeds {} bytes",
                        MAX_DIRECTORY_HEADER_PREFIX
                    ));
                }
                buffer.push(byte[0]);
            }
        }
    }

    Ok(Some(String::from_utf8(buffer)?))
}

fn read_bytes(stream: &mut BufReader<TcpStream>, len: u64) -> Result<String> {
    let len = usize::try_from(len)?;
    let mut buffer = vec![0; len];
    stream.read_exact(&mut buffer)?;
    Ok(GB18030.decode(&buffer, DecoderTrap::Strict)
        .map_err(|err| anyhow!("invalid GB18030 directory header: {:?}", err))?)
}

// Replace an existing regular file without ever opening a symlink target.
// create_new is atomic: if another process creates any entry after removal,
// the operation fails rather than following or truncating that entry.
fn create_transfer_file(file_path: &Path) -> Result<File> {
    match fs::symlink_metadata(file_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(anyhow!(
                    "download destination is not a regular file: {:?}",
                    file_path
                ));
            }
            fs::remove_file(file_path)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file_path)?;
    Ok(file)
}

fn read_bytes_to_file<F>(
    stream: &mut BufReader<TcpStream>,
    len: u64,
    file_path: &PathBuf,
    progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let mut file = create_transfer_file(file_path)?;
    info!("file len {:?}", len);

    let mut transferred = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    while transferred < len {
        let remaining = len - transferred;
        let limit = std::cmp::min(remaining, buffer.len() as u64) as usize;
        let count = stream.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(anyhow!(
                "incomplete file transfer for {:?}: expected {} bytes, received {}",
                file_path,
                len,
                transferred
            ));
        }
        file.write_all(&buffer[..count])?;
        transferred += count as u64;
        progress(transferred, len);
    }

    file.flush()?;
    Ok(())
}

///
/// unkown filesize can use follow
/// 可以不需要文件长度的读
///
fn read_bytes_to_file_unsize(stream: &mut BufReader<TcpStream>, file_path: &PathBuf) -> Result<()> {
    let mut file = create_transfer_file(file_path)?;

    loop {
        let mut buffer = [0; 2048];
        let num = stream.read(&mut buffer)?;
        if num == 0 {
            break;
        }
        file.write_all(&buffer[..num])?;
    }

    file.flush()?;
    Ok(())
}