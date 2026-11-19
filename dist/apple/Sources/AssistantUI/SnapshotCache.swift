import AssistantCore
import Foundation

/// What the window showed of the person's assistant the last time it was open, kept in this
/// device's caches so that the next opening shows it at once: the main chat's pieces, the
/// sub-agents, the tasks, the automations, the look their Tilo draws in, and the models they may
/// choose. The app's first frame has all of it, and what the source reads takes its place as it
/// comes. It is a copy for drawing and nothing else: nothing here is sent anywhere, and nothing
/// here decides anything.
///
/// What is kept is written a moment (`settle`) after the window last changed, never while a
/// reply is growing letter by letter: a person typing or reading is never made to wait for a
/// file. The file is the caches' own, so the system may take it away, and it is written whole or
/// not at all.
@MainActor
public final class SnapshotCache {
    /// What it keeps: one device's view of one person's assistant, as the window drew it.
    public struct Snapshot: Codable, Equatable, Sendable {
        /// The shape of the file. What was written as another shape is no snapshot of anything.
        public static let shape = 1
        var shape = Snapshot.shape
        var person: Person.ID?
        var main: [Piece]
        var subAgents: [SubAgent]
        var keptTasks: [KeptTask]
        var automations: [KeptTask]
        var look: String?
        var models: [Choice]
        var wrote: Date

        init(person: Person.ID?, main: [Piece], subAgents: [SubAgent], keptTasks: [KeptTask], automations: [KeptTask],
             look: String?, models: [Choice], wrote: Date) {
            self.person = person
            self.main = main
            self.subAgents = subAgents
            self.keptTasks = keptTasks
            self.automations = automations
            self.look = look
            self.models = models
            self.wrote = wrote
        }
    }

    /// A conversation's pieces as the snapshot holds them: enough for the window to draw them.
    /// A piece whose words are still being written is kept with the words it has.
    public struct Piece: Codable, Equatable, Sendable {
        public var id: String
        public var at: Date
        public var author: String
        public var text: String
        public var live: [Live]
        public var task: KeptTask?
        public var isImportant: Bool
        public var keptQuiet: String?
        public var notice: String?
        public var offers: [Choice]
        /// Choices of a question, kept apart from piece-level offers so round-tripping preserves both.
        public var choices: [Choice]? = nil
        public var raised: Int
        public var answers: Answered?
        public var kind: String
        /// What a piece that is no message says of itself, where its words are not enough to
        /// draw it: a brief's beginning and its words, one after the other.
        public var more: String?
    }

    /// A live Tilo thing as the window holds it: the room it leaves, before the source's own
    /// view draws it.
    public struct Live: Codable, Equatable, Sendable {
        public var id: String
        public var title: String
        public var chat: String
        public var reference: String
        public var sizesItself: Bool
        public var theirsToEnd: Bool
    }

    /// A task as its line names it, and as the Tasks panel draws it. One thing the assistant set
    /// up to happen by itself is kept the same way, its line saying when it comes.
    public struct KeptTask: Codable, Equatable, Sendable {
        public var id: String
        public var title: String
        public var state: String
        /// Whether it is marked as important, where the assistant said so.
        public var isImportant: Bool = false
        /// What it waits for, or one line about it.
        public var note: String?
        /// Where it stands in the main chat, when it does.
        public var piece: String?
        /// For one thing the assistant set up to happen by itself: what happens then, and when.
        public var summary: String = ""
        public var next: Date?
        public var once: Bool = false
        public var filter: String?
        public var threshold: Int?
        public var actsAlone: Bool = false
        public var isOn: Bool = true
    }

    /// An agent the main agent started for one job, as its row and its window's head say it.
    public struct SubAgent: Codable, Equatable, Sendable {
        public var id: String
        public var title: String
        public var role: String
        public var state: String
        public var placeName: String
        public var started: Date
        public var finished: Date?
        public var live: Live?
    }

    /// What a thing offered, or a model the person may choose: its name as it is drawn.
    public struct Choice: Codable, Equatable, Sendable {
        public var id: String
        public var title: String
        public var isMain: Bool
        /// The agent that offered it, by name, where one did.
        public var by: String?
        /// Who offers a model, for the Model line. Nil for what a piece offered.
        public var provider: String?

