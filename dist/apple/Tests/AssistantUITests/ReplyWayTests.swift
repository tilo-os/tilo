#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The way back to the provider step from the failure itself. Under the newest reply whose model
/// gave no answer, where the person's server says the cause is the key, the key's account or a
/// missing provider, the main chat offers one press that shows the app's one protected field
/// again. What is offered follows the typed cause a reply carries and never the reply's words.
/// Everything here is made up: the people, the providers' models and every key.
@MainActor
final class ReplyWayTests: XCTestCase {
    private let marker = "Zq7-a-made-up-key-typed-after-the-failure-8hH"
    private let openrouter = ProviderChoice(id: "p-7", name: "OpenRouter", isLinked: true)
    private let gemini = ProviderChoice(id: "gemini", name: "Google Gemini")
    /// As their server says it of a reply whose provider refused the key's account.
    private let refused = Message.KeyFault.key("p-7", name: "OpenRouter")

    private func asked(_ id: Item.ID, _ text: String) -> Item {
        Item(id: id, at: Drawing.now, .message(Message(.person, text)))
    }

    private func replied(_ id: Item.ID, _ text: String, _ fault: Message.KeyFault? = nil) -> Item {
        Item(id: id, at: Drawing.now, .message(Message(.assistant, text, keyFault: fault)))
    }

    /// A person whose assistant answers on a model of the one provider they linked, with this
    /// in their main chat.
    private func person(_ main: [Item], providers: [ProviderChoice]? = nil, setup: Setup = .ready) -> MemorySource.Content {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.main = main
        content.supervision = Supervision(model: "Claude Opus 5.5")
        content.models = Drawing.models
        content.providers = providers ?? [openrouter, gemini]
        content.offered = [ModelChoice(id: "google/gemini-3-flash", name: "Gemini 3 Flash", provider: "Google")]
        content.setup = setup
        return content
    }

    private func model(_ content: MemorySource.Content) async -> AssistantModel {
        let model = AssistantModel(source: MemorySource(content))
        await model.prime()
        return model
    }

