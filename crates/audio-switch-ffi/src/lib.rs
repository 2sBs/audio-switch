//! Swift-facing core of the macOS app.
//!
//! `AppCore` owns the routing controller, refreshes endpoints once a second
//! (as the Windows tray does), serves the local control channel for the
//! Stream Deck plugin and CLI, and pushes every state change to a Swift
//! observer. The Swift app handles UI and OS integration only.

#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use audio_switch_core::audio::{AudioBackend, CoreAudioBackend, Endpoint};
use audio_switch_core::config::ConfigError;
use audio_switch_core::control::{self, ControlError};
use audio_switch_core::ipc::{ControlCommand, ControlMessage};
use audio_switch_core::{
    ActiveMic, AppConfig, AppState, ConfigStore, ControllerError, DeviceLabels,
    EndpointAvailability, RoutingController,
};

uniffi::setup_scaffolding!();

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum CoreError {
    #[error("another copy of Audio Switch is already running")]
    AlreadyRunning,
    #[error("{0}")]
    Configuration(String),
    #[error("{0}")]
    Audio(String),
    #[error("{0}")]
    Control(String),
}

impl From<ControllerError> for CoreError {
    fn from(error: ControllerError) -> Self {
        match error {
            ControllerError::Audio(error) => Self::Audio(error.to_string()),
            error => Self::Configuration(error.to_string()),
        }
    }
}

impl From<ConfigError> for CoreError {
    fn from(error: ConfigError) -> Self {
        Self::Configuration(error.to_string())
    }
}

impl From<ControlError> for CoreError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::AlreadyRunning => Self::AlreadyRunning,
            error => Self::Control(error.to_string()),
        }
    }
}

/// What the UI shows: the routing state, which configured devices are
/// present, and the user's names for the roles.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Snapshot {
    pub state: AppState,
    pub availability: EndpointAvailability,
    pub labels: DeviceLabels,
    /// How the connected devices differ from the setup, one sentence per line.
    /// Empty at the configured setup.
    pub away: Vec<String>,
}

/// Receives every state change, in order, from background threads.
/// Implementations must return quickly and must not call back into `AppCore`
/// synchronously; hop to the main actor instead.
#[uniffi::export(with_foreign)]
pub trait StateObserver: Send + Sync {
    fn state_changed(&self, snapshot: Snapshot);
}

#[derive(uniffi::Object)]
pub struct AppCore {
    session: Arc<Session<CoreAudioBackend>>,
    configured_at_start: bool,
}

#[uniffi::export]
impl AppCore {
    /// Loads settings, claims the control channel, and starts routing. Fails
    /// with `AlreadyRunning` before touching any audio device if another copy
    /// owns the control channel.
    #[uniffi::constructor]
    pub fn start(observer: Arc<dyn StateObserver>) -> Result<Arc<Self>, CoreError> {
        let store = ConfigStore::for_current_user()?;
        let configured_at_start = store.exists();
        let config = store.load_or_default()?;
        let backend =
            CoreAudioBackend::new(config.audio.mirror_buffer_ms, config.audio.mic_buffer_ms);
        let session = Arc::new(Session::new(
            RoutingController::new(backend, store, config),
            observer,
        ));

        let control_session = Arc::downgrade(&session);
        control::start_server(move |command| handle_control_command(&control_session, command))?;
        session.run(RoutingController::start)?;
        spawn_refresh(Arc::downgrade(&session));
        Ok(Arc::new(Self {
            session,
            configured_at_start,
        }))
    }

    /// False on first launch, before setup has saved any settings.
    pub fn was_configured_at_start(&self) -> bool {
        self.configured_at_start
    }

    pub fn snapshot(&self) -> Snapshot {
        self.session.lock().snapshot()
    }

    pub fn config(&self) -> AppConfig {
        self.session.lock().controller.config().clone()
    }

    pub fn endpoints(&self) -> Result<Vec<Endpoint>, CoreError> {
        let session = self.session.lock();
        Ok(session
            .controller
            .backend()
            .enumerate_endpoints()
            .map_err(ControllerError::from)?)
    }

