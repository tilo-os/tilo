import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Wait until something holds, a little while at most.
@MainActor
private func until(_ holds: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async throws {
    for _ in 0..<400 {
        if holds() { return }
        try await Task.sleep(for: .milliseconds(5))
    }
    XCTFail("it never came", file: file, line: line)
}

@MainActor
final class ModelTests: XCTestCase {
    func testTheAppStartsEmptyAtTheWayIn() async {
        let model = AssistantModel(source: MemorySource())
        XCTAssertNil(model.session, "until the source has spoken, the window shows nothing")
        await model.prime()
        XCTAssertEqual(model.session, .signedOut)
        XCTAssertNil(model.person)
        XCTAssertTrue(model.conversation(.main).items.isEmpty)
        XCTAssertTrue(model.subAgents.isEmpty && model.tasks.isEmpty && model.automations.isEmpty)
        XCTAssertNil(model.supervision.model)
        XCTAssertEqual(model.waiting, 0)
    }

    func testItHoldsWhatTheSourceHolds() async {
        let model = AssistantModel(source: Drawing.source())
        await model.prime()
        XCTAssertEqual(model.person, Drawing.person)
        XCTAssertEqual(model.conversation(.main).items, Drawing.mainChat)
        XCTAssertEqual(model.conversation(.subAgent("usage-drop")).items, Drawing.usageDropChat)
        XCTAssertEqual(model.subAgents, Drawing.subAgents)
        XCTAssertEqual(model.tasks(.readyForReview).count, 2)
        XCTAssertEqual(model.waiting, 3, "two ready for review and one that needs input")
        XCTAssertEqual(model.supervision, Drawing.supervision)
    }

    func testSendingEmptiesTheFieldAndNothingIsLostWhenItFails() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        model.drafts[.main] = Draft(text: "**thanks**")
        model.send(in: .main)
        XCTAssertNil(model.drafts[.main], "the field is empty at once")
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 1 }
        XCTAssertNil(model.problem)

        source.set(.unreachable)
        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        try await until { model.problem != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertEqual(model.drafts[.main]?.text, "are you there", "what did not go through is back in the field")
        XCTAssertEqual(source.items().count, Drawing.mainChat.count + 1)
        try await until { model.reach == .unreachable }
    }

    func testWhatArrivesIsToldOnceAndWhatWasThereIsNot() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        var told: [Item.ID] = []
        var settled: [Item.ID] = []
        model.told = { item, _ in told.append(item.id) }
        model.settled = { item, _ in settled.append(item.id) }
        model.start()
        try await until { model.conversation(.main).isLoaded }
        XCTAssertEqual(told, [], "what the conversation held before is not news")
        source.post(Drawing.reminder)
        try await until { told == [Drawing.reminder.id] }
        model.answer(Drawing.mayaQuestion.id, with: "send")
        try await until { settled == ["maya"] }
        XCTAssertFalse(try XCTUnwrap(model.conversation(.main).items.first { $0.id == "maya" }).waits)
    }

    func testAPieceItsServerLaterCallsAReminderIsToldThenAndNotAgain() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        var told: [Item.ID] = []
        var settled: [Item.ID] = []
        model.told = { item, _ in told.append(item.id) }
        model.settled = { item, _ in settled.append(item.id) }
        model.start()
        try await until { model.conversation(.main).isLoaded }
        // The assistant's answer arrives as plain words: it is handed on, and nothing is to tell of it.
        var stretch = Item(id: "stretch", at: Date(), .message(Message(.assistant, "Time to stretch.")))
        source.post(stretch)
        try await until { told == ["stretch"] }
        XCTAssertNil(NoticePlan.notice(for: stretch, in: .main))
        // A moment later the person's Tilo says it is their reminder: now it is told.
        stretch.content = .reminder("Time to stretch.")
        source.change(stretch)
        try await until { told == ["stretch", "stretch"] }
        XCTAssertEqual(NoticePlan.notice(for: stretch, in: .main)?.kind, .reminder)
        XCTAssertEqual(settled, [])
        // What changes on it afterwards without making another kind of thing of it is not told again.
        stretch.isImportant = true
        source.change(stretch)
        try await until { settled == ["stretch"] }
        XCTAssertEqual(told, ["stretch", "stretch"])
    }

    func testASecretIsHandedOnAndHeldNowhere() async throws {
        var content = Drawing.content()
        content.main = Drawing.secretPiece
        let source = MemorySource(content)
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        let kept = await model.keep(Secret("correct horse battery staple"), for: "linkedin")
        XCTAssertTrue(kept)
        try await until { model.conversation(.main).items.first?.content.secret?.state == .kept }
        var held = ""
        dump(model.conversations, to: &held)
        dump(model.drafts, to: &held)
        XCTAssertFalse(held.contains("correct") || held.contains("staple"))
        XCTAssertEqual(source.kept(for: "linkedin")?.handOver(), "correct horse battery staple")
        let again = await model.keep(Secret("another"), for: "linkedin")
        XCTAssertFalse(again)
        XCTAssertNotNil(model.problem)
    }

    func testATasksRepliesOpenUnderItsLineAndArePutAwayAgain() async throws {
        let model = AssistantModel(source: Drawing.source())
        model.start()
        try await until { model.conversation(.main).isLoaded }
        XCTAssertTrue(model.openReplies.isEmpty)
        model.toggleReplies(of: "maya", under: "maya", in: .main)
        try await until { model.openReplies["maya"] != nil }
        XCTAssertEqual(model.openReplies["maya"], Drawing.mayaReplies)
        XCTAssertEqual(model.conversation(.main).items, Drawing.mainChat, "the replies are no pieces of the conversation")
        model.toggleReplies(of: "maya", under: "maya", in: .main)
        XCTAssertNil(model.openReplies["maya"], "the same press puts them away, at once")
        // A task with none to show opens on nothing, and says so by holding none.
        model.toggleReplies(of: "morning-rundown", under: "rundown-1", in: .main)
        try await until { model.openReplies["rundown-1"] != nil }
        XCTAssertEqual(model.openReplies["rundown-1"], [])
    }

    func testRepliesThatCannotBeFetchedOpenNothingAndSayWhy() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(.unreachable)
        model.toggleReplies(of: "maya", under: "maya", in: .main)
        try await until { model.problem != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertNil(model.openReplies["maya"])
    }

    func testWordsWithNoLinkInThemPairNothing() async {
        let model = AssistantModel(source: MemorySource())
        await model.prime()
        model.pair(with: "nothing to open here")
        XCTAssertEqual(model.problem, AssistantWords.noLink.said)
        model.pair(with: "12345", scanned: true)
        XCTAssertEqual(model.problem, AssistantWords.codeNoLink.said)
        XCTAssertEqual(model.session, .signedOut)
    }

    func testALinkWhereTiloDoesNotAnswerLeavesTheDeviceOutAndSaysSo() async throws {
        let model = AssistantModel(source: MemorySource())
        model.start()
        try await until { model.session == .signedOut }
        model.pair(with: "https://tilo.example/pair#abc")
        try await until { model.problem != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertEqual(model.session, .signedOut)
    }

    func testThePanelStandsBesideTheMainChatAndComesElsewhereWhenAskedFor() {
        let model = AssistantModel(source: Drawing.source())
        XCTAssertTrue(model.showsPanel)
        model.open(.subAgent("usage-drop"))
        XCTAssertFalse(model.showsPanel, "a sub-agent's window is drawn without the panel")
        model.showTasks(.scheduled)
        XCTAssertTrue(model.showsPanel)
        XCTAssertEqual(model.panelTab, .scheduled)
        model.open(.main)
        XCTAssertTrue(model.showsPanel)
        model.showsPanel = false
        model.open(.subAgent("usage-drop"))
        XCTAssertTrue(model.showsPanel)
    }

    func testChangingWhatItSetUpBeginsWithItsNameInTheField() {
        let model = AssistantModel(source: Drawing.source())
        model.place = .assistantView
        model.begin("Morning rundown: ")
        XCTAssertEqual(model.place, .chat(.main))
        XCTAssertEqual(model.drafts[.main]?.text, "Morning rundown: ")
        XCTAssertEqual(model.fieldCalls, 1)
        // What the person had begun is theirs: it is not written over.
        model.begin("Something else: ")
        XCTAssertEqual(model.drafts[.main]?.text, "Morning rundown: ")
    }
}

