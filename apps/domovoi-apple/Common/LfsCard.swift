import App
import SwiftUI

/// A file kept in Git LFS, over its preview: whether the server has its
/// object, the object's size and id, and Fetch when it isn't there.
struct LfsCard: View {
    let lfs: LfsView
    let fetch: () -> Void

    private var tint: Color {
        switch lfs.state {
        case "cached": Tint.success
        case "missing": Tint.caution
        default: Tint.danger
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            HStack(spacing: Space.s) {
                Text("Git LFS").badgeCapsule(tint: .accentColor)
                Text(lfs.stateLabel).font(TypeRole.meta.weight(.medium)).foregroundStyle(tint)
                Text("\(lfs.sizeLabel) · \(lfs.oidShort)")
                    .font(TypeRole.monoSmall)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .help(lfs.oid)
                Spacer(minLength: 0)
                if lfs.fetching {
                    ProgressView().controlSize(.small)
                    Text("Fetching…").font(TypeRole.meta).foregroundStyle(.secondary)
                } else if lfs.canFetch {
                    Button("Fetch", systemImage: "arrow.down.circle", action: fetch)
                        .help("Download this file's object from the repository's Git LFS remote onto the server")
                }
            }
            if let message = lfs.message {
                Text(message).font(TypeRole.meta).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Space.s)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
    }
}
