import Foundation

/// The issues of the workspace's tracker (Tasks' `tasks.issues`), as the workspace's server
/// answers them to the person on this device. The page this engine holds shows Chat and keeps
/// nothing of Tasks, so what the Tasks panel lists is asked of the server.
extension TiloEngine {
    /// What a row says of its issue beside its item and its name, which is the issue's title.
    static let issueFields = ["number", "status", "assigned_by", "parent", "work"]

    /// Ask the workspace's server about the tracker's issues, as the person on this device:
    /// their `read`, and `create`. Nothing else of Tasks is asked this way.
    ///
    /// - Throws: `Problem.refused` with the server's own code and sentence; `unavailable` for a
    ///   page that hands a native app no way to its workspace's server, `unknown_action` for a
    ///   server that keeps no tracker or offers no such thing.
    @discardableResult
    public func askTracker(_ action: String, _ arguments: [String: JSON] = [:], delivery: UUID? = nil) async throws -> JSON {
        let answer = try await ask("return globalThis.__tiloLive.tasks(given, action, argument, invocation)",
                                   ["given": pass, "action": action, "argument": JSON.object(arguments).any,
                                    "invocation": delivery.map { $0.uuidString.lowercased() as Any } ?? NSNull()])
        return try deviceOutput(answer)
    }

    /// File an issue and give it to one agent, as the person: Tasks' own `create`. Tasks keeps
    /// who gave it, and the workspace's server starts the agent's work on it with no device on.
    public func giveIssue(_ title: String, to agent: String, delivery: UUID) async throws {
        try await askTracker("create", ["title": .string(title), "assignee": .string("agent:\(agent)")], delivery: delivery)
    }

    /// The issues that are given to one agent, in the tracker's own order: a row each, with its
    /// item, its name and what `issueFields` names. A long list comes in pages.
    ///
    /// - Throws: `Problem.noAnswer` for an answer that holds no list: that is nobody's word.
    public func issues(givenTo agent: String) async throws -> [JSON] {
        var rows: [JSON] = []
        var cursor: JSON?
        for _ in 0..<50 {
            var asked: [String: JSON] = ["where": ["assignee": .string("agent:\(agent)")],
                                         "fields": .array(TiloEngine.issueFields.map(JSON.string)), "pageSize": 200]
            if let cursor { asked["cursor"] = cursor }
            let page = try await askTracker("read", asked)
            guard let listed = page["issues"]?.array else { throw Problem.noAnswer }
            rows += listed
            guard let next = page["nextCursor"], !next.isNull else { return rows }
            cursor = next
        }
        return rows
    }
}
