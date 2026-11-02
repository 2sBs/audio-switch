import Foundation

extension DeviceLabels {
    func name(for mic: ActiveMic) -> String {
        switch mic {
        case .primary: primary
        case .secondary: secondary
        }
    }
}

extension ActiveMic {
    static let both: [ActiveMic] = [.primary, .secondary]
}

extension RouteStatus {
    var text: String {
        switch self {
        case .stopped: "stopped"
        case .starting: "starting"
        case .routing: "routing"
        case .waiting(let reason): reason
        case .error(let message): message
        }
    }

    var symbol: String {
        switch self {
        case .routing: "checkmark.circle"
        case .starting: "circle.dotted"
        case .stopped: "pause.circle"
        case .waiting: "clock"
        case .error: "exclamationmark.triangle"
        }
    }

    var isError: Bool {
        if case .error = self { true } else { false }
    }
}

/// How the menu bar icon reads at a glance.
enum Presence {
    /// Every configured device is connected and in use.
    case normal
    /// Away from the configured devices, such as an undocked laptop. Expected,
    /// so it is not shown as a warning.
    case away
    /// A route failed, or the virtual cable is missing.
    case problem
}

extension Snapshot {
    var presence: Presence {
        if state.micStatus.isError || state.mirrorStatus.isError { return .problem }
        return away.isEmpty ? .normal : .away
    }

    /// Whether a mic role's device is configured but not connected.
    func isMissing(_ mic: ActiveMic, in endpoints: EndpointIds) -> Bool {
        switch mic {
        case .primary: endpoints.primaryCapture != nil && !availability.primaryMic
        case .secondary: endpoints.secondaryCapture != nil && !availability.secondaryMic
        }
    }
}

extension [Endpoint] {
    func flow(_ flow: EndpointFlow) -> [Endpoint] {
        filter { $0.flow == flow }
    }

    func isAvailable(_ id: String?) -> Bool {
        guard let id else { return false }
        return contains { $0.id == id && $0.available }
    }
}

extension Presence {
    var symbol: String {
        switch self {
        case .normal: "mic.fill"
        case .away: "mic"
        case .problem: "exclamationmark.triangle.fill"
        }
    }
}
