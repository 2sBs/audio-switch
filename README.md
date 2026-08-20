# Audio Switch

Audio Switch is a resource-light Windows tray utility that keeps applications on
stable audio endpoints while mirroring playback and switching the physical mic
behind a virtual microphone.

Audio Switch exposes device-neutral routes configured by the user:

- Primary playback endpoint
- Secondary mirrored playback endpoint
- Primary microphone endpoint
- Secondary microphone endpoint
- Stable virtual microphone render endpoint

Primary and secondary are only roles. Their display names and physical WASAPI
endpoints are user settings; the application contains no device-name matching.

## Development status

The repository contains the Rust workspace, live WASAPI routing engine, native
tray host, local control protocol, Stream Deck bridge, drift controller, and
switch-cue synthesis.

## Run

On first launch, Audio Switch opens a guided setup for the virtual cable,
playback devices, microphones, route verification, and startup preferences. If
the configured virtual cable later becomes unavailable, the app offers to reopen
setup without silently changing the saved route.

Start directly in guided setup:

```powershell
cargo run --release -- --setup
```

The virtual cable step links to the official VB-CABLE download page when a
compatible endpoint is not detected. Audio Switch never downloads or
redistributes the driver. Choose the five endpoints and optionally rename
Primary and Secondary. Playback is captured from Primary and mirrored to
Secondary; the selected physical microphone is routed into the virtual render
endpoint. Applications should keep using the virtual device's capture endpoint,
so changing the physical route does not change the endpoint seen by a game.

Left-click the tray icon or press `Ctrl+Shift+F11` to switch. Right-click it for
an explicit route choice, settings, and route status. Settings are saved under
`%LOCALAPPDATA%\AudioSwitch\settings.json`.

The native settings window keeps device roles, routing, cues, and startup under
General. Updates is reserved for the planned updater, and About reports the app
version. General can restart guided setup with fresh defaults after a
confirmation. The current configuration and audio routing remain active until
setup finishes, and cancelling returns to Settings without changing either.

Only one tray-app instance can run per Windows session. Attempting a second
launch displays an error instead of starting another set of audio routes.

The Stream Deck plugin is a controller for the tray app over a session-scoped
local named pipe. The tray app must already be running for Stream Deck controls
to work; the plugin does not contain or launch another copy of the tray app.
When the tray app is unavailable, the key displays `START AUDIO SWITCH` and an
alert.

Windows sign-in startup can optionally request UAC elevation and run Audio
Switch at high process priority. When enabled, accept the UAC prompt at each
sign-in to start the app. The existing non-elevated startup mode remains the
default; enable the UAC/high-priority option separately in Settings.

## Build

```powershell
cargo build --workspace
cargo test --workspace
```

To build and validate the native Stream Deck plugin package:

```powershell
.\scripts\package-stream-deck.ps1
```

To build the per-user Windows installer with Inno Setup 6:

```powershell
.\scripts\package-installer.ps1 -Version 0.4.0
```

The installer installs only the standalone app under
`%LOCALAPPDATA%\Programs\Audio Switch`. It preserves user settings during
upgrades and uninstall, and launches the guided setup after a fresh install.

The standalone app is built as `target\release\audio-switch.exe`; the plugin
package contains only `audio-switch-stream-deck.exe` and its Stream Deck assets.

## Release

Push a tag matching the Cargo workspace version, such as `v0.4.0`, to build a
GitHub release. The release contains a versioned standalone Windows executable,
a per-user Windows setup executable, a versioned controller-only Stream Deck
plugin installer, and SHA-256 checksums for all artifacts.

The project targets `stable-x86_64-pc-windows-msvc`. A virtual audio cable or
the planned Audio Switch virtual driver is required for stable microphone
routing and is not redistributed with this project.
