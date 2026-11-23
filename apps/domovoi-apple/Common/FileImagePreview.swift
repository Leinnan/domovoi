import App
import SwiftUI

/// An image file in the Files tool: it fits the pane and zooms in place; on
/// iPhone a tap shows it full screen, on the Mac its corner button does.
/// Share, Copy and Save are on its long press (and, through `file`, in the
/// screen's own menu).
struct FileImagePreview: View {
    let address: String
    let path: String
    /// Its bytes once loaded, for the screen's menu.
    var file: Binding<ImageFile?> = .constant(nil)
    @State private var picture: PlatformImage?
    @State private var loaded: ImageFile?
    @State private var failed = false
    @State private var attempt = 0
    @State private var gallery: ImageGallery?
    @State private var notice: ImageNotice?

    private var name: String { (path as NSString).lastPathComponent }

    var body: some View {
        Group {
            if let picture {
                shown(picture)
            } else if failed {
                failure
            } else {
                ProgressView()
            }
        }
        .padding(Space.s)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .imageNotice($notice)
        .imageGallery($gallery)
        .task(id: "\(address)\u{0}\(attempt)") {
            picture = nil
            loaded = nil
            file.wrappedValue = nil
            failed = false
            guard let url = URL(string: address) else { failed = true; return }
            do {
                let bytes = try await ImageStore.shared.file(url)
                picture = try await ImageStore.shared.picture(url)
                loaded = bytes
                file.wrappedValue = bytes
            } catch {
                if !Task.isCancelled { failed = true }
            }
        }
    }

    private func shown(_ picture: PlatformImage) -> some View {
        zoomable(picture)
            .accessibilityLabel(Text(path))
            .accessibilityAddTraits(.isImage)
            .accessibilityAction(named: "Show Full Size", openFull)
            .contextMenu {
                if let loaded {
                    ImageActionItems(file: loaded, name: name, preview: picture, notice: $notice)
                }
            }
    }

    @ViewBuilder
    private func zoomable(_ picture: PlatformImage) -> some View {
        #if os(iOS)
        ZoomingImage(image: picture, onTap: openFull)
        #else
        ZoomingImage(image: picture)
            .overlay(alignment: .topTrailing) {
                Button(action: openFull) {
                    Image(systemName: "arrow.up.left.and.arrow.down.right")
                        .frame(width: Size.chip, height: Size.chip)
                        .glassPanel(in: Circle())
                }
                .buttonStyle(.plain)
                .padding(Space.s)
                .help("Show Full Size")
                .accessibilityLabel("Show Full Size")
            }
        #endif
    }

    private func openFull() {
        gallery = ImageGallery(images: [ImageView(name: name, url: address)], start: 0)
    }

    private var failure: some View {
        VStack(spacing: Space.m) {
            Label("Couldn’t load image preview.", systemImage: "photo")
                .foregroundStyle(.secondary)
            Button("Try Again") { attempt += 1 }
        }
    }
}
