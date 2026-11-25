import App
import SwiftUI

/// Where the navigation stack can go.
enum Screen: Hashable {
    case thread(String)
    case newThread
    case diff
    case terminal
    case files
    case git
    case pullRequests
    case usage
    case settings
    case companion(String)
    case importSessions
    case browseFolders
    case serverSettings
    case providers
    case scheduledTasks
    case maintenance
    case access
}

struct RootView: View {
    @ObservedObject var core: Core
    @StateObject private var companion = CompanionBrain()

    private var connection: ConnectionView { core.view.connection }

    /// The app proper shows once an environment is active and paired.
    private var showMain: Bool {
        switch connection.state {
        case "connected", "reconnecting": return true
        case "connecting", "disconnected": return !connection.activeEnvironmentId.isEmpty && !connection.needsPairing
        default: return false
        }
    }

    var body: some View {
        Group {
            if connection.serverChanged {
                NavigationStack { ServerChangedView(core: core) }
            } else if showMain {
                MainStack(core: core)
            } else {
                NavigationStack { AddServerView(core: core, onboarding: true) }
            }
        }
        // In the app proper each screen shows toasts under its navigation
        // bar (`statusBars`); before it, over the pairing screens.
        .overlay(alignment: .top) {
            if !showMain || connection.serverChanged { ToastCapsule(core: core) }
        }
        .environmentObject(companion)
        .environment(\.notice, NoticeSink(core: core))
        .modifier(CompanionWindow(brain: companion, connection: connection))
        .task { await IOSSnapshots.run(core) }
        .onChange(of: connection.state) { _, state in
            // Already allowed: register at once. The first ask comes when the
            // user starts a thread, when hearing it finished makes sense.
            if state == "connected" { AppDelegate.registerIfAllowed() }
        }
    }
}

private struct SidebarOpenKey: EnvironmentKey {
    static let defaultValue: (@MainActor (Screen) -> Void)? = nil
}

extension EnvironmentValues {
    /// In the iPad split view, rows of the thread list open their screen in
    /// the detail column (instead of pushing onto the sidebar).
    var sidebarOpen: (@MainActor (Screen) -> Void)? {
        get { self[SidebarOpenKey.self] }
        set { self[SidebarOpenKey.self] = newValue }
    }
}

struct MainStack: View {
    @ObservedObject var core: Core
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var path: [Screen] = []
    /// A project was asked for (added folder, new project, no project): the
    /// composer opens once the server has made it.
    @State private var awaitingProject = false

    var body: some View {
        Group {
            if sizeClass == .regular {
                split
            } else {
                NavigationStack(path: $path) {
                    ThreadListView(core: core, open: open, expectProject: { awaitingProject = true })
                        .statusBars(core)
                        .navigationDestination(for: Screen.self) { screen in
                            // One view per screen: a path replaced in place
                            // ([.thread(A)] -> [.thread(B)], from a notification
                            // or the palette) builds B's afresh, with none of
                            // A's state (as the iPad's detail does).
                            destination(screen).statusBars(core).id(screen)
                        }
                }
            }
        }
        .onReceive(IOSSnapshots.navigate) { screens in path = screens }
        .sheet(isPresented: Binding(
            get: { core.view.palette != nil },
            // Only a palette still open: a quick ⌘K twice must not have the
            // first sheet's dismissal close the second palette.
            set: { if !$0, core.view.palette != nil { core.update(.closePalette) } }
        )) {
            PaletteScreen(core: core, run: runPalette)
                // Full height: it opens with the keyboard up, which leaves half a sheet no room for results.
                .presentationDetents([.large]).presentationDragIndicator(.visible)
        }
        // A tapped notification shows its thread, even the one last open.
        .onReceive(core.openedThread) { id in
            if path != [.thread(id)] { path = [.thread(id)] }
        }
        // A thread restored before this screen existed (the one open when
        // the app was closed) shows too.
        .onAppear {
            if path.isEmpty, core.view.route == "thread", let id = core.view.sidebar.selectedThreadId { path = [.thread(id)] }
        }
        .onChange(of: core.view.sidebar.selectedThreadId) { _, id in
            // Selected outside the stack: show it.
            guard let id, core.view.route == "thread", !path.contains(.thread(id)) else { return }
            path = [.thread(id)]
        }
        // The open thread was archived or deleted (the core went to a new
        // thread): on iPad the detail column must not wait on it forever.
        .onChange(of: core.view.route) { _, route in
            // Only when the top screen is a thread the core no longer has open:
            // a New Thread asked for from inside a thread pushes its own screen.
            guard route == "new-thread", core.view.threadId == nil, case .thread = path.last else { return }
            path = []
        }
        // Another server: the screens of the old one mean nothing there.
        .onChange(of: core.view.connection.activeEnvironmentId) { _, _ in
            path = []
            awaitingProject = false
        }
        // The core selects a project the server just made.
        .onChange(of: core.view.sidebar.selectedProjectId) { _, id in
            guard awaitingProject, id != nil else { return }
            awaitingProject = false
            path = [.newThread]
        }
        // A failed create leaves nothing to wait for.
        .onChange(of: core.view.toast?.id) { _, _ in
            if core.view.toast?.kind == "error" { awaitingProject = false }
        }
    }

