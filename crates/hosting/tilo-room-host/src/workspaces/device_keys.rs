//! The channel service of the device a workspace host runs on.
//!
//! A device that hosts a workspace takes part in the workspace's channels with the one
//! leaf it has in every other channel ([`channel_crypto::ChannelPeer`]). The
//! workspace host holds no keys of its own: when it seals or opens the records of
//! the workspaces it governs it asks that service through a handle its embedder
//! hands it at open ([`crate::host::HostSessions::open`]). The answer is
//! content keys only, which open and seal and admit nobody: the device's
//! group stays the device's business.
//!
//! A host that runs that service itself, as a relay whose device may own
//! workspaces does, is also handed the sink its members' channel governance is
//! delivered to ([`ChannelSink`]): the service settles the groups from what
//! the host's sessions hear, exactly as it settles them from its own.

use std::{future::Future, pin::Pin, sync::Arc};

use channel_crypto::ChannelContentKeys;
use collab_types::ChannelRoute;

/// What a device answers about the keys of a channel of one of its workspaces.
pub type ChannelKeysAnswer = Option<Result<ChannelContentKeys, String>>;

/// A device's answer, asked for.
pub type ChannelKeysAnswerFuture<'a> = Pin<Box<dyn Future<Output = ChannelKeysAnswer> + Send + 'a>>;

/// The channel keys of the device a workspace host runs on, asked of it.
pub trait ChannelKeys: Send + Sync + 'static {
    /// The content keys of `route`'s channel in `workspace`, opening the channel
    /// when it has none yet and `create`. `None` when the device takes no
    /// part in `workspace`'s channels; [`Err`] when it takes part and holds no
    /// keys of this channel yet.
    fn channel_keys<'a>(
        &'a self,
        workspace: &'a str,
        route: &'a ChannelRoute,
        create: bool,
    ) -> ChannelKeysAnswerFuture<'a>;
}

/// [`ChannelKeys`] as the host's shared handle.
pub type DeviceKeys = Arc<dyn ChannelKeys>;

/// Where a workspace host hands the channel governance its members send: the
/// device's channel service, when the host runs it.
///
/// A device whose channel service runs beside the host hears its members'
/// requests and commits on its own sessions, and is handed no sink. A host that
/// runs the service itself has no such sessions: its members' sessions end
/// here, so what they send for a channel arrives here too.
pub trait ChannelSink: Send + Sync + 'static {
    /// Take in what members sent for channels: access requests and commits,
    /// each signed by the device that sent it. The service verifies them as it
    /// verifies any it hears.
    fn take(&self, messages: Vec<neighbour_wire::NeighbourMsg>);
}

/// The channel service of the device a host runs on.
#[derive(Clone)]
pub struct DeviceChannels {
    /// The device's keys, asked when the host seals or opens a record.
    pub keys: DeviceKeys,
    /// Where the host hands the channel governance its members send, when it
    /// runs the device's channel service itself.
    pub sink: Option<Arc<dyn ChannelSink>>,
}
