#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// A service the person's assistant proposes to connect, against a page that speaks as Chat's
/// does and a server that keeps the person's connections as theirs does. No window is opened,
/// nothing is fetched, nothing kept. The service, its addresses and what its sign-in comes
/// back with are made up.
@MainActor
final class TiloConnectionsTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var defaults: UserDefaults!
    private var suite = ""
    private var loaded: [URL] = []
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var listening: [Task<Void, Never>] = []

    private func start() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.loaded.append($0) }
        source = TiloSource(engine: engine, mainChat: .conversation(id: "c_chat", name: "chat"), livesInOwn: false, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
        ]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
        // The field Chat keeps a message's conversation under, by the name the app's own reader asks for it.
        _ = try await page("window.keptIn = name; return ''", ["name": ChatMessage.drawn[0]])
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

    /// As Tilo's one-app page of Chat: its mount lists and reads Chat's messages and takes the
    /// answer to a question. It hands a native app its device's entry, behind
    /// which a server keeps the person's connections: it lists them, begins and finishes a
    /// sign-in and removes one, and on the person's yes to a proposal it adds the service it
    /// looked at and says so on the question, as the person's server does.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      window.messages = [];
      // What Chat's own actions were asked, and what her server was asked as her device.
      window.heard = [];
      window.asked = [];
      window.connections = [];
      window.vault = [];
      const said = value => ({state: 'completed', output: {value}});
      const touch = () => { document.getElementById('semantics').textContent = String(Math.random()); };
      // Every message of these tests stands in the one conversation the app is given as its
      // main chat, under the field Chat keeps a message's conversation in (`window.keptIn`,
      // handed in by the name the app's own reader asks for it).
      window.post = message => {
        window.messages.push({ts: 1794900000 + window.messages.length, [window.keptIn]: 'c_chat', question: null, share: null, attachments: [], ...message});
        touch();
      };
      const unknown = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      // A service as her server keeps one.
      window.kept = (id, address, more = {}) => ({id, address, name: 'Orchard Tools', addedAtMs: 1794900000000, state: 'ready',
        stateSinceMs: 1794900000000, signIn: 'none',
        commands: [{name: 'find_trees', title: 'Find trees', description: 'Lists the trees of an orchard.', readOnly: true, asks: false, changed: false, schema: {}}],
        commandsReadAtMs: 1794900000000, unusable: 0, ...more});
      // What her server puts into her conversation when her assistant proposes a service: its
      // own words, and the card it keeps as the question's detail.
      window.propose = (id, card, more = {}) => window.post({id, author: 'agent-1',
        text: 'Connect the service at ' + new URL(card.address).hostname + '?',
        question: {kind: 'action', person: ADA, answers: [{id: 'allow', label: ''}, {id: 'deny', label: ''}],
                   detail: JSON.stringify({ask: 'ask-' + id, connection: card}), answered: null, outcome: null, withdrawn: null, ...more}});
      // Her server carries her answer out: on her yes it adds the service it looked at when it
      // asked, and it says on the question what came of it. `window.fails` is why it could not.
      window.settle = id => {
        const asked = window.messages.find(message => message.id === id);
        const card = JSON.parse(asked.question.detail).connection;
        const outcome = (word, detail = '') => { asked.question = {...asked.question, outcome: {outcome: word, at: 1794900101, detail}}; touch(); };
        if (asked.question.answered.answer !== 'allow') return outcome('dropped');
        if (window.fails) return outcome('failed', window.fails);
        window.connections.push(window.kept('c' + (window.connections.length + 1), card.address, {name: card.name ?? '',
          ...(card.needsSignIn ? {state: 'needs-sign-in', signIn: 'needed', signInAt: card.signInAt ?? null, commands: []} : {})}));
        outcome('done');
      };
      const device = Object.freeze({
        person: ADA, workspace: 'ws-1',
        catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => unknown(),
        invoke: async (target, argument) => {
          const kind = target.provider + ' ' + target.address.action;
          window.asked.push({kind, resource: target.address.resource, argument: argument ?? null});
          const done = value => ({invocation: 'i', state: 'succeeded', output: {value, resources: []}});
          const refused = why => { throw Object.assign(new Error(why), {code: 'refused'}); };
          const one = window.connections.find(kept => kept.id === argument?.connection);
          switch (kind) {
            case 'tilo.connections list': return done({connections: window.connections});
            case 'tilo.connections begin-sign-in':
              if (!one) refused('No such connection');
              window.begun = {connection: one.id, returnTo: argument.returnTo};
              return done({open: 'https://accounts.orchard.example/authorize?client_id=made-up&state=s-1&redirect_uri=' + encodeURIComponent(argument.returnTo),
                expiresAtMs: 1794900600000});
            case 'tilo.connections finish-sign-in': {
              // Tried once: for the sign-in that was begun, at the place it was begun with.
              const begun = window.begun;
              window.begun = null;
              if (!one || begun?.connection !== one.id || !argument.returned.startsWith(begun.returnTo + '?')
                  || new URL(argument.returned).searchParams.get('state') !== 's-1') refused('That sign-in cannot be finished. Begin it again.');
              Object.assign(one, {state: 'ready', signIn: 'signed-in', commands: window.kept('', '').commands});
              window.vault.push({kind: 'key', belongsTo: [one.address]});
              return done(one);
            }
            case 'tilo.connections remove':
              if (!one) refused('No such connection');
              window.connections = window.connections.filter(kept => kept !== one);
              return done({connections: window.connections});
            case 'tilo.vault list':
              return done({items: window.vault.map((item, at) => ({reference: 'vault.item:0' + at, kind: item.kind, belongsTo: item.belongsTo, addedAt: 1794900000}))});
            default: return unknown();
          }
        },
      });
      window.enter = async () => {
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it.
        await giveDevice(device);
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          actions: () => [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages'}],
          invokeAction: async (resource, action, args) => {
            window.heard.push({resource, action, args});
            const item = resource.startsWith('chat.messages/messages/') ? window.messages.find(m => m.id === resource.split('/').pop()) : null;
            if (resource === 'chain:messages' && action === 'read') {
              return said({kind: 'chat.messages', messages: [...window.messages].reverse().slice(0, args.pageSize ?? 50)
                .map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text}))});
            }
            if (item && action === 'read') return said(item);
            if (resource === 'chat.workspace/people/' + ADA && action === 'read') return said({id: ADA, name: 'Ada Lovelace'});
            if (resource === 'chain:messages' && action === 'answer-question') {
              const asked = window.messages.find(m => m.id === args.message);
              if (!asked?.question) throw new Error('unknown_item: no such question');
              if (asked.question.answered) throw new Error('answered: This question was answered already');
              asked.question = {...asked.question, answered: {answer: args.answer, device: '', at: 1794900100, detail: args.detail ?? null}};
              touch();
              // Her server reads her answer a moment later (`window.carries`: after how long, or never).
              if (window.carries !== 'never') setTimeout(() => window.settle(args.message), window.carries ?? 0);
              return said(null);
            }
            throw new Error(`unknown_action: no action addressed by (${resource}, ${action})`);
          },
        };
      };
    </script></html>
    """#

    /// A service that says its name and where its person signs in, and lists nothing before that.
    private static let orchard = "{address: 'https://orchard.example/mcp', name: 'Orchard Tools', needsSignIn: true, signInAt: 'accounts.orchard.example'}"
    /// One that wants no sign-in, gives itself no name and lists seven commands.
    private static let open = "{address: 'https://open.example/mcp', needsSignIn: false, commands: 7}"

    private func inPage(_ script: String) async throws {
        _ = try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func page(_ script: String, _ arguments: [String: Any] = [:]) async throws -> String {
        try await source.engine.view.callAsyncJavaScript(script, arguments: arguments, in: nil, contentWorld: .page) as? String ?? ""
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

    /// The cards the conversation holds, oldest first.
    private var cards: [ConnectionOffer] {
        held.compactMap { if case .connection(let offer) = $0.content { offer } else { nil } }
    }

    private func enter(_ before: String = "") async throws {
        try await inPage("\(before); location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
    }

    /// The one card of the conversation. A question about a service that is drawn as words over
    /// its answers is none.
    private func card() throws -> ConnectionOffer {
        try XCTUnwrap(cards.first, "the question about a service is no card: \(held.map(\.content))")
    }

    /// What her server was asked of one action of her connections, as JSON.
    private func asked(_ action: String) async throws -> JSON? {
        JSON(text: try await page("return JSON.stringify(window.asked.filter(one => one.kind === 'tilo.connections ' + action).map(one => one.argument))",
                                  ["action": action]))
    }

    /// The answers Chat's own action was given, as JSON.
    private func answers() async throws -> JSON? {
        JSON(text: try await page("return JSON.stringify(window.heard.filter(call => call.action === 'answer-question').map(call => call.args))"))
    }

    private func refused(_ sentence: String, _ call: () async throws -> Void) async {
        do {
            try await call()
            XCTFail("taken for done: \(sentence)")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused(sentence))
        } catch {
            XCTFail("\(error)")
        }
    }

    func testAServiceHerAssistantProposesIsItsCardWithWhatHerServerKeptOfIt() async throws {
        try await start()
        try await enter("""
            propose('p1', \(Self.orchard));
            propose('p2', \(Self.open));
            propose('p3', {address: 'https://orchard.example/tools', name: 'Orchard Tools', needsSignIn: true, commands: 23})
            """)
        XCTAssertEqual(cards.count, 3, "\(held.map(\.content))")
        guard cards.count == 3 else { return }
        // The service's own name, where it signs in, and her yes worded for a sign-in. The words
        // over the card are her server's.
        XCTAssertEqual(cards[0], ConnectionOffer(id: "chat.messages/messages/p1", text: "Connect the service at orchard.example?", name: "Orchard Tools",
                                                 needsSignIn: true, signInAt: "accounts.orchard.example", state: .waiting))
        XCTAssertEqual(cards[0].note, "Sign in at accounts.orchard.example")
        XCTAssertEqual(cards[0].yes, "Authenticate")
        // A service that gave itself no name is called by the site of its address, and one that
        // wants no sign-in is only added.
        XCTAssertEqual(cards[1].name, "open.example")
        XCTAssertEqual(cards[1].note, "7 commands")
        XCTAssertEqual(cards[1].yes, "Add")
        XCTAssertEqual(cards[2].note, "23 commands after sign-in")
        // A card waits for its person as a question does.
        XCTAssertEqual(NoticePlan.notice(for: held[0], in: .main)?.kind, .decision)
    }

    func testNotNowIsHerNoAndHerServerAddsNothing() async throws {
        try await start()
        try await enter("propose('p1', \(Self.orchard))")
        let offer = try card()
        try await source.decline(offer.id)
        try await until("the card says it is not connected") { self.cards.first?.state == .declined }
        let given = try await answers()
        XCTAssertEqual(given, [["answer": "deny", "message": "p1"]])
        // Her server kept nothing and was asked to begin nothing.
        let kept = try await page("return JSON.stringify([window.connections.length, window.asked.filter(one => one.kind.startsWith('tilo.connections ') && !one.kind.endsWith(' list')).length])")
        XCTAssertEqual(kept, "[0,0]")
    }

    func testAuthenticateIsHerYesThenTheServicesOwnPageAndWhatComesBackGoesToHerServerAlone() async throws {
        try await start()
        try await enter("propose('p1', \(Self.orchard))")
        let offer = try card()
        let signInPage = try await source.authenticate(offer.id)
        // Her yes is Chat's own answer, given once.
        let given = try await answers()
        XCTAssertEqual(given, [["answer": "allow", "message": "p1"]])
        // The page is the service's own, begun for the connection her server added, and it
        // comes back to an address of this app's own.
        let begun = try await asked("begin-sign-in")
        XCTAssertEqual(begun, [["connection": "c1", "returnTo": "so.tilo.assistant:/signed-in"]])
        XCTAssertEqual(signInPage?.absoluteString.hasPrefix("https://accounts.orchard.example/authorize?"), true)
        XCTAssertEqual(signInPage.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false) }?.queryItems?.first { $0.name == "redirect_uri" }?.value,
                       "so.tilo.assistant:/signed-in")
        XCTAssertEqual(SignInReturn.address, "\(SignInReturn.scheme):/signed-in")
        // Added and not signed in to: the card still waits.
        try await until("the card waits for the sign-in") { self.cards.first?.state == .waiting }
        // She signs in at the service, and the system's sheet comes back with a code that opens
        // the sign-in once.
        let code = "made-up-code-that-opens-the-sign-in-once"
        let returned = try XCTUnwrap(URL(string: "so.tilo.assistant:/signed-in?code=\(code)&state=s-1"))
        try await source.signedIn(offer.id, at: returned)
        try await until("the card says connected") { self.cards.first?.state == .connected }
        // Her server got the whole address, for that connection.
        let finished = try await page("return JSON.stringify(window.asked.filter(one => one.kind.endsWith(' finish-sign-in')).map(one => [one.argument.connection, one.argument.returned === returned]))",
                                      ["returned": returned.absoluteString])
        XCTAssertEqual(finished, #"[["c1",true]]"#)
        // And nobody else: the code stands in no message, in nothing Chat's own actions were
        // asked, nowhere in the page, in nothing the app holds of the conversation or keeps.
        let leaked = try await page("return String(JSON.stringify([window.messages, window.heard]).includes(code)) + ' ' + String(document.documentElement.outerHTML.includes(code))",
                                    ["code": code])
        XCTAssertEqual(leaked, "false false")
        XCTAssertFalse(String(describing: held).contains(code))
        XCTAssertFalse(String(describing: changes).contains(code))
        XCTAssertFalse(String(describing: defaults.dictionaryRepresentation()).contains(code))
        // What it signs in with is an item of her vault, said by where it belongs.
        var seen = Supervision()
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        try await until("the vault's item is listed") { seen.logins.map(\.name) == ["orchard.example"] }
        supervision.cancel()
        // A sign-in is finished once.
        await refused("No sign-in was begun from this card") { try await self.source.signedIn(offer.id, at: returned) }
        let tries = try await asked("finish-sign-in")
        XCTAssertEqual(tries?.array?.count, 1)
    }

    func testAServiceThatWantsNoSignInIsAddedByHerYesAndNoPageIsOpened() async throws {
        try await start()
        try await enter("propose('p1', \(Self.open))")
        let offer = try card()
        let signInPage = try await source.authenticate(offer.id)
        XCTAssertNil(signInPage)
        try await until("the card says connected") { self.cards.first?.state == .connected }
        let begun = try await asked("begin-sign-in")
        XCTAssertEqual(begun, [])
        let kept = try await page("return JSON.stringify(window.connections.map(one => one.address))")
        XCTAssertEqual(kept, #"["https://open.example/mcp"]"#)
    }

    func testWhereHerServerCouldNotAddTheServiceThePressAndTheCardSayWhyInItsWords() async throws {
        try await start()
        try await enter("window.fails = 'The service at orchard.example did not answer'; propose('p1', \(Self.orchard))")
        let offer = try card()
        await refused("The service at orchard.example did not answer") { _ = try await self.source.authenticate(offer.id) }
        try await until("the card says it is not connected") { self.cards.first?.state == .declined }
        XCTAssertEqual(cards.first?.text, "Connect the service at orchard.example?\n\nThe service at orchard.example did not answer")
        let begun = try await asked("begin-sign-in")
        XCTAssertEqual(begun, [])
    }

    func testNotNowAfterHerYesAndASignInSheLeftTakesTheServiceOutAgain() async throws {
        try await start()
        try await enter("propose('p1', \(Self.orchard))")
        let offer = try card()
        let signInPage = try await source.authenticate(offer.id)
        XCTAssertNotNil(signInPage)
        // She closes the sign-in sheet: the card still offers both, and her no is not a second answer.
        try await until("the card waits for the sign-in") { self.cards.first?.state == .waiting }
        try await source.decline(offer.id)
        try await until("the card says it is not connected") { self.cards.first?.state == .declined }
        let removed = try await asked("remove")
        XCTAssertEqual(removed, [["connection": "c1"]])
        let given = try await answers()
        XCTAssertEqual(given, [["answer": "allow", "message": "p1"]])
        let kept = try await page("return String(window.connections.length)")
        XCTAssertEqual(kept, "0")
        // The sign-in that was begun is gone with it.
        let returned = try XCTUnwrap(URL(string: "so.tilo.assistant:/signed-in?code=made-up&state=s-1"))
        await refused("No sign-in was begun from this card") { try await self.source.signedIn(offer.id, at: returned) }
    }

    func testAServiceThatWasSignedInToIsNeverRemovedFromItsCard() async throws {
        try await start()
        // Answered and signed in to on another day; the service no longer takes that sign-in.
        try await enter("""
            window.connections.push(window.kept('c7', 'https://orchard.example/mcp', {state: 'sign-in-ran-out', signIn: 'needed'}));
            propose('p1', \(Self.orchard), {answered: {answer: 'allow', device: '', at: 1794900100, detail: null}, outcome: {outcome: 'done', at: 1794900101, detail: ''}})
            """)
        try await until("the card says connected") { self.cards.first?.state == .connected }
        try await source.decline(try card().id)
        let removed = try await asked("remove")
        XCTAssertEqual(removed, [])
        let kept = try await page("return String(window.connections.length)")
        XCTAssertEqual(kept, "1")
    }

    func testWhileHerServerHasNotAddedItThePressSaysSoAndTheNextPressGoesOnWithoutASecondYes() async throws {
        try await start()
        try await enter("window.carries = 'never'; propose('p1', \(Self.orchard))")
        let offer = try card()
        source.connected.patience = .milliseconds(600)
        await refused("Tilo has not connected this service yet. Try again in a moment.") { _ = try await self.source.authenticate(offer.id) }
        // Her yes stands, and the card still waits.
        try await until("the card still waits") { self.cards.first?.state == .waiting }
        let begun = try await asked("begin-sign-in")
        XCTAssertEqual(begun, [])
        // Her server adds it now, and the next press goes on from there.
        try await inPage("settle('p1')")
        let signInPage = try await source.authenticate(offer.id)
        XCTAssertEqual(signInPage?.absoluteString.hasPrefix("https://accounts.orchard.example/authorize?"), true)
        let given = try await answers()
        XCTAssertEqual(given, [["answer": "allow", "message": "p1"]])
    }

    func testASignInHerServerCannotFinishIsSaidInItsWordsAndIsBegunAgainFromTheCard() async throws {
        try await start()
        try await enter("propose('p1', \(Self.orchard))")
        let offer = try card()
        _ = try await source.authenticate(offer.id)
        // The sheet came back from another sign-in than the one that was begun.
        let other = try XCTUnwrap(URL(string: "so.tilo.assistant:/signed-in?code=made-up&state=another"))
        await refused("That sign-in cannot be finished. Begin it again.") { try await self.source.signedIn(offer.id, at: other) }
        try await until("the card still waits") { self.cards.first?.state == .waiting }
        // The card's yes begins it again, with no second answer to the question.
        _ = try await source.authenticate(offer.id)
        let returned = try XCTUnwrap(URL(string: "so.tilo.assistant:/signed-in?code=made-up&state=s-1"))
        try await source.signedIn(offer.id, at: returned)
        try await until("the card says connected") { self.cards.first?.state == .connected }
        let begun = try await asked("begin-sign-in")
        XCTAssertEqual(begun?.array?.count, 2)
        let given = try await answers()
        XCTAssertEqual(given, [["answer": "allow", "message": "p1"]])
    }

    func testACardAnsweredOnAnotherDeviceStandsByHerServersListWhichIsAskedOnceMore() async throws {
        try await start()
        try await enter()
        func lists() async -> Int { (try? await self.asked("list"))?.array?.count ?? -1 }
        try await until("her server listed her connections on the way in") { await lists() == 1 }
        // Her phone answered, her server added the service and she signed in there.
        try await inPage("""
            window.connections.push(window.kept('c9', 'https://orchard.example/mcp', {signIn: 'signed-in'}));
            propose('p1', \(Self.orchard), {answered: {answer: 'allow', device: 'Her phone', at: 1794900100, detail: null}, outcome: {outcome: 'done', at: 1794900101, detail: ''}})
            """)
        try await until("the card says connected") { self.cards.first?.state == .connected }
        let twice = await lists()
        XCTAssertEqual(twice, 2)
        // What else changes in the conversation asks her server nothing.
        for n in 0..<3 {
            try await inPage("post({id: 'n\(n)', author: 'agent-1', text: 'Note \(n)'})")
        }
        try await until("the conversation followed") { self.held.count == 4 }
        let after = await lists()
        XCTAssertEqual(after, 2)
        XCTAssertEqual(cards.first?.state, .connected)
    }

    /// A service that says its name and where its person signs in, at another address.
    private static let grove = "{address: 'https://grove.example/mcp', name: 'Grove', needsSignIn: true, signInAt: 'accounts.grove.example'}"

    func testTheAssistantViewListsHerConnectionsNewestFirstAndFollowsWhatSheDoesOnACard() async throws {
        try await start()
        var seen = Supervision()
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        defer { supervision.cancel() }
        let three = "['find_trees', 'count_trees', 'plant_tree'].map(name => ({name, description: '', readOnly: false, asks: true, changed: false, schema: {}}))"
        try await enter("""
            window.connections.push(window.kept('c1', 'https://old.example/mcp', {name: '', addedAtMs: 1794000000000, commands: \(three)}));
            window.connections.push(window.kept('c2', 'https://orchard.example/mcp'))
            """)
        try await until("her connections are listed") { seen.connections.count == 2 }
        // Newest first, each with when it was added and how many commands it offers. A service
        // that gives itself no name is called by the site of its address.
        XCTAssertEqual(seen.connections, [
            Supervision.Connection(id: "c2", name: "Orchard Tools", added: Date(timeIntervalSince1970: 1_794_900_000), commands: 1),
            Supervision.Connection(id: "c1", name: "old.example", added: Date(timeIntervalSince1970: 1_794_000_000), commands: 3),
        ])
        // A service she adds from its card is listed with no other way in. While nobody is
        // signed in to it, its line says that.
        try await inPage("propose('p1', \(Self.grove))")
        try await until("the card is there") { self.cards.count == 1 }
        let offer = try card()
        _ = try await source.authenticate(offer.id)
        try await until("the service she added is listed first, as one nobody is signed in to") {
            seen.connections.map(\.name) == ["Grove", "Orchard Tools", "old.example"] && seen.connections.first?.needsSignIn == true
        }
        let returned = try XCTUnwrap(URL(string: "so.tilo.assistant:/signed-in?code=made-up&state=s-1"))
        try await source.signedIn(offer.id, at: returned)
        try await until("its line counts its commands") {
            seen.connections.first == Supervision.Connection(id: "c3", name: "Grove", added: Date(timeIntervalSince1970: 1_794_900_000), commands: 1)
        }
        // Signing out forgets them.
        await source.signOut()
        try await until("nothing is listed") { seen.connections.isEmpty }
        XCTAssertNil(source.connected.listed)
    }

    func testAServiceSheTookOutAgainWithNotNowLeavesTheList() async throws {
        try await start()
        var seen = Supervision()
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        defer { supervision.cancel() }
        try await enter("propose('p1', \(Self.grove))")
        let offer = try card()
        _ = try await source.authenticate(offer.id)
        try await until("the service is listed as one nobody is signed in to") { seen.connections.map(\.needsSignIn) == [true] }
        try await source.decline(offer.id)
        try await until("the list is empty again") { seen.connections.isEmpty }
        XCTAssertEqual(source.connected.listed, [])
    }
}
#endif
