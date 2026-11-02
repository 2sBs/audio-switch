mod bridge;
#[cfg(target_os = "macos")]
mod coreaudio_backend;
mod cue;
mod drift;
mod traits;
#[cfg(windows)]
mod wasapi_backend;

pub use bridge::RouteDiagnostics;
#[cfg(target_os = "macos")]
pub use coreaudio_backend::CoreAudioBackend;
pub use cue::{CueKind, synthesize_cue};
pub use drift::{DriftController, DriftControllerConfig};
pub use traits::{AudioBackend, AudioError, Endpoint, EndpointFlow, VirtualMicSink};
#[cfg(windows)]
pub use wasapi_backend::WasapiBackend;
