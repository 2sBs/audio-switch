import SwiftUI

/// Hosts the setup wizard in its own window. Closing it without finishing
/// leaves the saved settings untouched.
@MainActor
enum SetupWindow {
    private static var window: NSWindow?

    /// Opens setup, or brings it forward if it is already open. Without
    /// `startingFresh`, setup edits the current settings.
    static func show(model: CoreModel, startingFresh: Bool = false) {
        defer { NSApplication.shared.activate() }
        if let window {
            window.makeKeyAndOrderFront(nil)
            return
        }
        var draft = model.config
        if startingFresh {
            // Keep the preferences setup does not show, as Windows does.
            draft = defaultConfig()
            draft.hotkey = model.config.hotkey
            draft.updatePolicy = model.config.updatePolicy
        }
        let window = NSWindow(
            contentViewController: NSHostingController(
                rootView: SetupView(model: model, draft: draft, isFirstRun: !model.isConfigured) {
                    Self.window?.close()
                }
            )
        )
        window.title = "Set Up Audio Switch"
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.center()
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: window, queue: .main
        ) { _ in
            MainActor.assumeIsolated { Self.window = nil }
        }
        Self.window = window
        window.makeKeyAndOrderFront(nil)
    }
}

private enum SetupStep: Int, CaseIterable {
    case welcome, virtualCable, microphones, playback, permissions, startup, verify

    var title: String {
        switch self {
        case .welcome: "Welcome"
        case .virtualCable: "Virtual cable"
        case .microphones: "Microphones"
        case .playback: "Playback mirror"
        case .permissions: "Permissions"
        case .startup: "Startup"
        case .verify: "Review"
        }
    }

    var symbol: String {
        switch self {
        case .welcome: "hand.wave"
        case .virtualCable: "cable.connector"
        case .microphones: "mic"
        case .playback: "speaker.wave.2"
        case .permissions: "lock.shield"
        case .startup: "power"
        case .verify: "checkmark.seal"
        }
    }
}

private struct SetupView: View {
    let model: CoreModel
    @State var draft: AppConfig
    let isFirstRun: Bool
    let close: () -> Void

