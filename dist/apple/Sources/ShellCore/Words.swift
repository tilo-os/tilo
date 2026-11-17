import Foundation

/// Every sentence the native part shows, by its key in `Localizable.strings`, with its
/// English. The page's own words are the page's; these are the menus, the screens the
/// shell shows when there is no page, and what the system shows for it.
public enum Words: String, CaseIterable, Sendable {
    case about, settings, hide, hideOthers = "hide-others", showAll = "show-all", quit
    case fileMenu = "file-menu", openLink = "open-link", closeWindow = "close-window"
    case editMenu = "edit-menu", undo, redo, cut, copy, paste, selectAll = "select-all"
    case viewMenu = "view-menu", reload
    case windowMenu = "window-menu", minimize
    case openAssistant = "open-assistant", dictate, openAtLogin = "open-at-login"
    case unreachableTitle = "unreachable-title", unreachableBody = "unreachable-body", tryAgain = "try-again"
    case addressRefused = "address-refused", addressNotAnAddress = "address-not-an-address"
    case addressNotSecure = "address-not-secure", noLink = "no-link"
    case listening, insertWords = "insert-words", cancel
    case dictationUnavailable = "dictation-unavailable", dictationNotAllowed = "dictation-not-allowed"
    case wordsWaiting = "words-waiting"
    case writeReply = "write-reply", replyButton = "reply-button", replyPlaceholder = "reply-placeholder"
    case siteLabel = "site-label", siteHelp = "site-help", useSite = "use-site", showDesktop = "show-desktop"
    case notificationsLabel = "notifications-label", openNotificationSettings = "open-notification-settings"
    case shareAdded = "share-added", shareNothing = "share-nothing", shareFailed = "share-failed", done
    case wayInMac = "way-in-mac", wayInPhone = "way-in-phone", pasteLink = "paste-link", scanCode = "scan-code"
    case codeNoLink = "code-no-link", cameraNotAllowed = "camera-not-allowed", pageStopped = "page-stopped"

    public var english: String {
        switch self {
        case .about: "About Tilo Assistant"
        case .settings: "Settings…"
        case .hide: "Hide Tilo Assistant"
        case .hideOthers: "Hide others"
        case .showAll: "Show all"
        case .quit: "Quit Tilo Assistant"
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
        case .openAssistant: "Open Tilo Assistant"
        case .dictate: "Dictate a message"
        case .openAtLogin: "Open at login"
        case .unreachableTitle: "Tilo cannot be reached"
        case .unreachableBody: "Check the connection, then try again."
        case .tryAgain: "Try again"
        case .addressRefused: "This address cannot be used"
        case .addressNotAnAddress: "Enter the address of a Tilo site, such as https://tilo.sh."
        case .addressNotSecure: "A Tilo site is reached over https."
        case .noLink: "The clipboard holds no link to this Tilo site."
        case .listening: "Listening"
        case .insertWords: "Insert"
        case .cancel: "Cancel"
        case .dictationUnavailable: "Dictation is not available in this language on this device."
        case .dictationNotAllowed: "Allow the microphone and speech recognition for Tilo Assistant in the system's settings, then try again."
        case .wordsWaiting: "Press where the words should go."
        case .writeReply: "Write a reply"
        case .replyButton: "Open in Tilo"
        case .replyPlaceholder: "Message"
        case .siteLabel: "Tilo site"
        case .siteHelp: "The address of the Tilo site this app opens."
        case .useSite: "Use this site"
        case .showDesktop: "Show the whole desktop"
        case .notificationsLabel: "Notifications"
        case .openNotificationSettings: "Open notification settings"
        case .shareAdded: "Added to your message"
        case .shareNothing: "Nothing here can go into a message."
        case .shareFailed: "Tilo Assistant could not take this."
        case .done: "Done"
        case .wayInMac: "Paste an invitation link, or the link that “Pair a new device” gives on a device where you are signed in."
        case .wayInPhone: "Scan the code or paste the link that “Pair a new device” gives on a device where you are signed in, or paste an invitation link."
        case .pasteLink: "Paste link"
        case .scanCode: "Scan code"
        case .codeNoLink: "That code is no link to this Tilo site."
        case .cameraNotAllowed: "Allow the camera for Tilo in the system's settings to scan a code."
        case .pageStopped: "The page stopped while it loaded."
        }
    }

    /// The sentence in the person's language, or its English where the bundle has none.
    public var said: String {
        NSLocalizedString(rawValue, bundle: .main, value: english, comment: "")
    }
}
