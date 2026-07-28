use thiserror::Error;

use crate::audio::{AudioBackend, AudioError, CueKind, Endpoint};
use crate::config::{AppConfig, ConfigError, ConfigStore};
use crate::state::{
    ActiveMic, AppState, DesiredTransition, EndpointAvailability, RouteStatus, RoutingStateMachine,
};

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("required endpoint is not configured: {0}")]
    MissingEndpoint(&'static str),
}

pub struct RoutingController<B: AudioBackend> {
    backend: B,
    store: ConfigStore,
    config: AppConfig,
    machine: RoutingStateMachine,
}

impl<B: AudioBackend> RoutingController<B> {
    pub fn new(backend: B, store: ConfigStore, config: AppConfig) -> Self {
        let machine = RoutingStateMachine::new(
            config.active_mic,
            EndpointAvailability {
                primary_mic: false,
                secondary_mic: false,
                virtual_mic: false,
                primary_render: false,
                secondary_render: false,
            },
        );
        Self {
            backend,
            store,
            config,
            machine,
        }
    }

    pub fn state(&self) -> &AppState {
        self.machine.state()
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn start(&mut self) -> Result<(), ControllerError> {
        self.refresh_endpoints()
    }

    pub fn refresh_endpoints(&mut self) -> Result<(), ControllerError> {
        let endpoints = self.backend.enumerate_endpoints()?;
        let transitions = self
            .machine
            .update_availability(self.availability(&endpoints));
        self.execute(transitions);
        Ok(())
    }

    pub fn toggle_mic(&mut self) -> Result<(), ControllerError> {
        self.select_mic(self.machine.state().active_mic.toggled())
    }

    pub fn select_mic(&mut self, active: ActiveMic) -> Result<(), ControllerError> {
        let changed = self.machine.state().active_mic != active;
        let transitions = self.machine.select_mic(active);
        self.config.active_mic = active;
        self.store.save(&self.config)?;
        self.execute(transitions);

        if changed
            && self.config.audio.cue_enabled
            && matches!(self.machine.state().mic_status, RouteStatus::Routing)
        {
            let endpoint =
                self.required(&self.config.endpoints.primary_render, "primary render")?;
            let cue = match active {
                ActiveMic::Primary => CueKind::Primary,
                ActiveMic::Secondary => CueKind::Secondary,
            };
            if let Err(error) = self
                .backend
                .play_cue(endpoint, cue, self.config.audio.cue_level_db)
            {
                // Cue failure must never roll back a successful microphone switch.
                eprintln!("Audio Switch cue failed: {error}");
            }
        }
        Ok(())
    }

    pub fn apply_config(&mut self, config: AppConfig) -> Result<(), ControllerError> {
        self.stop();
        self.store.save(&config)?;
        self.machine = RoutingStateMachine::new(
            config.active_mic,
            EndpointAvailability {
                primary_mic: false,
                secondary_mic: false,
                virtual_mic: false,
                primary_render: false,
                secondary_render: false,
            },
        );
        self.config = config;
        self.refresh_endpoints()
    }

    pub fn stop(&mut self) {
        let _ = self.backend.stop_microphone();
        let _ = self.backend.stop_mirror();
    }

    fn availability(&self, endpoints: &[Endpoint]) -> EndpointAvailability {
        let available = |configured: &Option<String>| {
            configured.as_ref().is_some_and(|id| {
                endpoints
                    .iter()
                    .any(|endpoint| endpoint.available && endpoint.id.eq_ignore_ascii_case(id))
            })
        };
        EndpointAvailability {
            primary_mic: available(&self.config.endpoints.primary_capture),
            secondary_mic: available(&self.config.endpoints.secondary_capture),
            virtual_mic: available(&self.config.endpoints.virtual_render),
            primary_render: available(&self.config.endpoints.primary_render),
            secondary_render: available(&self.config.endpoints.secondary_render),
        }
    }

    fn execute(&mut self, transitions: Vec<DesiredTransition>) {
        for transition in transitions {
            match transition {
                DesiredTransition::None => {}
                DesiredTransition::StopMic => {
                    if let Err(error) = self.backend.stop_microphone() {
                        self.machine.mic_failed(error.to_string());
                    }
                }
                DesiredTransition::StartMic(active) => {
                    let source = match active {
                        ActiveMic::Primary => self.config.endpoints.primary_capture.as_deref(),
                        ActiveMic::Secondary => self.config.endpoints.secondary_capture.as_deref(),
                    };
                    let sink = self.config.endpoints.virtual_render.as_deref();
                    match (source, sink) {
                        (Some(source), Some(sink)) => {
                            match self.backend.start_microphone(source, active, sink) {
                                Ok(()) => self.machine.mic_started(),
                                Err(error) => self.machine.mic_failed(error.to_string()),
                            }
                        }
                        _ => self
                            .machine
                            .mic_failed("microphone route is not fully configured"),
                    }
                }
                DesiredTransition::StopMirror => {
                    if let Err(error) = self.backend.stop_mirror() {
                        self.machine.mirror_failed(error.to_string());
                    }
                }
                DesiredTransition::StartMirror => {
                    let source = self.config.endpoints.primary_render.as_deref();
                    let target = self.config.endpoints.secondary_render.as_deref();
                    match (source, target) {
                        (Some(source), Some(target)) => {
                            match self.backend.start_mirror(source, target) {
                                Ok(()) => self.machine.mirror_started(),
                                Err(error) => self.machine.mirror_failed(error.to_string()),
                            }
                        }
                        _ => self
                            .machine
                            .mirror_failed("playback mirror is not fully configured"),
                    }
                }
            }
        }
    }

    fn required<'a>(
        &self,
        endpoint: &'a Option<String>,
        label: &'static str,
    ) -> Result<&'a str, ControllerError> {
        endpoint
            .as_deref()
            .ok_or(ControllerError::MissingEndpoint(label))
    }
}

