//! The route a channel carries: named by the channel id, as the SDK joins it.

use super::{CrdtRoute, JoinedChannel};

impl JoinedChannel {
    /// The route the connection works as: the record the channel id names, parsed back out here.
    pub(super) fn route(&self) -> CrdtRoute {
        self.resource_route()
    }

    /// The record the channel carries, as its channel id names it.
    pub(super) fn resource_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }
}
