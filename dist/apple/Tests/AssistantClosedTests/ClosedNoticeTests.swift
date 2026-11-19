import AssistantClosed
import AssistantCore
import XCTest

/// What the notice extension makes of a push: the server's words where the sealed part opens
/// with this device's key, and nothing where it does not, so that the plain line stands.
final class ClosedNoticeTests: XCTestCase {
    private let key = ClosedKey.made()
    private let said = #"{"from":"Travel helper","words":"May I send the form?","important":true,"# + #""actions":[{"id":"allow","label":"Allow","main":true},{"id":"deny","label":"Deny","main":false}]}"#

    func testAPushThatOpensShowsWhoToldItTheMarkAndTheServersWords() throws {
        let name = key.name(of: "question:ws-1:m_ask_1")
        let push = try XCTUnwrap(ClosedPush(Sealing.push(named: name, saying: said, for: key)))
        XCTAssertEqual(push.name, name)
        XCTAssertEqual(push.place, 12)
        XCTAssertEqual(push.kind, "question")
        XCTAssertFalse(push.isGone)
        let opened = try XCTUnwrap(push.opened(with: key))
        XCTAssertEqual(opened.title, "Travel helper")
        XCTAssertEqual(opened.subtitle, AssistantWords.important.said)
        XCTAssertEqual(opened.words, "May I send the form?")
        XCTAssertEqual(opened.category, "assistant.decision")
    }

    func testTheCategoryIsTheKindsOwnWhichTheAppGivesAtEveryStart() throws {
        let given = Set(Notice.Kind.allCases.map { "assistant.\($0.rawValue)" })
        for (kind, category) in [("question", "assistant.decision"), ("reminder", "assistant.reminder"), ("brief", "assistant.brief"),
                                 ("news", "assistant.important"), ("something-new", "assistant.important")] {
            let push = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", kind: kind, saying: said, for: key)))
            XCTAssertEqual(push.opened(with: key)?.category, category, kind)
            XCTAssertTrue(given.contains(category), category)
        }
    }

    func testWhatIsNotMarkedCarriesNoMarkAndWhatNamesNobodyIsTheAssistants() throws {
        let plain = #"{"from":"  ","words":"Stretch now.","important":false,"actions":[]}"#
        let push = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", kind: "reminder", saying: plain, for: key)))
        let opened = try XCTUnwrap(push.opened(with: key))
        XCTAssertEqual(opened.title, AssistantWords.assistant.said)
        XCTAssertNil(opened.subtitle)
        XCTAssertEqual(opened.words, "Stretch now.")
    }

    func testThePlainLineStandsWhereNothingIsSealedOrNothingOpens() throws {
        // A workspace whose words stay on its server: the push carries no sealed part.
        let bare = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", saying: nil, for: key)))
        XCTAssertNil(bare.sealed)
        XCTAssertNil(bare.opened(with: key))
        // Sealed for the key this device had before its person signed in anew.
        let old = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", saying: said, for: ClosedKey.made())))
        XCTAssertNil(old.opened(with: key))
        // Not sealed at all, no words, or not what a server writes.
        var fields = try Sealing.push(named: "n", saying: nil, for: key)
        fields["enc"] = "bm90IHNlYWxlZA=="
        XCTAssertNil(ClosedPush(fields)?.opened(with: key))
        fields["enc"] = "not even base64"
        XCTAssertNil(ClosedPush(fields)?.opened(with: key))
        for nothing in [#"{"from":"A","words":"  ","important":false,"actions":[]}"#, #"{"title":"A"}"#, "[]"] {
            XCTAssertNil(try XCTUnwrap(ClosedPush(Sealing.push(named: "n", saying: nothing, for: key))).opened(with: key), nothing)
        }
    }

    func testAPushThatTakesAThingBackShowsNothingAndNamesTheNoticeToTakeAway() throws {
        let gone = try XCTUnwrap(ClosedPush(["aps": ["content-available": 1], "told": "rf-qsLfA_59buuS5z2pKfA", "gone": true]))
        XCTAssertTrue(gone.isGone)
        XCTAssertEqual(gone.name, "rf-qsLfA_59buuS5z2pKfA")
        XCTAssertNil(gone.opened(with: key))
    }