    /// A screen already in the stack is gone back to rather than pushed
    /// again: a subagent's "Subagent of …" link, a fork that the stack and
    /// the thread screen both react to.
    private func open(_ screen: Screen) {
        if let i = path.firstIndex(of: screen) {
            path = Array(path.prefix(through: i))
        } else {
            path.append(screen)
        }
    }

    /// A palette item: the core runs what is its own; the screens it would
    /// show on the Mac (panel tabs, pages) open here instead.
    private func runPalette(_ item: PaletteItemView) {
        let id = item.id
        if id.hasPrefix("thread:") {
            core.update(.runPaletteItem(id: id))
            path = [.thread(String(id.dropFirst("thread:".count)))]
            return
        }
        if id.hasPrefix("file:") || id.hasPrefix("match:") {
            // The core opens it in the files panel: its screen here.
            core.update(.runPaletteItem(id: id))
            let rest = id.hasPrefix("file:") ? String(id.dropFirst("file:".count)) : String(id.dropFirst("match:".count))
            FilesScreen.reveal = id.hasPrefix("file:") ? rest : rest.split(separator: ":", omittingEmptySubsequences: false).dropLast().joined(separator: ":")
            open(.files)
            return
        }
        if id.hasPrefix("project:") || id == "action:new-thread" {
            core.update(.runPaletteItem(id: id))
            path = [.newThread]
            return
        }
        let screens: [String: Screen] = [
            "action:diff": .diff, "action:terminal": .terminal, "action:files": .files, "action:git": .git,
            "action:pull-requests": .pullRequests, "action:usage": .usage,
        ]
        if let screen = screens[id] {
            core.update(.closePalette)
            open(screen)
        } else if id == "action:scratch" {
            core.update(.closePalette)
            // An existing "No project" project is used as is.
            if let scratch = core.view.sidebar.projects.first(where: { $0.isScratch }) {
                core.update(.newThread(projectId: scratch.id))
                path = [.newThread]
            } else {
                awaitingProject = true
                core.update(.openScratch)
            }
        } else {
            // Find, pin, settle (on the open thread); go to file and search
            // (the palette stays open in that mode).
            core.update(.runPaletteItem(id: id))
        }
    }

