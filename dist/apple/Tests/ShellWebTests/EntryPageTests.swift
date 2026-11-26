#if os(macOS)
import ShellCore
@testable import ShellWeb
import WebKit
import XCTest

/// The way in at the site's entry page, against pages that show the entry page as Tilo's do. The
/// shell draws nothing beside the page: what a device offers is shown on the entry page itself.
/// No window is opened and nothing is fetched.
@MainActor
final class EntryPageTests: PageCase {
    private static let site = "https://tilo.test"
    private static let phone = WayInOffer(said: "Scan the code or paste the link that “Pair a new device” gives.",
                                         ways: [WayInOffer.Way(.scan, "Scan code"), WayInOffer.Way(.paste, "Paste link")])
    private static let mac = WayInOffer(said: "Paste the link that “Pair a new device” gives.",
                                       ways: [WayInOffer.Way(.paste, "Paste link")])

    private func show(_ page: String, offer: WayInOffer?, width: CGFloat = 1100, at address: String = "https://tilo.test/") async throws {
        host = PageHost(settings: try ShellSettings(site: Self.site), permission: .notAsked, offer: offer,
                        keepsData: false, frame: CGRect(x: 0, y: 0, width: width, height: 760))
        seen = Seen()
        host.delegate = seen
        host.view.loadHTMLString(page, baseURL: URL(string: address))
        try await until("the page arrives") { self.seen.arrivals == 1 }
    }

