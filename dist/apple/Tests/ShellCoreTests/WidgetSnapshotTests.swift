import XCTest
@testable import ShellCore

final class WidgetSnapshotTests: XCTestCase {
    /// What the shell kept is what the widget reads: the same things, in the same order.
    func testWhatTheWidgetReadsIsWhatTheShellKept() throws {
        let group = FileManager.default.temporaryDirectory
            .appendingPathComponent("tilo-widget-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: group) }
        let kept = WidgetSnapshot(items: [
            .init(title: "Deploy", body: "The deploy is waiting"),
            .init(title: "Scout needs you", body: "Open Tilo to see it."),
        ])
        kept.write(in: group, reload: false)
        XCTAssertEqual(WidgetSnapshot.load(in: group), kept)
        // No group, or none written: nothing waits, and the card says so rather than nothing.
        XCTAssertEqual(WidgetSnapshot.load(in: nil), WidgetSnapshot())
        XCTAssertEqual(WidgetSnapshot.load(in: group.appendingPathComponent("absent")),
                       WidgetSnapshot())
    }

    /// A tap on one thing opens where the page would have gone: the shell's own URL carries the
    /// page's link and gives it back unchanged, and nothing else is one.
    func testATapOnOneThingCarriesThePagesLink() throws {
        let activation = "tilo-mention:so.tilo.chat?r1#res#anchor"
        let written = WidgetLink.url(for: activation)
        let url = try XCTUnwrap(URL(string: written))
        XCTAssertEqual(WidgetLink.activation(in: url), activation)
        XCTAssertEqual(WidgetLink.url(for: ""), "")
        XCTAssertNil(WidgetLink.activation(in: try XCTUnwrap(URL(string: "https://tilo.sh"))))
    }
}
