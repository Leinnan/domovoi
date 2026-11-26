import App
import SwiftUI
import UIKit

/// Terminals on iPhone: the core emulates the screen; this view draws it,
/// fits rows and columns to the space above the keyboard, and sends keys from
/// the software keyboard plus a row of terminal keys.
struct TerminalScreenIOS: View {
    @ObservedObject var core: Core
    @State private var typing = false
    /// The screen's text in a sheet, to select part of it.
    @State private var selecting: String?
    /// 11 points at the default text size, following the user's setting.
    @ScaledMetric(relativeTo: .caption) private var baseSize: CGFloat = 11
    /// The terminal's own text size, on top of the system's (Larger / Smaller Text).
    @AppStorage("terminalTextScale") private var textScale: Double = 1
    private var fontSize: CGFloat { baseSize * textScale }

    private var terminal: TerminalPanelView { core.view.panel.terminal }

    /// The screen's margin inside its frame: the text, the cursor and the
    /// rows and columns that fit all count from it.
    private let inset = Space.tight

    private var font: UIFont { UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular) }
    private var cell: CGSize {
        let size = ("M" as NSString).size(withAttributes: [.font: font])
        return CGSize(width: ceil(size.width * 100) / 100, height: ceil(font.lineHeight))
    }

    var body: some View {
        VStack(spacing: 0) {
            if terminal.tabs.count > 1 {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: Space.s) {
                        ForEach(terminal.tabs, id: \.id) { tab in
                            Button { core.update(.selectTerminal(terminalId: tab.id)) } label: {
                                HStack(spacing: Space.xs) {
                                    Circle().fill(tab.running ? Tint.success : Color.secondary).frame(width: Size.dot, height: Size.dot)
                                    Text(tab.label)
                                }
                                .font(TypeRole.meta)
                                .padding(.horizontal, Space.m)
                                .frame(minHeight: Size.chip)
                                .glassChip(selected: tab.active)
                                .contentShape(Capsule())
                            }
                            .buttonStyle(.plain)
                            .accessibilityAddTraits(tab.active ? .isSelected : [])
                        }
                    }
                    .padding(Space.s)
                }
                Divider()
            }
            if let screen = terminal.active {
                GeometryReader { geo in
                    ZStack(alignment: .topLeading) {
                        Color(uiColor: .systemBackground)
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(screen.lines.enumerated()), id: \.offset) { _, line in
                                Text(attributed(line))
                                    .font(Font(font))
                                    .frame(height: cell.height, alignment: .leading)
                                    .fixedSize()
                            }
                        }
                        .padding(inset)
                        if screen.cursorVisible {
                            Rectangle()
                                .fill(Color.accentColor.opacity(typing ? 0.7 : 0.35))
                                .frame(width: cell.width, height: cell.height)
                                .offset(x: inset + CGFloat(screen.cursorCol) * cell.width, y: inset + CGFloat(screen.cursorRow) * cell.height)
                        }
                        TerminalKeyInput(active: $typing, cellHeight: cell.height, readOnly: screen.watching || screen.exited) { key, text, ctrl, alt in
                            core.update(.terminalKey(key: key, text: text, ctrl: ctrl, alt: alt))
                        } paste: { text in
                            core.update(.terminalPaste(text: text))
                        } scroll: { lines in
                            core.update(.terminalScroll(lines: Int32(lines)))
                        }
                        .frame(width: geo.size.width, height: geo.size.height)
                        // Looking back through the history: the way down.
                        if screen.scrollbackOffset > 0 {
                            Button {
                                core.update(.terminalScroll(lines: -Int32(screen.scrollbackOffset)))
                            } label: {
                                Label("\(screen.scrollbackOffset) of \(max(screen.scrollbackMax, screen.scrollbackOffset)) lines back", systemImage: "arrow.down.to.line")
                                    .font(TypeRole.meta.weight(.medium))
                                    .padding(.horizontal, Space.m)
                                    .frame(minHeight: Size.chip)
                                    .glassChip()
                            }
                            .buttonStyle(.plain)
                            .padding(Space.s)
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                            .accessibilityLabel("Back to the live screen")
                        }
                        if screen.exited {
                            // A chip like the tabs and the way back down.
                            HStack(spacing: Space.s) {
                                Text(screen.hasSavedScreen ? "Last saved screen" : screen.restored ? "Ended after server restart" : "Process exited").font(TypeRole.meta)
                                Button("New Terminal") { core.update(.newTerminal) }
                                    .font(TypeRole.meta.weight(.semibold))
                                    .buttonStyle(.borderless)
                            }
                            .padding(.horizontal, Space.m)
                            .frame(minHeight: Size.chip)
                            .glassChip()
                            .padding(Space.s)
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                        }
                    }
                    .clipped()
                    .onAppear { resize(geo.size, screen) }
                    .onChange(of: geo.size) { _, size in resize(size, screen) }
                    .onChange(of: screen.id) { _, _ in resize(geo.size, screen) }
                    .onChange(of: screen.watching) { _, _ in resize(geo.size, screen) }
                    .onChange(of: fontSize) { _, _ in resize(geo.size, screen) }
                }
            } else {
                ContentUnavailableView("No terminal", systemImage: "terminal", description: Text("Opening one in the thread's folder…"))
            }
        }
        .navigationTitle("Terminal")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            Button { core.update(.newTerminal) } label: { Image(systemName: "plus") }
                .accessibilityLabel("New terminal")
            if let id = terminal.active?.id {
                Menu {
                    if let screen = terminal.active {
                        if !screen.exited {
                            Button(screen.watching ? "Interact" : "Watch") {
                                typing = false
                                core.update(.setTerminalWatch(watching: !screen.watching))
                            }
                        }
                        Button("Copy Screen", systemImage: "doc.on.doc") { Platform.copy(screen.plainText) }
                        Button("Select Text…", systemImage: "selection.pin.in.out") { selecting = screen.plainText }
                    }
                    if terminal.active?.exited == true {
                        Button("Open fresh shell", systemImage: "terminal") { core.update(.reopenTerminal) }
                    }
                    Button("Clear", systemImage: "eraser") { core.update(.clearTerminal) }
                        .disabled(terminal.active?.watching != false || terminal.active?.exited != false)
                    Section("Text Size") {
                        Button("Larger", systemImage: "textformat.size.larger") { textScale = min(2, textScale + 0.15) }
                            .disabled(textScale >= 2)
                        Button("Smaller", systemImage: "textformat.size.smaller") { textScale = max(0.6, textScale - 0.15) }
                            .disabled(textScale <= 0.6)
                        if textScale != 1 { Button("Default", systemImage: "arrow.counterclockwise") { textScale = 1 } }
                    }
                    Button("Close Terminal", systemImage: "xmark", role: .destructive) { core.update(.closeTerminal(terminalId: id)) }
                } label: { Image(systemName: "ellipsis.circle") }
                .accessibilityLabel("Terminal actions")
            }
        }
        .onAppear { core.update(.showPanel(tab: "terminal")) }
        .sheet(item: Binding(get: { selecting.map(SelectableText.init) }, set: { selecting = $0?.text })) { item in
            SelectTextSheet(text: item.text)
        }
    }

    private func resize(_ size: CGSize, _ screen: TerminalScreenView) {
        // A size mid-layout can be infinite or NaN, which a UInt32 can't hold.
        guard size.width.isFinite, size.height.isFinite else { return }
        let cols = UInt32(min(max(20, (size.width - 2 * inset) / cell.width), 1000))
        let rows = UInt32(min(max(4, (size.height - 2 * inset) / cell.height), 500))
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
            if span.bold { piece.font = Font(UIFont.monospacedSystemFont(ofSize: fontSize, weight: .bold)) }
            if span.underline { piece.underlineStyle = .single }
            out.append(piece)
        }
        return out
    }
}

