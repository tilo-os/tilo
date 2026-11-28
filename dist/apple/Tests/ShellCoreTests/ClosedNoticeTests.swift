import ShellCore
import UserNotifications
import XCTest

/// What the notice extension makes of a push: the host's words where the sealed part opens
/// with this device's key, and nothing where it does not, so that the plain line stands.
final class ClosedNoticeTests: XCTestCase {
    private let key = ClosedKey.made()
    private let said = #"{"from":"Travel helper","words":"May I send the form?","important":true,"# + #""actions":[{"id":"allow","label":"Allow","main":true},{"id":"deny","label":"Deny","main":false}]}"#

    func testAPushThatOpensShowsWhoToldItTheMarkTheHostsWordsAndItsButtons() throws {
        let name = "6b772338188df9437a7c79158c3ce70b"
        let push = try XCTUnwrap(ClosedPush(Sealing.push(named: name, saying: said, for: key)))
        XCTAssertEqual(push.name, name)
        XCTAssertFalse(push.isGone)
        let opened = try XCTUnwrap(push.opened(with: key))
        XCTAssertEqual(opened.title, "Travel helper")
        XCTAssertEqual(opened.subtitle, "Important")
        XCTAssertEqual(opened.words, "May I send the form?")
        XCTAssertEqual(opened.actions.map(\.id), ["allow", "deny"])
        // A notice's buttons are kept under the buttons themselves, so the page's notice of the
        // same thing finds one category and one set of answers.
        XCTAssertEqual(opened.category, "tilo.told.allow:Allow|deny:Deny")
        XCTAssertEqual(ClosedPush.category(of: []), "tilo.told")
        // A page's notice named as a push names a thing is that thing's notice.
        XCTAssertTrue(ClosedPush.isName("6b772338188df9437a7c79158c3ce70b"))
        XCTAssertFalse(ClosedPush.isName("notice:6b772338188df9437a7c79158c3ce70b"))
        XCTAssertFalse(ClosedPush.isName(String(repeating: "z", count: 32)))
        // What the system is handed: the host's words under the push's own name, nothing sealed kept.
        let content = UNMutableNotificationContent()
        content.userInfo = Sealing.push(named: name, saying: said, for: key)
        let shown = ClosedPush.shown(content, opened: opened, named: name)
        XCTAssertEqual(shown.title, "Travel helper")
        XCTAssertEqual(shown.subtitle, "Important")
        XCTAssertEqual(shown.body, "May I send the form?")
        XCTAssertEqual(shown.categoryIdentifier, opened.category)
        XCTAssertEqual(shown.threadIdentifier, name)
        XCTAssertNil(shown.userInfo["enc"], "the sealed part is kept nowhere once opened")
        // What names nobody is Tilo, carries no mark and offers no buttons.
        let quiet = #"{"from":"  ","words":"Stretch now.","important":false,"actions":[]}"#
        let plain = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", kind: "reminder", saying: quiet, for: key)))
            .opened(with: key)
        XCTAssertEqual(plain?.title, "Tilo")
        XCTAssertNil(plain?.subtitle)
        XCTAssertEqual(plain?.words, "Stretch now.")
        XCTAssertTrue(plain?.actions.isEmpty == true)
    }

    func testThePlainLineStandsWhereNothingIsSealedOrNothingOpens() throws {
        // A workspace whose words stay on its host: the push carries no sealed part.
        let bare = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", saying: nil, for: key)))
        XCTAssertNil(bare.sealed)
        XCTAssertNil(bare.opened(with: key))
        // Sealed for the key this device had before its person signed in anew.
        let old = try XCTUnwrap(ClosedPush(Sealing.push(named: "n", saying: said, for: ClosedKey.made())))
        XCTAssertNil(old.opened(with: key))
        // Not sealed at all, no words, or not what a host writes.
        var fields = try Sealing.push(named: "n", saying: nil, for: key)
        fields["enc"] = "bm90IHNlYWxlZA=="
        XCTAssertNil(ClosedPush(fields)?.opened(with: key))
        fields["enc"] = "not even base64"
        XCTAssertNil(ClosedPush(fields)?.opened(with: key))
        for nothing in [#"{"from":"A","words":"  ","important":false,"actions":[]}"#, #"{"title":"A"}"#, "[]"] {
            XCTAssertNil(try XCTUnwrap(ClosedPush(Sealing.push(named: "n", saying: nothing, for: key))).opened(with: key), nothing)
        }
    }

    func testAPushThatTakesAThingBackShowsNothingAndNamesTheNoticeToTakeAway() throws {
        let gone = try XCTUnwrap(ClosedPush(["aps": ["content-available": 1], "told": "rf-qsLfA_59buuS5z2pKfA", "gone": true]))
        XCTAssertTrue(gone.isGone)
        XCTAssertEqual(gone.name, "rf-qsLfA_59buuS5z2pKfA")
        XCTAssertNil(gone.opened(with: key))
    }

    func testANotificationThatIsNoPushOfTilosIsLeftAlone() {
        XCTAssertNil(ClosedPush(["aps": ["alert": "Hello"]]))
        XCTAssertNil(ClosedPush(["told": ""]))
        XCTAssertNil(ClosedPush(["told": 7]))
        XCTAssertNil(ClosedPush([:]))
    }
}
