//! The lane's connections: one QUIC connection per pair of devices per lane,
//! on the session's own endpoint.
//!
//! A call is a mesh: each device dials every other device its presence names
//! in the same call and accepts the return dial, so one connection carries one
//! device's voice to one other device. A voice is datagrams, which never wait
//! and are never retransmitted: a lost one costs its frames and nothing
//! behind it. A lane carries one kind of media, so a screen share's stream
//! gets a connection of its own and never queues a call's audio behind it.
//!
//! Every connection starts with a [`Hello`] on a bi-stream, both ways: what
//! workspace, channel and call the two devices think they are in, and which
//! lane. The connection's endpoint id is the remote device's key, so the
//! handshake already proves who dialled: a device the workspace's roster does
//! not name is refused and the connection closed with a reason before a frame
//! is read.
//!
//! Nothing here touches media: a caller seals with [`crate::Microphone`] and
//! hands [`Peer::send`] the datagram, and opens what [`Peer::received`]
//! returns.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{
    ConnectOptions, ConnectingError, Connection, ConnectionError, QuicTransportConfig, ReadError,
    ReadToEndError, SendDatagramError, SendStream, VarInt, WriteError,
};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

use crate::frame::Lane;
use crate::roster::Roster;

/// ALPN of a call's lane connections. The version is in the name, as the
/// direct link's is: a device speaks this one or nothing, and there is no
/// negotiation.
pub const ALPN: &[u8] = b"tilo-call/1";

/// A hello is a few ids and one lane; anything larger is not one.
const MAX_HELLO_BYTES: usize = 1024;

/// The application close code of a connection this device refused.
const REFUSED: u32 = 1;

/// Bytes of a call's datagrams this device holds: the playout of a stalled
/// audio thread, not a call's backlog.
const RECEIVE_BUFFER: usize = 256 * 1024;

/// A lane's connection ends after this long without a packet from the peer:
/// the session endpoint's own idle timeout, so a call does not outlive the
/// rest of the session's links.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// Streams a lane's connection accepts at once: a picture stream per source
/// and layer of the call's other side, each with the room to start over, and
/// the control streams either side opens.
const MAX_UNI_STREAMS: u32 = 64;
const MAX_BI_STREAMS: u32 = 8;

/// The QUIC transport a lane's connections use: the session's idle timeout,
/// a receive buffer sized for a call's datagrams, and room for the picture
/// streams of a call.
#[must_use]
pub fn transport_config() -> QuicTransportConfig {
    let idle = u32::try_from(IDLE_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    QuicTransportConfig::builder()
        .max_idle_timeout(Some(VarInt::from_u32(idle).into()))
        .datagram_receive_buffer_size(Some(RECEIVE_BUFFER))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_UNI_STREAMS))
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_BI_STREAMS))
        .build()
}

/// What one device tells the other when a lane opens: which call this
/// connection is for, and which of its lanes it carries.
///
/// A device in two calls, or with another workspace's channel open, must not
/// have its audio reached by the wrong one; the handshake compares this with
/// its own and closes the connection when they differ.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The workspace the call is in, as its id travels.
    pub workspace: String,
    /// The channel whose content keys seal the call's frames.
    pub channel: collab_types::ChannelId,
    /// The call itself: the voice channel within the workspace.
    pub call: String,
    /// Which of the call's lanes this connection carries.
    pub lane: Lane,
}

impl Hello {
    /// Whether this is a hello for the same call: the two devices agree on
    /// the workspace, the channel and the call. The lane differs by design:
    /// one device's call has one connection per lane, and the accepting side
    /// answers whichever one was dialled.
    #[must_use]
    fn same_call(&self, other: &Self) -> bool {
        self.workspace == other.workspace
            && self.channel == other.channel
            && self.call == other.call
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        postcard::to_stdvec(self).map_err(|error| format!("the hello does not encode: {error}"))
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        postcard::from_bytes(bytes).map_err(|error| format!("not a hello: {error}"))
    }
}

/// Why two devices have no lane between them.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failed {
    /// The other device refused: it is not in this call, or what one of them
    /// says the call is differs from what the other says.
    #[error("the device refused the call: {0}")]
    Refused(String),
    /// The connection could not be made, or ended before the handshake did.
    #[error("{0}")]
    Ended(String),
}

/// One device's side of a call's lanes: the connections it dials, and the
/// ones the call's other devices dial to it.
///
/// Clone it to hand one to the session endpoint's [`iroh::protocol::Router`]
/// under [`ALPN`] (it is the accepting [`ProtocolHandler`]) and to keep
/// dialing with; every clone accepts and dials for the same call. The hello
/// it holds names the lane it dials, and the accepting side answers whichever
/// of the call's lanes was dialled, so one device's call has a connection per
/// lane and one handler for all of them.
#[derive(Clone)]
pub struct Calls(Arc<Inner>);

