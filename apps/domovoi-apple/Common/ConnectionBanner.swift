import App
import SwiftUI

/// What the connection is doing when it isn't simply connected: no network,
/// waiting to retry (with a countdown and Retry Now), trying, or
/// disconnected by the user (with Connect).
struct ConnectionBanner: View {
    @ObservedObject var core: Core
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var connection: ConnectionView { core.view.connection }

    private enum Kind { case offline, waiting, trying, disconnected }

    /// Whether the banner shows at all.
    static func showing(_ core: Core) -> Bool { ConnectionBanner(core: core).kind != nil }

    private var kind: Kind? {
        guard !connection.activeEnvironmentId.isEmpty, !connection.needsPairing else { return nil }
        if connection.state == "connected" { return nil }
        if connection.offline { return .offline }
        switch connection.state {
        case "reconnecting": return connection.attempting || core.retryAt == nil ? .trying : .waiting
        case "connecting": return .trying
        case "disconnected": return .disconnected
        default: return nil
        }
    }

    var body: some View {
        if let kind {
            HStack(alignment: .center, spacing: Space.s) {
                icon(kind).foregroundStyle(color(kind)).iconColumn(Size.rowIcon)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    headline(kind).font(.footnote.weight(.semibold))
                    if let detail = detail(kind) {
                        Text(detail).font(TypeRole.meta).lineLimit(4).foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
                switch kind {
                case .waiting:
                    Button("Retry Now") { core.update(.reconnect) }
                        .font(.footnote.weight(.semibold))
                        .glassButton()
                case .disconnected:
                    Button("Connect") { core.update(.reconnect) }
                        .font(.footnote.weight(.semibold))
                        .glassButton()
                default:
                    EmptyView()
                }
            }
            .padding(.horizontal, Space.l)
            .padding(.vertical, Space.m)
            .glassPanel(tint: color(kind))
            // The composer panel's own side inset: their edges line up.
            .padding(.horizontal, Space.s)
            .accessibilityElement(children: .combine)
            .transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
        }
    }

    @ViewBuilder
    private func icon(_ kind: Kind) -> some View {
        switch kind {
        case .offline: Image(systemName: "wifi.slash")
        case .waiting: Image(systemName: "arrow.triangle.2.circlepath")
        case .trying: ProgressView().controlSize(.small)
        case .disconnected: Image(systemName: "bolt.horizontal.circle")
        }
    }

    @ViewBuilder
    private func headline(_ kind: Kind) -> some View {
        switch kind {
        case .offline: Text("No network")
        case .trying: Text(connection.state == "connecting" ? "Connecting…" : "Reconnecting…")
        case .disconnected: Text("Disconnected")
        case .waiting:
            if let retryAt = core.retryAt {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    let left = max(1, Int(retryAt.timeIntervalSince(context.date).rounded(.up)))
                    Text("Reconnecting in \(left) s")
                }
            } else {
                Text("Reconnecting…")
            }
        }
    }

    private func detail(_ kind: Kind) -> String? {
        let base = baseDetail(kind)
        guard connection.showingSaved else { return base }
        let saved = "Showing what was saved when last connected."
        return base.map { "\($0) \(saved)" } ?? saved
    }

    private func baseDetail(_ kind: Kind) -> String? {
        // The composer's "goes once it's back" note, said here instead of
        // in a second notice above the field.
        let queued = core.view.composer.queuesOffline ? "What you send goes once it's back." : nil
        switch kind {
        case .offline:
            let waiting = Platform.isPhone ? "Waiting for Wi-Fi or cellular data. It reconnects by itself." : "Waiting for the network. It reconnects by itself."
            return [waiting, queued].compactMap { $0 }.joined(separator: " ")
        case .disconnected: return "You disconnected from \(serverName)."
        case .waiting, .trying:
            let parts = [connection.error, connection.hint.isEmpty ? nil : connection.hint, queued].compactMap { $0 }
            return parts.isEmpty ? nil : parts.joined(separator: " ")
        }
    }

    private var serverName: String {
        connection.environments.first { $0.id == connection.activeEnvironmentId }?.label ?? "the server"
    }

    private func color(_ kind: Kind) -> Color {
        switch kind {
        case .offline, .disconnected: return .gray
        case .waiting, .trying: return .orange
        }
    }
}
