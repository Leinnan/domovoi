import App
import SwiftUI

/// The core's toasts: a glass capsule with the toast's button ("Undo") and
/// how many wait behind it. It dismisses itself after long enough to read
/// (and to use its button); on iPhone a tap or a swipe up dismisses it, on
/// the Mac its close button, and the timer waits while the pointer is on it.
struct ToastCapsule: View {
    @ObservedObject var core: Core
    /// The edge it slides in from.
    var edge: Edge = .top
    @State private var drag: CGFloat = 0
    @State private var hovering = false
    @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if let toast = core.view.toast {
            HStack(spacing: Space.s) {
                Image(systemName: icon(toast.kind))
                    .foregroundStyle(tint(toast.kind))
                    .iconColumn(Size.rowIcon)
                Text(toast.message).font(.footnote).lineLimit(4)
                Spacer(minLength: 0)
                if !toast.actionLabel.isEmpty {
                    Button { core.update(.toastAction) } label: {
                        Text(toast.actionLabel).tapTarget(inPadding: Space.m)
                    }
                        .font(.footnote.weight(.semibold))
                        .buttonStyle(.borderless)
                        .foregroundStyle(Color.accentColor)
                }
                if toast.queued > 0 {
                    Text("+\(toast.queued)")
                        .monospacedDigit()
                        .badgeCapsule()
                        .accessibilityLabel("\(toast.queued) more")
                }
                #if os(macOS)
                Button { core.update(.dismissToast) } label: {
                    Image(systemName: "xmark").font(.caption.weight(.semibold))
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .help("Dismiss")
                .accessibilityLabel("Dismiss")
                #endif
            }
            .padding(.horizontal, Space.l)
            .padding(.vertical, Space.m)
            .glassPanel(in: Capsule())
            #if os(macOS)
            .frame(maxWidth: 560)
            .fixedSize(horizontal: false, vertical: true)
            .onHover { hovering = $0 }
            #else
            // The composer panel's own side inset: their edges line up.
            .padding(.horizontal, Space.s)
            .offset(y: min(0, drag))
            .gesture(
                DragGesture(minimumDistance: 8)
                    .onChanged { drag = $0.translation.height }
                    .onEnded { value in
                        if value.translation.height < -24 { core.update(.dismissToast) }
                        withAnimation(.snappy) { drag = 0 }
                    }
            )
            .onTapGesture { core.update(.dismissToast) }
            #endif
            .accessibilityElement(children: .combine)
            .accessibilityAction(named: "Dismiss") { core.update(.dismissToast) }
            .modifier(ToastAction(label: toast.actionLabel) { core.update(.toastAction) })
            .transition(reduceMotion ? .opacity : .move(edge: edge).combined(with: .opacity))
            .task(id: toast.id) {
                AccessibilityNotification.Announcement(toast.message).post()
            }
            .task(id: "\(toast.id)-\(hovering)") {
                guard !hovering else { return }
                // VoiceOver may not reach a button in time: such a toast
                // stays until it's used or dismissed.
                if voiceOver, !toast.actionLabel.isEmpty { return }
                // Long enough to read; one with a button long enough to use it.
                let seconds = !toast.actionLabel.isEmpty ? 7.0 : toast.kind == "error" ? 6.0 : 3.0
                try? await Task.sleep(for: .seconds(seconds + Double(toast.message.count) / 40))
                if !Task.isCancelled, core.view.toast?.id == toast.id { core.update(.dismissToast) }
            }
            .id(toast.id)
        }
    }

    private func icon(_ kind: String) -> String {
        switch kind {
        case "error": return "exclamationmark.octagon.fill"
        case "success": return "checkmark.circle.fill"
        default: return "info.circle.fill"
        }
    }

    private func tint(_ kind: String) -> Color {
        switch kind {
        case "error": return Tint.danger
        case "success": return Tint.success
        default: return .accentColor
        }
    }
}

/// The toast's own button (Undo) as a named VoiceOver action.
private struct ToastAction: ViewModifier {
    let label: String
    let run: () -> Void

    func body(content: Content) -> some View {
        if label.isEmpty { content } else { content.accessibilityAction(named: label, run) }
    }
}
