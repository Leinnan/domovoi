import App
import SwiftUI

@main
struct DomovoiApp: App {
    @StateObject private var core = Core()
    @State private var network = NetworkWatcher()

    var body: some Scene {
        // One window: the app's state (selection, panel, composer) is one.
        Window("Domovoi", id: "main") {
            ContentView(core: core)
                .environment(\.core, core)
                // Room for the thread and the panel; `MainView` hides the
                // sidebar when all three columns don't fit.
                .frame(minWidth: 720, minHeight: 560)
                // `domovoi://connect?…` links, from a browser or another app.
                .onOpenURL { url in
                    if url.scheme == "domovoi" { core.update(.connectRemote(link: url.absoluteString)) }
                }
                .task {
                    network.start(online: { core.update(.networkStatus(online: $0)) }) { core.update(.networkChanged) }
                }
                // After sleep the old connection is usually dead: check at once.
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didWakeNotification)) { _ in
                    network.refresh()
                    core.update(.networkChanged)
                }
        }
        .handlesExternalEvents(matching: ["domovoi"])
        .defaultSize(width: 1440, height: 900)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Thread") { core.update(.newThread(projectId: nil)) }
                    .keyboardShortcut("n")
                Button("New Thread Without a Project") { core.update(.openScratch) }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
            }
            CommandMenu("Thread") {
                Button("Command Palette") { core.update(.openPalette) }
                    .keyboardShortcut("k")
                // The menus read the narrow parts (header, composerMenu): the
                // whole thread or composer would rebuild them on every token
                // and keystroke.
                Button("Find in Thread") {
                    if core.view.header?.findTotal != nil { FindFocus.shared.revision += 1 } else { core.update(.findInThread(query: "")) }
                }
                .keyboardShortcut("f")
                .disabled(core.view.header == nil)
                Button("Find Next") { core.update(.findNext(forward: true)) }
                    .keyboardShortcut("g")
                    .disabled((core.view.header?.findTotal ?? 0) == 0)
                Button("Find Previous") { core.update(.findNext(forward: false)) }
                    .keyboardShortcut("g", modifiers: [.command, .shift])
                    .disabled((core.view.header?.findTotal ?? 0) == 0)
                Button("Stop") { core.update(.interrupt) }
                    .keyboardShortcut(".")
                    .disabled(!core.view.composerMenu.canStop)
                Divider()
                ComposerCommandItems(core: core)
            }
            SidebarCommands()
            ToolbarCommands()
            // The panel is part of the window's layout: in View, beside the sidebar.
            CommandGroup(after: .sidebar) {
                Button("Toggle Side Panel") { core.update(.togglePanel) }
                    .keyboardShortcut("j")
                Button("Show Diff") { core.update(.showPanel(tab: "diff")) }
                    .keyboardShortcut("d", modifiers: [.command, .shift])
                Button("Show Artifacts") { core.update(.showPanel(tab: "artifacts")) }
                    .keyboardShortcut("a", modifiers: [.command, .shift])
                Button("Show Terminal") {
                    TerminalFocus.requested = true
                    core.update(.showPanel(tab: "terminal"))
                }
                .keyboardShortcut("t", modifiers: [.command, .shift])
                Button("Show Files") { core.update(.showPanel(tab: "files")) }
                    .keyboardShortcut("e", modifiers: [.command, .shift])
                // ⇧⌘G is Find Previous, as everywhere on the Mac.
                Button("Show Git") { core.update(.showPanel(tab: "git")) }
                    .keyboardShortcut("g", modifiers: [.command, .option])
            }
            // Nothing here prints: ⌘P is Go to File, as in editors.
            CommandGroup(replacing: .printItem) {}
            CommandMenu("Go") {
                Button("Go to File…") { openPalette("files") }
                    .keyboardShortcut("p")
                Button("Search in Project…") { openPalette("content") }
                    .keyboardShortcut("f", modifiers: [.command, .shift])
                Divider()
                Button("Pull Requests") { core.update(.showRoute(route: "pull-requests")) }
                Button("Usage") { core.update(.showRoute(route: "usage")) }
                Divider()
                RemoteAccessMenuItem()
            }
        }

        Settings {
            SettingsWindow(core: core)
        }
    }

    /// The palette, open on going to a file or searching the project.
    private func openPalette(_ mode: String) {
        if core.view.palette == nil { core.update(.openPalette) }
        core.update(.setPaletteMode(mode: mode))
    }
}

/// Opens Settings on the Remote Access (Access) tab.
struct RemoteAccessMenuItem: View {
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        Button("Remote Access…") {
            SettingsTab.shared.selection = "access"
            openSettings()
        }
    }
}

/// The composer's choices in the Thread menu: focus, plan mode, effort,
/// model and permissions, each with a shortcut where it has one.
struct ComposerCommandItems: View {
    @ObservedObject var core: Core
    private let commands = ComposerCommands.shared

    private var composer: ComposerMenu { core.view.composerMenu }
    /// The composer is on screen (a thread or a new thread).
    private var available: Bool {
        let c = core.view.connection.state
        return (c == "connected" || c == "reconnecting") && !["pull-requests", "usage"].contains(core.view.route)
    }

    var body: some View {
        Button("Focus Message") { commands.focus() }
            .keyboardShortcut("l")
            .disabled(!available)
        Toggle("Plan First", isOn: Binding(
            get: { composer.interactionMode == "plan" },
            set: { _ in commands.togglePlan(core: core) }
        ))
        .keyboardShortcut("p", modifiers: [.command, .shift])
        .disabled(!available)
        Button("Increase Effort") { commands.stepEffort(1, core: core) }
            .keyboardShortcut("]")
            .disabled(!available || !composer.canRaiseEffort)
        Button("Decrease Effort") { commands.stepEffort(-1, core: core) }
            .keyboardShortcut("[")
            .disabled(!available || !composer.canLowerEffort)
        Button("Choose Model…") { commands.open = .model }
            .keyboardShortcut("m", modifiers: [.command, .shift])
            .disabled(!available || !composer.hasModels)
        Menu("Permissions") {
            ForEach(composer.runtimeModes, id: \.value) { mode in
                Toggle(mode.value == "full-access" ? "\(mode.label)…" : mode.label, isOn: Binding(
                    get: { composer.runtimeMode == mode.value },
                    set: { on in if on { commands.setPermissions(mode.value, core: core) } }
                ))
            }
        }
        .disabled(!available)
    }
}
