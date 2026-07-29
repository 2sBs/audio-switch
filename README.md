# Audio Switch

Audio Switch is a resource-light Windows tray utility that keeps applications on
stable audio endpoints while mirroring playback and switching the physical mic
behind a virtual microphone.

The first version exposes device-neutral routes configured by the user:

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

Start directly in the settings window on first use:

```powershell
cargo run --release -- --settings
```

Choose the five endpoints and optionally rename Primary and Secondary. Playback
is captured from Primary and mirrored to Secondary; the selected physical
microphone is routed into the virtual render endpoint. Applications should keep
using the virtual device's capture endpoint, so changing the physical route does
not change the endpoint seen by a game.

Left-click the tray icon or press `Ctrl+Shift+F11` to switch. Right-click it for
an explicit route choice, settings, and route status. Settings are saved under
`%LOCALAPPDATA%\AudioSwitch\settings.json`.

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

The project targets `stable-x86_64-pc-windows-msvc`. A virtual audio cable or
the planned Audio Switch virtual driver is required for stable microphone
routing and is not redistributed with this project.
