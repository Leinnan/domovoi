import App
import SwiftUI

/// A provider instance being edited, or added (`instanceId` empty).
struct ProviderEditing: Identifiable {
    let id: String
    var edit: ProviderInstanceEdit
    let methods: [ChoiceView]
    let builtIn: Bool
    let hasKey: Bool

    /// The editor of `provider`'s settings as they are.
    static func of(_ p: ProviderView) -> ProviderEditing {
        let c = p.config
        return ProviderEditing(
            id: p.instanceId,
            edit: ProviderInstanceEdit(instanceId: p.instanceId, driver: p.driver, displayName: c.displayName, enabled: p.enabled, authMethod: c.authMethod,
                                       apiKey: "", gcpProject: c.gcpProject, gcpLocation: c.gcpLocation, binaryPath: c.binaryPath),
            methods: c.authMethods, builtIn: c.builtIn, hasKey: c.hasApiKey)
    }

    /// A new Antigravity instance: an API key or Vertex (the Google sign-in is the built-in one's).
    static func newAntigravity() -> ProviderEditing {
        ProviderEditing(
            id: "new",
            edit: ProviderInstanceEdit(instanceId: "", driver: "antigravity", displayName: "", enabled: true, authMethod: "gemini-api-key",
                                       apiKey: "", gcpProject: "", gcpLocation: "", binaryPath: ""),
            methods: [ChoiceView(value: "gemini-api-key", label: "Gemini API key"), ChoiceView(value: "agent-platform", label: "Agent Platform (Vertex AI)")],
            builtIn: false, hasKey: false)
    }
}

/// A provider's sign-in, managed runtime and settings, inside its section of
/// the providers settings.
struct ProviderSetupRows: View {
    let provider: ProviderView
    let update: (Event) -> Void
    let edit: (ProviderEditing) -> Void
    @State private var redirect = ""

    var body: some View {
        let s = provider.signIn
        if provider.canAuthenticate && !provider.signedIn {
            if s.busy {
                if !s.url.isEmpty, let url = URL(string: s.url) {
                    Link(destination: url) { Label("Open Google Sign-In", systemImage: "safari") }
                }
                if s.takesRedirect {
                    // Google returns to the server's machine: from another device, paste where it went.
                    HStack {
                        TextField("Redirect URL from the browser", text: $redirect)
                            .autocorrectionDisabled()
                            #if os(iOS)
                            .textInputAutocapitalization(.never)
                            .keyboardType(.URL)
                            #endif
                        // Borderless: in a form row a plain button takes the whole row's tap.
                        Button("Paste") { if let text = Platform.pasteboardText() { redirect = text } }
                            .buttonStyle(.borderless)
                    }
                    Button("Continue") {
                        update(.completeProviderSignIn(instanceId: provider.instanceId, callbackUrl: redirect))
                        redirect = ""
                    }
                    .disabled(redirect.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                Button("Cancel Sign-In", role: .cancel) { update(.cancelProviderSignIn(instanceId: provider.instanceId)) }
            } else {
                Button(provider.config.authMethod.hasPrefix("oauth") ? "Sign In with Google" : "Connect") {
                    update(.startProviderSignIn(instanceId: provider.instanceId))
                }
            }
        }
        if !s.message.isEmpty && s.phase != "idle" {
            Text(s.message).font(TypeRole.meta).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        }
        if provider.canInstall { runtime }
        if provider.config.editable {
            Button("Edit Settings…") { edit(.of(provider)) }
        }
    }

    /// Domovoi's own install of the agent: offered when none is found, or to update the one it made.
    @ViewBuilder private var runtime: some View {
        let i = provider.install
        if i.busy {
            ProgressView(value: i.progress > 0 ? Double(i.progress) / 1000 : nil) {
                Text(i.message.isEmpty ? "Installing…" : i.message)
            } currentValueLabel: {
                Text(i.progressLabel)
            }
            Button("Cancel Install", role: .cancel) { update(.cancelProviderInstall(instanceId: provider.instanceId)) }
        } else {
            if !i.installedVersion.isEmpty {
                LabeledContent("Installed runtime", value: i.installedVersion)
            }
            if provider.version.isEmpty || i.updateAvailable {
                Button(i.updateAvailable ? "Update to \(i.offeredVersion)" : "Install Runtime") {
                    update(.installProvider(instanceId: provider.instanceId))
                }
            }
            if i.canRemove {
                Button("Remove Runtime", role: .destructive) { update(.removeProviderInstall(instanceId: provider.instanceId)) }
            }
            if !i.message.isEmpty && i.phase != "idle" && i.phase != "succeeded" {
                Text(i.message).font(TypeRole.meta).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// An Antigravity instance's settings: its sign-in method and credentials.
struct ProviderInstanceEditor: View {
    @State var editing: ProviderEditing
    let update: (Event) -> Void
    let done: () -> Void

    private var method: String { editing.edit.authMethod }
    private var usesKey: Bool { method == "gemini-api-key" || method == "agent-platform" }
    private var usesProject: Bool { method == "oauth-business" || method == "agent-platform" }
    private var isNew: Bool { editing.edit.instanceId.isEmpty }

    var body: some View {
        Form {
            if !editing.builtIn {
                TextField("Name", text: $editing.edit.displayName, prompt: Text("Antigravity (work)"))
            }
            Picker("Sign-in method", selection: $editing.edit.authMethod) {
                ForEach(editing.methods, id: \.value) { Text($0.label).tag($0.value) }
            }
            if usesKey {
                SecureField("API key", text: $editing.edit.apiKey, prompt: Text(editing.hasKey ? "Saved (type to replace)" : (method == "agent-platform" ? "Optional" : "Required")))
            }
            if usesProject {
                TextField("GCP project", text: $editing.edit.gcpProject, prompt: Text("my-project-id"))
                TextField("GCP location", text: $editing.edit.gcpLocation, prompt: Text("us-central1"))
            }
            TextField("Binary path", text: $editing.edit.binaryPath, prompt: Text("Automatic"))
            Toggle("Enabled", isOn: $editing.edit.enabled)
            if !isNew && !editing.builtIn {
                Button("Remove This Instance", role: .destructive) {
                    update(.removeProviderInstance(instanceId: editing.edit.instanceId))
                    done()
                }
            }
        }
        .formStyle(.grouped)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: done) }
            ToolbarItem(placement: .confirmationAction) {
                Button(isNew ? "Add" : "Save") {
                    update(.saveProviderInstance(edit: editing.edit))
                    done()
                }
                .disabled(isNew && method == "gemini-api-key" && editing.edit.apiKey.isEmpty)
            }
        }
    }
}
