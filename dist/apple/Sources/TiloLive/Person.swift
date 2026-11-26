import Foundation

/// A provider of the person's own on their host: what the host answers only the person, on a
/// device of theirs. These are the ones an app's own code may ask through the engine, and the
/// engine asks no other: not a workspace's governance, not a machine's commands, not pairing.
public enum PersonProvider: String, Sendable, CaseIterable {
    /// The model providers the person linked, and the model each kind of work runs on.
    case models = "tilo.models"
    /// What the person keeps to sign in with. A secret goes in and never comes back.
    case vault = "tilo.vault"
    /// What happens at a time, and when something else happens.
    case events = "tilo.events"
    /// What may interrupt the person, and what they were told.
    case telling = "tilo.telling"
    /// The assistant finding work on its own, and what it proposed.
    case initiative = "tilo.initiative"
    /// What the person's agents ask them, and what they need not ask any more.
    case asking = "tilo.asking"
    /// What the agents their host runs have learned about the person.
    case learned = "tilo.learned"
    /// The services the person connected by their address, and signing in to one.
    case connections = "tilo.connections"
}

/// What a person typed into a protected field, on its way to their host. It says nothing of
/// itself: printing it, describing it or looking into it shows no letter of what is inside.
public struct Protected: Sendable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    private let letters: [UInt8]

    public init(_ typed: String) {
        letters = Array(typed.utf8)
    }

    public var isEmpty: Bool { letters.isEmpty }

    /// What was typed, for the engine alone: it goes into the one call that carries it to the
    /// person's host, and is kept nowhere.
    func handOver() -> String {
        String(decoding: letters, as: UTF8.self)
    }

    public var description: String { "Protected" }
    public var debugDescription: String { "Protected" }
    public var customMirror: Mirror { Mirror(self, children: [], displayStyle: .struct) }
}

/// The model providers a person linked, as their host says them. A provider's key is never
/// part of it: `hasKey` says whether the host's vault keeps one.
public struct ModelProviders: Sendable, Equatable {
    public struct Model: Sendable, Equatable {
        public let id: String
        public let label: String
    }

    public struct Provider: Sendable, Equatable {
        public let id: String
        public let name: String
        /// The protocol it speaks, as the host names it.
        public let speaks: String
        public let endpoint: String
        public let hasKey: Bool
        public let models: [Model]
        public let defaultModel: String?

        init?(_ value: JSON) {
            guard let id = value["id"]?.string else { return nil }
            self.id = id
            name = value["name"]?.string ?? id
            speaks = value["protocol"]?.string ?? ""
            endpoint = value["endpoint"]?.string ?? ""
            hasKey = value["configured"]?.bool ?? false
            models = (value["models"]?.array ?? []).compactMap { model in
                model["id"]?.string.map { Model(id: $0, label: model["label"]?.string ?? $0) }
            }
            defaultModel = value["defaultModel"]?.string
        }
    }

    /// The model one kind of work runs on (`request`, `task`, `subtask`, `classifier`).
    public struct Assignment: Sendable, Equatable {
        public let kind: String
        public let provider: String
        public let model: String
    }

    public let providers: [Provider]
    /// The provider work runs on when its kind names none. None: the person linked no provider.
    public let defaultProvider: String?
    public let assignments: [Assignment]

    init(_ value: JSON) {
        providers = (value["providers"]?.array ?? []).compactMap(Provider.init)
        defaultProvider = value["defaultProvider"]?.string
        assignments = (value["assignments"]?.array ?? []).compactMap { row in
            guard let kind = row["kind"]?.string, let provider = row["provider"]?.string, let model = row["model"]?.string
            else { return nil }
            return Assignment(kind: kind, provider: provider, model: model)
        }
    }

    /// The provider and model a kind of work runs on now: its own, else the default.
    public func model(for kind: String) -> (provider: Provider, model: String)? {
        if let own = assignments.first(where: { $0.kind == kind }), let provider = providers.first(where: { $0.id == own.provider }) {
            return (provider, own.model)
        }
        guard let provider = providers.first(where: { $0.id == defaultProvider }), let model = provider.defaultModel else { return nil }
        return (provider, model)
    }
}

