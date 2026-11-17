#if os(macOS)
import ShellCore
@testable import ShellWeb
import WebKit
import XCTest

/// The web view and its script, against a page that speaks to the system the way Tilo's
/// does. No window is opened and nothing is fetched: the page is given as text under the
/// site's address.
@MainActor
final class PageHostTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var host: PageHost!
    private var seen: Seen!

    override func setUp() async throws {
        host = PageHost(settings: try ShellSettings(site: Self.site), permission: .notAsked, keepsData: false)
        seen = Seen()
        host.delegate = seen
        host.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page arrives") { self.seen.arrivals == 1 }
    }

    /// As Tilo's page: `web/host.mjs` takes a paste as words while its own text has the caret,
    /// and `crates/platform/tilo-notify/web/notify.mjs` shows and withdraws notifications.
    private static let page = #"""
    <!doctype html><title>Tilo</title>
    <textarea id="editor" aria-label="Tilo focused text editor"></textarea>
    <textarea id="plain"></textarea>
    <input id="secret" type="password">
    <script>
      window.pasted = [];
      document.addEventListener('paste', event => {
        if (document.activeElement?.id !== 'editor' || !event.clipboardData) return;
        window.pasted.push(event.clipboardData.getData('text/plain'));
        event.preventDefault();
      }, true);
      window.heard = [];
      window.show = (tag, title, body, activation, actions) => {
        const notification = new Notification(title, actions ? {body, tag, actions} : {body, tag});
        notification.addEventListener('error', () => window.heard.push(`error:${tag}`));
        notification.addEventListener('close', () => window.heard.push(`close:${tag}`));
        notification.addEventListener('click', event => {
          event.preventDefault();
          notification.close();
          window.heard.push(`click:${activation}:${event.action}:${event.reply}`);
        });
      };
    </script>
    """#

    func testAPagesNoticeReachesTheSystemAndThePersonsChoiceComesBack() async throws {
        try await expect("Notification.permission", "default")
        host.setPermission(.granted)
        try await expect("Notification.permission", "granted")
        try await run("show('agentq-1', 'Ada asks', 'Send the reply?', 'tilo-agent:ada')")
        try await until("the notice is told") { self.seen.notices.count == 1 }
        let notice = try XCTUnwrap(seen.notices.first)
        XCTAssertEqual(notice, PageNotice(id: notice.id, title: "Ada asks", body: "Send the reply?", tag: "agentq-1"))
        host.chose(notice: notice.id)
        // The page closes what was chosen and follows its activation, as in a browser.
        try await until("the page withdraws it") { self.seen.withdrawn == [notice.id] }
        try await expect("heard.join(' ')", "close:agentq-1 click:tilo-agent:ada::null")
        // A notice the page no longer knows is nothing to choose.
        host.chose(notice: notice.id)
        host.chose(notice: "never-shown")
        try await expect("heard.length", 2)
    }

    func testAnswersThePageNamesComeBackWithWhatWasTyped() async throws {
        host.setPermission(.granted)
        try await expect("Notification.maxActions", 4)
        try await run("""
        show('q', 'Draft ready', 'Send it?', 'q', [{action: 'send', title: 'Send'}, {action: 'say', title: 'Reply', type: 'text', placeholder: 'Message'}, {title: 'nameless'}])
        """)
        try await until("the notice is told") { self.seen.notices.count == 1 }
        let notice = try XCTUnwrap(seen.notices.first)
        XCTAssertEqual(notice.actions, [NoticeAction(id: "send", title: "Send"),
                                        NoticeAction(id: "say", title: "Reply", kind: .text, placeholder: "Message")])
        host.chose(notice: notice.id, action: "say", reply: "Not today")
        try await expect("heard.join(' ')", "close:q click:q:say:Not today")
    }

    func testThePersonIsAskedOnceAndARefusalShowsNothing() async throws {
        try await run("Notification.requestPermission().then(answer => { window.first = answer; }); Notification.requestPermission(answer => { window.second = answer; })")
        try await until("the page asks") { self.seen.asked == 2 }
        host.setPermission(.denied)
        try await expect("[first, second, Notification.permission].join(' ')", "denied denied denied")
        try await run("show('late', 'Too late', '', 'late')")
        try await expect("heard.join(' ')", "error:late")
        XCTAssertEqual(seen.notices, [])
        // Decided once, the answer is the system's and nobody is asked again.
        try await expect("await Notification.requestPermission()", "denied")
        XCTAssertEqual(seen.asked, 2)
    }

    func testAPutAwayNoticeClosesInThePage() async throws {
        host.setPermission(.granted)
        try await run("show('gone', 'Reminder', 'Call back', 'gone')")
        try await until("the notice is told") { self.seen.notices.count == 1 }
        host.closed(notice: try XCTUnwrap(seen.notices.first).id)
        try await expect("heard.join(' ')", "close:gone")
        XCTAssertEqual(seen.withdrawn, [])
    }

    func testWordsGoWhereThePersonWrites() async throws {
        try await run("document.getElementById('editor').focus()")
        let lines = "Two lines\nfor one message"
        let intoEditor = await host.compose(lines)
        XCTAssertEqual(intoEditor, .inserted)
        // Whole, as one paste: typed line by line the break would have sent the first line.
        try await expect("JSON.stringify(pasted)", "[\"Two lines\\nfor one message\"]")
        try await run("document.getElementById('plain').focus()")
        let intoPlain = await host.compose("Plain words")
        XCTAssertEqual(intoPlain, .inserted)
        try await expect("document.getElementById('plain').value", "Plain words")
        let nothing = await host.compose("")
        XCTAssertEqual(nothing, .nothing)
    }

    func testSeveralThingsThatWaitAllArriveInTheOrderTheyCame() async throws {
        let first = await host.compose("First shared")
        let second = await host.compose("Second shared")
        XCTAssertEqual([first, second], [.waiting, .waiting])
        try await run("document.getElementById('plain').focus()")
        try await until("the words are put in") { self.seen.inserted == 1 }
        try await expect("document.getElementById('plain').value", "First shared\nSecond shared")
    }

    func testALinkThatDiffersOnlyAfterTheHashLoadsThePageAfresh() async throws {
        XCTAssertTrue(PageHost.sameDocument(URL(string: "https://tilo.test/app/#/me/so.tilo.chat")!,
                                            URL(string: "https://tilo.test/app/#/@host/w/ws?pair=abc")!))
        XCTAssertFalse(PageHost.sameDocument(URL(string: "https://tilo.test/app/#/me")!, URL(string: "https://tilo.test/#/me")!))
        XCTAssertFalse(PageHost.sameDocument(URL(string: "https://tilo.test/app/?a=1#x")!, URL(string: "https://tilo.test/app/#x")!))
        // Tilo's page reads what a link carries only as it loads, so moving within the
        // document would leave a pairing link unread. The page here was given as text, so
        // loading it afresh asks the site, which does not exist: the load is asked for and fails.
        try await run("window.kept = true")
        host.open(try XCTUnwrap(URL(string: "\(Self.site)/app/#/@host/w/ws?pair=abc")))
        try await until("a fresh load is asked for") { self.seen.arrivals + self.seen.failures.count == 2 }
    }

    func testWordsWaitForAFieldAndNeverGoIntoAPassword() async throws {
        try await run("document.getElementById('secret').focus()")
        let outcome = await host.compose("Dictated words")
        XCTAssertEqual(outcome, .waiting)
        try await expect("document.getElementById('secret').value", "")
        try await run("document.getElementById('plain').focus()")
        try await until("the words are put in") { self.seen.inserted == 1 }
        try await expect("document.getElementById('plain').value", "Dictated words")
        // Put in once: the next field the person presses gets nothing.
        try await run("document.getElementById('editor').focus()")
        try await Task.sleep(for: .milliseconds(150))
        try await expect("pasted.length", 0)
    }

    func testTheShellHearsWhereThePageStandsAtTheDoor() async throws {
        // As web/entry-page.mjs writes it on the page's root element.
        try await run("document.documentElement.dataset.entry = 'refused'")
        try await until("the closed door is told") { self.seen.entry?.wantsWayIn == true }
        try await run("document.documentElement.dataset.pairingCode = '4821'")
        try await until("pairing under way is told") { self.seen.entry == PageEntry(state: "refused", pairing: true) }
        try await run("delete document.documentElement.dataset.pairingCode; document.documentElement.dataset.entry = 'open'")
        try await until("getting in is told") { self.seen.entry == PageEntry(state: "open") }
    }

    func testThePagesTitleIsWhatWaits() async throws {
        try await run("document.title = '(2) Ada needs you · Tilo · Lounge'")
        try await until("the title is read") { self.seen.attention?.unread == 2 }
        try await run("document.title = 'Tilo · Lounge'")
        try await until("the title is read") { self.seen.attention?.unread == 0 }
    }

    func testAnythingButTheSiteOpensElsewhere() async throws {
        try await run("location.href = 'https://example.com/pricing'")
        try await until("the system is handed the address") { self.seen.left.map(\.absoluteString) == ["https://example.com/pricing"] }
        try await run("window.open('https://example.org/docs')")
        try await until("a new tab goes the same way") { self.seen.left.count == 2 }
        try await run("location.href = 'file:///etc/hosts'")
        try await Task.sleep(for: .milliseconds(200))
        // The page is still the site's, and nothing more was handed on.
        try await expect("location.origin", Self.site)
        XCTAssertEqual(seen.left.count, 2)
        XCTAssertEqual(seen.arrivals, 1)
    }

    func testAPageThatIsNotTheSitesGetsNoWayToTheSystem() async throws {
        let other = PageHost(settings: try ShellSettings(site: Self.site), permission: .granted, keepsData: false)
        let quiet = Seen()
        other.delegate = quiet
        // `about:blank` may be shown, and it is not the site.
        other.view.loadHTMLString("<!doctype html><title>blank</title>", baseURL: nil)
        try await until("the blank page arrives") { quiet.arrivals == 1 }
        // Whatever the web view offers a page by itself, nothing there is the shell's.
        let found = try await other.view.callAsyncJavaScript(
            "return [typeof globalThis.__tiloShell, globalThis.Notification?.maxActions === 4].join(' ')",
            arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(found, "undefined false")
        let outcome = await other.compose("words")
        XCTAssertEqual(outcome, .noBridge)
    }

    func testADownloadNeverTakesAnotherFilesPlace() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("shell-downloads-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        XCTAssertEqual(PageHost.freePlace(for: "Report.pdf", in: folder).lastPathComponent, "Report.pdf")
        try Data().write(to: folder.appendingPathComponent("Report.pdf"))
        try Data().write(to: folder.appendingPathComponent("Report 2.pdf"))
        XCTAssertEqual(PageHost.freePlace(for: "Report.pdf", in: folder).lastPathComponent, "Report 3.pdf")
        // A name that points out of the folder is only its last part.
        XCTAssertEqual(PageHost.freePlace(for: "../../Library/x", in: folder).path, folder.appendingPathComponent("x").path)
        XCTAssertEqual(PageHost.freePlace(for: "..", in: folder).lastPathComponent, "download")
    }

    // MARK: -

    private func run(_ script: String) async throws {
        _ = try await host.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func expect<Value: Equatable>(_ expression: String, _ expected: Value,
                                          file: StaticString = #filePath, line: UInt = #line) async throws {
        var last: Any?
        for _ in 0..<150 {
            last = try await host.view.callAsyncJavaScript("return (\(expression))", arguments: [:], in: nil,
                                                           contentWorld: .page)
            if let number = last as? NSNumber, let wanted = expected as? Int, number.intValue == wanted { return }
            if last as? Value == expected { return }
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTFail("\(expression) is \(String(describing: last)), not \(expected)", file: file, line: line)
    }

    private func until(_ what: String, file: StaticString = #filePath, line: UInt = #line,
                       _ holds: @MainActor () -> Bool) async throws {
        for _ in 0..<400 {
            if holds() { return }
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTFail("waited in vain until \(what)", file: file, line: line)
    }
}

@MainActor
private final class Seen: PageHostDelegate {
    var notices: [PageNotice] = []
    var withdrawn: [String] = []
    var asked = 0
    var inserted = 0
    var attention: Attention?
    var entry: PageEntry?
    var arrivals = 0
    var failures: [String] = []
    var left: [URL] = []

    func page(_ host: PageHost, said message: PageMessage) {
        switch message {
        case .notify(let notice): notices.append(notice)
        case .withdraw(let id): withdrawn.append(id)
        case .askPermission: asked += 1
        case .entry(let entry): self.entry = entry
        case .wordsInserted: inserted += 1
        }
    }

    func page(_ host: PageHost, attention: Attention) { self.attention = attention }
    func pageArrived(_ host: PageHost) { arrivals += 1 }
    func page(_ host: PageHost, failed error: any Error) { failures.append(error.localizedDescription) }
    func page(_ host: PageHost, leavesFor url: URL) { left.append(url) }
}
#endif
