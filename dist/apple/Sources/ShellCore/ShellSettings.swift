import Foundation

/// Where the shell finds Tilo: the site it opens and the one app it shows there.
///
/// The site is never written into the source. A build names it in the bundle's
/// `Info.plist` (`TiloSite`), and a person changes it in Settings or with
/// `-TiloSite <origin>` on the command line.
public struct ShellSettings: Equatable, Sendable {
    /// The key of the site, in the defaults and in `Info.plist`.
    public static let siteKey = "TiloSite"
    /// The key of the app, in the defaults and in `Info.plist`.
    public static let appKey = "TiloApp"
    /// The site a build that names none opens.
    public static let productionSite = "https://tilo.sh"
    /// The app a build that names none shows: Chat, until the Assistant is in the page.
    public static let defaultApp = "so.tilo.chat"

    /// Why an address is not a Tilo site.
    public enum Refusal: Error, Equatable, Sendable {
        /// Not an origin at all, or one with a path, a query or a sign-in.
        case notAnAddress
        /// Plain `http` to anything but this computer: a page there is no secure
        /// context, so it has neither its keys nor WebGPU.
        case notSecure
        /// Not an application id.
        case notAnApp
    }

    /// The site's origin, as `scheme://host[:port]` with no path.
    public let site: URL
    /// The id of the app the shell shows.
    public let app: String

    public init(site: String, app: String = ShellSettings.defaultApp) throws(Refusal) {
        let written = site.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let parts = URLComponents(string: written), let scheme = parts.scheme?.lowercased(),
              let host = parts.host?.lowercased(), !host.isEmpty,
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty || parts.path == "/"
        else { throw .notAnAddress }
        guard scheme == "https" || scheme == "http" else { throw .notAnAddress }
        guard scheme == "https" || ShellSettings.isThisComputer(host) else { throw .notSecure }
        guard app.wholeMatch(of: /[a-z0-9]+(\.[a-z0-9][a-z0-9-]*)+/) != nil else { throw .notAnApp }
        var origin = URLComponents()
        origin.scheme = scheme
        origin.host = host
        let standard = scheme == "https" ? 443 : 80
        origin.port = parts.port == standard ? nil : parts.port
        guard let url = origin.url else { throw .notAnAddress }
        self.site = url
        self.app = app
    }

    /// The settings a launch runs with: what the person or the command line chose, else what
    /// the bundle names, else the production site.
    public static func load(defaults: UserDefaults = .standard,
                            info: [String: Any]? = Bundle.main.infoDictionary) -> Result<ShellSettings, Refusal> {
        // An empty choice is no choice: the field in the phone's Settings starts empty.
        let named = { (key: String) -> String? in
            [defaults.string(forKey: key), info?[key] as? String].compactMap(\.self).first { !$0.isEmpty }
        }
        do {
            return .success(try ShellSettings(site: named(siteKey) ?? productionSite,
                                              app: named(appKey) ?? defaultApp))
        } catch {
            return .failure(error)
        }
    }

    /// Keep a site or an app named on the command line (`-TiloSite <origin>`), so that it is
    /// given once, by whoever installs a build, and not again by the person who opens it.
    /// What cannot be used is not kept.
    public static func keepArguments(in defaults: UserDefaults = .standard) {
        let given = defaults.volatileDomain(forName: UserDefaults.argumentDomain)
        if let site = given[siteKey] as? String, let named = try? ShellSettings(site: site) {
            defaults.set(named.origin, forKey: siteKey)
        }
        if let app = given[appKey] as? String, (try? ShellSettings(site: productionSite, app: app)) != nil {
            defaults.set(app, forKey: appKey)
        }
    }

    /// The site's origin as text, the way a page's `location.origin` reads.
    public var origin: String {
        site.absoluteString.hasSuffix("/") ? String(site.absoluteString.dropLast()) : site.absoluteString
    }

    /// The link the shell opens: the app alone, in the person's own workspace.
    public var start: URL {
        // The link grammar's `entry "app/" "#" route`, with the route `/me/<app>`.
        URL(string: "\(origin)/app/#/me/\(app)") ?? site
    }

    /// Whether `host` is this computer, as the link grammar reads it: `localhost`, a name
    /// under it, a `127.x` address or `[::1]`.
    public static func isThisComputer(_ host: String) -> Bool {
        if host == "localhost" || host.hasSuffix(".localhost") || host == "::1" || host == "[::1]" { return true }
        let numbers = host.split(separator: ".", omittingEmptySubsequences: false)
        return numbers.count == 4 && numbers.first == "127" && numbers.allSatisfy { UInt8($0) != nil }
    }
}
