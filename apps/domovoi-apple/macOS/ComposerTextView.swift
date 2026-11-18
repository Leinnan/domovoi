import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Keys the composer can take over from the field: the mention list's
/// navigation, ⇧⇥ for plan mode, Escape.
enum ComposerKey {
    case up, down, tab, backtab, returnKey, escape
}

/// The message field: an `NSTextView` that grows with its text, sends on
/// Return (never while an input method is still composing), inserts a line
/// on ⇧↩ or ⌥↩, hands images pasted or dropped on it to the composer, and
/// draws its own placeholder.
///
/// Its height comes from `sizeThatFits`, in the same layout pass as the edit:
/// no first frame at a guessed height, and no frame where a new line has
/// scrolled the text before the field has grown. Text and placeholder start
/// at the field's edge (no padding), so they line up with what's below.
struct ComposerTextView: NSViewRepresentable {
    @Binding var text: String
    var placeholder: String
    /// Changing this puts the cursor in the field.
    var focusToken: Int
    var minLines = 1
    var maxLines = 12
    /// A key the composer handles; true when it did.
    var handleKey: (ComposerKey) -> Bool
    var submit: () -> Void
    /// Images from a paste or a drop; true when there were any.
    var takeImages: (NSPasteboard) -> Bool

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        // Set up as `NSTextView.scrollableTextView()` does: the text view
        // fills the scroll view's width and grows down. TextKit 1 from the
        // start: measuring needs its layout manager, and asking an existing
        // TextKit 2 view for one swaps its whole text system mid-life.
        let scroll = FieldScrollView()
        let size = scroll.contentSize
        let textView = FieldTextView(usingTextLayoutManager: false)
        textView.frame = NSRect(origin: .zero, size: size)
        textView.minSize = NSSize(width: 0, height: size.height)
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.containerSize = NSSize(width: size.width, height: CGFloat.greatestFiniteMagnitude)
        textView.delegate = context.coordinator
        textView.coordinator = context.coordinator
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.drawsBackground = false
        textView.font = Self.font
        textView.textColor = .labelColor
        textView.insertionPointColor = .controlAccentColor
        textView.textContainerInset = .zero
        textView.textContainer?.lineFragmentPadding = 0
        textView.textContainer?.widthTracksTextView = true
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        // Code and paths: no curly quotes or long dashes.
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.smartInsertDeleteEnabled = false
        textView.placeholder = placeholder
        textView.string = text
        textView.setAccessibilityLabel("Message")

        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.documentView = textView
        context.coordinator.textView = textView
        DispatchQueue.main.async { textView.window?.makeFirstResponder(textView) }
        return scroll
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView, context: Context) -> CGSize? {
        // Wide enough for the placeholder when asked for an ideal size.
        let width = proposal.width ?? 320
        return CGSize(width: width, height: context.coordinator.fittingHeight(width: width))
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        guard let textView = coordinator.textView else { return }
        // Never replace text an input method is still composing.
        if textView.string != text, !textView.hasMarkedText() {
            textView.string = text
            textView.setSelectedRange(NSRange(location: (text as NSString).length, length: 0))
            coordinator.resized()
        }
        if textView.placeholder != placeholder {
            textView.placeholder = placeholder
            textView.needsDisplay = true
        }
        if coordinator.focusToken != focusToken {
            coordinator.focusToken = focusToken
            DispatchQueue.main.async { textView.window?.makeFirstResponder(textView) }
        }
    }

    static let font = NSFont.preferredFont(forTextStyle: .body)
    /// One line of `font`, as the field's layout manager sets it.
    static let lineHeight = NSLayoutManager().defaultLineHeight(for: font)

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: ComposerTextView
        weak var textView: FieldTextView?
        var focusToken: Int

        /// A layout of its own for measuring, so asking for a size never
        /// disturbs the field (SwiftUI may ask about widths it won't use).
        private let measureStorage = NSTextStorage()
        private let measureLayout = NSLayoutManager()
        private let measureContainer = NSTextContainer()

        init(_ parent: ComposerTextView) {
            self.parent = parent
            self.focusToken = parent.focusToken
            super.init()
            measureContainer.lineFragmentPadding = 0
            measureLayout.addTextContainer(measureContainer)
            measureStorage.addLayoutManager(measureLayout)
        }

        /// The field's height at `width`: its text (an input method's marked
        /// text included), between `minLines` and `maxLines` lines.
        func fittingHeight(width: CGFloat) -> CGFloat {
            let line = ComposerTextView.lineHeight
            let low = line * CGFloat(parent.minLines)
            let high = line * CGFloat(parent.maxLines)
            let inset = (textView?.textContainerInset.height ?? 0) * 2
            guard let textView, let storage = textView.textStorage, storage.length > 0 else { return (low + inset).rounded(.up) }
            measureStorage.setAttributedString(storage)
            measureContainer.size = NSSize(width: max(width - textView.textContainerInset.width * 2, 1), height: .greatestFiniteMagnitude)
            // Past `maxLines` the field scrolls: no need to lay out the rest.
            measureLayout.ensureLayout(forBoundingRect: NSRect(x: 0, y: 0, width: measureContainer.size.width, height: high + line), in: measureContainer)
            let used = measureLayout.usedRect(for: measureContainer).height
            return (min(max(used, low), high) + inset).rounded(.up)
        }

        /// The text changed what the field needs: SwiftUI lays it out again
        /// before the next frame is drawn.
        func resized() {
            textView?.enclosingScrollView?.invalidateIntrinsicContentSize()
        }

        func textDidChange(_ notification: Notification) {
            guard let textView else { return }
            textView.needsDisplay = true
            resized()
            // While composing, the marked text isn't the message yet.
            if !textView.hasMarkedText(), parent.text != textView.string {
                parent.text = textView.string
            }
        }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            // An input method's own Return (committing a candidate) never
            // reaches here; these are the field's keys.
            switch selector {
            case #selector(NSResponder.insertNewline(_:)):
                let flags = NSApp.currentEvent?.modifierFlags ?? []
                if flags.contains(.shift) || flags.contains(.option) {
                    textView.insertNewlineIgnoringFieldEditor(nil)
                    return true
                }
                if parent.handleKey(.returnKey) { return true }
                parent.text = textView.string
                parent.submit()
                return true
            case #selector(NSResponder.insertTab(_:)):
                if parent.handleKey(.tab) { return true }
                textView.window?.selectNextKeyView(nil)
                return true
            case #selector(NSResponder.insertBacktab(_:)):
                return parent.handleKey(.backtab)
            case #selector(NSResponder.moveUp(_:)):
                return parent.handleKey(.up)
            case #selector(NSResponder.moveDown(_:)):
                return parent.handleKey(.down)
            case #selector(NSResponder.cancelOperation(_:)):
                return parent.handleKey(.escape)
            default:
                return false
            }
        }
    }
}

