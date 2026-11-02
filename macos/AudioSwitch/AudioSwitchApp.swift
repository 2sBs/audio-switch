import SwiftUI

// Placeholder menu that proves the Rust core is wired up. The real menu,
// settings, and setup UI replace it once their designs are chosen.
@main
struct AudioSwitchApp: App {
    @State private var model = CoreModel()

    var body: some Scene {
        MenuBarExtra {
            if let snapshot = model.snapshot {
                let state = snapshot.state
                Text("Active: \(snapshot.labels.name(for: state.activeMic))")
                Text("Microphone: \(state.micStatus.text)")
                Text("Playback mirror: \(state.mirrorStatus.text)")
                Button("Switch to \(snapshot.labels.name(for: state.activeMic.toggled))") {
                    model.toggleMic()
                }
            }
            if let error = model.errorMessage {
                Text(error)
            }
            Divider()
            Button("Quit Audio Switch") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        } label: {
            Image(systemName: model.snapshot?.state.activeMic == .secondary ? "mic.circle.fill" : "mic.fill")
        }
    }
}

extension DeviceLabels {
    func name(for mic: ActiveMic) -> String {
        switch mic {
        case .primary: primary
        case .secondary: secondary
        }
    }
}

extension ActiveMic {
    var toggled: ActiveMic {
        switch self {
        case .primary: .secondary
        case .secondary: .primary
        }
    }
}

extension RouteStatus {
    var text: String {
        switch self {
        case .stopped: "Stopped"
        case .starting: "Starting"
        case .routing: "Routing"
        case .waiting(let reason): "Waiting: \(reason)"
        case .error(let message): "Error: \(message)"
        }
    }
}
