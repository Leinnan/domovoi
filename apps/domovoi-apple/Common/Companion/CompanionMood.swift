import App

enum CompanionMood: Hashable {
    case offline, searching, wince, alert, working(Int), happy, idle, drowsy, asleep, waking

    var frameInterval: Double {
        switch self {
        case .asleep: 1 / 15
        case .idle, .drowsy, .searching: 1 / 30
        default: 1 / 60
        }
    }

    var stillTime: Double {
        switch self {
        case .waking: 0.4
        case .drowsy: 2
        default: 0
        }
    }

    func accessibilityValue(working: UInt32, attention: UInt32) -> String {
        switch self {
        case .offline: "Offline"
        case .searching: "Connecting"
        case .wince: "A thread failed"
        case .happy: "A thread finished"
        case .alert: attention == 1 ? "One thread needs you" : "\(attention) threads need you"
        case .working: working == 1 ? "One thread working" : "\(working) threads working"
        case .waking:
            if attention > 0 { Self.alert.accessibilityValue(working: working, attention: attention) }
            else if working > 0 { Self.working(1).accessibilityValue(working: working, attention: attention) }
            else { "Connected, waking up" }
        case .asleep: "Connected, asleep"
        case .drowsy, .idle: "Connected, nothing running"
        }
    }
}

struct CompanionActivity: Equatable {
    let server: String
    let connection: String
    let working: UInt32
    let attention: UInt32
    let beat: String
    let sequence: UInt64

    init(_ view: ConnectionView) {
        server = view.activeEnvironmentId
        connection = view.offline ? "disconnected" : view.state
        working = view.activity.working
        attention = view.activity.attention
        beat = view.activity.beat
        sequence = view.activity.beatSeq
    }
}

extension ConnectionView {
    var companionServerLabel: String {
        let name = environments.first { $0.id == activeEnvironmentId }?.label ?? environmentLabel
        return name.isEmpty ? "Server" : "Server, \(name)"
    }

    func companionVoiceOverValue(_ mood: CompanionMood) -> String {
        if state == "reconnecting" && !offline { return "Reconnecting" }
        return mood.accessibilityValue(working: activity.working, attention: activity.attention)
    }
}
