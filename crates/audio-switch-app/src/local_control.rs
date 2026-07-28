use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result, anyhow};
use audio_switch_core::ipc::{
    ControlCommand, ControlMessage, MAX_FRAME_BYTES, decode_command, decode_message, encode_frame,
};
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
use windows::core::w;

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn start_server(handler: impl Fn(ControlCommand) -> ControlMessage + Send + Sync + 'static) {
    let handler = Arc::new(handler);
    thread::Builder::new()
        .name("audio-switch-control".into())
        .spawn(move || server_loop(handler))
        .expect("starting local control server");
}

pub fn send_command(command: &ControlCommand) -> Result<ControlMessage> {
    let _ = unsafe { WaitNamedPipeW(w!(r"\\.\pipe\AudioSwitch.Control.v1"), 2_000) };
    let handle = unsafe {
        CreateFileW(
            w!(r"\\.\pipe\AudioSwitch.Control.v1"),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_MODE::default(),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .context("connecting to the Audio Switch tray")?;
    let handle = OwnedHandle(handle);
    write_all(handle.0, &encode_frame(command)?)?;
    let frame = read_frame(handle.0)?;
    decode_message(&frame).context("decoding the tray response")
}

fn server_loop(handler: Arc<dyn Fn(ControlCommand) -> ControlMessage + Send + Sync>) {
    loop {
        let handle = unsafe {
            CreateNamedPipeW(
                w!(r"\\.\pipe\AudioSwitch.Control.v1"),
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
            eprintln!("Audio Switch control pipe creation failed: {}", unsafe {
                GetLastError().0
            });
            return;
        }
        let handle = OwnedHandle(handle);
        let connected = match unsafe { ConnectNamedPipe(handle.0, None) } {
            Ok(()) => true,
            Err(_) => (unsafe { GetLastError() }) == ERROR_PIPE_CONNECTED,
        };
        if connected {
            if let Err(error) = serve_client(handle.0, &handler) {
                eprintln!("Audio Switch control request failed: {error:#}");
            }
            unsafe {
                let _ = FlushFileBuffers(handle.0);
                let _ = DisconnectNamedPipe(handle.0);
            }
        }
    }
}

fn serve_client(
    handle: HANDLE,
    handler: &Arc<dyn Fn(ControlCommand) -> ControlMessage + Send + Sync>,
) -> Result<()> {
    let frame = read_frame(handle).context("reading command")?;
    let response = match decode_command(&frame) {
        Ok(command) => handler(command),
        Err(error) => ControlMessage::Error {
            request_id: None,
            message: error.to_string(),
        },
    };
    write_all(handle, &encode_frame(&response)?).context("writing response")
}

fn read_frame(handle: HANDLE) -> Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(512);
    let mut buffer = [0_u8; 512];
    loop {
        let mut read = 0_u32;
        unsafe { ReadFile(handle, Some(&mut buffer), Some(&mut read), None) }
            .context("reading local control pipe")?;
        if read == 0 {
            return Err(anyhow!("local control pipe closed before a complete frame"));
        }
        frame.extend_from_slice(&buffer[..read as usize]);
        if frame.len() > MAX_FRAME_BYTES {
            return Err(anyhow!("local control frame is too large"));
        }
        if let Some(newline) = frame.iter().position(|byte| *byte == b'\n') {
            frame.truncate(newline);
            return Ok(frame);
        }
    }
}

fn write_all(handle: HANDLE, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let mut written = 0_u32;
        unsafe { WriteFile(handle, Some(bytes), Some(&mut written), None) }
            .context("writing local control pipe")?;
        if written == 0 {
            return Err(anyhow!("local control pipe accepted no bytes"));
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}
