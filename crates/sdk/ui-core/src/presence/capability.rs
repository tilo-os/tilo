//! Widgets that publish where their user is (caret, selection) to peers.

use crate::widget::Widget;
use crate::widget::capability::capability;

/// A widget with a presence identity and, while focused, outbound presence.
pub trait PublishesPresence: Widget {
    /// Stable identity for presence and permission lookup.
    fn presence_key(&self) -> Option<super::PresenceKey> {
        None
    }

    /// Outbound presence (typically caret/selection) broadcast while this
    /// widget holds focus; the host polls the focused widget after each
    /// event and diffs it out to peers.
    fn presence_payload(
        &self,
    ) -> Option<(super::PresenceKey, crate::collaboration::PresencePayload)> {
        None
    }
}

/// `widget`'s presence identity.
pub fn presence_key(widget: &(impl Widget + ?Sized)) -> Option<super::PresenceKey> {
    capability::<dyn PublishesPresence>(widget)?.presence_key()
}

/// `widget`'s outbound presence.
pub fn presence_payload(
    widget: &(impl Widget + ?Sized),
) -> Option<(super::PresenceKey, crate::collaboration::PresencePayload)> {
    capability::<dyn PublishesPresence>(widget)?.presence_payload()
}
