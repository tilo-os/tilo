import ShellCore
import CryptoKit
import XCTest

/// The device's key for being told while the app is closed: what it opens, what it names,
/// and what it lets out of itself. Every key here is made up, or is the published example of
/// the standard; none is a person's, and no keychain is asked.
final class ClosedKeyTests: XCTestCase {
    private func bytes(base64url text: String) -> Data {
        var padded = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while padded.count % 4 != 0 { padded += "=" }
        return Data(base64Encoded: padded) ?? Data()
    }

    /// The worked example the standard for web push messages gives (RFC 8291, section 5), all
    /// of it public: the receiver's private key, its secret, and the message a sender made for
    /// it with a fixed key and salt. It is the one message here that nothing of Tilo made.
    private var examplesKey: ClosedKey? {
        ClosedKey(kept: bytes(base64url: "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94") + bytes(base64url: "BTBZMqHH6r4Tts7J_aSIgg"))
    }

    private let examplesMessage = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml"
        + "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT"
        + "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"

    func testItOpensTheStandardsOwnExampleMessage() throws {
        let key = try XCTUnwrap(examplesKey)
        let opened = try XCTUnwrap(key.open(bytes(base64url: examplesMessage)))
        XCTAssertEqual(String(decoding: opened, as: UTF8.self), "When I grow up, I want to be a watermelon")
        // The public half it hands a server is the one the example names.
        XCTAssertEqual(key.handedOver.p256dh, bytes(base64url: "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4")
            .map { String(format: "%02x", $0) }.joined())
        XCTAssertEqual(key.handedOver.p256dh.count, 130)
        XCTAssertTrue(key.handedOver.p256dh.hasPrefix("04"))
        XCTAssertEqual(key.handedOver.auth.count, 32)
    }

    func testItOpensNothingThatWasChangedOnTheWayOrSealedForAnotherKey() throws {
        let key = try XCTUnwrap(examplesKey)
        let message = bytes(base64url: examplesMessage)
        // The salt, the sender's key, the sealed part and its tag. The four bytes after the salt
        // say how long a part may be and are read past: a message here is one part.
        for place in [0, 15, 21, 30, 85, 86, message.count - 20, message.count - 1] {
            var changed = message
            changed[place] ^= 1
            XCTAssertNil(key.open(changed), "a message changed at byte \(place) was opened")
        }
        XCTAssertNil(key.open(message.prefix(60)))
        XCTAssertNil(key.open(Data()))
        XCTAssertNil(ClosedKey.made().open(message), "another device's key opened it")
    }

    func testItOpensWhatIsSealedForItTheWayItsServerSealsAndNoOtherDeviceDoes() throws {
        let key = ClosedKey.made()
        let said = Data(#"{"from":"Assistant","words":"The form is ready to sign.","actions":[]}"#.utf8)
        let message = try Sealing.sealed(said, for: key)
        XCTAssertEqual(key.open(message), said)
        XCTAssertNil(ClosedKey.made().open(message))
        // Kept and read again, it is the same key.
        let again = try XCTUnwrap(ClosedKey(kept: key.kept))
        XCTAssertEqual(again.open(message), said)
        XCTAssertEqual(again.handedOver.p256dh, key.handedOver.p256dh)
    }

    func testAKeySaysNothingOfItselfAndIsNoKeyWhenItsBytesAreNot() {
        let key = ClosedKey.made()
        let secret = key.handedOver.auth
        for said in ["\(key)", String(describing: key), String(reflecting: key)] {
            XCTAssertFalse(said.contains(secret))
        }
        XCTAssertEqual("\(key)", "ClosedKey")
        XCTAssertEqual(Mirror(reflecting: key).children.count, 0)
        XCTAssertNil(ClosedKey(kept: Data(repeating: 1, count: 47)))
        XCTAssertNil(ClosedKey(kept: Data(repeating: 0, count: 48)), "thirty-two empty bytes are no private key")
        // Two keys made are two keys.
        XCTAssertNotEqual(ClosedKey.made().handedOver.p256dh, key.handedOver.p256dh)
    }

    func testAStoreInMemoryKeepsOneKeyAndForgetsIt() throws {
        let store = MemoryClosedKeys()
        XCTAssertNil(store.read())
        let key = ClosedKey.made()
        try store.keep(key)
        XCTAssertEqual(store.read()?.handedOver.p256dh, key.handedOver.p256dh)
        try store.keep(ClosedKey.made())
        XCTAssertNotEqual(store.read()?.handedOver.p256dh, key.handedOver.p256dh)
        XCTAssertEqual(store.kept, 2)
        store.forget()
        XCTAssertNil(store.read())
    }

    func testABuildHasPushOnlyWhereItsBundleSaysSo() {
        XCTAssertNil(ClosedBuild(info: nil))
        XCTAssertNil(ClosedBuild(info: ["TiloTeam": "TESTTEAM00"]))
        XCTAssertNil(ClosedBuild(info: ["TiloPush": "development", "TiloTeam": "", "TiloTold": "so.tilo.assistant.told"]))
        XCTAssertNil(ClosedBuild(info: ["TiloPush": "sandbox", "TiloTeam": "TESTTEAM00", "TiloTold": "so.tilo.assistant.told"]))
        let build = ClosedBuild(info: ["TiloPush": "production", "TiloTeam": "TESTTEAM00", "TiloTold": "so.tilo.assistant.told"])
        XCTAssertEqual(build?.service, .production)
        XCTAssertEqual(build?.group, "TESTTEAM00.so.tilo.assistant.told")
        // The notice extension's bundle names the same place and no service.
        XCTAssertEqual(KeychainClosedKeys(info: ["TiloTeam": "TESTTEAM00", "TiloTold": "so.tilo.assistant.told"])?.group, build?.group)
        XCTAssertNil(KeychainClosedKeys(info: ["TiloTeam": "TESTTEAM00"]))
    }
}
