import App
import SwiftUI

// The transcript, markdown, code and diff rendering shared by the macOS and
// iOS apps.

struct Transcript: View {
    @ObservedObject var core: Core
    let thread: ThreadPageView
    var artifactJump: String? = nil
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// New output keeps the end in view. Only the reader's own scrolling
    /// turns this off (away from the end) or on (back at it); content
    /// growing doesn't.
    @State private var following = true
    /// The reader is dragging or flinging the transcript.
    @State private var userScrolling = false
    /// Output arrived while the reader was scrolled up.
    @State private var unseen = false
    /// Scrolling to the end by edge measures the real content, where a
    /// scroll to a marker in a lazy stack can land in estimated blank space.
    @State private var position = ScrollPosition(edge: .bottom)
    /// Following and pinning bookkeeping (see `ScrollBook`).
    @State private var book = ScrollBook()
    /// A row's text shown to select from (see `selectText`).
    @State private var selectingText: SelectedText?
    /// Images shown full size (see `showImages`).
    @State private var showingImages: ImageGallery?
    @State private var activityJump: ActivityJump?
    /// How many of the last lines are drawn. A long thread (thousands of
    /// rows) in one lazy stack opened on blank space and pinned its end for
    /// ever: the stack guesses unmeasured rows from the few it measured (6.9
    /// million points for a 50,000-point thread) and each move corrects the
    /// guess by a little. The rest comes back with "Show Earlier".
    @State private var shownLines = Transcript.window
    @State private var earlierAnchor: String?
    static let window = 80

    private struct ActivityJump {
        let lineID: String
        let token = UUID()
    }

    /// What the scroll geometry tells: where the view is, how tall the
    /// content, whether the end is in view.
    private struct Geometry: Equatable {
        var offset: CGFloat
        var height: CGFloat
        /// The visible height: the keyboard, a taller composer or an
        /// approval card take from it.
        var container: CGFloat
        /// The top inset (a banner above the transcript): when it changes the
        /// view moves without the reader scrolling.
        var insetTop: CGFloat
        /// The bottom inset: on iOS the composer, approval card and keyboard
        /// sit over the transcript's end and take this, not the container.
        var insetBottom: CGFloat
        var atBottom: Bool
        /// Scrolled beyond the content's end by more than half a screen:
        /// blank space where rows were estimated taller than they measured.
        /// A stale offset, never the reader (outside a drag's rubber band).
        var pastEnd: Bool
    }

    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        let actions = TranscriptActions(send: { [weak core] in core?.update($0) })
        ScrollViewReader { proxy in
            ScrollView {
                // Not lazy: the lines drawn are bounded (`shownLines`), and a
                // lazy stack guesses the height of rows it hasn't measured,
                // which left the end in blank space it still counted.
                // `Space.m` between rows; a user message adds as much again
                // above it, so each turn reads as a block.
                VStack(alignment: .leading, spacing: Space.m) {
                    if let parent = thread.parent {
                        ParentBanner(parent: parent, canMergeBack: thread.canMergeBack, actions: actions).id("parent")
                    }
                    if thread.loading && thread.items.isEmpty {
                        ProgressView().frame(maxWidth: .infinity).padding(Space.xxl)
                    } else if thread.items.isEmpty && thread.pendingSends.isEmpty && thread.queued.isEmpty && !thread.working && thread.backgroundTasks.isEmpty {
                        EmptyThreadHint(thread: thread)
                    }
                    let retryBlocked = core.view.retryBlocked
                    // Built with the page, off the main thread.
                    let all = core.view.lines
                    let lines = all.suffix(shownLines)
                    let lastId = lines.last?.id
                    if lines.count < all.count {
                        let earlier = min(Transcript.window, all.count - lines.count)
                        Button { showEarlier(proxy, keeping: lines.first?.id) } label: {
                            Text("Show \(earlier) Earlier").frame(maxWidth: .infinity).tapTarget()
                        }
                        .buttonStyle(.borderless)
                        .font(TypeRole.meta)
                        .id("earlier")
                    } else if thread.hasEarlier {
                        Button {
                            earlierAnchor = lines.first.flatMap { line in
                                switch line {
                                case .item(let item): return item.id
                                case .group(_, _, let items): return items.first?.id
                                }
                            }
                            book.pinning = false
                            core.update(.loadEarlier)
                        } label: {
                            Text(thread.loadingEarlier ? "Loading earlier messages…" : "Show Earlier").frame(maxWidth: .infinity).tapTarget()
                        }
                        .buttonStyle(.borderless)
                        .disabled(thread.loadingEarlier)
                        .id("earlier")
                    }
                    ForEach(lines) { line in
                        switch line {
                        case .item(let item):
                            // Equatable: a row whose item didn't change isn't
                            // drawn again while another one streams.
                            ItemRow(item: item, retryBlocked: retryBlocked, actions: actions)
                                .equatable()
                                .id(item.id)
                        case .group(let id, let summary, let items):
                            // The last group of a turn under way stays open: it's
                            // what the agent is doing now.
                            StepGroup(summary: summary, items: items, live: thread.working && line.id == lastId,
                                      revealing: thread.find?.activeItemId ?? artifactJump,
                                      activityJump: activityJump?.lineID == line.id ? activityJump?.token : nil,
                                      actions: actions)
                                .equatable()
                                .id("group-\(id)")
                        }
                    }
                    ForEach(thread.pendingSends, id: \.id) { pending in
                        PendingSendRow(core: core, pending: pending).id("pending-\(pending.id)")
                    }
                    if !thread.queued.isEmpty {
                        QueuedSection(queued: thread.queued, held: thread.queueHeld, actions: actions).id("queued")
                    }
                    if thread.working || !thread.backgroundTasks.isEmpty {
                        VStack(alignment: .leading, spacing: Space.s) {
                            if thread.working {
                                WorkingRow(status: thread.status, since: thread.workingSinceMs, hasBackgroundWork: !thread.backgroundTasks.isEmpty,
                                           activity: thread.workingActivity, openActivity: { jumpToActivity($0, proxy) })
                            }
                            if !thread.backgroundTasks.isEmpty {
                                BackgroundActivity(tasks: thread.backgroundTasks, actions: actions).id(thread.id)
                            }
                        }
                    }
                    if !thread.working, thread.status == "interrupted", thread.pendingSends.isEmpty, !thread.items.isEmpty {
                        StoppedRow(retryBlocked: retryBlocked, actions: actions).id("stopped")
                    }
                    if !thread.limitResets.isEmpty {
                        UsageLimitBanner(resets: thread.limitResets, plan: thread.limitPlan, actions: actions).id("limit")
                    }
                    // The bottom padding is the marker itself: a scroll to the
                    // marker and one to the edge land on the same spot.
                    Color.clear.frame(height: Space.s).id("bottom")
                }
                .scrollTargetLayout()
                .padding(.horizontal, Space.gutter)
                .padding(.top, Space.l)
                .frame(maxWidth: Space.column)
                .frame(maxWidth: .infinity)
            }
            // Scrolled up, the top row on screen is remembered: coming back
            // to the thread (back from a subagent, another thread picked and
            // this one again) shows it there, not at the end.
            .onScrollTargetVisibilityChange(idType: String.self, threshold: 0.1) { ids in
                guard !following, !book.pinning, let top = ids.first else { return }
                ScrollMemory.shared.anchors[thread.id] = top
            }
            // Opens at the end; scrolled up, what's on screen stays put while
            // the reply grows below it.
            .scrollPosition($position)
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(thread.items.isEmpty ? .top : .bottom, for: .alignment)
            .scrollDismissesKeyboard(.interactively)
            .task(id: "\(thread.id)|\(thread.loading)") {
                // Back to a thread read scrolled up: to the row it was at,
                // once the page is here.
                if let anchor = ScrollMemory.shared.anchors[thread.id] {
                    reveal(anchor)
                    following = false
                    book.pinning = false
                    guard !thread.loading else { return }
                    for delay in [0.0, 0.25] {
                        try? await Task.sleep(for: .seconds(delay))
                        book.selfUntil = Date().addingTimeInterval(0.3)
                        proxy.scrollTo(anchor, anchor: .top)
                    }
                    return
                }
                // Opened, or the page finished loading: back to pinning the
                // end, unless the reader has scrolled away from it.
                guard following else { return }
                book.startPinning(loaded: !thread.loading)
                for delay in [0.0, 0.25, 0.8] {
                    try? await Task.sleep(for: .seconds(delay))
                    if following { toEnd(proxy) }
                }
            }
            .onChange(of: thread.loadingEarlier) { _, loading in
                guard !loading, let anchor = earlierAnchor else { return }
                earlierAnchor = nil
                let line = core.view.lines.first { line in
                    switch line {
                    case .item(let item): return item.id == anchor
                    case .group(_, _, let items): return items.contains { $0.id == anchor }
                    }
                }
                showEarlier(proxy, keeping: line?.id)
            }
            .onScrollPhaseChange { _, phase in
                userScrolling = phase == .tracking || phase == .interacting || phase == .decelerating
                // The reader took over: pinning stops.
                if userScrolling { book.pinning = false }
            }
            .onScrollGeometryChange(for: Geometry.self) { g in
                // `containerSize` is already what is visible, inside the insets
                // (the composer, keyboard and approval card on iOS); the offset
                // counts from under the top inset (the navigation bar), so the
                // end is at `height - container - insetTop` (measured on iOS).
                let seenTo = g.contentOffset.y + g.contentInsets.top + g.containerSize.height
                return Geometry(
                    offset: g.contentOffset.y,
                    height: g.contentSize.height,
                    container: g.containerSize.height,
                    insetTop: g.contentInsets.top,
                    insetBottom: g.contentInsets.bottom,
                    // Short content that fits is at its end too.
                    atBottom: g.contentSize.height <= g.containerSize.height || seenTo >= g.contentSize.height - 60,
                    pastEnd: g.contentSize.height > g.containerSize.height
                        && seenTo > g.contentSize.height + g.containerSize.height / 2
                )
            } action: { old, new in
                let sameHeight = abs(new.height - old.height) < 0.5
                #if os(macOS)
                // Moved up with nothing resized, once loaded: the reader with a
                // mouse wheel, which reports no scroll phase. (On iOS lazy rows
                // being measured move the view like this too; a touch always
                // has a phase.)
                if book.pinning, sameHeight, new.offset < old.offset - 1, !thread.loading { book.pinning = false }
                #endif
                // Blank space past the end: back to the real end, whatever
                // else is going on (unless the reader is dragging).
                if new.pastEnd, !userScrolling, Date() > book.holdUntil { pinToEnd(proxy) }
                if book.pinning {
                    // Opening: back to the end whenever the view isn't there;
                    // the heuristics below would read lazy rows resizing as
                    // the reader scrolling. Bounded: in a very long thread each
                    // move realizes rows that move the view again, and pinning
                    // without end kept the main thread busy (a hang on open).
                    if !new.atBottom { pinToEnd(proxy) }
                    book.settled = sameHeight && new.atBottom && !new.pastEnd ? book.settled + 1 : 0
                    book.pins += 1
                    let done = book.settled >= 2 || book.pins > 60 || Date() > book.pinUntil
                    if done, !thread.loading, !core.view.lines.isEmpty { book.pinning = false }
                    return
                }
                // Any growth (a reply, a command's output, a step group, a
                // plan) is new output, not only a new row or reply text; and
                // a smaller view (the keyboard up, an approval card, a taller
                // composer) mustn't hide the end the reader was following.
                // Not while the reader is dragging: rows above, measured as
                // they come into view, would pull the view back down.
                let covered = new.container < old.container - 0.5 || new.insetBottom > old.insetBottom + 0.5
                // Opening the inspected group changes the layout locally.
                let openingActivity = !following && activityJump != nil && Date() < book.selfUntil
                if !userScrolling, !openingActivity, new.height > old.height + 0.5 || covered { grew(proxy) }
                // Rows measured smaller than estimated (or a group folding):
                // the end the reader follows moved, though nothing is new.
                if !userScrolling, following, new.height < old.height - 0.5 { grew(proxy) }
                // The reader's own scroll: during a gesture, or (a mouse
                // wheel reports no scroll phase on the Mac) the view moving
                // up without new content, which following never does, or
                // down while we aren't moving it.
                // The view itself moved (a banner came or grew, the keyboard):
                // not the reader scrolling.
                let sameFrame = abs(new.insetTop - old.insetTop) < 0.5 && abs(new.container - old.container) < 0.5
                    && abs(new.insetBottom - old.insetBottom) < 0.5
                // With the height unchanged: content shrinking (lazy rows
                // measured) also moves the view up, and is not the reader.
                let wentUp = sameFrame && new.offset < old.offset - 1 && sameHeight && Date() > book.selfUntil
                let wentDown = sameFrame && new.offset > old.offset + 1 && sameHeight && Date() > book.selfUntil
                guard userScrolling || wentUp || wentDown else { return }
                following = new.atBottom
                if new.atBottom {
                    unseen = false
                    ScrollMemory.shared.anchors[thread.id] = nil
                }
            }
            .onChange(of: thread.working) { grew(proxy) }
            .onChange(of: thread.pendingSends.count) { old, new in
                // Sending: back to the end, where the message is.
                if new > old { jumpToEnd(proxy) }
            }
            .onChange(of: thread.find?.activeItemId ?? artifactJump) { _, id in
                guard let id else { return }
                activityJump = nil
                book.pinning = false
                reveal(id)
                // A match folded inside a step group: bring the group on
                // screen (it opens for the match), then the row once it's
                // laid out.
                let group = core.view.lines.first { line in
                    if case .group(_, _, let items) = line { return items.contains { $0.id == id } }
                    return false
                }
                if let group {
                    proxy.scrollTo(group.id, anchor: .center)
                    DispatchQueue.main.async { withAnimation { proxy.scrollTo(id, anchor: .center) } }
                } else {
                    withAnimation { proxy.scrollTo(id, anchor: .center) }
                }
            }
            .onChange(of: thread.working) { was, now in
                if was, !now { Platform.turnFinished() }
            }
            .environment(\.selectText) { selectingText = SelectedText(text: $0) }
            .sheet(item: $selectingText) { shown in SelectTextSheet(text: shown.text) }
            .environment(\.showImages) { showingImages = $0 }
            .imageGallery($showingImages)
            // The thread's title sits over the transcript's top edge: a firm
            // edge keeps it from reading over the messages behind it.
            .macHardTopScrollEdge()
            .overlay(alignment: .bottomTrailing) {
                // Away from the end: a way back, marked when output arrived.
                // Animated here, not on the whole transcript.
                Group {
                    if !following {
                        Button { jumpToEnd(proxy) } label: {
                            Image(systemName: "arrow.down")
                                .font(.body.weight(.semibold))
                                .frame(width: 44, height: 44)
                                .contentShape(Circle())
                                .glassChip(in: Circle())
                                .overlay(alignment: .topTrailing) {
                                    if unseen {
                                        Circle().fill(Color.accentColor).frame(width: 10, height: 10)
                                            .overlay(Circle().stroke(.background, lineWidth: 2))
                                    }
                                }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(unseen ? "New output, jump to the end" : "Jump to the end")
                        .padding(.trailing, Space.l)
                        .padding(.bottom, Space.m)
                        .transition(.scale(scale: 0.6).combined(with: .opacity))
                    }
                }
                .animation(.snappy, value: following)
                .animation(.snappy, value: unseen)
            }
        }
    }

