import SwiftUI
import WebKit

#if os(macOS)
import AppKit
public typealias LivePlatformView = NSView
public typealias LivePlatformImage = NSImage
#else
import UIKit
public typealias LivePlatformView = UIView
public typealias LivePlatformImage = UIImage
#endif

/// One live Tilo thing a native view shows: an app on a resource of the person's workspace, or
/// the view a message carries.
public struct LiveThing: Hashable, Sendable, Identifiable {
    public enum Content: Hashable, Sendable {
        /// This app on this resource.
        case record(app: String, record: String)
        /// The view a message carries, by its message and the conversation it stands in, never by
        /// its text: Tilo's page shows it alone, as this device's person, and says whose code it is.
        case messageView(message: String, conversation: String)
    }

    public let id: String
    /// What it is called, for people and for VoiceOver.
    public var title: String
    public var content: Content

    public init(id: String, title: String, _ content: Content) {
        self.id = id
        self.title = title
        self.content = content
    }

    /// From the colon-separated reference a conversation's model carries for an app resource or
    /// `view:<message>:<conversation>`. Nil for anything else.
    public init?(id: String, title: String, reference: String) {
        let parts = reference.split(separator: ":", maxSplits: 2, omittingEmptySubsequences: false).map(String.init)
        guard parts.count == 3, !parts[1].isEmpty, !parts[2].isEmpty else { return nil }
        switch parts[0] {
        case "record": self.init(id: id, title: title, .record(app: parts[1], record: parts[2]))
        case "view": self.init(id: id, title: title, .messageView(message: parts[1], conversation: parts[2]))
        default: return nil
        }
    }

    public var reference: String {
        switch content {
        case .record(let app, let record): "record:\(app):\(record)"
        case .messageView(let message, let conversation): "view:\(message):\(conversation)"
        }
    }
}

/// The sentences a live thing's place says. An app's own catalogue translates them under
/// these keys; without one they read as written here.
enum LiveWords: String {
    case loading = "live-loading"
    case offline = "live-offline"
    case failed = "live-failed"
    case showLive = "live-show"
    case openInTilo = "live-open-in-tilo"
    case tryAgain = "live-try-again"
    case notOffered = "live-not-offered"
    case cachedConnecting = "live-cached-connecting"

    var english: String {
        switch self {
        case .loading: "Opening…"
        case .offline: "This device is not connected to your Tilo right now"
        case .failed: "Tilo could not open this here"
        case .showLive: "Show live"
        case .openInTilo: "Open in Tilo"
        case .tryAgain: "Try again"
        case .notOffered: "This Tilo cannot show this here yet"
        case .cachedConnecting: "Cached · connecting…"
        }
    }

    var said: String { Bundle.main.localizedString(forKey: rawValue, value: english, table: nil) }
}

/// Persistent disk store for live-artifact preview PNG thumbnails, bounded to the latest files.
final class LiveThumbnailStore: Sendable {
    static let defaultLimit = 24
    static let forgottenNotification = Notification.Name("so.tilo.assistant.thumbnails.forgotten")
    private static let ioQueue = DispatchQueue(label: "so.tilo.live.thumbnails", qos: .utility)

    static var defaultDirectory: URL {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            ?? URL(filePath: NSTemporaryDirectory(), directoryHint: .isDirectory)
        return caches
            .appending(path: "so.tilo.assistant", directoryHint: .isDirectory)
            .appending(path: "thumbnails", directoryHint: .isDirectory)
    }

    let directory: URL
    let limit: Int

    init(directory: URL, limit: Int = LiveThumbnailStore.defaultLimit) {
        self.directory = directory
        self.limit = limit
    }

    static func safeID(_ id: LiveThing.ID) -> String {
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

    private static func decodeSafeID(_ stem: String) -> LiveThing.ID? {
        guard !stem.isEmpty, stem != "_" else { return nil }
        let bytes = Array(stem.utf8)
        var decoded: [UInt8] = []
        decoded.reserveCapacity(bytes.count)
        var idx = 0
        while idx < bytes.count {
            if bytes[idx] == 0x7E, idx + 2 < bytes.count,
               let hi = hexVal(bytes[idx + 1]), let lo = hexVal(bytes[idx + 2]) {
                decoded.append((hi << 4) | lo)
                idx += 3
            } else {
                decoded.append(bytes[idx])
                idx += 1
            }
        }
        return String(bytes: decoded, encoding: .utf8)
    }

    private static func hexVal(_ byte: UInt8) -> UInt8? {
        switch byte {
        case 0x30...0x39: byte - 0x30
        case 0x41...0x46: byte - 0x41 + 10
        case 0x61...0x66: byte - 0x61 + 10
        default: nil
        }
    }

    func url(for id: LiveThing.ID) -> URL {
        directory.appending(path: "\(Self.safeID(id)).png")
    }

    func save(_ pngData: Data, for id: LiveThing.ID) {
        guard !pngData.isEmpty else { return }
        let dir = directory
        let target = url(for: id)
        let cap = limit
        Self.ioQueue.async {
            let fm = FileManager.default
            try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
            try? pngData.write(to: target, options: .atomic)
            try? fm.setAttributes([.modificationDate: Date()], ofItemAtPath: target.path)
            Self.prune(in: dir, limit: cap)
        }
    }

    func loadData(for id: LiveThing.ID) -> Data? {
        try? Data(contentsOf: url(for: id))
    }

    func loadAllData() -> [LiveThing.ID: Data] {
        let fm = FileManager.default
        guard let urls = try? fm.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles]
        ) else { return [:] }
        var loaded: [LiveThing.ID: Data] = [:]
        for file in urls where file.pathExtension.lowercased() == "png" {
            let stem = file.deletingPathExtension().lastPathComponent
            guard let id = Self.decodeSafeID(stem), let data = try? Data(contentsOf: file), !data.isEmpty else { continue }
            loaded[id] = data
        }
        return loaded
    }

    func wipe() {
        let dir = directory
        Self.ioQueue.async {
            try? FileManager.default.removeItem(at: dir)
        }
    }

    func flush() async {
        await withCheckedContinuation { continuation in
            Self.ioQueue.async { continuation.resume() }
        }
    }

    private static func prune(in dir: URL, limit: Int) {
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
}

