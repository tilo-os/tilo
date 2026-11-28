import Foundation
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
}
