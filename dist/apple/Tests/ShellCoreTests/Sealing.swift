import ShellCore
import CryptoKit
import Foundation

/// A message made the way the person's server makes one (`web_push::encrypt` in tilo-server:
/// a key of the sender's for this one message, the receiver's public key and secret, a salt,
/// one part that ends in a byte 2), here by the tests themselves with a sender key and a salt
/// they make up. What opens such a message is also held to the standard's own example, which
/// nothing of Tilo made (`ClosedKeyTests`).
enum Sealing {
    static func sealed(_ plain: Data, for receiver: ClosedKey) throws -> Data {
        let sender = P256.KeyAgreement.PrivateKey()
        let salt = Data((0..<16).map { UInt8($0 &* 7 &+ 3) })
        let handed = receiver.handedOver
        let theirs = try P256.KeyAgreement.PublicKey(x963Representation: data(hex: handed.p256dh))
        let shared = try sender.sharedSecretFromKeyAgreement(with: theirs)
        let material = shared.hkdfDerivedSymmetricKey(
            using: SHA256.self, salt: data(hex: handed.auth),
            sharedInfo: Data("WebPush: info\0".utf8) + data(hex: handed.p256dh) + sender.publicKey.x963Representation,
            outputByteCount: 32)
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: material, salt: salt, info: Data("Content-Encoding: aes128gcm\0".utf8),
                                         outputByteCount: 16)
        let nonce = HKDF<SHA256>.deriveKey(inputKeyMaterial: material, salt: salt, info: Data("Content-Encoding: nonce\0".utf8),
                                           outputByteCount: 12)
        let box = try AES.GCM.seal(plain + [2], using: key, nonce: AES.GCM.Nonce(data: nonce.withUnsafeBytes { Data($0) }))
        return salt + Data([0, 0, 16, 0, 65]) + sender.publicKey.x963Representation + box.ciphertext + box.tag
    }

    static func data(hex: String) -> Data {
        var bytes = Data()
        var rest = Substring(hex)
        while rest.count >= 2 {
            bytes.append(UInt8(rest.prefix(2), radix: 16) ?? 0)
            rest = rest.dropFirst(2)
        }
        return bytes
    }

    /// A push as the person's server writes one for a thing that waits (`apple_push.rs`): the
    /// open part with the thing's name, its place and its kind, and the part sealed for `key`.
    static func push(named name: String, place: Int = 12, kind: String = "question", saying said: String?,
                     for key: ClosedKey) throws -> [AnyHashable: Any] {
        var fields: [AnyHashable: Any] = [
            "aps": ["alert": ["loc-key": "TOLD_WAITS"], "mutable-content": 1, "thread-id": "assistant", "interruption-level": "active"],
            "told": name, "seq": place, "kind": kind, "raised": 1,
        ]
        if let said { fields["enc"] = try sealed(Data(said.utf8), for: key).base64EncodedString() }
        return fields
    }
}