@MainActor
private func livePNGData(from image: LivePlatformImage) -> Data? {
    #if os(macOS)
    guard let tiff = image.tiffRepresentation,
          let bitmap = NSBitmapImageRep(data: tiff) else { return nil }
    return bitmap.representation(using: .png, properties: [:])
    #else
    return image.pngData()
    #endif
}

@MainActor
private func liveImage(from pngData: Data) -> LivePlatformImage? {
    #if os(macOS)
    return NSImage(data: pngData)
    #else
    return UIImage(data: pngData)
    #endif
}

/// Where the engine's one web view stands, and which live thing it shows.
///
/// Every live thing is one more mount in the engine's own document, so all of them are the
/// person's one device and one process holds Tilo's runtime once. The price is that one is
/// live at a time: the others show the picture taken as they left the front, and wake when
/// they come to it. At most `limit` stay mounted; past it the one longest away is taken down.
@MainActor
public final class LiveStage: ObservableObject {
    public enum State: Equatable, Sendable {
        /// Not in front. It shows its last picture, or its name.
        case asleep
        case loading
        case live
        /// The device is not connected to the person's Tilo.
        case offline
        /// Tilo's own refusal as it came, or none: for a program's log, never for a person.
        case failed(String)
    }

    /// Why a thing is not shown, for the words its place says. Tilo's own refusal names
    /// addresses and actions and is no sentence for a person: the place says one of these,
    /// in the app's words.
    public enum Trouble: Equatable, Sendable {
        /// The person's Tilo does not offer this yet. Asking again changes nothing.
        case notOffered
        /// It was asked for and did not come this time.
        case notShown
    }

    public let engine: TiloEngine
    @Published public private(set) var front: LiveThing.ID?
    @Published public private(set) var states: [LiveThing.ID: State] = [:]
    /// Why each thing that failed is not shown.
    @Published public private(set) var troubles: [LiveThing.ID: Trouble] = [:]
    @Published public private(set) var pictures: [LiveThing.ID: LivePlatformImage] = [:]
    /// Live-artifact ids currently showing a disk-cached preview before mounting live in this launch.
    @Published public private(set) var cachedPreviewIDs: Set<LiveThing.ID> = []
    /// Disk store backing persistent live-artifact PNG thumbnails across launches.
    private let thumbnailStore: LiveThumbnailStore?
    /// The picture each thing had when it was last in front, by the thing it showed: so that a
    /// thing that takes the front again says what it said last while its page comes, rather than
    /// nothing at all where the view it stood in was put away.
    private var lastShown: [LiveThing.ID: LivePlatformImage] = [:]
    /// What a place shows while its thing is brought to the front where no thing has shown yet:
    /// the view itself a moment ago, which is a closer look at the way in than a frame of nothing.
    private var remembered: LivePlatformImage?
    /// The thing a place has just asked to bring, and when it asked: a press on a thing whose
    /// bring has begun is not a second ask, so a place that comes back into its window while its
    /// thing is on its way in does not have it loaded again.
    private var bringing: (id: LiveThing.ID, asked: Date)?
    /// The size each thing's content asks for, as its mount in Tilo's page said it last.
    @Published public private(set) var sizes: [LiveThing.ID: CGSize] = [:]
    public var limit = 3

    /// The mounting of each thing, as the page counts it: which of its shows are at work, whether
    /// they all ended without bringing it, and whether it is mounted.
    private var reports: [LiveThing.ID: TiloEngine.LiveReports] = [:]
    /// The brings that are on their way in, one per thing: a page that answers none of them
    /// would leave the place saying that it opens for ever.
    private var bringingBy: [LiveThing.ID: Task<Void, Never>] = [:]

    /// The mounted things, the one longest away first.
    private var mounted: [LiveThing.ID] = []
    /// The things whose place wants one in front, and waits for a bring already on its way
    /// (`waitingFor`). Pressing again while it waits cancels the waiting: the thing in front is
    /// then asked for at once, as the last press is.
    private var waiting: Set<LiveThing.ID> = []
    /// The things the page is bringing now, and so whose place is told that it opens. Wanting a
    /// thing brings it in the same moment, and the place is not drawn between the two: the set is
    /// read as it was, before the ask, and told before the next drawing.
    private var showingNow: Set<LiveThing.ID> = []
    private weak var restPlace: LivePlatformView?
    private var listening: Task<Void, Never>?
    private var watching: Task<Void, Never>?
    private var sizing: Task<Void, Never>?
    private var forgetting: Task<Void, Never>?
    /// Monotonic generation so an older `bring` never overwrites a newer request.
    private var bringGen: UInt64 = 0
    /// Which SwiftUI view currently owns the front mount, so a disappearing inline card
    /// cannot rest a thing that just moved to the stage.
    private var activePlace: UUID?
    /// A thing wanted while the page was still connecting; brought automatically once `.open`.
    private var pendingBring: LiveThing?

