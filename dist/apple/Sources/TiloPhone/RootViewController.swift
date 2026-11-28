#if os(iOS)
import AVFoundation
import ShellCore
import ShellWeb
import UIKit
import VisionKit
import WebKit

/// The iPhone app's one screen: Tilo's page to every edge, and what only the system can do
/// around it. The page lays itself out for a phone; nothing native is drawn over it but a
/// screen for when there is no page, and a line while words wait for a place to go. The way
/// in on the site's entry page is on that page: the page shows what this iPhone offers there.
@MainActor
final class RootViewController: UIViewController {
    private var host: PageHost?
    private var arrived = false
    private var notices: NoticeCenter?
    private var permission = NoticePermission.notAsked
    /// Where this iPhone is reached while the app is closed, once Apple's service gave it one.
    private var push: String?
    /// What a push answered before the page was there to hear it.
    private var toldAnswer: (name: String, action: String?, reply: String?)?
    private var problem: UIView?
    private var hint: UIView?
    /// The site keeps this iPhone out, and the page shows the way in at its entry page.
    private var refused = false
    /// The person is following a link from the entry page, and belongs where the app starts once in.
    private var returnsToStart = false
    /// Asked for from the app's icon before the page was there.
    private var asked: QuickAction?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        if CommandLine.arguments.contains("--check") { return check() }
        ShellSettings.keepArguments()
        let notices = NoticeCenter()
        notices.answered = { [weak self] id, answer in self?.answered(id, answer) }
        notices.toldAnswered = { [weak self] name, action, reply in self?.told(name, action, reply) }
        self.notices = notices
        Task {
            permission = await notices.permission()
            registerPush()
            openSite()
        }
    }

    /// The app is in front again: the person may have changed the site or the notifications
    /// in the system's settings, or shared something meanwhile.
    func cameForward() {
        guard let notices, host != nil || problem != nil else { return }
        Task {
            permission = await notices.permission()
            if case .success(let chosen) = ShellSettings.load(), chosen == host?.settings {
                host?.setPermission(permission)
                registerPush()
                await takeShared()
            } else {
                openSite()
            }
        }
    }

    /// Ask Apple's service for this iPhone's address, once the build may be told while it is
    /// closed and the person lets notifications show.
    private func registerPush() {
        guard permission == .granted, ClosedBuild(info: Bundle.main.infoDictionary) != nil else { return }
        UIApplication.shared.registerForRemoteNotifications()
    }

    /// Apple's address for this iPhone goes to the person's host through the page's session.
    func registered(_ deviceToken: Data) {
        guard let address = PushAddress.handedOver(token: deviceToken, info: Bundle.main.infoDictionary) else { return }
        push = address
        host?.setPush(address)
    }

    /// A push was answered while the app was closed: the page answers it through the log.
    private func told(_ name: String, _ action: String?, _ reply: String?) {
        guard let host else {
            toldAnswer = (name, action, reply)
            return
        }
        host.told(name, action: action, reply: reply)
    }

    func perform(_ action: QuickAction?) {
        guard let action else { return }
        switch action {
        case .dictate:
            guard arrived else { asked = action; return }
            present(DictationSheet { [weak self] words in
                guard let words else { return }
                Task { await self?.compose(words) }
            }, animated: true)
        case .openLink:
            guard host != nil else { asked = action; return }
            follow(UIPasteboard.general.string, else: .noLink)
        case .scanCode:
            guard host != nil else { asked = action; return }
            scanCode()
        }
    }

    /// Open `text` when it is a link into the site, and say `refusal` when it is not.
    private func follow(_ text: String?, else refusal: Words) {
        guard let host else { return }
        guard let link = text.flatMap(host.policy.link(in:)) else { return say(refusal.said(site: host.policy.origin)) }
        // From the entry page a link is the way in; from anywhere else it is what they asked to see.
        returnsToStart = refused
        show(nil)
        host.open(link)
    }

    // MARK: The page

    private func openSite() {
        host?.view.removeFromSuperview()
        host = nil
        arrived = false
        refused = false
        switch ShellSettings.load() {
        case .success(let settings):
            let host = PageHost(settings: settings, permission: permission, offer: RootViewController.offer,
                                frame: view.bounds, inspectable: UserDefaults.standard.bool(forKey: "TiloInspect"))
            host.delegate = self
            self.host = host
            host.setPush(push)
            if let toldAnswer {
                host.told(toldAnswer.name, action: toldAnswer.action, reply: toldAnswer.reply)
                self.toldAnswer = nil
            }
            host.view.translatesAutoresizingMaskIntoConstraints = false
            view.insertSubview(host.view, at: 0)
            // To the screen's edges: the page keeps clear of the notch and the home bar itself.
            NSLayoutConstraint.activate([
                host.view.topAnchor.constraint(equalTo: view.topAnchor),
                host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
                host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
                host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            ])
            show(nil)
            host.open()
            // A link or a code needs only the site, not a page that has arrived.
            if let asked, asked != .dictate {
                self.asked = nil
                perform(asked)
            }
        case .failure(let why):
            show(problemView(title: Words.addressRefused.said,
                             body: (why == .notSecure ? Words.addressNotSecure : Words.addressNotAnAddress).said,
                             button: Words.settings.said) {
                guard let settings = URL(string: UIApplication.openSettingsURLString) else { return }
                UIApplication.shared.open(settings)
            })
        }
    }

    /// Show `screen` over the page, in place of an earlier one; `nil` shows the page.
    private func show(_ screen: UIView?) {
        problem?.removeFromSuperview()
        problem = screen
        guard let screen else { return }
        screen.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(screen)
        NSLayoutConstraint.activate([
            screen.topAnchor.constraint(equalTo: view.topAnchor),
            screen.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            screen.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            screen.trailingAnchor.constraint(equalTo: view.trailingAnchor),
        ])
        UIAccessibility.post(notification: .screenChanged, argument: screen)
    }

    /// What is wrong and the one thing to do, for when there is no page to show.
    private func problemView(title: String, body: String, button: String, act: @escaping () -> Void) -> UIView {
        let screen = UIView()
        screen.backgroundColor = .systemBackground
        let heading = UILabel()
        heading.text = title
        heading.font = .preferredFont(forTextStyle: .title2)
        let words = UILabel()
        words.text = body
        words.font = .preferredFont(forTextStyle: .body)
        words.textColor = .secondaryLabel
        for label in [heading, words] {
            label.numberOfLines = 0
            label.textAlignment = .center
            label.adjustsFontForContentSizeCategory = true
        }
        let press = UIButton(configuration: .borderedProminent(), primaryAction: UIAction(title: button) { _ in act() })
        let stack = UIStackView(arrangedSubviews: [heading, words, press])
        stack.axis = .vertical
        stack.alignment = .center
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        screen.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.centerYAnchor.constraint(equalTo: screen.centerYAnchor),
            stack.leadingAnchor.constraint(equalTo: screen.layoutMarginsGuide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: screen.layoutMarginsGuide.trailingAnchor),
        ])
        return screen
    }

    // MARK: The way in

    /// What this iPhone offers at the site's entry page: the code or the link from a device where
    /// the person is signed in, or an invitation's link. The page shows it in its entry page
    /// (`Bridge`), so nothing of it is drawn here.
    private static var offer: WayInOffer {
        let paste = WayInOffer.Way(.paste, Words.pasteLink.said)
        guard readsCodes else { return WayInOffer(said: Words.wayInMac.said, ways: [paste]) }
        return WayInOffer(said: Words.wayInPhone.said, ways: [WayInOffer.Way(.scan, Words.scanCode.said), paste])
    }

    /// A phone with a camera the system reads codes with: every iPhone. A simulator has no
    /// camera and offers the button all the same, so that the entry page can be looked at there.
    static var readsCodes: Bool {
        #if targetEnvironment(simulator)
        true
        #else
        DataScannerViewController.isSupported
        #endif
    }

    /// Read the pairing code another device shows, with the camera. The system asks for the
    /// camera here, when the person asked to scan, and never before.
    private func scanCode() {
        Task { [weak self] in
            // The system asks the person once; after that this only reads their answer.
            let allowed = await AVCaptureDevice.requestAccess(for: .video)
            self?.showScanner(allowed: allowed)
        }
    }

    private func showScanner(allowed: Bool) {
        guard allowed, DataScannerViewController.isSupported, DataScannerViewController.isAvailable
        else { return say(Words.cameraNotAllowed.said) }
        let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])],
                                                isHighlightingEnabled: true)
        scanner.delegate = self
        scanner.navigationItem.title = Words.scanCode.said
        scanner.navigationItem.leftBarButtonItem = UIBarButtonItem(systemItem: .cancel, primaryAction: UIAction { [weak self] _ in
            self?.dismiss(animated: true)
        })
        present(UINavigationController(rootViewController: scanner), animated: true) {
            try? scanner.startScanning()
        }
    }

    /// Say why nothing was opened: on the entry page while the page offers the way in there, where
    /// the person pressed, and as an alert anywhere else.
    private func say(_ words: String) {
        Task {
            if let host, refused, await host.say(onEntryPage: words) {
                UIAccessibility.post(notification: .announcement, argument: words)
                return
            }
            let alert = UIAlertController(title: words, message: nil, preferredStyle: .alert)
            alert.addAction(UIAlertAction(title: Words.done.said, style: .default))
            present(alert, animated: true)
        }
    }

    // MARK: Words into the message field

    @discardableResult
    private func compose(_ words: String) async -> ComposeOutcome {
        guard let host, arrived, !words.isEmpty else { return .noBridge }
        let outcome = await host.compose(words)
        guard outcome == .waiting else { return outcome }
        hint?.removeFromSuperview()
        var said = UIButton.Configuration.gray()
        said.title = Words.wordsWaiting.said
        said.cornerStyle = .capsule
        let line = UIButton(configuration: said)
        // It says something and does nothing: a press goes to the page under it.
        line.isUserInteractionEnabled = false
        line.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(line)
        NSLayoutConstraint.activate([
            line.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            line.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -72),
        ])
        hint = line
        UIAccessibility.post(notification: .announcement, argument: Words.wordsWaiting.said)
        Task { [weak line] in
            try? await Task.sleep(for: .seconds(12))
            line?.removeFromSuperview()
        }
        return outcome
    }

    /// What the share sheet left since the app was last in front.
    private func takeShared() async {
        guard host != nil, arrived, let place = SharedPlace.handOver(), let items = try? place.take() else { return }
        for item in items {
            // A link into this site is something to open. Anything else is something to say.
            if item.text == nil, let link = item.link {
                follow(link, else: .noLink)
            } else if let words = item.words, await compose(words) == .noBridge {
                // The page was not there to take them: they wait for the next time.
                try? place.leave(item)
            }
        }
    }

    /// A link the system handed the app: a tap on the home screen's card says where its page would
    /// have gone. Open that notice now, or when the page next shows it.
    func route(_ url: URL) {
        guard let activation = WidgetLink.activation(in: url), let notices else { return }
        if host != nil, let id = notices.notice(at: activation) {
            answered(id, .opened)
        } else {
            notices.pending = activation
        }
    }

    private func answered(_ id: String, _ answer: NoticeAnswer) {
        guard let host else { return }
        switch answer {
        case .opened:
            host.chose(notice: id)
        case .action(let action, let reply):
            host.chose(notice: id, action: action, reply: reply)
        case .reply(let words):
            host.chose(notice: id)
            Task { await compose(words) }
        case .dismissed:
            host.closed(notice: id)
        }
    }

    // MARK: Asked from a Mac, with `--check`

    /// What this iPhone's web view offers Tilo's page, on standard output and on the screen.
    private func check() {
        let shown = UITextView(frame: view.bounds)
        shown.isEditable = false
        shown.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        shown.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(shown)
        let screen: UIView = view
        Task {
            var lines = [PageCheck.Line(name: "site", value: "its address cannot be used", passed: false)]
            if case .success(let settings) = ShellSettings.load() {
                let check = PageCheck(settings: settings, live: CommandLine.arguments.contains("--live"))
                check.place = { web in
                    // On screen, and out of the way of the words.
                    web.frame = CGRect(x: 0, y: 0, width: 320, height: 240)
                    web.alpha = 0.02
                    screen.insertSubview(web, at: 0)
                }
                lines = await check.run()
            }
            let text = lines.map { line in
                "\(line.passed == true ? "ok  " : line.passed == false ? "FAIL" : "    ") \(line.name): \(line.value)"
            }
            for line in text { print("CHECK \(line)") }
            print("CHECK end")
            shown.text = text.joined(separator: "\n\n")
            // Asked by a script, which waits for the program to end. A person reads the screen.
            if CommandLine.arguments.contains("--quit") { exit(lines.contains { $0.passed == false } ? 1 : 0) }
        }
    }
}

