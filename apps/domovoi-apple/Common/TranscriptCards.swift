import App
import SwiftUI
#if os(macOS)
import AppKit
#endif

/// A plan the agent proposed: folded to its first lines when long, with
/// copy, save and share, and Implement while it can be carried out.
struct PlanItemCard: View {
    let item: ItemView
    let actions: TranscriptActions
    @State private var expanded = false
    @State private var fullHeight: CGFloat = 0
    @State private var saving = false
    @State private var savePath = ""

    /// About eight lines of the plan.
    private static let foldedHeight: CGFloat = 180
    /// Folding only pays when it hides more than a couple of lines.
    private var long: Bool { fullHeight > Self.foldedHeight + 40 }
    private var folded: Bool { long && !expanded }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            HStack(spacing: Space.s) {
                Label("Plan", systemImage: "list.bullet.clipboard").font(TypeRole.cardTitle).foregroundStyle(Tint.plan)
                if item.superseded {
                    Text("Superseded")
                        .badgeCapsule()
                        .foregroundStyle(.secondary)
                        .help("A later plan replaced this one")
                }
                Spacer(minLength: Space.s)
                menu
            }
            MarkdownBlocks(blocks: item.blocks, selectable: false)
                .fixedSize(horizontal: false, vertical: true)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { fullHeight = $0 }
                .frame(maxHeight: folded ? Self.foldedHeight : nil, alignment: .top)
                .clipped()
                .mask {
                    if folded {
                        LinearGradient(stops: [.init(color: .black, location: 0.7), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom)
                    } else {
                        Rectangle()
                    }
                }
            HStack(spacing: Space.m) {
                if item.canImplement {
                    Button { actions.send(.implementPlan(planId: item.planId)) } label: {
                        Label("Implement Plan", systemImage: "play.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Tint.plan)
                    .cardControlSize()
                    .help("Leave plan mode and have the agent carry out this plan")
                }
                if long {
                    Button {
                        withAnimation(.snappy) { expanded.toggle() }
                    } label: { Text(expanded ? "Collapse" : "Show Full Plan").tapTarget() }
                    .buttonStyle(.borderless)
                    .font(.callout)
                }
            }
        }
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface(tint: Tint.plan)
        .opacity(item.superseded ? 0.55 : 1)
        .alert("Save Plan to Workspace", isPresented: $saving) {
            TextField("plans/<name>.md", text: $savePath)
            Button("Save") { actions.send(.savePlan(itemId: item.id, path: savePath.trimmingCharacters(in: .whitespaces))) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("A path in the thread's folder. Left empty, it goes under plans/.")
        }
    }

    private var menu: some View {
        Menu {
            Button { Platform.copy(item.text) } label: { Label("Copy Markdown", systemImage: "doc.on.doc") }
            Button {
                savePath = ""
                saving = true
            } label: { Label("Save to Workspace…", systemImage: "square.and.arrow.down") }
            ShareLink(item: item.text, subject: Text("Plan")) { Label("Share…", systemImage: "square.and.arrow.up") }
            #if os(macOS)
            Button("Export Markdown…", action: export)
            #endif
        } label: {
            Image(systemName: "ellipsis.circle").foregroundStyle(.secondary).tapTarget(width: true)
        }
        .menuIndicator(.hidden)
        .fixedSize()
        .buttonStyle(.borderless)
        .accessibilityLabel("Plan actions")
    }

    #if os(macOS)
    /// The plan's markdown as a file wherever the reader picks.
    private func export() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.init(filenameExtension: "md") ?? .plainText]
        panel.nameFieldStringValue = "plan.md"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        try? item.text.write(to: url, atomically: true, encoding: .utf8)
    }
    #endif
}

/// The agent's to-do list; folded, its header says how far along it is.
struct TodoCard: View {
    let item: ItemView
    @State private var expanded: Bool?

    var body: some View {
        let isOpen = expanded ?? !item.collapsed
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.snappy) { expanded = !isOpen }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                    DisclosureChevron(open: isOpen)
                    Image(systemName: "checklist").iconColumn().accessibilityHidden(true)
                    Text(item.title.isEmpty ? "To-dos" : item.title)
                        .lineLimit(isOpen ? 3 : 1)
                    Spacer(minLength: Space.s)
                    if !isOpen, !item.progress.isEmpty {
                        Text(item.progress).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                .font(TypeRole.cardTitle)
                .cardRowInset()
            }
            .buttonStyle(.plain)
            .accessibilityValue(item.progress)
            if isOpen {
                // Under the header's icon, as in the other cards.
                TodoSteps(todos: item.todos)
                    .padding(.leading, Space.disclosureIndent)
                    .padding(.bottom, Space.m)
                    .transition(.opacity)
            }
        }
        .padding(.horizontal, Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardSurface()
    }
}

struct TodoSteps: View {
    let todos: [TodoView]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            ForEach(Array(todos.enumerated()), id: \.offset) { _, todo in
                HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                    Image(systemName: todo.status == "completed" ? "checkmark.circle.fill" : todo.status == "in_progress" ? "circle.dotted.circle" : "circle")
                        .foregroundStyle(todo.status == "completed" ? Tint.success : todo.status == "in_progress" ? Color.accentColor : .secondary)
                    Text(todo.text)
                        .strikethrough(todo.status == "completed")
                        .foregroundStyle(todo.status == "completed" ? .secondary : .primary)
                        .fontWeight(todo.status == "in_progress" ? .medium : .regular)
                }
                .font(.callout)
                .accessibilityElement(children: .combine)
                .accessibilityValue(todo.status.replacingOccurrences(of: "_", with: " "))
            }
        }
    }
}

/// A note in the conversation (context compacted, continued with another
/// model); one with a summary opens to show it.
struct NoticeRow: View {
    let item: ItemView

    var body: some View {
        if item.text.isEmpty {
            // A card like the notices that open, its icon in the same place
            // (past the chevron column they have).
            HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                Image(systemName: "info.circle").iconColumn().accessibilityHidden(true)
                Text(item.title)
            }
            .font(TypeRole.rowTitle)
            .foregroundStyle(.secondary)
            .padding(.leading, Space.disclosureIndent)
            .padding(Space.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .cardSurface()
            .accessibilityElement(children: .combine)
        } else {
            Collapsible(title: item.title, icon: "info.circle", status: item.status, startCollapsed: item.collapsed) {
                Text(item.text)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}
