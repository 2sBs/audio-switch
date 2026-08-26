# Audio Switch

[![Build and release](https://github.com/2sBs/audio-switch/actions/workflows/build-release.yml/badge.svg)](https://github.com/2sBs/audio-switch/actions/workflows/build-release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Audio Switch is a Windows tray app for switching physical microphones without
changing the input device selected in games, calls, or recording software. It
also mirrors one playback device to another.

The app uses device-neutral Primary and Secondary roles. You choose the WASAPI
endpoints and can rename the roles. Audio Switch does not match specific device
names in its routing logic.

```text
Primary playback -> Secondary playback
Selected microphone -> Virtual cable input -> Applications
```

## Requirements

- x64 Windows 10 version 1809 or newer, including Windows 11
- A virtual audio cable such as [VB-CABLE](https://vb-audio.com/Cable/)
- Elgato Stream Deck 7.1 or newer for the optional plugin

Audio Switch does not bundle or download a virtual audio driver. Install one
from its publisher before completing setup.

## Install

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

The setup wizard selects the virtual cable, playback devices, microphones, and
startup behavior. Applications should use the virtual cable's capture endpoint
as their microphone. Audio Switch then changes the physical microphone behind
that stable endpoint.

## Use

Left-click the tray icon or press `Ctrl+Shift+F11` to switch microphones.
Right-click the icon to choose a route, inspect status, or open Settings.

Audio Switch preserves the selected route if the Secondary device disconnects.
It waits for that device and resumes the route when it returns instead of
silently falling back to Primary.

Only one tray app can run in a Windows session. The optional Stream Deck plugin
controls that running app over a session-local named pipe. It does not include
or launch another tray host. If the app is unavailable, the key displays
`START AUDIO SWITCH`.

Settings are stored in `%LOCALAPPDATA%\AudioSwitch\settings.json`. The app has
no accounts, telemetry, or hosted service. Installed copies check the public
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

To package the Stream Deck plugin, install Node.js 24 and the pinned Elgato CLI:

```powershell
npm install --global @elgato/cli@1.7.4
.\scripts\package-stream-deck.ps1
```

To build the per-user installer, install Inno Setup 6 and run:

```powershell
.\scripts\package-installer.ps1 -Version 0.5.2
```

Generated packages are written to `dist\` and are not tracked by Git.

## Website

The static GitHub Pages site lives in `site`. It resolves the newest Windows
installer through the public GitHub Releases API and falls back to the latest
release page if a direct download cannot be found.

Preview it locally with:

```powershell
python -m http.server 4173 --directory site
```

## Repository layout

- `crates/audio-switch-core` contains configuration, routing state, IPC, drift
  control, and the WASAPI backend.
- `crates/audio-switch-app` contains the tray app, native setup and settings UI,
  autostart integration, and Stream Deck controller.
- `stream-deck` contains the plugin manifest and source artwork.
- `installer` and `scripts` contain the release packaging tools.
- `site` contains the static GitHub Pages landing page.

## Release process

The GitHub Actions workflow checks formatting, tests, and Clippy before building
the standalone app, installer, Stream Deck package, and SHA-256 checksums. A tag
matching the Cargo workspace version, such as `v0.5.2`, publishes those files as
an immutable GitHub release. Re-running a release succeeds only when every
published asset still has the expected GitHub SHA-256 digest.

## Contributing and security

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Report
security issues through the private process in [SECURITY.md](SECURITY.md), not a
public issue.

Audio Switch is available under the [MIT License](LICENSE).
