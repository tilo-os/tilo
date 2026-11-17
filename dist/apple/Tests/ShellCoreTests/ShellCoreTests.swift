import XCTest
@testable import ShellCore

final class SettingsTests: XCTestCase {
    func testASiteIsReachedOverHttpsOrIsThisComputer() throws {
        XCTAssertEqual(try ShellSettings(site: "https://tilo.sh").origin, "https://tilo.sh")
        XCTAssertEqual(try ShellSettings(site: " https://Tilo.SH:443/ ").origin, "https://tilo.sh")
        XCTAssertEqual(try ShellSettings(site: "https://tilo.sh:8443").origin, "https://tilo.sh:8443")
        XCTAssertEqual(try ShellSettings(site: "http://tilo.localhost:24120").origin, "http://tilo.localhost:24120")
        XCTAssertEqual(try ShellSettings(site: "http://127.0.0.1:8080").origin, "http://127.0.0.1:8080")
        let refused: [(String, ShellSettings.Refusal)] = [
            ("http://example.com", .notSecure),
            // A machine on the local network is not this computer: its page would be no secure context.
            ("http://192.168.1.4:24120", .notSecure),
            ("http://localhost.example.com", .notSecure),
            ("tilo.sh", .notAnAddress),
            ("", .notAnAddress),
            ("ftp://tilo.sh", .notAnAddress),
            ("https://tilo.sh/app/", .notAnAddress),
            ("https://tilo.sh?next=1", .notAnAddress),
            ("https://tilo.sh#/me", .notAnAddress),
            ("https://someone@tilo.sh", .notAnAddress),
        ]
        for (written, why) in refused {
            XCTAssertThrowsError(try ShellSettings(site: written), written) {
                XCTAssertEqual($0 as? ShellSettings.Refusal, why, written)
            }
        }
    }

    func testItOpensOneAppInThePersonsOwnWorkspace() throws {
        XCTAssertEqual(try ShellSettings(site: "http://tilo.localhost:24120").start.absoluteString,
                       "http://tilo.localhost:24120/app/#/me/so.tilo.chat")
        XCTAssertEqual(try ShellSettings(site: "https://tilo.sh", app: "so.tilo.assistant").start.absoluteString,
                       "https://tilo.sh/app/#/me/so.tilo.assistant")
        for app in ["Chat", "so", "so.tilo/chat", "so.tilo.chat?x", ""] {
            XCTAssertThrowsError(try ShellSettings(site: "https://tilo.sh", app: app), app) {
                XCTAssertEqual($0 as? ShellSettings.Refusal, .notAnApp, app)
            }
        }
    }

    func testThePersonsChoiceWinsOverTheBuildsAndTheBuildsOverProduction() throws {
        let name = "shell-tests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: nil).get().origin, "https://tilo.sh")
        let build: [String: Any] = [ShellSettings.siteKey: "https://staging.example", ShellSettings.appKey: "so.tilo.assistant"]
        let built = try ShellSettings.load(defaults: defaults, info: build).get()
        XCTAssertEqual(built.origin, "https://staging.example")
        XCTAssertEqual(built.app, "so.tilo.assistant")
        defaults.set("http://tilo.localhost:24120", forKey: ShellSettings.siteKey)
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: build).get().origin, "http://tilo.localhost:24120")
        // An address that cannot be used is said, never swapped for another site.
        defaults.set("http://192.168.1.4", forKey: ShellSettings.siteKey)
        XCTAssertEqual(ShellSettings.load(defaults: defaults, info: build), .failure(.notSecure))
        // An empty choice is no choice.
        defaults.set("", forKey: ShellSettings.siteKey)
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: build).get().origin, "https://staging.example")
    }
}

