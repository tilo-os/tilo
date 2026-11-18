#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import OSLog
import SwiftUI
import XCTest

/// Everything the process writes to its two outputs while it is listened to.
private final class Outputs {
    private let file = FileManager.default.temporaryDirectory.appendingPathComponent("frame-proof-\(UUID().uuidString).txt")
    private var saved: (Int32, Int32)?

    func listen() {
        fflush(stdout)
        fflush(stderr)
        FileManager.default.createFile(atPath: file.path, contents: nil)
        guard let handle = FileHandle(forWritingAtPath: file.path) else { return }
        saved = (dup(STDOUT_FILENO), dup(STDERR_FILENO))
        dup2(handle.fileDescriptor, STDOUT_FILENO)
        dup2(handle.fileDescriptor, STDERR_FILENO)
        try? handle.close()
    }

    /// Stop listening and say what was written. It goes on to where it would have gone.
    func heard() -> String {
        fflush(stdout)
        fflush(stderr)
        guard let saved else { return "" }
        dup2(saved.0, STDOUT_FILENO)
        dup2(saved.1, STDERR_FILENO)
        close(saved.0)
        close(saved.1)
        self.saved = nil
        let written = (try? String(contentsOf: file, encoding: .utf8)) ?? ""
        try? FileManager.default.removeItem(at: file)
        print(written, terminator: "")
        return written
    }
}

/// The protected field: what is typed into it reaches the source's "keep this secret" call and
/// nothing else. Each test types a marker and then looks for it everywhere it must not be.
@MainActor
final class SecretTests: XCTestCase {
    private let marker = "Zq7-marker-9f3KxW"

    private var asking: MemorySource.Content {
        var content = Drawing.content()
        content.main = Drawing.secretPiece
        content.tasks = []
        return content
    }

    /// The protected field of the window, as the system knows it.
    private func protected(_ run: Started, file: StaticString = #filePath, line: UInt = #line) throws -> (Node, NSSecureTextField) {
        let node = try XCTUnwrap(run.drive.tree().first { $0.subrole == "AXSecureTextField" },
                                 "nothing in the window is a protected field:\n\(run.drive.said())", file: file, line: line)
        func find(_ view: NSView) -> NSSecureTextField? {
            if let found = view as? NSSecureTextField { return found }
            for sub in view.subviews { if let found = find(sub) { return found } }
            return nil
        }
        return (node, try XCTUnwrap(run.window.contentView.flatMap(find), file: file, line: line))
    }

    /// What each view of a window would write down for the window's next start, were it asked to.
    private func restorable(_ window: NSWindow) -> [(String, Data)] {
        var written: [(String, Data)] = []
        func walk(_ view: NSView) {
            let coder = NSKeyedArchiver(requiringSecureCoding: false)
            view.encodeRestorableState(with: coder)
            coder.finishEncoding()
            written.append(("\(type(of: view))", coder.encodedData))
            for sub in view.subviews { walk(sub) }
        }
        if let content = window.contentView { walk(content) }
        return written
    }

    /// Everything the process has kept in its settings.
    private func settings() -> String {
        let defaults = UserDefaults.standard
        var said = "\(defaults.dictionaryRepresentation())"
        for name in [Bundle.main.bundleIdentifier, "so.tilo.assistant"].compactMap({ $0 }) {
            said += "\(defaults.persistentDomain(forName: name) ?? [:])"
        }
        return said
    }

    /// What this process wrote to the system's log since a moment.
    private func logged(since moment: Date) throws -> String {
        let store = try OSLogStore(scope: .currentProcessIdentifier)
        return try store.getEntries(at: store.position(date: moment)).map(\.composedMessage).joined(separator: "\n")
    }

    private func everything(_ value: Any) -> String {
        var said = ""
        dump(value, to: &said)
        return said + String(reflecting: value)
    }

