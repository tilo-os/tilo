#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import XCTest

/// Which links are a pairing link of another Tilo site than the app's own, and what is read of
/// one: the name and port it leads to. Every link here is made up, and so is what it carries.
final class PairingElsewhereTests: XCTestCase {
    private let settings = try! ShellSettings(site: "https://tilo.wifi.example:24520")
    private let secret = String(repeating: "c7", count: 48)

    func testAPairingLinkOfAnotherTiloSiteIsToldByTheNameAndPortItLeadsTo() throws {
        for (text, from) in [
            ("https://tilo.sh/#/w/1f0e?pair=\(secret)", "tilo.sh"),
            ("https://tilo.sh/app/#/@workspaces.example:8443/w/1f0e/so.tilo.notes?pair=\(secret)", "tilo.sh"),
            // The page of another Tilo on the computer that serves it, under a name only that
            // computer can use: another port than this app's site, so another site.
            ("http://tilo.localhost:24120/#/w/1f0e?pair=\(secret)", "tilo.localhost:24120"),
            ("http://127.0.0.1:24120/#/me?pair=\(secret)", "127.0.0.1:24120"),
            ("https://other.example:24520/#/w/1f0e?invite=abc&pair=\(secret)", "other.example:24520"),
            // However it is spelled, and whatever it carries inside: the site it leads to.
            ("HTTPS://Other.Example:443/?next=https://tilo.wifi.example:24520/#/w/1f0e?pair=\(secret)", "other.example"),
        ] {
            let link = try XCTUnwrap(URL(string: text), text)
            XCTAssertEqual(EngineAddress.pairingElsewhere(link, of: settings), from, text)
            // Saying where it is from opens nothing: this app pairs with none of them.
            XCTAssertNil(EngineAddress.pairing(link, of: settings), text)
        }
        // Where only http and https tell the two sites apart, the name carries which.
        let plain = try XCTUnwrap(URL(string: "http://tilo.wifi.example:24520/#/w/1f0e?pair=\(secret)"))
        XCTAssertEqual(EngineAddress.pairingElsewhere(plain, of: settings), "http://tilo.wifi.example:24520")
        XCTAssertNil(EngineAddress.pairing(plain, of: settings))
    }

    func testALinkThisAppPairsWithAndOneThatPairsNothingIsNoOtherSitesPairingLink() throws {
        for text in [
            // This app's own site, by its name and as its own computer writes it: it pairs.
            "https://tilo.wifi.example:24520/#/w/1f0e?pair=\(secret)", "http://tilo.localhost:24520/#/w/1f0e?pair=\(secret)",
            // This app's own site, and nothing to pair with.
            "https://tilo.wifi.example:24520/", "https://tilo.wifi.example:24520/#/w/1f0e",
            // Another site, and nothing a Tilo site pairs with.
            "https://other.example/", "https://other.example/#/w/1f0e", "https://other.example/#/w/1f0e?pair=",
            "https://other.example/#/w/1f0e?invite=abc", "https://other.example/i/abcdefghijklmnop#\(secret)",
            "https://other.example/docs/#/w/1f0e?pair=\(secret)", "https://other.example/#/settings?pair=\(secret)",
            // A sign-in before the name, which no link of Tilo's carries, and what is no web address.
            "https://someone@other.example/#/w/1f0e?pair=\(secret)",
            "tilo-assistant://open?link=https://other.example/#/w/1f0e?pair=\(secret)",
        ] {
            let link = try XCTUnwrap(URL(string: text), text)
            XCTAssertNil(EngineAddress.pairingElsewhere(link, of: settings), text)
        }
    }
}

/// What a person reads where the app refuses what they scanned or pasted to pair with, through
/// the source the app pairs by: what is wrong, the site the app is set up for, and where a code
/// for it is made. Of the link itself it says the name and port it leads to and nothing else,
/// and the app opens and keeps nothing of it.
@MainActor
final class PairingRefusalTests: XCTestCase {
    private static let site = "https://tilo.wifi.example:24520"
    private static let secret = String(repeating: "c7", count: 48)
    private var source: TiloSource!
    private var defaults: UserDefaults!
    private var suite = ""
    private var opened: [URL] = []

    private func start() throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.opened.append($0) }
        source = TiloSource(engine: engine, mainChat: .conversation(id: "c_chat", name: "chat"), livesInOwn: false, defaults: defaults)
    }

    override func tearDown() async throws {
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

    /// The sentence the app shows for what was scanned or pasted, which must be refused.
    private func refusal(of words: String) async throws -> String {
        let link = try XCTUnwrap(PairingLink(in: words), words)
        do {
            try await source.pair(with: link)
        } catch SourceProblem.refused(let why) {
            return why
        }
        XCTFail("paired with \(words)")
        return ""
    }

    func testACodeOfAnotherTiloSiteSaysWhichSiteItIsFromAndWhichThisAppIsSetUpFor() async throws {
        try start()
        let secret = Self.secret
        var said: [String] = []
        for (words, from) in [
            ("https://tilo.sh/#/w/1f0e?pair=\(secret)", "tilo.sh"),
            // As a phone scans it from the page of another Tilo on the computer that serves it.
            ("http://tilo.localhost:24120/#/w/1f0e?pair=\(secret)", "tilo.localhost:24120"),
            ("Open this on your phone: <https://Other.Example:8443/app/#/me?pair=\(secret)>.", "other.example:8443"),
        ] {
            let sentence = try await refusal(of: words)
            XCTAssertEqual(sentence, "This code is from another Tilo site, \(from). This app is set up for \(Self.site). "
                + "Open “Pair a new device” there.", words)
            said.append(sentence)
        }
        nothingOfALinkIsSaidOpenedOrKept(said)
    }

    func testALinkThatPairsNothingKeepsAPlainSentenceThatNamesNoOtherSite() async throws {
        try start()
        let secret = Self.secret
        var said: [String] = []
        for words in [
            "https://other.example/", "https://other.example/#/join/made-up", "https://other.example/#/w/1f0e?pair=",
            "https://other.example/docs/#/w/1f0e?pair=\(secret)",
            // This app's own site, and nothing to pair with: the sentence is as true of it.
            "\(Self.site)/", "\(Self.site)/#/w/1f0e",
        ] {
            let sentence = try await refusal(of: words)
            XCTAssertEqual(sentence, "This link pairs no device. This app is set up for \(Self.site). Open “Pair a new device” there.",
                           words)
            XCTAssertFalse(sentence.contains("other.example"), sentence)
            said.append(sentence)
        }
        nothingOfALinkIsSaidOpenedOrKept(said)
    }

    /// What a link carries to pair with, and the place in a site it names, are in no sentence;
    /// no page was opened for a refused link, and the app kept nothing of one.
    private func nothingOfALinkIsSaidOpenedOrKept(_ said: [String]) {
        for sentence in said {
            XCTAssertFalse(sentence.contains(Self.secret), sentence)
            XCTAssertFalse(sentence.contains("pair="), sentence)
            XCTAssertFalse(sentence.contains("/w/") || sentence.contains("/me") || sentence.contains("#"), sentence)
        }
        XCTAssertEqual(opened, [])
        let kept = defaults.persistentDomain(forName: suite) ?? [:]
        XCTAssertFalse("\(kept)".contains(Self.secret), "the app kept what a refused link carries")
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"))
    }
}
#endif