/// The drawing's signs and colours, as the views are given them.
@MainActor
final class LookTests: XCTestCase {
    func testEverySignIsTheDrawingsPathInsideItsSquare() {
        for sign in Sign.allCases {
            let bounds = sign.path(side: 16).boundingRect
            XCTAssertFalse(bounds.isEmpty, sign.rawValue)
            XCTAssertTrue(CGRect(x: 0, y: 0, width: 16, height: 16).insetBy(dx: -0.01, dy: -0.01).contains(bounds), sign.rawValue)
        }
        // An arc as a drawing writes it: half a circle of radius 4 under its two ends.
        let arc = SVGPath.path("M4 8a4 4 0 0 0 8 0").boundingRect
        XCTAssertEqual(arc.minX, 4, accuracy: 0.01)
        XCTAssertEqual(arc.maxX, 12, accuracy: 0.01)
        XCTAssertEqual(arc.minY, 8, accuracy: 0.01)
        XCTAssertEqual(arc.maxY, 12, accuracy: 0.01)
        // Numbers written against one another, and a smooth curve after a curve.
        let eye = SVGPath.path("M2 8s2.2-4 6-4 6 4 6 4-2.2 4-6 4-6-4-6-4z").boundingRect
        XCTAssertEqual(eye.minX, 2, accuracy: 0.01)
        XCTAssertEqual(eye.maxX, 14, accuracy: 0.01)
        XCTAssertEqual(eye.midY, 8, accuracy: 0.01)
    }

