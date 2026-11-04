# Contributing

Audio Switch has a Windows tray app and an in-development macOS menu bar app.
Both use the shared Rust core in `crates/audio-switch-core`. Windows development
requires the stable Rust MSVC toolchain and Visual Studio Build Tools for C++
desktop development. macOS development requires Xcode 16 or newer and Rust from
rustup.

Before opening a pull request on Windows, run:

```powershell
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

On macOS, run the same checks for the crates that build there, as listed in the
README.

Keep changes focused. Update current documentation when behavior changes and add
tests for routing state, configuration compatibility, or IPC behavior where a
regression would be hard to spot manually.

The microphone activity integration tests use an installed virtual cable. Close
applications recording from that cable before running them. They inspect its
endpoint pairing and briefly open recording streams without saving audio:

```powershell
$env:AUDIO_SWITCH_TEST_VIRTUAL_RENDER = (Get-Content "$env:LOCALAPPDATA\AudioSwitch\settings.json" -Raw | ConvertFrom-Json).endpoints.virtualRender
cargo test --locked -p audio-switch-core native_c -- --ignored --nocapture --test-threads=1
```

For a headset check, close the installed tray app before launching a development
build. Select the headset, start recording from the virtual cable, then stop.
Confirm sidetone follows recording, the selected mic stays remembered while idle,
and the beginning of a recording has acceptable startup latency.

On macOS, `cargo run -p audio-switch-core --example route_spike -- demand <cable-uid> [seconds] [capture-uid]`
prints recording demand on the cable as it changes. Record from the cable with
another app, for example `ffmpeg -f avfoundation -i ":BlackHole 2ch"`, to see it
turn true and back to false.

Protocol 1 reports an idle microphone as `routing` so older Stream Deck
controllers still recognize it as ready. The tray's route status distinguishes
idle capture from a running stream.

Preserve these project boundaries:

- Routing uses configurable Primary and Secondary roles, not hard-coded device
  names.
- Audio Switch does not bundle or download a third-party virtual audio driver.
- The Stream Deck plugin controls the running app and never launches its own copy.
- Saved routes do not silently fall back when a configured device disconnects.

Do not commit generated files from `target\`, `dist\`, or plugin packaging. If a
change affects setup, Settings, the tray or menu bar menu, or Stream Deck
feedback, describe the manual check you performed on each affected platform in
the pull request.

Bug reports may include logs or settings, but those files can contain audio
device names and endpoint IDs. Remove private device information before posting
them publicly.

By contributing, you agree that your contribution is licensed under the MIT
License.