    private func until(_ what: String, _ holds: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async throws {
        for _ in 0..<200 {
            if holds() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("waited two seconds for \(what)", file: file, line: line)
        throw CancellationError()
    }

    private func secureField(of started: Started, file: StaticString = #filePath, line: UInt = #line) throws -> NSSecureTextField {
        func find(_ view: NSView) -> NSSecureTextField? {
            if let found = view as? NSSecureTextField { return found }
            for sub in view.subviews { if let found = find(sub) { return found } }
            return nil
        }
        return try XCTUnwrap(started.window.contentView.flatMap(find), "the window has no protected field", file: file, line: line)
    }

    func testUnderAReplyThatFailedForTheKeyOnePressShowsTheProviderStepAgainAndTheNewKeyReachesOnlyTheLink() async throws {
        // The reply's words name no cause: what is offered comes from the typed cause it carries.
        let started = try await Started.app(person([asked("hello", "Hello?"), replied("no-answer", "These words name no cause.", refused)]))
        defer { started.end() }
        let drive = started.drive
        let press = AssistantWords.anotherKey.said(["name": "OpenRouter"])
        // Under the failure stands the one press. The main chat has no field for a key, and nothing
        // read the person's providers: the assistant view was never opened.
        await drive.until("the press under the reply that failed") { drive.has(press, role: "AXButton") }
        XCTAssertEqual(started.model.replyWay(in: .main), AssistantModel.ReplyWay(item: "no-answer", way: .anotherKey(openrouter)))
        XCTAssertNil(drive.tree().first { $0.subrole == "AXSecureTextField" }, drive.said())
        XCTAssertEqual(started.source.calls("providers"), [])
        drive.press(press, role: "AXButton")
        await drive.until("the provider step in the main chat again") {
            drive.shows(AssistantWords.anotherKeyStep.said(["name": "OpenRouter"]))
        }
        XCTAssertEqual(started.model.place, .chat(.main))
        XCTAssertEqual(started.model.keyStep?.picked, "p-7")
        XCTAssertEqual(started.source.calls("providers"), ["providers()"], "the step offers what their server lists at the press")
        // One protected field, for the provider the failure named; while the step is shown the press is not offered beside it.
        XCTAssertEqual(drive.tree().filter { $0.subrole == "AXSecureTextField" }.map(\.label),
                       [AssistantWords.providerKey.said(["name": "OpenRouter"])])
        XCTAssertFalse(drive.has(press, role: "AXButton"), drive.said())
        let use = AssistantWords.useKey.said(["name": "OpenRouter"])
        drive.click(try secureField(of: started))
        drive.type(marker)
        await drive.until("the key to be offered to the link") { drive.node(use, role: "AXButton")?.isEnabled ?? false }
        XCTAssertFalse(drive.said().contains(marker), "the window reads the key out")
        drive.press(use, role: "AXButton")
        await drive.until("the vault to be handed the key") { started.source.inner.key(for: "p-7") != nil }
        XCTAssertEqual(started.source.inner.key(for: "p-7")?.handOver(), marker, "the vault gets every letter that was typed")
        // The one call that carried it, to the provider the failure named, and it says nothing of the key.
        XCTAssertEqual(started.source.calls("link"), ["link(p-7, Secret)"])
        for call in started.source.calls { XCTAssertFalse(call.contains(marker), "a call carried the key: \(call.prefix(40))") }
        // The step is done with, and nothing is offered under that reply again: its key was typed anew.
        await drive.until("the step to be done with") { started.model.keyStep == nil }
        await drive.until("the press to be gone") { !drive.has(press, role: "AXButton") }
        XCTAssertNil(started.model.replyWay(in: .main))
        XCTAssertNil(drive.tree().first { $0.subrole == "AXSecureTextField" }, "a key that is kept leaves no field behind")
        XCTAssertEqual(started.source.calls("choose"), [])
        XCTAssertEqual(started.model.setup, .ready)
        XCTAssertFalse(drive.said().contains(marker))
    }

    func testThePressStandsUnderTheNewestReplyOnlyAndFollowsTheTypedCauseNeverTheWords() async throws {
        // A failure's words offer nothing by themselves: their server said no cause with them.
        let worded = await model(person([asked("a", "Hello?"), replied("b", "My model did not answer: Provider credits exhausted")]))
        XCTAssertNil(worded.replyWay(in: .main))
        // The typed cause offers the way, whatever the words are.
        let typed = await model(person([asked("a", "Hello?"), replied("b", "All is well.", refused)]))
        XCTAssertEqual(typed.replyWay(in: .main), AssistantModel.ReplyWay(item: "b", way: .anotherKey(openrouter)))
        XCTAssertEqual(typed.replyWay(in: .main)?.way.words, "Use another key for OpenRouter")
        // The step stands in the main chat, so no other conversation offers it.
        XCTAssertNil(typed.replyWay(in: .subAgent("s-1")))
        // Of two replies that failed the newer carries it, and a message of the person's own after it changes nothing.
        let two = await model(person([asked("a", "Hello?"), replied("b", "No.", refused), asked("c", "Again?"), replied("d", "No.", refused),
                                      asked("e", "And now?")]))
        XCTAssertEqual(two.replyWay(in: .main)?.item, "d")
        // An answer came after the failure: nothing is offered under what is past.
        let answered = await model(person([asked("a", "Hello?"), replied("b", "No.", refused), asked("c", "Again?"), replied("d", "Good morning.")]))
        XCTAssertNil(answered.replyWay(in: .main))
        // No provider was there to ask: the way to link one.
        let none = await model(person([asked("a", "Hello?"), replied("b", "No.", .noProvider)]))
        XCTAssertEqual(none.replyWay(in: .main), AssistantModel.ReplyWay(item: "b", way: .anotherProvider))
        XCTAssertEqual(none.replyWay(in: .main)?.way.words, "Link another provider")
        // At the first start the main chat shows the provider step by itself: nothing is offered beside it.
        let first = await model(person([asked("a", "Hello?"), replied("b", "No.", .noProvider)], setup: .needsProvider([gemini])))
        XCTAssertNil(first.replyWay(in: .main))
    }

    func testTheStepOffersWhatTheirServerListsWhenThePressIsTaken() async throws {
        // The provider lost its key since: the step is shown with it picked, to be linked.
        let unkeyed = ProviderChoice(id: "p-7", name: "OpenRouter")
        let lost = await model(person([asked("a", "Hello?"), replied("b", "No.", refused)], providers: [unkeyed, gemini]))
        lost.take(try XCTUnwrap(lost.replyWay(in: .main)))
        try await until("the step for the provider that lost its key") { lost.keyStep != nil }
        XCTAssertEqual(lost.keyStep, AssistantModel.KeyStep(providers: [unkeyed, gemini], picked: "p-7"))
        XCTAssertEqual(lost.place, .chat(.main))
        // Their server lists the provider no longer: the step is shown with one that can be linked.
        let gone = await model(person([asked("a", "Hello?"), replied("b", "No.", refused)], providers: [gemini]))
        gone.take(try XCTUnwrap(gone.replyWay(in: .main)))
        try await until("the step with a provider that can be linked") { gone.keyStep != nil }
        XCTAssertEqual(gone.keyStep?.picked, "gemini")
        // No provider was there to ask: the first that is not linked. Not now puts the step away,
        // and since nothing was typed the way is offered under the reply as before.
        let none = await model(person([asked("a", "Hello?"), replied("b", "No.", .noProvider)]))
        none.take(try XCTUnwrap(none.replyWay(in: .main)))
        try await until("the step with a provider that is not linked") { none.keyStep != nil }
        XCTAssertEqual(none.keyStep?.picked, "gemini")
        XCTAssertNil(none.replyWay(in: .main), "the step is shown: nothing is offered beside it")
        none.putAwayKeyStep()
        XCTAssertEqual(none.replyWay(in: .main)?.item, "b")
    }

    func testAKeyTypedForTheProviderThatFailedEndsTheOfferAndANewerFailureOffersItAnew() async throws {
        let source = MemorySource(person([asked("a", "Hello?"), replied("b", "No.", refused)]))
        let model = AssistantModel(source: source)
        model.start()
        try await until("the main chat to be held") { model.conversation(.main).isLoaded }
        XCTAssertEqual(model.replyWay(in: .main)?.item, "b")
        // A key for another provider mends nothing of this one's: the way stays offered.
        let other = await model.link("gemini", key: Secret("a-made-up-key-for-another-provider"))
        XCTAssertTrue(other)
        XCTAssertEqual(model.replyWay(in: .main)?.item, "b")
        // A key for the provider that failed: nothing is offered under that reply again.
        let same = await model.link("p-7", key: Secret("a-made-up-key-for-the-provider-that-failed"))
        XCTAssertTrue(same)
        XCTAssertNil(model.replyWay(in: .main))
        // The next reply that fails the same way offers it anew.
        source.post(asked("c", "Hello again?"))
        source.post(replied("d", "No.", refused))
        try await until("the newer failure to offer the way") { model.replyWay(in: .main)?.item == "d" }
    }
}
#endif
