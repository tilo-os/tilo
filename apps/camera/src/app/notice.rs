//! What just happened, as the window bar's notice
//! ([`ui::window_toolbar::notify`]): a photo saved, a scan under way, a
//! device that failed.

use ui::widget::toast::ToastKind;
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::CameraApp;

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
            None => ToolbarNotice::new(message.clone(), notice_kind(message)),
        });
    }
}

/// Device and file failures never read as successes.
fn notice_kind(message: &str) -> ToastKind {
    if message.starts_with("Save failed")
        || message.starts_with("Couldn't")
        || message.starts_with("Could not")
        || message.starts_with("Text recognition")
        || message.starts_with("Text could not")
    {
        ToastKind::Error
    } else if message.starts_with("Connect a camera")
        || message.starts_with("Nothing recorded")
        || message.starts_with("No code found")
    {
        ToastKind::Warning
    } else {
        ToastKind::Success
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
        app.toast.show("Page 1 captured".into());
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| (notice.text, notice.kind)),
            Some(("Page 1 captured".to_owned(), ToastKind::Success))
        );
        app.toast.show("Could not start scanner: busy".into());
        app.publish_notice();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.kind),
            Some(ToastKind::Error)
        );
        drop(app);
        assert_eq!(ui::window_toolbar::notice(), None);
    }
}