        init(id: String, title: String, isMain: Bool, by: String? = nil, provider: String? = nil) {
            self.id = id
            self.title = title
            self.isMain = isMain
            self.by = by
            self.provider = provider
        }
    }

    /// The piece of its conversation that an answer answers.
    public struct Answered: Codable, Equatable, Sendable {
        public var piece: String
        public var words: String
    }

    /// Maximum number of live-artifact preview PNG thumbnails kept on disk.
    public static let thumbnailLimit = 24
    /// Posted when cached thumbnails are wiped (for example on sign-out) so live stages clear theirs too.
    public static let thumbnailsDidForgetNotification = Notification.Name("so.tilo.assistant.thumbnails.forgotten")

    /// Where the snapshot is written: this device's caches, beside everything else Apple's
    /// system may take away when storage is short.
    public let file: URL
    /// Folder holding cached PNG previews of live artifacts (`Caches/so.tilo.assistant/thumbnails`).
    public let thumbnails: URL
    /// How long after the window last changed the snapshot is written.
    public var settle: Duration = .milliseconds(300)

    @ObservationIgnored private var waiting: Swift.Task<Void, Never>?
    @ObservationIgnored private static let thumbnailQueue = DispatchQueue(label: "so.tilo.assistant.thumbnails", qos: .utility)

    /// The snapshot kept in the caches' own folder for this app. A file that is not there, or is
    /// not what it was written as, is no snapshot: the window then starts empty.
    public init() {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            ?? URL(filePath: NSTemporaryDirectory(), directoryHint: .isDirectory)
        let folder = caches.appending(path: "so.tilo.assistant", directoryHint: .isDirectory)
        file = folder.appending(path: "snapshot.json")
        thumbnails = folder.appending(path: "thumbnails", directoryHint: .isDirectory)
    }

    /// Where a snapshot of its own is kept: for a check, which keeps no file in this Mac's caches.
    public init(file: URL, thumbnails: URL? = nil) {
        self.file = file
        if let thumbnails {
            self.thumbnails = thumbnails
        } else if file.lastPathComponent == "snapshot.json" {
            self.thumbnails = file.deletingLastPathComponent().appending(path: "thumbnails", directoryHint: .isDirectory)
        } else {
            let stem = file.deletingPathExtension().lastPathComponent
            self.thumbnails = file.deletingLastPathComponent().appending(path: "thumbnails-\(stem)", directoryHint: .isDirectory)
        }
    }

    // MARK: Live-artifact preview thumbnails

    /// Safe deterministic filename stem for a live-artifact identifier (no path separators or traversal).
    public nonisolated static func safeThumbnailID(_ id: String) -> String {
        guard !id.isEmpty else { return "_" }
        let hex = Array("0123456789ABCDEF".utf8)
        var encoded: [UInt8] = []
        encoded.reserveCapacity(id.utf8.count)
        for byte in id.utf8 {
            let isAlnum = (byte >= 0x30 && byte <= 0x39) || (byte >= 0x41 && byte <= 0x5A) || (byte >= 0x61 && byte <= 0x7A)
            if isAlnum || byte == 0x2D || byte == 0x5F {
                encoded.append(byte)
            } else {
                encoded.append(0x7E) // '~'
                encoded.append(hex[Int(byte >> 4)])
                encoded.append(hex[Int(byte & 0x0F)])
            }
        }
        let raw = String(decoding: encoded, as: UTF8.self)
        guard raw.count > 160 else { return raw }
        var hash: UInt64 = 14_695_981_039_346_656_037
        for byte in id.utf8 {
            hash ^= UInt64(byte)
            hash &*= 1_099_511_628_211
        }
        return "\(raw.prefix(140))-\(String(hash, radix: 16, uppercase: false))"
    }

    /// File URL for a live artifact's cached PNG preview.
    public nonisolated func thumbnailURL(for id: String) -> URL {
        thumbnails.appending(path: "\(Self.safeThumbnailID(id)).png")
    }

