import Foundation
import ShellCore
import WebKit

/// What can be learned about this device and build with no window: whether the system web
/// view offers what Tilo's page needs, whether the page's keys survive a restart, whether the
/// script that joins page and system works, and whether the site answers.
@MainActor
public final class PageCheck {
    public struct Line: Equatable, Sendable {
        public let name: String
        public let value: String
        /// `nil` for a fact that is neither right nor wrong.
        public let passed: Bool?

        public init(name: String, value: String, passed: Bool?) {
            self.name = name
            self.value = value
            self.passed = passed
        }
    }

    private let settings: ShellSettings
    private let live: Bool
    /// Called with each web view the check makes. A phone runs a page's code only for a view
    /// that is on screen, so its app puts them there; a Mac needs nothing.
    public var place: ((WKWebView) -> Void)?

    /// - Parameter live: also open and close one WebTransport session at the host the site
    ///   names. It opens no stream, so no person and no workspace is touched.
    public init(settings: ShellSettings, live: Bool) {
        self.settings = settings
        self.live = live
    }

    public func run() async -> [Line] {
        await device() + [shared(), opens()] + site()
    }

    /// What this build, with the settings kept for it on this device, shows of Tilo.
    private func opens() -> Line {
        let shown = settings.alone ? "one app alone" : settings.app == nil ? "Tilo as a whole" : "Tilo as a whole, one app in front"
        return Line(name: "it opens", value: "\(settings.start.absoluteString), which is \(shown)", passed: nil)
    }

    /// The app and its share extension are two programs. This asks whether the build holds the
    /// one place they share, by leaving something there and taking it back.
    private func shared() -> Line {
        let name = "the place shared with the share extension"
        guard let place = SharedPlace.handOver() else {
            return Line(name: name, value: "none: this build is signed by nobody", passed: nil)
        }
        let mark = "check \(UUID().uuidString)"
        do {
            try place.leave(SharedItem(text: mark))
            let taken = try place.take()
            // Whatever a person shared meanwhile is theirs: it goes back.
            for other in taken where other.text != mark { try place.leave(other) }
            let found = taken.contains { $0.text == mark }
            return Line(name: name, value: found ? "holds what is left there" : "lost what was left there",
                        passed: found)
        } catch {
            return Line(name: name, value: "cannot be used: \(error)", passed: false)
        }
    }

    /// The origin the device questions are asked on: secure, and never fetched.
    private static let bench = "https://shell-check.invalid"

    private func device() async -> [Line] {
        guard let bench = try? ShellSettings(site: PageCheck.bench) else { return [] }
        // The app's own data store, so the key written now is the one a later run reads.
        let host = PageHost(settings: bench, permission: .granted)
        let watch = Watch()
        host.delegate = watch
        place?(host.view)
        host.view.loadHTMLString("<!doctype html><title>Tilo</title><textarea id=\"field\"></textarea>",
                                 baseURL: bench.site.appendingPathComponent("check"))
        if let problem = await watch.arrival(within: 15) {
            return [Line(name: "web view", value: problem, passed: false)]
        }
        guard let seen = await PageCheck.ask(host, PageCheck.deviceQuestions) else {
            return [Line(name: "web view", value: "the page answered nothing", passed: false)]
        }
        let yes = { (key: String) in seen[key] as? Bool == true }
        var lines = [Line(name: "secure context", value: yes("secure") ? "yes" : "no", passed: yes("secure"))]
        let gpu = yes("device") ? "an adapter and a device" : yes("adapter") ? "an adapter, no device"
            : yes("gpu") ? "offered, but no graphics adapter" : "absent"
        lines.append(Line(name: "WebGPU", value: gpu, passed: yes("device")))
        lines.append(Line(name: "WebTransport", value: yes("webTransport") ? "present" : "absent",
                          passed: yes("webTransport")))
        // Tilo's page opens no session without them (crates/platform/tilo-net/src/web/webtransport.rs).
        lines.append(Line(name: "WebTransport datagrams", value: yes("datagrams") ? "present" : "absent",
                          passed: yes("datagrams")))
        let key = seen["key"] as? String ?? "failed"
        let kept = ["kept": "kept from an earlier run, and it signs",
                    "stored": "stored now; a second run says whether it is kept"][key]
        lines.append(Line(name: "the page's key", value: kept ?? "failed: \(seen["keyError"] as? String ?? "unknown")",
                          passed: key == "kept" ? true : key == "stored" ? nil : false))
        let told = watch.messages.contains { if case .notify(let notice) = $0 { notice.title == "Check" } else { false } }
        lines.append(Line(name: "a notice from the page reaches the system", value: told ? "yes" : "no", passed: told))
        let put = await host.compose("two\nlines") == .inserted
        let field = await PageCheck.ask(host, "return JSON.stringify({value: document.getElementById('field').value})")
        let written = put && field?["value"] as? String == "two\nlines"
        lines.append(Line(name: "words reach a field", value: written ? "yes" : "no", passed: written))
        return lines
    }

