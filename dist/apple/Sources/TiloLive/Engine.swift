import Foundation
import ShellCore
import WebKit

/// Tilo's own page as the client of a native app.
///
/// A person's device in Tilo is a key of its own that their identity certified, a connection
/// to the hosts that hold their workspaces, and the records it keeps in step with them. Tilo's
/// page is all of that already, for every browser. The engine holds that page in a system web
/// view that shows nothing, and gives the app three things of it: what a resource reads as,
/// word that something changed, and the typed actions with their answers.
///
/// - The device's key never leaves the page: it is a browser key that cannot be taken out,
///   which WebKit wraps with a key from the app's keychain. The engine hands the app no key.
/// - It reads and acts as the person's own device, through the entry the page's palette,
///   agents and assistive technology use, under the person's own state. It is no client of
///   the host's API and carries no API key, so nothing it does is announced as an agent's call.
/// - It opens its one site and nothing else. Another origin is never loaded, and only the
///   site's own page, in the main frame, is heard.
///
/// On an iPhone a web view runs its page only while it stands in a window, so the app puts
/// `view` into its own, unseen (`EngineRoom`). A Mac needs no window for it.
@MainActor
public final class TiloEngine: NSObject {
    /// Where the page keeps what it keeps: the device's key, its certificate, the records.
    public enum Store: Sendable, Equatable {
        /// A store of the app's own under this name, kept across launches: the device stays
        /// the person's for as long as the app keeps its signature and this store.
        case kept(UUID)
        /// Nothing is kept, for a check that leaves nothing behind. A device paired into it is
        /// gone with the program.
        case keepsNothing
    }

    /// Remove a device's named store from a fresh process, before any page has been opened.
    public static func forget(store id: UUID) async throws {
        // Creating a data store initializes WebKit's run loop. Its static removal API
        // dispatches to that loop even when this is the process's first WebKit operation.
        _ = WKWebsiteDataStore.nonPersistent()
        try await WKWebsiteDataStore.remove(forIdentifier: id)
    }

    /// Where this device stands with the person's Tilo.
    public enum State: Equatable, Sendable {
        /// Nothing is opened.
        case away
        /// On its way in. The word is the page's own step: `load`, `join`, `open`, `draw`.
        case arriving(String)
        /// The page asks before it pairs: whose device this becomes and which server adds it,
        /// in the page's own sentences. It goes on only when the person says so (`answerPairing`).
        case asking(PairingAsk)
        /// A device where the person is signed in is being asked. It shows this same code.
        case pairing(code: String)
        /// In. The route names the workspace and the app whose records the page holds.
        case open(route: String)
        /// The site said no. The sentence is its own.
        case refused(String)
        /// The site says that the workspace this device was let into is no longer there: its
        /// server was set up again since, or the workspace was deleted. The sentence is the
        /// page's own. No later load changes that, so nothing is loaded again: the device
        /// comes back by a pairing, as a new device does.
        case gone(String)
        /// The page could not go on.
        case failed(String)
    }

    /// What Tilo's page asks before a device is paired with a link: its own sentences, and its
    /// own words for the two ways on. A link is written by whoever made it, so the page says
    /// whose devices this one would join and which server adds it before it asks anyone.
    public struct PairingAsk: Equatable, Sendable {
        public let heading: String
        /// Whose device this becomes, which server adds it, and what follows from that.
        public let said: [String]
        /// What the page calls going on, and what it calls stopping.
        public let go: String
        public let stop: String
    }

    public enum Problem: Error, Equatable, Sendable {
        /// The page is not in: nothing can be read or asked yet.
        case notOpen
        /// Tilo said no: its code and its sentence.
        case refused(code: String, reason: String)
        /// The address is none of this engine's site, or names nothing the engine opens.
        case notOfThisSite
        /// The page gave no answer that can be read.
        case noAnswer
    }

    public let settings: ShellSettings
    public let policy: SitePolicy
    /// The web view that holds the page. It shows nothing a person is meant to look at.
    public let view: WKWebView
    /// The store the page keeps its data in. A view that shows a live Tilo thing uses the same.
    public var store: WKWebsiteDataStore { view.configuration.websiteDataStore }

    public private(set) var state: State = .away {
        didSet {
            guard state != oldValue else { return }
            if case .open = state {} else {
                found.removeAll()
                endAsks()
            }
            for listener in stateListeners.values { listener.yield(state) }
        }
    }

    /// This engine's own pass: thirty-two random bytes its script takes an ask of the person's
    /// providers with. It stands in the script's own scope and here, and nowhere a page's
    /// script, or the app, can read it.
    let pass: String
    /// The current site hands this device the server's action catalogue.
    private(set) var hosted = false
    /// What was asked of the page and is not answered yet. A page that is loaded again under
    /// an ask never answers it, so these end when the page they were asked of is put away.
    private var asked: [UUID: CheckedContinuation<String?, Never>] = [:]
    /// How long an ask may go unanswered before whoever asked is told that no answer came, and
    /// can ask again. A person's server holds one kind of ask open on purpose, for ten seconds
    /// at most; every other answers in a moment, or in seconds where the server is busy.
    ///
    /// Tilo's page itself ends an ask of a person's server after thirty seconds, looked at
    /// every half second, and says that no answer came. This bound is a little longer than
    /// that, so the page's own word comes first, and the engine ends only what a page never
    /// answers at all.
    var askPatience: Duration = .seconds(35)
    /// How long the ask for a person's own workspace may take. Their server makes that
    /// workspace the first time it is asked for, and the page follows the making for up to
    /// 160 seconds (10 for the answer, 120 for a making that still goes on, 30 to get in)
    /// before it says that none came. The engine waits a little longer than the page does.
    var ownPatience: Duration = .seconds(180)
    /// How long mounting a live thing may take. The page fetches the thing's app the first
    /// time it shows one, which on a slow connection takes longer than any ask of a server.
    var mountPatience: Duration = .seconds(120)
    /// How many frames inside the page were refused their content, for a check.
    private(set) var refusedFrames = 0

