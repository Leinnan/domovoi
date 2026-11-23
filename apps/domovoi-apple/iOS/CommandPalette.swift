import App
import SwiftUI

/// The command palette (⌘K on iPad, the command button on iPhone): the
/// core's list of threads, projects and actions, run by `run` (iOS moves
/// between screens itself).
struct PaletteScreen: View {
    @ObservedObject var core: Core
    let run: (PaletteItemView) -> Void
    @State private var query = ""
    @FocusState private var focused: Bool

    /// The Mac's side panel toggle has nothing to do here.
    static let hidden: Set<String> = ["action:toggle-panel"]

    private var palette: PaletteView? { core.view.palette }
    private var items: [PaletteItemView] { palette?.items ?? [] }
    /// The highlighted item, or the first shown after it (one hidden here is skipped).
    private var highlighted: Int? {
        guard !items.isEmpty else { return nil }
        let start = Int(palette?.highlighted ?? 0) % items.count
        return (0..<items.count).map { (start + $0) % items.count }.first { !Self.hidden.contains(items[$0].id) }
    }

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                List {
                    ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                        if !Self.hidden.contains(item.id) {
                            Button { run(item) } label: { row(item) }
                                .listRowBackground(index == highlighted ? Color.accentColor.opacity(0.15) : nil)
                                .id(index)
                        }
                    }
                }
                .listStyle(.plain)
                .overlay {
                    if items.allSatisfy({ Self.hidden.contains($0.id) }) {
                        if palette?.searching == true {
                            ProgressView()
                        } else if palette?.searchesFiles == true, query.isEmpty {
                            ContentUnavailableView(palette?.prompt ?? "", systemImage: palette?.mode == "files" ? "doc" : "text.magnifyingglass")
                        } else {
                            ContentUnavailableView.search(text: query)
                        }
                    }
                }
                .onChange(of: highlighted) { _, h in if let h { proxy.scrollTo(h) } }
            }
            .safeAreaInset(edge: .top, spacing: 0) { field }
            .navigationTitle("Command Palette")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    // Escape on a hardware keyboard: back to the commands
                    // from a file search, closed from the commands.
                    if palette?.searchesFiles == true {
                        Button("Back") { core.update(.setPaletteMode(mode: "commands")) }
                            .keyboardShortcut(.cancelAction)
                    } else {
                        Button("Cancel") { core.update(.closePalette) }
                            .keyboardShortcut(.cancelAction)
                    }
                }
            }
        }
        .onAppear {
            query = palette?.query ?? ""
            focused = true
        }
        // A new mode starts with an empty field (the core cleared its query).
        .onChange(of: palette?.mode) { _, _ in
            query = palette?.query ?? ""
            focused = true
        }
    }

    private var field: some View {
        HStack(spacing: Space.s) {
            if let palette, palette.searchesFiles {
                Button { core.update(.setPaletteMode(mode: "commands")) } label: {
                    Label(palette.modeTitle, systemImage: "chevron.left")
                        .font(.footnote.weight(.medium))
                        .padding(.horizontal, Space.m)
                        .frame(minHeight: Size.chip)
                        .glassChip()
                        .tapTarget(inPadding: Space.xs)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back to commands")
            } else {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            }
            TextField(palette?.prompt ?? "", text: $query)
                .focused($focused)
                .submitLabel(.go)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .onChange(of: query) { _, q in if q != palette?.query { core.update(.setPaletteQuery(query: q)) } }
                .onSubmit {
                    if let h = highlighted { run(items[h]) }
                }
                .onKeyPress(.upArrow) {
                    move(-1)
                    return .handled
                }
                .onKeyPress(.downArrow) {
                    move(1)
                    return .handled
                }
            if palette?.searching == true { ProgressView() }
        }
        // A chip's height in either mode: the field doesn't jump when the back chip shows.
        .frame(minHeight: Size.chip)
        .padding(.horizontal, Space.l)
        .padding(.vertical, Space.m)
        .background(.bar)
    }

    private func row(_ item: PaletteItemView) -> some View {
        HStack(spacing: Space.m) {
            Image(systemName: item.symbol).iconColumn(Size.listIcon).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: Space.xxs) {
                Text(item.title).foregroundStyle(.primary).lineLimit(1)
                    .font(item.kind == "match" ? TypeRole.mono : TypeRole.rowTitle)
                if !item.subtitle.isEmpty {
                    Text(item.subtitle).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                }
            }
            Spacer()
            // Shortcuts mean something with a hardware keyboard: on iPad.
            if !item.shortcut.isEmpty, !Platform.isPhone {
                KeyCaps(shortcut: item.shortcut)
            }
        }
        .contentShape(Rectangle())
    }

    /// The next shown item that way; the core counts the hidden ones too.
    private func move(_ step: Int) {
        guard let from = highlighted, !items.isEmpty else { return }
        var to = from
        for _ in items.indices {
            to = (to + step + items.count) % items.count
            if !Self.hidden.contains(items[to].id) { break }
        }
        core.update(.movePalette(delta: Int32(to - Int(palette?.highlighted ?? 0))))
    }
}
