import Foundation

/// A service the person connected by its address, as their server keeps it. Nothing of its
/// sign-in is part of it: what the service issued is an item of the person's vault, and no
/// read answers it.
public struct PersonConnection: Sendable, Equatable, Identifiable {
    /// One command the service offers.
    public struct Command: Sendable, Equatable {
        public let name: String
        /// What the service calls it for people, or its name where it says nothing: the
        /// service's own words.
        public let title: String
        /// Whether the assistant asks its person before it calls it.
        public let asks: Bool
    }

    /// The server's own id for it, which every ask about it names.
    public let id: String
    /// The service's address, as kept.
    public let address: String
    /// The name the service gives itself: its own word, shown as such.
    public let name: String
    public let added: Date
    /// How it stands, in the server's word: `ready`, `needs-sign-in`, `sign-in-ran-out`,
    /// `unreachable` or `refused`.
    public let state: String
    /// Whether its person has to sign in before it can be used: they never did, or the
    /// service no longer takes their sign-in.
    public let needsSignIn: Bool
    /// The site its sign-in happens at, by name, when it signs in.
    public let signInAt: String?
    public let commands: [Command]

    init?(_ value: JSON) {
        guard let id = value["id"]?.string, let address = value["address"]?.string else { return nil }
        self.id = id
        self.address = address
        name = value["name"]?.string ?? ""
        added = Date(timeIntervalSince1970: (value["addedAtMs"]?.number ?? 0) / 1000)
        state = value["state"]?.string ?? ""
        needsSignIn = value["signIn"]?.string == "needed"
        signInAt = value["signInAt"]?.string
        commands = (value["commands"]?.array ?? []).compactMap { command in
            guard let name = command["name"]?.string else { return nil }
            let title = command["title"]?.string ?? ""
            return Command(name: name, title: title.isEmpty ? name : title, asks: command["asks"]?.bool ?? true)
        }
    }
}

extension TiloEngine {
    // MARK: The person's connections

    /// Every service the person connected, oldest first.
    public func connections() async throws -> [PersonConnection] {
        // An answer with no list in it says nothing of what the person connected: it is no answer.
        guard let listed = try await ask(.connections, "list", of: "connections")["connections"]?.array else { throw Problem.noAnswer }
        return listed.compactMap(PersonConnection.init)
    }

    /// Begin signing in to a connection, as the person on this device, and answer the page
    /// their browser opens: the service's own. `returnTo` is where the browser comes back to
    /// when the service is done, an address this app catches. Nothing here signs in: the
    /// person does, at the service.
    public func beginSignIn(to connection: String, returnTo: String) async throws -> URL {
        let step = try await ask(.connections, "begin-sign-in", of: "connections",
                                 ["connection": .string(connection), "returnTo": .string(returnTo)])
        // Only a web page is opened for a sign-in, whatever was answered.
        guard let open = step["open"]?.string, let page = URLComponents(string: open), let scheme = page.scheme?.lowercased(),
              scheme == "https" || scheme == "http", let url = page.url
        else { throw Problem.noAnswer }
        return url
    }

    /// Finish a sign-in with the whole address the person's browser came back to. That address
    /// opens the sign-in once, so it travels as what a protected field holds: into this one
    /// call to the person's server, and back in no answer.
    @discardableResult
    public func finishSignIn(to connection: String, returned: Protected) async throws -> PersonConnection? {
        PersonConnection(try await ask(.connections, "finish-sign-in", of: "connections", ["connection": .string(connection)],
                                       protected: (field: "returned", value: returned)))
    }

    /// Remove a connection, as the person: what it signed in with leaves their vault with it.
    public func removeConnection(_ connection: String) async throws {
        try await ask(.connections, "remove", of: "connections", ["connection": .string(connection)])
    }
}
