import SwiftUI

/// The menu bar icon: the active role's name beside a filled mic at the
/// setup, an outlined mic while away from it, or a warning for a fault.
struct MenuBarLabel: View {
    let model: CoreModel

    var body: some View {
        if let snapshot = model.snapshot, model.isConfigured {
            Image(systemName: snapshot.presence.symbol)
            Text(snapshot.labels.name(for: snapshot.state.activeMic))
        } else {
            Image(systemName: "mic.badge.plus")
        }
    }
}

struct MenuContent: View {
    let model: CoreModel

    var body: some View {
        if let snapshot = model.snapshot, model.isConfigured {
            let away = !snapshot.away.isEmpty
            if away {
                Section("Away from your setup") {
                    ForEach(snapshot.away, id: \.self) { Text($0) }
                }
                Divider()
            }
            ForEach(ActiveMic.both, id: \.self) { mic in
                Toggle(isOn: Binding(
                    get: { snapshot.state.activeMic == mic },
                    set: { if $0 { model.select(mic) } }
                )) {
                    Text("\(snapshot.labels.name(for: mic)) microphone")
                    if snapshot.isMissing(mic, in: model.config.endpoints) {
                        Text("Not connected")
                    }
                }
            }
            // The summary already explains anything that is waiting, so while
            // away only a fault needs its own line.
            if !away { Divider() }
            if !away || snapshot.state.micStatus.isError {
                Label("Microphone: \(snapshot.state.micStatus.text)", systemImage: snapshot.state.micStatus.symbol)
            }
            if !away || snapshot.state.mirrorStatus.isError {
                Label("Playback mirror: \(snapshot.state.mirrorStatus.text)", systemImage: snapshot.state.mirrorStatus.symbol)
            }
            if model.config.hotkey.enabled, let shortcut = Shortcut(model.config.hotkey) {
                Text("Switch with \(shortcut.display)")
            }
        } else {
            Button("Set Up Audio Switch…") { SetupWindow.show(model: model) }
        }
        if let error = model.errorMessage {
            Divider()
            Label(error, systemImage: "exclamationmark.triangle")
        }
        Divider()
        SettingsLink { Text("Settings…") }
            .keyboardShortcut(",")
        Button("Quit Audio Switch") { NSApplication.shared.terminate(nil) }
            .keyboardShortcut("q")
    }
}
