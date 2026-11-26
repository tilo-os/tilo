//! Remote presences (agents, members) that hold their own view of a widget,
//! and the retained topology collaboration sessions walk.
//!
//! Both are capabilities: a multi-presence widget provides [`Presences`], an
//! owner whose live sessions differ from its presentation children provides
//! [`CollaborationTopology`]. The walks here reach every widget in the tree
//! through [`capability`](crate::widget::capability).

use automerge::AutoCommit;

use super::ViewPresence;
use super::Widget;
use super::capability::{capability, capability_mut};

/// A widget that keeps a view per remote presence (a caret, a
/// selection), published under that presence's identity.
pub trait Presences {
    /// Start a view for `presence`; `true` when it is new.
    fn join_presence(&mut self, presence: &tilo_actions::PresenceId) -> bool;

    /// Drop the presence's view (its run ended). Idempotent.
    fn leave_presence(&mut self, presence: &tilo_actions::PresenceId);

    /// Name shown beside the presence's caret.
    fn set_presence_label(&mut self, _presence: &tilo_actions::PresenceId, _label: &str) {}

    /// Current presence of every presence this widget holds a view for,
    /// derived from that presence's own view exactly as the local window
    /// derives its own.
    fn presences(&self, _doc: &AutoCommit) -> Vec<ViewPresence> {
        Vec::new()
    }

    /// The host serves only remote presences and has no local person (a
    /// headless host worker). The widget then publishes its
    /// presence-independent catalogue schema
    /// ([`tilo_actions::schema`]) instead of one
    /// caret's snapshot, so the publication does not change when a caret moves.
    fn set_remote_only(&mut self, _host: bool) {}
}

/// Retained session topology for owners with hidden or inactive live
/// sessions: the children collaboration reaches, which may differ from the
/// children drawn and given input.
pub trait CollaborationTopology {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)>;
}

pub fn collaboration_children(widget: &mut dyn Widget) -> Vec<&mut (dyn Widget + '_)> {
    // Probe first: borrowing `widget` mutably only on the returning path
    // leaves it free for the fallback.
    if capability::<dyn CollaborationTopology>(&*widget).is_some() {
        return capability_mut::<dyn CollaborationTopology>(widget)
            .map_or_else(Vec::new, CollaborationTopology::collaboration_children_mut);
    }
    widget.children_mut()
}

/// Start `presence`'s view in every widget under `widget` that keeps one.
pub fn join_presence(widget: &mut dyn Widget, presence: &tilo_actions::PresenceId) -> bool {
    if let Some(presences) = capability_mut::<dyn Presences>(&mut *widget) {
        return presences.join_presence(presence);
    }
    join_presence_in_children(widget, presence)
}

/// [`join_presence`] for `widget`'s collaboration children only.
pub fn join_presence_in_children(
    widget: &mut dyn Widget,
    presence: &tilo_actions::PresenceId,
) -> bool {
    let mut joined = false;
    for child in collaboration_children(widget) {
        joined |= join_presence(child, presence);
    }
    joined
}

/// Drop `presence`'s view everywhere under `widget`.
pub fn leave_presence(widget: &mut dyn Widget, presence: &tilo_actions::PresenceId) {
    if let Some(presences) = capability_mut::<dyn Presences>(&mut *widget) {
        presences.leave_presence(presence);
        return;
    }
    leave_presence_in_children(widget, presence);
}

/// [`leave_presence`] for `widget`'s collaboration children only.
pub fn leave_presence_in_children(widget: &mut dyn Widget, presence: &tilo_actions::PresenceId) {
    for child in collaboration_children(widget) {
        leave_presence(child, presence);
    }
}

/// Label `presence`'s caret everywhere under `widget`.
pub fn set_presence_label(
    widget: &mut dyn Widget,
    presence: &tilo_actions::PresenceId,
    label: &str,
) {
    if let Some(presences) = capability_mut::<dyn Presences>(&mut *widget) {
        presences.set_presence_label(presence, label);
        return;
    }
    set_presence_label_in_children(widget, presence, label);
}

/// [`set_presence_label`] for `widget`'s collaboration children only.
pub fn set_presence_label_in_children(
    widget: &mut dyn Widget,
    presence: &tilo_actions::PresenceId,
    label: &str,
) {
    for child in collaboration_children(widget) {
        set_presence_label(child, presence, label);
    }
}

/// Mark every widget under `widget` as serving only remote presences.
pub fn set_remote_only(widget: &mut dyn Widget, host: bool) {
    if let Some(presences) = capability_mut::<dyn Presences>(&mut *widget) {
        presences.set_remote_only(host);
        return;
    }
    set_remote_only_in_children(widget, host);
}

/// [`set_remote_only`] for `widget`'s collaboration children only.
pub fn set_remote_only_in_children(widget: &mut dyn Widget, host: bool) {
    for child in collaboration_children(widget) {
        set_remote_only(child, host);
    }
}

pub fn presences(widget: &dyn Widget, doc: &AutoCommit) -> Vec<ViewPresence> {
    if let Some(presences) = capability::<dyn Presences>(widget) {
        return presences.presences(doc);
    }
    let mut out = Vec::new();
    widget.for_each_child(&mut |child| out.extend(presences(child, doc)));
    out
}
