import App
import SwiftUI

/// Subtle context header at the top of every server-specific settings screen,
/// making it unmistakable which server is being configured.
private struct ServerContextSection: View {
    @ObservedObject var core: Core

    private var connection: ConnectionView { core.view.connection }
    private var serverName: String {
        connection.environmentLabel.isEmpty ? "This server" : connection.environmentLabel
    }

    var body: some View {
        Section {
            HStack(spacing: Space.m) {
                CompanionStill(
                    look: connection.companion.look,
                    size: 26,
                    mood: connection.state == "connected" ? .idle : .offline
                )
                VStack(alignment: .leading, spacing: 1) {
                    Text(serverName)
                        .font(.subheadline.weight(.semibold))
                    Text("Server setting · Shared across all devices")
                        .font(TypeRole.metaSmall)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Circle()
                    .fill(connection.state == "connected" ? Tint.success : Tint.caution)
                    .frame(width: 8, height: 8)
            }
            .padding(.vertical, 2)
        }
    }
}

/// Level-2 hub for configuring one specific server: live connection status,
/// server-stored configuration sections, and device-local customizations.
struct ServerHubScreen: View {
    @ObservedObject var core: Core
    let environmentId: String
    let open: (Screen) -> Void
    @EnvironmentObject private var companion: CompanionBrain
    @State private var renaming = false
    @State private var newName = ""
    @State private var forgetting = false

    private var connection: ConnectionView { core.view.connection }
    private var settings: SettingsView { core.view.settings }
    private var server: EnvironmentView? {
        connection.environments.first(where: { $0.id == environmentId })
    }
    private var isActive: Bool { connection.activeEnvironmentId == environmentId }
    private var isConnected: Bool { isActive && connection.state == "connected" }
    private var isConnecting: Bool {
        !isActive || ["connecting", "reconnecting", "pairing"].contains(connection.state)
    }
    private var serverLabel: String {
        server?.label ?? (connection.environmentLabel.isEmpty ? "Server" : connection.environmentLabel)
    }

    private var readyProviderCount: Int {
        settings.providers.filter { $0.status == "ready" }.count
    }

