//! The host side of the app identity service.
//!
//! Apps never hold the personal identity or the device key: they ask the
//! [`broker`] to sign for them, and it answers only for the identity the
//! [`registry`] selected for their workspace, only while this device holds a
//! valid, enrolled, non-revoked certificate ([`device_fleet`],
//! [`certificates`]). [`enrollment`] is the one place a token admits another
//! device; [`certification_ceremony`] gathers the identity key's signature
//! for it from the person's devices that hold the key. [`provisioning`] creates that chain, [`custody`]
//! selects the certificate this device presents to the hosts of each room,
//! and [`join_proof`] keeps the signed proof it presents there.
//!
//! Hosts supply their committed workspaces and channel service through
//! [`broker::WorkspaceMembership`] and [`broker::ChannelKeySource`].

#[cfg(unix)]
pub mod broker;
#[cfg(unix)]
pub mod certificates;
#[cfg(unix)]
pub mod certification_ceremony;
#[cfg(unix)]
pub mod custody;
#[cfg(unix)]
pub mod device_admin;
#[cfg(unix)]
pub mod device_fleet;
#[cfg(unix)]
pub mod enrolled_transport;
#[cfg(unix)]
pub mod enrollment;
#[cfg(unix)]
pub mod join_proof;
#[cfg(unix)]
pub mod person_files;
pub mod policy_change;
#[cfg(unix)]
pub mod provisioning;
#[cfg(unix)]
pub mod registry;

#[cfg(not(unix))]
mod unsupported_host;
#[cfg(not(unix))]
pub use unsupported_host::{device_fleet, provisioning, registry};
