import App
import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Shared palette; darker light-mode shades keep small letters legible.
enum ProjectPalette {
    static let keys = ["gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan", "sky", "blue", "indigo", "violet", "purple", "fuchsia", "pink", "rose"]
    static let light: [UInt32] = [0x4b5563, 0x991b1b, 0x9a3412, 0x92400e, 0x854d0e, 0x3f6212, 0x166534, 0x065f46, 0x115e59, 0x155e75, 0x075985, 0x1e40af, 0x4338ca, 0x6d28d9, 0x7e22ce, 0xa21caf, 0x9d174d, 0x9f1239]
    static let dark: [UInt32] = [0x9ca3af, 0xfca5a5, 0xfb923c, 0xfbbf24, 0xfacc15, 0xa3e635, 0x4ade80, 0x34d399, 0x2dd4bf, 0x22d3ee, 0x38bdf8, 0x60a5fa, 0xa5b4fc, 0xc4b5fd, 0xd8b4fe, 0xe879f9, 0xf9a8d4, 0xfda4af]
    static func rgb(_ key: String, dark usesDarkPalette: Bool) -> UInt32 {
        let index = keys.firstIndex(of: key) ?? 0
        return usesDarkPalette ? dark[index] : light[index]
    }

    static func foreground(_ key: String) -> Color {
        #if os(macOS)
        return Color(nsColor: NSColor(name: nil) { appearance in
            let hex = rgb(key, dark: appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua)
            return NSColor(srgbRed: Double((hex >> 16) & 255) / 255, green: Double((hex >> 8) & 255) / 255, blue: Double(hex & 255) / 255, alpha: 1)
        })
        #else
        return Color(uiColor: UIColor { traits in
            let hex = rgb(key, dark: traits.userInterfaceStyle == .dark)
            return UIColor(red: Double((hex >> 16) & 255) / 255, green: Double((hex >> 8) & 255) / 255, blue: Double(hex & 255) / 255, alpha: 1)
        })
        #endif
    }
}

enum LucideSymbols {
    static let names = ["folder-code", "code-2", "terminal", "globe-2", "server", "database", "bot", "sparkles", "smartphone", "monitor", "cloud-cog", "package", "book-open", "flask-conical", "shield-check", "rocket", "gamepad-2", "music", "image", "shopping-bag", "git-branch", "workflow", "wrench", "layers-3"]
    static let symbols = ["folder", "chevron.left.forwardslash.chevron.right", "terminal", "globe", "server.rack", "externaldrive", "cpu", "sparkles", "iphone", "desktopcomputer", "cloud", "shippingbox", "book", "flask", "checkmark.shield", "paperplane", "gamecontroller", "music.note", "photo", "bag", "arrow.triangle.branch", "point.3.connected.trianglepath.dotted", "wrench", "square.3.layers.3d"]
    static func sf(_ name: String) -> String {
        if name == "message-square-dashed" { return "bubble.left" }
        return names.firstIndex(of: name).map { symbols[$0] } ?? "folder"
    }
}

@MainActor
struct ProjectIconTile: View, @preconcurrency Equatable {
    let icon: ProjectIconView
    let size: CGFloat
    var core: Core? = nil
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Group {
            if let address = icon.imageUrl, let url = URL(string: address) {
                AsyncImage(url: url) { phase in
                    if let image = phase.image { image.resizable().scaledToFit() }
                    else { glyph }
                }
            } else {
                glyph
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.25, style: .continuous))
        .task(id: icon.loadKey) {
            if icon.loadKey != nil { core?.update(.loadProjectIcon(projectId: icon.projectId)) }
        }
        .accessibilityHidden(true)
    }

    private var glyph: some View {
        Group {
            switch icon.kind {
            case "emoji": Text(icon.text).font(.system(size: size * 0.7))
            case "symbol": Image(systemName: LucideSymbols.sf(icon.symbol)).font(.system(size: size * 0.55, weight: .semibold))
            default: Text(icon.text).font(.system(size: size * 0.45, weight: .bold, design: .monospaced)).lineLimit(1).minimumScaleFactor(0.5)
            }
        }
        .foregroundStyle(ProjectPalette.foreground(icon.color))
        .frame(width: size, height: size)
        .background(ProjectPalette.foreground(icon.color).opacity(scheme == .dark ? 0.16 : 0.10))
    }
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.icon == rhs.icon && lhs.size == rhs.size }
}

struct ThreadAvatar: View {
    let icon: ProjectIconView
    let status: String
    let size: CGFloat
    let core: Core
    var body: some View {
        ProjectIconTile(icon: icon, size: size, core: core)
            .overlay(alignment: .bottomTrailing) {
                if StatusStyle.symbol(status) != nil {
                    StatusIcon(status: status)
                        .frame(width: size * 0.6, height: size * 0.6)
                        .background(.background, in: Circle())
                        .offset(x: size * 0.15, y: size * 0.15)
                }
            }
            .accessibilityHidden(true)
    }
}

enum SectionStyle {
    static func symbol(_ kind: String) -> String {
        switch kind {
        case "inbox": "tray.fill"
        case "running": "play.circle"
        case "inactive": "moon.zzz.fill"
        case "archived": "archivebox.fill"
        case "snoozed": "moon.zzz"
        default: "hand.raised.fill"
        }
    }
    static func tint(_ kind: String) -> Color {
        switch kind {
        case "inbox": .accentColor
        case "running": .green
        case "inactive": .indigo
        case "snoozed": .purple
        case "needs-you": Tint.caution
        default: .secondary
        }
    }
}

struct SectionGlyph: View {
    let kind: String
    var count: UInt32 = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var body: some View {
        Group {
            if kind == "running", count > 0, !reduceMotion { ProgressView().controlSize(.mini) }
            else { Image(systemName: SectionStyle.symbol(kind)).foregroundStyle(SectionStyle.tint(kind)) }
        }.accessibilityHidden(true)
    }
}

struct CountCapsule: View {
    let count: UInt32
    var body: some View {
        Text("\(count)").monospacedDigit()
            .badgeCapsule()
            .accessibilityHidden(true)
    }
}

extension ThreadRowView {
    var spokenLabel: String {
        [title.isEmpty ? "New thread" : title, projectTitle, statusLabel.isEmpty ? age : statusLabel, unread ? "Unread" : "", pinned ? "Pinned" : ""].filter { !$0.isEmpty }.joined(separator: ", ")
    }
    func subtitle(showsProject: Bool) -> String {
        [showsProject ? projectTitle : "", modelLabel, branch.map { "⑂ \($0)" } ?? ""].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}