    var body: some View {
        Form {
            if let server {
                Section {
                    HStack(spacing: Space.m) {
                        if isActive {
                            CompanionView(look: server.companion.look, brain: companion, size: 40)
                        } else {
                            CompanionStill(look: server.companion.look, size: 40, mood: .offline)
                        }
                        VStack(alignment: .leading, spacing: Space.xxs) {
                            Text(server.label)
                                .font(.headline)
                            statusLine
                        }
                    }
                    .padding(.vertical, 4)

                    if isConnected {
                        if connection.remote {
                            LabeledContent("Route", value: connection.path.isEmpty ? "Remote" : "\(connection.path.capitalized), \(connection.rttMs) ms")
                        } else if !connection.serverUrl.isEmpty {
                            LabeledContent("Address", value: connection.serverUrl)
                        }
                        if !connection.serverVersion.isEmpty {
                            LabeledContent("Server", value: "domovoi-server \(connection.serverVersion)")
                        }
                        if !connection.platform.isEmpty {
                            LabeledContent("Platform", value: connection.platform)
                        }
                        Button("Refresh from Server", systemImage: "arrow.clockwise") { core.update(.resync) }
                        Button("Disconnect", role: .destructive) { core.update(.disconnect) }
                    } else if isConnecting {
                        HStack(spacing: Space.s) {
                            ProgressView().controlSize(.small)
                            Text(connection.state == "reconnecting" ? "Reconnecting to \(server.label)…" : "Connecting to \(server.label)…")
                                .foregroundStyle(.secondary)
                        }
                    } else {
                        if let error = connection.error, !error.isEmpty {
                            Text(error).font(TypeRole.meta).foregroundStyle(Tint.danger)
                        }
                        Button("Reconnect", systemImage: "arrow.clockwise") { core.update(.reconnect) }
                    }
                } header: {
                    Text("Connection")
                }

                Section {
                    NavigationLink(value: Screen.serverSettings) {
                        Label("General Settings", systemImage: "gearshape.2")
                    }
                    NavigationLink(value: Screen.providers) {
                        LabeledContent {
                            if isConnected && !settings.providers.isEmpty {
                                Text("\(readyProviderCount)/\(settings.providers.count)")
                                    .foregroundStyle(.secondary)
                            }
                        } label: {
                            Label("Providers", systemImage: "cpu")
                        }
                    }
                    NavigationLink(value: Screen.serverProjects) {
                        LabeledContent {
                            if isConnected {
                                Text("\(settings.projects.count)")
                                    .foregroundStyle(.secondary)
                            }
                        } label: {
                            Label("Projects", systemImage: "folder")
                        }
                    }
                    NavigationLink(value: Screen.scheduledTasks) {
                        Label("Scheduled Tasks", systemImage: "calendar.badge.clock")
                    }
                    NavigationLink(value: Screen.access) {
                        Label("Access & Devices", systemImage: "person.badge.key")
                    }
                    NavigationLink(value: Screen.archivedThreads) {
                        LabeledContent {
                            if isConnected && !settings.archived.isEmpty {
                                Text("\(settings.archived.count)")
                                    .foregroundStyle(.secondary)
                            }
                        } label: {
                            Label("Archived Threads", systemImage: "archivebox")
                        }
                    }
                    NavigationLink(value: Screen.maintenance) {
                        Label("Maintenance", systemImage: "wrench.and.screwdriver")
                    }
                } header: {
                    Text("Server Configuration")
                } footer: {
                    Text(isConnected
                         ? "Stored on \(server.label) and shared across all your devices."
                         : "Connect to \(server.label) to view and change server configuration.")
                }
                .disabled(!isConnected)

                Section {
                    NavigationLink(value: Screen.companion(server.id)) {
                        SettingsRow(
                            title: "Companion",
                            subtitle: server.companion.custom ? "Custom look on this device" : "Drawn from the server's key"
                        ) {
                            CompanionStill(look: server.companion.look, size: 26)
                        }
                    }
                    Button {
                        newName = server.label
                        renaming = true
                    } label: {
                        Label("Rename on This Device…", systemImage: "pencil")
                    }
                    Button(role: .destructive) {
                        forgetting = true
                    } label: {
                        Label("Forget Server…", systemImage: "trash")
                    }
                } header: {
                    Text("On This Device")
                } footer: {
                    Text("Saved locally on this device for \(server.label). Forgetting the active server also signs this device out there.")
                }
            } else {
                ContentUnavailableView("Server Removed", systemImage: "server.rack", description: Text("This server is no longer saved on this device."))
            }
        }
        .navigationTitle(serverLabel)
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            if connection.activeEnvironmentId != environmentId {
                core.update(.selectEnvironment(id: environmentId))
            } else if connection.state == "connected" {
                loadHubData()
            }
        }
        .onChange(of: connection.state) { _, state in
            if state == "connected" && isActive {
                loadHubData()
            }
        }
        .alert("Rename Server", isPresented: $renaming) {
            TextField("Name", text: $newName)
            Button("Rename") { core.update(.renameEnvironment(id: environmentId, label: newName)) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Changes how this server is named on this device only.")
        }
        .confirmationDialog("Forget \(serverLabel)?", isPresented: $forgetting, titleVisibility: .visible) {
            Button("Forget", role: .destructive) { core.update(.forgetEnvironment(id: environmentId)) }
        } message: {
            Text(isActive
                 ? "This device signs out there and stops getting its notifications. Pairing again needs a new code."
                 : "It's only removed from this device.")
        }
    }

    private func loadHubData() {
        core.update(.loadRemoteAccess)
        core.update(.loadSettings(section: "archived"))
        core.update(.loadSettings(section: "providers"))
    }

    @ViewBuilder
    private var statusLine: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(isConnected ? Tint.success : isConnecting ? Tint.caution : .secondary)
                .frame(width: 7, height: 7)
            Text(isConnected ? "Connected" : isConnecting ? (connection.state == "reconnecting" ? "Reconnecting…" : "Connecting…") : "Offline")
                .font(TypeRole.meta)
                .foregroundStyle(.secondary)
        }
    }
}

