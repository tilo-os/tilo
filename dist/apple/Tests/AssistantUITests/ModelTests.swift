import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import UserNotifications
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
    /// Choosing the main chat's model is one ask of the person's server, for the answering and
    /// the working together: with two, the app said ready after the first.
    func testChoosingTheMainChatsModelIsOneAsk() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.models = Drawing.models
        let started = try await Started.app(content)
        defer { started.end() }
        started.model.choose(Drawing.models[1].id)
        await started.drive.until("the choice to reach the source") { !started.source.calls("choose").isEmpty }
        await started.drive.settle(300)
        XCTAssertEqual(started.source.calls("choose").count, 1, "\(started.source.calls("choose"))")
    }

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

    func testWhatDidNotNotifyIsListedOnceWhatInsistsIsToldAgainAndNewOffersAreBroughtWithoutTelling() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        var told: [Item.ID] = []
        var settled: [Item.ID] = []
        var offered: [Item.ID] = []
        model.told = { item, _ in told.append(item.id) }
        model.settled = { item, _ in settled.append(item.id) }
        model.offered = { item, _ in offered.append(item.id) }
        model.start()
        try await until { model.conversation(.main).isLoaded }
        // A report arrives as plain words; a moment later the person's Tilo says why it did not notify.
        var report = Item(id: "sorted", at: Date(), .report("Nine mails sorted."))
        source.post(report)
        try await until { told == ["sorted"] }
        XCTAssertNil(NoticePlan.notice(for: report, in: .main))
        report.keptQuiet = Quiet.line(about: "", code: "no-rule")
        source.change(report)
        try await until { told == ["sorted", "sorted"] }
        XCTAssertEqual(NoticePlan.notice(for: report, in: .main)?.interrupts, false)
        // The person makes it insist, so it is to reach them after all: now it is told to them.
        report.keptQuiet = nil
        report.isImportant = true
        source.change(report)
        try await until { told == ["sorted", "sorted", "sorted"] }

        // A reminder that insists is told again each time it is raised, and for nothing else.
        var reminder = Drawing.reminder
        source.post(reminder)
        try await until { told.count == 4 }
        reminder.raised = 1
        source.change(reminder)
        try await until { told.count == 5 }
        reminder.isImportant = true
        source.change(reminder)
        try await until { settled == ["remind-jonas"] }
        // Once the person saw it, the count is gone, and nothing is told for that.
        reminder.raised = 0
        source.change(reminder)
        try await until { settled == ["remind-jonas", "remind-jonas"] }
        XCTAssertEqual(told.count, 5)

        // What is offered with a question that was told already is brought to its notice, and not told again.
        var asked = try XCTUnwrap(model.conversation(.main).items.first { $0.id == "maya" })
        asked.offers = [Offer(id: "send", title: "Send it", isMain: true)]
        source.change(asked)
        try await until { offered == ["maya"] }
        XCTAssertEqual(told.count, 5)
        // The press goes to the source as the person's own, and what is not offered is refused there.
        model.press("send", of: "maya", in: .main)
        try await until { source.pressed == [MemorySource.Press(offer: "send", item: "maya", chat: .main)] }
        XCTAssertNil(model.problem)
        model.press("gone", of: "maya", in: .main)
        try await until { model.problem == "This is no longer offered." }
        // Answered, it offers nothing more: that settles it, and nothing is brought.
        asked.offers = []
        if case .question(var question) = asked.content {
            question.answer = "send"
            asked.content = .question(question)
        }
        source.change(asked)
        try await until { settled.last == "maya" }
        XCTAssertEqual(offered, ["maya"])
    }

    func testWhatCameWhileThePersonWasAwayIsHeldAndFollowsItsSource() async throws {
        var content = Drawing.content()
        content.sinceLeft = Drawing.sinceLeft
        let source = MemorySource(content, now: { Drawing.now })
        let primed = AssistantModel(source: source)
        await primed.prime()
        XCTAssertEqual(primed.sinceLeft, Drawing.sinceLeft)
        let model = AssistantModel(source: source)
        XCTAssertNil(model.sinceLeft)
        model.start()
        try await until { model.sinceLeft == Drawing.sinceLeft }
        source.setSinceLeft(nil)
        try await until { model.sinceLeft == nil }
        // A source that keeps no such count says nothing of it.
        let plain = AssistantModel(source: Drawing.source())
        await plain.prime()
        XCTAssertNil(plain.sinceLeft)
    }

    func testTheModelStepNamesAProviderOnlyWhereThatSaysSomething() {
        let one = [ModelChoice(id: "a/large", name: "Large", provider: "Anthropic"), ModelChoice(id: "a/small", name: "Small", provider: "Anthropic")]
        let two = one + [ModelChoice(id: "o/large", name: "Large", provider: "OpenRouter")]
        XCTAssertEqual(ModelCard.note(chosen: nil, among: one), "Anthropic")
        XCTAssertEqual(ModelCard.note(chosen: nil, among: two), "", "of two providers the box names neither before a model is chosen")
        XCTAssertEqual(ModelCard.note(chosen: two[2], among: two), "OpenRouter")
        XCTAssertEqual(ModelCard.note(chosen: two[0], among: two), "Anthropic")
        XCTAssertEqual(ModelCard.providers(of: two), ["Anthropic", "OpenRouter"])
        XCTAssertEqual(ModelCard.note(chosen: nil, among: []), "")
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

    /// A pairing that ends before the device is anybody's leaves it at its way in, and the way
    /// in says Tilo's own sentence for why, until another link is taken.
    func testAPairingThatEndsSaysTilosOwnSentenceAtTheWayInUntilAnotherLinkIsTaken() async throws {
        var content = MemorySource.Content()
        content.pairsAs = Drawing.person
        content.pairingAsk = PairingAsk(heading: "Add this device?", said: ["Whose devices this one joins.", "Which server adds it."],
                                        go: "Add this device", stop: "Stop", site: "https://tilo.example")
        let source = MemorySource(content)
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.session == .signedOut }
        XCTAssertNil(model.problem)
        let why = "Lounge no longer exists. It was reset since this browser last opened it."
        source.endPairing(saying: why)
        try await until { model.problem == why }
        XCTAssertEqual(model.session, .signedOut, "a pairing that ended leaves the device at its way in")
        // Another link is taken: what the last pairing ended with is said no more.
        model.pair(with: "https://tilo.example/pair#abc")
        try await until { if case .asked = model.session { true } else { false } }
        try await until { model.problem == nil }
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

    func testAMessageNoTurnTakesUpInTimeGetsOneQuietLineWhichGoesWhenTheTurnBegins() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.takeUpPatience = .milliseconds(150)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        model.drafts[.main] = Draft(text: "Are you there?")
        model.send(in: .main)
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 1 }
        XCTAssertFalse(model.isNotTakenUp(.main), "the line stood before any time had passed")
        try await until { model.isNotTakenUp(.main) }
        XCTAssertEqual(AssistantWords.notTakenUp.said, "Tilo has not taken this up yet.")
        // The turn begins: the line goes, and does not come back while or after the answer is written.
        source.setAnswering(true)
        try await until { !model.isNotTakenUp(.main) }
        source.post(Item(id: "there", at: Date(), .message(Message(.assistant, "I am."))))
        source.setAnswering(false)
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 2 }
        try await Task.sleep(for: .milliseconds(350))
        XCTAssertFalse(model.isNotTakenUp(.main))
    }

    func testAMessageATurnTakesUpInTimeGetsNoLineAndNeitherDoesOneThatIsAnsweredAtOnce() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.takeUpPatience = .milliseconds(200)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        // A turn takes her message up before the time is over, and is still at work when it is.
        model.drafts[.main] = Draft(text: "One.")
        model.send(in: .main)
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 1 }
        source.setAnswering(true)
        try await Task.sleep(for: .milliseconds(450))
        XCTAssertFalse(model.isNotTakenUp(.main), "a message a turn is at work on was said not to be taken up")
        // The turn ends without a word of its own: nothing is said of the message then either.
        source.setAnswering(false)
        try await Task.sleep(for: .milliseconds(350))
        XCTAssertFalse(model.isNotTakenUp(.main))
        // An answer that comes whole, with no word that it was being written, is the turn as well.
        model.drafts[.main] = Draft(text: "Two.")
        model.send(in: .main)
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 2 }
        source.post(Item(id: "two", at: Date(), .message(Message(.assistant, "Two it is."))))
        try await Task.sleep(for: .milliseconds(450))
        XCTAssertFalse(model.isNotTakenUp(.main))
    }

    func testWhatSheWritesAgainBeginsTheWaitAnewAndTheLineStandsUnderHerNewestMessage() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.takeUpPatience = .milliseconds(300)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        model.drafts[.main] = Draft(text: "One.")
        model.send(in: .main)
        try await until { model.isNotTakenUp(.main) }
        // She writes again: the line goes with the wait for the message before, and comes again after its time.
        model.drafts[.main] = Draft(text: "Two.")
        model.send(in: .main)
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 2 }
        XCTAssertFalse(model.isNotTakenUp(.main))
        try await until { model.isNotTakenUp(.main) }
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

    func testAPiecesNoticeIsKeptUnderTheNameAPushGivesItWhereItHasOne() {
        var item = Item(id: .init("m-1"), at: Date(timeIntervalSince1970: 0), .reminder("Stretch"))
        XCTAssertEqual(Teller.request(item), "assistant-m-1")
        // Told of while the app is closed, the push and the app's own notice are one notice.
        item.toldAs = "rf-qsLfA_59buuS5z2pKfA"
        XCTAssertEqual(Teller.request(item), "rf-qsLfA_59buuS5z2pKfA")
    }

    func testTappingAClosedPushBeforeStartupOpensTheSyncedPieceAndAcknowledgesItOnce() async throws {
        let chat = Chat.subAgent("usage-drop")
        var content = Drawing.content()
        var item = Drawing.reminder
        item.content = .message(Message(.assistant, "Call Jonas."))
        content.subAgentChats["usage-drop"] = [item]
        let source = Recording(MemorySource(content))
        let model = AssistantModel(source: source)
        var announcements: [Item.ID] = []
        model.told = { item, _ in announcements.append(item.id) }
        let fields = ["told": "opaque", "item": "wrong", "chat": "main"]
        let tap = try XCTUnwrap(NoticeTap(UNNotificationDefaultActionIdentifier, fields: fields, pushed: true))
        tap.take(in: model)
        model.start()
        try await until { model.conversation(chat).isLoaded }
        XCTAssertNil(model.shownItem)
        XCTAssertTrue(source.calls("seen").isEmpty)
        item.content = Drawing.reminder.content
        item.toldAs = "opaque"
        source.inner.change(item, in: chat)
        try await until { !source.calls("seen").isEmpty }
        XCTAssertEqual(model.place, .chat(chat))
        XCTAssertEqual(model.shownItem, item.id)
        XCTAssertEqual(announcements, [], "the tapped push must not alert again when its reminder syncs")
        item.raised += 1
        source.inner.change(item, in: chat)
        try await until { model.conversation(chat).items.last?.raised == item.raised }
        XCTAssertEqual(source.calls("seen"), ["seen(\(item.id), \(chat))"])
    }

    func testAPendingPushExpiresAtSignOutAndAPushCannotInvokeAnOfferOrUseLocalFields() async throws {
        let source = Recording(Drawing.source())
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.person != nil && model.conversation(.main).isLoaded }
        let fields: [String: Any] = ["told": "opaque", "item": "wrong", "chat": "main", "choice": "yes", "question": "q"]
        XCTAssertNil(NoticeTap(NoticePlan.answerAction, fields: fields, pushed: true))
        XCTAssertNil(NoticeTap(NoticePlan.offerAction + "approve", fields: fields, pushed: true))
        XCTAssertNil(NoticeTap(NoticePlan.openAction, fields: ["told": "opaque", "gone": 1], pushed: true))
        XCTAssertNil(NoticeTap(NoticePlan.openAction, fields: ["item": "wrong", "chat": "main"], pushed: true))
        let tap = try XCTUnwrap(NoticeTap(NoticePlan.openAction, fields: fields, pushed: true))
        tap.take(in: model)
        source.inner.set(.signedOut)
        try await until { model.session == .signedOut }
        tap.take(in: model)
        source.inner.set(.signedIn(Person(id: "other", name: "Other", workspace: "Other")))
        try await until { model.person?.id == "other" }
        var item = Drawing.reminder
        item.toldAs = "opaque"
        source.inner.post(item)
        try await until { model.conversation(.main).items.last?.toldAs == "opaque" }
        XCTAssertNil(model.shownItem)
        XCTAssertTrue(source.calls("seen").isEmpty)
        tap.take(in: model)
        try await until { source.calls("seen").count == 1 }
        XCTAssertEqual(model.shownItem, item.id)

        let local = try XCTUnwrap(NoticeTap(NoticePlan.openAction, fields: fields, pushed: false))
        local.take(in: model)
        try await until { source.calls("seen").count == 2 }
        XCTAssertEqual(model.shownItem, "wrong")
    }

    func testAColdTapFindsTheFirstSnapshotAndTheSameTapCannotOpenACachedPieceAfterSwitchingWorkspaces() async throws {
        var content = Drawing.content()
        var person = Drawing.person
        person.workspaceRoute = "/w/first"
        content.session = .signedIn(person)
        var item = Drawing.reminder
        item.toldAs = "first-workspace"
        content.main = [item]
        let source = Recording(MemorySource(content))
        let model = AssistantModel(source: source)
        let tap = try XCTUnwrap(NoticeTap(NoticePlan.openAction, fields: ["told": "first-workspace"], pushed: true))
        tap.take(in: model)
        model.start()
        try await until { source.calls("seen").count == 1 }
        XCTAssertEqual(model.shownItem, item.id)

        var other = person
        other.workspaceRoute = "/w/second"
        source.inner.set(.signedIn(other))
        try await until { model.person == other }
        tap.take(in: model)
        item.toldAs = "second-workspace"
        source.inner.post(item)
        try await until { model.conversation(.main).items.last?.toldAs == "second-workspace" }
        XCTAssertNil(model.shownItem)
        XCTAssertEqual(source.calls("seen").count, 1)
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