    public init(engine: TiloEngine, thumbnailsDirectory: URL? = nil) {
        self.engine = engine
        if let thumbnailsDirectory {
            self.thumbnailStore = LiveThumbnailStore(directory: thumbnailsDirectory)
        } else if engine.store.isPersistent {
            self.thumbnailStore = LiveThumbnailStore(directory: LiveThumbnailStore.defaultDirectory)
        } else {
            self.thumbnailStore = nil
        }
        if let store = self.thumbnailStore {
            for (id, data) in store.loadAllData() {
                if let image = liveImage(from: data) {
                    pictures[id] = image
                    cachedPreviewIDs.insert(id)
                }
            }
        }
        forgetting = Task { [weak self] in
            for await _ in NotificationCenter.default.notifications(named: LiveThumbnailStore.forgottenNotification) {
                guard let self else { return }
                self.wipeCachedThumbnails()
            }
        }
        sizing = Task { [weak self] in
            for await (id, size) in engine.contentSizes() {
                guard let self else { return }
                if self.sizes[id] != size { self.sizes[id] = size }
            }
        }
        listening = Task { [weak self] in
            for await state in engine.states() {
                guard let self else { return }
                if case .open = state {
                    if let pending = self.pendingBring {
                        self.pendingBring = nil
                        Task { await self.bring(pending) }
                    }
                    continue
                }
                // The page left: whatever it had mounted went with it, and the pictures taken of
                // it are of a page that is not there.
                self.mounted.removeAll()
                self.forgetPictures()
                self.bringing = nil
                if let front = self.front {
                    self.states[front] = .offline
                    self.front = nil
                }
            }
        }
        watching = Task { [weak self] in
            for await told in engine.liveReports() {
                guard let self else { return }
                self.reports = told
                for (id, report) in told where report.failed > 0 && !report.mounted && report.mounting == 0 {
                    // Every show the page had of this thing ended without bringing it, and it is
                    // not at work on another: the place says so rather than opening for ever.
                    self.mountFailed(id, waited: false)
                }
            }
        }
    }

    deinit {
        listening?.cancel()
        watching?.cancel()
        sizing?.cancel()
        forgetting?.cancel()
    }

    public func state(of thing: LiveThing) -> State { states[thing.id] ?? .asleep }

    /// Wipe cached preview thumbnails from memory and disk (invoked on sign-out).
    public func wipeCachedThumbnails() {
        pictures.removeAll()
        lastShown.removeAll()
        cachedPreviewIDs.removeAll()
        remembered = nil
        thumbnailStore?.wipe()
    }

    /// Persist a snapshot image for `id` into memory and the bounded on-disk thumbnail cache.
    public func storeThumbnail(_ image: LivePlatformImage, for id: LiveThing.ID) {
        pictures[id] = image
        lastShown[id] = image
        cachedPreviewIDs.remove(id)
        if let store = thumbnailStore, let data = livePNGData(from: image) {
            store.save(data, for: id)
        }
    }

    /// Wait for pending thumbnail disk writes to complete (used by tests).
    func flushThumbnails() async {
        await thumbnailStore?.flush()
    }

    /// Whether `thing` is currently queued to mount as soon as the engine connects.
    func isPending(_ thing: LiveThing) -> Bool {
        pendingBring?.id == thing.id
    }

    /// Whether `thing` currently displays a disk-cached preview awaiting live mount.
    func isShowingCachedPreview(of thing: LiveThing) -> Bool {
        cachedPreviewIDs.contains(thing.id) && pictures[thing.id] != nil
    }

    /// The picture that stands for a thing where its page is not in front: the one taken as it
    /// left the front, or a cached PNG loaded from disk, or the last it was shown as, or what the
    /// view itself showed a moment before.
    func picture(of thing: LiveThing) -> LivePlatformImage? {
        if let existing = pictures[thing.id] { return existing }
        if let last = lastShown[thing.id] { return last }
        if let store = thumbnailStore, let data = store.loadData(for: thing.id), let image = liveImage(from: data) {
            pictures[thing.id] = image
            cachedPreviewIDs.insert(thing.id)
            return image
        }
        return remembered
    }

    /// Keep a picture of the view as it is now, where no thing has been in front to leave one: a
    /// place that comes to be where the way in stood has what stood there to say while its own
    /// page comes. A view in no window is not asked, and a thing's own picture is never written
    /// over by it.
    func tookPicture() {
        guard remembered == nil, engine.view.window != nil else { return }
        Task { [weak self] in
            guard let self, let picture = try? await self.engine.view.takeSnapshot(configuration: nil) else { return }
            if self.remembered == nil { self.remembered = picture }
        }
    }

    /// The page went away, and the picture taken of it with it. The next page that comes has its
    /// own.
    private func forgetPictures() {
        remembered = nil
    }

    /// Make a thing the one in front now: the place a person pressed says that it is opening at
    /// once, and is brought when it can be. Where several places press at the same moment, the
    /// last press is the thing in front, so its place is the one that opens. A press made while
    /// the person's page is not in says so in the place: the thing is not shown as opening, and
    /// nothing is waited for, where nothing can open it.
    public func wants(_ thing: LiveThing) {
        guard case .open = engine.state else {
            pendingBring = thing
            _ = waiting.remove(thing.id)
            showing(id: thing.id, false)
            states[thing.id] = .offline
            troubles[thing.id] = nil
            return
        }
        pendingBring = nil
        bringing = (thing.id, Date())
        // A thing shown is asked for nothing more: wanting it again is no ask of the page. One
        // that cannot be shown again says that it opens until its next ask says otherwise, so
        // that a press after a page went away shows that it was heard.
        if states[thing.id] != .live { states[thing.id] = .loading }
        troubles[thing.id] = nil
    }

    /// Whether a place has asked for this thing and its bring is on its way in: bringing it again
    /// would take it down from the page to put it back. Another press, or another place, is not
    /// that ask: it wants the thing in front now, which `wants` says and `waitingFor` sees.
    func isComing(_ thing: LiveThing) -> Bool {
        guard let bringing else { return false }
        return bringing.id == thing.id && Date().timeIntervalSince(bringing.asked) < LiveStage.comingPatience
    }

