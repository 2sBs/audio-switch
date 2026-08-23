#![forbid(unsafe_op_in_unsafe_fn)]

pub mod audio;
pub mod config;
pub mod ipc;
pub mod runtime;
pub mod state;

pub use config::{AppConfig, ConfigStore, DeviceLabels, UpdatePolicy};
pub use runtime::{ControllerError, RoutingController};
pub use state::{ActiveMic, AppState, RouteStatus, RoutingStateMachine};
