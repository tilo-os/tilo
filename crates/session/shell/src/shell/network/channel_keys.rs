//! This device's channel service, as this network's broker opens it.
//!
//! The room host this network runs keeps no channel keys of its own: it asks
//! the service installed here for the keys of each channel it takes part in
//! ([`super::ShellNetwork::channel_keys`]).

use super::ShellNetwork;
use crate::shell::channel_encryption::{ChannelKeyAccess, ChannelKeyProvider};

impl ShellNetwork {
    /// This device's channel service, as this network's broker opens it: the
    /// room host this network runs takes the channels' keys of the rooms it
    /// takes part in from here.
    pub(crate) fn channel_keys(&self) -> ChannelKeyProvider {
        self.channel_keys.clone()
    }

    /// Point this device's channel service at the broker's, or at none
    /// ([`Self::channel_keys`]).
    pub(crate) fn install_channel_keys(&self, access: Option<ChannelKeyAccess>) {
        self.channel_keys.set(access);
    }
}
