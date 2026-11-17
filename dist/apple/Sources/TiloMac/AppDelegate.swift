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
    private let name = Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String ?? "Tilo Assistant"
    private var window: NSWindow?
    private var host: PageHost?
    private var arrived = false
    private var notices: NoticeCenter?
    private var permission = NoticePermission.notAsked
    private var statusItem: NSStatusItem?
    private var statusLine: NSMenuItem?
    private var problem: ProblemView?
    private var hint: HintView?
    private var wayIn: WayInBar?
    private var pageBottom: NSLayoutConstraint?
    /// The person is following a link from the door, and belongs in the assistant once in.
    private var returnsToStart = false
    private var settings: SettingsWindow?
    private var dictation: DictationPanel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        ShellSettings.keepArguments()
        NSApp.mainMenu = Menus.main(for: self, name: name)
        installStatusItem()
        let notices = NoticeCenter()
        notices.answered = { [weak self] id, answer in self?.answered(id, answer) }
        self.notices = notices
        Task {
            permission = await notices.permission()
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
        wayIn?.removeFromSuperview()
        wayIn = nil
        host = nil
        arrived = false
        let window = window ?? makeWindow()
        self.window = window
        switch ShellSettings.load() {
        case .success(let settings):
            let host = PageHost(settings: settings, permission: permission,
                                inspectable: UserDefaults.standard.bool(forKey: "TiloInspect"))
            host.delegate = self
            self.host = host
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
        window.setFrameAutosaveName("TiloAssistant")
        return window
    }

    /// The page fills the window, down to its foot or to the bar that offers a way in.
    private func place(_ page: NSView, in window: NSWindow) {
        guard let content = window.contentView else { return }
        page.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(page, positioned: .below, relativeTo: nil)
        let bottom = page.bottomAnchor.constraint(equalTo: content.bottomAnchor)
        NSLayoutConstraint.activate([
            page.topAnchor.constraint(equalTo: content.topAnchor),
            page.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            page.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            bottom,
        ])
        pageBottom = bottom
    }

    /// While the site keeps this Mac out, a bar under the page offers the way in: the link
    /// from a device where the person is signed in.
    private func offerWayIn(_ wanted: Bool) {
        guard wanted != (wayIn != nil), let content = window?.contentView, let page = host?.view else { return }
        wayIn?.removeFromSuperview()
        wayIn = nil
        pageBottom?.isActive = false
        var foot = content.bottomAnchor
        if wanted {
            let bar = WayInBar(words: Words.wayInMac.said, button: Words.pasteLink.said) { [weak self] in
                self?.openLinkFromClipboard(nil)
            }
            bar.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(bar, positioned: .above, relativeTo: page)
            NSLayoutConstraint.activate([
                bar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
                bar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
                bar.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            ])
            wayIn = bar
            foot = bar.topAnchor
        }
        pageBottom = page.bottomAnchor.constraint(equalTo: foot)
        pageBottom?.isActive = true
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
        guard let host, let text = NSPasteboard.general.string(forType: .string),
              let link = host.policy.link(in: text)
        else { return say(Words.noLink.said) }
        follow(link)
    }

    /// Open a link into the site. From the door it is the way in, and the person belongs in
    /// the assistant afterwards; from anywhere else it is what they asked to see.
    private func follow(_ link: URL) {
        returnsToStart = wayIn != nil
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
        Menus.add(.openAssistant, #selector(showWindow(_:)), to: menu, target: self)
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
            }
        case .wordsInserted:
            hint?.removeFromSuperview()
            hint = nil
        case .entry(let entry):
            offerWayIn(entry.wantsWayIn)
            guard entry.state == "open", returnsToStart else { return }
            returnsToStart = false
            Task {
                // In. A moment for the page to put away what the link gave it, then the assistant.
                try? await Task.sleep(for: .seconds(1.5))
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
