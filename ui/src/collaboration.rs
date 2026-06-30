use std::os::unix::net::UnixStream;
use std::io::{Read, Write};
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use qos_collab_types::SignedOp;

use crate::presence::PresenceKey;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello { app_id: String, cluster_id: Uuid },
    /// Legacy: unsigned full-document state. Still accepted by the
    /// broker. Apps using this path do not get role enforcement and
    /// surface a "legacy / unsigned" badge in chrome.
    SyncCRDT { data: Vec<u8> },
    /// Legacy: unsigned incremental Automerge change blob. Still
    /// routed by the broker for backward compatibility while Phase 3
    /// migration is in flight. New code should prefer
    /// [`CollabRequest::SignedSyncIncremental`].
    SyncIncremental { data: Vec<u8> },
    /// Outbound presence update — the local user's pointer moved, caret
    /// changed, selection shifted, or they arrived/left a keyed object.
    /// The server fans this out to every connected peer as a
    /// `CollabEvent::RemotePresenceUpdate`; the test relay uses the same
    /// path. Round-trips complete the loop with `apply_remote_presence`.
    Presence { key: PresenceKey, payload: PresencePayload },
    /// Phase 3: signed incremental CRDT mutation. The broker validates
    /// the envelope against the channel's role table (see
    /// [`qos_collab_types::validate_inbound_op`]) before fanning it
    /// out. Invalid envelopes are dropped and a structured log line is
    /// emitted; nothing reaches other peers.
    ///
    /// `Box`ed so the discriminant + payload stay a thin handle on the
    /// stack — `SignedOp` is large (32-byte pubkey + 64-byte sig +
    /// payload + UUIDs) and growing the enum to its inline size would
    /// bloat every `CollabRequest` value passed by value.
    SignedSyncIncremental { envelope: Box<SignedOp> },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabEvent {
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    RemoteCursorMoved {
        user_id: String,
        x: f32,
        y: f32,
    },
    /// Legacy: full unsigned document state. Apps drop into a merge of
    /// the snapshot into their local Automerge doc.
    CRDTUpdate {
        data: Vec<u8>,
    },
    /// Legacy: unsigned incremental change blob. See
    /// [`CollabRequest::SyncIncremental`].
    CRDTIncremental {
        data: Vec<u8>,
    },
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
    /// Phase 3: a validated signed envelope from another peer. The
    /// validation pipeline ran on the broker side; receivers extract
    /// `envelope.payload` and feed it into the same
    /// `automerge::apply_changes` path the legacy
    /// [`CollabEvent::CRDTIncremental`] uses.
    ///
    /// `from_user_id` mirrors the synthetic id minted by the broker for
    /// [`CollabEvent::RemotePresenceUpdate`] (`"peer-<src_id>"`) so
    /// downstream attribution code can use a single id shape.
    SignedCRDTIncremental {
        envelope: Box<SignedOp>,
        from_user_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer { x: f32, y: f32 },
    Caret { pos: usize },
    Selection { start: usize, end: usize },
    Arrived,
    Left,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
}

/// Failure modes for [`CollaborationManager::send_signed_op`].
///
/// `NoIdentity` is the common path during migration: an app calls
/// `send_signed_op` before the shell has provisioned an identity for
/// it. App code can pattern-match on this and fall back to
/// `send(CollabRequest::SyncIncremental {..})` if it wants legacy
/// behaviour during the transition.
#[derive(Debug)]
pub enum SignedSendError {
    /// No [`SigningIdentity`] has been bound via
    /// [`CollaborationManager::with_identity`] or
    /// [`CollaborationManager::set_identity`].
    NoIdentity,
    /// Canonical-bytes serialisation of the envelope failed. The
    /// underlying [`qos_collab_types::CanonicalError`] carries the
    /// postcard error message; in practice this only fires for
    /// unrepresentable values, not for healthy inputs.
    Canonical(qos_collab_types::CanonicalError),
}

impl std::fmt::Display for SignedSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignedSendError::NoIdentity => {
                f.write_str("CollaborationManager has no signing identity bound")
            }
            SignedSendError::Canonical(e) => write!(f, "canonical encoder failed: {e}"),
        }
    }
}

