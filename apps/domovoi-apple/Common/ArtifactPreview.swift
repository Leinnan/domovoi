import App
import Shared
import SwiftUI
import WebKit
#if os(macOS)
import AppKit
import UniformTypeIdentifiers
#endif

// MARK: - Inline Artifact Card (Transcript)

/// Compact, rich card rendered inline in the transcript for an HTML, React,
/// SVG, Mermaid, Markdown, or hosted Claude Code artifact.
struct ArtifactCard: View {
    let title: String
    let kind: String
    let version: UInt32
    var streaming: Bool = false
    var hostedUrl: String? = nil
    let open: () -> Void
    var viewCode: (() -> Void)? = nil

    private var icon: String { ArtifactStyle.icon(for: kind) }
    private var subtitle: String {
        if streaming { return "Generating \(ArtifactStyle.label(for: kind))…" }
        let base = ArtifactStyle.label(for: kind)
        return version > 1 ? "\(base) · v\(version)" : base
    }

    var body: some View {
        Button(action: open) {
            HStack(spacing: Space.m) {
                Image(systemName: icon)
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 36, height: 36)
                    .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(title.isEmpty ? ArtifactStyle.label(for: kind) : title)
                        .font(TypeRole.cardTitle)
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                    HStack(spacing: Space.xs) {
                        if streaming {
                            ProgressView().controlSize(.mini)
                        }
                        Text(subtitle)
                            .font(TypeRole.meta)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                        if hostedUrl != nil {
                            Text("· claude.ai")
                                .font(TypeRole.meta)
                                .foregroundStyle(.tint)
                                .lineLimit(1)
                        }
                    }
                }
                Spacer(minLength: Space.s)
                if let viewCode, !streaming {
                    Button {
                        viewCode()
                    } label: {
                        Image(systemName: "chevron.left.forwardslash.chevron.right")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(Space.xs)
                    }
                    .buttonStyle(.borderless)
                    .help("View source")
                    .accessibilityLabel("View source")
                }
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .padding(Space.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .cardSurface()
        .contextMenu {
            Button("Open Artifact", systemImage: icon, action: open)
            if let viewCode {
                Button("View Code", systemImage: "chevron.left.forwardslash.chevron.right", action: viewCode)
            }
            if let urlStr = hostedUrl, let url = URL(string: urlStr) {
                Link(destination: url) { Label("Open on claude.ai", systemImage: "safari") }
            }
        }
    }
}

enum ArtifactStyle {
    static func icon(for kind: String) -> String {
        switch kind.lowercased() {
        case "react": "atom"
        case "svg": "scribble.variable"
        case "mermaid": "point.3.connected.trianglepath.dotted"
        case "markdown": "doc.richtext"
        case "hosted": "safari"
        case "code": "chevron.left.forwardslash.chevron.right"
        default: "globe"
        }
    }

    static func label(for kind: String) -> String {
        switch kind.lowercased() {
        case "react": "Interactive React"
        case "svg": "SVG graphic"
        case "mermaid": "Mermaid diagram"
        case "markdown": "Document"
        case "hosted": "Claude artifact"
        case "code": "Code"
        default: "Interactive HTML"
        }
    }

    static func fileExtension(for kind: String, language: String) -> String {
        switch kind.lowercased() {
        case "html": "html"
        case "svg": "svg"
        case "react": language.lowercased() == "jsx" ? "jsx" : "tsx"
        case "mermaid": "mmd"
        case "markdown": "md"
        default: language.isEmpty ? "txt" : language
        }
    }
}

// MARK: - Unified Artifact Side Panel / Mobile Viewer

/// First-class Artifacts pane embedded in the macOS `SidePanel` inspector,
/// iPad split view, and iOS full-screen sheet.
struct ArtifactPane: View {
    @ObservedObject var core: Core
    let artifacts: ArtifactsPanelView
    var close: (() -> Void)? = nil
    var jumpToMessage: ((String) -> Void)? = nil