    /// What the agent is on now: the summary of the open group at the end.
    /// Draws earlier lines too, keeping the line that was first on screen.
    private func showEarlier(_ proxy: ScrollViewProxy, keeping first: String?) {
        book.pinning = false
        book.hold()
        shownLines += Transcript.window
        guard let first else { return }
        DispatchQueue.main.async { proxy.scrollTo(first, anchor: .top) }
    }

    /// Widens the drawn lines to hold the row or line `id` (a find match, a
    /// remembered place), so a scroll to it has something to land on.
    private func reveal(_ id: String) {
        let all = core.view.lines
        let index = all.lastIndex { line in
            switch line {
            case .item(let item): return item.id == id
            case .group(let group, _, let items): return "group-\(group)" == id || items.contains { $0.id == id }
            }
        }
        guard let index else { return }
        book.hold()
        let needed = all.count - index + 10
        if needed > shownLines { shownLines = needed }
    }

    private func jumpToActivity(_ itemID: String, _ proxy: ScrollViewProxy) {
        guard let line = core.view.lines.last(where: { line in
            switch line {
            case .item(let item): return item.id == itemID
            case .group(_, _, let items): return items.contains { $0.id == itemID }
            }
        }) else { return }
        let jump = ActivityJump(lineID: line.id)
        book.pinning = false
        book.selfUntil = Date().addingTimeInterval(0.8)
        following = false
        unseen = false
        reveal(line.id)
        activityJump = jump
        ScrollMemory.shared.anchors[thread.id] = line.id
        // Resolve the scroll after the target has been revealed in the stack.
        DispatchQueue.main.async {
            guard activityJump?.token == jump.token, !following, core.view.thread?.id == thread.id else { return }
            withAnimation(reduceMotion ? nil : .snappy) {
                proxy.scrollTo(jump.lineID, anchor: .top)
            }
        }
    }

    /// New output: follow it (once this pass), or note it for the jump button.
    private func grew(_ proxy: ScrollViewProxy) {
        if following {
            guard !book.queued else { return }
            book.queued = true
            DispatchQueue.main.async {
                book.queued = false
                if following { toEnd(proxy) }
            }
        } else if thread.working {
            unseen = true
        }
    }

    /// To the end, now: by edge, then to the end marker once laid out. The
    /// edge alone can land in blank space the lazy stack still counts (rows
    /// estimated taller than they are), and asking for the edge it is
    /// already at is no move; the marker is the real end. The scroll
    /// heuristics ignore the moves this makes (`selfUntil`).
    private func toEnd(_ proxy: ScrollViewProxy) {
        book.selfUntil = Date().addingTimeInterval(0.5)
        position.scrollTo(edge: .bottom)
        DispatchQueue.main.async { proxy.scrollTo("bottom", anchor: .bottom) }
    }

    /// While opening, or out of blank space: to the end by edge (the content
    /// as measured, where the end marker of a lazy stack can sit in
    /// estimated space), at most once a pass.
    private func pinToEnd(_ proxy: ScrollViewProxy) {
        guard !book.queued else { return }
        book.queued = true
        DispatchQueue.main.async {
            book.queued = false
            toEnd(proxy)
        }
    }

    private func jumpToEnd(_ proxy: ScrollViewProxy) {
        activityJump = nil
        unseen = false
        following = true
        ScrollMemory.shared.anchors[thread.id] = nil
        book.selfUntil = Date().addingTimeInterval(0.8)
        withAnimation { position.scrollTo(edge: .bottom) }
        DispatchQueue.main.async { withAnimation { proxy.scrollTo("bottom", anchor: .bottom) } }
    }
}

/// The transcript's scroll bookkeeping that nothing on screen shows. A class
/// held in `@State`: changing it doesn't redraw the transcript (several of
/// these change on every scroll report).
@MainActor
final class ScrollBook {
    /// Opening: the end is pinned whenever the view leaves it, until the page
    /// has loaded and its lazy rows have stopped resizing (or the reader
    /// scrolls, or the limits below). A thread opens on a spinner and its
    /// history lands later: a few tries in the first second left it at the
    /// top, or in blank space past the measured rows.
    var pinning = true
    /// Scroll reports in a row at the end with the height unchanged.
    var settled = 0
    /// Scroll reports while pinning.
    var pins = 0
    /// Pinning ends by then, settled or not (set once the page is loaded).
    var pinUntil = Date.distantFuture
    /// A move to the end is already asked for this pass.
    var queued = false
    /// Until when a move of the view is ours (following, jumping to the
    /// end), not the reader's.
    var selfUntil = Date.distantPast
    /// Until when blank space past the end is left alone: rows were just
    /// added above or a scroll to a row is landing, and the content's
    /// measure passes through such states on the way.
    var holdUntil = Date.distantPast

    func hold() { holdUntil = Date().addingTimeInterval(1.5) }

    func startPinning(loaded: Bool) {
        pinning = true
        settled = 0
        pins = 0
        pinUntil = loaded ? Date().addingTimeInterval(2.5) : .distantFuture
    }
}

/// Where each thread was read, scrolled up (its top row on screen), while
/// the app runs. Not observed: views read it only when a thread opens.
@MainActor
final class ScrollMemory {
    static let shared = ScrollMemory()
    var anchors: [String: String] = [:]
}

/// A turn's current activity, elapsed time, and shortcut to its steps.
struct WorkingRow: View {
    let status: String
    let since: Int64
    let hasBackgroundWork: Bool
    let activity: WorkingActivityView?
    let openActivity: (String) -> Void

    private static let markSize: CGFloat = 32

    private var approval: Bool { status == "approval" }

    private var presentation: (title: String, icon: String) {
        if approval { return ("Waiting for you", "hand.raised.fill") }
        switch activity?.kind {
        case "starting": return ("Starting", "sparkle")
        case "assistant": return ("Writing a reply", "text.bubble")
        case "command": return ("Running a command", "terminal")
        case "file_change": return ("Editing files", "doc.badge.gearshape")
        case "tool": return ("Using a tool", "wrench.and.screwdriver")
        case "reasoning": return ("Thinking", "brain")
        default: return ("Working", hasBackgroundWork ? "clock" : "sparkle")
        }
    }

    var body: some View {
        let itemID = approval ? nil : activity?.stepItemId
        ViewThatFits(in: .horizontal) {
            HStack(spacing: Space.s) {
                statusMark
                statusText(inline: true).fixedSize(horizontal: true, vertical: false)
                if let itemID { activityButton(itemID) }
            }
            VStack(alignment: .leading, spacing: Space.xxs) {
                HStack(spacing: Space.s) {
                    statusMark
                    ViewThatFits(in: .horizontal) {
                        statusText(inline: true).fixedSize(horizontal: true, vertical: false)
                        statusText(inline: false)
                    }
                }
                if let itemID {
                    activityButton(itemID)
                        .padding(.leading, Self.markSize + Space.s)
                }
            }
        }
        .padding(.vertical, Space.xs)
    }

    private var statusMark: some View {
        let tint = approval ? Tint.caution : Color.accentColor
        return Image(systemName: presentation.icon)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(tint)
            .frame(width: Self.markSize, height: Self.markSize)
            .background {
                let fill = RoundedRectangle(cornerRadius: Radius.chip).fill(tint.opacity(0.13))
                if approval { fill } else { fill.pulsing() }
            }
            .accessibilityHidden(true)
    }

    private func statusText(inline: Bool) -> some View {
        let layout = inline ? AnyLayout(HStackLayout(spacing: Space.tight)) : AnyLayout(VStackLayout(alignment: .leading, spacing: Space.xxs))
        return layout {
            Text(presentation.title)
                .font(.callout.weight(.semibold))
                .foregroundStyle(.primary)
                .fixedSize(horizontal: false, vertical: true)
            if !approval, since > 0 {
                if inline {
                    Text("·").foregroundStyle(.secondary).accessibilityHidden(true)
                }
                WorkingElapsed(since: since)
            }
        }
    }

    private func activityButton(_ itemID: String) -> some View {
        Button { openActivity(itemID) } label: {
            HStack(spacing: Space.xs) {
                Text("Activity")
                Image(systemName: "arrow.up").accessibilityHidden(true)
            }
            .font(.caption.weight(.semibold))
            .padding(.horizontal, Space.s)
            .padding(.vertical, Space.xs)
            .background(.fill.secondary, in: Capsule())
            .contentShape(Rectangle())
            .tapTarget()
        }
        .buttonStyle(.borderless)
        .accessibilityLabel("Activity")
        .accessibilityHint("Shows the current run’s transcript steps")
    }
}

private struct BackgroundActivity: View {
    let tasks: [BackgroundTaskView]
    let actions: TranscriptActions
    @Environment(\.openThread) private var openThread
    @State private var expanded = false