impl std::error::Error for SignedSendError {}

/// Identity-side handle used by [`CollaborationManager::send_signed_op`]
/// to mint signed envelopes.
///
/// The toolkit deliberately keeps the secret key wrapped in a private
/// struct (we do not re-export `ed25519_dalek::SigningKey` on the
/// `CollaborationManager`'s public surface) so app code can only sign
/// via the manager's helper, not handle the raw secret. The bound
/// attestation id ties every minted envelope back to the role grant
/// that authorises it; the broker uses that id to look up the role
/// table entry.
///
/// In Phase 3 v1 a `SigningIdentity` is constructed by the shell at
/// boot (mint a keypair, persist it under `~/.config/qos/identity/`
/// per Phase 2, label it with the system username) and handed to every
/// `CollaborationManager` it spins up via
/// [`CollaborationManager::with_identity`]. The eventual threshold
/// upgrade in Phase 8 swaps the `secret_key` field's representation
/// without changing this struct's outward API.
pub struct SigningIdentity {
    /// Local secret half — never serialised, never logged.
    pub(crate) secret_key: ed25519_dalek::SigningKey,
    /// Public half — embedded as the `author` field on every signed
    /// op.
    pub public_key: qos_collab_types::Ed25519PublicKey,
    /// The role attestation under which this identity emits ops. Set
    /// at construction time by whichever flow minted the attestation
    /// (the channel-owner's invite flow in production; the test
    /// fixture in tests). One identity may hold attestations on
    /// multiple channels; v1 pins to one per `CollaborationManager`.
    pub attestation_id: qos_collab_types::AttestationId,
    /// Monotonic per-author seq counter. Bumped on every successful
    /// `sign` so the validator's replay check (`seq > last_seen`)
    /// passes. Re-using a seq is the spec-defined replay attack and
    /// the validator rejects it.
    pub(crate) seq: u64,
}

impl SigningIdentity {
    /// Construct an identity from a fresh keypair and the attestation
    /// id that authorises it. Mostly used in tests; production code
    /// builds via the shell-side identity-loading helper that will
    /// land alongside Phase 8's threshold sharing.
    pub fn new(
        secret_key: ed25519_dalek::SigningKey,
        attestation_id: qos_collab_types::AttestationId,
    ) -> Self {
        let public_key = qos_collab_types::Ed25519PublicKey::from_bytes(
            secret_key.verifying_key().to_bytes(),
        );
        Self {
            secret_key,
            public_key,
            attestation_id,
            seq: 0,
        }
    }

    /// Build a fresh `SignedOp` envelope wrapping `payload`. Bumps the
    /// internal seq so the validator's replay check passes on the
    /// receiving side; callers do not need to track sequence numbers
    /// themselves.
    pub fn sign_op(
        &mut self,
        payload: Vec<u8>,
        artifact: qos_collab_types::ArtifactId,
        op_class: qos_collab_types::OpClass,
    ) -> Result<qos_collab_types::SignedOp, qos_collab_types::CanonicalError> {
        let timestamp = std::time::SystemTime::now();
        self.seq = self.seq.wrapping_add(1);
        let mut op = qos_collab_types::SignedOp::new_unsigned(
            payload,
            artifact,
            self.public_key,
            self.attestation_id,
            op_class,
            timestamp,
            self.seq,
        );
        qos_collab_types::sign_op(&mut op, &self.secret_key)?;
        Ok(op)
    }
}

