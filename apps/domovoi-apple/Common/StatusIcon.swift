import SwiftUI

/// The status glyph and its spoken meaning, shared by rows and avatar badges.
enum StatusStyle {
    static func symbol(_ status: String) -> String? {
        switch status {
        case "working": "ellipsis.circle.fill"
        case "approval": "hand.raised.fill"
        case "failed": "exclamationmark.circle.fill"
        case "interrupted": "stop.circle.fill"
        default: nil
        }
    }
    static func tint(_ status: String) -> Color {
        switch status {
        case "approval": Tint.caution
        case "failed": Tint.danger
        default: .secondary
        }
    }
    static func label(_ status: String) -> String {
        switch status {
        case "working": "Working"
        case "approval": "Needs your approval"
        case "failed": "Failed"
        case "interrupted": "Stopped"
        default: ""
        }
    }
}

struct StatusIcon: View {
    let status: String
    var unread: Bool = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if status == "working", !reduceMotion {
                ProgressView().controlSize(.mini)
            } else if let symbol = StatusStyle.symbol(status) {
                Image(systemName: symbol).foregroundStyle(StatusStyle.tint(status))
            } else {
                Circle().fill(unread ? Color.accentColor : .clear).frame(width: Size.dot, height: Size.dot)
            }
        }
        .frame(width: 14)
        .font(.caption)
        .accessibilityElement()
        .accessibilityLabel(label)
        .accessibilityHidden(label.isEmpty)
    }
    private var label: String {
        let label = StatusStyle.label(status)
        return label.isEmpty && unread ? "Unread" : label
    }
}
