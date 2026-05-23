//! Shell-side bridge into the peer-sync [`Actor`]. Owns the
//! [`WorkspaceTreeReplica`], drives the `tokio` runtime that hosts the
//! actor, and exposes a small handle the shell calls into to drain
//! incoming `NetEvent`s and forward replica replies back out as
//! `NetCmd::Publish`s.
//!
//! v1 wires the actor into the shell's startup path behind an opt-in
//! flag (`--enable-peer-sync`). Tasks 3.2 + 3.3 add cursor-presence
//! pumping and palette commands on top.
//!
//! Plan reference: `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md`,
//! Phase 3 Task 3.1.

use std::sync::{Arc, Mutex};

use libp2p::SwarmBuilder;
use tokio::sync::mpsc;

use crate::network::actor::{Actor, NetCmd, NetEvent};
use crate::network::behaviour::{NiriosBehaviour, load_or_create_keypair};
use crate::network::hub::TopicSubscriber;
use crate::network::protocol::NeighbourMsg;
use crate::network::replica::WorkspaceTreeReplica;
use crate::shell::state::WorkspaceTreeHandle;
#[cfg(test)]
use crate::shell::state::WorkspaceTree;

/// Default workspace topic id that v1 publishes/subscribes on. Per-
/// workspace routing is a Phase 3 follow-up — every shell currently
/// shares the same gossipsub topic so a v1 LAN demo "just works"
/// without per-workspace negotiation.
pub const DEFAULT_WS_TOPIC: u64 = 0;

/// Default on-disk path the shell uses for its persistent libp2p
/// keypair. Lives alongside the FUSE driver's state so a single bind-
/// mount covers both.
pub const DEFAULT_KEYPAIR_PATH: &str = "/var/lib/qos-fs/peer-key";

/// Shell-side handle for the peer-sync subsystem.
///
/// Owns the tokio runtime that hosts the [`Actor`], the per-workspace
/// [`WorkspaceTreeReplica`], and the actor↔replica event channel. The
/// runtime is dropped (and the actor task awaited) when this struct is
/// dropped.
pub struct ShellNetwork {
    /// Send commands into the actor (e.g. publish a `NeighbourMsg`).
    pub cmd_tx: mpsc::Sender<NetCmd>,
    /// Shared replica that owns the tree-side reactions to
    /// `NetEvent::Msg`. Wrapped in `Arc<Mutex<_>>` so future tasks
    /// (cursor presence, palette commands) can hand a clone to the
    /// shell's per-frame update loop.
    pub replica: Arc<Mutex<WorkspaceTreeReplica>>,
    /// Sender side of the actor→replica event channel. Cloned and
    /// passed into the actor via `NetCmd::JoinWorkspace`.
    pub event_tx: mpsc::Sender<NetEvent>,
    /// Receiver side of the actor→replica event channel. Drained on
    /// each [`Self::tick`]. Wrapped in a `Mutex` so `tick` can take
    /// `&self` — cursor-presence pumping shares the handle behind
    /// `Arc<ShellNetwork>` between the broadcast sink (writes) and
    /// the frame-tick (reads).
    pub event_rx: Mutex<mpsc::Receiver<NetEvent>>,
    /// Tokio runtime that hosts the actor. `Option` so we can `take`
    /// it on drop and shut the runtime down cleanly without blocking
    /// the test thread.
    runtime: Option<tokio::runtime::Runtime>,
    /// Pre-computed hash of our local libp2p `PeerId`, used as the
    /// `peer` field on every outbound `NeighbourMsg::PresenceMoved`.
    /// Cached once at spawn so per-pointer-move publishes don't redo
    /// the hash work for every cursor delta.
    local_peer_hash: u64,
    /// Buffer of inbound presence updates pushed by [`Self::tick`] when
    /// a `NeighbourMsg::PresenceMoved` arrives. The shell's frame-tick
    /// hook drains this into `PresenceCtx::remote_cursors` once per
    /// wakeup so the overlay paints remote peers' cursors.
    inbound_presence: Arc<Mutex<Vec<InboundPresence>>>,
}

