import AssistantCore
import AssistantDrawing
import XCTest

private func first<Value: Sendable>(_ stream: AsyncStream<Value>) async -> Value? {
    for await value in stream { return value }
    return nil
}

/// What a stream says now.
private func now<Value: Sendable>(_ stream: AsyncStream<Value>) async throws -> Value {
    let value = await first(stream)
    return try XCTUnwrap(value)
}

/// The next `count` values of a stream.
private func take<Value: Sendable>(_ count: Int, of stream: AsyncStream<Value>) async -> [Value] {
    var taken: [Value] = []
    for await value in stream {
        taken.append(value)
        if taken.count == count { break }
    }
    return taken
}

final class SourceTests: XCTestCase {
    private func refused(_ call: () async throws -> Void, _ wanted: SourceProblem? = nil,
                         file: StaticString = #filePath, line: UInt = #line) async {
        do {
            try await call()
            XCTFail("the call went through", file: file, line: line)
        } catch let problem as SourceProblem {
            if let wanted { XCTAssertEqual(problem, wanted, file: file, line: line) }
            if wanted == nil, case .refused = problem {} else if wanted == nil {
                XCTFail("refused for another reason: \(problem)", file: file, line: line)
            }
        } catch {
            XCTFail("\(error)", file: file, line: line)
        }
    }

    func testRetryKeepsTheDraftsDeliveryIdAndEditingStartsAnotherMessage() {
        let original = Draft(text: "Hello")
        var retry = original
        XCTAssertEqual(original.delivery, retry.delivery)
        retry.text = "Hello"
        XCTAssertEqual(original.delivery, retry.delivery)
        retry.text += " again"
        XCTAssertNotEqual(original.delivery, retry.delivery)
        XCTAssertNotEqual(original.delivery, Draft(text: "Hello").delivery)
    }

    func testASourceStartsEmptyAndSignedOut() async {
        let source = MemorySource()
        let session = await first(source.session())
        XCTAssertEqual(session, .signedOut)
        let conversation = await first(source.conversation(.main))
        XCTAssertEqual(conversation, .all([]))
        let agents = await first(source.subAgents())
        XCTAssertEqual(agents, [])
        let tasks = await first(source.tasks())
        XCTAssertEqual(tasks, [])
        let automations = await first(source.automations())
        XCTAssertEqual(automations, [])
        let facts = await first(source.supervision())
        XCTAssertEqual(facts, Supervision())
        XCTAssertNil(facts?.model, "there is no model before the person links a provider")
    }

    func testAPairingShowsItsCodeToCompareBeforeTheDeviceIsIn() async throws {
        var content = MemorySource.Content()
        content.pairsAs = Drawing.person
        content.pairingCode = "481 207"
        let source = MemorySource(content)
        let link = try XCTUnwrap(PairingLink(in: "https://tilo.example/pair#abc"))
        async let seen = take(4, of: source.session())
        try await Task.sleep(for: .milliseconds(50))
        try await source.pair(with: link)
        let states = await seen
        XCTAssertEqual(states, [.signedOut, .pairing, .confirming(code: "481 207"), .signedIn(Drawing.person)])
        // A pairing that shows no code goes on as before.
        var plain = MemorySource.Content()
        plain.pairsAs = Drawing.person
        let quiet = MemorySource(plain)
        async let before = take(3, of: quiet.session())
        try await Task.sleep(for: .milliseconds(50))
        try await quiet.pair(with: link)
        let old = await before
        XCTAssertEqual(old, [.signedOut, .pairing, .signedIn(Drawing.person)])
    }

    func testAPairingThatAsksFirstGoesOnOnlyWhenThePersonAnswers() async throws {
        var content = MemorySource.Content()
        content.pairsAs = Drawing.person
        content.pairingAsk = Drawing.pairingAsk
        let link = try XCTUnwrap(PairingLink(in: "https://tilo.example/pair#abc"))
        let source = MemorySource(content)
        async let seen = take(4, of: source.session())
        try await Task.sleep(for: .milliseconds(50))
        try await source.pair(with: link)
        // The link alone lets nobody in: the question stands until it is answered.
        try await Task.sleep(for: .milliseconds(50))
        await source.answerPairing(goOn: true)
        let stood = await seen
        XCTAssertEqual(stood, [.signedOut, .asked(Drawing.pairingAsk), .pairing, .signedIn(Drawing.person)])
        // Stopping leaves the device out.
        let other = MemorySource(content)
        async let stopped = take(3, of: other.session())
        try await Task.sleep(for: .milliseconds(50))
        try await other.pair(with: link)
        await other.answerPairing(goOn: false)
        let out = await stopped
        XCTAssertEqual(out, [.signedOut, .asked(Drawing.pairingAsk), .signedOut])
    }

