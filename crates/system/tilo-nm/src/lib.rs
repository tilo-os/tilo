//! `NetworkManager` over D-Bus: is the network up, is it metered, and which
//! link carries it.
//!
//! # One answer
//!
//! The shell's status bar, the filesystem's predictive hydration and
//! `os-actions` used to fork `nmcli` with their own parsers and disagreed:
//! one called a machine metered when *any* device was, another allowed
//! background traffic when *any* connected device was not. Here metering is
//! `NetworkManager`'s global `Metered` property, the primary connection's,
//! and every reader gets the same [`NetworkState`].
//!
//! # No polling
//!
//! [`watch`] reads the root object once and then follows its
//! `PropertiesChanged` signals. A reading never makes `NetworkManager` probe
//! the internet; [`NmClient::check_connectivity_now`] is the deliberate
//! "check again" for the installer and Settings.
//!
//! # Shape
//!
//! - [`model`]: `NetworkManager`'s numbers and strings as types, testable
//!   with no bus.
//! - [`client`]: the zbus layer. [`NmClient::new`] takes any connection, so a
//!   test can serve a fake `NetworkManager` over a socket pair.
//!
//! Sandboxed apps do not use this crate: they have no system bus.

pub mod client;
pub mod model;

pub use client::{
    BUS_NAME, NmClient, NmError, NmWatch, ROOT_INTERFACE, ROOT_PATH, captive_portal_uri,
    check_connectivity_now, state, watch,
};
pub use model::{LinkKind, NetworkState, NmConnectivity, NmState, PrimaryLink, metered_from_raw};
