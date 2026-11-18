import App
import SwiftUI

struct ContentView: View {
    @ObservedObject var core: Core
    @Environment(\.openSettings) private var openSettings
    @State private var addingServer = false

    private var connection: ConnectionView { core.view.connection }

    /// The main window shows once an environment is active and paired.
    private var showMain: Bool {
        switch connection.state {
        case "connected", "reconnecting": return true
        case "connecting": return !connection.activeEnvironmentId.isEmpty && !connection.needsPairing
        default: return false
        }
    }

    var body: some View {
        Group {
            if showMain && !addingServer {
                MainView(core: core, addServer: { addingServer = true })
            } else {
                ConnectView(core: core, cancel: showMain ? { addingServer = false } : nil)
            }
        }
        .overlay(alignment: .bottom) {
            // In the main window toasts sit in the detail column instead.
            if !(showMain && !addingServer) {
                ToastCapsule(core: core, edge: .bottom)
                    .padding(.horizontal, Space.xl)
                    .padding(.bottom, Space.xl)
                    .animation(.snappy, value: core.view.toast?.id)
            }
        }
        .task { await Snapshots.run(core) { openSettings() } }
        .onAppear {
            if Snapshots.directory == nil { core.launch() }
        }
        .onChange(of: connection.state) { _, state in
            if state == "connected" { addingServer = false }
        }
    }
}

struct MainView: View {
    @ObservedObject var core: Core
    var addServer: () -> Void = {}
    @State private var columns = NavigationSplitViewVisibility.all

    private var showsPanel: Bool { !["pull-requests", "usage"].contains(core.view.route) }

    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        NavigationSplitView(columnVisibility: $columns) {
            SidebarPane(core: core)
                .navigationSplitViewColumnWidth(min: 230, ideal: 280, max: 420)
        } detail: {
            DetailView(core: core)
                .frame(minWidth: 460)
                // The panel belongs to a thread (its diff, terminal, files):
                // not shown over Pull Requests or Usage.
                .inspector(isPresented: Binding(
                    get: { core.view.panelOpen && showsPanel },
                    set: { open in if open != core.view.panelOpen { core.update(.togglePanel) } }
                )) {
                    SidePanel(core: core)
                        .inspectorColumnWidth(min: 300, ideal: 420, max: 900)
                }
        }
        .toolbar {
            ToolbarItem(placement: .navigation) {
                EnvironmentMenu(core: core, addServer: addServer)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button { core.update(.openPalette) } label: {
                    Label("Command Palette", systemImage: "magnifyingglass")
                }
                .help("Command palette (⌘K)")
                Button { core.update(.togglePanel) } label: {
                    Label("Side Panel", systemImage: "sidebar.right")
                }
                .help("Toggle the side panel (⌘J)")
                .disabled(!showsPanel)
            }
        }
        .onChange(of: core.view.attentionCount, initial: true) { _, count in
            Platform.setAttentionBadge(count)
        }
        .sheet(isPresented: Binding(
            get: { core.view.palette != nil },
            // Only a palette still open: a quick ⌘K twice must not have the
            // first sheet's dismissal close the second palette.
            set: { if !$0, core.view.palette != nil { core.update(.closePalette) } }
        )) {
            if let palette = core.view.palette {
                PaletteSheet(core: core, palette: palette)
            }
        }
    }
}

struct DetailView: View {
    @ObservedObject var core: Core

    var body: some View {
        VStack(spacing: 0) {
            // The connection banner on every page, not only in threads.
            StatusStrip(core: core)
            switch core.view.route {
            case "thread":
                ThreadView(core: core)
            case "pull-requests":
                PullRequestsPage(core: core)
            case "usage":
                UsagePage(core: core)
            default:
                NewThreadView(core: core)
            }
        }
        // Toasts under the toolbar, in the column they're about: never over
        // the message field.
        .overlay(alignment: .top) {
            ToastCapsule(core: core, edge: .top)
                .frame(maxWidth: 520)
                .padding(.horizontal, Space.xl)
                .padding(.top, Space.s)
                .animation(.snappy, value: core.view.toast?.id)
        }
    }
}

