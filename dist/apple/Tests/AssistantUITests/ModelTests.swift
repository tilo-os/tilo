import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// A snapshot place a check keeps to itself: nothing is written to this Mac's caches, and a check
/// may open a window twice and see what the first one kept.
@MainActor
func cacheOfTheCheck() -> SnapshotCache {
    SnapshotCache(file: URL(filePath: NSTemporaryDirectory(), directoryHint: .isDirectory)
        .appending(path: "snapshot-\(UUID().uuidString).json"))
}

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
        let model = AssistantModel(source: MemorySource(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        try await until { model.conversation(.main).isLoaded }
        model.drafts[.main] = Draft(text: "**thanks**")
        model.send(in: .main)
        XCTAssertNil(model.drafts[.main], "the field is empty at once")
        XCTAssertEqual(model.conversation(.main).items.count, Drawing.mainChat.count + 1,
                       "the message stands at once, before the source has it")
        try await until { source.items().count == Drawing.mainChat.count + 1 }
        // The source's own copy of it takes the place of the one that stood, and the wait for a
        // turn goes on as it did: one message, never two.
        try await until { model.conversation(.main).items.count == Drawing.mainChat.count + 1 }
        try await Task.sleep(for: .milliseconds(300))
        print("D", model.conversation(.main).items.map(\.id), source.items().count)
        XCTAssertEqual(model.conversation(.main).items.count, Drawing.mainChat.count + 1, "one message, never two")
        XCTAssertNil(model.problem)
        source.set(.unreachable)
        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        // The words appear in the conversation at once and stay visible as a retryable failed item.
        XCTAssertEqual(model.conversation(.main).items.count, Drawing.mainChat.count + 2, "the message appears at once")
        let failedItem = try XCTUnwrap(model.conversation(.main).items.last)
        try await until { model.failedSend(for: failedItem.id) != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertEqual(model.failedSend(for: failedItem.id)?.reason, AssistantWords.unreachable.said)
        XCTAssertNil(model.drafts[.main], "the composer stays clear for a new draft")
        XCTAssertEqual(model.conversation(.main).items.count, Drawing.mainChat.count + 2,
                       "failed optimistic item remains visible in the thread")
        XCTAssertEqual(source.items().count, Drawing.mainChat.count + 1)
        try await until { model.reach == .unreachable }
        // Retrying once reachable sends with the same submission id and reconciles cleanly.
        source.set(.reachable)
        model.retrySend(failedItem.id, in: .main)
        try await until { source.items().count == Drawing.mainChat.count + 2 }
        try await until { model.failedSend(for: failedItem.id) == nil }
        XCTAssertEqual(model.conversation(.main).items.count, Drawing.mainChat.count + 2, "one message after retry")
    }

    func testIdenticalShortRepliesAndAttachmentOnlyMessagesNeverCollide() async throws {
        var content = Drawing.content()
        content.main = [Item(id: "earlier-ok", at: Drawing.now.addingTimeInterval(-30), .message(Message(.person, "ok")))]
        let source = MemorySource(content, now: { Drawing.now })
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        try await until { model.conversation(.main).isLoaded }

        // Sending two identical short replies when an older "ok" is already in history never collides.
        model.drafts[.main] = Draft(text: "ok")
        model.send(in: .main)
        model.drafts[.main] = Draft(text: "ok")
        model.send(in: .main)
        XCTAssertEqual(model.conversation(.main).items.count, 3, "older ok plus both new optimistic oks")
        try await until { source.items().count == 3 }
        try await until { model.conversation(.main).items.allSatisfy { !$0.id.value.hasPrefix("local-") } }
        XCTAssertEqual(model.conversation(.main).items.count, 3, "every ok reconciled one-to-one")

        // Attachment-only messages appear optimistically and never collide by empty text.
        let fileA = Draft.File(url: URL(fileURLWithPath: "/tmp/alpha.pdf"))
        let fileB = Draft.File(url: URL(fileURLWithPath: "/tmp/beta.pdf"))
        model.drafts[.main] = Draft(text: "", files: [fileA])
        model.send(in: .main)
        model.drafts[.main] = Draft(text: "", files: [fileB])
        model.send(in: .main)
        XCTAssertEqual(model.conversation(.main).items.count, 5)
        try await until { source.items().count == 5 }
        try await until { model.conversation(.main).items.allSatisfy { !$0.id.value.hasPrefix("local-") } }
        XCTAssertEqual(model.conversation(.main).items.count, 5)
        XCTAssertEqual(model.conversation(.main).items.suffix(2).map { $0.message?.attachments.map(\.name) },
                       [["alpha.pdf"], ["beta.pdf"]])
    }

    func testFailedOptimisticSendPreservesComposerDraftAndSupportsRetryAndDismiss() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(.unreachable)

        model.drafts[.main] = Draft(text: "first failed")
        model.send(in: .main)
        let firstItem = try XCTUnwrap(model.conversation(.main).items.last)
        // User begins typing a new draft before the first send finishes failing.
        model.drafts[.main] = Draft(text: "typing next message")
        try await until { model.failedSend(for: firstItem.id) != nil }
        XCTAssertEqual(model.drafts[.main]?.text, "typing next message", "active draft is never overwritten")

        let originalSubmissionID = try XCTUnwrap(model.failedSend(for: firstItem.id)?.submissionID)
        // Dismissing removes only the failed optimistic item and leaves the draft untouched.
        model.dismissFailedSend(firstItem.id, in: .main)
        XCTAssertNil(model.failedSend(for: firstItem.id))
        XCTAssertFalse(model.conversation(.main).items.contains(where: { $0.id == firstItem.id }))
        XCTAssertEqual(model.drafts[.main]?.text, "typing next message")

        // Sending another message that fails and then retrying keeps the same submission id.
        model.send(in: .main)
        let secondItem = try XCTUnwrap(model.conversation(.main).items.last)
        try await until { model.failedSend(for: secondItem.id) != nil }
        let secondSubmissionID = try XCTUnwrap(model.failedSend(for: secondItem.id)?.submissionID)
        XCTAssertNotEqual(originalSubmissionID, secondSubmissionID)
        // Fail a second time on retry: submission id remains stable.
        model.retrySend(secondItem.id, in: .main)
        try await until { model.failedSend(for: secondItem.id) != nil }
        XCTAssertEqual(model.failedSend(for: secondItem.id)?.submissionID, secondSubmissionID)
    }

    func testMainThreadAndSubAgentModelsCanBeChosenIndependently() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.models = Drawing.models
        content.supervision = Supervision(model: Drawing.models[0].name)
        let started = try await Started.app(content)
        defer { started.end() }
        started.model.loadModels()
        await started.drive.until("models loaded") { started.model.models.count == Drawing.models.count }

        started.model.choose(Drawing.models[1].id, for: .mainThread)
        await started.drive.until("main thread choice reached source") { started.source.calls("choose").count == 1 }
        XCTAssertEqual(started.source.calls("choose").last, "choose(\(Drawing.models[1].id), request)")
        await started.drive.until("main model updated") { started.model.supervision.model == Drawing.models[1].name }

        started.model.choose(Drawing.models[0].id, for: .subAgentTask)
        await started.drive.until("sub-agent choice reached source") { started.source.calls("choose").count == 2 }
        XCTAssertEqual(started.source.calls("choose").last, "choose(\(Drawing.models[0].id), task+subtask)")
        await started.drive.until("sub-agent model tracked independently") { started.model.subAgentModel == Drawing.models[0].name }
        XCTAssertEqual(started.model.supervision.model, Drawing.models[1].name, "main thread model stays independent")
    }

    #if os(macOS)
    func testMacLiveArtifactCardOpensInSplitStudioWithoutInlineWebView() async throws {
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
        await model.prime()
        var inlineMounts = 0
        let thing = LiveThing(id: "brief-doc", title: "Quarterly Plan", chat: .main, reference: "shared:plan")
        let view = AssistantRoot(model: model) {
            MacLiveArtifactCard(thing: thing)
                .frame(width: 360)
                .padding(20)
                .background(Token.content)
        }
        .environment(\.liveThingView, { _ in
            inlineMounts += 1
            return AnyView(Color.clear.frame(height: 60))
        })
        let hosting = NSHostingView(rootView: view)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 400, height: 160), [.borderless])
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        let drive = Drive(window)
        await drive.settle(200)
        XCTAssertEqual(inlineMounts, 0, "Mac inline artifact card never mounts an inline live web view")
        XCTAssertNil(model.stagedLive)
        drive.press("\(thing.called), \(AssistantWords.openInStage.said)", role: "AXButton")
        await drive.until("artifact staged in Split Studio") { model.stagedLive?.id == thing.id }
        XCTAssertEqual(inlineMounts, 0)
    }
    #endif

    /// The words a person presses Return on stand where they go before anything has answered: the
    /// source is still on its way, and the message is already drawn.
    func testWhatThePersonSendsStandsAtOnceBeforeTheSourceHasIt() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        try await until { model.conversation(.main).isLoaded }
        var told: [Item.ID] = []
        model.told = { item, _ in told.append(item.id) }
        let before = model.conversation(.main).items
        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        let drawn = model.conversation(.main).items
        XCTAssertEqual(drawn.count, before.count + 1, "the message is drawn before the source has it")
        XCTAssertEqual(Array(drawn.dropLast()), before, "and stands after what was there")
        XCTAssertEqual(drawn.last?.message?.author, .person)
        XCTAssertEqual(drawn.last?.message?.text, "are you there")

        // The source holds it a moment later: it stands once, where it stood, and nothing is told.
        try await until { source.items().count == before.count + 1 }
        try await until { model.conversation(.main).items.count == before.count + 1 }
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(model.conversation(.main).items.count, before.count + 1, "one message, never two")
        XCTAssertEqual(told, [])
        XCTAssertNil(model.problem)
    }

    /// What the window showed is written down, and an app opened again holds it before anything
    /// of the source has come: the main chat's pieces, the sub-agents, the tasks and the
    /// automations stand in the very first frame.
    func testAnAppOpenedAgainHoldsWhatTheLastOneShowedBeforeTheSourceSaysAnything() async throws {
        let cache = cacheOfTheCheck()
        let first = AssistantModel(source: Drawing.source(), snapshots: cache)
        await first.prime()
        first.loadModels()
        try await until { first.models == Drawing.models }
        // The snapshot is written a moment after the window changed.
        for _ in 0..<400 where cache.read()?.keptModels != Drawing.models { try await Task.sleep(for: .milliseconds(5)) }
        let kept = try XCTUnwrap(cache.read(), "the window was kept")

        let source = MemorySource()
        let again = AssistantModel(source: source, snapshots: cache)
        XCTAssertEqual(again.person?.id, Drawing.person.id, "whose window it was stands before the source speaks")
        XCTAssertEqual(again.conversation(.main).items, Drawing.mainChat, "every piece stands before the source speaks")
        XCTAssertEqual(again.subAgents, Drawing.subAgents)
        XCTAssertEqual(again.tasks, Drawing.tasks)
        XCTAssertEqual(again.automations, Drawing.automations)
        XCTAssertEqual(again.models, Drawing.models, "cached models stand before the source speaks")
        XCTAssertEqual(kept.person, Drawing.person.id)
        // The source says this device is nobody's: what was kept of it is put away.
        await again.prime()
        XCTAssertEqual(again.session, .signedOut)
        XCTAssertTrue(again.conversation(.main).items.isEmpty, "nobody's window shows nothing")
    }

    /// Another person on this device is never shown the pieces of the window that was.
    func testWhatWasKeptIsPutAwayWhenTheSourceSaysThisDeviceIsAnotherPerson() async throws {
        let cache = cacheOfTheCheck()
        let first = AssistantModel(source: Drawing.source(), snapshots: cache)
        await first.prime()
        for _ in 0..<400 where cache.read() == nil { try await Task.sleep(for: .milliseconds(5)) }

        var content = MemorySource.Content()
        content.session = .signedIn(Person(id: "someone-else", name: "Someone Else", workspace: "another"))
        let source = MemorySource(content)
        let other = AssistantModel(source: source, snapshots: cache)
        XCTAssertFalse(other.conversation(.main).items.isEmpty, "the last window stands until the source says whose it is")
        other.start()
        try await until { other.person?.id == "someone-else" && other.conversation(.main).items.isEmpty }
        XCTAssertTrue(other.subAgents.isEmpty && other.tasks.isEmpty && other.automations.isEmpty)
        try await Task.sleep(for: .milliseconds(400))
        let kept = cache.read()
        XCTAssertNotEqual(kept?.person, Drawing.person.id, "their window holds nothing of the one before")
        XCTAssertTrue(kept?.pieces.isEmpty ?? true)
    }

    /// Signing out leaves nothing of the window on the device, and the app opened after it shows
    /// nothing at all.
    func testSigningOutForgetsWhatTheWindowShowed() async throws {
        let cache = cacheOfTheCheck()
        let model = AssistantModel(source: Drawing.source(), snapshots: cache)
        await model.prime()
        for _ in 0..<400 where cache.read() == nil { try await Task.sleep(for: .milliseconds(5)) }
        model.signOut()
        XCTAssertTrue(model.conversation(.main).items.isEmpty, "what stood is taken down at once")
        try await until { cache.read() == nil }
        let again = AssistantModel(source: Drawing.source(), snapshots: cache)
        XCTAssertNil(again.person)
        XCTAssertTrue(again.conversation(.main).items.isEmpty)
    }

    func testWhatArrivesIsToldOnceAndWhatWasThereIsNot() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        let primed = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        await primed.prime()
        XCTAssertEqual(primed.sinceLeft, Drawing.sinceLeft)
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        XCTAssertNil(model.sinceLeft)
        model.start()
        try await until { model.sinceLeft == Drawing.sinceLeft }
        source.setSinceLeft(nil)
        try await until { model.sinceLeft == nil }
        // A source that keeps no such count says nothing of it.
        let plain = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(.unreachable)
        model.toggleReplies(of: "maya", under: "maya", in: .main)
        try await until { model.problem != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertNil(model.openReplies["maya"])
    }

    func testWordsWithNoLinkInThemPairNothing() async {
        let model = AssistantModel(source: MemorySource(), snapshots: cacheOfTheCheck())
        await model.prime()
        model.pair(with: "nothing to open here")
        XCTAssertEqual(model.problem, AssistantWords.noLink.said)
        model.pair(with: "12345", scanned: true)
        XCTAssertEqual(model.problem, AssistantWords.codeNoLink.said)
        XCTAssertEqual(model.session, .signedOut)
    }

    func testALinkWhereTiloDoesNotAnswerLeavesTheDeviceOutAndSaysSo() async throws {
        let model = AssistantModel(source: MemorySource(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        source.setAnswering(false)
    }

    func testWhatSheWritesAgainBeginsTheWaitAnewAndTheLineStandsUnderHerNewestMessage() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
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
        source.setAnswering(false)
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

    func testALiveActivitysPressLeadsToWhatItWasAbout() throws {
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
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