pub struct CollaborationManager {
    stream: Option<UnixStream>,
    /// Persistent read buffer. The socket is non-blocking, so a single
    /// `poll` can see a frame split across reads (large `SyncCRDT`
    /// payloads especially) or several frames coalesced. We accumulate
    /// raw bytes here and only extract *complete* length-prefixed frames,
    /// leaving any partial tail for the next poll. Reading the body with
    /// `read_exact` directly (the old approach) dropped split frames and
    /// desynced the stream.
    rbuf: Vec<u8>,
    /// Optional Phase 3 signing identity. When `Some`, calls to
    /// [`Self::send_signed_op`] mint a [`qos_collab_types::SignedOp`]
    /// envelope and dispatch it as a
    /// [`CollabRequest::SignedSyncIncremental`]. `None` is the legacy
    /// path: apps that haven't migrated keep flowing unsigned
    /// `SyncIncremental` frames and the broker fans them out without
    /// validation.
    identity: Option<SigningIdentity>,
}

impl CollaborationManager {
    pub fn new() -> Self {
        let stream = if let Ok(path) = std::env::var("QUICK_COLLAB_SOCK") {
            UnixStream::connect(path).ok()
        } else {
            None
        };

        let mut manager = Self { stream, rbuf: Vec::new(), identity: None };

        if manager.stream.is_some() {
            let app_id = std::env::var("QUICK_APP_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
            let cluster_id = std::env::var("QUICK_CLUSTER_ID")
                .ok()
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_else(Uuid::nil);

            manager.send(CollabRequest::Hello { 
                app_id,
                cluster_id,
            });
        }

        manager
    }

    /// True when connected to a broker (a `QUICK_COLLAB_SOCK` was set and
    /// the connection succeeded). The event loop uses this to tick while
    /// collaborating so the socket is drained promptly instead of only on
    /// local input.
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Phase 3: bind a signing identity to this manager. Once set,
    /// [`Self::send_signed_op`] is the canonical outbound CRDT-mutation
    /// path; legacy unsigned `SyncIncremental` still works for apps
    /// that haven't migrated.
    ///
    /// Builder-style so the shell can chain
    /// `CollaborationManager::new().with_identity(id)` at boot. The
    /// previous identity (if any) is replaced; the manager owns its
    /// `SigningIdentity` and its seq counter outright.
    pub fn with_identity(mut self, identity: SigningIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Replace the bound signing identity in-place. Used by the shell
    /// to swap identities when the user switches the active identity
    /// in the chrome's identity picker without tearing down the
    /// manager.
    pub fn set_identity(&mut self, identity: SigningIdentity) {
        self.identity = Some(identity);
    }

    /// True when a [`SigningIdentity`] has been bound to this manager
    /// (via [`Self::with_identity`] or [`Self::set_identity`]). Apps
    /// can branch on this to decide whether to call
    /// [`Self::send_signed_op`] or fall back to the legacy unsigned
    /// path.
    pub fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// Phase 3: sign and send a CRDT mutation envelope. Returns
    /// `Err` if the manager has no identity bound, or the canonical
    /// encoder fails (which is effectively unreachable for the wire
    /// types in this crate — see [`qos_collab_types::canonical`]).
    ///
    /// The send itself is best-effort, mirroring the legacy
    /// [`Self::send`] path: a frame that doesn't reach the socket
    /// won't be retried, but the next live edit will republish the
    /// CRDT state so the loss is bounded by one round-trip.
    pub fn send_signed_op(
        &mut self,
        payload: Vec<u8>,
        artifact: qos_collab_types::ArtifactId,
        op_class: qos_collab_types::OpClass,
    ) -> Result<(), SignedSendError> {
        let identity = self
            .identity
            .as_mut()
            .ok_or(SignedSendError::NoIdentity)?;
        let envelope = identity
            .sign_op(payload, artifact, op_class)
            .map_err(SignedSendError::Canonical)?;
        self.send(CollabRequest::SignedSyncIncremental {
            envelope: Box::new(envelope),
        });
        Ok(())
    }

    pub fn send(&mut self, req: CollabRequest) {
        if let Some(ref mut s) = self.stream {
            if let Ok(json) = serde_json::to_vec(&req) {
                // Block for writes so a large frame (a full-document
                // `SyncCRDT` snapshot) is sent in full. `poll` flips the
                // socket to non-blocking for reads; the two alternate on
                // one thread, so toggling here is safe. Without this a big
                // `write_all` on a non-blocking socket returns WouldBlock
                // partway and the frame is truncated.
                let _ = s.set_nonblocking(false);
                let len = json.len() as u32;
                let _ = s.write_all(&len.to_le_bytes());
                let _ = s.write_all(&json);
            }
        }
    }

    pub fn poll(&mut self) -> Vec<CollabEvent> {
        // 1. Drain everything currently available on the socket into the
        //    persistent buffer. `read` (unlike `read_exact`) returns
        //    whatever bytes are ready without losing a partially-arrived
        //    frame.
        if let Some(ref mut s) = self.stream {
            let _ = s.set_nonblocking(true);
            let mut chunk = [0u8; 8192];
            loop {
                match s.read(&mut chunk) {
                    Ok(0) => break, // EOF
                    Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        // 2. Extract every *complete* length-prefixed frame from the
        //    buffer, leaving any partial tail for the next poll.
        let mut events = Vec::new();
        loop {
            if self.rbuf.len() < 4 {
                break;
            }
            let len = u32::from_le_bytes([
                self.rbuf[0],
                self.rbuf[1],
                self.rbuf[2],
                self.rbuf[3],
            ]) as usize;
            if self.rbuf.len() < 4 + len {
                break; // frame not fully arrived yet
            }
            let frame: Vec<u8> = self.rbuf[4..4 + len].to_vec();
            self.rbuf.drain(..4 + len);
            if let Ok(event) = serde_json::from_slice(&frame) {
                events.push(event);
            }
        }
        events
    }
}

/// Standalone N-peer fan-out broker for `QUICK_COLLAB_SOCK`.
///
/// This is the production counterpart to the test relay in
/// `ui/tests/real_socket_two_peer.rs`, generalized from a fixed two
/// peers to any number that connect over the lifetime of the process.
/// It carries no Automerge knowledge — it simply relays opaque frames:
///
///   * `Hello` registers the connection's `user_id` (from `app_id`) and
///     is then swallowed (never fanned out).
///   * `SyncIncremental` → `CRDTIncremental`, `SyncCRDT` → `CRDTUpdate`
///     are forwarded verbatim to every *other* connected peer.
///   * `Presence` → `RemotePresenceUpdate` is forwarded to every other
///     peer, attributed with the sender's `user_id`.
///
/// Late-join document convergence is handled at the app layer (a new
/// peer broadcasts `Presence::Arrived`; peers already in the room reply
/// with a full `SyncCRDT`) — the broker needs no special case for it.
pub mod broker {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::{CollabEvent, CollabRequest};

    fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        s.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        s.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
        let len = bytes.len() as u32;
        let _ = s.write_all(&len.to_le_bytes());
        let _ = s.write_all(bytes);
    }

    /// A connected peer's write side + attributed identity.
    struct Peer {
        user_id: String,
        write: UnixStream,
    }

    /// Bind `path` (removing any stale inode first) and serve forever.
    /// Convenience entry point for the `collab-broker` binary.
    pub fn serve_path<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        serve(listener);
        Ok(())
    }

    /// Serve on an already-bound listener until it stops yielding
    /// connections. Blocks the calling thread; spawns one reader thread
    /// per peer plus an accept thread.
    pub fn serve(listener: UnixListener) {
        let peers: Arc<Mutex<HashMap<usize, Peer>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (fwd_tx, fwd_rx) = mpsc::channel::<(usize, Vec<u8>)>();

        // Accept connections on a side thread so the forwarding loop can
        // own the calling thread. Each connection gets its own reader
        // thread so a silent peer can't stall accept for the others.
        let accept_peers = Arc::clone(&peers);
        thread::spawn(move || {
            let mut next_idx = 0usize;
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let idx = next_idx;
                next_idx += 1;
                let tx = fwd_tx.clone();
                let peers = Arc::clone(&accept_peers);
                thread::spawn(move || {
                    let Ok(write) = stream.try_clone() else { return };
                    // First frame establishes identity. The client sends
                    // `Hello` immediately on connect; if it doesn't, fall
                    // back to `peer-{idx}` and treat the frame as data.
                    match read_frame(&mut stream) {
                        Some(frame) => match serde_json::from_slice::<CollabRequest>(&frame) {
                            Ok(CollabRequest::Hello { app_id, .. }) => {
                                peers.lock().unwrap().insert(
                                    idx,
                                    Peer { user_id: app_id, write },
                                );
                            }
                            _ => {
                                peers.lock().unwrap().insert(
                                    idx,
                                    Peer { user_id: format!("peer-{idx}"), write },
                                );
                                let _ = tx.send((idx, frame));
                            }
                        },
                        None => return, // closed before saying anything
                    }

                    while let Some(frame) = read_frame(&mut stream) {
                        if tx.send((idx, frame)).is_err() {
                            break;
                        }
                    }
                    // Peer closed — stop writing to it.
                    peers.lock().unwrap().remove(&idx);
                });
            }
        });

        // Forwarding loop. Never returns while the accept thread holds a
        // `fwd_tx` clone (i.e. for the life of the process).
        while let Ok((src_idx, frame)) = fwd_rx.recv() {
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
                continue;
            };

            let user_id = peers
                .lock()
                .unwrap()
                .get(&src_idx)
                .map(|p| p.user_id.clone())
                .unwrap_or_else(|| format!("peer-{src_idx}"));

            let event = match req {
                CollabRequest::Hello { .. } => continue,
                CollabRequest::SyncIncremental { data } => {
                    CollabEvent::CRDTIncremental { data }
                }
                CollabRequest::SyncCRDT { data } => {
                    CollabEvent::CRDTUpdate { data }
                }
                CollabRequest::Presence { key, payload } => {
                    CollabEvent::RemotePresenceUpdate { user_id, key, payload }
                }
                CollabRequest::SignedSyncIncremental { envelope } => {
                    // The simple in-toolkit test broker is for two-peer
                    // demos and does NOT enforce roles — it just routes
                    // verbatim. Production fan-out is the
                    // `shell::shell::collab_broker` which runs every
                    // envelope through `validate_inbound_op` before
                    // forwarding. Documenting the divergence here
                    // explicitly so a reader doesn't assume the
                    // test broker is the production security boundary.
                    CollabEvent::SignedCRDTIncremental {
                        envelope,
                        from_user_id: user_id,
                    }
                }
            };

            let Ok(bytes) = serde_json::to_vec(&event) else { continue };

            let mut guard = peers.lock().unwrap();
            let recipients = guard.len().saturating_sub(1);
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                let kind = match &event {
                    CollabEvent::CRDTUpdate { data } => format!("CRDTUpdate({}B)", data.len()),
                    CollabEvent::CRDTIncremental { data } => {
                        format!("CRDTIncremental({}B)", data.len())
                    }
                    CollabEvent::SignedCRDTIncremental { envelope, from_user_id } => {
                        format!(
                            "SignedCRDTIncremental(from={from_user_id}, {}B payload)",
                            envelope.payload.len()
                        )
                    }
                    CollabEvent::RemotePresenceUpdate { payload, .. } => {
                        format!("Presence({payload:?})")
                    }
                    other => format!("{other:?}"),
                };
                eprintln!("[broker] peer{src_idx} -> {kind} -> {recipients} peer(s)");
            }
            for (idx, peer) in guard.iter_mut() {
                if *idx != src_idx {
                    write_frame(&mut peer.write, &bytes);
                }
            }
        }
    }
}
