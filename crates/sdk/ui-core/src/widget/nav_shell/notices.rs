//! The window's notices: shown over its bottom trailing corner, taken
//! down, and their one button published to assistive technology and
//! agents as a button of the window.

use super::NavShell;
use crate::widget::Widget as _;
use crate::widget::toast::ToastKind;

/// The semantic target of a notice's button: this prefix, then the
/// notice's text, which [`NavShell::take_toast_action`] reports.
pub(super) const TOAST_ACTION: &str = "toast-action:";

impl NavShell {
    /// Show a transient notice over the window's bottom trailing corner: an
    /// operation's result or failure. It replaces the one showing, expires on
    /// its own (held while the pointer rests on it and under the capture
    /// clock), and a click dismisses it.
    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push(text, kind);
        self.toasts_dirty = true;
    }

    /// Show a notice with one button, `action` ("Undo"), beside its text.
    /// [`Self::take_toast_action`] reports the press.
    pub fn toast_with_action(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        action: impl Into<String>,
    ) {
        self.toasts.push_with_action(text, kind, action);
        self.toasts_dirty = true;
    }

    /// The text of the notice whose button the person pressed since the
    /// last call.
    pub fn take_toast_action(&mut self) -> Option<String> {
        self.toasts.take_activated()
    }

    /// Take down the notice showing, when its news no longer holds.
    pub fn dismiss_toast(&mut self) {
        if !self.toasts.is_empty() {
            self.toasts.dismiss(0);
            self.toasts_dirty = true;
        }
    }

    /// The notice showing, if any.
    pub fn toast_text(&self) -> Option<&str> {
        self.toasts.iter().next().map(|toast| toast.text.as_str())
    }

    /// Whether the notices painted anything other than what the host last
    /// presented: one came, went, expired, moved or changed its look.
    pub(super) fn toasts_changed(&self) -> bool {
        self.toasts_painted != self.toasts_drawn
            || !self.toasts_scene.encoding_eq(&self.toasts_drawn_scene)
    }

    pub(super) fn toasts_expired(&self) -> bool {
        self.toasts
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now())
    }

    /// The showing notices' buttons ("Undo") as buttons of the window: the
    /// notices are painted over the window rather than laid out in it, so
    /// the window publishes and operates their buttons.
    pub(super) fn toast_buttons(&self) -> impl Iterator<Item = crate::a11y::AccessNode> + '_ {
        self.toasts.action_buttons().map(|(text, action, painted)| {
            let target = format!("{TOAST_ACTION}{text}");
            let button = crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                .with_name(action.to_owned())
                .with_description(text.to_owned())
                .with_hashed_key(&target)
                .with_action_target(target);
            match painted {
                Some(rect) => button.with_bounds(rect),
                None => button,
            }
        })
    }
}
