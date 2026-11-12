//! Drives the platform audio backend directly, for checking routes on real
//! hardware without the app.
//!
//! ```text
//! cargo run -p audio-switch-core --example route_spike -- list
//! cargo run -p audio-switch-core --example route_spike -- mirror <primary-id> <secondary-id> [seconds]
//! cargo run -p audio-switch-core --example route_spike -- mic <capture-id> <virtual-cable-id> [seconds]
//! cargo run -p audio-switch-core --example route_spike -- switch <capture-a> <capture-b> <virtual-cable-id>
//! cargo run -p audio-switch-core --example route_spike -- cue <render-id>
//! cargo run -p audio-switch-core --example route_spike -- demand <virtual-cable-id> [seconds] [capture-id]
//! ```
//!
//! `demand` prints whether another application is recording from the cable
//! each time it changes. With a capture ID it also routes that microphone into
//! the cable, to show this process's own render does not count as demand.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use audio_switch_core::ActiveMic;
use audio_switch_core::audio::{AudioBackend, CueKind, RouteDiagnostics};

#[cfg(target_os = "macos")]
fn backend() -> audio_switch_core::audio::CoreAudioBackend {
    audio_switch_core::audio::CoreAudioBackend::new(30, 25)
}

#[cfg(windows)]
fn backend() -> audio_switch_core::audio::WasapiBackend {
    audio_switch_core::audio::WasapiBackend::new(48_000, 30, 25)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let backend = backend();
    let seconds = |index: usize| -> Result<u64, Box<dyn std::error::Error>> {
        Ok(args.get(index).map_or(Ok(10), |value| value.parse())?)
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] => {
            for endpoint in backend.enumerate_endpoints()? {
                println!(
                    "{:?}\t{}{}\t{}\t{}",
                    endpoint.flow,
                    if endpoint.available {
                        "active"
                    } else {
                        "absent"
                    },
                    if endpoint.system_default {
                        ", default"
                    } else {
                        ""
                    },
                    endpoint.name,
                    endpoint.id
                );
            }
        }
        ["mirror", source, target, ..] => {
            backend.start_mirror(source, target)?;
            report(seconds(3)?, || backend.mirror_diagnostics());
            backend.stop_mirror()?;
        }
        ["mic", source, sink, ..] => {
            backend.start_microphone(source, ActiveMic::Primary, sink)?;
            report(seconds(3)?, || backend.microphone_diagnostics());
            backend.stop_microphone()?;
        }
        ["switch", first, second, sink] => {
            backend.start_microphone(first, ActiveMic::Primary, sink)?;
            report(2, || backend.microphone_diagnostics());
            let switch_started = Instant::now();
            backend.start_microphone(second, ActiveMic::Secondary, sink)?;
            println!("switched in {:?}", switch_started.elapsed());
            report(2, || backend.microphone_diagnostics());
            backend.stop_microphone()?;
        }
        ["cue", target] => {
            backend.play_cue(target, CueKind::Primary, -18.0)?;
            thread::sleep(Duration::from_millis(300));
            backend.play_cue(target, CueKind::Secondary, -18.0)?;
        }
        ["demand", cable, rest @ ..] => {
            let started = Instant::now();
            let (wake, woken) = mpsc::channel();
            backend.set_activity_callback(move || {
                let _ = wake.send(());
            });
            thread::spawn(move || {
                while woken.recv().is_ok() {
                    println!("{:>6.2}s  woken", started.elapsed().as_secs_f32());
                }
            });
            if let [_, capture, ..] = rest {
                backend.start_microphone(capture, ActiveMic::Primary, cable)?;
                println!("routing {capture} into {cable}");
            }
            let mut last = None;
            while started.elapsed() < Duration::from_secs(seconds(2)?) {
                let in_use = backend
                    .microphone_in_use(Some(cable), None)
                    .map_err(|error| error.to_string());
                if last.as_ref() != Some(&in_use) {
                    println!(
                        "{:>6.2}s  in use: {in_use:?}",
                        started.elapsed().as_secs_f32()
                    );
                    last = Some(in_use);
                }
                thread::sleep(Duration::from_millis(100));
            }
            backend.stop_activity_monitor();
            backend.stop_microphone()?;
        }
        _ => {
            return Err(
                "usage: list | mirror <src> <dst> [s] | mic <src> <sink> [s] | switch <a> <b> <sink> | cue <dst> | demand <cable> [s] [capture]".into(),
            );
        }
    }
    Ok(())
}

fn report(seconds: u64, diagnostics: impl Fn() -> Option<RouteDiagnostics>) {
    for _ in 0..seconds {
        thread::sleep(Duration::from_secs(1));
        if let Some(diagnostics) = diagnostics() {
            println!("{diagnostics:?}");
        }
    }
}
