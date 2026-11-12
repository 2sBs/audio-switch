import AppKit
import Observation

/// Owns the Rust app core and mirrors its state and settings for SwiftUI. Also
/// keeps the OS integrations (global shortcut, login item) in step with the
/// saved settings.
@MainActor
@Observable
final class CoreModel {
    private(set) var snapshot: Snapshot?
    /// Settings as last applied from this app. Settings edit it directly, so
    /// it is the source of truth for the UI and is never reloaded mid-edit.
    private(set) var config: AppConfig
    private(set) var isConfigured: Bool
    private(set) var errorMessage: String?

    /// False when the core failed to start or another copy is running.
    var isRunning: Bool { core != nil }

    @ObservationIgnored private var core: AppCore?
    @ObservationIgnored private var hotkey: GlobalHotkey?
    /// Core calls can block while an audio device starts. They run here, off
    /// the main actor, in the order the user made them.
    @ObservationIgnored private let queue = DispatchQueue(label: "com.2sbsi.audio-switch.core")

    init() {
        config = defaultConfig()
        isConfigured = false
        let relay = SnapshotRelay { [weak self] snapshot in
            self?.snapshot = snapshot
        }
        do {
            let core = try AppCore.start(observer: relay)
            self.core = core
            config = core.config()
            isConfigured = core.wasConfiguredAtStart()
            snapshot = core.snapshot()
        } catch CoreError.AlreadyRunning {
            // Quit once the app has finished launching; terminating from init is ignored.
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
            return
        } catch {
            errorMessage = error.localizedDescription
        }
        syncIntegrations(from: nil)
    }

    func select(_ mic: ActiveMic) {
        perform { try $0.selectMic(active: mic) }
    }

    func toggleMic() {
        perform { try $0.toggleMic() }
    }

    /// Applies one settings change immediately.
    func update(_ change: (inout AppConfig) -> Void) {
        var updated = config
        change(&updated)
        apply(updated)
    }

    func finishSetup(with config: AppConfig) {
        isConfigured = true
        apply(config)
    }

    /// The current audio devices, refreshed every two seconds until the
    /// calling task is cancelled.
    func watchEndpoints(_ receive: ([Endpoint]) -> Void) async {
        guard let core else { return }
        while !Task.isCancelled {
            let endpoints = await Task.detached { (try? core.endpoints()) ?? [] }.value
            receive(endpoints)
            try? await Task.sleep(for: .seconds(2))
        }
    }

    private func apply(_ updated: AppConfig) {
        let previous = config
        config = updated
        syncIntegrations(from: previous)
        perform { try $0.applyConfig(config: updated) }
    }

    private func perform(_ operation: @escaping @Sendable (AppCore) throws -> Void) {
        guard let core else { return }
        queue.async { [weak self] in
            let message: String?
            do {
                try operation(core)
                message = nil
            } catch {
                message = error.localizedDescription
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.errorMessage = message }
            }
        }
    }

    private func syncIntegrations(from previous: AppConfig?) {
        if previous?.hotkey != config.hotkey {
            hotkey?.invalidate()
            hotkey = Shortcut(config.hotkey).flatMap { shortcut in
                config.hotkey.enabled
                    ? GlobalHotkey(shortcut) { [weak self] in self?.toggleMic() }
                    : nil
            }
        }
        if previous?.autoStart != config.autoStart {
            do {
                try LoginItem.sync(enabled: config.autoStart)
            } catch {
                errorMessage = "Open at login could not be changed: \(error.localizedDescription)"
            }
        }
    }
}

/// Forwards core notifications to the main thread. The main queue is FIFO, so
/// snapshots are applied in the order the core sent them.
private final class SnapshotRelay: StateObserver {
    private let apply: @MainActor @Sendable (Snapshot) -> Void

    init(apply: @escaping @MainActor @Sendable (Snapshot) -> Void) {
        self.apply = apply
    }

    func stateChanged(snapshot: Snapshot) {
        DispatchQueue.main.async { [apply] in
            MainActor.assumeIsolated { apply(snapshot) }
        }
    }
}