    private let ear = Ear()
    private var stateListeners: [UUID: AsyncStream<State>.Continuation] = [:]
    private var changeListeners: [UUID: AsyncStream<Void>.Continuation] = [:]
    private var sizeListeners: [UUID: AsyncStream<(String, CGSize)>.Continuation] = [:]
    /// Whoever waits for the page's next word of a change, once.
    private var changeWaiters: [UUID: CheckedContinuation<Bool, Never>] = [:]
    /// How many loads in a row stopped since the page was last in. The first is loaded again
    /// without a word; every later one is said, and loaded again after a pause that grows.
    private var stops = 0
    /// How long a page that stopped waits before it is loaded again, the first time. The wait
    /// doubles with every load that stops as well, up to a minute.
    var stopPause: Duration = .seconds(1.5)
    /// Why the page last stopped on its way in and was loaded again: the page's own sentence,
    /// or the system's. Nil while it never did.
    public private(set) var lastStop: String?
    /// The technical details the page showed last under a failure's sentence (its
    /// `#arrival-failure-text`), and the sentence they stood under.
    private var detailsSaid: (under: String, text: String)?
    /// What the page offers to read or copy under the sentence the last try ended with: what it
    /// makes of the stop, the server, the step, the error and its build. Its sentence may say
    /// "copy the technical details below": these are what it points to. Empty where the try
    /// ended with another sentence than the one the page showed them under, and while none ended.
    public var stopDetails: String {
        guard let detailsSaid else { return "" }
        switch state {
        case .refused(let why), .failed(let why), .gone(let why): return detailsSaid.under == why ? detailsSaid.text : ""
        default: return ""
        }
    }
    /// How long a page that was loaded again after it stopped may say nothing before its stop
    /// is said. A page takes seconds to load, and a minute on a machine that is busy.
    var stopPatience: Duration = .seconds(60)
    private var retrying: Task<Void, Never>?
    /// Whether the try that ended last ended on the site's own page: the page was served and
    /// said that its join stopped, in a case that tells of a site that answered
    /// (`answered(failure:)`). A load the network failed says nothing of the kind, nor does a
    /// page that never said where it stands.
    private var stoppedOnItsPage = false
    private var endListeners: [UUID: AsyncStream<Void>.Continuation] = [:]
    /// Which look a page draws in: the theme its person has in Tilo is a light one or a dark one.
    public enum Look: Equatable, Sendable {
        case light
        case dark
    }

    /// The look the page said last that it draws in (`data-look` on its root element): the
    /// theme its person has in Tilo, which a frame around the page follows. Nil until a page
    /// has said one, and again once the page is left.
    public private(set) var look: Look? {
        didSet {
            guard look != oldValue else { return }
            for listener in lookListeners.values { listener.yield(look) }
        }
    }
    private var lookListeners: [UUID: AsyncStream<Look?>.Continuation] = [:]
    /// The address asked for last, which a retry loads again.
    private var wanted: URL?
    /// The workspace asked for last. A page that got in elsewhere is opened there.
    private var wantedWorkspace: String?
    /// How often a page that stands elsewhere than asked is still moved before it is said.
    private var moves = 0
    /// The page in the view was loaded to pair, and has not been loaded again since.
    private var pairs = false
    /// The workspace a pairing was let into: the page that pairs got past its join, which it
    /// does once a device where its person is signed in approved this one. From then on the
    /// store holds a device of that person's, whatever stops the page afterwards. Nil until
    /// then, and again once the engine is asked for another page or its page is put away.
    public private(set) var letInto: String?
    /// The page that pairs was heard on its own way in since it was asked for: at its load or
    /// its join, or asking. Only such a page says that the pairing was let in: a page of the
    /// try before, which may stand in the view for a moment longer, says nothing of this one.
    private var pairingHeard = false
    /// The leave whose last ask of the page is still out: the page stays until it answered.
    private var lingering: Int?
    private var leaves = 0
    /// Loads an address in place of the web view, for a test that fetches nothing.
    var loads: ((URL) -> Void)?
    /// What was looked up in the page's listing since it got in.
    var found: [String: String] = [:]

