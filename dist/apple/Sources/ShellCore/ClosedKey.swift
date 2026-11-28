import CryptoKit
import Foundation

/// The device's own key and secret for being told while the app is closed.
///
/// The person's host seals what a push says for this key, so that neither Apple's service nor
/// anything else on the way reads a word of it, and names a thing by a tag only this device
/// can tie to it. The app makes it and hands its public half and secret to the host; the
/// notice extension reads it from the place they share.
///
/// It says nothing of itself: printing it, describing it or looking into it shows no byte.
public struct ClosedKey: Sendable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    private let own: P256.KeyAgreement.PrivateKey
    private let secret: Data

    /// A new key and a new secret, from the system's own randomness.
    public static func made() -> ClosedKey {
        var generator = SystemRandomNumberGenerator()
        return ClosedKey(own: P256.KeyAgreement.PrivateKey(),
                         secret: Data((0..<16).map { _ in UInt8.random(in: .min ... .max, using: &generator) }))
    }

    private init(own: P256.KeyAgreement.PrivateKey, secret: Data) {
        self.own = own
        self.secret = secret
    }

    /// The key as its store keeps it: the 32 bytes of the private key, then the 16 of the secret.
    public init?(kept: Data) {
        guard kept.count == 48, let own = try? P256.KeyAgreement.PrivateKey(rawRepresentation: kept.prefix(32)) else { return nil }
        self.init(own: own, secret: Data(kept.suffix(16)))
    }

    /// What a store keeps of it. Only a store asks for this.
    public var kept: Data { own.rawRepresentation + secret }

    /// What the person's server is handed, in hex as it takes them: the public half (65 bytes,
    /// uncompressed) and the secret (16 bytes).
    public var handedOver: (p256dh: String, auth: String) {
        (ClosedKey.hex(own.publicKey.x963Representation), ClosedKey.hex(secret))
    }

    /// Open what the person's host sealed for this device: one `aes128gcm` message as a web
    /// push carries one (RFC 8291, RFC 8188). Nil for anything else, and for a message sealed
    /// for another key or changed on the way.
    ///
    /// The message is salt (16 bytes), four bytes, one byte with the length of the sender's
    /// public key, that key (65 bytes), then the sealed part with its tag.
    public func open(_ message: Data) -> Data? {
        let bytes = [UInt8](message)
        guard bytes.count > 21, bytes[20] == 65, bytes.count >= 21 + 65 + 16 else { return nil }
        let salt = Data(bytes[0..<16])
        let sender = Data(bytes[21..<86])
        let sealed = Data(bytes[86...])
        guard let theirs = try? P256.KeyAgreement.PublicKey(x963Representation: sender),
              let shared = try? own.sharedSecretFromKeyAgreement(with: theirs)
        else { return nil }
        let mine = own.publicKey.x963Representation
        let material = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: secret,
                                                      sharedInfo: Data("WebPush: info\0".utf8) + mine + sender, outputByteCount: 32)
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: material, salt: salt,
                                         info: Data("Content-Encoding: aes128gcm\0".utf8), outputByteCount: 16)
        let nonce = HKDF<SHA256>.deriveKey(inputKeyMaterial: material, salt: salt,
                                           info: Data("Content-Encoding: nonce\0".utf8), outputByteCount: 12)
        guard let once = try? AES.GCM.Nonce(data: nonce.withUnsafeBytes { Data($0) }),
              let box = try? AES.GCM.SealedBox(nonce: once, ciphertext: sealed.dropLast(16), tag: sealed.suffix(16)),
              var plain = try? AES.GCM.open(box, using: key)
        else { return nil }
        // The one part of the message ends in a byte 2; nothing but empty bytes may follow it.
        while plain.last == 0 { plain.removeLast() }
        guard plain.last == 2 else { return nil }
        return Data(plain.dropLast())
    }

    static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    public var description: String { "ClosedKey" }
    public var debugDescription: String { "ClosedKey" }
    public var customMirror: Mirror { Mirror(self, children: [], displayStyle: .struct) }
}
