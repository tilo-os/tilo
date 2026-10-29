//! What just happened to the media and to project files, as the window
//! bar's notice ([`ui::window_toolbar::notify`]): an import's outcome, a
//! chooser waiting for an answer, a project file opened or exported.

use ui::time::Instant;
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::{Notice, VideoEditorApp};

/// The bar action id of a refused import's "Choose media…".
pub(super) const CHOOSE_MEDIA_ACTION: &str = "video-editor.import.choose";
/// The bar action id of an Open's "Undo".
pub(super) const UNDO_OPEN_ACTION: &str = "video-editor.open.undo";

/// What a notice that asks the person to act lets them do from the bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NoticeAction {
    /// Choose media to import, after files were refused.
    ChooseMedia,
    /// Take back the Open that replaced the timeline.
    Undo,
}

impl NoticeAction {
    fn button(self) -> ToolbarAction {
        match self {
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
    /// Run the action pressed beside a notice in the bar: a refused
    /// import's "Choose media…", an Open's "Undo". Whether one was pressed.
    pub(super) fn take_notice_actions(&mut self) -> bool {
        let mut pressed = false;
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

    /// Say what just happened in the window's bar while it lasts: a media
    /// chooser's prompt stays while the chooser is open, anything else for a
    /// notice's lifetime.
    pub(super) fn publish_notices(&mut self) {
        let choosing_media = self.import_rx.is_some() || self.import_contents_rx.is_some();
        self.import_notice.keep_while(choosing_media);
        self.import_notice.expire();
        self.import_notice.publish(Notice::bar_notice);
        self.project_notice.expire();
        self.project_notice.publish(Notice::bar_notice);
    }

    /// When a notice in the bar next leaves by itself. A notice that stays
    /// while something holds it does not leave by the clock.
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