    /// Persist a live artifact's last-rendered PNG snapshot and prune oldest files beyond `thumbnailLimit`.
    public func saveThumbnail(_ pngData: Data, for id: String) {
        guard !pngData.isEmpty else { return }
        let dir = thumbnails
        let target = thumbnailURL(for: id)
        let limit = Self.thumbnailLimit
        Self.thumbnailQueue.async {
            let fm = FileManager.default
            try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
            try? pngData.write(to: target, options: .atomic)
            try? fm.setAttributes([.modificationDate: Date()], ofItemAtPath: target.path)
            Self.pruneThumbnails(in: dir, limit: limit)
        }
    }

    /// Read a cached PNG preview for `id`, or nil if none exists.
    public func readThumbnail(for id: String) -> Data? {
        try? Data(contentsOf: thumbnailURL(for: id))
    }

    /// Wipe all cached live-artifact preview thumbnails and notify observers.
    public func removeThumbnails() {
        let dir = thumbnails
        Self.thumbnailQueue.async {
            try? FileManager.default.removeItem(at: dir)
        }
        NotificationCenter.default.post(name: Self.thumbnailsDidForgetNotification, object: nil)
    }

    /// Wait for pending thumbnail disk work to finish (used by tests).
    func flushThumbnails() async {
        await withCheckedContinuation { continuation in
            Self.thumbnailQueue.async {
                continuation.resume()
            }
        }
    }

    private nonisolated static func pruneThumbnails(in dir: URL, limit: Int) {
        guard limit > 0 else { return }
        let fm = FileManager.default
        guard let urls = try? fm.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles]
        ) else { return }
        let pngs = urls.filter { $0.pathExtension.lowercased() == "png" }
        guard pngs.count > limit else { return }
        let sorted = pngs.sorted { lhs, rhs in
            let leftDate = (try? lhs.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
            let rightDate = (try? rhs.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
            return leftDate > rightDate
        }
        for stale in sorted.dropFirst(limit) {
            try? fm.removeItem(at: stale)
        }
    }

    // MARK: Reading

    /// What was kept the last time, read whole, or nil where nothing was. Read where the app
    /// opens, so that its first frame shows it: the reading is one pass over what was drawn.
    public func read() -> Snapshot? {
        guard let data = try? Data(contentsOf: file) else { return nil }
        // A file that was cut short, or written as something else, is not a snapshot of anything.
        guard let kept = try? JSONDecoder().decode(Snapshot.self, from: data), kept.shape == Snapshot.shape else { return nil }
        return kept
    }

    // MARK: Writing

    /// The window changed. What it shows now is written a moment from now, and the moment starts
    /// again with each change, so that a reply growing over a second is written once, as it
    /// stood when it stopped growing.
    public func keep(_ snapshot: @escaping @MainActor @Sendable () -> Snapshot) {
        waiting?.cancel()
        let settle = settle
        let file = file
        waiting = Task { [weak self] in
            try? await Task.sleep(for: settle)
            guard !Task.isCancelled else { return }
            self?.write(snapshot(), to: file)
            self?.waiting = nil
        }
    }

    /// What the window shows now, written at once: for the moment the app is put away, when no
    /// moment is left to wait.
    public func write(_ snapshot: Snapshot) {
        waiting?.cancel()
        waiting = nil
        write(snapshot, to: file)
    }

    /// Forget what was kept: the device is nobody's, and what it showed is nobody's to show again.
    public func forget() {
        waiting?.cancel()
        waiting = nil
        remove(file)
        removeThumbnails()
    }

    /// The writing itself, off the window's own turn: the snapshot is made on the main actor,
    /// where the window's values stand, and the file is written by a task of its own.
    private func write(_ snapshot: Snapshot, to file: URL) {
        guard let data = try? JSONEncoder().encode(snapshot) else { return }
        Swift.Task.detached(priority: .utility) {
            try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: file, options: .atomic)
        }
    }

    private func remove(_ file: URL) {
        Swift.Task.detached(priority: .utility) {
            try? FileManager.default.removeItem(at: file)
        }
    }
}

// MARK: - What the window shows, as the snapshot holds it

