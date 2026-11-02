use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ActiveMic {
    #[default]
    Primary,
    Secondary,
}

impl ActiveMic {
    pub const fn toggled(self) -> Self {
        match self {
            Self::Primary => Self::Secondary,
            Self::Secondary => Self::Primary,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RouteStatus {
    Stopped,
    Starting,
    Routing,
    Waiting { reason: String },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AppState {
    pub protocol: u32,
    pub revision: u64,
    pub active_mic: ActiveMic,
    pub mic_status: RouteStatus,
    pub mirror_status: RouteStatus,
    #[serde(default)]
    pub system_audio: SystemAudio,
}

/// The system's own default devices, where they differ from the configured
/// roles. Being away from the configured devices is normal (an undocked laptop,
/// Bluetooth earbuds), so this is informational: the mirror pauses while the
/// system output is elsewhere, and the system input never changes routing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SystemAudio {
    /// Name of the system output when it is not the Primary playback device.
    pub other_output: Option<String>,
    /// Name of the system input when it is not the virtual cable.
    pub other_input: Option<String>,
}

impl AppState {
    pub fn new(active_mic: ActiveMic) -> Self {
        Self {
            protocol: 1,
            revision: 0,
            active_mic,
            mic_status: RouteStatus::Stopped,
            mirror_status: RouteStatus::Stopped,
            system_audio: SystemAudio::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct EndpointAvailability {
    pub primary_mic: bool,
    pub secondary_mic: bool,
    pub virtual_mic: bool,
    pub primary_render: bool,
    pub secondary_render: bool,
    /// The Primary playback device is the system output, so there is
    /// something worth mirroring.
    pub primary_render_is_system_output: bool,
}

impl EndpointAvailability {
    fn mirror_ready(self) -> bool {
        self.primary_render && self.secondary_render && self.primary_render_is_system_output
    }

    fn mic_available(self, active: ActiveMic) -> bool {
        match active {
            ActiveMic::Primary => self.primary_mic,
            ActiveMic::Secondary => self.secondary_mic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesiredTransition {
    None,
    StartMic(ActiveMic),
    StopMic,
    StartMirror,
    StopMirror,
}

#[derive(Debug)]
pub struct RoutingStateMachine {
    state: AppState,
    availability: EndpointAvailability,
}

impl RoutingStateMachine {
    pub fn new(active_mic: ActiveMic, availability: EndpointAvailability) -> Self {
        let mut machine = Self {
            state: AppState::new(active_mic),
            availability,
        };
        machine.reconcile_status();
        machine
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn availability(&self) -> EndpointAvailability {
        self.availability
    }

    pub fn update_system_audio(&mut self, system_audio: SystemAudio) {
        if self.state.system_audio != system_audio {
            self.state.system_audio = system_audio;
            self.bump_revision();
        }
    }

    pub fn select_mic(&mut self, active: ActiveMic) -> Vec<DesiredTransition> {
        if self.state.active_mic == active {
            return Vec::new();
        }
        self.state.active_mic = active;
        self.bump_revision();
        self.reconcile(true)
    }

    pub fn toggle_mic(&mut self) -> Vec<DesiredTransition> {
        self.select_mic(self.state.active_mic.toggled())
    }

    pub fn update_availability(
        &mut self,
        availability: EndpointAvailability,
    ) -> Vec<DesiredTransition> {
        if self.availability == availability {
            return Vec::new();
        }
        self.availability = availability;
        self.bump_revision();
        self.reconcile(false)
    }

    pub fn retry_failed_routes(&mut self) -> Vec<DesiredTransition> {
        if !matches!(self.state.mic_status, RouteStatus::Error { .. })
            && !matches!(self.state.mirror_status, RouteStatus::Error { .. })
        {
            return Vec::new();
        }
        self.reconcile(false)
    }

    pub fn mic_started(&mut self) {
        self.state.mic_status = RouteStatus::Routing;
        self.bump_revision();
    }

    pub fn mirror_started(&mut self) {
        self.state.mirror_status = RouteStatus::Routing;
        self.bump_revision();
    }

    pub fn mic_failed(&mut self, message: impl Into<String>) {
        self.state.mic_status = RouteStatus::Error {
            message: message.into(),
        };
        self.bump_revision();
    }

    pub fn mirror_failed(&mut self, message: impl Into<String>) {
        self.state.mirror_status = RouteStatus::Error {
            message: message.into(),
        };
        self.bump_revision();
    }

    fn reconcile(&mut self, restart_mic: bool) -> Vec<DesiredTransition> {
        let mic_was_routing = matches!(self.state.mic_status, RouteStatus::Routing);
        let mirror_was_routing = matches!(self.state.mirror_status, RouteStatus::Routing);

        self.reconcile_status();

        if mic_was_routing
            && !restart_mic
            && self.availability.virtual_mic
            && self.availability.mic_available(self.state.active_mic)
        {
            self.state.mic_status = RouteStatus::Routing;
        }
        if mirror_was_routing && self.availability.mirror_ready() {
            self.state.mirror_status = RouteStatus::Routing;
        }

        let mut transitions = Vec::with_capacity(4);
        let mic_should_route = matches!(self.state.mic_status, RouteStatus::Starting);
        let mirror_should_route = matches!(self.state.mirror_status, RouteStatus::Starting);

        if mic_was_routing
            && (restart_mic || !matches!(self.state.mic_status, RouteStatus::Routing))
        {
            transitions.push(DesiredTransition::StopMic);
        }
        if mic_should_route {
            transitions.push(DesiredTransition::StartMic(self.state.active_mic));
        }
        if mirror_was_routing && !matches!(self.state.mirror_status, RouteStatus::Routing) {
            transitions.push(DesiredTransition::StopMirror);
        }
        if !mirror_was_routing && mirror_should_route {
            transitions.push(DesiredTransition::StartMirror);
        }
        transitions
    }

    fn reconcile_status(&mut self) {
        self.state.mic_status = if !self.availability.virtual_mic {
            RouteStatus::Error {
                message: "the virtual cable is not connected".into(),
            }
        } else if !self.availability.mic_available(self.state.active_mic) {
            RouteStatus::Waiting {
                reason: match self.state.active_mic {
                    ActiveMic::Primary => "waiting for the primary microphone",
                    ActiveMic::Secondary => "waiting for the secondary microphone",
                }
                .into(),
            }
        } else {
            RouteStatus::Starting
        };

        // A missing playback device is an expected state, such as an undocked
        // laptop, not an error.
        self.state.mirror_status = if !self.availability.primary_render {
            RouteStatus::Waiting {
                reason: "waiting for the primary playback device".into(),
            }
        } else if !self.availability.secondary_render {
            RouteStatus::Waiting {
                reason: "waiting for the secondary playback device".into(),
            }
        } else if !self.availability.primary_render_is_system_output {
            RouteStatus::Waiting {
                reason: "paused while the system plays through another output".into(),
            }
        } else {
            RouteStatus::Starting
        };
    }

    fn bump_revision(&mut self) {
        self.state.revision = self.state.revision.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_available() -> EndpointAvailability {
        EndpointAvailability {
            primary_mic: true,
            secondary_mic: true,
            virtual_mic: true,
            primary_render: true,
            secondary_render: true,
            primary_render_is_system_output: true,
        }
    }

    #[test]
    fn missing_playback_device_waits_instead_of_failing() {
        let mut machine = RoutingStateMachine::new(ActiveMic::Primary, all_available());
        machine.mirror_started();
        let transitions = machine.update_availability(EndpointAvailability {
            primary_render: false,
            primary_render_is_system_output: false,
            ..all_available()
        });

        assert!(matches!(
            machine.state().mirror_status,
            RouteStatus::Waiting { .. }
        ));
        assert!(transitions.contains(&DesiredTransition::StopMirror));
        assert!(machine.retry_failed_routes().is_empty());
    }

    #[test]
    fn mirror_pauses_while_the_system_plays_elsewhere() {
        let mut machine = RoutingStateMachine::new(ActiveMic::Primary, all_available());
        machine.mic_started();
        machine.mirror_started();
        let elsewhere = EndpointAvailability {
            primary_render_is_system_output: false,
            ..all_available()
        };

        assert_eq!(
            machine.update_availability(elsewhere),
            [DesiredTransition::StopMirror]
        );
        assert!(matches!(machine.state().mic_status, RouteStatus::Routing));
        assert_eq!(
            machine.update_availability(all_available()),
            [DesiredTransition::StartMirror]
        );
    }

    #[test]
    fn secondary_disconnect_waits_without_falling_back() {
        let mut machine = RoutingStateMachine::new(ActiveMic::Secondary, all_available());
        machine.mic_started();
        let transitions = machine.update_availability(EndpointAvailability {
            secondary_mic: false,
            ..all_available()
        });

        assert_eq!(machine.state().active_mic, ActiveMic::Secondary);
        assert!(matches!(
            machine.state().mic_status,
            RouteStatus::Waiting { .. }
        ));
        assert!(transitions.contains(&DesiredTransition::StopMic));
        assert!(!transitions.contains(&DesiredTransition::StartMic(ActiveMic::Primary)));
    }

    #[test]
    fn secondary_reconnect_requests_same_mic() {
        let mut unavailable = all_available();
        unavailable.secondary_mic = false;
        let mut machine = RoutingStateMachine::new(ActiveMic::Secondary, unavailable);

        let transitions = machine.update_availability(all_available());
        assert_eq!(
            transitions,
            [
                DesiredTransition::StartMic(ActiveMic::Secondary),
                DesiredTransition::StartMirror
            ]
        );
    }

    #[test]
    fn failed_route_can_be_retried_without_an_availability_change() {
        let mut machine = RoutingStateMachine::new(ActiveMic::Secondary, all_available());
        machine.mic_started();
        machine.mirror_started();
        machine.mic_failed("device is in use");

        assert_eq!(
            machine.retry_failed_routes(),
            [DesiredTransition::StartMic(ActiveMic::Secondary)]
        );
        assert!(matches!(
            machine.state().mirror_status,
            RouteStatus::Routing
        ));
    }

    #[test]
    fn toggling_stops_old_mic_before_starting_new_one() {
        let mut machine = RoutingStateMachine::new(ActiveMic::Primary, all_available());
        machine.mic_started();
        machine.mirror_started();
        let transitions = machine.toggle_mic();
        assert_eq!(
            transitions,
            [
                DesiredTransition::StopMic,
                DesiredTransition::StartMic(ActiveMic::Secondary)
            ]
        );
        assert!(matches!(
            machine.state().mirror_status,
            RouteStatus::Routing
        ));
    }
}
