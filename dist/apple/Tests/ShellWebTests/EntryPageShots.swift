#if os(macOS)
import AppKit
import ShellCore
@testable import ShellWeb
import WebKit
import XCTest

/// Pictures of a site's entry page as the apps show it, for a person to look at. They are taken
/// with nothing on screen, and only when asked for:
///
///     TILO_SHOTS_DIR=/tmp/entry TILO_SHOTS_SITE=http://tilo.localhost:24120 swift test --filter EntryPageShots
///
/// The site is opened as a device it has never seen, which stands at its entry page. Nothing is
/// kept and nobody is signed in. The pictures are the Mac's web view at a Mac window's size
/// and at an iPhone's width; an iPhone's own come from a simulator.
@MainActor
final class EntryPageShots: XCTestCase {
    func testPicturesOfTheEntryPage() async throws {
        let asked = ProcessInfo.processInfo.environment
        guard let folder = asked["TILO_SHOTS_DIR"], let site = asked["TILO_SHOTS_SITE"] else {
            throw XCTSkip("taken when TILO_SHOTS_DIR names a folder and TILO_SHOTS_SITE a Tilo site")
        }
        let paste = WayInOffer.Way(.paste, Words.pasteLink.said)
        let mac = WayInOffer(said: Words.wayInMac.said, ways: [paste])
        let phone = WayInOffer(said: Words.wayInPhone.said, ways: [WayInOffer.Way(.scan, Words.scanCode.said), paste])
        // The site as a whole, and the page that shows one app.
        let whole = try ShellSettings(site: site)
        let oneApp = try ShellSettings(site: site, app: "so.tilo.assistant", alone: true)
        let window = CGSize(width: 1100, height: 760), narrow = CGSize(width: 402, height: 874)
        let refusal = Words.noLink.said(site: whole.origin)
        try await picture("mac-site", of: whole, mac, window, into: folder)
        try await picture("mac-one-app", of: oneApp, mac, window, into: folder)
        try await picture("mac-refused-link", of: whole, mac, window, into: folder, saying: refusal)
        try await picture("mac-small-window", of: whole, mac, CGSize(width: 380, height: 480), into: folder)
        try await picture("narrow-site", of: whole, phone, narrow, into: folder)
        try await picture("narrow-one-app", of: oneApp, phone, narrow, into: folder)
        try await picture("narrow-refused-link", of: whole, phone, narrow, into: folder, saying: refusal)
    }

    private func picture(_ name: String, of settings: ShellSettings, _ offer: WayInOffer, _ size: CGSize,
                         into folder: String, saying refusal: String? = nil) async throws {
        // A window that is never shown: a web view draws for a picture once it is in one.
        let frame = CGRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: frame, styleMask: [.titled], backing: .buffered, defer: false)
        let host = PageHost(settings: settings, permission: .notAsked, offer: offer, keepsData: false, frame: frame)
        let heard = Heard()
        host.delegate = heard
        window.contentView = host.view
        host.open()
        for _ in 0..<1800 where heard.entry?.wantsWayIn != true {
            try await Task.sleep(for: .milliseconds(100))
        }
        // A picture of anything but the entry page would say nothing of it.
        guard heard.entry?.wantsWayIn == true else { return XCTFail("\(name): the site did not keep this device out") }
        try await Task.sleep(for: .seconds(2))
        if let refusal {
            let said = await host.say(onEntryPage: refusal)
            XCTAssertTrue(said, "\(name): the entry page shows no way in to say it under")
            try await Task.sleep(for: .milliseconds(300))
        }
        let shot = try await host.view.takeSnapshot(configuration: WKSnapshotConfiguration())
        let drawn = try XCTUnwrap(shot.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:)))
        let png = try XCTUnwrap(drawn.representation(using: .png, properties: [:]))
        try png.write(to: URL(fileURLWithPath: folder).appendingPathComponent("\(name).png"))
    }
}

@MainActor
private final class Heard: PageHostDelegate {
    var entry: PageEntry?

    func page(_ host: PageHost, said message: PageMessage) {
        if case .entry(let entry) = message { self.entry = entry }
    }
    func page(_ host: PageHost, attention: Attention) {}
    func pageArrived(_ host: PageHost) {}
    func page(_ host: PageHost, failed error: any Error) {}
    func page(_ host: PageHost, leavesFor url: URL) {}
}
#endif
