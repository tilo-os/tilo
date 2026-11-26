import AssistantCore
import AssistantDrawing
import XCTest

/// The lines the window puts together, against the lines of the drawing.
final class WordingTests: XCTestCase {
    private let wording = Drawing.wording

    func testTheMarksBetweenPiecesAreTheDrawings() {
        let items = Drawing.mainChat
        var marks: [String] = []
        var before: Date?
        for item in items {
            if wording.marks(item.at, after: before) { marks.append(wording.mark(item.at)) }
            before = item.at
        }
        XCTAssertEqual(marks, ["Today 7:30", "Today 9:12", "Today 9:44"])
        XCTAssertEqual(wording.mark(Drawing.at(18, 2, day: 16)), "Yesterday 18:02")
        XCTAssertEqual(wording.mark(Drawing.at(9, 5, day: 3)), "3 Nov 9:05")
    }

    func testATasksLineSaysHowItStands() {
        XCTAssertEqual(Drawing.mainChat.compactMap { $0.task.flatMap { wording.line($0.state) } },
                       ["scheduled, every day", "scheduled, every day", "done, by your lasting instruction", "2 replies"])
        XCTAssertEqual(wording.line(.replies(1)), "1 reply")
        XCTAssertNil(wording.line(.none))
    }

    func testSubAgentsAreNamedAsDrawn() {
        XCTAssertEqual(Drawing.subAgents.map(wording.sidebarLine), [
            "Coder · working", "Researcher · needs you", "Researcher · working", "Researcher · done yesterday",
        ])
        XCTAssertEqual(wording.header(Drawing.subAgents[0]),
                       "Coder · The cloud machine · started 9:20 by the main agent · it cannot read the main chat")
        // One the main agent has not given a role yet says only how it stands.
        let fresh = SubAgent(id: "new", title: "Job", started: Drawing.now, startedBy: .person)
        XCTAssertEqual(wording.sidebarLine(fresh), "working")
        XCTAssertEqual(wording.header(fresh), "started 9:44 by you · it cannot read the main chat")
        XCTAssertEqual(wording.caption(.job, at: Drawing.at(9, 20)), "The job, from the main agent")
        XCTAssertEqual(wording.caption(.instruction, at: Drawing.at(9, 31)), "Main agent, 9:31, while it worked")
    }

    func testAnEventShowsItsFilterItsThresholdAndItsNote() {
        let events = Drawing.automations.filter { $0.sort == .event }
        XCTAssertEqual(events.map(wording.tags), [
            ["Sender is an investor", "Note: your style, from 14 replies", "Acts alone"],
            ["Threshold: 10"],
            ["Calendar entries flagged investor"],
            [],
        ])
        XCTAssertEqual(wording.cardTags(Drawing.investorEvent), ["Sender is an investor", "Your style, from 14 replies"])
        XCTAssertEqual(wording.state(Drawing.investorEvent), "Event · on")
        var paused = Drawing.automations[0]
        paused.isOn = false
        XCTAssertEqual(wording.state(paused), "Scheduled · paused")
    }

    func testTheAssistantViewsLinesAreTheDrawings() {
        let facts = Drawing.supervision
        typealias Line = Wording.Line
        XCTAssertEqual(wording.model(facts), Line(first: "Claude Opus 5.5",
                                                  second: "From your Tilo providers · sub-agents may use a smaller one"))
        XCTAssertEqual(wording.messages(facts.usage), "42")
        XCTAssertEqual(wording.tokens(facts.usage), "1.8M")
        XCTAssertEqual(wording.spent(facts.usage), Line(first: "3.12 $", second: "Of 10 $ a day"))
        XCTAssertEqual(wording.connections(facts), Line(first: "Linear · added 17 Nov · 23 commands",
                                                        second: "Affinity · added 12 Nov · 14 commands"))
        XCTAssertEqual(wording.logins(facts), Line(first: "LinkedIn · typed 17 Nov · used today",
                                                   second: "GitHub and 31 more from Bitwarden, fetched when needed"))
        XCTAssertEqual(wording.learned(facts), Line(first: "You sign mails with Best, Xaver",
                                                    second: "Dima at Mento is an investor · 46 more"))
        XCTAssertEqual(wording.places(facts), Line(first: "The cloud machine, always",
                                                   second: "This Mac, in ~/dev/tilo and ~/Documents/Fundraising"))
        // Of several workspaces the row promises nothing. Under the one the assistant works in, it says so.
        XCTAssertEqual(wording.workspaces(facts), Line(first: "Personal · Tilo team · Northwind data room", second: nil))
        var one = facts
        one.workspaces = ["Personal"]
        XCTAssertEqual(wording.workspaces(one), Line(first: "Personal", second: "Your assistant works in this workspace."))
        one.workspaces = []
        XCTAssertEqual(wording.workspaces(one), Line(first: "Nothing yet", second: nil))
    }

    func testAnAssistantThatHasNothingYetSaysSo() {
        let facts = Supervision()
        XCTAssertEqual(wording.model(facts).first, "No model yet")
        XCTAssertEqual(wording.spent(facts.usage), Wording.Line(first: "0 $", second: "Spent today"))
        // What nobody counted is not said as nought.
        XCTAssertEqual(wording.messages(facts.usage), "Not counted yet")
        XCTAssertEqual(wording.tokens(facts.usage), "Not counted yet")
        for line in [wording.connections(facts), wording.logins(facts),
                     wording.learned(facts), wording.places(facts), wording.workspaces(facts)] {
            XCTAssertEqual(line, Wording.Line(first: "Nothing yet", second: nil))
        }
        XCTAssertEqual(wording.tokens(0), "0")
        XCTAssertEqual(wording.tokens(950), "950")
        XCTAssertEqual(wording.tokens(42_300), "42.3K")
    }

    func testThePersonsLettersAreTheFirstOfTheirNames() {
        XCTAssertEqual(Drawing.person.initials, "XE")
        XCTAssertEqual(Person(id: "a", name: "ada", workspace: "").initials, "A")
        XCTAssertEqual(Person(id: "a", name: "Ada Mary Lovelace", workspace: "").initials, "AL")
        XCTAssertEqual(Person(id: "a", name: "", workspace: "").initials, "")
    }
}