/// One received cursor-presence update, queued by [`ShellNetwork::tick`]
/// for the shell-side frame drainer to merge into `PresenceCtx`.
#[derive(Debug, Clone)]
pub struct InboundPresence {
    /// Hashed libp2p peer id of the publisher, mirrored from the
    /// originating `NeighbourMsg::PresenceMoved::peer` field. Mapped
    /// to a stable string id by the drainer before being stored on
    /// the `PresenceCtx::remote_cursors` row.
    pub peer: u64,
    /// Presence channel key (workspace, tab, widget path, …) — encoded
    /// JSON so the wire format stays opaque to the bridge. v1 doesn't
    /// inspect the key for routing; the chrome's drainer just attaches
    /// it to the `RemoteCursor` row by-id.
    pub key: String,
    /// Pointer position in the publisher's logical-pixel scene-space.
    pub point: (f64, f64),
    /// Wall-clock millisecond timestamp from the publisher; preserved
    /// so later eviction logic can age out stale rows.
    pub ts: u64,
}

impl ShellNetwork {
    /// Build a tokio runtime, spawn the libp2p [`Actor`], wrap the
    /// shared [`WorkspaceTree`] in a [`WorkspaceTreeReplica`], and
    /// return a handle the shell can use to drive sync.
    ///
    /// `keypair_path` defaults to [`DEFAULT_KEYPAIR_PATH`]; tests pass
    /// a tempdir path. The keypair is loaded if it already exists,
    /// otherwise a fresh ed25519 key is generated and persisted —
    /// matching [`load_or_create_keypair`]'s contract.
    pub fn spawn(
        tree: WorkspaceTreeHandle,
        keypair_path: &std::path::Path,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let _enter = runtime.enter();

        // Build the libp2p swarm. Mirrors the actor's existing
        // `build_swarm` test helper but uses the persistent keypair so
        // the shell's `PeerId` is stable across restarts.
        let keypair = load_or_create_keypair(keypair_path)?;
        // Cache the hashed peer id once so per-pointer-move
        // `publish_presence` doesn't redo the hash every frame.
        // Mirrors the same hash function `tick`'s receive path uses,
        // so a peer's own publishes hash to the same `peer` u64 on
        // both sides of the wire.
        let local_peer_hash = peer_id_to_u64(keypair.public().to_peer_id());
        let swarm = SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
            .with_tcp(
                Default::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .map_err(|e| std::io::Error::other(format!("tcp: {e:?}")))?
            .with_behaviour(|kp| {
                NiriosBehaviour::new(kp).expect("behaviour construction")
            })
            .map_err(|e| std::io::Error::other(format!("behaviour wrap: {e:?}")))?
            .build();

        let (cmd_tx, _join) = Actor::spawn(swarm);

        // Replica owns the tree-side reactions to `NetEvent::Msg`.
        // Peer id `1` here is the replica-side identity used by
        // `WorkspaceTreeReplica::handle`'s `_sender` arg; the actor's
        // libp2p `PeerId` is not the same thing — it's transport-level
        // and gets hashed to `u64` per-event by `tick`.
        let replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree)));

        // Event channel: actor → replica delivery. Capacity matches
        // `Actor::spawn`'s own command-channel default so a burst of
        // `NetEvent`s during peer discovery doesn't immediately back-
        // pressure the actor's send loop.
        let (event_tx, event_rx) = mpsc::channel(64);

        Ok(Self {
            cmd_tx,
            replica,
            event_tx,
            event_rx: Mutex::new(event_rx),
            runtime: Some(runtime),
            local_peer_hash,
            inbound_presence: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Pre-computed hash of the local libp2p `PeerId`. Stamped on every
    /// outbound presence move so peers can attribute the cursor to its
    /// publisher.
    pub fn local_peer_id_hash(&self) -> u64 {
        self.local_peer_hash
    }

    /// Workspace topic the next [`Self::publish_presence`] should
    /// publish to. v1 hard-codes [`DEFAULT_WS_TOPIC`] because the actor
    /// only joins the default workspace at startup; per-workspace
    /// routing arrives alongside the workspace-id resolver in a Phase
    /// 3 follow-up.
    pub fn active_ws_for_publish(&self) -> u64 {
        DEFAULT_WS_TOPIC
    }

    /// Shared handle to the inbound-presence buffer. The shell's
    /// per-frame drainer takes a clone, then on each frame swaps the
    /// `Vec` out under the lock and writes each entry into
    /// `PresenceCtx::remote_cursors`. Cloning the `Arc` is cheap; the
    /// underlying `Mutex` only contends with the `tick` path that
    /// pushes received messages.
    pub fn inbound_presence_handle(&self) -> Arc<Mutex<Vec<InboundPresence>>> {
        Arc::clone(&self.inbound_presence)
    }

    /// Forward a local-cursor presence move to the active workspace's
    /// peers. Throttling lives upstream in the toolkit
    /// (`pointer_broadcast_allowed`); this method just packages the
    /// event into a `NetCmd::Publish`. `try_send` failures (full
    /// command channel, actor dropped) are intentionally swallowed —
    /// presence is best-effort and a momentary backlog of cursor
    /// deltas should never block the render loop.
    pub fn publish_presence(&self, key: String, point: (f64, f64)) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let msg = NeighbourMsg::PresenceMoved {
            peer: self.local_peer_id_hash(),
            key,
            point,
            ts,
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Adapter: queue a received presence move into the inbound buffer
    /// the shell's frame-tick drains into `PresenceCtx::remote_cursors`.
    /// Public so integration tests (and a future test-only handle in
    /// the loopback harness) can inject presence updates without going
    /// through gossipsub.
    pub fn handle_presence_message(
        &self,
        peer: u64,
        key: String,
        point: (f64, f64),
        ts: u64,
    ) {
        if let Ok(mut buf) = self.inbound_presence.lock() {
            buf.push(InboundPresence { peer, key, point, ts });
        }
    }

    /// Subscribe the actor to the default workspace topic, registering
    /// our `event_tx` as the per-workspace event sink. v1 uses a
    /// single topic ([`DEFAULT_WS_TOPIC`]); per-workspace routing
    /// lands in a Phase 3 follow-up. Returns whether the command was
    /// successfully queued.
    pub fn join_default_workspace(&self) -> bool {
        self.cmd_tx
            .try_send(NetCmd::JoinWorkspace {
                ws: DEFAULT_WS_TOPIC,
                on_event: self.event_tx.clone(),
            })
            .is_ok()
    }

    /// Drive a single tick: drain pending [`NetEvent`]s from the actor
    /// and forward `Msg` variants into the replica's `handle`. Each
    /// resulting outbound message is published via `cmd_tx` so peers
    /// observe the reply.
    ///
    /// Called from the shell's per-frame update path (see
    /// `compositor::hosted::run` when `--enable-peer-sync` is set), or
    /// directly from tests. Non-blocking — uses `try_recv` /
    /// `try_send` so a starved channel never stalls the render loop.
    pub fn tick(&self) {
        // Hold the receiver lock for the whole drain so a concurrent
        // frame-tick (e.g. one queued from a redraw inside the very
        // same wakeup) doesn't see half-merged events. The lock has
        // no contention in practice — only the frame-tick path holds
        // it — but keeping the scope tight is cheap and safe.
        let mut event_rx = self
            .event_rx
            .lock()
            .expect("event_rx mutex must not be poisoned");
        while let Ok(event) = event_rx.try_recv() {
            if let NetEvent::Msg { ws: _, peer, msg } = event {
                // Presence updates skip the replica entirely — they're
                // ephemeral overlay state, not CRDT state — and land
                // in the inbound buffer the shell's frame-tick drains.
                if let NeighbourMsg::PresenceMoved {
                    peer: peer_hash,
                    ref key,
                    point,
                    ts,
                } = msg
                {
                    self.handle_presence_message(peer_hash, key.clone(), point, ts);
                    continue;
                }
                let mut rep = self.replica.lock().unwrap();
                rep.handle(peer_id_to_u64(peer), &msg);
                let outgoing = rep.drain_outgoing();
                drop(rep);
                for out_msg in outgoing {
                    let _ = self.cmd_tx.try_send(NetCmd::Publish {
                        ws: DEFAULT_WS_TOPIC,
                        msg: out_msg,
                    });
                }
            }
        }
    }
}

impl Drop for ShellNetwork {
    fn drop(&mut self) {
        // Drop the runtime explicitly so the actor's tokio task gets
        // a chance to wind down before the surrounding test (or the
        // process) exits. Using `take` + drop avoids the implicit
        // "drop on panic" path that would otherwise log a noisy
        // "runtime dropped while still running tasks" warning.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Hash a libp2p `PeerId` to a `u64` for the replica's `_sender`
/// argument. Mirrors the same hash the actor uses for
/// `Heartbeat::peer`; collisions are not security-critical because
/// the authenticated `PeerId` is already carried by gossipsub itself.
fn peer_id_to_u64(peer: libp2p::PeerId) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    peer.to_bytes().hash(&mut h);
    h.finish()
}

/// Test-only constructor that wires `ShellNetwork`'s command sender to
/// a caller-owned receiver, bypassing the libp2p actor entirely. Tests
/// use this to assert that `publish_presence` (and future Phase 3
/// publishers) emit the expected `NetCmd::Publish` shapes without
/// having to spin up a real swarm. Not exposed outside `cfg(test)`.
#[cfg(test)]
fn shell_network_for_test(
    tree: WorkspaceTreeHandle,
    local_peer_hash: u64,
) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (event_tx, event_rx) = mpsc::channel(64);
    let net = ShellNetwork {
        cmd_tx,
        replica: Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree))),
        event_tx,
        event_rx: Mutex::new(event_rx),
        runtime: None,
        local_peer_hash,
        inbound_presence: Arc::new(Mutex::new(Vec::new())),
    };
    (net, cmd_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_network_spawns_with_default_keypair() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_default().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        // `cmd_tx` capacity is the actor's bounded-channel size; just
        // assert it's nonzero so we know the channel was constructed.
        assert!(net.cmd_tx.capacity() > 0);
        // Persistence side-effect: the keypair must have been written.
        assert!(key_path.exists(), "keypair file must exist after spawn");
    }

    #[test]
    fn tick_with_no_events_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_default().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        // Should not panic on empty channel; should not block.
        net.tick();
    }

    #[test]
    fn publish_presence_emits_a_publish_command() {
        // Verify that `publish_presence` packages the (key, point) pair
        // into a `NetCmd::Publish { msg: PresenceMoved }` on the actor
        // command channel. Uses the test-only constructor so we own the
        // receiver and can drain commands without spinning up libp2p.
        let tree = WorkspaceTree::bootstrap_default().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xABCDu64);
        net.publish_presence("local-cursor".into(), (100.0, 200.0));
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_presence must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::PresenceMoved { peer, key, point, ts: _ } => {
                        assert_eq!(peer, 0xABCDu64);
                        assert_eq!(key, "local-cursor");
                        assert_eq!(point, (100.0, 200.0));
                    }
                    other => panic!("expected PresenceMoved, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    #[test]
    fn handle_presence_message_buffers_for_drain() {
        // The receive-side adapter pushes to the inbound buffer that
        // the shell's frame-tick drains. Verify that the entry round-
        // trips with all four fields preserved so the overlay can
        // attribute the cursor to the right peer at the right time.
        let tree = WorkspaceTree::bootstrap_default().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let buf_handle = net.inbound_presence_handle();
        net.handle_presence_message(7u64, "ws/0".into(), (10.0, 20.0), 1_700_000_000);
        let buf = buf_handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].key, "ws/0");
        assert_eq!(buf[0].point, (10.0, 20.0));
        assert_eq!(buf[0].ts, 1_700_000_000);
    }

    #[test]
    fn active_ws_for_publish_returns_default_topic() {
        // v1: publishes go to the single default workspace topic. This
        // contract is asserted explicitly so per-workspace routing
        // (Phase 3 follow-up) gets a tripping test rather than a
        // silent behaviour change.
        let tree = WorkspaceTree::bootstrap_default().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        assert_eq!(net.active_ws_for_publish(), DEFAULT_WS_TOPIC);
    }

}