extension SnapshotCache.Snapshot {
    /// The window as it stands: the pieces of the main chat, the things beside it, and whose they are.
    public init(pieces: [Item], subAgents: [SubAgent], works: [AssistantTask], automations: [Automation],
                look: Look?, models: [ModelChoice], person: Person.ID?, wrote: Date = Date()) {
        let tasks: [SnapshotCache.KeptTask] = works.map { SnapshotCache.KeptTask($0) }
        let meetings: [SnapshotCache.KeptTask] = automations.map { SnapshotCache.KeptTask(setUp: $0) }
        self.init(person: person, main: pieces.map { SnapshotCache.Piece($0) },
                  subAgents: subAgents.map { SnapshotCache.SubAgent($0) },
                  keptTasks: tasks, automations: meetings,
                  look: look == .dark ? "dark" : look == .light ? "light" : nil,
                  models: models.map { SnapshotCache.Choice($0) }, wrote: wrote)
    }

    /// Which look the person's Tilo draws in, as it was kept.
    public var keptLook: Look? {
        switch look {
        case "dark": .dark
        case "light": .light
        default: nil
        }
    }

    /// The pieces of the main chat, as the source would hand them over.
    public var pieces: [Item] { main.compactMap(\.item) }
    public var keptSubAgents: [SubAgent] { subAgents.compactMap(\.subAgent) }
    /// The tasks the Tasks panel draws.
    public var tasks: [AssistantTask] { keptTasks.compactMap(\.task) }
    public var keptAutomations: [Automation] { automations.compactMap(\.automation) }
    public var keptModels: [ModelChoice] { models.compactMap(\.model) }
}

extension SnapshotCache.Piece {
    public init(_ item: Item) {
        let message = item.message
        self.init(id: item.id.value, at: item.at, author: SnapshotCache.Piece.author(of: item),
                  text: message?.text ?? SnapshotCache.Piece.text(of: item.content),
                  live: (message?.live ?? []).map(SnapshotCache.Live.init),
                  task: item.task.map { SnapshotCache.KeptTask(id: $0.task.value, title: $0.title, state: SnapshotCache.KeptTask.lineName(of: $0.state)) },
                  isImportant: item.isImportant, keptQuiet: item.keptQuiet, notice: item.notice,
                  offers: item.offers.map(SnapshotCache.Choice.init),
                  choices: SnapshotCache.Piece.choices(of: item),
                  raised: item.raised,
                  answers: message?.answers.map { SnapshotCache.Answered(piece: $0.piece.value, words: $0.words) },
                  kind: SnapshotCache.Piece.kind(of: item.content),
                  more: message == nil ? SnapshotCache.Piece.more(of: item.content) : nil)
    }

    /// The piece as the source would hand it over, where what was kept says enough for it. A
    /// reply whose turn is not kept is a reply, and draws nothing of a turn.
    public var item: Item? {
        let piece = Item.ID(id)
        guard let author = SnapshotCache.Piece.author(named: author) else { return nil }
        switch kind {
        case "report":
            return Item(id: piece, at: at, .report(text), task: line, isImportant: isImportant, keptQuiet: keptQuiet)
        case "reminder":
            return Item(id: piece, at: at, .reminder(text), task: line, isImportant: isImportant, keptQuiet: keptQuiet)
        case "brief":
            guard let more, case let parts = SnapshotCache.Piece.split(more), parts.count == 2, let begins = Double(parts[0])
            else { return nil }
            let brief = Brief(title: text, begins: begins == 0 ? nil : Date(timeIntervalSince1970: begins), text: parts[1])
            return Item(id: piece, at: at, .brief(brief), task: line, isImportant: isImportant, keptQuiet: keptQuiet)
        // Question choices and piece-level offers are kept separately so round-tripping preserves both.
        case "question":
            let questionChoices = (choices ?? offers).map(\.choice)
            let pieceOffers = choices == nil ? [] : offers.map(\.offer)
            return Item(id: piece, at: at, .question(Question(id: Question.ID(more ?? id), text: text, choices: questionChoices)),
                        task: line, isImportant: isImportant, keptQuiet: keptQuiet, notice: notice, offers: pieceOffers)
        default:
            let message = Message(author, text, live: live.compactMap(\.thing),
                                  answers: answers.map { Answered(Item.ID($0.piece), words: $0.words) })
            return Item(id: piece, at: at, .message(message), task: line, isImportant: isImportant, keptQuiet: keptQuiet,
                        notice: notice, offers: offers.map(\.offer), raised: raised)
        }
    }