    /// The entry page's entry page in small (`web/index.html`): the screen for someone who came
    /// without an invitation, the screen for a refusal of another kind, and the status line,
    /// each shown by what the page writes on its root element.
    private static func entryPage(drawsItsOwn: Bool = false) -> String {
        #"""
        <!doctype html><title>Tilo</title>
        <style>
          html, body { margin: 0; height: 100%; background: #121214; color: #ddd; font: 14px system-ui, sans-serif; }
          #arrival-refusal, #arrival-failure { display: none; }
          [data-arrival=stopped][data-entry=refused][data-refusal] #arrival-refusal { position: fixed; inset: 0; z-index: 2; display: flex; flex-direction: column; align-items: center; box-sizing: border-box; padding: max(40px, calc(50vh - 230px)) 24px 0; overflow-y: auto; text-align: center; background: #121214; }
          .refusal-main { display: flex; flex-direction: column; align-items: center; width: min(400px, 100%); }
          .refusal-alt { width: 100%; margin: 20px 0 0; padding-top: 20px; border-top: 1px solid #23252a; }
          .refusal-alt p { margin: 0; font-size: 14px; line-height: 1.5; color: #a8a8b2; }
          .refusal-alt p + p { margin-top: 8px; }
          [data-failure] #arrival-failure { position: fixed; inset: 0; z-index: 2; display: flex; flex-direction: column; align-items: center; box-sizing: border-box; padding: max(40px, calc(50vh - 190px)) 24px 32px; overflow-y: auto; text-align: center; background: #121214; }
          .failure-main { display: flex; flex-direction: column; align-items: center; width: min(420px, 100%); }
          #status { position: fixed; z-index: 2; left: 50%; top: calc(50% + 22px); transform: translateX(-50%); width: min(36em, calc(100% - 48px)); margin: 0; text-align: center; line-height: 1.5; }
          [data-refusal] #status, [data-failure] #status { width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
        </style>
        <section id="arrival-refusal"><div class="refusal-main">
          <h1 id="arrival-refusal-heading">Lounge is invite-only for now</h1>
          <p id="arrival-refusal-body">Ask someone in it for an invite link, then paste it here.</p>
          <div class="refusal-alt">OWN<p class="refusal-ways"><button id="arrival-sign-in-open" type="button">Sign in</button> · <button id="arrival-invite-toggle" type="button">Have an invite link?</button></p></div>
        </div></section>
        <section id="arrival-failure"><div class="failure-main">
          <h1>You are no longer in Lounge</h1><p>Ask someone in it to invite you again.</p>
          <div class="failure-actions"><button type="button">Try again</button></div>
        </div></section>
        <p id="status" role="status">Loading Tilo…</p>
        <script>
          // As web/entry-page.mjs turns someone away: stopped, refused, then the screen for it.
          window.refuse = screen => {
            const root = document.documentElement;
            root.dataset.arrival = 'stopped';
            root.dataset.entry = 'refused';
            if (screen) root.dataset[screen] = screen === 'refusal' ? 'invite' : 'removed';
          };
          window.group = () => document.querySelector('.tilo-shell-way-in');
          window.box = element => element.getBoundingClientRect();
        </script>
        """#.replacingOccurrences(of: "OWN", with: drawsItsOwn ? #"<div data-way-in><button type="button">Scan</button></div>"# : "")
    }

    /// The page that shows one app (`web/app/index.html`): its entry page is the status line alone.
    private static let oneAppPage = #"""
    <!doctype html><title>Tilo</title>
    <style>
      html, body { margin: 0; height: 100%; background: #121214; color: #ddd; font: 14px system-ui, sans-serif; }
      #status { position: fixed; z-index: 2; left: 50%; top: calc(50% + 22px); transform: translateX(-50%); width: min(36em, calc(100% - 48px)); margin: 0; text-align: center; line-height: 1.5; }
    </style>
    <header id="bar"><span id="status" role="status">Loading Tilo…</span></header>
    <script>
      window.refuse = words => {
        const root = document.documentElement;
        root.dataset.arrival = 'stopped';
        root.dataset.entry = 'refused';
        document.getElementById('status').textContent = words;
      };
      window.group = () => document.querySelector('.tilo-shell-way-in');
      window.box = element => element.getBoundingClientRect();
    </script>
    """#

    func testOnARefusingEntryPageTheWaysInStandInThePageItself() async throws {
        // As wide as an iPhone.
        try await show(Self.entryPage(), offer: Self.phone, width: 402)
        try await expect("group() === null", true)
        try await run("refuse('refusal')")
        // First among what the entry page offers someone who already has something.
        try await expect("document.querySelector('#arrival-refusal .refusal-alt').firstElementChild === group()", true)
        // The sentence, with the name of the other device's control kept in one piece.
        try await expect("group().querySelector('p').textContent",
                         "Scan the code or paste the link that “Pair\u{a0}a\u{a0}new\u{a0}device” gives.")
        try await expect("[...group().querySelectorAll('button')].map(press => press.textContent).join(' | ')",
                         "Scan code | Paste link")
        // Controls a screen reader reads as buttons, by their words; the drawings say nothing.
        try await expect("""
        [...group().querySelectorAll('button')].every(press => press.type === 'button' && !press.disabled
          && press.querySelector('svg').getAttribute('aria-hidden') === 'true')
        """, true)
        // On the entry page's column, edge to edge with it, and nothing wider than the screen.
        try await expect("""
        (() => {
          const column = box(document.querySelector('.refusal-alt')), own = box(group());
          const [scan, paste] = [...group().querySelectorAll('button')].map(box);
          return [Math.abs(own.left - column.left) < 1 && Math.abs(own.right - column.right) < 1,
            Math.abs(scan.left - column.left) < 1 && Math.abs(paste.right - column.right) < 1,
            Math.abs(scan.top - paste.top) < 1 && Math.abs(scan.width - paste.width) < 1,
            scan.height >= 40 && paste.left - scan.right >= 8,
            box(group().querySelector('p')).top >= scan.bottom + 8,
            own.bottom <= box(document.getElementById('arrival-sign-in-open')).top - 16,
            document.documentElement.scrollWidth <= innerWidth].join(' ');
        })()
        """, "true true true true true true true")
        // A press is the shell's to act on.
        try await run("group().querySelector('[data-way=scan]').click()")
        try await until("the press is heard") { self.seen.ways == [.scan] }
        try await run("group().querySelector('[data-way=paste]').click()")
        try await until("the press is heard") { self.seen.ways == [.scan, .paste] }
        // A device of theirs is being asked: nothing more to bring in.
        try await run("document.documentElement.dataset.pairingCode = '4821'")
        try await expect("group() === null", true)
        try await run("delete document.documentElement.dataset.pairingCode")
        try await expect("document.querySelector('#arrival-refusal .refusal-alt').firstElementChild === group()", true)
        // In: nothing of it is left.
        try await run("document.documentElement.dataset.entry = 'open'")
        try await expect("group() === null", true)
    }

    func testAMacOffersTheLinkAloneInTheMiddleOfTheColumn() async throws {
        try await show(Self.entryPage(), offer: Self.mac)
        try await run("refuse('refusal')")
        try await expect("[...group().querySelectorAll('button')].map(press => press.textContent).join(' | ')", "Paste link")
        try await expect("""
        (() => {
          const column = box(document.querySelector('.refusal-alt')), press = box(group().querySelector('button'));
          return [press.width >= 136 && press.width < column.width / 2,
            Math.abs((press.left + press.right) - (column.left + column.right)) < 2].join(' ');
        })()
        """, "true true")
        try await run("group().querySelector('button').click()")
        try await until("the press is heard") { self.seen.ways == [.paste] }
    }

    func testALinkThatIsRefusedIsSaidOnTheEntryPage() async throws {
        try await show(Self.entryPage(), offer: Self.phone, width: 402)
        // No entry page, no place to say it: the words are the app's to say.
        let early = await host.say(onEntryPage: "The clipboard holds no link into https://tilo.test.")
        XCTAssertFalse(early)
        try await run("refuse('refusal')")
        try await expect("group() !== null", true)
        let said = await host.say(onEntryPage: "The clipboard holds no link into https://tilo.test.")
        XCTAssertTrue(said)
        try await expect("group().querySelector('[role=alert]').textContent", "The clipboard holds no link into https://tilo.test.")
        try await expect("box(group().querySelector('[role=alert]')).top >= box(group().querySelector('button')).bottom", true)
        // The next press starts afresh.
        try await run("group().querySelector('[data-way=paste]').click()")
        try await expect("group().querySelector('[role=alert]').textContent", "")
    }

    func testARefusalOfAnotherKindKeepsTheWaysInUnderItsActions() async throws {
        try await show(Self.entryPage(), offer: Self.phone, width: 402)
        try await run("refuse('failure')")
        try await expect("group().previousElementSibling.className", "failure-actions")
        try await expect("""
        box(group()).top >= box(document.querySelector('.failure-actions')).bottom + 16
          && Math.abs(box(group()).width - box(document.querySelector('.failure-main')).width) < 1
        """, true)
    }

    func testOnAPageWhoseEntryIsALineTheWaysInStandUnderThatLine() async throws {
        try await show(Self.oneAppPage, offer: Self.phone, width: 402, at: "https://tilo.test/app/#/me/so.tilo.assistant")
        try await run("refuse('Lounge is invite-only for now. Ask someone in it for an invite link, then open it here.')")
        try await expect("group().parentNode === document.body", true)
        let under = """
        (() => {
          const line = box(document.getElementById('status')), own = box(group());
          return [own.top >= line.bottom + 16 && own.top <= line.bottom + 24,
            Math.abs((own.left + own.right) - (line.left + line.right)) < 2,
            own.left >= 24 && own.right <= innerWidth - 24 && own.bottom <= innerHeight].join(' ');
        })()
        """
        try await expect(under, "true true true")
        // The line grows: what stands under it moves down with it.
        try await run("document.getElementById('status').textContent += ' ' + 'Tilo keeps this device out until someone lets it in. '.repeat(4)")
        try await expect(under, "true true true")
        try await run("document.documentElement.dataset.entry = 'open'")
        try await expect("group() === null", true)
    }

    func testAnEntryPageThatDrawsTheWaysInItselfIsLeftAlone() async throws {
        try await show(Self.entryPage(drawsItsOwn: true), offer: Self.phone, width: 402)
        try await run("refuse('refusal')")
        try await Task.sleep(for: .milliseconds(150))
        try await expect("group() === null", true)
        // Such an entry page reads what this device has, and asks for it by name.
        try await expect("__tiloShell.ways.join(' ')", "scan paste")
        try await expect("[__tiloShell.wayIn('paste'), __tiloShell.wayIn('open'), __tiloShell.wayIn()].join(' ')", "true false false")
        try await until("the entry page's own press is heard") { self.seen.ways == [.paste] }
        // The shell has no place of its own there to say a refusal in.
        let said = await host.say(onEntryPage: "No link.")
        XCTAssertFalse(said)
    }

    func testAShellWithNothingToOfferLeavesTheEntryPageAsItIs() async throws {
        try await show(Self.entryPage(), offer: nil)
        try await run("refuse('refusal')")
        try await Task.sleep(for: .milliseconds(150))
        try await expect("group() === null && document.querySelector('style + style') === null", true)
        try await expect("[__tiloShell.ways.length, __tiloShell.wayIn('paste')].join(' ')", "0 false")
        XCTAssertEqual(seen.ways, [])
    }
}
#endif
