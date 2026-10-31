//! The channel keys of the device a room host runs on.
//!
//! A device that hosts a room takes part in the room's channels with the one
//! leaf it has in every other channel ([`channel_crypto::ChannelPeer`]). The
//! room host holds no keys of its own: when it seals or opens the records of
//! the rooms it governs it asks that device through a handle its embedder
//! hands it at open ([`crate::host::HostSessions::open`]). The answer is
//! content keys only, which open and seal and admit nobody: the device's
//! group stays the device's business.
//!
//! An embedder that runs no channel service beside its host (a relay whose
//! certificate owns workspaces, and the host's own tests) hands none; the
//! host is then its device's one peer, holding the leaf in its own
//! participation ([`super::participation`]), and answers its own asks
//! through that.

use std::{future::Future, pin::Pin};

use channel_crypto::ChannelContentKeys;
use collab_types::ChannelRoute;

/// What a device answers about the keys of a channel of one of its rooms.
pub type ChannelKeysAnswer = Option<Result<ChannelContentKeys, String>>;

/// A device's answer, asked for.
pub type ChannelKeysAnswerFuture<'a> = Pin<Box<dyn Future<Output = ChannelKeysAnswer> + Send + 'a>>;

/// The channel keys of the device a room host runs on, asked of it.
pub trait ChannelKeys: Send + Sync + 'static {
    /// The content keys of `route`'s channel in `room`, opening the channel
    /// when it has none yet and `create`. `None` when the device takes no
    /// part in `room`'s channels; [`Err`] when it takes part and holds no
    /// keys of this channel yet.
    fn channel_keys<'a>(
        &'a self,
        room: &'a str,
        route: &'a ChannelRoute,
        create: bool,
    ) -> ChannelKeysAnswerFuture<'a>;
}

/// [`ChannelKeys`] as the host's shared handle.
pub type DeviceKeys = std::sync::Arc<dyn ChannelKeys>;
