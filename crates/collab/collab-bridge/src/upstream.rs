//! Upstream connection to the legacy serde-JSON `CollabRequest` /
//! `CollabEvent` broker — `shell::shell::collab_broker`, which the
//! compositor runs for the session.
//!
//! One upstream connection is opened per SDK connection: the
//! bridge owns a writer half (used by every ChannelHandle method
//! that emits a request) and a reader task that decodes
//! `CollabEvent`s off the wire and routes them into per-stream
//! queues.
//!
//! ## Backpressure
//!
//! - Signed CRDT ops (`SignedCRDTIncremental`) go through an
//!   unbounded mpsc — they are the only path the CRDT convergence
//!   guarantee relies on, so we NEVER drop them.
//! - Legacy unsigned CRDT frames (`CRDTUpdate` / `CRDTIncremental`)
//!   ride the same op queue — dropping them would silently break
//!   apps that still use the pre-signed path.
//! - Presence events (`RemotePresenceUpdate` + `RemoteCursorMoved`)
//!   go through a bounded drop-oldest queue (see [`DropOldest`]) so
//!   a slow SDK reader can't backpressure signed ops. Cursor
//!   moves are the highest-volume presence variant; dropping the
//!   oldest is the standard remote-cursor treatment.
//! - Context snapshots (`Context`) go through an unbounded mpsc:
//!   the shell emits one on roster change (rare), and dropping
//!   would break the identity picker.
//! - Scrollback batches (`ScrollbackBatch`) go through an unbounded
//!   mpsc; the caller matches replies to outstanding
//!   `FetchScrollback` requests FIFO.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Notify, mpsc};

use crate::wire::{CollabEvent, CollabRequest, Collaborator, PresencePayload};
use collab_types::{ArtifactId, SignedOp};

/// Presence stream capacity — drop-oldest kicks in above this many
/// unread events per subscriber. 128 events is roughly one second
/// of a very active pointer stream (60 Hz cursor moves + other
/// variants); sized so a burst survives a slow frame but a truly
/// disconnected reader cannot pin memory.
const PRESENCE_CAPACITY: usize = 128;

/// Bounded FIFO with drop-oldest eviction. Async-friendly: readers
/// await `pop()`; writers call synchronous `push()`. Tracks a
/// dropped-count for telemetry / tests.
pub struct DropOldest<T> {
    inner: Mutex<VecDeque<T>>,
    capacity: usize,
    notify: Notify,
    dropped: parking_lot::Mutex<u64>,
}

impl<T> DropOldest<T> {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            notify: Notify::new(),
            dropped: parking_lot::Mutex::new(0),
        })
    }

    /// Push an item. If the queue is at capacity, drop the oldest
    /// before pushing and bump the dropped-counter.
    pub fn push(&self, item: T) {
        let evicted = {
            let mut q = self.inner.lock();
            let evicted = if q.len() >= self.capacity {
                q.pop_front();
                true
            } else {
                false
            };
            q.push_back(item);
            evicted
        };
        if evicted {
            let mut d = self.dropped.lock();
            *d = d.saturating_add(1);
        }
        self.notify.notify_one();
    }

    /// Await the next item. Suspends without holding the queue lock.
    pub async fn pop(&self) -> T {
        loop {
            {
                let mut q = self.inner.lock();
                if let Some(item) = q.pop_front() {
                    return item;
                }
            }
            self.notify.notified().await;
        }
    }

    /// Cumulative count of dropped items. Only used by tests today.
    #[cfg(test)]
    pub fn dropped(&self) -> u64 {
        *self.dropped.lock()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }
}

/// Envelope emitted by the reader task on the presence path.
/// Merges `RemoteCursorMoved` and `RemotePresenceUpdate` into a
/// single record so `subscribeAll` can dequeue every presence
/// event in arrival order without having to peek across two
/// queues.
#[derive(Debug, Clone)]
pub enum PresenceEnvelope {
    /// Legacy `RemoteCursorMoved` — no key, plus a synthetic
    /// `PresenceKey` will be synthesised by the SDK-facing stream.
    Cursor {
        user_id: String,
        x: f32,
        y: f32,
    },
    /// Full `RemotePresenceUpdate` — carries a postcard key blob
    /// (the bridge does NOT decode it; it forwards the bytes so
    /// SDK apps can postcard-decode into their own PresenceKey).
    Update {
        user_id: String,
        key_bytes: Vec<u8>,
        payload: PresencePayload,
    },
}

