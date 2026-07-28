use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::DeviceLabels;
use crate::state::{ActiveMic, AppState};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ControlCommand {
    GetState {
        request_id: Option<String>,
    },
    Subscribe {
        request_id: Option<String>,
    },
    ToggleMic {
        request_id: Option<String>,
    },
    SetActiveMic {
        request_id: Option<String>,
        active_mic: ActiveMic,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ControlMessage {
    State {
        request_id: Option<String>,
        state: AppState,
        labels: DeviceLabels,
    },
    Error {
        request_id: Option<String>,
        message: String,
    },
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("IPC frame exceeds {MAX_FRAME_BYTES} bytes")]
    TooLarge,
    #[error("IPC frame contains invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn decode_command(frame: &[u8]) -> Result<ControlCommand, FrameError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    Ok(serde_json::from_slice(frame)?)
}

pub fn decode_message(frame: &[u8]) -> Result<ControlMessage, FrameError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    Ok(serde_json::from_slice(frame)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip_uses_stable_wire_names() {
        let command = ControlCommand::SetActiveMic {
            request_id: Some("42".into()),
            active_mic: ActiveMic::Secondary,
        };
        let encoded = encode_frame(&command).unwrap();
        assert_eq!(
            decode_command(&encoded[..encoded.len() - 1]).unwrap(),
            command
        );
        assert!(String::from_utf8(encoded).unwrap().contains("setActiveMic"));
    }

    #[test]
    fn state_message_round_trips() {
        let message = ControlMessage::State {
            request_id: Some("state-1".into()),
            state: AppState::new(ActiveMic::Primary),
            labels: DeviceLabels::default(),
        };
        let encoded = encode_frame(&message).unwrap();
        assert_eq!(
            decode_message(&encoded[..encoded.len() - 1]).unwrap(),
            message
        );
    }
}
