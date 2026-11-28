import Foundation
import ShellCore
import UserNotifications

/// What the person did with a notification.
public enum NoticeAnswer: Equatable, Sendable {
    /// Pressed it: the app comes forward.
    case opened
    /// Pressed one of the answers the page named, or typed one.
    case action(String, reply: String?)
    /// Typed a reply under a notice that named no answers of its own.
    case reply(String)
    case dismissed
}

private enum Names {
    static let plain = "tilo.plain"
    static let reply = "tilo.reply"
    static let page = "tilo.page."
    static let notice = "tilo-notice"
    static func request(_ id: String) -> String { "tilo-\(id)" }
}

/// The page's notices as the system's notifications, and the person's answers back.
///
/// A notice shows while the app runs: the page asks for it. Telling a person while no Tilo of
/// theirs runs is a push from their host, which this class does not do.
@MainActor
public final class NoticeCenter: NSObject {
    /// Called with the page's name for the notice and what the person did.
    public var answered: ((_ id: String, _ answer: NoticeAnswer) -> Void)?
    /// Called with what a push named, the button pressed (if any) and what was typed, when the
    /// person answered a notice their host pushed while no page ran.
    public var toldAnswered: ((_ name: String, _ action: String?, _ reply: String?) -> Void)?

    private let center = UNUserNotificationCenter.current()
    /// Where a home-screen widget asked to go, until the page shows the notice that names it.
    public var pending: String?
    /// What stands for the widget, newest first: the page's name for each notice, where it would
    /// go, and what it says, so withdrawing one takes the right one away.
    private var waiting: [(id: String, activation: String, item: WidgetSnapshot.Item)] = []
    private var categories: [String: UNNotificationCategory] = [:]
    private var byTag: [String: String] = [:]

    public override init() {
        super.init()
        center.delegate = self
        // A notice with no answers of its own still takes a reply: it lands in the message field.
        let reply = UNTextInputNotificationAction(
            identifier: Names.reply, title: Words.writeReply.said, options: [.foreground],
            textInputButtonTitle: Words.replyButton.said, textInputPlaceholder: Words.replyPlaceholder.said)
        register(UNNotificationCategory(identifier: Names.plain, actions: [reply], intentIdentifiers: [],
                                        options: [.customDismissAction]))
    }

    public func permission() async -> NoticePermission {
        let status = await center.notificationSettings().authorizationStatus
        if status == .notDetermined { return .notAsked }
        return status == .denied ? .denied : .granted
    }