    @State private var step = SetupStep.welcome
    @State private var devices: [Endpoint] = []
    @State private var microphoneAccess = MicrophoneAccess.current

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            Divider()
            VStack(spacing: 0) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        header
                        content
                    }
                    .padding(28)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                Divider()
                footer
            }
        }
        .frame(width: 720, height: 580)
        .task { await model.watchEndpoints(receive) }
    }

    // MARK: Layout

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Audio Switch")
                .font(.headline)
                .padding(.horizontal, 10)
                .padding(.bottom, 14)
            ForEach(SetupStep.allCases, id: \.self) { item in
                HStack(spacing: 10) {
                    Image(systemName: item.rawValue < step.rawValue ? "checkmark.circle.fill" : "\(item.rawValue + 1).circle")
                        .foregroundStyle(item.rawValue < step.rawValue ? Color.accentColor : .secondary)
                    Text(item.title)
                        .foregroundStyle(item.rawValue > step.rawValue ? .secondary : .primary)
                    Spacer()
                }
                .padding(.vertical, 6)
                .padding(.horizontal, 10)
                .background(
                    RoundedRectangle(cornerRadius: 6)
                        .fill(item == step ? Color.primary.opacity(0.08) : .clear)
                )
            }
            Spacer()
        }
        .padding(16)
        .frame(width: 200)
        .background(.background.secondary)
    }

    private var header: some View {
        HStack(spacing: 14) {
            Image(systemName: step.symbol)
                .font(.system(size: 22, weight: .medium))
                .foregroundStyle(.white)
                .frame(width: 44, height: 44)
                .background(RoundedRectangle(cornerRadius: 10).fill(Color.accentColor.gradient))
            Text(headline).font(.title2.weight(.semibold))
        }
    }

    private var footer: some View {
        HStack {
            Button(isFirstRun ? "Quit" : "Cancel") {
                isFirstRun ? NSApplication.shared.terminate(nil) : close()
            }
            Spacer()
            if step != .welcome {
                Button("Back") { move(by: -1) }
            }
            if step == .verify {
                Button("Finish") {
                    model.finishSetup(with: draft)
                    close()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!SetupStep.allCases.allSatisfy(isComplete))
            } else {
                Button("Continue") { move(by: 1) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!isComplete(step))
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    // MARK: Steps

    private var headline: String {
        switch step {
        case .welcome: "Set up Audio Switch"
        case .virtualCable: "Connect a virtual audio cable"
        case .microphones: "Choose your microphones"
        case .playback: "Mirror your playback"
        case .permissions: "Allow audio access"
        case .startup: "Choose startup behavior"
        case .verify: "Review your setup"
        }
    }

    @ViewBuilder
    private var content: some View {
        switch step {
        case .welcome:
            explanation("Audio Switch keeps one stable microphone in your games and calls while changing the physical microphone behind it. It can also copy everything one playback device plays to another.")
            RouteDiagram(cable: name(of: draft.endpoints.virtualRender) ?? "Virtual cable")
            explanation("Primary and Secondary are roles you name, such as Desk and Headset. Setup takes about a minute.")
        case .virtualCable:
            explanation("Your apps use the virtual cable as their microphone, and Audio Switch feeds the active microphone into it. Audio Switch does not install a cable itself. BlackHole 2ch is a free option.")
            Form {
                DevicePicker(title: "Virtual cable", selection: $draft.endpoints.virtualRender, devices: devices.flow(.render))
            }
            .setupForm()
            if !devices.isAvailable(draft.endpoints.virtualRender) {
                HStack {
                    Link("Get BlackHole…", destination: URL(string: "https://existential.audio/blackhole/")!)
                    Text("Installed cables appear here automatically.").foregroundStyle(.secondary)
                }
            }
        case .microphones:
            explanation("Name each role and choose its microphone. The shortcut and menu switch between them.")
            Form {
                Section("Primary") {
                    TextField("Name", text: $draft.labels.primary)
                    DevicePicker(title: "Microphone", selection: $draft.endpoints.primaryCapture, devices: devices.flow(.capture))
                }
                Section("Secondary") {
                    TextField("Name", text: $draft.labels.secondary)
                    DevicePicker(title: "Microphone", selection: $draft.endpoints.secondaryCapture, devices: devices.flow(.capture))
                }
            }
            .setupForm()
        case .playback:
            explanation("Everything that plays on \(draft.labels.primary)'s playback device is copied to \(draft.labels.secondary)'s, so you hear it on either. If one disconnects, mirroring waits for it to return.")
            Form {
                DevicePicker(title: "\(draft.labels.primary) playback", selection: $draft.endpoints.primaryRender, devices: devices.flow(.render))
                DevicePicker(title: "\(draft.labels.secondary) playback", selection: $draft.endpoints.secondaryRender, devices: devices.flow(.render))
            }
            .setupForm()
            if draft.endpoints.primaryRender != nil, draft.endpoints.primaryRender == draft.endpoints.secondaryRender {
                Label("Choose two different playback devices.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
            }
        case .permissions:
            explanation("macOS asks before an app can hear your microphone or record what other apps play. Audio Switch only passes this audio between your own devices.")
            Form {
                LabeledContent {
                    switch microphoneAccess {
                    case .allowed:
                        Label("Allowed", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                    case .notDetermined:
                        Button("Allow…") { Task { microphoneAccess = await MicrophoneAccess.request() } }
                    case .denied:
                        Button("Open System Settings…") { PrivacyPane.microphone.open() }
                    }
                } label: {
                    Text("Microphone")
                    Text("Needed to route your microphone.")
                }
                LabeledContent {
                    Button("Open System Settings…") { PrivacyPane.systemAudio.open() }
                } label: {
                    Text("System audio recording")
                    Text("macOS asks the first time mirroring starts.")
                }
            }
            .setupForm()
            .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
                microphoneAccess = MicrophoneAccess.current
            }
        case .startup:
            Form {
                Toggle("Open Audio Switch at login", isOn: $draft.autoStart)
                Toggle("Play a sound when switching microphones", isOn: $draft.audio.cueEnabled)
                if let shortcut = Shortcut(draft.hotkey), draft.hotkey.enabled {
                    LabeledContent("Switch shortcut", value: shortcut.display)
                }
            }
            .setupForm()
            explanation("You can change these later in Settings.")
        case .verify:
            Form {
                Section("Microphones") {
                    review(draft.labels.primary, draft.endpoints.primaryCapture)
                    review(draft.labels.secondary, draft.endpoints.secondaryCapture)
                }
                Section("Playback mirror") {
                    review("From \(draft.labels.primary)", draft.endpoints.primaryRender)
                    review("To \(draft.labels.secondary)", draft.endpoints.secondaryRender)
                }
                Section("Apps use") {
                    review("Virtual cable", draft.endpoints.virtualRender)
                }
            }
            .setupForm()
            if microphoneAccess != .allowed {
                Label("Microphone access is off, so apps will hear silence.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
            }
        }
    }

    private func explanation(_ text: String) -> some View {
        Text(text)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func review(_ role: String, _ id: String?) -> some View {
        LabeledContent(role) {
            if devices.isAvailable(id), let name = name(of: id) {
                Label(name, systemImage: "checkmark.circle.fill")
                    .labelStyle(TrailingIcon(color: .green))
            } else {
                Label("Unavailable", systemImage: "exclamationmark.triangle.fill")
                    .labelStyle(TrailingIcon(color: .orange))
            }
        }
    }

    // MARK: State

    private func isComplete(_ step: SetupStep) -> Bool {
        let endpoints = draft.endpoints
        let named = !draft.labels.primary.trimmingCharacters(in: .whitespaces).isEmpty
            && !draft.labels.secondary.trimmingCharacters(in: .whitespaces).isEmpty
        return switch step {
        case .welcome, .permissions, .startup, .verify: true
        case .virtualCable: devices.isAvailable(endpoints.virtualRender)
        case .microphones:
            named && devices.isAvailable(endpoints.primaryCapture) && devices.isAvailable(endpoints.secondaryCapture)
        case .playback:
            devices.isAvailable(endpoints.primaryRender) && devices.isAvailable(endpoints.secondaryRender)
                && endpoints.primaryRender != endpoints.secondaryRender
        }
    }

    private func move(by offset: Int) {
        step = SetupStep(rawValue: step.rawValue + offset) ?? step
    }

    private func receive(_ endpoints: [Endpoint]) {
        devices = endpoints
        // Suggest an installed BlackHole cable. Routing itself never depends
        // on device names.
        if draft.endpoints.virtualRender == nil {
            draft.endpoints.virtualRender = endpoints.flow(.render)
                .first { $0.name.localizedCaseInsensitiveContains("BlackHole") }?.id
        }
    }

    private func name(of id: String?) -> String? {
        devices.first { $0.id == id }?.name
    }
}

/// Mic → cable → apps, the route Audio Switch maintains.
private struct RouteDiagram: View {
    let cable: String

    var body: some View {
        HStack(spacing: 12) {
            node("mic.fill", "Primary or Secondary microphone")
            Image(systemName: "arrow.right").foregroundStyle(.tertiary)
            node("cable.connector", cable)
            Image(systemName: "arrow.right").foregroundStyle(.tertiary)
            node("app.badge", "Games, calls, and recording apps")
        }
        .padding(.vertical, 6)
    }

    private func node(_ symbol: String, _ title: String) -> some View {
        VStack(spacing: 8) {
            Image(systemName: symbol).font(.title2).foregroundStyle(Color.accentColor)
            Text(title).font(.callout).multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity, minHeight: 86)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 10).fill(.background.secondary))
    }
}

private extension View {
    /// A grouped form whose cards line up with the step's heading and text.
    /// Grouped forms inset their content by 20 points on each side.
    func setupForm() -> some View {
        formStyle(.grouped)
            .scrollDisabled(true)
            .padding(.horizontal, -20)
    }
}

private struct TrailingIcon: LabelStyle {
    let color: Color

    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.title
            configuration.icon.foregroundStyle(color)
        }
    }
}
