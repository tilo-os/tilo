#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import XCTest

/// A pairing link handed to the app from outside, by an address of the app's own, as the system
/// hands one over (`application(_:open:)`): `tilo-assistant://pair?link=<the link>`. Tilo's page
/// offers such an address beside the link and code of "Pair a new device". The app takes it only
/// while it stands at its way in, and then it is a pasted link: nothing goes on before the
/// person's own press under what Tilo asks. Driven with nothing on a screen.
@MainActor
final class HandedLinkTests: XCTestCase {
    private static let link = "https://tilo.example/#/w/1f0e?pair=" + String(repeating: "c7", count: 48)
    private static let other = "https://tilo.example/#/w/2a2a?pair=" + String(repeating: "d9", count: 48)

    /// The address as a page writes it: the link, every letter of it that could end the address written out.
    private func address(_ link: String) throws -> URL {
        let written = try XCTUnwrap(link.addingPercentEncoding(withAllowedCharacters: .alphanumerics))
        return try XCTUnwrap(URL(string: "tilo-assistant://pair?link=\(written)"))
    }

    private func wayIn(_ session: Session = .signedOut) -> MemorySource.Content {
        var content = MemorySource.Content()
        content.session = session
        content.pairsAs = Drawing.person
        content.pairingAsk = Drawing.pairingAsk
        return content
    }

    func testAtItsWayInTheAppTakesAHandedLinkAsAPastedOneAndNothingGoesOnBeforeThePersonsPress() async throws {
        let started = try await Started.app(wayIn())
        defer { started.end() }
        started.app.application(NSApplication.shared, open: [try address(Self.link)])
        await started.drive.until("the link to reach the source") { started.source.calls("pair") == ["pair(\(Self.link))"] }
        await started.drive.until("Tilo's question to stand") { started.model.session == .asked(Drawing.pairingAsk) && started.drive.has(Drawing.pairingAsk.go, role: "AXButton") }
        // Nobody pressed, so nothing was answered and nobody is in.
        await started.drive.settle(400)
        XCTAssertEqual(started.source.calls("answerPairing"), [])
        XCTAssertNil(started.model.person)
        started.drive.press(Drawing.pairingAsk.go, role: "AXButton")
        await started.drive.until("the device to be in") { started.model.person == Drawing.person }
    }