/// Projects registered on the active server.
struct ServerProjectsScreen: View {
    @ObservedObject var core: Core
    @State private var pickingProjectIcon: ProjectSettingsView?
    @State private var renamingProject: ProjectSettingsView?
    @State private var projectName = ""
    @State private var removing: ProjectSettingsView?

    var body: some View {
        Form {
            LinearProjectSettings(core: core)
            ServerContextSection(core: core)
            Section {
                if core.view.settings.projects.isEmpty {
                    Text("No projects on this server yet.")
                        .foregroundStyle(.secondary)
                }
                ForEach(core.view.settings.projects, id: \.id) { project in
                    VStack(alignment: .leading, spacing: Space.xs) {
                        HStack(spacing: Space.m) {
                            Button { pickingProjectIcon = project } label: {
                                ProjectIconTile(icon: project.icon, size: Size.listIcon, core: core)
                                    .tapTarget()
                            }
                            .buttonStyle(.borderless)
                            .disabled(project.icon.saving)
                            .accessibilityLabel("Change icon for \(project.title)")
                            .iconColumn(Size.listIcon)
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                Text(project.title)
                                Text("\(project.workspaceRoot) · \(project.threadCount) threads")
                                    .font(TypeRole.meta)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                            }
                        }
                        if !project.isScratch {
                            ProjectIndexRow(core: core, project: project)
                        }
                    }
                    .swipeActions {
                        Button("Remove", role: .destructive) { removing = project }
                        Button("Rename") {
                            projectName = project.title
                            renamingProject = project
                        }
                        .tint(.blue)
                    }
                    .contextMenu {
                        Button("Change Icon…") { pickingProjectIcon = project }.disabled(project.icon.saving)
                        Button("Rename…") {
                            projectName = project.title
                            renamingProject = project
                        }
                        Button("Remove", role: .destructive) { removing = project }
                    }
                }
            } header: {
                Text("Projects")
            } footer: {
                Text("Removing a project keeps its folder on the server. A removed project is not imported again on its own.")
            }

            Section("Add to Server") {
                NavigationLink(value: Screen.importSessions) { Label("Import from Claude Code", systemImage: "square.and.arrow.down") }
                NavigationLink(value: Screen.browseFolders) { Label("Add Folder on Server", systemImage: "folder.badge.plus") }
            }
        }
        .navigationTitle("Projects")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadSettings(section: "projects")) }
        .sheet(isPresented: Binding(get: { pickingProjectIcon != nil }, set: { if !$0 { pickingProjectIcon = nil } })) {
            if let project = pickingProjectIcon { ProjectIconPicker(project: project, core: core) }
        }
        .confirmationDialog(
            "Remove \(removing?.title ?? "this project")?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible,
            presenting: removing
        ) { project in
            Button("Remove", role: .destructive) { core.update(.removeProject(projectId: project.id)) }
        } message: { _ in
            Text("Its folder stays on the server. Its threads go with it.")
        }
        .alert("Rename Project", isPresented: Binding(get: { renamingProject != nil }, set: { if !$0 { renamingProject = nil } })) {
            TextField("Name", text: $projectName)
            Button("Rename") {
                if let project = renamingProject { core.update(.renameProject(projectId: project.id, title: projectName)) }
            }
            Button("Cancel", role: .cancel) {}
        }
    }
}

/// Archived threads stored on the active server.
struct ArchivedThreadsScreen: View {
    @ObservedObject var core: Core
    @State private var deleting: ThreadRowView?

