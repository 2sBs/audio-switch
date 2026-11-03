use thiserror::Error;

use crate::state::ActiveMic;

use super::CueKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum EndpointFlow {
    Render,
    Capture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub flow: EndpointFlow,
    pub available: bool,
    /// The system's default device for this flow.
    pub system_default: bool,
    /// Identifies the device that owns this endpoint. A device's input and
    /// output endpoints share it, which pairs a virtual cable's two sides.
    pub device_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("audio endpoint not found: {0}")]
    EndpointNotFound(String),
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),
    #[error("audio backend failed: {0}")]
    Backend(String),
}

pub trait VirtualMicSink: Send {
    fn endpoint_id(&self) -> &str;
    fn write_mono(&mut self, frames: &[f32]) -> Result<usize, AudioError>;
    fn write_silence(&mut self, frame_count: usize) -> Result<usize, AudioError>;
}

pub trait AudioBackend: Send + Sync {
    fn enumerate_endpoints(&self) -> Result<Vec<Endpoint>, AudioError>;
    fn microphone_in_use(
        &self,
        sink_render_id: Option<&str>,
        capture_id: Option<&str>,
    ) -> Result<bool, AudioError>;
    fn stop_activity_monitor(&self);
    fn start_mirror(
        &self,
        source_render_id: &str,
        target_render_id: &str,
    ) -> Result<(), AudioError>;
    fn stop_mirror(&self) -> Result<(), AudioError>;
    fn start_microphone(
        &self,
        source_capture_id: &str,
        active: ActiveMic,
        sink_render_id: &str,
    ) -> Result<(), AudioError>;
    fn stop_microphone(&self) -> Result<(), AudioError>;
    fn play_cue(
        &self,
        render_endpoint_id: &str,
        cue: CueKind,
        level_db: f32,
    ) -> Result<(), AudioError>;
}