    func testAProviderIsLinkedWithAKeyThatGoesNowhereElseAndAModelOfItIsChosen() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .needsProvider(Drawing.providers)
        content.offered = Drawing.models
        let source = MemorySource(content)
        await refused({ _ = try await source.link("anthropic", key: Secret("")) }, .refused("Nothing was typed."))
        await refused({ _ = try await source.link("nobody", key: Secret("made-up")) }, .refused("This provider cannot be linked."))
        let offered = try await source.link("anthropic", key: Secret("made-up-key"))
        XCTAssertEqual(offered, Drawing.models)
        XCTAssertEqual(source.key(for: "anthropic")?.handOver(), "made-up-key")
        let needs = await take(1, of: source.setup())
        XCTAssertEqual(needs, [.needsModel(Drawing.models)])
        try await source.choose(Drawing.models[0].id, for: .request)
        let ready = await take(1, of: source.setup())
        XCTAssertEqual(ready, [.ready])
        let facts = await take(1, of: source.supervision())
        XCTAssertEqual(facts.first?.model, Drawing.models[0].name)
        // A source with no first run of its own has nothing in the way.
        struct Plain: AssistantSource {
            func session() -> AsyncStream<Session> { AsyncStream { $0.finish() } }
            func reach() -> AsyncStream<Reach> { AsyncStream { $0.finish() } }
            func pair(with link: PairingLink) async throws {}
            func signOut() async {}
            func conversation(_ chat: Chat) -> AsyncStream<ConversationChange> { AsyncStream { $0.finish() } }
            func earlier(in chat: Chat, before item: Item.ID) async throws -> [Item] { [] }
            func send(_ draft: Draft, to chat: Chat) async throws {}
            func answer(_ question: Question.ID, with choice: Choice.ID) async throws {}
            func keep(_ secret: Secret, for request: SecretRequest.ID) async throws {}
            func fetchSecret(for request: SecretRequest.ID) async throws {}
            func decline(_ request: SecretRequest.ID) async throws {}
            func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL? { nil }
            func decline(_ offer: ConnectionOffer.ID) async throws {}
            func file(of attachment: Attachment.ID) async throws -> URL { URL(fileURLWithPath: "/") }
            func subAgents() -> AsyncStream<[SubAgent]> { AsyncStream { $0.finish() } }
            func startSubAgent(job: String) async throws {}
            func stop(_ subAgent: SubAgent.ID) async throws {}
            func tasks() -> AsyncStream<[AssistantTask]> { AsyncStream { $0.finish() } }
            func addTask(_ title: String, delivery: UUID) async throws {}
            func automations() -> AsyncStream<[Automation]> { AsyncStream { $0.finish() } }
            func setOn(_ on: Bool, for automation: Automation.ID) async throws {}
            func supervision() -> AsyncStream<Supervision> { AsyncStream { $0.finish() } }
            func models() async throws -> [ModelChoice] { [] }
            func choose(_ model: ModelChoice.ID, for work: [WorkKind]) async throws {}
            func forget(_ learned: Supervision.Learned.ID) async throws {}
        }
        let plain = await take(1, of: Plain().setup())
        XCTAssertEqual(plain, [.ready])
        await refused({ _ = try await Plain().link("anthropic", key: Secret("made-up")) }, .refused("No provider can be linked here"))
    }

    func testATasksRepliesAreHandedOverWhenAskedFor() async throws {
        let source = Drawing.source()
        let replies = try await source.replies(of: "maya", in: .main)
        XCTAssertEqual(replies, Drawing.mayaReplies)
        XCTAssertEqual(replies.count, 2, "as many as the task's line counts")
        guard case .replies(let counted)? = Drawing.mainChat.first(where: { $0.id == "maya" })?.task?.state else { return XCTFail() }
        XCTAssertEqual(counted, replies.count)
        // They are no part of the conversation, and a task without any has none.
        XCTAssertFalse(source.items().contains { item in replies.contains { $0.id == item.id } })
        let none = try await source.replies(of: "morning-rundown", in: .main)
        XCTAssertEqual(none, [])
        // Nobody signed in, nothing handed over.
        await refused({ _ = try await MemorySource().replies(of: "maya", in: .main) }, .signedOut)
    }

    /// A source written before replies were asked for still answers: it has none to show.
    func testASourceThatKeepsNoRepliesHasNoneToShow() async throws {
        struct Plain: AssistantSource {
            let inner = MemorySource()
            func session() -> AsyncStream<Session> { inner.session() }
            func reach() -> AsyncStream<Reach> { inner.reach() }
            func pair(with link: PairingLink) async throws {}
            func signOut() async {}
            func conversation(_ chat: Chat) -> AsyncStream<ConversationChange> { inner.conversation(chat) }
            func earlier(in chat: Chat, before item: Item.ID) async throws -> [Item] { [] }
            func send(_ draft: Draft, to chat: Chat) async throws {}
            func answer(_ question: Question.ID, with choice: Choice.ID) async throws {}
            func keep(_ secret: Secret, for request: SecretRequest.ID) async throws {}
            func fetchSecret(for request: SecretRequest.ID) async throws {}
            func decline(_ request: SecretRequest.ID) async throws {}
            func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL? { nil }
            func decline(_ offer: ConnectionOffer.ID) async throws {}
            func file(of attachment: Attachment.ID) async throws -> URL { throw SourceProblem.unreachable }
            func subAgents() -> AsyncStream<[SubAgent]> { inner.subAgents() }
            func startSubAgent(job: String) async throws {}
            func stop(_ subAgent: SubAgent.ID) async throws {}
            func tasks() -> AsyncStream<[AssistantTask]> { inner.tasks() }
            func addTask(_ title: String, delivery: UUID) async throws {}
            func automations() -> AsyncStream<[Automation]> { inner.automations() }
            func setOn(_ on: Bool, for automation: Automation.ID) async throws {}
            func supervision() -> AsyncStream<Supervision> { inner.supervision() }
            func models() async throws -> [ModelChoice] { [] }
            func choose(_ model: ModelChoice.ID, for work: [WorkKind]) async throws {}
            func forget(_ learned: Supervision.Learned.ID) async throws {}
        }
        let replies = try await Plain().replies(of: "maya", in: .main)
        XCTAssertEqual(replies, [])
    }

    func testNothingIsTakenBeforeSomeoneIsSignedIn() async {
        let source = MemorySource()
        await refused({ try await source.send(Draft(text: "hello"), to: .main) }, .signedOut)
        await refused({ try await source.startSubAgent(job: "look") }, .signedOut)
        await refused({ try await source.setOn(false, for: "investor-writes") }, .signedOut)
        XCTAssertEqual(source.items(), [])
    }

    func testALinkPairsTheDeviceWhereTiloAnswers() async throws {
        var content = MemorySource.Content()
        content.pairsAs = Drawing.person
        let source = MemorySource(content)
        let link = try XCTUnwrap(PairingLink(in: "https://tilo.example/pair#abc"))
        try await source.pair(with: link)
        let session = await first(source.session())
        XCTAssertEqual(session, .signedIn(Drawing.person))
        await source.signOut()
        let after = await first(source.session())
        XCTAssertEqual(after, .signedOut)
        // Where Tilo does not answer, pairing says so and the device stays out.
        let away = MemorySource()
        await refused({ try await away.pair(with: link) }, .unreachable)
        let still = await first(away.session())
        XCTAssertEqual(still, .signedOut)
    }

    func testAPairingLinkIsTakenOutOfTheWordsAroundIt() {
        XCTAssertEqual(PairingLink(in: "Open this: <https://tilo.example/pair?t=1#/w/a>, then wait.")?.url.absoluteString,
                       "https://tilo.example/pair?t=1#/w/a")
        XCTAssertEqual(PairingLink(in: "http://tilo.localhost:24120/#/pair/x")?.url.host, "tilo.localhost")
        for words in ["", "no link here", "ftp://tilo.example/x", "https://", "https://someone@tilo.example/pair", "tilo.example/pair"] {
            XCTAssertNil(PairingLink(in: words), words)
        }
    }

    func testWhatThePersonSendsArrivesAsTheirMessage() async throws {
        let source = Drawing.source()
        let stream = source.conversation(.main)
        async let changes = take(2, of: stream)
        try await Task.sleep(for: .milliseconds(20))
        try await source.send(Draft(text: "**thanks**"), to: .main)
        let taken = await changes
        XCTAssertEqual(taken.first, .all(Drawing.mainChat))
        guard case .added(let added) = taken.last, let item = added.first, case .message(let message) = item.content
        else { return XCTFail("the message is added") }
        XCTAssertEqual(message, Message(.person, "**thanks**"))
        XCTAssertEqual(item.at, Drawing.now)
        // Nothing but words and files is a message: an empty one is not sent.
        try await source.send(Draft(text: "  \n"), to: .main)
        XCTAssertEqual(source.items().count, Drawing.mainChat.count + 1)
    }

    func testAThingThatIsThePersonsToEndIsEndedAndNoOtherIs() async throws {
        let source = Drawing.source()
        let theirs = LiveThing(id: "poem", title: "A poem", reference: "shared:poem", theirsToEnd: true)
        let other = LiveThing(id: "letter", title: "A letter", reference: "shared:letter")
        source.post(Item(id: "shared", at: Drawing.now, .message(Message(.assistant, "The poem is above.", live: [theirs]))))
        source.post(Item(id: "other", at: Drawing.now, .message(Message(.assistant, "A letter.", live: [other]))))
        func live(_ id: Item.ID) -> [LiveThing] {
            guard case .message(let message) = source.items().first(where: { $0.id == id })?.content else { return [] }
            return message.live
        }
        // A thing that does not say it is theirs to end is not ended, and stays.
        do {
            try await source.stopSharing(other.id)
            XCTFail("a thing that is not theirs to end was ended")
        } catch let problem as SourceProblem {
            guard case .refused = problem else { return XCTFail("refused, not \(problem)") }
        }
        XCTAssertEqual(live("other"), [other])
        XCTAssertEqual(live("shared"), [theirs])
        // The one that is theirs is ended: its place is gone, its message stays, the other stays.
        try await source.stopSharing(theirs.id)
        XCTAssertEqual(live("shared"), [])
        XCTAssertTrue(source.items().contains { $0.id == "shared" })
        XCTAssertEqual(live("other"), [other])
    }

    func testAFileSentCanBeHadAgain() async throws {
        let source = Drawing.source()
        let file = URL(fileURLWithPath: "/tmp/numbers.csv")
        try await source.send(Draft(text: "", files: [Draft.File(url: file)]), to: .main)
        guard case .message(let message) = source.items().last?.content, let attachment = message.attachments.first
        else { return XCTFail("the file came with a message") }
        XCTAssertEqual(attachment.name, "numbers.csv")
        let found = try await source.file(of: attachment.id)
        XCTAssertEqual(found, file)
        await refused { _ = try await source.file(of: "none") }
    }

    func testAnAnswerSettlesAQuestionOnce() async throws {
        let source = Drawing.source()
        try await source.answer(Drawing.mayaQuestion.id, with: "send")
        let item = try XCTUnwrap(source.items().first { $0.id == "maya" })
        guard case .question(let asked) = item.content else { return XCTFail("still a question") }
        XCTAssertEqual(asked.answer, "send")
        XCTAssertFalse(item.waits)
        await refused { try await source.answer(Drawing.mayaQuestion.id, with: "later") }
        await refused { try await source.answer("nobody-asked", with: "send") }
        // A sub-agent's question is answered the same way, and an answer it never offered is refused.
        await refused { try await source.answer("safari-fix", with: "send") }
        try await source.answer("safari-fix", with: "stop")
        XCTAssertFalse(source.items(in: .subAgent("usage-drop")).contains(where: \.waits))
    }

    func testASecretGoesToTheVaultAndNowhereElse() async throws {
        var content = Drawing.content()
        content.main = Drawing.secretPiece
        let source = MemorySource(content)
        let typed = "correct horse battery staple"
        let secret = Secret(typed)
        // It says nothing of itself, however it is looked at.
        var dumped = ""
        dump(secret, to: &dumped)
        for said in ["\(secret)", String(describing: secret), String(reflecting: secret), dumped,
                     String(describing: [secret]), String(reflecting: Optional(secret))] {
            XCTAssertFalse(said.contains("correct"), said)
            XCTAssertFalse(said.contains("\(typed.utf8.first ?? 0), "), said)
        }
        XCTAssertTrue(Mirror(reflecting: secret).children.isEmpty)

        try await source.keep(secret, for: "linkedin")
        XCTAssertEqual(source.kept(for: "linkedin")?.handOver(), typed)
        // The conversation knows that it was kept, and holds no letter of it.
        let items = source.items()
        XCTAssertEqual(items.first?.content.secret?.state, .kept)
        var written = ""
        dump(items, to: &written)
        XCTAssertFalse(written.contains("correct"))
        XCTAssertFalse(written.contains("staple"))
        await refused { try await source.keep(Secret("again"), for: "linkedin") }
    }

    func testAnEmptySecretIsNotKeptAndARequestCanBeDeclinedOrFetched() async throws {
        var content = Drawing.content()
        content.main = Drawing.secretPiece + [
            Item(id: "other", at: Drawing.now, .secret(SecretRequest(id: "no-manager", text: "A key.", name: "Service"))),
            Item(id: "third", at: Drawing.now, .secret(SecretRequest(id: "third", text: "A key.", name: "Third"))),
        ]
        let source = MemorySource(content)
        await refused { try await source.keep(Secret(""), for: "linkedin") }
        XCTAssertNil(source.kept(for: "linkedin"))
        XCTAssertEqual(source.items()[0].content.secret?.state, .waiting)
        try await source.fetchSecret(for: "linkedin")
        XCTAssertEqual(source.items()[0].content.secret?.state, .kept)
        XCTAssertNil(source.kept(for: "linkedin"), "what the manager holds is fetched when needed, not kept here")
        await refused { try await source.fetchSecret(for: "no-manager") }
        try await source.decline(SecretRequest.ID("third"))
        XCTAssertEqual(source.items()[2].content.secret?.state, .declined)
    }

    func testAServiceIsSignedInToOrLeft() async throws {
        var content = Drawing.content()
        content.main = Drawing.connectionPiece
        let source = MemorySource(content)
        let page = try await source.authenticate("linear")
        XCTAssertNil(page)
        XCTAssertEqual(source.items()[1].content.connection?.state, .connected)
        await refused { try await source.decline(ConnectionOffer.ID("linear")) }
        let other = MemorySource(content)
        try await other.decline(ConnectionOffer.ID("linear"))
        XCTAssertEqual(other.items()[1].content.connection?.state, .declined)
    }

    func testAnEventsCardIsAnsweredAsAQuestion() async throws {
        var content = Drawing.content()
        content.main = Drawing.eventPiece
        let source = MemorySource(content)
        try await source.answer("investor-event", with: "ask")
        guard case .event(let card) = source.items()[1].content else { return XCTFail("still the card") }
        XCTAssertEqual(card.answer, "ask")
        await refused { try await source.answer("investor-event", with: "nonsense") }
    }

    func testASubAgentIsStartedWithItsJobAndStopped() async throws {
        let source = Drawing.source()
        let tasksBefore = await first(source.tasks())
        try await source.startSubAgent(job: "  Compare the two decks  ")
        let agents = try await now(source.subAgents())
        XCTAssertEqual(agents.count, Drawing.subAgents.count + 1)
        let made = agents[0]
        XCTAssertEqual(made.title, "Compare the two decks")
        XCTAssertEqual(made.startedBy, .person)
        XCTAssertEqual(made.state, .working)
        XCTAssertEqual(made.role, "", "the main agent picks the role, not the person")
        guard case .all(let pieces) = await first(source.conversation(.subAgent(made.id))),
              case .message(let job) = pieces.first?.content
        else { return XCTFail("its conversation begins with the job") }
        XCTAssertEqual(job, Message(.mainAgent(.job), "Compare the two decks"))
        // A task is a task: starting a sub-agent adds none.
        let tasksAfter = await first(source.tasks())
        XCTAssertEqual(tasksAfter, tasksBefore)

        try await source.send(Draft(text: "start with the newer one"), to: .subAgent(made.id))
        XCTAssertEqual(source.items(in: .subAgent(made.id)).count, 2)
        XCTAssertEqual(source.items().count, Drawing.mainChat.count, "what is written to a sub-agent is not in the main chat")

        try await source.stop(made.id)
        let after = try await now(source.subAgents())
        XCTAssertEqual(after[0].state, .stopped)
        XCTAssertEqual(after[0].finished, Drawing.now)
        await refused { try await source.stop(made.id) }
        await refused { try await source.stop("data-room") }
        await refused { try await source.send(Draft(text: "x"), to: .subAgent("nobody")) }
    }

    func testThePanelsGroupsAreTheDrawings() async throws {
        let tasks = try await now(Drawing.source().tasks())
        func titles(_ state: AssistantTask.State) -> [String] { tasks.filter { $0.state == state }.map(\.title) }
        XCTAssertEqual(titles(.readyForReview), ["Reply to Maya at Northwind", "Board deck for Friday: first draft"])
        XCTAssertEqual(titles(.needsInput), ["Find the Q3 board deck"])
        XCTAssertEqual(titles(.inProgress), ["Find out why usage dropped", "Shortlist the YC batch for the thesis"])
        XCTAssertEqual(titles(.upNext) + titles(.resolved), [])
        // A task is a task, however many sub-agents it takes: four sub-agents, and none of them is listed.
        XCTAssertEqual(tasks.count, 5)
        XCTAssertEqual(Drawing.subAgents.count, 4)
    }

    func testATaskIsAddedAndAScheduledThingPaused() async throws {
        let source = Drawing.source()
        try await source.addTask(" Finish the report ")
        let tasks = try await now(source.tasks())
        XCTAssertEqual(tasks.last?.title, "Finish the report")
        XCTAssertEqual(tasks.last?.state, .upNext)

        try await source.setOn(false, for: "investor-writes")
        var automations = try await now(source.automations())
        XCTAssertEqual(automations.filter { !$0.isOn }.map(\.id), ["investor-writes"])
        try await source.setOn(true, for: "investor-writes")
        automations = try await now(source.automations())
        XCTAssertTrue(automations.allSatisfy(\.isOn))
        await refused { try await source.setOn(false, for: "nothing") }
    }

    func testTheAssistantModelIsChosenAndAFactForgotten() async throws {
        let source = Drawing.source()
        let models = try await source.models()
        XCTAssertEqual(models.map(\.name), ["Claude Opus 5.5", "Gemini 3 Pro"])
        try await source.choose(models[1].id, for: .request)
        try await source.choose(models[0].id, for: .subtask)
        var facts = try await now(source.supervision())
        XCTAssertEqual(facts.model, "Gemini 3 Pro", "the view names the model that answers in the main chat")
        await refused { try await source.choose("someone-elses", for: .request) }
        try await source.forget("dima")
        facts = try await now(source.supervision())
        XCTAssertEqual(facts.learned.map(\.id), ["sign"])
        XCTAssertEqual(facts.learnedCount, 47)
    }

    func testWhileTiloIsAwayCallsSaySoAndReadsStayAsTheyWere() async throws {
        let source = Drawing.source()
        source.set(.unreachable)
        let reach = await first(source.reach())
        XCTAssertEqual(reach, .unreachable)
        await refused({ try await source.send(Draft(text: "hello"), to: .main) }, .unreachable)
        let conversation = await first(source.conversation(.main))
        XCTAssertEqual(conversation, .all(Drawing.mainChat))
        source.set(.reachable)
        try await source.send(Draft(text: "hello"), to: .main)
    }

    func testEveryListenerFollowsTheSameConversation() async throws {
        let source = Drawing.source()
        async let one = take(4, of: source.conversation(.main))
        async let two = take(4, of: source.conversation(.main))
        try await Task.sleep(for: .milliseconds(20))
        source.setAnswering(true)
        let reply = Item(id: "reply", at: Drawing.now, .message(Message(.assistant, "Done.")))
        source.post(reply)
        source.setAnswering(false)
        let (heardOne, heardTwo) = await (one, two)
        XCTAssertEqual(heardOne, [.all(Drawing.mainChat), .answering(true), .added([reply]), .answering(false)])
        XCTAssertEqual(heardOne, heardTwo)
        // Someone who comes later begins with everything, the reply included.
        let late = await first(source.conversation(.main))
        XCTAssertEqual(late, .all(Drawing.mainChat + [reply]))
    }
}