    /// Wait for the bring of a thing that is on its way in already. The wait ends when the ask
    /// does, and is cut short when a place wants the thing again.
    func waitingFor(_ thing: LiveThing) async {
        _ = await withTaskGroup(of: Bool.self) { group in
            group.addTask { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .milliseconds(50))
                    guard let self, await !self.isComing(thing) else { continue }
                    return true
                }
                return false
            }
            group.addTask { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .milliseconds(100))
                    guard let self else { return false }
                    if await !self.waiting.contains(thing.id) { return true }
                }
                return false
            }
            _ = await group.next()
            group.cancelAll()
            return true
        }
    }

    /// Whether the page is bringing this thing now: its place says that it opens, where its
    /// place says anything of it at all.
    func isShowing(_ thing: LiveThing) -> Bool { showingNow.contains(thing.id) }

    /// Said by a place whose thing is on its way in already: whether this is the wait it began,
    /// rather than a press made since, which wants the thing in front now.
    func wanted(_ thing: LiveThing) -> Bool { waiting.insert(thing.id).inserted }

    /// Say that a thing's place stands as opening or as not.
    private func showing(id: LiveThing.ID, _ on: Bool) {
        var now = showingNow
        if on { now.insert(id) } else { now.remove(id) }
        guard now != showingNow else { return }
        showingNow = now
        objectWillChange.send()
    }

    /// How long a bring that was asked for counts as on its way in: a page mounts a thing in
    /// seconds where it is the first, and the ask itself says so when it takes longer.
    private static let comingPatience: TimeInterval = 120

    /// Bring a live thing to the front: the one web view shows it and takes the person's input.
    public func bring(_ thing: LiveThing) async {
        await bring(thing, for: nil)
    }

    func bring(_ thing: LiveThing, for place: UUID?) async {
        let what: TiloEngine.Mounted = switch thing.content {
        case .record(let app, let record): .record(app: app, record: record)
        case .messageView(let message, let conversation): .messageView(message: message, conversation: conversation)
        }
        guard case .open = engine.state else {
            pendingBring = thing
            showing(id: thing.id, false)
            if states[thing.id] != .live { states[thing.id] = .offline }
            return
        }
        pendingBring = nil
        bringGen &+= 1
        let gen = bringGen
        if let place { activePlace = place }
        showing(id: thing.id, true)
        if let before = front, before != thing.id { await sleep(before) }
        guard gen == bringGen else { return }
        tookPicture()
        states[thing.id] = .loading
        troubles[thing.id] = nil
        do {
            try await engine.show(live: thing.id, what)
            guard gen == bringGen else { return }
            mounted.removeAll { $0 == thing.id }
            mounted.append(thing.id)
            front = thing.id
            cachedPreviewIDs.remove(thing.id)
            lastShown[thing.id] = pictures[thing.id] ?? lastShown[thing.id]
            states[thing.id] = .live
            engine.setSeen(true)
            while mounted.count > max(limit, 1), let oldest = mounted.first {
                mounted.removeFirst()
                await engine.close(live: oldest)
            }
        } catch TiloEngine.Problem.refused(let code, let reason) {
            guard gen == bringGen else { return }
            troubles[thing.id] = await offered(thing, refusedAs: code) ? .notShown : .notOffered
            states[thing.id] = .failed(reason)
        } catch TiloEngine.Problem.notOpen {
            guard gen == bringGen else { return }
            pendingBring = thing
            states[thing.id] = .offline
            troubles[thing.id] = nil
        } catch {
            guard gen == bringGen else { return }
            troubles[thing.id] = .notShown
            states[thing.id] = .failed("")
        }
        showing(id: thing.id, false)
        if bringing?.id == thing.id { bringing = nil }
    }

    /// How often a bring whose page answered nothing is looked at. A mount of a thing's app
    /// takes seconds, and the page's own word comes first while it is at work.
    private static let mountCheck: Duration = .seconds(2)

    /// A thing whose mount the page never answered: its place goes on saying so, and says what
    /// it can of it, with a way to ask again where another ask can be answered otherwise.
    /// `waited`: the page was asked, said that it was at work, and then said nothing more.
    private func mountFailed(_ id: LiveThing.ID, waited: Bool) {
        // Nothing waits for an ask that will not be answered.
        showing(id: id, false)
        bringingBy[id]?.cancel()
        bringingBy[id] = nil
        if bringing?.id == id { bringing = nil }
        // A thing shown already, or brought again since, is left as it is.
        guard states[id] == .loading else { return }
        // Tilo said why, and would say it again; or the page was simply never answered.
        let refused = (reports[id]?.refused ?? 0) > 0 && !waited
        troubles[id] = refused ? .notOffered : .notShown
        states[id] = .failed("")
    }

    /// Whether the person's Tilo offers what it just refused to show. A site older than views
    /// in messages answers that it knows no such action, and so does one that offers them
    /// while the Chat mounted for the view does not hold the message yet. They are told apart
    /// by what Chat's own page offers of the message: the action that opens its view, or none.
    private func offered(_ thing: LiveThing, refusedAs code: String) async -> Bool {
        guard code == "unknown_action", case .messageView(let message, _) = thing.content else { return true }
        let listed = (try? await engine.actions()) ?? []
        return listed.contains { action in
            guard action["view"]?["id"]?.string == "view", let opens = action["view"]?["open_action"]?.string else { return false }
            return opens == action["address"]?["action"]?.string
                && (action["address"]?["resource"]?.string ?? "").hasSuffix("/messages/\(message)")
        }
    }

    /// A thing nothing shows any more is taken down from Tilo's page: a stage put away, or come
    /// to show another thing. Left mounted, it would work in the person's one process with no place
    /// to be seen, and take one of the mounts `limit` holds for things that are.
    public func close(_ id: LiveThing.ID) async {
        if front == id, engine.view.window != nil,
           let picture = try? await engine.view.takeSnapshot(configuration: nil) {
            storeThumbnail(picture, for: id)
        }
        mounted.removeAll { $0 == id }
        lastShown[id] = nil
        showing(id: id, false)
        _ = waiting.remove(id)
        if bringing?.id == id { bringing = nil }
        bringingBy[id]?.cancel()
        bringingBy[id] = nil
        if front == id {
            front = nil
            engine.rest()
            engine.setSeen(false)
        }
        // Nothing of what it asked for stays: shown again, its own mount says how large its
        // content is now.
        await engine.close(live: id)
        sizes[id] = nil
        states[id] = nil
    }

    /// The thing leaves the front: it keeps the picture of how it looked, and the web view
    /// goes back to showing nothing.
    public func rest(_ id: LiveThing.ID) async {
        await rest(id, from: nil)
    }

    func rest(_ id: LiveThing.ID, from place: UUID?) async {
        if let place, let active = activePlace, active != place { return }
        guard front == id else { return }
        activePlace = nil
        showing(id: id, false)
        await sleep(id)
        engine.rest()
        engine.setSeen(false)
        home()
    }

    private func sleep(_ id: LiveThing.ID) async {
        if engine.view.window != nil, let picture = try? await engine.view.takeSnapshot(configuration: nil) {
            storeThumbnail(picture, for: id)
        }
        if front == id { front = nil }
        states[id] = .asleep
    }

    // MARK: Where the web view stands

    /// The place the web view rests in while nothing is in front (`EngineRest`).
    func rests(in place: LivePlatformView) {
        self.restPlace = place
        if front == nil { home() }
    }

    /// Put the web view into a live thing's place, filling it.
    ///
    /// It fills the place at every change of its size, not only when it is first put there: a
    /// page is handed a viewport when it is added and told of a new one only where its view
    /// changed size. A phone's stage shrinks for the software keyboard and grows again after
    /// it, and a Mac's window is dragged larger; a view left the size of the place it was first
    /// put in would draw the page into part of it, or into more than it holds.
    func stand(in place: LivePlatformView) {
        let view = engine.view
        #if os(iOS)
        // A page fills a person's place, and a person may be looking at it: nothing of the view's
        // own scrolling moves it under them, and a keyboard raised over it moves neither, so the
        // touch that lands is the page's own. The page that fills the place scrolls what it may
        // not fit.
        view.isUserInteractionEnabled = true
        view.scrollView.isScrollEnabled = false
        view.scrollView.bounces = false
        view.scrollView.contentInset = .zero
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.scrollView.keyboardDismissMode = .none
        place.isUserInteractionEnabled = true
        #endif
        if view.superview === place {
            if view.frame.size != place.bounds.size { view.frame = place.bounds }
            return
        }
        view.removeFromSuperview()
        view.frame = place.bounds
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        place.addSubview(view)
    }

    #if os(iOS)
    /// Tell the page how much of the bottom of the view a software keyboard covers: none while
    /// it is away.
    ///
    /// A page holds its own fields, and moving them into sight is the page's, as in every
    /// browser: with the keyboard up over the view it scrolls the field the person is in above
    /// it, and with it away it scrolls nothing back. That needs the keyboard's height, which a
    /// page in a system web view is told of only through the view's own scrolling: the view
    /// scrolls for nobody here, so it is said through the frame instead (`keyboardShown`).
    /// Nothing of it is a person's words, and nothing stays once the view is put away.
    public func keyboardShown(_ height: CGFloat) {
        let view = engine.view
        let inset = height.isFinite ? max(0, height) : 0
        view.callAsyncJavaScript(Self.keyboardScript,
                                 arguments: ["inset": Double(inset.rounded())], in: nil, in: .page,
                                 completionHandler: nil)
    }

    /// Safe to say as often as the system says the keyboard moved: the page moves nothing for a
    /// height it was told of already.
    ///
    /// Only a field of the page's moves: not a field the page has none of, and not a field of the
    /// app's own frame (a sheet's field, an editor outside the page). A page that draws its own
    /// text moves its own caret into sight and is only told to draw again.
    private static let keyboardScript = """
    (() => {
      const inset = Number(inset) || 0;
      const kept = globalThis.__tiloLiveKeyboard ??= {inset: 0, focused: null};
      if (!kept.listening) {
        kept.listening = true;
        const field = element => element?.closest?.('textarea, input, [contenteditable=\'true\'], [contenteditable=\'\']') ?? null;
        document.addEventListener('focusin', event => {
          const one = field(event.target);
          if (one) kept.focused = one;
        }, true);
        document.addEventListener('focusout', event => {
          if (kept.focused && event.target === kept.focused) kept.focused = null;
        }, true);
      }
      const was = kept.inset;
      kept.inset = inset;
      if (inset === was) return;
      const active = document.activeElement;
      const one = kept.focused && document.contains(kept.focused) ? kept.focused : field(active);
      if (!one) return;
      const here = document.activeElement;
      if (here !== one && here !== document.body && here !== document.documentElement
          && field(here) !== one && !one.contains(here)) return;
      // A field an app draws itself sits at the caret it paints: that app keeps the caret in
      // sight, and only draws again once the page is back at the size it has now.
      if (!one.isContentEditable && typeof one.scrollIntoView === 'function'
          && Number(getComputedStyle(one).opacity) <= 0.02) {
        window.dispatchEvent(new Event('resize'));
        return;
      }
      if (inset === 0) {
        one.scrollIntoView({block: 'nearest', inline: 'nearest', behavior: 'smooth'});
        return;
      }
      const box = one.getBoundingClientRect();
      const left = window.innerHeight - inset;
      // The gap a page leaves above a field it moves into sight: a line's height.
      const gap = Math.max(box.height, 24) + 12;
      if (box.bottom > left - 12) window.scrollBy({top: box.bottom - (left - gap), behavior: 'smooth'});
    })()
    """
    #endif

    /// Put away a keyboard that has nothing of the page's to type into: the person went to
    /// another view while a field of the page's was still in front, and one left up covers what
    /// they came to see.
    public func putKeyboardAway() {
        #if os(iOS)
        let view = engine.view
        // Only where no field of the page's took the response: a field's own keyboard is its own.
        guard view.isFirstResponder else { return }
        view.resignFirstResponder()
        #endif
    }

    /// Take the web view out of a place that goes away.
    func leaves(_ place: LivePlatformView) {
        if engine.view.superview === place { home() }
    }

    private func home() {
        let view = engine.view
        guard let restPlace else {
            #if os(macOS)
            // A Mac's web view works on its page in no window at all.
            view.removeFromSuperview()
            #endif
            return
        }
        // No place of a live thing holds it, so no field of the page's is being typed into: a
        // keyboard that is up has nothing to type into, and says so now rather than at the next
        // touch of whatever takes the place.
        #if os(iOS)
        if view.isFirstResponder { view.resignFirstResponder() }
        #endif
        guard view.superview !== restPlace else { return }
        view.removeFromSuperview()
        view.autoresizingMask = []
        view.frame = CGRect(x: 0, y: 0, width: restPlace.bounds.width == 0 ? 390 : restPlace.bounds.width,
                            height: restPlace.bounds.height == 0 ? 600 : restPlace.bounds.height)
        restPlace.addSubview(view)
    }
}

