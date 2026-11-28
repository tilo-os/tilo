import Foundation
import ShellCore
import WebKit

/// What the app around a page hears from it.
@MainActor
public protocol PageHostDelegate: AnyObject {
    func page(_ host: PageHost, said message: PageMessage)
    func page(_ host: PageHost, attention: Attention)
    func pageArrived(_ host: PageHost)
    func page(_ host: PageHost, failed error: any Error)
    /// The page pointed at something that is not Tilo's: the system opens it.
    func page(_ host: PageHost, leavesFor url: URL)
}

/// The system web view that holds Tilo's page, and nothing but Tilo's page.
///
/// It keeps the site's data: the page holds its device key as a browser key that WebKit
/// wraps with a key from the app's keychain, so a person who signed in once stays signed in
/// for as long as the app keeps its signature and its data.
///
/// In a build that shows one app it holds that app's page and nothing else of Tilo. The way
/// in may lead anywhere in the site (its entry page, an invitation, a pairing link). Once the
/// person is in, an address of the site that is not the app's own goes to the system like
/// any other, and a page that is not the app's own is put out of sight and the app opened in
/// its place.
@MainActor
public final class PageHost: NSObject {
    public let settings: ShellSettings
    public let policy: SitePolicy
    public let view: WKWebView
    public weak var delegate: (any PageHostDelegate)?

    private let messages = MessageHandler()
    private let offer: WayInOffer?
    private var permission: NoticePermission
    /// Where this app is reached while it is closed, as the page hands it to its host.
    private var push: String?
    private var lastEnd: Date?
    private var titleWatch: NSKeyValueObservation?
    private var addressWatch: NSKeyValueObservation?
    /// The page said it is in: past its site's entry page.
    private var entered = false
    /// How often in a row the page was taken back to the app without getting there.
    private var returns = 0
    private var returning: Task<Void, Never>?
    private var veiled = false
    /// How long a page that just got in has to put away what a link gave it before the app
    /// is opened in its place.
    public var settling = Duration.seconds(1.5)

    /// - Parameters:
    ///   - offer: the ways in this device has, which the page shows in its entry page while the
    ///     site keeps the device out.
    ///   - keepsData: `false` only for a check that must leave nothing behind.
    ///   - inspectable: lets Safari's Web Inspector attach, for development.
    public init(settings: ShellSettings, permission: NoticePermission, offer: WayInOffer? = nil, keepsData: Bool = true,
                frame: CGRect = CGRect(x: 0, y: 0, width: 1100, height: 760), inspectable: Bool = false) {
        self.settings = settings
        policy = SitePolicy(settings: settings)
        self.permission = permission
        self.offer = offer
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = keepsData ? .default() : .nonPersistent()
        configuration.applicationNameForUserAgent = "TiloShell"
        // The page keeps its session and goes on telling the person what needs them while
        // its window is closed: that is what the menu-bar item is for.
        configuration.preferences.inactiveSchedulingPolicy = .none
        #if os(iOS)
        configuration.allowsInlineMediaPlayback = true
        // A call plays the others' voices without a press for each.
        configuration.mediaTypesRequiringUserActionForPlayback = []
        #endif
        view = WKWebView(frame: frame, configuration: configuration)
        super.init()
        messages.host = self
        view.configuration.userContentController.add(messages, name: Bridge.name)
        installScript()
        view.navigationDelegate = self
        view.uiDelegate = self
        view.allowsBackForwardNavigationGestures = false
        view.isInspectable = inspectable
        #if os(iOS)
        // The page lays itself out to the screen's edges and never scrolls as a whole.
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.scrollView.bounces = false
        #endif
        titleWatch = view.observe(\.title, options: [.new]) { [weak self] _, _ in
            Task { @MainActor in self?.titleChanged() }
        }
        // A page moves within itself without asking anyone: its address says where it went.
        addressWatch = view.observe(\.url, options: [.new]) { [weak self] _, _ in
            Task { @MainActor in self?.hold(after: .zero) }
        }
    }