    private func site() async -> [Line] {
        let host = PageHost(settings: settings, permission: .notAsked)
        let watch = Watch()
        host.delegate = watch
        place?(host.view)
        host.view.load(URLRequest(url: settings.site.appendingPathComponent("healthz")))
        if let problem = await watch.arrival(within: 20) {
            return [Line(name: "site \(settings.origin)", value: problem, passed: false)]
        }
        let question = PageCheck.siteQuestions.replacingOccurrences(of: "%LIVE%", with: live ? "true" : "false")
        guard let seen = await PageCheck.ask(host, question) else {
            return [Line(name: "site \(settings.origin)", value: "answers, and says nothing of its host", passed: false)]
        }
        var lines = [Line(name: "site \(settings.origin)", value: "answers", passed: true)]
        if let problem = seen["error"] as? String {
            lines.append(Line(name: "the site's host", value: problem, passed: false))
            return lines
        }
        let place = { (origin: String?) -> String in
            guard let origin, let host = URL(string: origin)?.host() else { return "not named by the site" }
            return ShellSettings.isThisComputer(host) ? "\(origin), which is this computer only" : origin
        }
        let reach = seen["reach"] as? String
        lines.append(Line(name: "the site's host", value: [place(seen["host"] as? String), reach].compactMap(\.self).joined(separator: ", "),
                          passed: reach.map { $0 == "answers" }))
        lines.append(Line(name: "where pages open their live connection", value: place(seen["link"] as? String),
                          passed: nil))
        if let opened = seen["live"] as? String {
            lines.append(Line(name: "a live connection from this app", value: opened, passed: opened == "opened"))
        }
        return lines
    }

    /// Run `question`, the body of an asynchronous function that returns JSON text, in the page.
    private static func ask(_ host: PageHost, _ question: String) async -> [String: Any]? {
        let answer = try? await host.view.callAsyncJavaScript(question, arguments: [:], in: nil, contentWorld: .page)
        guard let text = answer as? String, let data = text.data(using: .utf8) else { return nil }
        return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }

