import AVFoundation
import Foundation
import Speech

/// Speech turned into words on this device, for the message field.
///
/// Nothing leaves the device: where the system cannot recognise the person's language by
/// itself, dictation is not offered.
@MainActor
public final class Dictation {
    public enum Problem: Error, Sendable, Equatable {
        /// This device does not recognise this language by itself, or has no microphone.
        case unavailable
        /// The person has not allowed the microphone or speech recognition.
        case notAllowed
    }

    /// Called with everything heard so far, each time it grows.
    public var heard: ((String) -> Void)?
    /// Called once when listening ends by itself (a long pause, an error).
    public var ended: (() -> Void)?
    public private(set) var words = ""
    public private(set) var listening = false

    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?

    public init() {}

    public func start() async throws(Problem) {
        guard !listening else { return }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable, recognizer.supportsOnDeviceRecognition
        else { throw .unavailable }
        guard await Dictation.allowed() else { throw .notAllowed }
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            throw .unavailable
        }
        #endif
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw .unavailable }
        Dictation.feed(input, format: format, into: request)
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw .unavailable
        }
        words = ""
        listening = true
        self.request = request
        task = Dictation.recognise(with: recognizer, request) { [weak self] said, over in
            Task { @MainActor in self?.took(said, over: over) }
        }
    }

    /// Stop listening. What was heard stays in `words`.
    public func stop() {
        guard listening else { return }
        listening = false
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        request?.endAudio()
        task?.finish()
        request = nil
        task = nil
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    private func took(_ said: String?, over: Bool) {
        guard listening else { return }
        if let said, said != words {
            words = said
            heard?(said)
        }
        if over {
            stop()
            ended?()
        }
    }

    // The system calls these blocks on threads of its own, so they are made where nothing
    // ties them to the main thread.

    private nonisolated static func allowed() async -> Bool {
        let speech = await withCheckedContinuation { (answer: CheckedContinuation<Bool, Never>) in
            SFSpeechRecognizer.requestAuthorization { answer.resume(returning: $0 == .authorized) }
        }
        guard speech else { return false }
        return await AVAudioApplication.requestRecordPermission()
    }

    private nonisolated static func feed(_ input: AVAudioInputNode, format: AVAudioFormat,
                                         into request: SFSpeechAudioBufferRecognitionRequest) {
        let heard = Heard(request)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            heard.request.append(buffer)
        }
    }

    private nonisolated static func recognise(
        with recognizer: SFSpeechRecognizer, _ request: SFSpeechAudioBufferRecognitionRequest,
        report: @escaping @Sendable (String?, Bool) -> Void
    ) -> SFSpeechRecognitionTask {
        recognizer.recognitionTask(with: request) { result, error in
            report(result?.bestTranscription.formattedString, error != nil || result?.isFinal == true)
        }
    }
}

/// The request the microphone's block appends to: the block runs on the audio thread, and
/// appending to a request from there is what the request is for.
private final class Heard: @unchecked Sendable {
    let request: SFSpeechAudioBufferRecognitionRequest

    init(_ request: SFSpeechAudioBufferRecognitionRequest) {
        self.request = request
    }
}
