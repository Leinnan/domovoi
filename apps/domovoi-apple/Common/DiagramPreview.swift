import App
import CryptoKit
import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// A completed Mermaid block, drawn by the server (the core asks for it with
/// `LoadDiagram`); its source when it can't be drawn.
struct DiagramPreview: View {
    let source: String
    var expanded = false
    @Environment(\.colorScheme) private var appearance
    @Environment(\.core) private var core
    @State private var picture: Image?
    @State private var loadFailed = false
    @State private var showSource = false
    @State private var showExpanded = false
    @State private var zoom: CGFloat = 1

    /// One per source and theme.
    private var key: String {
        let digest = SHA256.hash(data: Data(source.utf8)).map { String(format: "%02x", $0) }.joined()
        return "\(appearance == .dark ? "dark" : "light")-\(digest)"
    }

    private var diagram: DiagramView? {
        let key = key
        return core?.view.diagrams.first { $0.key == key }
    }

    private var failed: Bool { core == nil || diagram?.failed == true || (loadFailed && picture == nil) }
    private var offline: Bool { diagram?.offline == true && picture == nil }
    /// The server is drawing it.
    private var rendering: Bool { diagram?.pending == true && !failed }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            HStack {
                if !expanded && picture != nil {
                    Button("Open diagram", systemImage: "arrow.up.left.and.arrow.down.right") { showExpanded = true }
                }
                Button(showSource ? "View diagram" : "View source") { showSource.toggle() }
                Button("Copy source", systemImage: "doc.on.doc") { Platform.copy(source) }
                // Still drawing while the source shows.
                if rendering && showSource {
                    ProgressView().controlSize(.small).accessibilityLabel("Rendering diagram")
                }
            }
            .font(.callout)
            if showSource || failed || offline {
                if failed { Text("This diagram couldn’t be rendered.").foregroundStyle(.secondary) }
                if offline && !failed { Text("The diagram is drawn once the server is connected.").foregroundStyle(.secondary) }
                ScrollView(.horizontal) {
                    Text(source).font(.system(.body, design: .monospaced)).textSelection(.enabled).fixedSize()
                }
            } else if let picture {
                if expanded {
                    HStack {
                        Button("Zoom out", systemImage: "minus.magnifyingglass") { zoom = max(1, zoom / 1.4) }
                        Button("Zoom in", systemImage: "plus.magnifyingglass") { zoom = min(4, zoom * 1.4) }
                        Button("Fit") { zoom = 1 }
                    }
                    GeometryReader { geometry in
                        ScrollView([.horizontal, .vertical]) {
                            picture.resizable().scaledToFit()
                                .frame(width: geometry.size.width * zoom)
                                .accessibilityLabel("Mermaid diagram")
                        }
                    }
                } else {
                    picture.resizable().scaledToFit().frame(maxHeight: 360)
                        .accessibilityLabel("Mermaid diagram")
                }
            } else {
                // Drawn on the server, then fetched.
                ProgressView(rendering || diagram == nil ? "Rendering diagram" : "Loading diagram")
                    .frame(maxWidth: .infinity, minHeight: 120)
            }
        }
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
        // Asked for on appearing, and again when the core says so (the link
        // expired, or the connection came back).
        .task(id: "\(key)\0\(diagram?.loadKey ?? "")") {
            core?.update(.loadDiagram(key: key, source: source, dark: appearance == .dark))
        }
        .task(id: diagram?.url) {
            guard let address = diagram?.url, let url = URL(string: address) else { return }
            loadFailed = false
            do {
                let (data, response) = try await URLSession.shared.data(from: url)
                guard !Task.isCancelled else { return }
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
                #if os(macOS)
                picture = NSImage(data: data).map { Image(nsImage: $0) }
                #else
                picture = UIImage(data: data).map { Image(uiImage: $0) }
                #endif
                loadFailed = picture == nil
            } catch {
                if !Task.isCancelled { loadFailed = true }
            }
        }
        .onChange(of: key) { picture = nil; loadFailed = false }
        .sheet(isPresented: $showExpanded) {
            VStack(alignment: .trailing) {
                Button("Done") { showExpanded = false }.padding(Space.m)
                DiagramPreview(source: source, expanded: true)
            }
            #if os(macOS)
            .frame(minWidth: 680, minHeight: 560)
            #endif
        }
    }
}
