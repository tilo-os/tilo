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

/// One live Tilo thing a native view shows: an app on a record of the person's workspace, or
/// the view a message carries.
public struct LiveThing: Hashable, Sendable, Identifiable {
    public enum Content: Hashable, Sendable {
        /// This app on this record.
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

    /// From the words a conversation's model carries for it: `record:<app>:<record>`, as Tilo
    /// names a record's own actions, or `view:<message>:<conversation>`. Nil for anything else.
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

    var english: String {
        switch self {
        case .loading: "Loading"
        case .offline: "This device is offline"
        case .failed: "Tilo could not show this"
        case .showLive: "Show live"
        case .openInTilo: "Open in Tilo"
        case .tryAgain: "Try again"
        case .notOffered: "This Tilo cannot show this yet"
        }
    }

    var said: String { Bundle.main.localizedString(forKey: rawValue, value: english, table: nil) }
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
    /// The size each thing's content asks for, as its mount in Tilo's page said it last.
    @Published public private(set) var sizes: [LiveThing.ID: CGSize] = [:]
    public var limit = 3

    /// The mounted things, the one longest away first.
    private var mounted: [LiveThing.ID] = []
    private weak var room: LivePlatformView?
    private var listening: Task<Void, Never>?
    private var sizing: Task<Void, Never>?

    public init(engine: TiloEngine) {
        self.engine = engine
        sizing = Task { [weak self] in
            for await (id, size) in engine.contentSizes() {
                guard let self else { return }
                if self.sizes[id] != size { self.sizes[id] = size }
            }
        }
        listening = Task { [weak self] in
            for await state in engine.states() {
                guard let self else { return }
                if case .open = state { continue }
                // The page left: whatever it had mounted went with it.
                self.mounted.removeAll()
                if let front = self.front {
                    self.states[front] = .offline
                    self.front = nil
                }
            }
        }
    }

    deinit {
        listening?.cancel()
        sizing?.cancel()
    }

    public func state(of thing: LiveThing) -> State { states[thing.id] ?? .asleep }