    func testWhatIsTypedIntoTheProtectedFieldReachesOnlyTheVault() async throws {
        let began = Date()
        let outputs = Outputs()
        outputs.listen()
        var listening = true
        defer { if listening { _ = outputs.heard() } }
        let run = try await Started.app(asking)
        defer { run.end() }
        let drive = run.drive
        let board = NSPasteboard.general
        let count = board.changeCount

        // It is announced as what it is, and by what it is for.
        let (node, field) = try protected(run)
        XCTAssertEqual(node.role, "AXTextField")
        XCTAssertEqual(node.label, AssistantWords.secretField.said(["name": "LinkedIn"]))
        XCTAssertFalse(drive.node(AssistantWords.save.said, role: "AXButton")?.isEnabled ?? true, "nothing to save before something is typed")

        drive.click(field)
        drive.type(marker)
        await drive.until("Save to be offered") { drive.node(AssistantWords.save.said, role: "AXButton")?.isEnabled ?? false }
        // While it stands in the field: nothing the window says of itself holds it, and it cannot be copied or cut.
        XCTAssertFalse(drive.said().contains(marker), "the window reads the secret out")
        XCTAssertFalse(everything(run.model.drafts).contains(marker))
        // The system writes nothing of the window down for its next start. (A window it may restore
        // has the editor of whatever field is being typed in written down, letters and all.)
        XCTAssertFalse(run.window.isRestorable)
        // And no view of it would write the secret down, were it asked to.
        XCTAssertEqual(restorable(run.window).filter { $0.1.range(of: Data(marker.utf8)) != nil }.map(\.0), [])
        XCTAssertFalse(drive.key("a", [.command]).contains("off"), "everything in the field can be selected")
        XCTAssertTrue(drive.key("c", [.command]).contains("off"), "Copy is off in a protected field")
        XCTAssertTrue(drive.key("x", [.command]).contains("off"), "Cut is off in a protected field")
        await drive.settle()
        XCTAssertEqual(board.changeCount, count, "something was put on the pasteboard")
        XCTAssertFalse((board.string(forType: .string) ?? "").contains(marker))
        XCTAssertTrue(drive.node(AssistantWords.save.said, role: "AXButton")?.isEnabled ?? false, "Cut took nothing out of the field")
        drive.picture("mac-driven-secret-typed")

        // Return saves.
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("the vault to be handed it") { run.source.kept.count == 1 }
        XCTAssertEqual(run.source.kept, [marker], "the vault gets every letter that was typed")
        XCTAssertEqual(run.source.inner.kept(for: "linkedin")?.handOver(), marker)
        await drive.until("the card to say it is saved") { drive.shows(AssistantWords.secretKept.said) }
        await drive.settle(300)

        // The one call that carried it, and no other.
        XCTAssertEqual(run.source.calls("keep"), ["keep(Secret, linkedin)"])
        for call in run.source.calls { XCTAssertFalse(call.contains(marker), "a call carried the secret: \(call.prefix(40))") }
        // No message, no piece of any conversation, at the source or in the window.
        for chat in [Chat.main] + run.model.subAgents.map({ Chat.subAgent($0.id) }) {
            XCTAssertFalse(everything(run.source.inner.items(in: chat)).contains(marker))
        }
        XCTAssertFalse(everything(run.model.conversations).contains(marker))
        // No draft, and no word of the window's own.
        XCTAssertFalse(everything(run.model.drafts).contains(marker))
        XCTAssertFalse((run.model.problem ?? "").contains(marker))
        XCTAssertFalse(drive.said().contains(marker))
        XCTAssertNil(drive.tree().first { $0.subrole == "AXSecureTextField" }, "a secret that is kept leaves no field behind")
        // Not on the pasteboard.
        XCTAssertEqual(board.changeCount, count)
        // Not in what the app keeps: its settings, and what its window writes down for its next start.
        XCTAssertFalse(settings().contains(marker))
        XCTAssertFalse(run.window.isRestorable)
        // No line of a log: not on the process's outputs, not in the system's log.
        listening = false
        XCTAssertFalse(outputs.heard().contains(marker), "the secret was printed")
        XCTAssertFalse(try logged(since: began).contains(marker), "the secret was logged")
        // And the value that carried it says nothing of itself.
        XCTAssertFalse(everything(Secret(marker)).contains(marker))
        XCTAssertFalse("\(Secret(marker))".contains(marker))
    }

