# Collaboration & Peer Sync

This document describes the design intent of the collaboration layer: how two `quick-desktop` instances on different hosts share a workspace's CRDT state, presence, and foreign-client surfaces. The implementation is tracked in `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md` (phases 1-6 landed).

## 1. Core philosophy

- **Compositor as orchestrator.** The shell (`quick-desktop`) owns the network stack, identity, and security policy. Apps never speak directly to peers.
- **Apps as reactive clients.** `quick-ui` apps receive remote cursors and CRDT changes from the shell via a Unix-socket protocol (`CollabRequest`/`CollabEvent`, see `quick-ui/src/collaboration.rs`). They do not manage P2P connections or permission enforcement themselves.
- **Workspace-centric.** Collaboration is scoped to a workspace. All apps in a workspace share the same collaborator list, base permissions, and CRDT topic.

## 2. Transport: libp2p

The shell runs a single `Swarm<NiriosBehaviour>` per process (`quick-shell/src/network/p2p.rs`, `behaviour.rs`, `actor.rs`):

- **gossipsub** — one topic per workspace, named `/nirios/ws/<workspace_id>/v1`. Strict validation, signed messages, 1 s heartbeat, mesh size ~8.
- **mdns** — LAN auto-discovery. Peers dial discovered addresses; mesh formation is automatic.
- **noise** — transport encryption. **tcp + yamux** for the transport.
- **kad** is feature-gated for future relay/NAT-traversal work; off by default.

Identity is an ed25519 keypair stored at `/var/lib/quick-fs/peer-key`, auto-created if missing. The libp2p `PeerId` is the multihash of the protobuf-encoded public key.

The network actor (`quick-shell/src/network/actor.rs`) owns the swarm on a dedicated tokio task. It exchanges `NetCmd` (commands in) and `NetEvent` (events out) with the shell over `mpsc` channels.

## 3. Protocol: `NeighbourMsg`

The wire format is a serde-encoded enum (`quick-shell/src/network/protocol.rs`):

```rust
pub enum NeighbourMsg {
    SyncReq           { since: Vec<u8> },                 // automerge change-hashes
    SyncResp          { changes: Vec<Vec<u8>> },          // raw automerge change bytes
    PresenceMoved     { peer, key, point: (f64, f64), ts },
    PresenceCleared   { peer, key },
    SurfaceFrame      { surface, encoded_h264: Vec<u8>, ts },
    RemoteInput       { peer, surface, event },
    PermissionRequest { peer, capability },
    PermissionGrant   { peer, capability },
}
```

Round-trip tests exist for every variant. Bincode is the on-wire encoding.

## 4. CRDT sync (automerge incremental)

`WorkspaceTree` is backed by an `automerge::AutoCommit` (see `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md`). Three operations drive replication:

```rust
fn current_heads(&self) -> Vec<automerge::ChangeHash>;
fn export_changes_since(&self, since: &[ChangeHash]) -> Vec<Vec<u8>>;
fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), AutomergeError>;
```

A `WorkspaceTreeReplica` wraps the tree and a `Hub` subscriber (the in-process testing scaffold) or the live libp2p actor:

- On `NeighbourMsg::SyncReq { since }` → reply `SyncResp { changes: tree.export_changes_since(since) }`.
- On `NeighbourMsg::SyncResp { changes }` → `tree.apply_changes(&changes)`.
- The shell publishes a `SyncReq { since: tree.current_heads() }` heartbeat every 2 s per joined workspace so reconnects converge.

The replica path goes through `quick-shell/src/network/replica.rs` and `hub.rs`. The cached-materialisation design means writes go through both the public cache and the backing doc, with `assert_cache_matches_doc()` running under `debug_assertions` to catch drift.

## 5. Presence

Cursor moves and focus updates are throttled to ~30 Hz inside `quick-ui::App`, then forwarded to the shell, which publishes `PresenceMoved`/`PresenceCleared` on the workspace topic. Peers' incoming presence becomes remote cursors rendered on top of the local scene (`quick-shell/src/shell/remote_cursors.rs` + `quick-ui/src/collaboration.rs`).

Heartbeat + presence cleanup: a peer that hasn't published anything in 15 s is ejected from `PresenceCtx` and replaced with a "disconnected" placeholder. On `NetEvent::PeerJoined` (re-join), the shell publishes a fresh `SyncReq { since: current_heads() }` to that peer.

## 6. Foreign-surface streaming

When an owner shares a foreign Wayland client surface with peers (peer-sync Phase 4):

