import SwiftUI
import ImageIO
import UIKit
import UniformTypeIdentifiers

/// "Domovoi" in the share sheet: text, a link or images become the draft of a new
/// thread, which the app opens the next time it comes to the front.
final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        let items = (extensionContext?.inputItems as? [NSExtensionItem]) ?? []
        let host = UIHostingController(rootView: ShareView(items: items, done: { [weak self] in
            self?.extensionContext?.completeRequest(returningItems: nil)
        }, cancel: { [weak self] in
            self?.extensionContext?.cancelRequest(withError: CocoaError(.userCancelled))
        }))
        addChild(host)
        host.view.frame = view.bounds
        host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(host.view)
        host.didMove(toParent: self)
    }
}

private struct ShareView: View {
    let items: [NSExtensionItem]
    let done: () -> Void
    let cancel: () -> Void
    /// What the agent should do with it.
    @State private var instructions = ""
    /// The text or link that was shared, kept apart from the instructions.
    @State private var shared = ""
    @State private var images: [UIImage] = []
    @State private var loading = true
    @State private var error: String?
    /// The app's last list: the server and its projects (nil before pairing).
    private let snapshot = WidgetSnapshot.load()
    @State private var projectId = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Instructions") {
                    TextField("What should the agent do with this?", text: $instructions, axis: .vertical)
                        .lineLimit(2...6)
                }
                if !shared.isEmpty {
                    Section("Shared") {
                        Text(shared).font(.footnote).foregroundStyle(.secondary).lineLimit(8).textSelection(.enabled)
                    }
                }
                if let projects = snapshot?.projects, !projects.isEmpty {
                    Section {
                        Picker("Project", selection: $projectId) {
                            Text("Last used").tag("")
                            ForEach(projects, id: \.id) { Text($0.title).tag($0.id) }
                        }
                    }
                }
                if !images.isEmpty {
                    Section("\(images.count) image\(images.count == 1 ? "" : "s")") {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack {
                                ForEach(images.indices, id: \.self) { i in
                                    Image(uiImage: images[i]).resizable().scaledToFill().frame(width: 72, height: 72).clipShape(RoundedRectangle(cornerRadius: 8))
                                }
                            }
                        }
                    }
                }
                Section {
                    if let server = snapshot?.server, !server.isEmpty {
                        Text("Domovoi opens a new thread on \(server) with this the next time you open it.").font(.footnote).foregroundStyle(.secondary)
                    } else {
                        Label("Domovoi isn't paired with a server yet. This waits until you pair it.", systemImage: "exclamationmark.triangle")
                            .font(.footnote)
                            .foregroundStyle(.orange)
                    }
                    if let error { Text(error).font(.footnote).foregroundStyle(.red) }
                }
            }
            .navigationTitle("New Thread")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Add") { save() }
                        .disabled(loading || (message.isEmpty && images.isEmpty))
                }
            }
            .overlay { if loading { ProgressView() } }
        }
        .task { await load() }
    }

    /// The instructions, then what was shared.
    private var message: String {
        [instructions, shared].map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }.joined(separator: "\n\n")
    }

    private func save() {
        do {
            let data = images.prefix(4).compactMap { ShareView.jpeg($0) }
            try SharedDraft.save(text: message, images: data, projectId: projectId.isEmpty ? nil : projectId)
            done()
        } catch {
            self.error = "Couldn't keep it for Domovoi: \(error.localizedDescription)"
        }
    }

    private func load() async {
        var parts: [String] = []
        for provider in items.flatMap({ $0.attachments ?? [] }) {
            if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
                // Four are kept: more decoded photos only use up the
                // extension's small memory limit (iOS stops it past that).
                if images.count < 4, let image = await loadImage(provider) { images.append(image) }
            } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
                      let url = try? await provider.loadItem(forTypeIdentifier: UTType.url.identifier) as? URL {
                parts.append(url.absoluteString)
            } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier),
                      let string = try? await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier) as? String {
                parts.append(string)
            }
        }
        if shared.isEmpty { shared = parts.joined(separator: "\n\n") }
        loading = false
    }

    private func loadImage(_ provider: NSItemProvider) async -> UIImage? {
        if let image = try? await provider.loadItem(forTypeIdentifier: UTType.image.identifier) {
            if let image = image as? UIImage { return image }
            // Decoded straight to at most 2048 px, never at full size.
            if let url = image as? URL, let source = CGImageSourceCreateWithURL(url as CFURL, nil) { return Self.downsampled(source) }
            if let data = image as? Data, let source = CGImageSourceCreateWithData(data as CFData, nil) { return Self.downsampled(source) }
        }
        return nil
    }

    /// The image at most 2048 px on its long side, decoded at that size.
    private static func downsampled(_ source: CGImageSource) -> UIImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary).map { UIImage(cgImage: $0) }
    }

    /// A JPEG at most 2048 px on its long side, as the app sends photos.
    static func jpeg(_ image: UIImage) -> Data? {
        let longest = max(image.size.width, image.size.height)
        let scale = longest > 2048 ? 2048 / longest : 1
        let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
        let resized = UIGraphicsImageRenderer(size: size).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
        return resized.jpegData(compressionQuality: 0.85)
    }
}