    private let previewCount = 2

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            Text("Background activity · \(tasks.count)")
                .font(TypeRole.meta.weight(.semibold))
                .foregroundStyle(.secondary)
                .accessibilityAddTraits(.isHeader)
            ForEach(tasks.prefix(expanded ? tasks.count : previewCount), id: \.id) { task in
                if let child = task.childThreadId {
                    Button {
                        if let openThread { openThread(child) }
                        else { actions.send(.selectThread(threadId: child)) }
                    } label: {
                        taskRow(task, linked: true)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityHint("Opens the background session’s thread")
                } else {
                    taskRow(task, linked: false)
                }
            }
            if tasks.count > previewCount {
                Button {
                    expanded.toggle()
                } label: {
                    Label(expanded ? "Show less" : "Show all \(tasks.count)", systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(TypeRole.meta.weight(.semibold))
                        .contentShape(Rectangle())
                        .tapTarget()
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
        .onChange(of: tasks.count) {
            if tasks.count <= previewCount { expanded = false }
        }
    }

    private func taskRow(_ task: BackgroundTaskView, linked: Bool) -> some View {
        let icon = switch task.kind {
        case "subagent": "person.2"
        case "command": "terminal"
        case "monitor": "waveform.path"
        default: "gearshape"
        }
        return HStack(alignment: .firstTextBaseline, spacing: Space.s) {
            Image(systemName: icon)
                .foregroundStyle(.tint)
                .iconColumn()
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Space.xxs) {
                Text(task.label)
                    .font(.callout.weight(.medium))
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                Text(taskDetail(task))
                    .font(TypeRole.meta)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
            if linked {
                Image(systemName: "arrow.up.right")
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
            }
        }
        .font(TypeRole.meta)
        .multilineTextAlignment(.leading)
        #if os(iOS)
        .frame(minHeight: linked ? Size.tap : nil)
        #endif
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private func taskDetail(_ task: BackgroundTaskView) -> String {
        switch (task.status, task.progress) {
        case ("pending", _): "Starting"
        case ("waiting", .some(let progress)): "Waiting · \(progress)"
        case ("waiting", nil): "Waiting"
        case (_, .some(let progress)): progress
        default: "Running in the background"
        }
    }
}

private struct WorkingElapsed: View {
    let since: Int64

    var body: some View {
        TimelineView(.periodic(from: Date(timeIntervalSince1970: Double(since) / 1000), by: 1)) { context in
            let seconds = max(0, Int(context.date.timeIntervalSince1970 - Double(since) / 1000))
            let duration = Duration.seconds(seconds >= 3600 ? seconds / 60 * 60 : seconds)
            let spoken = duration.formatted(.units(allowed: [.hours, .minutes, .seconds], width: .wide))
            Text(TurnTime.duration(Int64(seconds) * 1000))
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
                .fixedSize()
                .accessibilityLabel(Text("Elapsed time: \(spoken)"))
        }
    }
}

/// The turn was stopped (by the user, or by the server going away).
struct StoppedRow: View {
    let retryBlocked: Bool
    let actions: TranscriptActions

    var body: some View {
        HStack(spacing: Space.s) {
            Image(systemName: "stop.circle").foregroundStyle(.secondary).iconColumn()
            Text("Stopped").font(.callout).foregroundStyle(.secondary)
            Spacer(minLength: Space.s)
            // The card's vertical padding is the button's, so all of it taps.
            Button { actions.send(.retry) } label: { Label("Retry", systemImage: "arrow.clockwise").cardRowInset() }
                .font(.footnote.weight(.medium))
                .linkButton()
                .disabled(retryBlocked)
        }
        .padding(.horizontal, Space.m)
        .cardSurface()
        .accessibilityElement(children: .combine)
    }
}

/// A thread with nothing in it yet: where it runs and how.
struct EmptyThreadHint: View {
    let thread: ThreadPageView

    var body: some View {
        VStack(spacing: Space.m) {
            Image(systemName: "bubble.left.and.text.bubble.right")
                .font(.system(size: Size.heroIcon))
                .foregroundStyle(.tertiary)
            Text(thread.projectTitle.isEmpty ? "New thread" : thread.projectTitle).font(.headline)
            HStack(spacing: Space.s) {
                if let branch = thread.branch {
                    Label(branch, systemImage: "arrow.triangle.branch")
                }
                if !thread.modelLabel.isEmpty {
                    Label(thread.modelLabel, systemImage: "cpu")
                }
                if thread.interactionMode == "plan" {
                    Label("Plan first", systemImage: "list.bullet.clipboard").foregroundStyle(Tint.plan)
                }
            }
            .font(TypeRole.meta)
            .foregroundStyle(.secondary)
            .lineLimit(1)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Space.xxl)
        .accessibilityElement(children: .combine)
    }
}

/// Times and durations for turn footers.
enum TurnTime {
    static func duration(_ ms: Int64) -> String {
        let s = Int(ms / 1000)
        if s < 60 { return "\(s) s" }
        if s < 3600 { return "\(s / 60) min \(s % 60) s" }
        return "\(s / 3600) h \(s % 3600 / 60) min"
    }

    static func footer(started: Int64, duration ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(started) / 1000)
        let time = Calendar.current.isDateInToday(date) ? date.formatted(date: .omitted, time: .shortened) : date.formatted(date: .abbreviated, time: .shortened)
        return "\(time) · \(duration(ms))"
    }
}

/// Steps of a turn (commands, edits, tools, thinking) folded under one
/// summary; open while the agent is on them.
struct StepGroup: View, @MainActor Equatable {
    let summary: String
    let items: [ItemView]
    let live: Bool
    /// The find bar's current match: the group opens while it holds it.
    var revealing: String?
    var activityJump: UUID?
    let actions: TranscriptActions
    @State private var expanded: Bool?

    /// What it shows: a group whose steps didn't change isn't drawn again
    /// while another row streams (`actions` only sends events).
    static func == (a: Self, b: Self) -> Bool {
        a.summary == b.summary && a.items == b.items && a.live == b.live && a.revealing == b.revealing
            && a.activityJump == b.activityJump
    }

    var body: some View {
        #if DEBUG
        let _ = DebugViews.printChanges ? Self._printChanges() : ()
        #endif
        let holdsMatch = revealing.map { id in items.contains { $0.id == id } } ?? false
        let open = holdsMatch || (expanded ?? live)
        // The footer sits as close under the steps as under any other row.
        VStack(alignment: .leading, spacing: Space.xs) {
            VStack(alignment: .leading, spacing: Space.s) {
                Button {
                    withAnimation(.snappy) { expanded = !open }
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                        DisclosureChevron(open: open)
                        Image(systemName: icon).foregroundStyle(failed ? Tint.danger : .secondary).iconColumn()
                        Text(summary).font(TypeRole.rowTitle).foregroundStyle(.secondary).lineLimit(2)
                        Spacer(minLength: 0)
                        StatusDot(status: running ? "running" : stopped ? "interrupted" : "")
                    }
                    .contentShape(Rectangle())
                    .tapTarget()
                }
                .buttonStyle(.plain)
                .accessibilityHint(open ? "Hides the steps" : "Shows the steps")
                if !open, let images = items.first?.groupImages, !images.isEmpty {
                    // The images the steps looked at show while they are folded.
                    GroupImageStrip(images: images)
                        .padding(.leading, Space.disclosureIndent)
                }
                if open {
                    // The steps start under the header's icon, not its chevron.
                    VStack(alignment: .leading, spacing: Space.s) {
                        ForEach(items, id: \.id) { item in
                            ItemRow(item: item, retryBlocked: false, actions: actions).equatable()
                                .id(item.id)
                        }
                    }
                    .padding(.leading, Space.disclosureIndent)
                    .overlay(alignment: .leading) {
                        // A thread down the side, under the chevron, ties the
                        // steps to their summary.
                        Rectangle().fill(.quaternary).frame(width: 1).padding(.leading, Size.chevron / 2)
                    }
                    .transition(.opacity.combined(with: .move(edge: .top)))
                }
            }
            if let last = items.last, last.runDurationMs > 0 {
                Text(TurnTime.footer(started: last.runStartedMs, duration: last.runDurationMs)).font(TypeRole.metaSmall).foregroundStyle(.tertiary)
            }
        }
        .onChange(of: activityJump, initial: true) {
            if activityJump != nil { expanded = true }
        }
    }

    private var running: Bool { items.contains { $0.status == "running" || $0.status == "waiting" } }
    private var stopped: Bool { items.contains { $0.status == "interrupted" } }

    private var failed: Bool { items.contains { $0.status == "failed" || $0.badgeTone == "failed" } }

    private var icon: String {
        if items.contains(where: { $0.kind == "file_change" }) { return "pencil" }
        if items.contains(where: { $0.icon == "command" }) { return "terminal" }
        if items.contains(where: { ["read", "search", "list"].contains($0.icon) }) { return "doc.text.magnifyingglass" }
        if items.contains(where: { $0.icon == "image" }) { return "photo" }
        if items.contains(where: { ["web", "fetch"].contains($0.icon) }) { return "globe" }
        if items.allSatisfy({ $0.kind == "reasoning" }) { return "brain" }
        return "wrench.and.screwdriver"
    }
}

/// Small thumbnails of the images a folded group of steps looked at; a tap
/// shows them full size, from that one.
struct GroupImageStrip: View {
    let images: [ImageView]
    @State private var gallery: ImageGallery?
    @Environment(\.showImages) private var showImages

    var body: some View {
        HStack(spacing: Space.xs) {
            ForEach(Array(images.enumerated()), id: \.offset) { i, image in
                ImageTile(image: image, side: 48) {
                    let shown = ImageGallery(images: images, start: i)
                    if let showImages { showImages(shown) } else { gallery = shown }
                }
                .id(image.url)
            }
        }
        .imageGallery($gallery)
    }
}

/// A message's text, selectable a word at a time.
/// Text to select from, as a sheet's item.
struct SelectedText: Identifiable {
    let id = UUID()
    let text: String
}

struct SelectTextSheet: View {
    let text: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                Text(text)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(Space.l)
            }
            .navigationTitle("Select Text")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) { Button("Copy All") { Platform.copy(text) } }
            }
        }
    }
}

/// A message the server hasn't taken yet: sending, or waiting for the
/// connection (then it can be taken back into the composer).
struct PendingSendRow: View {
    @ObservedObject var core: Core
    let pending: PendingSendView

    var body: some View {
        HStack {
            Spacer(minLength: Space.xxl * 1.5)
            VStack(alignment: .trailing, spacing: Space.s) {
                Text(pending.text).selectableText()
                HStack(spacing: Space.xs) {
                    if pending.state == "waiting" {
                        Image(systemName: "clock")
                        Text("Sends when connected")
                        Button { core.update(.cancelSend(id: pending.id)) } label: { Text("Cancel").tapTarget() }
                            .buttonStyle(.borderless)
                    } else {
                        ProgressView().controlSize(.mini)
                        Text("Sending…")
                    }
                }
                .font(TypeRole.meta)
                .foregroundStyle(.secondary)
            }
            .padding(.horizontal, Space.l)
            .padding(.vertical, Space.m)
            .background(Color.accentColor.opacity(0.07), in: RoundedRectangle(cornerRadius: Radius.bubble, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Radius.bubble, style: .continuous).strokeBorder(Color.accentColor.opacity(0.35), style: StrokeStyle(lineWidth: 1, dash: [4, 3])))
        }
        // A new turn, like a sent message (see `ItemRow`).
        .padding(.top, Space.m)
        .accessibilityElement(children: .combine)
    }
}

/// What transcript rows can ask the core to do. Rows hold this instead of
/// the core itself, so they redraw only when their own item changes.
struct TranscriptActions {
    let send: (Event) -> Void
}

/// A message's images as thumbnails; a tap shows them full size, from that
/// one. Past three, the third says how many more there are (the viewer
/// swipes through them all).
struct MessageImages: View {
    let images: [ImageView]
    /// The side of one image alone (several show smaller).
    var single: CGFloat = 180
    @State private var gallery: ImageGallery?
    @Environment(\.showImages) private var showImages

    /// The most tiles a row shows.
    private static let shown = 3