/// What an [`OpEnvelope`]'s bytes are.
///
/// The op stream carries three kinds of frame and they are not
/// interchangeable — a delta is applied incrementally, a whole
/// document is merged, a signed envelope is decoded before either.
/// Mirrors the wire's `OpKind`; see `schema/collab.capnp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpKind {
    /// A postcard-encoded `SignedOp`.
    #[default]
    Signed,
    /// A raw Automerge change delta.
    UnsignedIncremental,
    /// A whole Automerge document, for a peer with no history.
    UnsignedSnapshot,
}

/// Envelope for a CRDT op delivered on the op stream.
#[derive(Debug, Clone)]
pub struct OpEnvelope {
    pub op_bytes: Vec<u8>,
    pub from_user_id: String,
    /// What `op_bytes` are. A reader that ignores this and assumes
    /// a signed envelope drops every unsigned frame silently.
    pub kind: OpKind,
}

/// Full roster / context snapshot delivered by the upstream broker.
#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    pub collaborators: Vec<Collaborator>,
    pub permissions: Vec<String>,
}

/// One `ScrollbackBatch` upstream frame. Matched FIFO against the
/// caller's outstanding `fetch_scrollback` request queue keyed by
/// artifact.
#[derive(Debug, Clone)]
pub struct ScrollbackReply {
    pub artifact: ArtifactId,
    pub ops: Vec<Box<SignedOp>>,
    pub exhausted: bool,
}

/// Handles used by capnp server implementations to talk to the
/// upstream broker. Cloned freely — the underlying `Arc`s are
/// cheap.
#[derive(Clone)]
pub struct UpstreamHandle {
    writer: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<UnixStream>>>,
    pub op_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<OpEnvelope>>>,
    pub presence: Arc<DropOldest<PresenceEnvelope>>,
    pub context_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ContextSnapshot>>>,
    pub scrollback_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ScrollbackReply>>>,
}

impl UpstreamHandle {
    /// Test-only constructor for wiring a handle from a pre-built
    /// writer + receivers instead of dialing a real Unix socket.
    /// Kept `pub` (not `pub(crate)`) so integration tests in
    /// `tests/*.rs` can inject a mock upstream.
    #[doc(hidden)]
    pub fn new_for_test(
        writer: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<UnixStream>>>,
        op_rx: mpsc::UnboundedReceiver<OpEnvelope>,
        presence: Arc<DropOldest<PresenceEnvelope>>,
        context_rx: mpsc::UnboundedReceiver<ContextSnapshot>,
        scrollback_rx: mpsc::UnboundedReceiver<ScrollbackReply>,
    ) -> Self {
        Self {
            writer,
            op_rx: Arc::new(tokio::sync::Mutex::new(op_rx)),
            presence,
            context_rx: Arc::new(tokio::sync::Mutex::new(context_rx)),
            scrollback_rx: Arc::new(tokio::sync::Mutex::new(scrollback_rx)),
        }
    }

    /// Serialize + frame a `CollabRequest` and push it upstream.
    pub async fn send_request(&self, req: &CollabRequest) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(req)
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))?;
        let len = (bytes.len() as u32).to_le_bytes();
        let mut w = self.writer.lock().await;
        w.write_all(&len).await?;
        w.write_all(&bytes).await?;
        w.flush().await?;
        Ok(())
    }
}