    private var line: TaskLine? {
        guard let task, let state = SnapshotCache.KeptTask.keptState(named: task.state) else { return nil }
        return TaskLine(task: AssistantTask.ID(task.id), title: task.title, state: state)
    }

    /// Question choices for a question piece, or nil for other piece kinds.
    static func choices(of item: Item) -> [SnapshotCache.Choice]? {
        guard case .question(let question) = item.content else { return nil }
        return question.choices.map(SnapshotCache.Choice.init)
    }

    static func kind(of content: Item.Content) -> String {
        switch content {
        case .message: "message"
        case .report: "report"
        case .question: "question"
        case .secret: "secret"
        case .connection: "connection"
        case .event: "event"
        case .brief: "brief"
        case .reminder: "reminder"
        }
    }

    /// What a piece that is no message says of itself, where its words are not enough: a brief's
    /// beginning and words, or a question's own id.
    static func more(of content: Item.Content) -> String? {
        switch content {
        case .brief(let brief): "\(brief.begins?.timeIntervalSince1970 ?? 0)\n\(brief.text)"
        case .question(let question): question.id.value
        default: nil
        }
    }

    static func split(_ more: String) -> [String] {
        more.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
    }

    /// A piece's words where it carries no message: what the window draws of it in one line.
    static func text(of content: Item.Content) -> String {
        switch content {
        case .message(let message): message.text
        case .report(let words), .reminder(let words): words
        case .question(let question): question.text
        case .secret(let secret): secret.text
        case .connection(let offer): offer.text
        case .event(let card): card.text
        case .brief(let brief): brief.title
        }
    }

    static func author(of item: Item) -> String {
        guard let message = item.message else { return "assistant" }
        switch message.author {
        case .person: return "person"
        case .assistant: return "assistant"
        case .mainAgent(let word): return word == .job ? "job" : "instruction"
        }
    }

    static func author(named author: String) -> Message.Author? {
        switch author {
        case "person": .person
        case "assistant": .assistant
        case "job": .mainAgent(.job)
        case "instruction": .mainAgent(.instruction)
        default: nil
        }
    }
}

extension SnapshotCache.Live {
    public init(_ thing: LiveThing) {
        let chat: String = switch thing.chat {
        case .main: "main"
        case .subAgent(let id): "sub:\(id.value)"
        }
        self.init(id: thing.id.value, title: thing.title, chat: chat, reference: thing.reference,
                  sizesItself: thing.sizesItself, theirsToEnd: thing.theirsToEnd)
    }

    public var thing: LiveThing? {
        let inChat: Chat = if chat.hasPrefix("sub:") {
            .subAgent(SubAgent.ID(String(chat.dropFirst("sub:".count))))
        } else {
            .main
        }
        return LiveThing(id: LiveThing.ID(id), title: title, chat: inChat, reference: reference,
                         sizesItself: sizesItself, theirsToEnd: theirsToEnd)
    }
}

extension SnapshotCache.KeptTask {
    public init(_ line: TaskLine) {
        self.init(id: line.task.value, title: line.title, state: SnapshotCache.KeptTask.lineName(of: line.state))
    }

    public init(_ task: AssistantTask) {
        self.init(id: task.id.value, title: task.title, state: SnapshotCache.KeptTask.taskName(of: task.state),
                  isImportant: task.isImportant, note: task.note, piece: task.item?.value)
    }

    /// One thing the assistant set up to happen by itself, kept the way a task is: when it comes
    /// at a time it is as up next, and what happens when something else does as one waiting for input.
    public init(setUp meeting: Automation) {
        self.init(id: meeting.id.value, title: meeting.title,
                  state: SnapshotCache.KeptTask.taskName(of: meeting.sort == .scheduled ? .upNext : .inProgress),
                  note: meeting.note, summary: meeting.summary, next: meeting.next, once: meeting.once, filter: meeting.filter,
                  threshold: meeting.threshold, actsAlone: meeting.actsAlone, isOn: meeting.isOn)
    }