#if os(macOS)
private extension NSView.AutoresizingMask {
    static let flexibleWidth: NSView.AutoresizingMask = .width
    static let flexibleHeight: NSView.AutoresizingMask = .height
}
#endif

/// A native place that holds the engine's web view.
private struct Holder {
    let made: @MainActor (LivePlatformView) -> Void
    let gone: @MainActor (LivePlatformView) -> Void
}

#if os(macOS)
private struct HolderView: NSViewRepresentable {
    let holder: Holder

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        view.wantsLayer = true
        view.layer?.masksToBounds = true
        return view
    }

    func updateNSView(_ view: NSView, context: Context) { holder.made(view) }

    static func dismantleNSView(_ view: NSView, coordinator: Coordinator) { coordinator.gone(view) }

    func makeCoordinator() -> Holder { holder }
}
#else
private final class WindowedHolderView: UIView {
    var onWindow: ((UIView) -> Void)?

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil { onWindow?(self) }
    }
}

private struct HolderView: UIViewRepresentable {
    let holder: Holder

    func makeUIView(context: Context) -> UIView {
        let view = WindowedHolderView()
        view.clipsToBounds = true
        view.onWindow = holder.made
        holder.made(view)
        return view
    }

    func updateUIView(_ view: UIView, context: Context) {
        if let hosted = view as? WindowedHolderView { hosted.onWindow = holder.made }
        holder.made(view)
    }

