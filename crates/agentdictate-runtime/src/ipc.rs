use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use agentdictate_core::{ClientCommand, PROTOCOL_VERSION, ServerMessage};
use thiserror::Error;

const SOCKET_FILE_NAME: &str = "agentdictate.sock";
const LOCK_FILE_NAME: &str = "agentdictate.lock";
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IPC I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("IPC message is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("IPC peer uses protocol {received}, but this process requires {expected}")]
    ProtocolVersion { received: u16, expected: u16 },
    #[error("IPC peer disconnected before sending a complete message")]
    Disconnected,
    #[error("AgentDictate is already listening at {path}")]
    AlreadyRunning { path: PathBuf },
}

pub trait IpcHandler {
    fn snapshot(&self, request_id: u64) -> ServerMessage;
    fn handle(&mut self, command: ClientCommand) -> ServerMessage;
}

pub struct IpcServer {
    listener: UnixListener,
    _singleton_lock: File,
    socket_path: PathBuf,
    socket_device: u64,
    socket_inode: u64,
}

impl IpcServer {
    pub fn bind(runtime_directory: impl AsRef<Path>) -> Result<Self, IpcError> {
        let runtime_directory = runtime_directory.as_ref();
        fs::create_dir_all(runtime_directory)?;
        fs::set_permissions(runtime_directory, fs::Permissions::from_mode(0o700))?;
        let socket_path = runtime_directory.join(SOCKET_FILE_NAME);
        let singleton_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(runtime_directory.join(LOCK_FILE_NAME))?;
        singleton_lock.set_permissions(fs::Permissions::from_mode(0o600))?;
        if let Err(error) = singleton_lock.try_lock() {
            return match error {
                fs::TryLockError::WouldBlock => Err(IpcError::AlreadyRunning { path: socket_path }),
                fs::TryLockError::Error(error) => Err(error.into()),
            };
        }
        match fs::symlink_metadata(&socket_path) {
            Ok(metadata) if !metadata.file_type().is_socket() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "refusing to replace non-socket path {}",
                        socket_path.display()
                    ),
                )
                .into());
            }
            Ok(_) => match UnixStream::connect(&socket_path) {
                Ok(_) => {
                    return Err(IpcError::AlreadyRunning { path: socket_path });
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::NotFound
                    ) =>
                {
                    match fs::remove_file(&socket_path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        let socket_metadata = fs::symlink_metadata(&socket_path)?;
        Ok(Self {
            listener,
            _singleton_lock: singleton_lock,
            socket_path,
            socket_device: socket_metadata.dev(),
            socket_inode: socket_metadata.ino(),
        })
    }

    pub fn socket_mode(&self) -> Result<u32, IpcError> {
        Ok(fs::metadata(&self.socket_path)?.permissions().mode() & 0o777)
    }

    /// Serves one connected UI session. A current snapshot is sent before
    /// waiting for commands, so reconnects never depend on replayed events.
    pub fn serve_next(&self, handler: &mut impl IpcHandler) -> Result<(), IpcError> {
        let (mut stream, _) = self.listener.accept()?;
        write_message(&mut stream, &handler.snapshot(0))?;

        let reader_stream = stream.try_clone()?;
        let mut reader = BufReader::new(reader_stream);
        while let Some(command) = read_message::<ClientCommand>(&mut reader)? {
            check_version(command.protocol_version)?;
            let response = handler.handle(command);
            check_version(response.protocol_version)?;
            write_message(&mut stream, &response)?;
        }
        Ok(())
    }

    /// Accepts one session and serves it on its own thread. Only individual
    /// commands hold the handler lock, so a connected but silent UI cannot
    /// block hotkeys, recorder-exit notifications, or other clients.
    pub fn serve_next_concurrent<H>(
        &self,
        handler: Arc<Mutex<H>>,
    ) -> Result<JoinHandle<Result<(), IpcError>>, IpcError>
    where
        H: IpcHandler + Send + 'static,
    {
        let (stream, _) = self.listener.accept()?;
        Ok(std::thread::spawn(move || serve_shared(stream, &handler)))
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let owns_path = fs::symlink_metadata(&self.socket_path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.socket_device
                && metadata.ino() == self.socket_inode
        });
        if owns_path {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

pub struct IpcClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl IpcClient {
    pub fn connect(runtime_directory: impl AsRef<Path>) -> Result<(Self, ServerMessage), IpcError> {
        let socket_path = runtime_directory.as_ref().join(SOCKET_FILE_NAME);
        let stream = UnixStream::connect(socket_path)?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut client = Self { stream, reader };
        let initial = client.read_server_message()?;
        Ok((client, initial))
    }

    pub fn send(&mut self, command: ClientCommand) -> Result<ServerMessage, IpcError> {
        check_version(command.protocol_version)?;
        write_message(&mut self.stream, &command)?;
        self.read_server_message()
    }

    /// Wakes a server blocked in `accept` without creating a live session.
    pub fn wake(runtime_directory: impl AsRef<Path>) -> Result<(), IpcError> {
        let socket_path = runtime_directory.as_ref().join(SOCKET_FILE_NAME);
        drop(UnixStream::connect(socket_path)?);
        Ok(())
    }

    fn read_server_message(&mut self) -> Result<ServerMessage, IpcError> {
        let message: ServerMessage =
            read_message(&mut self.reader)?.ok_or(IpcError::Disconnected)?;
        check_version(message.protocol_version)?;
        Ok(message)
    }
}

fn write_message(writer: &mut impl Write, message: &impl serde::Serialize) -> Result<(), IpcError> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> Result<Option<T>, IpcError> {
    let mut line = String::new();
    let read = reader
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > MAX_FRAME_BYTES || !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC frame exceeds the 1 MiB limit or is incomplete",
        )
        .into());
    }
    Ok(Some(serde_json::from_str(&line)?))
}

fn serve_shared<H>(mut stream: UnixStream, handler: &Arc<Mutex<H>>) -> Result<(), IpcError>
where
    H: IpcHandler,
{
    let initial = handler
        .lock()
        .map_err(|_| std::io::Error::other("IPC handler lock is poisoned"))?
        .snapshot(0);
    write_message(&mut stream, &initial)?;
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    while let Some(command) = read_message::<ClientCommand>(&mut reader)? {
        check_version(command.protocol_version)?;
        let response = handler
            .lock()
            .map_err(|_| std::io::Error::other("IPC handler lock is poisoned"))?
            .handle(command);
        check_version(response.protocol_version)?;
        write_message(&mut stream, &response)?;
    }
    Ok(())
}

fn check_version(received: u16) -> Result<(), IpcError> {
    if received == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(IpcError::ProtocolVersion {
            received,
            expected: PROTOCOL_VERSION,
        })
    }
}
