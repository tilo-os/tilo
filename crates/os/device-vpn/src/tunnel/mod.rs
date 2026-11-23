//! The tunnel between two of a person's devices: IPv4 packets as QUIC
//! datagrams on the [`ALPN`] connection between their iroh endpoints.

pub mod client;
pub mod device;
pub mod dns;
pub mod exit;
pub mod wire;

#[cfg(test)]
mod tests;

pub const ALPN: &[u8] = b"tilo-device-vpn/1";

pub use client::{ClientError, Ended, Refusal, Tunnel};
pub use device::PacketDevice;
pub use exit::{Authority, Decision, Exit, ExitDevices, SessionView};
pub use wire::Offer;