    var body: some View {
        let tiles = Array(images.prefix(Self.shown))
        let more = images.count - tiles.count
        HStack(spacing: Space.s) {
            ForEach(Array(tiles.enumerated()), id: \.offset) { i, image in
                ImageTile(image: image, side: images.count == 1 ? single : 96, more: i == tiles.count - 1 ? more : 0) {
                    let shown = ImageGallery(images: images, start: i)
                    if let showImages { showImages(shown) } else { gallery = shown }
                }
                .id(image.url)
            }
        }
        .imageGallery($gallery)
    }
}

extension ItemView {
    /// The HTML files a change wrote, which `OpenArtifactFile` reads from the workspace.
    var htmlArtifactPaths: [String] {
        self.artifactPaths.filter { ["html", "htm"].contains(($0 as NSString).pathExtension.lowercased()) }
    }
}

struct ItemRow: View, @MainActor Equatable {
    let item: ItemView
    let retryBlocked: Bool
    let actions: TranscriptActions
    @State private var selecting = false
    @State private var hovering = false
    @Environment(\.selectText) private var selectText
    @Environment(\.core) private var core
    @Environment(\.colorScheme) private var colorScheme

    private var coreThreadId: String { core?.view.threadId ?? "" }

    private var selectableBody: String {
        guard !item.blocks.isEmpty else { return item.text }
        return item.blocks.map { block in
            if !block.cells.isEmpty {
                return block.cells.enumerated().map { index, source in
                    let runs = block.cellRuns.indices.contains(index) ? block.cellRuns[index] : []
                    return runs.isEmpty ? source : runs.map(\.text).joined()
                }.joined(separator: " | ")
            }
            return block.runs.isEmpty ? block.text : block.runs.map(\.text).joined()
        }.joined(separator: "\n\n")
    }

    static func == (a: ItemRow, b: ItemRow) -> Bool {
        a.item == b.item && a.retryBlocked == b.retryBlocked
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            content
            if item.runDurationMs > 0, item.group.isEmpty {
                Text(TurnTime.footer(started: item.runStartedMs, duration: item.runDurationMs))
                    .font(TypeRole.metaSmall)
                    .foregroundStyle(.tertiary)
            }
        }
            .contextMenu { menu }
            #if os(macOS)
            .onHover { hovering = $0 }
            #endif
            .sheet(isPresented: $selecting) { SelectTextSheet(text: selectableBody) }
            .overlay(alignment: .topTrailing) {
                if item.findMatches > 0 {
                    Text("\(item.findMatches)")
                        .font(.caption2.bold())
                        // Dark text: white on yellow doesn't read in dark mode.
                        .foregroundStyle(.black)
                        .padding(.horizontal, Space.xs)
                        .background(item.findActive ? Tint.highlight : Tint.highlight.opacity(0.4), in: Capsule())
                        .offset(x: Space.s, y: -Space.tight)
                }
            }
            // A user message starts a turn: more room above it than between
            // the rows of one.
            .padding(.top, item.kind == "user" ? Space.m : 0)
    }

    /// Copy, select and resend, on messages and errors.
    @ViewBuilder private var menu: some View {
        switch item.kind {
        case "user", "assistant", "plan":
            Button { Platform.copy(item.text) } label: { Label(item.kind == "user" ? "Copy" : "Copy Markdown", systemImage: "doc.on.doc") }
            #if os(iOS)
            // The Mac selects message text in place.
            Button {
                if let selectText { selectText(selectableBody) } else { selecting = true }
            } label: { Label("Select Text", systemImage: "selection.pin.in.out") }
            #endif
            if item.kind == "user" {
                Button { actions.send(.editMessage(text: item.text)) } label: { Label("Edit and Send Again", systemImage: "pencil") }
            }
            if item.canForkHere {
                // The new thread opens once the server has made it.
                Button { actions.send(.forkFromItem(itemId: item.id)) } label: { Label("Fork from Here", systemImage: "arrow.triangle.branch") }
            }
        case "error":
            Button { Platform.copy(item.title) } label: { Label("Copy Error", systemImage: "doc.on.doc") }
        case "command":
            // The command as run (the line may name only the file it read).
            Button { Platform.copy(item.text.isEmpty ? item.title : item.text) } label: { Label("Copy Command", systemImage: "doc.on.doc") }
            if let output = item.output { Button { Platform.copy(output) } label: { Label("Copy Output", systemImage: "doc.on.clipboard") } }
        case "tool":
            let fields = item.fields.map { "\($0.label): \($0.value)" }.joined(separator: "\n")
            let code = item.blocks.map(\.text).joined(separator: "\n")
            Button { Platform.copy([[item.verb, item.title].filter { !$0.isEmpty }.joined(separator: " "), item.text, fields, code, item.output ?? ""].filter { !$0.isEmpty }.joined(separator: "\n\n")) } label: { Label("Copy", systemImage: "doc.on.doc") }
            if let output = item.output { Button { Platform.copy(output) } label: { Label("Copy Output", systemImage: "doc.on.clipboard") } }
        case "reasoning":
            Button { Platform.copy(item.text) } label: { Label("Copy Thinking", systemImage: "doc.on.doc") }
        case "file_change":
            // The file as it is in the workspace now, in the Artifacts panel.
            let pages = item.htmlArtifactPaths
            ForEach(pages, id: \.self) { path in
                Button {
                    actions.send(.openArtifactFile(threadId: coreThreadId, path: path))
                    actions.send(.showPanel(tab: "artifacts"))
                } label: {
                    Label(pages.count == 1 ? "Open Preview" : "Open Preview of \((path as NSString).lastPathComponent)", systemImage: "safari")
                }
            }
            Button { Platform.copy(item.diff?.path ?? item.title) } label: { Label("Copy Path", systemImage: "doc.on.doc") }
            if let diff = item.diff {
                Button { Platform.copy(diff.hunks.flatMap(\.lines).map(\.text).joined(separator: "\n")) } label: { Label("Copy Changed Lines", systemImage: "doc.on.clipboard") }
            }
        default:
            EmptyView()
        }
    }

    @ViewBuilder private var content: some View {
        switch item.kind {
        case "todo":
            TodoCard(item: item)
        case "artifact":
            if let artifactId = item.artifactId {
                ArtifactCard(
                    title: item.artifactTitle.isEmpty ? item.title : item.artifactTitle,
                    kind: item.artifactKind.isEmpty ? "html" : item.artifactKind,
                    version: item.artifactVersion,
                    streaming: item.streaming,
                    hostedUrl: item.artifactHostedUrl,
                    open: { actions.send(.openArtifact(threadId: coreThreadId, artifactId: artifactId, version: item.artifactVersion > 0 ? item.artifactVersion : nil, dark: colorScheme == .dark)) },
                    viewCode: {
                        actions.send(.openArtifact(threadId: coreThreadId, artifactId: artifactId, version: item.artifactVersion > 0 ? item.artifactVersion : nil, dark: colorScheme == .dark))
                        actions.send(.setArtifactViewMode(mode: "code"))
                    }
                )
            } else {
                ArtifactCard(
                    title: item.artifactTitle.isEmpty ? item.title : item.artifactTitle,
                    kind: item.artifactKind.isEmpty ? "html" : item.artifactKind,
                    version: item.artifactVersion,
                    streaming: item.streaming,
                    hostedUrl: item.artifactHostedUrl,
                    open: { actions.send(.showPanel(tab: "artifacts")) }
                )
            }
        case "user":
            HStack {
                Spacer(minLength: Space.xxl * 1.5)
                #if os(macOS)
                hoverActions.opacity(hovering ? 1 : 0).allowsHitTesting(hovering)
                #endif
                VStack(alignment: .trailing, spacing: Space.s) {
                    if !item.images.isEmpty { MessageImages(images: item.images) }
                    if !item.text.isEmpty { Text(item.text).selectableText() }
                    ForEach(item.attachments.filter { name in !item.images.contains { $0.name == name } }, id: \.self) { name in
                        Label(name, systemImage: "paperclip")
                            .font(TypeRole.meta)
                            .padding(.horizontal, Space.s)
                            .padding(.vertical, Space.xxs)
                            .background(.background.opacity(0.6), in: Capsule())
                    }
                }
                .padding(.horizontal, Space.l)
                .padding(.vertical, Space.m)
                .background(Color.accentColor.opacity(0.14), in: RoundedRectangle(cornerRadius: Radius.bubble, style: .continuous))
            }
        case "assistant":
            VStack(alignment: .leading, spacing: Space.s) {
                MarkdownBlocks(blocks: item.blocks, selectable: false, streaming: item.streaming)
                // The caret sits at the end of the text; on its own line
                // only when the reply ends in code, a table or nothing yet.
                if item.streaming, !MarkdownBlocks.caretInline(item.blocks) { StreamingCaret() }
            }
            #if os(macOS)
            .overlay(alignment: .topTrailing) {
                if hovering, !item.streaming { hoverActions.offset(y: -Space.m) }
            }
            #endif
        case "reasoning":
            Collapsible(title: "Thinking", icon: "brain", startCollapsed: item.collapsed) {
                Text(item.text).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
            }
        case "command", "tool", "file_change":
            StepRow(item: item, actions: actions)
        case "question":
            Label {
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(item.title).font(TypeRole.rowTitle).lineLimit(4)
                    Text(item.status == "waiting" ? "Waiting for your answer" : (item.text.isEmpty ? "Skipped" : item.text))
                        .font(TypeRole.meta).foregroundStyle(.secondary)
                }
            } icon: {
                Image(systemName: item.status == "waiting" ? "questionmark.bubble.fill" : "questionmark.bubble").foregroundStyle(item.status == "waiting" ? Tint.question : .secondary)
            }
            .padding(Space.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .cardSurface(tint: item.status == "waiting" ? Tint.question : nil)
            .accessibilityElement(children: .combine)
        case "approval":
            Label {
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(item.title).font(TypeRole.rowTitle).lineLimit(3)
                    Text(item.status == "waiting" ? "Waiting for approval" : "Answered").font(TypeRole.meta).foregroundStyle(.secondary)
                }
            } icon: {
                Image(systemName: item.status == "waiting" ? "hand.raised.fill" : "hand.raised").foregroundStyle(item.status == "waiting" ? Tint.caution : .secondary)
            }
            .padding(Space.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .cardSurface(tint: item.status == "waiting" ? Tint.caution : nil)
        case "plan":
            PlanItemCard(item: item, actions: actions)
        case "checkpoint":
            CheckpointRow(item: item, actions: actions)
        case "subagent":
            SubagentRow(item: item, actions: actions)
        case "error":
            VStack(alignment: .leading, spacing: Space.s) {
                Label(item.title, systemImage: "exclamationmark.triangle.fill")
                    .font(TypeRole.rowTitle)
                    .foregroundStyle(Tint.danger)
                    .selectableText()
                if item.canRetry {
                    Button { actions.send(.retry) } label: { Label("Try Again", systemImage: "arrow.clockwise") }
                        .buttonStyle(.bordered)
                        .cardControlSize()
                        .disabled(retryBlocked)
                }
            }
            .padding(Space.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .cardSurface(tint: Tint.danger)
        default:
            NoticeRow(item: item)
        }
    }

    /// Copy (and Edit and Send Again on your own messages) under the
    /// pointer, on the Mac; the context menu has them too.
    private var hoverActions: some View {
        HStack(spacing: Space.xxs) {
            Button { Platform.copy(item.text) } label: { Image(systemName: "doc.on.doc") }
                .help("Copy")
                .accessibilityLabel("Copy")
            if item.kind == "user" {
                Button { actions.send(.editMessage(text: item.text)) } label: { Image(systemName: "pencil") }
                    .help("Edit and Send Again")
                    .accessibilityLabel("Edit and Send Again")
            }
            if item.canForkHere {
                Button { actions.send(.forkFromItem(itemId: item.id)) } label: { Image(systemName: "arrow.triangle.branch") }
                    .help("Fork from Here: a new thread that starts from this reply")
                    .accessibilityLabel("Fork from Here")
            }
        }
        .buttonStyle(.borderless)
        .font(TypeRole.meta)
        .foregroundStyle(.secondary)
        .padding(.horizontal, Space.s)
        .padding(.vertical, Space.xs)
        .glassChip()
    }

    /// Long output shows its end; this shows all of it.
    private var showAll: some View {
        Button { actions.send(.expandItem(id: item.id)) } label: { Text("Show All").tapTarget() }
            .font(TypeRole.meta)
            .buttonStyle(.borderless)
    }

}

