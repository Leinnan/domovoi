import App
import AppKit
import SwiftUI

/// Terminals: the core emulates the screen (vt100); this view draws its
/// styled runs, measures how many cells fit, and forwards keystrokes.
struct TerminalPane: View {
    @ObservedObject var core: Core
    let terminal: TerminalPanelView

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: Space.xs) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: Space.xs) {
                        ForEach(terminal.tabs, id: \.id) { tab in
                            HStack(spacing: Space.xs) {
                                Circle().fill(tab.running ? Tint.success : Color.secondary).frame(width: 6, height: 6)
                                Text(tab.label)
                                Button { core.update(.closeTerminal(terminalId: tab.id)) } label: { Image(systemName: "xmark").font(.caption2) }
                                    .buttonStyle(.plain)
                                    .help("Close terminal")
                                    .accessibilityLabel("Close \(tab.label)")
                            }
                            .font(.caption)
                            .padding(.horizontal, Space.s)
                            .padding(.vertical, Space.xs)
                            .background(tab.active ? Color.accentColor.opacity(0.2) : Color.clear, in: RoundedRectangle(cornerRadius: Radius.chip))
                            .onTapGesture { core.update(.selectTerminal(terminalId: tab.id)) }
                        }
                    }
                }
                if let screen = terminal.active, !screen.exited {
                    Button(screen.watching ? "Interact" : "Watch") {
                        core.update(.setTerminalWatch(watching: !screen.watching))
                    }
                    .buttonStyle(.borderless)
                }
                if terminal.active?.exited == true {
                    Button("Open fresh shell") { core.update(.reopenTerminal) }
                        .buttonStyle(.borderless)
                }
                Button { core.update(.clearTerminal) } label: { Image(systemName: "eraser") }
                    .buttonStyle(.borderless)
                    .help("Clear the terminal and its scrollback")
                    .accessibilityLabel("Clear terminal")
                    .disabled(terminal.active?.watching != false || terminal.active?.exited != false)
                Button {
                    TerminalFocus.requested = true
                    core.update(.newTerminal)
                } label: { Image(systemName: "plus") }
                    .buttonStyle(.borderless)
                    .help("New terminal")
                    .accessibilityLabel("New terminal")
            }
            .padding(Space.s)
            Divider()
            if let screen = terminal.active {
                TerminalScreen(core: core, screen: screen)
            } else {
                ContentUnavailableView("No terminal", systemImage: "terminal", description: Text(core.view.thread == nil ? "Open a thread to use terminals." : "Press + to open one in the thread's folder."))
            }
        }
    }
}

struct TerminalScreen: View {
    @ObservedObject var core: Core
    let screen: TerminalScreenView
    @State private var focused = false

    private static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    private static let cell: CGSize = {
        let size = ("M" as NSString).size(withAttributes: [.font: font])
        return CGSize(width: ceil(size.width * 100) / 100, height: ceil(font.ascender - font.descender + font.leading) + 1)
    }()

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                Color(nsColor: .textBackgroundColor)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(screen.lines.enumerated()), id: \.offset) { _, line in
                        Text(attributed(line))
                            .font(Font(Self.font))
                            .frame(height: Self.cell.height, alignment: .leading)
                            .fixedSize()
                    }
                }
                .padding(Space.tight)
                if screen.cursorVisible {
                    Rectangle()
                        .fill(Color.accentColor.opacity(focused ? 0.7 : 0.3))
                        .frame(width: Self.cell.width, height: Self.cell.height)
                        .offset(x: 6 + CGFloat(screen.cursorCol) * Self.cell.width, y: 6 + CGFloat(screen.cursorRow) * Self.cell.height)
                }
                KeyCatcher(focused: $focused, onKey: handle, menu: menu) { rows in
                    core.update(.terminalScroll(lines: Int32(rows)))
                }
                .frame(width: geo.size.width, height: geo.size.height)
                if screen.scrollbackOffset > 0 {
                    Button { core.update(.terminalScroll(lines: -Int32(screen.scrollbackOffset))) } label: {
                        Label("\(screen.scrollbackOffset) of \(max(screen.scrollbackMax, screen.scrollbackOffset)) lines back", systemImage: "arrow.down.to.line").font(.caption)
                    }
                    .buttonStyle(.borderless)
                    .padding(Space.tight)
                    .background(.regularMaterial, in: Capsule())
                    .padding(Space.s)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                    .help("Back to the live screen")
                }
                if screen.exited {
                    Text(screen.hasSavedScreen ? "Last saved screen" : screen.restored ? "Ended after server restart" : "Process exited").font(.caption).padding(Space.tight).background(.regularMaterial, in: Capsule()).padding(Space.s)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                }
            }
            .clipped()
            .onAppear { resize(geo.size) }
            .onChange(of: geo.size) { _, size in resize(size) }
            .onChange(of: screen.id) { _, _ in resize(geo.size) }
            .onChange(of: screen.watching) { _, _ in resize(geo.size) }
        }
    }

    private func resize(_ size: CGSize) {
        // A size mid-layout can be infinite or NaN, which a UInt32 can't hold.
        guard size.width.isFinite, size.height.isFinite else { return }
        let cols = UInt32(min(max(10, (size.width - 12) / Self.cell.width), 1000))
        let rows = UInt32(min(max(2, (size.height - 12) / Self.cell.height), 500))
        if cols != screen.cols || rows != screen.rows {
            core.update(.terminalResize(rows: rows, cols: cols))
        }
    }

    private func attributed(_ line: TerminalLineView) -> AttributedString {
        var out = AttributedString()
        for span in line.spans {
            var piece = AttributedString(span.text)
            var fg = color(span.fg, foreground: true)
            var bg = color(span.bg, foreground: false)
            if span.inverse { swap(&fg, &bg) }
            piece.foregroundColor = fg ?? Color.primary
            if let bg { piece.backgroundColor = bg }
            if span.bold { piece.font = Font(NSFont.monospacedSystemFont(ofSize: 12, weight: .bold)) }
            if span.underline { piece.underlineStyle = .single }
            if span.italic { piece.font = Font(NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)).italic() }
            out.append(piece)
        }
        return out
    }

    private var screenText: String { screen.plainText }

    private func paste() {
        if let text = NSPasteboard.general.string(forType: .string) { core.update(.terminalPaste(text: text)) }
    }

    /// Clears the screen and its scrollback; the core then sends ⌃L for a
    /// fresh prompt (⌘K stays the Command Palette).
    private func clear() {
        core.update(.clearTerminal)
    }

    private func menu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(ClosureMenuItem("Copy Screen") { Platform.copy(screenText) })
        let pasteItem = ClosureMenuItem("Paste", action: paste)
        pasteItem.isEnabled = NSPasteboard.general.string(forType: .string) != nil
        menu.addItem(pasteItem)
        menu.addItem(.separator())
        menu.addItem(ClosureMenuItem("Clear", action: clear))
        return menu
    }

    private func handle(_ event: NSEvent) -> Bool {
        let flags = event.modifierFlags
        if flags.contains(.command) {
            switch event.charactersIgnoringModifiers {
            case "v": paste(); return true
            // No selection to copy from: ⌘C copies what's on screen.
            case "c": Platform.copy(screenText); return true
            default: return false
            }
        }
        let ctrl = flags.contains(.control)
        let alt = flags.contains(.option)
        let named: String? = switch event.keyCode {
        case 36, 76: "enter"
        case 48: flags.contains(.shift) ? "backtab" : "tab"
        case 51: "backspace"
        case 53: "escape"
        case 117: "delete"
        case 123: "left"
        case 124: "right"
        case 125: "down"
        case 126: "up"
        case 115: "home"
        case 119: "end"
        case 116: "pageup"
        case 121: "pagedown"
        default: nil
        }
        if let named {
            core.update(.terminalKey(key: named, text: "", ctrl: ctrl, alt: alt))
            return true
        }
        let text = ctrl ? (event.charactersIgnoringModifiers ?? "") : (event.characters ?? "")
        guard !text.isEmpty else { return false }
        core.update(.terminalKey(key: "char", text: text, ctrl: ctrl, alt: alt && !ctrl))
        return true
    }
}