    func testASecretThatCouldNotBeKeptIsGoneFromTheFieldAndFromEverywhere() async throws {
        let run = try await Started.app(asking)
        defer { run.end() }
        let drive = run.drive
        let (_, field) = try protected(run)
        drive.click(field)
        drive.type(marker)
        await drive.until("Save to be offered") { drive.node(AssistantWords.save.said, role: "AXButton")?.isEnabled ?? false }
        run.source.inner.set(.unreachable)
        // Save, by its button this time.
        drive.press(AssistantWords.save.said, role: "AXButton")
        await drive.until("the window to say it did not go through") { run.model.problem == AssistantWords.unreachable.said }
        await drive.settle(300)
        // It is held in one place at a time: once handed on, the field is empty, also when the hand-over failed.
        let (_, after) = try protected(run)
        XCTAssertEqual(after.stringValue, "")
        XCTAssertFalse(drive.node(AssistantWords.save.said, role: "AXButton")?.isEnabled ?? true)
        XCTAssertNil(run.source.inner.kept(for: "linkedin"), "nothing was kept")
        XCTAssertFalse(drive.said().contains(marker))
        XCTAssertFalse(everything(run.model.drafts).contains(marker))
        XCTAssertFalse(everything(run.model.conversations).contains(marker))
        for call in run.source.calls { XCTAssertFalse(call.contains(marker)) }
    }

    func testFetchingFromTheManagerEmptiesTheFieldFirst() async throws {
        let run = try await Started.app(asking)
        defer { run.end() }
        let drive = run.drive
        let (_, field) = try protected(run)
        drive.click(field)
        drive.type(marker)
        await drive.settle(200)
        drive.press(AssistantWords.getFromManager.said(["manager": "Bitwarden"]), role: "AXButton")
        await drive.until("the manager to be asked") { run.source.calls("fetchSecret") == ["fetchSecret(linkedin)"] }
        await drive.until("the card to say it is saved") { drive.shows(AssistantWords.secretKept.said) }
        XCTAssertEqual(run.source.kept, [], "what was typed was handed to nobody")
        XCTAssertFalse(drive.said().contains(marker))
        for call in run.source.calls { XCTAssertFalse(call.contains(marker)) }
    }

    // MARK: Out of sight while the app is not in front

    /// The secret's card alone, with something in its field, as a window draws it.
    private func card(covered: Bool) async throws -> (HeldWindow, Drive, NSBitmapImageRep) {
        let model = AssistantModel(source: MemorySource(asking, now: { Drawing.now }))
        await model.prime()
        let view = AssistantRoot(model: model) {
            ConversationView(chat: .main, picturedSecret: "correct horse").background(Token.content)
        }
        .environment(\.coversSecrets, covered)
        .environment(\.colorScheme, .light)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 430, height: 330), [.borderless])
        window.appearance = NSAppearance(named: .aqua)
        window.contentView = NSHostingView(rootView: view)
        let drive = Drive(window)
        await drive.settle(500)
        let content = try XCTUnwrap(window.contentView)
        let picture = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
        content.cacheDisplay(in: content.bounds, to: picture)
        return (window, drive, picture)
    }

    /// How many points of a stretch of a picture are not its ground's colour.
    private func marks(in picture: NSBitmapImageRep, _ area: CGRect) -> Int {
        let scale = CGFloat(picture.pixelsWide) / picture.size.width
        guard let ground = picture.colorAt(x: Int(area.maxX * scale) - 2, y: Int(area.midY * scale))?.usingColorSpace(.sRGB) else { return -1 }
        var count = 0
        for x in stride(from: Int(area.minX * scale), to: Int(area.maxX * scale), by: 1) {
            for y in stride(from: Int(area.minY * scale), to: Int(area.maxY * scale), by: 1) {
                guard let colour = picture.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                if abs(colour.redComponent - ground.redComponent) + abs(colour.greenComponent - ground.greenComponent)
                    + abs(colour.blueComponent - ground.blueComponent) > 0.2 { count += 1 }
            }
        }
        return count
    }

    func testWhatStandsInTheProtectedFieldIsCoveredWhileTheAppIsNotInFront() async throws {
        // In front: the field shows its dots, and is there for whoever reads the window.
        let (shown, shownDrive, shownPicture) = try await card(covered: false)
        defer { shown.close() }
        let node = try XCTUnwrap(shownDrive.tree().first { $0.subrole == "AXSecureTextField" }, shownDrive.said())
        // Where the dots stand: the field's own place, in the picture's measure from its top.
        let content = try XCTUnwrap(shown.contentView)
        let inWindow = node.frame.offsetBy(dx: -shown.frame.minX, dy: -shown.frame.minY)
        let area = CGRect(x: inWindow.minX, y: content.bounds.height - inWindow.maxY, width: inWindow.width, height: inWindow.height)
        XCTAssertGreaterThan(marks(in: shownPicture, area), 40, "in front, the field shows that something stands in it")
        shownDrive.picture("mac-secret-in-front", of: content)

        // Not in front: the same place is plain ground, and the field is told to nobody.
        let (covered, coveredDrive, coveredPicture) = try await card(covered: true)
        defer { covered.close() }
        XCTAssertEqual(marks(in: coveredPicture, area), 0, "a dot of the secret shows while the app is not in front")
        XCTAssertNil(coveredDrive.tree().first { $0.subrole == "AXSecureTextField" })
        XCTAssertFalse(coveredDrive.said().contains("correct horse"))
        XCTAssertFalse(shownDrive.said().contains("correct horse"), "a protected field never says what stands in it")
        coveredDrive.picture("mac-secret-covered", of: covered.contentView)
        XCTAssertEqual(Drive.onScreen(), [])
    }
}

