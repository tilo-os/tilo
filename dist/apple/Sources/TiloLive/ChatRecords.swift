import Foundation

/// Who wrote a message, or whom a conversation is with: a person by their identity, an agent or
/// an automation by its id.
public struct ChatMember: Sendable, Equatable, Hashable {
    public enum Kind: String, Sendable, CaseIterable {
        case person, agent, automation = "bot"
    }

    /// Nil where the page says only an id: a page from before Chat named a member by its kind.
    public let kind: Kind?
    public let id: String

    public init(_ kind: Kind?, id: String) {
        self.kind = kind
        self.id = id
    }

    /// As Chat reads one: `{"person": "<identity>"}`, `{"agent": "<id>"}`, or the bare id.
    public init?(_ value: JSON?) {
        if let id = value?.string {
            self.init(nil, id: id)
            return
        }
        for kind in Kind.allCases {
            if let id = value?[kind.rawValue]?.string {
                self.init(kind, id: id)
                return
            }
        }
        return nil
    }

    /// As Chat writes one into the id of a conversation between members.
    fileprivate var spelled: String {
        let id = JSON.string(self.id).text
        return kind.map { "{\"\($0.rawValue)\":\(id)}" } ?? id
    }
}

public enum ChatConversation {
    /// The id Chat keeps the one conversation of exactly `members` under: `direct:` and the
    /// members, sorted, as JSON. It is derived from who is in it, so two devices that open it
    /// at once make one, and it is known before it holds a message.
    public static func direct(_ members: [ChatMember]) -> String {
        let order = ChatMember.Kind.allCases
        let place = { (member: ChatMember) in member.kind.flatMap(order.firstIndex(of:)) ?? -1 }
        let sorted = members.sorted { place($0) == place($1) ? $0.id < $1.id : place($0) < place($1) }
        return "direct:[" + sorted.map(\.spelled).joined(separator: ",") + "]"
    }
}

/// One message as Chat keeps it (`chat.messages`, an item of its `messages`).
public struct ChatMessage: Sendable, Equatable, Identifiable {
    /// The item's resource, which its own actions are invoked on: `chat.messages/messages/<id>`.
    public let id: String
    /// The conversation it is in, by its id: one that Chat lists by name, or one between members.
    public let conversation: String
    public let author: ChatMember
    public let at: Date
    public let text: String
    /// Every field as Chat reads it: attachments, a shared window, an agent's work, a question.
    public let fields: JSON

    /// What a read of a conversation asks of each message: all that a client draws.
    static let drawn = ["channel", "author", "ts", "text", "question", "share", "attachments", "work", "parent", "stopped", "cause"]

    public init?(id: String, fields: JSON) {
        guard let conversation = fields["channel"]?.string, let author = ChatMember(fields["author"]),
              let seconds = fields["ts"]?.number, let text = fields["text"]?.string
        else { return nil }
        self.id = id
        self.conversation = conversation
        self.author = author
        at = Date(timeIntervalSince1970: seconds)
        self.text = text
        self.fields = fields
    }

    /// The name Chat's own actions take for it: the last part of its item.
    public var name: String { id.split(separator: "/").last.map(String.init) ?? id }

    /// Whether an agent is still at work on it: its work has not ended, and nobody stopped it.
    public var isWorkedOn: Bool {
        guard let work = fields["work"], !work.isNull, fields["stopped"]?.isNull ?? true else { return false }
        let state = work["state"]?["state"]
        guard let step = state?.string ?? state?["$variant"]?.string else { return false }
        return !["done", "refused", "provider_finished"].contains(step)
    }
}

/// Whom Chat's own send goes to.
public struct ChatRecipient: Sendable, Equatable {
    public enum Kind: String, Sendable {
        /// A conversation, by its id.
        case conversation
        /// An agent the workspace keeps, by its id.
        case agent
        /// A person the workspace knows, by their identity.
        case person
    }

    public var kind: Kind
    public var id: String
    public var name: String

    public init(_ kind: Kind, id: String, name: String) {
        self.kind = kind
        self.id = id
        self.name = name
    }

    var json: JSON { ["kind": .string(kind.rawValue), "id": .string(id), "name": .string(name)] }
}

extension TiloEngine {
    /// The resource that holds Chat's messages, under the address the page publishes it at now.
    /// Nil while the page has not published it: a device that just got in holds nothing yet.
    public func chatMessagesResource() async throws -> String? {
        if let known = found["chat.messages"] { return known }
        let listed = try await actions()
        guard let read = listed.first(where: {
            $0["resource_kind"]?.string == "chat.messages" && $0["address"]?["action"]?.string == "read"
        }), let resource = read["address"]?["resource"]?.string
        else { return nil }
        found["chat.messages"] = resource
        // A page whose read takes `where` and `fields` reads one conversation whole in one ask.
        let takes = Set((read["params"]?.array ?? []).compactMap { $0["name"]?.string })
        found["chat.messages.narrows"] = takes.isSuperset(of: ["where", "fields"]) ? "yes" : "no"
        return resource
    }

    /// The newest messages' items, oldest first, at most `count` (Chat answers 200 at most).
    /// Empty while Chat's messages have not reached this device.
    public func chatLatest(_ count: Int = 50) async throws -> [String] {
        guard let resource = try await chatMessagesResource() else { return [] }
        let page = try await read(resource, ["pageSize": .number(Double(min(max(count, 1), 200)))])
        let items = (page["messages"]?.array ?? []).compactMap { $0["item"]?.string }
        return items.reversed()
    }

    /// One message whole. Nil when Chat no longer has it.
    public func chatMessage(_ item: String) async throws -> ChatMessage? {
        ChatMessage(id: item, fields: try await read(item))
    }

    /// The newest messages of one conversation, oldest first, at most `count`, in one read of
    /// the page. Nil where the page's read cannot name a conversation: then `chatLatest` and
    /// `chatMessage` read what there is, message by message.
    public func chatConversation(_ id: String, newest count: Int = 60) async throws -> [ChatMessage]? {
        guard let resource = try await chatMessagesResource() else { return [] }
        guard found["chat.messages.narrows"] == "yes" else { return nil }
        let page = try await read(resource, ["where": ["channel": .string(id)], "fields": .array(ChatMessage.drawn.map(JSON.string)),
                                             "pageSize": .number(Double(min(max(count, 1), 200)))])
        let read = (page["messages"]?.array ?? []).compactMap { row in
            row["item"]?.string.flatMap { ChatMessage(id: $0, fields: row) }
        }
        return read.reversed()
    }

    /// Chat's own send, as the person: the words go to the recipient's conversation, which is
    /// made when it is the first to them. Returns the new message's id. On a device that just
    /// got in it waits, for a few seconds at most, until Chat offers its send.
    @discardableResult
    public func chatSend(_ text: String, to recipient: ChatRecipient) async throws -> String {
        let answer = try await whenPublished {
            try await self.invoke("send", on: "chat.workspace",
                                  ["to": [recipient.json], "attached": [], "text": .string(text)])
        }
        guard let value = answer["output"]?["value"], value["accepted"]?.bool == true,
              let message = value["message"]?.string
        else { throw Problem.refused(code: "not-sent", reason: "Chat did not take the message") }
        return message
    }

    /// Stop the agent that is at work on a message, as the person: Chat's own `stop-agent`.
    public func chatStop(_ message: ChatMessage) async throws {
        guard let resource = try await chatMessagesResource() else { throw Problem.notOpen }
        try await invoke("stop-agent", on: resource, ["message": .string(message.name)])
    }
}