final class KeptArgumentTests: XCTestCase {
    func testASiteNamedOnTheCommandLineIsKeptAndOneThatCannotBeUsedIsNot() throws {
        let name = "shell-tests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        let before = defaults.volatileDomain(forName: UserDefaults.argumentDomain)
        defer {
            defaults.setVolatileDomain(before, forName: UserDefaults.argumentDomain)
            defaults.removePersistentDomain(forName: name)
        }
        let kept = { defaults.persistentDomain(forName: name) ?? [:] }
        defaults.setVolatileDomain([ShellSettings.siteKey: "https://Stack.example:8443/", ShellSettings.appKey: "so.tilo.assistant"],
                                   forName: UserDefaults.argumentDomain)
        ShellSettings.keepArguments(in: defaults)
        XCTAssertEqual(kept()[ShellSettings.siteKey] as? String, "https://stack.example:8443")
        XCTAssertEqual(kept()[ShellSettings.appKey] as? String, "so.tilo.assistant")
        defaults.setVolatileDomain([ShellSettings.siteKey: "http://192.168.1.4:24120", ShellSettings.appKey: "Chat"],
                                   forName: UserDefaults.argumentDomain)
        ShellSettings.keepArguments(in: defaults)
        XCTAssertEqual(kept()[ShellSettings.siteKey] as? String, "https://stack.example:8443")
        XCTAssertEqual(kept()[ShellSettings.appKey] as? String, "so.tilo.assistant")
    }
}

final class SitePolicyTests: XCTestCase {
    private func policy() throws -> SitePolicy {
        SitePolicy(settings: try ShellSettings(site: "http://tilo.localhost:24120"))
    }

    func testOnlyTheSiteStaysInside() throws {
        let policy = try policy()
        let cases: [(String, Destination)] = [
            ("http://tilo.localhost:24120/app/#/me/so.tilo.chat", .inside),
            ("http://TILO.localhost:24120/i/abcdefghijklmnop#key", .inside),
            ("about:blank", .inside),
            ("blob:http://tilo.localhost:24120/0b5c9a1e", .inside),
            ("https://example.com/page", .outside),
            ("http://tilo.localhost:24121/", .outside),
            ("https://tilo.localhost:24120/", .outside),
            ("http://evil.example/http://tilo.localhost:24120/", .outside),
            ("mailto:ada@example.com", .outside),
            ("tel:+4930123456", .outside),
            ("blob:https://example.com/0b5c9a1e", .refused),
            ("file:///etc/hosts", .refused),
            ("javascript:alert(1)", .refused),
            ("data:text/html,hello", .refused),
            ("tilo://open", .refused),
        ]
        for (address, expected) in cases {
            let url = try XCTUnwrap(URL(string: address), address)
            XCTAssertEqual(policy.destination(of: url), expected, address)
        }
    }

    func testAPastedLinkOpensOnlyWhenItLeadsIntoTheSite() throws {
        let policy = try policy()
        let pairing = "http://tilo.localhost:24120/app/#/me/so.tilo.chat?pair=\(String(repeating: "a", count: 64))"
        XCTAssertEqual(policy.link(in: "  \(pairing)\n")?.absoluteString, pairing)
        XCTAssertNil(policy.link(in: "see \(pairing)"))
        XCTAssertNil(policy.link(in: "https://tilo.sh/app/#/me/so.tilo.chat"))
        XCTAssertNil(policy.link(in: "remind me at nine"))
        XCTAssertNil(policy.link(in: ""))
    }
}

final class AttentionTests: XCTestCase {
    func testThePagesTitleSaysWhatWaits() {
        let three = Attention(title: "(3) Ada needs you · Tilo · Lounge")
        XCTAssertEqual(three.unread, 3)
        XCTAssertEqual(three.badge, "3")
        XCTAssertEqual(three.line, "Ada needs you · Tilo · Lounge")
        XCTAssertEqual(Attention(title: "(99+) Tilo").badge, "99+")
        for quiet in ["Tilo · Lounge", "Tilo", "", "(soon) Tilo", "(3)Tilo"] {
            XCTAssertEqual(Attention(title: quiet).unread, 0, quiet)
            XCTAssertNil(Attention(title: quiet).badge, quiet)
            XCTAssertEqual(Attention(title: quiet).line, quiet)
        }
        XCTAssertNil(Attention(title: nil).badge)
    }
}

