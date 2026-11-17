import AssistantCore
import SwiftUI

/// What the window shows before this device is paired: the way in. Scan the code, or paste the
/// link, that "Pair a new device" shows on a device where the person is signed in.
public struct WayIn: View {
    @Environment(AssistantModel.self) private var model
    @State private var scans = false

    public init() {}

    public var body: some View {
        VStack(spacing: 18) {
            Mark(side: 56)
            VStack(spacing: 8) {
                Text(AssistantWords.wayInTitle.said).font(.tilo(26, .semibold, relativeTo: .title))
                    .foregroundStyle(Token.ink).multilineTextAlignment(.center)
                    .accessibilityAddTraits(.isHeader)
                Text(AssistantWords.wayInLead.said).font(.tilo(15))
                    .foregroundStyle(Token.ink2).multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
            }
            if model.session == .pairing {
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(AssistantWords.pairing.said).font(.body14).foregroundStyle(Token.ink2)
                }
                .padding(.top, 6)
            } else {
                Flow(spacing: 10, lineSpacing: 10) {
                    #if os(iOS)
                    Button(AssistantWords.scanCode.said) { scans = true }.buttonStyle(WayButton(isMain: true))
                    Button(AssistantWords.pasteLink.said, action: paste).buttonStyle(WayButton())
                    #else
                    Button(AssistantWords.pasteLink.said, action: paste).buttonStyle(WayButton(isMain: true))
                        .keyboardShortcut("v")
                    #endif
                }
                .padding(.top, 6)
            }
            if let problem = model.problem {
                Text(problem).font(.small125).foregroundStyle(Token.you).multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
            }
            Text(help.said).font(.small125).foregroundStyle(Token.ink3)
                .multilineTextAlignment(.center).frame(maxWidth: 420)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Token.content)
        #if os(iOS)
        .sheet(isPresented: $scans) {
            CodeScanner { found in
                scans = false
                model.pair(with: found, scanned: true)
            }
            .ignoresSafeArea()
        }
        #endif
    }

    /// A phone scans or pastes; a Mac pastes.
    private var help: AssistantWords {
        #if os(iOS)
        .wayInHelp
        #else
        .wayInHelpOne
        #endif
    }

    private func paste() {
        #if os(macOS)
        let words = NSPasteboard.general.string(forType: .string) ?? ""
        #else
        let words = UIPasteboard.general.string ?? ""
        #endif
        model.pair(with: words)
    }
}

/// The way in's buttons: the cards' buttons, a size larger.
private struct WayButton: ButtonStyle {
    var isMain = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.tilo(15, .medium))
            .foregroundStyle(isMain ? Token.content : Token.ink)
            .padding(.horizontal, 20)
            .padding(.vertical, 11)
            .background(isMain ? Token.ink : Token.raised, in: RoundedRectangle(cornerRadius: 11))
            .opacity(configuration.isPressed ? 0.7 : 1)
            .contentShape(RoundedRectangle(cornerRadius: 11))
    }
}

#if os(iOS)
import AVFoundation
import UIKit

/// The camera, looking for a pairing code. It asks for the camera only when the person chose to scan.
struct CodeScanner: UIViewControllerRepresentable {
    let found: (String) -> Void

    func makeUIViewController(context: Context) -> ScannerController {
        let controller = ScannerController()
        controller.found = found
        return controller
    }

    func updateUIViewController(_ controller: ScannerController, context: Context) {}
}

final class ScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var found: ((String) -> Void)?
    private let session = AVCaptureSession()
    private var preview: AVCaptureVideoPreviewLayer?
    private let note = UILabel()
    private var took = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        note.textColor = .white
        note.numberOfLines = 0
        note.textAlignment = .center
        note.font = .preferredFont(forTextStyle: .body)
        note.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(note)
        NSLayoutConstraint.activate([
            note.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            note.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 32),
            note.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -32),
        ])
        Task {
            guard await AVCaptureDevice.requestAccess(for: .video) else {
                self.note.text = AssistantWords.cameraNotAllowed.said
                return
            }
            self.begin()
        }
    }

    private func begin() {
        guard let camera = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: camera),
              session.canAddInput(input)
        else {
            note.text = AssistantWords.cameraNotAllowed.said
            return
        }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        view.layer.insertSublayer(preview, at: 0)
        self.preview = preview
        let running = Running(session)
        DispatchQueue.global(qos: .userInitiated).async { running.session.startRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        let running = Running(session)
        DispatchQueue.global(qos: .userInitiated).async { running.session.stopRunning() }
    }

    nonisolated func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject],
                                    from connection: AVCaptureConnection) {
        guard let words = (objects.first as? AVMetadataMachineReadableCodeObject)?.stringValue else { return }
        Task { @MainActor in
            guard !self.took else { return }
            self.took = true
            self.found?(words)
        }
    }
}

/// The camera's session, started and stopped off the main thread as the system asks.
private final class Running: @unchecked Sendable {
    let session: AVCaptureSession

    init(_ session: AVCaptureSession) {
        self.session = session
    }
}
#endif
