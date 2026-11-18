import AVFoundation
import Speech
import SwiftUI

/// Dictation into the composer with Apple's speech recognition, on the
/// iPhone when it can (nothing leaves the device then). `start` hands each
/// growing transcript to `onText`; `stop` ends it.
@MainActor
final class Dictation: ObservableObject {
    @Published private(set) var listening = false
    @Published var error: String?
    /// The error is refused access, which only Settings can change.
    @Published private(set) var needsSettings = false
    private var session: SpeechSession?

    /// A start is under way (asking for access, starting the engine): a
    /// second tap meanwhile must not start another session on the mic.
    private var starting = false

    func toggle(onText: @escaping @MainActor (String) -> Void) {
        if listening { stop() } else { Task { await start(onText: onText) } }
    }

    func start(onText: @escaping @MainActor (String) -> Void) async {
        guard !starting, !listening else { return }
        starting = true
        defer { starting = false }
        error = nil
        needsSettings = false
        guard await Self.authorized() else {
            error = "Dictation needs microphone and speech recognition access."
            needsSettings = true
            return
        }
        let recognizer = SFSpeechRecognizer(locale: .current) ?? SFSpeechRecognizer(locale: Locale(identifier: "en-US"))
        guard let recognizer, recognizer.isAvailable else {
            error = "Speech recognition isn't available right now."
            return
        }
        let session = SpeechSession(recognizer: recognizer)
        do {
            try session.start(
                onText: { text in Task { @MainActor in onText(text) } },
                onEnd: { [weak self] message in
                    Task { @MainActor in
                        guard let self, self.session === session else { return }
                        if let message { self.error = message }
                        self.finish()
                    }
                }
            )
        } catch {
            self.error = "Couldn't start dictation: \(error.localizedDescription)"
            session.stop()
            return
        }
        self.session = session
        listening = true
        Platform.selectionChanged()
    }

    func stop() {
        session?.stop()
        finish()
    }

    private func finish() {
        session = nil
        if listening { Platform.selectionChanged() }
        listening = false
    }

    private static func authorized() async -> Bool {
        let speech: Bool = await withCheckedContinuation { done in
            SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0 == .authorized) }
        }
        guard speech else { return false }
        return await AVAudioApplication.requestRecordPermission()
    }
}

/// The audio engine and recognition task of one dictation. Their callbacks
/// arrive on audio and recognition threads; this only forwards them.
private final class SpeechSession: @unchecked Sendable {
    private let recognizer: SFSpeechRecognizer
    private let engine = AVAudioEngine()
    private let request = SFSpeechAudioBufferRecognitionRequest()
    private var task: SFSpeechRecognitionTask?
    private let lock = NSLock()
    private var stopped = false

    init(recognizer: SFSpeechRecognizer) {
        self.recognizer = recognizer
    }

    func start(onText: @escaping @Sendable (String) -> Void, onEnd: @escaping @Sendable (String?) -> Void) throws {
        let audio = AVAudioSession.sharedInstance()
        try audio.setCategory(.record, mode: .measurement, options: .duckOthers)
        try audio.setActive(true, options: .notifyOthersOnDeactivation)
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        let input = engine.inputNode
        let request = request
        // No input to record (a call, a route without a mic, a Simulator
        // without one): installing a tap on it raises an Objective-C
        // exception, which no `catch` stops.
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw NSError(domain: "Dictation", code: 1, userInfo: [NSLocalizedDescriptionKey: "No microphone input is available."])
        }
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            request.append(buffer)
        }
        engine.prepare()
        try engine.start()
        task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            if let result {
                onText(result.bestTranscription.formattedString)
                if result.isFinal {
                    self?.stop()
                    onEnd(nil)
                    return
                }
            }
            if let error = error as NSError? {
                self?.stop()
                // Stopping by hand ends the task with "no speech" or a cancel: not worth saying.
                let quiet = error.domain == "kAFAssistantErrorDomain" && [203, 216, 1110].contains(error.code)
                onEnd(quiet ? nil : "Dictation stopped: \(error.localizedDescription)")
            }
        }
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        guard !stopped else { return }
        stopped = true
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        request.endAudio()
        task?.finish()
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
}
