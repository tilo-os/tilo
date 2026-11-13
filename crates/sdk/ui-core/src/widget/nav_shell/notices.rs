//! The window's notice: an operation's result or failure, shown in the
//! window's bar ([`crate::window_toolbar::notify`]) for a while, with at
//! most one button there.

use super::NavShell;
use crate::widget::toast::ToastKind;
use crate::window_toolbar::{ToolbarAction, ToolbarNotice};

pub const WINDOW_NOTICE_ACTION: &str = "nav-shell-notice";

#[derive(Clone, Debug)]
pub(super) struct ShellNotice {
    text: String,
    kind: ToastKind,
    action: Option<(String, String)>,
}

impl NavShell {
    /// Show a transient notice in the window's bar: an operation's result or
    /// failure. It replaces the one showing and leaves on its own.
    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.notice.show(ShellNotice {
            text: text.into(),
            kind,
            action: None,
        });
        self.notice_pressed = None;
        self.publish_notice();
    }

    /// Show a notice with one button beside it in the bar, named `action`
    /// ("Undo") with the icon `icon` ("undo-2").
    /// [`Self::take_toast_action`] reports the press.
    pub fn toast_with_action(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        icon: &str,
        action: impl Into<String>,
    ) {
        self.notice.show(ShellNotice {
            text: text.into(),
            kind,
            action: Some((icon.to_owned(), action.into())),
        });
        self.notice_pressed = None;
        self.publish_notice();
    }

    pub fn take_toast_action(&mut self) -> Option<String> {
        self.notice_pressed.take()
    }

    pub fn dismiss_toast(&mut self) {
        if self.notice.dismiss() {
            self.publish_notice();
        }
    }

    pub fn toast_text(&self) -> Option<&str> {
        self.notice.get().map(|notice| notice.text.as_str())
    }

    pub(super) fn publish_notice(&self) {
        self.notice.publish(|notice| {
            let shown = ToolbarNotice::new(notice.text.clone(), notice.kind);
            match &notice.action {
                Some((icon, name)) => shown.action(ToolbarAction::new(
                    WINDOW_NOTICE_ACTION,
                    icon.clone(),
                    name.clone(),
                )),
                None => shown,
            }
        });
    }

    /// The notice's button was pressed in the bar: report it once, and take
    /// the notice down. Whether there was one to press.
    pub(super) fn press_notice_action(&mut self) -> bool {
        let Some(text) = self
            .notice
            .get()
            .filter(|notice| notice.action.is_some())
            .map(|notice| notice.text.clone())
        else {
            return false;
        };
        self.notice_pressed = Some(text);
        self.notice.dismiss();
        self.publish_notice();
        true
    }
}
