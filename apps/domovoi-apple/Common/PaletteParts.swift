import App
import SwiftUI

/// A shortcut such as "⌘⇧K" drawn as keys.
struct KeyCaps: View {
    let shortcut: String

    var body: some View {
        HStack(spacing: Space.xxs) {
            ForEach(Array(shortcut.enumerated()), id: \.offset) { _, key in
                Text(String(key))
                    .font(TypeRole.meta.weight(.medium))
                    .foregroundStyle(.secondary)
                    .frame(minWidth: Size.rowIcon, minHeight: Size.rowIcon)
                    // Half a chip's radius: a key is a small chip.
                    .background(.fill.tertiary, in: RoundedRectangle(cornerRadius: Radius.chip / 2, style: .continuous))
            }
        }
        .accessibilityElement()
        .accessibilityLabel(shortcut)
    }
}

extension PaletteItemView {
    /// The command palette's symbol for the item's kind.
    var symbol: String {
        switch kind {
        case "thread": return "bubble.left"
        case "project": return "folder"
        case "file": return "doc"
        case "match": return "text.magnifyingglass"
        default: return "bolt"
        }
    }
}

extension PaletteView {
    /// Going to a file or searching the project (not the command list).
    var searchesFiles: Bool { mode == "files" || mode == "content" }

    /// The field's prompt in this mode.
    var prompt: String {
        !placeholder.isEmpty ? placeholder : searchesFiles ? (mode == "files" ? "Go to file" : "Search the project") : "Search threads, projects and actions"
    }

    /// The back chip's label in a file mode.
    var modeTitle: String { mode == "files" ? "Files" : "Search" }
}
