//! The window's notice: an operation's result or failure, shown in the
//! window's bar ([`tilo_ui::window_toolbar::notify`]) for a while, with at
//! most one button there.

use super::NavShell;
use tilo_ui::widget::toast::ToastKind;
use tilo_ui::window_toolbar::{ToolbarAction, ToolbarNotice};

pub const WINDOW_NOTICE_ACTION: &str = "nav-shell-notice";

#[derive(Clone, Debug)]
pub(super) struct ShellNotice {
    text: String,
    kind: ToastKind,
    action: Option<(String, String)>,
}

/// A notice a shell holds, to show again in the shell built in its place: a window that builds
/// its shell anew keeps the notice it had up.
#[derive(Clone, Debug)]
pub struct HeldNotice {
    text: String,
    kind: ToastKind,
    action: Option<(String, String)>,
}

impl NavShell {
    /// Show a transient notice in the window's bar: an operation's result or
    /// failure. It replaces the one showing and leaves on its own.
    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.show_notice(ShellNotice {
            text: text.into(),
            kind,
            action: None,
        });
    }

    /// Show a notice with one button beside it in the bar, named `action`
    /// ("Undo") with the icon `icon` ("undo-2"). It stays
    /// [`ACTION_NOTICE_LIFETIME`](tilo_ui::widget::toast::ACTION_NOTICE_LIFETIME), long enough
    /// to reach the button.
    /// [`Self::take_toast_action`] reports the press.
    pub fn toast_with_action(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        icon: &str,
        action: impl Into<String>,
    ) {
        self.show_notice(ShellNotice {
            text: text.into(),
            kind,
            action: Some((icon.to_owned(), action.into())),
        });
    }

    fn show_notice(&mut self, notice: ShellNotice) {
        let lifetime = if notice.action.is_some() {
            tilo_ui::widget::toast::ACTION_NOTICE_LIFETIME
        } else {
            tilo_ui::widget::toast::NOTICE_LIFETIME
        };
        self.notice.show_for(notice, lifetime);
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

    /// The notice this shell has up, and its button, while it is up: what a window keeps of a
    /// shell it builds anew.
    #[must_use]
    pub fn held_notice(&self) -> Option<HeldNotice> {
        let notice = self.notice.get()?;
        Some(HeldNotice {
            text: notice.text.clone(),
            kind: notice.kind,
            action: notice.action.clone(),
        })
    }

    /// Show `held` again in the shell built in its place, as it stood.
    pub fn restore_notice(&mut self, held: HeldNotice) {
        self.show_notice(ShellNotice {
            text: held.text,
            kind: held.kind,
            action: held.action,
        });
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
