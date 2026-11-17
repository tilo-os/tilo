import Foundation

/// Where the app finds Tilo and what of it the app shows: the site it opens and, there, Tilo
/// as a whole or one app alone.
///
/// The site is never written into the source. A build names it in the bundle's
/// `Info.plist` (`TiloSite`), and a person changes it in Settings or with
/// `-TiloSite <origin>` on the command line. That a build shows one app and nothing else of
/// Tilo is the build's alone (`TiloAlone` with `TiloApp`): no setting and no argument leads
/// such a build to the desktop or to another app.
public struct ShellSettings: Equatable, Sendable {
    /// The key of the site, in the defaults and in `Info.plist`.
    public static let siteKey = "TiloSite"
    /// The key of the app, in the defaults and in `Info.plist`.
    public static let appKey = "TiloApp"
    /// The key of "this build shows one app and nothing else of Tilo", in `Info.plist` only.
    public static let aloneKey = "TiloAlone"
    /// The site a build that names none opens.
    public static let productionSite = "https://tilo.sh"

    /// Why an address is not a Tilo site.
    public enum Refusal: Error, Equatable, Sendable {
        /// Not an origin at all, or one with a path, a query or a sign-in.
        case notAnAddress
        /// Plain `http` to anything but this computer: a page there is no secure
        /// context, so it has neither its keys nor WebGPU.
        case notSecure
        /// Not an application id, or none where one app is all a build shows.
        case notAnApp
    }

    /// The site's origin, as `scheme://host[:port]` with no path.
    public let site: URL
    /// The id of the app that is shown alone, or that is in front on the desktop. Without
    /// one the site opens Tilo as it does in a browser.
    public let app: String?
    /// Whether the app is all that is shown of Tilo: no desktop, no other app.
    public let alone: Bool

    public init(site: String, app: String? = nil, alone: Bool = false) throws(Refusal) {
        let written = site.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let parts = URLComponents(string: written), let scheme = parts.scheme?.lowercased(),
              let host = parts.host?.lowercased(), !host.isEmpty,
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty || parts.path == "/"
        else { throw .notAnAddress }
        guard scheme == "https" || scheme == "http" else { throw .notAnAddress }
        guard scheme == "https" || ShellSettings.isThisComputer(host) else { throw .notSecure }
        if let app {
            guard app.wholeMatch(of: /[a-z0-9]+(\.[a-z0-9][a-z0-9-]*)+/) != nil else { throw .notAnApp }
        } else if alone {
            throw .notAnApp
        }
        var origin = URLComponents()
        origin.scheme = scheme
        origin.host = host
        let standard = scheme == "https" ? 443 : 80
        origin.port = parts.port == standard ? nil : parts.port
        guard let url = origin.url else { throw .notAnAddress }
        self.site = url
        self.app = app
        self.alone = alone
    }

    /// The settings a launch runs with. The site: what the person or the command line chose,
    /// else what the bundle names, else the production site. What is shown: in a build of one
    /// app, the bundle's app and nothing else; otherwise Tilo as a whole, with the app in
    /// front that the command line or the bundle names.
    public static func load(defaults: UserDefaults = .standard,
                            info: [String: Any]? = Bundle.main.infoDictionary) -> Result<ShellSettings, Refusal> {
        // An empty choice is no choice: the field in the phone's Settings starts empty.
        let chosen = { (key: String) -> String? in defaults.string(forKey: key).flatMap { $0.isEmpty ? nil : $0 } }
        let built = { (key: String) -> String? in (info?[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        let alone = info?[aloneKey] as? Bool ?? false
        do {
            return .success(try ShellSettings(site: chosen(siteKey) ?? built(siteKey) ?? productionSite,
                                              app: alone ? built(appKey) : chosen(appKey) ?? built(appKey),
                                              alone: alone))
        } catch {
            return .failure(error)
        }
    }

    /// Keep a site or an app named on the command line (`-TiloSite <origin>`), so that it is
    /// given once, by whoever installs a build, and not again by the person who opens it.
    /// What cannot be used is not kept, and a build of one app keeps no other.
    public static func keepArguments(in defaults: UserDefaults = .standard,
                                     info: [String: Any]? = Bundle.main.infoDictionary) {
        let given = defaults.volatileDomain(forName: UserDefaults.argumentDomain)
        if let site = given[siteKey] as? String, let named = try? ShellSettings(site: site) {
            defaults.set(named.origin, forKey: siteKey)
        }
        guard info?[aloneKey] as? Bool != true else { return }
        if let app = given[appKey] as? String, (try? ShellSettings(site: productionSite, app: app)) != nil {
            defaults.set(app, forKey: appKey)
        }
    }

    /// The site's origin as text, the way a page's `location.origin` reads.
    public var origin: String {
        site.absoluteString.hasSuffix("/") ? String(site.absoluteString.dropLast()) : site.absoluteString
    }

    /// The link the app opens: Tilo as the site opens it, the desktop with one app of the
    /// person's own workspace in front, or that app alone.
    public var start: URL {
        // The link grammar's `entry ["app/"] ["#" route]`, with the route `/me/<app>`.
        guard let app else { return URL(string: "\(origin)/") ?? site }
        return URL(string: "\(origin)/\(alone ? "app/" : "")#/me/\(app)") ?? site
    }

    /// Whether `host` is this computer, as the link grammar reads it: `localhost`, a name
    /// under it, a `127.x` address or `[::1]`.
    public static func isThisComputer(_ host: String) -> Bool {
        if host == "localhost" || host.hasSuffix(".localhost") || host == "::1" || host == "[::1]" { return true }
        let numbers = host.split(separator: ".", omittingEmptySubsequences: false)
        return numbers.count == 4 && numbers.first == "127" && numbers.allSatisfy { UInt8($0) != nil }
    }
}
