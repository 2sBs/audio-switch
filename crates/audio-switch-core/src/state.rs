use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActiveMic {
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
pub enum RouteStatus {
    Stopped,
    Starting,
    Routing,
    Waiting { reason: String },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub protocol: u32,
    pub revision: u64,
    pub active_mic: ActiveMic,
    pub mic_status: RouteStatus,
    pub mirror_status: RouteStatus,
}

impl AppState {
    pub fn new(active_mic: ActiveMic) -> Self {
        Self {
            protocol: 1,
            revision: 0,
            active_mic,
            mic_status: RouteStatus::Stopped,
            mirror_status: RouteStatus::Stopped,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointAvailability {
    pub primary_mic: bool,
    pub secondary_mic: bool,
    pub virtual_mic: bool,
    pub primary_render: bool,
    pub secondary_render: bool,
}

impl EndpointAvailability {
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
        if mirror_was_routing
            && self.availability.primary_render
            && self.availability.secondary_render
        {
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
                message: "virtual microphone render endpoint is unavailable".into(),
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

        self.state.mirror_status = if !self.availability.primary_render {
            RouteStatus::Error {
                message: "primary render endpoint is unavailable".into(),
            }
        } else if !self.availability.secondary_render {
            RouteStatus::Waiting {
                reason: "waiting for the secondary playback endpoint".into(),
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
        }
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
