import Foundation

/// One message as Chat keeps it (`chat.messages`, an item of its `messages`).
public struct ChatMessage: Sendable, Equatable, Identifiable {
    /// The item's resource, which its own actions are invoked on: `chat.messages/messages/<id>`.
    public let id: String
    /// The conversation it stands in: a channel's id, or a direct conversation's.
    public let channel: String
    /// Who wrote it: a person's identity or an agent's id, as Chat's people and agents name them.
    public let author: String
    public let at: Date
    public let text: String
    /// Every field as Chat reads it: attachments, a shared window, an agent's work, a question.
    public let fields: JSON

    public init?(id: String, fields: JSON) {
        guard let channel = fields["channel"]?.string, let author = fields["author"]?.string,
              let seconds = fields["ts"]?.number, let text = fields["text"]?.string
        else { return nil }
        self.id = id
        self.channel = channel
        self.author = author
        at = Date(timeIntervalSince1970: seconds)
        self.text = text
        self.fields = fields
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
    /// Nil while the page has not published it: a device that just got in holds no record yet.
    public func chatMessagesResource() async throws -> String? {
        if let known = found["chat.messages"] { return known }
        let listed = try await actions()
        guard let resource = listed.first(where: {
            $0["resource_kind"]?.string == "chat.messages" && $0["address"]?["action"]?.string == "read"
        })?["address"]?["resource"]?.string
        else { return nil }
        found["chat.messages"] = resource
        return resource
    }

    /// The newest messages' items, oldest first, at most `count` (Chat answers 200 at most).
    /// Empty while Chat's records have not reached this device.
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
}
