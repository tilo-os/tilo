//! The app identity service.
//!
//! Apps never hold the personal identity or the device key: they ask the
//! [`broker`] to sign for them, and it answers only for the identity the
//! [`store`] selected for their workspace, only while this device holds a
//! valid, enrolled, non-revoked certificate ([`device_fleet`],
//! [`certificates`]). A new device is added with a ticket the person's
//! device issues; [`certification_ceremony`] holds the code a new device shows.
//! [`provisioning`] creates that chain, [`store`]
//! selects the certificate this device presents to the device serving each
//! workspace, and [`join_proof`] keeps the signed proof it presents there.
//! [`store`] is the person's identity at rest: its secret providers,
//! backups and recovery words.
//!
//! The session supplies its committed workspaces and channel service through
//! [`broker::WorkspaceMembership`] and [`broker::GroupKeySource`].

#![cfg_attr(
    target_family = "wasm",
    allow(
        clippy::arc_with_non_send_sync,
        reason = "a page's identity store reaches JavaScript and stays on the page's one thread; `Arc` keeps a page's handles the same type as a desktop's"
    )
)]

#[cfg(not(target_family = "wasm"))]
pub mod broker;
pub mod certificates;
pub mod certification_ceremony;
#[cfg(not(target_family = "wasm"))]
pub mod device_admin;
pub mod device_fleet;
pub mod identity_store;
#[cfg(not(target_family = "wasm"))]
pub mod join_proof;
#[cfg(unix)]
pub mod local_person;
#[cfg(not(target_family = "wasm"))]
pub mod person_files;
pub mod policy_change;
#[cfg(not(target_family = "wasm"))]
pub mod provisioning;
pub mod sources;
#[cfg(not(target_family = "wasm"))]
pub mod store;
#[cfg(not(target_family = "wasm"))]
pub mod trusted_device;