    /// Open what the settings name: Tilo as a whole, or the one app.
    public func open() {
        go(to: settings.start)
    }

    /// Open a link into the site: an invitation, a pairing link, a document.
    public func open(_ link: URL) {
        guard policy.destination(of: link) == .inside, !handsOn(link) else { return }
        go(to: link)
    }

    /// In a build that shows one app, whatever else the site has is the system's to open once
    /// the person is in, like any address that is not Tilo's. Says whether `url` was handed on.
    private func handsOn(_ url: URL) -> Bool {
        guard entered, !policy.isOwn(url) else { return false }
        delegate?.page(self, leavesFor: url)
        return true
    }

    /// Load the page that is shown again, from the site.
    public func reload() {
        returns = 0
        if view.url == nil || veiled { open() } else { view.reload() }
    }

    /// Once the person is in, a page that is not what this build is for is put out of sight,
    /// and the app is opened in its place after `wait`.
    private func hold(after wait: Duration) {
        guard entered, returning == nil, let shown = view.url, !policy.isOwn(shown) else { return }
        veil(true)
        returns += 1
        // A site that leaves the app each time it is opened is said, not tried for ever.
        guard returns <= 2 else {
            delegate?.page(self, failed: PageAway())
            return
        }
        returning = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard let self, !Task.isCancelled else { return }
            self.go(to: self.settings.start)
        }
    }

    /// Out of sight, or back. A page out of sight goes on running and keeps the keyboard.
    private func veil(_ hidden: Bool) {
        veiled = hidden
        #if os(macOS)
        view.alphaValue = hidden ? 0 : 1
        #else
        view.alpha = hidden ? 0 : 1
        #endif
    }

    /// An address that differs from the one shown only after its `#` would leave the page as
    /// it is: a web view then moves within the document, and Tilo's page reads what a link
    /// carries only as it loads. So the page is taken there and loaded afresh.
    private func go(to address: URL) {
        returning?.cancel()
        returning = nil
        // The page that is shown is on its way out: what it said of the entry page is over.
        entered = false
        guard let shown = view.url, PageHost.sameDocument(shown, address) else {
            view.load(URLRequest(url: address))
            return
        }
        view.callAsyncJavaScript("location.replace(address); location.reload();",
                                 arguments: ["address": address.absoluteString], in: nil, in: .page,
                                 completionHandler: nil)
    }

    static func sameDocument(_ one: URL, _ other: URL) -> Bool {
        let bare = { (url: URL) -> String? in
            var parts = URLComponents(url: url, resolvingAgainstBaseURL: false)
            parts?.fragment = nil
            return parts?.string
        }
        return bare(one) != nil && bare(one) == bare(other)
    }

    /// Put `words` where the person writes.
    public func compose(_ words: String) async -> ComposeOutcome {
        let answer = try? await view.callAsyncJavaScript(
            "return globalThis.__tiloShell ? globalThis.__tiloShell.compose(words) : 'no-bridge'",
            arguments: ["words": words], in: nil, contentWorld: .page)
        return (answer as? String).flatMap(ComposeOutcome.init(rawValue:)) ?? .noBridge
    }

    /// Say `words` on the entry page, under the ways in the shell offers there: why a link the
    /// person brought was not followed. `false` where the entry page shows no such offer, and the
    /// words are the app's to say.
    public func say(onEntryPage words: String) async -> Bool {
        let answer = try? await view.callAsyncJavaScript(
            "return Boolean(globalThis.__tiloShell?.refused(words))",
            arguments: ["words": words], in: nil, contentWorld: .page)
        return answer as? Bool ?? false
    }

    /// Tell the page what the system now says about notifications. Pages loaded later read
    /// the same.
    public func setPermission(_ permission: NoticePermission) {
        guard permission != self.permission else { return }
        self.permission = permission
        installScript()
        call("permission", ["state": permission.rawValue], "state")
    }

    /// Give the page where this app is reached while it is closed, for its session to hand its
    /// host (`tilo.telling` `addresses#register`). Pages loaded later read the same.
    public func setPush(_ address: String?) {
        push = address
        installScript()
        guard let address else { return }
        call("push", ["address": address], "address")
    }

    /// What a push answered while no page ran: the thing by the name the push gave it, and the
    /// button pressed, if one was. The page answers it through the log's own action.
    public func told(_ name: String, action: String? = nil, reply: String? = nil) {
        view.callAsyncJavaScript(
            "globalThis.__tiloShell?.told(name, action, reply)",
            arguments: ["name": name, "action": action ?? NSNull(), "reply": reply ?? NSNull()],
            in: nil, in: .page, completionHandler: nil)
    }

    /// The person chose a notice, one of its answers, or typed one.
    public func chose(notice id: String, action: String? = nil, reply: String? = nil) {
        view.callAsyncJavaScript(
            "return globalThis.__tiloShell ? globalThis.__tiloShell.chosen(id, action, reply) : false",
            arguments: ["id": id, "action": action ?? NSNull(), "reply": reply ?? NSNull()],
            in: nil, in: .page, completionHandler: nil)
    }

    /// The person put a notice away.
    public func closed(notice id: String) {
        call("closed", ["id": id], "id")
    }

    private func call(_ function: String, _ arguments: [String: Any], _ names: String) {
        view.callAsyncJavaScript("globalThis.__tiloShell?.\(function)(\(names))", arguments: arguments,
                                 in: nil, in: .page, completionHandler: nil)
    }

    private func installScript() {
        let scripts = view.configuration.userContentController
        scripts.removeAllUserScripts()
        scripts.addUserScript(WKUserScript(
            source: Bridge.script(origin: settings.origin, permission: permission, offer: offer, push: push),
            injectionTime: .atDocumentStart, forMainFrameOnly: true))
    }

    private func titleChanged() {
        delegate?.page(self, attention: Attention(title: view.title))
    }

    fileprivate func heard(_ message: WKScriptMessage) {
        // Only Tilo's own page, in the main frame, speaks to the shell.
        guard message.frameInfo.isMainFrame, PageHost.text(of: message.frameInfo.securityOrigin) == settings.origin,
              let said = PageMessage(body: message.body)
        else { return }
        if case .entry(let entry) = said {
            let wasEntered = entered
            entered = entry.state == "open"
            if wasEntered, entry.state == "refused" { call("signedOut", [:], "") }
            if entered, let shown = view.url, policy.isOwn(shown) { returns = 0 }
        }
        delegate?.page(self, said: said)
        if case .entry = said { hold(after: settling) }
    }

    private func failed(_ error: any Error) {
        let problem = error as NSError
        // A load that another replaced, or that became a download, did not fail.
        if problem.domain == NSURLErrorDomain, problem.code == NSURLErrorCancelled { return }
        if problem.domain == "WebKitErrorDomain", problem.code == 102 { return }
        delegate?.page(self, failed: error)
    }

    static func text(of origin: WKSecurityOrigin) -> String {
        let standard = origin.protocol == "https" ? 443 : 80
        let port = origin.port == 0 || origin.port == standard ? "" : ":\(origin.port)"
        return "\(origin.protocol)://\(origin.host)\(port)"
    }

    /// A place in the person's downloads that no file has yet, for a file named `name`.
    static func freePlace(for name: String, in folder: URL) -> URL {
        let last = (name as NSString).lastPathComponent
        let safe = last.isEmpty || last == "." || last == ".." ? "download" : last
        let stem = (safe as NSString).deletingPathExtension
        let kind = (safe as NSString).pathExtension
        var place = folder.appendingPathComponent(safe)
        var count = 2
        while FileManager.default.fileExists(atPath: place.path) {
            let numbered = kind.isEmpty ? "\(stem) \(count)" : "\(stem) \(count).\(kind)"
            place = folder.appendingPathComponent(numbered)
            count += 1
        }
        return place
    }
}