    /// - Parameters:
    ///   - unseen: the view is in no window anyone looks at, so its page's frames are driven by
    ///     a clock (`EngineScript`). `false` only for a view a person looks at.
    ///   - inspectable: lets Safari's Web Inspector attach, for development.
    ///   - deviceName: what this device is called where its person lets it in and lists their
    ///     devices, as the app says it ("Tilo Assistant on Ada's Mac"). Empty leaves it to the page.
    public init(settings: ShellSettings, store: Store, unseen: Bool = true,
                frame: CGRect = CGRect(x: 0, y: 0, width: 1100, height: 760), inspectable: Bool = false,
                deviceName: String = "") {
        self.settings = settings
        policy = SitePolicy(settings: settings)
        let configuration = WKWebViewConfiguration()
        switch store {
        case .kept(let name): configuration.websiteDataStore = WKWebsiteDataStore(forIdentifier: name)
        case .keepsNothing: configuration.websiteDataStore = .nonPersistent()
        }
        configuration.applicationNameForUserAgent = "TiloLive"
        // The page keeps its connection and its records in step while nobody looks at it.
        configuration.preferences.inactiveSchedulingPolicy = .none
        view = WKWebView(frame: frame, configuration: configuration)
        var random = SystemRandomNumberGenerator()
        pass = (0..<32).map { _ in String(format: "%02x", UInt8.random(in: .min ... .max, using: &random)) }.joined()
        super.init()
        ear.engine = self
        view.configuration.userContentController.add(ear, name: EngineScript.name)
        view.configuration.userContentController.addUserScript(WKUserScript(
            source: EngineScript.source(origin: settings.origin, unseen: unseen, pass: pass,
                                        providers: PersonProvider.allCases.map(\.rawValue), device: deviceName),
            injectionTime: .atDocumentStart, forMainFrameOnly: true))
        view.navigationDelegate = self
        view.uiDelegate = self
        view.allowsBackForwardNavigationGestures = false
        view.isInspectable = inspectable
    }

    // MARK: The way in

    /// Pair this device with a link that "Pair a new device" made where the person is signed
    /// in. `states()` says how it goes on: the code to compare, then in, or the refusal.
    public func pair(with link: URL) throws(Problem) {
        guard let address = EngineAddress.pairing(link, of: settings) else { throw .notOfThisSite }
        go(to: address, in: EngineAddress.workspace(ofPage: address), pairing: true)
    }

    /// Open the workspace `route` names (`/@<host>/w/<workspace>`, `/w/<workspace>` or `/me`)
    /// as the device this store already is.
    public func open(route: String) throws(Problem) {
        guard let address = EngineAddress.page(of: EngineAddress.chat, in: route, of: settings) else { throw .notOfThisSite }
        go(to: address, in: EngineAddress.workspace(of: route))
    }

    /// The person's answer to what the page asks before it pairs (`State.asking`): go on, or
    /// stop. It presses the page's own button and nothing else, and says whether the page was
    /// asking. Only the person's own press leads here: the engine never answers by itself.
    @discardableResult
    public func answerPairing(goOn: Bool) async -> Bool {
        guard case .asking = state else { return false }
        let pressed = try? await view.callAsyncJavaScript("return globalThis.__tiloLive?.pairing(given, goOn) === true",
                                                          arguments: ["given": pass, "goOn": goOn], in: nil, contentWorld: .page)
        return pressed as? Bool ?? false
    }

    /// Put the page away. The engine stands nowhere until it is asked to pair or to open
    /// again; the device's key stays in its store.
    ///
    /// With `last`, the page is asked one more thing on its way out, and nobody waits for the
    /// answer: the engine stands away at once, and the page itself is put away when it has
    /// answered, or when the engine's patience with an ask is over. It is heard no more
    /// meanwhile, and a page that is asked for meanwhile takes its place.
    public func leave(asking last: (script: String, arguments: [String: Any])? = nil) {
        retrying?.cancel()
        retrying = nil
        wanted = nil
        wantedWorkspace = nil
        pairs = false
        letInto = nil
        lingering = nil
        var isIn = false
        if case .open = state { isIn = true }
        state = .away
        // Whoever comes to this device next has a look of their own.
        look = nil
        guard let last, isIn else {
            view.loadHTMLString("", baseURL: nil)
            return
        }
        leaves += 1
        let mine = leaves
        lingering = mine
        let patience = askPatience
        let giveUp = Task { @MainActor [weak self] in
            try? await Task.sleep(for: patience)
            guard !Task.isCancelled else { return }
            self?.putAway(mine)
        }
        view.callAsyncJavaScript(last.script, arguments: last.arguments, in: nil, in: .page) { [weak self] _ in
            Task { @MainActor in
                giveUp.cancel()
                self?.putAway(mine)
            }
        }
    }

    /// The page whose last ask was out is put away, unless the engine was asked for a page since.
    private func putAway(_ leave: Int) {
        guard lingering == leave else { return }
        lingering = nil
        view.loadHTMLString("", baseURL: nil)
    }

    /// Where the device stands, from now on. The first value is where it stands now.
    public func states() -> AsyncStream<State> {
        let (stream, continuation) = AsyncStream.makeStream(of: State.self, bufferingPolicy: .bufferingNewest(32))
        let id = UUID()
        stateListeners[id] = continuation
        continuation.yield(state)
        continuation.onTermination = { [weak self] _ in Task { @MainActor in self?.stateListeners[id] = nil } }
        return stream
    }

    /// The look the page draws in, from now on. The first value is what is known now.
    public func looks() -> AsyncStream<Look?> {
        let (stream, continuation) = AsyncStream.makeStream(of: Look?.self, bufferingPolicy: .bufferingNewest(32))
        let id = UUID()
        lookListeners[id] = continuation
        continuation.yield(look)
        continuation.onTermination = { [weak self] _ in Task { @MainActor in self?.lookListeners[id] = nil } }
        return stream
    }

