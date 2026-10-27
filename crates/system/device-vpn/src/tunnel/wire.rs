//! The handshake on a tunnel connection's control stream, and the codes a
//! connection closes with.
//!
//! The client opens one bidirectional stream and sends a [`ClientHello`];
//! the exit answers with an [`Offer`] or closes the connection with one of
//! the [`close`] codes. Each message is a little-endian `u16` length and a
//! postcard body.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::net::Ipv4Net;

/// This handshake's version.
pub const VERSION: u16 = 1;

/// The largest handshake message either side reads.
pub const MAX_MESSAGE_BYTES: usize = 4096;

/// Why a client came.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purpose {
    /// To learn what the exit offers, without routing.
    Probe,
    /// To route its traffic through the exit.
    Route,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub version: u16,
    pub purpose: Purpose,
}

/// What an exit offers a client of the same person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The exit's address in the tunnel network, and its DNS server.
    pub gateway: Ipv4Addr,
    /// The client's address, when it came to route.
    pub address: Option<Ipv4Addr>,
    /// The exit's tunnel MTU.
    pub mtu: u16,
    /// The networks the exit is attached to, for "only my home network".
    pub home_networks: Vec<Ipv4Net>,
}

/// The application codes a tunnel connection closes with.
pub mod close {
    /// The client is done.
    pub const BYE: u32 = 0;
    /// The client is not one of the exit person's devices.
    pub const NOT_YOURS: u32 = 1;
    /// The exit does not offer itself as a VPN.
    pub const EXIT_OFF: u32 = 2;
    /// The person stopped this device using the exit.
    pub const STOPPED: u32 = 3;
    /// A message broke the handshake.
    pub const PROTOCOL: u32 = 4;
    /// The exit has no address left to give.
    pub const FULL: u32 = 5;
    /// A newer connection of the same device replaced this one.
    pub const REPLACED: u32 = 6;
}

/// `message` framed for the control stream.
///
/// # Errors
///
/// The message does not encode, or is longer than [`MAX_MESSAGE_BYTES`].
pub fn frame<T: Serialize>(message: &T) -> Result<Vec<u8>, String> {
    let body = postcard::to_allocvec(message).map_err(|error| error.to_string())?;
    let length = u16::try_from(body.len())
        .ok()
        .filter(|length| usize::from(*length) <= MAX_MESSAGE_BYTES)
        .ok_or("handshake message too long")?;
    let mut framed = length.to_le_bytes().to_vec();
    framed.extend_from_slice(&body);
    Ok(framed)
}

/// Read one framed message from `stream`.
///
/// # Errors
///
/// The stream ends, the length is out of bounds, or the body does not decode.
pub async fn read<T: for<'de> Deserialize<'de>>(
    stream: &mut iroh::endpoint::RecvStream,
) -> Result<T, String> {
    let mut length = [0_u8; 2];
    stream
        .read_exact(&mut length)
        .await
        .map_err(|error| error.to_string())?;
    let length = usize::from(u16::from_le_bytes(length));
    if length > MAX_MESSAGE_BYTES {
        return Err("handshake message too long".into());
    }
    let mut body = vec![0_u8; length];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    postcard::from_bytes(&body).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_frame_with_their_length() {
        let offer = Offer {
            gateway: crate::net::GATEWAY,
            address: Some(Ipv4Addr::new(100, 100, 64, 2)),
            mtu: 1400,
            home_networks: vec!["192.168.1.0/24".parse().unwrap()],
        };
        let framed = frame(&offer).unwrap();
        let length = usize::from(u16::from_le_bytes([framed[0], framed[1]]));
        assert_eq!(length, framed.len() - 2);
        assert_eq!(postcard::from_bytes::<Offer>(&framed[2..]).unwrap(), offer);
    }
}
