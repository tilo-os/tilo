#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// Why a reply's model gave no answer, as the app reads it: the typed cause the person's server
/// keeps with the reply's work, against the page that speaks as Chat's does
/// (`TiloSourceTests.page`). No word of a reply is read. No window is opened, nothing is
/// fetched, nothing kept; every name is made up.
@MainActor
final class ReplyCauseTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
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

    /// What each message held carries as its typed cause, oldest first.
    private var faults: [Message.KeyFault?] {
        held.map { item in if case .message(let message) = item.content { message.keyFault } else { nil } }
    }

    func testOnlyACauseAnotherKeyOrAnotherProviderMendsIsCarried() {
        let named: JSON = ["id": "p-7", "name": "OpenRouter"]
        func fault(_ kind: String, _ provider: JSON? = nil) -> Message.KeyFault? {
            var cause: [String: JSON] = ["kind": .string(kind), "recovery": "sign_in"]
            if let provider { cause["provider"] = provider }
            return TiloSource.keyFault(of: ["state": ["state": "provider_finished"], "failure_cause": .object(cause)])
        }
        // The key, or the key's account: the provider as their server names it.
        for kind in ["authentication", "permission_denied", "credits_exhausted"] {
            XCTAssertEqual(fault(kind, named), .key("p-7", name: "OpenRouter"), kind)
            XCTAssertNil(fault(kind), "\(kind) with no provider named: there is nothing to type a key for")
        }
        XCTAssertEqual(fault("authentication", ["id": "p-7"]), .key("p-7", name: "p-7"), "a provider given no name is named by its id")
        XCTAssertEqual(fault("no_provider"), .noProvider)
        XCTAssertEqual(fault("no_provider", named), .noProvider)
        // Every other kind their server names is nothing another key mends, and neither is one it names tomorrow.
        for kind in ["rate_limited", "context_limit", "provider", "timed_out", "transport", "unavailable", "invalid_request",
                     "invalid_response", "unsupported", "conflict", "cancelled", "truncated", "refused", "named_tomorrow"] {
            XCTAssertNil(fault(kind, named), kind)
        }
        // Work that says no cause, as every server before the field says it.
        XCTAssertNil(TiloSource.keyFault(of: nil))
        XCTAssertNil(TiloSource.keyFault(of: ["state": ["state": "provider_finished"], "failure_detail": "Provider credits exhausted"]))
        XCTAssertNil(TiloSource.keyFault(of: ["failure_cause": .null]))
    }

    func testAReplyCarriesTheCauseItsWorkSaysAndNothingIsReadFromItsWords() async throws {
        try await start()
        _ = try await source.engine.view.callAsyncJavaScript("""
            window.byKind = true;
            const failed = cause => ({stream: null, state: {state: {$variant: 'provider_finished', did: '', about: ''}, since: 1794900002},
              failure_detail: null, ...(cause ? {failure_cause: cause} : {})});
            const openrouter = {id: 'p-7', name: 'OpenRouter'};
            const said = (id, text, cause) => direct({id, author: {agent: 'assistant'}, text, work: failed(cause), stopped: null});
            direct({id: 'm1', author: {person: ADA}, text: 'Hello?'});
            said('r1', 'My model did not answer: Provider credits exhausted', null);
            said('r2', 'These words name no cause.', {kind: 'credits_exhausted', recovery: 'add_credits', provider: openrouter});
            said('r3', 'Nor do these.', {kind: 'authentication', recovery: 'sign_in', provider: openrouter});
            said('r4', 'Nor these.', {kind: 'permission_denied', recovery: 'sign_in', provider: openrouter});
            said('r5', 'Nor these.', {kind: 'no_provider', recovery: 'add_provider'});
            said('r6', 'Provider credits exhausted', {kind: 'rate_limited', recovery: 'wait_and_retry', provider: openrouter});
            said('r7', 'Nor these.', {kind: {$variant: 'credits_exhausted'}, recovery: {$variant: 'add_credits'}, provider: openrouter});
            direct({id: 'r8', author: {agent: 'assistant'}, text: 'Good morning, Ada'});
            location.hash = '#/w/ws-1/so.tilo.chat'; enter(false)
            """, arguments: [:], in: nil, contentWorld: .page)
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation with the assistant is held") { self.held.count == 9 }
        let openrouter = Message.KeyFault.key("p-7", name: "OpenRouter")
        // Her own message; a failure that says its cause in words alone; the three kinds of a key's
        // fault; no provider; a kind no key mends, whatever its words; a kind said as a variant; an answer.
        XCTAssertEqual(faults, [nil, nil, openrouter, openrouter, openrouter, .noProvider, nil, openrouter, nil])
    }
}
#endif