    /// Bring a live thing to the front: the one web view shows it and takes the person's input.
    public func bring(_ thing: LiveThing) async {
        let what: TiloEngine.Mounted = switch thing.content {
        case .record(let app, let record): .record(app: app, record: record)
        case .messageView(let message, let conversation): .messageView(message: message, conversation: conversation)
        }
        guard case .open = engine.state else {
            states[thing.id] = .offline
            return
        }
        if let before = front, before != thing.id { await sleep(before) }
        states[thing.id] = .loading
        troubles[thing.id] = nil
        do {
            try await engine.show(live: thing.id, what)
            mounted.removeAll { $0 == thing.id }
            mounted.append(thing.id)
            front = thing.id
            states[thing.id] = .live
            engine.setSeen(true)
            while mounted.count > max(limit, 1), let oldest = mounted.first {
                mounted.removeFirst()
                await engine.close(live: oldest)
            }
        } catch TiloEngine.Problem.refused(let code, let reason) {
            troubles[thing.id] = await offered(thing, refusedAs: code) ? .notShown : .notOffered
            states[thing.id] = .failed(reason)
        } catch TiloEngine.Problem.notOpen {
            states[thing.id] = .offline
        } catch {
            troubles[thing.id] = .notShown
            states[thing.id] = .failed("")
        }
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

    /// The thing leaves the front: it keeps the picture of how it looked, and the web view
    /// goes back to showing nothing.
    public func rest(_ id: LiveThing.ID) async {
        guard front == id else { return }
        await sleep(id)
        engine.rest()
        engine.setSeen(false)
        home()
    }

    private func sleep(_ id: LiveThing.ID) async {
        if engine.view.window != nil, let picture = try? await engine.view.takeSnapshot(configuration: nil) {
            pictures[id] = picture
        }
        if front == id { front = nil }
        states[id] = .asleep
    }

    // MARK: Where the web view stands

    /// The place the web view rests in while nothing is in front (`EngineRoom`).
    func rests(in room: LivePlatformView) {
        self.room = room
        if front == nil { home() }
    }

    /// Put the web view into a live thing's place, filling it.
    func stand(in place: LivePlatformView) {
        let view = engine.view
        if view.superview === place {
            if view.frame != place.bounds { view.frame = place.bounds }
            return
        }
        view.removeFromSuperview()
        view.frame = place.bounds
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        place.addSubview(view)
    }

    /// Take the web view out of a place that goes away.
    func leaves(_ place: LivePlatformView) {
        if engine.view.superview === place { home() }
    }

    private func home() {
        let view = engine.view
        guard let room else {
            #if os(macOS)
            // A Mac's web view runs its page in no window at all.
            view.removeFromSuperview()
            #endif
            return
        }
        guard view.superview !== room else { return }
        view.removeFromSuperview()
        view.autoresizingMask = []
        view.frame = CGRect(x: 0, y: 0, width: 390, height: 600)
        room.addSubview(view)
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
private struct HolderView: UIViewRepresentable {
    let holder: Holder

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.clipsToBounds = true
        return view
    }

    func updateUIView(_ view: UIView, context: Context) { holder.made(view) }

    static func dismantleUIView(_ view: UIView, coordinator: Coordinator) { coordinator.gone(view) }

    func makeCoordinator() -> Holder { holder }
}
#endif

/// Where the engine's web view rests while no live thing is shown. An app puts one of these
/// into its root view: on an iPhone a web view runs its page only while it stands in a window.
/// It takes no room, shows nothing and takes no touch.
public struct EngineRoom: View {
    private let stage: LiveStage

    public init(stage: LiveStage) {
        self.stage = stage
    }

    public var body: some View {
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
/// the room it is given. Both are the same record and the same mount: nothing loads again
/// between them.
public struct TiloLiveView: View {
    public enum Size: Sendable {
        case compact
        case working
    }

    @ObservedObject private var stage: LiveStage
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
        if size == .working || stage.pictures[thing.id] != nil { return true }
        switch stage.state(of: thing) {
        case .live, .loading: return true
        // Nothing is in front, so this one is brought at once.
        case .asleep: return stage.front == nil
        case .offline, .failed: return false
        }
    }

    public var body: some View {
        let shows = stage.front == thing.id && stage.state(of: thing) == .live
        let shaped = keepsShape
        ZStack {
            // A page and its picture stand on a ground of their own. Words alone stand on the
            // ground of whoever holds the place: one tone, no ground under them and no box in it.
            if shows || stage.pictures[thing.id] != nil { Rectangle().fill(.background.secondary) }
            if shows {
                HolderView(holder: Holder(made: { [stage] view in stage.stand(in: view) },
                                          gone: { [stage] view in stage.leaves(view) }))
            } else {
                resting
            }
        }
        .modifier(PageShape(keeps: shaped, ratio: size == .compact && asked == nil ? 6.0 / 5.0 : nil))
        .frame(height: shaped && size == .compact ? asked : nil)
        .clipShape(RoundedRectangle(cornerRadius: shaped && size == .compact ? 10 : 0, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(thing.title)
        .task(id: "\(thing.id) \(size)") {
            // A thing the person works in is always the one in front. One that is only looked
            // at comes to the front when nothing else is there.
            if size == .working || stage.front == nil { await stage.bring(thing) }
        }
        .onDisappear {
            Task { await stage.rest(thing.id) }
        }
    }

    /// How tall a message's view stands in its message: what its content asked for, as Tilo's
    /// page said it. Until it has, a few lines' height. Nil for a thing that keeps a shape.
    private var asked: CGFloat? {
        guard case .messageView = thing.content else { return nil }
        return stage.sizes[thing.id].map { min(max($0.height, 44), 1200) } ?? 132
    }

    @ViewBuilder private var resting: some View {
        let state = stage.state(of: thing)
        if let picture = stage.pictures[thing.id] {
            // Over the picture of how the thing looked, its words stand in a box of their own.
            ZStack {
                #if os(macOS)
                Image(nsImage: picture).resizable().aspectRatio(contentMode: .fill)
                #else
                Image(uiImage: picture).resizable().aspectRatio(contentMode: .fill)
                #endif
                said(state, inTheMiddle: true)
                    .padding(12)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .padding(12)
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
        VStack(alignment: inTheMiddle ? .center : .leading, spacing: 8) {
            Text(thing.title).fontWeight(.semibold).lineLimit(2).multilineTextAlignment(inTheMiddle ? .center : .leading)
            switch state {
            case .loading:
                ProgressView().accessibilityLabel(LiveWords.loading.said)
            case .asleep, .live:
                Button(LiveWords.showLive.said) { Task { await stage.bring(thing) } }
            case .offline:
                Text(LiveWords.offline.said).foregroundStyle(.secondary)
                Button(LiveWords.tryAgain.said) { Task { await stage.bring(thing) } }
            case .failed:
                if let unshown = Self.unshown(thing, on: stage, says: says) {
                    Text(unshown.sentence).foregroundStyle(.secondary).multilineTextAlignment(inTheMiddle ? .center : .leading)
                    if unshown.asksAgain { Button(LiveWords.tryAgain.said) { Task { await stage.bring(thing) } } }
                }
            }
            if let openInTilo, state != .loading, state != .asleep {
                Button(LiveWords.openInTilo.said, action: openInTilo)
            }
        }
    }
}

/// A page's shape for a place that keeps one: sixteen to ten, or the page's own where the
/// place is given a height. A place that only says words keeps none: its words take the width
/// they are given and begin at its leading edge.
private struct PageShape: ViewModifier {
    let keeps: Bool
    let ratio: CGFloat?

    func body(content: Content) -> some View {
        if keeps { content.aspectRatio(ratio, contentMode: .fit) } else { content }
    }
}
