//! Borrowed views that group a peer's concerns outside admission.
//!
//! [`StatePeer`] itself carries admission and the reads of what it admitted.
//! Every other concern is reached through one view per concern, a shared one
//! for its checks and reads and a mutable one for what it authors, so a change
//! to admission is reviewable on the one type that admits. The methods of each
//! view live beside the concern they serve; a view adds no state, and nothing
//! reaches the peer's core through it that the peer did not already expose.
use super::StatePeer;

/// The media-call governance of a peer's room, read side: the security
/// metadata proofs and the admitted Chat bindings a call is checked against.
#[derive(Clone, Copy)]
pub struct PeerMediaGovernance<'peer> {
    pub(crate) peer: &'peer StatePeer,
}

/// The media-call governance of a peer's room, write side: authoring the room
/// media policy and reconciling it with verified revocations.
pub struct PeerMediaGovernanceMut<'peer> {
    pub(crate) peer: &'peer mut StatePeer,
}

/// The workspace a peer's room governs, read side: who may read, name and
/// write its checkpoints, run its tasks and administer it, and the roots its
/// effect history and checkpoints currently stand at.
#[derive(Clone, Copy)]
pub struct PeerWorkspace<'peer> {
    pub(crate) peer: &'peer StatePeer,
}

/// The workspace a peer's room governs, write side: effect receipts, checkpoint
/// and effect-history roots, task commands and membership commands.
pub struct PeerWorkspaceMut<'peer> {
    pub(crate) peer: &'peer mut StatePeer,
}

/// A peer's own outbox, read side: whether what it published is retired, and
/// the recovery reservations and projections it would republish.
#[derive(Clone, Copy)]
pub struct PeerOutbox<'peer> {
    pub(crate) peer: &'peer StatePeer,
}

/// A peer's own outbox, write side: reconciling, renewing and recovering what
/// it published.
pub struct PeerOutboxMut<'peer> {
    pub(crate) peer: &'peer mut StatePeer,
}

/// The room's membership and its governance, read side: who is a member,
/// agent or bot and with what access, and the checks that verify a member's
/// or owner's refresh against the admitted roster.
#[derive(Clone, Copy)]
pub struct PeerGovernance<'peer> {
    pub(crate) peer: &'peer StatePeer,
}

/// The room's membership and its governance, write side: inviting, removing,
/// banning and re-roling members, admitting agents, installing delegations,
/// editing grants, and naming the room.
pub struct PeerGovernanceMut<'peer> {
    pub(crate) peer: &'peer mut StatePeer,
}

impl StatePeer {
    /// Read this peer's room membership and governance.
    #[must_use]
    pub fn governance(&self) -> PeerGovernance<'_> {
        PeerGovernance { peer: self }
    }

    /// Author this peer's room membership and governance.
    #[must_use]
    pub fn governance_mut(&mut self) -> PeerGovernanceMut<'_> {
        PeerGovernanceMut { peer: self }
    }

    /// Read this peer's room media governance.
    #[must_use]
    pub fn media(&self) -> PeerMediaGovernance<'_> {
        PeerMediaGovernance { peer: self }
    }

    /// Author or reconcile this peer's room media governance.
    #[must_use]
    pub fn media_mut(&mut self) -> PeerMediaGovernanceMut<'_> {
        PeerMediaGovernanceMut { peer: self }
    }

    /// Check against, or read, the workspace this peer's room governs.
    #[must_use]
    pub fn workspace(&self) -> PeerWorkspace<'_> {
        PeerWorkspace { peer: self }
    }

    /// Apply a workspace effect, task or membership command to this peer's room.
    #[must_use]
    pub fn workspace_mut(&mut self) -> PeerWorkspaceMut<'_> {
        PeerWorkspaceMut { peer: self }
    }

    /// Read this peer's outbox and recovery state.
    #[must_use]
    pub fn outbox(&self) -> PeerOutbox<'_> {
        PeerOutbox { peer: self }
    }

    /// Reconcile, renew or recover what this peer published.
    #[must_use]
    pub fn outbox_mut(&mut self) -> PeerOutboxMut<'_> {
        PeerOutboxMut { peer: self }
    }
}
