import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#else
import Photos
import UIKit
#endif

/// An image to share: its own bytes and type (not the server's link, which
/// expires and opens nowhere else), so the share sheet offers Save Image,
/// Copy, AirDrop and Messages.
struct SharedImage: Transferable {
    let file: ImageFile
    let name: String

    var fileName: String { file.fileName(name) }

    static var transferRepresentation: some TransferRepresentation {
        DataRepresentation(exportedContentType: .png) { $0.file.data }
            .exportingCondition { $0.file.type == .png }
            .suggestedFileName { $0.fileName }
        DataRepresentation(exportedContentType: .jpeg) { $0.file.data }
            .exportingCondition { $0.file.type == .jpeg }
            .suggestedFileName { $0.fileName }
        DataRepresentation(exportedContentType: .heic) { $0.file.data }
            .exportingCondition { $0.file.type == .heic }
            .suggestedFileName { $0.fileName }
        DataRepresentation(exportedContentType: .gif) { $0.file.data }
            .exportingCondition { $0.file.type == .gif }
            .suggestedFileName { $0.fileName }
        DataRepresentation(exportedContentType: .webP) { $0.file.data }
            .exportingCondition { $0.file.type == .webP }
            .suggestedFileName { $0.fileName }
        DataRepresentation(exportedContentType: .tiff) { $0.file.data }
            .exportingCondition { $0.file.type == .tiff }
            .suggestedFileName { $0.fileName }
        // Any other kind still goes as an image.
        DataRepresentation(exportedContentType: .image) { $0.file.data }
            .suggestedFileName { $0.fileName }
    }
}

/// What an image action did, shown for a moment over the images.
struct ImageNotice: Identifiable, Equatable {
    let id = UUID()
    let text: String
    var failed = false
    /// Photos access was refused: the notice offers Settings.
    var opensSettings = false
}

@MainActor
enum ImageActions {
    /// Kinds other apps paste as they are; others go as PNG.
    private static let pastable: [UTType] = [.png, .jpeg, .heic, .gif, .tiff]

    static func copy(_ file: ImageFile) -> ImageNotice {
        #if os(macOS)
        let board = NSPasteboard.general
        board.clearContents()
        let item = NSPasteboardItem()
        item.setData(file.data, forType: NSPasteboard.PasteboardType(file.type.identifier))
        // TIFF too: what most Mac apps read an image from.
        if file.type != .tiff, let tiff = NSImage(data: file.data)?.tiffRepresentation {
            item.setData(tiff, forType: .tiff)
        }
        board.writeObjects([item])
        #else
        if pastable.contains(file.type) {
            UIPasteboard.general.setItems([[file.type.identifier: file.data]])
        } else if let png = UIImage(data: file.data)?.pngData() {
            UIPasteboard.general.setItems([[UTType.png.identifier: png]])
        } else {
            return ImageNotice(text: "Couldn’t copy the image.", failed: true)
        }
        #endif
        return ImageNotice(text: "Image copied")
    }

    #if os(iOS)
    /// Adds the image to Photos with add-only access (the picker never shows
    /// the library). The original bytes go, so the format and its metadata stay.
    static func saveToPhotos(_ file: ImageFile) async -> ImageNotice {
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else {
            return ImageNotice(text: "Domovoi can’t add to Photos.", failed: true, opensSettings: true)
        }
        let data = pastable.contains(file.type) ? file.data : (UIImage(data: file.data)?.pngData() ?? file.data)
        do {
            try await PHPhotoLibrary.shared().performChanges {
                PHAssetCreationRequest.forAsset().addResource(with: .photo, data: data, options: nil)
            }
            Platform.answered(true)
            return ImageNotice(text: "Saved to Photos")
        } catch {
            return ImageNotice(text: "Couldn’t save to Photos.", failed: true)
        }
    }
    #else
    /// Writes the original bytes where the user picks; nil when cancelled.
    static func saveAs(_ file: ImageFile, name: String) async -> ImageNotice? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = file.fileName(name)
        panel.allowedContentTypes = [file.type]
        panel.canCreateDirectories = true
        let response: NSApplication.ModalResponse
        if let window = NSApp.keyWindow {
            response = await panel.beginSheetModal(for: window)
        } else {
            response = panel.runModal()
        }
        guard response == .OK, let url = panel.url else { return nil }
        do {
            try file.data.write(to: url, options: .atomic)
            return ImageNotice(text: "Saved \(url.lastPathComponent)")
        } catch {
            return ImageNotice(text: "Couldn’t save: \(error.localizedDescription)", failed: true)
        }
    }
    #endif
}

/// Share, Copy and Save for one image: for menus, context menus and
/// toolbars alike.
struct ImageActionItems: View {
    let file: ImageFile
    let name: String
    /// What the share sheet shows of it.
    let preview: PlatformImage?
    @Binding var notice: ImageNotice?

    var body: some View {
        let shared = SharedImage(file: file, name: name)
        if let preview {
            ShareLink(item: shared, preview: SharePreview(name, image: Image(platformImage: preview))) {
                Label("Share…", systemImage: "square.and.arrow.up")
            }
        } else {
            ShareLink(item: shared, preview: SharePreview(name)) {
                Label("Share…", systemImage: "square.and.arrow.up")
            }
        }
        Button("Copy Image", systemImage: "doc.on.doc") { notice = ImageActions.copy(file) }
        #if os(iOS)
        Button("Save to Photos", systemImage: "square.and.arrow.down") {
            Task { notice = await ImageActions.saveToPhotos(file) }
        }
        #else
        Button("Save As…", systemImage: "square.and.arrow.down") {
            Task { if let saved = await ImageActions.saveAs(file, name: name) { notice = saved } }
        }
        #endif
    }
}

extension View {
    /// Shows an image action's notice over this view for a moment.
    func imageNotice(_ notice: Binding<ImageNotice?>) -> some View {
        modifier(ImageNoticeCapsule(notice: notice))
    }
}

private struct ImageNoticeCapsule: ViewModifier {
    @Binding var notice: ImageNotice?
    @Environment(\.openURL) private var openURL

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .top) {
                if let shown = notice {
                    HStack(spacing: Space.s) {
                        Image(systemName: shown.failed ? "exclamationmark.octagon.fill" : "checkmark.circle.fill")
                            .foregroundStyle(shown.failed ? Tint.danger : Tint.success)
                        Text(shown.text).font(.footnote).lineLimit(3)
                        #if os(iOS)
                        if shown.opensSettings, let settings = URL(string: UIApplication.openSettingsURLString) {
                            Button("Settings") { openURL(settings) }
                                .font(.footnote.weight(.semibold))
                                .buttonStyle(.borderless)
                        }
                        #endif
                    }
                    .padding(.horizontal, Space.l)
                    .padding(.vertical, Space.s)
                    .glassPanel(in: Capsule())
                    .fixedSize()
                    .padding(.top, Space.s)
                    .transition(.opacity.combined(with: .scale(scale: 0.95)))
                    .accessibilityElement(children: .combine)
                    .task(id: shown.id) {
                        AccessibilityNotification.Announcement(shown.text).post()
                        try? await Task.sleep(for: .seconds(shown.opensSettings ? 5 : 2))
                        if !Task.isCancelled, notice?.id == shown.id { notice = nil }
                    }
                }
            }
            .animation(.snappy, value: notice)
    }
}