    func testANotificationThatIsNoPushOfTilosIsLeftAlone() {
        XCTAssertNil(ClosedPush(["aps": ["alert": "Hello"]]))
        XCTAssertNil(ClosedPush(["told": ""]))
        XCTAssertNil(ClosedPush(["told": 7]))
        XCTAssertNil(ClosedPush([:]))
    }

    func testRemotePrivacyFilteredPushRoutesThroughOpaqueToldTag() throws {
        let tag = key.name(of: "question:ws-1:m_ask_1")
        let payload = try Sealing.push(named: tag, kind: "question", saying: nil, for: key)
        let route = try XCTUnwrap(ClosedRoute(userInfo: payload))
        XCTAssertTrue(route.isPrivacyFiltered)
        XCTAssertEqual(route.target, .told(tag, kind: "question"))
        XCTAssertEqual(route.told, tag)
        XCTAssertEqual(route.kind, "question")

        var waiting = Item(
            id: "ws-1/chat.messages/messages/m_ask_1",
            at: Date(timeIntervalSince1970: 0),
            .question(Question(id: "m_ask_1", text: "Send the form?", choices: [Choice(id: "allow", title: "Allow", isMain: true)]))
        )
        waiting.toldAs = tag
        let resolved = route.resolve(conversations: [.main: [waiting]], tasks: [])
        XCTAssertEqual(resolved, .item(waiting.id, chat: .main))
    }

    func testRemotePushParsesExplicitItemDecisionAndTaskIdentifiers() throws {
        let decisionPayload: [AnyHashable: Any] = [
            "aps": ["alert": ["loc-key": "TOLD_WAITS"]],
            "tilo": ["decision_id": "m_ask_2", "sub_agent": "code-review"],
        ]
        let decisionRoute = try XCTUnwrap(ClosedRoute(userInfo: decisionPayload))
        XCTAssertFalse(decisionRoute.isPrivacyFiltered)
        XCTAssertEqual(decisionRoute.target, .decision("m_ask_2", chat: .subAgent("code-review")))

        let subItem = Item(
            id: "ws-1/chat.messages/messages/m_ask_2",
            at: Date(timeIntervalSince1970: 0),
            .question(Question(id: "ws-1/chat.messages/messages/m_ask_2", text: "Apply patch?", choices: []))
        )
        XCTAssertEqual(
            decisionRoute.resolve(conversations: [.subAgent("code-review"): [subItem]], tasks: []),
            .item(subItem.id, chat: .subAgent("code-review"))
        )

        let taskPayload: [AnyHashable: Any] = ["aps": ["alert": "Update"], "task_id": "task-42"]
        let taskRoute = try XCTUnwrap(ClosedRoute(userInfo: taskPayload))
        XCTAssertEqual(taskRoute.target, .task("task-42"))
        let task = AssistantTask(id: "task-42", title: "Review draft", state: .needsInput, item: "m-42")
        XCTAssertEqual(taskRoute.resolve(conversations: [:], tasks: [task]), .task("task-42", item: "m-42", state: .needsInput))
    }

    func testWithdrawnAndPrivateOrInvalidPayloadsAreFilteredOut() {
        XCTAssertNil(ClosedRoute(userInfo: ["told": "rf-qsLfA_59buuS5z2pKfA", "gone": true]))
        XCTAssertNil(ClosedRoute(userInfo: ["aps": ["alert": "Hello"], "enc": "secret-blob"]))
        XCTAssertNil(ClosedRoute(userInfo: ["item": "bad\nidentifier"]))
        XCTAssertNil(ClosedRoute(userInfo: ["item": String(repeating: "a", count: 300)]))
        XCTAssertNil(ClosedRoute(userInfo: [:], requestIdentifier: "unrelated-uuid"))
    }
}