extension RootViewController: PageHostDelegate {
    func page(_ host: PageHost, said message: PageMessage) {
        switch message {
        case .notify(let notice):
            notices?.show(notice)
        case .withdraw(let id):
            notices?.withdraw(id: id)
        case .askPermission:
            Task {
                guard let notices else { return }
                permission = await notices.ask()
                host.setPermission(permission)
                registerPush()
            }
        case .wordsInserted:
            hint?.removeFromSuperview()
            hint = nil
        case .wayIn(let way):
            // Pressed on the entry page.
            guard refused else { return }
            switch way {
            case .scan: scanCode()
            case .paste: follow(UIPasteboard.general.string, else: .noLink)
            }
        case .entry(let entry):
            refused = entry.wantsWayIn
            guard entry.state == "open", returnsToStart else { return }
            returnsToStart = false
            Task {
                // In. A moment for the page to put away what the link gave it, then the start.
                try? await Task.sleep(for: host.settling)
                host.open()
            }
        }
    }

    func page(_ host: PageHost, attention: Attention) {
        notices?.setBadge(attention.unread)
    }

    func pageArrived(_ host: PageHost) {
        arrived = true
        show(nil)
        if let asked {
            self.asked = nil
            perform(asked)
        }
        Task { await takeShared() }
    }