    /// Whether this device's site answers and does not let its page in: the try that ended
    /// last ended on the site's own page, which said that its join stopped. False while a try
    /// is in work, while the page is in, and where the try ended otherwise.
    public var joinStops: Bool {
        if case .failed = state { stoppedOnItsPage } else { false }
    }

    /// Word of every try that ended without getting in, from now on. The state says the same
    /// only where its sentence changed: a try that ends in the sentence of the one before
    /// changes no state, and may still change `joinStops`.
    public func triesEnded() -> AsyncStream<Void> {
        let (stream, continuation) = AsyncStream.makeStream(of: Void.self, bufferingPolicy: .bufferingNewest(1))
        let id = UUID()
        endListeners[id] = continuation
        continuation.onTermination = { [weak self] _ in Task { @MainActor in self?.endListeners[id] = nil } }
        return stream
    }

    /// Word that what the page holds changed, from now on. It says nothing of what changed:
    /// the listener reads what it shows. Changes that come while it reads are one word.
    public func changes() -> AsyncStream<Void> {
        let (stream, continuation) = AsyncStream.makeStream(of: Void.self, bufferingPolicy: .bufferingNewest(1))
        let id = UUID()
        changeListeners[id] = continuation
        continuation.onTermination = { [weak self] _ in Task { @MainActor in self?.changeListeners[id] = nil } }
        return stream
    }

    /// The size a live thing's content asks for, by the thing's id, as its mount in the page
    /// says it: once it is known and whenever it changes, from now on.
    public func contentSizes() -> AsyncStream<(String, CGSize)> {
        let (stream, continuation) = AsyncStream.makeStream(of: (String, CGSize).self, bufferingPolicy: .bufferingNewest(32))
        let id = UUID()
        sizeListeners[id] = continuation
        continuation.onTermination = { [weak self] _ in Task { @MainActor in self?.sizeListeners[id] = nil } }
        return stream
    }

    /// The app expects what the page holds to change soon: an agent is at work on what it
    /// shows. For `time`, a minute at most, every word of the page is handed on, also where
    /// nothing says that its device heard anything.
    public func expectChanges(for time: Duration) {
        let ms = Double(time.components.seconds) * 1000 + Double(time.components.attoseconds) / 1e15
        view.callAsyncJavaScript("globalThis.__tiloLive?.expect(ms)", arguments: ["ms": ms], in: nil, in: .page, completionHandler: nil)
    }

    /// The page's next word of a change: `false` when none came within `patience`.
    public func nextChange(within patience: Duration) async -> Bool {
        let id = UUID()
        let timer = Task { [weak self] in
            try? await Task.sleep(for: patience)
            guard !Task.isCancelled else { return }
            self?.changeWaiters.removeValue(forKey: id)?.resume(returning: false)
        }
        let heard = await withCheckedContinuation { waiting in changeWaiters[id] = waiting }
        timer.cancel()
        return heard
    }

    /// Run `call`, and where Tilo answers that no such action is published, run it again at the
    /// page's next word of a change (two seconds later at the latest), until `patience` is over.
    ///
    /// A device that just got in holds its records before its app has published everything it
    /// offers: Chat lists its messages a few seconds before it offers its send. An action that
    /// is still not published when the patience is over is refused in Tilo's own words. Any
    /// other refusal is said at once.
    public func whenPublished<Value: Sendable>(within patience: Duration = .seconds(20),
                                               _ call: @MainActor () async throws -> Value) async throws -> Value {
        let end = ContinuousClock.now.advanced(by: patience)
        while true {
            do {
                return try await call()
            } catch let problem as Problem {
                guard case .refused(code: "unknown_action", reason: _) = problem else { throw problem }
                let left = ContinuousClock.now.duration(to: end)
                guard left > .zero else { throw problem }
                _ = await nextChange(within: min(left, .seconds(2)))
            }
        }
    }

    // MARK: Resources and actions

    /// The actions the page publishes now, with the person's permissions applied: each with
    /// its address (`resource`, `action`), its kind and its typed parameters. `resource`
    /// narrows to one resource and what stands under it.
    /// `live` names a live thing that is shown (`show`): its own actions are listed, not Chat's.
    public func actions(under resource: String? = nil, in live: String? = nil, ofKind kind: String? = nil) async throws -> [JSON] {
        let answer = try await ask("return globalThis.__tiloLive.actions(resource, live, given, kind)",
                                   ["given": pass, "kind": kind.map { $0 as Any } ?? NSNull(), "resource": resource.map { $0 as Any } ?? NSNull(),
                                    "live": live.map { $0 as Any } ?? NSNull()])
        let actions = answer.array ?? []
        guard let kind else { return actions }
        return actions.filter { $0["resource_kind"]?.string == kind }
    }

