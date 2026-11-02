#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result};
use std::thread;
use std::time::Duration;

use audio_switch_core::audio::{AudioBackend, EndpointFlow, WasapiBackend};
use audio_switch_core::control;
use audio_switch_core::ipc::ControlCommand;
use audio_switch_core::{ActiveMic, AppConfig, ConfigStore};

mod autostart;
mod settings;
mod setup;
mod tray;
mod ui;
mod updates;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(error) = run(&args) {
        if uses_interactive_error_dialog(&args) {
            ui::show_fatal_error(&error);
        } else {
            eprintln!("{error:#}");
        }
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--elevated-autostart") {
        return autostart::launch_elevated(
            &std::env::current_exe().context("locating the Audio Switch executable")?,
        );
    }
    if args.iter().any(|arg| arg == "--high-priority") {
        autostart::set_high_process_priority()?;
    }
    if args.iter().any(|arg| arg == "--remove-autostart") {
        #[cfg(not(debug_assertions))]
        autostart::reconcile(
            false,
            false,
            &std::env::current_exe().context("locating the Audio Switch executable")?,
        )?;
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--list-devices") {
        return list_devices();
    }
    if let Some(index) = args.iter().position(|arg| arg == "--spike-seconds") {
        let seconds = args
            .get(index + 1)
            .context("--spike-seconds requires a duration")?
            .parse::<u64>()
            .context("spike duration must be a positive integer")?;
        return run_hardware_spike(seconds);
    }
    if let Some(index) = args.iter().position(|arg| arg == "--set-active") {
        let active = match args.get(index + 1).map(String::as_str) {
            Some("primary") => ActiveMic::Primary,
            Some("secondary") => ActiveMic::Secondary,
            _ => anyhow::bail!("--set-active expects 'primary' or 'secondary'"),
        };
        return set_active_mic(active);
    }
    if let Some(index) = args.iter().position(|arg| arg == "--control") {
        let command = match args.get(index + 1).map(String::as_str) {
            Some("state") => ControlCommand::GetState { request_id: None },
            Some("toggle") => ControlCommand::ToggleMic { request_id: None },
            Some("primary") => ControlCommand::SetActiveMic {
                request_id: None,
                active_mic: ActiveMic::Primary,
            },
            Some("secondary") => ControlCommand::SetActiveMic {
                request_id: None,
                active_mic: ActiveMic::Secondary,
            },
            _ => anyhow::bail!("--control expects state, toggle, primary, or secondary"),
        };
        let response = control::send_command(&command)?;
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    run_tray_role(
        args.iter().any(|argument| argument == "--settings"),
        args.iter().any(|argument| argument == "--setup"),
    )
}

fn uses_interactive_error_dialog(args: &[String]) -> bool {
    if args.iter().any(|arg| arg == "--elevated-autostart") {
        return true;
    }
    !args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--remove-autostart"
                | "--list-devices"
                | "--spike-seconds"
                | "--set-active"
                | "--control"
        )
    })
}

fn run_tray_role(open_settings: bool, force_setup: bool) -> Result<()> {
    let Some(instance) = tray::acquire_single_instance()? else {
        return Ok(());
    };
    let store = ConfigStore::for_current_user().context("locating user configuration")?;
    let first_run = !store.exists();
    let mut config = store.load_or_default().context("loading configuration")?;
    let mut endpoints = enumerate_endpoints().context("enumerating setup endpoints")?;
    let mut virtual_cable_correction =
        startup_virtual_cable_correction(force_setup, &mut config, &endpoints);

    let mut repair_missing_cable = false;
    let retry_virtual_cable =
        should_retry_virtual_cable(&config, &endpoints, virtual_cable_correction.is_some());
    if !force_setup && !first_run && retry_virtual_cable {
        // Give slow virtual-cable drivers a moment to register at sign-in before prompting.
        std::thread::sleep(std::time::Duration::from_secs(3));
        endpoints = enumerate_endpoints().context("re-enumerating setup endpoints")?;
        if virtual_cable_correction.is_none() {
            virtual_cable_correction =
                startup_virtual_cable_correction(false, &mut config, &endpoints);
        }
        repair_missing_cable = setup::virtual_cable_missing(&config, &endpoints)
            && setup::prompt_for_missing_virtual_cable();
    }

    if force_setup || first_run || repair_missing_cable {
        let Some(updated) = setup::run(config.clone(), endpoints, enumerate_endpoints)? else {
            if first_run {
                return Ok(());
            }
            return continue_tray(
                config,
                store,
                open_settings,
                virtual_cable_correction,
                instance,
            );
        };
        store.save(&updated).context("saving initial setup")?;
        config = updated;
        virtual_cable_correction = None;
    }

    continue_tray(
        config,
        store,
        open_settings,
        virtual_cable_correction,
        instance,
    )
}