struct CheckpointRow: View {
    let item: ItemView
    let actions: TranscriptActions
    @Environment(\.openDiff) private var openDiff
    @State private var confirmRevert = false
    /// The turn's files are listed.
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Space.s) {
                if let cp = item.checkpoint, !cp.files.isEmpty {
                    Button { withAnimation(.snappy) { expanded.toggle() } } label: {
                        HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                            DisclosureChevron(open: expanded)
                            title(cp)
                        }
                        .cardRowInset()
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(item.title), ^[\(cp.files.count) file](inflect: true) changed")
                    .accessibilityHint(expanded ? "Hides the files" : "Lists the files")
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s) { title(item.checkpoint) }
                        .padding(.vertical, Space.m)
                }
                Spacer()
                if let cp = item.checkpoint {
                    if cp.filesChanged > 0 {
                        Button { showDiff(cp) } label: { Text("View Diff").tapTarget() }
                            .linkButton()
                            // On one line: the title gives way (it truncates) before a
                            // button wraps.
                            .lineLimit(1)
                            .fixedSize()
                    }
                    if cp.canRevert {
                        Button { confirmRevert = true } label: { Text("Revert to Here").tapTarget() }
                            .linkButton()
                            .lineLimit(1)
                            .fixedSize()
                            .confirmationDialog("Revert files and the conversation to \(cp.label)?", isPresented: $confirmRevert) {
                                Button("Revert", role: .destructive) { actions.send(.revert(checkpointId: cp.id)) }
                            } message: {
                                Text("Later changes and messages are discarded.")
                            }
                    }
                }
            }
            if expanded, let cp = item.checkpoint {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(cp.files, id: \.path) { file in
                        Button { showDiff(cp) } label: {
                            HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                                Image(systemName: "doc").foregroundStyle(.secondary).font(TypeRole.meta)
                                FileChangeRow(path: file.path, added: Int(file.insertions), removed: Int(file.deletions))
                            }
                            .padding(.vertical, Space.xxs)
                            .contentShape(Rectangle())
                            .tapTarget()
                        }
                        .buttonStyle(.plain)
                        .help("Show the turn's diff")
                        .accessibilityLabel("\(file.path), \(file.insertions) added, \(file.deletions) removed")
                    }
                }
                .padding(.leading, Space.disclosureIndent)
                .padding(.bottom, Space.m)
                .transition(.opacity)
            }
        }
        .padding(.horizontal, Space.m)
        .cardSurface()
    }

    @ViewBuilder private func title(_ cp: CheckpointView?) -> some View {
        Image(systemName: "flag.checkered").foregroundStyle(.secondary).iconColumn()
        Text(item.title).font(TypeRole.rowTitle).lineLimit(1)
        // A turn that changed nothing has no counts or diff to show.
        if let cp, cp.filesChanged > 0 {
            Text("+\(cp.additions) −\(cp.deletions)").font(TypeRole.meta.monospacedDigit()).foregroundStyle(.secondary)
        }
    }

    /// The turn's diff: the Mac's side panel, the iPhone's diff screen.
    private func showDiff(_ cp: CheckpointView) {
        actions.send(.selectDiffSource(source: cp.diffSource.isEmpty ? "turn:\(cp.turnCount)" : cp.diffSource))
        actions.send(.showPanel(tab: "diff"))
        openDiff()
    }
}

/// A step of the agent's work as one line: what it did, to what, and how it
/// went ("Read present.rs · src · lines 1–60", "Ran cargo test · exit 101").
/// Open, it shows what more there is: the command as run, the tool's
/// arguments, links, the file it read, the diff, the output. A step with
/// nothing more is a plain line with no disclosure.
struct StepRow: View {
    let item: ItemView
    let actions: TranscriptActions
    @State private var expanded: Bool?
    @Environment(\.core) private var core
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let isOpen = item.expandable && (expanded ?? !item.collapsed)
        VStack(alignment: .leading, spacing: 0) {
            if item.expandable {
                Button {
                    withAnimation(.snappy) { expanded = !isOpen }
                } label: {
                    header(open: isOpen)
                }
                .buttonStyle(.plain)
                .accessibilityHint(isOpen ? "Hides the details" : "Shows the details")
            } else {
                header(open: nil)
            }
            if !item.images.isEmpty {
                // What the agent looked at shows without opening the step.
                MessageImages(images: item.images, single: 120)
                    .padding(.leading, Space.disclosureIndent)
                    .padding(.bottom, Space.m)
            }
            if isOpen {
                details
                    .padding(.leading, Space.disclosureIndent)
                    .padding(.bottom, Space.m)
                    .transition(.opacity)
            }
        }
        .padding(.horizontal, Space.m)
        .cardSurface()
    }

    private func header(open: Bool?) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s) {
            // A plain line keeps the chevron's column, so the icons line up.
            DisclosureChevron(open: open ?? false).opacity(open == nil ? 0 : 1)
            Image(systemName: StepRow.symbol(item.icon, kind: item.kind)).foregroundStyle(iconTint).iconColumn()
            line(open: open ?? false)
                .frame(maxWidth: .infinity, alignment: .leading)
            if item.kind == "file_change", let artifactId = item.artifactId, !item.streaming {
                Button {
                    actions.send(.openArtifact(threadId: core?.view.threadId ?? "", artifactId: artifactId, version: item.artifactVersion > 0 ? item.artifactVersion : nil, dark: colorScheme == .dark))
                } label: {
                    Label("Preview", systemImage: "sparkles.rectangle.stack")
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, Space.s)
                        .padding(.vertical, 3)
                        .background(Color.accentColor.opacity(0.14), in: Capsule())
                        .foregroundStyle(Color.accentColor)
                }
                .buttonStyle(.borderless)
                .fixedSize()
            }
            // The exit badge says "exit 101" itself.
            if !item.badge.isEmpty, item.badge != exitCode.map({ "exit \($0)" }) {
                Text(item.badge)
                    .font(TypeRole.meta.monospacedDigit())
                    .foregroundStyle(item.badgeTone == "failed" ? AnyShapeStyle(Tint.danger) : AnyShapeStyle(.secondary))
                    .lineLimit(1)
                    .fixedSize()
            }
            // A quiet check for success (as on Android and desktop); a pill only when it failed.
            if exitCode == 0 {
                Image(systemName: "checkmark")
                    .font(TypeRole.meta)
                    .foregroundStyle(.secondary)
                    .help("Exited successfully")
                    .accessibilityLabel("Exited successfully")
            } else if let exitCode {
                Text("exit \(exitCode)")
                    .monospacedDigit()
                    .foregroundStyle(Tint.danger)
                    .badgeCapsule(tint: Tint.danger)
                    .fixedSize()
                    .help("Exited with code \(exitCode)")
            }
            StatusDot(status: item.status)
        }
        .cardRowInset()
        .accessibilityElement(children: .combine)
    }

    /// Verb, then what it worked on, then the detail; the detail gives way first.
    @ViewBuilder private func line(open: Bool) -> some View {
        let title = item.title
        let dimmed = item.status == "interrupted"
        HStack(alignment: .firstTextBaseline, spacing: Space.xs) {
            if !item.verb.isEmpty {
                Text(item.verb).font(TypeRole.rowTitle.weight(.medium)).foregroundStyle(dimmed ? .secondary : .primary)
                    .lineLimit(1).fixedSize()
            }
            if !title.isEmpty || item.verb.isEmpty {
                Text(title.isEmpty ? "…" : title)
                    .font(item.mono ? TypeRole.mono : TypeRole.rowTitle)
                    .foregroundStyle(item.verb.isEmpty && !dimmed ? .primary : .secondary)
                    .lineLimit(open ? 4 : 1)
                    .truncationMode(item.icon == "command" ? .tail : .middle)
                    .layoutPriority(1)
            }
            if !item.subtitle.isEmpty {
                Text(item.subtitle).font(TypeRole.meta).foregroundStyle(.tertiary).lineLimit(1)
            }
        }
    }

    @ViewBuilder private var details: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            if !item.text.isEmpty {
                Text(item.text)
                    .font(TypeRole.monoSmall)
                    .foregroundStyle(item.kind == "file_change" ? AnyShapeStyle(Tint.danger) : AnyShapeStyle(.secondary))
                    .textSelection(.enabled)
            }
            if !item.fields.isEmpty {
                Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: Space.m, verticalSpacing: Space.xs) {
                    ForEach(Array(item.fields.enumerated()), id: \.offset) { _, field in
                        GridRow {
                            Text(field.label).font(TypeRole.meta).foregroundStyle(.secondary).gridColumnAlignment(.trailing)
                            Text(field.value)
                                .font(field.mono ? TypeRole.monoSmall : TypeRole.meta)
                                .lineLimit(item.truncated ? 12 : nil)
                                .textSelection(.enabled)
                        }
                    }
                }
            }
            if !item.links.isEmpty {
                VStack(alignment: .leading, spacing: Space.xs) {
                    ForEach(item.links, id: \.url) { link in
                        if let url = URL(string: link.url) {
                            Link(destination: url) {
                                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                                    Text(link.title).font(TypeRole.meta).lineLimit(1)
                                    Text(link.host).font(TypeRole.metaSmall).foregroundStyle(.secondary).lineLimit(1)
                                }
                            }
                            .help(link.url)
                        }
                    }
                }
            }
            if !item.blocks.isEmpty {
                MarkdownBlocks(blocks: item.blocks, selectable: true, streaming: false)
            }
            if let diff = item.diff {
                // Long lines scroll sideways rather than widening the card.
                ScrollView(.horizontal, showsIndicators: false) {
                    DiffFileBody(file: diff)
                }
            }
            if let output = item.output, !output.isEmpty {
                if !item.fields.isEmpty || !item.text.isEmpty { Divider() }
                if item.kind == "command" {
                    #if os(iOS)
                    // A phone: the last lines, scrolling only sideways (a
                    // box that scrolls up and down inside the transcript
                    // catches the reader's swipes); all of it in a sheet.
                    CommandOutputTail(title: item.text.isEmpty ? item.title : item.text, output: output)
                    #else
                    ScrollView([.horizontal, .vertical]) {
                        Text(output)
                            .font(TypeRole.monoSmall)
                            .selectableText()
                            .fixedSize()
                    }
                    .defaultScrollAnchor(.bottomLeading)
                    .frame(maxWidth: .infinity, maxHeight: 280, alignment: .topLeading)
                    #endif
                } else {
                    Text(output).font(TypeRole.monoSmall).lineLimit(item.truncated ? 40 : nil).selectable(limited: item.truncated)
                }
            } else if item.kind == "command", item.status == "running" {
                Text("Running…").font(TypeRole.meta).foregroundStyle(.secondary)
            }
            if item.truncated {
                Button { actions.send(.expandItem(id: item.id)) } label: { Text("Show All").tapTarget() }
                    .font(TypeRole.meta)
                    .buttonStyle(.borderless)
            }
        }
    }

    /// A finished command's exit code.
    private var exitCode: Int64? {
        item.kind == "command" && item.status != "running" ? item.exitCode : nil
    }

    private var iconTint: Color {
        if item.status == "failed" || item.badgeTone == "failed" { return Tint.danger }
        switch item.status {
        case "waiting": return Tint.caution
        case "running": return .accentColor
        default: return .secondary
        }
    }

    /// A step's icon from its kind of work.
    static func symbol(_ icon: String, kind: String) -> String {
        switch icon {
        case "read": return "doc.text"
        case "image": return "photo"
        case "search": return "magnifyingglass"
        case "list": return "folder"
        case "command": return "terminal"
        case "edit": return "pencil"
        case "web": return "globe"
        case "fetch": return "link"
        case "mcp": return "puzzlepiece.extension"
        case "skill": return "sparkles"
        case "agent": return "person.2"
        case "question": return "questionmark.bubble"
        case "wait": return "clock"
        case "tool": return "wrench.and.screwdriver"
        default:
            switch kind {
            case "command": return "terminal"
            case "file_change": return "doc.badge.gearshape"
            default: return "wrench.and.screwdriver"
            }
        }
    }
}