    /// Ask the person, with the system's own question.
    public func ask() async -> NoticePermission {
        let granted = (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
        return granted ? .granted : .denied
    }

    public func show(_ notice: PageNotice) {
        let content = UNMutableNotificationContent()
        content.title = notice.title
        content.body = notice.body
        content.sound = .default
        content.userInfo = [Names.notice: notice.id]
        content.categoryIdentifier = category(for: notice.actions)
        // A notice named as a push names a thing is that thing's notice, under the same name.
        let named = notice.tag.flatMap { ClosedPush.isName($0) ? $0 : nil }
        let identifier = named ?? Names.request(notice.id)
        if let tag = notice.tag {
            content.threadIdentifier = named ?? tag
            // A newer notice under a tag takes the older one's place.
            if let before = byTag[tag] { center.removeDeliveredNotifications(withIdentifiers: [before]) }
            byTag[tag] = identifier
            ClosedPush.withdraw(named ?? tag)
        }
        center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil),
                   withCompletionHandler: nil)
        // The home screen's widget shows the same things: it keeps this one first.
        waiting.removeAll { $0.id == notice.id }
        waiting.insert((notice.id, notice.link,
                        WidgetSnapshot.Item(title: notice.title, body: notice.body,
                                            link: WidgetLink.url(for: notice.link))), at: 0)
        published()
        // A tap on that widget came before the page showed what it names: it is a click now.
        if let pending, !notice.link.isEmpty, pending == notice.link {
            self.pending = nil
            answered?(notice.id, .opened)
        }
    }

    public func withdraw(id: String) {
        let identifier = Names.request(id)
        byTag = byTag.filter { $0.value != identifier }
        center.removeDeliveredNotifications(withIdentifiers: [identifier])
        waiting.removeAll { $0.id == id }
        published()
    }

    /// The notice the page shows for the page link, if it shows one.
    public func notice(at activation: String) -> String? {
        waiting.first { $0.activation == activation }?.id
    }

    /// Keep what the widget shows in the app group, and ask it to draw again.
    private func published() {
        WidgetSnapshot(items: waiting.prefix(WidgetSnapshot.most).map { $0.item })
            .write(in: WidgetSnapshot.group())
    }

    /// The count on the app's icon.
    public func setBadge(_ count: Int) {
        center.setBadgeCount(max(0, count), withCompletionHandler: nil)
    }

    private func category(for actions: [NoticeAction]) -> String {
        guard !actions.isEmpty else { return Names.plain }
        // The name a push's own notice of the same thing uses too: one set of buttons, whether
        // the page or the host told of it.
        let identifier = ClosedPush.category(of: actions.map { ClosedPush.Opened.Action(id: $0.id, label: $0.title) })
        guard categories[identifier] == nil else { return identifier }
        // On a Mac the app runs behind its menu-bar item, so the page answers without coming
        // forward. A phone has to bring the app up for the page to run at all.
        #if os(macOS)
        let options: UNNotificationActionOptions = []
        #else
        let options: UNNotificationActionOptions = [.foreground]
        #endif
        let buttons = actions.map { action -> UNNotificationAction in
            switch action.kind {
            case .button:
                UNNotificationAction(identifier: Names.page + action.id, title: action.title, options: options)
            case .text:
                UNTextInputNotificationAction(
                    identifier: Names.page + action.id, title: action.title, options: options,
                    textInputButtonTitle: action.title, textInputPlaceholder: action.placeholder ?? "")
            }
        }
        register(UNNotificationCategory(identifier: identifier, actions: buttons, intentIdentifiers: [],
                                        options: [.customDismissAction]))
        return identifier
    }

    private func register(_ category: UNNotificationCategory) {
        categories[category.identifier] = category
        center.setNotificationCategories(Set(categories.values))
    }
}

// The system calls these on a thread of its choosing, and wants to be told on the main thread
// that an answer was taken. So they are tied to no actor, read what they need where they are
// called, and go to the main thread themselves.
extension NoticeCenter: UNUserNotificationCenterDelegate {
    public nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        // The page asks only while it is out of view, so what arrives is shown.
        completionHandler([.banner, .list, .sound])
    }

    public nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let taken = Taken(completionHandler)
        let fields = response.notification.request.content.userInfo
        let id = fields[Names.notice] as? String
        let push = ClosedPush(fields)
        let answer = NoticeCenter.answer(to: response.actionIdentifier,
                                         typed: (response as? UNTextInputNotificationResponse)?.userText)
        DispatchQueue.main.async {
            if let push, !push.isGone, let answer {
                // What a push named is answered where the log is: the page's own session.
                switch answer {
                case .opened: MainActor.assumeIsolated { self.toldAnswered?(push.name, nil, nil) }
                case .action(let action, let reply): MainActor.assumeIsolated { self.toldAnswered?(push.name, action, reply) }
                default: break
                }
            } else if let id, let answer {
                MainActor.assumeIsolated { self.answered?(id, answer) }
            }
            taken.tell()
        }
    }

    private nonisolated static func answer(to chosen: String, typed: String?) -> NoticeAnswer? {
        switch chosen {
        case UNNotificationDefaultActionIdentifier: .opened
        case UNNotificationDismissActionIdentifier: .dismissed
        case Names.reply: .reply(typed ?? "")
        case let chosen where chosen.hasPrefix(ClosedPush.actionPrefix):
            .action(String(chosen.dropFirst(ClosedPush.actionPrefix.count)), reply: typed)
        case let chosen where chosen.hasPrefix(Names.page): .action(String(chosen.dropFirst(Names.page.count)), reply: typed)
        default: nil
        }
    }
}

/// The system's own block, carried to the main thread to be called there once.
private final class Taken: @unchecked Sendable {
    let tell: () -> Void

    init(_ tell: @escaping () -> Void) {
        self.tell = tell
    }
}
