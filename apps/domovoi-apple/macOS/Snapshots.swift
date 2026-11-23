import App
import AppKit
import ScreenCaptureKit
import SwiftUI

/// Debug aid: with `DOMOVOI_SNAPSHOT_DIR` (plus `DOMOVOI_SERVER` and `DOMOVOI_PAIRING`)
/// set, the app connects, walks through its main screens and writes each
/// window as a PNG, then quits. Renders the app's own windows, so it needs no
/// screen-recording permission.
@MainActor
enum Snapshots {
    static var directory: URL? {
        ProcessInfo.processInfo.environment["DOMOVOI_SNAPSHOT_DIR"].map { URL(fileURLWithPath: $0) }
    }

    /// Set while the snapshots run: the Scheduled tab opens its editor.
    static var openScheduledEditorStore = false
    static var openScheduledEditor: Bool {
        get { openScheduledEditorStore }
        set { openScheduledEditorStore = newValue; NotificationCenter.default.post(name: .snapshotOpenScheduledEditor, object: nil) }
    }

    static func run(_ core: Core, openSettings: () -> Void) async {
        guard let dir = directory else { return }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        await settle(0.5)
        if let window = NSApp.windows.first(where: { $0.isVisible }) {
            window.setFrame(NSRect(x: 60, y: 60, width: 1440, height: 900), display: true)
        }
        let env = ProcessInfo.processInfo.environment
        #if DEBUG
        if env["DOMOVOI_COMPANION_SNAPSHOTS"] == "1" {
            do { try CompanionGallery.capture(in: dir) }
            catch { print("companion gallery: \(error)") }
            if let server = env["DOMOVOI_SERVER"], let code = env["DOMOVOI_PAIRING"] {
                core.update(.connect(serverUrl: server, pairingCode: code))
            }
            await wait(20) { core.view.connection.state == "connected" }
            await settle()
            await capture("companion-toolbar", dir)
            SettingsTab.shared.selection = "companion"
            openSettings()
            await settle()
            await capture("companion-editor", dir)
            core.update(.disconnect)
            await wait(5) { core.view.connection.state == "disconnected" }
            await settle()
            await capture("companion-offline", dir)
            NSApp.terminate(nil)
            return
        }
        #endif
        await capture("00-connect", dir)
        core.update(.connect(serverUrl: env["DOMOVOI_SERVER"] ?? "http://127.0.0.1:13774", pairingCode: env["DOMOVOI_PAIRING"] ?? ""))
        await wait(20) { core.view.connection.state == "connected" && !core.view.sidebar.projects.isEmpty }
        await wait(15) { core.view.composer.selectedModel != nil }
        await settle()
        await capture("01-new-thread", dir)

        if env["DOMOVOI_SIDEBAR_SNAPSHOTS"] == "1" {
            for (mode, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
                NSApp.appearance = NSAppearance(named: appearance)
                for grouping in ["status", "project"] {
                    core.update(.setSidebarGrouping(grouping: grouping))
                    if grouping == "status" {
                        for section in core.view.sidebar.sections where !section.expanded { core.update(.toggleSection(section: section.kind)) }
                    }
                    await settle(1)
                    await capture("sidebar-\(grouping)-\(mode)", dir)
                }
            }
            NSApp.terminate(nil)
            return
        }

        let threads = core.view.sidebar.pickableRows
        let pick = env["DOMOVOI_THREAD"].flatMap { id in threads.first { $0.id == id } } ?? threads.first

        #if DEBUG
        // The window narrowed with a thread and the side panel open: three
        // columns must never be wider than the window.
        if env["DOMOVOI_NARROW_SNAPSHOTS"] == "1", let thread = pick {
            core.update(.selectThread(threadId: thread.id))
            await wait(10) { !(core.view.thread?.loading ?? true) }
            if !core.view.panelOpen { core.update(.togglePanel) }
            core.update(.showPanel(tab: "diff"))
            // Narrowed, then widened again: the sidebar hides, then comes back.
            for (index, width) in [1440, 1100, 930, 760, 1100, 1440, 1700].enumerated() {
                guard let window = NSApp.windows.first(where: { $0.isVisible }) else { break }
                window.setFrame(NSRect(x: 60, y: 60, width: CGFloat(width), height: 640), display: true)
                await settle(1.5)
                logColumns(window, label: "\(width)")
                await capture(String(format: "narrow-%d-%d", index, width), dir)
            }
            NSApp.terminate(nil)
            return
        }
        #endif
        if let thread = pick {
            core.update(.selectThread(threadId: thread.id))
            await wait(10) { !(core.view.thread?.loading ?? true) }
            await settle()
            await capture("02-thread", dir)
            // The message field typed into as a person would: one line (it
            // must sit where the placeholder was), several, then cleared.
            type("Ask anything, @ to mention files")
            await settle(0.5)
            await capture("02f-typed", dir)
            type("First line\nSecond line\nThird line")
            await settle(0.5)
            await capture("02g-multiline", dir)
            type("")
            await settle(0.5)
            await capture("02h-cleared", dir)
            // The composer's mode switcher: plan mode, then each popover.
            core.update(.setInteractionMode(mode: "plan"))
            await settle()
            await capture("02a-plan-mode", dir)
            core.update(.setInteractionMode(mode: "default"))
            for (name, option) in [("02b-effort", ComposerOption.effort), ("02c-permissions", .permissions), ("02d-model", .model)] {
                ComposerCommands.shared.open = option
                await settle(1)
                await capture(name, dir)
            }
            ComposerCommands.shared.open = nil
            // A toast with Undo (archiving, undone at once).
            core.update(.archiveThread(threadId: thread.id, archived: true))
            await wait(5) { core.view.toast?.actionLabel.isEmpty == false }
            await settle(0.5)
            await capture("02e-toast-undo", dir)
            if core.view.toast?.actionLabel.isEmpty == false { core.update(.toastAction) }
            await settle(1)
            core.update(.selectThread(threadId: thread.id))
            await settle()
            for (index, tab) in ["diff", "terminal", "files", "git"].enumerated() {
                core.update(.showPanel(tab: tab))
                await settle(2.5)
                if tab == "terminal" {
                    core.update(.terminalPaste(text: "ls -la"))
                    core.update(.terminalKey(key: "enter", text: "", ctrl: false, alt: false))
                    await settle(1.5)
                }
                if tab == "files", let file = core.view.panel.files.entries.first(where: { !$0.directory }) {
                    core.update(.openFile(path: file.path))
                    await settle(1)
                }
                await capture(String(format: "%02d-panel-%@", 3 + index, tab), dir)
            }
            if let id = core.view.panel.terminal.active?.id { core.update(.closeTerminal(terminalId: id)) }
            core.update(.togglePanel)
            core.update(.findInThread(query: "secret"))
            await settle(1)
            await capture("07-find", dir)
            core.update(.closeFind)
        }
        core.update(.openPalette)
        await settle(1)
        await capture("08-palette", dir)
        core.update(.closePalette)
        core.update(.showRoute(route: "pull-requests"))
        await settle(4)
        await capture("09-pull-requests", dir)
        openSettings()
        await settle(2)
        await capture("10-settings", dir)
        SettingsTab.shared.selection = "access"
        core.update(.createPairingLink)
        await wait(10) { !core.view.settings.remote.link.isEmpty || core.view.settings.remote.error != nil }
        await settle(1.5)
        await capture("11-remote-access", dir)
        SettingsTab.shared.selection = "scheduled"
        await settle(1.5)
        await capture("12-scheduled", dir)
        openScheduledEditor = true
        await settle(1.5)
        await capture("13-scheduled-editor", dir)
        NSApp.terminate(nil)
    }

