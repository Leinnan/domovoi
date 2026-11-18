// Headless check of the shell bridge: drives the same `Core` the app uses
// (Rust core via FFI, URLSession HTTP + WebSocket) against a live server,
// through the features that need no provider turn.
// Usage: DomovoiSmoke <server-url> <pairing-code>
//        DomovoiSmoke <domovoi://connect?… link>   (remote, over iroh)
// DOMOVOI_SMOKE_THREAD=<thread id> opens that thread instead of the newest finished one.
// DOMOVOI_SMOKE_MAX_STALL_MS=<ms> fails the run if the main thread stalls longer.
import App
import Foundation

/// How long the main thread goes without running: a timer on another queue
/// posts to the main queue every 2 ms and records how late each post runs.
/// A frame is 8 to 16 ms, so a stall near that is a dropped frame in the app.
final class MainStallProbe: @unchecked Sendable {
    private let timer: DispatchSourceTimer
    private let lock = NSLock()
    private var pending = false
    private var longest = 0.0

    init() {
        timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "domovoi.smoke.stall", qos: .userInteractive))
        timer.schedule(deadline: .now(), repeating: .milliseconds(2), leeway: .microseconds(200))
        timer.setEventHandler { [self] in
            lock.lock()
            let waiting = pending
            pending = true
            lock.unlock()
            if waiting { return }
            let sent = DispatchTime.now().uptimeNanoseconds
            DispatchQueue.main.async { [self] in
                let late = Double(DispatchTime.now().uptimeNanoseconds - sent) / 1e6
                lock.lock()
                pending = false
                longest = max(longest, late)
                lock.unlock()
            }
        }
        timer.resume()
    }

    /// The longest stall since the last call, in milliseconds.
    func take() -> Double {
        lock.lock()
        defer { lock.unlock() }
        let value = longest
        longest = 0
        return value
    }
}

@MainActor
func waitFor(_ seconds: Double, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 100_000_000)
    }
    return condition()
}

@MainActor
func check(_ name: String, _ seconds: Double = 10, _ condition: () -> Bool) async -> Bool {
    let ok = await waitFor(seconds, condition)
    print(ok ? "ok   \(name)" : "FAIL \(name)")
    return ok
}

