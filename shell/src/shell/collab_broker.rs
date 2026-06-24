//! Local CRDT fan-out broker for the `QUICK_COLLAB_SOCK` Unix socket.
//!
//! Every quick-ui app instantiates a [`ui::collaboration::CollaborationManager`]
//! that connects to `QUICK_COLLAB_SOCK` and exchanges length-prefixed JSON
//! frames (`u32` little-endian length + JSON body). Pre-broker, nothing in
//! production bound that socket — the protocol was only exercised by the
//! `ui/tests/real_socket_two_peer.rs` test-only relay.
//!
//! This module lifts the relay's fan-out pattern into the shell so two
//! co-located apps (e.g. two `cargo run -p terminal` instances pointed at
//! the same socket) share scrollback CRDT updates live.
//!
//! Behaviour:
//! - Bind the configured socket path (best-effort `unlink` first so a
//!   crashed prior shell doesn't lock us out).
//! - Accept any number of peer connections (unlike the test broker's
//!   hardcoded N=2).
//! - For each peer, spawn a reader thread that decodes `CollabRequest`s
//!   and translates them to the matching `CollabEvent`, then fans the
//!   event to every OTHER currently-connected peer.
//! - Drop dead peers (write failure) from the fan-out table.
//!
//! The mapping from request → event mirrors the test relay verbatim:
//! - `SyncIncremental { data }` → `CRDTIncremental { data }`
//! - `SyncCRDT { data }` → `CRDTUpdate { data }`
//! - `Presence { key, payload }` → `RemotePresenceUpdate { user_id:
//!   "peer-<src_id>", key, payload }`
//! - `Hello { .. }` → swallowed (no fan-out; the hello is a no-op handshake)

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ui::collaboration::{CollabEvent, CollabRequest};

use crate::shell::network::ShellNetwork;

/// Per-peer entry in the broker's fan-out table.
struct Peer {
    /// Stable id assigned at accept time. Threaded into the synthetic
    /// `user_id = "peer-<id>"` on presence updates.
    id: u64,
    /// Write side of the connected stream. The read side is owned by
    /// the per-peer reader thread.
    writer: Mutex<UnixStream>,
    /// App identity from the peer's `CollabRequest::Hello`. Used by
    /// the Phase 2 libp2p bridge to stamp outbound
    /// `NeighbourMsg::AppCrdtIncremental` payloads. `None` until the
    /// peer says hello.
    identity: Mutex<Option<PeerIdentity>>,
}

/// App identity carried on every `CollabRequest::Hello` and forwarded
/// onto libp2p as part of `NeighbourMsg::AppCrdtIncremental`.
#[derive(Clone, Debug)]
struct PeerIdentity {
    app_id: String,
    cluster_id: String,
}

/// Shared registry of connected peers. Reader threads call
/// [`PeerTable::fanout_except`] for every translated event.
struct PeerTable {
    peers: Mutex<Vec<Arc<Peer>>>,
}

impl PeerTable {
    fn new() -> Self {
        Self {
            peers: Mutex::new(Vec::new()),
        }
    }

    /// Add a peer to the table. Called once per `accept`.
    fn insert(&self, peer: Arc<Peer>) {
        if let Ok(mut guard) = self.peers.lock() {
            guard.push(peer);
        }
    }

    /// Remove a peer by id. Called when its reader thread observes EOF
    /// or when a write fails (peer is presumed dead).
    fn remove(&self, id: u64) {
        if let Ok(mut guard) = self.peers.lock() {
            guard.retain(|p| p.id != id);
        }
    }

