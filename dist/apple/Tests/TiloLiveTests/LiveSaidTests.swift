#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// What a live thing's place says where its thing cannot be shown, against a page that refuses
/// the way Tilo's does. No window is opened, nothing is fetched and nothing is kept.
@MainActor
final class LiveSaidTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var engine: TiloEngine!
    private var stage: LiveStage!
    private let view = TiloLive.LiveThing(id: "m_reply_7", title: "Word Processor",
                                          .messageView(message: "m_reply_7", conversation: "c_main"))

    /// As Tilo's page. Chat's own mount lists what it offers of a message: on a site that has
    /// views in messages, the action that opens the view. The Chat mounted beside for one view
    /// holds no message here, and refuses the view's action in the page's own words, which
    /// name an address and an action. `how` is `old` for a site without views in messages,
    /// `early` for one that has them, `closed` for one that lets nothing be opened beside.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span>
    <script>
      window.enter = how => {
        const item = 'chat.workspace/messages/m_reply_7';
        const own = [{address: {resource: item, action: 'reveal-message'}, view: {id: 'message', open_action: 'reveal-message', primary: true}}];
        if (how === 'early') own.push({address: {resource: item, action: 'show-view'}, view: {id: 'view', open_action: 'show-view'}});
        globalThis.tiloOpen = async () => {
          if (how === 'closed') throw new Error('not_permitted: Editors may not open chat.workspace');
          return {
            semantics: document.createElement('div'), state: () => ({phase: 'presented'}), actions: () => [],
            invokeAction: async (resource, action) => { throw new Error(`unknown_action: no action addressed by (${resource}, ${action})`); },
            cleanup: async () => {},
          };
        };
        document.documentElement.dataset.arrival = 'done';
        document.documentElement.dataset.entry = 'open';
        globalThis.tiloMount = {semantics: document.createElement('div'), actions: () => own, invokeAction: async () => ({state: 'completed'})};
      };
    </script></html>
    """#

    private func enter(_ how: String) async throws {
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        self.engine = engine
        stage = LiveStage(engine: engine)
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
        _ = try await engine.view.callAsyncJavaScript("location.hash = '#/w/ws-1/so.tilo.chat'; enter(how)", arguments: ["how": how],
                                                      in: nil, contentWorld: .page)
        try await until("the engine is in") { if case .open = engine.state { true } else { false } }
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<240 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited twelve seconds for \(what)")
        throw CancellationError()
    }

    /// What the view's place says now, in the app's sentences.
    private var place: TiloLiveView.Unshown? { TiloLiveView.unshown(view, on: stage, says: TiloSource.said) }

    /// Nothing of Tilo's refusal as it came: no address, no id, no action's name, no code.
    private func assertAPersonsWords(_ sentence: String, line: UInt = #line) {
        for raw in ["chat.workspace", "m_reply_7", "show-view", "unknown_action", "not_permitted", "addressed", "Editors", "(", "/", "_"] {
            XCTAssertFalse(sentence.contains(raw), "the place says “\(sentence)”, which holds “\(raw)”", line: line)
        }
    }

    func testAViewThisTiloDoesNotOfferIsSaidSoInAPersonsWordsWithNoTryAgain() async throws {
        try await enter("old")
        await stage.bring(view)
        // Tilo's refusal as it came is kept for a program's log. It is no sentence for a person.
        XCTAssertEqual(stage.state(of: view), .failed("no action addressed by (chat.workspace/messages/m_reply_7, show-view)"))
        XCTAssertEqual(stage.troubles[view.id], .notOffered)
        let said = try XCTUnwrap(place)
        XCTAssertEqual(said.sentence, AssistantWords.viewNotOffered.said)
        XCTAssertEqual(said.sentence, "This Tilo cannot show views in messages yet")
        assertAPersonsWords(said.sentence)
        // Asking again would be answered the same, so it is not offered.
        XCTAssertFalse(said.asksAgain)
        // An app that brings no sentences has this package's own, and never the refusal either.
        XCTAssertEqual(TiloLiveView.unshown(view, on: stage, says: nil),
                       TiloLiveView.Unshown(sentence: "This Tilo cannot show this yet", asksAgain: false))
    }

    func testAViewThatDidNotComeWhereTiloOffersViewsIsSaidSoAndMayBeAskedForAgain() async throws {
        try await enter("early")
        await stage.bring(view)
        XCTAssertEqual(stage.state(of: view), .failed("no action addressed by (chat.workspace/messages/m_reply_7, show-view)"))
        // Chat's own page offers this message's view: the site has views, and this one did not come.
        XCTAssertEqual(stage.troubles[view.id], .notShown)
        let said = try XCTUnwrap(place)
        XCTAssertEqual(said.sentence, AssistantWords.liveNotShown.said)
        XCTAssertEqual(said.sentence, "Tilo could not show this")
        assertAPersonsWords(said.sentence)
        XCTAssertTrue(said.asksAgain)
    }

    func testAnyOtherRefusalIsSaidInTheAppsWordsAndNeverAsItCame() async throws {
        try await enter("closed")
        await stage.bring(view)
        XCTAssertEqual(stage.state(of: view), .failed("Editors may not open chat.workspace"))
        XCTAssertEqual(stage.troubles[view.id], .notShown)
        let said = try XCTUnwrap(place)
        XCTAssertEqual(said.sentence, "Tilo could not show this")
        assertAPersonsWords(said.sentence)
        XCTAssertTrue(said.asksAgain)
    }
}
#endif
