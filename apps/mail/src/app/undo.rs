//! The window's Undo and Redo.

use super::MailApp;

/// The window's Undo: what has a history of its own inside it (a message
/// being written) first, then the person's last change to messages, which
/// lives in the parts beside the mailbox; the mailbox's own changes after
/// that.
impl ui::undo::LocalUndo for MailApp {
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        for child in ui::widget::Widget::children_mut(self) {
            if ui::undo::handle_local_undo_request(child, request) {
                return true;
            }
        }
        let Some(chain) = self.shared.borrow().chain.clone() else {
            return false;
        };
        match request {
            ui::undo::UndoRequest::Undo => chain.undo_last(),
            ui::undo::UndoRequest::Redo => chain.redo_last(),
        }
    }
}

impl ui::undo::UndoControl for MailApp {
    fn take_undo_request(&mut self) -> Option<ui::undo::UndoRequest> {
        self.nav.take_toast_action()?;
        if self.send_notice && !self.sent_here.is_empty() {
            self.unsend = true;
            self.dirty = true;
            return None;
        }
        Some(ui::undo::UndoRequest::Undo)
    }
}
