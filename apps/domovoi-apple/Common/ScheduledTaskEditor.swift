import App
import SwiftUI

/// Creates or changes a scheduled task: when it runs (at a time on chosen
/// days, once, every so often, or on webhook requests), where (a new thread
/// each time, or one thread continued or started fresh), in which folder,
/// and with which agent settings.
struct ScheduledTaskEditor: View {
    @ObservedObject var core: Core
    @State var draft: ScheduledTaskEdit
    var onDone: () -> Void
    @State private var time: Date
    @State private var runAt: Date

    init(core: Core, draft: ScheduledTaskEdit, onDone: @escaping () -> Void) {
        self.core = core
        _draft = State(initialValue: draft)
        self.onDone = onDone
        _time = State(initialValue: ScheduledTaskEditor.time(of: draft.timeOfDay))
        _runAt = State(initialValue: ServerTime.date(draft.runAt) ?? Date().addingTimeInterval(3600))
    }

    /// A new task in `projectId`: daily at 9:00 in a new thread, with the composer's model.
    static func new(projectId: String) -> ScheduledTaskEdit {
        ScheduledTaskEdit(id: "", name: "", prompt: "", projectId: projectId, scheduleKind: "fixed_time", intervalMinutes: 60, timeOfDay: "09:00",
                          weekdays: [], runAt: "", threadMode: "new_thread", threadId: "", workspace: "root", baseBranch: "",
                          modelInstance: "", model: "", runtimeMode: "approval-required", interactionMode: "default", enabled: true)
    }

    /// The editor's fields for a task that exists.
    static func from(_ t: ScheduledTaskView) -> ScheduledTaskEdit {
        ScheduledTaskEdit(id: t.id, name: t.name, prompt: t.prompt, projectId: t.projectId, scheduleKind: t.scheduleKind.isEmpty ? "interval" : t.scheduleKind,
                          intervalMinutes: t.intervalMinutes, timeOfDay: t.timeOfDay.isEmpty ? "09:00" : t.timeOfDay, weekdays: t.weekdays, runAt: t.runAt,
                          threadMode: t.threadMode, threadId: t.threadId, workspace: t.workspace, baseBranch: t.baseBranch, modelInstance: t.modelInstance,
                          model: t.model, runtimeMode: t.runtimeMode, interactionMode: t.interactionMode, enabled: t.enabled)
    }

    private static func time(of text: String) -> Date {
        let parts = text.split(separator: ":").compactMap { Int($0) }
        let calendar = Calendar.current
        return calendar.date(bySettingHour: parts.first ?? 9, minute: parts.dropFirst().first ?? 0, second: 0, of: Date()) ?? Date()
    }

    private var threads: [ThreadRowView] {
        core.view.sidebar.projects.first { $0.id == draft.projectId }.map { $0.threads + $0.earlier } ?? []
    }

    private var modelKey: Binding<String> {
        Binding(get: { draft.model.isEmpty ? "" : "\(draft.modelInstance)|\(draft.model)" },
                set: { key in
                    let parts = key.split(separator: "|", maxSplits: 1).map(String.init)
                    draft.modelInstance = parts.first ?? ""
                    draft.model = parts.count > 1 ? parts[1] : ""
                })
    }

    var body: some View {
        Form {
            Section {
                TextField("Name", text: $draft.name)
                TextField("Prompt", text: $draft.prompt, axis: .vertical).lineLimit(3...8)
                if draft.scheduleKind == "webhook" {
                    Text("The prompt can use {{body.path}}, {{headers.name}} and {{query.name}} from each request.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            Section("When") {
                Picker("Runs", selection: $draft.scheduleKind) {
                    Text("At a time").tag("fixed_time")
                    Text("Once").tag("once")
                    Text("Every").tag("interval")
                    Text("On webhook").tag("webhook")
                }
                .pickerStyle(.segmented)
                switch draft.scheduleKind {
                case "fixed_time":
                    DatePicker("Time", selection: $time, displayedComponents: .hourAndMinute)
                    WeekdayChips(selected: $draft.weekdays)
                case "once":
                    DatePicker("At", selection: $runAt, in: Date()...)
                case "interval":
                    Picker("Every", selection: Binding(get: { Int(draft.intervalMinutes) }, set: { draft.intervalMinutes = UInt32($0) })) {
                        ForEach(ScheduleIntervals.all, id: \.minutes) { Text($0.label).tag($0.minutes) }
                    }
                default:
                    Text("Runs on each request to the task's URL, shown once it is saved.").foregroundStyle(.secondary)
                }
            }
            Section("Where") {
                Picker("Project", selection: $draft.projectId) {
                    ForEach(core.view.settings.projects, id: \.id) { Text($0.title).tag($0.id) }
                }
                Picker("Thread", selection: $draft.threadMode) {
                    Text("A new thread each time").tag("new_thread")
                    Text("Continue one thread").tag("continue")
                    Text("One thread, fresh session each time").tag("fresh_session")
                }
                if draft.threadMode != "new_thread" {
                    Picker("Which", selection: $draft.threadId) {
                        Text("Choose…").tag("")
                        ForEach(threads, id: \.id) { Text($0.title).tag($0.id) }
                    }
                }
                Picker("Folder", selection: $draft.workspace) {
                    Text("Project folder").tag("root")
                    Text("New worktree").tag("worktree")
                }
                if draft.workspace == "worktree" {
                    TextField("Base branch", text: $draft.baseBranch)
                }
            }
            Section("Agent") {
                Picker("Model", selection: modelKey) {
                    Text("The composer's model").tag("")
                    ForEach(core.view.composer.modelGroups, id: \.instanceId) { group in
                        ForEach(group.models, id: \.option.model) { row in
                            Text("\(group.provider) · \(row.name)").tag("\(row.option.instanceId)|\(row.option.model)")
                        }
                    }
                }
                Picker("Permissions", selection: $draft.runtimeMode) {
                    Text("Ask first").tag("approval-required")
                    Text("Accept edits").tag("auto-accept-edits")
                    Text("Full access").tag("full-access")
                }
                Toggle("Plan first", isOn: Binding(get: { draft.interactionMode == "plan" }, set: { draft.interactionMode = $0 ? "plan" : "default" }))
                Toggle("Enabled", isOn: $draft.enabled)
            }
        }
        .formStyle(.grouped)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { onDone() } }
            ToolbarItem(placement: .confirmationAction) {
                Button(draft.id.isEmpty ? "Create" : "Save") { save() }.disabled(!complete)
            }
        }
    }

