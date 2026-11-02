import AppKit
import Observation

/// Owns the Rust app core and mirrors its state for SwiftUI.
@MainActor
@Observable
final class CoreModel {
    private(set) var snapshot: Snapshot?
    private(set) var errorMessage: String?
    @ObservationIgnored private var core: AppCore?

    init() {
        let relay = SnapshotRelay { [weak self] snapshot in
            self?.snapshot = snapshot
        }
        do {
            let core = try AppCore.start(observer: relay)
            self.core = core
            snapshot = core.snapshot()
        } catch CoreError.AlreadyRunning {
            NSApplication.shared.terminate(nil)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func toggleMic() {
        perform { try $0.toggleMic() }
    }

    /// Core calls can block while an audio device starts, so they run off the
    /// main actor. State changes arrive through the observer.
    private func perform(_ operation: @escaping @Sendable (AppCore) throws -> Void) {
        guard let core else { return }
        Task.detached {
            do {
                try operation(core)
                await MainActor.run { self.errorMessage = nil }
            } catch {
                let message = error.localizedDescription
                await MainActor.run { self.errorMessage = message }
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