struct Inner {
    hello: Hello,
    roster: Mutex<Roster>,
    accepted: mpsc::UnboundedSender<Peer>,
}

impl std::fmt::Debug for Calls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Calls")
            .field("call", &self.0.hello.call)
            .field("lane", &self.0.hello.lane)
            .field("roster", &self.0.roster.lock_recover().len())
            .finish_non_exhaustive()
    }
}

impl Calls {
    /// Accept the call `hello` names from the devices `roster` names, and
    /// report every peer whose handshake completed on the returned inbox.
    ///
    /// The inbox is a channel rather than a callback so a caller pumps
    /// connections on whatever it already runs: a device with no lane opened
    /// to it yet hears nothing, and a peer whose connection ended is gone from
    /// it (its [`Peer::received`] ends).
    #[must_use]
    pub fn new(hello: Hello, roster: Roster) -> (Self, mpsc::UnboundedReceiver<Peer>) {
        let (accepted, inbox) = mpsc::unbounded_channel();
        (
            Self(Arc::new(Inner {
                hello,
                roster: Mutex::new(roster),
                accepted,
            })),
            inbox,
        )
    }

    /// The call this device accepts and dials for.
    #[must_use]
    pub fn hello(&self) -> &Hello {
        &self.0.hello
    }

    /// Who may send to this device in this call from now on: a device the
    /// workspace removed stops being admitted, and one it certified starts.
    /// Connections already open are the caller's to close.
    pub fn set_roster(&self, roster: Roster) {
        *self.0.roster.lock_recover() = roster;
    }

    /// Open a lane to the device at `addr`, and wait for it to answer with
    /// its own [`Hello`].
    ///
    /// # Errors
    ///
    /// [`Failed::Refused`] when the device closed the connection with a reason
    /// (it does not have this device on the roster, or what it says the call
    /// is differs), and [`Failed::Ended`] when the connection could not be
    /// made or ended first.
    pub async fn dial(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<Peer, Failed> {
        let connection = endpoint
            .connect_with_opts(
                addr,
                ALPN,
                ConnectOptions::new().with_transport_config(transport_config()),
            )
            .await
            .map_err(|error| Failed::Ended(error.to_string()))?
            .await
            .map_err(|error| match error {
                ConnectingError::ConnectionError { source, .. } => closed(source),
                error => Failed::Ended(error.to_string()),
            })?;
        self.handshake(&connection, true).await
    }

    /// Say who this is on `connection` and check what the other device says,
    /// whichever side dialled.
    async fn handshake(&self, connection: &Connection, dialling: bool) -> Result<Peer, Failed> {
        let remote = *connection.remote_id().as_bytes();
        let opened = async {
            if dialling {
                connection.open_bi().await
            } else {
                connection.accept_bi().await
            }
        };
        let (mut send, mut recv) = opened.await.map_err(closed)?;
        if dialling {
            send.write_all(&self.0.hello.encode().map_err(Failed::Ended)?)
                .await
                .map_err(writing)?;
            send.finish().map_err(ended)?;
        }
        let theirs = recv.read_to_end(MAX_HELLO_BYTES).await.map_err(reading)?;
        let theirs = Hello::decode(&theirs).map_err(Failed::Ended)?;
        if !theirs.same_call(&self.0.hello) {
            let reason = "the device asked for another call";
            connection.close(REFUSED.into(), reason.as_bytes());
            return Err(Failed::Refused(reason.to_owned()));
        }
        let lane = theirs.lane;
        if !dialling {
            // The dialer reads this on the stream it opened: it learns whether
            // it reached the call it meant to, and is closed if it did not.
            // The lane is the one it dialled, so both sides know which of the
            // call's lanes this connection carries.
            let reply = Hello {
                lane,
                ..self.0.hello.clone()
            };
            send.write_all(&reply.encode().map_err(Failed::Ended)?)
                .await
                .map_err(writing)?;
            send.finish().map_err(ended)?;
        }
        Ok(Peer {
            connection: connection.clone(),
            lane,
            remote,
        })
    }
}

impl ProtocolHandler for Calls {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let remote = *connection.remote_id().as_bytes();
        if !self.0.roster.lock_recover().admits(remote) {
            let reason = "the device is not on the workspace's roster";
            connection.close(REFUSED.into(), reason.as_bytes());
            return Err(AcceptError::from_err(std::io::Error::other(reason)));
        }
        let peer = self
            .handshake(&connection, false)
            .await
            .map_err(|failed| AcceptError::from_err(std::io::Error::other(failed.to_string())))?;
        // No one is waiting for this device's call any more: the call is over,
        // and the connection with it.
        self.0.accepted.send(peer).map_err(|_| {
            let reason = "the call was closed";
            connection.close(REFUSED.into(), reason.as_bytes());
            AcceptError::from_err(std::io::Error::other(reason))
        })
    }
}

