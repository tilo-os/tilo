//! The host side of the app identity service.
//!
//! Apps never hold the personal identity or the device key: they ask the
//! [`broker`] to sign for them, and it answers only for the identity the
//! [`registry`] selected for their workspace, only while this device holds a
//! valid, enrolled, non-revoked certificate ([`device_fleet`],
//! [`certificates`]). [`provisioning`] creates that chain, and [`custody`]
//! selects the certificate this device presents to the hosts of each room.
//!
//! Hosts supply their committed workspaces and channel service through
//! [`broker::WorkspaceMembership`] and [`broker::ChannelKeySource`].

#[cfg(unix)]
pub mod broker;
#[cfg(unix)]
pub mod certificates;
#[cfg(unix)]
pub mod custody;
#[cfg(unix)]
pub mod device_fleet;
#[cfg(unix)]
pub mod provisioning;
#[cfg(unix)]
pub mod registry;

#[cfg(not(unix))]
mod unsupported_host;
#[cfg(not(unix))]
pub use unsupported_host::{device_fleet, provisioning, registry};