/// The first run, in a window that is on no screen: what Tilo asks before it pairs goes on only
/// at the person's press, and the provider step's protected field hands its key to the link and
/// to nothing else.
@MainActor
final class SetupTests: XCTestCase {
    private let marker = "Zq7-made-up-key-4tT"

    func testWhatTiloAsksBeforePairingStandsInTheWindowAndOnlyAPressAnswersIt() async throws {
        var content = MemorySource.Content()
        content.session = .asked(Drawing.pairingAsk)
        content.pairsAs = Drawing.person
        var run = try await Started.app(content)
        // Tilo's own sentences, whole, and its own words on the two buttons.
        for sentence in [Drawing.pairingAsk.heading] + Drawing.pairingAsk.said {
            XCTAssertTrue(run.drive.shows(sentence), "the window does not say “\(sentence)”:\n\(run.drive.said())")
        }
        XCTAssertNotNil(run.drive.node(Drawing.pairingAsk.go, role: "AXButton"))
        XCTAssertNotNil(run.drive.node(Drawing.pairingAsk.stop, role: "AXButton"))
        XCTAssertFalse(run.drive.has(AssistantWords.pasteLink.said), "while Tilo asks, nothing else is offered")
        // Nobody pressed, so nothing was answered.
        await run.drive.settle(400)
        XCTAssertEqual(run.source.calls("answerPairing"), [])
        XCTAssertEqual(run.model.session, .asked(Drawing.pairingAsk))
        run.drive.press(Drawing.pairingAsk.go, role: "AXButton")
        await run.drive.until("the person's yes to reach the source") { run.source.calls("answerPairing") == ["answerPairing(true)"] }
        await run.drive.until("the device to be in") { run.model.person == Drawing.person }
        run.end()

        // Stop leaves the device out, at the way in.
        run = try await Started.app(content)
        defer { run.end() }
        run.drive.press(Drawing.pairingAsk.stop, role: "AXButton")
        await run.drive.until("the person's no to reach the source") { run.source.calls("answerPairing") == ["answerPairing(false)"] }
        await run.drive.until("the way in to stand again") { run.model.session == .signedOut && run.drive.has(AssistantWords.pasteLink.said) }
    }