    static func dismantleUIView(_ view: UIView, coordinator: Coordinator) { coordinator.gone(view) }

    func makeCoordinator() -> Holder { holder }
}

#endif

#if os(iOS)
/// How much of the bottom of a window a software keyboard covers: the keyboard's own frame, as
/// the system says it, and nothing else.
@MainActor
private func covered(_ frame: CGRect, of window: UIWindow) -> CGFloat {
    let box = window.bounds
    guard frame.width > 0, frame.height > 0, box.width > 0, box.height > 0 else { return 0 }
    let covered = box.maxY - frame.minY
    guard covered.isFinite, covered > 0 else { return 0 }
    return min(covered, box.height)
}

/// A place a page stands in, while a person works in it and the software keyboard is up over it.
///
/// The system lays the keyboard over this place, and the place keeps its whole size: nothing is
/// shrunk and grown again under a person's finger while they type, and what their place draws
/// over the page keeps the place it is drawn in. What moves is only how much of the bottom a
/// keyboard covers, said to the page as often as the system says the keyboard moved
/// (`LiveStage.keyboardShown`), so that a field the person is in comes into sight above it.
private struct TypingPlace: UIViewRepresentable {
    let typing: @MainActor (CGFloat) -> Void

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.isUserInteractionEnabled = false
        view.clipsToBounds = true
        let coordinator = context.coordinator
        let center = NotificationCenter.default
        coordinator.watching = [
            center.addObserver(forName: UIResponder.keyboardWillChangeFrameNotification, object: nil, queue: .main) { note in
                let frame = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue
                Task { @MainActor in
                    guard let window = view.window else { return }
                    coordinator.say(covered(frame ?? .zero, of: window), in: window)
                }
            },
            center.addObserver(forName: UIResponder.keyboardWillHideNotification, object: nil, queue: .main) { _ in
                Task { @MainActor in
                    guard let window = view.window else { return }
                    coordinator.say(0, in: window)
                }
            },
        ]
        return view
    }

    /// A place that was put into another window, or into none, hears the keyboard where it stands
    /// now: the frame may be gathering again for a key pressed in a field of its own.
    func updateUIView(_ view: UIView, context: Context) {
        guard let window = view.window else { return }
        context.coordinator.say(covered(TypingPlace.keyboard(in: window), of: window), in: window)
    }

    /// Where the keyboard stands in a window now: the system's own window for it, which is no
    /// window of the app's, and nothing where it is not up.
    @MainActor
    private static func keyboard(in window: UIWindow) -> CGRect {
        let keyboard = "UIRemoteKeyboardWindow"
        for one in window.windowScene?.windows ?? [] where String(describing: type(of: one)) == keyboard {
            return one.frame
        }
        return .zero
    }

    static func dismantleUIView(_ view: UIView, coordinator: Coordinator) { coordinator.stop() }

    func makeCoordinator() -> Coordinator { Coordinator(typing: typing) }

    @MainActor
    final class Coordinator {
        /// The window the keyboard was last said for, and its height: said again only where one
        /// of the two moves.
        private var said: (window: ObjectIdentifier, height: CGFloat)?
        var watching: [NSObjectProtocol] = []

        private let typed: @MainActor (CGFloat) -> Void

        init(typing: @escaping @MainActor (CGFloat) -> Void) {
            typed = typing
        }

        func say(_ height: CGFloat, in window: UIWindow) {
            let height = height.isFinite ? max(0, height.rounded()) : 0
            let here = ObjectIdentifier(window)
            if let said, said.window == here, said.height == height { return }
            said = (here, height)
            typed(height)
        }

        func stop() {
            for one in watching { NotificationCenter.default.removeObserver(one) }
            watching = []
        }
    }
}

#endif