/// What is safe to say of something the person's vault keeps. The secret itself is no part of
/// it and cannot be asked for: whatever signs in names the item by its reference.
public struct VaultItem: Sendable, Equatable, Identifiable {
    /// `vault.item:<id>`, which every use names.
    public let reference: String
    /// `login`, `key`, `code-generator`, `ssh-key` or `value`.
    public let kind: String
    /// Where it belongs: a site's or a service's address, or a server.
    public let belongsTo: [String]
    public let added: Date
    public let lastUsed: Date?

    public var id: String { reference }

    init?(_ value: JSON) {
        guard let reference = value["reference"]?.string else { return nil }
        self.reference = reference
        kind = value["kind"]?.string ?? ""
        belongsTo = (value["belongsTo"]?.array ?? []).compactMap(\.string)
        added = Date(timeIntervalSince1970: value["addedAt"]?.number ?? 0)
        lastUsed = value["lastUsedAt"]?.number.map(Date.init(timeIntervalSince1970:))
    }
}

extension TiloEngine {
    /// Ask one of the person's own providers on their host, as the person on this device, and
    /// answer what the action answered.
    ///
    /// The ask goes through the session Tilo's page holds with the workspace's host, and the
    /// host decides it by this device's certificate: an app, an agent and anyone else's device
    /// are answered what an unknown action is answered. Only the app's own code reaches this:
    /// the page hands its device's entry to the engine alone, and the engine's script takes an
    /// ask only with the pass this engine made.
    ///
    /// - Parameter protected: a field of the action's argument, and what the person typed into
    ///   a protected field for it. It goes into this one call and comes back in no answer.
    /// - Throws: `Problem.refused` with the host's own code and sentence; `unavailable` for a
    ///   page that hands a native app no way to its host.
    @discardableResult
    public func ask(_ provider: PersonProvider, _ action: String, of resource: String, _ arguments: [String: JSON] = [:],
                    protected: (field: String, value: Protected)? = nil,
                    forDevice scope: (person: String, workspace: String)? = nil) async throws -> JSON {
        let secret = protected?.value.handOver()
        var passed: [String: Any] = ["given": pass, "provider": provider.rawValue, "resource": resource, "action": action,
                                     "argument": JSON.object(arguments).any, "hidden": NSNull()]
        passed["scope"] = scope.map { ["person": $0.person, "workspace": $0.workspace] as Any } ?? NSNull()
        if let protected, let secret { passed["hidden"] = ["field": protected.field, "value": secret] }
        let answer = try await ask("return globalThis.__tiloLive.person(given, provider, resource, action, argument, hidden, scope)",
                                   passed, hiding: secret)
        return try deviceOutput(answer)
    }

    /// The certified person's action addresses and parameter names for one host provider.
    public func personActions(_ provider: PersonProvider,
                              forDevice scope: (person: String, workspace: String)) async throws -> [JSON] {
        let answer = try await ask("return globalThis.__tiloLive.personActions(given, provider, scope)", [
            "given": pass, "provider": provider.rawValue, "scope": ["person": scope.person, "workspace": scope.workspace],
        ])
        guard let rows = answer.array else { throw Problem.noAnswer }
        return rows
    }

    /// The signed device action result shared by person and workspace providers.
    func deviceOutput(_ answer: JSON) throws -> JSON {
        switch answer["state"]?.string {
        case "succeeded":
            return answer["value"] ?? .null
        case "failed":
            throw Problem.refused(code: answer["error"]?["code"]?.string ?? "failed",
                                  reason: answer["error"]?["message"]?.string ?? "")
        default:
            throw Problem.noAnswer
        }
    }

    /// Whose device this is, as the page's session says it: the person's root key, hex, and
    /// the workspace the page stands in. Nil for a page that hands a native app nothing.
    public func whose() async throws -> (person: String, workspace: String)? {
        do {
            let answer = try await ask("return globalThis.__tiloLive.whose(given)", ["given": pass])
            guard let person = answer["person"]?.string, let workspace = answer["workspace"]?.string else { return nil }
            return (person, workspace)
        } catch Problem.refused(code: "unavailable", reason: _) {
            return nil
        }
    }

    /// The person's own workspace on this host, which the host makes the first time it is asked
    /// for, with this device in it: its route for `open(route:)`, and its name.
    public func ownWorkspace() async throws -> (route: String, name: String) {
        let answer = try await ask("return globalThis.__tiloLive.own(given)", patience: ownPatience, ["given": pass])
        guard let id = answer["id"]?.string, !id.isEmpty else { throw Problem.noAnswer }
        // A workspace of a named host is written `/@<host>/w/<id>`, the host as its origin names it.
        let host = answer["origin"]?.string.flatMap { URLComponents(string: $0) }.flatMap { parts -> String? in
            guard let name = parts.host else { return nil }
            return parts.port.map { "\(name):\($0)" } ?? name
        }
        return (host.map { "/@\($0)/w/\(id)" } ?? "/w/\(id)", answer["name"]?.string ?? "")
    }

