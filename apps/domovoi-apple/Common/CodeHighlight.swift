import App
import SwiftUI

/// The core's highlighted code as styled text.
enum CodeHighlight {
    static func color(_ kind: String) -> Color? {
        switch kind {
        case "keyword": return .pink
        case "string": return .orange
        case "comment": return .secondary
        case "number": return .blue
        case "type": return .teal
        case "function": return .indigo
        case "added": return Tint.success
        case "removed": return Tint.danger
        default: return nil
        }
    }

    /// `spans` as styled text, up to `maxLines` lines (nil: all); `fallback`
    /// plain when there are no spans.
    static func text(_ spans: [CodeSpan], fallback: String, maxLines: Int? = nil) -> AttributedString {
        guard !spans.isEmpty else {
            guard let maxLines else { return AttributedString(fallback) }
            return AttributedString(fallback.split(separator: "\n", omittingEmptySubsequences: false).prefix(maxLines).joined(separator: "\n"))
        }
        var out = AttributedString()
        var lines = 0
        for span in spans {
            var piece = span.text
            if let maxLines {
                var kept = Substring()
                for ch in piece {
                    if ch == "\n" {
                        lines += 1
                        if lines >= maxLines { break }
                    }
                    kept.append(ch)
                }
                piece = String(kept)
            }
            var part = AttributedString(piece)
            if let color = color(span.kind) { part.foregroundColor = color }
            if span.kind == "comment" { part.inlinePresentationIntent = .emphasized }
            out += part
            if let maxLines, lines >= maxLines { break }
        }
        return out
    }
}
