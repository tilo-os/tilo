import Foundation
import ShellCore
import UserNotifications
import XCTest

/// A push the person's host sealed for a device (`apple_push.rs` `sealed`), made by the Rust side
/// and kept here (`Fixtures/SealedPush.json`): the extension opens exactly that, under the name
/// the page's session works out, and the address handed over is the one the phone makes.
final class SealedFixtureTests: XCTestCase {
    private func fixture() throws -> [String: Any] {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("Fixtures/SealedPush.json")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    private func bytes(hex: String) -> Data {
        var bytes = Data()
        var rest = Substring(hex)
        while rest.count >= 2 {
            bytes.append(UInt8(rest.prefix(2), radix: 16) ?? 0)
            rest = rest.dropFirst(2)
        }
        return bytes
    }

    func testTheExtensionOpensThePushTheRustSenderSealed() throws {
        let fixture = try fixture()
        let kept = bytes(hex: try XCTUnwrap(fixture["kept"] as? String))
        let key = try XCTUnwrap(ClosedKey(kept: kept))
        let told = try XCTUnwrap(fixture["told"] as? String)
        let fields: [AnyHashable: Any] = [
            "aps": ["alert": ["loc-key": "TOLD_WAITS"], "mutable-content": 1], "told": told,
            "enc": try XCTUnwrap(fixture["sealed"] as? String),
        ]
        let opened = try XCTUnwrap(try XCTUnwrap(ClosedPush(fields)).opened(with: key))
        XCTAssertEqual(opened.title, fixture["from"] as? String)
        XCTAssertEqual(opened.words, fixture["words"] as? String)
        let actions = try XCTUnwrap(fixture["actions"] as? [[String: Any]])
        XCTAssertEqual(opened.actions.map(\.id), actions.compactMap { $0["id"] as? String })
        XCTAssertEqual(opened.actions.map(\.label), actions.compactMap { $0["label"] as? String })
        // What the system is handed: the host's words under the push's own name, nothing sealed kept.
        let content = UNMutableNotificationContent()
        content.userInfo = fields
        let shown = ClosedPush.shown(content, opened: opened, named: told)
        XCTAssertEqual(shown.title, fixture["from"] as? String)
        XCTAssertEqual(shown.body, fixture["words"] as? String)
        XCTAssertEqual(shown.threadIdentifier, told)
        XCTAssertNil(shown.userInfo["enc"])

        // The address the page hands its host is the `addresses#register` argument the phone makes.
        let address = PushAddress(token: Data([0xab, 0xcd]), service: .development,
                                  topic: "so.tilo.assistant.shell", key: key)
        XCTAssertEqual(address.token, "abcd")
        let handed = address.handed
        XCTAssertEqual(handed["kind"] as? String, "apns")
        XCTAssertEqual(handed["environment"] as? String, "development")
        XCTAssertEqual(handed["topic"] as? String, "so.tilo.assistant.shell")
        let named = handed["key"] as? [String: String]
        XCTAssertEqual(named?["p256dh"], key.handedOver.p256dh)
        XCTAssertEqual(named?["auth"], key.handedOver.auth)
        let json = (try? JSONSerialization.jsonObject(with: Data(address.json.utf8))) as? [String: Any]
        XCTAssertEqual(json?["kind"] as? String, "apns")
        XCTAssertEqual(json?["token"] as? String, "abcd")
    }
}