/// Where the engine's web view rests while no live thing is shown. An app puts one of these
/// into its root view: on an iPhone a web view works on its page only while it stands in a window.
/// It takes no area, shows nothing and takes no touch.
///
/// The software keyboard coming and going changes nothing of where it rests: the engine's page
/// holds the person's session with their workspace, and a root view gathered again for every
/// key pressed would carry the live page with it.
public struct EngineRest: View {
    private let stage: LiveStage

    public init(stage: LiveStage) {
        self.stage = stage
    }

    public var body: some View {
        #if os(iOS)
        resting.ignoresSafeArea(.keyboard, edges: .bottom)
        #else
        resting
        #endif
    }

    private var resting: some View {
        HolderView(holder: Holder(made: { [stage] view in stage.rests(in: view) }, gone: { _ in }))
            .frame(width: 1, height: 1)
            .opacity(0.02)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

/// One live Tilo thing, drawn by Tilo's own runtime, in a native view.
///
/// In a message it is `compact`: it keeps its shape while allowing direct editing and
/// interaction with the live app inside it. When the person hops in it is `working`: it fills
/// the area it is given. Both are the same resource and the same mount: nothing loads again
/// between them.
public struct TiloLiveView: View {
    public enum Size: Sendable {
        case compact
        case working
    }

    @ObservedObject private var stage: LiveStage
    @State private var placeID = UUID()
    private let thing: LiveThing
    private let size: Size
    private let openInTilo: (() -> Void)?
    private let says: Says?

    /// The app's own sentence for why a thing is not shown, in its person's language.
    public typealias Says = @MainActor (LiveStage.Trouble) -> String

    /// - Parameters:
    ///   - openInTilo: opens the same thing in Tilo itself. It is offered when the thing cannot
    ///     be shown here.
    ///   - says: the app's sentences for a thing that is not shown. Without them the place
    ///     says this package's own.
    public init(_ thing: LiveThing, on stage: LiveStage, size: Size = .compact, openInTilo: (() -> Void)? = nil,
                says: Says? = nil) {
        self.thing = thing
        self.stage = stage
        self.size = size
        self.openInTilo = openInTilo
        self.says = says
    }

    /// What a place says of a thing that could not be shown, and whether it offers to ask again.
    struct Unshown: Equatable {
        var sentence: String
        var asksAgain: Bool
    }

    /// The words under the name of a thing that could not be shown: the app's sentence for
    /// why, or this package's own, and never Tilo's refusal as it came. Asking again is
    /// offered only where another ask can be answered otherwise.
    @MainActor static func unshown(_ thing: LiveThing, on stage: LiveStage, says: Says?) -> Unshown? {
        guard case .failed = stage.state(of: thing) else { return nil }
        let trouble = stage.troubles[thing.id] ?? .notShown
        let own = trouble == .notOffered ? LiveWords.notOffered : LiveWords.failed
        return Unshown(sentence: says?(trouble) ?? own.said, asksAgain: trouble == .notShown)
    }

    /// Whether the place keeps a page's shape now: while the page shows, while its picture
    /// stands for it, and while the page comes. A thing that is not shown says so at the height
    /// of its words.
    private var keepsShape: Bool {
        if size == .working || stage.picture(of: thing) != nil { return true }
        switch stage.state(of: thing) {
        case .live, .loading: return true
        // Nothing is in front, so this one is brought at once. A thing whose place was wanted,
        // and whose page is not there to bring it, keeps the shape it has, so that its words
        // are read where the page would be and not in a line under the thing's name.
        case .asleep: return stage.front == nil || stage.isShowing(thing)
        case .offline: return stage.isShowing(thing) || stage.front == thing.id
        case .failed: return false
        }
    }

    public var body: some View {
        let shows = stage.front == thing.id && stage.state(of: thing) == .live
        let shaped = keepsShape
        let picture = stage.picture(of: thing)
        ZStack {
            // A page and its picture stand on a ground of their own. Words alone stand on the
            // ground of whoever holds the place: one tone, no ground under them and no box in it.
            if shows || picture != nil { pageGround }
            if shows {
                #if os(iOS)
                // A person works in this one with the software keyboard up, and the area it
                // stands in stays the area it is: the page is not shrunk and grown again under
                // their finger while they type, and what their place draws over it — a tab bar
                // at the foot of a sheet — keeps its place above the keyboard and is never
                // covered. The page moves its own text field into the part of the view the
                // keyboard leaves, as a browser does with a page, told the keyboard's height by
                // the frame (`keyboardShown`).
                if size == .working {
                    page.typing { [stage] height in stage.keyboardShown(height) }
                        .ignoresSafeArea(.keyboard, edges: .bottom)
                } else {
                    page
                }
                #else
                page
                #endif
            } else {
                resting
            }
        }
        .modifier(PageShape(keeps: shaped, ratio: size == .compact && asked == nil ? 1.0 : nil))
        .frame(height: shaped && size == .compact ? asked : nil)
        .clipShape(RoundedRectangle(cornerRadius: shaped && size == .compact ? 10 : 0, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(thing.title)
        .task(id: "\(thing.id) \(size)") {
            // A thing the person works in is always the one in front. One that is only looked
            // at comes to the front when nothing else is there.
            if size == .working || stage.front == nil { await bring() }
        }
        .onDisappear {
            Task { await stage.rest(thing.id, from: placeID) }
        }
    }

    /// Bring the thing to the front immediately for this place.
    private func bring() async {
        await stage.bring(thing, for: placeID)
    }

    /// The place the engine's web view stands in, which the live thing is drawn in.
    private var page: some View {
        HolderView(holder: Holder(made: { [stage] view in stage.stand(in: view) },
                                  gone: { [stage] view in stage.leaves(view) }))
    }

    /// The ground a page and its picture stand on. Where a person works in the page it goes on
    /// under the keyboard as well, so the part of the page the keyboard comes over is the page's
    /// own ground and no edge of the frame's shows through where the keyboard ends.
    private var pageGround: some View {
        Rectangle()
            .fill(.background.secondary)
            #if os(iOS)
            // Only where a person works in the page: the sheet that holds it goes on under the
            // keyboard, and so does its ground.
            .modifier(GoesUnderKeyboard(mostly: size == .working))
            #endif
    }

    /// How tall a message's view stands in its message: what its content asked for, as Tilo's
    /// page said it. Until it has, a few lines' height. Nil for a thing that keeps a shape, and
    /// for a page a person works in: what they work in fills its area.
    private var asked: CGFloat? {
        guard size == .compact, case .messageView = thing.content else { return nil }
        return stage.sizes[thing.id].map { min(max($0.height, 44), 1200) } ?? 132
    }

    @ViewBuilder private var resting: some View {
        let state = stage.state(of: thing)
        // What the thing showed the last time it was in front, and what the page showed a moment
        // before: a thing that is switched in says what it said last while its own page comes,
        // and never nothing at all.
        if let picture = stage.picture(of: thing) {
            let showsConnectingPill = state == .loading
                || (state == .offline && (stage.isPending(thing) || stage.isShowingCachedPreview(of: thing)))
                || (state == .asleep && (size == .working || stage.isShowingCachedPreview(of: thing)))
            // GeometryReader keeps the image strictly bounded so it never stretches vertically.
            GeometryReader { geo in
                ZStack(alignment: showsConnectingPill ? .bottomTrailing : .center) {
                    #if os(macOS)
                    Image(nsImage: picture)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(width: geo.size.width, height: geo.size.height)
                        .clipped()
                    #else
                    Image(uiImage: picture)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(width: geo.size.width, height: geo.size.height)
                        .clipped()
                    #endif
                    if showsConnectingPill {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.mini)
                            Text(LiveWords.cachedConnecting.said)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(.regularMaterial, in: Capsule())
                        .padding(12)
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel(LiveWords.cachedConnecting.said.replacingOccurrences(of: " · ", with: ", "))
                    } else {
                        said(state, inTheMiddle: true)
                            .padding(12)
                            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                            .padding(12)
                    }
                }
            }
        } else {
            // In the middle of a page's shape while the page comes; else from the leading edge,
            // as high as the words are.
            let middle = keepsShape
            said(state, inTheMiddle: middle)
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .frame(maxWidth: .infinity, maxHeight: middle ? .infinity : nil, alignment: middle ? .center : .leading)
        }
    }

    /// The thing's name and what there is to say or do about it. The name takes the type of the
    /// place it stands in, a weight heavier.
    private func said(_ state: LiveStage.State, inTheMiddle: Bool) -> some View {
        // A thing a person works in fills its area, and the area already says what it is called:
        // its name is not written over it again, where what stands there is only a word about it.
        let titled: Bool = switch state {
        case .offline, .failed: true
        case .asleep, .loading, .live: size != .working
        }
        return VStack(alignment: inTheMiddle ? .center : .leading, spacing: 8) {
            if titled {
                Text(Self.plain(thing.title)).fontWeight(.semibold).lineLimit(2).multilineTextAlignment(inTheMiddle ? .center : .leading)
            }
            switch state {
            case .loading:
                // While the place has one, its shape says that the thing is on its way in in a
                // line: the thing's name alone, and this beside it. One spinner stands where the
                // place has no shape to show yet.
                if keepsShape, stage.state(of: thing) == .loading {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(LiveWords.loading.said).foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(LiveWords.loading.said)
                } else {
                    ProgressView().accessibilityLabel(LiveWords.loading.said)
                }
            case .asleep, .live:
                Button(LiveWords.showLive.said) { Task { await bring() } }
            case .offline:
                Text(LiveWords.offline.said).foregroundStyle(.secondary)
                    .multilineTextAlignment(inTheMiddle ? .center : .leading)
                Button(LiveWords.tryAgain.said) { Task { await bring() } }
            case .failed:
                if let unshown = Self.unshown(thing, on: stage, says: says) {
                    Text(unshown.sentence).foregroundStyle(.secondary).multilineTextAlignment(inTheMiddle ? .center : .leading)
                    if unshown.asksAgain { Button(LiveWords.tryAgain.said) { Task { await bring() } } }
                }
            }
            if let openInTilo, state != .loading, state != .asleep {
                Button(LiveWords.openInTilo.said, action: openInTilo)
            }
        }
    }

    private static func plain(_ raw: String) -> String {
        raw.replacingOccurrences(of: "**", with: "")
            .replacingOccurrences(of: "__", with: "")
            .replacingOccurrences(of: "`", with: "")
            .replacingOccurrences(of: "*", with: "")
    }
}

#if os(iOS)
/// Whether a place goes on under the software keyboard, rather than being gathered above it.
private struct GoesUnderKeyboard: ViewModifier {
    let mostly: Bool

    func body(content: Content) -> some View {
        if mostly { content.ignoresSafeArea(.keyboard, edges: .bottom) } else { content }
    }
}

private extension View {
    /// Keep the keyboard's height for the page this place holds, in a place a person works in.
    ///
    /// The place stays drawn over the page, of no size and taking no touch: it is only there to
    /// hear the system say where the keyboard is, which a place laid over the page is the one to
    /// hear. The page is told as often as the keyboard moves, and moves its own field itself.
    func typing(_ typing: @escaping @MainActor (CGFloat) -> Void) -> some View {
        ZStack {
            self
            TypingPlace(typing: typing)
                .frame(width: 0, height: 0)
                .allowsHitTesting(false)
                .accessibilityHidden(true)
        }
    }
}
#endif

/// A page's shape for a place that keeps one: sixteen to ten, or the page's own where the
/// place is given a height. A place that only says words keeps none: its words take the width
/// they are given and begin at its leading edge.
private struct PageShape: ViewModifier {
    let keeps: Bool
    let ratio: CGFloat?

    func body(content: Content) -> some View {
        if keeps, let ratio { content.aspectRatio(ratio, contentMode: .fit) } else { content }
    }
}