/// A row with a disclosure header (commands, file changes, tools, reasoning).
struct Collapsible<Content: View>: View {
    let title: String
    let icon: String
    var monospaced = false
    var status = ""
    let startCollapsed: Bool
    var trailing: String?
    @ViewBuilder let content: () -> Content
    @State private var expanded: Bool?

    var body: some View {
        let isOpen = expanded ?? !startCollapsed
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.snappy) { expanded = !isOpen }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                    DisclosureChevron(open: isOpen)
                    Image(systemName: icon).foregroundStyle(statusColor).iconColumn()
                    Text(title.isEmpty ? "…" : title)
                        .font(monospaced ? TypeRole.mono : TypeRole.rowTitle)
                        .foregroundStyle(status == "interrupted" ? .secondary : .primary)
                        .lineLimit(isOpen ? 4 : 1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let trailing { Text(trailing).font(TypeRole.meta.monospacedDigit()).foregroundStyle(.secondary) }
                    StatusDot(status: status)
                }
                .cardRowInset()
            }
            .buttonStyle(.plain)
            if isOpen {
                // Under the header's icon, not its chevron.
                content()
                    .padding(.leading, Space.disclosureIndent)
                    .padding(.bottom, Space.m)
                    .transition(.opacity)
            }
        }
        .padding(.horizontal, Space.m)
        .cardSurface()
    }

    private var statusColor: Color {
        switch status {
        case "failed": return Tint.danger
        case "waiting": return Tint.caution
        case "running": return .accentColor
        default: return .secondary
        }
    }
}

/// A step's state at the end of its row: a pulsing dot while it runs, an
/// orange one while it waits, "Stopped" when it was cut short.
struct StatusDot: View {
    let status: String

    var body: some View {
        switch status {
        case "running":
            Circle().fill(Color.accentColor).frame(width: Size.dot, height: Size.dot).pulsing()
                .accessibilityLabel("Running")
        case "waiting":
            Circle().fill(Tint.caution).frame(width: Size.dot, height: Size.dot).pulsing()
                .accessibilityLabel("Waiting")
        case "interrupted":
            Text("Stopped").font(TypeRole.badge).foregroundStyle(.secondary)
        default:
            EmptyView()
        }
    }
}

/// The end of a reply still being written.
struct StreamingCaret: View {
    var body: some View {
        Capsule()
            .fill(.tint)
            .frame(width: 8, height: 16)
            .pulsing()
            .accessibilityLabel("Writing")
    }
}

struct ApprovalBar: View {
    @ObservedObject var core: Core
    let requests: [RequestView]
    @State private var showing: RequestView?
    @State private var contentHeight: CGFloat?
    #if os(iOS)
    @Environment(\.verticalSizeClass) private var verticalSize
    #endif

    /// Requests already announced, so each buzzes only when it arrives.
    @MainActor private static var announced: Set<String> = []

    /// Tall enough for a question form, never most of a landscape phone
    /// (Show All opens the whole request).
    private var maxHeight: CGFloat {
        #if os(iOS)
        if verticalSize == .compact { return 150 }
        #endif
        return requests.contains { !$0.questions.isEmpty } ? 480 : 320
    }

    var body: some View {
        ScrollView {
            VStack(spacing: Space.m) {
                ForEach(requests, id: \.id) { request in
                    if request.kind == "user_input", !request.questions.isEmpty {
                        QuestionForm(core: core, request: request)
                    } else if request.kind == "user_input" {
                        question(request)
                    } else {
                        // Side by side where there is room; on a phone (or with large
                        // text) the prompt gets the full width and the buttons a row.
                        ViewThatFits(in: .horizontal) {
                            HStack(alignment: .top, spacing: Space.m) {
                                prompt(request)
                                Spacer(minLength: Space.m)
                                buttons(request)
                            }
                            .frame(minWidth: 620)
                            VStack(alignment: .leading, spacing: Space.m) {
                                prompt(request)
                                HStack(spacing: Space.s) { buttons(request, stretch: true) }
                            }
                        }
                    }
                }
            }
            .padding(Space.l)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { contentHeight = $0 }
        }
        // Fit the rows when they fit, and scroll longer stacks above the composer.
        .scrollBounceBehavior(.basedOnSize)
        .frame(maxHeight: min(contentHeight ?? maxHeight, maxHeight))
        // The composer's shape and side inset: the two stack as one.
        .glassPanel(in: RoundedRectangle(cornerRadius: Radius.panel, style: .continuous), tint: Tint.caution.opacity(0.25))
        .padding(.horizontal, Space.s)
        .padding(.top, Space.xs)
        // Once per request: a new one buzzes (and is announced), coming back
        // to a thread whose request was already seen doesn't.
        .onChange(of: requests.map(\.id), initial: true) { _, ids in
            let new = ids.filter { !Self.announced.contains($0) }
            guard !new.isEmpty else { return }
            Self.announced.formUnion(new)
            Platform.needsAttention()
            AccessibilityNotification.Announcement("The agent needs your answer").post()
        }
        .sheet(item: Binding(get: { showing.map(IdentifiedRequest.init) }, set: { showing = $0?.request })) { shown in
            NavigationStack {
                ScrollView {
                    VStack(alignment: .leading, spacing: Space.m) {
                        Text(shown.request.prompt).font(shown.request.kind == "command" ? .system(.body, design: .monospaced) : .body).textSelection(.enabled)
                        if let detail = shown.request.detail { Text(detail).font(.callout).foregroundStyle(.secondary).textSelection(.enabled) }
                        ForEach(shown.request.files, id: \.path) { file in
                            VStack(alignment: .leading, spacing: Space.xs) {
                                Text(file.path).font(TypeRole.monoSmall.weight(.semibold))
                                ScrollView(.horizontal, showsIndicators: false) {
                                    DiffFileBody(file: file)
                                }
                                .background(.fill.quaternary, in: RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(Space.l)
                }
                .navigationTitle(kindLabel(shown.request.kind))
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showing = nil } } }
            }
            #if os(macOS)
            .frame(minWidth: 520, idealWidth: 640, minHeight: 320, idealHeight: 480)
            #endif
            .presentationDetents([.medium, .large])
        }
    }

    /// A question the agent asked: answered by the next message.
    private func question(_ request: RequestView) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.m) {
            Image(systemName: "questionmark.bubble.fill").foregroundStyle(Tint.question).iconColumn().accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Space.tight) {
                Text("Question").font(TypeRole.meta.bold()).foregroundStyle(.secondary)
                Text(request.prompt).font(.callout).textSelection(.enabled)
                if let detail = request.detail { Text(detail).font(TypeRole.meta).foregroundStyle(.secondary) }
                Text("Answer in the message box below.").font(TypeRole.meta).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Button("Skip") { core.update(.respond(requestId: request.id, decision: "decline")) }
                .glassButton()
        }
        .accessibilityElement(children: .contain)
    }

    private func prompt(_ request: RequestView) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.m) {
            Image(systemName: "hand.raised.fill").foregroundStyle(Tint.caution).iconColumn().accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Space.xxs) {
                Text(kindLabel(request.kind)).font(TypeRole.meta.bold()).foregroundStyle(.secondary)
                Text(request.prompt)
                    .font(request.kind == "command" ? TypeRole.mono : .callout)
                    .lineLimit(6)
                    .selectable(limited: true)
                if let detail = request.detail {
                    Text(detail).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(3)
                }
                if !request.files.isEmpty {
                    // An edit: what it would change, before saying yes.
                    Button { showing = request } label: { Text("Review Changes (^[\(request.files.count) file](inflect: true))").tapTarget() }
                        .font(TypeRole.meta.weight(.semibold))
                        .buttonStyle(.borderless)
                } else if request.prompt.count > 240 || request.prompt.filter({ $0 == "\n" }).count > 5 || (request.detail?.count ?? 0) > 160 {
                    Button { showing = request } label: { Text("Show All").tapTarget() }.font(TypeRole.meta).buttonStyle(.borderless)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func buttons(_ request: RequestView, stretch: Bool = false) -> some View {
        let width: CGFloat? = stretch ? .infinity : nil
        Button {
            Platform.answered(false)
            core.update(.respond(requestId: request.id, decision: "decline"))
        } label: { Text("Decline").frame(maxWidth: width) }
            .glassButton()
        Button {
            Platform.answered(true)
            core.update(.respond(requestId: request.id, decision: "acceptForSession"))
        } label: { Text(stretch ? (request.sessionLabel.hasSuffix("Thread") ? "Thread" : "Session") : request.sessionLabel).frame(maxWidth: width) }
            .glassButton()
            .accessibilityLabel(request.sessionLabel)
        // ⌘↩, not plain Return: a Return meant for somewhere else (focus
        // in the transcript, say) must never approve a command.
        Button {
            Platform.answered(true)
            core.update(.respond(requestId: request.id, decision: "accept"))
        } label: { Text("Allow").fontWeight(.semibold).frame(maxWidth: width) }
            .glassButton(prominent: true)
            .keyboardShortcut(.return, modifiers: .command)
            .help("Allow (⌘↩)")
    }

    private func kindLabel(_ kind: String) -> String {
        switch kind {
        case "user_input": return "Question"
        case "command": return "Run command"
        case "file-change": return "Edit files"
        case "file-read": return "Read files"
        default: return "Permission"
        }
    }
}

/// A question the agent asked with options: one choice (or several) per
/// question, or an answer of your own; Submit sends them, Skip leaves it.
/// Answers picked or typed but not sent yet, by request: leaving the
/// thread (or the bar being rebuilt) doesn't lose them.
@MainActor
private enum QuestionDrafts {
    static var chosen: [String: [String: Set<String>]] = [:]
    static var other: [String: [String: String]] = [:]
}

struct QuestionForm: View {
    @ObservedObject var core: Core
    let request: RequestView
    @State private var chosen: [String: Set<String>] = [:]
    @State private var other: [String: String] = [:]
    /// The question shown when there are several (one at a time).
    @State private var page = 0
    /// The form itself has focus (digits pick options), or an Other… field.
    @FocusState private var focus: Field?

    private enum Field: Hashable {
        case form
        case other(String)
    }

    private func values(_ q: QuestionView) -> [String] {
        let picked = q.options.map(\.value).filter { chosen[q.id]?.contains($0) == true }
        let typed = q.allowCustomAnswer ? (other[q.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines) : ""
        return typed.isEmpty ? picked : picked + [typed]
    }

    private var answered: Bool { request.questions.allSatisfy { !$0.required || !values($0).isEmpty } }
    private var paged: Bool { request.questions.count > 1 }
    private var current: Int { min(page, max(request.questions.count - 1, 0)) }
    private var shown: [QuestionView] {
        paged ? [request.questions[current]] : request.questions
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.l) {
            ForEach(shown, id: \.id) { q in
                VStack(alignment: .leading, spacing: Space.s) {
                    HStack(spacing: Space.xs) {
                        Image(systemName: "questionmark.bubble.fill").foregroundStyle(Tint.question).accessibilityHidden(true)
                        Text(q.header.isEmpty ? "Question" : q.header).font(TypeRole.meta.bold()).foregroundStyle(.secondary)
                        if q.multiSelect { Text("· choose any").font(TypeRole.meta).foregroundStyle(.secondary) }
                        Spacer(minLength: 0)
                        if paged {
                            Text("\(current + 1)/\(request.questions.count)")
                                .font(TypeRole.meta.monospacedDigit().weight(.semibold))
                                .foregroundStyle(.secondary)
                                .accessibilityLabel("Question \(current + 1) of \(request.questions.count)")
                        }
                    }
                    Text(q.question).font(.callout.weight(.medium)).textSelection(.enabled)
                    // By position: an agent can repeat a label.
                    ForEach(Array(q.options.enumerated()), id: \.offset) { index, option in
                        optionRow(q, option, number: index < 9 ? index + 1 : nil)
                    }
                    if q.allowCustomAnswer {
                        TextField("Other…", text: Binding(get: { other[q.id] ?? "" }, set: { other[q.id] = $0 }), axis: .vertical)
                            .font(.callout)
                            .lineLimit(1...3)
                            .focused($focus, equals: .other(q.id))
                            .padding(.horizontal, Space.m)
                            .padding(.vertical, Space.s)
                            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: Radius.chip))
                            .onChange(of: other[q.id] ?? "") { _, text in
                                // Typing an answer of your own replaces a single choice.
                                if !q.multiSelect, !text.isEmpty { chosen[q.id] = [] }
                            }
                    }
                }
                .accessibilityElement(children: .contain)
            }
            HStack(spacing: Space.s) {
                if paged {
                    Button { page = current - 1 } label: { Label("Previous", systemImage: "chevron.left") }
                        .glassButton()
                        .disabled(current == 0)
                }
                Spacer(minLength: 0)
                Button("Skip") { core.update(.dismissUserInput(requestId: request.id)) }
                    .glassButton()
                if paged, current < request.questions.count - 1 {
                    Button { page = current + 1 } label: { Label("Next", systemImage: "chevron.right").labelStyle(.titleAndIcon) }
                        .glassButton(prominent: true)
                        .keyboardShortcut(.return, modifiers: .command)
                        .help("Next question (⌘↩)")
                } else {
                    Button {
                        Platform.answered(true)
                        core.update(.respondUserInput(requestId: request.id, answers: request.questions.map { QuestionAnswer(questionId: $0.id, values: values($0)) }))
                    } label: { Text("Submit").fontWeight(.semibold) }
                        .glassButton(prominent: true)
                        .disabled(!answered)
                        .keyboardShortcut(.return, modifiers: .command)
                        .help(answered ? "Submit (⌘↩)" : "Answer every required question first")
                }
            }
        }
        // A hardware keyboard: 1–9 pick the shown question's options.
        .focusable()
        .focusEffectDisabled()
        .focused($focus, equals: .form)
        .onKeyPress(characters: .decimalDigits) { press in
            guard focus == .form, let digit = Int(press.characters), digit >= 1, let q = shown.first,
                  digit <= min(q.options.count, 9) else { return .ignored }
            toggle(q, q.options[digit - 1])
            return .handled
        }
        .onAppear {
            chosen = QuestionDrafts.chosen[request.id] ?? [:]
            other = QuestionDrafts.other[request.id] ?? [:]
            // Ready for the keys, unless a message is being written.
            if !Platform.isPhone, core.view.composer.draft.isEmpty { focus = .form }
        }
        .onChange(of: chosen) { _, value in QuestionDrafts.chosen[request.id] = value }
        .onChange(of: other) { _, value in QuestionDrafts.other[request.id] = value }
    }

    private func toggle(_ q: QuestionView, _ option: QuestionOptionView) {
        let on = chosen[q.id]?.contains(option.value) == true
        var set = chosen[q.id] ?? []
        if q.multiSelect {
            if on { set.remove(option.value) } else { set.insert(option.value) }
        } else {
            set = on ? [] : [option.value]
            other[q.id] = ""
        }
        chosen[q.id] = set
        Platform.selectionChanged()
    }

    private func optionRow(_ q: QuestionView, _ option: QuestionOptionView, number: Int?) -> some View {
        let on = chosen[q.id]?.contains(option.value) == true
        return Button {
            toggle(q, option)
            if !Platform.isPhone { focus = .form }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: Space.m) {
                Image(systemName: q.multiSelect ? (on ? "checkmark.square.fill" : "square") : (on ? "largecircle.fill.circle" : "circle"))
                    .foregroundStyle(on ? Tint.question : .secondary)
                    .font(.body)
                    .iconColumn()
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text(option.label).font(.callout).foregroundStyle(.primary)
                    if !option.description.isEmpty, option.description != option.label {
                        Text(option.description).font(TypeRole.meta).foregroundStyle(.secondary).multilineTextAlignment(.leading)
                    }
                }
                Spacer(minLength: 0)
                if let number, !Platform.isPhone {
                    Text("\(number)").font(TypeRole.meta.monospacedDigit()).foregroundStyle(.tertiary).accessibilityHidden(true)
                }
            }
            .padding(.horizontal, Space.m)
            .padding(.vertical, Space.s)
            .contentShape(Rectangle())
            .background(on ? Tint.question.opacity(0.12) : Color.clear, in: RoundedRectangle(cornerRadius: Radius.chip))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityHint(q.multiSelect ? "Toggles this choice" : "Chooses this answer")
    }
}

