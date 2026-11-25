import App
import SwiftUI

/// Secrets the agent asked for (`request_secret`), each a card with a
/// `SecureField`, Save and Decline, above the composer beside the approvals.
///
/// What is typed lives in the card's own `@State` until it is sent: it is
/// handed to the core once (`AnswerSecret`), which sends it and keeps it
/// nowhere, and the field is cleared at once. Nothing typed is saved,
/// restored or logged.
struct SecretRequestArea: View {
    @ObservedObject var core: Core
    let requests: [SecretRequestView]

    var body: some View {
        VStack(spacing: Space.s) {
            ForEach(requests, id: \.itemId) { request in
                SecretRequestCard(core: core, request: request)
            }
        }
        .padding(.horizontal, Space.s)
        .padding(.top, Space.xs)
        .onChange(of: requests.map(\.itemId), initial: true) { old, ids in
            guard ids.contains(where: { !old.contains($0) }) else { return }
            Platform.needsAttention()
            AccessibilityNotification.Announcement("The agent asks for a secret").post()
        }
    }
}

struct SecretRequestCard: View {
    @ObservedObject var core: Core
    let request: SecretRequestView
    @State private var value = ""
    @FocusState private var focused: Bool

    private var canSave: Bool { !request.sending && !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            HStack(alignment: .firstTextBaseline, spacing: Space.m) {
                Image(systemName: "key.fill").foregroundStyle(Tint.caution).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Space.xxs) {
                    Text("The agent asks for \(request.label)").font(TypeRole.cardTitle)
                    if !request.reason.isEmpty {
                        Text(request.reason).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
                    }
                }
                Spacer(minLength: 0)
                if let url = request.url.flatMap(URL.init(string:)) {
                    Link("Get One", destination: url).font(TypeRole.meta.weight(.semibold))
                }
            }
            SecureField(request.placeholder.isEmpty ? "Secret value" : request.placeholder, text: $value)
                .textFieldStyle(.roundedBorder)
                .textContentType(.password)
                #if os(iOS)
                .textInputAutocapitalization(.never)
                #endif
                .autocorrectionDisabled()
                .focused($focused)
                .disabled(request.sending)
                .onSubmit(save)
            Text(request.hint).font(TypeRole.meta).foregroundStyle(.secondary)
            HStack(spacing: Space.s) {
                Spacer()
                Button("Decline") {
                    value = ""
                    core.update(.declineSecret(itemId: request.itemId))
                }
                .glassButton()
                .disabled(request.sending)
                Button(request.sending ? "Saving…" : "Save Secret", action: save)
                    .glassButton(prominent: true)
                    .disabled(!canSave)
            }
        }
        .padding(Space.l)
        .cardSurface(tint: Tint.caution.opacity(0.12))
        .onDisappear { value = "" }
    }

    private func save() {
        guard canSave else { return }
        let secret = value
        value = ""
        core.update(.answerSecret(itemId: request.itemId, value: secret))
    }
}

/// The pull requests and issues linked to a thread, as chips that open in
/// the browser.
struct WorkLinksStrip: View {
    let links: [WorkLinkView]

    var body: some View {
        if !links.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: Space.s) {
                    ForEach(links, id: \.url) { link in
                        if let url = URL(string: link.url) {
                            Link(destination: url) { chip(link) }
                                .buttonStyle(.plain)
                                .help(link.kind == "pull_request" ? "Open the pull request in the browser" : "Open the issue in the browser")
                        }
                    }
                }
                .padding(.horizontal, Space.gutter)
                .padding(.vertical, Space.xs)
            }
        }
    }

    private func chip(_ link: WorkLinkView) -> some View {
        HStack(spacing: Space.xs) {
            Image(systemName: link.kind == "pull_request" ? "arrow.triangle.pull" : "smallcircle.filled.circle")
                .foregroundStyle(.secondary)
            Text(link.label).font(TypeRole.meta.weight(.semibold))
            if !link.title.isEmpty {
                Text(link.title).font(TypeRole.meta).foregroundStyle(.secondary).lineLimit(1)
            }
            if !link.state.isEmpty {
                Text(link.state).font(TypeRole.badge).foregroundStyle(stateTint(link.state))
            }
        }
        .padding(.horizontal, Space.s)
        .padding(.vertical, Space.xs)
        .background(.fill.quaternary, in: Capsule())
        .accessibilityElement(children: .combine)
    }

    private func stateTint(_ state: String) -> Color {
        switch state {
        case "merged": Tint.plan
        case "closed": Tint.danger
        case "draft": .secondary
        default: Tint.success
        }
    }
}
