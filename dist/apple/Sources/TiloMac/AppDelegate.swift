#if os(macOS)
import AppKit
import ServiceManagement
import ShellCore
import ShellWeb

/// The Mac app: one window on Tilo's page, and what only the system can do around it.
/// Closing the window leaves the app in the menu bar, where the page goes on telling the
/// person what needs them.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private let name = Words.appName()
    private var window: NSWindow?
    private var host: PageHost?
    private var arrived = false
    private var notices: NoticeCenter?
    private var permission = NoticePermission.notAsked
    /// Where this Mac is reached while the app is closed, once Apple's service gave it one.
    private var push: String?
    /// What a push answered before the page was there to hear it.
    private var toldAnswer: (name: String, action: String?, reply: String?)?
    private var statusItem: NSStatusItem?
    private var statusLine: NSMenuItem?
    private var problem: ProblemView?
    private var hint: HintView?
    /// The site keeps this Mac out, and the page shows the way in at its entry page.
    private var refused = false
    /// The person is following a link from the entry page, and belongs where the app starts once in.
    private var returnsToStart = false
    private var settings: SettingsWindow?
    private var dictation: DictationPanel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        ShellSettings.keepArguments()
        NSApp.mainMenu = Menus.main(for: self, name: name)
        installStatusItem()
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

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        showWindow(nil)
        return true
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        Task {
            // The person may have answered the system's question in its settings meanwhile.
            if let notices { permission = await notices.permission() }
            host?.setPermission(permission)
            registerPush()
            await takeShared()
        }
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(toggleOpenAtLogin(_:)) {
            item.state = SMAppService.mainApp.status == .enabled ? .on : .off
        }
        if item.action == #selector(reload(_:)) || item.action == #selector(openLinkFromClipboard(_:)) {
            return host != nil
        }
        // Words need a page to go to.
        if item.action == #selector(dictate(_:)) { return arrived }
        return true
    }

    // MARK: The window and the page

    /// Open the site the settings name, in place of whatever was shown.
    private func openSite() {
        host?.view.removeFromSuperview()
        host = nil
        arrived = false
        refused = false
        let window = window ?? makeWindow()
        self.window = window
        switch ShellSettings.load() {
        case .success(let settings):
            // A Mac reads no code: its one way in is the link, which the page offers at its entry page.
            let offer = WayInOffer(said: Words.wayInMac.said, ways: [WayInOffer.Way(.paste, Words.pasteLink.said)])
            let host = PageHost(settings: settings, permission: permission, offer: offer,
                                inspectable: UserDefaults.standard.bool(forKey: "TiloInspect"))
            host.delegate = self
            self.host = host
            host.setPush(push)
            if let toldAnswer {
                host.told(toldAnswer.name, action: toldAnswer.action, reply: toldAnswer.reply)
                self.toldAnswer = nil
            }
            place(host.view, in: window)
            window.makeFirstResponder(host.view)
            show(nil)
            host.open()
        case .failure(let why):
            show(ProblemView(title: Words.addressRefused.said,
                             body: (why == .notSecure ? Words.addressNotSecure : Words.addressNotAnAddress).said,
                             button: Words.settings.said) { [weak self] in self?.showSettings(nil) })
        }
        showWindow(nil)
    }

    private func makeWindow() -> NSWindow {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 760),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = name
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 380, height: 480)
        window.contentView = NSView()
        window.center()
        window.setFrameAutosaveName("TiloPage")
        return window
    }

    /// The page fills the window. Nothing of the app stands beside it: the way in at the
    /// site's entry page is on that page.
    private func place(_ page: NSView, in window: NSWindow) {
        guard let content = window.contentView else { return }
        page.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(page, positioned: .below, relativeTo: nil)
        NSLayoutConstraint.activate([
            page.topAnchor.constraint(equalTo: content.topAnchor),
            page.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            page.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            page.trailingAnchor.constraint(equalTo: content.trailingAnchor),
        ])
    }

    private func cover(_ window: NSWindow, with view: NSView) {
        guard let content = window.contentView else { return }
        view.frame = content.bounds
        view.autoresizingMask = [.width, .height]
        content.addSubview(view)
    }

    /// Show `view` over the page, in place of an earlier one; `nil` shows the page.
    private func show(_ view: ProblemView?) {
        problem?.removeFromSuperview()
        problem = view
        guard let view else { return }
        let window = window ?? makeWindow()
        self.window = window
        cover(window, with: view)
    }

    @objc func showWindow(_ sender: Any?) {
        guard let window else { return }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    @objc func reload(_ sender: Any?) {
        show(nil)
        host?.reload()
    }

    @objc func openLinkFromClipboard(_ sender: Any?) {
        guard let host else { return }
        guard let text = NSPasteboard.general.string(forType: .string), let link = host.policy.link(in: text)
        else { return refuse(Words.noLink.said(site: host.policy.origin)) }
        follow(link)
    }

    /// Say why a link was not followed: on the entry page while the page offers the way in there,
    /// where the person pressed, and as a sheet anywhere else.
    private func refuse(_ words: String) {
        Task {
            guard let host, refused, await host.say(onEntryPage: words) else { return say(words) }
            showWindow(nil)
            if let window {
                NSAccessibility.post(element: window, notification: .announcementRequested,
                                     userInfo: [.announcement: words])
            }
        }
    }

    /// Open a link into the site. From the entry page it is the way in, and the person belongs
    /// where the app starts afterwards; from anywhere else it is what they asked to see.
    private func follow(_ link: URL) {
        returnsToStart = refused
        showWindow(nil)
        show(nil)
        host?.open(link)
    }

    private func say(_ words: String) {
        let alert = NSAlert()
        alert.messageText = words
        showWindow(nil)
        if let window { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    // MARK: Words into the message field

    /// Put `words` where the person writes; where nothing has the caret, say how to place it.
    @discardableResult
    private func compose(_ words: String) async -> ComposeOutcome {
        guard let host, arrived, !words.isEmpty else { return .noBridge }
        let outcome = await host.compose(words)
        if outcome == .waiting, let content = window?.contentView {
            hint?.removeFromSuperview()
            let hint = HintView(Words.wordsWaiting.said)
            hint.place(in: content)
            self.hint = hint
        }
        return outcome
    }

    /// What the share sheet left since the app was last in front.
    private func takeShared() async {
        guard let host, arrived, let place = SharedPlace.handOver(), let items = try? place.take() else { return }
        for item in items {
            // A link into this site is something to open. Anything else is something to say.
            if item.text == nil, let link = item.link.flatMap(host.policy.link(in:)) {
                follow(link)
            } else if let words = item.words, await compose(words) == .noBridge {
                // The page was not there to take them: they wait for the next time.
                try? place.leave(item)
            }
        }
    }

    @objc func dictate(_ sender: Any?) {
        guard dictation == nil else { return }
        let panel = DictationPanel { [weak self] words in
            self?.dictation = nil
            guard let self, let words else { return }
            self.showWindow(nil)
            Task { await self.compose(words) }
        }
        dictation = panel
        panel.start()
    }

    // MARK: Notifications

    /// Ask Apple's service for this Mac's address, once the build may be told while it is
    /// closed and the person lets notifications show.
    private func registerPush() {
        guard permission == .granted, ClosedBuild(info: Bundle.main.infoDictionary) != nil else { return }
        NSApp.registerForRemoteNotifications()
    }

    /// Apple's address for this Mac, with the device's own key, goes to the person's host
    /// through the page's own session, as the phone's app hands its own over.
    func application(_ application: NSApplication,
                     didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        guard let address = PushAddress.handedOver(token: deviceToken, info: Bundle.main.infoDictionary) else { return }
        push = address
        host?.setPush(address)
    }

    /// A push that shows nothing: the thing it names waits no more, and its notice is taken away.
    func application(_ application: NSApplication, didReceiveRemoteNotification userInfo: [String: Any]) {
        guard let push = ClosedPush(userInfo), push.isGone else { return }
        ClosedPush.withdraw(push.name)
    }

    /// A push was answered while the app was closed: the page answers it through the log.
    private func told(_ name: String, _ action: String?, _ reply: String?) {
        showWindow(nil)
        guard let host else {
            toldAnswer = (name, action, reply)
            return
        }
        host.told(name, action: action, reply: reply)
    }

    /// A tap on the home screen's card: the URL says where its page would have gone, so the notice
    /// is opened now, or when the page next shows it.
    private func route(_ url: URL) {
        guard let activation = WidgetLink.activation(in: url), let notices else { return }
        if host != nil, let id = notices.notice(at: activation) {
            answered(id, .opened)
        } else {
            notices.pending = activation
        }
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        if let url = urls.first { route(url) }
    }

    private func answered(_ id: String, _ answer: NoticeAnswer) {
        guard let host else { return }
        switch answer {
        case .opened:
            showWindow(nil)
            host.chose(notice: id)
        case .action(let action, let reply):
            host.chose(notice: id, action: action, reply: reply)
        case .reply(let words):
            showWindow(nil)
            host.chose(notice: id)
            Task { await compose(words) }
        case .dismissed:
            host.closed(notice: id)
        }
    }

    // MARK: The menu-bar item

    private func installStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.image = NSImage(systemSymbolName: "bubble.left", accessibilityDescription: name)
        item.button?.imagePosition = .imageLeading
        let menu = NSMenu()
        // What the page says waits, when something does.
        let line = NSMenuItem(title: "", action: nil, keyEquivalent: "")
        line.isHidden = true
        menu.addItem(line)
        Menus.add(.openApp, #selector(showWindow(_:)), to: menu, target: self)
        Menus.add(.dictate, #selector(dictate(_:)), to: menu, target: self)
        menu.addItem(.separator())
        Menus.add(.openAtLogin, #selector(toggleOpenAtLogin(_:)), to: menu, target: self)
        Menus.add(.settings, #selector(showSettings(_:)), to: menu, target: self)
        menu.addItem(.separator())
        Menus.add(.quit, #selector(NSApplication.terminate(_:)), to: menu, target: NSApp)
        item.menu = menu
        statusItem = item
        statusLine = line
    }

    private func show(_ attention: Attention) {
        let waits = attention.unread > 0
        statusItem?.button?.image = NSImage(systemSymbolName: waits ? "bubble.left.fill" : "bubble.left",
                                            accessibilityDescription: name)
        statusItem?.button?.title = attention.badge ?? ""
        statusLine?.title = attention.line
        statusLine?.isHidden = !waits
        NSApp.dockTile.badgeLabel = attention.badge
    }

    @objc func toggleOpenAtLogin(_ sender: Any?) {
        // The person's choice, and only theirs: nothing registers the app before they ask.
        let service = SMAppService.mainApp
        do {
            if service.status == .enabled { try service.unregister() } else { try service.register() }
        } catch {
            say(error.localizedDescription)
        }
    }

    @objc func showSettings(_ sender: Any?) {
        if settings == nil {
            settings = SettingsWindow { [weak self] in self?.openSite() }
        }
        settings?.show()
    }
}

extension AppDelegate: PageHostDelegate {
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
            // Pressed on the entry page. A Mac's one way is the link in the clipboard.
            guard refused, way == .paste else { return }
            openLinkFromClipboard(nil)
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
        show(attention)
    }

    func pageArrived(_ host: PageHost) {
        arrived = true
        show(nil)
        Task { await takeShared() }
    }

    func page(_ host: PageHost, failed error: any Error) {
        arrived = false
        show(ProblemView(title: Words.unreachableTitle.said,
                         body: "\(Words.unreachableBody.said)\n\(error.localizedDescription)",
                         button: Words.tryAgain.said) { [weak self] in self?.reload(nil) })
    }

    func page(_ host: PageHost, leavesFor url: URL) {
        NSWorkspace.shared.open(url)
    }
}
#endif
