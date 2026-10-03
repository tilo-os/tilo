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

/// Nodes [`subtree_presence`] visits at most.
const SUBTREE_NODES: usize = 4096;
/// Payloads [`subtree_presence`] returns at most.
const SUBTREE_PAYLOADS: usize = 8;

/// The outbound presence of the widgets under `root` (itself included), in
/// tree order: where each editor's caret is even while it does not hold
/// keyboard focus (an action moved it, or focus is elsewhere). Bounded in
/// nodes and results.
pub fn subtree_presence(
    root: &dyn Widget,
) -> Vec<(super::PresenceKey, crate::collaboration::PresencePayload)> {
    fn walk(
        widget: &dyn Widget,
        visited: &mut usize,
        found: &mut Vec<(super::PresenceKey, crate::collaboration::PresencePayload)>,
    ) {
        if *visited >= SUBTREE_NODES || found.len() >= SUBTREE_PAYLOADS {
            return;
        }
        *visited += 1;
        if let Some(payload) = presence_payload(widget) {
            found.push(payload);
        }
        widget.for_each_child(&mut |child| walk(child, visited, found));
    }
    let mut found = Vec::new();
    walk(root, &mut 0, &mut found);
    found
}
