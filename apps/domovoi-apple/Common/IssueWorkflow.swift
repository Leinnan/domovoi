import App
import SwiftUI

struct IssueBrowser: View {
    @ObservedObject var core: Core

    var body: some View {
        let page = core.view.issues
        List {
            Picker("Project", selection: Binding(
                get: { page.issueProject ?? "" },
                set: { core.update(.loadIssues(projectId: $0)) }
            )) {
                Text("Choose a project").tag("")
                ForEach(core.view.sidebar.projects, id: \.id) { project in
                    Text(project.title).tag(project.id)
                }
            }
            if page.issueProject != nil {
                Picker("State", selection: Binding(get: { page.issueFilter }, set: { core.update(.setIssueFilter(filter: $0)) })) {
                    Text("Open").tag("open")
                    Text("Closed").tag("closed")
                    Text("All").tag("all")
                }
            }
            if page.issuesLoading { ProgressView() }
            if let notice = page.issuesNotice { Text(notice).foregroundStyle(.secondary) }
            if page.issueProject != nil && !page.issuesLoading && page.issueRows.isEmpty { Text("No issues found.") }
            ForEach(page.issueRows, id: \.issueIndex) { issue in
                VStack(alignment: .leading) {
                    Button(issue.issueLabel) { core.update(.selectIssue(index: issue.issueIndex)) }
                    HStack {
                        if let id = issue.linkedThreadId {
                            Button(issue.linkedThreadTitle ?? "Open thread") { core.update(.selectThread(threadId: id)) }
                        } else {
                            Button("Start thread") { core.update(.startIssueThread(index: issue.issueIndex)) }
                        }
                        if let url = URL(string: issue.issueUrl) { Link("Open issue", destination: url) }
                    }
                    .font(.callout)
                }
            }
            .buttonStyle(.borderless)
            if page.moreIssues {
                Button("Load more") { core.update(.loadMoreIssues) }.disabled(page.issuesLoading)
            }
            if let issue = page.issueDetail {
                Section("Issue details") {
                    Text(issue.issueBody).selectableText()
                    if let value = issue.closingPullRequestUrl, let url = URL(string: value) {
                        Link("Open closing pull request", destination: url)
                    }
                    ForEach(Array(issue.issueComments.enumerated()), id: \.offset) { comment in
                        Text(comment.element).selectableText()
                    }
                }
            }
        }
        .navigationTitle("Issues")
    }
}

struct IssueComposerControls: View {
    @ObservedObject var core: Core

    var body: some View {
        let composer = core.view.composer
        if let issue = composer.issueChip {
            Menu {
                Toggle("Assign to me", isOn: Binding(
                    get: { composer.issueAssignToMe },
                    set: { core.update(.setAssignIssueToMe(assign: $0)) }
                ))
                Button("Remove issue") { core.update(.detachIssue) }
            } label: {
                Label(issue, systemImage: "number").lineLimit(1)
            }
        } else if composer.issueAvailable {
            Button("Attach issue") { core.update(.openIssuePicker) }
                .popover(isPresented: Binding(
                    get: { core.view.composer.issuePicker != nil },
                    set: { if !$0 && core.view.composer.issuePicker != nil { core.update(.detachIssue) } }
                )) {
                    VStack(alignment: .leading) {
                        TextField("Issue URL or #123", text: Binding(
                            get: { core.view.composer.issuePicker ?? "" },
                            set: { core.update(.setIssuePickerQuery(query: $0)) }
                        ))
                        .onSubmit { core.update(.attachIssue) }
                        Button("Attach") { core.update(.attachIssue) }
                    }
                    .padding()
                    .frame(minWidth: 260)
                }
        }
    }
}

struct ThreadPullRequestControls: View {
    @ObservedObject var core: Core
    var showStatus = true
    var showSheet = true