    pub fn toggle_mic(&self) -> Result<(), CoreError> {
        self.session.run(RoutingController::toggle_mic)
    }

    pub fn select_mic(&self, active: ActiveMic) -> Result<(), CoreError> {
        self.session.run(|controller| controller.select_mic(active))
    }

    /// Saves `config` and restarts only the routes whose devices changed.
    /// The live microphone choice is kept: only `toggle_mic` and `select_mic`
    /// change it, so a settings edit made after a hotkey switch cannot undo it.
    pub fn apply_config(&self, config: AppConfig) -> Result<(), CoreError> {
        self.session.apply_config(config)
    }
}

/// Fresh settings for setup to start from.
#[uniffi::export]
pub fn default_config() -> AppConfig {
    AppConfig::default()
}

/// The routing controller plus the last snapshot sent to the observer, behind
/// one lock so notifications are delivered in the order changes happen.
struct Session<B: AudioBackend> {
    inner: Mutex<Inner<B>>,
    observer: Arc<dyn StateObserver>,
}

struct Inner<B: AudioBackend> {
    controller: RoutingController<B>,
    published: Option<Snapshot>,
}

impl<B: AudioBackend> Inner<B> {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            state: self.controller.state().clone(),
            availability: self.controller.availability(),
            labels: self.controller.config().labels.clone(),
            away: self.controller.away_summary(),
        }
    }
}

impl<B: AudioBackend> Session<B> {
    fn new(controller: RoutingController<B>, observer: Arc<dyn StateObserver>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                controller,
                published: None,
            }),
            observer,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<B>> {
        self.inner.lock().expect("session mutex poisoned")
    }

    fn apply_config(&self, mut config: AppConfig) -> Result<(), CoreError> {
        self.run(|controller| {
            config.active_mic = controller.state().active_mic;
            controller.apply_config(config)
        })
    }

    /// Runs one controller operation and notifies the observer if the
    /// snapshot changed, even when the operation failed part-way.
    fn run<T>(
        &self,
        operation: impl FnOnce(&mut RoutingController<B>) -> Result<T, ControllerError>,
    ) -> Result<T, CoreError> {
        let mut inner = self.lock();
        let result = operation(&mut inner.controller);
        let snapshot = inner.snapshot();
        if inner.published.as_ref() != Some(&snapshot) {
            inner.published = Some(snapshot.clone());
            self.observer.state_changed(snapshot);
        }
        Ok(result?)
    }
}

/// Polls endpoints so routes follow devices being plugged in or removed.
/// Stops once the session is dropped.
fn spawn_refresh<B: AudioBackend + 'static>(session: Weak<Session<B>>) {
    let _ = thread::Builder::new()
        .name("audio-switch-refresh".into())
        .spawn(move || {
            loop {
                thread::sleep(REFRESH_INTERVAL);
                let Some(session) = session.upgrade() else {
                    return;
                };
                if let Err(error) = session.run(RoutingController::refresh_endpoints) {
                    eprintln!("Audio Switch endpoint refresh failed: {error}");
                }
            }
        });
}

