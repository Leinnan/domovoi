import App
import AVFoundation
import SwiftUI
import VisionKit

/// Pairing: scan the server's QR code, paste its link, or enter a local
/// address and code. Also lists saved servers.
struct AddServerView: View {
    @ObservedObject var core: Core
    var onboarding = false
    @Environment(\.dismiss) private var dismiss
    @State private var scanning = false
    @State private var link = ""
    @State private var serverURL = ""
    @State private var pairingCode = ""
    @State private var forgetting: EnvironmentView?
    /// Camera permission, read again when the app comes back (from Settings).
    @State private var camera = AVCaptureDevice.authorizationStatus(for: .video)
    @Environment(\.scenePhase) private var scenePhase

    private var connection: ConnectionView { core.view.connection }
    private var busy: Bool { ["pairing", "connecting"].contains(connection.state) }
    /// Not asked yet counts as possible: the first tap asks.
    private var canScan: Bool {
        guard DataScannerViewController.isSupported else { return false }
        switch camera {
        case .notDetermined: return true
        case .authorized: return DataScannerViewController.isAvailable
        default: return false
        }
    }
    /// Why scanning is unavailable, when it is.
    private var scanProblem: String? {
        if !DataScannerViewController.isSupported { return "This device cannot scan codes here. Paste the link instead." }
        if camera == .denied || camera == .restricted { return "Domovoi may not use the camera. Allow it in Settings, or paste the link." }
        return nil
    }

    private func scan() {
        guard camera == .notDetermined else {
            scanning = true
            return
        }
        Task {
            let granted = await AVCaptureDevice.requestAccess(for: .video)
            camera = AVCaptureDevice.authorizationStatus(for: .video)
            if granted { scanning = true }
        }
    }

    var body: some View {
        Form {
            if onboarding {
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Image(systemName: "terminal.fill").font(.system(size: 40)).foregroundStyle(.tint)
                        Text("Domovoi").font(.largeTitle.bold())
                        Text("Your coding agents, from anywhere. Pair this device with domovoi-server on your Mac.")
                            .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 8)
                }
            }
            Section {
                Button(action: scan) {
                    Label("Scan Pairing QR Code", systemImage: "qrcode.viewfinder")
                }
                .disabled(!canScan || busy)
                if let scanProblem {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(scanProblem).font(.footnote).foregroundStyle(.secondary)
                        if camera == .denied, let settings = URL(string: UIApplication.openSettingsURLString) {
                            Link("Open Settings", destination: settings).font(.footnote)
                        }
                    }
                }
                HStack {
                    TextField("domovoi://connect?…", text: $link)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.footnote.monospaced())
                        .submitLabel(.go)
                        .onSubmit { if !link.isEmpty { connectRemote(link) } }
                    // No paste prompt with PasteButton; a pairing link
                    // connects straight away.
                    PasteButton(payloadType: String.self) { strings in
                        guard let pasted = strings.first?.trimmingCharacters(in: .whitespacesAndNewlines) else { return }
                        link = pasted
                        if pasted.hasPrefix("domovoi://") { connectRemote(pasted) }
                    }
                    .labelStyle(.iconOnly)
                    .buttonBorderShape(.capsule)
                }
                Button("Connect") { connectRemote(link) }
                    .disabled(link.isEmpty || busy)
            } header: {
                Text("Remote server")
            } footer: {
                Text("On the Mac: Settings → Access in Domovoi, or `domovoi-server pair --remote`. Works over any network; traffic is end-to-end encrypted.")
            }
            Section {
                TextField("Server address", text: $serverURL, prompt: Text(verbatim: "http://192.168.1.10:13773"))
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                TextField("Pairing code", text: $pairingCode)
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                Button("Connect") {
                    core.update(.connect(serverUrl: serverURL, pairingCode: pairingCode))
                }
                .disabled(serverURL.isEmpty || busy)
            } header: {
                Text("Local network address")
            } footer: {
                Text("domovoi-server prints its address and a one-time code when it starts; `domovoi-server pair` makes a new code.")
            }
            // Right under the Connect buttons, not below the saved servers.
            if busy || connection.error != nil {
                Section {
                    if busy {
                        HStack {
                            ProgressView()
                            Text(connection.state == "pairing" ? "Pairing…" : "Connecting…").foregroundStyle(.secondary)
                        }
                    }
                    if let error = connection.error, !busy {
                        Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(Tint.danger)
                    }
                }
            }
            if !connection.environments.isEmpty {
                Section("Saved servers") {
                    ForEach(connection.environments, id: \.id) { env in
                        Button {
                            core.update(.selectEnvironment(id: env.id))
                        } label: {
                            EnvironmentRow(env: env)
                        }
                        .swipeActions {
                            Button("Forget", role: .destructive) { forgetting = env }
                        }
                        .accessibilityHint(env.active ? "The server in use" : "Connects to this server")
                    }
                }
            }
        }
        .navigationTitle(onboarding ? "" : "Add Server")
        .sheet(isPresented: $scanning) {
            ScannerSheet { scanned in
                scanning = false
                connectRemote(scanned)
            }
        }
        .onChange(of: connection.state) { _, state in
            guard state == "connected" else { return }
            // What was typed stays until it has worked.
            link = ""
            pairingCode = ""
            if !onboarding { dismiss() }
        }
        .sensoryFeedback(trigger: connection.state) { old, new in
            if new == "connected", old != "connected" { return .success }
            if new == "failed" || (["pairing", "connecting"].contains(old) && connection.error != nil) { return .error }
            return nil
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { camera = AVCaptureDevice.authorizationStatus(for: .video) }
        }
        .confirmationDialog(
            forgetting.map { "Forget \($0.label)?" } ?? "",
            isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
            titleVisibility: .visible,
            presenting: forgetting
        ) { env in
            Button("Forget", role: .destructive) { core.update(.forgetEnvironment(id: env.id)) }
        } message: { _ in
            Text("This device signs out of the server and removes it from the list. Pair again to come back.")
        }
    }

    private func connectRemote(_ text: String) {
        core.update(.connectRemote(link: text))
    }
}

