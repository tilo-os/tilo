import UserNotifications

/// The notice extension of Tilo Assistant. The system hands it a push before the person sees
/// it, so that what their server sealed for this device alone is opened here, on the device,
/// and nowhere on the way.
///
/// Only a build that is told while it is closed carries it (`build.sh`, `TILO_APPLE_PUSH`).
/// It hands a notice on as it came, so the line a push carries in the open stands: that
/// something waits, and nothing of what.
@objc(NoticeService)
final class NoticeService: UNNotificationServiceExtension {
    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        contentHandler(request.content)
    }
}