    private static let deviceQuestions = #"""
    const out = {secure: isSecureContext, gpu: typeof navigator.gpu === 'object', adapter: false, device: false,
      webTransport: typeof WebTransport === 'function', datagrams: false, key: 'failed'};
    if (out.webTransport) out.datagrams = 'datagrams' in WebTransport.prototype;
    if (out.gpu) {
      try {
        const adapter = await navigator.gpu.requestAdapter();
        out.adapter = Boolean(adapter);
        const device = adapter ? await adapter.requestDevice() : null;
        out.device = Boolean(device);
        device?.destroy?.();
      } catch (error) { out.gpuError = String(error); }
    }
    // A key that cannot be taken out, kept in the page's own storage: what Tilo's page keeps
    // its device key as. WebKit wraps it with a key from the app's keychain.
    try {
      const db = await new Promise((resolve, reject) => {
        const open = indexedDB.open('tilo-shell-check', 1);
        open.onupgradeneeded = () => open.result.createObjectStore('keys');
        open.onsuccess = () => resolve(open.result);
        open.onerror = () => reject(open.error);
      });
      const kept = await new Promise((resolve, reject) => {
        const read = db.transaction('keys').objectStore('keys').get('device');
        read.onsuccess = () => resolve(read.result);
        read.onerror = () => reject(read.error);
      });
      if (kept) {
        await crypto.subtle.sign({name: 'ECDSA', hash: 'SHA-256'}, kept.privateKey, new Uint8Array([1, 2, 3]));
        out.key = 'kept';
      } else {
        const pair = await crypto.subtle.generateKey({name: 'ECDSA', namedCurve: 'P-256'}, false, ['sign', 'verify']);
        await new Promise((resolve, reject) => {
          const write = db.transaction('keys', 'readwrite');
          write.objectStore('keys').put(pair, 'device');
          write.oncomplete = resolve;
          write.onerror = () => reject(write.error);
          write.onabort = () => reject(write.error);
        });
        out.key = 'stored';
      }
      db.close();
    } catch (error) { out.keyError = String(error); }
    try { new Notification('Check', {body: 'A notice from the page', tag: 'check'}); } catch (error) { out.noticeError = String(error); }
    document.getElementById('field').focus();
    return JSON.stringify(out);
    """#

    private static let siteQuestions = #"""
    const out = {};
    try {
      const entry = await (await fetch('/entry.json', {cache: 'no-store'})).json();
      const host = entry?.home?.host ?? null;
      const info = typeof host?.info === 'string' ? JSON.parse(host.info) : host?.info ?? null;
      const web = info?.address?.web ?? null;
      out.host = host?.origin ?? null;
      // Any answer will do: what is asked is whether this app may reach the host at all.
      if (out.host) out.reach = await fetch(`${out.host}/`, {mode: 'no-cors', cache: 'no-store'}).then(() => 'answers', error => `does not answer this app: ${error}`);
      out.link = web?.origin ?? null;
      const pin = Array.isArray(web?.certificate_sha256) && web.certificate_sha256.length === 32 ? web.certificate_sha256 : null;
      if (%LIVE% && out.link) {
        if (typeof WebTransport !== 'function') out.live = 'this web view has no WebTransport';
        else {
          const session = new WebTransport(`${out.link}/peer/v1/link/session`,
            pin ? {serverCertificateHashes: [{algorithm: 'sha-256', value: new Uint8Array(pin)}]} : {});
          out.live = await Promise.race([
            session.ready.then(() => 'opened', error => `refused: ${error}`),
            new Promise(resolve => setTimeout(() => resolve('no answer within 8 seconds'), 8000)),
          ]);
          try { session.close(); } catch { /* it never opened */ }
        }
      }
    } catch (error) { out.error = String(error); }
    return JSON.stringify(out);
    """#
}

/// Waits for a page to arrive, and keeps what it said meanwhile.
@MainActor
private final class Watch: PageHostDelegate {
    private(set) var messages: [PageMessage] = []
    private var outcome: String??
    private var waiting: CheckedContinuation<String?, Never>?

    /// `nil` once the page arrived, else why it did not.
    func arrival(within seconds: Double) async -> String? {
        if let outcome { return outcome }
        return await withCheckedContinuation { waiting in
            self.waiting = waiting
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(seconds))
                self.settle("no answer within \(Int(seconds)) seconds")
            }
        }
    }

    private func settle(_ problem: String?) {
        guard outcome == nil else { return }
        outcome = .some(problem)
        waiting?.resume(returning: problem)
        waiting = nil
    }

    func page(_ host: PageHost, said message: PageMessage) { messages.append(message) }
    func page(_ host: PageHost, attention: Attention) {}
    func pageArrived(_ host: PageHost) { settle(nil) }
    func page(_ host: PageHost, failed error: any Error) { settle(error.localizedDescription) }
    func page(_ host: PageHost, leavesFor url: URL) {}
}