    func page(_ host: PageHost, failed error: any Error) {
        arrived = false
        show(problemView(title: Words.unreachableTitle.said,
                         body: "\(Words.unreachableBody.said)\n\(error.localizedDescription)",
                         button: Words.tryAgain.said) { [weak self] in
            self?.show(nil)
            self?.host?.reload()
        })
    }

    func page(_ host: PageHost, leavesFor url: URL) {
        UIApplication.shared.open(url)
    }
}

extension RootViewController: DataScannerViewControllerDelegate {
    func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem],
                     allItems: [RecognizedItem]) {
        for case .barcode(let code) in addedItems {
            guard let text = code.payloadStringValue else { continue }
            dataScanner.stopScanning()
            dismiss(animated: true) { [weak self] in self?.follow(text, else: .codeNoLink) }
            return
        }
    }
}

/// A sheet that shows what is heard while the person dictates, with Insert and Cancel. The
/// system's own indicator shows that the microphone is on.
@MainActor
final class DictationSheet: UIViewController {
    private let heard = UILabel()
    private let dictation = Dictation()
    private let done: (String?) -> Void
    private var finished = false
    private lazy var insert = UIButton(configuration: .borderedProminent(),
                                       primaryAction: UIAction(title: Words.insertWords.said) { [weak self] _ in
        self?.finish(inserting: true)
    })

