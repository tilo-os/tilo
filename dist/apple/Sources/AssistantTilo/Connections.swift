import AssistantCore
import Foundation
import TiloLive

/// What a message asks its person about a service their assistant proposes to connect: the
/// question their server put, with the card it keeps beside its words. Their server looked at
/// the address itself, so nothing of it is the assistant's own word; the service's name is
/// the service's own, shown as such.
struct ConnectionAsk: Equatable {
    /// The service's address, as a connection keeps it: after the person's yes, the connection
    /// of this address in their list is the one their server added.
    var address: String
    /// The name the service gives itself. Empty where it says nothing before a sign-in.
    var name: String
    /// Whether the service wants its person to sign in before it is used.
    var needsSignIn: Bool
    /// The site that sign-in happens at, by name, where the service names it.
    var signInAt: String?
    /// How many commands it lists, where it lists them before a sign-in.
    var commands: Int?
    /// The person's answer, `allow` or `deny`. Nil while the question waits.
    var answered: String?
    /// What their server did with the answer: `started`, `done`, `dropped` or `failed`.
    var outcome: String?
    /// What their server says of that, in its own words.
    var why: String
    /// Whether their server took the question back.
    var withdrawn: Bool

    /// Nil for a question that asks about no connection.
    init?(detail: JSON?, answered: String?, outcome: String?, why: String, withdrawn: Bool) {
        guard let card = detail?["connection"], let address = card["address"]?.string, !address.isEmpty else { return nil }
        self.address = address
        name = card["name"]?.string ?? ""
        needsSignIn = card["needsSignIn"]?.bool ?? false
        signInAt = card["signInAt"]?.string
        commands = card["commands"]?.number.map { Int($0) }
        self.answered = answered
        self.outcome = outcome
        self.why = why
        self.withdrawn = withdrawn
    }

    /// Nobody answered it yet, and it was not taken back.
    var waits: Bool { answered == nil && !withdrawn }
    /// The person said yes, and their server added the service.
    var added: Bool { answered == "allow" && outcome == "done" }
    /// The person said yes, and their server did not add it.
    var failed: Bool { answered == "allow" && (outcome == "failed" || outcome == "dropped") }

    /// How the card stands, by the question and by the person's connections as their server
    /// last listed them (`listed`, nil while it has not).
    ///
    /// The person's yes is what adds the service on their server, and signing in to it comes
    /// after, on their device. So a service that was added and never signed in to still waits
    /// on its card: its yes goes on to the sign-in, and its no takes it out again. One that
    /// was ever signed in to is connected, however its sign-in stands today.
    ///
    /// - Parameter readSince: the list was read after the question said the service was added.
    ///   Only then does a list without it say that it is gone again.
    func stands(among listed: [PersonConnection]?, readSince: Bool) -> ConnectionOffer.State {
        if withdrawn { return .declined }
        guard let answered else { return .waiting }
        guard answered == "allow", !failed else { return .declined }
        if let kept = listed?.first(where: { $0.address == address }) {
            return kept.state == "needs-sign-in" ? .waiting : .connected
        }
        // Their server is still adding it.
        guard added else { return .waiting }
        if listed != nil, readSince { return .declined }
        // The list is not read yet: by the card's own word.
        return needsSignIn ? .waiting : .connected
    }

    /// The card: the service by its own name, or by `site`, the site of its address, where it
    /// gave none. Where their server could not add the service, its reason stands under its words.
    func offer(_ id: ConnectionOffer.ID, text: String, site: String, among listed: [PersonConnection]?, readSince: Bool) -> ConnectionOffer {
        ConnectionOffer(id: id, text: failed && !why.isEmpty ? text + "\n\n" + why : text, name: name.isEmpty ? site : name, commands: commands,
                        needsSignIn: needsSignIn, signInAt: signInAt, state: stands(among: listed, readSince: readSince))
    }
}

/// What the app holds of the person's connections between two reads of their server. None of
/// it is state of its own: the list is their server's, and the rest says what was asked of it.
struct Connected {
    /// Their connections as their server last listed them. Nil while it has not.
    var listed: [PersonConnection]?
    /// The cards whose question said that their service was added, by their message: whether
    /// their server's list was read since. Only a list read since says that one is gone again.
    private var addedSeen: [String: Bool] = [:]
    /// The cards a press is at work on: the press reads their server's list itself.
    private var pressed: Set<String> = []
    /// The sign-ins begun on this device and not finished: the connection each card's is for.
    var signingIn: [ConnectionOffer.ID: String] = [:]
    /// How long their server may take to add a service after their yes before the press says
    /// that it has not yet.
    var patience: Duration = .seconds(30)

    /// The cards that wait for their server's list to be read. Taken before the list is asked
    /// for, so that a card whose service was added while the answer was on its way is not
    /// counted as read since.
    var awaited: Set<String> {
        Set(addedSeen.filter { !$0.value }.keys)
    }

    /// The assistant view's line of each, newest first: its name as the service gives it (the
    /// site of its address where it gives none, as `site` says it), when it was added, how many
    /// commands it offers.
    static func lines(of listed: [PersonConnection], site: (String) -> String) -> [Supervision.Connection] {
        listed.reversed().map { kept in
            Supervision.Connection(id: .init(kept.id), name: kept.name.isEmpty ? site(kept.address) : kept.name,
                                   added: kept.added, commands: kept.commands.count, needsSignIn: kept.needsSignIn)
        }
    }

    /// The card of what `message` asks, and whether their server's list is to be asked again
    /// for it: once, where the question says the service was added and the list as last read
    /// does not hold it. Somebody answered on another device, then.
    mutating func card(_ asked: ConnectionAsk, of message: String, text: String, site: String) -> (offer: ConnectionOffer, askAgain: Bool) {
        var askAgain = false
        if asked.added, addedSeen[message] == nil {
            addedSeen[message] = false
            if let listed, !listed.contains(where: { $0.address == asked.address }), !pressed.contains(message) { askAgain = true }
        }
        return (asked.offer(.init(message), text: text, site: site, among: listed, readSince: addedSeen[message] ?? false), askAgain)
    }

    /// A press on the card of `message` begins, or ends. While it lasts it reads their server's
    /// list itself, and the card asks for none.
    mutating func press(_ message: String, begins: Bool) {
        if begins { pressed.insert(message) } else { pressed.remove(message) }
    }

    /// The service of the card of `message` was taken out again: the list that is read next
    /// says so.
    mutating func readAgain(for message: String) {
        addedSeen[message] = false
    }

    /// Their server listed their connections, asked for while the cards `awaited` waited for
    /// it. Says whether a card may stand otherwise now.
    mutating func heard(_ now: [PersonConnection], for awaited: Set<String>) -> Bool {
        for message in awaited { addedSeen[message] = true }
        defer { listed = now }
        return !awaited.isEmpty || listed != now
    }
}
