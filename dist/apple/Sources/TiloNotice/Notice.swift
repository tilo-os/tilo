import Foundation
import ShellCore
import UserNotifications

/// The notice extension of Tilo Shell. The system hands it a push before the person sees it, so
/// what their host sealed for this device alone is opened here, on the device, and nowhere on
/// the way. Only a build that is told while it is closed carries it (`TILO_APPLE_PUSH`). Where
/// the sealed part opens with the key the app keeps, the notice says who told it, the host's
/// words, the Important mark and its buttons; where it does not, the push is handed on as it
/// came, and the line it carries in the open stands.
@objc(NoticeService)
final class NoticeService: UNNotificationServiceExtension {
    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        let content = request.content
        guard let push = ClosedPush(content.userInfo),
              let key = KeychainClosedKeys(info: Bundle.main.infoDictionary)?.read(),
              let opened = push.opened(with: key)
        else { return contentHandler(content) }
        // The app registers a notice's buttons as it shows the same thing; this covers the
        // first push, when it has shown nothing with these buttons yet.
        UNUserNotificationCenter.current().setNotificationCategories([NoticeService.category(opened)])
        contentHandler(ClosedPush.shown(content, opened: opened, named: push.name))
    }

    /// The buttons an opened notice offers, under the category it names.
    static func category(_ opened: ClosedPush.Opened) -> UNNotificationCategory {
        let actions = opened.actions.map {
            UNNotificationAction(identifier: ClosedPush.actionPrefix + $0.id, title: $0.label,
                                 options: [.foreground])
        }
        return UNNotificationCategory(identifier: opened.category, actions: actions,
                                      intentIdentifiers: [], options: [])
    }
}