fn startup_virtual_cable_correction(
    force_setup: bool,
    config: &mut AppConfig,
    endpoints: &[audio_switch_core::audio::Endpoint],
) -> Option<setup::VirtualCableCorrection> {
    (!force_setup)
        .then(|| setup::repair_legacy_16_channel_selection(config, endpoints))
        .flatten()
}

fn should_retry_virtual_cable(
    config: &AppConfig,
    endpoints: &[audio_switch_core::audio::Endpoint],
    already_corrected: bool,
) -> bool {
    setup::virtual_cable_missing(config, endpoints)
        || (!already_corrected && setup::legacy_16_channel_is_selected(config, endpoints))
}

fn continue_tray(
    config: AppConfig,
    store: ConfigStore,
    open_settings: bool,
    virtual_cable_correction: Option<setup::VirtualCableCorrection>,
    instance: tray::SingleInstanceGuard,
) -> Result<()> {
    let updater = match updates::UpdateController::start(config.update_policy, store.path())? {
        updates::StartOutcome::Installing => return Ok(()),
        updates::StartOutcome::Ready(updater) => updater,
    };
    let startup_diagnostics = tray::StartupDiagnostics {
        approval: config.auto_start.then(autostart::disabled_by_windows),
        virtual_cable_correction,
    };
    let backend = backend_from_config(&config);
    #[cfg(not(debug_assertions))]
    if std::env::var_os("AUDIO_SWITCH_SKIP_AUTOSTART").is_none() {
        autostart::reconcile(
            config.auto_start,
            config.auto_start_elevated,
            &std::env::current_exe().context("locating the Audio Switch executable")?,
        )?;
    }
    tray::run(
        backend,
        store,
        config,
        updater,
        startup_diagnostics,
        open_settings,
        instance,
    )
}

fn enumerate_endpoints() -> Result<Vec<audio_switch_core::audio::Endpoint>> {
    default_backend()
        .enumerate_endpoints()
        .context("enumerating WASAPI endpoints")
}

fn list_devices() -> Result<()> {
    let backend = default_backend();
    let endpoints = backend
        .enumerate_endpoints()
        .context("enumerating WASAPI endpoints")?;
    for endpoint in endpoints {
        let flow = match endpoint.flow {
            EndpointFlow::Render => "render ",
            EndpointFlow::Capture => "capture",
        };
        println!("{flow}\t{}\t{}", endpoint.name, endpoint.id);
    }
    Ok(())
}

fn run_hardware_spike(seconds: u64) -> Result<()> {
    let store = ConfigStore::for_current_user().context("locating user configuration")?;
    let config = store.load_or_default().context("loading configuration")?;
    let backend = backend_from_config(&config);
    let primary = required_id(&config.endpoints.primary_render, "primary render")?;
    let secondary = required_id(&config.endpoints.secondary_render, "secondary render")?;
    let virtual_render = required_id(&config.endpoints.virtual_render, "virtual mic render")?;
    let microphone = match config.active_mic {
        ActiveMic::Primary => required_id(&config.endpoints.primary_capture, "primary capture")?,
        ActiveMic::Secondary => {
            required_id(&config.endpoints.secondary_capture, "secondary capture")?
        }
    };

    backend
        .start_mirror(primary, secondary)
        .context("starting primary-to-secondary playback mirror")?;
    if let Err(error) = backend.start_microphone(microphone, config.active_mic, virtual_render) {
        let _ = backend.stop_mirror();
        return Err(error).context("starting microphone-to-virtual-device route");
    }

    println!(
        "Routing {:?} microphone and mirrored playback for {seconds} seconds.",
        config.active_mic
    );
    for elapsed in 1..=seconds {
        thread::sleep(Duration::from_secs(1));
        let mirror = backend.mirror_diagnostics().unwrap_or_default();
        let mic = backend.microphone_diagnostics().unwrap_or_default();
        println!(
            "{elapsed:>3}s mirror cap={} out={} under={} over={} disc={} | mic cap={} out={} under={} over={} disc={}",
            mirror.captured_frames,
            mirror.rendered_frames,
            mirror.underrun_frames,
            mirror.overrun_frames,
            mirror.discontinuities,
            mic.captured_frames,
            mic.rendered_frames,
            mic.underrun_frames,
            mic.overrun_frames,
            mic.discontinuities,
        );
    }
    backend
        .stop_microphone()
        .context("stopping microphone route")?;
    backend.stop_mirror().context("stopping mirror route")?;
    Ok(())
}