    // MARK: The person's models

    /// The providers the person linked and the model each kind of work runs on.
    public func modelProviders() async throws -> ModelProviders {
        let answer = try await ask(.models, "list", of: "models")
        // An answer with no list in it says nothing of what the person linked: it is no answer.
        guard answer["providers"]?.array != nil else { throw Problem.noAnswer }
        return ModelProviders(answer)
    }

    /// Link a provider the host's catalogue knows (`anthropic`, `gemini`, `openai`, `openrouter`).
    @discardableResult
    public func linkModelProvider(preset: String) async throws -> ModelProviders.Provider? {
        ModelProviders.Provider(try await ask(.models, "add-preset", of: "models", ["preset": .string(preset)]))
    }

    /// Keep a provider's key in the person's vault on their host. The key goes from the
    /// protected field to the host and is said to nobody else.
    @discardableResult
    public func keepModelKey(_ key: Protected, for provider: String) async throws -> ModelProviders.Provider? {
        ModelProviders.Provider(try await ask(.models, "set-key", of: "models", ["provider": .string(provider)],
                                              protected: (field: "key", value: key)))
    }

    /// Ask a provider for the models it offers, with the key the vault keeps for it. The second
    /// value says why its list could not be read (`no-key`, `key-refused`, …), nil when it was.
    public func refreshModels(of provider: String) async throws -> (provider: ModelProviders.Provider?, problem: String?) {
        let answer = try await ask(.models, "refresh-models", of: "models", ["provider": .string(provider)])
        return (answer["provider"].flatMap(ModelProviders.Provider.init), answer["problem"]?.string)
    }

    /// Choose the provider and model work runs on when its kind names none.
    @discardableResult
    public func chooseDefaultModel(_ model: String, of provider: String) async throws -> ModelProviders {
        ModelProviders(try await ask(.models, "set-default", of: "models", ["provider": .string(provider), "model": .string(model)]))
    }

    /// Choose one model for several kinds of work together (`request`, `task`, `subtask`,
    /// `classifier`). With no model, those kinds follow the default again.
    @discardableResult
    public func chooseModel(_ model: (id: String, provider: String)?, for kinds: [String]) async throws -> ModelProviders {
        var arguments: [String: JSON] = ["kinds": .array(kinds.map(JSON.string))]
        if let model {
            arguments["provider"] = .string(model.provider)
            arguments["model"] = .string(model.id)
        }
        return ModelProviders(try await ask(.models, "set-assignments", of: "models", arguments))
    }

    // MARK: The person's events

    /// Set up something that happens at a time, or when something else does: what its person
    /// reads it by, what starts it (`once at 2026-11-17T11:32:00Z`, `every day at 09:00`, or a
    /// change in the workspace), and what their assistant is told to do then.
    @discardableResult
    public func setUpEvent(name: String, when: String, instruction: String) async throws -> JSON {
        try await ask(.events, "set-up", of: "events",
                      ["name": .string(name), "when": .string(when), "instruction": .string(instruction)])
    }

    // MARK: The person's vault

    /// What is safe to say of every item the vault keeps, oldest first.
    public func vaultItems() async throws -> [VaultItem] {
        // An answer with no list in it says nothing of what the vault keeps: it is no answer.
        guard let items = try await ask(.vault, "list", of: "vault")["items"]?.array else { throw Problem.noAnswer }
        return items.compactMap(VaultItem.init)
    }

    /// Keep what the person typed into a protected field in their vault on their host, and
    /// answer the item by its reference. The secret travels in and never back.
    public func keepInVault(_ secret: Protected, kind: String, belongsTo: [String], username: String = "") async throws -> VaultItem {
        var arguments: [String: JSON] = ["kind": .string(kind), "belongsTo": .array(belongsTo.map(JSON.string))]
        if !username.isEmpty { arguments["username"] = .string(username) }
        guard let item = VaultItem(try await ask(.vault, "add", of: "vault", arguments, protected: (field: "secret", value: secret)))
        else { throw Problem.noAnswer }
        return item
    }
}
