//! The key service of the device a workspace service runs on.
//!
//! A device that hosts a workspace takes part in the workspace's groups with the one
//! leaf it has in every other group ([`key_group::GroupPeer`]). The
//! workspace service holds no keys of its own: when it seals or opens the records of
//! the workspaces it hosts it asks that service through a handle its embedder
//! hands it at open ([`crate::service::ServiceSessions::open`]). The answer is
//! content keys only, which open and seal and admit nobody: the device's
//! group stays the device's business.
//!
//! A host that runs that service itself, as a server whose device may own
//! workspaces does, is also handed the sink its members' group governance is
//! delivered to ([`KeySink`]): the service settles the groups from what
//! the host's sessions hear, exactly as it settles them from its own.

use std::{future::Future, pin::Pin, sync::Arc};

use key_group::ContentKeys;

/// What a device answers about the keys of a group of one of its workspaces.
pub type GroupKeysAnswer = Option<Result<ContentKeys, String>>;

/// A device's answer, asked for.
#[cfg(not(target_family = "wasm"))]
pub type GroupKeysAnswerFuture<'a> = Pin<Box<dyn Future<Output = GroupKeysAnswer> + Send + 'a>>;
/// A device's answer, asked for: a page's key service answers through JavaScript, on its one
/// thread.
#[cfg(target_family = "wasm")]
pub type GroupKeysAnswerFuture<'a> = Pin<Box<dyn Future<Output = GroupKeysAnswer> + 'a>>;

/// Where a device's keys of a workspace stand, waited on: the value to wait from next, or `None`
/// when the device never says its keys changed.
#[cfg(not(target_family = "wasm"))]
pub type KeysChangedFuture<'a> = Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>>;
/// Where a device's keys of a workspace stand, waited on: a page's key service answers on its one
/// thread.
#[cfg(target_family = "wasm")]
pub type KeysChangedFuture<'a> = Pin<Box<dyn Future<Output = Option<u64>> + 'a>>;

/// The group keys of the device a workspace service runs on, asked of it.
pub trait GroupKeys: collab_types::signer::MaybeSendSync + 'static {
    /// The reader list bound to a document's registered key group, if held here.
    fn named_readers(
        &self,
        _workspace: collab_types::WorkspaceId,
        _document: &str,
    ) -> Option<String> {
        None
    }

    /// The content keys of the group of `readers` in `workspace`
    /// ([`collab_types::reader_group`]: [`collab_types::EVERY_MEMBER`] is the
    /// workspace's own), opening the group when it has none yet and `create`.
    /// `None` when the device takes no part in that group; [`Err`] when it
    /// takes part and holds no keys yet.
    fn group_keys<'a>(
        &'a self,
        workspace: &'a str,
        readers: &'a str,
        create: bool,
    ) -> GroupKeysAnswerFuture<'a>;

    /// Wait until the keys or access of `workspace`'s groups change after `after` (0 at first),
    /// and answer the value to wait from next: at once when they changed since. `None` when this
    /// device never says, so a wait for keys it does not hold yet would never end.
    fn keys_changed<'a>(&'a self, _workspace: &'a str, _after: u64) -> KeysChangedFuture<'a> {
        Box::pin(std::future::ready(None))
    }

    /// This device holds `group` at `held`, and a record it must open was sealed under a later
    /// epoch: ask the person's other devices for the group's frames from `held`, as the
    /// workspace's host would replay them. A device with none of the person's other devices to
    /// ask does nothing. `group` is a workspace, a reader set's group or a document's.
    fn behind(&self, _group: collab_types::WorkspaceId, _held: u64) {}
}

/// [`GroupKeys`] as the host's shared handle.
pub type DeviceKeys = Arc<dyn GroupKeys>;

/// Where a workspace service hands the group governance its members send: the
/// device's key service, when the host runs it.
#[cfg(not(target_arch = "wasm32"))]
///
/// A device whose key service runs beside the host hears its members'
/// requests and commits on its own sessions, and is handed no sink. A host that
/// runs the service itself has no such sessions: its members' sessions end
/// here, so what they send for a group arrives here too.
pub trait KeySink: collab_types::signer::MaybeSendSync + 'static {
    /// Take in what members sent for groups: access requests and commits,
    /// each signed by the device that sent it. The service verifies them as it
    /// verifies any it hears.
    fn take(&self, messages: Vec<neighbour_wire::NeighbourMsg>);

    /// Note `workspace`'s governance as it now stands, before anyone is told
    /// it moved: the service decides under which group a record seals from
    /// what it reads here, so a workspace that settled must be readable there
    /// at once, not on a later pass.
    fn settled(&self, workspace: &str, state: tilo_peer_state::StatePeer);

    /// Read the governance of `workspaces`, the service the sink was handed to, from the
    /// moment it is built: before it opens a workspace, so the first record it seals there,
    /// while it starts, finds who reads it.
    fn attach(&self, _workspaces: &crate::workspaces::Workspaces) {}
}

/// The key service of the device a host runs on.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct DeviceKeyGroups {
    /// The device's keys, asked when the host seals or opens a record.
    pub keys: DeviceKeys,
    /// Where the host hands the group governance its members send, when it
    /// runs the device's key service itself.
    pub sink: Option<Arc<dyn KeySink>>,
}

/// Asks the person's other devices for the frames of a group this device holds behind them: the
/// group, and the epoch the device holds it at.
#[cfg(not(target_arch = "wasm32"))]
pub type AskingBehind = Arc<dyn Fn(collab_types::WorkspaceId, u64) + Send + Sync>;