    /// Invoke one published action with typed arguments. Returns what it answered
    /// (`state`, and `output` with its `value`). Throws `Problem.refused` with Tilo's own code
    /// and sentence when the address is not published now, the person's permissions refuse it,
    /// the arguments do not bind, or it wants a confirmation that `confirm` does not give.
    @discardableResult
    public func invoke(_ action: String, on resource: String, _ arguments: [String: JSON] = [:],
                       confirm: Bool = false, in live: String? = nil, invocation: UUID? = nil) async throws -> JSON {
        try await ask("return globalThis.__tiloLive.invoke(resource, action, args, confirm, live, given, invocation)",
                      ["given": pass, "invocation": invocation.map { $0.uuidString.lowercased() as Any } ?? NSNull(),
                       "resource": resource, "action": action, "args": JSON.object(arguments).any, "confirm": confirm,
                       "live": live.map { $0 as Any } ?? NSNull()])
    }

    /// What a resource reads as now, from the records the page holds: its `read` action's value.
    public func read(_ resource: String, _ arguments: [String: JSON] = [:], in live: String? = nil) async throws -> JSON {
        try await invoke("read", on: resource, arguments, in: live)["output"]?["value"] ?? .null
    }

    // MARK: Live things

    /// What a live thing is mounted on.
    public enum Mounted: Equatable, Sendable {
        /// This app on this record of the workspace the engine is in.
        case record(app: String, record: String)
        /// This app on a record it makes now.
        case new(app: String)
        /// The view a message carries, shown alone by Chat in the page: named by its message and
        /// the conversation it stands in, never by its text.
        case messageView(message: String, conversation: String)
    }

    /// Mount a live thing in the engine's own document, beside Chat and through the page's own
    /// connection, and bring it to the front of the view: it is this same device, never one
    /// more. One that is mounted already only comes to the front. Returns the record it shows.
    @discardableResult
    public func show(live id: String, _ what: Mounted) async throws -> String? {
        let asked: [String: JSON] = switch what {
        case .record(let app, let record): ["app": .string(app), "record": .string(record)]
        case .new(let app): ["app": .string(app), "newDocument": true]
        case .messageView(let message, let conversation):
            ["app": .string(EngineAddress.chat), "view": .object(["message": .string(message), "conversation": .string(conversation)])]
        }
        return try await ask("return globalThis.__tiloLive.show(id, what)", patience: mountPatience,
                             ["id": id, "what": JSON.object(asked).any])["record"]?.string
    }

    /// Nothing live is in front: the view shows nothing a person is meant to look at.
    public func rest() {
        view.callAsyncJavaScript("globalThis.__tiloLive?.rest()", arguments: [:], in: nil, in: .page, completionHandler: nil)
    }

    /// What a live thing draws now, as its mount in the page says it: its frames, the size its
    /// content asks for and the names its semantics hold. For a check that has no eyes, never
    /// for a person.
    public func drawn(live id: String) async throws -> JSON {
        try await ask("return globalThis.__tiloLive.drawn(id)", ["id": id])
    }

    /// Take a live thing down. Its record stays what it is.
    public func close(live id: String) async {
        _ = try? await view.callAsyncJavaScript("return globalThis.__tiloLive?.close(id)", arguments: ["id": id],
                                                in: nil, contentWorld: .page)
    }

    /// Whether a person looks at the view now. While one does, the screen asks the page for
    /// its frames; while none does, the engine's clock does, slowly.
    public func setSeen(_ seen: Bool) {
        view.callAsyncJavaScript("globalThis.__tiloLive?.seen(seen)", arguments: ["seen": seen], in: nil, in: .page,
                                 completionHandler: nil)
    }

    /// - Parameters:
    ///   - patience: how long this ask may go unanswered, where it is one that rightly takes
    ///     longer than `askPatience`.
    ///   - hiding: something a person typed into a protected field, which went into this ask.
    ///     An answer that holds it is handed to nobody.
    func ask(_ script: String, patience: Duration? = nil, _ arguments: [String: Any], hiding: String? = nil) async throws -> JSON {
        guard case .open = state else { throw Problem.notOpen }
        let id = UUID()
        let patience = patience ?? askPatience
        let answer: String? = await withCheckedContinuation { waiting in
            asked[id] = waiting
            // An answer that never comes ends the ask as well: whoever asked is told that none
            // came, and can ask again. Should the page answer after all, nobody waits for it.
            let giveUp = Task { @MainActor [weak self] in
                try? await Task.sleep(for: patience)
                guard !Task.isCancelled else { return }
                self?.asked.removeValue(forKey: id)?.resume(returning: nil)
            }
            view.callAsyncJavaScript(script, arguments: arguments, in: nil, in: .page) { [weak self] result in
                let text = (try? result.get()) as? String
                Task { @MainActor in
                    giveUp.cancel()
                    self?.asked.removeValue(forKey: id)?.resume(returning: text)
                }
            }
        }
        guard let text = answer else { throw Problem.noAnswer }
        // The script takes a protected value out of what it answers. Should one come back all
        // the same, as typed or as JSON writes it, the answer goes no further.
        if let hiding, !hiding.isEmpty,
           text.contains(hiding) || text.contains(String(JSON.string(hiding).text.dropFirst().dropLast())) {
            throw Problem.noAnswer
        }
        guard let read = JSON(text: text) else { throw Problem.noAnswer }
        if let refusal = read["error"] {
            let code = refusal["code"]?.string ?? "failed"
            if code == "not-open" { throw Problem.notOpen }
            // The page's own word that a person's server gave no answer within the page's time.
            // It is no refusal of the server's: whoever asked is told that none came.
            if code == "unanswered" { throw Problem.noAnswer }
            throw Problem.refused(code: code, reason: refusal["reason"]?.string ?? "")
        }
        return read["ok"] ?? .null
    }