/// `sheet(item:)` needs Identifiable.
private struct IdentifiedRequest: Identifiable {
    let request: RequestView
    var id: String { request.id }
}

struct FindBar: View {
    @ObservedObject var core: Core
    let find: FindView
    @State private var query = ""
    @FocusState private var focused: Bool
    @ObservedObject private var focus = FindFocus.shared

    var body: some View {
        HStack(spacing: Space.s) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Find in thread", text: $query)
                .textFieldStyle(.plain)
                .focused($focused)
                .onSubmit { core.update(.findNext(forward: true)) }
                .onChange(of: query) { _, q in core.update(.findInThread(query: q)) }
            if !find.query.isEmpty {
                Text(find.total == 0 ? "No matches" : "\(find.active) of \(find.total)")
                    .font(TypeRole.meta.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            Button { core.update(.findNext(forward: false)) } label: { Image(systemName: "chevron.up").tapTarget(width: true) }
                .help("Previous match (⇧⌘G)")
                .accessibilityLabel("Previous match")
                .disabled(find.total == 0)
            Button { core.update(.findNext(forward: true)) } label: { Image(systemName: "chevron.down").tapTarget(width: true) }
                .help("Next match (⌘G)")
                .accessibilityLabel("Next match")
                .disabled(find.total == 0)
            Button("Done") { core.update(.closeFind) }
                .keyboardShortcut(.cancelAction)
        }
        .buttonStyle(.borderless)
        // The transcript's side margin, so the field lines up with the rows.
        .padding(.horizontal, Space.gutter)
        .padding(.vertical, Space.s)
        .onAppear {
            query = find.query
            focused = true
        }
        // ⌘F again while open: back to the field; and the query in step
        // with the core's.
        .onChange(of: focus.revision) { _, _ in focused = true }
        // The core's query only while the field isn't being typed in: a view
        // that answers an earlier keystroke must not put that text back.
        .onChange(of: find.query) { _, q in if !focused, q != query { query = q } }
    }
}

/// Requests to put the cursor in the find bar (⌘F while it's open).
@MainActor
final class FindFocus: ObservableObject {
    static let shared = FindFocus()
    @Published var revision = 0
}

// MARK: - Markdown

struct MarkdownBlocks: View {
    let blocks: [MarkdownBlock]
    /// Inline selection; off on iPhone rows whose long press is a menu
    /// (with "Select Text" in it), where the two would fight.
    var selectable = true
    /// A reply still being written: a caret after its last text.
    var streaming = false

    /// Whether the caret can sit at the end of the last block's text.
    static func caretInline(_ blocks: [MarkdownBlock]) -> Bool {
        guard let last = blocks.last else { return false }
        return !["code", "html_document", "html_source", "table_row", "rule"].contains(last.kind)
    }

    /// Blocks in order, with consecutive table rows gathered into one table.
    private enum Segment: Identifiable {
        case block(Int, MarkdownBlock)
        case table(Int, [MarkdownBlock])
        var id: Int {
            switch self {
            case .block(let i, _), .table(let i, _): return i
            }
        }
    }

    private var segments: [Segment] {
        var out: [Segment] = []
        for (i, block) in blocks.enumerated() {
            if block.kind == "table_row" {
                if !block.header, case .table(let start, var rows) = out.last {
                    rows.append(block)
                    out[out.count - 1] = .table(start, rows)
                    continue
                }
                out.append(.table(i, [block]))
            } else {
                out.append(.block(i, block))
            }
        }
        return out
    }

    var body: some View {
        let content = VStack(alignment: .leading, spacing: Space.m) {
            ForEach(segments) { segment in
                switch segment {
                case .block(let index, let block):
                    // Equatable: while a reply streams, its earlier blocks
                    // aren't laid out (and their markdown parsed) again.
                    MarkdownBlockView(block: block, caret: streaming && index == blocks.count - 1).equatable()
                case .table(_, let rows):
                    MarkdownTable(rows: rows).equatable()
                }
            }
        }
        if selectable { content.textSelection(.enabled) } else { content.selectableText() }
    }
}

func styledMarkdown(_ runs: [MarkdownRun], fallback: String) -> AttributedString {
    guard !runs.isEmpty else { return AttributedString(fallback) }
    return runs.reduce(into: AttributedString()) { text, run in
        var part = AttributedString(run.text)
        var intent: InlinePresentationIntent = []
        if run.bold { intent.insert(.stronglyEmphasized) }
        if run.italic { intent.insert(.emphasized) }
        if run.strike { intent.insert(.strikethrough) }
        if run.code { intent.insert(.code) }
        part.inlinePresentationIntent = intent
        if let link = run.link {
            part.link = URL(string: link)
        } else if let wsPath = run.workspaceLink,
                  let encoded = wsPath.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed),
                  let url = URL(string: "domovoi-artifact://workspace?path=\(encoded)") {
            part.link = url
        }
        text += part
    }
}

struct MarkdownBlockView: View, @MainActor Equatable {
    let block: MarkdownBlock
    /// Ends with the streaming caret.
    var caret = false
    @Environment(\.core) private var core
    @Environment(\.colorScheme) private var colorScheme

    static func == (a: Self, b: Self) -> Bool {
        a.block == b.block && a.caret == b.caret
    }

    /// The block's text, with the caret after it while the reply streams.
    private func text(_ source: String) -> Text {
        let body = Text(styledMarkdown(block.runs, fallback: source))
        guard caret else { return body }
        let mark = Text(" ▍").foregroundStyle(.tint)
        return Text("\(body)\(mark)")
    }

    var body: some View {
        Group {
            switch block.kind {
            case "heading":
                text(block.text)
                    .accessibilityAddTraits(.isHeader)
                    .frame(maxWidth: Space.readable, alignment: .leading)
                    .font(block.level <= 1 ? .title2.bold() : block.level == 2 ? .title3.bold() : .headline)
                    .padding(.top, Space.xs)
            case "code" where block.fenceClosed && block.language.lowercased() == "mermaid":
                DiagramPreview(source: block.text)
            case let kind where !caret && block.artifactId != nil && ["html_document", "svg_document", "code"].contains(kind):
                let artifactId = block.artifactId ?? ""
                let threadId = core?.view.threadId ?? ""
                ArtifactCard(
                    title: block.artifactTitle.isEmpty ? ArtifactStyle.label(for: block.artifactKind) : block.artifactTitle,
                    kind: block.artifactKind.isEmpty ? "html" : block.artifactKind,
                    version: block.artifactVersion,
                    open: {
                        core?.update(.openArtifact(threadId: threadId, artifactId: artifactId, version: block.artifactVersion > 0 ? block.artifactVersion : nil, dark: colorScheme == .dark))
                    },
                    viewCode: {
                        core?.update(.openArtifact(threadId: threadId, artifactId: artifactId, version: block.artifactVersion > 0 ? block.artifactVersion : nil, dark: colorScheme == .dark))
                        core?.update(.setArtifactViewMode(mode: "code"))
                    }
                )
            case let kind where (kind == "html_document" && !caret) || (kind == "code" && block.fenceClosed && block.language.lowercased() == "html"):
                VStack(alignment: .leading, spacing: Space.s) {
                    HtmlArtifactButton(source: block.text)
                    CodeBlock(code: block.text, language: "html", spans: block.spans).equatable()
                }
            case "code", "html_source", "html_document", "svg_document":
                CodeBlock(code: block.text, language: block.language, spans: block.spans).equatable()
            case "list_item":
                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                    if let checked = block.taskChecked {
                        Image(systemName: checked ? "checkmark.square" : "square")
                            .foregroundStyle(.secondary)
                            .accessibilityLabel(checked ? "Completed" : "Incomplete")
                    } else {
                        Text(block.continuation ? " " : block.ordered ? "\(block.number)." : "•")
                            .foregroundStyle(.secondary).monospacedDigit()
                            .frame(minWidth: Space.l, alignment: .trailing)
                    }
                    text(block.text)
                }
                .padding(.leading, CGFloat(min(block.level + block.quoteDepth, 6)) * Space.l)
                .frame(maxWidth: Space.readable, alignment: .leading)
            case "quote":
                HStack(spacing: Space.s) {
                    Capsule().fill(.tertiary).frame(width: Space.xxs)
                    text(block.text).foregroundStyle(.secondary)
                }
                // Nested quotes step in as far as nested list items.
                .padding(.leading, CGFloat(min(block.quoteDepth > 0 ? block.quoteDepth - 1 : 0, 5)) * Space.l)
                .frame(maxWidth: Space.readable, alignment: .leading)
            case "rule":
                Divider().padding(.vertical, Space.xs)
            default:
                text(block.text).frame(maxWidth: Space.readable, alignment: .leading)
            }
        }
        .environment(\.openURL, OpenURLAction { url in
            if url.scheme == "domovoi-artifact",
               let comps = URLComponents(url: url, resolvingAgainstBaseURL: false),
               let path = comps.queryItems?.first(where: { $0.name == "path" })?.value,
               let threadId = core?.view.threadId {
                core?.update(.openArtifactWorkspacePath(threadId: threadId, path: path, dark: colorScheme == .dark))
                return .handled
            }
            return .systemAction
        })
    }
}

