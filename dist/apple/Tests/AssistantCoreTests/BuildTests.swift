#if os(macOS)
import Foundation
import XCTest

/// What `build.sh` makes of Tilo Assistant as far as being told while the app is closed goes:
/// the rights its signature claims, what the bundle says of itself, the files it carries, and
/// what is signed in which order.
///
/// The script's own functions make a bundle here, in a folder of the test's, from stand-ins
/// for the programs. Nothing is compiled, nothing is signed (the signing command is written
/// down instead of carried out), no keychain is asked, and no profile is a real one: a
/// profile here is the list a real one decodes to, made up by the test.
final class BuildTests: XCTestCase {
    private static let team = "TESTTEAM00"
    private static let app = "so.tilo.assistant"
    private static let sentence = "dist/apple: no push in this build: "

    /// The script's functions with every tool that compiles, draws, signs or asks the system
    /// replaced by one that writes down what it was asked.
    private static let harness = #"""
    set -euo pipefail
    script="$1"; work="$2"; what="$3"
    source "$script" assistant
    made="$work/made"; team=TESTTEAM00; identity="A Made Up Signer"
    mkdir -p "$work/bin"
    for one in AssistantMac AssistantPhone AssistantWidgets AssistantNotice; do printf '%s' "$one" > "$work/bin/$one"; done
    products() { printf '%s\n' "$work/bin"; }
    running() { return 1; }
    faces() { mkdir -p "$1"; printf face > "$1/face"; }
    draw() { printf picture > "$1"; }
    sips() { while [ $# -gt 1 ]; do [ "$1" != --out ] || printf picture > "$2"; shift; done; }
    iconutil() { printf icons > "$5"; }
    xcrun() { printf '26.4\n'; }
    security() { cat "$4"; }
    codesign() { printf '%s\n' "$*" >> "$work/signed.txt"; }
    case "$what" in
      mac) build_assistant_mac ;;
      iphone) build_assistant_iphone ;;
      mark) printf '%s\n' "$mark" >&2 ;;
    esac
    """#

    private var folder: URL!

    override func setUpWithError() throws {
        folder = FileManager.default.temporaryDirectory.appendingPathComponent("tilo-build-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: folder)
    }

    /// One bundle as the script made it.
    private struct Made {
        let work: URL
        let bundle: URL
        /// The files the bundle carries, by their place in it.
        let files: [String]
        /// What the script said while it built, line by line.
        let said: [String]
        /// What was handed to the signing command, call by call, with the test's folder as WORK.
        let signed: [String]

        func rights(_ name: String) throws -> NSDictionary {
            try XCTUnwrap(NSDictionary(contentsOf: work.appendingPathComponent("made/\(name)")), name)
        }

        func info(_ part: String = "") throws -> NSDictionary {
            try XCTUnwrap(NSDictionary(contentsOf: bundle.appendingPathComponent(part + "Info.plist")), part)
        }

        func bytes(_ name: String) throws -> Data {
            try Data(contentsOf: work.appendingPathComponent("made/\(name)"))
        }
    }

    /// A profile as a real one decodes: the team it is for and the rights it grants.
    private func profile(_ name: String, grants rights: [String: Any]) throws -> String {
        let file = folder.appendingPathComponent(name)
        let listed: [String: Any] = ["TeamIdentifier": [BuildTests.team], "Entitlements": rights]
        try PropertyListSerialization.data(fromPropertyList: listed, format: .xml, options: 0).write(to: file)
        return file.path
    }

    /// Have the script's functions make one bundle, with only the given words in its
    /// environment: nothing of the environment around this test reaches the script.
    private func build(_ what: String, _ words: [String: String], ends status: Int32 = 0) throws -> Made {
        let script = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("build.sh")
        let work = folder.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        let harness = work.appendingPathComponent("harness.sh")
        try BuildTests.harness.write(to: harness, atomically: true, encoding: .utf8)
        func quoted(_ text: String) -> String { "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'" }
        let given = words.merging(["PATH": "/usr/bin:/bin:/usr/sbin:/sbin"]) { word, _ in word }
        let command = (["/usr/bin/env", "-i"] + given.sorted { $0.key < $1.key }.map { quoted("\($0.key)=\($0.value)") }
            + ["/bin/bash", quoted(harness.path), quoted(script.path), quoted(work.path), what,
               ">/dev/null", "2>" + quoted(work.appendingPathComponent("said.txt").path)]).joined(separator: " ")
        let spoken: [UnsafeMutablePointer<CChar>?] = ["/bin/sh", "-c", command].map { strdup($0) } + [nil]
        defer { for word in spoken { free(word) } }
        var started: pid_t = 0
        XCTAssertEqual(posix_spawn(&started, "/bin/sh", nil, nil, spoken, nil), 0)
        var ended: Int32 = 0
        XCTAssertEqual(waitpid(started, &ended, 0), started)
        let said = (try? String(contentsOf: work.appendingPathComponent("said.txt"), encoding: .utf8)) ?? ""
        XCTAssertEqual((ended >> 8) & 0xff, status, said)
        let bundle = work.appendingPathComponent(what == "mac" ? "made/mac/Tilo Assistant.app" : "made/iphone/Tilo Assistant.app")
        let all = FileManager.default.enumerator(atPath: bundle.path)?.allObjects as? [String] ?? []
        let files = all.filter { path in
            var isFolder: ObjCBool = false
            return FileManager.default.fileExists(atPath: bundle.appendingPathComponent(path).path, isDirectory: &isFolder)
                && !isFolder.boolValue
        }
        let signed = (try? String(contentsOf: work.appendingPathComponent("signed.txt"), encoding: .utf8)) ?? ""
        return Made(work: work, bundle: bundle, files: files.sorted(),
                    said: said.split(separator: "\n").map(String.init),
                    signed: signed.replacingOccurrences(of: work.path, with: "WORK").split(separator: "\n").map(String.init))
    }

    // MARK: The profiles a build is handed

    /// A profile for every app of the team, as a trial build carries one: it can grant no push.
    private func profileForAny() throws -> String {
        try profile("any.plist", grants: [
            "application-identifier": "\(BuildTests.team).*", "get-task-allow": true,
            "keychain-access-groups": ["\(BuildTests.team).*"],
        ])
    }

    /// The app's own profile, made after its App ID was given push at Apple.
    private func profileWithPush() throws -> String {
        try profile("push.plist", grants: [
            "application-identifier": "\(BuildTests.team).\(BuildTests.app)", "get-task-allow": true,
            "keychain-access-groups": ["\(BuildTests.team).*"], "aps-environment": "development",
            "com.apple.developer.usernotifications.time-sensitive": true,
        ])
    }

    private func macProfile(_ name: String, push: String?) throws -> String {
        var rights: [String: Any] = [
            "com.apple.application-identifier": "\(BuildTests.team).\(BuildTests.app)",
            "com.apple.developer.team-identifier": BuildTests.team,
        ]
        if let push { rights["com.apple.developer.aps-environment"] = push }
        return try profile(name, grants: rights)
    }

    // MARK: What a build was until now

    private static let phoneFiles = [
        "AppIcon60x60@2x.png", "AppIcon60x60@3x.png", "Fonts/face", "Info.plist",
        "PlugIns/AssistantWidgets.appex/AssistantWidgets", "PlugIns/AssistantWidgets.appex/Fonts/face",
        "PlugIns/AssistantWidgets.appex/Info.plist", "PlugIns/AssistantWidgets.appex/embedded.mobileprovision",
        "PlugIns/AssistantWidgets.appex/en.lproj/Localizable.strings", "TiloAssistant", "embedded.mobileprovision",
        "en.lproj/Localizable.strings",
    ].sorted()
    private static var phoneRights: NSDictionary {
        [
            "application-identifier": "TESTTEAM00.so.tilo.assistant", "com.apple.developer.team-identifier": "TESTTEAM00",
            "keychain-access-groups": ["TESTTEAM00.so.tilo.assistant"], "get-task-allow": true,
        ]
    }
    private static let phoneSigned = [
        "--force --timestamp=none --sign A Made Up Signer --entitlements WORK/made/iphone/so.tilo.assistant.widgets.entitlements "
            + "WORK/made/iphone/Tilo Assistant.app/PlugIns/AssistantWidgets.appex",
        "--force --timestamp=none --sign A Made Up Signer --entitlements WORK/made/iphone/so.tilo.assistant.entitlements "
            + "WORK/made/iphone/Tilo Assistant.app",
        "--verify --deep --strict WORK/made/iphone/Tilo Assistant.app",
    ]
    private static let macFiles = [
        "Contents/Info.plist", "Contents/MacOS/TiloAssistant", "Contents/Resources/AppIcon.icns",
        "Contents/Resources/Fonts/face", "Contents/Resources/en.lproj/Localizable.strings",
    ].sorted()
    private static var macRights: NSDictionary {
        [
            "com.apple.security.app-sandbox": true, "com.apple.security.network.client": true,
            "com.apple.security.device.microphone": true, "com.apple.security.device.audio-input": true,
            "com.apple.security.files.user-selected.read-only": true,
        ]
    }
    private static let macSigned = [
        "--force --options runtime --timestamp=none --sign A Made Up Signer --entitlements WORK/made/mac/app.entitlements "
            + "WORK/made/mac/Tilo Assistant.app",
        "--verify --deep --strict WORK/made/mac/Tilo Assistant.app",
    ]
    /// What an Info.plist says only in a build that is told while it is closed.
    private static let toldKeys = ["TiloPush", "TiloTold", "UIBackgroundModes"]

    // MARK: Not told

    func testABuildThatIsNotToldToClaimsNoPushAndCarriesNoNoticeExtensionWhateverItsProfileGrants() throws {
        // Even a profile that grants push changes nothing while the build is not told to.
        let phone = try build("iphone", ["TILO_APPLE_PROFILE": profileWithPush(), "TILO_APPLE_WIDGETS_PROFILE": profileForAny()])
        XCTAssertEqual(phone.files, BuildTests.phoneFiles)
        XCTAssertEqual(try phone.rights("iphone/so.tilo.assistant.entitlements"), BuildTests.phoneRights)
        XCTAssertEqual(phone.signed, BuildTests.phoneSigned)
        for key in BuildTests.toldKeys { XCTAssertNil(try phone.info()[key], key) }
        XCTAssertFalse(phone.said.contains { $0.contains("push") }, "\(phone.said)")

        let mac = try build("mac", ["TILO_APPLE_MAC_PROFILE": macProfile("mac.plist", push: "production")])
        XCTAssertEqual(mac.files, BuildTests.macFiles)
        XCTAssertEqual(try mac.rights("mac/app.entitlements"), BuildTests.macRights)
        XCTAssertEqual(mac.signed, BuildTests.macSigned)
        for key in BuildTests.toldKeys { XCTAssertNil(try mac.info("Contents/")[key], key) }
        XCTAssertFalse(mac.said.contains { $0.contains("push") }, "\(mac.said)")
    }

    // MARK: What the phone's bundle says of what it asks its person for

    /// An iPhone asks its person before an app may reach anything on their own network, and
    /// shows them the bundle's sentence for it. Tilo Assistant reaches its person's Tilo there
    /// when that is where it is, so its bundle says so, beside what it says of the camera, the
    /// microphone and dictation: each in the app's own name, as build.sh writes it.
    func testThePhoneBundleSaysWhyItReachesItsPersonsOwnNetwork() throws {
        let phone = try build("iphone", ["TILO_APPLE_PROFILE": profileForAny()])
        let info = try phone.info()
        XCTAssertEqual(info["NSLocalNetworkUsageDescription"] as? String,
                       "Tilo Assistant reaches your Tilo when it is on your own network.")
        let asked = (info.allKeys as? [String] ?? []).filter { $0.hasSuffix("UsageDescription") }.sorted()
        XCTAssertEqual(asked, ["NSCameraUsageDescription", "NSLocalNetworkUsageDescription", "NSMicrophoneUsageDescription",
                               "NSSpeechRecognitionUsageDescription"])
        for key in asked {
            let sentence = try XCTUnwrap(info[key] as? String, key)
            XCTAssertTrue(sentence.hasPrefix("Tilo Assistant "), "\(key): \(sentence)")
            XCTAssertFalse(sentence.contains("__"), "\(key): \(sentence)")
        }
    }

    // MARK: Told, and the profile grants it

    func testAPhoneBuildToldToWhoseProfileGrantsPushClaimsItAndCarriesTheNoticeExtension() throws {
        let phone = try build("iphone", [
            "TILO_APPLE_PUSH": "1", "TILO_APPLE_PROFILE": profileWithPush(), "TILO_APPLE_WIDGETS_PROFILE": profileForAny(),
            "TILO_APPLE_NOTICE_PROFILE": profileForAny(),
        ])
        let rights = try phone.rights("iphone/so.tilo.assistant.entitlements")
        XCTAssertEqual(rights["aps-environment"] as? String, "development")
        XCTAssertEqual(rights["com.apple.developer.usernotifications.time-sensitive"] as? Bool, true)
        // The app's own group stays the first, where what it keeps today is; the shared one follows.
        XCTAssertEqual(rights["keychain-access-groups"] as? [String], ["TESTTEAM00.so.tilo.assistant", "TESTTEAM00.so.tilo.assistant.told"])
        XCTAssertEqual(rights.count, BuildTests.phoneRights.count + 2)
        let info = try phone.info()
        XCTAssertEqual(info["TiloPush"] as? String, "development")
        XCTAssertEqual(info["TiloTold"] as? String, "so.tilo.assistant.told")
        XCTAssertEqual(info["UIBackgroundModes"] as? [String], ["remote-notification"])

        let notice = "PlugIns/AssistantNotice.appex/"
        XCTAssertEqual(phone.files, (BuildTests.phoneFiles + ["AssistantNotice", "Info.plist", "embedded.mobileprovision",
                                                               "en.lproj/Localizable.strings"].map { notice + $0 }).sorted())
        XCTAssertEqual(try phone.rights("iphone/so.tilo.assistant.notice.entitlements"), [
            "application-identifier": "TESTTEAM00.so.tilo.assistant.notice", "com.apple.developer.team-identifier": "TESTTEAM00",
            "keychain-access-groups": ["TESTTEAM00.so.tilo.assistant.notice", "TESTTEAM00.so.tilo.assistant.told"],
            "get-task-allow": true,
        ] as NSDictionary)
        let said = try phone.info(notice)
        XCTAssertEqual(said["CFBundleIdentifier"] as? String, "so.tilo.assistant.notice")
        XCTAssertEqual(said["TiloTold"] as? String, "so.tilo.assistant.told")
        XCTAssertEqual(said["TiloTeam"] as? String, "TESTTEAM00")
        XCTAssertEqual((said["NSExtension"] as? [String: String])?["NSExtensionPointIdentifier"], "com.apple.usernotifications.service")
        // The extensions are signed before the app that seals them.
        XCTAssertEqual(phone.signed, [
            BuildTests.phoneSigned[0],
            "--force --timestamp=none --sign A Made Up Signer --entitlements WORK/made/iphone/so.tilo.assistant.notice.entitlements "
                + "WORK/made/iphone/Tilo Assistant.app/PlugIns/AssistantNotice.appex",
            BuildTests.phoneSigned[1], BuildTests.phoneSigned[2],
        ])
        XCTAssertTrue(phone.said.contains("dist/apple: push: the build is told while it is closed, through Apple's development service"),
                      "\(phone.said)")
    }

    func testAMacBuildToldToWhoseProfileGrantsPushClaimsItAndCarriesTheNoticeExtension() throws {
        let mac = try build("mac", ["TILO_APPLE_PUSH": "1", "TILO_APPLE_MAC_PROFILE": macProfile("mac.plist", push: "production")])
        let rights = try mac.rights("mac/app.entitlements")
        XCTAssertEqual(rights["com.apple.developer.aps-environment"] as? String, "production")
        XCTAssertEqual(rights["com.apple.application-identifier"] as? String, "TESTTEAM00.so.tilo.assistant")
        XCTAssertEqual(rights["com.apple.developer.team-identifier"] as? String, "TESTTEAM00")
        XCTAssertEqual(rights["com.apple.security.application-groups"] as? [String], ["TESTTEAM00.so.tilo.assistant.told"])
        XCTAssertEqual(rights.count, BuildTests.macRights.count + 4)
        for (key, value) in BuildTests.macRights { XCTAssertEqual(rights[key] as? Bool, value as? Bool, "\(key)") }
        let info = try mac.info("Contents/")
        XCTAssertEqual(info["TiloPush"] as? String, "production")
        XCTAssertEqual(info["TiloTold"] as? String, "so.tilo.assistant.told")

        let notice = "Contents/PlugIns/AssistantNotice.appex/Contents/"
        XCTAssertEqual(mac.files, (BuildTests.macFiles + ["Contents/embedded.provisionprofile", notice + "Info.plist",
                                                           notice + "MacOS/AssistantNotice",
                                                           notice + "Resources/en.lproj/Localizable.strings"]).sorted())
        XCTAssertEqual(try mac.rights("mac/notice.entitlements"), [
            "com.apple.security.app-sandbox": true,
            "com.apple.security.application-groups": ["TESTTEAM00.so.tilo.assistant.told"],
        ] as NSDictionary)
        XCTAssertEqual((try mac.info(notice)["NSExtension"] as? [String: String])?["NSExtensionPointIdentifier"],
                       "com.apple.usernotifications.service")
        XCTAssertEqual(mac.signed, [
            "--force --options runtime --timestamp=none --sign A Made Up Signer --entitlements WORK/made/mac/notice.entitlements "
                + "WORK/made/mac/Tilo Assistant.app/Contents/PlugIns/AssistantNotice.appex",
            BuildTests.macSigned[0], BuildTests.macSigned[1],
        ])
    }

    // MARK: A site on this computer, by plain http

    /// Inside a bundle the system refuses plain http to a name with a dot in it, so a Mac app
    /// built for a Tilo on this computer could not load its own site. Such a build names
    /// exactly its site's name as the one the system lets through by plain http, and nothing
    /// else. A build for an https site names nothing, and is held to https everywhere.
    func testAMacBuildForAPlainHttpSiteOnThisComputerIsLetThroughToThatSiteAloneAndAnHttpsBuildToNone() throws {
        let named = try build("mac", ["TILO_SHELL_SITE": "http://tilo.localhost:24520"])
        XCTAssertEqual(try named.info("Contents/")["TiloSite"] as? String, "http://tilo.localhost:24520")
        XCTAssertEqual(try named.info("Contents/")["NSAppTransportSecurity"] as? NSDictionary,
                       ["NSExceptionDomains": ["tilo.localhost": ["NSExceptionAllowsInsecureHTTPLoads": true]]] as NSDictionary)
        let bare = try build("mac", ["TILO_SHELL_SITE": "http://localhost"])
        XCTAssertEqual(try bare.info("Contents/")["NSAppTransportSecurity"] as? NSDictionary,
                       ["NSExceptionDomains": ["localhost": ["NSExceptionAllowsInsecureHTTPLoads": true]]] as NSDictionary)
        // An address cannot be named: what the system has for one, and no name beside it.
        let address = try build("mac", ["TILO_SHELL_SITE": "http://127.0.0.1:24520"])
        XCTAssertEqual(try address.info("Contents/")["NSAppTransportSecurity"] as? NSDictionary,
                       ["NSAllowsLocalNetworking": true] as NSDictionary)
        for site in ["https://tilo.sh", "https://tilo.wifi.example:24520"] {
            let secure = try build("mac", ["TILO_SHELL_SITE": site])
            XCTAssertEqual(try secure.info("Contents/")["TiloSite"] as? String, site)
            XCTAssertNil(try secure.info("Contents/")["NSAppTransportSecurity"], site)
        }
    }

    // MARK: Told, and the profile does not grant it

    func testAPhoneBuildToldToWhoseProfileGrantsNoPushIsAsOneNotToldAndSaysWhy() throws {
        let asEver = try build("iphone", ["TILO_APPLE_PROFILE": profileForAny()])
        let told = try build("iphone", ["TILO_APPLE_PUSH": "1", "TILO_APPLE_PROFILE": profileForAny()])
        XCTAssertEqual(asEver.files, BuildTests.phoneFiles)
        XCTAssertEqual(told.files, asEver.files)
        for file in ["iphone/so.tilo.assistant.entitlements", "iphone/so.tilo.assistant.widgets.entitlements",
                     "iphone/Tilo Assistant.app/Info.plist"] {
            XCTAssertEqual(try told.bytes(file), try asEver.bytes(file), file)
        }
        XCTAssertEqual(told.signed, BuildTests.phoneSigned)
        XCTAssertEqual(told.said.filter { $0.contains("push") },
                       [BuildTests.sentence + "its profile grants none (README.md, \"Told while the app is closed\")"])
        XCTAssertEqual(asEver.said.filter { $0.contains("push") }, [])
    }

    func testAMacBuildToldToWithNoProfileOrOneThatGrantsNoPushIsAsOneNotToldAndSaysWhy() throws {
        let asEver = try build("mac", [:])
        let bare = try build("mac", ["TILO_APPLE_PUSH": "1"])
        let ungranted = try build("mac", ["TILO_APPLE_PUSH": "1", "TILO_APPLE_MAC_PROFILE": macProfile("mac.plist", push: nil)])
        XCTAssertEqual(asEver.files, BuildTests.macFiles)
        for told in [bare, ungranted] {
            XCTAssertEqual(told.files, asEver.files)
            for file in ["mac/app.entitlements", "mac/Tilo Assistant.app/Contents/Info.plist"] {
                XCTAssertEqual(try told.bytes(file), try asEver.bytes(file), file)
            }
            XCTAssertEqual(told.signed, BuildTests.macSigned)
        }
        XCTAssertEqual(bare.said.filter { $0.contains("push") },
                       [BuildTests.sentence + "it carries no profile (README.md, \"Told while the app is closed\")"])
        XCTAssertEqual(ungranted.said.filter { $0.contains("push") },
                       [BuildTests.sentence + "its profile grants none (README.md, \"Told while the app is closed\")"])
    }

    /// A build that is told to and has the right, and no profile for the extension: it stops
    /// and names the word, and makes no app that would be refused on the phone.
    func testAPhoneBuildWithPushWhoseNoticeExtensionNoProfileCoversStopsAndNamesTheWord() throws {
        let stopped = try build("iphone", [
            "TILO_APPLE_PUSH": "1", "TILO_APPLE_PROFILE": profileWithPush(), "TILO_APPLE_WIDGETS_PROFILE": profileForAny(),
        ], ends: 1)
        XCTAssertEqual(stopped.said.last, "dist/apple: the profile covers TESTTEAM00.so.tilo.assistant, not so.tilo.assistant.notice: "
            + "name the profile of the notice extension in TILO_APPLE_NOTICE_PROFILE")
        // Only the widget extension was signed: neither the notice extension nor the app itself.
        XCTAssertEqual(stopped.signed, [BuildTests.phoneSigned[0]])
    }

    // MARK: The icon

    /// Tilo Assistant is drawn from a drawing of its own, not from Tilo Shell's: on a phone and
    /// in the Dock the two apps are told apart at a glance, by their ground and by their figure.
    func testTheAssistantIsDrawnFromADrawingOfItsOwn() throws {
        let asked = try build("mark", [:])
        let own = URL(fileURLWithPath: try XCTUnwrap(asked.said.last { !$0.isEmpty }))
        XCTAssertTrue(own.path.hasSuffix("/dist/apple/AssistantBundle/AppIcon.svg"), own.path)
        let shell = own.deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("Bundle/AppIcon.svg")
        let mine = try String(contentsOf: own, encoding: .utf8)
        let theirs = try String(contentsOf: shell, encoding: .utf8)
        XCTAssertTrue(mine.contains("<svg") && theirs.contains("<svg"))
        // The colour each drawing fills its whole square with: not the same, so not a shade apart.
        func ground(_ drawing: String) -> String? {
            guard let before = drawing.range(of: "<rect width=\"1024\" height=\"1024\" fill=\"") else { return nil }
            return String(drawing[before.upperBound...].prefix(7))
        }
        XCTAssertNotNil(ground(mine))
        XCTAssertNotNil(ground(theirs))
        XCTAssertNotEqual(ground(mine), ground(theirs))
    }
}
#endif
