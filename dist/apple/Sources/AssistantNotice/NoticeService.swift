import AssistantClosed
import Foundation
import UserNotifications

/// The notice extension of Tilo Assistant. The system hands it a push before the person sees
/// it, so that what their server sealed for this device alone is opened here, on the device,
/// and nowhere on the way.
///
/// Only a build that is told while it is closed carries it (`build.sh`, `TILO_APPLE_PUSH`).
/// Where the sealed part opens with the key the app keeps for this, the notice says who told
/// it, the server's words, and the Important mark. Where a push carries no sealed part, where
/// it does not open, or where no key is kept, the notice is handed on as it came: the line a
/// push carries in the open stands, that something waits, and nothing of what.
@objc(NoticeService)
final class NoticeService: UNNotificationServiceExtension {
    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        contentHandler(NoticeService.shown(request.content, keys: KeychainClosedKeys(info: Bundle.main.infoDictionary)))
    }

    /// What the person is shown for a push.
    static func shown(_ content: UNNotificationContent, keys: (any ClosedKeyStore)?) -> UNNotificationContent {
        guard let push = ClosedPush(content.userInfo), let key = keys?.read(), let opened = push.opened(with: key),
              let changed = content.mutableCopy() as? UNMutableNotificationContent
        else { return content }
        changed.title = opened.title
        changed.subtitle = opened.subtitle ?? ""
        changed.body = opened.words
        changed.categoryIdentifier = opened.category
        // Opened, the sealed part is kept nowhere: the notice carries the thing's name and its place only.
        changed.userInfo["enc"] = nil
        return changed
    }
}