final class HandOverTests: XCTestCase {
    func testWhatIsSharedBecomesWordsForOneMessage() {
        XCTAssertEqual(SharedItem(text: "Read this").words, "Read this")
        XCTAssertEqual(SharedItem(link: "https://example.com/a").words, "https://example.com/a")
        XCTAssertEqual(SharedItem(text: "Read this", link: "https://example.com/a").words, "Read this\nhttps://example.com/a")
        XCTAssertEqual(SharedItem(text: "At https://example.com/a now", link: "https://example.com/a").words,
                       "At https://example.com/a now")
        XCTAssertNil(SharedItem().words)
        XCTAssertNil(SharedItem(text: "", link: "").words)
        XCTAssertNil(SharedItem(text: String(repeating: "x", count: SharedItem.limit + 1)).words)
    }

    func testAnItemLeftIsTakenOnceAndInTheOrderItWasLeft() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("shell-hand-over-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: folder) }
        let place = FolderHandOver(folder: folder)
        XCTAssertEqual(try place.take(), [])
        let start = Date(timeIntervalSince1970: 1_800_000_000)
        try place.leave(SharedItem(text: "second", at: start.addingTimeInterval(2)))
        try place.leave(SharedItem(text: "first", at: start))
        try place.leave(SharedItem(link: "https://example.com/third", at: start.addingTimeInterval(60)))
        // What is no item (a file half written) is dropped, not handed over and not kept.
        try Data("{".utf8).write(to: folder.appendingPathComponent("00000000000000001-broken.json"))
        XCTAssertEqual(try place.take().map(\.words), ["first", "second", "https://example.com/third"])
        XCTAssertEqual(try place.take(), [])
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: folder.path), [])
    }

    func testABuildWithoutATeamSharesNoPlace() {
        XCTAssertNil(SharedPlace.group(info: [:]))
        XCTAssertNil(SharedPlace.group(info: [SharedPlace.teamKey: ""]))
        XCTAssertNil(SharedPlace.handOver(info: nil))
        XCTAssertEqual(SharedPlace.group(info: [SharedPlace.teamKey: "TEAM123456"]), "TEAM123456.so.tilo.assistant.shell.shared")
    }
}

final class WordsTests: XCTestCase {
    /// The English catalogue the bundle carries, read as the system reads it.
    private func catalogue() throws -> [String: String] {
        let file = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Bundle/en.lproj/Localizable.strings")
        let read = try PropertyListSerialization.propertyList(from: Data(contentsOf: file), format: nil)
        return try XCTUnwrap(read as? [String: String])
    }

    func testEverySentenceIsInTheCatalogueAsItIsInTheCode() throws {
        let catalogue = try catalogue()
        XCTAssertEqual(Set(catalogue.keys), Set(Words.allCases.map(\.rawValue)))
        for words in Words.allCases {
            XCTAssertEqual(catalogue[words.rawValue], words.english, words.rawValue)
        }
    }

    func testSentencesFollowTheDesignLanguage() {
        for words in Words.allCases {
            let english = words.english
            XCTAssertEqual(english.first?.isUppercase, true, english)
            // Sentence case: after the first word only a name is capitalised.
            let later = english.split(separator: " ").dropFirst().filter { $0.first?.isUppercase == true }
                .map { $0.trimmingCharacters(in: .punctuationCharacters) }
            XCTAssertTrue(later.allSatisfy { ["Tilo", "Assistant", "Mac"].contains($0) }, english)
            // No facts joined by a dot or a dash, nothing shouted.
            for joined in [" · ", " - ", " – ", " — ", "!"] {
                XCTAssertFalse(english.contains(joined), english)
            }
        }
    }
}