struct EnvironmentRow: View {
    let env: EnvironmentView

    var body: some View {
        HStack {
            Image(systemName: env.remote ? "globe" : "desktopcomputer").foregroundStyle(.secondary).frame(width: 24)
            VStack(alignment: .leading) {
                Text(env.label).foregroundStyle(.primary)
                Text(env.detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            if env.active { Image(systemName: "checkmark").foregroundStyle(.tint) }
        }
    }
}

/// The scanner with a way out and a word about codes that are not Domovoi's.
struct ScannerSheet: View {
    let found: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var notice: String?

    var body: some View {
        NavigationStack {
            QRScanner(found: { link in
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                found(link)
            }, other: { notice = "That code is not a Domovoi pairing link." }, failed: { notice = $0 })
                .ignoresSafeArea(edges: .bottom)
                .overlay(alignment: .top) {
                    Text("On your Mac: Domovoi → Settings → Access shows the code.")
                        .font(.footnote)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(.regularMaterial, in: Capsule())
                        .padding(.top, Space.m)
                }
                .overlay(alignment: .bottom) {
                    if let notice {
                        Text(notice)
                            .font(.callout)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 10)
                            .background(.regularMaterial, in: Capsule())
                            .padding(.bottom, 32)
                            .transition(.opacity)
                    }
                }
                // A wrong code's notice goes after a moment (another one
                // brings it back).
                .task(id: notice) {
                    guard notice != nil else { return }
                    try? await Task.sleep(for: .seconds(3))
                    withAnimation { notice = nil }
                }
                .navigationTitle("Scan Pairing Code")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                }
        }
    }
}

/// VisionKit's live scanner, reporting the first `domovoi://` QR code it sees.
struct QRScanner: UIViewControllerRepresentable {
    let found: (String) -> Void
    /// A code that is not a Domovoi link was seen.
    let other: () -> Void
    /// The camera could not start.
    let failed: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            isHighlightingEnabled: true
        )
        scanner.delegate = context.coordinator
        do {
            try scanner.startScanning()
        } catch {
            DispatchQueue.main.async { failed("The camera could not start: \(error.localizedDescription)") }
        }
        return scanner
    }

    func updateUIViewController(_ controller: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(found: found, other: other) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let found: (String) -> Void
        let other: () -> Void
        private var done = false

        init(found: @escaping (String) -> Void, other: @escaping () -> Void) {
            self.found = found
            self.other = other
        }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in items {
                guard !done, case .barcode(let code) = item, let text = code.payloadStringValue else { continue }
                guard text.hasPrefix("domovoi://") else {
                    other()
                    continue
                }
                done = true
                scanner.stopScanning()
                found(text)
            }
        }
    }
}