/// Connect to `path` and start the reader task. Returns a handle
/// callers can use to send requests + receive routed events.
///
/// The reader task drives itself off `stream.read()`; when the
/// upstream socket closes it drops all channel senders which causes
/// every SDK-side `.read()` to unblock with `NotAvailable`.
pub async fn connect(path: PathBuf) -> std::io::Result<UpstreamHandle> {
    let stream = UnixStream::connect(&path).await?;
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));

    let (op_tx, op_rx) = mpsc::unbounded_channel();
    let (context_tx, context_rx) = mpsc::unbounded_channel();
    let (scrollback_tx, scrollback_rx) = mpsc::unbounded_channel();
    let presence = DropOldest::<PresenceEnvelope>::new(PRESENCE_CAPACITY);
    let presence_task = Arc::clone(&presence);

    tokio::task::spawn_local(async move {
        // Reader loop: length-prefixed serde-JSON frames. On any
        // error (EOF, malformed frame) we exit and let the senders
        // drop, which surfaces as NotAvailable to every capnp
        // subscriber.
        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf).await {
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(error = %e, "upstream closed");
                    break;
                }
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            // Guard against absurd frame sizes so a corrupt reader
            // can't OOM the bridge. 32 MB is comfortably above any
            // sane CRDT snapshot but bounded.
            if len > 32 * 1024 * 1024 {
                tracing::warn!(len, "upstream frame too large — closing");
                break;
            }
            let mut body = vec![0u8; len];
            if let Err(e) = reader.read_exact(&mut body).await {
                tracing::debug!(error = %e, "upstream read body failed");
                break;
            }
            let event: CollabEvent = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "upstream frame not valid CollabEvent — skipping");
                    continue;
                }
            };
            tracing::trace!(?event, "upstream event");
            match event {
                CollabEvent::SignedCRDTIncremental { envelope, from_user_id } => {
                    let op_bytes = match postcard::to_stdvec(&*envelope) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "cannot re-encode SignedOp — dropping");
                            continue;
                        }
                    };
                    let _ = op_tx.send(OpEnvelope {
                        op_bytes,
                        from_user_id,
                        kind: OpKind::Signed,
                    });
                }
                CollabEvent::CRDTIncremental { data } => {
                    // Unsigned frames ride the op stream so an SDK app
                    // that hasn't migrated to signed ops keeps seeing
                    // peer changes — tagged, so the reader applies a
                    // delta as a delta. The `from_user_id` is unknown
                    // at this hop; the shell broker didn't tag it, so
                    // we ship the conventional empty attribution.
                    let _ = op_tx.send(OpEnvelope {
                        op_bytes: data,
                        from_user_id: String::new(),
                        kind: OpKind::UnsignedIncremental,
                    });
                }
                CollabEvent::CRDTUpdate { data } => {
                    // A whole document, not a delta. Merging it is the
                    // only way a late arrival adopts what is already
                    // there, and applying it as a delta does nothing.
                    let _ = op_tx.send(OpEnvelope {
                        op_bytes: data,
                        from_user_id: String::new(),
                        kind: OpKind::UnsignedSnapshot,
                    });
                }
                CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                    presence_task.push(PresenceEnvelope::Cursor { user_id, x, y });
                }
                CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
                    let key_bytes = match postcard::to_stdvec(&key) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "cannot postcard-encode PresenceKey — dropping");
                            continue;
                        }
                    };
                    presence_task.push(PresenceEnvelope::Update {
                        user_id,
                        key_bytes,
                        payload,
                    });
                }
                CollabEvent::Context { collaborators, permissions } => {
                    let _ = context_tx.send(ContextSnapshot { collaborators, permissions });
                }
                CollabEvent::ScrollbackBatch { artifact, ops, exhausted } => {
                    let _ = scrollback_tx.send(ScrollbackReply {
                        artifact,
                        ops,
                        exhausted,
                    });
                }
            }
        }
        tracing::debug!("upstream reader task exiting");
    });

    Ok(UpstreamHandle {
        writer,
        op_rx: Arc::new(tokio::sync::Mutex::new(op_rx)),
        presence,
        context_rx: Arc::new(tokio::sync::Mutex::new(context_rx)),
        scrollback_rx: Arc::new(tokio::sync::Mutex::new(scrollback_rx)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::task::LocalSet;

    #[test]
    fn drop_oldest_drops_when_full() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let q = DropOldest::<u32>::new(3);
            q.push(1);
            q.push(2);
            q.push(3);
            q.push(4);
            q.push(5);
            assert_eq!(q.dropped(), 2);
            assert_eq!(q.len(), 3);
            assert_eq!(q.pop().await, 3);
            assert_eq!(q.pop().await, 4);
            assert_eq!(q.pop().await, 5);
        });
    }

    #[test]
    fn drop_oldest_wakes_pending_pop() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let q = DropOldest::<u32>::new(4);
            let q2 = q.clone();
            let handle = tokio::task::spawn_local(async move { q2.pop().await });
            // Give the pop future a chance to register interest.
            tokio::task::yield_now().await;
            q.push(99);
            let v = handle.await.unwrap();
            assert_eq!(v, 99);
        });
    }
}
