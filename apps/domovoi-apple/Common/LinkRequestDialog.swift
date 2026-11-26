import App
import SwiftUI

extension View {
    /// Asks before a `domovoi://` link from outside the app connects to
    /// anything: the core holds the link (`ConnectionView.linkRequest`)
    /// until the user answers here.
    func linkRequestDialog(core: Core) -> some View {
        modifier(LinkRequestDialog(core: core))
    }
}

private struct LinkRequestDialog: ViewModifier {
    @ObservedObject var core: Core
    /// The request answered here: its sheet closes at once, a moment before
    /// the core has taken the answer.
    @State private var answered: UInt32?

    func body(content: Content) -> some View {
        let waiting = core.view.connection.linkRequest
        let paletteOpen = core.view.palette != nil
        // The latest request is the one shown: a newer one replaces it in
        // the same sheet. One sheet at a time, so it waits for the palette
        // to close (asked for below).
        let request = waiting?.id == answered || paletteOpen ? nil : waiting
        content
            .sheet(isPresented: Binding(
                get: { request != nil },
                // Closed any other way (Escape, a swipe down): Cancel.
                set: { if !$0, let request { answer(request, accept: false) } }
            )) {
                if let request {
                    LinkRequestSheet(request: request) { answer(request, accept: $0) }
                }
            }
            .onChange(of: waiting?.id) { _, id in
                if id != answered { answered = nil }
            }
            // The palette is a sheet too: it makes way for the question.
            .onChange(of: waiting != nil && paletteOpen, initial: true) { _, blocked in
                if blocked { core.update(.closePalette) }
            }
    }

    private func answer(_ request: LinkRequestView, accept: Bool) {
        answered = request.id
        core.update(.answerLinkRequest(id: request.id, accept: accept))
    }
}

/// The question: what the link asks for, what identifies the server it
/// leads to, and the two answers.
private struct LinkRequestSheet: View {
    let request: LinkRequestView
    let answer: (Bool) -> Void

    var body: some View {
        #if os(macOS)
        VStack(alignment: .leading, spacing: Space.l) {
            question
            HStack(spacing: Space.s) {
                Spacer()
                cancel.keyboardShortcut(.cancelAction)
                // Not the default button: Return typed as the sheet comes up
                // must not accept a link the user has not read.
                confirm
            }
        }
        .padding(Space.xl)
        .frame(width: 480)
        #else
        ScrollView {
            VStack(alignment: .leading, spacing: Space.l) {
                question
                VStack(spacing: Space.s) {
                    confirm
                    cancel
                }
                .controlSize(.large)
            }
            .padding(Space.xl)
        }
        .scrollBounceBehavior(.basedOnSize)
        .presentationDetents([.medium, .large])
        #endif
    }

    @ViewBuilder private var question: some View {
        Text(request.title).font(.title2.bold())
        Text(request.message)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: Space.m, verticalSpacing: Space.s) {
            GridRow {
                Text("Server key").foregroundStyle(.secondary)
                Text("\(request.endpoint)…")
                    .font(TypeRole.mono)
                    .textSelection(.enabled)
            }
            // The name this device gave the server, when it is a saved one;
            // otherwise the name the link's author wrote, if any.
            if !request.knownLabel.isEmpty {
                GridRow {
                    Text("Saved here as").foregroundStyle(.secondary)
                    Text(request.knownLabel).lineLimit(3)
                }
            } else if !request.linkName.isEmpty {
                GridRow {
                    Text("Name in the link").foregroundStyle(.secondary)
                    Text(request.linkName).lineLimit(3)
                }
            }
        }
    }

    private var cancel: some View {
        Button(role: .cancel) { answer(false) } label: {
            Text("Cancel").frame(maxWidth: fillsWidth ? .infinity : nil)
        }
        .buttonStyle(.bordered)
    }

    private var confirm: some View {
        Button { answer(true) } label: {
            Text(request.confirmLabel).frame(maxWidth: fillsWidth ? .infinity : nil)
        }
        .buttonStyle(.borderedProminent)
    }

    /// The phone's buttons are as wide as the sheet, one above the other.
    private var fillsWidth: Bool {
        #if os(macOS)
        false
        #else
        true
        #endif
    }
}