fn set_active_mic(active: ActiveMic) -> Result<()> {
    let store = ConfigStore::for_current_user().context("locating user configuration")?;
    let mut config = store.load_or_default().context("loading configuration")?;
    config.active_mic = active;
    store.save(&config).context("saving active microphone")?;
    println!("Selected {active:?} microphone.");
    Ok(())
}

fn default_backend() -> WasapiBackend {
    WasapiBackend::new(48_000, 30, 15)
}

fn backend_from_config(config: &AppConfig) -> WasapiBackend {
    WasapiBackend::new(
        config.audio.sample_rate,
        config.audio.mirror_buffer_ms,
        config.audio.mic_buffer_ms,
    )
}

fn required_id<'a>(id: &'a Option<String>, label: &str) -> Result<&'a str> {
    id.as_deref()
        .with_context(|| format!("{label} endpoint has not been configured"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_switch_core::audio::{Endpoint, EndpointFlow};

    fn args(values: &[&str]) -> Vec<String> {
        std::iter::once("audio-switch.exe".to_owned())
            .chain(values.iter().map(|value| (*value).to_owned()))
            .collect()
    }

    #[test]
    fn tray_modes_show_fatal_errors_interactively() {
        assert!(uses_interactive_error_dialog(&args(&[])));
        assert!(uses_interactive_error_dialog(&args(&["--tray"])));
        assert!(uses_interactive_error_dialog(&args(&["--setup"])));
        assert!(uses_interactive_error_dialog(&args(&[
            "--elevated-autostart"
        ])));
    }

    #[test]
    fn maintenance_and_control_modes_never_open_error_dialogs() {
        for mode in [
            "--remove-autostart",
            "--list-devices",
            "--spike-seconds",
            "--set-active",
            "--control",
        ] {
            assert!(!uses_interactive_error_dialog(&args(&[mode])));
        }
    }

    #[test]
    fn forced_setup_does_not_apply_an_automatic_cable_repair() {
        let mut config = AppConfig::default();
        config.endpoints.virtual_render = Some("cable-16ch".into());
        let endpoints = [
            Endpoint {
                id: "cable-16ch".into(),
                name: "CABLE In 16ch (VB-Audio Virtual Cable)".into(),
                flow: EndpointFlow::Render,
                available: true,
                system_default: false,
                device_id: None,
            },
            Endpoint {
                id: "cable-input".into(),
                name: "CABLE Input (VB-Audio Virtual Cable)".into(),
                flow: EndpointFlow::Render,
                available: true,
                system_default: false,
                device_id: None,
            },
        ];

        let correction = startup_virtual_cable_correction(true, &mut config, &endpoints);

        assert_eq!(correction, None);
        assert_eq!(
            config.endpoints.virtual_render.as_deref(),
            Some("cable-16ch")
        );
    }

    #[test]
    fn delayed_endpoint_enumeration_can_still_trigger_the_cable_repair() {
        let mut config = AppConfig::default();
        config.endpoints.virtual_render = Some("cable-16ch".into());
        let initial = [Endpoint {
            id: "cable-16ch".into(),
            name: "CABLE In 16ch (VB-Audio Virtual Cable)".into(),
            flow: EndpointFlow::Render,
            available: true,
            system_default: false,
            device_id: None,
        }];
        let refreshed = [
            initial[0].clone(),
            Endpoint {
                id: "cable-input".into(),
                name: "CABLE Input (VB-Audio Virtual Cable)".into(),
                flow: EndpointFlow::Render,
                available: true,
                system_default: false,
                device_id: None,
            },
        ];

        let initial_correction = startup_virtual_cable_correction(false, &mut config, &initial);
        assert_eq!(initial_correction, None);
        assert!(should_retry_virtual_cable(
            &config,
            &initial,
            initial_correction.is_some()
        ));
        assert!(startup_virtual_cable_correction(false, &mut config, &refreshed).is_some());
        assert_eq!(
            config.endpoints.virtual_render.as_deref(),
            Some("cable-input")
        );
    }
}
