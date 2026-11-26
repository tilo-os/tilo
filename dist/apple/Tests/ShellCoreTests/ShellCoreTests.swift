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
        // Another site's link is none of this one's, and neither is what is no link at all.
        XCTAssertNil(policy.link(in: "https://tilo.sh/app/#/me/so.tilo.chat"))
        XCTAssertNil(policy.link(in: "tilo.localhost:24120/app/"))
        XCTAssertNil(policy.link(in: "remind me at nine"))
        XCTAssertNil(policy.link(in: ""))
    }

    /// The computer that runs a site makes its links in a page opened as `tilo.localhost`, and
    /// a phone reaches the same site under a name on the network. The link the phone scans or
    /// is handed names that computer twice by names only it can use: as the site, and in the
    /// route as the workspace's host.
    func testALinkTheSitesOwnComputerMadeIsReadUnderTheNameThisAppReachesItBy() throws {
        let phone = SitePolicy(settings: try ShellSettings(site: "https://tilo.wifi.example:24120"))
        let workspace = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90"
        let secret = String(repeating: "a", count: 64) + String(repeating: "b", count: 32)
        let made = "http://tilo.localhost:24120/#/@127.0.0.1:24100/w/\(workspace)?pair=\(secret)"
        let read = "https://tilo.wifi.example:24120/#/@tilo.wifi.example:24100/w/\(workspace)?pair=\(secret)"
        XCTAssertEqual(phone.link(in: made)?.absoluteString, read)
        // As a scanner or the clipboard hands it over, with whatever is written around it.
        XCTAssertEqual(phone.link(in: "Open this on your phone: <\(made)>.\nIt works once.")?.absoluteString, read)
        // Every name a computer has for itself, over whichever scheme it serves itself by.
        for name in ["localhost", "tilo.localhost", "TILO.Localhost", "127.0.0.1", "127.8.9.10", "[::1]"] {
            for scheme in ["http", "https"] {
                let written = "\(scheme)://\(name):24120/#/@127.0.0.1:24100/w/\(workspace)?pair=\(secret)"
                XCTAssertEqual(phone.link(in: written)?.absoluteString, read, written)
            }
        }
        // Every way the link grammar writes a host on that computer.
        for host in ["localhost:24100", "tilo.localhost:24100", "127.0.0.1%3A24100", "https:127.0.0.1:24100",
                     "[::1]:24100"] {
            let written = "http://tilo.localhost:24120/#/@\(host)/w/\(workspace)?pair=\(secret)"
            XCTAssertEqual(phone.link(in: written)?.absoluteString, read, written)
        }
        let key = String(repeating: "k", count: 42)
        let onlyTheSite: [(String, String)] = [
            // A host that names no port has none under the site's name either.
            ("http://localhost:24120/#/@127.0.0.1/w/\(workspace)", "/#/@tilo.wifi.example/w/\(workspace)"),
            // A host that is another computer already stays the host.
            ("http://tilo.localhost:24120/#/@ada.tilo.cloud/w/\(workspace)?pair=\(secret)",
             "/#/@ada.tilo.cloud/w/\(workspace)?pair=\(secret)"),
            ("http://tilo.localhost:24120/#/@10.0.0.26:24100/w/\(workspace)", "/#/@10.0.0.26:24100/w/\(workspace)"),
            // The person's own workspace, one app's page, an invitation, the bare site.
            ("http://tilo.localhost:24120/app/#/me/so.tilo.chat?pair=\(secret)", "/app/#/me/so.tilo.chat?pair=\(secret)"),
            ("http://tilo.localhost:24120/i/abcdefghijklmnop#\(key)-", "/i/abcdefghijklmnop#\(key)-"),
            ("http://tilo.localhost:24120/?from=mail#/me", "/?from=mail#/me"),
            ("http://tilo.localhost:24120", "/"),
        ]
        for (written, rest) in onlyTheSite {
            XCTAssertEqual(phone.link(in: written)?.absoluteString, "https://tilo.wifi.example:24120\(rest)", written)
        }
        // Whatever the link said, what is opened is inside the site this app opens.
        for (written, _) in onlyTheSite + [(made, "")] {
            let link = try XCTUnwrap(phone.link(in: written), written)
            XCTAssertEqual(SitePolicy.origin(of: link), phone.origin, written)
            XCTAssertEqual(phone.destination(of: link), .inside, written)
        }
    }

    func testAnAddressIsReadAsTheSitesByWhereItLeadsAndNotByWhatItCarries() throws {
        let phone = SitePolicy(settings: try ShellSettings(site: "https://tilo.wifi.example:24120"))
        let inside = "https://tilo.wifi.example:24120/#/w/1f0e?pair=abc"
        XCTAssertEqual(phone.inSite(try XCTUnwrap(URL(string: inside)))?.absoluteString, inside)
        let made = try XCTUnwrap(URL(string: "http://tilo.localhost:24120/#/@127.0.0.1:24100/w/1f0e?pair=abc"))
        XCTAssertEqual(phone.inSite(made)?.absoluteString, "https://tilo.wifi.example:24120/#/@tilo.wifi.example:24100/w/1f0e?pair=abc")
        for text in ["https://other.example/?next=\(inside)", "HTTPS://other.example/?next=\(inside)", "tilo://open?link=\(inside)"] {
            XCTAssertNil(phone.inSite(try XCTUnwrap(URL(string: text), text)), text)
        }
        // Words written round a link are another matter: the link is taken out of them.
        XCTAssertEqual(phone.link(in: "Open <\(inside)>.")?.absoluteString, inside)
    }

    func testOnlyALinkOfTheSitesOwnComputerIsReadThatWay() throws {
        let phone = SitePolicy(settings: try ShellSettings(site: "https://tilo.wifi.example:24120"))
        let route = "#/@127.0.0.1:24100/w/6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90?pair=\(String(repeating: "a", count: 96))"
        for refused in [
            // Another port is another program on that computer, not this site.
            "http://tilo.localhost:24121/\(route)", "http://localhost/\(route)", "http://localhost:3000/\(route)",
            "https://127.0.0.1/\(route)",
            // Another site, however much its name looks like a computer's own.
            "https://example.com:24120/\(route)", "http://localhost.example.com:24120/\(route)",
            "http://127.0.0.1.example.com:24120/\(route)", "http://128.0.0.1:24120/\(route)",
            "http://10.0.0.26:24120/\(route)", "https://tilo.wifi.example.evil.example:24120/\(route)",
            "https://evil.example/?next=http://tilo.localhost:24120/\(route)",
            // What stands before an @ is a sign-in, and the site is what follows it.
            "http://tilo.localhost:24120@evil.example/\(route)", "http://someone:word@tilo.localhost:24120/\(route)",
            "http://someone@localhost:24120/\(route)",
            // Not an address of the web.
            "tilo://localhost:24120/\(route)", "ftp://localhost:24120/\(route)", "localhost:24120/\(route)",
        ] {
            XCTAssertNil(phone.link(in: refused), refused)
        }
        // An app that opens the production site takes no link of somebody's own computer for its own.
        let production = SitePolicy(settings: try ShellSettings(site: "https://tilo.sh"))
        XCTAssertNil(production.link(in: "http://tilo.localhost:24120/\(route)"))
        XCTAssertNil(production.link(in: "http://localhost/\(route)"))
        // A link under the site's own name is followed as it is written, also where its route
        // names this computer: on a Mac that is a host on that Mac.
        let own = "https://tilo.wifi.example:24120/\(route)"
        XCTAssertEqual(phone.link(in: own)?.absoluteString, own)
    }

    /// The app on the computer that serves the site: every name of that computer on the
    /// site's port is the site, and is opened under the one name the page keeps its keys by.
    func testOnTheSitesOwnComputerItsOtherNamesAreReadAsTheSite() throws {
        let mac = try policy()
        let secret = String(repeating: "a", count: 96)
        XCTAssertEqual(mac.link(in: "http://localhost:24120/app/#/me/so.tilo.chat?pair=\(secret)")?.absoluteString,
                       "http://tilo.localhost:24120/app/#/me/so.tilo.chat?pair=\(secret)")
        XCTAssertEqual(mac.link(in: "http://127.0.0.1:24120/")?.absoluteString, "http://tilo.localhost:24120/")
        // The host in the route is on this same computer, and stays as the page wrote it.
        let route = "#/@127.0.0.1:24100/w/6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90?pair=\(secret)"
        XCTAssertEqual(mac.link(in: "http://localhost:24120/\(route)")?.absoluteString,
                       "http://tilo.localhost:24120/\(route)")
        XCTAssertNil(mac.link(in: "http://localhost:24121/\(route)"))
        XCTAssertNil(mac.link(in: "https://tilo.sh/\(route)"))
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
            // The page the server renders, which the entry hands every admitted browser to:
            // an app's own when the path names it, and nothing of this app otherwise.
            ("https://tilo.sh/w/\(workspace)/assistant", true),
            ("https://tilo.sh/w/\(workspace)/assistant/tilo.workspace.agent%2Fagents%2Fscout", true),
            ("https://tilo.sh/w/\(workspace)/so.tilo.assistant", true),
            ("https://tilo.sh/w/\(workspace)/chat", false),
            ("https://tilo.sh/w/\(workspace)/so.tilo.chat", false),
            ("https://tilo.sh/w/\(workspace)/assistant-notes", false),
            ("https://tilo.sh/w//assistant", false),
            ("https://tilo.sh/w/\(workspace)", false),
            ("https://tilo.sh/w/\(workspace)/", false),
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

    func testTheShellOffersAWayInOnlyOnARefusingEntryPage() {
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