    func testTheColoursAreTheDrawings() throws {
        func parts(_ colour: NSColor) throws -> [Int] {
            let rgb = try XCTUnwrap(colour.usingColorSpace(.sRGB))
            return [rgb.redComponent, rgb.greenComponent, rgb.blueComponent].map { Int(($0 * 255).rounded()) }
        }
        // `--asst` is written in the drawing as it is here; the others were turned from OKLCH once.
        XCTAssertEqual(try parts(Token.asst.platform(.light)), [0x2a, 0x3f, 0xd1])
        XCTAssertEqual(try parts(Token.asst.platform(.dark)), [0x7f, 0x90, 0xff])
        XCTAssertEqual(try parts(Token.ink.platform(.light)), [0x14, 0x16, 0x19])
        XCTAssertEqual(try parts(Token.content.platform(.dark)), [0x14, 0x16, 0x19])
        XCTAssertEqual(Token.clear.platform(.light).alphaComponent, 0)
    }

    func testAChatIsNamedAndFoundAgainForANotification() {
        for chat in [Chat.main, .subAgent("usage-drop")] {
            XCTAssertEqual(Teller.chat(Teller.name(chat)), chat)
        }
        XCTAssertNil(Teller.chat("elsewhere"))
        XCTAssertNil(Teller.chat(nil))
    }

    func testALiveActivitysPressLeadsToWhatItWasAbout() throws {
        let model = AssistantModel(source: Drawing.source())
        LiveLink.open(try XCTUnwrap(LiveLink.url(for: "sub-agent-usage-drop")), in: model)
        XCTAssertEqual(model.place, .chat(.subAgent("usage-drop")))
        LiveLink.open(try XCTUnwrap(LiveLink.url(for: "reminder-remind-jonas")), in: model)
        XCTAssertEqual(model.place, .chat(.main))
        XCTAssertEqual(model.panelTab, .scheduled)
        LiveLink.open(try XCTUnwrap(LiveLink.url(for: "brief-brief")), in: model)
        XCTAssertEqual(model.shownItem, "brief")
        // An address that is not the app's own changes nothing.
        model.place = .assistantView
        LiveLink.open(try XCTUnwrap(URL(string: "https://example.com/live/task-maya")), in: model)
        XCTAssertEqual(model.place, .assistantView)
    }
}
