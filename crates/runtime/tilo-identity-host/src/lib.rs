//! The host side of the app identity service.
//!
//! Apps never hold the personal identity or the device key: they ask the
//! [`broker`] to sign for them, and it answers only for the identity the
//! [`registry`] selected for their workspace, only while this device holds a
//! valid, enrolled, non-revoked certificate ([`device_fleet`],
//! [`certificates`]). [`provisioning`] creates that chain.
//!
//! Hosts supply their committed workspaces and channel service through
//! [`broker::WorkspaceMembership`] and [`broker::ChannelKeySource`].

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod broker;
pub mod certificates;
pub mod device_fleet;
pub mod provisioning;
pub mod registry;
