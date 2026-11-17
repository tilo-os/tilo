import ShellCore
import XCTest

/// What the shell offers at the site's door, as it reaches the page and as a press comes back.
final class DoorOfferTests: XCTestCase {
    func testAPressAtTheDoorIsHeardAsTheWayItNames() {
        XCTAssertEqual(PageMessage(body: ["kind": "way-in", "way": "scan"]), .wayIn(.scan))
        XCTAssertEqual(PageMessage(body: ["kind": "way-in", "way": "paste"]), .wayIn(.paste))
        // A way the system does not have is nothing to act on.
        let dropped: [[String: Any]] = [["kind": "way-in"], ["kind": "way-in", "way": "open"], ["kind": "way-in", "way": 7]]
        for body in dropped {
            XCTAssertNil(PageMessage(body: body), "\(body)")
        }
    }

    func testTheScriptCarriesWhatTheDeviceOffersAsData() throws {
        // Whatever a sentence holds, it reaches the page as words and never as code.
        let offer = DoorOffer(said: "The “code”</script> and `${link}` come\\ from\nthere.",
                              ways: [DoorOffer.Way(.scan, "Scan \"code\""), DoorOffer.Way(.paste, "Paste link")])
        let script = Bridge.script(origin: "https://tilo.sh", permission: .notAsked, offer: offer)
        XCTAssertFalse(script.contains("%OFFER%"))
        let line = try XCTUnwrap(script.split(separator: "\n").first { $0.contains("const offer = ") })
        let written = line.trimmingCharacters(in: .whitespaces).dropFirst("const offer = ".count).dropLast()
        let read = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(written.utf8)) as? [String: Any])
        XCTAssertEqual(read["said"] as? String, offer.said)
        XCTAssertEqual(read["ways"] as? [[String: String]],
                       [["way": "scan", "label": "Scan \"code\""], ["way": "paste", "label": "Paste link"]])
        // A shell with nothing to offer hands the page nothing.
        XCTAssertTrue(Bridge.script(origin: "https://tilo.sh", permission: .notAsked).contains("const offer = null;"))
    }

    func testTheSentencesAtTheDoorSayWhereACodeAndALinkComeFrom() {
        // One sentence each, under the buttons that name what happens.
        for words in [Words.wayInMac, Words.wayInPhone] {
            let said = words.said(as: "Tilo Shell")
            XCTAssertTrue(said.contains("“Pair a new device”"), said)
            XCTAssertEqual(said.filter { $0 == "." }.count, 1, said)
        }
        XCTAssertFalse(Words.wayInMac.english.contains("code"))
        XCTAssertEqual([Words.scanCode.english, Words.pasteLink.english], ["Scan code", "Paste link"])
    }
}
