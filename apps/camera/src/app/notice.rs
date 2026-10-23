//! What just happened, as the window bar's notice
//! ([`ui::window_toolbar::notify`]): a photo saved, a scan under way, a
//! device that failed.

use ui::widget::toast::ToastKind;
use ui::window_toolbar::ToolbarNotice;

use super::CameraApp;

impl CameraApp {
    /// Say what just happened in the window's bar while it lasts: a
    /// progress note stays while its photo, video or scan is in the works.
    pub(super) fn publish_notice(&mut self) {
        self.toast.keep_while(
            self.worker.saving_photo() || self.video_finalizing || self.pending_scan.is_some(),
        );
        self.toast.expire();
        self.toast
            .publish(|message| ToolbarNotice::new(message.clone(), notice_kind(message)));
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
