import Foundation

/// What a part of the app shows of the workspace the engine is in, named as the workspace's own
/// feed of changes names it: an app, and one document the app keeps there or all of them.
public struct Followed: Hashable, Sendable {
    /// The app that keeps it, by its id (`so.tilo.tasks`).
    public let app: String
    /// One document of the app, by the id the workspace knows it under. Nil for everything the
    /// app keeps in the workspace: its own document and the parts beside it.
    public let document: String?

    public init(app: String, document: String? = nil) {
        self.app = app
        self.document = document
    }

    /// Whether word that `changed` changed is word about this.
    public func covers(_ changed: Followed) -> Bool {
        app == changed.app && (document == nil || document == changed.document)
    }
}

extension TiloEngine {
    /// Which of `things` changed in the workspace since `cursor`, as the entry of the page's
    /// device says it (its `changed`): their names and where to ask on from.
    /// It says that something is to be read again and nothing of what it reads as, and the
    /// workspace's server names a thing only to a person who may read it.
    ///
    /// With no cursor nothing that happened yet is news: the answer names nothing and gives the
    /// cursor to ask on from. With `waiting`, the page holds the ask until one of the things
    /// changed, ten seconds at most.
    ///
    /// - Throws: `Problem.refused` with `unknown_action` for a page that tells nothing this
    ///   way, and with `unavailable` for one that hands a native app no entry of its device.
    public func changed(of things: [Followed], after cursor: String? = nil,
                        waiting: Duration? = nil) async throws -> (changed: [Followed], cursor: String) {
        let named: [[String: Any]] = things.map { thing in
            var one: [String: Any] = ["app": thing.app]
            if let document = thing.document { one["document"] = document }
            return one
        }
        let held = waiting.map { Double($0.components.seconds) * 1000 + Double($0.components.attoseconds) / 1e15 } ?? 0
        let answer = try await ask("return globalThis.__tiloLive.followed(given, of, after, waitMs)",
                                   ["given": pass, "of": named, "after": cursor.map { $0 as Any } ?? NSNull(),
                                    "waitMs": min(max(held, 0), 10000)])
        guard let next = answer["cursor"]?.string, !next.isEmpty else { throw Problem.noAnswer }
        let told = (answer["changed"]?.array ?? []).compactMap { one -> Followed? in
            guard let app = one["app"]?.string, let document = one["document"]?.string else { return nil }
            return Followed(app: app, document: document)
        }
        return (told, next)
    }
}

/// The one place that follows what the app's parts show. A part says what it shows and how it
/// reads it again; the follower holds one ask of the page for all of them, and at the page's
/// word that a thing changed it has every part that shows the thing read again. It keeps
/// nothing of what a part shows: the read goes through the resources the part reads at any
/// other moment.
///
/// One ask is out at a time. The page holds it until something that is followed changed, ten
/// seconds at most, and it is asked again when it ends. An ask that is not answered is made
/// again after a pause that grows. A page that tells nothing this way is not asked again
/// before the device gets in anew: the parts then read at the moments they always read at.
@MainActor
public final class ChangeFollower {
    private let engine: TiloEngine
    private var parts: [UUID: (thing: Followed, readAgain: @MainActor () async -> Void)] = [:]
    private var asking = false
    /// This page tells nothing this way.
    public private(set) var isRefused = false
    /// Where the workspace's feed was read up to on this way in. Nil before the first answer.
    private var cursor: String?
    /// Moves when what is followed changes: an ask that was out meanwhile named the things of
    /// before, so what it answered is asked anew for the things of now.
    private var followed = 0
    private var listening: Task<Void, Never>?
    /// How long an ask that failed waits before it is made again, the first time. The wait
    /// doubles each time, up to a minute.
    var firstPause: Duration = .seconds(1)
    /// How long the page is asked to hold one ask.
    var hold: Duration = .seconds(10)

    public init(engine: TiloEngine) {
        self.engine = engine
        listening = Task { [weak self, engine] in
            for await state in engine.states() {
                guard let self else { return }
                guard case .open = state else { continue }
                // A device that got in anew is asked anew, and from now: its parts read whole
                // on the way in.
                self.isRefused = false
                self.cursor = nil
                self.begin()
            }
        }
    }

    deinit {
        listening?.cancel()
    }

    /// Follow `thing`: `readAgain` is called each time the page says that it changed. What
    /// comes back ends the following (`unfollow`).
    @discardableResult
    public func follow(_ thing: Followed, _ readAgain: @escaping @MainActor () async -> Void) -> UUID {
        let id = UUID()
        parts[id] = (thing, readAgain)
        followed += 1
        begin()
        return id
    }

    public func unfollow(_ id: UUID) {
        guard parts.removeValue(forKey: id) != nil else { return }
        followed += 1
    }

    private func begin() {
        guard !asking, !isRefused, !parts.isEmpty, case .open = engine.state else { return }
        asking = true
        Task { [weak self] in
            var pause = self?.firstPause ?? .seconds(1)
            var quick = 0
            while let self, !self.isRefused, !self.parts.isEmpty, case .open = self.engine.state {
                let things = Array(Set(self.parts.values.map(\.thing)))
                let from = self.cursor
                let before = self.followed
                let asked = ContinuousClock.now
                do {
                    let told = try await self.engine.changed(of: things, after: from, waiting: from == nil ? nil : self.hold)
                    pause = self.firstPause
                    // What is followed changed while the ask was out: it is asked anew from
                    // where it began, for the things of now.
                    if from != nil, before != self.followed { continue }
                    self.cursor = told.cursor
                    if told.changed.isEmpty {
                        // A page that answers at once with nothing, again and again, holds no
                        // ask: it is not asked in a circle.
                        quick = from != nil && ContinuousClock.now - asked < .milliseconds(100) ? quick + 1 : 0
                        if quick >= 3 { self.isRefused = true }
                        continue
                    }
                    quick = 0
                    for part in Array(self.parts.values) where told.changed.contains(where: { part.thing.covers($0) }) {
                        await part.readAgain()
                    }
                } catch TiloEngine.Problem.refused(let code, _) where code == "unknown_action" || code == "unavailable" {
                    self.isRefused = true
                } catch TiloEngine.Problem.notOpen {
                    // The page is on its way in again: the following begins anew when it is in.
                    break
                } catch {
                    // Not answered, or the page failed at it this time: asked again by itself.
                    try? await Task.sleep(for: pause)
                    pause = min(pause * 2, .seconds(60))
                }
            }
            self?.asking = false
        }
    }
}
