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

/// What a native action did, for the core's toast (`ShowNotice`).
struct Notice {
    /// `success` | `error` | `info`
    let kind: String
    let message: String

    static func success(_ message: String) -> Notice { Notice(kind: "success", message: message) }
    static func error(_ message: String) -> Notice { Notice(kind: "error", message: message) }
}

/// Hands notices to the core, which shows them in the same toast as its own.
struct NoticeSink {
    /// Also read by full-screen covers and sheets, which show the toast themselves.
    let core: Core?

    @MainActor func callAsFunction(_ notice: Notice) {
        core?.update(.showNotice(kind: notice.kind, message: notice.message))
    }
}

extension EnvironmentValues {
    /// Where native actions (copy, save, share) report how they went.
    @Entry var notice = NoticeSink(core: nil)
}

@MainActor
enum ImageActions {
    /// Kinds other apps paste as they are; others go as PNG.
    private static let pastable: [UTType] = [.png, .jpeg, .heic, .gif, .tiff]

    static func copy(_ file: ImageFile) -> Notice {
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
            return .error("Couldn’t copy the image.")
        }
        #endif
        return .success("Image copied")
    }

    #if os(iOS)
    /// Adds the image to Photos with add-only access (the picker never shows
    /// the library). The original bytes go, so the format and its metadata stay.
    static func saveToPhotos(_ file: ImageFile) async -> Notice {
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else {
            return .error("Domovoi can’t add to Photos. Allow it in Settings > Privacy & Security > Photos.")
        }
        let data = pastable.contains(file.type) ? file.data : (UIImage(data: file.data)?.pngData() ?? file.data)
        do {
            try await addToPhotos(data)
            Platform.answered(true)
            return .success("Saved to Photos")
        } catch {
            return .error("Couldn’t save to Photos.")
        }
    }

    /// Photos runs the change block on its own queue. Made here, outside the
    /// main actor, so Swift 6 doesn't take it for main-actor code and trap
    /// when Photos calls it off the main thread.
    nonisolated private static func addToPhotos(_ data: Data) async throws {
        try await PHPhotoLibrary.shared().performChanges { @Sendable in
            PHAssetCreationRequest.forAsset().addResource(with: .photo, data: data, options: nil)
        }
    }
    #else
    /// Writes the original bytes where the user picks; nil when cancelled.
    static func saveAs(_ file: ImageFile, name: String) async -> Notice? {
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
            return .success("Saved \(url.lastPathComponent)")
        } catch {
            return .error("Couldn’t save: \(error.localizedDescription)")
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
    @Environment(\.notice) private var notice

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
        Button("Copy Image", systemImage: "doc.on.doc") { notice(ImageActions.copy(file)) }
        #if os(iOS)
        Button("Save to Photos", systemImage: "square.and.arrow.down") {
            Task { notice(await ImageActions.saveToPhotos(file)) }
        }
        #else
        Button("Save As…", systemImage: "square.and.arrow.down") {
            Task { if let saved = await ImageActions.saveAs(file, name: name) { notice(saved) } }
        }
        #endif
    }
}
