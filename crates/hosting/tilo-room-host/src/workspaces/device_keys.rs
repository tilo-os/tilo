//! The key service of the device a workspace host runs on.
//!
//! A device that hosts a workspace takes part in the workspace's groups with the one
//! leaf it has in every other group ([`key_group::GroupPeer`]). The
//! workspace host holds no keys of its own: when it seals or opens the records of
//! the workspaces it governs it asks that service through a handle its embedder
//! hands it at open ([`crate::host::HostSessions::open`]). The answer is
//! content keys only, which open and seal and admit nobody: the device's
//! group stays the device's business.
//!
//! A host that runs that service itself, as a relay whose device may own
//! workspaces does, is also handed the sink its members' group governance is
//! delivered to ([`KeySink`]): the service settles the groups from what
//! the host's sessions hear, exactly as it settles them from its own.

use std::{future::Future, pin::Pin, sync::Arc};

use key_group::ContentKeys;

/// What a device answers about the keys of a group of one of its workspaces.
pub type GroupKeysAnswer = Option<Result<ContentKeys, String>>;

/// A device's answer, asked for.
pub type GroupKeysAnswerFuture<'a> = Pin<Box<dyn Future<Output = GroupKeysAnswer> + Send + 'a>>;

/// The group keys of the device a workspace host runs on, asked of it.
pub trait GroupKeys: Send + Sync + 'static {
    /// The content keys of `workspace`, opening its group when it has none
    /// yet and `create`. `None` when the device takes no part in `workspace`'s
    /// group; [`Err`] when it takes part and holds no keys yet.
    fn group_keys<'a>(&'a self, workspace: &'a str, create: bool) -> GroupKeysAnswerFuture<'a>;
}

/// [`GroupKeys`] as the host's shared handle.
pub type DeviceKeys = Arc<dyn GroupKeys>;

/// Where a workspace host hands the group governance its members send: the
/// device's key service, when the host runs it.
///
/// A device whose key service runs beside the host hears its members'
/// requests and commits on its own sessions, and is handed no sink. A host that
/// runs the service itself has no such sessions: its members' sessions end
/// here, so what they send for a group arrives here too.
pub trait KeySink: Send + Sync + 'static {
    /// Take in what members sent for groups: access requests and commits,
    /// each signed by the device that sent it. The service verifies them as it
    /// verifies any it hears.
    fn take(&self, messages: Vec<neighbour_wire::NeighbourMsg>);

    /// Note `workspace`'s governance as it now stands, before anyone is told
    /// it moved: the service decides under which group a record seals from
    /// what it reads here, so a workspace that settled must be readable there
    /// at once, not on a later pass.
    fn settled(&self, workspace: &str, state: tilo_peer_state::StatePeer);
}

/// The key service of the device a host runs on.
#[derive(Clone)]
pub struct DeviceKeyGroups {
    /// The device's keys, asked when the host seals or opens a record.
    pub keys: DeviceKeys,
    /// Where the host hands the group governance its members send, when it
    /// runs the device's key service itself.
    pub sink: Option<Arc<dyn KeySink>>,
}
