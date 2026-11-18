import App
import SwiftUI

/// Model, effort, permissions and plan mode, with room to explain each.
/// The sections are the shared ones in `ComposerModes.swift`.
struct ComposerOptionsSheet: View {
    @ObservedObject var core: Core
    let focus: ComposerOption
    @Environment(\.dismiss) private var dismiss
    @State private var confirmFullAccess = false

    private var composer: ComposerView { core.view.composer }

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: Space.xl) {
                        WorkingNote(core: core)
                        if composer.showsInteractionMode || composer.interactionMode == "plan" {
                            PlanCard(on: composer.interactionMode == "plan") {
                                core.update(.setInteractionMode(mode: composer.interactionMode == "plan" ? "default" : "plan"))
                            }
                            .id(ComposerOption.plan)
                        }
                        if !composer.efforts.isEmpty {
                            EffortPicker(composer: composer) { core.update(.selectEffort(value: $0)) }
                                .id(ComposerOption.effort)
                        }
                        PermissionCards(composer: composer) { mode in
                            if mode == "full-access" {
                                confirmFullAccess = true
                            } else {
                                core.update(.setRuntimeMode(mode: mode))
                            }
                        }
                        .id(ComposerOption.permissions)
                        ModelList(composer: composer, update: { core.update($0) }) {
                            core.update(.selectModel(instanceId: $0.instanceId, model: $0.model))
                            // Opened from the model chip: picking one is the whole errand.
                            if focus == .model { dismiss() }
                        }
                            .id(ComposerOption.model)
                    }
                    .padding(.horizontal, Space.l)
                    .padding(.vertical, Space.m)
                }
                .onAppear { proxy.scrollTo(focus, anchor: .top) }
            }
            .navigationTitle("Options")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", systemImage: "checkmark") { dismiss() }
                }
            }
            .confirmationDialog(ModeInfo.fullAccessTitle, isPresented: $confirmFullAccess, titleVisibility: .visible) {
                Button("Allow Full Access", role: .destructive) { core.update(.setRuntimeMode(mode: "full-access")) }
            } message: {
                Text(ModeInfo.fullAccessMessage)
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}
