#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// An event the person's assistant set up, as its card in the main chat: against the page that
/// speaks as Chat's does, and the server behind it that keeps the person's events
/// (`TiloSourceTests.page`). No window is opened, nothing is fetched, nothing kept.
@MainActor
final class EventCardTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var listed: [Automation] = []
    private var listening: [Task<Void, Never>] = []

    private func start() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        source = TiloSource(engine: engine, mainChat: .agent(id: "assistant", name: "Assistant"), livesInOwn: false, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
            Task { for await now in source.automations() { self.listed = now } },
        ]
        engine.view.loadHTMLString(TiloSourceTests.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

    /// What the tests say to the page: it reads a conversation whole and names members by
    /// their kind; `wrote` is a message of hers, `answer` the assistant's reply with the work
    /// Chat keeps on it (begun at `from`, ended at `to`, or still going), `ends` that work's end.
    private static let words = """
        window.byKind = true;
        const work = (state, since) => ({stream: null, state: {state: {$variant: state, did: '', about: ''}, since}});
        window.wrote = (id, text, ts) => direct({id, ts, author: {person: ADA}, text});
        window.answer = (id, text, from, to) => direct({id, ts: from, author: {agent: 'assistant'}, text,
          work: work(to ? 'done' : 'doing', to ?? from), stopped: null});
        window.ends = (id, to) => { window.messages.find(one => one.id === id).work = work('done', to); touch(); }
        """

    private func inPage(_ script: String) async throws {
        _ = try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func page(_ script: String) async throws -> String {
        try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page) as? String ?? ""
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<240 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited twelve seconds for \(what)")
        throw CancellationError()
    }

    private var held: [Item] {
        var conversation = Conversation()
        for change in changes { conversation.apply(change) }
        return conversation.items
    }

    private var names: [String] {
        held.map { $0.id.value.replacingOccurrences(of: "chat.messages/messages/", with: "") }
    }

    /// How often her server was asked for her events.
    private func eventsAsked() async -> Int {
        Int((try? await page("return String(window.asks.filter(what => what === 'tilo.events list').length)")) ?? "") ?? -1
    }

    private func enter(_ before: String) async throws {
        try await inPage("\(Self.words); \(before); location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
    }

    func testAnEventStandsAsItsCardUnderTheAnswerItWasSetUpInWithWhatItHas() async throws {
        try await start()
        try await enter("""
            wrote('m1', 'Tell me when a supplier writes', 1794900100);
            answer('m2', 'Done. I tell you each third time.', 1794900101, 1794900110);
            wrote('m3', 'Thanks', 1794900200);
            answer('m4', 'Gladly.', 1794900201, 1794900203);
            window.events = [
              {id: 'e-old', name: 'Morning summary', sort: 'scheduled', when: 'every day at 07:30', instruction: 'Say what matters today',
               on: true, permits: [], setUpAtMs: 1794800000000},
              {id: 'e-1', name: 'When a supplier writes', sort: 'event', when: 'when something new is in "tilo.accounts.mail"',
               onlyIf: 'The sender is a supplier', every: 3, instruction: 'Tell me who wrote', on: true, permits: [],
               setUpAtMs: 1794900105500},
              {id: 'e-apart', name: 'Set up during no answer', sort: 'event', when: 'when something new is in "tilo.accounts.mail"',
               instruction: 'Sort it', on: false, permits: [], setUpAtMs: 1794900150000}]
            """)
        try await until("the event's card stands in the conversation") { self.names.contains("m2#event:e-1") }
        // The card is a piece of its own under the answer during which the event was set up.
        XCTAssertEqual(names, ["m1", "m2", "m2#event:e-1", "m3", "m4"])
        guard case .event(let card) = held[2].content else { return XCTFail("no card under the answer") }
        XCTAssertEqual(card.automation, Automation(id: "e-1", sort: .event, title: "When a supplier writes", summary: "Tell me who wrote",
                                                   filter: "The sender is a supplier", threshold: 3, isOn: true))
        XCTAssertEqual(card.text, "", "the card says nothing of its own: the answer above it is the assistant's")
        // The answer stays what the assistant wrote.
        guard case .message(let answer) = held[1].content else { return XCTFail("the answer is no message any more") }
        XCTAssertEqual(answer.text, "Done. I tell you each third time.")
        XCTAssertEqual(answer.author, .assistant)
        // What was set up before anything held, and what was set up during no answer, stand in the list alone.
        try await until("her events are listed") { self.listed.count == 3 }
        XCTAssertEqual(listed.map(\.id.value), ["e-old", "e-1", "e-apart"])
    }

    func testTheCardComesWhenTheAnswerThatSetTheEventUpEndsAndGoesWhenTheEventIsGone() async throws {
        try await start()
        try await enter("""
            window.events = [];
            wrote('m1', 'Sum up every ten mails', 1794900100);
            answer('m2', 'I am setting it up.', 1794900101)
            """)
        try await until("the answer is being written") { self.changes.contains(.answering(true)) }
        try await until("her events were asked for on the way in") { await self.eventsAsked() == 1 }
        XCTAssertEqual(names, ["m1", "m2"])
        // The assistant sets the event up in its answer, and the answer ends.
        try await inPage("""
            window.events = [{id: 'e-7', name: 'Every ten new mails', sort: 'event', when: 'when something new is in "tilo.accounts.mail"',
              every: 10, instruction: 'One summary', on: true, permits: [], setUpAtMs: 1794900104000}];
            ends('m2', 1794900106)
            """)
        try await until("the card stands under the answer") { self.names == ["m1", "m2", "m2#event:e-7"] }
        guard case .event(let card) = held[2].content else { return XCTFail("no card") }
        XCTAssertEqual(card.automation.title, "Every ten new mails")
        XCTAssertEqual(card.automation.threshold, 10)
        try await until("the Tasks panel lists it too") { self.listed.map(\.id.value) == ["e-7"] }
        // Her server was asked for her events once more, at the answer's end, and not on a clock.
        try await Task.sleep(for: .milliseconds(400))
        let asked = await eventsAsked()
        XCTAssertEqual(asked, 2)
        // The event is removed elsewhere: the end of the next answer says so here.
        try await inPage("""
            window.events = [];
            wrote('m3', 'Thanks', 1794900300);
            answer('m4', 'Gladly.', 1794900301)
            """)
        try await until("the next answer is being written") { self.names == ["m1", "m2", "m2#event:e-7", "m3", "m4"] }
        try await inPage("ends('m4', 1794900303)")
        try await until("the card is gone") { self.names == ["m1", "m2", "m3", "m4"] }
        try await until("and the list is empty") { self.listed.isEmpty }
    }

    /// The card of an event, wherever it stands among the pieces held.
    private func card(of event: String) -> EventCard? {
        for item in held where item.id.value.hasSuffix("#event:" + event) {
            if case .event(let card) = item.content { return card }
        }
        return nil
    }

    /// What was asked of her server that changes an event, in order.
    private func calls() async throws -> String {
        try await page("return JSON.stringify(window.eventCalls ?? [])")
    }

    func testShowTheAnswerShowsWhatTheEventKeepsReadFreshAndCallsNothingThatChangesIt() async throws {
        try await start()
        try await enter("""
            wrote('m1', 'Answer every supplier that we buy in spring', 1794900100);
            answer('m2', 'Done. I answer in your words and tell you afterwards.', 1794900101, 1794900110);
            window.events = [
              {id: 'e-1', name: 'When a supplier writes', sort: 'event', when: 'when something new is in "tilo.accounts.mail"',
               onlyIf: 'The sender is a supplier', instruction: 'Answer that we are not buying before spring, then tell me', on: true,
               permits: [{effect: 'allow', action: 'so.tilo.mail#send'}], setUpAtMs: 1794900105000,
               note: {instruction: '', style: 'Short and warm', guidance: '', rejected: [], kept: null,
                      accepted: ['Thank you, we will come back to you.', 'Thanks for writing. We are not buying before spring.']}},
              {id: 'e-2', name: 'Every ten new mails', sort: 'event', when: 'when something new is in "tilo.accounts.mail"', every: 10,
               instruction: 'One summary', on: true, permits: [], setUpAtMs: 1794900106000,
               note: {instruction: '', style: '', guidance: '', accepted: [], rejected: [], kept: null}}]
            """)
        try await until("both cards stand under the answer") { self.names == ["m1", "m2", "m2#event:e-1", "m2#event:e-2"] }
        let first = try XCTUnwrap(card(of: "e-1"))
        // What may be pressed is what the event allows: one that may act alone can be told to ask each time.
        XCTAssertEqual(first.choices.map(\.title), ["Show the answer", "Ask me each time", "Undo"])
        XCTAssertEqual(card(of: "e-2")?.choices.map(\.title), ["Show the answer", "Undo"])
        XCTAssertNil(first.shown)
        // The assistant changed how the event writes since the app last asked: what is shown is read fresh.
        try await inPage("window.events[0].note.style = 'Two sentences at most'")
        let asked = await eventsAsked()
        try await source.answer(first.question, with: "show")
        try await until("what the event keeps is shown") { self.card(of: "e-1")?.shown != nil }
        XCTAssertEqual(card(of: "e-1")?.shown, """
            **What it is told to do**

            Answer that we are not buying before spring, then tell me

            **How it writes**

            Two sentences at most

            **An answer you accepted**

            Thanks for writing. We are not buying before spring.
            """)
        XCTAssertEqual(card(of: "e-1")?.choices.map(\.title), ["Ask me each time", "Undo"])
        // An event that keeps nothing for its answers says so.
        try await source.answer(EventCards.question(of: "e-2"), with: "show")
        try await until("the second says what it keeps") { self.card(of: "e-2")?.shown != nil }
        XCTAssertEqual(card(of: "e-2")?.shown, """
            **What it is told to do**

            One summary

            It keeps no answer of its own yet. It writes one each time from what it is told.
            """)
        // Showing asked her server for the list each time and called nothing that changes an event.
        let after = await eventsAsked()
        XCTAssertEqual(after, asked + 2)
        let called = try await calls()
        XCTAssertEqual(called, "[]")
    }

    func testAskMeEachTimeTakesBackEveryPermissionAndUndoRemovesTheEventAndSaysSo() async throws {
        try await start()
        try await enter("""
            wrote('m1', 'Answer every supplier that we buy in spring', 1794900100);
            answer('m2', 'Done.', 1794900101, 1794900110);
            window.events = [{id: 'e-1', name: 'When a supplier writes', sort: 'event', when: 'when something new is in "tilo.accounts.mail"',
              instruction: 'Answer, then tell me', on: true, setUpAtMs: 1794900105000,
              permits: [{effect: 'allow', action: 'so.tilo.mail#send'}, {effect: 'allow', action: 'so.tilo.mail#reply'}]}]
            """)
        try await until("the card stands under the answer") { self.card(of: "e-1") != nil }
        let question = EventCards.question(of: "e-1")
        XCTAssertEqual(card(of: "e-1")?.automation.actsAlone, true)
        try await source.answer(question, with: "ask")
        try await until("the card says it asks each time") { self.card(of: "e-1")?.settled == .asksEachTime }
        // Each permission was taken back at her server by its own rule, and nothing else was called.
        var called = try await calls()
        XCTAssertEqual(called, #"["withdraw-permission e-1 so.tilo.mail#send","withdraw-permission e-1 so.tilo.mail#reply"]"#)
        XCTAssertEqual(card(of: "e-1")?.automation.actsAlone, false)
        XCTAssertEqual(card(of: "e-1")?.choices.map(\.title), ["Show the answer", "Undo"])
        try await until("the Tasks panel says the same") { self.listed.first?.actsAlone == false }
        // Undo removes the event at her server; the card stays and says so, with nothing left to press.
        try await source.answer(question, with: "undo")
        try await until("the card says it is undone") { self.card(of: "e-1")?.settled == .undone }
        called = try await calls()
        XCTAssertTrue(called.hasSuffix(#","remove e-1"]"#), called)
        XCTAssertEqual(card(of: "e-1")?.choices, [])
        XCTAssertEqual(names, ["m1", "m2", "m2#event:e-1"])
        try await until("the Tasks panel lists it no more") { self.listed.isEmpty }
        let kept = try await page("return String(window.events.length)")
        XCTAssertEqual(kept, "0")
        // What is no answer of a card is refused, and nothing is called for it.
        do {
            try await source.answer(question, with: "nonsense")
            XCTFail("something that is no answer was taken")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("This is not one of the answers"))
        }
        let last = try await calls()
        XCTAssertEqual(last, called)
    }

    func testAnAnswerLeftOpenEndsWhereTheNextBeganAndTwoEventsOfOneAnswerEachHaveTheirCard() throws {
        var cards = EventCards()
        func answer(_ name: String, from: Int, to: Int?) throws -> Item {
            let work = try XCTUnwrap(JSON(text: #"{"stream":null,"state":{"state":{"$variant":"\#(to == nil ? "doing" : "done")"},"since":\#(to ?? from)}}"#))
            let began = Date(timeIntervalSince1970: TimeInterval(from))
            cards.see(work: work, began: began, going: to == nil, as: .init(name))
            return Item(id: .init(name), at: began, .message(Message(.assistant, name)))
        }
        func event(_ id: String, at seconds: Int) throws -> JSON {
            try XCTUnwrap(JSON(text: #"{"id":"\#(id)","name":"\#(id)","sort":"event","when":"w","instruction":"i","on":true,"permits":[],"setUpAtMs":\#(seconds * 1000)}"#))
        }
        // The first answer's work never ended; the second began at 200 and ended at 210.
        let pieces = [try answer("a1", from: 100, to: nil), try answer("a2", from: 200, to: 210)]
        cards.take([try event("x", at: 150), try event("y", at: 300), try event("z1", at: 205), try event("z2", at: 206)])
        let carded = cards.carded(pieces)
        XCTAssertEqual(carded.map(\.id.value), ["a1", "a1#event:x", "a2", "a2#event:z1", "a2#event:z2"])
        // Said again, a card stands once.
        XCTAssertEqual(cards.carded(carded), carded)
        // With no event left, the pieces are the answers again.
        cards.take([])
        XCTAssertEqual(cards.carded(carded), pieces)
    }
}
#endif