    @Environment(\.colorScheme) private var colorScheme
    @State private var interactive = true
    @State private var forceTheme: Bool? = nil
    @State private var runtimeError: String? = nil
    @State private var popout = false
    @State private var quickEdit = ""
    @State private var savingWorkspace = false
    @State private var savePath = ""

    private var previewDark: Bool { forceTheme ?? (colorScheme == .dark) }
    private var threadId: String { core.view.threadId ?? "" }

    var body: some View {
        VStack(spacing: 0) {
            if let file = core.view.panel.files.artifactPreview {
                WorkspaceArtifactFile(core: core, preview: file, dark: previewDark)
            } else if let active = artifacts.active {
                activeHeader(active)
                Divider()
                activeBody(active)
                if let err = runtimeError, active.mode == "preview" {
                    errorBanner(err, artifactId: active.id)
                }
                #if os(iOS)
                quickIterateBar(active)
                #endif
            } else if artifacts.items.isEmpty {
                ContentUnavailableView {
                    Label("No artifacts yet", systemImage: "sparkles.rectangle.stack")
                } description: {
                    Text("HTML dashboards, React components, SVG graphics, and Mermaid diagrams from this thread appear here.")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                artifactList
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onChange(of: artifacts.active?.id) { runtimeError = nil }
        .onChange(of: artifacts.active?.version) { runtimeError = nil }
        .onChange(of: previewDark) { _, dark in
            if artifacts.active != nil {
                core.update(.refreshArtifactPreview(dark: dark))
            }
        }
        .sheet(isPresented: $popout) {
            if artifacts.active != nil {
                ArtifactPane(core: core, artifacts: artifacts, close: { popout = false }, jumpToMessage: jumpToMessage)
                    #if os(macOS)
                    .frame(minWidth: 820, minHeight: 640)
                    #endif
            }
        }
        .alert("Save Artifact to Workspace", isPresented: $savingWorkspace) {
            TextField("path/to/file.html", text: $savePath)
            Button("Save") {
                if let active = artifacts.active, !savePath.trimmingCharacters(in: .whitespaces).isEmpty {
                    core.update(.saveFile(path: savePath.trimmingCharacters(in: .whitespaces), text: active.source))
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    private var artifactList: some View {
        VStack(spacing: 0) {
            if let close {
                HStack {
                    Text("Artifacts").font(.headline)
                    Spacer()
                    Button("Done", action: close)
                }
                .padding(Space.m)
                Divider()
            }
            List(artifacts.items, id: \.id) { item in
                HStack(spacing: Space.s) {
                    Button {
                        core.update(.openArtifact(threadId: threadId, artifactId: item.id, version: nil, dark: previewDark))
                    } label: {
                        HStack(spacing: Space.s) {
                            Image(systemName: ArtifactStyle.icon(for: item.kind))
                                .foregroundStyle(Color.accentColor)
                                .frame(width: 24)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(item.title).font(.callout.weight(.medium)).lineLimit(1)
                                Text(item.versionCount > 1 ? "\(ArtifactStyle.label(for: item.kind)) · v\(item.latestVersion)" : ArtifactStyle.label(for: item.kind))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    if let path = item.path, ["html", "htm"].contains((path as NSString).pathExtension.lowercased()) {
                        Button {
                            core.update(.openArtifactFile(threadId: threadId, path: path))
                        } label: {
                            Image(systemName: "doc.text")
                        }
                        .buttonStyle(.borderless)
                        .help("View the workspace file")
                        .accessibilityLabel("View the workspace file")
                    }
                    if let jumpToMessage, !item.itemId.isEmpty {
                        Button {
                            jumpToMessage(item.itemId.components(separatedBy: ":").first ?? item.itemId)
                        } label: {
                            Image(systemName: "arrow.turn.down.right")
                        }
                        .buttonStyle(.borderless)
                        .help("Jump to message")
                    }
                }
            }
            .listStyle(.plain)
        }
    }

    private func activeHeader(_ active: ActiveArtifactView) -> some View {
        VStack(spacing: Space.s) {
            HStack(spacing: Space.s) {
                if artifacts.items.count > 1 {
                    Menu {
                        ForEach(artifacts.items, id: \.id) { item in
                            Button {
                                core.update(.openArtifact(threadId: threadId, artifactId: item.id, version: nil, dark: previewDark))
                            } label: {
                                Label(item.title, systemImage: ArtifactStyle.icon(for: item.kind))
                            }
                        }
                        Divider()
                        Button("All Artifacts (\(artifacts.items.count))") { core.update(.closeArtifact) }
                    } label: {
                        HStack(spacing: Space.xs) {
                            Image(systemName: ArtifactStyle.icon(for: active.kind)).foregroundStyle(Color.accentColor)
                            Text(active.title).font(.headline).lineLimit(1)
                            Image(systemName: "chevron.up.chevron.down").font(.caption2).foregroundStyle(.secondary)
                        }
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize(horizontal: false, vertical: true)
                } else {
                    Label(active.title, systemImage: ArtifactStyle.icon(for: active.kind))
                        .font(.headline)
                        .lineLimit(1)
                }

                Spacer(minLength: Space.xs)

                if active.versions.count > 1 {
                    Menu("v\(active.version)") {
                        ForEach(active.versions.reversed(), id: \.version) { ver in
                            Button {
                                core.update(.selectArtifactVersion(version: ver.version, dark: previewDark))
                            } label: {
                                let tag = ver.version == active.versions.last?.version ? "v\(ver.version) (Latest)" : "v\(ver.version)"
                                Text(ver.summary.isEmpty ? tag : "\(tag) — \(ver.summary)")
                            }
                        }
                    }
                    .font(.caption.monospacedDigit().weight(.semibold))
                    .fixedSize()
                }

                #if os(macOS)
                if close == nil {
                    Button { popout = true } label: { Image(systemName: "arrow.up.left.and.arrow.down.right") }
                        .buttonStyle(.borderless)
                        .help("Expand artifact")
                }
                #endif

                actionsMenu(active)

                if let close {
                    Button("Done", action: close).keyboardShortcut(.cancelAction)
                }
            }

            HStack(spacing: Space.s) {
                Picker("Artifact view", selection: Binding(
                    get: { active.mode },
                    set: { core.update(.setArtifactViewMode(mode: $0)) }
                )) {
                    Text("Preview").tag("preview")
                    Text("Code").tag("code")
                }
                .labelsHidden()
                .pickerStyle(.segmented)

                if active.mode == "preview" {
                    if ["html", "svg"].contains(active.kind) {
                        Button {
                            interactive.toggle()
                            runtimeError = nil
                        } label: {
                            Image(systemName: interactive ? "bolt.shield.fill" : "shield")
                                .foregroundStyle(interactive ? Color.accentColor : .secondary)
                        }
                        .buttonStyle(.borderless)
                        .help(interactive ? "Offline sandbox interactivity ON (click for static)" : "Static preview (click to enable offline scripts)")
                    }
                    Button {
                        forceTheme = !previewDark
                    } label: {
                        Image(systemName: previewDark ? "moon.fill" : "sun.max")
                    }
                    .buttonStyle(.borderless)
                    .help(previewDark ? "Switch preview to light theme" : "Switch preview to dark theme")
                } else {
                    Button {
                        Platform.copy(active.source)
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }
        }
        .padding(.horizontal, Space.m)
        .padding(.vertical, Space.s)
    }

    private func actionsMenu(_ active: ActiveArtifactView) -> some View {
        Menu {
            if !active.source.isEmpty {
                Button { Platform.copy(active.source) } label: { Label("Copy Source", systemImage: "doc.on.doc") }
                Button {
                    let ext = ArtifactStyle.fileExtension(for: active.kind, language: active.language)
                    savePath = active.path ?? "\(active.title.lowercased().replacingOccurrences(of: " ", with: "-")).\(ext)"
                    savingWorkspace = true
                } label: { Label("Save to Workspace…", systemImage: "square.and.arrow.down") }
                ShareLink(item: active.source, subject: Text(active.title)) {
                    Label("Share…", systemImage: "square.and.arrow.up")
                }
                #if os(macOS)
                Button("Export File…", systemImage: "arrow.down.doc") { exportFile(active) }
                #endif
            }
            if let hosted = active.hostedUrl, let url = URL(string: hosted) {
                Link(destination: url) { Label("Open on claude.ai", systemImage: "safari") }
            }
            if let jumpToMessage, !active.itemId.isEmpty {
                Button("Jump to Message", systemImage: "arrow.turn.down.right") {
                    jumpToMessage(active.itemId.components(separatedBy: ":").first ?? active.itemId)
                }
            }
            Divider()
            Button("Ask Claude to Edit…", systemImage: "sparkles") {
                core.update(.askAboutArtifact(artifactId: active.id, prompt: ""))
                close?()
            }
            Button("Reload Preview", systemImage: "arrow.clockwise") {
                runtimeError = nil
                core.update(.refreshArtifactPreview(dark: previewDark))
            }
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .buttonStyle(.borderless)
        .accessibilityLabel("Artifact actions")
    }

    @ViewBuilder
    private func activeBody(_ active: ActiveArtifactView) -> some View {
        if active.streaming && active.source.isEmpty {
            VStack(spacing: Space.m) {
                ProgressView()
                Text("Generating \(active.title)…").font(.callout).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if active.loading {
            ProgressView("Loading artifact…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = active.error {
            ContentUnavailableView {
                Label("Preview unavailable", systemImage: "exclamationmark.triangle")
            } description: {
                Text(error)
            } actions: {
                Button("Try again") { core.update(.refreshArtifactPreview(dark: previewDark)) }
            }
        } else if active.truncated {
            ContentUnavailableView("This artifact is too large to preview", systemImage: "doc.questionmark")
        } else if active.mode == "code" || active.streaming {
            ScrollView([.vertical, .horizontal]) {
                Text(CodeHighlight.text(active.spans, fallback: active.source))
                    .font(TypeRole.code)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(Space.l)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        } else {
            previewSurface(active)
        }
    }

    @ViewBuilder
    private func previewSurface(_ active: ActiveArtifactView) -> some View {
        switch active.kind {
        case "mermaid":
            if let urlStr = active.previewUrl, let url = URL(string: urlStr) {
                ServerArtifactWebView(url: url, onError: { runtimeError = $0 })
                    .id(urlStr)
            } else {
                DiagramPreview(source: active.source, expanded: true)
            }
        case "markdown":
            ScrollView {
                VStack(alignment: .leading, spacing: Space.m) {
                    Text(active.source)
                        .font(.body)
                        .textSelection(.enabled)
                        .frame(maxWidth: Space.readable, alignment: .leading)
                }
                .padding(Space.l)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        case "hosted":
            if !active.source.isEmpty {
                ArtifactWebView(kind: "html", source: active.source, interactive: interactive, dark: previewDark, onError: { runtimeError = $0 })
                    .id("\(active.id)-\(active.version)-\(interactive)-\(previewDark)")
            } else {
                ContentUnavailableView {
                    Label(active.title, systemImage: "safari")
                } description: {
                    Text("This artifact is hosted on claude.ai.")
                } actions: {
                    if let hosted = active.hostedUrl, let url = URL(string: hosted) {
                        Link("Open on claude.ai", destination: url)
                            .buttonStyle(.borderedProminent)
                    }
                }
            }
        case "react":
            if let urlStr = active.previewUrl, let url = URL(string: urlStr) {
                ServerArtifactWebView(url: url, onError: { runtimeError = $0 })
                    .id(urlStr)
            } else {
                ProgressView("Preparing React preview…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        case "html" where active.previewUrl != nil && active.path != nil:
            if let urlStr = active.previewUrl, let url = URL(string: urlStr) {
                ServerArtifactWebView(url: url, onError: { runtimeError = $0 })
                    .id(urlStr)
            }
        case "html", "svg":
            ArtifactWebView(kind: active.kind, source: active.source, interactive: interactive, dark: previewDark, onError: { runtimeError = $0 })
                .id("\(active.id)-\(active.version)-\(interactive)-\(previewDark)")
        default:
            ScrollView([.vertical, .horizontal]) {
                Text(CodeHighlight.text(active.spans, fallback: active.source))
                    .font(TypeRole.code)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(Space.l)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }

    private func errorBanner(_ message: String, artifactId: String) -> some View {
        HStack(spacing: Space.s) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Tint.danger)
            Text(message).font(.caption).lineLimit(2).foregroundStyle(.primary)
            Spacer(minLength: Space.xs)
            Button("Fix with Claude") {
                core.update(.askAboutArtifact(artifactId: artifactId, prompt: "Fix this runtime error in the preview:\n\(message)"))
                runtimeError = nil
                close?()
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            Button { runtimeError = nil } label: { Image(systemName: "xmark") }
                .buttonStyle(.borderless)
        }
        .padding(.horizontal, Space.m)
        .padding(.vertical, Space.s)
        .background(Tint.danger.opacity(0.12))
    }

    #if os(iOS)
    private func quickIterateBar(_ active: ActiveArtifactView) -> some View {
        HStack(spacing: Space.s) {
            TextField("Ask Claude to update this artifact…", text: $quickEdit)
                .textFieldStyle(.roundedBorder)
            Button {
                let text = quickEdit.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !text.isEmpty else { return }
                core.update(.askAboutArtifact(artifactId: active.id, prompt: text))
                quickEdit = ""
                close?()
            } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title2)
            }
            .disabled(quickEdit.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .padding(.horizontal, Space.m)
        .padding(.vertical, Space.s)
        .background(.bar)
    }
    #endif

    #if os(macOS)
    private func exportFile(_ active: ActiveArtifactView) {
        let ext = ArtifactStyle.fileExtension(for: active.kind, language: active.language)
        let panel = NSSavePanel()
        panel.allowedContentTypes = [UTType(filenameExtension: ext) ?? .plainText]
        panel.nameFieldStringValue = "\(active.title.replacingOccurrences(of: "/", with: "-")).\(ext)"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        try? active.source.write(to: url, atomically: true, encoding: .utf8)
    }
    #endif
}

// MARK: - Workspace HTML File

/// An HTML file in the workspace an artifact was written to, read as it is
/// now (`OpenArtifactFile`): drawn, or its source. Back closes it.
struct WorkspaceArtifactFile: View {
    @ObservedObject var core: Core
    let preview: FilePreviewView
    let dark: Bool
    @State private var source = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: Space.s) {
                Button { core.update(.closeArtifactFile) } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(.borderless)
                    .help("Back to the artifacts")
                    .accessibilityLabel("Back")
                Text(preview.path).font(TypeRole.monoSmall).lineLimit(1).truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(preview.path)
                if !preview.loading, preview.error == nil {
                    Picker("View", selection: $source) {
                        Text("Preview").tag(false)
                        Text("Source").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                    Menu {
                        Button("Add to Message", systemImage: "at") { core.update(.appendToDraft(text: "@\(preview.path) ")) }
                        Button("Copy Source", systemImage: "doc.on.doc") { Platform.copy(preview.text) }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .accessibilityLabel("More")
                }
            }
            .padding(Space.s)
            Divider()
            if preview.loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error = preview.error {
                ContentUnavailableView("Can’t show the file", systemImage: "doc.questionmark", description: Text(error))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if source {
                ScrollView([.vertical, .horizontal]) {
                    Text(CodeHighlight.text(preview.spans, fallback: preview.text))
                        .font(TypeRole.code)
                        .textSelection(.enabled)
                        .fixedSize()
                        .padding(Space.s)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else {
                ArtifactWebView(kind: "html", source: preview.text, interactive: true, dark: dark)
                    .id("\(preview.path)-\(preview.text.hashValue)-\(dark)")
            }
            if preview.truncated {
                Text("Showing the first megabyte.").font(.caption).foregroundStyle(Tint.caution).padding(Space.tight)
            }
        }
    }
}

// MARK: - Legacy Compatibility Entrypoints (Files pane / Sheets)

struct HtmlArtifactButton: View {
    let source: String
    var title = "HTML artifact"
    @State private var opened = false

    var body: some View {
        Button("Preview", systemImage: "sparkles.rectangle.stack") { opened = true }
            .disabled(source.utf8.count > 2_097_152)
            .sheet(isPresented: $opened) {
                HtmlArtifactPreview(title: title, source: source) { opened = false }
            }
    }
}

struct HtmlArtifactPreview: View {
    let title: String
    let source: String
    let close: () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var tab = 0
    @State private var interactive = true

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title).font(.headline).lineLimit(1)
                Spacer()
                Button("Done", action: close).keyboardShortcut(.cancelAction)
            }.padding(Space.m)
            HStack {
                Picker("Artifact view", selection: $tab) {
                    Text("Preview").tag(0)
                    Text("Source").tag(1)
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                if tab == 0 {
                    Toggle("Interactive", isOn: $interactive)
                        .toggleStyle(.button)
                        .fixedSize()
                } else {
                    Button("Copy source") { Platform.copy(source) }.fixedSize()
                }
            }.padding(.horizontal, Space.m).padding(.bottom, Space.s)
            Divider()
            if tab == 0 {
                ArtifactWebView(kind: "html", source: source, interactive: interactive, dark: colorScheme == .dark)
                    .id("\(source.hashValue)-\(interactive)-\(colorScheme == .dark)")
            } else {
                ScrollView([.vertical, .horizontal]) {
                    Text(source).font(.system(.body, design: .monospaced)).textSelection(.enabled).fixedSize().padding(Space.l)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
        #if os(macOS)
        .frame(minWidth: 680, minHeight: 560)
        #endif
    }
}

struct ArtifactGallery: View {
    @ObservedObject var core: Core
    let thread: ThreadPageView
    let close: () -> Void
    let jump: (String) -> Void

    var body: some View {
        ArtifactPane(core: core, artifacts: core.view.panel.artifacts, close: close, jumpToMessage: jump)
            #if os(macOS)
            .frame(minWidth: 680, minHeight: 540)
            #endif
    }
}

// MARK: - Sandboxed WebViews

private struct ArtifactWebView: View {
    let kind: String
    let source: String
    let interactive: Bool
    let dark: Bool
    var onError: ((String) -> Void)? = nil
    @State private var rules: WKContentRuleList?
    @State private var document: String?
    @State private var failed = false

    var body: some View {
        if failed || document?.isEmpty == true {
            ContentUnavailableView {
                Label("Preview unavailable", systemImage: "doc.questionmark")
            } actions: {
                if failed {
                    Button("Try again") {
                        rules = nil
                        document = nil
                        failed = false
                    }
                }
            }
        } else if let rules, let document {
            OfflineWebView(document: document, interactive: interactive, rules: rules, onError: onError) { failed = true }
        } else {
            ProgressView("Preparing preview")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .task(id: "\(kind)-\(source.hashValue)-\(interactive)-\(dark)") {
                    let kind = kind
                    let source = source
                    let interactive = interactive
                    let dark = dark
                    let build = Task.detached(priority: .userInitiated) {
                        artifactPreviewDocument(kind: kind, source: source, interactive: interactive, dark: dark)
                    }
                    do {
                        let compiled = try await WKContentRuleListStore.default().compileContentRuleList(
                            forIdentifier: "domovoi-offline-artifact-v1",
                            encodedContentRuleList: """
                            [{"trigger":{"url-filter":"^http"},"action":{"type":"block"}},
                             {"trigger":{"url-filter":"^ws"},"action":{"type":"block"}},
                             {"trigger":{"url-filter":"^ftp:"},"action":{"type":"block"}},
                             {"trigger":{"url-filter":"^file:"},"action":{"type":"block"}},
                             {"trigger":{"url-filter":"^content:"},"action":{"type":"block"}}]
                            """
                        )
                        let page = await build.value
                        guard !Task.isCancelled else { return }
                        rules = compiled
                        document = page
                        failed = compiled == nil
                    } catch { failed = !Task.isCancelled }
                }
        }
    }
}

/// Loads a signed Domovoi server artifact URL (multi-file workspace bundle,
/// React runtime preview, or vector Mermaid SVG) while blocking all external hosts.
@MainActor
private struct ServerArtifactWebView {
    let url: URL
    var onError: ((String) -> Void)? = nil

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        var loadedURL: URL?
        let allowedHost: String?
        let allowedPort: Int?
        let onError: ((String) -> Void)?

        init(url: URL, onError: ((String) -> Void)?) {
            self.allowedHost = url.host
            self.allowedPort = url.port
            self.onError = onError
        }

        func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
            guard message.name == "domovoiArtifact",
                  let dict = message.body as? [String: Any],
                  let text = dict["message"] as? String, !text.isEmpty else { return }
            let line = (dict["line"] as? Int).flatMap { $0 > 0 ? " (line \($0))" : nil } ?? ""
            onError?("\(text)\(line)")
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            guard let target = action.request.url else { return .cancel }
            if ["about", "data", "blob"].contains(target.scheme) { return .allow }
            if target.host == allowedHost && target.port == allowedPort && target.path.hasPrefix("/api/assets/") {
                return .allow
            }
            return .cancel
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(url: url, onError: onError) }

    func makeView(_ coordinator: Coordinator) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        config.userContentController.add(coordinator, name: "domovoiArtifact")
        let view = WKWebView(frame: .zero, configuration: config)
        view.navigationDelegate = coordinator
        return view
    }

    func updateView(_ view: WKWebView, coordinator: Coordinator) {
        guard coordinator.loadedURL != url else { return }
        coordinator.loadedURL = url
        view.load(URLRequest(url: url))
    }
}

#if os(macOS)
extension ServerArtifactWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView { makeView(context.coordinator) }
    func updateNSView(_ view: WKWebView, context: Context) { updateView(view, coordinator: context.coordinator) }
}
#else
extension ServerArtifactWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView { makeView(context.coordinator) }
    func updateUIView(_ view: WKWebView, context: Context) { updateView(view, coordinator: context.coordinator) }
}
#endif

@MainActor
private struct OfflineWebView {
    let document: String
    let interactive: Bool
    let rules: WKContentRuleList
    var onError: ((String) -> Void)? = nil
    let failed: () -> Void

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        var document: String?
        let onError: ((String) -> Void)?
        let failed: () -> Void

        init(onError: ((String) -> Void)?, failed: @escaping () -> Void) {
            self.onError = onError
            self.failed = failed
        }

        func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
            guard message.name == "domovoiArtifact",
                  let dict = message.body as? [String: Any],
                  let text = dict["message"] as? String, !text.isEmpty else { return }
            let line = (dict["line"] as? Int).flatMap { $0 > 0 ? " (line \($0))" : nil } ?? ""
            onError?("\(text)\(line)")
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            failed()
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            guard let url = action.request.url?.absoluteString, action.targetFrame != nil else { return .cancel }
            if action.targetFrame?.isMainFrame == true && (url == "about:blank" || url.hasPrefix("about:blank#")) { return .allow }
            if action.targetFrame?.isMainFrame == false && (url == "about:srcdoc" || url.hasPrefix("about:srcdoc#")) { return .allow }
            return .cancel
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(onError: onError, failed: failed) }

    func makeView(_ coordinator: Coordinator) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.userContentController.add(rules)
        configuration.userContentController.add(coordinator, name: "domovoiArtifact")
        configuration.defaultWebpagePreferences.allowsContentJavaScript = interactive
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = coordinator
        return view
    }

    func updateView(_ view: WKWebView, coordinator: Coordinator) {
        guard coordinator.document != document else { return }
        coordinator.document = document
        view.loadHTMLString(document, baseURL: nil)
    }
}

#if os(macOS)
extension OfflineWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView { makeView(context.coordinator) }
    func updateNSView(_ view: WKWebView, context: Context) { updateView(view, coordinator: context.coordinator) }
}
#else
extension OfflineWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView { makeView(context.coordinator) }
    func updateUIView(_ view: WKWebView, context: Context) { updateView(view, coordinator: context.coordinator) }
}
#endif
