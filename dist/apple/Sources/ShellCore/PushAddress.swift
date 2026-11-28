import Foundation
#if os(iOS)
import ActivityKit
#endif
public struct LivePush: Equatable, Sendable {
    public var start = "", update = "", turn = ""
    public init() {}
    public var isEmpty: Bool { start.isEmpty }
}

#if os(iOS)
@MainActor
public final class LiveActivities {
    public static let shared = LiveActivities()
    public private(set) var push = LivePush()
    public var changed: ((LivePush) -> Void)?
    private var start: Task<Void, Never>?
    private var running: Task<Void, Never>?
    private init() {}
    public func watch() {
        let activities = Activity<TiloLiveTurn>.self
        start = start ?? Task { @MainActor [weak self] in
            for await token in activities.pushToStartTokenUpdates {
                guard let self else { return }
                self.push.start = Self.hex(token)
                self.changed?(self.push)
            }
        }
        running = running ?? Task { @MainActor [weak self] in
            for await activity in activities.activityUpdates {
                let name = activity.attributes.name
                for await token in activity.pushTokenUpdates {
                    guard let self else { return }
                    self.push.update = Self.hex(token)
                    self.push.turn = name
                    self.changed?(self.push)
                }
            }
        }
    }
    private static func hex(_ token: Data) -> String { token.map { String(format: "%02x", $0) }.joined() }
}
#endif
public struct PushAddress: Sendable {
    public let token: String
    public let service: ClosedBuild.Service
    public let topic: String
    public let key: ClosedKey
    public let live: LivePush?
    public init(token: Data, service: ClosedBuild.Service, topic: String, key: ClosedKey, live: LivePush? = nil) {
        self.token = token.map { String(format: "%02x", $0) }.joined()
        (self.service, self.topic, self.key, self.live) = (service, topic, key, live)
    }

    public var handed: [String: Any] {
        var handed: [String: Any] = ["kind": "apns", "token": token, "environment": service.rawValue, "topic": topic,
                                     "key": ["p256dh": key.handedOver.p256dh, "auth": key.handedOver.auth]]
        if let live, !live.isEmpty {
            handed["live"] = ["start": live.start, "update": live.update, "turn": live.turn]
        }
        return handed
    }

    public var json: String {
        (try? JSONSerialization.data(withJSONObject: handed)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }

    public static func handedOver(token: Data, info: [String: Any]?, live: LivePush? = nil) -> String? {
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
        return PushAddress(token: token, service: build.service, topic: topic, key: key, live: live).json
    }
}
