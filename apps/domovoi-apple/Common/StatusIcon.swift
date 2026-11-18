import SwiftUI

struct StatusIcon: View {
    let status: String
    var unread: Bool = false

    var body: some View {
        Group {
            switch status {
            case "working":
                ProgressView().controlSize(.mini)
            case "approval":
                Image(systemName: "hand.raised.fill").foregroundStyle(Tint.caution)
            case "failed":
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(Tint.danger)
            case "interrupted":
                Image(systemName: "stop.circle").foregroundStyle(.secondary)
            default:
                Circle()
                    .fill(unread ? Color.accentColor : Color.clear)
                    .frame(width: 7, height: 7)
            }
        }
        .frame(width: 14)
        .font(.caption)
        .accessibilityElement()
        .accessibilityLabel(label)
        .accessibilityHidden(label.isEmpty)
    }

    /// What VoiceOver says for the icon.
    private var label: String {
        switch status {
        case "working": return "Working"
        case "approval": return "Needs your approval"
        case "failed": return "Failed"
        case "interrupted": return "Stopped"
        default: return unread ? "Unread" : ""
        }
    }
}
