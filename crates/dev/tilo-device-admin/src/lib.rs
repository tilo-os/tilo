//! The device-admin route (docs/device-reach.md, "Reaching it by name"): beyond the LAN, a
//! developer's machine reaches one of the person's dev devices over the peer network, hole-punched
//! QUIC with relay fallback, under the versioned ALPN [`ALPN`]. It carries tilo-vmd's
//! newline-delimited request protocol, never a shell.
//!
//! [`handshake`] decides who may open the route: both sides present their root-signed device
//! certificate and sign a challenge bound to the connection's two endpoints, and the device admits
//! only a device of its own identity whose certificate grants administration and is not revoked.
//! [`identity`] holds the keys each side uses: the route is served by its own service, which binds
//! an endpoint with a transport key derived from the device key, so two processes never publish one
//! endpoint id; the device key itself proves who answers, inside the handshake. [`serve`] is the
//! device side, [`dial`] the stdio bridge `tilo-vm` runs in place of ssh.

pub mod dial;
pub mod handshake;
pub mod identity;
pub mod net;
pub mod serve;

/// The versioned ALPN of the device-admin route.
pub const ALPN: &[u8] = b"tilo-device-admin/1";

/// The relay both sides use instead of the public n0 relays and lookup, e.g.
/// `http://10.0.2.2:3340`.
pub const RELAY_ENV: &str = "TILO_DEVICE_ADMIN_RELAY";

/// What the device's running service last reported about its endpoint.
pub const SERVICE_STATE: &str = "/run/tilo-device-admin/endpoint.json";