/// The page's process ended twice in a row.
struct PageStopped: LocalizedError {
    var errorDescription: String? { Words.pageStopped.said }
}

/// The site left the one app this build shows for another page, each time it was opened.
struct PageAway: LocalizedError {
    var errorDescription: String? { Words.pageAway.said }
}

/// The message handler holds its target weakly: a content controller keeps its handlers.
private final class MessageHandler: NSObject, WKScriptMessageHandler {
    weak var host: PageHost?

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        host?.heard(message)
    }
}

extension PageHost: WKNavigationDelegate {
    public func webView(_ webView: WKWebView,
                        decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        guard let url = action.request.url else { return .cancel }
        if action.shouldPerformDownload { return .download }
        guard action.targetFrame?.isMainFrame ?? true else {
            // A frame the page embeds. It is shown, and it never meets the shell's script.
            return ["http", "https", "about", "blob", "data"].contains(url.scheme?.lowercased() ?? "") ? .allow : .cancel
        }
        switch policy.destination(of: url) {
        case .inside:
            if handsOn(url) { return .cancel }
            guard action.targetFrame == nil else { return .allow }
            // The page asked for a tab of its own. The shell has one place.
            webView.load(action.request)
            return .cancel
        case .outside:
            delegate?.page(self, leavesFor: url)
            return .cancel
        case .refused:
            return .cancel
        }
    }

