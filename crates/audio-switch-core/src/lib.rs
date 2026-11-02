#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

pub mod audio;
mod away;
pub mod config;
pub mod control;
pub mod ipc;
pub mod runtime;
pub mod state;

pub use away::away_summary;
pub use config::{AppConfig, ConfigStore, DeviceLabels, UpdatePolicy};
pub use runtime::{ControllerError, RoutingController};
pub use state::{
    ActiveMic, AppState, EndpointAvailability, RouteStatus, RoutingStateMachine, SystemAudio,
};
