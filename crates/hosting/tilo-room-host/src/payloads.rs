//! What the host may know about an opaque co-edit body.
//!
//! A [`CoeditPayload`](crate::coedit::CoeditPayload) carries its channel and
//! retention kind in the clear and the collaboration message as an opaque
//! body. The host decisions that need to know a little about that body ask a
//! [`ShellPayloads`] adapter; the transport core never names the desktop's
//! message vocabulary itself.
//!
//! [`NeighbourPayloads`] answers them for the desktop's peer-sync messages
//! (`NeighbourMsg`); it is the only place compaction, surface admission,
//! retained-checkpoint replacement and the live checkpoint gate decode one.
//!
//! The adapter is injected, not looked up: a host takes one when it opens
//! ([`HostConfig::shell_payloads`](crate::host::HostConfig::shell_payloads),
//! [`NeighbourPayloads`] by default) and hands it to the compactor and to every
//! workspace store it opens, and the peer session asks the store of the
//! workspace it serves. Workspace governance still uses the desktop's
//! vocabulary directly.
//! An adapter that classifies nothing makes the host keep every entry and
//! fold none: unknown data never grants removal.

use std::collections::BTreeSet;
use std::sync::Arc;

mod neighbour;

pub use neighbour::NeighbourPayloads;

/// The document a retained entry belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// Application id.
    pub app_id: String,
    /// Workspace (cluster) id.
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

/// What a checkpoint is, as host policy tells them apart.
///
/// Every kind but [`Self::Other`] is latest-wins state: each value replaces
/// the last on its route and no delta carries what changed between them, so
/// a live device must receive every one. [`Self::Other`] is a compaction
/// baseline of content whose deltas a live device already holds; it reaches
/// a live device once per session, or again when the device asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointKind {
    /// A step of channel governance (a governance commit, an access request
    /// or its acknowledgement): each replaces the last on one route.
    Governance,
    /// A device's snapshot of its person's workspace list, authored for
    /// `user`: each replaces that device's last.
    Directory {
        /// The user public key the list belongs to.
        user: [u8; 32],
    },
    /// A device's certificate, introducing it to the workspace: each replaces
    /// that device's last.
    Identity,
    /// A compaction baseline: a shell tree or document checkpoint.
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

    /// An upper bound on the memory one decoded body holds beyond its bytes,
    /// for the store's bounded-work accounting.
    fn decoded_overhead(&self) -> usize;
}

/// The desktop's adapter, as a host
/// ([`HostConfig::new`](crate::host::HostConfig::new)) and
/// [`WorkspaceStore::open`](crate::store::WorkspaceStore::open) wire it.
#[must_use]
pub fn neighbour() -> Arc<dyn ShellPayloads> {
    Arc::new(NeighbourPayloads)
}