impl<B: AudioBackend> Drop for RoutingController<B> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::audio::EndpointFlow;

    #[derive(Default)]
    struct FakeBackend {
        calls: Mutex<Vec<String>>,
        endpoints: Mutex<Vec<Endpoint>>,
    }

    impl FakeBackend {
        fn with_configured_endpoints(config: &AppConfig) -> Self {
            let endpoint = |id: &Option<String>, flow| Endpoint {
                id: id.clone().unwrap(),
                name: id.clone().unwrap(),
                flow,
                available: true,
            };
            Self {
                calls: Mutex::new(Vec::new()),
                endpoints: Mutex::new(vec![
                    endpoint(&config.endpoints.primary_render, EndpointFlow::Render),
                    endpoint(&config.endpoints.secondary_render, EndpointFlow::Render),
                    endpoint(&config.endpoints.primary_capture, EndpointFlow::Capture),
                    endpoint(&config.endpoints.secondary_capture, EndpointFlow::Capture),
                    endpoint(&config.endpoints.virtual_render, EndpointFlow::Render),
                ]),
            }
        }
    }

    impl AudioBackend for FakeBackend {
        fn enumerate_endpoints(&self) -> Result<Vec<Endpoint>, AudioError> {
            Ok(self.endpoints.lock().unwrap().clone())
        }

        fn start_mirror(&self, source: &str, target: &str) -> Result<(), AudioError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("mirror:{source}->{target}"));
            Ok(())
        }

        fn stop_mirror(&self) -> Result<(), AudioError> {
            self.calls.lock().unwrap().push("stop-mirror".into());
            Ok(())
        }

        fn start_microphone(
            &self,
            source: &str,
            active: ActiveMic,
            sink: &str,
        ) -> Result<(), AudioError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("mic:{active:?}:{source}->{sink}"));
            Ok(())
        }

        fn stop_microphone(&self) -> Result<(), AudioError> {
            self.calls.lock().unwrap().push("stop-mic".into());
            Ok(())
        }

        fn play_cue(
            &self,
            _render_endpoint_id: &str,
            cue: CueKind,
            _level_db: f32,
        ) -> Result<(), AudioError> {
            self.calls.lock().unwrap().push(format!("cue:{cue:?}"));
            Ok(())
        }
    }

    fn configured() -> AppConfig {
        let mut config = AppConfig::default();
        config.endpoints.primary_render = Some("primary".into());
        config.endpoints.secondary_render = Some("secondary".into());
        config.endpoints.primary_capture = Some("primary-mic".into());
        config.endpoints.secondary_capture = Some("secondary-mic".into());
        config.endpoints.virtual_render = Some("virtual-mic".into());
        config
    }

    #[test]
    fn startup_and_toggle_use_one_authoritative_path() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let config = configured();
        store.save(&config).unwrap();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let mut controller = RoutingController::new(backend, store.clone(), config);

        controller.start().unwrap();
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Routing
        ));
        assert!(matches!(
            controller.state().mirror_status,
            RouteStatus::Routing
        ));
        controller.toggle_mic().unwrap();

        assert_eq!(controller.state().active_mic, ActiveMic::Secondary);
        assert_eq!(
            store.load_or_default().unwrap().active_mic,
            ActiveMic::Secondary
        );
        let calls = controller.backend().calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|call| call == "mic:Primary:primary-mic->virtual-mic")
        );
        assert!(
            calls
                .iter()
                .any(|call| call == "mic:Secondary:secondary-mic->virtual-mic")
        );
        assert!(calls.iter().any(|call| call == "cue:Secondary"));
    }
}
