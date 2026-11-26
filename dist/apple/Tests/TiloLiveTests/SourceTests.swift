#if os(macOS)
import AssistantClosed
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
    private var setups: [Setup] = []
    private var sinces: [SinceLeft?] = []
    private var supervisions: [Supervision] = []
    private var listening: [Task<Void, Never>] = []

    private func start(_ mainChat: TiloSource.MainChat = .conversation(id: "c_chat", name: "chat"), livesInOwn: Bool = false,
                       closedKeys: (any ClosedKeyStore)? = nil) async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.asked.append($0) }
        source = TiloSource(engine: engine, mainChat: mainChat, livesInOwn: livesInOwn, defaults: defaults, closedKeys: closedKeys)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
            Task { for await setup in source.setup() { self.setups.append(setup) } },
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
    static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      // As a page that names a member by its kind spells the conversation of Ada and her assistant.
      const DIRECT = 'direct:[{"person":"' + ADA + '"},{"agent":"assistant"}]';
      const hex = text => [...text].map(letter => letter.charCodeAt(0).toString(16).padStart(2, '0')).join('');
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
      // A person who linked no provider yet (`window.unlinked`): what they add stands here.
      window.added = [];
      window.keys = [];
      // What her server told her (`tilo.telling`, its log), and what a device said she saw. An
      // ask that waits is held until the log moves, for `window.holds` milliseconds at most.
      window.told = [];
      window.acknowledged = [];
      // What a device handed her server to be reached while its app is closed, and how often one took that back.
      window.registered = [];
      window.unregistered = 0;
      // A message in her conversation with the assistant.
      window.direct = message => window.post({channel: DIRECT, ...message});
      window.tell = thing => { window.told = window.told.filter(one => one.id !== thing.id).concat([thing]); window.logMoved?.(); };
      const own = () => window.unlinked ? {providers: window.added, defaultProvider: null, assignments: window.assignments} : providers();
      // An agent as the workspace keeps one. `window.agents` is what its server keeps and offers
      // a person's device; a server that has none of it offers no such read.
      window.kept = (id, more = {}) => ({id, name: id, keeper: {person: ADA}, model: null, mcp_client: null, sub: null,
        state: {$variant: 'active'}, created_at: 1794900000000, ...more});
      // Assistant's own document of the workspace, as its server names what it offers of one,
      // and what a person's device asked of the server: `agents read`, `tilo.models list`.
      const AGENTS = 'record:so.tilo.assistant:';
      window.asks = [];
      const noAgents = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      // As the page that asks before it pairs: its card, its sentences, its two buttons.
      window.askWhose = (whose, server) => {
        const card = document.createElement('div'); card.className = 'tilo-entry-card tilo-pair'; card.dataset.card = 'pair-whose';
        const heading = document.createElement('h2'); heading.textContent = 'Add this device?';
        const line = text => { const said = document.createElement('p'); said.textContent = text; return said; };
        const go = document.createElement('button'); go.className = 'tilo-entry-primary'; go.textContent = 'Add this device';
        const stop = document.createElement('button'); stop.textContent = 'Stop';
        go.addEventListener('click', () => { card.remove(); window.agreed = true; });
        stop.addEventListener('click', () => { card.remove(); window.agreed = false; });
        card.append(heading, line(whose), line(server), go, stop);
        document.body.append(card);
      };
      let rosterRevision = 0, rosterChanged;
      window.changeRoster = () => { rosterRevision++; rosterChanged?.(); };
      const device = Object.freeze({
        person: ADA, get workspace() { return window.admittedTo ?? (location.hash.includes('/w/own-7/') ? 'own-7' : 'ws-1'); },
        catalogue: async ({provider, target}) => {
          if (!window.grantChoices || provider !== 'agents' || target?.resource !== 'agent:reed'
              || target.action !== 'request-app-grant') return {rows: []};
          const apps = {name: 'apps', ty: 'json', required: true, schema: {type: 'array', items: {oneOf: window.grantChoices}}};
          return {rows: [{provider, action: {address: target, params: [apps]}}]};
        },
        status: async invocation => ({invocation, state: 'succeeded'}),
        changed: async ({after}) => {
          if (!window.observeAgents) noAgents();
          if (after == null) return {changed: [], cursor: String(rosterRevision)};
          if (after === String(rosterRevision)) await new Promise(resolve => { rosterChanged = resolve; });
          return {changed: [{app: 'so.tilo.assistant', document: 'agents'}], cursor: String(rosterRevision)};
        },
        ownWorkspace: async () => {
          if (window.noOwn) throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'});
          return {id: 'own-7', name: 'Mine', origin: 'https://host.example:8443'};
        },
        invoke: async (target, argument) => {
          window.hostAsked.push(target.provider + ' ' + target.address.action);
          window.asks.push(target.provider.replace(AGENTS, 'agents') + ' ' + target.address.action);
          const done = value => ({invocation: 'i', state: 'succeeded', output: {value, resources: []}});
          switch (target.provider + ' ' + target.address.action) {
            case 'tilo.vault add':
              window.vault.push({...argument});
              return done({reference: 'vault.item:0a', kind: argument.kind, belongsTo: argument.belongsTo, addedAt: 1794900000});
            case 'tilo.vault list':
              if (window.holdVaultRead) {
                window.holdVaultRead = false;
                await new Promise(resolve => { window.resumeVaultRead = resolve; });
              }
              return done({items: window.vault.map((item, at) => ({reference: 'vault.item:0' + at, kind: item.kind, belongsTo: item.belongsTo, addedAt: 1794900000}))});
            case 'tilo.models list': return done(own());
            case 'tilo.telling list': {
              // A server that tells nothing this way (`window.told = null`) does not know the ask.
              if (!window.told) throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'});
              const newer = () => window.told.filter(one => one.seq > (argument?.after ?? 0));
              if (!newer().length && argument?.waitMs) {
                await new Promise(moved => { window.logMoved = moved; setTimeout(moved, Math.min(argument.waitMs, window.holds ?? 150)); });
              }
              return done({told: newer(), newest: Math.max(0, ...window.told.map(one => one.seq))});
            }
            case 'tilo.telling acknowledge': {
              window.acknowledged.push(argument);
              const seen = window.told.find(one => one.id === argument.told);
              if (seen) window.tell({...seen, seq: Math.max(...window.told.map(one => one.seq)) + 1, acknowledged: {atMs: 1794900300000, device: argument.device ?? '', by: argument.by}});
              return done(null);
            }
            // What she was told while she was away, where she was (`window.since`).
            case 'tilo.telling since-you-left':
              return done(window.since ?? {cameIn: [], didAlone: [], waits: []});
            case 'tilo.telling register': window.registered.push(argument); return done({addresses: []});
            case 'tilo.telling unregister': window.unregistered += 1; return done({addresses: []});
            case 'tilo.models add-preset': {
              const made = {id: argument.preset, name: argument.preset === 'anthropic' ? 'Anthropic' : argument.preset, protocol: argument.preset,
                endpoint: 'https://api.' + argument.preset + '.example', configured: false, models: []};
              window.added.push(made);
              return done(made);
            }
            case 'tilo.models set-key': {
              window.keys.push(argument.key);
              const keyed = window.added.find(one => one.id === argument.provider);
              keyed.configured = true;
              return done(keyed);
            }
            case 'tilo.models refresh-models': {
              const listed = window.added.find(one => one.id === argument.provider);
              listed.models = [{id: 'm-1', label: 'Model one'}, {id: 'm-2', label: 'Model two'}];
              return done({provider: listed});
            }
            case 'agents request-app-grant': {
              if (!window.chatGrant || target.address.resource !== 'agent:reed' || !argument
                  || Object.keys(argument).length !== 1 || !Array.isArray(argument.apps)) noAgents();
              (window.selectedScopes ??= []).push(argument.apps);
              Object.assign(window.chatGrant, {state: 'pending', ready: false, apps: argument.apps, expiresAtMs: Date.now() + 6 * 86400000});
              return done({grant: 'reed', pairingCode: '274 905'});
            }
            case 'agents grant-status': case 'agents request-chat-grant': case 'agents request-assistant-work-grant': case 'agents revoke': {
              if (!window.chatGrant || target.address.resource !== 'agent:reed' || argument !== null) noAgents();
              if (target.address.action === 'grant-status') return done(window.chatGrant);
              if (target.address.action === 'revoke') {
                Object.assign(window.chatGrant, {state: 'revoked', ready: false});
                return done({});
              }
              const work = target.address.action === 'request-assistant-work-grant';
              const apps = work ? ['so.tilo.chat', 'so.tilo.tasks', 'so.tilo.assistant', 'so.tilo.shortcuts'] : ['so.tilo.chat'];
              Object.assign(window.chatGrant, {state: 'pending', ready: false, apps, expiresAtMs: Date.now() + 6 * 86400000});
              return done({grant: 'reed', pairingCode: work ? '291 804' : '493 127'});
            }
            case AGENTS + ' read': {
              // A server whose answer never comes (`window.hangs`).
              if (window.hangs) return new Promise(() => {});
              if (!window.agents) noAgents();
              const one = target.address.resource.split('/')[2];
              // A server that lists the agents and lets nobody read one of them (`window.rowsOnly`).
              if (one && window.rowsOnly) noAgents();
              // A server that says each agent whole in its list and offers no read of one agent
              // (`window.wholeRows`): what main's server answered on 17 November 2026.
              const listed = !one && window.hides > 0 ? (window.hides -= 1, window.agents.filter(agent => !agent.fresh)) : window.agents;
              if (window.wholeRows) { if (one) noAgents(); return done({agents: listed, personal_assistants: window.personal}); }
              if (one) return done(window.agents.find(agent => agent.id === one) ?? null);
              return done({kind: 'tilo.workspace.agent', agents: listed.map(agent => ({item: 'tilo.workspace.agent/agents/' + agent.id, name: agent.name}))});
            }
            case AGENTS + ' ensure-personal-assistant':
              if (!window.agents) noAgents();
              if (window.ensureNotPublished) { window.ensureNotPublished = false; noAgents(); }
              if (window.mayNotMake) throw Object.assign(new Error('Your role in this workspace may not make an agent'), {code: 'not_permitted'});
              window.madeWith = argument;
              // A second device can finish setup between the roster read and the action.
              if (window.raced) { window.agents.push(window.kept('other-device', {name: 'Assistant'})); return done(null); }
              window.agents.push(window.kept('assistant', {name: 'Assistant', created_at: 1794900009000 + window.agents.length,
                ...(window.wholeRows ? {keeper: ADA, state: 'active'} : {})}));
              if (window.personal) window.personal[ADA] = 'assistant';
              if (window.late) { window.agents.at(-1).fresh = true; window.hides = window.late; }
              if (window.unanswered) return {invocation: 'i', state: 'accepted'};
              return done(null);
            case AGENTS + ' stop-sub-agent': {
              // A person stops a sub-agent: her server ends what it is doing at its next step.
              window.stopped = argument;
              const working = (window.agents ?? []).find(agent => agent.id === argument.agent);
              if (working?.sub) working.sub.stands = {$variant: 'stopped'};
              return done(null);
            }
            case 'tilo.models set-assignments':
              for (const kind of argument.kinds) {
                window.assignments = window.assignments.filter(row => row.kind !== kind);
                if (argument.model) window.assignments.push({kind, provider: argument.provider, model: argument.model});
              }
              return done(own());
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
            // The person's alone: one permission of an event is taken back, and the event is answered.
            case 'tilo.events withdraw-permission': {
              const one = window.events.find(event => event.id === argument.event);
              if (!one) throw Object.assign(new Error('you keep no such event here'), {code: 'refused'});
              (window.eventCalls ??= []).push('withdraw-permission ' + argument.event + ' ' + argument.rule.action);
              one.permits = one.permits.filter(rule => rule.action !== argument.rule.action);
              return done(one);
            }
            // An event is removed for good, and the events that remain are answered.
            case 'tilo.events remove':
              if (!window.events.some(event => event.id === argument.event)) throw Object.assign(new Error('you keep no such event here'), {code: 'refused'});
              (window.eventCalls ??= []).push('remove ' + argument.event);
              window.events = window.events.filter(event => event.id !== argument.event);
              return done({events: window.events});
            default: throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'});
          }
        },
      });
      window.enter = async (withDevice, hosted = false) => {
        localStorage.setItem('tilo-host-memberships', JSON.stringify({home: {workspace: 'ws-1', userPublicKey: ADA}}));
        if (withDevice) {
          const entry = hosted ? Object.assign(Object.create(device), {workspaceRecord: async app => `record:${app}:`}) : device;
          await giveDevice(entry);
        }
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        if (hosted) return;
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          // `window.byKind` makes it the page that names members by their kind: its read
          // names a conversation and the fields it wants, and a person's row stands under
          // another address than their identity.
          actions: () => window.byKind
            ? [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages',
                params: [{name: 'cursor'}, {name: 'pageSize'}, {name: 'fields'}, {name: 'where'}]},
               {address: {resource: 'chat.workspace/people/' + hex(ADA), action: 'read'}, resource_kind: 'chat.workspace.people'}]
            : [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages'}],
          invokeAction: async (resource, action, args) => {
            window.heard.push({resource, action, args});
            const item = resource.startsWith('chat.messages/messages/') ? window.messages.find(m => m.id === resource.split('/').pop()) : null;
            if (resource === 'chain:messages' && action === 'read') {
              if (window.chatReply) await window.chatReply;
              // A Chat that keeps a field on no message refuses a read that names it.
              const none = (args.fields ?? []).find(field => (window.noFields ?? []).includes(field));
              if (none) throw new Error('unknown_field: a message has no field ' + none);
              const wanted = args.where?.channel;
              return said({kind: 'chat.messages', messages: [...window.messages].reverse()
                .filter(m => wanted === undefined || m.channel === wanted).slice(0, args.pageSize ?? 50)
                .map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text,
                  ...Object.fromEntries((args.fields ?? []).map(field => [field, m[field] ?? null]))}))});
            }
            if (item && action === 'read') return said(item);
            if (resource === 'chat.workspace/people/' + ADA && action === 'read' && !window.byKind) return said({id: ADA, name: 'Ada Lovelace'});
            if (resource === 'chat.workspace/people/' + hex(ADA) && action === 'read' && window.byKind) return said({id: {person: ADA}, name: 'Ada Lovelace'});
            if (resource === 'chat.workspace' && action === 'send') {
              const id = 's' + window.messages.length;
              const to = args.to[0];
              window.post({id, channel: to.kind === 'agent' ? DIRECT : to.id, author: window.byKind ? {person: ADA} : ADA, text: args.text});
              return said({accepted: true, message: id, conversation: to.id});
            }
            if (resource === 'chain:messages' && action === 'stop-agent') {
              const worked = window.messages.find(m => m.id === args.message);
              if (!worked?.work) throw new Error('unknown_item: no agent works on this message');
              worked.stopped = {by: ADA, at: 1794900200};
              touch();
              return said(null);
            }
            if (resource === 'chain:messages' && action === 'stop-sharing') {
              const shared = window.messages.find(m => m.id === args.message);
              if (!shared?.share) throw new Error('refused: this message shares nothing');
              shared.share = {...shared.share, open: false, ended: true};
              touch();
              return said(null);
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

    func testMembersAreReadByKindOrByIdAndTheirConversationIsSpelledAsChatDoes() {
        XCTAssertEqual(ChatMember(["person": "ab"]), ChatMember(.person, id: "ab"))
        XCTAssertEqual(ChatMember(["agent": "assistant"]), ChatMember(.agent, id: "assistant"))
        XCTAssertEqual(ChatMember(["bot": "nightly"]), ChatMember(.automation, id: "nightly"))
        XCTAssertEqual(ChatMember("ab"), ChatMember(nil, id: "ab"))
        XCTAssertNil(ChatMember(["team": "x"]))
        XCTAssertNil(ChatMember(nil))
        // A person stands before an agent whatever their ids, and two of a kind by their ids.
        XCTAssertEqual(ChatConversation.direct([ChatMember(.agent, id: "assistant"), ChatMember(.person, id: "ff")]),
                       #"direct:[{"person":"ff"},{"agent":"assistant"}]"#)
        XCTAssertEqual(ChatConversation.direct([ChatMember(.person, id: "b"), ChatMember(.person, id: "a")]),
                       #"direct:[{"person":"a"},{"person":"b"}]"#)
        XCTAssertEqual(ChatConversation.direct([ChatMember(nil, id: "b"), ChatMember(nil, id: "a")]), #"direct:["a","b"]"#)
    }

    func testAReplyAtWorkCarriesWhatItsTurnDoesAsChatKeepsItWithTheReply() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        // The reply's work is what Chat gives a reader of a turn of three steps (`KeptTurn`).
        let work = try KeptTurn.work()
        try await enter("""
            window.byKind = true;
            direct({id: 'm1', author: {person: ADA}, text: 'Add the line to the letter'});
            direct({id: 'm2', author: {agent: 'assistant'}, text: '', work: \(work), stopped: null})
            """)
        try await until("the reply is held") { self.held.count == 2 }
        guard case .message(let asked) = held[0].content, case .message(let reply) = held[1].content else { return XCTFail("two messages") }
        XCTAssertNil(asked.turn, "a person's own message carries a turn")
        let turn = try XCTUnwrap(reply.turn, "the reply at work says nothing of what its turn does")
        XCTAssertEqual(turn.now, .step("Append paragraph: word-processor"))
        XCTAssertEqual(turn.steps.map(\.words),
                       ["List: word-processor", "Append paragraph: word-processor", "Append paragraph: word-processor"])
        XCTAssertEqual(turn.steps.map(\.outcome), [.done, .done, .failed])
    }

    func testOnAPageThatNamesMembersByKindTheConversationWithTheAgentFillsInOneReadAndItsAnswerIsStopped() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter("""
            window.byKind = true;
            direct({id: 'm1', author: {person: ADA}, text: 'Good morning'});
            post({id: 'm2', channel: 'c_chat', author: {person: ADA}, text: 'Said in a channel'});
            post({id: 'm3', channel: DIRECT, author: {agent: 'assistant'}, text: 'Good morning, Ada',
              work: {stream: null, state: {state: {$variant: 'doing', did: '', about: ''}, since: 1794900002}}, stopped: null})
            """)
        try await until("the conversation with the agent is held, and nothing of a channel") { self.held.count == 2 }
        guard case .signedIn(let person) = sessions.last else { return XCTFail("not signed in") }
        XCTAssertEqual(person.name, "Ada Lovelace", "her row stands under another address than her identity")
        guard case .message(let mine) = held[0].content, case .message(let theirs) = held[1].content else { return XCTFail("two messages") }
        XCTAssertEqual(mine.author, .person)
        XCTAssertEqual(theirs.author, .assistant)
        try await until("the agent is said to be at work") { self.changes.contains(.answering(true)) }
        // Each look is one read that names the conversation: no message is read by itself.
        let reads = try await page("return JSON.stringify(window.heard.filter(h => h.action === 'read' && h.resource !== 'chat.workspace/people/' + hex(ADA)).map(h => h.args?.where?.channel === DIRECT ? 'one' : h.resource))")
        XCTAssertFalse(reads.contains("chat.messages/messages/"), reads)
        XCTAssertTrue(reads.contains("one"), reads)

        try await source.send(Draft(text: "And now?"), to: .main)
        let sent = try await page("return JSON.stringify(window.heard.filter(h => h.action === 'send').map(h => h.args.to[0]))")
        XCTAssertEqual(JSON(text: sent), [["kind": "agent", "id": "assistant", "name": "Assistant"]])
        try await until("what was sent stands in the conversation as hers") { self.held.count == 3 }

        try await source.stopAnswering(in: .main)
        let stopped = try await page("return JSON.stringify(window.heard.filter(h => h.action === 'stop-agent').map(h => h.args.message))")
        XCTAssertEqual(stopped, #"["m3"]"#)
        try await until("the agent is said to have stopped") { self.changes.last(where: { if case .answering = $0 { true } else { false } }) == .answering(false) }
        // With nothing at work, stopping asks nothing.
        try await source.stopAnswering(in: .main)
        let again = try await page("return String(window.heard.filter(h => h.action === 'stop-agent').length)")
        XCTAssertEqual(again, "1")
    }

    /// A share stands in its message as a live thing. It is the person's to end only where the
    /// message says an agent made it for them and for nobody else: not what was made for someone
    /// else, for two, for nobody, what says nothing, or their own. Ending it is Chat's own
    /// `stop-sharing` on that message and no other, and the ended share's place is gone.
    func testAShareTheirAssistantMadeForThemIsTheirsToEndWithChatsOwnStopSharing() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter("""
            window.byKind = true;
            const BO = 'b0'.repeat(32);
            const poem = [{at: {app: 'so.tilo.word-processor', reference: {label: 'A poem'}}}];
            const share = {access: 'edit', open: true, ended: false};
            const made = people => ({by: {member: {agent: 'assistant'}}, for: people.map(person => ({by: {member: {person}}}))});
            const shared = (id, more) => direct({id, author: {agent: 'assistant'}, text: id, share: {...share}, attachments: poem, ...more});
            shared('s1', {acting: made([ADA])});
            shared('s2', {acting: made([BO])});
            shared('s3', {acting: made([ADA, BO])});
            shared('s4', {acting: made([])});
            shared('s5', {});
            shared('s6', {author: {person: ADA}, acting: {by: {member: {person: ADA}}}});
            shared('s7', {acting: {by: {member: {bot: 'bridge'}}, for: [made([ADA])]}})
            """)
        try await until("the seven shares are held") { self.held.count == 7 }
        let things = held.compactMap { item -> AssistantCore.LiveThing? in
            if case .message(let message) = item.content { message.live.first } else { nil }
        }
        XCTAssertEqual(things.map(\.title), Array(repeating: "A poem", count: 7))
        XCTAssertTrue(things.allSatisfy { $0.reference.hasSuffix(":so.tilo.word-processor:@tilo:package-primary") }, "\(things.map(\.reference))")
        // The first, and the one an automation posted through her agent for her alone: the one
        // person down the chain of who acted for whom, as Chat reads it.
        XCTAssertEqual(things.map(\.theirsToEnd), [true, false, false, false, false, false, true])

        try await source.stopSharing(things[0].id)
        let stopped = try await page("return JSON.stringify(window.heard.filter(h => h.action === 'stop-sharing').map(h => [h.resource, h.args.message]))")
        XCTAssertEqual(stopped, #"[["chain:messages","s1"]]"#)
        try await until("the ended share's place is gone and its message stays") {
            guard self.held.count == 7, case .message(let ended) = self.held[0].content else { return false }
            return ended.live.isEmpty && ended.text == "s1"
        }
        guard case .message(let other) = held[6].content else { return XCTFail("the last message") }
        XCTAssertEqual(other.live.map(\.theirsToEnd), [true], "no other share was ended")
    }

    /// A page whose Chat keeps on no message whom its author acted for refuses a read that
    /// names the field. The conversation fills all the same, the field is asked for once and
    /// from then on not, and nothing there is offered to be ended.
    func testOnAPageWhoseChatKeepsNoActingTheConversationFillsAndNothingIsOfferedToEnd() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter("""
            window.byKind = true;
            window.noFields = ['acting'];
            direct({id: 's1', author: {agent: 'assistant'}, text: 'The poem', share: {access: 'edit', open: true, ended: false},
              attachments: [{at: {app: 'so.tilo.word-processor'}}],
              acting: {by: {member: {agent: 'assistant'}}, for: [{by: {member: {person: ADA}}}]}})
            """)
        try await until("the conversation is held") { self.held.count == 1 }
        guard case .message(let shared) = held[0].content else { return XCTFail("a message") }
        XCTAssertEqual(shared.live.map(\.title), ["The poem"])
        XCTAssertEqual(shared.live.map(\.theirsToEnd), [false])
        _ = try await page("direct({id: 'm2', author: {person: ADA}, text: 'Thank you'}); return ''")
        try await until("what follows is held") { self.held.count == 2 }
        let named = try await page("return JSON.stringify(window.heard.filter(h => h.action === 'read' && Object.values(h.args?.where ?? {}).includes(DIRECT)).map(h => (h.args.fields ?? []).includes('acting')))")
        let reads = (JSON(text: named)?.array ?? []).map { $0.bool ?? false }
        XCTAssertEqual(reads.first, true, "the first read names the field: \(named)")
        XCTAssertEqual(reads.last, false, "the reads after its refusal do not: \(named)")
        XCTAssertEqual(reads.filter { $0 }.count, 1, "it is named once: \(named)")
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
        XCTAssertEqual(sessions.last, .signedIn(Person(id: .init(Self.ada), name: "Ada Lovelace", workspace: "Ada's workspace", workspaceRoute: "/w/ws-1")))
        // What the conversation held, oldest first, and only its own pieces.
        guard case .all(let first) = changes.first else { return XCTFail("the first change is not the whole conversation") }
        XCTAssertEqual(first.map(words), ["Good morning", "Good morning, Ada"])
        XCTAssertEqual(first.map(\.id.value), ["chat.messages/messages/m1", "chat.messages/messages/m3"])
        guard case .message(let mine) = first[0].content, case .message(let theirs) = first[1].content else { return XCTFail("no messages") }
        XCTAssertEqual(mine.author, .person)
        XCTAssertEqual(theirs.author, .assistant)
        // A message posted elsewhere arrives at the page's word, as an addition. This one carries a view.
        try await run("post({id: 'm4', channel: 'c_chat', author: 'agent-1', text: 'One more thing', view: {source: 'view areas() { for a in tasks.tracker.areas { text(a.name) } }', reads: ['tasks'], version: 1, at: 5, earlier: []}})")
        try await until("the new piece is told") { self.held.count == 3 }
        guard case .added(let added) = changes.last else { return XCTFail("a new piece was not told as added") }
        XCTAssertEqual(added.map(words), ["One more thing"])
        // The view stands in its message as a live thing named by the message and its conversation,
        // sized by what Tilo's page says of it. The view's text is nowhere in what the app keeps of it.
        guard case .message(let carrying) = added.first?.content else { return XCTFail("the new piece is no message") }
        XCTAssertEqual(carrying.live.map(\.reference), ["view:m4:c_chat"])
        XCTAssertEqual(carrying.live.map(\.title), ["Tasks"])
        XCTAssertEqual(carrying.live.map(\.sizesItself), [true])
        XCTAssertEqual(carrying.live.first?.reference.contains("tracker"), false)
        XCTAssertTrue(mine.live.isEmpty)
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
        XCTAssertEqual(request.state, .waiting)
        let typed = "sk-typed-into-the-protected-field"
        try await source.keep(Secret(typed), for: request.id)
        try await until("the request says the vault has it") {
            if case .secret(let now)? = self.held.first?.content { now.state == .kept } else { false }
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

    /// A mail account's password as its person's server asks for it: the question above, for two
    /// servers, typed and kept, and told by the server that one of them did not take it.
    private static let refusedPassword = secretQuestion
        .replacingOccurrences(of: "['https://api.example:8443']",
                              with: "['imap://imap.provider.example:993', 'smtp://smtp.provider.example:465']")
        .replacingOccurrences(of: "answered: null, outcome: null", with: """
            answered: {answer: 'filled', device: '', at: 1794900100, detail: JSON.stringify({reference: 'vault.item:0a'})}, \
            outcome: {outcome: 'failed', detail: 'That password was not accepted by imap.provider.example.'}
            """)

    func testAPasswordItsServerDidNotTakeSaysSoAndItsCardNamesBothServers() async throws {
        try await start()
        try await enter(device: true, Self.refusedPassword)
        guard case .secret(let request) = held[0].content else { return XCTFail("the request for a secret is none") }
        // Both servers the password belongs to, not the first alone.
        XCTAssertEqual(request.name, "imap.provider.example, smtp.provider.example")
        // Typed and kept is not signed in: the card says what the server said, not that the vault has it.
        XCTAssertNotEqual(request.state, .kept)
        XCTAssertEqual(request.state, .refused("That password was not accepted by imap.provider.example."))
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
            if case .secret(let now)? = self.held.first?.content { now.state == .declined } else { false }
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
        try await until("the device's entry was asked on the way in") {
            (try? await self.page("return String(window.asks.filter(what => what !== 'agents read').length)")) == "11"
        }
        // Ten changes of Chat's records: the conversation follows, and the host is asked nothing.
        for n in 0..<10 {
            try await run("post({id: 'n\(n)', channel: 'c_chat', author: 'agent-1', text: 'Note \(n)'})")
        }
        try await until("the conversation followed") { self.held.count == 10 }
        try await Task.sleep(for: .milliseconds(300))
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what !== 'agents read'))")
        // The first is what she was told so far: its log whole, read before her conversation is said,
        // so that nothing old is told again. The last four are what came while she was away, then the
        // ask her server is to hold until the log moves. This one answers it at once with nothing,
        // three times in a row, and is asked no more.
        XCTAssertEqual(asked, #"["tilo.telling list","tilo.models list","tilo.vault list","tilo.learned list","tilo.telling read","tilo.events list","tilo.connections list","tilo.telling since-you-left","tilo.telling list","tilo.telling list","tilo.telling list"]"#)
        // The workspace's agents are Assistant's: her server is asked for them on the way in and
        // nothing of them per change, however many changes follow.
        let roster = try await page("return String(window.asks.filter(what => what === 'agents read').length)")
        XCTAssertEqual(roster, "1", "the workspace's agents are read once on the way in")
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
        // She is signed in from the moment the page says whose device this is, before it went there.
        try await until("the app no longer keeps the workspace it was let in at") {
            self.defaults.string(forKey: "TiloLiveRoute") != "/w/ws-1"
        }
        XCTAssertEqual(asked.count, 1)
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/@host.example:8443/w/own-7")
    }

    func testWhatTiloCannotDoFromHereYetIsRefusedWithASentence() async throws {
        try await start()
        try await enter()
        let refused: [(String, () async throws -> Void)] = [
            ("You have no assistant in this workspace yet", { try await self.source.addTask("Renew the passport") }),
            ("No password manager is connected to Tilo yet", { try await self.source.fetchSecret(for: "k1") }),
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

    // MARK: What her server told her

    /// A thing her server told her about a message, as its log holds one.
    private static func told(_ place: Int, _ kind: String, about message: String, title: String = "", important: Bool = false) -> String {
        """
        tell({seq: \(place), id: 'turn:ws-1:\(message):0', atMs: 1794900000000, kind: '\(kind)', title: '\(title)', words: 'said',
          about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/\(message)'},
          mark: {important: \(important), why: {code: '\(kind)'}}, reaches: true})
        """
    }

    func testTheAnswerToAReminderStandsUnderItsLineAndIsTheReminderOnceHerServerSaysSo() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        // Her reminder comes: what her server tells the assistant then, and its answer to that.
        try await run("""
            direct({id: 'm_ask_1', author: {agent: 'e-9'}, text: 'Remind me to stretch. (for the assistant)',
              cause: {kind: 'reminder', label: 'Stretch', key: 'k-1', person: {person: ADA}}});
            direct({id: 'm_reply_1', author: {agent: 'assistant'}, text: 'Time to stretch.', parent: 'm_ask_1'})
            """)
        try await until("the answer stands under the reminder's line") { self.held.last?.task?.title == "Stretch" }
        // What her server told the assistant is no piece of her conversation, and the answer is plain words so far.
        XCTAssertEqual(held.map(words), ["Time to stretch."])
        try await run(Self.told(1, "reminder", about: "m_ask_1", title: "Stretch"))
        try await until("the answer is her reminder") { self.held.last?.content == .reminder("Time to stretch.") }
        XCTAssertEqual(held.count, 1)
        XCTAssertFalse(held[0].isImportant)
        XCTAssertTrue(changes.contains { if case .changed(let item) = $0 { item.content == .reminder("Time to stretch.") } else { false } })
        // She saw it on this device: her server hears so, once.
        await source.seen(held[0].id, in: .main)
        await source.seen(held[0].id, in: .main)
        let heard = try await page("return JSON.stringify(window.acknowledged.map(one => [one.told, one.by]))")
        XCTAssertEqual(heard, #"[["turn:ws-1:m_ask_1:0","opened"]]"#)
    }

    func testABriefAndWhatHerRuleCallsImportantAreMarkedAsHerServerToldThem() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        try await run("""
            direct({id: 'm_ask_2', author: {agent: 'e-2'}, text: 'Brief me.',
              cause: {kind: 'brief', label: 'Northwind call in 15 minutes', key: 'k-2', person: {person: ADA}}});
            direct({id: 'm_reply_2', author: {agent: 'assistant'}, text: 'Northwind led your seed.', parent: 'm_ask_2'});
            direct({id: 'm_news', author: {agent: 'assistant'}, text: 'Maya asks for the October numbers.'})
            """)
        try await until("both stand in the main chat") { self.held.count == 2 }
        XCTAssertFalse(held.contains(where: \.isImportant))
        try await run(Self.told(1, "brief", about: "m_ask_2", title: "Northwind call in 15 minutes", important: true) + "; "
            + Self.told(2, "news", about: "m_news", important: true))
        try await until("the brief and the mark are there") { self.held.count == 2 && self.held.allSatisfy(\.isImportant) }
        XCTAssertEqual(held[0].content, .brief(Brief(title: "Northwind call in 15 minutes", text: "Northwind led your seed.")))
        XCTAssertNil(held[0].task, "a brief carries its own line")
        XCTAssertEqual(words(held[1]), "Maya asks for the October numbers.")
    }

    func testWhatFollowsAnAnsweredQuestionStandsUnderItAndCarriesNoLine() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        // She asks. Her assistant asks her before it acts, and its reply ends there. She allows it: what her
        // server then tells the assistant is no piece of her conversation, and the assistant's next words are
        // its answer to that.
        _ = try await page("""
            direct({id: 'm_1', author: {person: ADA}, text: 'Send the numbers to Maya.'});
            direct({id: 'm_r1', author: {agent: 'assistant'}, text: 'I asked you below.', parent: 'm_1'});
            direct({id: 'm_q', author: {agent: 'assistant'}, text: 'I need your answer before I do this: Send (Mail).',
              question: {kind: 'choice', person: ADA, answers: [{id: 'allow', label: ''}, {id: 'deny', label: ''}], detail: '',
                         answered: {answer: 'allow'}, outcome: null, withdrawn: null}});
            direct({id: 'm_ask_q', author: {agent: 'question'}, text: 'Your person allowed it. (for the assistant)',
              cause: {kind: 'news', label: 'I need your answer before I do this: Send (Mail).', key: 'question:m_q', person: {person: ADA}}});
            direct({id: 'm_r2', author: {agent: 'assistant'}, text: 'It is sent.', parent: 'm_ask_q'})
            """)
        try await until("what follows the answer is held") { self.held.last.map(self.words) == "It is sent." }
        // In the order things happened: her words, the reply that ended where it asked, the question, what followed.
        XCTAssertEqual(held.map(words), ["Send the numbers to Maya.", "I asked you below.",
                                         "I need your answer before I do this: Send (Mail).", "It is sent."])
        // A task line names a task. The question's own sentence is none, and nothing is watched here.
        XCTAssertNil(held.last?.task, "what follows an answered question carries no line made of the question's sentence")
        XCTAssertTrue(held.allSatisfy { $0.task == nil })
    }

    func testALogThatMovesOnceWithNothingForThisDeviceDoesNotEndTheFollowing() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        // Her server's log moves for something this device is not told: the ask it held comes back at once, with nothing.
        try await until("the ask is held") { (try? await self.page("return String(typeof window.logMoved)")) == "function" }
        _ = try await page("window.logMoved(); window.logMoved = undefined")
        try await until("it is asked again, and held") { (try? await self.page("return String(typeof window.logMoved)")) == "function" }
        // What she is told after that still reaches the app.
        _ = try await page("""
            direct({id: 'm_ask_6', author: {agent: 'e-6'}, text: 'Remind me to stretch. (for the assistant)',
              cause: {kind: 'reminder', label: 'Stretch', key: 'k-6', person: {person: ADA}}});
            direct({id: 'm_reply_6', author: {agent: 'assistant'}, text: 'Time to stretch.', parent: 'm_ask_6'})
            """)
        _ = try await page(Self.told(1, "reminder", about: "m_ask_6", title: "Stretch"))
        try await until("the answer is her reminder") { self.held.last?.content == .reminder("Time to stretch.") }
    }

    func testAServerThatTellsNothingThisWayIsAskedOnceAndTheConversationStandsAsItIs() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.told = null")
        try await run("direct({id: 'm_1', author: {agent: 'assistant'}, text: 'Good morning.'})")
        try await until("the message stands") { self.held.map(self.words) == ["Good morning."] }
        try await run("direct({id: 'm_2', author: {agent: 'assistant'}, text: 'Nine mails sorted.'})")
        try await until("the next one stands") { self.held.count == 2 }
        let asked = try await page("return String(window.asks.filter(what => what === 'tilo.telling list').length)")
        XCTAssertEqual(asked, "1")
    }

    /// A question of the assistant's in her conversation with it, as Chat keeps one that waits.
    private static func asks(_ id: String, _ text: String, _ answers: [String]) -> String {
        """
        direct({id: '\(id)', author: {agent: 'assistant'}, text: '\(text)',
          question: {kind: 'choice', person: ADA, answers: [\(answers.map { "{id: '\($0)', label: ''}" }.joined(separator: ", "))], detail: '',
                     answered: null, outcome: null, withdrawn: null}})
        """
    }

    func testWhatHerServerKeptFromNotifyingSaysWhyAndWhatInsistsIsRaisedAgainUntilSheSawIt() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        _ = try await page("""
            direct({id: 'm_ask_3', author: {agent: 'e-3'}, text: 'Sort the mail. (for the assistant)',
              cause: {kind: 'event', label: 'Sort the mail', key: 'k-3', person: {person: ADA}}});
            direct({id: 'm_reply_3', author: {agent: 'assistant'}, text: 'Nine mails sorted.', parent: 'm_ask_3'});
            direct({id: 'm_ask_4', author: {agent: 'e-4'}, text: 'Remind me to call. (for the assistant)',
              cause: {kind: 'reminder', label: 'Call', key: 'k-4', person: {person: ADA}}});
            direct({id: 'm_reply_4', author: {agent: 'assistant'}, text: 'Call now.', parent: 'm_ask_4'})
            """)
        try await until("both answers stand") { self.held.count == 2 }
        XCTAssertNil(held[0].keptQuiet)
        // Her server tells the first as news that does not reach her, and gives its reason.
        _ = try await page("""
            tell({seq: 1, id: 'turn:ws-1:m_ask_3:0', atMs: 1794900000000, kind: 'news', title: 'Sort the mail', words: 'Nine mails sorted.',
              about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/m_ask_3'},
              mark: {important: false, why: {code: 'nothing-holds', fact: 'travelling', value: 'no'}}, reaches: false, actions: []})
            """)
        try await until("the piece says why it did not notify") { self.held.first?.keptQuiet != nil }
        XCTAssertEqual(held[0].keptQuiet, "Sort the mail did not notify you: not important while travelling is no.")
        XCTAssertEqual(NoticePlan.notice(for: held[0], in: .main)?.kind, .quiet)
        XCTAssertEqual(words(held[0]), "Nine mails sorted.", "its words in the conversation stay as they were")

        // The reminder insists: each time her server raises it again, the piece counts it.
        func raised(_ place: Int, _ count: Int) -> String {
            """
            tell({seq: \(place), id: 'turn:ws-1:m_ask_4:0', atMs: 1794900000000, kind: 'reminder', title: 'Call', words: 'Call now.',
              about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/m_ask_4'},
              mark: {important: false, why: {code: 'reminder'}}, reaches: true, actions: [],
              insists: {everySecs: 60, times: 3, raised: \(count), nextAtMs: 1794900060000}})
            """
        }
        _ = try await page(raised(2, 0))
        try await until("the answer is her reminder") { self.held.last?.content == .reminder("Call now.") }
        XCTAssertEqual(held[1].raised, 0)
        XCTAssertNil(held[1].keptQuiet)
        _ = try await page(raised(3, 1))
        try await until("it was raised again") { self.held.last?.raised == 1 }
        XCTAssertTrue(changes.contains { if case .changed(let item) = $0 { item.raised == 1 } else { false } })
        // She saw it on this device: her server hears so, and nothing counts as raised any more.
        await source.seen(held[1].id, in: .main)
        try await until("it is settled") { self.held.last?.raised == 0 }
        let heard = try await page("return JSON.stringify(window.acknowledged.map(one => [one.told, one.by]))")
        XCTAssertEqual(heard, #"[["turn:ws-1:m_ask_4:0","opened"]]"#)
    }

    func testAQuestionHerServerTellsOffersItsAnswersUnderItsNoticeAndAPressGivesThatAnswer() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        _ = try await page(Self.asks("q1", "May I use the key of the reports server?", ["allow", "always", "deny"]))
        try await until("the question stands") { self.held.count == 1 }
        XCTAssertEqual(held[0].offers, [])
        func action(_ answer: String, main: Bool = false) -> String {
            "{id: '\(answer)', label: '', main: \(main), target: {app: 'so.tilo.chat', resource: 'chain:messages', action: 'answer-question'}, arguments: {message: 'q1', answer: '\(answer)'}}"
        }
        _ = try await page("""
            tell({seq: 1, id: 'question:ws-1:q1', atMs: 1794900000000, kind: 'question', title: '', words: 'May I use the key of the reports server?',
              about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/q1'},
              mark: {important: false, why: {code: 'question'}}, reaches: true,
              actions: [\(action("allow", main: true)), \(action("always")), \(action("deny")),
                {id: 'elsewhere', label: 'Open the sheet', main: false, target: {app: 'so.tilo.sheets', resource: 'sheets', action: 'open'}, arguments: {}}]})
            """)
        try await until("its answers are offered") { self.held.first?.offers.isEmpty == false }
        // A permission that is to stay is not offered under a notice, nor what another app would have to do.
        XCTAssertEqual(held[0].offers, [Offer(id: "allow", title: "Allow", isMain: true), Offer(id: "deny", title: "Deny")])
        XCTAssertEqual(NoticePlan.notice(for: held[0], in: .main)?.offers.map(\.title), ["Allow", "Deny"])
        // What is not offered cannot be pressed.
        for refused in ["always", "elsewhere", "nothing"] {
            do {
                try await source.take(.init(refused), of: held[0].id, in: .main)
                XCTFail("\(refused) was taken")
            } catch let problem as SourceProblem {
                XCTAssertEqual(problem, .refused("This is no longer offered"))
            }
        }
        let before = try await page("return String(window.heard.filter(call => call.action === 'answer-question').length)")
        XCTAssertEqual(before, "0")
        // The press gives that answer the way the app's own button does, and her server hears which it was.
        try await source.take("allow", of: held[0].id, in: .main)
        let answered = try await page("return JSON.stringify(window.heard.filter(call => call.action === 'answer-question').map(call => [call.resource, call.args]))")
        XCTAssertEqual(JSON(text: answered), [["chain:messages", ["answer": "allow", "message": "q1"]]])
        let heard = try await page("return JSON.stringify(window.acknowledged.map(one => [one.told, one.by]))")
        XCTAssertEqual(heard, #"[["question:ws-1:q1","allow"]]"#)
        try await until("an answered question offers nothing more") { self.held.first?.offers == [] }
    }

    func testWhatCameWhileSheWasAwayIsAskedOnceAndLeadsToThePiecesSheHolds() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        listening.append(Task { for await since in self.source.sinceLeft() { self.sinces.append(since) } })
        try await enter(device: true, """
            window.byKind = true; window.holds = 60000;
            const thing = (seq, message, kind, words, more = {}) => ({seq, id: 'turn:ws-1:' + message + ':0', atMs: 1794900000000, kind,
              title: '', words, about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/' + message},
              mark: {important: false, why: {code: 'no-rule'}}, reaches: kind !== 'news', ...more});
            window.told = [thing(1, 'm_ask_5', 'news', 'Nine mails sorted.'),
              thing(2, 'q5', 'question', 'Send the reply?', {mark: {important: true, why: {code: 'question'}}})];
            window.since = {leftAtMs: 1794899000000, backAtMs: 1794900500000, waits: [window.told[1]], didAlone: [window.told[0]],
              cameIn: [thing(3, 'gone', 'news', '**An old** note')]};
            direct({id: 'm_ask_5', author: {agent: 'e-5'}, text: 'Sort the mail. (for the assistant)',
              cause: {kind: 'event', label: 'Sort the mail', key: 'k-5', person: {person: ADA}}});
            direct({id: 'm_reply_5', author: {agent: 'assistant'}, text: 'Nine mails sorted.', parent: 'm_ask_5'});
            \(Self.asks("q5", "Send the reply?", ["send", "no"]))
            """)
        try await until("what came meanwhile is said, with the pieces she holds") {
            (self.sinces.last ?? nil)?.didAlone.first?.item != nil && (self.sinces.last ?? nil)?.waits.first?.item != nil
        }
        let said = try XCTUnwrap(sinces.last ?? nil)
        XCTAssertEqual(said.left, Date(timeIntervalSince1970: 1_794_899_000))
        XCTAssertEqual(said.waits, [SinceLeft.Line(id: "turn:ws-1:q5:0", words: "Send the reply?", item: "chat.messages/messages/q5", isImportant: true)])
        XCTAssertEqual(said.didAlone, [SinceLeft.Line(id: "turn:ws-1:m_ask_5:0", words: "Nine mails sorted.", item: "chat.messages/messages/m_reply_5")])
        // What she no longer holds a piece of is said without one, in plain words.
        XCTAssertEqual(said.cameIn, [SinceLeft.Line(id: "turn:ws-1:gone:0", words: "An old note")])
        XCTAssertEqual(held.first?.keptQuiet, "This did not notify you: you have not said what is important to you.")
        let asked = try await page("return String(window.asks.filter(what => what === 'tilo.telling since-you-left').length)")
        XCTAssertEqual(asked, "1", "her server counts that stretch once, at her first look after it")
    }

    func testTheAssistantViewNamesTheWorkspaceTheAssistantWorksInAndNoOther() async throws {
        try await start(.assistant)
        listening.append(Task { for await facts in self.source.supervision() { self.supervisions.append(facts) } })
        try await enter(device: true, "window.byKind = true; window.agents = [kept('reed', {name: 'Reed'})]")
        try await until("the workspace the assistant works in is named") { self.supervisions.last?.workspaces == ["Ada's workspace"] }
        XCTAssertEqual(Wording().workspaces(try XCTUnwrap(supervisions.last)).second, "Your assistant works in this workspace.")
    }

    func testWhereTheAppFoundNoAssistantTheAssistantViewNamesNoWorkspace() async throws {
        try await start()
        listening.append(Task { for await facts in self.source.supervision() { self.supervisions.append(facts) } })
        try await enter(device: true)
        try await until("what her server keeps was read") { self.supervisions.last?.importance.isEmpty == false }
        XCTAssertEqual(supervisions.last?.workspaces, [])
    }

    // MARK: The first run

    private func agentsKept() async throws -> String {
        try await page("return JSON.stringify(window.agents.map(one => one.id))")
    }

    /// Whom Chat's own send was last asked to send to.
    private func sentTo() async throws -> JSON? {
        JSON(text: try await page("return JSON.stringify(window.heard.filter(h => h.action === 'send').map(h => h.args.to[0]))"))
    }

    /// A start that finds its assistant: from the moment the page is in until the main chat
    /// stands the source says one thing, that it looks, and never that anything is being made.
    func testAStartThatFindsItsAssistantSaysOneThingUntilTheMainChatStands() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.agents = [kept('reed', {name: 'Reed'})]")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertEqual(Array(setups.drop { $0 == .unknown }), [.findingAssistant, .ready],
                       "between the look for her assistant and the main chat that stands something else was said")
        let made = try await page("return String(window.asks.filter(what => what === 'agents ensure-personal-assistant').length)")
        XCTAssertEqual(made, "0")
    }

    /// A first start, where the person keeps no assistant and one is made: only then is it said
    /// that an assistant is being made, between the look and the main chat that stands.
    func testOnlyAStartThatMakesAnAssistantSaysThatOneIsBeingMade() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.agents = []")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let said = Array(setups.drop { $0 == .unknown })
        XCTAssertEqual(said.count, 3, "\(said)")
        XCTAssertEqual(said.first, .findingAssistant)
        XCTAssertEqual(said.last, .ready)
        XCTAssertFalse([.unknown, .findingAssistant].contains(said.dropFirst().first), "nothing said that her assistant was being made: \(said)")
        XCTAssertEqual(said, [.findingAssistant, .makingAssistant, .ready])
        let made = try await page("return String(window.asks.filter(what => what === 'agents ensure-personal-assistant').length)")
        XCTAssertEqual(made, "1")
    }

    func testWhereThePersonKeepsNoAssistantTheAppMakesItOnTheirServersWordAndWritesToIt() async throws {
        try await start(.assistant)
        // The workspace keeps agents, and none of them is hers to write to: another person's,
        // one an outside client connects as, one that was retired, and a sub-agent of hers.
        try await enter(device: true, """
            window.byKind = true;
            window.agents = [kept('scout', {keeper: {person: 'ff'.repeat(32)}}), kept('bridge', {mcp_client: 'client-1'}),
              kept('old', {state: {$variant: 'retired'}}), kept('helper', {sub: {role: 'Researcher', stands: {$variant: 'working'}}})]
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertEqual(setups.first { $0 != .unknown }, .findingAssistant)
        // Her server was read first, and only its word that she keeps none made one: the
        // action takes the keeper from the signed-in caller.
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('agents ')).map(what => what.split(' ')[1]))")
        let steps = JSON(text: asked)?.array?.compactMap(\.string) ?? []
        XCTAssertEqual(steps.first, "read")
        XCTAssertEqual(steps.filter { $0 != "read" }, ["ensure-personal-assistant"])
        let with = try await page("return JSON.stringify(window.madeWith)")
        XCTAssertEqual(with, "null")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["scout","bridge","old","helper","assistant"]"#)
        // The main chat is her conversation with it.
        try await source.send(Draft(text: "Good morning"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "assistant", "name": "Assistant"]])
        try await until("what was sent stands in the main chat") { self.held.map(self.words) == ["Good morning"] }
        // Nothing was made through the page's own window.
        let throughPage = try await page("return String(window.heard.filter(h => h.action === 'ensure-personal-assistant').length)")
        XCTAssertEqual(throughPage, "0")
    }

    func testASecondDeviceFinishingSetupIsUsedWithoutRetiringAnyAgent() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.agents = []; window.raced = true")
        try await until("the other device's assistant was found") { self.setups.last == .ready }
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "other-device", "name": "Assistant"]])
        let writes = try await page("return JSON.stringify(window.asks.filter(a => a.startsWith('agents ') && a !== 'agents read'))")
        XCTAssertEqual(writes, #"["agents ensure-personal-assistant"]"#)
    }

    func testANativePersonalAssistantReadSelectsTheConversation() async throws {
        try await start(.assistant)
        try await enter(device: true, """
            window.byKind = true; window.wholeRows = true;
            window.agents = [kept('first', {name: 'First'}), kept('native', {name: 'Native'})]; window.personal = {[ADA]: 'native'}
            """)
        try await until("the native selection was read") { self.setups.last == .ready }
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "native", "name": "Native"]])
    }

    func testAServerThatListsTheNewAssistantLateAndLeavesAnAskUnansweredIsAskedAgainAndNothingIsSaid() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.agents = []; window.late = 2; window.unanswered = 1")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains { if case .noAssistant = $0 { true } else { false } }, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
        let made = try await page("return String(window.asks.filter(what => what === 'agents ensure-personal-assistant').length)")
        XCTAssertEqual(made, "1", "it is made once, however often the rest is asked")
    }

    func testAnExistingAssistantIsUsedWithoutModifyingItOrOtherAgents() async throws {
        try await start(.assistant)
        // Both records survive setup with their original IDs.
        try await enter(device: true, "window.byKind = true; window.agents = [kept('assistant', {name: 'Assistant'}), kept('reed', {name: 'Reed', created_at: 1794900005000})]")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant","reed"]"#)
        let made = try await page("return String(window.asks.filter(what => what === 'agents ensure-personal-assistant').length)")
        XCTAssertEqual(made, "0")
    }

    func testAnAssistantThePersonKeepsIsFoundAndNoneIsMade() async throws {
        try await start(.assistant)
        // Two of hers: the earliest made is the assistant.
        try await enter(device: true, """
            window.byKind = true;
            window.agents = [kept('later', {name: 'Later', created_at: 1794900005000}), kept('reed', {name: 'Reed', keeper: ADA.toUpperCase(), created_at: 1794900001000}),
              kept('scout', {keeper: {person: 'ff'.repeat(32)}, created_at: 1794900000000})]
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "reed", "name": "Reed"]])
        let changed = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('agents ') && !what.endsWith(' read')))")
        XCTAssertEqual(changed, "[]")
    }

    /// As the second site's server answered on 17 November 2026: its list holds each agent
    /// whole, the keeper a bare key and the state a bare word, and it offers no read of one agent.
    func testAServerThatSaysEachAgentWholeInItsListIsTakenAtItsWordAndTheAssistantIsMade() async throws {
        try await start(.assistant)
        try await enter(device: true, """
            window.byKind = true; window.wholeRows = true;
            window.agents = [kept('scout', {keeper: 'ff'.repeat(32), state: 'active'})]
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        // None of hers was listed, so setup was requested once.
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('agents ')).map(what => what.split(' ')[1]))")
        XCTAssertEqual(JSON(text: asked)?.array?.compactMap(\.string).filter { $0 != "read" }, ["ensure-personal-assistant"])
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["scout","assistant"]"#)
        try await source.send(Draft(text: "Good morning"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "assistant", "name": "Assistant"]])
    }

    func testAServerThatSaysEachAgentWholeInItsListIsTakenAtItsWordAndTheAssistantIsFound() async throws {
        try await start(.assistant)
        try await enter(device: true, """
            window.byKind = true; window.wholeRows = true;
            window.agents = [kept('reed', {name: 'Reed', keeper: ADA, state: 'active'}),
              kept('gone', {keeper: ADA, state: 'retired', created_at: 1794800000000})]
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "reed", "name": "Reed"]])
        // Nothing was made, and no agent's own read was asked for.
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('agents ') && !what.endsWith(' read')))")
        XCTAssertEqual(asked, "[]")
    }

    /// As main's page does once after it let a device in: it starts over while its server is asked.
    func testAPageThatStartsOverWhileItsServerIsAskedLeavesNoWrongSentenceAndIsAskedAgainOnceItIsIn() async throws {
        try await start(.assistant)
        try await run("window.byKind = true; window.hangs = true; window.agents = [kept('reed', {name: 'Reed'})]; location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("her server is asked for the agents") {
            (try? await self.page("return String(window.asks.includes('agents read'))")) == "true"
        }
        XCTAssertEqual(setups.last, .findingAssistant)
        // The page starts over under the ask, and is in again a moment later.
        try await run("document.documentElement.dataset.entry = ''; document.documentElement.dataset.arrival = 'load'")
        try await until("the page is on its way in again") { if case .arriving = self.source.engine.state { true } else { false } }
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(setups.last, .findingAssistant, "nothing is said while the page is on its way in")
        try await run("window.hangs = false; enter(true)")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains { if case .noAssistant = $0 { true } else { false } })
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "reed", "name": "Reed"]])
    }

    /// As main's page did once in the proof against a site: it was in before it handed over its device's entry.
    func testAPageThatHandsOverItsDevicesEntryAMomentAfterItIsInIsWaitedFor() async throws {
        try await start(.assistant)
        try await run("""
            window.byKind = true; window.agents = [kept('reed', {name: 'Reed'})]; location.hash = '#/w/ws-1/so.tilo.chat'; enter(false);
            setTimeout(() => enter(true), 700)
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains { if case .noAssistant = $0 { true } else { false } })
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "reed", "name": "Reed"]])
    }

    func testAnUnansweredAskForThePersonsOwnWorkspaceIsAskedAgainAndTheAssistantIsMadeThere() async throws {
        try await start(.assistant, livesInOwn: true)
        // Her server does not say at first which workspace is her own; a moment later it does.
        try await run("""
            window.byKind = true; window.agents = [];
            const own = await device.ownWorkspace();
            window.noOwn = true; location.hash = '#/@' + own.origin.slice(8) + '/w/' + own.id + '/so.tilo.chat'; enter(true);
            setTimeout(() => { window.noOwn = false; }, 500)
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains { if case .noAssistant = $0 { true } else { false } })
        let made = try await agentsKept()
        XCTAssertEqual(made, #"["assistant"]"#)
    }

    func testFirstUseRetriesPersonalAssistantSetupWhenItsActionIsPublishedAfterTheRosterRead() async throws {
        try await start(.assistant, livesInOwn: true)
        try await run("""
            window.wholeRows = true; window.agents = []; window.personal = {}; window.unlinked = true;
            window.ensureNotPublished = true;
            location.hash = '#/@host.example:8443/w/own-7/so.tilo.chat'; return await enter(true, true)
            """)
        try await until("the fresh workspace reaches provider setup without another press") {
            if case .needsProvider = self.setups.last { return true }
            return false
        }
        XCTAssertTrue(source.engine.hosted)
        let asked = try await page("return JSON.stringify(window.hostAsked.filter(a => a.startsWith('record:so.tilo.assistant: ')))")
        let actions = try JSONDecoder().decode([String].self, from: Data(asked.utf8))
        XCTAssertEqual(actions.first, "record:so.tilo.assistant: read")
        XCTAssertEqual(actions.filter { $0 == "record:so.tilo.assistant: ensure-personal-assistant" }.count, 2)
        XCTAssertEqual(actions.last, "record:so.tilo.assistant: read")
        let made = try await agentsKept()
        XCTAssertEqual(made, #"["assistant"]"#)
    }

    func testWhereTheServerRefusesToMakeOneTheMainChatSaysWhyAndAskingAgainMakesIt() async throws {
        try await start(.assistant)
        try await run("window.byKind = true; window.agents = []; window.mayNotMake = true; location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the server's refusal is said") {
            self.setups.last == .noAssistant("Your role in this workspace may not make an agent")
        }
        let refused = try await page("return String(window.asks.filter(a => a === 'agents ensure-personal-assistant').length)")
        XCTAssertEqual(refused, "1", "a role refusal must reach the person on the first answer")
        do {
            try await source.send(Draft(text: "Hello"), to: .main)
            XCTFail("something was sent to nobody")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("You have no assistant in this workspace yet"))
        }
        // Asked again once the server lets her, it is made.
        try await run("window.mayNotMake = false")
        try await source.makeAssistant()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let made = try await agentsKept()
        XCTAssertEqual(made, #"["assistant"]"#)
    }

    func testAnAssistantIsMadeInThePersonsOwnWorkspaceAndNowhereElse() async throws {
        try await start(.assistant, livesInOwn: true)
        try await run("window.byKind = true; window.agents = []; location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the person's own workspace is asked for") { !self.asked.isEmpty }
        // Nothing was made where the link led.
        var made = try await page("return String(window.agents.length)")
        XCTAssertEqual(made, "0")
        try await run("location.hash = '#/@host.example:8443/w/own-7/so.tilo.chat'")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        made = try await agentsKept()
        XCTAssertEqual(made, #"["assistant"]"#)
    }

    func testWhereTheServerDoesNotSayWhichWorkspaceIsThePersonsOwnNoAssistantIsMade() async throws {
        try await start(.assistant, livesInOwn: true)
        try await run("window.byKind = true; window.agents = []; window.noOwn = true; location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the main chat says why") {
            self.setups.last == .noAssistant("Your assistant lives in your own workspace, and this Tilo did not say which one that is")
        }
        let made = try await page("return String(window.agents.length)")
        XCTAssertEqual(made, "0")
    }

    func testAnExistingAgentInAnotherAdmittedWorkspaceCannotBecomeThePersonalConversation() async throws {
        try await start(.assistant, livesInOwn: true)
        try await run("""
            window.byKind = true; window.admittedTo = 'shared'; window.agents = [kept('shared-agent')];
            location.hash = '#/@host.example:8443/w/own-7/so.tilo.chat'; enter(true)
            """)
        try await until("the signed-in workspace is checked") {
            self.setups.last == .noAssistant("Waiting for Tilo to open your own workspace")
        }
        let roster = try await page("return JSON.stringify(window.asks.filter(a => a.startsWith('agents ')))")
        XCTAssertEqual(roster, "[]")
        XCTAssertTrue(changes.isEmpty)
    }

    func testAServerThatDoesNotSayItsAgentsWholeIsNotTakenAtItsWordAndNoneIsMade() async throws {
        try await start(.assistant)
        // Her server lists an agent and lets her device read nothing of it: whether it is hers
        // is not known, so nothing says that she keeps none.
        try await run("window.byKind = true; window.agents = [kept('reed', {name: 'Reed'})]; window.rowsOnly = true; location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the main chat says so") {
            self.setups.last == .noAssistant("This Tilo does not let an app find or make your assistant yet")
        }
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('agents ') && !what.endsWith(' read')))")
        XCTAssertEqual(asked, "[]")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["reed"]"#)
    }

    /// The page shows Chat alone, which keeps no roster, and a page that hands an app no way
    /// to its server offers none anywhere: the main chat says so, nothing is listed, and none
    /// is made on what no page holds.
    func testAPageThatReachesNoServerOffersNoAssistantAndMakesNone() async throws {
        try await start(.assistant)
        var listed: [SubAgent] = []
        let listening = Task { for await agents in self.source.subAgents() { listed = agents } }
        defer { listening.cancel() }
        try await run("window.byKind = true; location.hash = '#/w/ws-1/so.tilo.chat'; enter(false)")
        try await until("the main chat says so") {
            self.setups.last == .noAssistant("This Tilo does not let an app find or make your assistant yet")
        }
        let made = try await page("return String(window.heard.filter(h => h.action === 'ensure-personal-assistant').length)")
        XCTAssertEqual(made, "0")
        XCTAssertTrue(listed.isEmpty, "nothing is listed")
    }

    /// Where nothing offers the workspace's agents the main chat says so. Where her device's
    /// entry comes in a moment later, her press finds her assistant in her server's word.
    func testWhereNothingOffersTheWorkspacesAgentsTheMainChatSaysSoAndFindsThemWhenTheDeviceGetsIn() async throws {
        try await start(.assistant)
        try await run("window.byKind = true; window.agents = [kept('reed', {name: 'Reed'})]; location.hash = '#/w/ws-1/so.tilo.chat'; enter(false)")
        try await until("the main chat says so") {
            self.setups.last == .noAssistant("This Tilo does not let an app find or make your assistant yet")
        }
        // The page hands her device's entry over: what her server says is found when she presses.
        try await run("enter(true)")
        try await source.makeAssistant()
        try await until("the assistant is found") { if case .noAssistant = self.setups.last { false } else { true } }
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await sentTo()
        XCTAssertEqual(sent, [["kind": "agent", "id": "reed", "name": "Reed"]])
        let made = try await page("return String(window.heard.filter(h => h.action === 'ensure-personal-assistant').length)")
        XCTAssertEqual(made, "0")
    }

    func testFirstUseOffersAProviderWhileTheConversationReadIsPending() async throws {
        try await start(.assistant)
        try await run("""
            window.byKind = true; window.unlinked = true; window.agents = [];
            window.chatReply = new Promise(resolve => { window.resumeChat = resolve; });
            direct({id: 'm1', author: {person: ADA}, text: 'Kept in Chat'});
            location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)
            """)
        do {
            try await until("Chat was asked for the personal conversation") {
                (try? await self.page("return String(window.heard.some(h => h.resource === 'chain:messages' && h.action === 'read' && h.args.where?.channel === DIRECT))")) == "true"
            }
            try await until("the provider step is offered while Chat is still reading") {
                guard case .needsProvider(let choices) = self.setups.last else { return false }
                return choices.contains(ProviderChoice(id: "openai", name: "OpenAI"))
            }
        } catch {
            try await run("window.resumeChat()")
            throw error
        }
        XCTAssertTrue(setups.contains(.makingAssistant))
        XCTAssertFalse(setups.contains(.ready))
        XCTAssertTrue(changes.isEmpty, "Chat has not answered the first read")
        let listed = try await page("return String(window.asks.includes('tilo.models list'))")
        XCTAssertEqual(listed, "true", "the provider choices follow the person's model listing")
        let writes = try await page("""
            return JSON.stringify(window.asks.filter(what => (what.startsWith('agents ') && what !== 'agents read')
                || (what.startsWith('tilo.models ') && what !== 'tilo.models list')))
            """)
        XCTAssertEqual(writes, #"["agents ensure-personal-assistant"]"#)
        try await run("window.resumeChat()")
        try await until("the held read delivers the original message") { self.held.map(self.words) == ["Kept in Chat"] }
    }

    /// Open the workspace's hosted page, where the admitted native device serves Chat without a local mount.
    private func enterHostedAssistant(_ prepare: String) async throws {
        try await start(.assistant, livesInOwn: true)
        listening.append(Task { for await facts in self.source.supervision() { self.supervisions.append(facts) } })
        try await run("""
            window.wholeRows = true; window.agents = [kept('someone-else'), kept('reed')];
            window.personal = {[ADA]: 'reed'};
            \(prepare)
            location.hash = '#/@host.example:8443/w/own-7/so.tilo.chat'; return await enter(true, true)
            """)
    }

    func testChoosingAModelNeedsTheHostedAssistantsOwnChatGrantBeforeItCanReply() async throws {
        try await enterHostedAssistant("""
            window.unlinked = true;
            window.added = [{id: 'p1', name: 'Local', configured: true, models: [{id: 'm1', label: 'First'}]}];
            window.chatGrant = {state: 'missing', ready: false, apps: [], expiresAtMs: null};
            """)
        try await until("the person's hosted workspace offers their model") {
            if case .needsModel = self.setups.last { return true }
            return false
        }
        XCTAssertTrue(source.engine.hosted)
        try await source.choose("p1/m1", for: .request, .task)
        try await until("the chosen model needs Chat access for the personal assistant") {
            self.setups.last == .chatPermission(.needed)
        }
        func requests() async throws -> String {
            try await page("return String(window.hostAsked.filter(a => a === 'agents request-chat-grant').length)")
        }
        let before = try await requests()
        XCTAssertEqual(before, "0")

        try await source.requestChatPermission()
        try await until("the person can compare the code from their signed request") {
            self.setups.last == .chatPermission(.pending(code: "493 127"))
        }
        await source.checkChatPermission()
        XCTAssertEqual(setups.last, .chatPermission(.pending(code: "493 127")))
        try await run("window.chatGrant.apps.push('so.tilo.tasks')")
        await source.checkChatPermission()
        try await until("a different pending scope drops the earlier comparison code") {
            self.setups.last == .chatPermission(.pending(code: nil))
        }
        try await run("window.chatGrant.state = 'denied'")
        await source.checkChatPermission()
        try await until("the root browser's refusal reaches the requester") { self.setups.last == .chatPermission(.declined) }
        let afterDenial = try await requests()
        XCTAssertEqual(afterDenial, "1")

        // The same visible route can outlive its admitted device. Its old permission card may
        // not send an agent request into the workspace the device now belongs to.
        try await run("window.admittedTo = 'another-workspace'")
        do {
            try await source.requestChatPermission()
            XCTFail("a stale workspace could request Chat access")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("Your account or workspace changed"))
        }
        let afterSwitch = try await requests()
        XCTAssertEqual(afterSwitch, afterDenial)
        try await run("window.admittedTo = 'own-7'")
        await source.checkChatPermission()
        try await until("the person's own refusal is read again") { self.setups.last == .chatPermission(.declined) }

        try await source.requestChatPermission()
        try await until("the person has another pending request") { self.setups.last == .chatPermission(.pending(code: "493 127")) }
        try await run("window.chatGrant.expiresAtMs += 60000")
        await source.checkChatPermission()
        try await until("a replacement with another expiry drops the comparison code") {
            self.setups.last == .chatPermission(.pending(code: nil))
        }
        try await run("Object.assign(window.chatGrant, {state:'approved', apps:['so.tilo.chat'], expiresAtMs: Date.now() + 3600000})")
        await source.checkChatPermission()
        try await until("root approval is waiting for a signing session") { self.setups.last == .chatPermission(.joining) }
        XCTAssertFalse(setups.contains(.ready))
        try await run("window.chatGrant.ready = true")
        try await until("the pending check observes that the Chat signer is available") { self.setups.last == .ready }
        let afterApproval = try await requests()
        XCTAssertEqual(afterApproval, "2", "only the person's two explicit requests write a grant")
        XCTAssertFalse(String(describing: defaults.dictionaryRepresentation()).contains("493 127"))
    }

    func testExpandingAHostedAssistantsApprovedChatScopeRequiresRevocationAndFreshConsent() async throws {
        try await enterHostedAssistant("""
            window.chatGrant = {state: 'approved', ready: true, apps: ['so.tilo.chat'], expiresAtMs: 2000000000000};
            """)
        try await until("the assistant's granted Chat access is reported") {
            self.setups.last == .ready && self.supervisions.last?.assistantAccess?.state == .approved
        }
        let chat = try XCTUnwrap(supervisions.last?.assistantAccess)
        XCTAssertEqual(chat.apps, ["so.tilo.chat"])
        XCTAssertTrue(chat.ready)
        XCTAssertEqual(chat.expires, Date(timeIntervalSince1970: 2_000_000_000))
        func decisions() async throws -> String {
            try await page("return JSON.stringify(window.hostAsked.filter(a => a.startsWith('agents ') && a !== 'agents grant-status'))")
        }
        // Asking for more cannot replace the already approved grant.
        do {
            try await source.requestAssistantWorkPermission()
        } catch is SourceProblem { }
        let before = try await decisions()
        XCTAssertEqual(before, "[]")
        try await source.revokeChatPermission()
        try await until("the old signer is revoked") {
            self.setups.last == .chatPermission(.revoked) && self.supervisions.last?.assistantAccess?.state == .revoked
        }
        let revoked = try await decisions()
        XCTAssertEqual(revoked, #"["agents revoke"]"#)
        try await source.requestAssistantWorkPermission()
        try await until("the new scope is awaiting its own approval") {
            self.setups.last == .chatPermission(.pending(code: "291 804")) && self.supervisions.last?.assistantAccess?.state == .pending
        }
        let apps: Set<String> = ["so.tilo.chat", "so.tilo.tasks", "so.tilo.assistant", "so.tilo.shortcuts"]
        let pending = try XCTUnwrap(supervisions.last?.assistantAccess)
        XCTAssertEqual(Set(pending.apps), apps)
        XCTAssertFalse(pending.ready)
        try await run("Object.assign(window.chatGrant, {state: 'approved', ready: true, expiresAtMs: 2000100000000})")
        await source.checkChatPermission()
        try await until("the granted work scope is live") {
            self.setups.last == .ready && self.supervisions.last?.assistantAccess?.ready == true
        }
        let approved = try XCTUnwrap(supervisions.last?.assistantAccess)
        XCTAssertEqual(approved.state, .approved)
        XCTAssertEqual(Set(approved.apps), apps)
        XCTAssertEqual(approved.expires, Date(timeIntervalSince1970: 2_000_100_000))
        let finished = try await decisions()
        XCTAssertEqual(finished, #"["agents revoke","agents request-assistant-work-grant"]"#)
    }

    func testRequestingOnlyTheSelectedCatalogueAppsNeedsASeparateGrantAfterRevocation() async throws {
        try await enterHostedAssistant("""
            window.chatGrant = {state: 'approved', ready: true, apps: ['so.tilo.chat'], expiresAtMs: 2000000000000};
            window.grantChoices = [{const:'so.tilo.calendar', title:'Workspace calendar'}, {const:'so.tilo.chat', title:'Chat'},
                {const:'so.tilo.mail', title:'Workspace mail'}, {const:'so.tilo.notes', title:'Workspace notes'}];
            """)
        try await until("the named applications in the signed action's catalogue are available") {
            self.setups.last == .ready && self.supervisions.last?.assistantAppChoices.count == 4
        }
        let choices = try XCTUnwrap(supervisions.last?.assistantAppChoices)
        XCTAssertEqual(Set(choices.map { "\($0.id) \($0.name)" }),
                       ["so.tilo.calendar Workspace calendar", "so.tilo.chat Chat", "so.tilo.mail Workspace mail", "so.tilo.notes Workspace notes"])

        let selected = ["so.tilo.chat", "so.tilo.mail", "so.tilo.calendar"]
        func mutations() async throws -> String {
            try await page("return JSON.stringify(window.hostAsked.filter(a => a.startsWith('agents ') && a !== 'agents grant-status'))")
        }
        do {
            try await source.requestAssistantAppPermission(apps: selected)
        } catch is SourceProblem { }
        let oldGrant = try await mutations()
        XCTAssertEqual(oldGrant, "[]")
        try await source.revokeChatPermission()
        try await until("the old Chat grant is revoked") { self.setups.last == .chatPermission(.revoked) }
        do {
            try await source.requestAssistantAppPermission(apps: ["so.tilo.mail", "so.tilo.calendar"])
            XCTFail("The person must explicitly include Chat in the request")
        } catch SourceProblem.refused { }
        try await source.requestAssistantAppPermission(apps: selected)
        try await until("the selected applications have their own pending code") {
            self.setups.last == .chatPermission(.pending(code: "274 905")) && self.supervisions.last?.assistantAccess?.state == .pending
        }
        let sent = try await page("return JSON.stringify(window.selectedScopes)")
        let scopes = try JSONDecoder().decode([[String]].self, from: Data(sent.utf8))
        XCTAssertEqual(scopes, [selected])
        let pending = try XCTUnwrap(supervisions.last?.assistantAccess)
        XCTAssertEqual(pending.apps.sorted(), selected.sorted())
        XCTAssertFalse(pending.ready)
        try await run("window.chatGrant.state = 'denied'")
        await source.checkChatPermission()
        try await until("the refusal closes the pending request") { self.setups.last == .chatPermission(.declined) }
        await source.checkChatPermission()
        let finished = try await mutations()
        XCTAssertEqual(finished, #"["agents revoke","agents request-app-grant"]"#)
    }

    func testWithNoProviderLinkedTheMainChatOffersTheStepAndTheKeyGoesToThePersonsServerOnly() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.unlinked = true; window.agents = [kept('assistant', {name: 'Assistant'})]")
        let linkable = [ProviderChoice(id: "anthropic", name: "Anthropic"), ProviderChoice(id: "gemini", name: "Google Gemini"),
                        ProviderChoice(id: "openai", name: "OpenAI"), ProviderChoice(id: "openrouter", name: "OpenRouter")]
        try await until("the step is offered") { self.setups.last == .needsProvider(linkable) }
        do {
            _ = try await source.link("anthropic", key: Secret(""))
            XCTFail("nothing was taken for a key")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("Nothing was typed"))
        }
        // A made-up key, typed as into the protected field.
        let typed = "sk-made-up-and-typed-into-the-protected-field"
        let offered = try await source.link("anthropic", key: Secret(typed))
        let two = [ModelChoice(id: "anthropic/m-1", name: "Model one", provider: "Anthropic"),
                   ModelChoice(id: "anthropic/m-2", name: "Model two", provider: "Anthropic")]
        XCTAssertEqual(offered, two, "the models are the ones her server listed")
        try await until("the models are offered in the main chat") { self.setups.last == .needsModel(two) }
        // Her server got the key, in the one ask that keeps it.
        let keys = try await page("return JSON.stringify(window.keys)")
        XCTAssertEqual(keys, "[\"\(typed)\"]")
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('tilo.models ') && !what.endsWith(' list')))")
        XCTAssertEqual(asked, #"["tilo.models add-preset","tilo.models set-key","tilo.models refresh-models"]"#)
        // It stands nowhere else: in nothing Chat was asked, in no message, nowhere in the
        // page's document, and in nothing the app says or keeps.
        let leaked = try await page("return String(JSON.stringify([window.messages, window.heard, window.asks]).includes(typed)) + ' ' + String(document.documentElement.outerHTML.includes(typed))",
                                    ["typed": typed])
        XCTAssertEqual(leaked, "false false")
        XCTAssertFalse(String(describing: setups).contains(typed))
        XCTAssertFalse(String(describing: changes).contains(typed))
        XCTAssertFalse(String(describing: defaults.dictionaryRepresentation()).contains(typed))
        // The model that answers and works is one of those her server listed: one ask for both.
        try await source.choose("anthropic/m-2", for: .request, .task)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let chosen = try await page("return String(window.asks.filter(what => what === 'tilo.models set-assignments').length) + ' ' + JSON.stringify(window.assignments.map(row => row.kind))")
        XCTAssertEqual(chosen, #"1 ["request","task"]"#)
    }

    /// Nothing is said to stand in the way no more before a model stands for the kind of work a
    /// turn in the main chat is worked as: a message sent in between found no model linked.
    func testNothingIsSaidToBeReadyBeforeAModelStandsForTheKindATurnIsWorkedAs() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.unlinked = true; window.agents = [kept('assistant', {name: 'Assistant'})]")
        try await until("the step is offered") { if case .needsProvider = self.setups.last { true } else { false } }
        // A made-up key, typed as into the protected field.
        _ = try await source.link("anthropic", key: Secret("sk-made-up-and-typed-into-the-protected-field"))
        try await until("the models are offered in the main chat") { if case .needsModel = self.setups.last { true } else { false } }
        // As a choice that reached her server for the answering alone.
        try await source.choose("anthropic/m-2", for: .request)
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertNotEqual(setups.last, .ready, "the app said that nothing stands in the way while no model stood for the work of a turn")
        try await source.choose("anthropic/m-2", for: .task)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
    }

    func testAPersonComesBackToAProviderTheyLinkedAndItsNewKeyTakesTheOldOnesPlaceOnTheirServer() async throws {
        try await start(.assistant)
        // A person whose server keeps one provider with its key, and whose main chat answers on a model of it.
        try await enter(device: true, """
            window.byKind = true; window.unlinked = true; window.agents = [kept('assistant', {name: 'Assistant'})];
            window.holdVaultRead = true;
            window.added = [{id: 'p-7', name: 'OpenRouter', protocol: 'openrouter', endpoint: 'https://openrouter.example',
              configured: true, models: [{id: 'm-1', label: 'Model one'}], defaultModel: 'm-1'}];
            window.assignments = [{kind: 'request', provider: 'p-7', model: 'm-1'}, {kind: 'task', provider: 'p-7', model: 'm-1'}]
            """)
        try await until("nothing stands in the way of an answer") { self.setups.last == .ready }
        // The background survey has read the old list and is still waiting for the vault.
        try await until("the survey is waiting on the vault") {
            (try? await self.page("return typeof window.resumeVaultRead")) == "function"
        }
        // The step they come back to offers the provider they linked, said to be linked, and the
        // rest their server links by name. The one they linked is not offered a second time.
        let offered = try await source.providers()
        XCTAssertEqual(offered, [ProviderChoice(id: "p-7", name: "OpenRouter", isLinked: true), ProviderChoice(id: "anthropic", name: "Anthropic"),
                                 ProviderChoice(id: "gemini", name: "Google Gemini"), ProviderChoice(id: "openai", name: "OpenAI")])
        let said = setups.count
        // Another made-up key, typed as into the protected field, for the provider that is linked.
        let typed = "sk-made-up-second-key-typed-into-the-protected-field"
        let models = try await source.link("p-7", key: Secret(typed))
        try await run("window.resumeVaultRead()")
        XCTAssertEqual(models, [ModelChoice(id: "p-7/m-1", name: "Model one", provider: "OpenRouter"),
                                ModelChoice(id: "p-7/m-2", name: "Model two", provider: "OpenRouter")])
        // The provider keeps its place on their server: it is not added again, only its key is set.
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what.startsWith('tilo.models ') && !what.endsWith(' list')))")
        XCTAssertEqual(asked, #"["tilo.models set-key","tilo.models refresh-models"]"#)
        let keys = try await page("return JSON.stringify(window.keys)")
        XCTAssertEqual(keys, "[\"\(typed)\"]")
        // The model they chose stands, and nothing came to stand in the way of an answer.
        let assigned = try await page("return JSON.stringify(window.assignments)")
        XCTAssertEqual(assigned, #"[{"kind":"request","provider":"p-7","model":"m-1"},{"kind":"task","provider":"p-7","model":"m-1"}]"#)
        XCTAssertEqual(setups.dropFirst(said).filter { $0 != .ready }, [])
        XCTAssertEqual(setups.last, .ready)
        // The key stands nowhere else.
        let leaked = try await page("return String(JSON.stringify([window.messages, window.heard, window.asks]).includes(typed)) + ' ' + String(document.documentElement.outerHTML.includes(typed))",
                                    ["typed": typed])
        XCTAssertEqual(leaked, "false false")
        XCTAssertFalse(String(describing: setups).contains(typed))
        XCTAssertFalse(String(describing: changes).contains(typed))
        XCTAssertFalse(String(describing: defaults.dictionaryRepresentation()).contains(typed))
    }

    func testWhatThePageAsksBeforePairingIsWhereTheAppStandsAndOnlyThePersonsAnswerPressesIt() async throws {
        try await start()
        let whose = "You are adding this device to Ada Lovelace’s devices. If that is not you, stop."
        let server = "The server that adds it is this site’s own, tilo.test."
        _ = try await page("askWhose(whose, server); return ''", ["whose": whose, "server": server])
        let ask = PairingAsk(heading: "Add this device?", said: [whose, server], go: "Add this device", stop: "Stop", site: Self.site)
        try await until("the app stands at the question") { self.sessions.last == .asked(ask) }
        // Nothing answers for the person.
        try await Task.sleep(for: .milliseconds(300))
        let untouched = try await page("return String(window.agreed)")
        XCTAssertEqual(untouched, "undefined")
        await source.answerPairing(goOn: true)
        let agreed = try await page("return String(window.agreed)")
        XCTAssertEqual(agreed, "true")
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

    // MARK: Told while the app is closed

    /// An address as Apple's service would give one. It is made up, as is everything here.
    private static let madeUpAddress = PushAddress(use: .notices, token: Data(repeating: 0xab, count: 32), service: .development,
                                                   app: "so.tilo.assistant")

    /// What her server was handed, said so that no token and no key stands in a test's words.
    private func handedOver() async throws -> String {
        try await page("""
            return JSON.stringify(window.registered.map(one => [one.token === 'ab'.repeat(32), one.environment, one.topic,
              one.key.p256dh.length, one.key.p256dh.slice(0, 2), one.key.auth.length]))
            """)
    }

    func testABuildWithoutThePushRightHandsHerServerNoAddressAndAsksItNothing() async throws {
        try await start(.agent(id: "assistant", name: "Assistant"))
        try await enter(device: true)
        await source.register(Self.madeUpAddress)
        await source.unregister()
        let asked = try await page("return JSON.stringify([window.registered.length, window.unregistered])")
        XCTAssertEqual(asked, "[0,0]")
    }

    func testADeviceHandsHerServerItsAddressWithAKeyOfItsOwnAndAPieceCarriesTheNameAPushGivesIt() async throws {
        let keys = MemoryClosedKeys()
        try await start(.agent(id: "assistant", name: "Assistant"), closedKeys: keys)
        try await enter(device: true, "window.byKind = true; window.holds = 60000")
        XCTAssertNil(keys.read(), "a key was made before any address came")
        await source.register(Self.madeUpAddress)
        let handed = try await handedOver()
        XCTAssertEqual(handed, #"[[true,"development","so.tilo.assistant",130,"04",32]]"#)
        // The key her server seals for is the one this device keeps, and no other.
        let kept = try XCTUnwrap(keys.read())
        let halves = try await page("return JSON.stringify([window.registered[0].key.p256dh, window.registered[0].key.auth])")
        XCTAssertTrue(halves == #"["\#(kept.handedOver.p256dh)","\#(kept.handedOver.auth)"]"#, "her server was handed another key than the one kept")
        // Only an address for notifications is handed over.
        await source.register(PushAddress(use: .activityStart, token: Data(repeating: 0xcd, count: 32)))
        // At the next launch the system gives the address again: handed over again, with the same key.
        await source.register(Self.madeUpAddress)
        let again = try await page("return JSON.stringify([window.registered.length, window.registered[1].key.p256dh === window.registered[0].key.p256dh])")
        XCTAssertEqual(again, "[2,true]")
        XCTAssertEqual(keys.kept, 1)

        // A thing her server tells her of: its piece carries the name a push gives it on this device.
        _ = try await page("""
            direct({id: 'm_ask_1', author: {agent: 'e-9'}, text: 'Remind me to stretch. (for the assistant)',
              cause: {kind: 'reminder', label: 'Stretch', key: 'k-1', person: {person: ADA}}});
            direct({id: 'm_reply_1', author: {agent: 'assistant'}, text: 'Time to stretch.', parent: 'm_ask_1'});
            return 'said'
            """)
        try await until("the answer stands under the reminder's line") { self.held.last?.task?.title == "Stretch" }
        XCTAssertNil(held.last?.toldAs)
        _ = try await page(Self.told(1, "reminder", about: "m_ask_1", title: "Stretch") + "; return 'told'")
        let name = kept.name(of: "turn:ws-1:m_ask_1:0")
        try await until("the piece carries the name a push gives it") { self.held.last?.toldAs == name }

        await source.unregister()
        let forgotten = try await page("return String(window.unregistered)")
        XCTAssertEqual(forgotten, "1")
    }

    func testAnAddressThatComesBeforeTheDeviceIsInWaitsUntilItIs() async throws {
        let keys = MemoryClosedKeys()
        try await start(.agent(id: "assistant", name: "Assistant"), closedKeys: keys)
        await source.register(Self.madeUpAddress)
        XCTAssertNil(keys.read())
        try await enter(device: true)
        try await until("her server has the address") { (try? await self.handedOver()) == #"[[true,"development","so.tilo.assistant",130,"04",32]]"# }
        XCTAssertEqual(keys.kept, 1)
    }

    func testTheKeyGoesAtSignOutAndAKeyLeftBehindIsNotTheNextPersons() async throws {
        let keys = MemoryClosedKeys()
        try await start(.agent(id: "assistant", name: "Assistant"), closedKeys: keys)
        try await enter(device: true)
        await source.register(Self.madeUpAddress)
        XCTAssertNotNil(keys.read())
        await source.signOut()
        XCTAssertNil(keys.read(), "the key outlived its person's sign-out")
        // A key some earlier build left in the store is gone the moment a sign-in begins, whatever
        // comes of the sign-in: this link leads into another site and pairs nothing.
        try keys.keep(ClosedKey.made())
        let link = try XCTUnwrap(PairingLink(in: "https://elsewhere.example/#/join/made-up"))
        do {
            try await source.pair(with: link)
            XCTFail("a link into another site paired")
        } catch {}
        XCTAssertNil(keys.read(), "a key left behind met the next sign-in")
    }

    func testALinkThatLeadsIntoAnotherSiteIsRefusedWithTheNameOfTheSiteThisAppOpens() async throws {
        try await start()
        let secret = String(repeating: "ab", count: 48)
        // Another site, this computer on another port than the site's, and another site's link
        // that carries an address of this site inside it: none of them leads into this site.
        for (text, from) in [("https://elsewhere.example/#/w/ws-1?pair=\(secret)", "elsewhere.example"),
                             ("http://tilo.localhost:24520/#/w/ws-1?pair=\(secret)", "tilo.localhost:24520"),
                             ("HTTPS://elsewhere.example/?next=\(Self.site)/#/w/ws-1?pair=\(secret)", "elsewhere.example")] {
            let link = try XCTUnwrap(PairingLink(in: text), text)
            do {
                try await source.pair(with: link)
                XCTFail("paired with \(text)")
            } catch {
                // Each is a pairing link of another Tilo site: the refusal names that site by its
                // name and port, the site this app opens, and where a code for it is made.
                XCTAssertEqual(error as? SourceProblem, .refused("This code is from another Tilo site, \(from). This app is set up for "
                    + "\(Self.site). Open “Pair a new device” there."), text)
            }
        }
    }

    func testAJobUsesTheSameDirectChatAsAPersonWriting() async throws {
        try await start(.assistant)
        try await enter(device: true, "window.byKind = true; window.agents = [kept('reed', {name: 'Reed'})]")
        try await until("the assistant is ready") { self.setups.last == .ready }
        try await source.startSubAgent(job: "Review the budget")
        let calls = try await page("return JSON.stringify(window.heard.filter(a => ['send', 'ask-agent'].includes(a.action)).map(a => [a.action, a.args.text, a.args.to]))")
        XCTAssertEqual(JSON(text: calls), [["send", "Start a sub-agent for this job: Review the budget", [["kind": "agent", "id": "reed", "name": "Reed"]]]])
    }

    func testASubAgentHeaderShowsItsRoleWithoutAssigningItsTurnsToAHost() async throws {
        try await start(.assistant)
        var listed: [SubAgent] = []
        let listening = Task { for await agents in self.source.subAgents() { listed = agents } }
        defer { listening.cancel() }
        try await enter(device: true, """
            window.byKind = true;
            window.agents = [kept('reed', {name: 'Reed'}),
              kept('reader', {name: 'Open requests', created_at: 1794900000000,
                sub: {role: 'Researcher', stands: {$variant: 'working'}}})]
            """)
        try await until("her sub-agent is listed") { listed.count == 1 }
        XCTAssertEqual(listed.map(\.title), ["Open requests"])
        let headers = listed.map { Wording().header($0) }
        XCTAssertTrue(headers[0].hasPrefix("Researcher · started "), headers[0])
    }

    /// Stopping a sub-agent is her server's own `stop-sub-agent` on the workspace's agents,
    /// asked as the person on this device: the page holds Chat, which keeps no roster.
    func testStoppingASubAgentIsAskedOfHerServerUnderTheWorkspacesAgents() async throws {
        try await start(.assistant)
        var listed: [SubAgent] = []
        let listening = Task { for await agents in self.source.subAgents() { listed = agents } }
        defer { listening.cancel() }
        try await enter(device: true, """
            window.byKind = true;
            window.agents = [kept('reed', {name: 'Reed'}),
              kept('coder', {name: 'Usage drop', sub: {role: 'Coder', stands: {$variant: 'working'}}})]
            """)
        try await until("the sub-agent is listed") { listed.count == 1 }
        try await source.stop(listed[0].id)
        let asked = try await page("return JSON.stringify(window.asks.filter(what => what === 'agents stop-sub-agent'))")
        XCTAssertEqual(JSON(text: asked)?.array?.count, 1)
        let stopped = try await page("return JSON.stringify(window.stopped)")
        XCTAssertEqual(stopped, #"{"agent":"coder"}"#)
    }

    /// The page shows Chat alone and holds no roster of its own: the workspace's agents are
    /// Assistant's, and her server is where the sub-agents and the day are read from. The
    /// page's mount is asked nothing of them.
    func testTheSubAgentsAndTheDayAreHerServersRosterThoughThePageHoldsChatAlone() async throws {
        try await start(.assistant)
        var listed: [SubAgent] = []
        var seen = Supervision()
        let agents = Task { for await now in self.source.subAgents() { listed = now } }
        let supervision = Task { for await now in self.source.supervision() { seen = now } }
        defer { agents.cancel(); supervision.cancel() }
        try await enter(device: true, """
            window.byKind = true;
            window.agents = [kept('assistant', {name: 'Assistant', spend_limit_micros: 5000000,
                spent: {day: '\(AssistantDay().today)', cost_micros: 1250000}}),
              kept('coder', {name: 'Usage drop', sub: {role: 'Coder', stands: {$variant: 'working'}}})]
            """)
        try await until("the sub-agent is listed") { listed.count == 1 }
        try await until("the day is counted") { seen.usage.spentTodayMicros == 1_250_000 }
        XCTAssertEqual(listed.map(\.title), ["Usage drop"])
        XCTAssertEqual(seen.usage.dailyLimitMicros, 5_000_000)
        // Nothing of the roster is read of the page: it holds Chat, which keeps none.
        let ofPage = try await page("return String(window.heard.filter(call => String(call.resource).includes('tilo.workspace.agent')).length)")
        XCTAssertEqual(ofPage, "0")
    }
}
#endif
