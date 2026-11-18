import App
import SwiftUI

/// xterm's 256-color palette (16 ANSI, 6×6×6 cube, 24 grays) and true color.
func color(_ value: Int32, foreground: Bool) -> Color? {
    if value < 0 { return nil }
    if value >= 0x100_0000 {
        let rgb = value & 0xFF_FFFF
        return Color(red: Double((rgb >> 16) & 0xFF) / 255, green: Double((rgb >> 8) & 0xFF) / 255, blue: Double(rgb & 0xFF) / 255)
    }
    let ansi: [(Double, Double, Double)] = [
        (0, 0, 0), (0.80, 0.19, 0.19), (0.05, 0.74, 0.47), (0.90, 0.90, 0.06), (0.14, 0.45, 0.78), (0.74, 0.25, 0.74), (0.07, 0.66, 0.80), (0.90, 0.90, 0.90),
        (0.40, 0.40, 0.40), (0.95, 0.30, 0.30), (0.14, 0.82, 0.55), (0.96, 0.96, 0.26), (0.23, 0.56, 0.92), (0.84, 0.44, 0.84), (0.16, 0.72, 0.86), (1, 1, 1),
    ]
    let i = Int(value)
    if i < 16 {
        let c = ansi[i]
        return Color(red: c.0, green: c.1, blue: c.2)
    }
    if i < 232 {
        let n = i - 16
        let level = { (v: Int) in v == 0 ? 0 : (Double(v) * 40 + 55) / 255 }
        return Color(red: level(n / 36), green: level((n / 6) % 6), blue: level(n % 6))
    }
    let gray = (Double(i - 232) * 10 + 8) / 255
    return Color(white: gray)
}

extension TerminalScreenView {
    /// What's on screen as plain text, without trailing blanks or blank
    /// lines at the end: for Copy and Select Text.
    var plainText: String {
        let rows = lines.map { line in
            String(line.spans.map(\.text).joined().reversed().drop(while: { $0 == " " }).reversed())
        }
        return rows.reversed().drop(while: \.isEmpty).reversed().joined(separator: "\n")
    }
}
