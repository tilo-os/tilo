import AssistantCore
import AssistantDrawing
import XCTest

/// What the assistant view says of a service the person connected.
final class ConnectionWordsTests: XCTestCase {
    private let wording = Drawing.wording

    func testAConnectionNobodyIsSignedInToSaysSoWhereTheOthersCountTheirCommands() {
        var facts = Supervision()
        facts.connections = [
            Supervision.Connection(id: "a", name: "Orchard Tools", added: Drawing.at(8, 0), commands: 0, needsSignIn: true),
            Supervision.Connection(id: "b", name: "Grove", added: Drawing.at(8, 0, day: 12), commands: 14),
        ]
        XCTAssertEqual(wording.connections(facts), Wording.Line(first: "Orchard Tools · added 17 Nov · sign-in needed",
                                                               second: "Grove · added 12 Nov · 14 commands"))
        // A connection is counted as before where nothing says otherwise.
        XCTAssertFalse(Supervision.Connection(id: "c", name: "Grove", added: Drawing.at(8, 0), commands: 14).needsSignIn)
    }
}
