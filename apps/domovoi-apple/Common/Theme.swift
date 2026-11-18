import SwiftUI

// The one scale of spacing, corner radii, tints and surfaces both apps draw
// with. Views use these instead of literal numbers, so a row, a card and a
// chip line up wherever they appear.

/// Spacing on a 4 pt grid.
enum Space {
    static let xxs: CGFloat = 2
    static let xs: CGFloat = 4
    /// Between `xs` and `s`: chip insets, compact rows.
    static let tight: CGFloat = 6
    static let s: CGFloat = 8
    static let m: CGFloat = 12
    static let l: CGFloat = 16
    static let xl: CGFloat = 24
    static let xxl: CGFloat = 32

    /// The transcript's side margin: narrow on a phone.
    static var gutter: CGFloat { Platform.isPhone ? l : xl }

    /// The reading column: the transcript and the composer under it are
    /// never wider than this, so their edges line up.
    static let column: CGFloat = 900
}

/// Corner radii.
enum Radius {
    /// Code blocks, inline badges, diff hunks.
    static let chip: CGFloat = 8
    /// Tool rows, plans, to-dos, approvals.
    static let card: CGFloat = 14
    /// The user's bubble and the composer field.
    static let bubble: CGFloat = 20
    /// The composer's floating panel.
    static let panel: CGFloat = bubble + 6
}

/// What a colour means, not which colour it is.
enum Tint {
    static let plan = Color.purple
    static let danger = Color.red
    static let caution = Color.orange
    static let success = Color.green
    static let question = Color.blue
    /// Find matches.
    static let highlight = Color.yellow
    /// A favourite (a starred model).
    static let favorite = Color.yellow
    /// Added and removed lines in a diff.
    static let diffAdd = Color.green
    static let diffDel = Color.red
    /// Which agent a thread runs on.
    static let codex = Color.teal
    static let claude = Color.orange
    static let pi = Color.pink

    /// The tint of an agent as the core names it ("Claude Code", "Codex", "Pi").
    static func agent(_ name: String) -> Color {
        switch name {
        case "Codex": codex
        case "Pi": pi
        default: claude
        }
    }
}

/// Type roles.
enum TypeRole {
    static let rowTitle = Font.callout
    static let meta = Font.caption
    static let mono = Font.system(.callout, design: .monospaced)
    static let monoSmall = Font.system(.caption, design: .monospaced)
    /// Code blocks and diffs: a touch under `mono`, so more fits per line.
    /// On iPhone and iPad it follows the user's text size.
    #if os(iOS)
    static let code = Font.system(.caption, design: .monospaced)
    #else
    static let code = Font.system(size: 11.5, design: .monospaced)
    #endif
    /// A section's title in options and panels.
    static let sectionHeader = Font.subheadline.weight(.semibold)
}

extension View {
    /// A quiet card: one fill and a hairline edge, the same everywhere.
    func cardSurface(_ radius: CGFloat = Radius.card, tint: Color? = nil) -> some View {
        modifier(CardSurface(radius: radius, tint: tint))
    }

    /// A chip on glass where the OS has it (iOS 26, macOS 26), on a
    /// material otherwise. `selected` fills it with `tint`.
    func glassChip(tint: Color? = nil, selected: Bool = false, in shape: some Shape = Capsule()) -> some View {
        modifier(GlassChip(tint: tint, selected: selected, shape: AnyShape(shape)))
    }

    /// Floating chrome (the composer, banners, jump-to-end) on glass.
    func glassPanel(in shape: some Shape = RoundedRectangle(cornerRadius: Radius.bubble, style: .continuous), tint: Color? = nil) -> some View {
        modifier(GlassPanel(shape: AnyShape(shape), tint: tint))
    }
}

private struct CardSurface: ViewModifier {
    let radius: CGFloat
    let tint: Color?

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
        content
            .background {
                if let tint {
                    shape.fill(tint.opacity(0.08))
                } else {
                    shape.fill(.fill.quaternary)
                }
            }
            .overlay {
                shape.strokeBorder(tint.map { $0.opacity(0.28) } ?? Color.primary.opacity(0.07), lineWidth: 0.5)
            }
    }
}

private struct GlassChip: ViewModifier {
    let tint: Color?
    let selected: Bool
    let shape: AnyShape

    func body(content: Content) -> some View {
        if #available(iOS 26, macOS 26, *) {
            content.glassEffect(selected ? .regular.tint((tint ?? .accentColor).opacity(0.55)).interactive() : .regular.interactive(), in: shape)
        } else {
            content
                .background(selected ? AnyShapeStyle((tint ?? .accentColor).opacity(0.25)) : AnyShapeStyle(.regularMaterial), in: shape)
                .overlay(shape.stroke(Color.primary.opacity(0.08), lineWidth: 0.5))
        }
    }
}

