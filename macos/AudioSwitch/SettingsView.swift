import SwiftUI

/// The Settings window: one grouped form per tab. Every change applies
/// immediately; there is no Save button.
struct SettingsView: View {
    let model: CoreModel

    var body: some View {
        TabView {
            DevicesSettings(model: model)
                .tabItem { Label("Devices", systemImage: "mic") }
            GeneralSettings(model: model)
                .tabItem { Label("General", systemImage: "gearshape") }
            AboutSettings()
                .tabItem { Label("About", systemImage: "info.circle") }
        }
        .frame(width: 520)
        .fixedSize(horizontal: false, vertical: true)
        // A menu bar app is not frontmost when Settings opens from its menu.
        .onAppear { NSApplication.shared.activate() }
    }
}

private struct DevicesSettings: View {
    let model: CoreModel
    @State private var devices: [Endpoint] = []

    var body: some View {
        let labels = model.config.labels
        Form {
            Section("Role names") {
                TextField("Primary", text: model.binding(\.labels.primary))
                TextField("Secondary", text: model.binding(\.labels.secondary))
            }
            Section {
                DevicePicker(title: labels.primary, selection: model.binding(\.endpoints.primaryCapture), devices: devices.flow(.capture))
                DevicePicker(title: labels.secondary, selection: model.binding(\.endpoints.secondaryCapture), devices: devices.flow(.capture))
            } header: {
                Text("Microphones")
            } footer: {
                Text("The active role's microphone feeds the virtual cable.").foregroundStyle(.secondary)
            }
            Section {
                DevicePicker(title: "From \(labels.primary)", selection: model.binding(\.endpoints.primaryRender), devices: devices.flow(.render))
                DevicePicker(title: "To \(labels.secondary)", selection: model.binding(\.endpoints.secondaryRender), devices: devices.flow(.render))
            } header: {
                Text("Playback mirror")
            } footer: {
                Text("Everything playing on the first device is copied to the second.").foregroundStyle(.secondary)
            }
            Section {
                DevicePicker(title: "Virtual cable", selection: model.binding(\.endpoints.virtualRender), devices: devices.flow(.render))
            } header: {
                Text("Virtual microphone")
            } footer: {
                Text("In your apps, choose the virtual cable as the microphone.").foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        // Size the window to the form instead of scrolling inside it.
        .scrollDisabled(true)
        .task { await model.watchEndpoints { devices = $0 } }
    }
}

private struct GeneralSettings: View {
    let model: CoreModel

    var body: some View {
        Form {
            Section("Switching") {
                Toggle("Switch microphones with a shortcut", isOn: model.binding(\.hotkey.enabled))
                LabeledContent("Shortcut") {
                    ShortcutRecorder(hotkey: model.binding(\.hotkey))
                }
                .disabled(!model.config.hotkey.enabled)
                Toggle("Play a sound when switching", isOn: model.binding(\.audio.cueEnabled))
            }
            Section {
                Toggle("Open at login", isOn: model.binding(\.autoStart))
                if model.config.autoStart, LoginItem.needsApproval {
                    LabeledContent("macOS needs your approval") {
                        Button("Open Login Items…") { PrivacyPane.loginItems.open() }
                    }
                }
            } header: {
                Text("Startup")
            }
            Section {
                LabeledContent("Reset settings") {
                    Button("Reset and Run Setup…") { SetupWindow.show(model: model, startingFresh: true) }
                }
            } footer: {
                Text("Setup starts from fresh defaults. Nothing changes unless you finish it.")
                    .foregroundStyle(.secondary)
            }
            if let error = model.errorMessage {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                }
            }
        }
        .formStyle(.grouped)
        // Size the window to the form instead of scrolling inside it.
        .scrollDisabled(true)
    }
}

/// Shows the saved shortcut. Click, then press a new key combination; Escape
/// cancels.
private struct ShortcutRecorder: View {
    @Binding var hotkey: HotkeyConfig
    @State private var monitor: Any?

    var body: some View {
        Button(label) { monitor == nil ? start() : stop() }
            .onDisappear(perform: stop)
    }

    private var label: String {
        if monitor != nil { return "Press a shortcut…" }
        return Shortcut(hotkey)?.display ?? "Not set"
    }

    private func start() {
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            if event.keyCode == 53 {  // Escape
                stop()
            } else if let shortcut = Shortcut(event: event) {
                var updated = shortcut.config
                updated.enabled = hotkey.enabled
                hotkey = updated
                stop()
            } else {
                NSSound.beep()
            }
            return nil
        }
    }

    private func stop() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
    }
}

private struct AboutSettings: View {
    private let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""

    var body: some View {
        VStack(spacing: 10) {
            Image(nsImage: NSApplication.shared.applicationIconImage)
                .resizable()
                .frame(width: 72, height: 72)
            Text("Audio Switch").font(.title2.weight(.semibold))
            Text("Version \(version)").foregroundStyle(.secondary)
            Text("Switch physical microphones without changing the input your apps use.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .frame(maxWidth: 340)
            HStack(spacing: 16) {
                Link("GitHub", destination: URL(string: "https://github.com/2sBs/audio-switch")!)
                Link("Report an issue", destination: URL(string: "https://github.com/2sBs/audio-switch/issues")!)
                Link("MIT License", destination: URL(string: "https://github.com/2sBs/audio-switch/blob/main/LICENSE")!)
            }
            .padding(.top, 6)
        }
        .padding(28)
        .frame(maxWidth: .infinity)
    }
}
