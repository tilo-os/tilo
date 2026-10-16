//! The device-admin route (docs/device-reach.md, "Reaching it by name").
//!
//! Beyond the LAN, a developer's machine reaches one of the person's dev
//! devices over the peer network: hole-punched QUIC with relay fallback,
//! under the versioned ALPN [`ALPN`]. The route carries tilo-vmd's own
//! newline-delimited request protocol (what `tilo-vm` otherwise runs as
//! `ssh <device> tilo-vmd rpc`), never a shell.
//!
//! * [`handshake`]: who may open the route. Both sides present their
//!   root-signed device certificate and sign a challenge bound to this QUIC
//!   connection's two endpoints. The device admits only a device of its own
//!   identity whose certificate grants administration
//!   ([`tilo_user_identity::DeviceCapabilities::can_administer_devices`]) and
//!   is not revoked; the dialer accepts only the device it asked for.
//! * [`identity`]: the keys and certificates each side uses. The device's
//!   route is served by its own system service, so it binds an endpoint with
//!   a transport key derived from the device key, as tilo-relay does: two
//!   processes publishing one endpoint id would take each other's relay
//!   slot. The device key itself proves who answers, inside the handshake.
//! * [`net`]: binding an endpoint, dialing with retries, naming the route.
//! * [`serve`]: the device side, which bridges each admitted session to a
//!   fresh `tilo-vmd rpc` and logs every session.
//! * [`dial`]: the dialer side, a stdio bridge `tilo-vm` runs in place of
//!   ssh, which reconnects by itself between requests.

pub mod dial;
pub mod handshake;
pub mod identity;
pub mod net;
pub mod serve;

/// The versioned ALPN of the device-admin route.
pub const ALPN: &[u8] = b"tilo-device-admin/1";

/// Configures the relay both sides use instead of the public n0 relays
/// and lookup, e.g. `http://10.0.2.2:3340` (a self-hosted relay, or a test
/// harness's).
pub const RELAY_ENV: &str = "TILO_DEVICE_ADMIN_RELAY";

/// What the device's running service last reported about its endpoint.
pub const SERVICE_STATE: &str = "/run/tilo-device-admin/endpoint.json";