    var body: some View {
        Form {
            ServerContextSection(core: core)
            if !core.view.settings.archived.isEmpty {
                Section("Archived Threads") {
                    ForEach(core.view.settings.archived, id: \.id) { thread in
                        HStack {
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                Text(thread.title).lineLimit(2)
                                Text(thread.age).font(TypeRole.meta).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("Unarchive") { core.update(.archiveThread(threadId: thread.id, archived: false)) }
                                .buttonStyle(.borderless)
                        }
                        .swipeActions {
                            Button("Delete", role: .destructive) { deleting = thread }
                        }
                    }
                }
            }
        }
        .overlay {
            if core.view.settings.archived.isEmpty {
                ContentUnavailableView(
                    "No Archived Threads",
                    systemImage: "archivebox",
                    description: Text("Threads you archive on \(core.view.connection.environmentLabel.isEmpty ? "this server" : core.view.connection.environmentLabel) appear here.")
                )
            }
        }
        .navigationTitle("Archived Threads")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadSettings(section: "archived")) }
        .onChange(of: core.view.connection.state) { _, state in
            if state == "connected" { core.update(.loadSettings(section: "archived")) }
        }
        .confirmationDialog(
            "Delete \(deleting?.title ?? "this thread")?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { thread in
            Button("Delete", role: .destructive) { core.update(.deleteThread(threadId: thread.id)) }
        } message: { _ in
            Text("The thread and its messages are deleted for good. This can't be undone.")
        }
    }
}

/// The connected server's own settings, grouped as the server describes them.
struct ServerSettingsScreen: View {
    @ObservedObject var core: Core

    private var groups: [SettingGroupView] { core.view.settings.groups }

    private var connected: Bool { ["connected", "reconnecting"].contains(core.view.connection.state) }

    var body: some View {
        Form {
            ServerContextSection(core: core)
            ForEach(groups, id: \.title) { group in
                Section(group.title) {
                    ForEach(group.fields, id: \.key) { field in
                        SettingFieldRow(core: core, field: field)
                    }
                }
            }
        }
        .overlay {
            // Not a spinner forever: the settings come from a server.
            if groups.isEmpty, connected {
                ProgressView()
            } else if groups.isEmpty {
                ContentUnavailableView {
                    Label("Not Connected", systemImage: "network.slash")
                } description: {
                    Text("Server settings load once Domovoi is connected to the server.")
                } actions: {
                    Button("Try Again") { core.update(.loadSettings(section: "general")) }
                }
            }
        }
        .navigationTitle("General Settings")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadSettings(section: "general")) }
        .onChange(of: core.view.connection.state) { _, state in
            if state == "connected" { core.update(.loadSettings(section: "general")) }
        }
    }
}

/// The agents on the server: installed, signed in, how many models.
struct ProvidersScreen: View {
    @ObservedObject var core: Core
    @State private var editing: ProviderEditing?

    var body: some View {
        Form {
            ServerContextSection(core: core)
            ForEach(core.view.settings.providers, id: \.instanceId) { p in
                Section {
                    LabeledContent("Status") {
                        HStack(spacing: Space.xs) {
                            Image(systemName: p.status == "ready" ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                            Text(p.status.capitalized)
                        }
                        .foregroundStyle(p.status == "ready" ? Tint.success : Tint.caution)
                    }
                    if !p.version.isEmpty { LabeledContent("Version", value: p.version) }
                    if !p.auth.isEmpty { LabeledContent("Account", value: p.auth) }
                    LabeledContent("Models", value: "\(p.modelCount)")
                    if !p.limits.isEmpty { LabeledContent("Limits", value: p.limits) }
                    if !p.message.isEmpty { Text(p.message).font(TypeRole.meta).foregroundStyle(.secondary) }
                    ProviderSetupRows(provider: p, update: { core.update($0) }, edit: { editing = $0 })
                } header: {
                    Text(p.name.isEmpty ? p.instanceId : p.name)
                }
            }
            Section {
                Button("Check Again") { core.update(.refreshProviders) }
                if core.view.settings.canAddAntigravity {
                    Button("Add Antigravity Account") { editing = .newAntigravity() }
                }
            } footer: {
                Text("Other agents sign in and install on the server itself, in their command-line tools. Antigravity signs in here: Google returns to the server's machine, so from this device paste the address the browser ended on.")
            }
            Section("Failover Pools & Availability") {
                FailoverPoolsBody(core: core)
            }
        }
        .navigationTitle("Providers")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $editing) { draft in
            NavigationStack {
                ProviderInstanceEditor(editing: draft, update: { core.update($0) }) { editing = nil }
                    .navigationTitle(draft.edit.instanceId.isEmpty ? "New Account" : "Edit Account")
                    .navigationBarTitleDisplayMode(.inline)
            }
        }
        .onAppear { core.update(.loadSettings(section: "providers")) }
        .onChange(of: core.view.connection.state) { _, state in
            if state == "connected" { core.update(.loadSettings(section: "providers")) }
        }
        .refreshable { await core.refresh(.refreshProviders) }
    }
}

