//! Drives the platform audio backend directly, for checking routes on real
//! hardware without the app.
//!
//! ```text
//! cargo run -p audio-switch-core --example route_spike -- list
//! cargo run -p audio-switch-core --example route_spike -- mirror <primary-id> <secondary-id> [seconds]
//! cargo run -p audio-switch-core --example route_spike -- mic <capture-id> <virtual-cable-id> [seconds]
//! cargo run -p audio-switch-core --example route_spike -- switch <capture-a> <capture-b> <virtual-cable-id>
//! cargo run -p audio-switch-core --example route_spike -- cue <render-id>
//! ```

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
                    "{:?}\t{}\t{}\t{}",
                    endpoint.flow,
                    if endpoint.available {
                        "active"
                    } else {
                        "absent"
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
        _ => {
            return Err(
                "usage: list | mirror <src> <dst> [s] | mic <src> <sink> [s] | switch <a> <b> <sink> | cue <dst>".into(),
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