/// A markdown table: columns as wide as their content (up to 280 pt, then
/// wrapped), scrolling sideways when wider than the screen instead of
/// squeezing every cell.
struct MarkdownTable: View, Equatable {
    let rows: [MarkdownBlock]

    var body: some View {
        let columns = rows.map(\.cells.count).max() ?? 0
        ScrollView(.horizontal, showsIndicators: false) {
            TableLayout(columns: columns, maxColumnWidth: 280) {
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    let fill = row.header ? Color.secondary.opacity(0.12) : index.isMultiple(of: 2) ? Color.secondary.opacity(0.04) : Color.clear
                    // Short rows padded, so every row has a cell per column.
                    ForEach(0..<columns, id: \.self) { column in
                        let align = row.alignments.indices.contains(column) ? row.alignments[column] : "left"
                        Text(column < row.cells.count ? styledMarkdown(row.cellRuns.indices.contains(column) ? row.cellRuns[column] : [], fallback: row.cells[column]) : AttributedString())
                            .multilineTextAlignment(align == "right" ? .trailing : align == "center" ? .center : .leading)
                            .fontWeight(row.header ? .semibold : .regular)
                            .padding(.horizontal, Space.s)
                            .padding(.vertical, Space.tight)
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: align == "right" ? .topTrailing : align == "center" ? .top : .topLeading)
                            .background(fill)
                    }
                }
            }
            .font(.callout)
            .clipShape(RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Radius.chip, style: .continuous).strokeBorder(Color.primary.opacity(0.08), lineWidth: 0.5))
        }
    }
}

/// Cells in rows of `columns`, row-major. Each column is as wide as its
/// widest cell's one-line width, at most `maxColumnWidth`; each row as tall
/// as its tallest cell at its column's width. Measuring at the width a cell
/// is drawn at keeps wrapped cells from spilling into the rows below (as a
/// `Grid` in a sideways scroll view did).
struct TableLayout: Layout {
    let columns: Int
    let maxColumnWidth: CGFloat

    struct Sizes {
        var widths: [CGFloat] = []
        var heights: [CGFloat] = []
    }

    func makeCache(subviews: Subviews) -> Sizes? { nil }

    private func sizes(_ subviews: Subviews, _ cache: inout Sizes?) -> Sizes {
        if let cache { return cache }
        guard columns > 0 else { return Sizes() }
        var widths = Array(repeating: CGFloat(0), count: columns)
        for (i, cell) in subviews.enumerated() {
            widths[i % columns] = max(widths[i % columns], min(cell.sizeThatFits(.unspecified).width, maxColumnWidth))
        }
        var heights: [CGFloat] = []
        for (i, cell) in subviews.enumerated() {
            let height = cell.sizeThatFits(ProposedViewSize(width: widths[i % columns], height: nil)).height
            if i % columns == 0 { heights.append(height) } else { heights[heights.count - 1] = max(heights[heights.count - 1], height) }
        }
        let sizes = Sizes(widths: widths, heights: heights)
        cache = sizes
        return sizes
    }

    func updateCache(_ cache: inout Sizes?, subviews: Subviews) { cache = nil }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Sizes?) -> CGSize {
        let s = sizes(subviews, &cache)
        return CGSize(width: s.widths.reduce(0, +), height: s.heights.reduce(0, +))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Sizes?) {
        let s = sizes(subviews, &cache)
        guard columns > 0 else { return }
        var y = bounds.minY
        var x = bounds.minX
        for (i, cell) in subviews.enumerated() {
            let (row, column) = (i / columns, i % columns)
            guard row < s.heights.count else { break }
            if column == 0 { x = bounds.minX }
            cell.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(width: s.widths[column], height: s.heights[row]))
            x += s.widths[column]
            if column == columns - 1 { y += s.heights[row] }
        }
    }
}

struct CodeBlock: View, @MainActor Equatable {
    let code: String
    let language: String
    var spans: [CodeSpan] = []
    @State private var copied = false
    @State private var expanded = false

    /// Drawn again only when the code changes: building its highlighted text
    /// walks every character.
    static func == (a: Self, b: Self) -> Bool {
        a.code == b.code && a.language == b.language && a.spans == b.spans
    }

    /// Longer than this folds to its first `foldedLines`.
    private static let foldAbove = 30
    private static let foldedLines = 20

    private var lineCount: Int { code.reduce(1) { $1 == "\n" ? $0 + 1 : $0 } }
    private var folded: Bool { !expanded && lineCount > Self.foldAbove }
    private var shown: AttributedString {
        CodeHighlight.text(spans, fallback: code, maxLines: folded ? Self.foldedLines : nil)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language.isEmpty ? "code" : language).font(TypeRole.badge).foregroundStyle(.secondary)
                Spacer()
                Button {
                    Platform.copy(code)
                    Platform.selectionChanged()
                    copied = true
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .labelStyle(.iconOnly)
                        .contentTransition(.symbolEffect(.replace))
                        .foregroundStyle(copied ? Tint.success : .secondary)
                        .frame(minWidth: Size.chip, minHeight: Size.chip)
                        .contentShape(Rectangle())
                        .tapTarget(width: true)
                }
                .buttonStyle(.borderless)
                .help("Copy")
                .accessibilityLabel(copied ? "Copied" : "Copy code")
            }
            // The copy button sets the bar's height.
            .padding(.leading, Space.m)
            .padding(.trailing, Space.xs)
            .background(.fill.tertiary)
            ScrollView(.horizontal) {
                Text(shown)
                    .font(TypeRole.mono)
                    .fixedSize()
                    .padding(Space.m)
            }
            .defaultScrollAnchor(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
            if lineCount > Self.foldAbove {
                Button {
                    withAnimation(.snappy) { expanded.toggle() }
                } label: {
                    Label(expanded ? "Show less" : "Show all \(lineCount) lines", systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(TypeRole.meta.weight(.medium))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, Space.tight)
                        .contentShape(Rectangle())
                        .tapTarget()
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .background(.fill.tertiary)
            }
        }
        .background(.fill.quaternary)
        .clipShape(RoundedRectangle(cornerRadius: Radius.chip, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Radius.chip, style: .continuous).strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.5))
        .task(id: copied) {
            guard copied else { return }
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

// MARK: - Diffs

struct DiffFileBody: View {
    let file: DiffFileView
    /// "Ask About This Line" on a line's long press (instead of selecting text).
    var onLine: ((DiffLineView) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(file.hunks.enumerated()), id: \.offset) { _, hunk in
                if !hunk.header.isEmpty {
                    Text(hunk.header)
                        .font(TypeRole.monoSmall)
                        .foregroundStyle(.secondary)
                        .padding(.vertical, Space.xxs)
                        .padding(.horizontal, Space.tight)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Tint.question.opacity(0.08))
                }
                ForEach(Array(hunk.lines.enumerated()), id: \.offset) { _, line in
                    if let onLine, line.kind != "note" {
                        DiffLineRow(line: line)
                            .contextMenu {
                                Button("Ask About This Line", systemImage: "text.bubble") { onLine(line) }
                                Button("Copy Line", systemImage: "doc.on.doc") { Platform.copy(line.text) }
                            }
                    } else {
                        DiffLineRow(line: line)
                    }
                }
            }
        }
        .modifier(SelectableUnless(off: onLine != nil))
    }
}

/// Text selection, unless long presses do something else.
private struct SelectableUnless: ViewModifier {
    let off: Bool

    func body(content: Content) -> some View {
        if off { content } else { content.textSelection(.enabled) }
    }
}

struct DiffLineRow: View {
    let line: DiffLineView
    /// Number columns that grow with the text.
    @ScaledMetric(relativeTo: .caption) private var gutter: CGFloat = 38
    @ScaledMetric(relativeTo: .caption) private var markerWidth: CGFloat = 16
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var widthClass
    /// A phone: one number column (the new line's, or the old one for a
    /// removed line), so the code gets the width.
    private var oneGutter: Bool { widthClass == .compact }
    #else
    private let oneGutter = false
    #endif

    var body: some View {
        HStack(spacing: 0) {
            if oneGutter {
                Text((line.newLine ?? line.oldLine).map(String.init) ?? "").frame(width: gutter, alignment: .trailing)
                    .foregroundStyle(.secondary)
            } else {
                Text(line.oldLine.map(String.init) ?? "").frame(width: gutter, alignment: .trailing)
                Text(line.newLine.map(String.init) ?? "").frame(width: gutter, alignment: .trailing)
            }
            Text(marker).frame(width: markerWidth)
            Text(line.text).fixedSize()
            Spacer(minLength: 0)
        }
        .font(TypeRole.code)
        .foregroundStyle(line.kind == "note" ? Color.secondary : Color.primary)
        .background(background)
    }

    private var marker: String {
        switch line.kind {
        case "add": return "+"
        case "del": return "−"
        default: return " "
        }
    }

    private var background: Color {
        switch line.kind {
        case "add": return Tint.diffAdd.opacity(0.14)
        case "del": return Tint.diffDel.opacity(0.14)
        default: return .clear
        }
    }
}

extension View {
    /// On the Mac (26 and later), a hard top scroll edge under the toolbar.
    @ViewBuilder func macHardTopScrollEdge() -> some View {
        #if os(macOS)
        if #available(macOS 26.0, *) {
            self.scrollEdgeEffectStyle(.hard, for: .top)
        } else {
            self
        }
        #else
        self
        #endif
    }
}

#if os(iOS)
/// A command's last lines, and a sheet with all of its output.
private struct CommandOutputTail: View {
    let title: String
    let output: String
    @State private var showing = false
    private static let lines = 12

    var body: some View {
        let all = output.split(separator: "\n", omittingEmptySubsequences: false)
        VStack(alignment: .leading, spacing: Space.s) {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(all.suffix(Self.lines).joined(separator: "\n"))
                    .font(TypeRole.monoSmall)
                    .fixedSize()
            }
            if all.count > Self.lines {
                Button { showing = true } label: { Text("Open Output (\(all.count) lines)").tapTarget() }
                    .font(TypeRole.meta)
                    .buttonStyle(.borderless)
            }
        }
        .contextMenu { Button("Copy Output", systemImage: "doc.on.doc") { Platform.copy(output) } }
        .sheet(isPresented: $showing) {
            NavigationStack {
                ScrollView([.horizontal, .vertical]) {
                    Text(output).font(TypeRole.monoSmall).textSelection(.enabled).fixedSize().padding(Space.l)
                }
                .defaultScrollAnchor(.bottomLeading)
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { showing = false } }
                    ToolbarItem(placement: .primaryAction) {
                        Button { Platform.copy(output) } label: { Label("Copy", systemImage: "doc.on.doc") }
                    }
                }
            }
        }
    }
}
#endif
