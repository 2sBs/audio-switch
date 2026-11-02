import AVFoundation
import AppKit
import ServiceManagement

enum LoginItem {
    /// Registers or removes the app as a login item. Debug builds leave the
    /// system setting alone so they never replace the installed app's entry.
    static func sync(enabled: Bool) throws {
        #if !DEBUG
            let service = SMAppService.mainApp
            if enabled, service.status != .enabled {
                try service.register()
            } else if !enabled, service.status == .enabled {
                try service.unregister()
            }
        #endif
    }

    /// True when macOS is waiting for the user to allow the login item.
    static var needsApproval: Bool {
        SMAppService.mainApp.status == .requiresApproval
    }
}

enum MicrophoneAccess {
    case allowed, notDetermined, denied

    static var current: MicrophoneAccess {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: .allowed
        case .notDetermined: .notDetermined
        default: .denied
        }
    }

    static func request() async -> MicrophoneAccess {
        await AVCaptureDevice.requestAccess(for: .audio) ? .allowed : .denied
    }
}

enum PrivacyPane: String {
    case microphone = "Privacy_Microphone"
    /// Screen & System Audio Recording, which holds the audio capture permission.
    case systemAudio = "Privacy_ScreenCapture"
    case loginItems = "LoginItems"

    func open() {
        let url = switch self {
        case .loginItems: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
        default: "x-apple.systempreferences:com.apple.preference.security?\(rawValue)"
        }
        NSWorkspace.shared.open(URL(string: url)!)
    }
}
