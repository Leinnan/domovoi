import App
import SwiftUI

/// Evidence-checked AI PR review body shared by macOS (`GroupBox`) and iOS (`Section`).
struct AiCodeReviewBody: View {
    @ObservedObject var core: Core
    let detail: PullRequestDetailView
    let busy: Bool

    private var review: PrReviewView? { detail.aiReview }
    private var isBusy: Bool { busy || (review?.busy ?? false) }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            HStack(spacing: Space.s) {
                if let rev = review, rev.busy, !rev.runId.isEmpty {
                    ProgressView().controlSize(.small)
                    Text(rev.phase.isEmpty ? rev.status : "\(rev.status) · \(rev.phase)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel", role: .destructive) {
                        core.update(.cancelPrReview(runId: rev.runId))
                    }
                    .controlSize(.small)
                } else {
                    Button("Run AI Review") {
                        core.update(.startPrReview(key: detail.key, full: false))
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(isBusy)
                    Button("Full Review") {
                        core.update(.startPrReview(key: detail.key, full: true))
                    }
                    .controlSize(.small)
                    .disabled(isBusy)
                    Spacer()
                }
            }

            if let rev = review {
                HStack(spacing: Space.s) {
                    Text(rev.status.capitalized)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(statusColor(rev.status))
                    if !rev.phase.isEmpty {
                        Text(rev.phase).font(.caption).foregroundStyle(.secondary)
                    }
                    if !rev.scopeMode.isEmpty {
                        Text(rev.scopeMode)
                            .font(.caption2)
                            .padding(.horizontal, Space.xs)
                            .padding(.vertical, 1)
                            .background(.quaternary, in: Capsule())
                    }
                    if !rev.headSha.isEmpty {
                        Text(String(rev.headSha.prefix(8)))
                            .font(TypeRole.monoSmall)
                            .foregroundStyle(.secondary)
                    }
                    if rev.riskScore > 0 {
                        Text("Risk \(rev.riskScore)/100")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(rev.riskScore >= 70 ? Tint.danger : (rev.riskScore >= 40 ? Tint.caution : Tint.success))
                    }
                }

                if let msg = rev.phaseMessage, !msg.isEmpty {
                    Text(msg).font(.caption).foregroundStyle(.secondary)
                }
                if rev.isStale {
                    Label("PR head changed since this review; re-run before publishing.", systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(Tint.caution)
                }
                if let err = rev.error, !err.isEmpty {
                    Label(err, systemImage: "xmark.octagon.fill")
                        .font(.caption)
                        .foregroundStyle(Tint.danger)
                }
                if let pubId = rev.publishedReviewId, !pubId.isEmpty {
                    Label("Published to remote review (\(pubId))", systemImage: "checkmark.circle.fill")
                        .font(.caption)
                        .foregroundStyle(Tint.success)
                }
                if !rev.riskDrivers.isEmpty {
                    VStack(alignment: .leading, spacing: Space.xxs) {
                        ForEach(Array(rev.riskDrivers.enumerated()), id: \.offset) { _, drv in
                            Text("+\(drv.points) \(drv.label) — \(drv.detail)")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                if !rev.summary.isEmpty {
                    Text(rev.summary).font(.callout)
                }

                ForEach(rev.findings, id: \.id) { f in
                    VStack(alignment: .leading, spacing: Space.xxs) {
                        HStack(alignment: .firstTextBaseline, spacing: Space.xs) {
                            Button {
                                core.update(.togglePrFinding(runId: rev.runId, findingId: f.id, selected: !f.selected))
                            } label: {
                                Image(systemName: f.selected ? "checkmark.square.fill" : "square")
                                    .foregroundStyle(f.selected ? Color.accentColor : .secondary)
                            }
                            .buttonStyle(.plain)
                            .disabled(rev.busy)
                            Text(f.severity.uppercased())
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(f.severity == "high" ? Tint.danger : (f.severity == "medium" ? Tint.caution : .secondary))
                            Text(f.kind)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                            Text(f.title)
                                .font(.callout.weight(.medium))
                            Spacer()
                            if !f.sourceStage.isEmpty {
                                Text(f.sourceStage).font(.caption2).foregroundStyle(.tertiary)
                            }
                        }
                        if !f.path.isEmpty {
                            Text("\(f.path):\(f.line) (\(f.side))")
                                .font(TypeRole.monoSmall)
                                .foregroundStyle(.secondary)
                        }
                        if !f.detail.isEmpty {
                            Text(f.detail).font(.caption)
                        }
                        if !f.action.isEmpty {
                            Text("Suggested fix: \(f.action)").font(.caption).foregroundStyle(.secondary)
                        }
                        ForEach(Array(f.evidenceQuotes.enumerated()), id: \.offset) { _, quote in
                            Text(quote)
                                .font(TypeRole.monoSmall)
                                .padding(Space.xs)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 4))
                        }
                    }
                    .padding(Space.s)
                    .background(.quaternary.opacity(0.25), in: RoundedRectangle(cornerRadius: Radius.chip))
                }

                if !rev.findings.isEmpty || rev.canPublish {
                    HStack {
                        Text("\(rev.selectedCount) of \(rev.findings.count) selected for publication")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("Publish Selected") {
                            core.update(.publishPrReview(key: detail.key, runId: rev.runId))
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(!rev.canPublish || rev.busy)
                    }
                }
            } else {
                Text("Runs an evidence-checked review in a read-only snapshot. Findings stay local until you publish selected items; never auto-approves.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "validated", "published": Tint.success
        case "failed": Tint.danger
        case "running", "queued": .accentColor
        default: .secondary
        }
    }
}

/// Failover pools and live provider availability shared across macOS & iOS Providers settings.
struct FailoverPoolsBody: View {
    @ObservedObject var core: Core

    private var settings: SettingsView { core.view.settings }

    var body: some View {
        if settings.failoverInstances.isEmpty {
            Text("No live availability data yet.")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            ForEach(settings.failoverInstances, id: \.instanceId) { inst in
                HStack(spacing: Space.s) {
                    Circle()
                        .fill(inst.available ? Tint.success : Tint.caution)
                        .frame(width: 7, height: 7)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(inst.displayName.isEmpty ? inst.instanceId : inst.displayName)
                            .font(.callout.weight(.medium))
                        Text(instanceDetail(inst))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                }
            }
        }

        if settings.failoverPools.isEmpty && !settings.providers.isEmpty {
            Button("Create Default Failover Pool") {
                let ids = settings.providers.map(\.instanceId)
                core.update(.saveFailoverPool(
                    poolId: "default",
                    name: "Default Pool",
                    instanceIds: ids,
                    sameDriverFirst: true,
                    allowCrossProvider: true
                ))
            }
        }

        ForEach(settings.failoverPools, id: \.id) { pool in
            VStack(alignment: .leading, spacing: Space.xs) {
                HStack {
                    Text("\(pool.name) (\(pool.id))").font(.callout.weight(.semibold))
                    Spacer()
                    if pool.instanceIds.count > 1 {
                        Button("Rotate") {
                            var rotated = pool.instanceIds
                            let first = rotated.removeFirst()
                            rotated.append(first)
                            core.update(.saveFailoverPool(
                                poolId: pool.id,
                                name: pool.name,
                                instanceIds: rotated,
                                sameDriverFirst: pool.sameDriverFirst,
                                allowCrossProvider: pool.allowCrossProvider
                            ))
                        }
                        .controlSize(.small)
                    }
                }
                Text(pool.instanceIds.joined(separator: " → "))
                    .font(TypeRole.monoSmall)
                    .foregroundStyle(.secondary)
                Toggle("Same driver first", isOn: Binding(
                    get: { pool.sameDriverFirst },
                    set: { value in
                        core.update(.saveFailoverPool(
                            poolId: pool.id,
                            name: pool.name,
                            instanceIds: pool.instanceIds,
                            sameDriverFirst: value,
                            allowCrossProvider: pool.allowCrossProvider
                        ))
                    }
                ))
                Toggle("Allow cross-provider failover", isOn: Binding(
                    get: { pool.allowCrossProvider },
                    set: { value in
                        core.update(.saveFailoverPool(
                            poolId: pool.id,
                            name: pool.name,
                            instanceIds: pool.instanceIds,
                            sameDriverFirst: pool.sameDriverFirst,
                            allowCrossProvider: value
                        ))
                    }
                ))
            }
            .padding(.vertical, Space.xxs)
        }
    }

    private func instanceDetail(_ inst: FailoverInstanceView) -> String {
        var parts = [inst.driver, inst.available ? "available" : "rate-limited"]
        if inst.activeJobs > 0 { parts.append("\(inst.activeJobs) active") }
        if let until = inst.limitedUntil, !until.isEmpty { parts.append("until \(until)") }
        if let err = inst.lastError, !err.isEmpty { parts.append(err) }
        return parts.joined(separator: " · ")
    }
}

/// Codebase index summary and Reindex action shown under each project in Settings -> Projects.
struct ProjectIndexRow: View {
    @ObservedObject var core: Core
    let project: ProjectSettingsView

    private var idx: ProjectIndexView { project.index }

    var body: some View {
        HStack(spacing: Space.s) {
            VStack(alignment: .leading, spacing: 2) {
                Text(summaryLine)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if !idx.conventionPaths.isEmpty {
                    Text("Conventions: \(idx.conventionPaths.joined(separator: ", "))")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                if let err = idx.error, !err.isEmpty {
                    Text(err).font(.caption2).foregroundStyle(Tint.danger)
                }
            }
            Spacer(minLength: Space.xs)
            Button(idx.status == "indexing" ? "Indexing…" : "Reindex") {
                core.update(.reindexProject(projectId: project.id))
            }
            .controlSize(.small)
            .disabled(idx.status == "indexing")
        }
    }

    private var summaryLine: String {
        var parts = [
            "Index: \(idx.status)",
            "\(idx.fileCount) files",
            "\(idx.packageCount) pkgs",
            "\(idx.edgeCount) edges",
            "\(idx.symbolCount) symbols",
        ]
        if !idx.commitSha.isEmpty { parts.append(String(idx.commitSha.prefix(8))) }
        if !idx.indexedAt.isEmpty { parts.append(idx.indexedAt) }
        return parts.joined(separator: " · ")
    }
}
