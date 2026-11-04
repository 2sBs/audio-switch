# Audio Switch

[![Build and release](https://github.com/2sBs/audio-switch/actions/workflows/build-release.yml/badge.svg)](https://github.com/2sBs/audio-switch/actions/workflows/build-release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Audio Switch is a Windows tray app and macOS menu bar app for switching
physical microphones without changing the input device selected in games,
calls, or recording software. It also mirrors one playback device to another.

The app uses device-neutral Primary and Secondary roles. You choose the audio
devices and can rename the roles. Audio Switch does not match specific device
names in its routing logic.

```text
Primary playback -> Secondary playback
Selected microphone -> Virtual cable input -> Applications
```

## Requirements

- x64 Windows 10 version 1809 or newer, including Windows 11, or macOS 14.2 or
  newer on Apple silicon
- A virtual audio cable such as [VB-CABLE](https://vb-audio.com/Cable/) on
  Windows or [BlackHole](https://existential.audio/blackhole/) on macOS
- Elgato Stream Deck 7.1 or newer for the optional plugin

Audio Switch does not bundle or download a virtual audio driver. Install one
from its publisher before completing setup.

## Install

### Windows

1. Download the setup executable and `SHA256SUMS.txt` from the
   [latest release](https://github.com/2sBs/audio-switch/releases/latest).
2. Compare the download with the published checksum:

   ```powershell
   Get-FileHash .\audio-switch-*-setup-windows-x86_64.exe -Algorithm SHA256
   ```

3. Run the installer and follow the guided setup.

Release binaries are not code-signed. Windows SmartScreen may warn before
launching them. Check the SHA-256 checksum or build from source if you do not
want to run an unsigned download.

### macOS

1. Download the `macos-arm64.zip` file and `SHA256SUMS.txt` from the
   [latest release](https://github.com/2sBs/audio-switch/releases/latest).
2. Compare the download with the published checksum:

   ```sh
   shasum -a 256 audio-switch-*-macos-arm64.zip
   ```

3. Unzip it, move Audio Switch to Applications, and open it.

The macOS app is ad-hoc signed and not notarized, so macOS blocks the first
launch. Choose **Open Anyway** in System Settings > Privacy & Security, then
open it again. macOS asks for microphone and audio capture access again after
each update. The macOS app does not update itself yet.

### Setup

The setup wizard selects the virtual cable, playback devices, microphones, and
startup behavior. Applications should use the virtual cable's capture endpoint
as their microphone. Audio Switch then changes the physical microphone behind
that stable endpoint.

## Use

On Windows, left-click the tray icon or press `Ctrl+Shift+F11` to switch
microphones. Right-click the icon to choose a route, inspect status, or open
Settings. On macOS, choose a microphone from the menu bar menu or press `⌃⇧F11`.

The selected physical microphone opens when an application starts recording
from the virtual cable. When the last recording stream stops, Audio Switch
releases the microphone and reports it as idle. This lets headsets turn off
automatic sidetone while unused. The selection and playback mirror remain
active, and switching microphones still plays its cue while idle.

Applications that keep recording while muted or waiting for push-to-talk keep
the microphone active. Capture must reopen when recording begins, so the start
of a recording can contain a brief gap while the physical microphone starts.

Audio Switch preserves the selected route if the Secondary device disconnects.
It waits for that device and resumes the route when it returns instead of
silently falling back to Primary.

Being away from your setup is normal. When a configured device is missing, or
the system uses other devices such as laptop speakers or Bluetooth earbuds, the
icon changes to a neutral away state and the menu says what differs. Mirroring
pauses until the system output is the Primary playback device again. The
warning icon is reserved for faults, such as a missing virtual cable.

Only one copy of the app can run per user session. The optional Stream Deck
plugin controls that running app over a session-local named pipe on Windows, or
a per-user Unix socket on macOS. One plugin package runs on both platforms. It
does not include or launch the app. If the app is unavailable, the key displays
`START AUDIO SWITCH`.

On Windows, the virtual cable's recording endpoint is detected from its audio
adapter, including when the endpoint has been renamed. If the adapter exposes multiple
recording endpoints, set `endpoints.virtualCapture` in `settings.json` to the
desired capture endpoint ID from `audio-switch.exe --list-devices`, then restart
Audio Switch. Omit the field or set it to `null` for automatic detection. A
detection failure appears in route status and leaves the physical microphone
closed until detection succeeds.

On macOS, applications record from the same device Audio Switch plays into. If
they record from a different device, set `endpoints.virtualCapture` to its Core
Audio device UID from
`cargo run -p audio-switch-core --example route_spike -- list`. macOS reports
whether an application is recording, but not from which device, so an
application that plays into the cable while recording from another microphone
also opens the physical microphone.

Settings are stored in `%LOCALAPPDATA%\AudioSwitch\settings.json` on Windows
and `~/Library/Application Support/AudioSwitch/settings.json` on macOS. The app
has no accounts, telemetry, or hosted service. Installed Windows copies check the public
GitHub Releases API once a day. By default, a newer Inno Setup installer is
downloaded, checked against the SHA-256 digest returned by GitHub, and installed
the next time Audio Switch starts. Settings can change this to notify-only or
disable scheduled checks. Standalone builds only open the release page.

Update state and downloaded installers are stored under
`%LOCALAPPDATA%\AudioSwitch\updates`. Release checks disclose the usual HTTPS
request metadata to GitHub. The Stream Deck controller connects only to the
local Stream Deck WebSocket supplied at launch. The driver download button
opens the publisher's website in your browser.

Settings and logs may contain audio device names or endpoint IDs. Remove those
values before attaching files to a public issue.

## Build from source

### Windows

Install the stable Rust toolchain with the MSVC target and Visual Studio Build
Tools for C++ desktop development. The repository pins the required Rust
components in `rust-toolchain.toml`.

```powershell
cargo build --workspace
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

The standalone executable is `target\debug\audio-switch.exe`. Start the setup
wizard directly with:

```powershell
cargo run --release -- --setup
```

To package the Stream Deck plugin, install Node.js 24 and the pinned Elgato CLI.
The package holds the controller for both platforms, so first copy a macOS
build of `audio-switch-stream-deck` (from `scripts/package-stream-deck.sh` or
CI) into `stream-deck\com.2sbsi.audio-switch.sdPlugin`.

```powershell
npm install --global @elgato/cli@1.7.4
.\scripts\package-stream-deck.ps1
```

To build the per-user installer, install Inno Setup 6 and run:

```powershell
.\scripts\package-installer.ps1 -Version 0.5.3
```

Generated packages are written to `dist\` and are not tracked by Git.

### macOS (in development)

The macOS app is a SwiftUI menu bar app over the same Rust core, with a setup
wizard, Settings, a global switch shortcut (`⌃⇧F11` by default), and an open at
login option. It needs macOS 14.2 or newer, Xcode 16 or newer, Rust from
[rustup](https://rustup.rs), and a virtual audio driver such as
[BlackHole](https://existential.audio/blackhole/). Settings are stored in
`~/Library/Application Support/AudioSwitch/settings.json`.

```sh
open macos/AudioSwitch.xcodeproj
```

Building the app runs `scripts/build-macos-core.sh`, which compiles
`audio-switch-ffi` and regenerates the Swift bindings in `macos/Generated`.
Pass `--setup` to the app executable to open setup directly. Debug builds do
not change the login item. Builds are ad-hoc signed, so macOS asks for
microphone and audio capture access again after each rebuild. To keep those
permissions, set `DEVELOPMENT_TEAM` and `CODE_SIGN_IDENTITY = Apple Development`
in `macos/Local.xcconfig`, which Git ignores.

The Windows app crate does not build on macOS, so check the shared crates
directly:

```sh
crates=(-p audio-switch-core -p audio-switch-ffi -p audio-switch-stream-deck -p uniffi-bindgen)
cargo test --locked "${crates[@]}"
cargo clippy --locked "${crates[@]}" --all-targets -- -D warnings
```

To build the release zip in `dist/`, run `scripts/package-macos.sh`.

To package the Stream Deck plugin, install Node.js 24 and the pinned Elgato CLI,
copy a Windows build of `audio-switch-stream-deck.exe` into
`stream-deck/com.2sbsi.audio-switch.sdPlugin`, then run
`scripts/package-stream-deck.sh`. For a development test, build the
controller, copy it into the plugin folder, and run
`streamdeck link stream-deck/com.2sbsi.audio-switch.sdPlugin`. Remove it with
`streamdeck unlink com.2sbsi.audio-switch`.

To check audio routes on real hardware without the app, run
`cargo run -p audio-switch-core --example route_spike -- list`. The example's
header lists its other commands.

## Website

The static GitHub Pages site lives in `site`. It resolves the Windows installer,
macOS app zip, and checksums from the latest public GitHub release. Each platform
is resolved independently: a missing download is labelled unavailable in that
release, and its button opens the release page. If the API is unavailable or
JavaScript is disabled, both buttons open GitHub Releases.

Preview it locally with:

```powershell
python -m http.server 4173 --directory site
```

## Repository layout

- `crates/audio-switch-core` contains configuration, routing state, the local
  control channel, drift control, and the platform audio backends (WASAPI on
  Windows, Core Audio on macOS). `examples/route_spike.rs` drives a backend
  directly for hardware checks.
- `crates/audio-switch-windows` contains the Windows tray app, native setup and
  settings UI, autostart integration, and updater.
- `crates/audio-switch-stream-deck` contains the Stream Deck plugin controller.
- `crates/audio-switch-ffi` contains the macOS app core and its Swift bindings;
  `crates/uniffi-bindgen` generates those bindings.
- `macos` contains the SwiftUI app and its Xcode project.
- `stream-deck` contains the plugin manifest and source artwork.
- `installer` and `scripts` contain the release packaging tools.
- `site` contains the static GitHub Pages landing page.

## Release process

The GitHub Actions workflow checks formatting, tests, and Clippy before building
the Windows standalone app and installer, the macOS app zip, the Stream Deck
package, and SHA-256 checksums. A tag
matching the Cargo workspace version, such as `v0.5.3`, publishes those files as
an immutable GitHub release. Re-running a release succeeds only when every
published asset still has the expected GitHub SHA-256 digest.

## Contributing and security

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Report
security issues through the private process in [SECURITY.md](SECURITY.md), not a
public issue.

Audio Switch is available under the [MIT License](LICENSE).