    /// - Parameter done: called once, with the words to insert or `nil` when there are none.
    init(done: @escaping (String?) -> Void) {
        self.done = done
        super.init(nibName: nil, bundle: nil)
        modalPresentationStyle = .pageSheet
        sheetPresentationController?.detents = [.medium()]
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("made in code") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let title = UILabel()
        title.text = Words.dictate.said
        title.font = .preferredFont(forTextStyle: .headline)
        heard.text = Words.listening.said
        heard.font = .preferredFont(forTextStyle: .body)
        heard.textColor = .secondaryLabel
        heard.numberOfLines = 0
        for label in [title, heard] { label.adjustsFontForContentSizeCategory = true }
        insert.isEnabled = false
        let cancel = UIButton(configuration: .bordered(), primaryAction: UIAction(title: Words.cancel.said) { [weak self] _ in
            self?.finish(inserting: false)
        })
        let buttons = UIStackView(arrangedSubviews: [cancel, insert])
        buttons.spacing = 12
        buttons.distribution = .fillEqually
        let stack = UIStackView(arrangedSubviews: [title, heard, UIView(), buttons])
        stack.axis = .vertical
        stack.spacing = 16
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        let margins = view.layoutMarginsGuide
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: margins.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: margins.bottomAnchor, constant: -12),
            stack.leadingAnchor.constraint(equalTo: margins.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: margins.trailingAnchor),
        ])
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        dictation.heard = { [weak self] words in
            self?.heard.text = words
            self?.heard.textColor = .label
            self?.insert.isEnabled = true
        }
        // Listening ended by itself. What was heard stays to be inserted; with nothing heard
        // there is nothing to wait for.
        dictation.ended = { [weak self] in
            guard let self, self.dictation.words.isEmpty else { return }
            self.finish(inserting: false)
        }
        Task {
            do throws(Dictation.Problem) {
                try await dictation.start()
            } catch {
                heard.text = (error == .notAllowed ? Words.dictationNotAllowed : Words.dictationUnavailable).said
            }
        }
    }

    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        // Pulled down: the same as Cancel.
        finish(inserting: false)
    }

    private func finish(inserting: Bool) {
        guard !finished else { return }
        finished = true
        dictation.stop()
        let words = inserting && !dictation.words.isEmpty ? dictation.words : nil
        if presentingViewController != nil { dismiss(animated: true) }
        done(words)
    }
}
#endif
