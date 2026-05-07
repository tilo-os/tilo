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
use crate::network::replica::WorkspaceTreeReplica;
use crate::shell::state::WorkspaceTree;

/// Default workspace topic id that v1 publishes/subscribes on. Per-
/// workspace routing is a Phase 3 follow-up — every shell currently
/// shares the same gossipsub topic so a v1 LAN demo "just works"
/// without per-workspace negotiation.
pub const DEFAULT_WS_TOPIC: u64 = 0;

/// Default on-disk path the shell uses for its persistent libp2p
/// keypair. Lives alongside the FUSE driver's state so a single bind-
/// mount covers both.
pub const DEFAULT_KEYPAIR_PATH: &str = "/var/lib/quick-fs/peer-key";

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
    /// each [`Self::tick`].
    pub event_rx: mpsc::Receiver<NetEvent>,
    /// Tokio runtime that hosts the actor. `Option` so we can `take`
    /// it on drop and shut the runtime down cleanly without blocking
    /// the test thread.
    runtime: Option<tokio::runtime::Runtime>,
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
        tree: Arc<Mutex<WorkspaceTree>>,
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
            event_rx,
            runtime: Some(runtime),
        })
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
    pub fn tick(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            // Only `Msg` events drive replica state. PeerJoined /
            // PeerLeft are presence-level signals consumed by Task
            // 3.2's cursor-presence pump.
            if let NetEvent::Msg { ws: _, peer, msg } = event {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_network_spawns_with_default_keypair() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = Arc::new(Mutex::new(WorkspaceTree::bootstrap_default()));
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
        let tree = Arc::new(Mutex::new(WorkspaceTree::bootstrap_default()));
        let mut net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        // Should not panic on empty channel; should not block.
        net.tick();
    }
}
