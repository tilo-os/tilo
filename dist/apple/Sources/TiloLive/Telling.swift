import Foundation

/// One thing a person's server told them outside an answer to their own words (`tilo.telling`,
/// its log `told`): what kind of thing it is, the message of a conversation it names, and the
/// mark the person's own rule gave it. Their server decides all of it; a device only reads it.
public struct ToldThing: Sendable, Equatable, Identifiable {
    public enum Kind: String, Sendable {
        case question, reminder, brief, news
    }

    /// Its id, which stays when the thing changes and takes a new place in the log.
    public let id: String
    /// Its place in the log.
    public let place: Int
    public let at: Date
    public let kind: Kind
    /// A few words over it; empty when it has none.
    public let title: String
    public let words: String
    /// The message of Chat it names, by the name Chat's own actions take: the last part of
    /// its item. Nil when it names nothing in Chat.
    public let message: String?
    /// The person's own rule marked it important.
    public let isImportant: Bool
    /// It is to reach the person now, on their devices.
    public let reaches: Bool
    /// The person saw it or acted on it, or what it tells was taken back.
    public let isSettled: Bool
    /// Whoever told it took it back.
    public let isWithdrawn: Bool
    /// Why it reaches the person or does not, as their server says it.
    public let why: Why
    /// What the person can do about it with one press, in the order their server gives.
    public let offers: [Offer]
    /// The agent whose own telling carried the offers, by name. Empty for what Tilo offers
    /// itself: a question's answers.
    public let offeredBy: String
    /// It is told again until the person saw it.
    public let insists: Bool
    /// How often their server raised it again because it insists.
    public let raised: Int

    /// Why a thing reaches its person or does not: a code, and what the code speaks of. For
    /// `nothing-holds` the fact a line of the person's rule waits on and the value it has now;
    /// for `judged` the one line their own model gave; for `unjudged` why no model was asked.
    public struct Why: Sendable, Equatable {
        public let code: String
        public let fact: String
        public let value: String
        public let words: String
    }

    /// One typed action their server offers with a thing it tells: what a button under a
    /// notification invokes as the person on this device, with the arguments exactly as given.
    public struct Offer: Sendable, Equatable, Identifiable {
        public let id: String
        /// Empty where the app the action belongs to words it itself.
        public let label: String
        /// The one their server expects first.
        public let isMain: Bool
        /// The app the action belongs to, the resource it is invoked on, and its name there.
        public let app: String
        public let resource: String
        public let action: String
        public let arguments: JSON
    }

    init?(_ value: JSON) {
        guard let id = value["id"]?.string, let place = value["seq"]?.number else { return nil }
        self.id = id
        self.place = Int(place)
        at = Date(timeIntervalSince1970: (value["atMs"]?.number ?? 0) / 1000)
        kind = value["kind"]?.string.flatMap(Kind.init(rawValue:)) ?? .news
        title = value["title"]?.string ?? ""
        words = value["words"]?.string ?? ""
        let named = value["about"]?["app"]?.string == "so.tilo.chat" ? value["about"]?["resource"]?.string : nil
        message = named.flatMap { $0.hasPrefix("chat.messages/messages/") ? $0.split(separator: "/").last.map(String.init) : nil }
        isImportant = value["mark"]?["important"]?.bool ?? false
        reaches = value["reaches"]?.bool ?? false
        isWithdrawn = !(value["withdrawnAtMs"]?.isNull ?? true)
        isSettled = !(value["acknowledged"]?.isNull ?? true) || isWithdrawn
        let said = value["mark"]?["why"]
        why = Why(code: said?["code"]?.string ?? "", fact: said?["fact"]?.string ?? "", value: said?["value"]?.string ?? "",
                  words: said?["words"]?.string ?? "")
        offers = (value["actions"]?.array ?? []).compactMap { one in
            guard let id = one["id"]?.string, let app = one["target"]?["app"]?.string,
                  let resource = one["target"]?["resource"]?.string, let action = one["target"]?["action"]?.string
            else { return nil }
            return Offer(id: id, label: one["label"]?.string ?? "", isMain: one["main"]?.bool ?? false, app: app,
                         resource: resource, action: action, arguments: one["arguments"] ?? .object([:]))
        }
        offeredBy = value["offeredBy"]?["name"]?.string ?? ""
        let again = value["insists"]
        insists = !(again?.isNull ?? true)
        raised = Int(again?["raised"]?.number ?? 0)
    }
}

/// What a person's server told them while no device of theirs was looking: from the moment
/// they left to their first look after.
public struct ToldSince: Sendable, Equatable {
    /// When the person left: their last look before ten minutes with none.
    public let left: Date
    /// What still waits for them: questions not answered, and what insists and was not seen.
    public let waits: [ToldThing]
    /// What a turn that an event of theirs caused told them without asking.
    public let didAlone: [ToldThing]
    /// Everything else they were told meanwhile.
    public let cameIn: [ToldThing]
}

extension TiloEngine {
    /// What the person's server told them after `place` in its log, oldest first, and the
    /// newest place the log has given out. A thing that changed comes again under a new place
    /// with the id it had.
    ///
    /// With `waiting`, the server holds its answer until its log moves, ten seconds at most:
    /// the live read of a device that is open. Every ask counts as a device of the person
    /// looking, so their server tells them nowhere else while this device does.
    public func told(after place: Int = 0, waiting: Duration? = nil, device: String = "") async throws -> (told: [ToldThing], newest: Int) {
        var arguments: [String: JSON] = ["after": .number(Double(place))]
        if let waiting { arguments["waitMs"] = .number(Double(min(max(waiting.components.seconds, 0), 10)) * 1000) }
        if !device.isEmpty { arguments["device"] = .string(device) }
        let answer = try await ask(.telling, "list", of: "told", arguments)
        return ((answer["told"]?.array ?? []).compactMap(ToldThing.init), Int(answer["newest"]?.number ?? Double(place)))
    }

    /// Say that the person saw a thing they were told, on this device: their server then
    /// tells them of it nowhere else and raises it no more.
    public func acknowledge(told id: String, by how: String = "opened", device: String = "") async throws {
        var arguments: [String: JSON] = ["told": .string(id), "by": .string(how)]
        if !device.isEmpty { arguments["device"] = .string(device) }
        try await ask(.telling, "acknowledge", of: "told", arguments)
    }

    /// What the person's server told them while they were away, as it counts that stretch at
    /// their first look after. Nil where they were not away.
    public func toldSinceLeft() async throws -> ToldSince? {
        let answer = try await ask(.telling, "since-you-left", of: "told")
        guard let left = answer["leftAtMs"]?.number else { return nil }
        func things(_ name: String) -> [ToldThing] { (answer[name]?.array ?? []).compactMap(ToldThing.init) }
        return ToldSince(left: Date(timeIntervalSince1970: left / 1000), waits: things("waits"), didAlone: things("didAlone"),
                         cameIn: things("cameIn"))
    }
}
