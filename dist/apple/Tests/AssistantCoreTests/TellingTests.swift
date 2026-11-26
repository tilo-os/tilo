import AssistantCore
import AssistantDrawing
import XCTest

/// What is told outside the window: notifications and the live activity.
final class TellingTests: XCTestCase {
    func testOnlyWhatThePersonChoseInterruptsThem() throws {
        let notices = Drawing.mainChat.map { NoticePlan.notice(for: $0, in: .main) }
        // The rundown, their own message and the plain answer are told in the conversation only.
        XCTAssertNil(notices[0])
        XCTAssertNil(notices[1])
        XCTAssertNil(notices[4])
        XCTAssertNil(notices[5])

        // A report after it acted alone does not interrupt, and says why.
        let quiet = try XCTUnwrap(notices[2])
        XCTAssertEqual(quiet.kind, .quiet)
        XCTAssertFalse(quiet.interrupts)
        XCTAssertEqual(quiet.body, "Dima's mail did not notify you: it is news, so it waits in the conversation.")
        XCTAssertNil(quiet.answer)

        // A decision that waits interrupts, carries the mark, and offers the first answer.
        let decision = try XCTUnwrap(notices[3])
        XCTAssertEqual(decision.kind, .decision)
        XCTAssertTrue(decision.interrupts)
        XCTAssertEqual(decision.title, "Assistant")
        XCTAssertEqual(decision.subtitle, "Important")
        XCTAssertEqual(decision.body, "Maya at Northwind asks for the October numbers. A reply is drafted.",
                       "a notification says what the assistant chose to say there")
        var plain = Drawing.mainChat[3]
        plain.notice = nil
        XCTAssertEqual(NoticePlan.notice(for: plain, in: .main)?.body, "Maya, who invested in your seed, asks for the October numbers. I drafted a reply with the figures from the metrics sheet.")
        XCTAssertEqual(decision.answer, Notice.Answer(question: "maya-reply", choice: "send", title: "Send it"))
        XCTAssertEqual(NoticePlan.category(of: decision), "assistant.decision.Send it")
    }

    func testWhatItsServerOffersStandsUnderTheNoticeTheExpectedOneFirst() throws {
        var asked = Drawing.mainChat[3]
        asked.offers = [Offer(id: "later", title: "Not now"), Offer(id: "send", title: "Send it", isMain: true),
                        Offer(id: "show", title: "Show me the draft"), Offer(id: "more", title: "One more")]
        let notice = try XCTUnwrap(NoticePlan.notice(for: asked, in: .main))
        XCTAssertEqual(notice.offers.map(\.title), ["Send it", "Not now", "Show me the draft"], "the system shows a handful, and Open is one")
        XCTAssertEqual(NoticePlan.category(of: notice), "assistant.decision.offers.send=Send it|later=Not now|show=Show me the draft")
        XCTAssertNil(notice.offeredBy)
        XCTAssertEqual(notice.words, notice.body)
        // Without anything offered a question keeps its own first answer.
        let plain = try XCTUnwrap(NoticePlan.notice(for: Drawing.mainChat[3], in: .main))
        XCTAssertEqual(plain.offers, [])
        XCTAssertEqual(NoticePlan.category(of: plain), "assistant.decision.Send it")

        // What an agent offers with its own news names the agent with it.
        var news = Item(id: "n", at: Drawing.now, .message(Message(.assistant, "The draft is ready.")), isImportant: true,
                        offers: [Offer(id: "show-draft", title: "Show the draft", isMain: true, by: "Scout")])
        let told = try XCTUnwrap(NoticePlan.notice(for: news, in: .main))
        XCTAssertEqual(told.kind, .important)
        XCTAssertEqual(told.offers.map(\.id), ["show-draft"])
        XCTAssertEqual(told.words, "The draft is ready.\nScout offers this")
        // What did not interrupt offers nothing to press.
        news.isImportant = false
        news.keptQuiet = "This did not notify you: it is news."
        let quiet = try XCTUnwrap(NoticePlan.notice(for: news, in: .main))
        XCTAssertEqual(quiet.kind, .quiet)
        XCTAssertEqual(quiet.offers, [])
        XCTAssertEqual(quiet.words, "This did not notify you: it is news.")
    }

    func testWhatCameSinceThePersonLeftStandsBeforeTheFirstPieceThatCameAfter() {
        var conversation = Conversation()
        conversation.apply(.all(Drawing.mainChat))
        XCTAssertEqual(conversation.first(since: Drawing.at(8, 5)), "dima")
        XCTAssertEqual(conversation.first(since: Drawing.at(7, 0)), "rundown-1")
        XCTAssertNil(conversation.first(since: Drawing.at(11, 0)), "nothing held came after: it stands at the foot")
        // It stands before everything that belongs to that piece: here the mark of its time.
        let rows = conversation.rows(in: .main, Drawing.wording)
        XCTAssertEqual(rows.first { $0.item == "dima" }?.id, "mark-dima")
        XCTAssertEqual(rows.filter { $0.item == "dima" }.map(\.id), ["mark-dima", "task-dima", "piece-dima"])
        XCTAssertFalse(Drawing.sinceLeft.isEmpty)
        XCTAssertTrue(SinceLeft(left: Drawing.now).isEmpty)
    }

