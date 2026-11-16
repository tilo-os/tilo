//! What a failed save or import of pages offers in the window bar: Try
//! again, which asks where to save, or which pages to import, again.

use ui::widget::toast::ToastKind;
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::PdfApp;

pub(super) const TRY_AGAIN_ACTION: &str = "pdf.notice.try-again";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoteAction {
    Save,
    Import,
}

impl PdfApp {
    pub(super) fn show_failure(&mut self, text: String, action: NoteAction) {
        self.status_note.show(text.clone());
        self.note_action = Some((text, action));
    }

    fn note_action(&self) -> Option<NoteAction> {
        let shown = self.status_note.as_deref()?;
        self.note_action
            .as_ref()
            .filter(|(text, _)| text == shown)
            .map(|(_, action)| *action)
    }

    pub(super) fn take_note_action(&mut self) {
        let Some(action) = self.note_action() else {
            return;
        };
        if !ui::window_toolbar::take_notice_press(TRY_AGAIN_ACTION) {
            return;
        }
        self.status_note.dismiss();
        self.note_action = None;
        match action {
            NoteAction::Save => self.request_save_via_portal(),
            NoteAction::Import => self.request_import_via_portal(),
        }
        self.dirty = true;
    }

    pub(super) fn publish_status_note(&self) {
        let action = self.note_action();
        self.status_note.publish(|note| {
            let notice = ToolbarNotice::new(
                note.clone(),
                if action.is_some() {
                    ToastKind::Error
                } else {
                    ToastKind::Info
                },
            );
            match action {
                Some(_) => notice.action(ToolbarAction::new(
                    TRY_AGAIN_ACTION,
                    "rotate-cw",
                    crate::t("pdf-try-again"),
                )),
                None => notice,
            }
        });
    }
}
