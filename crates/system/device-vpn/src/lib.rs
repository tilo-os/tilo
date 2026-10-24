//! Your devices as a VPN: one of a person's devices routes another's
//! traffic, over the iroh connection their shells already share
//! (docs/device-vpn.md).
//!
//! - [`net`]: the tunnel's fixed addressing.
//! - [`protocol`]: what the shell asks the root helper, and
//!   [`protocol::call`].
//! - [`packet`]: the little of IPv4 the tunnel reads and writes.
//! - `tunnel` (feature `tunnel`): the `tilo-device-vpn/1` ALPN: handshake,
//!   authorisation at the exit, packets as QUIC datagrams.
//! - `service` (feature `service`): the root helper itself.

pub mod net;
pub mod packet;
pub mod protocol;

#[cfg(target_os = "linux")]
pub mod fdpass;

#[cfg(feature = "tunnel")]
pub mod tunnel;

#[cfg(all(feature = "service", target_os = "linux"))]
pub mod service;
