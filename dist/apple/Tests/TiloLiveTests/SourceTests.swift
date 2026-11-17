#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The assistant's source on Tilo against a page that speaks as Chat's does, and a host that
/// answers the person's own providers. No window is opened, nothing is fetched, nothing kept.
@MainActor
final class TiloSourceTests: XCTestCase {
    private static let site = "https://tilo.test"
    private static let ada = String(repeating: "ad", count: 32)
    private var source: TiloSource!
    private var defaults: UserDefaults!
    private var suite = ""
    private var asked: [URL] = []
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var listening: [Task<Void, Never>] = []

    private func start(_ mainChat: TiloSource.MainChat = .conversation(id: "c_chat", name: "chat"), livesInOwn: Bool = false) async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.asked.append($0) }
        source = TiloSource(engine: engine, mainChat: mainChat, livesInOwn: livesInOwn, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
        ]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

    /// As Tilo's one-app page of Chat: its mount lists and reads Chat's messages, takes Chat's
    /// send and the answer to a question, and names the person's row. With `enter(true)` it
    /// hands a native host its device's entry, behind which a host keeps a vault, the person's
    /// models and their events.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      window.messages = [];
      window.heard = [];
      window.hostAsked = [];
      window.vault = [];
      window.assignments = [];
      window.learned = [{id: 'f2', text: 'Answers mail in the morning', source: 'said in the main chat', atMs: 1794900200000, by: 'assistant'},
        {id: 'f1', text: 'Prefers short replies', source: 'said in the main chat', atMs: 1794900100000, by: 'assistant'}];
      window.events = [{id: 'e-1', name: 'Morning rundown', sort: 'scheduled', when: 'every day at 07:30', nextAtMs: 1794990600000,
        instruction: 'Tell me what matters today', on: true, permits: []},
        {id: 'e-2', name: 'When a mail comes in', sort: 'event', when: 'when "so.tilo.mail" sends "arrival"', onlyIf: 'The sender is new',
        every: 3, instruction: 'Draft a reply', on: false, permits: [{action: 'send'}]}];
      const said = value => ({state: 'completed', output: {value}});
      const touch = () => { document.getElementById('semantics').textContent = String(Math.random()); };
      window.post = message => { window.messages.push({ts: 1794900000 + window.messages.length, question: null, share: null, attachments: [], ...message}); touch(); };
      const refuse = (resource, action) => { throw new Error(`unknown_action: no action addressed by (${resource}, ${action})`); };
      const providers = () => ({providers: [{id: 'anthropic', name: 'Anthropic', protocol: 'anthropic', endpoint: 'https://api.anthropic.com',
        configured: true, models: [{id: 'm-1', label: 'Model one'}, {id: 'm-2', label: 'Model two'}], defaultModel: 'm-1'},
        {id: 'openai', name: 'OpenAI', protocol: 'openai', endpoint: 'https://api.openai.com', configured: false, models: [{id: 'o-1', label: 'Other'}]}],
        defaultProvider: 'anthropic', assignments: window.assignments});
      const device = Object.freeze({
        person: ADA, workspace: 'ws-1',
        catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => ({id: 'own-7', name: 'Mine', origin: 'https://host.example:8443'}),
        invoke: async (target, argument) => {
          window.hostAsked.push(target.provider + ' ' + target.address.action);
          const done = value => ({invocation: 'i', state: 'succeeded', output: {value, resources: []}});
          switch (target.provider + ' ' + target.address.action) {
            case 'tilo.vault add':
              window.vault.push({...argument});
              return done({reference: 'vault.item:0a', kind: argument.kind, belongsTo: argument.belongsTo, addedAt: 1794900000});
            case 'tilo.vault list':
              return done({items: window.vault.map((item, at) => ({reference: 'vault.item:0' + at, kind: item.kind, belongsTo: item.belongsTo, addedAt: 1794900000}))});
            case 'tilo.models list': return done(providers());
            case 'tilo.models set-assignments':
              for (const kind of argument.kinds) {
                window.assignments = window.assignments.filter(row => row.kind !== kind);
                if (argument.model) window.assignments.push({kind, provider: argument.provider, model: argument.model});
              }
              return done(providers());
            case 'tilo.learned list':
              return done({workspace: 'ws-1', revision: 'r' + window.learned.length, facts: window.learned});
            case 'tilo.learned forget':
              window.learned = window.learned.filter(fact => fact.id !== argument.fact);
              return done({workspace: 'ws-1', revision: 'r' + window.learned.length, facts: window.learned});
            case 'tilo.telling read':
              return done({lines: [{id: 'l1', words: 'Anything from the people I work for', by: 'person', atMs: 1794900000000},
                {id: 'l2', words: 'Nothing about newsletters', onlyWhile: {fact: 'travelling', is: 'no'}, by: 'person', atMs: 1794900000001}],
                facts: [{name: 'travelling', value: 'no', by: 'person', atMs: 1794900000002}], throughQuiet: [], keptOnHost: [], revision: 3});
            case 'tilo.events list': return done({events: window.events});
            case 'tilo.events pause': case 'tilo.events resume':
              window.events.find(event => event.id === argument.event).on = target.address.action === 'resume';
              return done({events: window.events});
            default: throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'});
          }
        },
      });
      window.enter = withDevice => {
        localStorage.setItem('tilo-host-memberships', JSON.stringify({home: {workspace: 'ws-1', userPublicKey: ADA}}));
        if (withDevice) globalThis.tiloNativeHost.device(device);
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
            if (resource === 'chat.workspace' && action === 'send') {
              const id = 's' + window.messages.length;
              window.post({id, channel: args.to[0].id, author: ADA, text: args.text});
              return said({accepted: true, message: id, conversation: args.to[0].id});
            }
            if (resource === 'chain:messages' && action === 'answer-question') {
              const asked = window.messages.find(m => m.id === args.message);
              if (!asked?.question) throw new Error('unknown_item: no such question');
              if (asked.question.answered) throw new Error('answered: This question was answered already');
              asked.question = {...asked.question, answered: {answer: args.answer, device: '', at: 1794900100, detail: args.detail ?? null}};
              touch();
              return said(null);
            }
            return refuse(resource, action);
          },
        };
      };
    </script></html>
    """#

    private func run(_ script: String) async throws {
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

    private func words(_ item: Item) -> String {
        switch item.content {
        case .message(let message): message.text
        case .question(let question): question.text
        case .secret(let request): request.text
        default: ""
        }
    }

    private func enter(device: Bool = false, _ before: String = "") async throws {
        try await run("\(before); location.hash = '#/w/ws-1/so.tilo.chat'; enter(\(device))")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
    }

    func testTheAppIsSignedInAsThePersonAndItsConversationFillsAndFollows() async throws {
        try await start()
        // An app nobody paired says so, before anything else.
        try await until("the app says where it stands") { !self.sessions.isEmpty }
        XCTAssertEqual(sessions.first, .signedOut)
        try await enter("""
            post({id: 'm1', channel: 'c_chat', author: ADA, text: 'Good morning'});
            post({id: 'm2', channel: 'c_elsewhere', author: 'agent-1', text: 'Not of this conversation'});
            post({id: 'm3', channel: 'c_chat', author: 'agent-1', text: 'Good morning, Ada'})
            """)
        XCTAssertEqual(sessions.last, .signedIn(Person(id: .init(Self.ada), name: "Ada Lovelace", workspace: "Ada's workspace")))
        // What the conversation held, oldest first, and only its own pieces.
        guard case .all(let first) = changes.first else { return XCTFail("the first change is not the whole conversation") }
        XCTAssertEqual(first.map(words), ["Good morning", "Good morning, Ada"])
        XCTAssertEqual(first.map(\.id.value), ["chat.messages/messages/m1", "chat.messages/messages/m3"])
        guard case .message(let mine) = first[0].content, case .message(let theirs) = first[1].content else { return XCTFail("no messages") }
        XCTAssertEqual(mine.author, .person)
        XCTAssertEqual(theirs.author, .assistant)
        // A message posted elsewhere arrives at the page's word, as an addition.
        try await run("post({id: 'm4', channel: 'c_chat', author: 'agent-1', text: 'One more thing'})")
        try await until("the new piece is told") { self.held.count == 3 }
        guard case .added(let added) = changes.last else { return XCTFail("a new piece was not told as added") }
        XCTAssertEqual(added.map(words), ["One more thing"])
        // The app keeps where it was paired into, to come back as itself.
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/w/ws-1")
    }

    func testWhatIsTypedGoesThroughChatsOwnSendAndStandsAsThePersons() async throws {
        try await start()
        try await enter()
        try await source.send(Draft(text: "Please draft a reply"), to: .main)
        try await until("what was sent stands in the conversation") { self.held.map(self.words) == ["Please draft a reply"] }
        guard case .message(let message) = held[0].content else { return XCTFail("no message") }
        XCTAssertEqual(message.author, .person)
        let sent = try await page("return JSON.stringify(window.heard.filter(call => call.action === 'send').map(call => [call.resource, call.args.to, call.args.text, call.args.attached]))")
        XCTAssertEqual(JSON(text: sent), [["chat.workspace", [["id": "c_chat", "kind": "conversation", "name": "chat"]], "Please draft a reply", []]])
        // A file is refused with a sentence, and nothing is sent in its place.
        do {
            try await source.send(Draft(text: "With a file", files: [.init(url: URL(fileURLWithPath: "/tmp/report.pdf"))]), to: .main)
            XCTFail("a file was taken for sent")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("A file cannot be sent from this app yet"))
        }
        XCTAssertEqual(held.count, 1)
    }

    func testAQuestionThatWaitsIsAnsweredOnceWithOneOfItsAnswers() async throws {
        try await start()
        try await enter("""
            post({id: 'q1', channel: 'c_chat', author: 'agent-1', text: 'Shall I send the reply?',
                  question: {kind: 'choice', person: ADA, answers: [{id: 'send', label: 'Send it'}, {id: 'no', label: ''}], detail: '',
                             answered: null, outcome: null, withdrawn: null}})
            """)
        guard case .question(let question) = held[0].content else { return XCTFail("the question is no question") }
        XCTAssertEqual(question.text, "Shall I send the reply?")
        XCTAssertEqual(question.choices, [Choice(id: "send", title: "Send it", isMain: true), Choice(id: "no", title: "no")])
        XCTAssertTrue(question.waits)
        try await source.answer(question.id, with: "send")
        try await until("the answer stands on the question") {
            if case .question(let now)? = self.held.first?.content { now.answer == "send" } else { false }
        }
        let answered = try await page("return JSON.stringify(window.heard.filter(call => call.action === 'answer-question').map(call => [call.resource, call.args]))")
        XCTAssertEqual(JSON(text: answered), [["chain:messages", ["answer": "send", "message": "q1"]]])
        // Chat's own refusal of a second answer is said as it is.
        do {
            try await source.answer(question.id, with: "no")
            XCTFail("a question was answered twice")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("This question was answered already"))
        }
    }

    private static let secretQuestion = """
        post({id: 'k1', channel: 'c_chat', author: 'agent-1', text: 'I need the key for the reports API.',
              question: {kind: 'secret', person: ADA, answers: [{id: 'filled', label: ''}, {id: 'declined', label: ''}],
                         detail: JSON.stringify({kind: 'key', belongsTo: ['https://api.example:8443'], username: 'ada'}),
                         answered: null, outcome: null, withdrawn: null}})
        """

    func testASecretGoesToTheVaultAndTheConversationHoldsOnlyItsReference() async throws {
        try await start()
        try await enter(device: true, Self.secretQuestion)
        guard case .secret(let request) = held[0].content else { return XCTFail("the request for a secret is none") }
        XCTAssertEqual(request.name, "api.example")
        XCTAssertEqual(request.standing, .waiting)
        let typed = "sk-typed-into-the-protected-field"
        try await source.keep(Secret(typed), for: request.id)
        try await until("the request says the vault has it") {
            if case .secret(let now)? = self.held.first?.content { now.standing == .kept } else { false }
        }
        // The host's vault got it, with what the question said it is for.
        let kept = try await page("return JSON.stringify(window.vault)")
        XCTAssertEqual(JSON(text: kept), [["kind": "key", "belongsTo": ["https://api.example:8443"], "username": "ada",
                                           "secret": "sk-typed-into-the-protected-field"]])
        // The conversation was told the item's reference, and Chat was told nothing else:
        // what was typed stands in no message and in nothing Chat's own actions were asked.
        let told = try await page("return JSON.stringify(window.heard.filter(call => call.action === 'answer-question').map(call => call.args))")
        XCTAssertEqual(JSON(text: told), [["answer": "filled", "detail": ["reference": "vault.item:0a"], "message": "k1"]])
        let leaked = try await page("return String(JSON.stringify([window.messages, window.heard]).includes(typed)) + ' ' + String(document.documentElement.outerHTML.includes(typed))",
                                    ["typed": typed])
        XCTAssertEqual(leaked, "false false")
        XCTAssertFalse(String(describing: held).contains(typed))
        XCTAssertFalse(String(describing: changes).contains(typed))
        // Nothing of it is kept by the app beside the conversation.
        XCTAssertFalse(String(describing: defaults.dictionaryRepresentation()).contains(typed))
        // What it can sign in to is said by where it belongs, never by what it is.
        var seen = Supervision()
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        try await until("the vault's item is listed") { seen.logins.map(\.name) == ["api.example"] }
        supervision.cancel()
    }

    func testNothingTypedIsKeptNowhereAndAPageWithNoWayToTheVaultLeavesTheQuestionWaiting() async throws {
        try await start()
        // Tilo's page as it was before it knew of a native host: no way to the person's vault.
        try await enter(device: false, Self.secretQuestion)
        guard case .secret(let request) = held[0].content else { return XCTFail("the request for a secret is none") }
        do {
            try await source.keep(Secret(""), for: request.id)
            XCTFail("nothing was taken for a secret")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("Nothing was typed"))
        }
        do {
            try await source.keep(Secret("typed"), for: request.id)
            XCTFail("a secret was taken where no vault is reached")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("This page hands a native app no way to its person's own providers"))
        }
        // The secret went nowhere instead: the question still waits, unanswered.
        let state = try await page("return JSON.stringify([window.vault.length, window.heard.filter(call => call.action === 'answer-question').length, JSON.stringify(window.heard).includes('typed')])")
        XCTAssertEqual(state, "[0,0,false]")
        // Declining needs no vault: it is Chat's own answer.
        try await source.decline(request.id)
        try await until("the request says it was declined") {
            if case .secret(let now)? = self.held.first?.content { now.standing == .declined } else { false }
        }
    }

    func testTheModelsAreThePersonsOwnAndAChoiceGoesToTheirHost() async throws {
        try await start()
        try await enter(device: true)
        // Only the models of a provider that has its key can be chosen.
        let models = try await source.models()
        XCTAssertEqual(models, [ModelChoice(id: "anthropic/m-1", name: "Model one", provider: "Anthropic"),
                                ModelChoice(id: "anthropic/m-2", name: "Model two", provider: "Anthropic")])
        var seen = Supervision()
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        try await until("the main chat's model is the default") { seen.model == "Model one" }
        try await source.choose("anthropic/m-2", for: .request)
        try await until("the chosen model is said") { seen.model == "Model two" }
        supervision.cancel()
        let assigned = try await page("return JSON.stringify(window.assignments)")
        XCTAssertEqual(assigned, #"[{"kind":"request","provider":"anthropic","model":"m-2"}]"#)
        do {
            try await source.choose("no such thing", for: .task)
            XCTFail("something that is no model was chosen")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("This is no model of a provider you linked"))
        }
    }

    func testScheduledThingsAndEventsAreThePersonsOwnAndOneIsPaused() async throws {
        try await start()
        var listed: [Automation] = []
        let following = Task { for await now in self.source.automations() { listed = now } }
        try await enter(device: true)
        try await until("the events are listed") { listed.count == 2 }
        XCTAssertEqual(listed[0], Automation(id: "e-1", sort: .scheduled, title: "Morning rundown", summary: "every day at 07:30",
                                             next: Date(timeIntervalSince1970: 1_794_990_600), isOn: true))
        XCTAssertEqual(listed[1], Automation(id: "e-2", sort: .event, title: "When a mail comes in", summary: "Draft a reply",
                                             filter: "The sender is new", threshold: 3, actsAlone: true, isOn: false))
        try await source.setOn(false, for: "e-1")
        try await until("the pause is said") { listed.first?.isOn == false }
        try await source.setOn(true, for: "e-2")
        try await until("it goes on again") { listed.last?.isOn == true }
        following.cancel()
        let asked = try await page("return JSON.stringify(window.hostAsked.filter(what => what.startsWith('tilo.events ') && !what.endsWith(' list')))")
        XCTAssertEqual(asked, #"["tilo.events pause","tilo.events resume"]"#)
    }

    func testTheHostIsAskedWhenTheDeviceGetsInAndNotOnEveryChange() async throws {
        try await start()
        try await enter(device: true)
        try await until("the host was asked on the way in") {
            (try? await self.page("return String(window.hostAsked.length)")) == "5"
        }
        // Ten changes of Chat's records: the conversation follows, and the host is asked nothing.
        for n in 0..<10 {
            try await run("post({id: 'n\(n)', channel: 'c_chat', author: 'agent-1', text: 'Note \(n)'})")
        }
        try await until("the conversation followed") { self.held.count == 10 }
        try await Task.sleep(for: .milliseconds(300))
        let asked = try await page("return JSON.stringify(window.hostAsked)")
        XCTAssertEqual(asked, #"["tilo.models list","tilo.vault list","tilo.learned list","tilo.telling read","tilo.events list"]"#)
    }

    func testWhatItLearnedAndWhatIsImportantAreThePersonsOwnOnTheirHost() async throws {
        try await start()
        var seen = Supervision()
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        try await enter(device: true)
        try await until("what the host's agents learned is listed") { seen.learned.count == 2 }
        XCTAssertEqual(seen.learned.map(\.text), ["Answers mail in the morning", "Prefers short replies"])
        XCTAssertEqual(seen.learnedCount, 2)
        // The rule in the person's own words, and the situations it depends on.
        XCTAssertEqual(seen.importance, ["Anything from the people I work for", "Nothing about newsletters"])
        XCTAssertEqual(seen.situations, [Supervision.Situation(name: "travelling", value: "no")])
        // Taking one back goes to the host that keeps it.
        try await source.forget(seen.learned[1].id)
        try await until("the fact is gone") { seen.learned.map(\.text) == ["Answers mail in the morning"] }
        supervision.cancel()
        let asked = try await page("return JSON.stringify(window.hostAsked.filter(what => what.startsWith('tilo.learned ')))")
        XCTAssertEqual(asked, #"["tilo.learned list","tilo.learned forget","tilo.learned list"]"#)
    }

    func testADeviceLetInElsewhereGoesToThePersonsOwnWorkspaceWhereTheAssistantLives() async throws {
        try await start(livesInOwn: true)
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the person's own workspace is asked for") { !self.asked.isEmpty }
        XCTAssertEqual(asked.map(\.absoluteString), ["\(Self.site)/app/#/@host.example:8443/w/own-7/so.tilo.chat"])
        // There it stays: the page's own word of where it is, and no second move.
        try await run("location.hash = '#/@host.example:8443/w/own-7/so.tilo.chat'")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        XCTAssertEqual(asked.count, 1)
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/@host.example:8443/w/own-7")
    }

    func testWhatTiloCannotDoFromHereYetIsRefusedWithASentence() async throws {
        try await start()
        try await enter()
        let refused: [(String, () async throws -> Void)] = [
            ("Tasks cannot be reached from this app yet", { try await self.source.addTask("Renew the passport") }),
            ("Tilo cannot pause an assistant yet", { try await self.source.setAssistantOn(false) }),
            ("No password manager is connected to Tilo yet", { try await self.source.fetchSecret(for: "k1") }),
            ("Tilo keeps no connections to sign in to yet", { _ = try await self.source.authenticate("c1") }),
            ("Tilo keeps no connections to sign in to yet", { try await self.source.decline(ConnectionOffer.ID("c1")) }),
            ("A file cannot be fetched into this app yet", { _ = try await self.source.file(of: "f1") }),
        ]
        for (sentence, call) in refused {
            do {
                try await call()
                XCTFail("something Tilo cannot do yet was taken for done: \(sentence)")
            } catch let problem as SourceProblem {
                XCTAssertEqual(problem, .refused(sentence))
            }
        }
        // This page reaches no host, so the person's models are not there, and it says so.
        do {
            _ = try await source.models()
            XCTFail("models were listed where no host is reached")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("This page hands a native app no way to its person's own providers"))
        }
    }

    func testSigningOutForgetsThePersonAndWhatWasHeld() async throws {
        try await start()
        try await enter("post({id: 'm1', channel: 'c_chat', author: ADA, text: 'Good morning'})")
        XCTAssertEqual(held.count, 1)
        await source.signOut()
        try await until("the app is signed out") { self.sessions.last == .signedOut }
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"))
        XCTAssertNil(defaults.array(forKey: "TiloLivePerson"))
        XCTAssertTrue(held.isEmpty)
    }
}
#endif