final class BridgeTests: XCTestCase {
    func testANoticeIsReadWithItsAnswers() {
        let said: [String: Any] = [
            "kind": "notify", "id": "k1-1", "title": "Ada asks", "body": "Send the reply?", "tag": "agentq-1",
            "actions": [
                ["action": "yes", "title": "Send", "type": "button", "placeholder": ""],
                ["action": "say", "title": "Reply", "type": "text", "placeholder": "Message"],
                ["action": "", "title": "No name"],
                ["title": "No action"],
            ],
        ]
        XCTAssertEqual(PageMessage(body: said), .notify(PageNotice(
            id: "k1-1", title: "Ada asks", body: "Send the reply?", tag: "agentq-1",
            actions: [NoticeAction(id: "yes", title: "Send"),
                      NoticeAction(id: "say", title: "Reply", kind: .text, placeholder: "Message")])))
    }

    func testTheShellOffersAWayInOnlyAtAClosedDoor() {
        XCTAssertEqual(PageMessage(body: ["kind": "entry", "state": "refused", "pairing": false]),
                       .entry(PageEntry(state: "refused")))
        XCTAssertTrue(PageEntry(state: "refused").wantsWayIn)
        // A device of theirs is already being asked: nothing more to bring in.
        XCTAssertFalse(PageEntry(state: "refused", pairing: true).wantsWayIn)
        for state in ["open", "failed", "idle", "unsupported", ""] {
            XCTAssertFalse(PageEntry(state: state).wantsWayIn, state)
        }
        XCTAssertNil(PageMessage(body: ["kind": "entry"]))
        XCTAssertNil(PageMessage(body: ["kind": "entry", "state": String(repeating: "x", count: 33)]))
    }

    func testWhatIsNoMessageIsDropped() {
        XCTAssertEqual(PageMessage(body: ["kind": "withdraw", "id": "k1-1"]), .withdraw(id: "k1-1"))
        XCTAssertEqual(PageMessage(body: ["kind": "ask-permission"]), .askPermission)
        XCTAssertEqual(PageMessage(body: ["kind": "words-inserted"]), .wordsInserted)
        let dropped: [Any] = [
            "notify", 7, ["kind": "run", "command": "ls"], ["kind": "notify"], ["kind": "notify", "id": "", "title": "x"],
            ["kind": "notify", "id": "a", "title": ""], ["kind": "notify", "id": String(repeating: "a", count: 65), "title": "x"],
            ["kind": "withdraw"], ["id": "a"],
        ]
        for body in dropped {
            XCTAssertNil(PageMessage(body: body), "\(body)")
        }
    }

    func testLongWordsAreCutAndAtMostFourAnswersKept() throws {
        let many = (1...6).map { ["action": "a\($0)", "title": "Answer \($0)"] }
        let said: [String: Any] = ["kind": "notify", "id": "a", "title": String(repeating: "t", count: 400),
                                   "body": String(repeating: "b", count: 5000), "actions": many]
        guard case .notify(let notice) = try XCTUnwrap(PageMessage(body: said)) else { return XCTFail("no notice") }
        XCTAssertEqual(notice.title.count, 256)
        XCTAssertEqual(notice.body.count, 2048)
        XCTAssertEqual(notice.actions.map(\.id), ["a1", "a2", "a3", "a4"])
        XCTAssertNil(notice.tag)
    }

    func testTheScriptIsBoundToOneOriginWhateverItIsCalled() {
        let script = Bridge.script(origin: "https://tilo.sh", permission: .notAsked)
        XCTAssertTrue(script.contains(#"location.origin !== "https:\/\/tilo.sh""#) || script.contains(#"location.origin !== "https://tilo.sh""#))
        XCTAssertTrue(script.contains(#"let permission = "default";"#))
        XCTAssertFalse(script.contains("%ORIGIN%") || script.contains("%PERMISSION%"))
        // An origin is written as a string, whatever it holds.
        XCTAssertEqual(Bridge.literal("a\"; alert(1); \""), #""a\"; alert(1); \"""#)
    }
}