- The compositor's per-surface task encodes committed buffers to H.264 via openh264 (`quick-shell/src/network/encoder.rs`), at a 33 ms minimum interval (30 fps cap).
- Encoded NALUs ride `NeighbourMsg::SurfaceFrame` on the workspace topic.
- Peers decode in a `peer_view` widget (`quick-shell/src/shell/peer_view.rs`) with an `H264Decoder`. Decoded frames blit into the tab's content rect.
- Peer pointer/keyboard events on a `peer_view` get serialised as `RemoteInput { peer, surface, event }`. The owner's compositor replays them via `smithay::input::Seat::input`, going through real focus rules.
- The owner can flip a per-peer `read_only` flag (default false) to drop incoming input events.

Note: openh264 is the software path for v1; hardware encoding (VAAPI via ffmpeg-next) is a follow-up.

## 7. Invite tokens

Invites are minted by `make_invite_token` and validated by `validate_invite_token` in `quick-shell/src/network/invite.rs`. Wire format (base64-url, no padding, no JSON pretty-printing):

```json
{ "workspace_id":         u64,
  "owner_peer_id":        "12D3KooW…",                 // libp2p PeerId, base58
  "owner_public_key":     "<base64-url protobuf PublicKey>",
  "owner_addr":           "/ip4/192.168.1.5/tcp/4001", // libp2p multiaddr
  "expiry_unix_seconds":  u64,
  "signature":            <bytes>                       // ed25519 over canonical payload
}
```

The canonical signing payload joins fields with `|` in a fixed order: `workspace_id | owner_peer_id | owner_public_key | owner_addr | expiry_unix_seconds`. The order is pinned by `canonical_payload_is_stable` so future field additions can't silently break verification of already-issued tokens.

Validation steps (in order):

1. base64-url decode → JSON parse.
2. Reject if `expiry_unix_seconds` is in the past (before signature check, so expired-but-valid signatures can't leak validity oracles).
3. base64-url decode `owner_public_key`; parse as libp2p `PublicKey`.
4. Cross-check `owner_peer_id == PeerId::from(public_key)`.
5. Verify the signature over the canonical payload.

`owner_public_key` is embedded because libp2p's `PeerId` is a multihash with no stable accessor for reversing back into a `PublicKey`. Tests pin rejection of tampered tokens.

UI surfaces: `/invite` palette command mints a token and copies it to the clipboard; `/connect <token>` validates and dials.

## 8. Apps: how they collaborate

### Native `quick-ui` apps

The toolkit's collaboration plumbing (`quick-ui/src/collaboration.rs`) connects to the shell over a Unix-socket protocol. `CollabRequest::Sync { changes }` flows from app to shell; `CollabEvent::{RemoteCursorMoved, SyncIncremental, …}` flows back. The shell forwards `SyncIncremental` to the network actor, which publishes it.

Inside the app:

- `state.collab.cursors` is populated from `RemoteCursorMoved`.
- Local automerge changes auto-flush via the existing hook in `quick-ui::App`.
- A `permissions()` helper lets widgets check capability grants for the current workspace.
- Remote cursors render natively inside the vello scene for zero-latency feedback.

### Legacy / foreign Wayland clients

For non-collaboration-aware Wayland clients (anything bound through the per-workspace bridge socket):

- **Compositor-side cursors.** The shell draws remote cursors over the foreign-client window — the client itself sees only its single local pointer.
- **Input multiplexing.** The compositor decides whose input reaches the foreign client; default policy is "host always wins" with a per-peer `read_only` toggle for guests.
- **Environment emulation.** The bwrap profile presents the workspace's `/work` and `/cache` directories; `/home` is empty tmpfs. The app sees no path outside the sandbox.

## 9. Disconnect / reconnect

- Owner publishes a heartbeat per joined workspace every 5 s.
- Peer that hasn't seen a heartbeat in 15 s → ejected from `PresenceCtx`.
- On reconnect: `NetEvent::PeerJoined` triggers a fresh `SyncReq { since: current_heads() }`; missing automerge changes flow in via `SyncResp`.

Tests in `quick-shell/tests/peer_sync_test.rs` exercise the convergence path.

## Where to read first

- `quick-shell/src/network/{actor.rs, behaviour.rs, hub.rs, replica.rs, protocol.rs, invite.rs}` — the live wiring.
- `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md` — phase-by-phase implementation log.
- `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md` — why the cache + backing doc.
- `quick-ui/src/collaboration.rs` — the in-process channel between toolkit and shell.
