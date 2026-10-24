//! The client side: ask an exit what it offers, route through it, and say
//! why the tunnel ended.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, ConnectionError, VarInt};
use iroh::{Endpoint, EndpointAddr};

use super::ALPN;
use super::device::PacketDevice;
use super::exit::send_or_shrink;
use super::wire::{self, ClientHello, Offer, Purpose, close};
use crate::net::{MAX_MTU, MIN_MTU, is_client_address};
use crate::packet::Ipv4Packet;

/// How long dialling an exit and its handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Why an exit turned this device away or ended its tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The exit is not one of this person's devices, or does not count this
    /// one as theirs.
    NotYours,
    /// The exit's switch is off.
    ExitOff,
    /// The person stopped this device using the exit, from the exit.
    Stopped,
    /// The exit has no address left, or could not open its tunnel.
    Full,
    /// Another connection of this device took over.
    Replaced,
    /// The exit did not follow the handshake.
    Protocol,
}

impl Refusal {
    /// The refusal a connection's close code says, if it says one.
    #[must_use]
    pub fn of_code(code: u64) -> Option<Self> {
        let code = u32::try_from(code).ok()?;
        Some(match code {
            close::NOT_YOURS => Self::NotYours,
            close::EXIT_OFF => Self::ExitOff,
            close::STOPPED => Self::Stopped,
            close::FULL => Self::Full,
            close::REPLACED => Self::Replaced,
            close::PROTOCOL => Self::Protocol,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    Refused(Refusal),
    /// The exit could not be reached, or the connection broke.
    Unreachable(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "refused: {refusal:?}"),
            Self::Unreachable(reason) => write!(f, "unreachable: {reason}"),
        }
    }
}

/// How a tunnel ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The exit ended it, saying why.
    Refused(Refusal),
    /// This side closed it.
    Closed,
    /// The path to the exit or the local TUN broke.
    Lost(String),
}

impl Ended {
    #[must_use]
    pub fn of(error: &ConnectionError) -> Self {
        match error {
            ConnectionError::ApplicationClosed(close) => {
                Refusal::of_code(close.error_code.into_inner()).map_or_else(
                    || Self::Lost("the exit closed the tunnel".into()),
                    Self::Refused,
                )
            }
            ConnectionError::LocallyClosed => Self::Closed,
            other => Self::Lost(other.to_string()),
        }
    }
}

/// A tunnel the exit accepted.
pub struct Tunnel {
    pub connection: Connection,
    pub offer: Offer,
    /// This device's address in the tunnel network.
    pub address: Ipv4Addr,
}

impl Tunnel {
    /// The MTU for this device's TUN: what the exit offers and the path
    /// carries now, never below the IPv4 minimum.
    #[must_use]
    pub fn mtu(&self) -> u16 {
        let path = self
            .connection
            .max_datagram_size()
            .and_then(|size| u16::try_from(size).ok())
            .unwrap_or(MIN_MTU);
        self.offer.mtu.min(path).clamp(MIN_MTU, MAX_MTU)
    }

    /// End the tunnel from this side.
    pub fn close(&self) {
        self.connection.close(VarInt::from_u32(close::BYE), b"bye");
    }
}

fn refused_or(connection: &Connection, reason: String) -> ClientError {
    match connection.close_reason().as_ref().map(Ended::of) {
        Some(Ended::Refused(refusal)) => ClientError::Refused(refusal),
        _ => ClientError::Unreachable(reason),
    }
}

async fn handshake(
    endpoint: &Endpoint,
    exit: EndpointAddr,
    purpose: Purpose,
) -> Result<(Connection, Offer), ClientError> {
    let connection = endpoint
        .connect(exit, ALPN)
        .await
        .map_err(|error| ClientError::Unreachable(error.to_string()))?;
    let exchanged = async {
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| error.to_string())?;
        let hello = wire::frame(&ClientHello {
            version: wire::VERSION,
            purpose,
        })?;
        send.write_all(&hello)
            .await
            .map_err(|error| error.to_string())?;
        send.finish().map_err(|error| error.to_string())?;
        wire::read::<Offer>(&mut recv).await
    }
    .await;
    match exchanged {
        Ok(offer) => Ok((connection, offer)),
        Err(reason) => {
            // A refusal closes the connection before the offer; give the
            // close a moment to arrive so its code is the answer.
            let _ = tokio::time::timeout(Duration::from_secs(2), connection.closed()).await;
            Err(refused_or(&connection, reason))
        }
    }
}

/// What `exit` offers, without routing through it.
///
/// # Errors
///
/// The exit refused, or could not be reached.
pub async fn probe(
    endpoint: &Endpoint,
    exit: impl Into<EndpointAddr>,
) -> Result<Offer, ClientError> {
    let (connection, offer) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        handshake(endpoint, exit.into(), Purpose::Probe),
    )
    .await
    .map_err(|_| ClientError::Unreachable("the exit did not answer".into()))??;
    connection.close(VarInt::from_u32(close::BYE), b"bye");
    Ok(offer)
}

/// Open a tunnel through `exit`.
///
/// # Errors
///
/// The exit refused, gave no usable address, or could not be reached.
pub async fn connect(
    endpoint: &Endpoint,
    exit: impl Into<EndpointAddr>,
) -> Result<Tunnel, ClientError> {
    let (connection, offer) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        handshake(endpoint, exit.into(), Purpose::Route),
    )
    .await
    .map_err(|_| ClientError::Unreachable("the exit did not answer".into()))??;
    let Some(address) = offer.address.filter(|address| is_client_address(*address)) else {
        connection.close(VarInt::from_u32(close::PROTOCOL), b"no address");
        return Err(ClientError::Refused(Refusal::Protocol));
    };
    Ok(Tunnel {
        connection,
        offer,
        address,
    })
}

/// Carry packets between `device` and the exit until the tunnel ends.
pub async fn run<D: PacketDevice>(tunnel: &Tunnel, device: Arc<D>) -> Ended {
    let connection = &tunnel.connection;
    let address = tunnel.address;
    let outward = async {
        let mut buf = vec![0_u8; usize::from(MAX_MTU)];
        loop {
            let length = match device.recv(&mut buf).await {
                Ok(length) => length,
                Err(error) => return Ended::Lost(format!("the tunnel device: {error}")),
            };
            let Some(bytes) = buf.get(..length) else {
                continue;
            };
            let Some(packet) = Ipv4Packet::parse(bytes) else {
                continue;
            };
            // Only this device's own traffic goes to the exit.
            if packet.source() != address {
                continue;
            }
            send_or_shrink(connection, &packet, &*device).await;
        }
    };
    let inward = async {
        loop {
            let datagram = match connection.read_datagram().await {
                Ok(datagram) => datagram,
                Err(error) => return Ended::of(&error),
            };
            let Some(packet) = Ipv4Packet::parse(&datagram) else {
                continue;
            };
            if packet.destination() != address {
                continue;
            }
            if let Err(error) = device.send(&datagram).await {
                return Ended::Lost(format!("the tunnel device: {error}"));
            }
        }
    };
    tokio::select! {
        ended = outward => ended,
        ended = inward => ended,
        error = connection.closed() => Ended::of(&error),
    }
}