fn handle_control_command<B: AudioBackend>(
    session: &Weak<Session<B>>,
    command: ControlCommand,
) -> ControlMessage {
    let request_id = match &command {
        ControlCommand::GetState { request_id }
        | ControlCommand::Subscribe { request_id }
        | ControlCommand::ToggleMic { request_id }
        | ControlCommand::SetActiveMic { request_id, .. } => request_id.clone(),
    };
    let Some(session) = session.upgrade() else {
        return ControlMessage::Error {
            request_id,
            message: "Audio Switch is shutting down".into(),
        };
    };
    let result = session.run(|controller| match command {
        ControlCommand::GetState { .. } | ControlCommand::Subscribe { .. } => Ok(()),
        ControlCommand::ToggleMic { .. } => controller.toggle_mic(),
        ControlCommand::SetActiveMic { active_mic, .. } => controller.select_mic(active_mic),
    });
    match result {
        Ok(()) => {
            let Snapshot { state, labels, .. } = session.lock().snapshot();
            ControlMessage::State {
                request_id,
                state,
                labels,
            }
        }
        Err(error) => ControlMessage::Error {
            request_id,
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_switch_core::audio::{AudioError, CueKind, EndpointFlow};

    struct Recorder(Mutex<Vec<Snapshot>>);

    impl StateObserver for Recorder {
        fn state_changed(&self, snapshot: Snapshot) {
            self.0.lock().unwrap().push(snapshot);
        }
    }

    struct FakeBackend(Vec<Endpoint>);

    impl AudioBackend for FakeBackend {
        fn enumerate_endpoints(&self) -> Result<Vec<Endpoint>, AudioError> {
            Ok(self.0.clone())
        }
        fn start_mirror(&self, _: &str, _: &str) -> Result<(), AudioError> {
            Ok(())
        }
        fn stop_mirror(&self) -> Result<(), AudioError> {
            Ok(())
        }
        fn start_microphone(&self, _: &str, _: ActiveMic, _: &str) -> Result<(), AudioError> {
            Ok(())
        }
        fn stop_microphone(&self) -> Result<(), AudioError> {
            Ok(())
        }
        fn play_cue(&self, _: &str, _: CueKind, _: f32) -> Result<(), AudioError> {
            Ok(())
        }
    }

    fn session(directory: &tempfile::TempDir) -> (Arc<Session<FakeBackend>>, Arc<Recorder>) {
        let mut config = AppConfig::default();
        config.audio.cue_enabled = false;
        let endpoints = &mut config.endpoints;
        endpoints.primary_capture = Some("mic-a".into());
        endpoints.secondary_capture = Some("mic-b".into());
        endpoints.virtual_render = Some("cable".into());
        let backend = FakeBackend(
            [
                ("mic-a", EndpointFlow::Capture),
                ("mic-b", EndpointFlow::Capture),
                ("cable", EndpointFlow::Render),
            ]
            .map(|(id, flow)| Endpoint {
                id: id.into(),
                name: id.into(),
                flow,
                available: true,
                system_default: false,
                device_id: None,
            })
            .into(),
        );
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
        let session = Arc::new(Session::new(
            RoutingController::new(backend, store, config),
            recorder.clone(),
        ));
        session.run(RoutingController::start).unwrap();
        (session, recorder)
    }

    #[test]
    fn observer_hears_each_change_once() {
        let directory = tempfile::tempdir().unwrap();
        let (session, recorder) = session(&directory);
        session.run(RoutingController::refresh_endpoints).unwrap();
        session.run(RoutingController::toggle_mic).unwrap();

        let published = recorder.0.lock().unwrap();
        assert_eq!(
            published.len(),
            2,
            "an unchanged refresh is not republished"
        );
        assert_eq!(published[1].state.active_mic, ActiveMic::Secondary);
    }

    #[test]
    fn applying_settings_keeps_the_live_microphone() {
        let directory = tempfile::tempdir().unwrap();
        let (session, _) = session(&directory);
        let stale = session.lock().controller.config().clone();
        session.run(RoutingController::toggle_mic).unwrap();

        let mut edited = stale;
        edited.labels.primary = "Desk".into();
        session.apply_config(edited).unwrap();

        let snapshot = session.lock().snapshot();
        assert_eq!(snapshot.state.active_mic, ActiveMic::Secondary);
        assert_eq!(snapshot.labels.primary, "Desk");
    }

    #[test]
    fn control_commands_toggle_and_report_state() {
        let directory = tempfile::tempdir().unwrap();
        let (session, _) = session(&directory);
        let weak = Arc::downgrade(&session);

        let response = handle_control_command(
            &weak,
            ControlCommand::ToggleMic {
                request_id: Some("1".into()),
            },
        );

        let ControlMessage::State {
            request_id, state, ..
        } = response
        else {
            panic!("expected state, got {response:?}");
        };
        assert_eq!(request_id.as_deref(), Some("1"));
        assert_eq!(state.active_mic, ActiveMic::Secondary);
        drop(session);
        assert!(matches!(
            handle_control_command(&weak, ControlCommand::GetState { request_id: None }),
            ControlMessage::Error { .. }
        ));
    }
}