    /// iPad: the thread list beside the open thread. The first screen of the
    /// path fills the detail column; the rest push onto it.
    private var split: some View {
        NavigationSplitView {
            ThreadListView(core: core, open: { path = [$0] }, expectProject: { awaitingProject = true })
                .statusBars(core)
                .environment(\.sidebarOpen, { screen in path = [screen] })
        } detail: {
            NavigationStack(path: Binding(get: { Array(path.dropFirst()) }, set: { rest in path = Array(path.prefix(1)) + rest })) {
                Group {
                    if let root = path.first {
                        destination(root).statusBars(core).id(root)
                    } else {
                        ContentUnavailableView {
                            Label("No thread open", systemImage: "bubble.left.and.text.bubble.right")
                        } description: {
                            Text("Pick a thread in the list, or start a new one.")
                        } actions: {
                            Button("New Thread") {
                                core.update(.newThread(projectId: core.view.sidebar.selectedProjectId))
                                path = [.newThread]
                            }
                            // ⌘N belongs to the sidebar's New Thread button.
                        }
                    }
                }
                .navigationDestination(for: Screen.self) { screen in
                    destination(screen).statusBars(core)
                }
            }
        }
    }

    @ViewBuilder
    private func destination(_ screen: Screen) -> some View {
        switch screen {
        case .thread(let id): ThreadScreen(core: core, threadId: id, open: open)
        case .newThread: NewThreadScreen(core: core, launched: { id in
            path = [.thread(id)]
            AppDelegate.requestPush()
        })
        case .diff: DiffScreen(core: core)
        case .terminal: TerminalScreenIOS(core: core)
        case .files: FilesScreen(core: core)
        case .git: GitScreen(core: core)
        case .pullRequests: PullRequestsScreen(core: core)
        case .usage: UsageScreen(core: core)
        case .settings: SettingsScreen(core: core)
        case .companion(let id):
            CompanionEditor(core: core, environmentId: id)
                .navigationTitle("Companion").navigationBarTitleDisplayMode(.inline)
        case .importSessions: ImportSessionsScreen(core: core)
        case .browseFolders: FolderBrowserScreen(core: core, open: open, expectProject: { awaitingProject = true })
        case .serverSettings: ServerSettingsScreen(core: core)
        case .providers: ProvidersScreen(core: core)
        case .scheduledTasks: ScheduledTasksScreen(core: core, open: open)
        case .maintenance: MaintenanceScreen(core: core)
        case .access: AccessScreen(core: core)
        }
    }
}

extension View {
    /// Connection trouble and toasts, under this screen's navigation bar:
    /// they push the content down instead of covering the bar's buttons.
    func statusBars(_ core: Core) -> some View {
        safeAreaBar(edge: .top, spacing: 0) {
            StatusBars(core: core)
        }
    }
}

private struct StatusBars: View {
    @ObservedObject var core: Core

    var body: some View {
        VStack(spacing: Space.s) {
            ConnectionBanner(core: core)
            ToastCapsule(core: core)
        }
        .padding(.bottom, core.view.toast != nil || ConnectionBanner.showing(core) ? Space.s : 0)
        .animation(.snappy, value: core.view.connection.state)
        .animation(.snappy, value: core.view.connection.offline)
    }
}


/// The server refuses this device for good (reinstalled, replaced, or not a
/// Domovoi server any more): pair again, or forget it.
struct ServerChangedView: View {
    @ObservedObject var core: Core
    @State private var pairing = false

    private var connection: ConnectionView { core.view.connection }
    private var label: String {
        connection.environments.first { $0.id == connection.activeEnvironmentId }?.label ?? "This server"
    }

    var body: some View {
        VStack(spacing: Space.l) {
            Image(systemName: "exclamationmark.arrow.triangle.2.circlepath")
                .font(.system(size: Size.heroIcon))
                .foregroundStyle(Tint.caution)
                .accessibilityHidden(true)
            Text("\(label) no longer accepts this device").font(.title3.bold()).multilineTextAlignment(.center)
            if let error = connection.error {
                Text(error).font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            Button {
                pairing = true
            } label: {
                Text("Pair Again").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            Button(role: .destructive) {
                core.update(.forgetEnvironment(id: connection.activeEnvironmentId))
            } label: {
                Text("Forget \(label)").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
        }
        .padding(Space.xl)
        .frame(maxWidth: 420)
        .navigationDestination(isPresented: $pairing) { AddServerView(core: core, onboarding: false) }
    }
}