    /// Replaces the message field's text through its text view, so the
    /// edit takes the same path as typing.
    private static func type(_ text: String) {
        func field(in view: NSView) -> FieldTextView? {
            if let field = view as? FieldTextView { return field }
            return view.subviews.lazy.compactMap(field(in:)).first
        }
        guard let window = NSApp.windows.first(where: { $0.isVisible }), let content = window.contentView, let field = field(in: content) else { return }
        window.makeFirstResponder(field)
        field.selectAll(nil)
        if text.isEmpty { field.deleteBackward(nil) } else { field.insertText(text, replacementRange: field.selectedRange()) }
    }

    #if DEBUG
    /// Prints the window's width and each split view's columns, outermost first.
    private static func logColumns(_ window: NSWindow, label: String) {
        func splits(in view: NSView) -> [NSSplitView] {
            ((view as? NSSplitView).map { [$0] } ?? []) + view.subviews.flatMap(splits(in:))
        }
        let root = window.contentView.map { [$0] } ?? []
        var line = "columns \(label): window \(Int(window.frame.width)) content \(Int(window.contentView?.frame.width ?? 0)) minSize \(Int(window.minSize.width))/\(Int(window.contentMinSize.width))"
        for split in root.flatMap(splits(in:)) {
            line += " | split \(Int(split.frame.minX))+\(Int(split.frame.width)): " + split.arrangedSubviews.map { "\(Int($0.frame.width))\($0.isHidden ? "h" : "")" }.joined(separator: ",")
        }
        print(line)
    }
    #endif

    private static func wait(_ seconds: Double, _ condition: () -> Bool) async {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline, !condition() {
            try? await Task.sleep(for: .milliseconds(100))
        }
    }

    private static func settle(_ seconds: Double = 1) async {
        try? await Task.sleep(for: .seconds(seconds))
    }

    /// Writes every visible window (sheets included) as `<name>[-n].png`.
    /// `SCShareableContent.currentProcess` lists this process's own windows,
    /// which it may capture without screen-recording permission.
    private static func capture(_ name: String, _ dir: URL) async {
        guard let content = try? await SCShareableContent.currentProcess else { return }
        let visible = NSApp.windows.filter { $0.isVisible && $0.frame.width > 200 }
        for (i, window) in visible.enumerated() {
            guard let sc = content.windows.first(where: { $0.windowID == CGWindowID(window.windowNumber) }) else { continue }
            let config = SCStreamConfiguration()
            let scale = window.backingScaleFactor
            config.width = Int(sc.frame.width * scale)
            config.height = Int(sc.frame.height * scale)
            config.showsCursor = false
            guard let image = try? await SCScreenshotManager.captureImage(contentFilter: SCContentFilter(desktopIndependentWindow: sc), configuration: config) else { continue }
            let file = dir.appendingPathComponent(i == 0 ? "\(name).png" : "\(name)-\(i).png")
            try? NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])?.write(to: file)
        }
    }
}

extension Notification.Name {
    static let snapshotOpenScheduledEditor = Notification.Name("snapshotOpenScheduledEditor")
}
