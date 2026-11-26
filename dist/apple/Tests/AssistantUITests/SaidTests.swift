#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// What the Mac's window says of itself to someone who cannot see it: every control by what it
/// does, the window's parts in the order they are read, and a mark, a question that waits and
/// a protected field as what they are.
@MainActor
final class SaidTests: XCTestCase {
    /// The first place in a tree where something holds.
    private func place(_ said: [Node], _ what: String, _ holds: (Node) -> Bool, file: StaticString = #filePath, line: UInt = #line) throws -> Int {
        try XCTUnwrap(said.firstIndex(where: holds), "nothing is \(what) in:\n\(said.map(\.description).joined(separator: "\n"))",
                      file: file, line: line)
    }

    /// Every element a person can act on says what it does, and no label reads out a sign of the drawing.
    private func checkLabels(_ said: [Node], file: StaticString = #filePath, line: UInt = #line) {
        let acted = ["AXButton", "AXMenuButton", "AXCheckBox", "AXSwitch", "AXTextArea", "AXTextField", "AXPopUpButton"]
        for node in said where acted.contains(node.role) && !(node.object is NSScroller) && String(describing: type(of: node.object)) != "NSAccessibilityScrollerPart" {
            XCTAssertFalse(node.name.trimmingCharacters(in: .whitespaces).isEmpty, "a control has no name: \(node)", file: file, line: line)
        }
        for node in said {
            for sign in ["·", "›", "•"] {
                XCTAssertFalse(node.name.contains(sign), "“\(node.name)” reads a sign of the drawing out", file: file, line: line)
            }
        }
    }

    func testTheMainChatIsReadPartByPartAndEveryControlSaysWhatItDoes() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let said = run.drive.tree()
        run.drive.keep("mac-main-chat")
        checkLabels(said)
        typealias W = AssistantWords

        // Three parts, each named, in the order a person reads the window: the sidebar, the
        // conversation with its field, the Tasks panel.
        let sidebar = try place(said, "the sidebar") { $0.role == "AXGroup" && $0.label == W.sidebar.said }
        let middle = try place(said, "the conversation") { $0.role == "AXGroup" && $0.label == W.conversation.said }
        let panel = try place(said, "the Tasks panel") { $0.role == "AXGroup" && $0.label == W.tasks.said }
        XCTAssertLessThan(sidebar, middle)
        XCTAssertLessThan(middle, panel)

        // Inside the sidebar: the assistant view, search, the main chat, the sub-agents, the person.
        let view = try place(said, "the way to the assistant view") { $0.isButton && $0.label == W.assistantView.said }
        let search = try place(said, "search") { $0.isButton && $0.label == W.search.said }
        let main = try place(said, "the main chat") { $0.isButton && $0.label == W.mainChat.said }
        let agents = try place(said, "the sub-agents' heading") { $0.isHeading && $0.label == W.subAgents.said }
        let agent = try place(said, "a sub-agent") { $0.isButton && $0.label == "Find the Q3 board deck, Researcher, needs you" }
        let person = try place(said, "the person") { $0.role == "AXMenuButton" && $0.name == "Xaver E, Personal" }
        XCTAssertEqual([sidebar, view, search, main, agents, agent, person], [sidebar, view, search, main, agents, agent, person].sorted())
        XCTAssertLessThan(person, middle, "the person closes the sidebar, before the conversation begins")
        XCTAssertEqual(said.filter { $0.role == "AXMenuButton" }.map(\.name), ["Xaver E, Personal", W.askAboutLookRound.said])
        XCTAssertTrue(said[main].isSelected)

        // Inside the middle: the bar's two buttons, the conversation oldest first, then the field and what stands under it.
        let tasksButton = try place(said, "the Tasks button") { $0.isButton && $0.label == W.hideTasks.said }
        let rundown = try place(said, "the first task's line") { $0.isHeading && $0.label == "Morning rundown, scheduled, every day" }
        let quiet = try place(said, "the report") { $0.value.contains("Dima at Mento reached out") }
        let maya = try place(said, "the important task's line") { $0.isHeading && $0.label == "Reply to Maya at Northwind, Important" }
        let replies = try place(said, "its replies") { $0.isButton && $0.label == W.counted(2, one: .showReplyOne, many: .showReplyMany) }
        let question = try place(said, "the question") { $0.label.contains("Maya, who invested in your seed") }
        XCTAssertEqual(said[question].value, W.waitsForAnswer.said, "a question that waits is read out as one")
        let waits = try place(said, "the waiting answers") { $0.role == "AXGroup" && $0.label == W.waitsForAnswer.said }
        let answer = try place(said, "an answer") { $0.isButton && $0.label == "Send it" }
        let own = try place(said, "the person's own words") { $0.name == "\(W.you.said): remind me in 30 minutes to call Jonas" }
        XCTAssertEqual(said[own].role, "AXStaticText")
        let ready = try place(said, "what is ready") { $0.isButton && $0.label == W.seeAllTasks.said }
        let field = try place(said, "the message field") { $0.role == "AXTextArea" && $0.label == W.messageAssistant.said }
        let send = try place(said, "Send") { $0.isButton && $0.label == W.send.said }
        let attach = try place(said, "Attach") { $0.isButton && $0.label == W.attach.said }
        let order = [middle, tasksButton, rundown, quiet, maya, replies, question, waits, answer, own, ready, field, send, attach]
        XCTAssertEqual(order, order.sorted(), "the middle is read from its bar down to the field")
        XCTAssertLessThan(attach, panel)
        XCTAssertEqual(said[waits].depth + 1, said[answer].depth, "the answers are read inside the question that waits")

