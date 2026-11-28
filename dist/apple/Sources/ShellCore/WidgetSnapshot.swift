import Foundation
#if os(iOS)
import ActivityKit
#endif
#if canImport(WidgetKit)
import WidgetKit
#endif

/// What Tilo's home-screen widget shows: the things that wait for the person, in the words their
/// host tells outside Tilo.
///
/// A page shows and withdraws notices while the app runs, each one a thing that waits; the shell
/// keeps the newest few in the app group the app and its widget share, and asks the widget to
/// draw again. The widget holds no copy of the log: it has what the page showed, as
/// `words_outside` gave it.
public struct WidgetSnapshot: Codable, Equatable, Sendable {
    public struct Item: Codable, Equatable, Sendable {
        public var title: String
        public var body: String
        /// The link a tap opens; empty where the page named none.
        public var link: String

        public init(title: String, body: String = "", link: String = "") {
            self.title = title
            self.body = body
            self.link = link
        }
    }

    public var items: [Item]
    /// How many things the widget shows: a home-screen card reads as much.
    public static let most = 3
    /// The file in the app group.
    public static let file = "widget.json"

    public init(items: [Item] = []) {
        self.items = items
    }

    /// The app group the app and its widget share: `build.sh` writes it into both signatures and
    /// names it in both `Info.plist`s (`TiloGroup`), since the widget's bundle id is not the app's.
    public static func group(bundle: Bundle = .main) -> URL? {
        guard let group = bundle.object(forInfoDictionaryKey: "TiloGroup") as? String,
              !group.isEmpty
        else { return nil }
        return FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    }

    /// What the group keeps, or nothing where it keeps none.
    public static func load(in group: URL?) -> WidgetSnapshot {
        guard let group,
              let data = try? Data(contentsOf: group.appendingPathComponent(file)),
              let snapshot = try? JSONDecoder().decode(WidgetSnapshot.self, from: data)
        else { return WidgetSnapshot() }
        return snapshot
    }

    /// Keep `self` in the group and ask the widget to draw again.
    public func write(in group: URL?, reload: Bool = true) {
        guard let group else { return }
        try? FileManager.default.createDirectory(at: group, withIntermediateDirectories: true)
        if let data = try? JSONEncoder().encode(self) {
            try? data.write(to: group.appendingPathComponent(Self.file), options: .atomic)
        }
        if reload { WidgetSnapshot.reload() }
    }

    /// Ask the system to draw the widget again: what it reads changed.
    public static func reload() {
        #if canImport(WidgetKit)
        WidgetCenter.shared.reloadAllTimelines()
        #endif
    }
}

/// The URL a home-screen widget opens for one thing: the shell's own scheme, carrying the page's
/// own link, so a tap opens where the page would have gone. The page names its links; the shell
/// only carries one through and hands it back.
public enum WidgetLink {
    public static let scheme = "tilo-phone"

    /// The URL that opens `activation`, or nothing where the page named none.
    public static func url(for activation: String) -> String {
        guard !activation.isEmpty else { return "" }
        var parts = URLComponents()
        parts.scheme = scheme
        parts.host = "open"
        parts.queryItems = [URLQueryItem(name: "link", value: activation)]
        return parts.string ?? ""
    }

    /// The page link a URL names, or nothing for a URL that is not the shell's.
    public static func activation(in url: URL) -> String? {
        guard url.scheme?.lowercased() == scheme, url.host == "open",
              let parts = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        return parts.queryItems?.first { $0.name == "link" }?.value
    }

    public static func turn(in url: URL, origin: String) -> URL? {
        guard url.scheme?.lowercased() == scheme, url.host == "turn",
              let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let workspace = value("workspace", in: parts), let app = value("app", in: parts)
        else { return nil }
        let record = value("record", in: parts).map { "/\($0)" } ?? ""
        return URL(string: "\(origin)/#/w/\(workspace)/\(app)\(record)")
    }
    private static func value(_ name: String, in parts: URLComponents) -> String? {
        parts.queryItems?.first { $0.name == name }?.value.flatMap { $0.isEmpty ? nil : $0 }
    }
}

public struct LiveTurnWords: Codable, Hashable, Sendable {
    public var from: String
    public var asked: String
    public var words: String
    public var opens: String
    public init(from: String, asked: String, words: String = "", opens: String = "") {
        (self.from, self.asked, self.words, self.opens) = (from, asked, words, opens)
    }
}

#if os(iOS)
public struct TiloLiveTurn: ActivityAttributes {
    public typealias ContentState = LiveTurnState
    public var name: String
    public init(name: String) { self.name = name }
}

public struct LiveTurnState: Codable, Hashable, Sendable {
    public enum Phase: String, Codable, Hashable, Sendable {
        case working, asking, answered, stopped, failed, cancelled
        public var isOver: Bool { self != .working && self != .asking }
    }

    public var phase: Phase
    public var step: Int
    public var steps: Int
    public var name: String
    public var enc: String?
    public init(phase: Phase = .working, step: Int = 0, steps: Int = 0, name: String, enc: String? = nil) {
        (self.phase, self.step, self.steps, self.name, self.enc) = (phase, step, steps, name, enc)
    }
    public var progress: Double? { steps > 0 ? min(max(Double(step) / Double(steps), 0), 1) : nil }
}
#endif

