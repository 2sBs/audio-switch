use std::time::{Duration, Instant};

use thiserror::Error;

use crate::audio::{AudioBackend, AudioError, CueKind, Endpoint, EndpointFlow};
use crate::config::{AppConfig, ConfigError, ConfigStore};
use crate::state::{
    ActiveMic, AppState, DesiredTransition, EndpointAvailability, RouteStatus, RoutingStateMachine,
    SystemAudio,
};

const ROUTE_ERROR_RETRY_INTERVAL: Duration = Duration::from_secs(5);

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
    next_error_retry: Instant,
}

impl<B: AudioBackend> RoutingController<B> {
    pub fn new(backend: B, store: ConfigStore, config: AppConfig) -> Self {
        let machine = RoutingStateMachine::new(config.active_mic, EndpointAvailability::default());
        Self {
            backend,
            store,
            config,
            machine,
            next_error_retry: Instant::now() + ROUTE_ERROR_RETRY_INTERVAL,
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

    /// Which configured devices were present at the last refresh.
    pub fn availability(&self) -> EndpointAvailability {
        self.machine.availability()
    }

    /// How the connected devices differ from the configured setup; see
    /// [`away_summary`](crate::away_summary).
    pub fn away_summary(&self) -> Vec<String> {
        crate::away_summary(&self.config, self.state(), self.availability())
    }

    pub fn start(&mut self) -> Result<(), ControllerError> {
        self.refresh_endpoints()
    }

    pub fn refresh_endpoints(&mut self) -> Result<(), ControllerError> {
        let endpoints = self.backend.enumerate_endpoints()?;
        let now = Instant::now();
        let had_route_error = route_has_error(self.machine.state());
        self.machine
            .update_system_audio(self.system_audio(&endpoints));
        let availability = self.availability_of(&endpoints);
        let activity = self.backend.microphone_in_use(
            self.config
                .endpoints
                .virtual_render
                .as_deref()
                .filter(|_| availability.virtual_mic),
            self.config.endpoints.virtual_capture.as_deref(),
        );
        let mut transitions = self
            .machine
            .update_availability(availability, activity.map_err(|error| error.to_string()));
        let retry_attempted = transitions.is_empty() && now >= self.next_error_retry;
        if retry_attempted {
            transitions = self.machine.retry_failed_routes();
        }
        self.execute(transitions);
        if route_has_error(self.machine.state()) && (!had_route_error || retry_attempted) {
            self.next_error_retry = now + ROUTE_ERROR_RETRY_INTERVAL;
        }
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
            && matches!(
                self.machine.state().mic_status,
                RouteStatus::Routing | RouteStatus::Idle
            )
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
        let microphone_changed = self.config.active_mic != config.active_mic
            || active_capture(&self.config) != active_capture(&config)
            || self.config.endpoints.virtual_render != config.endpoints.virtual_render;
        let mirror_changed = self.config.endpoints.primary_render
            != config.endpoints.primary_render
            || self.config.endpoints.secondary_render != config.endpoints.secondary_render;
        let microphone_was_routing =
            matches!(self.machine.state().mic_status, RouteStatus::Routing);
        let mirror_was_routing = matches!(self.machine.state().mirror_status, RouteStatus::Routing);

        self.store.save(&config)?;
        if microphone_changed {
            let _ = self.backend.stop_microphone();
        }
        if mirror_changed {
            let _ = self.backend.stop_mirror();
        }
        self.machine = RoutingStateMachine::new(config.active_mic, EndpointAvailability::default());
        if !microphone_changed && microphone_was_routing {
            self.machine.mic_started();
        }
        if !mirror_changed && mirror_was_routing {
            self.machine.mirror_started();
        }
        self.config = config;
        self.refresh_endpoints()
    }

    pub fn stop(&mut self) {
        self.backend.stop_activity_monitor();
        let _ = self.backend.stop_microphone();
        let _ = self.backend.stop_mirror();
    }

    fn availability_of(&self, endpoints: &[Endpoint]) -> EndpointAvailability {
        let available = |configured: &Option<String>| {
            endpoints
                .iter()
                .any(|endpoint| endpoint.available && is_configured(endpoint, configured))
        };
        let primary_render = &self.config.endpoints.primary_render;
        EndpointAvailability {
            primary_mic: available(&self.config.endpoints.primary_capture),
            secondary_mic: available(&self.config.endpoints.secondary_capture),
            virtual_mic: available(&self.config.endpoints.virtual_render),
            primary_render: available(primary_render),
            secondary_render: available(&self.config.endpoints.secondary_render),
            primary_render_is_system_output: system_default(endpoints, EndpointFlow::Render)
                .is_some_and(|output| is_configured(output, primary_render)),
        }
    }

    /// Names the system's default devices where they differ from the
    /// configuration. Reports nothing for a role that is not configured yet.
    fn system_audio(&self, endpoints: &[Endpoint]) -> SystemAudio {
        let configured = &self.config.endpoints;
        let other_output = system_default(endpoints, EndpointFlow::Render)
            .filter(|output| {
                configured.primary_render.is_some()
                    && !is_configured(output, &configured.primary_render)
            })
            .map(|output| output.name.clone());
        // Apps record from the cable's input side, which belongs to the same
        // device as the output Audio Switch writes to.
        let cable_device = endpoints
            .iter()
            .find(|endpoint| {
                endpoint.flow == EndpointFlow::Render
                    && is_configured(endpoint, &configured.virtual_render)
            })
            .and_then(|cable| cable.device_id.as_ref());
        let other_input = system_default(endpoints, EndpointFlow::Capture)
            .filter(|input| {
                configured.virtual_render.is_some()
                    && (cable_device.is_none() || input.device_id.as_ref() != cable_device)
            })
            .map(|input| input.name.clone());
        SystemAudio {
            other_output,
            other_input,
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

fn is_configured(endpoint: &Endpoint, configured: &Option<String>) -> bool {
    configured
        .as_ref()
        .is_some_and(|id| endpoint.id.eq_ignore_ascii_case(id))
}

fn system_default(endpoints: &[Endpoint], flow: EndpointFlow) -> Option<&Endpoint> {
    endpoints
        .iter()
        .find(|endpoint| endpoint.flow == flow && endpoint.system_default)
}

fn route_has_error(state: &AppState) -> bool {
    matches!(state.mic_status, RouteStatus::Error { .. })
        || matches!(state.mirror_status, RouteStatus::Error { .. })
}

fn active_capture(config: &AppConfig) -> Option<&str> {
    match config.active_mic {
        ActiveMic::Primary => config.endpoints.primary_capture.as_deref(),
        ActiveMic::Secondary => config.endpoints.secondary_capture.as_deref(),
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

    struct FakeBackend {
        calls: Mutex<Vec<String>>,
        endpoints: Mutex<Vec<Endpoint>>,
        fail_microphone: Mutex<bool>,
        microphone_in_use: Mutex<Result<bool, String>>,
    }

    impl FakeBackend {
        fn with_configured_endpoints(config: &AppConfig) -> Self {
            let endpoint = |id: &Option<String>, flow| Endpoint {
                id: id.clone().unwrap(),
                name: id.clone().unwrap(),
                flow,
                available: true,
                // Docked: the system plays through the Primary playback device.
                system_default: id == &config.endpoints.primary_render,
                device_id: id.clone(),
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
                fail_microphone: Mutex::new(false),
                microphone_in_use: Mutex::new(Ok(true)),
            }
        }
    }

    impl AudioBackend for FakeBackend {
        fn microphone_in_use(&self, _: Option<&str>, _: Option<&str>) -> Result<bool, AudioError> {
            self.microphone_in_use
                .lock()
                .unwrap()
                .clone()
                .map_err(AudioError::Backend)
        }

        fn stop_activity_monitor(&self) {}

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
            if *self.fail_microphone.lock().unwrap() {
                return Err(AudioError::Backend("device is in use".into()));
            }
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
    fn idle_selection_is_remembered_and_capture_follows_consumer_activity() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        *backend.microphone_in_use.lock().unwrap() = Ok(false);
        let mut controller = RoutingController::new(backend, store.clone(), config);

        controller.start().unwrap();
        assert_eq!(controller.state().mic_status, RouteStatus::Idle);
        assert_eq!(
            *controller.backend().calls.lock().unwrap(),
            ["mirror:primary->secondary"]
        );
        controller.backend().calls.lock().unwrap().clear();

        controller.toggle_mic().unwrap();
        assert_eq!(
            store.load_or_default().unwrap().active_mic,
            ActiveMic::Secondary
        );
        assert_eq!(controller.state().mic_status, RouteStatus::Idle);
        assert_eq!(
            *controller.backend().calls.lock().unwrap(),
            ["cue:Secondary"]
        );
        controller.backend().calls.lock().unwrap().clear();

        *controller.backend().microphone_in_use.lock().unwrap() = Ok(true);
        controller.refresh_endpoints().unwrap();
        controller.refresh_endpoints().unwrap();
        assert_eq!(
            *controller.backend().calls.lock().unwrap(),
            ["mic:Secondary:secondary-mic->virtual-mic"]
        );
        controller.backend().calls.lock().unwrap().clear();

        *controller.backend().microphone_in_use.lock().unwrap() = Ok(false);
        controller.refresh_endpoints().unwrap();
        controller.refresh_endpoints().unwrap();
        assert_eq!(*controller.backend().calls.lock().unwrap(), ["stop-mic"]);
        assert_eq!(controller.state().mic_status, RouteStatus::Idle);
        assert_eq!(controller.state().mirror_status, RouteStatus::Routing);
    }

    #[test]
    fn activity_failure_releases_capture_and_recovers_when_detection_returns() {
        let directory = tempfile::tempdir().unwrap();
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let mut controller = RoutingController::new(
            backend,
            ConfigStore::at(directory.path().join("settings.json")),
            config,
        );
        controller.start().unwrap();
        controller.backend().calls.lock().unwrap().clear();

        *controller.backend().microphone_in_use.lock().unwrap() = Err("cable was unplugged".into());
        controller.refresh_endpoints().unwrap();
        assert_eq!(*controller.backend().calls.lock().unwrap(), ["stop-mic"]);
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Error { .. }
        ));
        assert_eq!(controller.state().mirror_status, RouteStatus::Routing);

        controller.toggle_mic().unwrap();
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Error { .. }
        ));
        *controller.backend().microphone_in_use.lock().unwrap() = Ok(false);
        controller.refresh_endpoints().unwrap();
        assert_eq!(controller.state().mic_status, RouteStatus::Idle);

        *controller.backend().microphone_in_use.lock().unwrap() = Ok(true);
        controller.refresh_endpoints().unwrap();
        assert_eq!(controller.state().mic_status, RouteStatus::Routing);
    }

    #[test]
    fn settings_changes_and_device_reconnection_do_not_open_an_idle_microphone() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        *backend.microphone_in_use.lock().unwrap() = Ok(false);
        let mut controller = RoutingController::new(
            backend,
            ConfigStore::at(directory.path().join("settings.json")),
            config.clone(),
        );
        controller.start().unwrap();
        controller.backend().calls.lock().unwrap().clear();

        config.labels.primary = "Desk".into();
        controller.apply_config(config).unwrap();
        controller
            .backend()
            .endpoints
            .lock()
            .unwrap()
            .iter_mut()
            .find(|endpoint| endpoint.id == "primary-mic")
            .unwrap()
            .available = false;
        controller.refresh_endpoints().unwrap();
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Waiting { .. }
        ));
        controller
            .backend()
            .endpoints
            .lock()
            .unwrap()
            .iter_mut()
            .find(|endpoint| endpoint.id == "primary-mic")
            .unwrap()
            .available = true;
        controller.refresh_endpoints().unwrap();
        assert_eq!(controller.state().mic_status, RouteStatus::Idle);
        assert!(controller.backend().calls.lock().unwrap().is_empty());
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

