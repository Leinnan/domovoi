import App
import SwiftUI

struct ProjectIconPicker: View {
    let projectId: String
    let title: String
    let initialAutomatic: ProjectIconView
    let environment: String
    @ObservedObject var core: Core
    @Environment(\.dismiss) private var dismiss
    @State private var kind: String
    @State private var text: String
    @State private var symbol: String
    @State private var color: String
    @State private var query = ""
    @State private var selectedPath: String?
    @State private var selectedURL: String?
    private static let emojis = ["💻", "🛠️", "🚀", "🤖", "✨", "⚡", "🌐", "📱", "🖥️", "⌨️", "⚙️", "🗄️", "☁️", "📦", "📚", "🧪", "🔒", "🎮", "🎵", "🎬", "🖼️", "🛍️", "🔥", "💡", "🧩", "📊", "🧠", "🦄", "🐙", "🌱"]

    init(project: SidebarProjectView, core: Core) {
        self.init(id: project.id, title: project.title, icon: project.icon, automatic: project.automaticIcon, core: core)
    }

    init(project: ProjectSettingsView, core: Core) {
        self.init(id: project.id, title: project.title, icon: project.icon, automatic: project.automaticIcon, core: core)
    }

    private init(id: String, title: String, icon: ProjectIconView, automatic: ProjectIconView, core: Core) {
        projectId = id
        self.title = title
        initialAutomatic = automatic
        environment = core.view.connection.activeEnvironmentId
        self.core = core
        _kind = State(initialValue: icon.filePath != nil ? "file" : icon.custom ? icon.kind : "")
        _text = State(initialValue: icon.text)
        _symbol = State(initialValue: icon.symbol.isEmpty ? "folder-code" : icon.symbol)
        _color = State(initialValue: ProjectPalette.keys.contains(icon.color) ? icon.color : "gray")
        _selectedPath = State(initialValue: icon.filePath)
        _selectedURL = State(initialValue: icon.imageUrl)
    }

    private var current: ProjectSettingsView? { core.view.settings.projects.first { $0.id == projectId } }
    private var automatic: ProjectIconView { current?.automaticIcon ?? initialAutomatic }

    private var valid: Bool {
        let scalars = text.unicodeScalars
        switch kind {
        case "monogram":
            let startsWithLetterOrDigit = scalars.first.map { CharacterSet.alphanumerics.contains($0) } ?? false
            let allowed = CharacterSet.alphanumerics.union(.nonBaseCharacters)
            return startsWithLetterOrDigit && text.count <= 2 && text.utf16.count <= 32
                && scalars.allSatisfy { allowed.contains($0) || $0.value == 0x200c || $0.value == 0x200d }
        case "file": return selectedPath != nil && selectedURL != nil
        case "emoji":
            let emoji = scalars.contains { $0.properties.isEmoji && $0.value > 127 } || scalars.contains { $0.value == 0x20e3 }
            return text.count == 1 && text.utf16.count <= 32 && emoji
        default: return true
        }
    }
    /// The preview tile and a file thumbnail.
    private let tileSize: CGFloat = 52
    private let thumbSize: CGFloat = 32

    var body: some View {
        layout
            .task(id: [kind, query, core.view.connection.state]) {
                guard kind == "file" else { return }
                if !query.isEmpty {
                    do { try await Task.sleep(for: .milliseconds(200)) } catch { return }
                }
                core.update(.searchProjectIconFiles(projectId: projectId, query: query))
            }
            .onChange(of: core.view.connection.activeEnvironmentId) { _, active in
                if active != environment { dismiss() }
            }
            .onChange(of: current == nil) { _, missing in
                if missing { dismiss() }
            }
            .onDisappear {
                if core.view.connection.activeEnvironmentId == environment {
                    core.update(.closeProjectIconFiles(projectId: projectId))
                }
            }
    }