    /// The work as the Tasks panel draws it. What its line says it waits for is not kept: it says
    /// so again once the source reads it.
    public var task: AssistantTask? {
        guard let state = SnapshotCache.KeptTask.taskState(named: state) else { return nil }
        return AssistantTask(id: AssistantTask.ID(id), title: title, state: state, isImportant: isImportant,
                             note: note, item: piece.map { Item.ID($0) })
    }

    /// One thing the assistant set up to happen by itself, as its line in the Tasks panel says it:
    /// what comes at a time is drawn as scheduled, what happens when something else does as an event.
    public var automation: Automation? {
        guard let state = SnapshotCache.KeptTask.taskState(named: state) else { return nil }
        let sort: Automation.Sort = state == .upNext || state == .resolved ? .scheduled : .event
        return Automation(id: Automation.ID(id), sort: sort, title: title, summary: summary, next: next, once: once,
                          filter: filter, threshold: threshold, note: note, actsAlone: actsAlone, isOn: isOn)
    }

    static func lineName(of state: TaskLine.State) -> String {
        switch state {
        case .none: "none"
        case .scheduled(let words): "scheduled:\(words)"
        case .actedAlone: "actedAlone"
        case .replies(let count): "replies:\(count)"
        }
    }

    static func keptState(named name: String) -> TaskLine.State? {
        let parts = name.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        switch parts.first {
        case "none": return TaskLine.State.none
        case "scheduled": return .scheduled(parts.count > 1 ? parts[1] : "")
        case "actedAlone": return .actedAlone
        case "replies": return .replies(parts.count > 1 ? Int(parts[1]) ?? 0 : 0)
        default: return nil
        }
    }

    static func taskName(of state: AssistantTask.State) -> String {
        switch state {
        case .readyForReview: "readyForReview"
        case .needsInput: "needsInput"
        case .inProgress: "inProgress"
        case .upNext: "upNext"
        case .resolved: "resolved"
        }
    }

    static func taskState(named name: String) -> AssistantTask.State? {
        switch name {
        case "readyForReview": .readyForReview
        case "needsInput": .needsInput
        case "inProgress": .inProgress
        case "upNext": .upNext
        case "resolved": .resolved
        default: nil
        }
    }
}

extension SnapshotCache.Choice {
    public init(_ choice: Choice) {
        self.init(id: choice.id.value, title: choice.title, isMain: choice.isMain)
    }

    public init(_ offer: Offer) {
        self.init(id: offer.id.value, title: offer.title, isMain: offer.isMain, by: offer.by)
    }

    public init(_ model: ModelChoice) {
        self.init(id: model.id.value, title: model.name, isMain: false, provider: model.provider)
    }

    public var choice: Choice {
        Choice(id: Choice.ID(id), title: title, isMain: isMain)
    }

    public var offer: Offer {
        Offer(id: Offer.ID(id), title: title, isMain: isMain, by: by)
    }

    /// The model as the assistant view's Model line names it.
    public var model: ModelChoice {
        ModelChoice(id: ModelChoice.ID(id), name: title, provider: provider ?? "")
    }
}

extension SnapshotCache.SubAgent {
    public init(_ agent: SubAgent) {
        self.init(id: agent.id.value, title: agent.title, role: agent.role, state: SnapshotCache.SubAgent.name(of: agent.state),
                  placeName: agent.runsOn, started: agent.started, finished: agent.finished, live: agent.live.map(SnapshotCache.Live.init))
    }

    public var subAgent: SubAgent? {
        guard let state = SnapshotCache.SubAgent.state(named: state) else { return nil }
        return SubAgent(id: SubAgent.ID(id), title: title, role: role, state: state, runsOn: placeName,
                        started: started, finished: finished, live: live?.thing)
    }

    static func name(of state: SubAgent.State) -> String {
        switch state {
        case .working: "working"
        case .needsYou: "needsYou"
        case .done: "done"
        case .failed: "failed"
        case .stopped: "stopped"
        }
    }

    static func state(named name: String) -> SubAgent.State? {
        switch name {
        case "working": .working
        case "needsYou": .needsYou
        case "done": .done
        case "failed": .failed
        case "stopped": .stopped
        default: nil
        }
    }
}