@MainActor
func run() async -> Int32 {
    let args = CommandLine.arguments
    let remote = args.count >= 2 && args[1].hasPrefix("domovoi://")
    guard args.count >= 3 || remote else {
        print("usage: DomovoiSmoke <server-url> <pairing-code> | DomovoiSmoke <domovoi://connect?… link>")
        return 2
    }
    let core = Core()
    var failures = 0
    func expect(_ ok: Bool) { if !ok { failures += 1 } }
    let stalls = MainStallProbe()
    var worstStall = 0.0
    func stalled(_ during: String) {
        let ms = stalls.take()
        worstStall = max(worstStall, ms)
        print(String(format: "  main thread: longest stall %.1f ms while %@", ms, during))
    }

    if remote {
        core.update(.connectRemote(link: args[1]))
    } else {
        core.update(.connect(serverUrl: args[1], pairingCode: args[2]))
    }
    guard await check("connect and receive the shell", remote ? 45 : 15, { core.view.connection.state == "connected" && !core.view.sidebar.projects.isEmpty }) else {
        print("  state=\(core.view.connection.state) error=\(core.view.connection.error ?? "-")")
        return 1
    }
    let v = core.view
    print("  \(v.sidebar.projects.count) project(s); server \(v.connection.serverVersion) \(v.connection.platform)")
    stalled("connecting and receiving the shell")
    if remote {
        _ = await waitFor(8) { !core.view.connection.path.isEmpty }
        print("  remote via \(core.view.connection.serverUrl): path \(core.view.connection.path.isEmpty ? "?" : core.view.connection.path), rtt \(core.view.connection.rttMs) ms")
    }
    expect(await check("providers offer models", 15, { !core.view.composer.modelGroups.isEmpty && core.view.composer.selectedModel != nil }))
    print("  default model: \(core.view.composer.selectedModel?.label ?? "-"), effort: \(core.view.composer.selectedEffort ?? "-")")
    print("  \(core.view.composer.modelCount) model(s), \(core.view.composer.favoriteModels.count) favorite(s)")

    // A favorite the server keeps (then taken back off, as it was).
    if let row = core.view.composer.modelGroups.flatMap(\.models).first(where: { !$0.favorite && !$0.hidden }) {
        let key = row.option
        let isFavorite = { core.view.composer.favoriteModels.contains { $0.option.instanceId == key.instanceId && $0.option.model == key.model } }
        core.update(.setModelFavorite(instanceId: key.instanceId, model: key.model, favorite: true))
        expect(await check("a model marked favorite is listed first", 5, { isFavorite() }))
        try? await Task.sleep(for: .seconds(1))
        expect(await check("the server keeps the favorite", 1, { isFavorite() }))
        core.update(.setModelFavorite(instanceId: key.instanceId, model: key.model, favorite: false))
        expect(await check("and unmarked", 5, { !isFavorite() }))
    }

    // The thread asked for, or else a finished one; and its project.
    let wanted = ProcessInfo.processInfo.environment["DOMOVOI_SMOKE_THREAD"]
    let rows = core.view.sidebar.pickableRows
    let picked = rows.first { $0.id == wanted } ?? rows.first { $0.status == "completed" } ?? rows.first
    guard let thread = picked, let project = core.view.sidebar.projects.first(where: { $0.id == thread.projectId }) else {
        print("FAIL no threads listed")
        return 1
    }
    _ = stalls.take()
    core.update(.selectThread(threadId: thread.id))
    expect(await check("thread transcript loads with message text", 10, {
        guard let t = core.view.thread, !t.loading, let first = t.items.first else { return false }
        return first.kind == "user" && !first.text.isEmpty
    }))
    stalled("loading a thread of \(core.view.thread?.items.count ?? 0) item(s)")
    if let t = core.view.thread {
        print("  \"\(t.title)\": \(t.items.count) item(s), \(t.checkpoints.count) checkpoint(s)")
        for item in t.items.prefix(12) {
            print("    [\(item.kind)/\(item.status)] \((item.text.isEmpty ? item.title : item.text).prefix(70).replacingOccurrences(of: "\n", with: " "))")
        }
    }

    core.update(.showPanel(tab: "diff"))
    expect(await check("diff panel loads", 10, { !core.view.panel.diff.loading && !core.view.panel.diff.sources.isEmpty }))
    print("  diff: \(core.view.panel.diff.files.count) file(s) for \(core.view.panel.diff.selectedSource) \(core.view.panel.diff.error ?? "")")

    core.update(.showPanel(tab: "files"))
    expect(await check("file tree lists the workspace", 10, { !core.view.panel.files.entries.isEmpty }))
    if let file = core.view.panel.files.entries.first(where: { !$0.directory }) {
        core.update(.openFile(path: file.path))
        expect(await check("file preview opens \(file.path)", 10, { core.view.panel.files.preview.map { !$0.loading } ?? false }))
    }

    core.update(.showPanel(tab: "git"))
    expect(await check("git status arrives", 10, { core.view.panel.git.isRepo || core.view.panel.git.branch != nil }))
    print("  branch: \(core.view.panel.git.branch ?? "-"), \(core.view.panel.git.changedFiles.count) changed file(s)")

    core.update(.showPanel(tab: "terminal"))
    expect(await check("terminal opens", 10, { core.view.panel.terminal.active != nil }))
    // The app resizes the terminal to its view before typing.
    core.update(.terminalResize(rows: 30, cols: 90))
    try? await Task.sleep(for: .milliseconds(300))
    // A paste is bracketed (zsh enables it), so Enter is its own keystroke.
    core.update(.terminalPaste(text: "ls -la; echo domovoi-smoke-$((40+2))"))
    core.update(.terminalKey(key: "enter", text: "", ctrl: false, alt: false))
    expect(await check("terminal shows command output", 10, {
        core.view.panel.terminal.active?.lines.contains { $0.spans.map(\.text).joined().contains("domovoi-smoke-42") } ?? false
    }))
    for line in core.view.panel.terminal.active?.lines.prefix(12) ?? [] {
        print("    |" + line.spans.map(\.text).joined())
    }
    if let id = core.view.panel.terminal.active?.id { core.update(.closeTerminal(terminalId: id)) }

    core.update(.setDraft(text: "look at @"))
    expect(await check("@ mention suggests files", 10, { !(core.view.composer.mention?.suggestions.isEmpty ?? true) }))
    core.update(.setDraft(text: ""))

    core.update(.findInThread(query: "the"))
    expect(await check("find in thread answers", 10, { core.view.thread?.find != nil }))
    print("  find: \(core.view.thread?.find?.total ?? 0) match(es)")
    core.update(.closeFind)

    core.update(.setSearch(query: thread.title))
    expect(await check("sidebar search filters", 5, { core.view.sidebar.listedRows.contains { $0.id == thread.id } }))
    core.update(.setSearch(query: ""))

    core.update(.openPalette)
    expect(await check("palette lists actions and threads", 5, { (core.view.palette?.items.count ?? 0) > 3 }))
    core.update(.closePalette)

    core.update(.loadSettings(section: "storage"))
    expect(await check("storage settings load", 10, { !core.view.settings.storage.databaseSize.isEmpty }))
    core.update(.loadSettings(section: "diagnostics"))
    expect(await check("diagnostics load", 10, { !core.view.settings.diagnostics.processes.isEmpty }))
    expect(await check("server settings and keybindings arrive", 5, { !core.view.settings.groups.isEmpty && !core.view.settings.keybindings.isEmpty }))

    core.update(.showRoute(route: "usage"))
    expect(await check("usage loads", 15, { !core.view.usage.loading && !core.view.usage.rangeLabel.isEmpty }))

    // Optional: one real (cheap) provider turn launched from the composer.
    if ProcessInfo.processInfo.environment["DOMOVOI_SMOKE_TURN"] == "1" {
        core.update(.newThread(projectId: project.id))
        let rows = core.view.composer.favoriteModels + core.view.composer.modelGroups.flatMap(\.models)
        if let haiku = rows.map(\.option).first(where: { $0.model.contains("haiku") }) {
            core.update(.selectModel(instanceId: haiku.instanceId, model: haiku.model))
            core.update(.selectEffort(value: "low"))
        }
        core.update(.setDraft(text: ProcessInfo.processInfo.environment["DOMOVOI_SMOKE_PROMPT"] ?? "Reply with just the word: pong"))
        _ = stalls.take()
        core.update(.send)
        expect(await check("the launched thread opens", 15, { core.view.route == "thread" && core.view.thread != nil }))
        expect(await check("the turn completes with one assistant reply", 120, {
            guard let t = core.view.thread, !t.working, !t.loading else { return false }
            let replies = t.items.filter { $0.kind == "assistant" }
            return replies.count == 1 && !replies[0].streaming && (ProcessInfo.processInfo.environment["DOMOVOI_SMOKE_PROMPT"] != nil || replies[0].text.lowercased().contains("pong"))
        }))
        stalled("a reply streamed")
        if let t = core.view.thread {
            for item in t.items { print("    [\(item.kind)/\(item.status)\(item.streaming ? "/streaming" : "")] \((item.text.isEmpty ? item.title : item.text).prefix(60))") }
            core.update(.deleteThread(threadId: t.id))
        }
    }

    stalled("the remaining checks ran")
    if let limit = ProcessInfo.processInfo.environment["DOMOVOI_SMOKE_MAX_STALL_MS"].flatMap(Double.init) {
        let ok = worstStall <= limit
        print(String(format: "%@ the main thread never stalls longer than %.0f ms (longest %.1f ms)", ok ? "ok  " : "FAIL", limit, worstStall))
        expect(ok)
    }
    print(failures == 0 ? "PASS" : "FAILED: \(failures) check(s)")
    return failures == 0 ? 0 : 1
}

let code = await run()
exit(code)