/// Whether the next terminal to appear takes the keyboard: only when asked
/// for (+, Show Terminal), so a terminal appearing never takes it from the
/// message field mid-sentence.
@MainActor
enum TerminalFocus {
    static var requested = false
}

/// A menu item that runs a closure.
final class ClosureMenuItem: NSMenuItem {
    private let run: () -> Void

    init(_ title: String, action run: @escaping () -> Void) {
        self.run = run
        super.init(title: title, action: #selector(runAction), keyEquivalent: "")
        target = self
    }

    required init(coder: NSCoder) { fatalError("init(coder:) is not used") }

    @objc private func runAction() { run() }
}

/// A focusable AppKit view that receives key events for the terminal.
struct KeyCatcher: NSViewRepresentable {
    @Binding var focused: Bool
    let onKey: (NSEvent) -> Bool
    let menu: () -> NSMenu
    /// Rows to look back (negative: towards the live screen).
    var scroll: (Int) -> Void = { _ in }

    func makeNSView(context: Context) -> KeyView {
        let view = KeyView()
        updateNSView(view, context: context)
        view.onFocus = { focused = $0 }
        return view
    }

    func updateNSView(_ view: KeyView, context: Context) {
        view.onKey = onKey
        view.makeMenu = menu
        view.onScroll = scroll
        if TerminalFocus.requested {
            TerminalFocus.requested = false
            DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        }
    }

    final class KeyView: NSView {
        var onKey: ((NSEvent) -> Bool)?
        var onFocus: ((Bool) -> Void)?
        var makeMenu: (() -> NSMenu)?
        var onScroll: ((Int) -> Void)?
        /// Wheel and trackpad deltas not yet a whole row.
        private var pending: CGFloat = 0

        /// The wheel or a trackpad looks back through the history.
        override func scrollWheel(with event: NSEvent) {
            let rowHeight: CGFloat = 15
            pending += event.hasPreciseScrollingDeltas ? event.scrollingDeltaY / rowHeight : event.scrollingDeltaY * 3
            let rows = Int(pending.rounded(.towardZero))
            guard rows != 0 else { return }
            pending -= CGFloat(rows)
            onScroll?(rows)
        }

        override func menu(for event: NSEvent) -> NSMenu? { makeMenu?() }

        override var acceptsFirstResponder: Bool { true }

        override func becomeFirstResponder() -> Bool {
            onFocus?(true)
            return true
        }

        override func resignFirstResponder() -> Bool {
            onFocus?(false)
            return true
        }

        override func mouseDown(with event: NSEvent) {
            window?.makeFirstResponder(self)
        }

        override func keyDown(with event: NSEvent) {
            if onKey?(event) != true { super.keyDown(with: event) }
        }

        override func performKeyEquivalent(with event: NSEvent) -> Bool {
            guard window?.firstResponder === self else { return false }
            return onKey?(event) ?? false
        }
    }
}
