#if os(macOS)
@testable import TiloLive
import XCTest

final class LiveThingTests: XCTestCase {
    func testAReferenceNamesAnAppOnARecordOrAMessagesView() {
        let document = LiveThing(id: "m1", title: "Report", reference: "record:so.tilo.word-processor:uuid:1f0e-22")
        XCTAssertEqual(document?.content, .record(app: "so.tilo.word-processor", record: "uuid:1f0e-22"))
        XCTAssertEqual(document?.reference, "record:so.tilo.word-processor:uuid:1f0e-22")
        // A message's view is named by its message and its conversation, whose id may hold colons.
        let between = #"direct:[{"person":"ad"},{"agent":"assistant"}]"#
        let view = LiveThing(id: "m2", title: "Tasks", reference: "view:m2:\(between)")
        XCTAssertEqual(view?.content, .messageView(message: "m2", conversation: between))
        XCTAssertEqual(view?.reference, "view:m2:\(between)")
        for words in ["", "record:", "record:so.tilo.notes", "record::uuid:1", "record:so.tilo.notes:", "https://tilo.sh/#/me",
                      "view:", "view:m2", "view::c_chat", "view:m2:", "other:m2:c_chat"] {
            XCTAssertNil(LiveThing(id: "m", title: "", reference: words), words)
        }
    }

    func testEverySentenceOfALiveThingsPlaceHasItsWords() {
        for words in [LiveWords.loading, .offline, .failed, .showLive, .openInTilo, .tryAgain] {
            XCTAssertFalse(words.english.isEmpty)
            XCTAssertTrue(words.rawValue.hasPrefix("live-"))
            // Without a catalogue the sentence reads as written.
            XCTAssertEqual(words.said, words.english)
        }
    }
}

@MainActor
final class LiveStageTests: XCTestCase {
    func testAThingThatCannotBeShownSaysWhy() async throws {
        let engine = TiloEngine(settings: try .init(site: "https://tilo.test"), store: .keepsNothing)
        let stage = LiveStage(engine: engine)
        // A message's view is a thing Tilo's page shows like any other: with the device not in,
        // its place says that, and no longer that Tilo cannot show one.
        let view = LiveThing(id: "v", title: "Tasks", .messageView(message: "m2", conversation: "c_chat"))
        await stage.bring(view)
        XCTAssertEqual(stage.state(of: view), .offline)
        XCTAssertNil(stage.sizes[view.id])
        // The device is not in: a document is not asked for, and the place says so.
        let document = LiveThing(id: "d", title: "Report", .record(app: "so.tilo.word-processor", record: "uuid:1"))
        XCTAssertEqual(stage.state(of: document), .asleep)
        await stage.bring(document)
        XCTAssertEqual(stage.state(of: document), .offline)
        XCTAssertNil(stage.front)
    }
}
#endif