    var body: some View {
        let thread = core.view.thread
        Group {
            if !showStatus {
                Color.clear.frame(width: 0, height: 0)
            } else if let pr = thread?.pullRequestStatus {
                HStack {
                    if let url = URL(string: pr.prStatusUrl) { Link(pr.prStatusLabel, destination: url).lineLimit(1) }
                    if let checks = pr.prStatusChecks { Text(checks) }
                    if let review = pr.prStatusReview { Text(review) }
                    Menu("Actions") {
                        if pr.prStatusHasComments { Button("Send review to agent") { core.update(.handReviewCommentsToAgent) } }
                        if pr.prStatusFollowing { Button("Stop following") { core.update(.stopFollowingPullRequest) } }
                        if thread?.canCreatePullRequest == true { Button("Create pull request") { core.update(.preparePullRequest) } }
                    }
                }
            } else if thread?.canCreatePullRequest == true {
                Button("Create pull request") { core.update(.preparePullRequest) }
            }
        }
        .sheet(isPresented: Binding(
            get: { showSheet && core.view.thread?.pullRequestSheet != nil },
            set: { if !$0 { core.update(.closePullRequestSheet) } }
        )) {
            if let sheet = core.view.thread?.pullRequestSheet {
                VStack(alignment: .leading) {
                    Text("Create pull request").font(.headline)
                    Form {
                        if let branch = sheet.prSheetBranch { Text("\(branch) → \(sheet.prSheetBase)") }
                        if let notice = sheet.prSheetNotice { Text(notice).foregroundStyle(.secondary) }
                        if let value = sheet.prSheetExistingUrl, let url = URL(string: value) {
                            Link("Open existing pull request", destination: url)
                        } else {
                            TextField("Title", text: field("title", \.prSheetTitle))
                            TextEditor(text: field("body", \.prSheetBody))
                                .accessibilityLabel("Description")
                                .frame(minHeight: 100)
                            if !sheet.prSheetBases.isEmpty {
                                Picker("Base branch", selection: field("base", \.prSheetBase)) {
                                    ForEach(sheet.prSheetBases, id: \.self) { Text($0).tag($0) }
                                }
                            }
                            if sheet.prSheetSupportsDraft {
                                Toggle("Draft", isOn: Binding(get: { core.view.thread?.pullRequestSheet?.prSheetDraft ?? false }, set: {
                                    core.update(.setPullRequestSheetField(field: "draft", value: $0 ? "true" : "false"))
                                }))
                            }
                            ForEach(sheet.prSheetFiles, id: \.prFilePath) { file in
                                Toggle(file.prFilePath, isOn: Binding(get: { file.prFileSelected }, set: { _ in
                                    core.update(.togglePullRequestFile(path: file.prFilePath))
                                }))
                            }
                        }
                    }
                    HStack {
                        Button("Close") { core.update(.closePullRequestSheet) }
                        Spacer()
                        if sheet.prSheetLoading { ProgressView().controlSize(.small) }
                        Button("Create") { core.update(.createPullRequest) }
                            .disabled(!sheet.prSheetCanCreate)
                    }
                }
                .padding()
                #if os(macOS)
                .frame(width: 560, height: 460)
                #endif
            }
        }
    }

    private func field(_ field: String, _ key: KeyPath<PullRequestSheetView, String>) -> Binding<String> {
        Binding(get: { core.view.thread?.pullRequestSheet?[keyPath: key] ?? "" },
                set: { core.update(.setPullRequestSheetField(field: field, value: $0)) })
    }
}

struct LinearProjectSettings: View {
    @ObservedObject var core: Core
    @State private var token = ""

    var body: some View {
        if let linear = core.view.settings.linearSettings {
            Section("Linear issue tracker") {
                if let account = linear.linearAccount { Text("Connected as \(account)") }
                if let notice = linear.linearNotice { Text(notice).foregroundStyle(.secondary) }
                SecureField("Personal API key", text: $token)
                Button(linear.linearConnected ? "Replace API key" : "Connect Linear") {
                    core.update(.connectIssueTracker(token: token))
                    token = ""
                }.disabled(token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                if linear.linearConnected {
                    Button("Disconnect Linear", role: .destructive) {
                        core.update(.connectIssueTracker(token: nil))
                    }
                }
                if !linear.linearTeams.isEmpty {
                    ForEach(core.view.settings.projects.filter { !$0.isScratch }, id: \.id) { project in
                        Picker(project.title, selection: Binding(get: { project.linearScope ?? "" }, set: { value in
                            core.update(.bindProjectIssueTracker(projectId: project.id, scope: value.isEmpty ? nil : value))
                        })) {
                            Text("No Linear team").tag("")
                            ForEach(linear.linearTeams, id: \.linearTeamKey) { team in
                                Text(team.linearTeamName).tag(team.linearTeamKey)
                            }
                        }
                    }
                }
            }
            .onChange(of: core.view.connection.activeEnvironmentId) { _, _ in token = "" }
        }
    }
}
