import Foundation

/// Every sentence the native part shows, by its key in `Localizable.strings`, with its
/// English. The page's own words are the page's; these are the menus, the screens the
/// shell shows when there is no page, and what the system shows for it.
///
/// Two apps are built from these sources, so no sentence names one: where the app's own name
/// belongs a sentence has `{app}`, and the name is the bundle's. Where a sentence names the
/// site the app opens it has `{site}`.
public enum Words: String, CaseIterable, Sendable {
    case about, settings, hide, hideOthers = "hide-others", showAll = "show-all", quit
    case fileMenu = "file-menu", openLink = "open-link", closeWindow = "close-window"
    case editMenu = "edit-menu", undo, redo, cut, copy, paste, selectAll = "select-all"
    case viewMenu = "view-menu", reload
    case windowMenu = "window-menu", minimize
    case openApp = "open-app", dictate, openAtLogin = "open-at-login"
    case unreachableTitle = "unreachable-title", unreachableBody = "unreachable-body", tryAgain = "try-again"
    case addressRefused = "address-refused", addressNotAnAddress = "address-not-an-address"
    case addressNotSecure = "address-not-secure", noLink = "no-link"
    case listening, insertWords = "insert-words", cancel
    case dictationUnavailable = "dictation-unavailable", dictationNotAllowed = "dictation-not-allowed"
    case wordsWaiting = "words-waiting"
    case writeReply = "write-reply", replyButton = "reply-button", replyPlaceholder = "reply-placeholder"
    case siteLabel = "site-label", siteHelp = "site-help", useSite = "use-site"
    case notificationsLabel = "notifications-label", openNotificationSettings = "open-notification-settings"
    case shareAdded = "share-added", shareNothing = "share-nothing", shareFailed = "share-failed", done
    case wayInMac = "way-in-mac", wayInPhone = "way-in-phone", pasteLink = "paste-link", scanCode = "scan-code"
    case codeNoLink = "code-no-link", cameraNotAllowed = "camera-not-allowed", pageStopped = "page-stopped"
    case pageAway = "page-away", important

    /// The sentence in English, with `{app}` where the app's own name goes and `{site}` where
    /// the site does.
    public var english: String {
        switch self {
        case .about: "About {app}"
        case .settings: "Settings…"
        case .hide: "Hide {app}"
        case .hideOthers: "Hide others"
        case .showAll: "Show all"
        case .quit: "Quit {app}"
        case .fileMenu: "File"
        case .openLink: "Open link from clipboard"
        case .closeWindow: "Close window"
        case .editMenu: "Edit"
        case .undo: "Undo"
        case .redo: "Redo"
        case .cut: "Cut"
        case .copy: "Copy"
        case .paste: "Paste"
        case .selectAll: "Select all"
        case .viewMenu: "View"
        case .reload: "Reload"
        case .windowMenu: "Window"
        case .minimize: "Minimize"
        case .openApp: "Open {app}"
        case .dictate: "Dictate a message"
        case .openAtLogin: "Open at login"
        case .unreachableTitle: "Tilo cannot be reached"
        case .unreachableBody: "Check the connection, then try again."
        case .tryAgain: "Try again"
        case .addressRefused: "This address cannot be used"
        case .addressNotAnAddress: "Enter the address of a Tilo site, such as https://tilo.sh."
        case .addressNotSecure: "A Tilo site is reached over https."
        case .noLink: "The clipboard holds no link into {site}, the Tilo site this app opens."
        case .listening: "Listening"
        case .insertWords: "Insert"
        case .cancel: "Cancel"
        case .dictationUnavailable: "Dictation is not available in this language on this device."
        case .dictationNotAllowed: "Allow the microphone and speech recognition for {app} in the system's settings, then try again."
        case .wordsWaiting: "Press where the words should go."
        case .writeReply: "Write a reply"
        case .replyButton: "Open in {app}"
        case .replyPlaceholder: "Message"
        case .siteLabel: "Tilo site"
        case .siteHelp: "The address of the Tilo site this app opens."
        case .useSite: "Use this site"
        case .notificationsLabel: "Notifications"
        case .openNotificationSettings: "Open notification settings"
        case .shareAdded: "Added to your message"
        case .shareNothing: "Nothing here can go into a message."
        case .shareFailed: "{app} could not take this."
        case .done: "Done"
        case .wayInMac: "The link comes from “Pair a new device” on a device where you are signed in."
        case .wayInPhone: "Both come from “Pair a new device” on a device where you are signed in."
        case .pasteLink: "Paste link"
        case .scanCode: "Scan code"
        case .codeNoLink: "That code is no link into {site}, the Tilo site this app opens."
        case .cameraNotAllowed: "Allow the camera for {app} in the system's settings to scan a code."
        case .pageStopped: "The page stopped while it loaded."
        case .pageAway: "This Tilo site keeps leaving {app} for another page."
        case .important: "Important"
        }
    }

    /// The app's own name, as its bundle says it: "Tilo Shell". The share extension's bundle
    /// carries the name of its app.
    public static func appName(info: [String: Any]? = Bundle.main.infoDictionary) -> String {
        let named = ["CFBundleDisplayName", "CFBundleName"].compactMap { info?[$0] as? String }
        return named.first { !$0.isEmpty } ?? "Tilo Shell"
    }

    /// The sentence in the person's language, or its English where the bundle has none, with
    /// the app's own name in it.
    public var said: String {
        said(as: Words.appName())
    }

    /// A sentence that names the site, about `site`: the origin the app opens.
    public func said(site: String) -> String {
        said(as: Words.appName(), site: site)
    }

    /// The sentence as an app named `name` says it, about the site `site`.
    public func said(as name: String, site: String = "", in bundle: Bundle = .main) -> String {
        NSLocalizedString(rawValue, bundle: bundle, value: english, comment: "")
            .replacingOccurrences(of: "{app}", with: name)
            .replacingOccurrences(of: "{site}", with: site)
    }
}