/// An invisible responder over the screen: a tap shows the keyboard, typed
/// characters, the accessory row's keys and a hardware keyboard's arrows,
/// Esc, Tab, Control and Option go to the terminal.
struct TerminalKeyInput: UIViewRepresentable {
    @Binding var active: Bool
    /// One row's height: a drag moves through the history a row at a time.
    var cellHeight: CGFloat = 14
    var readOnly = false
    /// (key, text, ctrl, alt)
    let key: (String, String, Bool, Bool) -> Void
    let paste: (String) -> Void
    /// Rows to look back (negative: towards the live screen).
    var scroll: (Int) -> Void = { _ in }

    func makeUIView(context: Context) -> KeyView {
        let view = KeyView()
        view.onKey = key
        view.onPaste = paste
        view.onScroll = scroll
        view.cellHeight = cellHeight
        view.readOnly = readOnly
        view.onActive = { active in DispatchQueue.main.async { self.active = active } }
        return view
    }

    func updateUIView(_ view: KeyView, context: Context) {
        view.onKey = key
        view.onPaste = paste
        view.onScroll = scroll
        view.cellHeight = cellHeight
        view.readOnly = readOnly
        if readOnly && view.isFirstResponder { _ = view.resignFirstResponder() }
    }

    final class KeyView: UIView, UIKeyInput {
        var onKey: (String, String, Bool, Bool) -> Void = { _, _, _, _ in }
        var onPaste: (String) -> Void = { _ in }
        var onActive: (Bool) -> Void = { _ in }
        var onScroll: (Int) -> Void = { _ in }
        var cellHeight: CGFloat = 14
        var readOnly = false
        /// The drag's rows already sent.
        private var scrolledRows = 0
        private var ctrl = false
        private var ctrlButton: UIButton?

