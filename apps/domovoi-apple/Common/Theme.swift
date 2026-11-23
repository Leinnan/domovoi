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

    /// The widest a run of prose gets inside the column (markdown text).
    static let readable: CGFloat = 680

    /// Content under a disclosure header starts under its icon, not its chevron.
    static let disclosureIndent: CGFloat = Size.chevron + s
}

/// Fixed sizes: tap targets, icon columns, dots, avatars.
enum Size {
    /// The smallest a thing to tap gets on a touch screen.
    static let tap: CGFloat = 44
    /// Every chip: the composer's, the new thread's, the palette's back chip.
    static let chip: CGFloat = 36
    /// The icon column of a card or transcript row: titles line up whatever the symbol.
    static let rowIcon: CGFloat = 20
    /// The leading column of a list or settings row, and of option cards.
    static let listIcon: CGFloat = 28
    /// A thread's avatar (the base for `@ScaledMetric`).
    static let avatar: CGFloat = 32
    /// Status and unread dots.
    static let dot: CGFloat = 8
    /// A disclosure chevron's column.
    static let chevron: CGFloat = 12
    /// The big symbol over an empty state or the pairing screen.
    static let heroIcon: CGFloat = 44
}

/// Corner radii. One per kind of thing, never adjusted by a point or two:
/// `card` for transcript cards and inline banners, `chip` for blocks set in
/// text (code, tables, hunks) and thumbnails in a bubble, `bubble` for
/// floating banners, `panel` for the composer and what stacks on it.
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
    static let antigravity = Color.indigo

    /// The tint of an agent as the core names it ("Claude Code", "Codex", "Pi", "Antigravity").
    static func agent(_ name: String) -> Color {
        switch name {
        case "Codex": codex
        case "Pi": pi
        case "Antigravity": antigravity
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
    /// A card's own title (plan, to-dos, subagent).
    static let cardTitle = Font.callout.weight(.semibold)
    /// The second line of meta: under `meta`, or beside it.
    static let metaSmall = Font.caption2
    /// Text in a badge capsule.
    static let badge = Font.caption2.weight(.medium)
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
        // The repeating animation reaches only the opacity in the closure.
        // Started with `withAnimation`, or keyed to `on` with
        // `.animation(_:value:)`, it also took any move or resize of the view
        // made in the same update (a lazy row settling from its estimated
        // place, the transcript following its end) and repeated that
        // forever: dots swung between stale heights, tiles slid off icons.
        content
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.8).repeatForever(autoreverses: true)) {
                $0.opacity(reduceMotion || on ? 1 : 0.35)
            }
            .onAppear { on = true }
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

    /// On a touch screen, at least `Size.tap` tall and tappable across its
    /// frame; as is elsewhere. Goes on a button's label, inside the button.
    @ViewBuilder
    func tapTarget(width: Bool = false) -> some View {
        #if os(iOS)
        frame(minWidth: width ? Size.tap : nil, minHeight: Size.tap).contentShape(Rectangle())
        #else
        self
        #endif
    }

    /// A button's label in a padded panel (a toast, a notice): `Size.tap`
    /// tall on a touch screen, reaching into the panel's own padding
    /// (`inset`) instead of making the panel taller. As is elsewhere.
    @ViewBuilder
    func tapTarget(inPadding inset: CGFloat, width: Bool = false) -> some View {
        #if os(iOS)
        tapTarget(width: width).padding(.vertical, -inset)
        #else
        self
        #endif
    }

    /// A card's header (or the button row that sets a card's height): the
    /// card's top and bottom padding inside the button, so all of it taps.
    /// The card itself then pads only its sides.
    func cardRowInset() -> some View {
        padding(.vertical, Space.m).contentShape(Rectangle()).tapTarget()
    }

    /// A card's action buttons: small on the Mac, full size on a touch screen.
    @ViewBuilder
    func cardControlSize() -> some View {
        #if os(iOS)
        controlSize(.regular)
        #else
        controlSize(.small)
        #endif
    }

    /// A small capsule beside a title (a PR, a source, a count): one size
    /// and fill everywhere. No tint: a quiet grey.
    func badgeCapsule(tint: Color? = nil) -> some View {
        font(TypeRole.badge)
            .padding(.horizontal, Space.tight)
            .padding(.vertical, Space.xxs)
            .background(tint.map { AnyShapeStyle($0.opacity(0.15)) } ?? AnyShapeStyle(.fill.tertiary), in: Capsule())
    }

    /// A fixed-width icon column, so the titles beside icons line up.
    func iconColumn(_ width: CGFloat = Size.rowIcon) -> some View {
        frame(width: width, alignment: .center)
    }

    /// The sheets that hold options and pickers: half height, pulled up for
    /// more, with the grabber showing.
    func standardSheet() -> some View {
        #if os(iOS)
        presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
        #else
        self
        #endif
    }
}

/// A chevron that turns as its section opens, in its fixed column.
struct DisclosureChevron: View {
    let open: Bool

    var body: some View {
        Image(systemName: "chevron.right")
            .font(.caption2.bold())
            .rotationEffect(.degrees(open ? 90 : 0))
            .frame(width: Size.chevron)
            .foregroundStyle(.tertiary)
            .accessibilityHidden(true)
    }
}

/// Something under way: a spinner and what it is.
struct LoadingRow: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        HStack(spacing: Space.s) {
            ProgressView()
            Text(text).foregroundStyle(.secondary)
        }
    }
}

/// Something that went wrong, the same way on every screen.
struct ErrorRow: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Label(text, systemImage: "exclamationmark.triangle.fill")
            .foregroundStyle(Tint.danger)
    }
}

/// A list or settings row: an icon in its column, a title, and a line under it.
struct SettingsRow<Icon: View>: View {
    let title: String
    var subtitle: String = ""
    @ViewBuilder let icon: () -> Icon

    var body: some View {
        HStack(spacing: Space.m) {
            icon().iconColumn(Size.listIcon)
            VStack(alignment: .leading, spacing: Space.xxs) {
                Text(title)
                if !subtitle.isEmpty {
                    Text(subtitle).font(TypeRole.meta).foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// A changed file: its path, and the lines added and removed.
struct FileChangeRow: View {
    let path: String
    let added: Int
    let removed: Int

    var body: some View {
        HStack(spacing: Space.s) {
            Text(path).font(TypeRole.monoSmall).lineLimit(1).truncationMode(.middle)
            Spacer(minLength: Space.s)
            if added > 0 { Text("+\(added)").foregroundStyle(Tint.diffAdd) }
            if removed > 0 { Text("−\(removed)").foregroundStyle(Tint.diffDel) }
        }
        .font(TypeRole.meta.monospacedDigit())
    }
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
