#if os(macOS)
@testable import TiloLive
import XCTest

final class LiveThingTests: XCTestCase {
    func testAReferenceNamesAnAppOnARecordOrAWrittenView() {
        let document = LiveThing(id: "m1", title: "Report", reference: "record:so.tilo.word-processor:uuid:1f0e-22")
        XCTAssertEqual(document?.content, .record(app: "so.tilo.word-processor", record: "uuid:1f0e-22"))
        XCTAssertEqual(document?.reference, "record:so.tilo.word-processor:uuid:1f0e-22")
        let view = LiveThing(id: "m2", title: "Open tasks", reference: "view:view shown() { ui.Card(count: 3) }")
        XCTAssertEqual(view?.content, .writtenView(source: "view shown() { ui.Card(count: 3) }"))
        for words in ["", "record:", "record:so.tilo.notes", "record::uuid:1", "record:so.tilo.notes:", "https://tilo.sh/#/me"] {
            XCTAssertNil(LiveThing(id: "m", title: "", reference: words), words)
        }
    }

    func testEverySentenceOfALiveThingsPlaceHasItsWords() {
        for words in [LiveWords.loading, .offline, .failed, .notYet, .showLive, .openInTilo, .tryAgain] {
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
        let view = LiveThing(id: "v", title: "Open tasks", .writtenView(source: "view shown() {}"))
        await stage.bring(view)
        XCTAssertEqual(stage.standing(of: view), .notYet)
        // The device is not in: a document is not asked for, and the place says so.
        let document = LiveThing(id: "d", title: "Report", .record(app: "so.tilo.word-processor", record: "uuid:1"))
        XCTAssertEqual(stage.standing(of: document), .asleep)
        await stage.bring(document)
        XCTAssertEqual(stage.standing(of: document), .offline)
        XCTAssertNil(stage.front)
    }
}
#endif