/// A lane connection with one other device, its handshake done, which each
/// side's capture and playout may hold their own handle to.
#[derive(Clone)]
pub struct Peer {
    connection: Connection,
    lane: Lane,
    remote: [u8; 32],
}

impl Peer {
    /// The remote device's public key: the one the connection's handshake
    /// proved, and the one the roster named.
    #[must_use]
    pub const fn remote(&self) -> [u8; 32] {
        self.remote
    }

    /// Which of the call's lanes this connection carries.
    #[must_use]
    pub const fn lane(&self) -> Lane {
        self.lane
    }

    /// The connection this lane runs on: what a lane's pictures are read
    /// from and written to.
    #[must_use]
    pub(crate) fn connection(&self) -> Connection {
        self.connection.clone()
    }

    /// Open the stream a picture of `source` at a layer is written to, with
    /// `priority` deciding what waits behind what: a stage before a grid.
    /// `None` when the connection ended or the other side is not reading
    /// pictures.
    pub(crate) async fn open_pictures(&self, priority: i32) -> Option<SendStream> {
        let stream = self.connection.open_uni().await.ok()?;
        stream.set_priority(priority).ok()?;
        Some(stream)
    }

    /// Open a control stream, which carries what this device shows of the
    /// other and what it asks to start over.
    pub(crate) async fn open_control(&self) -> Option<SendStream> {
        let (send, _recv) = self.connection.open_bi().await.ok()?;
        Some(send)
    }

    /// Send one datagram.
    ///
    /// A datagram is sent at once or not at all: it is never queued behind
    /// another, and the transport drops the oldest one when its buffer is
    /// full. A failure means this frame was not sent, which a call counts and
    /// drops rather than waits on.
    ///
    /// # Errors
    ///
    /// [`SendDatagramError::TooLarge`] when the datagram does not fit a QUIC
    /// packet, and the connection's end when it went away.
    pub fn send(&self, datagram: impl Into<Bytes>) -> Result<(), SendDatagramError> {
        self.connection.send_datagram(datagram.into())
    }

    /// The next datagram the other device sent.
    ///
    /// # Errors
    ///
    /// The connection's end: the device closed the call, or went away.
    pub async fn received(&self) -> Result<Bytes, ConnectionError> {
        self.connection.read_datagram().await
    }

    /// Ends when this lane does: a device whose lane went away opens another
    /// one on the same path it opened this one.
    pub async fn ended(&self) {
        self.connection.closed().await;
    }

    /// The frames this peer's transport lost, and the packets it sent, as the
    /// connection counts them: what a sender sizes its repeats from.
    #[must_use]
    pub fn packets(&self) -> (u64, u64) {
        let stats = self.connection.stats();
        (stats.udp_tx.datagrams, stats.lost_packets)
    }

    /// End the call this connection carries, telling the other device so.
    pub fn close(&self) {
        self.connection.close(0_u32.into(), b"");
    }
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("remote", &self.remote)
            .field("lane", &self.lane)
            .finish_non_exhaustive()
    }
}

/// A connection error as a [`Failed`], keeping the reason a device closed
/// the connection with.
fn ended(error: impl std::fmt::Display) -> Failed {
    Failed::Ended(error.to_string())
}

/// A handshake step that failed because the other device closed the
/// connection: its reason is what this device gets to act on.
fn closed(error: ConnectionError) -> Failed {
    match error {
        ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(REFUSED) =>
        {
            Failed::Refused(String::from_utf8_lossy(&close.reason).into_owned())
        }
        error => Failed::Ended(error.to_string()),
    }
}

/// A read that ended because the connection did, keeping the reason a device
/// closed it with: the stream error alone says only that the connection is
/// gone.
fn reading(error: ReadToEndError) -> Failed {
    match error {
        ReadToEndError::Read(ReadError::ConnectionLost(error)) => closed(error),
        error => Failed::Ended(error.to_string()),
    }
}

/// A write that ended because the connection did, as [`reading`].
fn writing(error: WriteError) -> Failed {
    match error {
        WriteError::ConnectionLost(error) => closed(error),
        error => Failed::Ended(error.to_string()),
    }
}
