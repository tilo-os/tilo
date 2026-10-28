//! `NetworkManager` over D-Bus, Tilo's one client of it: is the network up,
//! is it metered, which link carries it and which VPNs are up; and every
//! read and change network settings makes (radios, Wi-Fi, saved profiles,
//! connecting and disconnecting).
//!
//! # One answer
//!
//! The shell's status bar, the filesystem's predictive hydration and
//! `os-actions` once forked `nmcli` with their own parsers and disagreed:
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
//! - [`settings`]: a saved profile's settings as `NetworkManager` sends them,
//!   with typed readers and writers, testable with no bus.
//! - [`client`]: the zbus layer. [`NmClient::new`] takes any connection, so a
//!   test can serve a fake `NetworkManager` over a socket pair.
//! - [`control`]: devices, access points, profiles, activation.
//!
//! Sandboxed apps do not use this crate: they have no system bus.

pub mod client;
pub mod control;
pub mod model;
pub mod settings;

pub use client::{
    ACTIVE_INTERFACE, BUS_NAME, NmChanges, NmClient, NmError, NmWatch, ROOT_INTERFACE, ROOT_PATH,
    VpnWatch, WIRELESS_INTERFACE, captive_portal_uri, check_connectivity_now, scan_wifi, state,
    watch,
};
pub use control::Profile;
pub use model::{
    AccessPoint, ActiveConnection, ActiveState, ActiveVpn, Device, DeviceKind, IpDetails, LinkKind,
    NetworkState, NmConnectivity, NmState, PrimaryLink, Radios, VpnReading, activation_failure,
    ended_on_purpose, is_vpn_type, metered_from_raw,
};
pub use settings::{IpFamily, Setting, Settings, WireGuardPeer};