/// The toolbar's environment switcher: state, path and saved servers.
struct EnvironmentMenu: View {
    @ObservedObject var core: Core
    var addServer: () -> Void
    @State private var renaming: EnvironmentView?
    @State private var newName = ""
    @State private var forgetting: EnvironmentView?

    private var connection: ConnectionView { core.view.connection }

    var body: some View {
        Menu {
            Section("Servers") {
                ForEach(connection.environments, id: \.id) { env in
                    Button {
                        core.update(.selectEnvironment(id: env.id))
                    } label: {
                        Label("\(env.label) — \(env.detail)", systemImage: env.active ? "checkmark" : (env.remote ? "globe" : "desktopcomputer"))
                    }
                }
            }
            if let active = connection.environments.first(where: { $0.active }) {
                Button("Rename \(active.label)…") {
                    newName = active.label
                    renaming = active
                }
                Button("Forget \(active.label)…") { forgetting = active }
            }
            Divider()
            Button("Add Server…", action: addServer)
            if connection.state == "connected" {
                Button("Disconnect") { core.update(.disconnect) }
            }
        } label: {
            ConnectionBadge(connection: connection)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help([connection.serverUrl, "domovoi-server \(connection.serverVersion)", connection.remote && !connection.path.isEmpty ? "\(connection.rttMs) ms" : "", connection.hint].filter { !$0.isEmpty }.joined(separator: " · "))
        .alert("Rename Server", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("Name", text: $newName)
            Button("Rename") {
                if let env = renaming { core.update(.renameEnvironment(id: env.id, label: newName)) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .forgettingServer($forgetting, core: core)
    }
}

struct ConnectionBadge: View {
    let connection: ConnectionView

    private var route: String {
        guard connection.remote else { return "" }
        if connection.path.isEmpty { return " · remote" }
        // The round trip changes by the second: it's in the tooltip, so
        // the toolbar item doesn't keep changing width.
        return " · \(connection.path)"
    }

    private var label: String {
        switch connection.state {
        case "connected": return (connection.environmentLabel.isEmpty ? "Connected" : connection.environmentLabel) + route
        case "reconnecting": return "Reconnecting…"
        case "connecting": return "Connecting…"
        case "pairing": return "Pairing…"
        case "failed": return "Can't reach server"
        default: return "Not connected"
        }
    }

    private var color: Color {
        switch connection.state {
        case "connected": return Tint.success
        case "reconnecting", "connecting", "pairing": return Tint.caution
        case "failed": return Tint.danger
        default: return .secondary
        }
    }

    /// A small dot (or a globe for a remote server) in `color`.
    static func dot(_ color: NSColor, globe: Bool) -> NSImage {
        if globe, let symbol = NSImage(systemSymbolName: "globe", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .medium).applying(.init(paletteColors: [color]))) {
            symbol.isTemplate = false
            return symbol
        }
        let image = NSImage(size: NSSize(width: 8, height: 8), flipped: false) { rect in
            color.setFill()
            NSBezierPath(ovalIn: rect.insetBy(dx: 0.5, dy: 0.5)).fill()
            return true
        }
        image.isTemplate = false
        return image
    }

    var body: some View {
        HStack(spacing: Space.tight) {
            // A menu's label keeps only text and images, drawn in one
            // colour: the dot is a coloured, non-template image.
            Image(nsImage: Self.dot(NSColor(color), globe: connection.remote && connection.state == "connected"))
            Text(label)
        }
        .font(TypeRole.meta.weight(.medium))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Connection: \(label)")
    }
}


// MARK: - Connect

struct ConnectView: View {
    @ObservedObject var core: Core
    /// Set while the main window is behind this (adding another server).
    var cancel: (() -> Void)?
    @State private var mode = "local"
    @State private var serverURL = "http://127.0.0.1:13773"
    @State private var pairingCode = ""
    @State private var link = ""
    @State private var forgetting: EnvironmentView?

    private var connection: ConnectionView { core.view.connection }
    private var busy: Bool { ["pairing", "connecting"].contains(connection.state) }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.l) {
            HStack(spacing: Space.m) {
                Image(systemName: "terminal.fill").font(.system(size: 34)).foregroundStyle(.tint)
                VStack(alignment: .leading) {
                    Text("Connect to a Domovoi server").font(.title2.bold())
                    Text("domovoi-server or any t3code-compatible environment").foregroundStyle(.secondary)
                }
            }
            if !connection.environments.isEmpty {
                GroupBox("Saved servers") {
                    VStack(spacing: Space.tight) {
                        ForEach(connection.environments, id: \.id) { env in
                            HStack(spacing: Space.s) {
                                Image(systemName: env.remote ? "globe" : "desktopcomputer").foregroundStyle(.secondary)
                                VStack(alignment: .leading) {
                                    Text(env.label)
                                    Text(env.detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                }
                                Spacer()
                                Button("Connect") { core.update(.selectEnvironment(id: env.id)) }
                                    .disabled(busy)
                                Button(role: .destructive) { forgetting = env } label: { Image(systemName: "trash") }
                                    .buttonStyle(.borderless)
                                    .help("Forget this server and its sign-in")
                            }
                        }
                    }
                    .padding(Space.xs)
                }
            }
            Picker("", selection: $mode) {
                Text("Server address").tag("local")
                Text("Remote link (iroh)").tag("remote")
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            if mode == "local" {
                Text("The server address and the one-time pairing code it printed (or from `domovoi-server pair`).")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Form {
                    TextField("Server URL", text: $serverURL)
                    TextField("Pairing code", text: $pairingCode)
                        .onSubmit(connect)
                }
                .formStyle(.grouped)
                .scrollDisabled(true)
                .fixedSize(horizontal: false, vertical: true)
            } else {
                Text("Paste the `domovoi://connect?…` link from `domovoi-server pair --remote` or from Settings → Remote Access on the server's Mac. It reaches the server from anywhere, over iroh.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Form {
                    TextField("Pairing link", text: $link, axis: .vertical)
                        .lineLimit(2...4)
                        .onSubmit(connect)
                }
                .formStyle(.grouped)
                .scrollDisabled(true)
                .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: Space.s) {
                Button(busy ? "Connecting…" : "Connect", action: connect)
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy || (mode == "local" ? serverURL.isEmpty : link.isEmpty))
                if busy { ProgressView().controlSize(.small) }
                if let cancel {
                    Button("Cancel", action: cancel).keyboardShortcut(.cancelAction)
                }
            }
            if let error = connection.error {
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Tint.danger)
                    if !connection.hint.isEmpty {
                        Text(connection.hint).font(TypeRole.meta).foregroundStyle(.secondary)
                    }
                }
                .textSelection(.enabled)
            }
        }
        .padding(Space.xxl)
        .frame(maxWidth: 580)
        .cardSurface(Radius.card + 6)
        .padding(Space.xxl)
        .frame(maxWidth: .infinity)
        // Many saved servers or a short window: scroll rather than clip.
        .scrollableCentered()
        .forgettingServer($forgetting, core: core)
        .onChange(of: connection.state) { _, state in
            if state == "connected" {
                pairingCode = ""
                link = ""
            }
        }
    }

    /// The code and link stay until the connection works: a typo or an
    /// unreachable server shouldn't mean typing them again.
    private func connect() {
        if mode == "local" {
            core.update(.connect(serverUrl: serverURL, pairingCode: pairingCode))
        } else {
            core.update(.connectRemote(link: link))
        }
    }
}

extension View {
    /// Centred in the available space while it fits, scrolling when it
    /// doesn't.
    func scrollableCentered() -> some View {
        GeometryReader { geo in
            ScrollView {
                self.frame(minHeight: geo.size.height)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }
}

extension View {
    /// Asks before forgetting a saved server (and signing out of it).
    func forgettingServer(_ server: Binding<EnvironmentView?>, core: Core) -> some View {
        confirmingDestructive(server, title: { "Forget \($0.label)?" }, action: "Forget",
                              message: "This Mac signs out of the server and removes it from the list. Pair again to come back.") {
            core.update(.forgetEnvironment(id: $0.id))
        }
    }
}