    #[test]
    fn applying_non_route_settings_preserves_live_audio_clients() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let mut controller = RoutingController::new(backend, store, config.clone());
        controller.start().unwrap();
        controller.backend().calls.lock().unwrap().clear();

        let mut updated = config;
        updated.labels.primary = "Studio".into();
        updated.audio.cue_enabled = false;
        controller.apply_config(updated).unwrap();

        assert!(controller.backend().calls.lock().unwrap().is_empty());
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Routing
        ));
        assert!(matches!(
            controller.state().mirror_status,
            RouteStatus::Routing
        ));
    }

    #[test]
    fn changing_playback_restarts_only_the_mirror() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let mut controller = RoutingController::new(backend, store, config.clone());
        controller.start().unwrap();
        controller.backend().calls.lock().unwrap().clear();

        let mut updated = config;
        updated.endpoints.secondary_render = Some("new-secondary".into());
        controller
            .backend()
            .endpoints
            .lock()
            .unwrap()
            .push(Endpoint {
                id: "new-secondary".into(),
                name: "new-secondary".into(),
                flow: EndpointFlow::Render,
                available: true,
                system_default: false,
                device_id: None,
            });
        controller.apply_config(updated).unwrap();

        assert_eq!(
            *controller.backend().calls.lock().unwrap(),
            ["stop-mirror", "mirror:primary->new-secondary"]
        );
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Routing
        ));
    }

    #[test]
    fn changing_inactive_microphone_preserves_the_live_route() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let mut controller = RoutingController::new(backend, store, config.clone());
        controller.start().unwrap();
        controller.backend().calls.lock().unwrap().clear();

        let mut updated = config;
        updated.endpoints.secondary_capture = Some("new-secondary-mic".into());
        controller.apply_config(updated).unwrap();

        assert!(controller.backend().calls.lock().unwrap().is_empty());
        assert!(matches!(
            controller.state().mic_status,
            RouteStatus::Routing
        ));
    }

    #[test]
    fn failed_route_retry_deadline_is_not_postponed_by_endpoint_polls() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let mut controller = RoutingController::new(backend, store, config);
        controller.start().unwrap();
        *controller.backend().fail_microphone.lock().unwrap() = true;
        controller.machine.mic_failed("device is in use");
        controller.next_error_retry = Instant::now() - Duration::from_secs(1);
        controller.backend().calls.lock().unwrap().clear();

        controller.refresh_endpoints().unwrap();
        controller.refresh_endpoints().unwrap();

        let calls = controller.backend().calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|call| call.starts_with("mic:")).count(),
            1
        );
        assert!(controller.next_error_retry > Instant::now());
    }

    #[test]
    fn undocked_laptop_pauses_the_mirror_and_reports_the_system_devices() {
        let directory = tempfile::tempdir().unwrap();
        let config = configured();
        let backend = FakeBackend::with_configured_endpoints(&config);
        let laptop = |id: &str, flow| Endpoint {
            id: id.into(),
            name: format!("Laptop {id}"),
            flow,
            available: true,
            system_default: true,
            device_id: Some(id.into()),
        };
        {
            let mut endpoints = backend.endpoints.lock().unwrap();
            // Only the virtual cable and the built-in devices remain.
            endpoints
                .retain(|endpoint| Some(&endpoint.id) == config.endpoints.virtual_render.as_ref());
            endpoints.push(laptop("speakers", EndpointFlow::Render));
            endpoints.push(laptop("microphone", EndpointFlow::Capture));
        }
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let mut controller = RoutingController::new(backend, store, config);

        controller.start().unwrap();

        let state = controller.state();
        assert!(matches!(state.mirror_status, RouteStatus::Waiting { .. }));
        assert!(matches!(state.mic_status, RouteStatus::Waiting { .. }));
        assert_eq!(
            state.system_audio,
            SystemAudio {
                other_output: Some("Laptop speakers".into()),
                other_input: Some("Laptop microphone".into()),
            }
        );
    }
}