private struct GlassPanel: ViewModifier {
    let shape: AnyShape
    let tint: Color?

    func body(content: Content) -> some View {
        if #available(iOS 26, macOS 26, *) {
            content.glassEffect(tint.map { .regular.tint($0.opacity(0.5)) } ?? .regular, in: shape)
        } else {
            content
                .background(.regularMaterial, in: shape)
                .background(tint.map { $0.opacity(0.3) } ?? .clear, in: shape)
                .shadow(color: .black.opacity(0.12), radius: 8, y: 2)
        }
    }
}

/// Groups glass shapes so they blend and morph into each other (iOS 26),
/// or just stacks them.
struct GlassGroup<Content: View>: View {
    var spacing: CGFloat = Space.s
    @ViewBuilder let content: () -> Content

    var body: some View {
        if #available(iOS 26, macOS 26, *) {
            GlassEffectContainer(spacing: spacing) { content() }
        } else {
            content()
        }
    }
}

/// A gentle pulse for "something is happening", off under Reduce Motion.
struct Pulse: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var on = false

    func body(content: Content) -> some View {
        // The repeating animation is scoped to the opacity: started with
        // `withAnimation` it also took any move of the row made in the same
        // update and repeated that, so rows jittered.
        content
            .opacity(reduceMotion ? 1 : (on ? 1 : 0.35))
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.8).repeatForever(autoreverses: true), value: on)
            .onAppear { on = true }
    }
}

/// A light sweep across text ("Working…"), off under Reduce Motion.
struct Shimmer: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: CGFloat = -1

    func body(content: Content) -> some View {
        if reduceMotion {
            content
        } else {
            content
                .overlay {
                    GeometryReader { geo in
                        LinearGradient(colors: [.clear, .white.opacity(0.55), .clear], startPoint: .leading, endPoint: .trailing)
                            .frame(width: geo.size.width * 0.6)
                            .offset(x: phase * geo.size.width * 1.6)
                            // Scoped to the sweep (see `Pulse`).
                            .animation(.linear(duration: 1.6).repeatForever(autoreverses: false), value: phase)
                            .blendMode(.plusLighter)
                    }
                    .mask(content)
                    .allowsHitTesting(false)
                }
                .onAppear { phase = 1 }
        }
    }
}

extension View {
    /// Selectable text that may be cut to a few lines. On macOS a selectable
    /// `Text` with a line limit lays out at the limit but draws every line,
    /// over whatever is below it: while `limited` it isn't selectable there
    /// (and is clipped to its frame).
    @ViewBuilder
    func selectable(limited: Bool) -> some View {
        #if os(macOS)
        if limited { clipped() } else { textSelection(.enabled) }
        #else
        textSelection(.enabled)
        #endif
    }

    /// Selectable text on the Mac. On iPhone a long press on a transcript
    /// row opens its menu (Copy, Select Text), which inline selection would
    /// take over.
    @ViewBuilder
    func selectableText() -> some View {
        #if os(macOS)
        textSelection(.enabled)
        #else
        self
        #endif
    }

    /// A glass button (`prominent`: tinted) where the OS has glass.
    @ViewBuilder
    func glassButton(prominent: Bool = false) -> some View {
        if #available(iOS 26, macOS 26, *) {
            if prominent { buttonStyle(.glassProminent) } else { buttonStyle(.glass) }
        } else {
            if prominent { buttonStyle(.borderedProminent) } else { buttonStyle(.bordered) }
        }
    }

    /// A quiet fill under the pointer (Mac rows, chips and tabs); nothing
    /// on a touch screen.
    func hoverHighlight(_ radius: CGFloat = Radius.chip, active: Bool = false) -> some View {
        modifier(HoverHighlight(radius: radius, active: active))
    }

    func pulsing() -> some View { modifier(Pulse()) }
    func shimmering() -> some View { modifier(Shimmer()) }
}

private struct HoverHighlight: ViewModifier {
    let radius: CGFloat
    /// Drawn as if hovered (the highlighted row of a list driven by keys).
    let active: Bool
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .background {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(active ? AnyShapeStyle(Color.accentColor.opacity(0.18)) : hovering ? AnyShapeStyle(Color.primary.opacity(0.06)) : AnyShapeStyle(.clear))
            }
            #if os(macOS)
            .onHover { hovering = $0 }
            #endif
    }
}

/// `DOMOVOI_PRINT_CHANGES=1` (debug builds): the heavy views print why they
/// drew again (`Self._printChanges()`), to find what redraws per token.
enum DebugViews {
    #if DEBUG
    static let printChanges = ProcessInfo.processInfo.environment["DOMOVOI_PRINT_CHANGES"] == "1"
    #else
    static let printChanges = false
    #endif
}
