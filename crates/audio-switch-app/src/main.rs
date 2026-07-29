#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result};
use std::thread;
use std::time::Duration;

use audio_switch_core::audio::{AudioBackend, EndpointFlow, WasapiBackend};
use audio_switch_core::ipc::ControlCommand;
use audio_switch_core::{ActiveMic, AppConfig, ConfigStore};

mod autostart;
mod local_control;
mod settings;
mod stream_deck;
mod tray;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--elevated-autostart") {
        return autostart::launch_elevated(
            &std::env::current_exe().context("locating the Audio Switch executable")?,
        );
    }
    if args.iter().any(|arg| arg == "--high-priority") {
        autostart::set_high_process_priority()?;
    }
    if args.iter().any(|arg| arg == "--stream-deck-plugin") || args.iter().any(|arg| arg == "-port")
    {
        return stream_deck::run(&args);
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
        let response = local_control::send_command(&command)?;
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    run_tray_role(args.iter().any(|argument| argument == "--settings"))
}

fn run_tray_role(open_settings: bool) -> Result<()> {
    let store = ConfigStore::for_current_user().context("locating user configuration")?;
    let config = store.load_or_default().context("loading configuration")?;
    let backend = backend_from_config(&config);
    #[cfg(not(debug_assertions))]
    if std::env::var_os("AUDIO_SWITCH_SKIP_AUTOSTART").is_none() {
        autostart::reconcile(
            config.auto_start,
            config.auto_start_elevated,
            &std::env::current_exe().context("locating the Audio Switch executable")?,
        )?;
    }
    tray::run(backend, store, config, open_settings)
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
