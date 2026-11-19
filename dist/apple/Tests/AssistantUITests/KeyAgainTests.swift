#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The way back to the provider step. A person who linked a provider comes back to the app's one
/// protected field from the assistant view's Model line, for another key or another provider.
/// Everything here is made up: the people, the providers' models and every key.
@MainActor
final class KeyAgainTests: XCTestCase {
    private let marker = "Zq7-another-made-up-key-8hH"
    private let anthropic = ProviderChoice(id: "anthropic", name: "Anthropic", isLinked: true)
    private let gemini = ProviderChoice(id: "gemini", name: "Google Gemini")

    /// A person whose assistant answers on a model of the one provider they linked, and whose
    /// main chat holds a message and the answer that its model gave none.
    private var linked: MemorySource.Content {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.main = [Item(id: "hello", at: Drawing.now, .message(Message(.person, "Hello?"))),
                        Item(id: "no-answer", at: Drawing.now, .message(Message(.assistant, "My model did not answer.")))]
        content.supervision = Supervision(model: "Claude Opus 5.5")
        content.models = Drawing.models
        content.providers = [anthropic, gemini]
        content.offered = [ModelChoice(id: "google/gemini-3-flash", name: "Gemini 3 Flash", provider: "Google")]
        return content
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

    func testTheModelLineOffersAnotherKeyForEachLinkedProviderAndAnotherProviderWhereOneIsLeft() async throws {
        let model = AssistantModel(source: MemorySource(linked), snapshots: cacheOfTheCheck())
        await model.prime()
        XCTAssertEqual(model.keyWays, [], "nothing is offered before the person's providers are read")
        model.loadModels()
        try await until("the person's providers to be read") { model.providers == [self.anthropic, self.gemini] }
        XCTAssertEqual(model.keyWays, [.anotherKey(anthropic), .anotherProvider])
        XCTAssertEqual(model.keyWays.map(\.words), ["Use another key for Anthropic", "Link another provider"])

        // Where every provider is linked, each can be given another key and none is left to link.
        var both = linked
        both.providers = [anthropic, ProviderChoice(id: "gemini", name: "Google Gemini", isLinked: true)]
        let every = AssistantModel(source: MemorySource(both), snapshots: cacheOfTheCheck())
        await every.prime()
        every.loadModels()
        try await until("both providers to be read") { every.providers.count == 2 }
        XCTAssertEqual(every.keyWays.map(\.words), ["Use another key for Anthropic", "Use another key for Google Gemini"])

        // Where none is linked the main chat shows the step by itself: the Model line adds no way of its own.
        var none = linked
        none.providers = [gemini]
        let unlinked = AssistantModel(source: MemorySource(none), snapshots: cacheOfTheCheck())
        await unlinked.prime()
        unlinked.loadModels()
        try await until("the provider to be read") { unlinked.providers == [self.gemini] }
        XCTAssertEqual(unlinked.keyWays, [])
    }

    func testAnotherKeyShowsTheProviderStepInTheMainChatAgainAndTheNewKeyReachesOnlyTheLink() async throws {
        let started = try await Started.app(linked)
        defer { started.end() }
        let drive = started.drive
        let board = NSPasteboard.general
        let count = board.changeCount
        // The main chat of a person whose assistant answers has no field for a key.
        XCTAssertNil(drive.tree().first { $0.subrole == "AXSecureTextField" }, drive.said())
        // The assistant view reads the person's providers; its Model line's Change is there to open.
        drive.press(AssistantWords.assistantView.said, role: "AXButton")
        await drive.until("the assistant view") { started.model.place == .assistantView }
        await drive.until("the person's providers to be read") { started.model.keyWays == [.anotherKey(self.anthropic), .anotherProvider] }
        XCTAssertTrue(drive.tree().contains { $0.role == "AXMenuButton" && $0.name == AssistantWords.change.said }, drive.said())
        // The menu's line "Use another key for Anthropic". A menu is a window of the system's own,
        // which no test puts on a screen: what the line does is done.
        started.model.take(.anotherKey(anthropic))
        await drive.until("the main chat") { started.model.place == .chat(.main) }
        await drive.until("the provider step in the main chat again") {
            drive.shows(AssistantWords.anotherKeyStep.said(["name": "Anthropic"]))
        }
        XCTAssertFalse(drive.shows(AssistantWords.needsProvider.said), "a provider is linked, and the step does not say that none is")
        // One protected field, announced by what it is for, and the button says whose key this becomes.
        XCTAssertEqual(drive.tree().filter { $0.subrole == "AXSecureTextField" }.map(\.label),
                       [AssistantWords.providerKey.said(["name": "Anthropic"])])
        let use = AssistantWords.useKey.said(["name": "Anthropic"])
        XCTAssertFalse(drive.node(use, role: "AXButton")?.isEnabled ?? true, "nothing to hand over before something is typed")
        drive.click(try secureField(of: started))
        drive.type(marker)
        await drive.until("the key to be offered to the link") { drive.node(use, role: "AXButton")?.isEnabled ?? false }
        XCTAssertFalse(drive.said().contains(marker), "the window reads the key out")
        drive.press(use, role: "AXButton")
        await drive.until("the vault to be handed the key") { started.source.inner.key(for: "anthropic") != nil }
        XCTAssertEqual(started.source.inner.key(for: "anthropic")?.handOver(), marker, "the vault gets every letter that was typed")
        // The one call that carried it, to the provider that was picked, and it says nothing of the key.
        XCTAssertEqual(started.source.calls("link"), ["link(anthropic, Secret)"])
        for call in started.source.calls { XCTAssertFalse(call.contains(marker), "a call carried the key: \(call.prefix(40))") }
        // The model step follows as at the first start. The model the person chose stands: it is
        // said, nothing chose another, and nothing is in the way of an answer.
        await drive.until("the model step to follow") { drive.shows(AssistantWords.keyTaken.said) }
        XCTAssertNil(started.model.keyStep)
        XCTAssertNil(drive.tree().first { $0.subrole == "AXSecureTextField" }, "a key that is kept leaves no field behind")
        XCTAssertTrue(drive.shows(AssistantWords.answersOn.said(["model": "Claude Opus 5.5"])), drive.said())
        XCTAssertTrue(drive.tree().contains { $0.role == "AXMenuButton" && $0.name == AssistantWords.change.said }, drive.said())
        XCTAssertEqual(started.source.calls("choose"), [])
        XCTAssertEqual(started.model.supervision.model, "Claude Opus 5.5")
        XCTAssertEqual(started.model.setup, .ready)
        // Nothing of the window, the model or the pasteboard holds the key.
        XCTAssertFalse(drive.said().contains(marker))
        var held = ""
        dump([started.model.drafts as Any, started.model.conversations as Any, started.model.models as Any, started.model.providers as Any,
              started.model.keyStep as Any, started.model.problem as Any], to: &held)
        XCTAssertFalse(held.contains(marker))
        XCTAssertEqual(board.changeCount, count, "something was put on the pasteboard")
    }

    func testLinkAnotherProviderShowsTheStepWithAProviderThatIsNotLinkedAndNotNowPutsItAway() async throws {
        let started = try await Started.app(linked)
        defer { started.end() }
        let drive = started.drive
        started.model.loadModels()
        await drive.until("the person's providers to be read") { started.model.keyWays.count == 2 }
        // The menu's line "Link another provider": the step stands with the first provider that is not linked.
        started.model.take(.anotherProvider)
        await drive.until("the provider step in the main chat again") { drive.shows(AssistantWords.anotherProviderStep.said) }
        let link = AssistantWords.linkProvider.said(["name": "Google Gemini"])
        XCTAssertNotNil(drive.node(link, role: "AXButton"), drive.said())
        XCTAssertEqual(drive.tree().filter { $0.subrole == "AXSecureTextField" }.map(\.label),
                       [AssistantWords.providerKey.said(["name": "Google Gemini"])])
        // The linked provider can be picked in the step as well: then the key typed is another key for it.
        drive.press("Anthropic", role: "AXButton")
        await drive.until("the linked provider to be the one picked") {
            drive.node(AssistantWords.useKey.said(["name": "Anthropic"]), role: "AXButton") != nil
        }
        XCTAssertTrue(drive.shows(AssistantWords.anotherKeyStep.said(["name": "Anthropic"])), drive.said())
        // Not now puts the step away: no field is left, nothing was typed and nothing was linked.
        drive.press(AssistantWords.notNow.said, role: "AXButton")
        await drive.until("the step to be put away") { started.model.keyStep == nil && !drive.tree().contains { $0.subrole == "AXSecureTextField" } }
        XCTAssertEqual(started.source.calls("link"), [])
        XCTAssertFalse(drive.shows(AssistantWords.keyTaken.said), "nothing was linked, so no model step follows")

        // Asked for again, the other provider is linked with a key of its own.
        started.model.take(.anotherProvider)
        await drive.until("the step again") { drive.node(link, role: "AXButton") != nil }
        drive.click(try secureField(of: started))
        drive.type(marker)
        await drive.until("the link to be offered") { drive.node(link, role: "AXButton")?.isEnabled ?? false }
        drive.press(link, role: "AXButton")
        await drive.until("the vault to be handed the key") { started.source.inner.key(for: "gemini") != nil }
        XCTAssertEqual(started.source.inner.key(for: "gemini")?.handOver(), marker)
        XCTAssertNil(started.source.inner.key(for: "anthropic"), "a key typed for one provider is no other provider's")
        XCTAssertEqual(started.source.calls("link"), ["link(gemini, Secret)"])
        // Both are linked from then on: each can be given another key, and none is left to link.
        await drive.until("both providers to be linked") {
            started.model.keyWays.map(\.words) == ["Use another key for Anthropic", "Use another key for Google Gemini"]
        }
        // The models of the provider that was linked are offered beside the ones the person had, and their choice stands.
        await drive.until("the model step to follow") { drive.shows(AssistantWords.keyTaken.said) }
        XCTAssertEqual(started.model.models.map(\.name), ["Claude Opus 5.5", "Gemini 3 Pro", "Gemini 3 Flash"])
        XCTAssertEqual(started.source.calls("choose"), [])
        XCTAssertEqual(started.model.supervision.model, "Claude Opus 5.5")
        XCTAssertFalse(drive.said().contains(marker))
    }
}
#endif