    /// Snapshot the current peer set so the caller can write without
    /// holding the registry lock during slow IO.
    fn snapshot(&self) -> Vec<Arc<Peer>> {
        self.peers
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Serialize `event` and write it to every peer in the table whose
    /// id is not `except`. Drops any peer that errors on write.
    fn fanout_except(&self, event: &CollabEvent, except: u64) {
        let bytes = match serde_json::to_vec(event) {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut dead: Vec<u64> = Vec::new();
        for peer in self.snapshot() {
            if peer.id == except {
                continue;
            }
            if write_frame(&peer.writer, &bytes).is_err() {
                dead.push(peer.id);
            }
        }
        for id in dead {
            self.remove(id);
        }
    }
}

/// Read one length-prefixed frame. Returns `None` on EOF / broken pipe
/// — the caller treats that as "peer disconnected" and unwinds.
fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Write one length-prefixed frame. Errors propagate so the caller can
/// evict the peer from the fan-out table.
fn write_frame(writer: &Mutex<UnixStream>, bytes: &[u8]) -> std::io::Result<()> {
    let mut guard = writer
        .lock()
        .map_err(|_| std::io::Error::other("writer mutex poisoned"))?;
    let len = bytes.len() as u32;
    guard.write_all(&len.to_le_bytes())?;
    guard.write_all(bytes)?;
    Ok(())
}

/// Translate one `CollabRequest` (already deserialized off the wire)
/// to the `CollabEvent` peers should observe. Returns `None` for
/// `Hello` (which is swallowed — see module docs).
fn translate(src_id: u64, req: CollabRequest) -> Option<CollabEvent> {
    match req {
        CollabRequest::Hello { .. } => None,
        CollabRequest::SyncIncremental { data } => Some(CollabEvent::CRDTIncremental { data }),
        CollabRequest::SyncCRDT { data } => Some(CollabEvent::CRDTUpdate { data }),
        CollabRequest::Presence { key, payload } => Some(CollabEvent::RemotePresenceUpdate {
            user_id: format!("peer-{src_id}"),
            key,
            payload,
        }),
    }
}

/// Handle the read side of one connected peer. Runs until EOF / broken
/// pipe, then evicts the peer from the table so concurrent writers
/// stop targeting a dead stream.
///
/// When `peer_sync` is `Some`, every `SyncIncremental` is also
/// republished over libp2p as a `NeighbourMsg::AppCrdtIncremental`
/// so remote hosts running the same broker can fan it out to their
/// own local apps.
fn reader_loop(
    peer: Arc<Peer>,
    mut stream: UnixStream,
    table: Arc<PeerTable>,
    peer_sync: Option<Arc<ShellNetwork>>,
) {
    while let Some(frame) = read_frame(&mut stream) {
        let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
            tracing::debug!(
                "collab_broker: dropping malformed frame from peer-{} ({} bytes)",
                peer.id,
                frame.len()
            );
            continue;
        };
        // Capture the Hello identity before translating — the
        // request is consumed by `translate`, and the bridge needs
        // the (app_id, cluster_id) pair to stamp every
        // `AppCrdtIncremental` it republishes.
        if let CollabRequest::Hello { ref app_id, cluster_id } = req {
            if let Ok(mut guard) = peer.identity.lock() {
                *guard = Some(PeerIdentity {
                    app_id: app_id.clone(),
                    cluster_id: cluster_id.to_string(),
                });
            }
        }
        // Phase 2 outbound bridge: send the raw incremental over
        // libp2p before we fan it to local peers. Cloning the data
        // here is unavoidable because we still need the original
        // payload for the local `translate` path.
        if let (Some(net), CollabRequest::SyncIncremental { data }) =
            (peer_sync.as_ref(), &req)
        {
            let identity = peer
                .identity
                .lock()
                .ok()
                .and_then(|g| g.clone());
            let (app_id, cluster_id) = identity
                .map(|i| (i.app_id, i.cluster_id))
                .unwrap_or_else(|| (String::new(), String::new()));
            net.publish_app_crdt_incremental(app_id, cluster_id, data.clone());
        }
        if let Some(event) = translate(peer.id, req) {
            table.fanout_except(&event, peer.id);
        }
    }
    tracing::debug!("collab_broker: peer-{} disconnected", peer.id);
    table.remove(peer.id);
}

/// Public handle returned by [`spawn`]. Owns the listener thread's join
/// handle so the broker doesn't get torn down mid-session — drop this
/// (e.g. at shell shutdown) to start the cleanup chain. The socket
/// file is best-effort-removed on `Drop` too, so a crashed shell can
/// be restarted without a stale-inode collision.
pub struct BrokerHandle {
    /// Path the listener is bound to. Held for the `Drop` `unlink`.
    socket_path: PathBuf,
    /// Listener thread handle. Wrapped in `Option` so `Drop` can
    /// `take()` and `join()` it without violating the moved-out rule.
    listener_thread: Option<JoinHandle<()>>,
    /// Phase 2 inbound-drain thread handle. Spawned only when the
    /// broker has a `ShellNetwork` to pump.
    drain_thread: Option<JoinHandle<()>>,
    /// Drain-thread quit flag. Drop sets it; the drainer exits at
    /// its next wake.
    drain_quit: Arc<AtomicBool>,
    /// Shared peer table — the reader threads use it; the broker keeps
    /// a reference so external callers (Phase 2: libp2p bridge) can
    /// fan a remote event in to every locally-connected peer.
    #[allow(dead_code)]
    peers: Arc<PeerTable>,
}

impl BrokerHandle {
    /// Snapshot the number of currently-connected peers. Used by tests
    /// to wait for the listener to register everyone before sending.
    pub fn peer_count(&self) -> usize {
        self.peers
            .peers
            .lock()
            .map(|g| g.len())
            .unwrap_or(0)
    }

