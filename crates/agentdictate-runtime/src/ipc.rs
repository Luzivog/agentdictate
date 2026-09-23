use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use agentdictate_core::{ClientCommand, PROTOCOL_VERSION, ServerMessage};
use thiserror::Error;

const SOCKET_FILE_NAME: &str = "agentdictate.sock";
const LOCK_FILE_NAME: &str = "agentdictate.lock";
const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// A session that sends nothing for this long is closed, so a client that
/// connects and goes silent cannot keep its thread forever.
const SESSION_READ_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Answers IPC sessions. Sessions can run concurrently, so a handler does
/// its own locking and holds a lock only while a command needs it.
pub trait IpcHandler {
    fn snapshot(&self, request_id: u64) -> ServerMessage;
    fn handle(&self, command: ClientCommand) -> ServerMessage;
}

pub struct IpcServer {
    listener: UnixListener,
    _singleton_lock: File,
    socket_path: PathBuf,
    socket_device: u64,
    socket_inode: u64,
    session_timeout: Duration,
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
            session_timeout: SESSION_READ_TIMEOUT,
        })
    }

    /// Replaces the idle-session timeout, so tests need not wait a minute.
    #[doc(hidden)]
    #[must_use]
    pub const fn with_session_timeout(mut self, timeout: Duration) -> Self {
        self.session_timeout = timeout;
        self
    }

    pub fn socket_mode(&self) -> Result<u32, IpcError> {
        Ok(fs::metadata(&self.socket_path)?.permissions().mode() & 0o777)
    }

    /// Serves the next connected UI session on this thread.
    pub fn serve_next(&self, handler: &impl IpcHandler) -> Result<(), IpcError> {
        let (stream, _) = self.listener.accept()?;
        serve_session(stream, handler, self.session_timeout)
    }

    /// Accepts one session and serves it on its own thread, so a connected
    /// but silent UI cannot block other clients.
    pub fn serve_next_concurrent<H>(
        &self,
        handler: H,
    ) -> Result<JoinHandle<Result<(), IpcError>>, IpcError>
    where
        H: IpcHandler + Send + 'static,
    {
        let (stream, _) = self.listener.accept()?;
        let timeout = self.session_timeout;
        Ok(std::thread::Builder::new()
            .name("agentdictate-ipc-session".into())
            .spawn(move || serve_session(stream, &handler, timeout))?)
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

/// Serves one UI session. A current snapshot is sent before waiting for
/// commands, so reconnects never depend on replayed events. The session ends
/// when the client disconnects or stays silent for `idle_timeout`.
fn serve_session(
    mut stream: UnixStream,
    handler: &impl IpcHandler,
    idle_timeout: Duration,
) -> Result<(), IpcError> {
    stream.set_read_timeout(Some(idle_timeout))?;
    write_message(&mut stream, &handler.snapshot(0))?;
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    loop {
        let command = match read_message::<ClientCommand>(&mut reader) {
            Ok(Some(command)) => command,
            Ok(None) => return Ok(()),
            Err(IpcError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        check_version(command.protocol_version)?;
        let response = handler.handle(command);
        check_version(response.protocol_version)?;
        write_message(&mut stream, &response)?;
    }
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