        // Inside the panel: its name, its two buttons, its four parts, then what waits, each group named with its count.
        let add = try place(said, "Add a task") { $0.isButton && $0.label == W.addTask.said }
        let active = try place(said, "Active") { $0.isButton && $0.label == W.active.said }
        let group = try place(said, "the first group") { $0.isHeading && $0.label == "\(W.readyForReview.said), 2" }
        let important = try place(said, "the important task") { $0.isButton && $0.label == "Reply to Maya at Northwind, Important" }
        let needs = try place(said, "what needs input") { $0.isButton && $0.label == "Find the Q3 board deck, Two versions exist. Which one?" }
        XCTAssertEqual([panel, add, active, group, important, needs], [panel, add, active, group, important, needs].sorted())
        XCTAssertTrue(said[active].isSelected)
    }

    func testASubAgentsWindowScheduledThingsAndTheAssistantViewSayWhatTheyAre() async throws {
        let run = try await Started.app()
        defer { run.end() }
        let drive = run.drive
        typealias W = AssistantWords

        drive.press("Usage drop: investigate, Coder, working", role: "AXButton")
        await drive.until("the sub-agent's window") { drive.shows(W.jobFromMain.said) }
        var said = drive.tree()
        drive.keep("mac-sub-agent")
        checkLabels(said)
        let head = try place(said, "the sub-agent's head") { $0.isHeading && $0.label.hasPrefix("Usage drop: investigate, Coder, ") }
        let job = try place(said, "whose words the job is") { $0.label == W.jobFromMain.said || $0.value == W.jobFromMain.said }
        let asked = try place(said, "the person's question") { $0.name == "\(W.you.said): is it only Safari?" }
        let field = try place(said, "the field") { $0.role == "AXTextArea" && $0.label == W.messageSubAgent.said }
        XCTAssertEqual([head, job, asked, field], [head, job, asked, field].sorted())

        drive.press(W.mainChat.said, role: "AXButton")
        await drive.until("the main chat") { drive.has(W.tasks.said, role: "AXHeading") }
        drive.press(W.scheduled.said, role: "AXButton")
        await drive.until("Scheduled") { drive.shows("Every ten new mails") }
        said = drive.tree()
        drive.keep("mac-scheduled")
        checkLabels(said)
        // A scheduled thing is a button that says what pressing it leads to, and a switch that says how it stands.
        let row = try place(said, "an event") { $0.isButton && $0.label.hasPrefix("When an investor writes") }
        XCTAssertEqual(said[row].help, W.changeByTelling.said)
        XCTAssertTrue(said[row].label.contains("Acts alone"), "an event's tags are read with it: \(said[row].label)")
        let toggle = try place(said, "its switch") { $0.label == "When an investor writes" && !$0.isButton && $0.role != "AXStaticText" }
        XCTAssertEqual(said[toggle].value, "1")
        XCTAssertLessThan(row, toggle)

        drive.press(W.assistantView.said, role: "AXButton")
        await drive.until("the assistant view") { drive.shows(W.assistantActive.said) }
        said = drive.tree()
        drive.keep("mac-assistant-view")
        checkLabels(said)
        _ = try place(said, "the assistant view as a part") { $0.role == "AXGroup" && $0.label == W.assistantView.said }
        _ = try place(said, "a line read with pauses") { $0.name.hasPrefix("Linear, added ") && $0.name.hasSuffix(", 23 commands") }
        _ = try place(said, "the model's menu") { $0.role == "AXMenuButton" && $0.name == W.change.said }
    }

    func testAProtectedFieldTheWayInAndTheCodeToCompareSayWhatTheyAre() async throws {
        var asking = Drawing.content()
        asking.main = Drawing.secretPiece
        var run = try await Started.app(asking)
        var said = run.drive.tree()
        run.drive.keep("mac-secret")
        checkLabels(said)
        let field = try place(said, "the protected field") { $0.subrole == "AXSecureTextField" }
        XCTAssertEqual(said[field].label, AssistantWords.secretField.said(["name": "LinkedIn"]))
        let fine = try place(said, "what the field is") { $0.value == AssistantWords.secretFine.said }
        XCTAssertLessThan(field, fine)
        _ = try place(said, "where it is kept") { $0.value == AssistantWords.keptInVault.said || $0.label == AssistantWords.keptInVault.said }
        run.end()

        // Before a device is paired, and while its code is compared.
        var content = MemorySource.Content()
        content.session = .confirming(code: "481 207")
        run = try await Started.app(content)
        defer { run.end() }
        said = run.drive.tree()
        run.drive.keep("mac-compare-code")
        checkLabels(said)
        _ = try place(said, "the way in's heading") { $0.isHeading && $0.label == AssistantWords.wayInTitle.said }
        let code = try place(said, "the code") { $0.name == "4 8 1 2 0 7" }
        let why = try place(said, "what to do with it") { $0.value == AssistantWords.compareCode.said }
        XCTAssertLessThan(code, why)
        XCTAssertFalse(run.drive.has(AssistantWords.pasteLink.said), "while a code is compared nothing else is offered")
        run.drive.picture("mac-driven-compare-code")
    }
}
#endif