    /// Fan a `CollabEvent` to every connected peer. The optional
    /// `except` skips a single peer id (used by the libp2p bridge: a
    /// remote update should reach every local peer, but a locally-
    /// originated update has already been delivered to the broker's
    /// other peers via the per-reader fan-out and only needs to go
    /// onto the wire).
    ///
    /// Phase 2 entry point; harmless to call in Phase 1 unit tests
    /// that want to simulate an "inbound" update without setting up
    /// a real libp2p mesh.
    pub fn fanout_event(&self, event: &CollabEvent, except: Option<u64>) {
        self.peers.fanout_except(event, except.unwrap_or(u64::MAX));
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        // Signal the inbound-drain thread to exit at its next wake.
        self.drain_quit.store(true, Ordering::Relaxed);
        // Removing the socket file is best-effort — if the listener
        // thread already cleaned up (or another shell instance rebound
        // the path), don't surface a spurious error.
        let _ = std::fs::remove_file(&self.socket_path);
        // We can't reliably join the listener thread here because it's
        // blocked on `listener.accept()`; the socket-file removal alone
        // doesn't unblock `accept`. Detach the join handle — the
        // process-exit path will reap it. Tests that need a clean
        // teardown drop their connections first; reader threads
        // observe EOF and exit on their own.
        let _ = self.listener_thread.take();
        // Best-effort join the drain thread — it polls its quit flag
        // on a 100ms cadence so a clean shutdown is bounded.
        if let Some(t) = self.drain_thread.take() {
            let _ = t.join();
        }
    }
}

/// Bind `socket_path` and detach the listener thread.
///
/// Returns the [`BrokerHandle`] — keep it alive for the lifetime of
/// the session so the broker doesn't get dropped (and its socket
/// `unlink`ed) out from under live apps.
///
/// On bind failure (path already locked by another process, no write
/// permission, …) returns the underlying `io::Error` so the caller
/// can log a `tracing::warn!` and continue without collab — the shell
/// must keep working even without the broker.
pub fn spawn(socket_path: PathBuf) -> std::io::Result<BrokerHandle> {
    spawn_with_peer_sync(socket_path, None)
}

/// Variant of [`spawn`] that wires the Phase 2 libp2p bridge.
///
/// When `peer_sync` is `Some`, every local `SyncIncremental` is
/// republished as a `NeighbourMsg::AppCrdtIncremental` and remote
/// `AppCrdtIncremental`s drained from `ShellNetwork`'s inbound buffer
/// get fanned out to every locally-connected peer as a
/// `CollabEvent::CRDTIncremental`.
///
/// Pass `None` to get the Phase 1 (local-only) behaviour.
pub fn spawn_with_peer_sync(
    socket_path: PathBuf,
    peer_sync: Option<Arc<ShellNetwork>>,
) -> std::io::Result<BrokerHandle> {
    // Best-effort unlink: a previous shell instance may have left a
    // stale inode behind. Ignore the result — if the file doesn't
    // exist (the common case) the `unlink` errors and we continue;
    // if the file exists but `bind` will still fail (e.g. wrong
    // owner), the listener creation below surfaces the real error.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;

    let table = Arc::new(PeerTable::new());
    let peer_id_counter = Arc::new(AtomicU64::new(0));

    let listener_table = Arc::clone(&table);
    let listener_peer_sync = peer_sync.clone();
    let listener_thread = std::thread::Builder::new()
        .name("qos-collab-broker-listener".into())
        .spawn(move || {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let id = peer_id_counter.fetch_add(1, Ordering::Relaxed);
                        let writer = match stream.try_clone() {
                            Ok(w) => w,
                            Err(e) => {
                                tracing::warn!(
                                    "collab_broker: try_clone failed for peer-{id}: {e}"
                                );
                                continue;
                            }
                        };
                        let peer = Arc::new(Peer {
                            id,
                            writer: Mutex::new(writer),
                            identity: Mutex::new(None),
                        });
                        listener_table.insert(Arc::clone(&peer));
                        tracing::info!(
                            "collab_broker: peer-{id} connected ({} total)",
                            listener_table.snapshot().len()
                        );
                        let reader_table = Arc::clone(&listener_table);
                        let reader_peer = Arc::clone(&peer);
                        let reader_peer_sync = listener_peer_sync.clone();
                        if let Err(e) = std::thread::Builder::new()
                            .name(format!("qos-collab-broker-peer-{id}"))
                            .spawn(move || {
                                reader_loop(reader_peer, stream, reader_table, reader_peer_sync);
                            })
                        {
                            tracing::warn!(
                                "collab_broker: failed to spawn reader for peer-{id}: {e}"
                            );
                            listener_table.remove(id);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("collab_broker: accept failed: {e}; listener exiting");
                        break;
                    }
                }
            }
        })?;

    // Phase 2 inbound-drain: when peer-sync is on, drain the
    // `ShellNetwork`'s `inbound_app_crdt` buffer every 100 ms and
    // fan each entry out to every locally-connected app as a
    // `CRDTIncremental`. 100 ms is well under typical CRDT
    // convergence cadence and keeps the drainer's CPU footprint
    // negligible.
    let drain_quit = Arc::new(AtomicBool::new(false));
    let drain_thread = peer_sync.as_ref().map(|net| {
        let inbound = net.inbound_app_crdt_handle();
        let drain_table = Arc::clone(&table);
        let quit = Arc::clone(&drain_quit);
        std::thread::Builder::new()
            .name("qos-collab-broker-drain".into())
            .spawn(move || {
                while !quit.load(Ordering::Relaxed) {
                    let drained: Vec<_> = {
                        let mut buf = match inbound.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        std::mem::take(&mut *buf)
                    };
                    for entry in drained {
                        let event = CollabEvent::CRDTIncremental { data: entry.data };
                        // Remote messages go to ALL local peers — they
                        // originated outside this host, so no peer here
                        // should be skipped.
                        drain_table.fanout_except(&event, u64::MAX);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok()
    }).flatten();

    Ok(BrokerHandle {
        socket_path,
        listener_thread: Some(listener_thread),
        drain_thread,
        drain_quit,
        peers: table,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use ui::collaboration::{CollaborationManager, PresencePayload};
    use ui::presence::PresenceKey;

    /// Unique-per-test socket path so concurrent test runs (or a
    /// resurrected stale inode) don't collide.
    fn temp_sock(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qos-collab-broker-{}-{}-{:?}.sock",
            tag,
            std::process::id(),
            std::thread::current().id(),
        ))
    }

    /// Drain a manager until at least one of each desired variant
    /// shows up or `timeout` elapses. Returns whatever it managed
    /// to collect.
    fn poll_until(mgr: &mut CollaborationManager, want: usize, timeout: Duration) -> Vec<CollabEvent> {
        let deadline = std::time::Instant::now() + timeout;
        let mut out: Vec<CollabEvent> = Vec::new();
        while std::time::Instant::now() < deadline && out.len() < want {
            out.extend(mgr.poll());
            if out.len() >= want {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        out
    }

    /// Block briefly until the broker has accepted exactly `n`
    /// connections. Replaces a wall-clock sleep with a poll on the
    /// peer table.
    fn wait_for_peers(handle: &BrokerHandle, n: usize, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if handle.peer_count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Connect a CollaborationManager to `path`, mirroring the toolkit's
    /// own `new()` constructor but using the explicit path so the test
    /// is independent of process-wide env state.
    fn connect_manager(path: &PathBuf) -> CollaborationManager {
        // SAFETY: tests set this env var serially before each call;
        // no other thread reads QUICK_COLLAB_SOCK between sets.
        unsafe {
            std::env::set_var("QUICK_COLLAB_SOCK", path);
        }
        CollaborationManager::new()
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn broker_fans_out_to_three_peers() {
        // Phase 1 acceptance test: prove N=3 fan-out, not just N=2.
        // Mirrors `ui/tests/real_socket_two_peer.rs` but exercises the
        // production module and a peer count beyond the test broker.
        let socket_path = temp_sock("fanout3");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let mut peer_b = connect_manager(&socket_path);
        let mut peer_c = connect_manager(&socket_path);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        assert_eq!(handle.peer_count(), 3, "broker should see all three peers");

        // Drain the Hello echoes. The broker swallows them, so this is
        // just a steady-state hygiene poll.
        let _ = peer_a.poll();
        let _ = peer_b.poll();
        let _ = peer_c.poll();

        // --- Assertion 1: SyncIncremental from A fans out to B and C.
        peer_a.send(CollabRequest::SyncIncremental {
            data: b"hello-world".to_vec(),
        });

        let events_b = poll_until(&mut peer_b, 1, Duration::from_millis(500));
        let events_c = poll_until(&mut peer_c, 1, Duration::from_millis(500));

        let crdt_b: Vec<&Vec<u8>> = events_b
            .iter()
            .filter_map(|e| match e {
                CollabEvent::CRDTIncremental { data } => Some(data),
                _ => None,
            })
            .collect();
        let crdt_c: Vec<&Vec<u8>> = events_c
            .iter()
            .filter_map(|e| match e {
                CollabEvent::CRDTIncremental { data } => Some(data),
                _ => None,
            })
            .collect();

        assert_eq!(
            crdt_b.len(),
            1,
            "peer B should receive exactly one CRDTIncremental, got {events_b:?}"
        );
        assert_eq!(
            crdt_c.len(),
            1,
            "peer C should receive exactly one CRDTIncremental, got {events_c:?}"
        );
        assert_eq!(crdt_b[0].as_slice(), b"hello-world");
        assert_eq!(crdt_c[0].as_slice(), b"hello-world");

        // --- Assertion 2: source peer does NOT see its own update.
        let events_a = peer_a.poll();
        assert!(
            !events_a.iter().any(|e| matches!(
                e,
                CollabEvent::CRDTIncremental { data } if data.as_slice() == b"hello-world"
            )),
            "peer A must not see its own SyncIncremental echoed back, got {events_a:?}"
        );

        // --- Assertion 3: presence updates carry the synthetic user_id.
        peer_b.send(CollabRequest::Presence {
            key: PresenceKey::Workspace { id: "42".into() },
            payload: PresencePayload::Arrived,
        });
        let events_a = poll_until(&mut peer_a, 1, Duration::from_millis(500));
        let events_c = poll_until(&mut peer_c, 1, Duration::from_millis(500));

        // peer B was the second to connect → its synthetic id is "peer-1".
        let saw_a = events_a.iter().any(|e| matches!(
            e,
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key: PresenceKey::Workspace { id },
                payload: PresencePayload::Arrived,
            } if user_id == "peer-1" && id == "42"
        ));
        let saw_c = events_c.iter().any(|e| matches!(
            e,
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key: PresenceKey::Workspace { id },
                payload: PresencePayload::Arrived,
            } if user_id == "peer-1" && id == "42"
        ));
        assert!(
            saw_a,
            "peer A should see peer-1's Workspace 42 Arrived, got {events_a:?}"
        );
        assert!(
            saw_c,
            "peer C should see peer-1's Workspace 42 Arrived, got {events_c:?}"
        );

        drop(peer_a);
        drop(peer_b);
        drop(peer_c);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn broker_drops_dead_peer_from_fanout() {
        // If a peer disconnects, subsequent fan-outs must not block on
        // the dead writer — they should reach the surviving peer.
        let socket_path = temp_sock("dropdead");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let peer_b = connect_manager(&socket_path);
        let mut peer_c = connect_manager(&socket_path);
        wait_for_peers(&handle, 3, Duration::from_millis(500));

        // Drop B; its reader thread observes EOF and removes it from
        // the table. We then assert a send from A reaches only C.
        drop(peer_b);
        // Give the reader thread a chance to notice EOF and evict.
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline && handle.peer_count() > 2 {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(handle.peer_count(), 2, "broker should evict dead peer B");

        let _ = peer_a.poll();
        let _ = peer_c.poll();
        peer_a.send(CollabRequest::SyncIncremental {
            data: b"after-drop".to_vec(),
        });
        let events_c = poll_until(&mut peer_c, 1, Duration::from_millis(500));
        let saw = events_c.iter().any(|e| matches!(
            e,
            CollabEvent::CRDTIncremental { data } if data.as_slice() == b"after-drop"
        ));
        assert!(saw, "peer C should still receive after B died, got {events_c:?}");

        drop(peer_a);
        drop(peer_c);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn broker_translates_sync_crdt_to_crdt_update() {
        // `SyncCRDT` is a full-state replacement; the broker must map
        // it to `CRDTUpdate` (not `CRDTIncremental`).
        let socket_path = temp_sock("full-state");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let mut peer_b = connect_manager(&socket_path);
        wait_for_peers(&handle, 2, Duration::from_millis(500));

        let _ = peer_a.poll();
        let _ = peer_b.poll();

        peer_a.send(CollabRequest::SyncCRDT { data: vec![0xDE, 0xAD] });
        let events_b = poll_until(&mut peer_b, 1, Duration::from_millis(500));
        let saw = events_b.iter().any(|e| matches!(
            e,
            CollabEvent::CRDTUpdate { data } if data.as_slice() == [0xDE, 0xAD]
        ));
        assert!(saw, "peer B should see CRDTUpdate, got {events_b:?}");

        drop(peer_a);
        drop(peer_b);
        drop(handle);
    }
}