    func testAReminderAndABriefInterrupt() throws {
        let reminder = try XCTUnwrap(NoticePlan.notice(for: Drawing.reminder, in: .main))
        XCTAssertEqual(reminder.kind, .reminder)
        XCTAssertEqual(reminder.body, "Call Jonas.")
        XCTAssertNil(reminder.subtitle)
        XCTAssertEqual(NoticePlan.category(of: reminder), "assistant.reminder")

        let brief = try XCTUnwrap(NoticePlan.notice(for: Drawing.briefPiece[0], in: .main))
        XCTAssertEqual(brief.kind, .brief)
        XCTAssertEqual(brief.subtitle, "Important")
        XCTAssertTrue(brief.body.hasPrefix("Northwind call in 15 minutes\nRundown on the fund: Northwind led your seed"))
    }

    func testAnAnsweredQuestionASecretAndASubAgentsNews() throws {
        var answered = Drawing.mayaQuestion
        answered.answer = "send"
        XCTAssertEqual(NoticePlan.notice(for: Item(id: "a", at: Drawing.now, .question(answered), isImportant: true), in: .main)?.kind,
                       .important, "nothing waits any more, and it is still important")
        XCTAssertNil(NoticePlan.notice(for: Item(id: "a", at: Drawing.now, .question(answered)), in: .main))

        // A secret is asked for with no button that answers: it is typed in the app only.
        let secret = try XCTUnwrap(NoticePlan.notice(for: Drawing.secretPiece[0], in: .main))
        XCTAssertEqual(secret.kind, .decision)
        XCTAssertNil(secret.answer)
        XCTAssertEqual(NoticePlan.notice(for: Drawing.connectionPiece[1], in: .main)?.kind, .decision)

        // A sub-agent's news does not interrupt; its question does.
        let chat = Chat.subAgent("usage-drop")
        let news = Drawing.usageDropChat.map { NoticePlan.notice(for: $0, in: chat)?.kind }
        XCTAssertEqual(news, [nil, nil, nil, nil, nil, .decision])
    }

    func testWhatEarnsALiveActivity() throws {
        func plan(at now: Date, items: [Item] = Drawing.mainChat, agents: [SubAgent] = Drawing.subAgents,
                  automations: [Automation] = Drawing.automations) -> [String: LiveFacts] {
            LiveActivityPlan.activities(tasks: Drawing.tasks, subAgents: agents, automations: automations, items: items, now: now)
        }
        // At 9:44 the reminder of 10:14 counts down, and the important task that waits is there.
        let morning = plan(at: Drawing.now)
        XCTAssertEqual(Set(morning.keys), ["reminder-remind-jonas", "task-maya"])
        XCTAssertEqual(morning["reminder-remind-jonas"],
                       LiveFacts(kind: .reminder, title: "Remind: call Jonas", until: Drawing.at(10, 14)))
        let maya = try XCTUnwrap(morning["task-maya"])
        XCTAssertEqual(maya.state, .needsYou)
        XCTAssertFalse(LiveActivityPlan.mayBegin(maya), "work born needing the person is told by a notification")
        XCTAssertTrue(LiveActivityPlan.mayBegin(try XCTUnwrap(morning["reminder-remind-jonas"])))

        // The daily rundown comes again and again: it earns none. Nor does a paused reminder,
        // one that has come, or one that is hours away.
        XCTAssertNil(plan(at: Drawing.at(7, 0, day: 18))["reminder-morning-rundown"])
        var paused = Drawing.automations
        paused[0].isOn = false
        XCTAssertNil(plan(at: Drawing.now, automations: paused)["reminder-remind-jonas"])
        XCTAssertNil(plan(at: Drawing.at(10, 14))["reminder-remind-jonas"])
        XCTAssertNil(plan(at: Drawing.at(8, 0))["reminder-remind-jonas"])

        // A brief shows its points until what it prepares for begins.
        let brief = try XCTUnwrap(plan(at: Drawing.at(10, 15), items: Drawing.briefPiece)["brief-brief"])
        XCTAssertEqual(brief.kind, .brief)
        XCTAssertEqual(brief.until, Drawing.at(10, 30))
        XCTAssertEqual(brief.points, ["Northwind led your seed with 1.2 million.",
                                      "Maya asked for the October numbers on Monday. The reply waits for you.",
                                      "Last call was 3 October. You promised the hiring plan."])
        XCTAssertTrue(brief.isImportant)
        XCTAssertNil(plan(at: Drawing.at(10, 30), items: Drawing.briefPiece)["brief-brief"])

        // A sub-agent the person started is theirs to wait on; the main agent's own are not.
        var agents = Drawing.subAgents
        agents[0].startedBy = .person
        let work = try XCTUnwrap(plan(at: Drawing.now, agents: agents)["sub-agent-usage-drop"])
        XCTAssertEqual(work, LiveFacts(kind: .work, state: .running, title: "Usage drop: investigate", detail: "Coder"))
        XCTAssertTrue(LiveActivityPlan.mayBegin(work))
        agents[0].state = .done
        XCTAssertNil(plan(at: Drawing.now, agents: agents)["sub-agent-usage-drop"])
    }

    func testAnActivitysFactsCrossAsPlainData() throws {
        let facts = LiveFacts(kind: .brief, state: .needsYou, title: "Call", detail: "soon", points: ["a"],
                              until: Drawing.at(10, 30), isImportant: true)
        let data = try JSONEncoder().encode(facts)
        XCTAssertEqual(try JSONDecoder().decode(LiveFacts.self, from: data), facts)
    }
}
