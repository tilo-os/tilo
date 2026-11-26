#if os(macOS)
import AppKit
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

/// The app's first seconds. Until a first try to get in has ended, nothing is known of whether
/// Tilo answers, and nothing is said of it; what a person sends meanwhile waits.
@MainActor
final class StartTests: XCTestCase {
    func testUntilTheSourceHasSaidItNothingIsKnownOfWhetherTiloAnswers() async {
        let model = AssistantModel(source: MemorySource())
        XCTAssertEqual(model.reach, .unknown, "the window held whether Tilo answers before anything had said it")
        await model.prime()
        XCTAssertEqual(model.reach, .reachable)
    }

    /// The owner's picture: the app right after its start, an empty conversation, and over the
    /// message field "Tilo cannot be reached right now", though nothing had failed.
    func testTheNoticeIsSaidOnlyAfterATryThatFailedAndGoesTheMomentALaterOneGetsIn() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        let notice = AssistantWords.notReachable.said, waiting = AssistantWords.pairing.said

        // The start: not known yet. The quiet word for waiting, and no word of a failure.
        source.set(Reach.unknown)
        await drive.until("the window holds that nothing is known yet") { started.model.reach == .unknown }
        await drive.until("the quiet word for waiting") { drive.shows(waiting) }
        XCTAssertFalse(drive.shows(notice), "the window said that Tilo cannot be reached before a try had ended")
        drive.picture("start-not-known-yet")

        // A try failed: now it is said, and nothing is waited for.
        source.set(Reach.unreachable)
        await drive.until("the notice, after a try that failed") { drive.shows(notice) }
        XCTAssertFalse(drive.shows(waiting), "a try that failed was still said to be waited for")
        drive.picture("start-a-try-failed")

