use std::collections::HashSet;
use std::net::TcpStream;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use audio_switch_core::ipc::{ControlCommand, ControlMessage};
use audio_switch_core::{ActiveMic, AppState, DeviceLabels, RouteStatus};
use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error as WebSocketError, Message, WebSocket, connect};

use crate::local_control;

type StreamDeckSocket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Clone, PartialEq, Eq)]
struct UiState {
    state: AppState,
    labels: DeviceLabels,
}

pub fn run(args: &[String]) -> Result<()> {
    let port = argument(args, "-port")?;
    let plugin_uuid = argument(args, "-pluginUUID")?;
    let register_event = argument(args, "-registerEvent")?;
    let endpoint = format!("ws://127.0.0.1:{port}");
    let (mut socket, _) = connect(endpoint.as_str()).context("connecting to Stream Deck")?;
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .context("setting Stream Deck polling interval")?;
    }
    send_json(
        &mut socket,
        &json!({ "event": register_event, "uuid": plugin_uuid }),
    )?;

    let mut contexts = HashSet::new();
    let mut last_state: Option<UiState> = None;
    let mut last_poll = Instant::now() - Duration::from_secs(2);
    let mut host_launch_attempted = false;

    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                handle_event(&mut socket, &mut contexts, &mut last_state, text.as_str())?;
            }
            Ok(Message::Ping(payload)) => socket.send(Message::Pong(payload))?,
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(WebSocketError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => break,
            Err(error) => return Err(error).context("reading Stream Deck event"),
        }

        if last_poll.elapsed() >= Duration::from_secs(1) {
            match request_state(ControlCommand::GetState { request_id: None }) {
                Ok(state) => {
                    host_launch_attempted = false;
                    if last_state.as_ref() != Some(&state) {
                        update_contexts(&mut socket, &contexts, &state)?;
                        last_state = Some(state);
                    }
                }
                Err(_) if !host_launch_attempted => {
                    launch_tray_host()?;
                    host_launch_attempted = true;
                }
                Err(_) => {}
            }
            last_poll = Instant::now();
        }
    }
    Ok(())
}

fn handle_event(
    socket: &mut StreamDeckSocket,
    contexts: &mut HashSet<String>,
    last_state: &mut Option<UiState>,
    text: &str,
) -> Result<()> {
    let event: Value = serde_json::from_str(text).context("decoding Stream Deck event")?;
    let event_name = event
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let context = event
        .get("context")
        .and_then(Value::as_str)
        .map(str::to_owned);

    match event_name {
        "willAppear" => {
            if let Some(context) = context {
                contexts.insert(context);
                if let Some(state) = last_state {
                    update_contexts(socket, contexts, state)?;
                }
            }
        }
        "willDisappear" => {
            if let Some(context) = context {
                contexts.remove(&context);
            }
        }
        "keyUp" => {
            let result = request_state(ControlCommand::ToggleMic { request_id: None });
            match result {
                Ok(state) => {
                    update_contexts(socket, contexts, &state)?;
                    if let Some(context) = context {
                        send_json(socket, &json!({ "event": "showOk", "context": context }))?;
                    }
                    *last_state = Some(state);
                }
                Err(error) => {
                    if let Some(context) = context {
                        send_json(socket, &json!({ "event": "showAlert", "context": context }))?;
                    }
                    eprintln!("Audio Switch Stream Deck action failed: {error:#}");
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn update_contexts(
    socket: &mut StreamDeckSocket,
    contexts: &HashSet<String>,
    state: &UiState,
) -> Result<()> {
    let index = match state.state.active_mic {
        ActiveMic::Primary => 0,
        ActiveMic::Secondary => 1,
    };
    let title = match state.state.active_mic {
        ActiveMic::Primary => state.labels.primary.to_uppercase(),
        ActiveMic::Secondary => state.labels.secondary.to_uppercase(),
    };
    for context in contexts {
        send_json(
            socket,
            &json!({ "event": "setState", "context": context, "payload": { "state": index } }),
        )?;
        send_json(
            socket,
            &json!({ "event": "setTitle", "context": context, "payload": { "title": title.as_str(), "target": 0 } }),
        )?;
        if !matches!(state.state.mic_status, RouteStatus::Routing) {
            send_json(socket, &json!({ "event": "showAlert", "context": context }))?;
        }
    }
    Ok(())
}

fn request_state(command: ControlCommand) -> Result<UiState> {
    match local_control::send_command(&command)? {
        ControlMessage::State { state, labels, .. } => Ok(UiState { state, labels }),
        ControlMessage::Error { message, .. } => Err(anyhow!(message)),
    }
}

fn launch_tray_host() -> Result<()> {
    let executable = std::env::current_exe().context("locating Audio Switch executable")?;
    Command::new(executable)
        .arg("--tray")
        .spawn()
        .context("starting the Audio Switch tray")?;
    thread::sleep(Duration::from_millis(300));
    Ok(())
}

fn send_json(socket: &mut StreamDeckSocket, value: &Value) -> Result<()> {
    socket
        .send(Message::Text(value.to_string().into()))
        .context("sending Stream Deck command")
}

fn argument<'a>(args: &'a [String], name: &str) -> Result<&'a str> {
    let index = args
        .iter()
        .position(|argument| argument == name)
        .with_context(|| format!("Stream Deck did not provide {name}"))?;
    args.get(index + 1)
        .map(String::as_str)
        .with_context(|| format!("Stream Deck did not provide a value for {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parses_stream_deck_arguments() {
        let args = vec!["plugin.exe".into(), "-port".into(), "12345".into()];
        assert_eq!(argument(&args, "-port").unwrap(), "12345");
        assert!(argument(&args, "-pluginUUID").is_err());
    }

    #[test]
    fn registers_with_stream_deck_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let plugin = thread::spawn(move || {
            run(&[
                "audio-switch.exe".into(),
                "-port".into(),
                port.to_string(),
                "-pluginUUID".into(),
                "com.2sbsi.audio-switch".into(),
                "-registerEvent".into(),
                "registerPlugin".into(),
            ])
        });
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let registration = socket.read().unwrap().into_text().unwrap();
        let registration: Value = serde_json::from_str(&registration).unwrap();
        assert_eq!(registration["event"], "registerPlugin");
        assert_eq!(registration["uuid"], "com.2sbsi.audio-switch");
        socket.close(None).unwrap();
        plugin.join().unwrap().unwrap();
    }
}