    @ViewBuilder private var layout: some View {
        #if os(iOS)
        // A sheet like the others: its title, Cancel and Done in the bar.
        NavigationStack {
            VStack(spacing: Space.l) { editor }
                .padding(Space.l)
                .navigationTitle("Icon for \(title)")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                    ToolbarItem(placement: .confirmationAction) { Button("Done", action: save).disabled(!canSave) }
                }
        }
        .standardSheet()
        #else
        VStack(spacing: Space.l) {
            Text("Icon for \(title)").font(.headline).lineLimit(2)
            editor
            HStack {
                Button("Cancel") { dismiss() }
                Spacer()
                Button("Done", action: save).disabled(!canSave).keyboardShortcut(.defaultAction)
            }
        }
        .padding(Space.l)
        .frame(width: 360, height: 440)
        #endif
    }

    private var canSave: Bool {
        valid && current?.icon.saving != true && core.view.connection.state == "connected"
    }

    private func save() {
        if kind == "file", let path = selectedPath {
            if path != current?.icon.filePath { core.update(.setProjectIconFile(projectId: projectId, path: path)) }
        } else {
            core.update(.setProjectIcon(projectId: projectId, kind: kind, text: text, symbol: symbol, color: color))
        }
        dismiss()
    }

    /// The preview, the kind and its choices: the same on both platforms.
    @ViewBuilder private var editor: some View {
        ProjectIconTile(icon: preview, size: tileSize, core: core)
        Picker("Icon", selection: $kind) {
            Text("Automatic").tag("")
            Text("Letters").tag("monogram")
            Text("Emoji").tag("emoji")
            Text("Symbol").tag("symbol")
            Text("From Project Files").tag("file")
        }.pickerStyle(.menu)
        .onChange(of: kind) { _, value in
            if value == "monogram" { text = automatic.text }
            if value == "emoji", text.count != 1 { text = "💻" }
        }
        ScrollView {
            VStack(spacing: Space.m) {
                if kind == "monogram" {
                    TextField("One or two letters or digits", text: $text)
                        .textFieldStyle(.roundedBorder)
                        .onChange(of: text) { _, value in
                            text = String(value.filter { $0.unicodeScalars.allSatisfy { (CharacterSet.alphanumerics.union(.nonBaseCharacters).contains($0) || $0.value == 0x200c || $0.value == 0x200d) } }.prefix(2))
                        }
                }
                if kind == "emoji" {
                    TextField("One emoji", text: $text).textFieldStyle(.roundedBorder)
                        .onChange(of: text) { _, value in text = String(value.prefix(1)) }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: Size.tap))]) {
                        ForEach(Self.emojis, id: \.self) { emoji in
                            // Highlighted when chosen, as a symbol is.
                            Button { text = emoji } label: { Text(emoji).font(.title2).frame(width: Size.tap, height: Size.tap).selectedCell(text == emoji) }
                                .buttonStyle(.plain).accessibilityLabel(emoji).accessibilityAddTraits(text == emoji ? .isSelected : [])
                        }
                    }
                }
                if kind == "symbol" {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: Size.tap))]) {
                        ForEach(LucideSymbols.names, id: \.self) { name in
                            Button { symbol = name } label: {
                                Image(systemName: LucideSymbols.sf(name)).frame(width: Size.tap, height: Size.tap)
                                    .selectedCell(symbol == name)
                            }.buttonStyle(.plain).accessibilityLabel(name).accessibilityAddTraits(symbol == name ? .isSelected : [])
                        }
                    }
                }
                if kind == "monogram" || kind == "symbol" {
                    PaletteSwatchGrid(selection: $color)
                }
                if kind == "file" { filePicker }
                if kind == "" { Text("Uses a project icon file when found, or initials from the project name.").font(.callout).foregroundStyle(.secondary) }
            }.padding(Space.xs)
        }
    }

    @ViewBuilder
    private var filePicker: some View {
        TextField("Search project images", text: $query).textFieldStyle(.roundedBorder)
        if let selectedPath {
            Text(selectedPath).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
        }
        let files = core.view.settings.projectIconFiles
        if files.projectId != projectId || files.loading {
            LoadingRow("Finding project images…")
        } else if let error = files.error {
            ErrorRow(error)
        } else if files.files.isEmpty {
            Text(query.isEmpty ? "No project images found." : "No matching project images.").foregroundStyle(.secondary)
        } else {
            LazyVStack(alignment: .leading, spacing: Space.xs) {
                ForEach(files.files, id: \.path) { file in
                    Button {
                        if let url = file.url {
                            selectedPath = file.path
                            selectedURL = url
                        }
                    } label: {
                        HStack(spacing: Space.s) {
                            if let url = file.url.flatMap(URL.init(string:)) {
                                AsyncImage(url: url) { phase in
                                    if let image = phase.image { image.resizable().scaledToFit() }
                                    else { Image(systemName: "photo") }
                                }.frame(width: thumbSize, height: thumbSize)
                            } else {
                                Image(systemName: file.unavailable ? "photo.badge.exclamationmark" : "photo")
                                    .frame(width: thumbSize, height: thumbSize)
                            }
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                Text(file.path.split(separator: "/").last.map(String.init) ?? file.path)
                                Text(file.unavailable ? "\(file.path) · Preview unavailable" : file.path)
                                    .font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                            }
                            Spacer()
                            if selectedPath == file.path { Image(systemName: "checkmark") }
                        }.tapTarget()
                    }
                    .buttonStyle(.plain)
                    .disabled(file.url == nil)
                    .accessibilityAddTraits(selectedPath == file.path ? .isSelected : [])
                    .task(id: "\(files.revision):\(file.refresh)") {
                        if file.refresh { core.update(.loadProjectIconThumbnail(projectId: projectId, path: file.path)) }
                    }
                }
                if files.truncated { Text("Search to narrow the results.").font(TypeRole.meta).foregroundStyle(.secondary) }
            }
        }
    }

    private var preview: ProjectIconView {
        if kind == "" { return automatic }
        if kind == "file" {
            var icon = automatic
            icon.imageUrl = selectedURL
            icon.loadKey = nil
            return icon
        }
        var icon = automatic
        icon.kind = kind
        icon.text = text
        icon.symbol = symbol
        icon.color = color
        icon.imageUrl = nil
        icon.loadKey = nil
        return icon
    }
}

private extension View {
    /// A grid cell's highlight while it is the choice (a symbol, an emoji).
    func selectedCell(_ selected: Bool) -> some View {
        background(selected ? Color.accentColor.opacity(0.2) : .clear, in: RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
    }
}
