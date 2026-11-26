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
        let sorted = Set(members).sorted { place($0) == place($1) ? $0.id < $1.id : place($0) < place($1) }
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
    /// Every field as Chat reads it: attachments, a shared window, an agent's work, a question,
    /// the view it carries.
    public let fields: JSON

    /// What a read of a conversation asks of each message: all that a client draws.
    static let drawn = ["channel", "author", "ts", "text", "question", "share", "attachments", "work", "parent", "stopped", "cause", "view", "quoted"]
    /// What a read of a conversation names: `drawn`, whom a message's author acted for, and the
    /// message it answers.
    static let wanted = drawn + ["acting", "answers"]
    /// The fields of `wanted` that a page's Chat may keep on no message yet.
    static let newer = ["acting", "view", "answers"]
    /// Chat's own name for the field that says which conversation a message is in.
    static let place = drawn[0]

    public init?(id: String, fields: JSON, milliseconds: Bool = false) {
        guard let conversation = fields["channel"]?.string, let author = ChatMember(fields["author"]),
              let stamp = fields["ts"]?.number, let text = fields["text"]?.string
        else { return nil }
        self.id = id
        self.conversation = conversation
        self.author = author
        at = Date(timeIntervalSince1970: milliseconds ? stamp / 1000 : stamp)
        self.text = text
        self.fields = fields
    }

    /// The name Chat's own actions take for it: the last part of its item.
    public var name: String { id.split(separator: "/").last.map(String.init) ?? id }

    /// The one person the software that wrote it acted for, as Chat keeps who wrote a message
    /// for whom (`acting`: who acted, and under `for` whom they acted for, as a tree). Nil for a
    /// person's own message, for software that acted for nobody, and for what acted for several
    /// people at once. Chat's own check of who ends a share reads the same.
    public var actedFor: String? {
        guard var node = fields["acting"], !node.isNull else { return nil }
        if ChatMember(node["by"]?["member"])?.kind == .person { return nil }
        while true {
            if let member = ChatMember(node["by"]?["member"]), member.kind == .person {
                return member.id.isEmpty ? nil : member.id
            }
            guard let acted = node["for"]?.array, acted.count == 1 else { return nil }
            node = acted[0]
        }
    }

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
        let listed = try await actions(ofKind: "chat.messages")
        guard let read = listed.first(where: {
            $0["resource_kind"]?.string == "chat.messages" && $0["address"]?["action"]?.string == "read"
        }), let resource = read["address"]?["resource"]?.string
        else { return nil }
        found["chat.messages"] = resource
        // A page whose read takes `where` and `fields` reads one conversation whole in one ask.
        let takes = Set((read["params"]?.array ?? []).compactMap { $0["name"]?.string })
        found["chat.messages.narrows"] = takes.isSuperset(of: ["where", "fields"]) ? "yes" : "no"
        // A Chat that keeps which message a message answers says so in what its post takes.
        let post = listed.first { $0["resource_kind"]?.string == "chat.messages" && $0["address"]?["action"]?.string == "post" }
        let given = Set((post?["params"]?.array ?? []).compactMap { $0["name"]?.string })
        found["chat.post.answers"] = given.contains("answers") ? "yes" : "no"
        return resource
    }

    /// Whether the page's Chat keeps which message a message answers: its post takes `answers`.
    /// False while the page has not published Chat's messages.
    public func chatKeepsAnswers() async throws -> Bool {
        _ = try await chatMessagesResource()
        return found["chat.post.answers"] == "yes"
    }

    /// Chat's own post, as the person, into a conversation that is there: the words answer the
    /// message `answered` of that conversation, named as Chat's actions name a message, whole or
    /// the part of its words that `quoted` says (`text`, and where it stands as `start` and
    /// `end`). `attached` is what the message carries, as `chatSend` takes it. Only a Chat that
    /// keeps which message a message answers takes this (`chatKeepsAnswers`).
    public func chatPost(_ text: String, in conversation: String, answering answered: String, quoted: JSON? = nil,
                         carrying attached: [JSON] = []) async throws {
        guard let resource = try await chatMessagesResource() else { throw Problem.notOpen }
        var message: [String: JSON] = [ChatMessage.place: .string(conversation), "text": .string(text), "answers": .string(answered)]
        if let quoted { message["quoted"] = quoted }
        if !attached.isEmpty { message["attachments"] = .array(attached) }
        try await invoke("post", on: resource, message)
    }

    // The hosted chain lists {id, value}. A window from an older site
    // publishes the same item's full resource address beside its fields.
    private func messageItem(_ row: JSON) -> String? {
        if let item = row["item"]?.string { return item }
        guard let id = row["id"]?.string, !id.isEmpty else { return nil }
        return "chat.messages/messages/\(id)"
    }

    /// The newest messages' items, oldest first, at most `count` (Chat answers 200 at most).
    /// Empty while Chat's messages have not reached this device.
    public func chatLatest(_ count: Int = 50) async throws -> [String] {
        guard let resource = try await chatMessagesResource() else { return [] }
        let page = try await read(resource, ["pageSize": .number(Double(min(max(count, 1), 200)))])
        let items = (page["messages"]?.array ?? []).compactMap { messageItem($0) }
        return items.reversed()
    }

    /// One message whole. Nil when Chat no longer has it.
    public func chatMessage(_ item: String) async throws -> ChatMessage? {
        ChatMessage(id: item, fields: try await read(item), milliseconds: hosted)
    }

    /// The newest messages of one conversation, oldest first, at most `count`, in one read of
    /// the page. Nil where the page's read cannot name a conversation: then `chatLatest` and
    /// `chatMessage` read what there is, message by message.
    public func chatConversation(_ id: String, newest count: Int = 60) async throws -> [ChatMessage]? {
        guard let resource = try await chatMessagesResource() else { return [] }
        guard found["chat.messages.narrows"] == "yes" else { return nil }
        // A read refuses a field its items do not have. A page whose Chat keeps a newer field on
        // no message yet (the view a message carries, whom its author acted for) is asked once
        // with it and from then on without: first without each alone, then without all of them.
        func asked(_ fields: [String]) async throws -> JSON {
            try await self.read(resource, ["where": ["channel": .string(id)], "fields": .array(fields.map(JSON.string)),
                                      "pageSize": .number(Double(min(max(count, 1), 200)))])
        }
        let kept = ChatMessage.wanted.filter { found["chat.messages.\($0)"] != "no" }
        let doubted = ChatMessage.newer.filter(kept.contains)
        var without: [[String]] = [[]] + doubted.map { [$0] }
        if doubted.count > 1 { without.append(doubted) }
        var page: JSON = .null
        for (index, left) in without.enumerated() {
            do {
                page = try await asked(kept.filter { !left.contains($0) })
                for field in left { found["chat.messages.\(field)"] = "no" }
                break
            } catch TiloEngine.Problem.refused where index + 1 < without.count {
                continue
            }
        }
        let read = (page["messages"]?.array ?? []).compactMap { row in
            messageItem(row).flatMap { ChatMessage(id: $0, fields: row["value"] ?? row, milliseconds: hosted) }
        }
        return read.reversed()
    }

    /// Chat's own send, as the person: the words go to the recipient's conversation, which is
    /// made when it is the first to them. Returns the new message's id. On a device that just
    /// got in it waits, for a few seconds at most, until Chat offers its send. `attached` is
    /// what the message carries beside its words, each as Chat names it: a file by its name,
    /// by what the workspace keeps its content under and by its size.
    @discardableResult
    public func chatSend(_ text: String, to recipient: ChatRecipient, carrying attached: [JSON] = [],
                         delivery: UUID = UUID()) async throws -> String {
        if hosted {
            guard let identity = try await whose() else { throw Problem.notOpen }
            let channel: String
            switch recipient.kind {
            case .conversation:
                channel = recipient.id
            case .person, .agent:
                let other = ChatMember(recipient.kind == .agent ? .agent : .person, id: recipient.id)
                channel = ChatConversation.direct([ChatMember(.person, id: identity.person), other])
            }
            let id = delivery.uuidString.lowercased()
            do {
                try await whenPublished {
                    guard let resource = try await self.chatMessagesResource() else {
                        throw Problem.refused(code: "unknown_action", reason: "Chat has not published its messages")
                    }
                    try await self.invoke("post", on: resource, ["id": .string(id), ChatMessage.place: .string(channel),
                                                                "text": .string(text), "attachments": .array(attached)],
                                          invocation: delivery)
                }
            } catch let problem as Problem {
                // A lost receipt can follow a committed write. Verify its body and
                // authenticated author in this workspace before acknowledging it.
                switch problem {
                case .noAnswer, .refused(code: "unavailable", reason: _): break
                default: throw problem
                }
                guard let here = try? await whose(), here.workspace == identity.workspace, here.person == identity.person,
                      let posted = try? await chatMessage("chat.messages/messages/\(id)"),
                      posted.conversation == channel, posted.author == ChatMember(.person, id: identity.person),
                      posted.text == text, posted.fields["attachments"]?.array ?? [] == attached
                else { throw problem }
            }
            return id
        }
        let answer = try await whenPublished {
            try await self.invoke("send", on: "chat.workspace",
                                  ["to": [recipient.json], "attached": .array(attached), "text": .string(text)])
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

    /// End the share the message `item` carries, as the person: Chat's own `stop-sharing`. Chat
    /// decides whose it is to end: its sharer's, or the person their agent shared it for.
    public func chatStopSharing(_ item: String) async throws {
        guard let resource = try await chatMessagesResource() else { throw Problem.notOpen }
        let name = item.split(separator: "/").last.map(String.init) ?? item
        try await invoke("stop-sharing", on: resource, ["message": .string(name)])
    }
}

/// The workspace host's assessment of one agent's permission to work in Chat.
public struct ChatGrantStatus: Sendable, Equatable {
    public enum State: String, Sendable {
        case missing, pending, approved, denied, expired, revoked
    }

    public let state: State
    public let ready: Bool
    public let apps: [String]
    public let expiresAtMs: Int64?

    fileprivate init?(_ value: JSON) {
        guard let raw = value["state"]?.string, let state = State(rawValue: raw),
              let ready = value["ready"]?.bool, !ready || state == .approved,
              let names = value["apps"]?.array, let expiry = value["expiresAtMs"]
        else { return nil }
        let apps = names.compactMap(\.string)
        guard apps.count == names.count else { return nil }
        if expiry.isNull {
            expiresAtMs = nil
        } else {
            guard let number = expiry.number, let milliseconds = Int64(exactly: number), milliseconds >= 0 else { return nil }
            expiresAtMs = milliseconds
        }
        self.state = state
        self.ready = ready
        self.apps = apps
    }
}

extension TiloEngine {
    /// Ask the workspace's server about the agents the workspace keeps (Assistant's
    /// `tilo.workspace.agent`), as the person on this device: `read`,
    /// `ensure-personal-assistant` or `stop-sub-agent`, and with `agent` the read of
    /// that one agent. The page a native app opens shows Chat alone, which keeps no agents, so
    /// this is where the workspace's agents are read from, and nothing else of Assistant's
    /// document is asked through the device's entry.
    ///
    /// - Throws: `Problem.refused` with the server's own code and sentence; `unavailable` for a
    ///   page that hands a native app no way to its workspace's server, `unknown_action` for a
    ///   server that offers no such thing.
    @discardableResult
    public func askAgents(_ action: String, _ arguments: [String: JSON] = [:], agent: String? = nil) async throws -> JSON {
        let answer = try await ask("return globalThis.__tiloLive.agents(given, action, argument, item)",
                                   ["given": pass, "action": action, "argument": JSON.object(arguments).any,
                                    "item": agent.map { $0 as Any } ?? NSNull()])
        return try deviceOutput(answer)
    }

    /// The roster agent's current Chat grant as the certified person in this workspace.
    public func chatGrantStatus(agent: String, forDevice scope: (person: String, workspace: String)) async throws -> ChatGrantStatus {
        let value = try await chatGrant("grant-status", agent: agent, scope: scope)
        guard let status = ChatGrantStatus(value) else { throw Problem.noAnswer }
        return status
    }

    /// Request Chat access for this agent and return its approval code, if approval is pending.
    public func requestChatGrant(agent: String, forDevice scope: (person: String, workspace: String)) async throws -> String? {
        try await requestGrant("request-chat-grant", agent: agent, scope: scope)
    }

    /// Request the host's Assistant work scope for this agent, for the person's approval.
    public func requestAssistantWorkGrant(agent: String, forDevice scope: (person: String, workspace: String)) async throws -> String? {
        try await requestGrant("request-assistant-work-grant", agent: agent, scope: scope)
    }

    /// Withdraw the named agent's current grant as its keeper.
    public func revokeChatGrant(agent: String, forDevice scope: (person: String, workspace: String)) async throws {
        let value = try await chatGrant("revoke", agent: agent, scope: scope)
        guard value.object?.isEmpty == true else { throw Problem.noAnswer }
    }

    private func requestGrant(_ action: String, agent: String, scope: (person: String, workspace: String)) async throws -> String? {
        let value = try await chatGrant(action, agent: agent, scope: scope)
        guard value["grant"]?.string == agent, let code = value["pairingCode"]?.string else { throw Problem.noAnswer }
        return code.isEmpty ? nil : code
    }

    private func chatGrant(_ action: String, agent: String, scope: (person: String, workspace: String)) async throws -> JSON {
        let answer = try await ask("return globalThis.__tiloLive.chatGrant(given, action, agent, scope)", [
            "given": pass, "action": action, "agent": agent, "scope": ["person": scope.person, "workspace": scope.workspace],
        ])
        return try deviceOutput(answer)
    }
}
