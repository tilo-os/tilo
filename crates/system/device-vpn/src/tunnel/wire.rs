//! The handshake on a tunnel connection's control stream, and the codes a
//! connection closes with.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::net::Ipv4Net;

pub const VERSION: u16 = 1;

pub const MAX_MESSAGE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purpose {
    Probe,
    Route,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub version: u16,
    pub purpose: Purpose,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub gateway: Ipv4Addr,
    pub address: Option<Ipv4Addr>,
    pub mtu: u16,
    pub home_networks: Vec<Ipv4Net>,
}

pub mod close {
    pub const BYE: u32 = 0;
    pub const NOT_YOURS: u32 = 1;
    pub const EXIT_OFF: u32 = 2;
    pub const STOPPED: u32 = 3;
    pub const PROTOCOL: u32 = 4;
    pub const FULL: u32 = 5;
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
