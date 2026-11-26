import AssistantCore
import SwiftUI

/// What the window shows before this device is paired: the way in. Scan the code, or paste the
/// link, that "Pair a new device" shows on a device where the person is signed in.
public struct WayIn: View {
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @State private var scans = false

    public init() {}

    public var body: some View {
        // In the middle of the window while it fits, and scrolled where the person's text size makes it taller.
        GeometryReader { space in
          ScrollView {
            VStack(spacing: 18) {
            Mark(side: 56)
            VStack(spacing: 8) {
                Text(AssistantWords.wayInTitle.said).font(.tilo(26, .semibold, relativeTo: .title))
                    .foregroundStyle(Token.ink).multilineTextAlignment(.center)
                    .accessibilityAddTraits(.isHeader)
                // While a code is compared, the code is what the window is about.
                if !comparing {
                    // A device whose workspace is gone is told so, in the place of what a first pairing
                    // promises. So is one that was approved and stopped before it got in.
                    Text(lead.said).font(.tilo(15))
                        .foregroundStyle(Token.ink2).multilineTextAlignment(.center)
                        .frame(maxWidth: 420)
                    if let approved = model.joinApproved {
                        Text(AssistantWords.approvedWhen.said(["when": wording.mark(approved)])).font(.small125)
                            .foregroundStyle(Token.ink3).multilineTextAlignment(.center)
                    }
                }
            }
            if case .asked(let ask) = model.session {
                PairingAskView(ask: ask)
            } else if model.session == .pairing {
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(AssistantWords.pairing.said).font(.body14).foregroundStyle(Token.ink2)
                }
                .padding(.top, 6)
                // A device that goes on from an approval can be paired afresh while it does:
                // no count of seconds ends its try, so its person is never left without a way.
                Flow(spacing: 10, lineSpacing: 10) {
                    if model.joinApproved != nil { pairButtons(isMain: false) }
                    Button(AssistantWords.cancel.said) { model.signOut() }.buttonStyle(WayButton())
                }
            } else if case .confirming(let code) = model.session {
                // The code the other device asks about, large enough to compare at a glance.
                VStack(spacing: 10) {
                    Text(code).font(.tiloMono(34)).kerning(3).foregroundStyle(Token.ink)
                        .lineLimit(1).minimumScaleFactor(0.4)
                        .padding(.horizontal, 22).padding(.vertical, 12)
                        .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.cardRadius))
                        .accessibilityLabel(code.filter { !$0.isWhitespace }.map(String.init).joined(separator: " "))
                    Text(AssistantWords.compareCode.said).font(.body14).foregroundStyle(Token.ink2)
                        .multilineTextAlignment(.center).frame(maxWidth: 420)
                    // What Tilo said before it asked, once more while the other device decides.
                    ForEach(model.pairingSaid, id: \.self) { sentence in
                        Text(sentence).font(.small125).foregroundStyle(Token.ink3)
                            .multilineTextAlignment(.center).frame(maxWidth: 420)
                    }
                    Button(AssistantWords.cancel.said) { model.signOut() }.buttonStyle(WayButton())
                        .padding(.top, 6)
                }
                .padding(.top, 6)
            } else {
                Flow(spacing: 10, lineSpacing: 10) {
                    // The device that was approved goes on as itself: that is the first way on.
                    if model.joinApproved != nil {
                        Button(AssistantWords.tryAgain.said) { model.goOn() }.buttonStyle(WayButton(isMain: true))
                    }
                    pairButtons(isMain: model.joinApproved == nil)
                }
                .padding(.top, 6)
            }
            if let problem = model.problem {
                Text(problem).font(.small125).foregroundStyle(Token.you).multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
                // Tilo's sentence may say "copy the technical details below": here they are.
                if let details = model.problemDetails { ProblemDetails(details: details) }
            }
            if !comparing {
                Text(help.said).font(.small125).foregroundStyle(Token.ink3)
                    .multilineTextAlignment(.center).frame(maxWidth: 420)
            }
            // On a phone the app says which build it is: its Settings show the version alone.
            BuildLine()
            }
            .padding(32)
            .frame(maxWidth: .infinity, minHeight: space.size.height)
          }
          .scrollBounceBehavior(.basedOnSize)
        }
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

    /// While Tilo asks, or a code is compared, that is what the window is about.
    private var comparing: Bool {
        switch model.session {
        case .asked, .confirming: true
        default: false
        }
    }

    /// What stands under the title: what a first pairing promises, that the workspace is gone,
    /// or that this device was approved and goes on from there. While a try of that device is
    /// in work and nothing is said of the one before, it says that Tilo is opening.
    private var lead: AssistantWords {
        if model.joinApproved != nil { return model.session == .pairing && model.problem == nil ? .wayInGoesOn : .wayInApproved }
        return model.session == .reset ? .wayInReset : .wayInLead
    }

    /// A phone scans or pastes; a Mac pastes. A device that was approved is told that pairing
    /// it afresh is the other way.
    private var help: AssistantWords {
        if model.joinApproved != nil { return .wayInApprovedHelp }
        #if os(iOS)
        return .wayInHelp
        #else
        return .wayInHelpOne
        #endif
    }

    /// Open the person's Tilo in the browser, or take a link they already have.
    @ViewBuilder private func pairButtons(isMain: Bool) -> some View {
        let site = model.source.devicePairingSite
        if let site {
            Link(AssistantWords.getPairingLink.said, destination: site)
                .buttonStyle(WayButton(isMain: isMain))
        }
        #if os(iOS)
        Button(AssistantWords.scanCode.said) { scans = true }.buttonStyle(WayButton(isMain: isMain && site == nil))
        Button(AssistantWords.pasteLink.said, action: paste).buttonStyle(WayButton())
        #else
        Button(AssistantWords.pasteLink.said, action: paste).buttonStyle(WayButton(isMain: isMain && site == nil))
            .keyboardShortcut("v")
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

/// What Tilo asks before this device is paired, in Tilo's own sentences: whose devices it would
/// join, which server adds it, and what follows. The two buttons are Tilo's two ways on, under
/// Tilo's own words for them. Nothing goes on until the person presses one, and a stray Return
/// stops.
private struct PairingAskView: View {
    let ask: PairingAsk
    @Environment(AssistantModel.self) private var model

    var body: some View {
        VStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 8) {
                Text(ask.heading).font(.tilo(16, .semibold)).foregroundStyle(Token.ink)
                    .accessibilityAddTraits(.isHeader)
                ForEach(Array(ask.said.enumerated()), id: \.offset) { place, sentence in
                    // Whose devices and which server come first, and are what the person decides on.
                    Text(sentence).font(place < 2 ? Font.tilo(14, .medium) : Font.body14)
                        .foregroundStyle(place < 2 ? Token.ink : Token.ink2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                // The site this app pairs with, its address in full, whoever handed the link.
                if !ask.site.isEmpty {
                    Text(AssistantWords.pairingSite.said(["site": ask.site])).font(.body14).foregroundStyle(Token.ink2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.horizontal, 18).padding(.vertical, 14)
            .frame(maxWidth: 480, alignment: .leading)
            .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.cardRadius))
            Flow(spacing: 10, lineSpacing: 10) {
                Button(ask.go) { model.answerPairing(goOn: true) }.buttonStyle(WayButton(isMain: true))
                Button(ask.stop) { model.answerPairing(goOn: false) }.buttonStyle(WayButton())
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(.top, 6)
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

/// The technical details Tilo's page offers under the sentence a pairing ended with, folded as
/// they are on the page: a press lays them open, with Copy details under them. So a sentence
/// that says "copy the technical details below" has them below it.
private struct ProblemDetails: View {
    @Environment(AssistantModel.self) private var model
    let details: String
    @State private var isOpen = false

    var body: some View {
        VStack(spacing: 10) {
            Button(AssistantWords.technicalDetails.said) { isOpen.toggle() }
                .buttonStyle(.plain).font(.small125).foregroundStyle(Token.ink2).underline()
            if isOpen {
                Text(details).font(.tiloMono(12)).foregroundStyle(Token.ink2).textSelection(.enabled)
                    .multilineTextAlignment(.leading).frame(maxWidth: 420, alignment: .leading)
                    .padding(12)
                    .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.cardRadius))
                Button(AssistantWords.copyDetails.said) { model.copyProblemDetails() }.buttonStyle(WayButton())
            }
        }
    }
}

extension AssistantModel {
    /// The details under the way in's sentence go to the pasteboard, for the person to send on.
    func copyProblemDetails() {
        guard let details = problemDetails else { return }
        #if os(macOS)
        pasteboard.clearContents()
        pasteboard.setString(details, forType: .string)
        #else
        UIPasteboard.general.string = details
        #endif
    }
}