    /// The page that was asked is put away: whoever waits for its answer is told there is none.
    private func endAsks() {
        let waiting = asked
        asked.removeAll()
        for one in waiting.values { one.resume(returning: nil) }
    }

    // MARK: The page

    private func go(to address: URL, in workspace: String?, pairing: Bool = false) {
        retrying?.cancel()
        retrying = nil
        lingering = nil
        stops = 0
        moves = 3
        pairs = pairing
        letInto = nil
        pairingHeard = false
        wanted = address
        wantedWorkspace = workspace
        state = .arriving("load")
        load(address)
    }

    /// An address that differs from the one shown only after its `#` would leave the page as
    /// it is, and Tilo's page reads what a link carries only as it loads.
    private func load(_ address: URL) {
        if let loads {
            loads(address)
            return
        }
        guard let shown = view.url, TiloEngine.sameDocument(shown, address) else {
            view.load(URLRequest(url: address))
            return
        }
        view.callAsyncJavaScript("location.replace(address); location.reload();",
                                 arguments: ["address": address.absoluteString], in: nil, in: .page,
                                 completionHandler: nil)
    }

    /// The page's sentence for a device that kept another server's certificate than the one
    /// that answers at its site now (web/peer.mjs keeps it). Today's page says this case in
    /// these words alone: its failure screen calls it `refused`, as it calls a link that
    /// could not be used, and no name of the case is on the page. A test holds this sentence
    /// against the page's.
    static let resetSaid = "This workspace was reset since this browser last opened it."

    /// What Tilo's page calls a connection to its server that is cut (web/connection-status.mjs):
    /// its session closed or failed, or the transport under it is down or trying again. A test
    /// holds this name against the page's.
    static let cut = "offline"

    /// Whether what the page's failure screen says is that the workspace is no longer there:
    /// its cases `gone` (another server answers at the site now) and `deleted` (its owner
    /// deleted it), and the case it has only words for (`resetSaid`).
    static func gone(failure: String, status: String) -> Bool {
        ["gone", "deleted"].contains(failure) || status.contains(resetSaid)
    }

    /// The cases of the page's failure screen that say nothing of a site that answered and
    /// stopped the page's join (web/failure-screen.mjs, `failureCause`): the browser is
    /// offline, the site answered nothing once the page stopped, the server the page was sent
    /// to was not reached, and the device's own key was held by another page of its store.
    static let unanswered: Set<String> = ["offline", "no-connection", "unreachable", "device-key-busy"]

    /// Whether a page that names its entry `failed` says, by its failure screen's case, that
    /// its site answered and its join stopped there. A page that has not named its case yet
    /// says nothing either way.
    static func answered(failure: String) -> Bool {
        !failure.isEmpty && !unanswered.contains(failure)
    }

    static func sameDocument(_ one: URL, _ other: URL) -> Bool {
        let bare = { (url: URL) -> String? in
            var parts = URLComponents(url: url, resolvingAgainstBaseURL: false)
            parts?.fragment = nil
            return parts?.string
        }
        return bare(one) != nil && bare(one) == bare(other)
    }

    fileprivate func heard(_ message: WKScriptMessage) {
        // Only the site's own page, in the main frame, speaks to the engine, and not a page
        // that is on its way out.
        guard lingering == nil, message.frameInfo.isMainFrame, TiloEngine.text(of: message.frameInfo.securityOrigin) == settings.origin,
              let said = message.body as? [String: Any], let kind = said["kind"] as? String
        else { return }
        switch kind {
        case "device-challenge":
            guard let nonce = said["nonce"] as? String, UUID(uuidString: nonce) != nil else { return }
            view.callAsyncJavaScript(
                "if (location.origin === origin && window.top === window) globalThis.__tiloLive?.confirmDevice(given, nonce)",
                arguments: ["origin": settings.origin, "given": pass, "nonce": nonce], in: message.frameInfo, in: .page,
                completionHandler: nil)
        case "changed":
            for listener in changeListeners.values { listener.yield() }
            let waiting = changeWaiters
            changeWaiters.removeAll()
            for waiter in waiting.values { waiter.resume(returning: true) }
        case "live":
            // What a live thing's content asks for. A page's number is a wish, never a command:
            // it is taken only as a size a message can have.
            guard let id = said["id"] as? String, let width = said["width"] as? Double, let height = said["height"] as? Double,
                  width.isFinite, height.isFinite, width >= 0, height >= 0
            else { return }
            let size = CGSize(width: min(width, 4096), height: min(height, 4096))
            for listener in sizeListeners.values { listener.yield((id, size)) }
        case "state":
            hosted = said["hosted"] as? Bool ?? false
            let details = said["details"] as? String ?? ""
            let under = said["status"] as? String ?? ""
            let came = !details.isEmpty && (detailsSaid?.under != under || detailsSaid?.text != details)
            if came { detailsSaid = (under: under, text: details) }
            stand(entry: said["entry"] as? String ?? "", arrival: said["arrival"] as? String ?? "",
                  code: said["code"] as? String ?? "", status: said["status"] as? String ?? "",
                  failure: said["failure"] as? String ?? "", refusal: said["refusal"] as? String ?? "",
                  connection: said["connection"] as? String ?? "",
                  route: said["route"] as? String ?? "", mounted: said["mounted"] as? Bool ?? false,
                  ask: PairingAsk(said["ask"]))
            // A page may say its sentence first and show the details under it a moment later:
            // whoever was told how the try ended hears that there is more to it.
            if came { for listener in endListeners.values { listener.yield() } }
        case "look":
            // Light or dark, as the page says it of itself: one of two cases, and nothing else is taken.
            guard let dark = said["dark"] as? Bool else { return }
            look = dark ? .dark : .light
        default:
            break
        }
    }

