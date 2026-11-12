//! Session-local control channel between the running app and its controllers
//! (CLI and Stream Deck). Each connection carries one newline-terminated
//! command frame and one response frame. Windows uses a per-session named
//! pipe; macOS uses a Unix socket in the per-user data directory.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use thiserror::Error;

use crate::config::ConfigError;
use crate::ipc::{
    ControlCommand, ControlMessage, FrameError, MAX_FRAME_BYTES, decode_command, decode_message,
    encode_frame,
};

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("Audio Switch is not running: {0}")]
    Unavailable(#[source] io::Error),
    #[error("another Audio Switch instance already owns the control channel")]
    AlreadyRunning,
    #[error("local control I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("local control connection closed before a complete frame")]
    Closed,
}

type Handler = Arc<dyn Fn(ControlCommand) -> ControlMessage + Send + Sync>;

/// Starts the control server on a background thread and returns once it is
/// accepting connections.
pub fn start_server(
    handler: impl Fn(ControlCommand) -> ControlMessage + Send + Sync + 'static,
) -> Result<(), ControlError> {
    let handler: Handler = Arc::new(handler);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("audio-switch-control".into())
        .spawn(move || match transport::Listener::bind() {
            Ok(listener) => {
                let _ = ready_tx.send(Ok(()));
                listener.serve(|connection| serve_client(connection, &handler));
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error));
            }
        })?;
    ready_rx.recv().map_err(|_| ControlError::Closed)?
}

pub fn send_command(command: &ControlCommand) -> Result<ControlMessage, ControlError> {
    let mut connection = transport::connect()?;
    connection.write_all(&encode_frame(command)?)?;
    connection.flush()?;
    Ok(decode_message(&read_frame(&mut connection)?)?)
}

fn serve_client(mut connection: impl Read + Write, handler: &Handler) {
    let result = read_frame(&mut connection).and_then(|frame| {
        let response = match decode_command(&frame) {
            Ok(command) => handler(command),
            Err(error) => ControlMessage::Error {
                request_id: None,
                message: error.to_string(),
            },
        };
        connection.write_all(&encode_frame(&response)?)?;
        Ok(connection.flush()?)
    });
    if let Err(error) = result {
        eprintln!("Audio Switch control request failed: {error}");
    }
}

fn read_frame(connection: &mut impl Read) -> Result<Vec<u8>, ControlError> {
    let mut frame = Vec::with_capacity(512);
    let mut buffer = [0_u8; 512];
    loop {
        let read = connection.read(&mut buffer)?;
        if read == 0 {
            return Err(ControlError::Closed);
        }
        frame.extend_from_slice(&buffer[..read]);
        if let Some(newline) = frame.iter().position(|byte| *byte == b'\n') {
            frame.truncate(newline);
            return Ok(frame);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge.into());
        }
    }
}

#[cfg(unix)]
mod transport {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    use super::ControlError;
    use crate::config::data_directory;

    const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

    fn socket_path() -> Result<PathBuf, ControlError> {
        Ok(data_directory()?.join("control.v1.sock"))
    }

    pub fn connect() -> Result<UnixStream, ControlError> {
        let stream = UnixStream::connect(socket_path()?).map_err(ControlError::Unavailable)?;
        stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
        Ok(stream)
    }

    pub struct Listener(UnixListener);

    impl Listener {
        /// Binds the per-user socket. A socket file left by a crashed instance
        /// is replaced; a live one means another instance is running.
        pub fn bind() -> Result<Self, ControlError> {
            let path = socket_path()?;
            if UnixStream::connect(&path).is_ok() {
                return Err(ControlError::AlreadyRunning);
            }
            if let Some(directory) = path.parent() {
                std::fs::create_dir_all(directory)?;
            }
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
            let listener = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self(listener))
        }

