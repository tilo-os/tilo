//! What just happened to the media and to the project file, as the window
//! bar's notice ([`ui::window_toolbar::notify`]): an import's outcome, a
//! chooser waiting for an answer, a project opened. A save that failed
//! stays there, with Try again, until a save succeeds.

use ui::time::Instant;
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::{Notice, VideoEditorApp};

/// The bar action id of a failed save's "Try again".
pub(super) const SAVE_RETRY_ACTION: &str = "video-editor.save.retry";
/// The bar action id of a refused import's "Choose media…".
pub(super) const CHOOSE_MEDIA_ACTION: &str = "video-editor.import.choose";
/// The bar action id of an Open's "Undo".
pub(super) const UNDO_OPEN_ACTION: &str = "video-editor.open.undo";

/// What a notice that asks the person to act lets them do from the bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NoticeAction {
    /// Save the project again, after a save that failed.
    SaveAgain,
    /// Choose media to import, after files were refused.
    ChooseMedia,
    /// Take back the Open that replaced the timeline.
    Undo,
}

impl NoticeAction {
    fn button(self) -> ToolbarAction {
        match self {
            Self::SaveAgain => ToolbarAction::new(SAVE_RETRY_ACTION, "rotate-cw", "Try again"),
            Self::ChooseMedia => {
                ToolbarAction::new(CHOOSE_MEDIA_ACTION, "file-plus", "Choose media…")
            }
            Self::Undo => ToolbarAction::new(UNDO_OPEN_ACTION, "undo-2", "Undo"),
        }
    }
}

impl Notice {
    fn bar_notice(&self) -> ToolbarNotice {
        let notice = ToolbarNotice::new(self.text.clone(), self.level.toast_kind());
        match self.action {
            Some(action) => notice.action(action.button()),
            None => notice,
        }
    }
}

impl VideoEditorApp {
    /// A save failed for `reason`: say so in the bar, and keep saying it
    /// until a save succeeds.
    pub(super) fn hold_save_failure(&mut self, reason: String) {
        self.project_notice
            .show_progress(Notice::failed(reason.clone()).with_action(NoticeAction::SaveAgain));
        self.save_failure = Some(reason);
    }

    /// A project chooser came back or went out without news: the bar goes
    /// back to the failed save it held, or to saying nothing.
    pub(super) fn show_save_failure_or_nothing(&mut self) {
        match self.save_failure.clone() {
            Some(reason) => self
                .project_notice
                .show_progress(Notice::failed(reason).with_action(NoticeAction::SaveAgain)),
            None => {
                self.project_notice.dismiss();
            }
        }
    }

    /// Run the action pressed beside a notice in the bar: a failed save's
    /// "Try again" (the save runs on the next sync), a refused import's
    /// "Choose media…", an Open's "Undo". Whether one was pressed.
    pub(super) fn take_notice_actions(&mut self) -> bool {
        let mut pressed = false;
        if ui::window_toolbar::take_notice_press(SAVE_RETRY_ACTION) {
            self.project_sync.save_requested = true;
            pressed = true;
        }
        if ui::window_toolbar::take_notice_press(CHOOSE_MEDIA_ACTION) {
            self.trigger_import();
            pressed = true;
        }
        if ui::window_toolbar::take_notice_press(UNDO_OPEN_ACTION) {
            self.project_notice.dismiss();
            self.undo.request(ui::UndoRequest::Undo);
            pressed = true;
        }
        pressed
    }

    /// Say what just happened in the window's bar while it lasts: a
    /// chooser's prompt stays while the chooser is open, a failed save until
    /// a save succeeds, anything else for a notice's lifetime.
    pub(super) fn publish_notices(&mut self) {
        let choosing_media = self.import_rx.is_some() || self.import_contents_rx.is_some();
        self.import_notice.keep_while(choosing_media);
        self.import_notice.expire();
        self.import_notice.publish(Notice::bar_notice);
        let held = self.project_rx.is_some() || self.save_failure.is_some();
        self.project_notice.keep_while(held);
        self.project_notice.expire();
        // A passing notice that stood in for a failed save gives the bar back
        // to it.
        if self.save_failure.is_some()
            && self.project_rx.is_none()
            && !self.project_notice.is_shown()
        {
            self.show_save_failure_or_nothing();
        }
        // A save under way is not offered again.
        let saving = self.project_rx.is_some();
        self.project_notice.publish(|notice| {
            if saving && notice.action == Some(NoticeAction::SaveAgain) {
                ToolbarNotice::new(notice.text.clone(), notice.level.toast_kind())
            } else {
                notice.bar_notice()
            }
        });
    }

    /// When a notice in the bar next leaves by itself. A project notice
    /// that stays while something holds it does not leave by the clock: a
    /// chooser's answer syncs the editor, and a failed save waits for the
    /// person.
    pub(super) fn notice_deadline(&self) -> Option<Instant> {
        let project = self
            .project_notice
            .deadline()
            .filter(|_| !self.project_notice.is_progress());
        match (self.import_notice.deadline(), project) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}