/// One scheduled task: what it does, its last runs (each opens its thread),
/// and Run Now, Open Thread and Edit.
struct ScheduledTaskDetail: View {
    @ObservedObject var core: Core
    let taskId: String
    let open: (Screen) -> Void
    @State private var editing: ScheduledTaskEdit?

    private var task: ScheduledTaskView? { core.view.settings.scheduledTasks.first { $0.id == taskId } }
    private var runs: [ScheduledRunView] { core.view.settings.scheduledRunsTask == taskId ? core.view.settings.scheduledRuns : [] }

    var body: some View {
        List {
            if let task {
                Section {
                    Toggle("Enabled", isOn: Binding(get: { task.enabled }, set: { core.update(.setScheduledEnabled(id: task.id, enabled: $0)) }))
                    LabeledContent("Runs", value: task.schedule)
                    LabeledContent("Project", value: task.projectTitle)
                    LabeledContent("Where", value: task.destination)
                    if task.enabled && task.scheduleKind != "webhook" { LabeledContent("Next", value: task.nextRun) }
                    if !task.runState.isEmpty { LabeledContent("Now", value: task.runState) }
                } footer: {
                    Text(task.prompt).lineLimit(6)
                }
                Section {
                    if task.scheduleKind != "webhook" {
                        Button("Run Now", systemImage: "play") { core.update(.runScheduledNow(id: task.id)) }
                    }
                    if let thread = task.openableThread {
                        Button(task.openThreadLabel, systemImage: "bubble.left.and.text.bubble.right") { open(.thread(thread)) }
                    }
                    Button("Edit", systemImage: "pencil") { editing = ScheduledTaskEditor.from(task) }
                }
                Section("Last runs") {
                    if runs.isEmpty {
                        Text(task.lastRun).foregroundStyle(.secondary)
                    }
                    ForEach(Array(runs.enumerated()), id: \.offset) { _, run in
                        Button {
                            if !run.threadId.isEmpty { open(.thread(run.threadId)) }
                        } label: {
                            VStack(alignment: .leading, spacing: Space.xxs) {
                                HStack {
                                    Text(run.status).foregroundStyle(run.failed ? Tint.danger : .primary)
                                    Spacer()
                                    Text(run.trigger).font(TypeRole.meta).foregroundStyle(.secondary)
                                }
                                Text([run.started, run.threadTitle].filter { !$0.isEmpty }.joined(separator: " · "))
                                    .font(TypeRole.meta).foregroundStyle(.secondary)
                                if !run.error.isEmpty { Text(run.error).font(TypeRole.meta).foregroundStyle(Tint.danger).lineLimit(3) }
                            }
                        }
                        .buttonStyle(.plain)
                        .disabled(run.threadId.isEmpty)
                    }
                }
            }
        }
        .overlay {
            if task == nil {
                ContentUnavailableView("Task Deleted", systemImage: "calendar.badge.exclamationmark", description: Text("This task is no longer on the server."))
            }
        }
        .navigationTitle(task?.name ?? "Task")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { core.update(.loadScheduledRuns(id: taskId)) }
        .refreshable { core.update(.loadScheduledRuns(id: taskId)) }
        .sheet(item: $editing) { draft in
            NavigationStack {
                ScheduledTaskEditor(core: core, draft: draft) { editing = nil }
                    .navigationTitle("Edit Task")
                    .navigationBarTitleDisplayMode(.inline)
            }
        }
    }
}

/// Prompts the server runs on a schedule (or on webhook requests).
struct ScheduledTasksScreen: View {
    @ObservedObject var core: Core
    let open: (Screen) -> Void
    @State private var editing: ScheduledTaskEdit?
    @State private var deleting: ScheduledTaskView?
    /// The task whose runs and details are pushed.
    @State private var detail: String?

    private var settings: SettingsView { core.view.settings }