        pub fn serve(self, mut serve_client: impl FnMut(UnixStream)) {
            for stream in self.0.incoming() {
                match stream {
                    Ok(stream) => {
                        // A stalled client must not block later requests.
                        let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
                        serve_client(stream);
                    }
                    Err(error) => eprintln!("Audio Switch control accept failed: {error}"),
                }
            }
        }
    }
}

#[cfg(windows)]
mod transport {
    use std::io::{self, Read, Write};

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, FlushFileBuffers, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        WaitNamedPipeW,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::core::PCWSTR;

    use super::{ControlError, MAX_FRAME_BYTES};

    pub struct Pipe(HANDLE);

    impl Drop for Pipe {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    impl Read for Pipe {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut read = 0_u32;
            unsafe { ReadFile(self.0, Some(buffer), Some(&mut read), None) }
                .map_err(io::Error::other)?;
            Ok(read as usize)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut written = 0_u32;
            unsafe { WriteFile(self.0, Some(bytes), Some(&mut written), None) }
                .map_err(io::Error::other)?;
            Ok(written as usize)
        }

        fn flush(&mut self) -> io::Result<()> {
            unsafe { FlushFileBuffers(self.0) }.map_err(io::Error::other)
        }
    }

    fn pipe_name() -> Result<Vec<u16>, ControlError> {
        let mut session_id = 0;
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) }
            .map_err(io::Error::other)?;
        Ok(
            format!(r"\\.\pipe\AudioSwitch.Control.v1.Session-{session_id}")
                .encode_utf16()
                .chain(Some(0))
                .collect(),
        )
    }

    pub fn connect() -> Result<Pipe, ControlError> {
        let name = pipe_name()?;
        let name = PCWSTR(name.as_ptr());
        let _ = unsafe { WaitNamedPipeW(name, 2_000) };
        let handle = unsafe {
            CreateFileW(
                name,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE::default(),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|error| ControlError::Unavailable(io::Error::other(error)))?;
        Ok(Pipe(handle))
    }

    pub struct Listener {
        name: Vec<u16>,
        first: Pipe,
    }

    impl Listener {
        pub fn bind() -> Result<Self, ControlError> {
            let name = pipe_name()?;
            let first = create_pipe(&name)?;
            Ok(Self { name, first })
        }

        pub fn serve(self, mut serve_client: impl FnMut(&mut Pipe)) {
            let mut pending = Some(self.first);
            loop {
                let mut pipe = match pending
                    .take()
                    .map(Ok)
                    .unwrap_or_else(|| create_pipe(&self.name))
                {
                    Ok(pipe) => pipe,
                    Err(error) => {
                        eprintln!("Audio Switch control pipe creation failed: {error}");
                        return;
                    }
                };
                let connected = match unsafe { ConnectNamedPipe(pipe.0, None) } {
                    Ok(()) => true,
                    Err(_) => (unsafe { GetLastError() }) == ERROR_PIPE_CONNECTED,
                };
                if connected {
                    serve_client(&mut pipe);
                    unsafe {
                        let _ = DisconnectNamedPipe(pipe.0);
                    }
                }
            }
        }
    }

    fn create_pipe(name: &[u16]) -> Result<Pipe, ControlError> {
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                MAX_FRAME_BYTES as u32,
                MAX_FRAME_BYTES as u32,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            return Err(io::Error::other(format!(
                "Windows error {} creating the named pipe",
                unsafe { GetLastError().0 }
            ))
            .into());
        }
        Ok(Pipe(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_one_frame_and_ignores_trailing_bytes() {
        let mut input: &[u8] = b"{\"type\":\"getState\"}\nleftover";
        assert_eq!(read_frame(&mut input).unwrap(), b"{\"type\":\"getState\"}");
    }

    #[test]
    fn rejects_a_connection_closed_mid_frame() {
        let mut input: &[u8] = b"{\"type\":";
        assert!(matches!(read_frame(&mut input), Err(ControlError::Closed)));
    }
}
