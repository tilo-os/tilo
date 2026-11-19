#if os(macOS)
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Which build the app is, said inside the app on a phone. An iPhone's Settings show a version
/// alone, so two builds of one release read alike there, while the Mac's About window shows
/// "Version 0.1.13 (49855)". A phone's app says that same line itself, from the same two
/// entries of its bundle: at the foot of its way in and under its person at the foot of its
/// list. A Mac, whose About window says it, is handed no line and shows none. Driven with
/// nothing on a screen; with TILO_SHOTS_DIR set, the light and the dark look are kept.
@MainActor
final class BuildLineTests: XCTestCase {
    private static let info: [String: Any] = ["CFBundleShortVersionString": "0.1.13", "CFBundleVersion": "49855", "TiloCommit": "ea721665fa2"]
    private static let line = "Version 0.1.13 (49855)"
    /// An iPhone's width and height in points.
    private static let phone = CGSize(width: 393, height: 852)

    func testABundlesVersionAndBuildNumberAreSaidAsTheMacsAboutWindowShowsThem() {
        XCTAssertEqual(AssistantBuild.said(info: Self.info), Self.line)
        // A bundle that carries no build number says its version alone; a program in no bundle says nothing.
        XCTAssertEqual(AssistantBuild.said(info: ["CFBundleShortVersionString": "0.1.13"]), "Version 0.1.13")
        XCTAssertEqual(AssistantBuild.said(info: ["CFBundleShortVersionString": "0.1.13", "CFBundleVersion": ""]), "Version 0.1.13")
        XCTAssertNil(AssistantBuild.said(info: ["CFBundleVersion": "49855"]))
        XCTAssertNil(AssistantBuild.said(info: nil))
    }

    /// One of the app's views in a window that is never on a screen, as a root hands it its build's line.
    private struct Hosted {
        let window: HeldWindow
        let hosting: NSView
        let drive: Drive

        @MainActor func end() {
            window.contentView = nil
            window.close()
        }
    }

    private func hosted(_ content: MemorySource.Content, build: String?, @ViewBuilder _ view: () -> some View) async -> Hosted {
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await model.prime()
        let rooted = AssistantRoot(model: model) { view().background(Token.content) }
            .environment(\.buildSaid, build)
            .frame(width: Self.phone.width, height: Self.phone.height)
        let hosting = NSHostingView(rootView: rooted)
        hosting.frame = CGRect(origin: .zero, size: Self.phone)
        let window = HeldWindow.make(hosting.frame, [.borderless])
        window.contentView = hosting
        let drive = Drive(window)
        await drive.settle(400)
        return Hosted(window: window, hosting: hosting, drive: drive)
    }

    private func pictures(_ hosted: Hosted, _ name: String) async {
        for dark in [false, true] {
            let look = NSAppearance(named: dark ? .darkAqua : .aqua)
            hosted.window.appearance = look
            hosted.hosting.appearance = look
            await hosted.drive.settle(200)
            hosted.drive.picture("\(name)-\(dark ? "dark" : "light")", of: hosted.hosting)
        }
    }

    func testAWayInThatIsHandedItsBuildSaysItAtItsFoot() async throws {
        var out = MemorySource.Content()
        out.session = .signedOut
        let wayIn = await hosted(out, build: Self.line) { WayIn() }
        defer { wayIn.end() }
        XCTAssertTrue(wayIn.drive.shows(AssistantWords.wayInTitle.said), "the way in does not stand")
        XCTAssertTrue(wayIn.drive.shows(Self.line), "a way in that was handed its build does not say it")
        await pictures(wayIn, "build-line-way-in")
    }

    func testAListThatIsHandedItsBuildSaysItUnderItsPerson() async throws {
        let list = await hosted(Drawing.content(), build: Self.line) { Sidebar() }
        defer { list.end() }
        XCTAssertTrue(list.drive.shows(AssistantWords.assistant.said), "the list does not stand")
        XCTAssertTrue(list.drive.shows(Self.line), "a list that was handed its build does not say it")
        await pictures(list, "build-line-list")
    }

    /// A Mac's About window says the build, so a Mac's window is handed no line and shows none:
    /// neither at its way in, nor in its sidebar, nor anywhere in its app.
    func testAWindowThatIsHandedNoBuildShowsNoSuchLine() async throws {
        var out = MemorySource.Content()
        out.session = .signedOut
        let wayIn = await hosted(out, build: nil) { WayIn() }
        defer { wayIn.end() }
        XCTAssertTrue(wayIn.drive.shows(AssistantWords.wayInTitle.said), "the way in does not stand")
        XCTAssertFalse(wayIn.drive.shows(AssistantWords.versionAlone.said(["version": ""])), "a way in that was handed no build says one")
        let started = try await Started.app()
        defer { started.end() }
        XCTAssertTrue(started.drive.has(AssistantWords.newSubAgent.said), "the Mac's window does not stand")
        XCTAssertFalse(started.drive.shows(AssistantWords.versionAlone.said(["version": ""])), "the Mac's window says its build beside its About window")
    }
}
#endif
