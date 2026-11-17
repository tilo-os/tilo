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
/// a view written in the app language.
public struct LiveThing: Hashable, Sendable, Identifiable {
    public enum Content: Hashable, Sendable {
        /// This app on this record.
        case record(app: String, record: String)
        /// A view written in the app language, by its source. Tilo's page cannot run one yet.
        case writtenView(source: String)
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
    /// names a record's own actions, or `view:<source>`. Nil for anything else.
    public init?(id: String, title: String, reference: String) {
        if reference.hasPrefix("view:") {
            self.init(id: id, title: title, .writtenView(source: String(reference.dropFirst("view:".count))))
            return
        }
        let parts = reference.split(separator: ":", maxSplits: 2, omittingEmptySubsequences: false).map(String.init)
        guard parts.count == 3, parts[0] == "record", !parts[1].isEmpty, !parts[2].isEmpty else { return nil }
        self.init(id: id, title: title, .record(app: parts[1], record: parts[2]))
    }

    public var reference: String {
        switch content {
        case .record(let app, let record): "record:\(app):\(record)"
        case .writtenView(let source): "view:\(source)"
        }
    }
}

/// The sentences a live thing's place says. An app's own catalogue translates them under
/// these keys; without one they read as written here.
enum LiveWords: String {
    case loading = "live-loading"
    case offline = "live-offline"
    case failed = "live-failed"
    case notYet = "live-written-view-not-yet"
    case showLive = "live-show"
    case openInTilo = "live-open-in-tilo"
    case tryAgain = "live-try-again"

    var english: String {
        switch self {
        case .loading: "Loading"
        case .offline: "This device is offline"
        case .failed: "Tilo could not show this"
        case .notYet: "Tilo cannot show a written view here yet"
        case .showLive: "Show live"
        case .openInTilo: "Open in Tilo"
        case .tryAgain: "Try again"
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
        /// Tilo's own sentence, or none.
        case failed(String)
        /// A kind of thing Tilo's page cannot show yet.
        case notYet
    }

    public let engine: TiloEngine
    @Published public private(set) var front: LiveThing.ID?
    @Published public private(set) var states: [LiveThing.ID: State] = [:]
    @Published public private(set) var pictures: [LiveThing.ID: LivePlatformImage] = [:]
    public var limit = 3

    /// The mounted things, the one longest away first.
    private var mounted: [LiveThing.ID] = []
    private weak var room: LivePlatformView?
    private var listening: Task<Void, Never>?

    public init(engine: TiloEngine) {
        self.engine = engine
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

    deinit { listening?.cancel() }

    public func state(of thing: LiveThing) -> State { states[thing.id] ?? .asleep }

    /// Bring a live thing to the front: the one web view shows it and takes the person's input.
    public func bring(_ thing: LiveThing) async {
        guard case .record(let app, let record) = thing.content else {
            states[thing.id] = .notYet
            return
        }
        guard case .open = engine.state else {
            states[thing.id] = .offline
            return
        }
        if let before = front, before != thing.id { await sleep(before) }
        states[thing.id] = .loading
        do {
            try await engine.show(live: thing.id, .record(app: app, record: record))
            mounted.removeAll { $0 == thing.id }
            mounted.append(thing.id)
            front = thing.id
            states[thing.id] = .live
            engine.setSeen(true)
            while mounted.count > max(limit, 1), let oldest = mounted.first {
                mounted.removeFirst()
                await engine.close(live: oldest)
            }
        } catch TiloEngine.Problem.refused(_, let reason) {
            states[thing.id] = .failed(reason)
        } catch TiloEngine.Problem.notOpen {
            states[thing.id] = .offline
        } catch {
            states[thing.id] = .failed("")
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
        guard view.superview !== place else { return }
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
/// In a message it is `compact`: it is looked at, keeps its shape, and takes no touch, so the
/// conversation scrolls over it like over a picture. When the person hops in it is `working`:
/// it fills the room it is given, and the keyboard and the pointer are its own. Both are the
/// same record and the same mount: nothing loads again between them.
public struct TiloLiveView: View {
    public enum Size: Sendable {
        case compact
        case working
    }

    @ObservedObject private var stage: LiveStage
    private let thing: LiveThing
    private let size: Size
    private let openInTilo: (() -> Void)?

    /// - Parameter openInTilo: opens the same thing in Tilo itself. It is offered when the
    ///   thing cannot be shown here.
    public init(_ thing: LiveThing, on stage: LiveStage, size: Size = .compact, openInTilo: (() -> Void)? = nil) {
        self.thing = thing
        self.stage = stage
        self.size = size
        self.openInTilo = openInTilo
    }

    public var body: some View {
        ZStack {
            Rectangle().fill(.background.secondary)
            if stage.front == thing.id, stage.state(of: thing) == .live {
                HolderView(holder: Holder(made: { [stage] view in stage.stand(in: view) },
                                          gone: { [stage] view in stage.leaves(view) }))
                    .allowsHitTesting(size == .working)
            } else {
                resting
            }
        }
        .aspectRatio(size == .compact ? 16.0 / 10.0 : nil, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: size == .compact ? 10 : 0, style: .continuous))
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

    @ViewBuilder private var resting: some View {
        let state = stage.state(of: thing)
        ZStack {
            if let picture = stage.pictures[thing.id] {
                #if os(macOS)
                Image(nsImage: picture).resizable().aspectRatio(contentMode: .fill)
                #else
                Image(uiImage: picture).resizable().aspectRatio(contentMode: .fill)
                #endif
            }
            VStack(spacing: 8) {
                Text(thing.title).font(.headline).lineLimit(2).multilineTextAlignment(.center)
                switch state {
                case .loading:
                    ProgressView().accessibilityLabel(LiveWords.loading.said)
                case .asleep, .live:
                    Button(LiveWords.showLive.said) { Task { await stage.bring(thing) } }
                case .offline:
                    Text(LiveWords.offline.said).foregroundStyle(.secondary)
                    Button(LiveWords.tryAgain.said) { Task { await stage.bring(thing) } }
                case .failed(let why):
                    Text(why.isEmpty ? LiveWords.failed.said : why).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    Button(LiveWords.tryAgain.said) { Task { await stage.bring(thing) } }
                case .notYet:
                    Text(LiveWords.notYet.said).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
                if let openInTilo, state != .loading, state != .asleep {
                    Button(LiveWords.openInTilo.said, action: openInTilo)
                }
            }
            .padding(12)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .padding(12)
        }
    }
}
