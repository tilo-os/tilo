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
@MainActor
public final class PageHost: NSObject {
    public let settings: ShellSettings
    public let policy: SitePolicy
    public let view: WKWebView
    public weak var delegate: (any PageHostDelegate)?

    private let door = Door()
    private var permission: NoticePermission
    private var titleWatch: NSKeyValueObservation?

    /// - Parameters:
    ///   - keepsData: `false` only for a check that must leave nothing behind.
    ///   - inspectable: lets Safari's Web Inspector attach, for development.
    public init(settings: ShellSettings, permission: NoticePermission, keepsData: Bool = true,
                frame: CGRect = CGRect(x: 0, y: 0, width: 1100, height: 760), inspectable: Bool = false) {
        self.settings = settings
        policy = SitePolicy(settings: settings)
        self.permission = permission
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = keepsData ? .default() : .nonPersistent()
        configuration.applicationNameForUserAgent = "TiloAssistant"
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
        door.host = self
        view.configuration.userContentController.add(door, name: Bridge.name)
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
    }

    /// Open the app the settings name.
    public func open() {
        view.load(URLRequest(url: settings.start))
    }

    /// Open a link into the site: an invitation, a pairing link, a document.
    public func open(_ link: URL) {
        guard policy.destination(of: link) == .inside else { return }
        view.load(URLRequest(url: link))
    }

    /// Put `words` where the person writes.
    public func compose(_ words: String) async -> ComposeOutcome {
        let answer = try? await view.callAsyncJavaScript(
            "return globalThis.__tiloShell ? globalThis.__tiloShell.compose(words) : 'no-bridge'",
            arguments: ["words": words], in: nil, contentWorld: .page)
        return (answer as? String).flatMap(ComposeOutcome.init(rawValue:)) ?? .noBridge
    }

    /// Tell the page what the system now says about notifications. Pages loaded later read
    /// the same.
    public func setPermission(_ permission: NoticePermission) {
        guard permission != self.permission else { return }
        self.permission = permission
        installScript()
        call("permission", ["state": permission.rawValue], "state")
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
            source: Bridge.script(origin: settings.origin, permission: permission),
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
        delegate?.page(self, said: said)
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

/// The message handler holds its target weakly: a content controller keeps its handlers.
private final class Door: NSObject, WKScriptMessageHandler {
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
        // Another page: whatever the last one said of the site's door is over.
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
        // The page's process ended (memory, a crash). Its records are on the host: open again.
        open()
    }
}

extension PageHost: WKUIDelegate {
    public func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                        for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let url = action.request.url else { return nil }
        switch policy.destination(of: url) {
        case .inside: webView.load(action.request)
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