    var body: some View {
        List {
            ServerContextSection(core: core)
            Section {
                ForEach(settings.scheduledTasks, id: \.id) { task in
                    VStack(alignment: .leading, spacing: Space.xs) {
                        Toggle(isOn: Binding(get: { task.enabled }, set: { core.update(.setScheduledEnabled(id: task.id, enabled: $0)) })) {
                            Text(task.name)
                        }
                        Text(task.prompt).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(2)
                        Text([task.schedule, task.projectTitle, task.destination, task.enabled && task.scheduleKind != "webhook" ? "next \(task.nextRun)" : "", task.lastRun].filter { !$0.isEmpty }.joined(separator: " · "))
                            .font(TypeRole.metaSmall).foregroundStyle(.tertiary)
                        if !task.runState.isEmpty {
                            Label(task.runState, systemImage: task.runState == "Needs you" ? "hand.raised.fill" : task.runState == "Running" ? "play.circle" : "hourglass")
                                .font(TypeRole.metaSmall).foregroundStyle(task.runState == "Needs you" ? AnyShapeStyle(Tint.caution) : AnyShapeStyle(.tint))
                        } else if !task.lastError.isEmpty {
                            Text(task.lastError).font(TypeRole.metaSmall).foregroundStyle(Tint.danger).lineLimit(2)
                        }
                        if task.scheduleKind == "webhook" {
                            WebhookTaskDetails(core: core, task: task)
                        }
                        // A button, not a link nested in the row: a link took
                        // the row's tap, so a tap meant to edit opened details.
                        Button { detail = task.id } label: {
                            HStack(spacing: Space.xs) {
                                Text("Runs and details")
                                Image(systemName: "chevron.right").imageScale(.small)
                            }
                            .font(TypeRole.meta)
                            .tapTarget()
                        }
                        .buttonStyle(.borderless)
                    }
                    .contentShape(Rectangle())
                    // The rest of the row edits.
                    .onTapGesture { editing = ScheduledTaskEditor.from(task) }
                    .swipeActions {
                        Button("Delete", role: .destructive) { deleting = task }
                        if task.scheduleKind != "webhook" {
                            Button("Run Now") { core.update(.runScheduledNow(id: task.id)) }.tint(.blue)
                        }
                    }
                    .swipeActions(edge: .leading) {
                        if let thread = task.openableThread {
                            Button(task.openThreadLabel) { open(.thread(thread)) }.tint(.indigo)
                        }
                    }
                    .contextMenu {
                        Button("Edit", systemImage: "pencil") { editing = ScheduledTaskEditor.from(task) }
                        if let thread = task.openableThread {
                            Button(task.openThreadLabel, systemImage: "bubble.left.and.text.bubble.right") { open(.thread(thread)) }
                        }
                        if task.scheduleKind != "webhook" {
                            Button("Run Now", systemImage: "play") { core.update(.runScheduledNow(id: task.id)) }
                        }
                        Button("Delete…", systemImage: "trash", role: .destructive) { deleting = task }
                    }
                }
            } footer: {
                if !settings.scheduledTimeZone.isEmpty { Text("Times are in \(settings.scheduledTimeZone), the server's time zone.") }
            }
        }
        .overlay {
            if settings.scheduledTasks.isEmpty {
                ContentUnavailableView("No Scheduled Tasks", systemImage: "calendar.badge.clock", description: Text("A scheduled task sends its prompt on its own: at a time, every so often, or when its URL gets a request."))
            }
        }
        .navigationTitle("Scheduled Tasks")
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $detail) { id in
            ScheduledTaskDetail(core: core, taskId: id, open: open)
        }
        .toolbar {
            Button { editing = ScheduledTaskEditor.new(projectId: settings.projects.first?.id ?? "") } label: { Image(systemName: "plus") }
                .accessibilityLabel("New Scheduled Task")
                .disabled(settings.projects.isEmpty)
        }
        .confirmationDialog(
            "Delete \(deleting?.name ?? "this task")?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { task in
            Button("Delete", role: .destructive) { core.update(.deleteScheduled(id: task.id)) }
        } message: { _ in
            Text("It stops running. Threads it already started stay.")
        }
        .sheet(item: $editing) { draft in
            NavigationStack {
                ScheduledTaskEditor(core: core, draft: draft) { editing = nil }
                    .navigationTitle(draft.id.isEmpty ? "New Scheduled Task" : "Edit Task")
                    .navigationBarTitleDisplayMode(.inline)
            }
        }
        .onAppear { core.update(.loadSettings(section: "scheduled")) }
        .onChange(of: core.view.connection.state) { _, state in
            if state == "connected" { core.update(.loadSettings(section: "scheduled")) }
        }
        .onReceive(IOSSnapshots.openScheduledEditor) { draft in
            // Closed first: an open sheet keeps its draft for the same (empty) id.
            editing = nil
            Task { @MainActor in
                try? await Task.sleep(for: .milliseconds(600))
                editing = draft
            }
        }
    }
}

