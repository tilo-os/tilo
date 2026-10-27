//! Remote participants (agents, collaborators) that hold their own view of a
//! widget, and the retained topology collaboration sessions walk.
//!
//! Both are capabilities: a multi-participant widget provides
//! [`Participants`], an owner whose live sessions differ from its
//! presentation children provides [`CollaborationTopology`]. The walks here
//! reach every widget in the tree through [`capability`](crate::widget::capability).

use automerge::AutoCommit;

use crate::widget::capability::{capability, capability_mut};
use crate::widget::{ParticipantPresence, Widget};

/// A widget that keeps a view per remote participant (a caret, a
/// selection), published under that participant's identity.
pub trait Participants {
    /// Start a view for `participant`; `true` when it is new.
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool;

    /// Drop the participant's view (its task ended). Idempotent.
    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId);

    /// Name shown beside the participant's caret.
    fn set_participant_label(&mut self, _participant: &tilo_actions::ParticipantId, _label: &str) {}

    /// Current presence of every participant this widget holds a view for,
    /// derived from that participant's own view exactly as the local window
    /// derives its own.
    fn participant_presence(&self, _doc: &AutoCommit) -> Vec<ParticipantPresence> {
        Vec::new()
    }

    /// The host serves only remote participants and has no local person (a
    /// headless host worker). The widget then publishes its
    /// participant-independent catalogue schema
    /// ([`tilo_actions::schema`]) instead of one
    /// caret's snapshot, so the publication does not change when a caret moves.
    fn set_participant_host(&mut self, _host: bool) {}
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

/// Start `participant`'s view in every widget under `widget` that keeps one.
pub fn join_participant(
    widget: &mut dyn Widget,
    participant: &tilo_actions::ParticipantId,
) -> bool {
    if let Some(participants) = capability_mut::<dyn Participants>(&mut *widget) {
        return participants.join_participant(participant);
    }
    join_participant_in_children(widget, participant)
}

/// [`join_participant`] for `widget`'s collaboration children only.
pub fn join_participant_in_children(
    widget: &mut dyn Widget,
    participant: &tilo_actions::ParticipantId,
) -> bool {
    let mut joined = false;
    for child in collaboration_children(widget) {
        joined |= join_participant(child, participant);
    }
    joined
}

/// Drop `participant`'s view everywhere under `widget`.
pub fn leave_participant(widget: &mut dyn Widget, participant: &tilo_actions::ParticipantId) {
    if let Some(participants) = capability_mut::<dyn Participants>(&mut *widget) {
        participants.leave_participant(participant);
        return;
    }
    leave_participant_in_children(widget, participant);
}

/// [`leave_participant`] for `widget`'s collaboration children only.
pub fn leave_participant_in_children(
    widget: &mut dyn Widget,
    participant: &tilo_actions::ParticipantId,
) {
    for child in collaboration_children(widget) {
        leave_participant(child, participant);
    }
}

/// Label `participant`'s caret everywhere under `widget`.
pub fn set_participant_label(
    widget: &mut dyn Widget,
    participant: &tilo_actions::ParticipantId,
    label: &str,
) {
    if let Some(participants) = capability_mut::<dyn Participants>(&mut *widget) {
        participants.set_participant_label(participant, label);
        return;
    }
    set_participant_label_in_children(widget, participant, label);
}

/// [`set_participant_label`] for `widget`'s collaboration children only.
pub fn set_participant_label_in_children(
    widget: &mut dyn Widget,
    participant: &tilo_actions::ParticipantId,
    label: &str,
) {
    for child in collaboration_children(widget) {
        set_participant_label(child, participant, label);
    }
}

/// Mark every widget under `widget` as serving only remote participants.
pub fn set_participant_host(widget: &mut dyn Widget, host: bool) {
    if let Some(participants) = capability_mut::<dyn Participants>(&mut *widget) {
        participants.set_participant_host(host);
        return;
    }
    set_participant_host_in_children(widget, host);
}

/// [`set_participant_host`] for `widget`'s collaboration children only.
pub fn set_participant_host_in_children(widget: &mut dyn Widget, host: bool) {
    for child in collaboration_children(widget) {
        set_participant_host(child, host);
    }
}

pub fn participant_presence(widget: &dyn Widget, doc: &AutoCommit) -> Vec<ParticipantPresence> {
    if let Some(participants) = capability::<dyn Participants>(widget) {
        return participants.participant_presence(doc);
    }
    let mut out = Vec::new();
    widget.for_each_child(&mut |child| out.extend(participant_presence(child, doc)));
    out
}