    private func stand(entry: String, arrival: String, code: String, status: String, failure: String, refusal: String,
                       connection: String, route: String, mounted: Bool, ask: PairingAsk?) {
        // The page that pairs is past its join: it was let in, and its store holds the device
        // that was approved. Held before anything that may stop the page is taken.
        if pairs {
            if ask != nil || (entry.isEmpty && ["load", "join"].contains(arrival)) { pairingHeard = true }
            if pairingHeard, letInto == nil, TiloEngine.letIn(entry: entry, arrival: arrival) { letInto = wantedWorkspace }
        }
        // A server that turns an open page's session away leaves that page as it was, with a
        // card over it, and the page names the refusal on its root. What it shows is no longer
        // what its server holds, so it is no page that is in: it is loaded again, as one that
        // stopped, and the entry of the page that comes says where the device stands now.
        if !refusal.isEmpty, entry == "open", !pairs {
            if case .open = state { stopped(status.isEmpty ? "The server turned this device's session away" : status) }
            return
        }
        // A page that is in and says that its connection to its server is cut goes on showing
        // what it showed, and nothing says whether the server that answers at its address next
        // is the one that let this device in: a site that is set up again is away for a while
        // and comes back as another, and turns nobody's open session away. So the page is
        // asked for again, as one that stopped: the page that comes says where the device
        // stands, in again or in a workspace that is gone, as it says it to an app that was
        // just started. A page that finds its server again before it is asked for is left as
        // it is (`stopped`).
        if connection == TiloEngine.cut, entry == "open", !pairs {
            if case .open = state { stopped(status.isEmpty ? "The page lost its connection to the server" : status) }
            return
        }
        switch entry {
        case "open":
            guard mounted else { state = .arriving("draw"); return }
            // `route` is the page's path and what follows its `#`.
            let parts = route.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
            let inside = parts.count > 1 ? parts[1] : ""
            // The page that paired stands where its site lets a new device in (its home
            // workspace), under the address of the workspace its link names: it shows that
            // address and holds the other's records. Loaded again, without the link's secret,
            // the device opens the workspace it was asked for, as itself.
            if pairs, let address = EngineAddress.page(of: EngineAddress.chat, in: wantedWorkspace ?? inside, of: settings) {
                pairs = false
                wanted = address
                state = .arriving("open")
                load(address)
                return
            }
            // A page may still stand in another workspace than was asked for, and a link may
            // have led to the desktop. The engine holds the one app's page, in the workspace it
            // was asked for.
            let elsewhere = EngineAddress.isElsewhere(inside, than: wantedWorkspace)
            if parts.first != "/app/" || elsewhere {
                guard moves > 0, let address = EngineAddress.page(of: EngineAddress.chat, in: elsewhere ? wantedWorkspace ?? inside : inside,
                                                                  of: settings)
                else {
                    fail(elsewhere ? "The site opened another workspace than the one that was asked for"
                        : "The site did not open the page that shows one app")
                    return
                }
                moves -= 1
                wanted = address
                state = .arriving("open")
                load(address)
                return
            }
            stops = 0
            state = .open(route: inside)
        case "refused":
            // A link that leads into a workspace that is gone is a refused link. A device that
            // was let in before is told more: where it was let in is no longer there.
            state = !pairs && TiloEngine.gone(failure: failure, status: status) ? .gone(status) : .refused(status)
        case "unsupported":
            // The page was let in and cannot draw here (a simulator offers a page no graphics
            // adapter). Nothing a second load brings changes that, so it is not loaded again,
            // and what is said is the page's own sentence.
            fail(status.isEmpty ? "Tilo's page cannot draw on this device" : status)
        case "failed":
            // A page whose join stopped otherwise than by its server's refusal names its entry
            // `failed`, asks its site what became of the workspace it remembers, and names its
            // failure screen's case after that. Where that case is that the workspace is no
            // longer there, a device that was let in before stands where one does that its
            // server refused. The page says it once (at its next load it has forgotten the
            // workspace), so the load that waits is called off.
            if !pairs, TiloEngine.gone(failure: failure, status: status) {
                retrying?.cancel()
                retrying = nil
                state = .gone(status)
                return
            }
            // Its site served this page, and the page's case says whether its site answered
            // where the join stopped.
            stopped(status, onItsPage: TiloEngine.answered(failure: failure))
        default:
            // The page's question stands before anything else is asked of anyone.
            if let ask {
                state = .asking(ask)
                return
            }
            state = !code.isEmpty && arrival == "join" ? .pairing(code: code) : .arriving(arrival)
        }
    }

