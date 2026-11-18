import App
import SwiftUI

/// Above the composer when the selected provider can't run on the server
/// (not installed, not signed in): what's wrong, and a way to look again.
struct ProviderNotice: View {
    @ObservedObject var core: Core

    var body: some View {
        let reason = core.view.composer.blockedReason
        let hint = core.view.composer.sendHint
        // While the connection banner is up it says this already.
        if reason.isEmpty, !hint.isEmpty, !ConnectionBanner.showing(core) {
            Label(hint, systemImage: core.view.composer.sending ? "hourglass" : "bolt.horizontal.circle")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        if !reason.isEmpty {
            HStack(alignment: .firstTextBaseline, spacing: Space.s) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Tint.caution)
                Text(reason)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button("Check Again") { core.update(.refreshProviders) }
                    .font(.footnote.weight(.medium))
                    .buttonStyle(.borderless)
            }
            .padding(Space.s + Space.xxs)
            .background(Tint.caution.opacity(0.12), in: RoundedRectangle(cornerRadius: Radius.chip + 2, style: .continuous))
        }
    }
}