        // A later try gets in: the notice goes with it.
        source.set(Reach.reachable)
        await drive.until("the notice to go") { !drive.shows(notice) }
        XCTAssertFalse(drive.shows(waiting))
        drive.picture("start-in")
    }

    /// A person's words on 18 November 2026: the app said "Waiting for Tilo" and then "Setting up
    /// your assistant", at a start that set nothing up. From opening the app until the main chat
    /// stands a person reads one sentence, through every step the app takes behind it.
    func testFromOpeningTheAppUntilTheMainChatStandsAPersonReadsOneSentence() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .findingAssistant
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        let waiting = AssistantWords.pairing.said, settingUp = "Setting up your assistant"

        // The page and the session: nothing is known yet.
        source.set(Reach.unknown)
        await drive.until("the window holds that nothing is known yet") { started.model.reach == .unknown }
        await drive.until("the one sentence") { drive.shows(waiting) }
        XCTAssertFalse(drive.shows(settingUp), "a start that sets nothing up said that it does")
        drive.picture("start-page-and-session")

        // The page is in, and the app looks for its assistant, as every start does: the same
        // sentence, and no other.
        source.set(Reach.reachable)
        await drive.until("the window holds that Tilo answers") { started.model.reach == .reachable }
        await drive.settle(300)
        XCTAssertTrue(drive.shows(waiting), "the one sentence went before the main chat stood:\n\(drive.said())")
        XCTAssertFalse(drive.shows(settingUp), "the look for an assistant was said as a setting up")
        XCTAssertFalse(drive.shows(AssistantWords.nothingYet.said), "a chat that is still waited for was said to be empty")
        drive.picture("start-the-look")

        // The assistant is found and what answers for it is read: the main chat stands, and
        // nothing is waited for.
        source.set(Setup.ready)
        await drive.until("the sentence to go") { !drive.shows(waiting) }
        XCTAssertFalse(drive.shows(settingUp))
        drive.picture("start-the-main-chat-stands")
    }

    /// The first time, while an assistant is made for a person who keeps none: only then does
    /// the main chat say that their assistant is being set up, and it is the one sentence that stands.
    func testThatAnAssistantIsBeingSetUpIsSaidOnlyWhileOneIsMade() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .makingAssistant
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        await drive.until("the main chat to say that an assistant is being set up") { drive.shows(AssistantWords.makingAssistant.said) }
        XCTAssertFalse(drive.shows(AssistantWords.pairing.said), "two sentences stood at once:\n\(drive.said())")
        XCTAssertFalse(drive.shows(AssistantWords.nothingYet.said), "a chat that says what it waits for is not empty")
        drive.picture("first-time-an-assistant-is-made")
        // It is made, and no provider is linked yet: the step that follows takes the sentence's place.
        source.set(Setup.needsProvider(Drawing.providers))
        await drive.until("the sentence to go") { !drive.shows(AssistantWords.makingAssistant.said) }
        XCTAssertFalse(drive.shows(AssistantWords.pairing.said))
    }

    private func withPermissionWindow<Content: View>(setup: Setup, access: Supervision.AssistantPermission? = nil,
                                                    appChoices: [AssistantAppChoice] = [],
                                                    place: AssistantModel.Place = .chat(.main),
                                                    @ViewBuilder view: () -> Content,
                                                    check: @MainActor (MemorySource, AssistantModel, Drive) async throws -> Void) async rethrows {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = setup
        content.supervision.assistantAccess = access
        content.supervision.assistantAppChoices = appChoices
        let source = MemorySource(content, now: { Drawing.now })
        let model = AssistantModel(source: source)
        model.place = place
        NSApplication.shared.setActivationPolicy(.prohibited)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 800, height: 740), [.borderless])
        let drive = Drive(window)
        window.contentView = NSHostingView(rootView: AssistantRoot(model: model, content: view))
        defer {
            window.contentView = nil
            window.close()
        }
        model.start()
        try await check(source, model, drive)
    }

    func testChatAccessInTheConversationRequiresARequestAndAnExternalApproval() async throws {
        let offers: [(ChatPermission, AssistantWords, AssistantWords)] = [
            (.needed, .chatPermissionScope, .requestChatPermission),
            (.declined, .chatPermissionDeclined, .requestAssistantWorkPermission),
            (.expired, .chatPermissionExpired, .requestChatPermission),
            (.revoked, .chatPermissionRevoked, .requestAssistantWorkPermission),
        ]
        let codeInstruction = try XCTUnwrap(AssistantWords.chatPermissionCode.said
            .components(separatedBy: "{code}").max { $0.count < $1.count })
        for (permission, message, choice) in offers {
            await withPermissionWindow(setup: .chatPermission(permission), view: { ConversationView(chat: .main) }) { _, model, drive in
                await drive.until("the two scopes for \(permission)") { drive.shows(message.said) && drive.has(choice.said, role: "AXButton") }
                XCTAssertEqual(drive.tree().filter(\.isButton).map(\.name).sorted(),
                               [AssistantWords.requestChatPermission.said, AssistantWords.requestAssistantWorkPermission.said].sorted())
                XCTAssertTrue(drive.shows(AssistantWords.chatPermissionScope.said))
                XCTAssertNil(model.supervision.assistantAccess)
                drive.press(choice.said, role: "AXButton")
                await drive.until("the explicit request to reach the workspace") {
                    model.supervision.assistantAccess != nil && drive.shows(AssistantWords.chatPermissionPending.said)
                }
                let apps = choice == .requestChatPermission ? ["so.tilo.chat"]
                    : ["so.tilo.chat", "so.tilo.tasks", "so.tilo.assistant", "so.tilo.shortcuts"]
                XCTAssertEqual(model.supervision.assistantAccess, .init(state: .pending, apps: apps,
                    expires: Drawing.now.addingTimeInterval(6 * 24 * 3600), ready: false))
                XCTAssertEqual(model.setup, .chatPermission(.pending(code: nil)))
                XCTAssertEqual(drive.tree().filter(\.isButton).map(\.name), [AssistantWords.checkChatPermission.said])
                XCTAssertFalse(drive.shows(codeInstruction), "an uncoded request showed a comparison instruction")
            }
        }
    }

    func testTheAppPickerSubmitsOnlyTheSelectedWorkspaceAppsAfterAnExplicitRequest() async throws {
        let chat = AssistantAppChoice(id: "so.tilo.chat", name: "Workspace conversations")
        let future = AssistantAppChoice(id: "org.example.review", name: "Design review")
        let mail = AssistantAppChoice(id: "so.tilo.mail", name: "Team mailbox")
        try await withPermissionWindow(setup: .chatPermission(.needed), appChoices: [chat, future, mail],
                                       view: { ConversationView(chat: .main) }) { _, model, drive in
            let choose = AssistantWords.chooseAssistantApps.said
            let request = AssistantWords.requestSelectedAssistantApps.said
            await drive.until("the workspace app choices") { drive.has(choose, role: "AXButton") }
            XCTAssertTrue(drive.has(AssistantWords.requestChatPermission.said, role: "AXButton"))
            XCTAssertTrue(drive.has(AssistantWords.requestAssistantWorkPermission.said, role: "AXButton"))
            XCTAssertFalse(drive.has(request, role: "AXButton"))
            drive.press(choose, role: "AXButton")
            await drive.until("the optional apps") { drive.has(mail.name, role: "AXCheckBox") && drive.has(future.name, role: "AXCheckBox") }
            let required = try XCTUnwrap(drive.node(AssistantWords.assistantAppRequired.said(["name": chat.name]), role: "AXCheckBox"))
            XCTAssertFalse(required.isEnabled)
            XCTAssertEqual(required.value, "1")
            drive.press(mail.name, role: "AXCheckBox")
            await drive.until("Mail to be checked") { drive.nodes(mail.name, role: "AXCheckBox").first?.value == "1" }
            XCTAssertNil(model.supervision.assistantAccess)
            XCTAssertEqual(model.setup, .chatPermission(.needed))
            drive.press(AssistantWords.cancel.said, role: "AXButton")
            await drive.until("the unsubmitted choices to close") { !drive.has(request, role: "AXButton") }
            XCTAssertNil(model.supervision.assistantAccess)

            drive.press(choose, role: "AXButton")
            await drive.until("a new choice of apps") { drive.has(future.name, role: "AXCheckBox") }
            XCTAssertEqual(drive.nodes(mail.name, role: "AXCheckBox").first?.value, "0")
            drive.press(future.name, role: "AXCheckBox")
            await drive.until("the future app to be checked") { drive.nodes(future.name, role: "AXCheckBox").first?.value == "1" }
            XCTAssertNil(model.supervision.assistantAccess)
            drive.press(request, role: "AXButton")
            await drive.until("the selected scope to await approval") {
                model.setup == .chatPermission(.pending(code: nil)) && model.supervision.assistantAccess?.state == .pending
            }
            XCTAssertEqual(model.supervision.assistantAccess?.apps, [chat.id, future.id])
            XCTAssertFalse(drive.has(choose, role: "AXButton"))
        }

        await withPermissionWindow(setup: .chatPermission(.needed), appChoices: [mail, future],
                                   view: { ConversationView(chat: .main) }) { _, _, drive in
            await drive.until("the Chat preset without the required catalogue entry") { drive.has(AssistantWords.requestChatPermission.said) }
            XCTAssertFalse(drive.has(AssistantWords.chooseAssistantApps.said, role: "AXButton"))
        }
    }

    func testAnExistingGrantCannotBeReplacedThroughTheAppPicker() async {
        let chat = AssistantAppChoice(id: "so.tilo.chat", name: "Workspace conversations")
        let mail = AssistantAppChoice(id: "so.tilo.mail", name: "Team mailbox")
        for state in [Supervision.AssistantPermission.State.pending, .approved] {
            let grant = Supervision.AssistantPermission(state: state, apps: [chat.id], expires: nil, ready: state == .approved)
            await withPermissionWindow(setup: .chatPermission(.needed), access: grant, appChoices: [chat, mail],
                                       view: { ConversationView(chat: .main) }) { _, model, drive in
                await drive.until("the stale setup and the workspace grant to arrive") {
                    model.person != nil && model.setup == .chatPermission(.needed) && model.supervision.assistantAccess == grant
                }
                await drive.settle()
                XCTAssertFalse(drive.has(AssistantWords.chooseAssistantApps.said, role: "AXButton"))
                let replaced = await model.requestAssistantAppPermission(apps: [chat.id, mail.id])
                XCTAssertFalse(replaced)
                XCTAssertEqual(model.supervision.assistantAccess, grant)
            }
        }
    }

    func testCheckingApprovalPreservesThePendingRequestUntilTheWorkspaceAdmitsIt() async {
        let code = "243168"
        let pending = Setup.chatPermission(.pending(code: code))
        await withPermissionWindow(setup: pending, view: { ConversationView(chat: .main) }) { source, model, drive in
            let comparison = AssistantWords.chatPermissionCode.said(["code": code])
            await drive.until("the optional comparison code") { drive.shows(comparison) }
            XCTAssertTrue(drive.shows(AssistantWords.chatPermissionPending.said))
            drive.press(AssistantWords.checkChatPermission.said, role: "AXButton")
            await drive.settle()
            XCTAssertEqual(model.setup, pending)
            XCTAssertTrue(drive.shows(comparison))
            XCTAssertNil(model.problem)

            let uncoded = Setup.chatPermission(.pending(code: ""))
            source.set(uncoded)
            await drive.until("the empty code to disappear") { model.setup == uncoded && !drive.shows(comparison) }
            XCTAssertTrue(drive.shows(AssistantWords.chatPermissionPending.said))
            XCTAssertFalse(drive.shows(AssistantWords.chatPermissionCode.said(["code": ""])))

            let unavailable = ChatPermission.unavailable("Workspace offline")
            source.set(.chatPermission(unavailable))
            await drive.until("the workspace's error") { drive.shows("Workspace offline") }
            XCTAssertTrue(drive.shows(AssistantWords.chatPermissionUnavailable.said))
            XCTAssertEqual(drive.tree().filter(\.isButton).map(\.name), [AssistantWords.tryAgain.said])
            XCTAssertFalse(drive.shows(comparison))
            drive.press(AssistantWords.tryAgain.said, role: "AXButton")
            await drive.settle()
            XCTAssertEqual(model.setup, .chatPermission(unavailable))
            XCTAssertNil(model.problem)

            source.set(.chatPermission(.joining))
            await drive.until("the assistant to join") { drive.shows(AssistantWords.chatPermissionJoining.said) }
            XCTAssertEqual(drive.tree().filter(\.isButton).map(\.name), [AssistantWords.checkChatPermission.said])
            source.set(Setup.ready)
            await drive.until("the empty conversation after admission") { drive.shows(AssistantWords.nothingYet.said) }
            XCTAssertFalse(drive.has(AssistantWords.checkChatPermission.said, role: "AXButton"))
        }
    }

    func testAssistantAccessShowsTheRequestedOrApprovedAppsAndConfirmsBeforeRevoking() async throws {
        let expires = Drawing.now.addingTimeInterval(6 * 24 * 3600)
        let unknown = "example.workspace.custom-app"
        let pending = Supervision.AssistantPermission(state: .pending, apps: [unknown], expires: expires, ready: false)
        let approved = Supervision.AssistantPermission(state: .approved, apps: [], expires: expires, ready: true)
        for permission in [pending, approved] {
            try await withPermissionWindow(setup: .ready, access: permission, place: .assistantView, view: { SupervisionView() }) { _, model, drive in
                let isPending = permission.state == .pending
                let scope = isPending ? AssistantWords.assistantAccessRequestedApps.said(["apps": unknown]) : AssistantWords.assistantAccessAllApps.said
                let status = isPending ? AssistantWords.assistantAccessPending.said : AssistantWords.assistantAccessReady.said
                await drive.until("the permission's actual app scope") { drive.shows(scope) && drive.shows(status) }
                XCTAssertTrue(drive.shows(AssistantWords.assistantAccess.said))
                let expiry = AssistantWords.assistantAccessExpires.said(["date": expires.formatted(date: .abbreviated, time: .shortened)])
                XCTAssertTrue(drive.shows(expiry))
                drive.press(AssistantWords.checkChatPermission.said, role: "AXButton")
                await drive.settle()
                XCTAssertEqual(model.supervision.assistantAccess, permission)

                drive.press(AssistantWords.revokeChatPermission.said, role: "AXButton")
                await drive.until("the revocation confirmation") { !drive.window.heldSheets.isEmpty }
                let alert = try XCTUnwrap(drive.window.heldSheets.first)
                let sheet = try XCTUnwrap(alert.contentView)
                await drive.until("the scope under review") { drive.shows(scope, in: sheet) }
                XCTAssertTrue(drive.shows(AssistantWords.confirmRevokeChatPermission.said, in: sheet))
                XCTAssertEqual(model.supervision.assistantAccess, permission)
                let choice = isPending ? AssistantWords.cancel.said : AssistantWords.revokeChatPermission.said
                let button = try XCTUnwrap(drive.tree(of: sheet).first { $0.isButton && $0.name == choice })
                @MainActor func nativeButton(in view: NSView) -> NSButton? {
                    if let control = view as? NSButton, control.title == choice { return control }
                    return view.subviews.lazy.compactMap { nativeButton(in: $0) }.first
                }
                let control = try XCTUnwrap(button.object as? NSButton ?? nativeButton(in: sheet), "Native alert button for \(choice) (\(type(of: button.object)))")
                XCTAssertTrue(button.isEnabled)
                XCTAssertFalse(button.frame.isEmpty)
                XCTAssertNotNil(control.action)
                let response = NSApplication.ModalResponse(rawValue: control.tag)
                control.performClick(nil)
                // HeldWindow captures beginSheet without attaching the alert. Deliver the pressed
                // NSAlert button's return code to the completion that AppKit registered for it.
                if drive.window.heldSheets.contains(alert) {
                    drive.window.endSheet(alert, returnCode: response)
                }
                await drive.until("the confirmation to close") { drive.window.heldSheets.isEmpty }
                if isPending {
                    XCTAssertEqual(model.supervision.assistantAccess, pending)
                } else {
                    await drive.until("the revoked status") { drive.shows(AssistantWords.chatPermissionRevoked.said) }
                    XCTAssertEqual(model.supervision.assistantAccess?.state, .revoked)
                    XCTAssertFalse(drive.has(AssistantWords.revokeChatPermission.said, role: "AXButton"))
                }
            }
        }
    }

    func testAMessageSentBeforeItIsKnownWaitsAndGoesOutOnceTheTryGetsIn() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(Reach.unknown)
        try await until { model.reach == .unknown }

        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        XCTAssertNil(model.drafts[.main], "the field is empty at once")
        // It waits: nothing has gone out, and nothing is said to have failed.
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertNil(model.problem, "a message sent in the first seconds was said not to have gone")
        XCTAssertNil(model.drafts[.main])
        XCTAssertEqual(source.items().count, Drawing.mainChat.count)

        source.set(Reach.reachable)
        try await until { source.items().count == Drawing.mainChat.count + 1 }
        XCTAssertNil(model.problem)
        XCTAssertNil(model.drafts[.main])
    }

    func testAMessageSentBeforeItIsKnownIsBackInTheFieldWithTheReasonWhereTheTryFails() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(Reach.unknown)
        try await until { model.reach == .unknown }

        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertNil(model.problem, "a message sent in the first seconds was said not to have gone before the try ended")
        source.set(Reach.unreachable)
        try await until { model.problem != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertEqual(model.drafts[.main]?.text, "are you there", "what did not go through is back in the field")
        XCTAssertEqual(source.items().count, Drawing.mainChat.count)
    }
}
#endif
