import SwiftUI

@main
struct AudioSwitchApp: App {
    @NSApplicationDelegateAdaptor private var appDelegate: AppDelegate

    var body: some Scene {
        MenuBarExtra {
            MenuContent(model: appDelegate.model)
        } label: {
            MenuBarLabel(model: appDelegate.model)
        }
        Settings {
            SettingsView(model: appDelegate.model)
        }
    }
}

/// Owns the model so the core starts at launch, and opens setup on first run
/// (or with `--setup`, as on Windows) without waiting for the menu.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = CoreModel()

    func applicationDidFinishLaunching(_ notification: Notification) {
        if model.isRunning, !model.isConfigured || CommandLine.arguments.contains("--setup") {
            SetupWindow.show(model: model)
        }
    }
}
