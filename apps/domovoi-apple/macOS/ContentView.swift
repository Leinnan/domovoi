import App
import AppKit
import SwiftUI

struct ContentView: View {
    @ObservedObject var core: Core
    @Environment(\.openSettings) private var openSettings
    @State private var addingServer = false
    @StateObject private var companion = CompanionBrain()

    private var connection: ConnectionView { core.view.connection }

    /// The main window shows once an environment is active and paired.
    private var showMain: Bool {
        switch connection.state {
        case "connected", "reconnecting": return true
        case "connecting", "disconnected": return !connection.activeEnvironmentId.isEmpty && !connection.needsPairing
        default: return false
        }
    }

    var body: some View {
        Group {
            if core.keychainState == .unavailable {
                ContentUnavailableView {
                    Label("Keychain Access Needed", systemImage: "lock")
                } description: {
                    Text("Unlock your login keychain and authorize Domovoi to read its saved servers and device identity. If macOS asks for permission, choose Always Allow to keep that approval for future updates.")
                } actions: {
                    Button("Retry") { core.launch() }
                }
            } else if core.keychainState != .ready {
                ProgressView("Checking Keychain access…")
            } else if connection.serverChanged && !addingServer {
                ServerChangedView(core: core) { addingServer = true }
            } else if showMain && !addingServer {
                MainView(core: core, addServer: { addingServer = true })
            } else {
                // Back to the main window, or to the server that refused this Mac.
                ConnectView(core: core, cancel: showMain || connection.serverChanged ? { addingServer = false } : nil)
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
        .environmentObject(companion)
        .environment(\.notice, NoticeSink(core: core))
        .modifier(CompanionWindow(brain: companion, connection: connection))
        // A link from a browser or another app, on whichever screen is up.
        .linkRequestDialog(core: core)
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
    /// The window's width, read from the window itself: the split view's own
    /// width is no guide, it is laid out wider than the window when its
    /// columns don't fit.
    @State private var windowWidth: CGFloat = 0
    /// The width the split view is laid out at.
    @State private var splitWidth: CGFloat = 0
    /// Set when the sidebar was hidden because the columns didn't fit: the
    /// width they needed then, so the sidebar comes back once there is that
    /// much room. A sidebar the user hid stays hidden.
    @State private var sidebarHiddenNeeding: CGFloat?

    private var showsPanel: Bool { !["pull-requests", "usage"].contains(core.view.route) }
    private var panelShown: Bool { core.view.panelOpen && showsPanel }

    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        NavigationSplitView(columnVisibility: $columns) {
            SidebarPane(core: core)
                .navigationSplitViewColumnWidth(min: 230, ideal: 280, max: 420)
        } detail: {
            // No minimum width of its own: on macOS 26 the split view counts
            // the sidebar (which the detail runs under) twice in the detail's
            // minimum, so any floor here makes the columns wider than the
            // window, centred and clipped on both sides. `fitColumns` hides
            // the sidebar instead.
            DetailView(core: core)
                // The panel belongs to a thread (its diff, terminal, files):
                // not shown over Pull Requests or Usage.
                .inspector(isPresented: Binding(
                    get: { panelShown },
                    set: { open in if open != core.view.panelOpen { core.update(.togglePanel) } }
                )) {
                    SidePanel(core: core)
                        .inspectorColumnWidth(min: 280, ideal: 420, max: 900)
                }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { splitWidth = $0; fitColumns() }
        .background(WindowWidthReader { windowWidth = $0; fitColumns() })
        .onChange(of: panelShown) { _, shown in
            // The panel closed: the room it took is back, so try the sidebar
            // again (it hides once more if that still doesn't fit).
            if !shown, sidebarHiddenNeeding != nil {
                sidebarHiddenNeeding = nil
                columns = .all
            }
        }
        .onChange(of: columns) { _, visibility in
            // The sidebar shown again by hand while it was hidden for room:
            // the latest ask wins, and the panel makes way.
            guard visibility != .detailOnly, sidebarHiddenNeeding != nil else { return }
            sidebarHiddenNeeding = nil
            if panelShown { core.update(.togglePanel) }
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

extension MainView {
    /// Hides the sidebar when the columns are laid out wider than the window
    /// (the window doesn't grow to fit them), and shows it again once the
    /// window is as wide as they needed.
    private func fitColumns() {
        guard windowWidth > 0, splitWidth > 0 else { return }
        if columns != .detailOnly, splitWidth > windowWidth + 1 {
            sidebarHiddenNeeding = splitWidth
            columns = .detailOnly
        } else if let needed = sidebarHiddenNeeding, windowWidth >= needed {
            sidebarHiddenNeeding = nil
            columns = .all
        }
    }
}

/// Reports its window's width now and whenever the window resizes.
private struct WindowWidthReader: NSViewRepresentable {
    let changed: (CGFloat) -> Void

    func makeNSView(context: Context) -> ReaderView { ReaderView() }

    func updateNSView(_ view: ReaderView, context: Context) { view.changed = changed }

    final class ReaderView: NSView {
        var changed: ((CGFloat) -> Void)?
        private var observer: NSObjectProtocol?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            observer.map(NotificationCenter.default.removeObserver)
            observer = nil
            guard let window else { return }
            observer = NotificationCenter.default.addObserver(forName: NSWindow.didResizeNotification, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.report() }
            }
            DispatchQueue.main.async { [weak self] in self?.report() }
        }

        private func report() {
            guard let window else { return }
            changed?(window.contentLayoutRect.width)
        }

        deinit { observer.map(NotificationCenter.default.removeObserver) }
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

    @EnvironmentObject private var companion: CompanionBrain
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var location = MenuLocation()
    @State private var openingMenu = false

    var body: some View {
        Button {
            guard !openingMenu else { return }
            if reduceMotion || companion.expression.mood == .offline {
                showMenu()
            } else {
                companion.poke()
                openingMenu = true
                Task { @MainActor in
                    defer { openingMenu = false }
                    do { try await Task.sleep(for: .milliseconds(100)) }
                    catch { return }
                    showMenu()
                }
            }
        } label: {
            ToolbarCompanion(look: connection.companion.look, brain: companion)
                .padding(4)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(MenuAnchor(location: location))
        .accessibilityLabel(connection.companionServerLabel)
        .accessibilityValue(connection.companionVoiceOverValue(companion.expression.mood))
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

    private func showMenu() {
        guard let anchor = location.anchor, anchor.window != nil else { return }
        let menu = NSMenu()
        menu.addItem(.sectionHeader(title: "Servers"))
        for env in connection.environments {
            let item = ServerMenuItem("\(env.label) — \(env.detail)") {
                core.update(.selectEnvironment(id: env.id))
            }
            item.state = env.active ? .on : .off
            item.image = NSImage(systemSymbolName: env.remote ? "globe" : "desktopcomputer", accessibilityDescription: nil)
            menu.addItem(item)
        }
        if let active = connection.environments.first(where: { $0.active }) {
            menu.addItem(ServerMenuItem("Rename \(active.label)…") {
                newName = active.label
                renaming = active
            })
            menu.addItem(ServerMenuItem("Forget \(active.label)…") { forgetting = active })
            if let index = connection.environments.firstIndex(where: { $0.id == active.id }), connection.environments.count > 1 {
                if index > 0 {
                    menu.addItem(ServerMenuItem("Move \(active.label) Up") { core.update(.moveEnvironment(id: active.id, delta: -1)) })
                }
                if index < connection.environments.count - 1 {
                    menu.addItem(ServerMenuItem("Move \(active.label) Down") { core.update(.moveEnvironment(id: active.id, delta: 1)) })
                }
            }
        }
        menu.addItem(.separator())
        menu.addItem(ServerMenuItem("Add Server…", perform: addServer))
        if connection.state == "connected" {
            menu.addItem(ServerMenuItem("Disconnect") { core.update(.disconnect) })
        }
        menu.popUp(positioning: nil, at: CGPoint(x: anchor.bounds.minX, y: anchor.bounds.minY), in: anchor)
    }

    @MainActor
    private final class MenuLocation {
        weak var anchor: NSView?
    }

    private struct MenuAnchor: NSViewRepresentable {
        let location: MenuLocation
        func makeNSView(context: Context) -> NSView { Anchor() }
        func updateNSView(_ view: NSView, context: Context) { location.anchor = view }

        private final class Anchor: NSView {
            override func hitTest(_ point: NSPoint) -> NSView? { nil }
        }
    }
}

@MainActor
private final class ServerMenuItem: NSMenuItem {
    private let perform: () -> Void

    init(_ title: String, perform: @escaping () -> Void) {
        self.perform = perform
        super.init(title: title, action: #selector(invoke), keyEquivalent: "")
        target = self
    }

    required init(coder: NSCoder) { fatalError("Server menu items are built in code") }

    @objc private func invoke() { perform() }
}

// MARK: - Connect

/// Moves a saved server up or down the list (the order servers show in).
private struct ServerOrderButtons: View {
    @ObservedObject var core: Core
    let env: EnvironmentView

    private var servers: [EnvironmentView] { core.view.connection.environments }

    var body: some View {
        let index = servers.firstIndex { $0.id == env.id } ?? 0
        HStack(spacing: 0) {
            Button { core.update(.moveEnvironment(id: env.id, delta: -1)) } label: { Image(systemName: "chevron.up") }
                .disabled(index == 0)
                .help("Move up")
                .accessibilityLabel("Move \(env.label) up")
            Button { core.update(.moveEnvironment(id: env.id, delta: 1)) } label: { Image(systemName: "chevron.down") }
                .disabled(index >= servers.count - 1)
                .help("Move down")
                .accessibilityLabel("Move \(env.label) down")
        }
        .buttonStyle(.borderless)
    }
}

/// The server refuses this device for good (it was reset, or signed the
/// device out): pair again, or forget it.
struct ServerChangedView: View {
    @ObservedObject var core: Core
    let pairAgain: () -> Void
    @State private var forgetting: EnvironmentView?

    private var connection: ConnectionView { core.view.connection }
    private var active: EnvironmentView? { connection.environments.first { $0.id == connection.activeEnvironmentId } }
    private var label: String { active?.label ?? "This server" }

    var body: some View {
        VStack(spacing: Space.l) {
            Image(systemName: "exclamationmark.arrow.triangle.2.circlepath")
                .font(.system(size: Size.heroIcon))
                .foregroundStyle(Tint.caution)
                .accessibilityHidden(true)
            Text("\(label) no longer accepts this Mac").font(.title2.bold()).multilineTextAlignment(.center)
            if let error = connection.error {
                Text(error).foregroundStyle(.secondary).multilineTextAlignment(.center).textSelection(.enabled)
            }
            if !connection.hint.isEmpty {
                Text(connection.hint).font(TypeRole.meta).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            HStack(spacing: Space.s) {
                Button("Forget \(label)…", role: .destructive) { forgetting = active }
                    .disabled(active == nil)
                Button("Pair Again", action: pairAgain)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(Space.xxl)
        .frame(maxWidth: 520)
        .cardSurface(Radius.card + 6)
        .padding(Space.xxl)
        .frame(maxWidth: .infinity)
        .scrollableCentered()
        .forgettingServer($forgetting, core: core)
    }
}

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
                DomovoiMark(size: 42)
                VStack(alignment: .leading) {
                    Text("Connect to a Domovoi server").font(.title2.bold())
                    Text("domovoi-server on this Mac or another machine").foregroundStyle(.secondary)
                }
            }
            if let local = connection.localServer {
                // The server running on this Mac lets this user's apps in
                // without a code.
                GroupBox("Server on this computer") {
                    HStack(spacing: Space.s) {
                        Image(systemName: "desktopcomputer").foregroundStyle(.secondary)
                        VStack(alignment: .leading) {
                            Text("\(local.label) · \(local.url)").lineLimit(1)
                            Text("No pairing code needed.").font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Connect") { core.update(.connectLocalServer) }
                            .buttonStyle(.borderedProminent)
                            .disabled(busy)
                    }
                    .padding(Space.xs)
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
                                if connection.environments.count > 1 {
                                    ServerOrderButtons(core: core, env: env)
                                }
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