    func testAnAppThatIsInTakesNoLinkFromOutside() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let place = started.model.place
        started.app.application(NSApplication.shared, open: [try address(Self.link)])
        await started.drive.settle(400)
        XCTAssertEqual(started.source.calls("pair"), [], "an app that is in began a pairing at a word from outside")
        XCTAssertEqual(started.model.person, Drawing.person)
        XCTAssertEqual(started.model.place, place)
        XCTAssertNil(started.model.problem)
        XCTAssertFalse(started.drive.has(AssistantWords.pasteLink.said))
    }

    func testAnAppInTheMiddleOfAPairingTakesNoSecondLink() async throws {
        let started = try await Started.app(wayIn())
        defer { started.end() }
        started.app.application(NSApplication.shared, open: [try address(Self.link)])
        await started.drive.until("Tilo's question to stand") { started.model.session == .asked(Drawing.pairingAsk) }
        started.app.application(NSApplication.shared, open: [try address(Self.other)])
        await started.drive.settle(400)
        XCTAssertEqual(started.source.calls("pair"), ["pair(\(Self.link))"], "a second link from outside was taken while Tilo asked about the first")
        XCTAssertEqual(started.model.session, .asked(Drawing.pairingAsk))
        XCTAssertEqual(started.source.calls("answerPairing"), [])
    }

    func testAnAddressThatHandsOverNoLinkPairsNothingAndSaysSoAtTheWayIn() async throws {
        let started = try await Started.app(wayIn())
        defer { started.end() }
        started.app.application(NSApplication.shared, open: [try XCTUnwrap(URL(string: "tilo-assistant://pair?link=nothing%20to%20open%20here"))])
        await started.drive.until("the app to say that it holds no link") { started.model.problem == AssistantWords.noLink.said }
        XCTAssertEqual(started.source.calls("pair"), [])
        XCTAssertEqual(started.model.session, .signedOut)
        // Addresses that are none of these hand over nothing at all.
        for text in ["tilo-assistant://pair", "tilo-assistant://pair?link=", "tilo-assistant://open?link=https%3A%2F%2Ftilo.example%2F%23%2Fw%2F1f0e%3Fpair%3Dc7c7",
                     "tilo-assistant://live/pair?link=https%3A%2F%2Ftilo.example%2F%23%2Fw%2F1f0e%3Fpair%3Dc7c7"] {
            started.model.problem = nil
            started.app.application(NSApplication.shared, open: [try XCTUnwrap(URL(string: text), text)])
            await started.drive.settle(200)
            XCTAssertEqual(started.source.calls("pair"), [], text)
            XCTAssertNil(started.model.problem, text)
            XCTAssertEqual(started.model.session, .signedOut, text)
        }
    }

    // MARK: What the address is, a reset app, and an app that does not know yet where it stands

    func testAnAddressOfTheAppsOwnCarriesAPairingLinkAndNoOtherAddressDoes() throws {
        let link = try XCTUnwrap(URL(string: Self.link))
        let made = try XCTUnwrap(HandedLink.address(for: link))
        XCTAssertEqual(made.scheme, "tilo-assistant")
        XCTAssertEqual(made, try address(Self.link))
        XCTAssertEqual(HandedLink.words(in: made), Self.link)
        XCTAssertEqual(PairingLink(in: try XCTUnwrap(HandedLink.words(in: made)))?.url, link)
        for text in ["tilo-assistant://pair", "tilo-assistant://pair?link=", "tilo-assistant://pair?other=x", "tilo-assistant://live/task-1",
                     "tilo-assistant://open?link=https%3A%2F%2Ftilo.example%2F", "https://tilo.example/pair?link=https%3A%2F%2Ftilo.example%2F",
                     "other-app://pair?link=https%3A%2F%2Ftilo.example%2F"] {
            XCTAssertNil(HandedLink.words(in: try XCTUnwrap(URL(string: text), text)), text)
        }
    }

    func testAnAppWhoseWorkspaceIsGoneTakesAHandedLinkAtItsWayInToo() async throws {
        let started = try await Started.app(wayIn(.reset))
        defer { started.end() }
        started.app.application(NSApplication.shared, open: [try address(Self.link)])
        await started.drive.until("the link to reach the source") { started.source.calls("pair") == ["pair(\(Self.link))"] }
        await started.drive.until("Tilo's question to stand") { started.model.session == .asked(Drawing.pairingAsk) }
        await started.drive.settle(300)
        XCTAssertEqual(started.source.calls("answerPairing"), [])
        XCTAssertNil(started.model.person)
    }

    func testTilosQuestionNamesTheAppsOwnSiteInFull() async throws {
        var ask = Drawing.pairingAsk
        ask.site = "http://tilo.localhost:24520"
        var content = wayIn()
        content.session = .asked(ask)
        let started = try await Started.app(content)
        defer { started.end() }
        XCTAssertTrue(started.drive.shows(AssistantWords.pairingSite.said(["site": "http://tilo.localhost:24520"])),
                      "the card does not name the site the app pairs with")
        XCTAssertTrue(started.drive.has(ask.go, role: "AXButton"))
    }

    private func model(_ content: MemorySource.Content, reach: Reach = .reachable) -> (AssistantModel, Recording) {
        let inner = MemorySource(content)
        inner.set(reach)
        let source = Recording(inner)
        return (AssistantModel(source: source, snapshots: cacheOfTheCheck()), source)
    }

    private func until(_ what: String, _ holds: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<600 {
            if holds() { return }
            try? await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("waited for \(what)", file: file, line: line)
    }

    func testAnAppStartedByTheAddressTakesItsLinkOnceItStandsAtItsWayIn() async throws {
        let (model, source) = model(wayIn())
        AppAddress.open(try address(Self.link), in: model)
        XCTAssertEqual(source.calls("pair"), [])
        model.start()
        await until("the link to reach the source") { source.calls("pair") == ["pair(\(Self.link))"] }
        await until("Tilo's question to stand") { model.session == .asked(Drawing.pairingAsk) }
        XCTAssertEqual(source.calls("answerPairing"), [])
    }

    func testALinkHandedAsAnAppStartsThatThenGetsInIsNotKeptForLater() async throws {
        let (model, source) = model(Drawing.content())
        AppAddress.open(try address(Self.link), in: model)
        model.start()
        await until("the device to be in") { model.person == Drawing.person && model.reach == .reachable }
        try await Task.sleep(for: .milliseconds(200))
        source.inner.set(.reset)
        await until("the way in to stand") { model.session == .reset }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(source.calls("pair"), [], "a link handed before the device was in was used long after")
    }

    func testALinkHandedWhileTheAppFindsItsWorkspaceGoneIsTakenAtItsWayIn() async throws {
        let (model, source) = model(Drawing.content(), reach: .unknown)
        model.start()
        await until("the app to say who is signed in") { model.person == Drawing.person }
        AppAddress.open(try address(Self.link), in: model)
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(source.calls("pair"), [])
        source.inner.set(.reset)
        await until("the link to reach the source") { source.calls("pair") == ["pair(\(Self.link))"] }
    }
}
#endif