    /// The page stopped: on its way in, or after it was in. In a system web view the page
    /// before this one may still hold the lock on its storage as the next asks for it, and the
    /// next then stops; loaded again a moment later it gets in. So the first stop is not said:
    /// the page is loaded once more.
    ///
    /// A load that stops as well is said, in the page's own sentence. A pairing ends there: its
    /// link is spent. A device that is its person's already is not given up: its key is in its
    /// store, and its person's server may only be away. Its page is loaded again after a pause
    /// that grows with every load that stops, up to a minute, for as long as it takes.
    private func stopped(_ why: String, onItsPage: Bool = false) {
        guard let wanted else {
            fail(why, onItsPage: onItsPage)
            return
        }
        let before = stops
        stops += 1
        if before == 0 {
            lastStop = why
            state = .arriving("load")
        } else {
            fail(why, onItsPage: onItsPage)
            if pairs { return }
        }
        let pause = min(stopPause * (1 << min(before, 6)), .seconds(60))
        retrying?.cancel()
        retrying = Task { [weak self] in
            try? await Task.sleep(for: pause)
            guard let self, !Task.isCancelled else { return }
            // A page that got in meanwhile is left as it is.
            if case .open = self.state { return }
            self.state = .arriving("load")
            if let loads = self.loads { loads(wanted) } else { self.view.load(URLRequest(url: wanted)) }
            // The page that is loaded again may never get as far as saying where it stands (it
            // can wait for what the page before it held). Then what stopped the first is said,
            // and nobody waits for ever at a way in that shows nothing.
            try? await Task.sleep(for: self.stopPatience)
            guard !Task.isCancelled, self.state == .arriving("load") else { return }
            self.stopped(why)
        }
    }

    /// Whether a page says that it got past its join: it is in, or it was let in and cannot draw
    /// (`data-entry`), or its arrival stands at a step behind the join (`data-arrival`,
    /// web/arrival.mjs: fetch, compile, join, open, draw). A page that stopped at its join, or
    /// that its site refused, says neither.
    static func letIn(entry: String, arrival: String) -> Bool {
        ["open", "unsupported"].contains(entry) || ["open", "draw"].contains(arrival)
    }

    /// A try ended without getting in. `onItsPage`: on the site's own page, which said that its
    /// join stopped where its site answered.
    private func fail(_ why: String, onItsPage: Bool = false) {
        stoppedOnItsPage = onItsPage
        state = .failed(why)
        for listener in endListeners.values { listener.yield() }
    }

    private func failed(_ error: any Error) {
        let problem = error as NSError
        // A load that another replaced did not fail.
        if problem.domain == NSURLErrorDomain, problem.code == NSURLErrorCancelled { return }
        if problem.domain == "WebKitErrorDomain", problem.code == 102 { return }
        stopped(error.localizedDescription)
    }

    static func text(of origin: WKSecurityOrigin) -> String {
        let standard = origin.protocol == "https" ? 443 : 80
        let port = origin.port == 0 || origin.port == standard ? "" : ":\(origin.port)"
        return "\(origin.protocol)://\(origin.host)\(port)"
    }
}

extension TiloEngine.PairingAsk {
    /// As the engine's script says the page's card: its heading, its sentences, its two buttons.
    fileprivate init?(_ card: Any?) {
        guard let card = card as? [String: Any], let go = card["go"] as? String, let stop = card["stop"] as? String,
              !go.isEmpty, !stop.isEmpty
        else { return nil }
        self.init(heading: card["heading"] as? String ?? "", said: card["said"] as? [String] ?? [], go: go, stop: stop)
    }
}

/// The message handler holds its target weakly: a content controller keeps its handlers.
private final class Ear: NSObject, WKScriptMessageHandler {
    weak var engine: TiloEngine?

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        engine?.heard(message)
    }
}

extension TiloEngine: WKNavigationDelegate {
    public func webView(_ webView: WKWebView,
                        decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        guard let url = action.request.url, !action.shouldPerformDownload else { return .cancel }
        // Tilo's page draws everything itself and makes no frame. Content from anywhere that
        // something inside a live thing would put into one is not loaded: it gets no document
        // to run in, so it stands beside nothing of the person's.
        guard action.targetFrame?.isMainFrame != false else {
            refusedFrames += 1
            return .cancel
        }
        // The engine shows nothing, so nothing leaves it for a browser: what is not the site's
        // own is not loaded at all.
        return policy.destination(of: url) == .inside ? .allow : .cancel
    }

    public func webView(_ webView: WKWebView,
                        decidePolicyFor response: WKNavigationResponse) async -> WKNavigationResponsePolicy {
        response.canShowMIMEType ? .allow : .cancel
    }

    public func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                        withError error: any Error) {
        failed(error)
    }

    public func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) {
        failed(error)
    }

    public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        // The page's process ended. Its records are on the host and its key is in the store,
        // so it is loaded again as the same device.
        stopped("The page's process ended")
    }
}

extension TiloEngine: WKUIDelegate {
    public func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                        for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        nil
    }

    public func webView(_ webView: WKWebView, decideMediaCapturePermissionsFor origin: WKSecurityOrigin,
                        initiatedBy frame: WKFrameInfo, type: WKMediaCaptureType) async -> WKPermissionDecision {
        .deny
    }
}
