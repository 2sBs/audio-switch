import AppKit
import Carbon.HIToolbox

/// A keyboard shortcut in the settings format shared with Windows: modifier
/// names plus a key name, such as `["control", "shift"]` and `"F11"`.
struct Shortcut: Equatable {
    var key: String
    var modifiers: Set<Modifier>

    enum Modifier: String, CaseIterable {
        // Display order follows the macOS convention: ⌃⌥⇧⌘.
        case control, option, shift, command

        var symbol: String {
            switch self {
            case .control: "⌃"
            case .option: "⌥"
            case .shift: "⇧"
            case .command: "⌘"
            }
        }

        var carbonFlag: Int {
            switch self {
            case .control: controlKey
            case .option: optionKey
            case .shift: shiftKey
            case .command: cmdKey
            }
        }

        var eventFlag: NSEvent.ModifierFlags {
            switch self {
            case .control: .control
            case .option: .option
            case .shift: .shift
            case .command: .command
            }
        }
    }

    /// Nil when the saved key is not one Audio Switch can register on macOS.
    init?(_ config: HotkeyConfig) {
        guard Self.keyCodes[config.key] != nil else { return nil }
        key = config.key
        modifiers = Set(config.modifiers.compactMap { name in
            // Windows settings may say "alt" for the Option key.
            Modifier(rawValue: name == "alt" ? "option" : name)
        })
    }

    /// Nil for keys that cannot be a shortcut, or letters and digits pressed
    /// without a modifier, which would steal ordinary typing.
    init?(event: NSEvent) {
        guard let key = Self.keyCodes.first(where: { $0.value == Int(event.keyCode) })?.key else {
            return nil
        }
        let modifiers = Set(Modifier.allCases.filter { event.modifierFlags.contains($0.eventFlag) })
        guard !modifiers.isEmpty || key.hasPrefix("F") && key.count > 1 else { return nil }
        self.key = key
        self.modifiers = modifiers
    }

    var config: HotkeyConfig {
        HotkeyConfig(
            modifiers: Modifier.allCases.filter(modifiers.contains).map(\.rawValue),
            key: key,
            enabled: true
        )
    }

    var display: String {
        Modifier.allCases.filter(modifiers.contains).map(\.symbol).joined() + key
    }

    fileprivate var keyCode: UInt32 { UInt32(Self.keyCodes[key] ?? 0) }

    fileprivate var carbonModifiers: UInt32 {
        UInt32(modifiers.reduce(0) { $0 | $1.carbonFlag })
    }

    private static let keyCodes: [String: Int] = {
        var codes: [String: Int] = [
            "F1": kVK_F1, "F2": kVK_F2, "F3": kVK_F3, "F4": kVK_F4, "F5": kVK_F5,
            "F6": kVK_F6, "F7": kVK_F7, "F8": kVK_F8, "F9": kVK_F9, "F10": kVK_F10,
            "F11": kVK_F11, "F12": kVK_F12, "F13": kVK_F13, "F14": kVK_F14, "F15": kVK_F15,
            "F16": kVK_F16, "F17": kVK_F17, "F18": kVK_F18, "F19": kVK_F19, "F20": kVK_F20,
            "Space": kVK_Space,
        ]
        let letters = [
            kVK_ANSI_A, kVK_ANSI_B, kVK_ANSI_C, kVK_ANSI_D, kVK_ANSI_E, kVK_ANSI_F, kVK_ANSI_G,
            kVK_ANSI_H, kVK_ANSI_I, kVK_ANSI_J, kVK_ANSI_K, kVK_ANSI_L, kVK_ANSI_M, kVK_ANSI_N,
            kVK_ANSI_O, kVK_ANSI_P, kVK_ANSI_Q, kVK_ANSI_R, kVK_ANSI_S, kVK_ANSI_T, kVK_ANSI_U,
            kVK_ANSI_V, kVK_ANSI_W, kVK_ANSI_X, kVK_ANSI_Y, kVK_ANSI_Z,
        ]
        for (letter, code) in zip("ABCDEFGHIJKLMNOPQRSTUVWXYZ", letters) {
            codes[String(letter)] = code
        }
        let digits = [
            kVK_ANSI_0, kVK_ANSI_1, kVK_ANSI_2, kVK_ANSI_3, kVK_ANSI_4,
            kVK_ANSI_5, kVK_ANSI_6, kVK_ANSI_7, kVK_ANSI_8, kVK_ANSI_9,
        ]
        for (digit, code) in zip("0123456789", digits) {
            codes[String(digit)] = code
        }
        return codes
    }()
}

/// A system-wide shortcut registered through the Carbon hot key API, which
/// works without Accessibility access. Call `invalidate()` before discarding.
@MainActor
final class GlobalHotkey {
    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?
    private let action: () -> Void

    init?(_ shortcut: Shortcut, action: @escaping () -> Void) {
        self.action = action
        var pressed = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )
        let installed = InstallEventHandler(
            GetEventDispatcherTarget(),
            { _, _, context in
                guard let context else { return OSStatus(eventNotHandledErr) }
                let hotkey = Unmanaged<GlobalHotkey>.fromOpaque(context).takeUnretainedValue()
                // Carbon delivers hot key events on the main thread.
                MainActor.assumeIsolated { hotkey.action() }
                return noErr
            },
            1,
            &pressed,
            Unmanaged.passUnretained(self).toOpaque(),
            &handler
        )
        guard installed == noErr else { return nil }
        let registered = RegisterEventHotKey(
            shortcut.keyCode,
            shortcut.carbonModifiers,
            EventHotKeyID(signature: OSType(0x4153_5357), id: 1),  // 'ASSW'
            GetEventDispatcherTarget(),
            0,
            &hotKey
        )
        guard registered == noErr else {
            invalidate()
            return nil
        }
    }

    func invalidate() {
        if let hotKey { UnregisterEventHotKey(hotKey) }
        if let handler { RemoveEventHandler(handler) }
        hotKey = nil
        handler = nil
    }
}
