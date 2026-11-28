import Foundation

/// Where this app is reached while it is closed, as the person's host takes it in
/// `tilo.telling`'s `addresses#register`: Apple's token for this device, which of its services the
/// token belongs to, the app's identifier, and the key the words of a push are sealed for. The
/// page's session hands it over, because the person's identity is the page's device.
public struct PushAddress: Sendable {
    /// The token Apple's push service gave the app, in lowercase hex.
    public let token: String
    public let service: ClosedBuild.Service
    /// The app the token was given to: its bundle identifier.
    public let topic: String
    public let key: ClosedKey

    public init(token: Data, service: ClosedBuild.Service, topic: String, key: ClosedKey) {
        self.token = token.map { String(format: "%02x", $0) }.joined()
        self.service = service
        self.topic = topic
        self.key = key
    }

    /// The action's argument, as `addresses#register` takes it.
    public var handed: [String: Any] {
        ["kind": "apns", "token": token, "environment": service.rawValue, "topic": topic,
         "key": ["p256dh": key.handedOver.p256dh, "auth": key.handedOver.auth]]
    }

    /// The same as the page reads it.
    public var json: String {
        (try? JSONSerialization.data(withJSONObject: handed))
            .flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }

    /// Where this device is reached, from Apple's token: the key is made and kept the first
    /// time, and none is handed over where it cannot be kept.
    public static func handedOver(token: Data, info: [String: Any]?) -> String? {
        guard let build = ClosedBuild(info: info) else { return nil }
        let keys = build.keys
        var key = keys.read()
        if key == nil {
            let made = ClosedKey.made()
            guard (try? keys.keep(made)) != nil else { return nil }
            key = made
        }
        guard let key else { return nil }
        let topic = (info?["CFBundleIdentifier"] as? String) ?? ""
        return PushAddress(token: token, service: build.service, topic: topic, key: key).json
    }
}