    public func webView(_ webView: WKWebView,
                        decidePolicyFor response: WKNavigationResponse) async -> WKNavigationResponsePolicy {
        response.canShowMIMEType ? .allow : .download
    }

    public func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) {
        download.delegate = self
    }

    public func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) {
        download.delegate = self
    }

    public func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        // Another page: whatever the last one said of the site's entry page is over.
        entered = false
        veil(false)
        delegate?.page(self, said: .entry(PageEntry(state: "")))
    }

    public func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        delegate?.pageArrived(self)
    }

    public func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                        withError error: any Error) {
        failed(error)
    }

    public func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) {
        failed(error)
    }

    public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        // The page's process ended (memory, a crash). Its records are on the host, so it is
        // loaded again; one that ends as soon as it loads is said instead of tried forever.
        let now = Date()
        defer { lastEnd = now }
        if let lastEnd, now.timeIntervalSince(lastEnd) < 10 {
            delegate?.page(self, failed: PageStopped())
        } else {
            reload()
        }
    }
}

extension PageHost: WKUIDelegate {
    public func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                        for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let url = action.request.url else { return nil }
        switch policy.destination(of: url) {
        case .inside: if !handsOn(url) { webView.load(action.request) }
        case .outside: delegate?.page(self, leavesFor: url)
        case .refused: break
        }
        return nil
    }

    public func webView(_ webView: WKWebView, decideMediaCapturePermissionsFor origin: WKSecurityOrigin,
                        initiatedBy frame: WKFrameInfo, type: WKMediaCaptureType) async -> WKPermissionDecision {
        // Tilo's page may ask for the microphone and the camera; the system still asks the
        // person once. A frame from elsewhere may not.
        frame.isMainFrame && PageHost.text(of: origin) == settings.origin ? .grant : .deny
    }

    #if os(macOS)
    public func webView(_ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
                        initiatedByFrame frame: WKFrameInfo) async -> [URL]? {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = parameters.allowsMultipleSelection
        panel.canChooseDirectories = parameters.allowsDirectories
        return await panel.begin() == .OK ? panel.urls : nil
    }
    #endif
}

extension PageHost: WKDownloadDelegate {
    public func download(_ download: WKDownload, decideDestinationUsing response: URLResponse,
                         suggestedFilename: String) async -> URL? {
        #if os(macOS)
        let folders = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask)
        #else
        // The app's own documents, which the Files app shows.
        let folders = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)
        #endif
        return folders.first.map { PageHost.freePlace(for: suggestedFilename, in: $0) }
    }
}
