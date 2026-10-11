//! Peer-to-peer frame streams: an [`Endpoint`] other peers dial by its
//! ticket, carrying length-prefixed frames one way over each connection.
//!
//! Every frame on the wire is a little-endian `u32` length and that many
//! bytes; an empty frame carries nothing. Connections are told apart by an
//! application protocol name (ALPN), which a protocol versions so that a
//! change is a clean break rather than a silent mis-parse.
//!
//! iroh QUIC on the desktop. A page cannot accept connections, so
//! [`Endpoint::bind`] answers [`Unsupported`](crate::Unsupported) there
//! ([`Feature::PeerToPeer`]).
//!
//! [`Feature::PeerToPeer`]: crate::Feature::PeerToPeer

use crate::NetError;

#[cfg(not(target_family = "wasm"))]
use imp_native as imp;
#[cfg(target_family = "wasm")]
use imp_web as imp;

/// A bound endpoint: its ticket, and the frames peers sent it.
///
/// Dropping it closes the endpoint, its connections and their tasks.
pub struct Endpoint<T>(imp::Endpoint<T>);

/// The sending half of a connection [`Endpoint::connect`] opened.
pub struct Peer(imp::Peer);

/// What became of a frame handed to [`Peer::try_send`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// Queued for the connection.
    Queued,
    /// The connection's queue is full; the frame was dropped.
    Full,
    /// The connection has ended.
    Closed,
}

impl<T> std::fmt::Debug for Endpoint<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer").finish_non_exhaustive()
    }
}

impl<T: Send + 'static> Endpoint<T> {
    /// Bind an endpoint that accepts `alpn` connections.
    ///
    /// Each non-empty frame a peer sends, at most `max_frame` bytes, passes
    /// through `accept`; what it returns waits for [`try_recv`](Self::try_recv)
    /// in a queue of `capacity`. A frame `accept` refuses is dropped, and so
    /// is one that finds the queue full: a stale realtime frame is worse than
    /// a lost one. Await it on the runtime that is to run the endpoint's
    /// tasks.
    ///
    /// # Errors
    ///
    /// [`NetError::Unsupported`] in a page, [`NetError::Connect`] when the
    /// endpoint cannot bind (no usable network stack).
    pub async fn bind(
        alpn: &'static [u8],
        max_frame: usize,
        capacity: usize,
        accept: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
    ) -> Result<Self, NetError> {
        imp::Endpoint::bind(alpn, max_frame, capacity, accept)
            .await
            .map(Self)
    }
}

impl<T> Endpoint<T> {
    /// What a peer passes to [`connect`](Self::connect) to reach this
    /// endpoint: its identity and the addresses it was bound on.
    pub fn ticket(&self) -> &str {
        self.0.ticket()
    }

    /// The next accepted frame, if one has arrived.
    pub fn try_recv(&mut self) -> Option<T> {
        self.0.try_recv()
    }

    /// Dial the endpoint `ticket` names, over this endpoint's protocol. The
    /// connection is made in the background; frames handed to the returned
    /// [`Peer`] meanwhile wait in its queue of `capacity`.
    ///
    /// # Errors
    ///
    /// [`NetError::InvalidRequest`] for a ticket that does not parse.
    pub fn connect(&self, ticket: &str, capacity: usize) -> Result<Peer, NetError> {
        self.0.connect(ticket, capacity).map(Peer)
    }
}

impl Peer {
    /// Queue `frame` for the connection without waiting.
    pub fn try_send(&self, frame: Vec<u8>) -> Sent {
        self.0.try_send(frame)
    }
}

#[cfg(not(target_family = "wasm"))]
mod imp_native {
    use tokio::sync::mpsc;

    use super::Sent;
    use crate::NetError;

    pub(super) struct Endpoint<T> {
        iroh: iroh::Endpoint,
        // Owns the accept loop; kept alive so peers can dial in.
        router: iroh::protocol::Router,
        alpn: &'static [u8],
        framing: wire_frame::Framing,
        ticket: String,
        inbound: mpsc::Receiver<T>,
        // The runtime `bind` ran on, which runs the writers and the shutdown.
        runtime: tokio::runtime::Handle,
    }

    /// Reads the frames of each accepted connection into the endpoint's
    /// queue.
    struct Accept<T, F> {
        framing: wire_frame::Framing,
        decode: std::sync::Arc<F>,
        inbound: mpsc::Sender<T>,
    }

    impl<T, F> Clone for Accept<T, F> {
        fn clone(&self) -> Self {
            Self {
                framing: self.framing,
                decode: std::sync::Arc::clone(&self.decode),
                inbound: self.inbound.clone(),
            }
        }
    }