/// The server's database and processes.
struct MaintenanceScreen: View {
    @ObservedObject var core: Core
    @State private var reclaiming = false

    private var storage: StorageView { core.view.settings.storage }
    private var diagnostics: DiagnosticsView { core.view.settings.diagnostics }

    var body: some View {
        Form {
            ServerContextSection(core: core)
            Section {
                LabeledContent("Database", value: storage.databaseSize)
                LabeledContent("Reclaimable", value: storage.reclaimable)
                LabeledContent("Last cleanup", value: storage.lastCleanup)
                Button("Clean Up Now") { core.update(.runStorageCleanup) }
                Button("Reclaim Database Space…") { reclaiming = true }
                if storage.busy { LoadingRow("Working…") }
            } header: {
                Text("Storage")
            } footer: {
                Text("Cleanup removes what deleted threads and old turns left behind. Reclaiming compacts the database file.")
            }
            .disabled(storage.busy)
            Section("Server processes") {
                LabeledContent("CPU", value: diagnostics.cpu)
                LabeledContent("Memory", value: diagnostics.memory)
                ForEach(diagnostics.processes, id: \.pid) { p in
                    HStack {
                        Text(p.command).font(TypeRole.monoSmall).lineLimit(1).truncationMode(.middle)
                            .padding(.leading, CGFloat(p.depth) * Space.m)
                        Spacer()
                        Text(p.cpu).font(TypeRole.meta.monospacedDigit()).foregroundStyle(.secondary)
                        Text(p.memory).font(TypeRole.meta.monospacedDigit()).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .navigationTitle("Maintenance")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            core.update(.loadSettings(section: "storage"))
            core.update(.loadSettings(section: "diagnostics"))
        }
        .onChange(of: core.view.connection.state) { _, state in
            if state == "connected" {
                core.update(.loadSettings(section: "storage"))
                core.update(.loadSettings(section: "diagnostics"))
            }
        }
        .refreshable { await core.refresh(.loadSettings(section: "diagnostics")) }
        .confirmationDialog("Reclaim database space?", isPresented: $reclaiming, titleVisibility: .visible) {
            Button("Reclaim \(storage.reclaimable)") { core.update(.reclaimDatabase) }
        } message: {
            Text("The server compacts its database file. It may pause for a moment while it does.")
        }
    }
}

/// Remote access (iroh), server push status, and the devices signed in to the server.
struct AccessScreen: View {
    @ObservedObject var core: Core
    @State private var signingOut: AccessEntryView?
    @State private var turningOff = false

    private var remote: RemoteAccessView { core.view.settings.remote }

    var body: some View {
        Form {
            ServerContextSection(core: core)
            if remote.available {
                Section {
                    Toggle("Reach this server from other devices", isOn: Binding(
                        get: { remote.enabled || remote.running },
                        // Off cuts every remote device at once: ask first.
                        set: { on in if on { core.update(.setRemoteAccess(enabled: true)) } else { turningOff = true } }
                    ))
                    .disabled(remote.busy || core.view.connection.remote)
                    if remote.running {
                        // The server's iroh key: what a pairing link dials.
                        LabeledContent("Endpoint") {
                            Text(remote.endpointId).font(TypeRole.monoSmall).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                        }
                        .contextMenu { Button("Copy", systemImage: "doc.on.doc") { Platform.copy(remote.endpointId) } }
                        LabeledContent("Relay", value: remote.homeRelay.isEmpty ? (remote.relays.first ?? "connecting…") : remote.homeRelay)
                        if remote.link.isEmpty {
                            Button("Pair a Device…") { core.update(.createPairingLink) }
                                .disabled(remote.busy)
                        } else {
                            pairing
                        }
                    }
                    if remote.busy { LoadingRow("Working…") }
                    if let error = remote.error { ErrorRow(error) }
                } header: {
                    Text("Remote access")
                } footer: {
                    Text(core.view.connection.remote
                        ? "This device is connected remotely, so remote access can only be turned off on the server's Mac."
                        : "Devices connect over iroh: end-to-end encrypted, direct when possible, through a relay otherwise. Each paired device signs in with its own key.")
                }
                Section {
                    LabeledContent("Push notifications", value: remote.pushConfigured ? "Configured" : "Off (no APNs key on server)")
                } header: {
                    Text("Notifications from this server")
                } footer: {
                    Text("Allows this server to wake your iPhone or iPad for approvals and finished turns.")
                }
            }
            Section("Signed-in devices") {
                ForEach(core.view.settings.access, id: \.id) { entry in
                    SettingsRow(title: entry.label + (entry.current ? " (this device)" : ""), subtitle: entry.detail + (entry.connected ? " · connected" : "")) {
                        Image(systemName: icon(entry))
                            .foregroundStyle(entry.connected ? Tint.success : .secondary)
                    }
                    .swipeActions {
                        if entry.kind == "session", !entry.current {
                            Button("Sign Out", role: .destructive) { signingOut = entry }
                        }
                    }
                    .contextMenu {
                        if entry.kind == "session", !entry.current {
                            Button("Sign Out…", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) { signingOut = entry }
                        }
                    }
                }
            }
        }
        .navigationTitle("Access & Devices")
        .confirmationDialog("Turn off remote access?", isPresented: $turningOff, titleVisibility: .visible) {
            Button("Turn Off", role: .destructive) { core.update(.setRemoteAccess(enabled: false)) }
        } message: {
            Text("Devices paired for remote access can't reach this server until it's back on.")
        }
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            "Sign out \(signingOut?.label ?? "this device")?",
            isPresented: Binding(get: { signingOut != nil }, set: { if !$0 { signingOut = nil } }),
            titleVisibility: .visible,
            presenting: signingOut
        ) { entry in
            Button("Sign Out", role: .destructive) { core.update(.revokeClient(sessionId: entry.id)) }
        } message: { _ in
            Text("It needs a new pairing code to connect again.")
        }
        .onAppear { core.update(.loadSettings(section: "access")) }
        .onChange(of: core.view.connection.state) { _, state in
            if state == "connected" { core.update(.loadSettings(section: "access")) }
        }
    }

    private var pairing: some View {
        VStack(spacing: Space.m) {
            QRCodeView(text: remote.link)
                .frame(width: 200, height: 200)
                .padding(Space.s)
                .background(.white, in: RoundedRectangle(cornerRadius: Radius.chip))
                .accessibilityLabel("Pairing QR code")
            Text("Scan with Domovoi on another iPhone or iPad, or send the link to a Mac.")
                .font(.callout).multilineTextAlignment(.center)
            Text("Code \(remote.code) · works once, until \(ServerTime.local(remote.linkExpiresAt))")
                .font(TypeRole.meta).foregroundStyle(.secondary).multilineTextAlignment(.center)
            HStack(spacing: Space.l) {
                ShareLink(item: remote.link) { Label("Share", systemImage: "square.and.arrow.up") }
                Button("Copy", systemImage: "doc.on.doc") { Platform.copy(remote.link) }
                Button("New Code", systemImage: "arrow.clockwise") { core.update(.createPairingLink) }
            }
            .buttonStyle(.borderless)
            .font(.callout)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Space.s)
    }

    private func icon(_ entry: AccessEntryView) -> String {
        if entry.kind != "session" { return "qrcode" }
        switch entry.deviceType {
        case "mobile": return "iphone"
        case "tablet": return "ipad"
        default: return "laptopcomputer"
        }
    }
}