        override init(frame: CGRect) {
            super.init(frame: frame)
            backgroundColor = .clear
            addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(tapped)))
            let pan = UIPanGestureRecognizer(target: self, action: #selector(panned(_:)))
            pan.allowedScrollTypesMask = .continuous
            addGestureRecognizer(pan)
        }

        /// Dragging down looks back through the history, as scrolling does.
        @objc private func panned(_ pan: UIPanGestureRecognizer) {
            switch pan.state {
            case .began: scrolledRows = 0
            case .changed:
                let rows = Int((pan.translation(in: self).y / max(cellHeight, 1)).rounded(.towardZero))
                if rows != scrolledRows {
                    onScroll(rows - scrolledRows)
                    scrolledRows = rows
                }
            default: break
            }
        }

        required init?(coder: NSCoder) { fatalError() }

        @objc private func tapped() {
            if isFirstResponder { _ = resignFirstResponder() } else { _ = becomeFirstResponder() }
        }

        override var canBecomeFirstResponder: Bool { !readOnly }

        // ⌘V on a hardware keyboard, and Paste from the edit menu.
        override func paste(_ sender: Any?) {
            if let text = UIPasteboard.general.string { onPaste(text) }
        }

        override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
            if action == #selector(paste(_:)) { return UIPasteboard.general.hasStrings }
            return super.canPerformAction(action, withSender: sender)
        }

        override func becomeFirstResponder() -> Bool {
            let ok = super.becomeFirstResponder()
            if ok { onActive(true) }
            return ok
        }

        override func resignFirstResponder() -> Bool {
            let ok = super.resignFirstResponder()
            if ok { onActive(false) }
            return ok
        }

        // Plain keyboard: no autocorrect, capitals or smart punctuation.
        var autocorrectionType: UITextAutocorrectionType = .no
        var autocapitalizationType: UITextAutocapitalizationType = .none
        var spellCheckingType: UITextSpellCheckingType = .no
        var smartQuotesType: UITextSmartQuotesType = .no
        var smartDashesType: UITextSmartDashesType = .no
        var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
        var keyboardType: UIKeyboardType = .asciiCapable

        var hasText: Bool { true }

        func insertText(_ text: String) {
            if text == "\n" {
                onKey("enter", "", false, false)
            } else if ctrl {
                onKey("char", text.lowercased(), true, false)
                setCtrl(false)
            } else {
                onKey("char", text, false, false)
            }
        }

        func deleteBackward() {
            onKey("backspace", "", false, false)
        }

        // A hardware keyboard: keys the software keyboard has no way to type.
        override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            var handled = false
            for press in presses {
                guard let key = press.key else { continue }
                let control = key.modifierFlags.contains(.control)
                let option = key.modifierFlags.contains(.alternate)
                let named: String? = switch key.keyCode {
                case .keyboardUpArrow: "up"
                case .keyboardDownArrow: "down"
                case .keyboardLeftArrow: "left"
                case .keyboardRightArrow: "right"
                case .keyboardEscape: "escape"
                case .keyboardTab: "tab"
                case .keyboardHome: "home"
                case .keyboardEnd: "end"
                case .keyboardPageUp: "pageup"
                case .keyboardPageDown: "pagedown"
                case .keyboardDeleteForward: "delete"
                default: nil
                }
                if let named {
                    onKey(named, "", control, option)
                    handled = true
                } else if control || option, !key.charactersIgnoringModifiers.isEmpty {
                    onKey("char", key.charactersIgnoringModifiers.lowercased(), control, option)
                    handled = true
                }
            }
            // Everything else (plain typing) arrives through `insertText`.
            if !handled { super.pressesBegan(presses, with: event) }
        }

        /// Built once: a new bar on every read left the Control key's
        /// highlight on a bar no longer shown.
        override var inputAccessoryView: UIView? { accessoryBar }

        /// (title, what it sends, what VoiceOver says). `char:` keys type
        /// their character; the rest are named keys.
        private static let keys: [(String, String, String)] = [
            ("esc", "escape", "Escape"), ("ctrl", "ctrl", "Control"), ("tab", "tab", "Tab"), ("^C", "interrupt", "Control C"),
            ("←", "left", "Left arrow"), ("↓", "down", "Down arrow"), ("↑", "up", "Up arrow"), ("→", "right", "Right arrow"),
            ("|", "char:|", "Pipe"), ("~", "char:~", "Tilde"), ("/", "char:/", "Slash"), ("-", "char:-", "Hyphen"),
            ("⎘", "paste", "Paste"),
        ]

        /// Scrolls sideways when the keys don't all fit; keys a tap target
        /// tall, with text that follows the user's size.
        private lazy var accessoryBar: UIView = {
            let margin = Space.xs
            let container = UIInputView(frame: CGRect(x: 0, y: 0, width: 320, height: Size.tap + 2 * margin), inputViewStyle: .keyboard)
            let scroll = UIScrollView()
            scroll.showsHorizontalScrollIndicator = false
            scroll.translatesAutoresizingMaskIntoConstraints = false
            let bar = UIStackView()
            bar.axis = .horizontal
            bar.spacing = Space.tight
            bar.translatesAutoresizingMaskIntoConstraints = false
            let font = UIFontMetrics(forTextStyle: .body).scaledFont(for: .monospacedSystemFont(ofSize: 15, weight: .medium), maximumPointSize: 22)
            for (title, name, spoken) in Self.keys {
                let button = UIButton(type: .system)
                button.setTitle(title, for: .normal)
                button.titleLabel?.font = font
                button.titleLabel?.adjustsFontForContentSizeCategory = true
                button.backgroundColor = .tertiarySystemBackground
                button.layer.cornerRadius = Radius.chip
                button.accessibilityLabel = spoken
                button.addAction(UIAction { [weak self] _ in self?.press(name) }, for: .touchUpInside)
                button.widthAnchor.constraint(greaterThanOrEqualToConstant: Size.tap).isActive = true
                button.contentEdgeInsets = UIEdgeInsets(top: 0, left: Space.m, bottom: 0, right: Space.m)
                if name == "ctrl" { ctrlButton = button }
                bar.addArrangedSubview(button)
            }
            container.addSubview(scroll)
            scroll.addSubview(bar)
            NSLayoutConstraint.activate([
                scroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                scroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
                scroll.topAnchor.constraint(equalTo: container.topAnchor),
                scroll.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                bar.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: Space.tight),
                bar.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -Space.tight),
                bar.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: margin),
                bar.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -margin),
                bar.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -2 * margin),
                bar.heightAnchor.constraint(greaterThanOrEqualToConstant: Size.tap),
            ])
            return container
        }()

        private func press(_ name: String) {
            switch name {
            case "ctrl": setCtrl(!ctrl)
            case "paste":
                if let text = UIPasteboard.general.string { onPaste(text) }
            case "interrupt": onKey("char", "c", true, false)
            case let typed where typed.hasPrefix("char:"):
                insertText(String(typed.dropFirst(5)))
            default:
                // Control carries over to arrows and Tab too (⌃← and the like).
                onKey(name, "", ctrl, false)
                setCtrl(false)
            }
        }

        private func setCtrl(_ on: Bool) {
            ctrl = on
            ctrlButton?.backgroundColor = on ? .tintColor.withAlphaComponent(0.35) : .tertiarySystemBackground
            ctrlButton?.accessibilityTraits = on ? [.button, .selected] : .button
            ctrlButton?.accessibilityValue = on ? "On" : "Off"
        }
    }
}

/// `sheet(item:)` needs Identifiable.
private struct SelectableText: Identifiable {
    let text: String
    var id: String { text }
}