    impl<T, F> std::fmt::Debug for Accept<T, F> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Accept")
        }
    }

    impl<T, F> iroh::protocol::ProtocolHandler for Accept<T, F>
    where
        T: Send + 'static,
        F: Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
    {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            let recv = connection.accept_uni().await?;
            let mut frames = wire_frame::FrameReader::new(recv, self.framing);
            loop {
                match frames.read_frame_async().await {
                    Ok(Some([])) => {}
                    Ok(Some(frame)) => {
                        let Some(value) = (self.decode)(frame) else {
                            continue;
                        };
                        match self.inbound.try_send(value) {
                            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                            // The endpoint was dropped: reading on would
                            // only hide that the connection is unwanted.
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                    // Clean end of stream or a torn one: the peer is done.
                    Ok(None) | Err(_) => break,
                }
            }
            Ok(())
        }
    }

    impl<T: Send + 'static> Endpoint<T> {
        pub(super) async fn bind(
            alpn: &'static [u8],
            max_frame: usize,
            capacity: usize,
            accept: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
        ) -> Result<Self, NetError> {
            let framing = wire_frame::Framing::little_endian(max_frame);
            let (inbound_tx, inbound) = mpsc::channel(capacity);
            let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                .bind()
                .await
                .map_err(|error| NetError::connect(crate::native::chain(&error)))?;
            let router = iroh::protocol::Router::builder(endpoint.clone())
                .accept(
                    alpn,
                    Accept {
                        framing,
                        decode: std::sync::Arc::new(accept),
                        inbound: inbound_tx,
                    },
                )
                .spawn();
            // Local (direct) addresses are known once bound; LAN and loopback
            // peers need no relay.
            let ticket = serde_json::to_string(&endpoint.addr())
                .map_err(|error| NetError::Protocol(error.to_string()))?;
            Ok(Self {
                iroh: endpoint,
                router,
                alpn,
                framing,
                ticket,
                inbound,
                runtime: tokio::runtime::Handle::current(),
            })
        }
    }

    impl<T> Endpoint<T> {
        pub(super) fn ticket(&self) -> &str {
            &self.ticket
        }

        pub(super) fn try_recv(&mut self) -> Option<T> {
            self.inbound.try_recv().ok()
        }

        pub(super) fn connect(&self, ticket: &str, capacity: usize) -> Result<Peer, NetError> {
            let addr = serde_json::from_str::<iroh::EndpointAddr>(ticket)
                .map_err(|error| NetError::InvalidRequest(format!("peer ticket: {error}")))?;
            let (frames, mut outbound) = mpsc::channel::<Vec<u8>>(capacity);
            let (endpoint, alpn, framing) = (self.iroh.clone(), self.alpn, self.framing);
            drop(self.runtime.spawn(async move {
                let Ok(connection) = endpoint.connect(addr, alpn).await else {
                    return;
                };
                let Ok(mut send) = connection.open_uni().await else {
                    return;
                };
                while let Some(frame) = outbound.recv().await {
                    if framing.write_async(&mut send, &frame).await.is_err() {
                        break;
                    }
                }
                let _ = send.finish();
            }));
            Ok(Peer(frames))
        }
    }

    impl<T> Drop for Endpoint<T> {
        fn drop(&mut self) {
            let router = self.router.clone();
            drop(self.runtime.spawn(async move {
                // Closes the accept loop, its handlers and the endpoint.
                let _ = router.shutdown().await;
            }));
        }
    }

    pub(super) struct Peer(mpsc::Sender<Vec<u8>>);

    impl Peer {
        pub(super) fn try_send(&self, frame: Vec<u8>) -> Sent {
            match self.0.try_send(frame) {
                Ok(()) => Sent::Queued,
                Err(mpsc::error::TrySendError::Full(_)) => Sent::Full,
                Err(mpsc::error::TrySendError::Closed(_)) => Sent::Closed,
            }
        }
    }
}

#[cfg(target_family = "wasm")]
mod imp_web {
    //! A page cannot accept connections: the types are uninhabited, and
    //! binding answers `Unsupported`.
    #![allow(
        clippy::unused_async,
        reason = "the same signatures as the desktop backend"
    )]

    use std::marker::PhantomData;

    use super::Sent;
    use crate::{Feature, NetError};

    enum Never {}

    pub(super) struct Endpoint<T>(Never, PhantomData<T>);

    impl<T: Send + 'static> Endpoint<T> {
        pub(super) async fn bind(
            _alpn: &'static [u8],
            _max_frame: usize,
            _capacity: usize,
            _accept: impl Fn(&[u8]) -> Option<T> + Send + Sync + 'static,
        ) -> Result<Self, NetError> {
            Err(crate::caps::unsupported(Feature::PeerToPeer).into())
        }
    }

    impl<T> Endpoint<T> {
        pub(super) fn ticket(&self) -> &str {
            match self.0 {}
        }

        pub(super) fn try_recv(&mut self) -> Option<T> {
            match self.0 {}
        }

        pub(super) fn connect(&self, _ticket: &str, _capacity: usize) -> Result<Peer, NetError> {
            match self.0 {}
        }
    }

    pub(super) struct Peer(Never);

    impl Peer {
        pub(super) fn try_send(&self, _frame: Vec<u8>) -> Sent {
            match self.0 {}
        }
    }
}

#[cfg(test)]
#[cfg(not(target_family = "wasm"))]
mod tests {
    /// Peers on other builds read these bytes: a little-endian `u32` length,
    /// then the frame; an empty frame is legal.
    #[test]
    fn frame_wire_bytes_are_pinned() {
        let framing = wire_frame::Framing::little_endian(8);
        let mut wire = Vec::new();
        framing.write(&mut wire, b"obj").unwrap();
        framing.write(&mut wire, b"").unwrap();
        assert_eq!(wire, [3, 0, 0, 0, b'o', b'b', b'j', 0, 0, 0, 0]);
        let oversized = 9u32.to_le_bytes();
        assert!(framing.read(&mut oversized.as_slice()).is_err());
    }
}
