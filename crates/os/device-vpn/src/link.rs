//! One device-VPN link between a person's desktop session and this device's daemon.
//!
//! The daemon holds the device's one endpoint, so the tunnel's peer-to-peer half runs there: it
//! dials the exit another of the person's devices serves, answers the tunnel on this device's
//! endpoint, and carries the packets. A shell keeps what only it may do — open its TUN devices
//! through the root helper and route the person's traffic — and asks the daemon for a link on the
//! shell link the daemon gave it.
//!
//! A link opens with one [`Ask`], answered by one [`Control`]: an offer and where the tunnel's
//! own packets leave this device, or why the other device would not serve it. Packets then cross
//! it both ways, and the control stream carries what changes while they do: how the tunnel ended,
//! who routes through this device, and a request to stop one of them.
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 1 | kind ([`Kind`]) |
//! | 4 | payload length, little-endian, at most [`MAX_CONTROL`] |
//! | length | payload |
//!
//! A link carries one tunnel, so its frames need no stream id: what is a control message is
//! JSON, and what is not is one IPv4 packet.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::net::MAX_MTU;
use crate::protocol::Carrier;
use crate::tunnel::client::{Ended, Refusal};
use crate::tunnel::wire::Offer;

/// The largest control message a link carries.
pub const MAX_CONTROL: usize = 64 * 1024;
/// The largest packet a link carries: a TUN's MTU and the IPv4 header's room.
pub const MAX_PACKET: usize = MAX_MTU as usize + 64;

/// What one end of a link sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// The shell asks what `exit` offers, without routing through it. `at` is where the shell
    /// knows the device is reached, when it knows: the daemon's own discovery answers otherwise.
    Probe {
        exit: String,
        #[serde(default)]
        at: Option<iroh::EndpointAddr>,
    },
    /// The shell asks to route through `exit` until the link ends, at `at` when it knows where
    /// the device is reached.
    Through {
        exit: String,
        #[serde(default)]
        at: Option<iroh::EndpointAddr>,
    },
    /// The shell lends the daemon this device's exit TUN, so the person's other devices may route
    /// through this one: the link carries that TUN's packets until it ends.
    Exit,
    /// What the exit `exit` offered, the MTU this device's TUN takes, and what must reach the exit
    /// outside the tunnel.
    Offer {
        offer: Offer,
        mtu: u16,
        carrier: Carrier,
    },
    /// The exit refused this device, for the reason the tunnel's close said.
    Refused(Refusal),
    /// The exit was not reached, or did not answer as the tunnel expects.
    Failed(String),
    /// This device takes the shell's exit TUN: the person's other devices may dial it.
    Ready,
    /// The tunnel ended, and why.
    Ended(Ended),
    /// The devices routing through this device now, by their endpoint ids in hex.
    Users { devices: Vec<String> },
    /// Stop `device` routing through this device.
    Stop { device: String },
}

/// What a frame carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Control(Control),
    Packet(Vec<u8>),
}

/// One frame's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Control = 1,
    Packet = 2,
}

impl Kind {
    const fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Control,
            2 => Self::Packet,
            _ => return None,
        })
    }
}

/// Write `control` as one frame.
///
/// # Errors
/// The stream did not take it, or the message is longer than [`MAX_CONTROL`].
pub async fn write_control<W: AsyncWrite + Unpin>(
    writer: &mut W,
    control: &Control,
) -> io::Result<()> {
    let body = serde_json::to_vec(control).map_err(io::Error::other)?;
    if body.len() > MAX_CONTROL {
        return Err(io::Error::other("a device VPN control too long to send"));
    }
    write_frame(writer, Kind::Control, &body).await
}

/// Write `packet` as one frame.
///
/// # Errors
/// The stream did not take it, or the packet is longer than [`MAX_PACKET`].
pub async fn write_packet<W: AsyncWrite + Unpin>(writer: &mut W, packet: &[u8]) -> io::Result<()> {
    if packet.len() > MAX_PACKET {
        return Err(io::Error::other("a tunnel packet too long to pass on"));
    }
    write_frame(writer, Kind::Packet, packet).await
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    kind: Kind,
    payload: &[u8],
) -> io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(io::Error::other)?;
    let mut head = [0_u8; 5];
    head[0] = kind as u8;
    head[1..].copy_from_slice(&length.to_le_bytes());
    writer.write_all(&head).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

/// Read one frame.
///
/// # Errors
/// The stream ended or failed, or what came is no frame.
pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Frame> {
    let mut head = [0_u8; 5];
    reader.read_exact(&mut head).await?;
    let kind = Kind::of(head[0])
        .ok_or_else(|| io::Error::other(format!("no device VPN frame kind {}", head[0])))?;
    let length = u32::from_le_bytes(head[1..].try_into().unwrap_or_default());
    let length = usize::try_from(length).map_err(io::Error::other)?;
    if length > MAX_CONTROL {
        return Err(io::Error::other("a device VPN frame too long to read"));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    match kind {
        Kind::Packet => Ok(Frame::Packet(payload)),
        Kind::Control => serde_json::from_slice(&payload)
            .map(Frame::Control)
            .map_err(|error| io::Error::other(format!("a device VPN control: {error}"))),
    }
}
