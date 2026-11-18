import CoreImage
import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

enum Platform {
    static func copy(_ text: String) {
        #if os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        #else
        UIPasteboard.general.string = text
        #endif
    }

    /// A phone (narrow margins), not an iPad or a Mac.
    static var isPhone: Bool {
        #if os(iOS)
        UIDevice.current.userInterfaceIdiom == .phone
        #else
        false
        #endif
    }

    /// A light tap when a turn ends while the user is watching it.
    @MainActor static func turnFinished() {
        #if os(iOS)
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        #endif
    }

    /// Something needs the user (an approval, a question).
    @MainActor static func needsAttention() {
        #if os(iOS)
        UINotificationFeedbackGenerator().notificationOccurred(.warning)
        #else
        // Away in another app: the Dock icon bounces once.
        if !NSApp.isActive { NSApp.requestUserAttention(.informationalRequest) }
        #endif
    }

    /// How many threads wait for the user, on the Dock icon (Mac only).
    @MainActor static func setAttentionBadge(_ count: Int) {
        #if os(macOS)
        NSApp.dockTile.badgeLabel = count > 0 ? "\(count)" : nil
        #endif
    }

    /// An answer went out: confirmed (Allow, Submit) or a firmer tap
    /// (Decline, Send).
    @MainActor static func answered(_ approved: Bool) {
        #if os(iOS)
        if approved {
            UINotificationFeedbackGenerator().notificationOccurred(.success)
        } else {
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        }
        #endif
    }

    /// A tick when a choice changes (a mode, an effort level).
    @MainActor static func selectionChanged() {
        #if os(iOS)
        UISelectionFeedbackGenerator().selectionChanged()
        #endif
    }

    static func pasteboardText() -> String? {
        #if os(macOS)
        NSPasteboard.general.string(forType: .string)
        #else
        UIPasteboard.general.string
        #endif
    }
}

/// A QR code image for a pairing link (CoreImage).
struct QRCodeView: View {
    let text: String

    var body: some View {
        if let image = QRCodeView.render(text) {
            image.interpolation(.none).resizable().scaledToFit()
        } else {
            Image(systemName: "qrcode").resizable().scaledToFit().foregroundStyle(.secondary)
        }
    }

    static func render(_ text: String) -> Image? {
        guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        filter.setValue(Data(text.utf8), forKey: "inputMessage")
        filter.setValue("M", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
              let cg = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return Image(decorative: cg, scale: 1)
    }
}

extension View {
    /// A text-like button: `.link` on macOS, borderless on iOS.
    @ViewBuilder
    func linkButton() -> some View {
        #if os(macOS)
        buttonStyle(.link)
        #else
        buttonStyle(.borderless)
        #endif
    }
}

private struct OpenThreadKey: EnvironmentKey {
    static let defaultValue: (@MainActor (String) -> Void)? = nil
}

private struct SelectTextKey: EnvironmentKey {
    static let defaultValue: (@MainActor (String) -> Void)? = nil
}

private struct OpenDiffKey: EnvironmentKey {
    static let defaultValue: @MainActor () -> Void = {}
}

extension EnvironmentValues {
    /// Shows the diff after a checkpoint's "View diff" (iOS pushes a screen;
    /// macOS shows the side panel, which the core already opened).
    var openDiff: @MainActor () -> Void {
        get { self[OpenDiffKey.self] }
        set { self[OpenDiffKey.self] = newValue }
    }

    /// Shows a row's text to select from, in a sheet the transcript holds:
    /// a sheet on the row itself went when the row scrolled out of the lazy
    /// stack (or moved into a step group) while a reply streamed.
    var selectText: (@MainActor (String) -> Void)? {
        get { self[SelectTextKey.self] }
        set { self[SelectTextKey.self] = newValue }
    }

    /// Opens another thread (a subagent's, a fork's parent): iOS pushes its
    /// screen; unset, the row selects it (the Mac shows it in place).
    var openThread: (@MainActor (String) -> Void)? {
        get { self[OpenThreadKey.self] }
        set { self[OpenThreadKey.self] = newValue }
    }
}
