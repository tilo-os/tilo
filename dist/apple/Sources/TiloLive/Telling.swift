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
        isSettled = !(value["acknowledged"]?.isNull ?? true) || !(value["withdrawnAtMs"]?.isNull ?? true)
    }
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
}
