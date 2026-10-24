//! What a workspace host may know about an opaque co-edit body.
//!
//! A co-edit payload carries its channel and retention kind in the clear and
//! the collaboration message as an opaque body. Some host decisions still
//! need to know a little about that body: whether it is a document frame
//! compaction may fold, whether a checkpoint replaces its author's previous
//! one, whether it is a member's own continuity checkpoint or a step of
//! channel governance, and which devices it is addressed to. The relay core
//! (`tilo-relay`) asks the [`ShellPayloads`] its embedder injects those
//! questions and never names the desktop's message vocabulary itself.
//!
//! [`NeighbourPayloads`] answers them for the desktop's peer-sync messages
//! (`NeighbourMsg`); it is the only place compaction, surface admission,
//! retained-checkpoint replacement, continuity evidence and the live
//! checkpoint gate decode one. With the `web` feature, `legacy` is the
//! browser legacy-window bridge, which decodes and originates those
//! messages on the browser live plane.
//!
//! [`selection`] is native shell replay selection: with a desktop
//! [`RetentionAuthority`](retained_proof::RetentionAuthority)
//! bound to a workspace store, retention and replay select an immutable basis
//! and retire only what exact historical proof pairs cover. The store reaches
//! it only through the `tilo_relay_store::shell` seam, binding through the
//! adapter [`ShellPayloads::replay`] returns ([`NeighbourReplay`]).
//! [`retained_proof`] is the historical proof selection judges by: it pairs
//! a retained room with its snapshots and tails against the pinned governor,
//! decoding every peer-supplied Automerge body in `tilo_automerge_isolate`.
//!
//! An adapter that classifies nothing makes the host keep every entry and
//! fold none: unknown data never grants removal.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::BTreeSet;

#[cfg(feature = "web")]
pub mod legacy;
mod neighbour;
pub mod retained_proof;
pub mod selection;

pub use neighbour::NeighbourPayloads;
pub use selection::binding::{NeighbourReplay, ShellAuthority};

// Selection verifies peer-supplied Automerge bytes in worker processes
// started from this test binary; this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

/// The document a retained entry belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// Application id.
    pub app_id: String,
    /// Room (cluster) id.
    pub cluster_id: String,
    /// Record wire name.
    pub record_id: String,
}

/// What a checkpoint body is, once the adapter has confirmed it is a whole
/// message retained as a checkpoint on the channel it was framed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// What the checkpoint carries.
    pub kind: CheckpointKind,
    /// Whether the checkpoint is the whole of its route: its author's next
    /// one on the same channel leaves the previous one nothing to add.
    pub standalone: bool,
}

/// The checkpoints host policy treats differently from the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointKind {
    /// A step of channel governance (a governance commit, an access request
    /// or its acknowledgement): each replaces the last on one route, so every
    /// one must reach a live device.
    Governance,
    /// A person's own continuity checkpoint, authored for `user`.
    Continuity {
        /// The user public key the checkpoint belongs to.
        user: [u8; 32],
    },
    Other,
}

/// The host's questions about an opaque co-edit body.
///
/// Every answer must be a pure function of the bytes: retained entries are
/// classified again after a restart, and replay, compaction and
/// garbage-collection verdicts rely on getting the same answer.
pub trait ShellPayloads: Send + Sync + 'static {
    /// Whether `body` is an application-document frame, the only kind
    /// compaction counts. Runs for every routed delta, so it must not
    /// allocate or copy in proportion to the body.
    fn is_document(&self, body: &[u8]) -> bool;

    /// The route a document frame names and its signed envelope, or `None`
    /// when `body` is not a document frame.
    fn open_document(&self, body: &[u8]) -> Option<(Route, Box<collab_types::SignedOp>)>;

    /// The body publishing `op` for `route` as the device `author`, framed
    /// exactly as a device publishes one.
    ///
    /// # Errors
    ///
    /// Returns a description when the body cannot be encoded.
    fn document_body(
        &self,
        author: &[u8; 32],
        route: &Route,
        op: collab_types::SignedOp,
    ) -> Result<Vec<u8>, String>;

    /// The channel and retained body that introduce `cert` to the devices a
    /// replay reaches.
    ///
    /// # Errors
    ///
    /// Returns a description when the body cannot be encoded.
    fn host_identity(
        &self,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<([u8; 32], Vec<u8>), String>;

    /// What checkpoint `body`, framed for `channel`, is: `None` unless it is
    /// one whole message (no trailing bytes) that retains as a checkpoint on
    /// exactly that channel.
    fn checkpoint(&self, channel: &[u8; 32], body: &[u8]) -> Option<Checkpoint>;

    /// The devices `body` goes to as it arrives when only they can use it;
    /// `None` for everyone.
    fn audience(&self, body: &[u8]) -> Option<BTreeSet<[u8; 32]>>;

    /// Whether only the steward of a channel reads `body`: what a member
    /// sends the device governing its room, which every other member
    /// ignores. A host governing the room itself stewards it, so it
    /// delivers such a body to no member and replays it to none. `false`
    /// unless the adapter knows.
    fn steward_only(&self, body: &[u8]) -> bool {
        let _ = body;
        false
    }

    /// An upper bound on the memory one decoded body holds beyond its bytes,
    /// for the store's bounded-work accounting.
    fn decoded_overhead(&self) -> usize;

    /// The shell replay adapter that binds a workspace's shell retention
    /// authority, or `None` when this adapter's host selects no shell replay:
    /// a store bound through it would then refuse the authority.
    fn replay(&self) -> Option<&dyn tilo_relay_store::shell::ShellReplay> {
        None
    }
}
