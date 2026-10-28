//! The tunnel between two of a person's devices: IPv4 packets as QUIC
//! datagrams on the [`ALPN`] connection between their iroh endpoints.
//!
//! - [`wire`]: the handshake and the codes a connection closes with.
//! - [`exit`]: who may route through this device, and its side of the
//!   packets.
//! - [`client`]: dialling an exit and carrying this device's packets.
//! - [`dns`]: the exit answering its clients' DNS queries.
//! - [`device`]: where packets meet the network stack.

pub mod client;
pub mod device;
pub mod dns;
pub mod exit;
pub mod wire;

#[cfg(test)]
mod tests;

/// The ALPN of a tunnel connection.
pub const ALPN: &[u8] = b"tilo-device-vpn/1";

pub use client::{ClientError, Ended, Refusal, Tunnel};
pub use device::PacketDevice;
pub use exit::{Authority, Decision, Exit, ExitDevices, SessionView};
pub use wire::Offer;
