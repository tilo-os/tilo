//! What just happened, as the window bar's notice
//! ([`ui::window_toolbar::notify`]): a photo saved, a scan under way, a
//! device that failed.

use ui::widget::toast::{ToastKind, Transient};
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::CameraApp;

/// What this window says in the bar, and how seriously — carried together.
///
/// The severity is a property of the event, not of its wording: a save that
/// failed is a failure in any language, and rewording a message must never
/// quietly turn it into a success pill. So every message is shown with the
/// [`ToastKind`] its own code chose, and nothing reads the text back.
#[derive(Debug, Default)]
pub(super) struct Notice {
    text: Transient<String>,
    kind: ToastKind,
}

impl Notice {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Something worked: the picture saved, the text copied.
    pub(super) fn show(&mut self, text: impl Into<String>) {
        self.show_kind(ToastKind::Success, text);
    }

    /// Something needs the person's attention but is not a failure.
    pub(super) fn warn(&mut self, text: impl Into<String>) {
        self.show_kind(ToastKind::Warning, text);
    }

    /// Something did not work: a save, a device, a scan. The person is owed
    /// the failure itself, not a cheerful acknowledgement.
    pub(super) fn fail(&mut self, text: impl Into<String>) {
        self.show_kind(ToastKind::Error, text);
    }

    /// A plain statement of what is happening ("Refreshing camera"), which
    /// is information rather than a result.
    pub(super) fn info(&mut self, text: impl Into<String>) {
        self.show_kind(ToastKind::Info, text);
    }

    fn show_kind(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.kind = kind;
        self.text.show(text.into());
    }

    /// Work under way, which stays while it runs.
    pub(super) fn show_progress(&mut self, text: impl Into<String>) {
        self.kind = ToastKind::Info;
        self.text.show_progress(text.into());
    }

    /// The severity of the message shown now.
    pub(super) fn kind(&self) -> ToastKind {
        self.kind
    }

    pub(super) fn dismiss(&mut self) {
        self.text.dismiss();
    }

    pub(super) fn expire(&mut self) {
        self.text.expire();
    }

    pub(super) fn keep_while(&mut self, busy: bool) {
        self.text.keep_while(busy);
    }

    #[cfg(test)]
    pub(super) fn is_shown(&self) -> bool {
        self.text.is_shown()
    }

    pub(super) fn deadline(&self) -> Option<ui::time::Instant> {
        self.text.deadline()
    }

    pub(super) fn publish(&self, notice: impl FnOnce(&String) -> ToolbarNotice) {
        self.text.publish(notice);
    }
}

impl std::ops::Deref for Notice {
    type Target = Transient<String>;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

/// The bar action id of a notice's button.
pub(super) const NOTICE_ACTION: &str = "camera.notice-action";

/// What a message that asks the person to act lets them do from the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoticeAction {
    /// Scan the page again, after a scan that failed.
    ScanAgain,
    /// Start recognizing the selected page's text again.
    RecognizeAgain,
    /// Look for cameras again.
    RefreshCameras,
}

impl NoticeAction {
    fn button(self) -> ToolbarAction {
        let (icon, name) = match self {
            Self::ScanAgain | Self::RecognizeAgain => ("rotate-cw", "Try again"),
            Self::RefreshCameras => ("refresh", "Refresh"),
        };
        ToolbarAction::new(NOTICE_ACTION, icon, name)
    }

    /// A message asking to try again follows a failure; one asking to look
    /// for cameras again follows finding none.
    fn kind(self) -> ToastKind {
        match self {
            Self::ScanAgain | Self::RecognizeAgain => ToastKind::Error,
            Self::RefreshCameras => ToastKind::Warning,
        }
    }
}

impl CameraApp {
    /// Show `text`, which asks the person to do something, with `action`
    /// beside it in the bar.
    pub(super) fn show_with_action(&mut self, text: String, action: NoticeAction) {
        self.toast.show(text.clone());
        self.toast_action = Some((text, action));
    }

    /// The action of the message up, if it carries one.
    fn notice_action(&self) -> Option<NoticeAction> {
        let shown = self.toast.as_deref()?;
        self.toast_action
            .as_ref()
            .filter(|(text, _)| text == shown)
            .map(|(_, action)| *action)
    }

    /// Run the notice's action when it was pressed in the bar; whether it
    /// was.
    pub(super) fn take_notice_action(&mut self) -> bool {
        if !ui::window_toolbar::take_notice_press(NOTICE_ACTION) {
            return false;
        }
        let Some(action) = self.notice_action() else {
            return false;
        };
        self.toast.dismiss();
        match action {
            NoticeAction::ScanAgain => self.capture_document_page(),
            NoticeAction::RecognizeAgain => self.recognize_document_page(),
            NoticeAction::RefreshCameras => self.retry_camera(),
        }
        true
    }

    /// Say what just happened in the window's bar while it lasts: a
    /// progress note stays while its photo, video or scan is in the works.
    pub(super) fn publish_notice(&mut self) {
        self.toast.keep_while(
            self.worker.saving_photo() || self.video_finalizing || self.pending_scan.is_some(),
        );
        self.toast.expire();
        let action = self.notice_action();
        self.toast.publish(|message| match action {
            Some(action) => {
                ToolbarNotice::new(message.clone(), action.kind()).action(action.button())
            }
            None => ToolbarNotice::new(message.clone(), self.toast.kind()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A result says so in the window's bar, a failure as one, and the bar
    /// lets it go once its owner does.
    #[test]
    fn a_result_is_the_window_bars_notice() {
        let mut app = CameraApp::sample();
        app.toast.show("Page 1 captured");
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| (notice.text, notice.kind)),
            Some(("Page 1 captured".to_owned(), ToastKind::Success))
        );
        app.toast.fail("Could not start scanner: busy");
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.kind),
            Some(ToastKind::Error)
        );
        drop(app);
        assert_eq!(ui::window_toolbar::notice(), None);
    }

    /// The severity is carried with the message, so the words cannot change
    /// it: a failure that thanks the person is still a failure, and a success
    /// that mentions a failure is still a success.
    #[test]
    fn rewording_a_message_cannot_change_how_serious_it_is() {
        let mut app = CameraApp::sample();
        app.toast.fail("Thanks! The photo could not be saved.");
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.kind),
            Some(ToastKind::Error),
            "a failure reworded as good news is still a failure"
        );
        app.toast
            .show("Could not find anything wrong with the picture.");
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.kind),
            Some(ToastKind::Success),
            "a success that mentions a failure is still a success"
        );
        app.toast.warn("Connect a camera to record.");
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.kind),
            Some(ToastKind::Warning)
        );
        drop(app);
    }
}
