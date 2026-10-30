//! Your devices as a VPN: one of a person's devices routes another's
//! traffic, over the iroh connection their shells already share
//! (docs/device-vpn.md). The same root helper holds the kill switch of the
//! person's `NetworkManager` VPN profiles.
//!
//! - [`net`]: the tunnel's fixed addressing.
//! - [`protocol`]: what the shell asks the root helper, and
//!   [`protocol::call`].
//! - [`packet`]: the little of IPv4 the tunnel reads and writes.
//! - [`profile`]: when the kill switch of the person's `NetworkManager` VPN
//!   profiles blocks traffic.
//! - `tunnel` (feature `tunnel`): the `tilo-device-vpn/1` ALPN: handshake,
//!   authorisation at the exit, packets as QUIC datagrams.
//! - `controller`, `profile_block` (feature `session`): what the session
//!   does about its route through another device, and the follower of the
//!   profiles' kill switch.
//! - `service` (feature `service`): the root helper itself.

pub mod net;
pub mod packet;
pub mod profile;
pub mod protocol;

#[cfg(target_os = "linux")]
pub mod fdpass;

#[cfg(feature = "tunnel")]
pub mod tunnel;

#[cfg(feature = "session")]
pub mod controller;
#[cfg(feature = "session")]
pub mod profile_block;

#[cfg(all(feature = "service", target_os = "linux"))]
pub mod service;