/// The field's text view: its placeholder, and images on paste and drop.
final class FieldTextView: NSTextView {
    weak var coordinator: ComposerTextView.Coordinator?
    var placeholder = ""

    /// Marked text (an input method composing) can add a line before the
    /// message changes.
    override func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        super.setMarkedText(string, selectedRange: selectedRange, replacementRange: replacementRange)
        coordinator?.resized()
    }

    override func paste(_ sender: Any?) {
        let board = NSPasteboard.general
        // Copied text pastes as text even when an image rides along (rich
        // text from a browser); only an image on its own attaches.
        if board.string(forType: .string) == nil, coordinator?.parent.takeImages(board) == true { return }
        super.paste(sender)
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        ImagePasteboard.hasImages(sender.draggingPasteboard) ? .copy : super.draggingEntered(sender)
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        if ImagePasteboard.hasImages(sender.draggingPasteboard), coordinator?.parent.takeImages(sender.draggingPasteboard) == true { return true }
        return super.performDragOperation(sender)
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard string.isEmpty, !hasMarkedText(), !placeholder.isEmpty else { return }
        let padding = textContainer?.lineFragmentPadding ?? 0
        let origin = NSPoint(x: textContainerOrigin.x + padding, y: textContainerOrigin.y)
        let attributes: [NSAttributedString.Key: Any] = [.font: font ?? ComposerTextView.font, .foregroundColor: NSColor.placeholderTextColor]
        (placeholder as NSString).draw(at: origin, withAttributes: attributes)
    }
}

/// The field's scroll view: while the text fits it never stays scrolled (a
/// new line scrolls the caret into view a moment before the field grows).
final class FieldScrollView: NSScrollView {
    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        guard let document = documentView else { return }
        if document.frame.height <= contentView.bounds.height + 0.5, contentView.bounds.origin.y != 0 {
            contentView.scroll(to: .zero)
            reflectScrolledClipView(contentView)
        }
    }
}

/// Images on a pasteboard: image files, or image data (a screenshot, a
/// picture dragged out of a browser).
enum ImagePasteboard {
    private static let imageFiles: [NSPasteboard.ReadingOptionKey: Any] = [
        .urlReadingFileURLsOnly: true,
        .urlReadingContentsConformToTypes: [UTType.image.identifier],
    ]
    private static let dataTypes: [NSPasteboard.PasteboardType] = [.png, .tiff, NSPasteboard.PasteboardType(UTType.jpeg.identifier)]

    static func hasImages(_ board: NSPasteboard) -> Bool {
        board.canReadObject(forClasses: [NSURL.self], options: imageFiles) || board.availableType(from: dataTypes) != nil
    }

    /// Each image as its data and a name.
    static func read(_ board: NSPasteboard) -> [(data: @Sendable () -> Data?, name: String)] {
        if let urls = board.readObjects(forClasses: [NSURL.self], options: imageFiles) as? [URL], !urls.isEmpty {
            return urls.map { url in ({ try? Data(contentsOf: url) }, url.lastPathComponent) }
        }
        if let type = board.availableType(from: dataTypes), let data = board.data(forType: type) {
            return [({ data }, "pasted image")]
        }
        return []
    }
}