    func testTheProviderStepStandsInTheMainChatAndItsKeyReachesOnlyTheLink() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .needsProvider(Drawing.providers)
        content.offered = Drawing.models
        let run = try await Started.app(content)
        defer { run.end() }
        let drive = run.drive
        let board = NSPasteboard.general
        let count = board.changeCount
        // The main chat says that no provider is linked, where an answer would stand.
        XCTAssertTrue(drive.shows(AssistantWords.needsProvider.said), drive.said())
        XCTAssertFalse(drive.shows(AssistantWords.nothingYet.said), "a chat with something to say of its first run is not empty")
        // The other provider is picked and the first again: the field is for the one picked.
        drive.press("Google Gemini", role: "AXButton")
        await drive.until("the other provider to be the one to link") {
            drive.node(AssistantWords.linkProvider.said(["name": "Google Gemini"]), role: "AXButton") != nil
        }
        drive.press("Anthropic", role: "AXButton")
        let link = AssistantWords.linkProvider.said(["name": "Anthropic"])
        await drive.until("the first provider to be the one to link") { drive.node(link, role: "AXButton") != nil }
        // One protected field, announced by what it is for, and no other field for a key.
        XCTAssertEqual(drive.tree().filter { $0.subrole == "AXSecureTextField" }.map(\.label),
                       [AssistantWords.providerKey.said(["name": "Anthropic"])])
        XCTAssertFalse(drive.node(link, role: "AXButton")?.isEnabled ?? true, "nothing to link before something is typed")
        func find(_ view: NSView) -> NSSecureTextField? {
            if let found = view as? NSSecureTextField { return found }
            for sub in view.subviews { if let found = find(sub) { return found } }
            return nil
        }
        drive.click(try XCTUnwrap(run.window.contentView.flatMap(find)))
        drive.type(marker)
        await drive.until("the link to be offered") { drive.node(link, role: "AXButton")?.isEnabled ?? false }
        XCTAssertFalse(drive.said().contains(marker), "the window reads the key out")
        drive.press(link, role: "AXButton")
        await drive.until("the vault to be handed the key") { run.source.inner.key(for: "anthropic") != nil }
        XCTAssertEqual(run.source.inner.key(for: "anthropic")?.handOver(), marker, "the vault gets every letter that was typed")
        // The one call that carried it, and it says nothing of the key.
        XCTAssertEqual(run.source.calls("link"), ["link(anthropic, Secret)"])
        for call in run.source.calls { XCTAssertFalse(call.contains(marker), "a call carried the key: \(call.prefix(40))") }
        // The step goes on to the models that were offered. No field is left, and nothing of
        // the window, the model or the pasteboard holds the key.
        await drive.until("the models to be offered") { drive.shows(AssistantWords.needsModel.said) }
        XCTAssertEqual(run.model.setup, .needsModel(Drawing.models))
        XCTAssertNil(drive.tree().first { $0.subrole == "AXSecureTextField" }, "a key that is kept leaves no field behind")
        XCTAssertFalse(drive.said().contains(marker))
        var held = ""
        dump([run.model.drafts as Any, run.model.conversations as Any, run.model.models as Any, run.model.problem as Any], to: &held)
        XCTAssertFalse(held.contains(marker))
        XCTAssertEqual(board.changeCount, count, "something was put on the pasteboard")
        XCTAssertTrue(drive.tree().contains { $0.role == "AXMenuButton" && $0.name == AssistantWords.chooseModel.said },
                      "the models are offered to choose from:\n\(drive.said())")
        // The choice is the source's own choice of a model, and then nothing is in the way.
        run.model.choose(Drawing.models[1].id)
        await drive.until("the chosen model to answer") { run.model.setup == .ready && run.model.supervision.model == Drawing.models[1].name }
    }

    func testWhileTheirServerMakesTheirWorkspaceTheMainChatSaysSoAndTheWindowGoesOnWaiting() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .makingWorkspace
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive
        await drive.until("the main chat to say that their server is making their workspace") { drive.shows(AssistantWords.makingWorkspace.said) }
        XCTAssertTrue(drive.shows(AssistantWords.pairing.said), "the window no longer says that it waits:\n\(drive.said())")
        XCTAssertFalse(drive.shows(AssistantWords.makingAssistant.said), "an assistant was said to be set up while none was being made")
        XCTAssertFalse(drive.shows(AssistantWords.nothingYet.said), "a chat that says what it waits for is not empty")
        // Their server answered: the sentence is gone, and the window goes on waiting for the rest.
        started.source.inner.set(.findingAssistant)
        await drive.until("the model to hold the step that follows") { started.model.setup == .findingAssistant }
        await drive.until("the sentence to be gone") { !drive.shows(AssistantWords.makingWorkspace.said) }
        XCTAssertTrue(drive.shows(AssistantWords.pairing.said), drive.said())
        XCTAssertFalse(drive.shows(AssistantWords.makingAssistant.said))
    }
}
#endif