    private var complete: Bool {
        !draft.name.trimmingCharacters(in: .whitespaces).isEmpty && !draft.prompt.trimmingCharacters(in: .whitespaces).isEmpty
            && !draft.projectId.isEmpty && (draft.threadMode == "new_thread" || !draft.threadId.isEmpty)
    }

    private func save() {
        var task = draft
        let parts = Calendar.current.dateComponents([.hour, .minute], from: time)
        task.timeOfDay = String(format: "%02d:%02d", parts.hour ?? 9, parts.minute ?? 0)
        let iso = ISO8601DateFormatter()
        iso.timeZone = .current
        iso.formatOptions = [.withInternetDateTime]
        task.runAt = task.scheduleKind == "once" ? iso.string(from: runAt) : ""
        core.update(.saveScheduled(task: task))
        onDone()
    }
}

/// The days a task runs on, Sunday (0) to Saturday (6); none is every day.
struct WeekdayChips: View {
    @Binding var selected: [UInt8]
    private let names = Calendar.current.veryShortWeekdaySymbols

    var body: some View {
        HStack(spacing: Space.xs) {
            ForEach(0..<7, id: \.self) { day in
                let on = selected.contains(UInt8(day))
                let toggle = { if on { selected.removeAll { $0 == UInt8(day) } } else { selected.append(UInt8(day)) } }
                Group {
                    if on {
                        Button(names[day], action: toggle).buttonStyle(.borderedProminent)
                    } else {
                        Button(names[day], action: toggle).buttonStyle(.bordered)
                    }
                }
                .fontWeight(on ? .bold : .regular)
                .accessibilityLabel(Calendar.current.weekdaySymbols[day])
                .accessibilityAddTraits(on ? .isSelected : [])
            }
            Spacer()
            Text(selected.isEmpty ? "Every day" : "").font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// A webhook task's URL with copy and a new token, and its last deliveries.
struct WebhookTaskDetails: View {
    @ObservedObject var core: Core
    let task: ScheduledTaskView
    @State private var confirmRotate = false

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            HStack {
                Text(task.webhookUrl).font(.caption.monospaced()).textSelection(.enabled).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button { Platform.copy(task.webhookUrl) } label: { Image(systemName: "doc.on.doc") }
                    .buttonStyle(.borderless).help("Copy the URL").accessibilityLabel("Copy webhook URL")
                Button("New URL") { confirmRotate = true }.controlSize(.small)
                Button("Deliveries") { core.update(.loadWebhookDeliveries(id: task.id)) }.controlSize(.small)
            }
            if core.view.settings.webhookDeliveriesTask == task.id {
                if core.view.settings.webhookDeliveries.isEmpty {
                    Text("No requests yet.").font(.caption).foregroundStyle(.secondary)
                }
                ForEach(core.view.settings.webhookDeliveries, id: \.id) { d in
                    HStack {
                        Text(d.outcome).font(.caption.weight(.medium))
                        Text(d.method).font(.caption).foregroundStyle(.secondary)
                        Text(d.detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        Spacer()
                        Text(d.received).font(.caption2).foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .confirmationDialog("Give this task a new URL?", isPresented: $confirmRotate, titleVisibility: .visible) {
            Button("New URL", role: .destructive) { core.update(.rotateWebhookToken(id: task.id)) }
        } message: {
            Text("The current URL stops working.")
        }
    }
}

extension ScheduledTaskEdit: @retroactive Identifiable {}
