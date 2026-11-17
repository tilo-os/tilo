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

    func testItOpensTiloAsAWholeOrOneAppAlone() throws {
        // Tilo as the site itself opens it.
        XCTAssertEqual(try ShellSettings(site: "http://tilo.localhost:24120").start.absoluteString,
                       "http://tilo.localhost:24120/")
        // The whole desktop, with one app of the person's own workspace in front.
        XCTAssertEqual(try ShellSettings(site: "https://tilo.sh", app: "so.tilo.chat").start.absoluteString,
                       "https://tilo.sh/#/me/so.tilo.chat")
        // One app, and nothing else of Tilo.
        let one = try ShellSettings(site: "https://tilo.sh", app: "so.tilo.assistant", alone: true)
        XCTAssertEqual(one.start.absoluteString, "https://tilo.sh/app/#/me/so.tilo.assistant")
        for app in ["Chat", "so", "so.tilo/chat", "so.tilo.chat?x", ""] {
            XCTAssertThrowsError(try ShellSettings(site: "https://tilo.sh", app: app), app) {
                XCTAssertEqual($0 as? ShellSettings.Refusal, .notAnApp, app)
            }
        }
        // One app alone is some app.
        XCTAssertThrowsError(try ShellSettings(site: "https://tilo.sh", alone: true)) {
            XCTAssertEqual($0 as? ShellSettings.Refusal, .notAnApp)
        }
    }

    func testThePersonsChoiceWinsOverTheBuildsAndTheBuildsOverProduction() throws {
        let name = "shell-tests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: nil).get().origin, "https://tilo.sh")
        XCTAssertNil(try ShellSettings.load(defaults: defaults, info: nil).get().app)
        // A build that names no app writes an empty one: Tilo as a whole.
        XCTAssertNil(try ShellSettings.load(defaults: defaults, info: [ShellSettings.appKey: ""]).get().app)
        let build: [String: Any] = [ShellSettings.siteKey: "https://staging.example", ShellSettings.appKey: "so.tilo.chat"]
        let built = try ShellSettings.load(defaults: defaults, info: build).get()
        XCTAssertEqual(built.origin, "https://staging.example")
        XCTAssertEqual(built.app, "so.tilo.chat")
        XCTAssertFalse(built.alone)
        defaults.set("http://tilo.localhost:24120", forKey: ShellSettings.siteKey)
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: build).get().origin, "http://tilo.localhost:24120")
        // An address that cannot be used is said, never swapped for another site.
        defaults.set("http://192.168.1.4", forKey: ShellSettings.siteKey)
        XCTAssertEqual(ShellSettings.load(defaults: defaults, info: build), .failure(.notSecure))
        // An empty choice is no choice.
        defaults.set("", forKey: ShellSettings.siteKey)
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: build).get().origin, "https://staging.example")
        // The app in front is whoever installs the build's to name, over the build's.
        defaults.set("so.tilo.notes", forKey: ShellSettings.appKey)
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: build).get().start.absoluteString,
                       "https://staging.example/#/me/so.tilo.notes")
    }

    func testABuildOfOneAppIsLedNowhereElse() throws {
        let name = "shell-tests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        let build: [String: Any] = [ShellSettings.siteKey: "https://tilo.sh", ShellSettings.appKey: "so.tilo.assistant",
                                    ShellSettings.aloneKey: true]
        // Whatever is kept under the app's name: another app, the desktop an earlier build
        // had a setting for. The site stays the person's to choose.
        defaults.set("so.tilo.chat", forKey: ShellSettings.appKey)
        defaults.set(true, forKey: "TiloDesktop")
        defaults.set("http://tilo.localhost:24120", forKey: ShellSettings.siteKey)
        let shown = try ShellSettings.load(defaults: defaults, info: build).get()
        XCTAssertTrue(shown.alone)
        XCTAssertEqual(shown.start.absoluteString, "http://tilo.localhost:24120/app/#/me/so.tilo.assistant")
        // Without its app such a build shows nothing, never something else.
        XCTAssertEqual(ShellSettings.load(defaults: defaults, info: [ShellSettings.aloneKey: true]), .failure(.notAnApp))
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
        defaults.setVolatileDomain([ShellSettings.siteKey: "https://Stack.example:8443/", ShellSettings.appKey: "so.tilo.chat"],
                                   forName: UserDefaults.argumentDomain)
        ShellSettings.keepArguments(in: defaults, info: [:])
        XCTAssertEqual(kept()[ShellSettings.siteKey] as? String, "https://stack.example:8443")
        XCTAssertEqual(kept()[ShellSettings.appKey] as? String, "so.tilo.chat")
        defaults.setVolatileDomain([ShellSettings.siteKey: "http://192.168.1.4:24120", ShellSettings.appKey: "Chat"],
                                   forName: UserDefaults.argumentDomain)
        ShellSettings.keepArguments(in: defaults, info: [:])
        XCTAssertEqual(kept()[ShellSettings.siteKey] as? String, "https://stack.example:8443")
        XCTAssertEqual(kept()[ShellSettings.appKey] as? String, "so.tilo.chat")
        // A build of one app keeps the site it is given, and no other app.
        defaults.setVolatileDomain([ShellSettings.siteKey: "https://other.example", ShellSettings.appKey: "so.tilo.notes"],
                                   forName: UserDefaults.argumentDomain)
        let one: [String: Any] = [ShellSettings.appKey: "so.tilo.assistant", ShellSettings.aloneKey: true]
        ShellSettings.keepArguments(in: defaults, info: one)
        XCTAssertEqual(kept()[ShellSettings.siteKey] as? String, "https://other.example")
        XCTAssertEqual(kept()[ShellSettings.appKey] as? String, "so.tilo.chat")
        // And the app named beside it for this one start leads it nowhere either.
        XCTAssertEqual(try ShellSettings.load(defaults: defaults, info: one).get().start.absoluteString,
                       "https://other.example/app/#/me/so.tilo.assistant")
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
        // Taken out of whatever is written around it.
        for written in ["see \(pairing)", "Open this on your phone: <\(pairing)>.\nIt works once.", "(\(pairing))",
                        "[Pair](\(pairing)),", "“\(pairing)”"] {
            XCTAssertEqual(policy.link(in: written)?.absoluteString, pairing, written)
        }
        // The first that leads into the site, past one that leads elsewhere.
        XCTAssertEqual(policy.link(in: "https://example.com/a \(pairing)")?.absoluteString, pairing)
        // An invitation's key may end in a dash, which is part of it.
        let sealed = "http://tilo.localhost:24120/i/abcdefghijklmnop#\(String(repeating: "k", count: 42))-"
        XCTAssertEqual(policy.link(in: "Join here: \(sealed).")?.absoluteString, sealed)
        // The same computer under another name is another site: the page's keys are not there.
        XCTAssertNil(policy.link(in: "http://localhost:24120/app/#/me/so.tilo.chat?pair=\(String(repeating: "a", count: 64))"))
        XCTAssertNil(policy.link(in: "http://127.0.0.1:24120/"))
        XCTAssertNil(policy.link(in: "https://tilo.sh/app/#/me/so.tilo.chat"))
        XCTAssertNil(policy.link(in: "tilo.localhost:24120/app/"))
        XCTAssertNil(policy.link(in: "remind me at nine"))
        XCTAssertNil(policy.link(in: ""))
    }

    func testABuildOfOneAppIsForThatAppsOwnPageAndNothingElseOfTilo() throws {
        let workspace = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90"
        let one = SitePolicy(settings: try ShellSettings(site: "https://tilo.sh", app: "so.tilo.assistant", alone: true))
        let cases: [(String, Bool)] = [
            ("https://tilo.sh/app/#/me/so.tilo.assistant", true),
            ("https://tilo.sh/app/#/me/so.tilo.assistant/-/assistant.thread:main?view=tasks", true),
            ("https://tilo.sh/app/#/@eu1.tilo.sh/w/\(workspace)/so.tilo.assistant", true),
            ("https://tilo.sh/app/#/w/\(workspace)/so.tilo.assistant/uuid:1", true),
            // No page of the site, so nothing of Tilo.
            ("about:blank", true),
            // The desktop, with the app in front or without.
            ("https://tilo.sh/", false),
            ("https://tilo.sh/#/me", false),
            ("https://tilo.sh/#/me/so.tilo.assistant", false),
            ("https://tilo.sh/#/@eu1.tilo.sh/w/\(workspace)/so.tilo.assistant", false),
            // Another app alone, an app whose name only begins the same, the app named further on.
            ("https://tilo.sh/app/#/me/so.tilo.chat", false),
            ("https://tilo.sh/app/#/me/so.tilo.assistant-notes", false),
            ("https://tilo.sh/app/#/@eu1.tilo.sh/w/\(workspace)/so.tilo.chat/-/so.tilo.assistant", false),
            ("https://tilo.sh/app/#/w/so.tilo.assistant", false),
            ("https://tilo.sh/app/#/me?next=/me/so.tilo.assistant", false),
            ("https://tilo.sh/app/#/me", false),
            ("https://tilo.sh/app/", false),
            // The site's other pages, and other sites.
            ("https://tilo.sh/i/abcdefghijklmnop#key", false),
            ("https://tilo.sh/apps/#/me/so.tilo.assistant", false),
            ("https://example.com/app/#/me/so.tilo.assistant", false),
        ]
        for (address, expected) in cases {
            XCTAssertEqual(one.isOwn(try XCTUnwrap(URL(string: address), address)), expected, address)
        }
        // A build that shows Tilo as a whole is for all of its site.
        let whole = SitePolicy(settings: try ShellSettings(site: "https://tilo.sh", app: "so.tilo.chat"))
        for (address, _) in cases.dropLast() {
            XCTAssertTrue(whole.isOwn(try XCTUnwrap(URL(string: address), address)), address)
        }
        XCTAssertFalse(whole.isOwn(try XCTUnwrap(URL(string: "https://example.com/app/#/me/so.tilo.chat"))))
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

    func testAnAppSharesAPlaceWithItsOwnExtensionOnlyAndABuildWithoutATeamSharesNone() {
        let place = [SharedPlace.nameKey: "so.tilo.example.shared"]
        XCTAssertNil(SharedPlace.group(info: place))
        XCTAssertNil(SharedPlace.group(info: place.merging([SharedPlace.teamKey: ""]) { $1 }))
        XCTAssertNil(SharedPlace.group(info: [SharedPlace.teamKey: "TEAM123456"]))
        XCTAssertNil(SharedPlace.handOver(info: nil))
        // The place is named after the app, so the two apps of one team share none.
        XCTAssertEqual(SharedPlace.group(info: place.merging([SharedPlace.teamKey: "TEAM123456"]) { $1 }),
                       "TEAM123456.so.tilo.example.shared")
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

    private static let apps = ["Tilo Shell", "Tilo Assistant"]

    func testSentencesFollowTheDesignLanguage() {
        for app in Self.apps {
            for words in Words.allCases {
                let english = words.said(as: app, site: "http://tilo.localhost:24120")
                XCTAssertEqual(english.first?.isUppercase, true, english)
                // Sentence case: after the first word only a name is capitalised. What stands in
                // quotes is another control's own name, as that control writes it.
                let unquoted = english.replacing(/“[^”]*”/, with: "")
                let later = unquoted.split(separator: " ").dropFirst().filter { $0.first?.isUppercase == true }
                    .map { $0.trimmingCharacters(in: .punctuationCharacters) }
                XCTAssertTrue(later.allSatisfy { ["Tilo", "Shell", "Assistant", "Mac"].contains($0) }, english)
                // No facts joined by a dot or a dash, nothing shouted.
                for joined in [" · ", " - ", " – ", " — ", "!"] {
                    XCTAssertFalse(english.contains(joined), english)
                }
            }
        }
    }

    func testEachAppSaysItsOwnNameAndNeverTheOthers() {
        for words in Words.allCases {
            // No sentence names an app by itself: the name is the bundle's.
            for word in ["Shell", "Assistant"] {
                XCTAssertFalse(words.english.contains(word), words.english)
            }
            for app in Self.apps {
                let said = words.said(as: app, site: "https://tilo.example")
                XCTAssertFalse(said.contains("{") || said.contains("%"), said)
                XCTAssertEqual(said.contains(app), words.english.contains("{app}"), said)
                XCTAssertEqual(said.contains("https://tilo.example"), words.english.contains("{site}"), said)
            }
        }
        // Where a link is refused, the sentence names the site the app expects one into.
        XCTAssertEqual(Words.allCases.filter { $0.english.contains("{site}") }, [.noLink, .codeNoLink])
        XCTAssertEqual(Words.noLink.said(as: "Tilo Shell", site: "http://tilo.localhost:24120"),
                       "The clipboard holds no link into http://tilo.localhost:24120, the Tilo site this app opens.")
        XCTAssertEqual(Words.quit.said(as: "Tilo Shell"), "Quit Tilo Shell")
        XCTAssertEqual(Words.quit.said(as: "Tilo Assistant"), "Quit Tilo Assistant")
        // The name a person reads under the icon, before the bundle's short one.
        XCTAssertEqual(Words.appName(info: ["CFBundleDisplayName": "Tilo Assistant", "CFBundleName": "Short"]), "Tilo Assistant")
        XCTAssertEqual(Words.appName(info: ["CFBundleDisplayName": "", "CFBundleName": "Tilo Shell"]), "Tilo Shell")
    }

    /// What a bundle says of itself is written by the build, for the app it builds: a file
    /// that named one app would say that name in the other.
    func testNoFileOfABundleNamesOneOfTheTwoApps() throws {
        let folder = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Bundle")
        let files = try XCTUnwrap(FileManager.default.enumerator(at: folder, includingPropertiesForKeys: nil))
            .compactMap { $0 as? URL }.filter { ["plist", "entitlements", "strings"].contains($0.pathExtension) }
        XCTAssertEqual(files.count, 11)
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for named in Self.apps + ["so.tilo."] {
                XCTAssertFalse(text.contains(named), "\(file.lastPathComponent) names \(named)")
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
