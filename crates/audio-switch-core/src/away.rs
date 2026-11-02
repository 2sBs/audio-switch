//! Describes, in words shared by the Windows tray and the macOS menu, how the
//! connected devices differ from the configured setup.

use crate::config::AppConfig;
use crate::state::{AppState, EndpointAvailability, RouteStatus};

const SYSTEM: &str = if cfg!(target_os = "macos") {
    "macOS"
} else {
    "Windows"
};

/// One sentence per line, or nothing while every configured device is
/// connected and the system uses them. Faults such as a missing virtual cable
/// are route errors instead, so they are not described here.
pub fn away_summary(
    config: &AppConfig,
    state: &AppState,
    availability: EndpointAvailability,
) -> Vec<String> {
    let endpoints = &config.endpoints;
    let primary = MissingRole {
        label: &config.labels.primary,
        mic: endpoints.primary_capture.is_some() && !availability.primary_mic,
        playback: endpoints.primary_render.is_some() && !availability.primary_render,
    };
    let secondary = MissingRole {
        label: &config.labels.secondary,
        mic: endpoints.secondary_capture.is_some() && !availability.secondary_mic,
        playback: endpoints.secondary_render.is_some() && !availability.secondary_render,
    };
    let system = describe_system(
        state.system_audio.other_output.as_deref(),
        state.system_audio.other_input.as_deref(),
    );
    let missing = describe_missing(primary, secondary);
    if missing.is_none() && system.is_none() {
        return Vec::new();
    }

    let mirror_paused = matches!(state.mirror_status, RouteStatus::Waiting { .. })
        .then(|| "Mirroring is paused.".to_owned());
    [missing, mirror_paused, system]
        .into_iter()
        .flatten()
        .collect()
}

#[derive(Clone, Copy)]
struct MissingRole<'a> {
    label: &'a str,
    mic: bool,
    playback: bool,
}

fn describe_missing(primary: MissingRole, secondary: MissingRole) -> Option<String> {
    let all_missing = |role: MissingRole| role.mic && role.playback;
    if all_missing(primary) && all_missing(secondary) {
        return Some(format!(
            "{} and {} devices aren't connected.",
            primary.label, secondary.label
        ));
    }
    let parts: Vec<String> = [primary, secondary]
        .into_iter()
        .filter_map(|role| match (role.mic, role.playback) {
            (true, true) => Some(format!("{} devices", role.label)),
            (true, false) => Some(format!("{} microphone", role.label)),
            (false, true) => Some(format!("{} playback", role.label)),
            (false, false) => None,
        })
        .collect();
    let verb = match parts.as_slice() {
        [] => return None,
        [only] if !only.ends_with("devices") => "isn't",
        _ => "aren't",
    };
    Some(format!("{} {verb} connected.", parts.join(" and ")))
}

fn describe_system(output: Option<&str>, input: Option<&str>) -> Option<String> {
    Some(match (output, input) {
        (Some(output), Some(input)) if output == input => format!("{SYSTEM} is using {output}."),
        (Some(output), Some(input)) => format!("{SYSTEM} is using {output} and {input}."),
        (Some(output), None) => format!("{SYSTEM} is playing through {output}."),
        (None, Some(input)) => format!("{SYSTEM} is recording from {input}."),
        (None, None) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ActiveMic, SystemAudio};

    fn configured() -> AppConfig {
        let mut config = AppConfig::default();
        config.labels.primary = "Desk".into();
        config.labels.secondary = "Headset".into();
        let endpoints = &mut config.endpoints;
        for id in [
            &mut endpoints.primary_capture,
            &mut endpoints.secondary_capture,
            &mut endpoints.primary_render,
            &mut endpoints.secondary_render,
            &mut endpoints.virtual_render,
        ] {
            *id = Some("configured".into());
        }
        config
    }

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
    fn nothing_to_say_at_the_configured_setup() {
        let state = AppState::new(ActiveMic::Primary);

        assert!(away_summary(&configured(), &state, all_available()).is_empty());
    }

    #[test]
    fn undocked_laptop_names_the_missing_roles_and_the_system_devices() {
        let mut state = AppState::new(ActiveMic::Primary);
        state.mirror_status = RouteStatus::Waiting {
            reason: "waiting".into(),
        };
        state.system_audio = SystemAudio {
            other_output: Some("Speakers".into()),
            other_input: Some("Built-in Microphone".into()),
        };
        let undocked = EndpointAvailability {
            virtual_mic: true,
            ..EndpointAvailability::default()
        };

        assert_eq!(
            away_summary(&configured(), &state, undocked),
            [
                "Desk and Headset devices aren't connected.".to_owned(),
                "Mirroring is paused.".to_owned(),
                format!("{SYSTEM} is using Speakers and Built-in Microphone."),
            ]
        );
    }

    #[test]
    fn earbuds_on_the_desk_name_one_device_once() {
        let mut state = AppState::new(ActiveMic::Primary);
        state.mirror_status = RouteStatus::Waiting {
            reason: "waiting".into(),
        };
        state.system_audio = SystemAudio {
            other_output: Some("Earbuds".into()),
            other_input: Some("Earbuds".into()),
        };

        assert_eq!(
            away_summary(&configured(), &state, all_available()),
            [
                "Mirroring is paused.".to_owned(),
                format!("{SYSTEM} is using Earbuds.")
            ]
        );
    }

    #[test]
    fn unconfigured_roles_are_not_reported_missing() {
        let mut config = configured();
        config.endpoints.secondary_render = None;
        let state = AppState::new(ActiveMic::Primary);
        let availability = EndpointAvailability {
            primary_mic: false,
            secondary_render: false,
            ..all_available()
        };

        assert_eq!(
            away_summary(&config, &state, availability),
            ["Desk microphone isn't connected."]
        );
    }
}
