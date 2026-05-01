# End-to-End Peer Sync — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two `quick-desktop` instances on different hosts that have
both joined the same workspace see each other's edits in real time:
shared CRDT document changes (Automerge), live presence (cursors,
focus), and — for foreign Wayland clients — composited surface frames
+ input replay (RDP-style).

**Architecture:** libp2p stack with gossipsub for the workspace topic
+ direct streams for high-bandwidth surface frames. Each workspace is
its own gossipsub topic `/nirios/ws/<id>/v1`. Discovery via mDNS on
LAN; manual peer-ID exchange for cross-network. The existing
`quick-ui::collaboration` Unix-socket protocol becomes the
in-process channel between the toolkit and the shell's network layer.

**Tech Stack:** `libp2p` (already in deps via `quick-shell`), `gossipsub`,
`mdns`, `noise` for transport encryption, `automerge` for the doc
sync, `tokio` for the network runtime.

---

## Spec coverage

This plan implements §5 + §6 of
`specs/2026-05-02-nirios-foundation-design.md`. It depends on the
compositor plan phases 0–2 (compositor must be a real compositor for
foreign-surface streaming to work, but the CRDT-level sync of native
widgets has no compositor dependency — phase 1 of THIS plan ships
without the compositor work).

## Out of scope

- Discovery beyond mDNS + manual peer-ID — relay servers + NAT
  traversal are a follow-up.
- Audio sharing for collaborative video calls — separate spec.
- Federated identity (who is "Alice" really?) — for v1 each peer is
  identified by an opaque libp2p peer-ID + a self-asserted display name.
- Conflict / divergence UI — separate spec.
- Foreign-surface input replay across NAT — local-LAN only for v1.

## Definition of done

- Two `quick-desktop` instances on the same LAN auto-discover each
  other via mDNS.
- The owner of a workspace can issue an invite that produces a
  serialisable token; the peer pastes it into a `/connect` palette
  command and joins.
- After joining: both peers see each other's cursor moves; both peers
  see Notes-app text edits in real time; layout changes (e.g. opening
  the palette) sync.
- For foreign Wayland clients shared by the owner: peers see the
  composited frames at ≥30 fps.
- Disconnect / reconnect: each peer resyncs cleanly using Automerge's
  incremental change protocol.
- Tests: ≥3 new integration tests covering the loopback path
  (two in-process libp2p nodes); 1 manual-test recipe for cross-host.

---

## Phase 0 — Branch + dependencies

### Task 0.1: libp2p deps

**Files:** modify `quick-shell/Cargo.toml`

- [ ] **Step 1:**

   ```toml
   [dependencies]
   libp2p = { version = "0.55", features = [
       "gossipsub", "mdns", "noise", "tcp", "yamux", "tokio",
       "kad",   # for future relay; off by default behind a feature
   ] }
   tokio = { version = "1", features = ["rt-multi-thread", "net", "macros", "sync"] }
   futures = "0.3"
   bincode = "1.3"
   ```

- [ ] **Step 2:** verify build.
- [ ] **Step 3:** commit; branch off `nirios-shell`.

---

## Phase 1 — Loopback CRDT sync (no network)

### Task 1.1: `NeighbourMsg` enum + serde

**Files:** new `quick-shell/src/network/protocol.rs`

- [ ] **Step 1:**

```rust
#[derive(Serialize, Deserialize, Debug)]
pub enum NeighbourMsg {
    SyncReq  { since: Vec<u8> },           // automerge change-hash blob
    SyncResp { changes: Vec<Vec<u8>> },    // raw automerge change bytes
    PresenceMoved { peer: PeerId, key: PresenceKey, point: (f64, f64), ts: u64 },
    PresenceCleared { peer: PeerId, key: PresenceKey },
    SurfaceFrame { surface: SurfaceId, encoded_h264: Vec<u8>, ts: u64 },
    RemoteInput  { peer: PeerId, surface: SurfaceId, event: SerializedInputEvent },
    PermissionRequest { peer: PeerId, capability: Capability },
    PermissionGrant   { peer: PeerId, capability: Capability },
}
```

- [ ] **Step 2:** A round-trip serde test for every variant.
- [ ] **Step 3:** Commit.

### Task 1.2: In-process `Hub` ferrying messages between two doc replicas

**Files:** new `quick-shell/src/network/hub.rs`

For testability before the network: an in-process `Hub` that owns
a `Vec<TopicSubscriber>` and broadcasts `NeighbourMsg`s among them.
Two `WorkspaceTree` replicas can attach as subscribers and exchange
SyncReq/SyncResp until convergent.

- [ ] **Step 1:** Define `Hub` + `TopicSubscriber` trait.
- [ ] **Step 2:** Test: two `WorkspaceTree`s, peer A renames a workspace,
   peer B receives the change after one Hub tick.
- [ ] **Step 3:** Commit.

### Task 1.3: Wire automerge ChangeHash + apply path

**Files:** modify `quick-shell/src/shell/state.rs` (already
serializes via serde_json; this task adds an `apply_changes`
counterpart).

- [ ] **Step 1:**

```rust
impl WorkspaceTree {
    pub fn export_changes_since(&self, since: &[ChangeHash]) -> Vec<Vec<u8>>;
    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), AutomergeError>;
    pub fn current_heads(&self) -> Vec<ChangeHash>;
}
```

(Relies on `WorkspaceTree` being backed by an `automerge::AutoCommit`
internally. Currently it serializes via serde_json — read state.rs
to confirm; if it's pure-serde-not-automerge, this phase needs a
larger refactor to back the doc with `automerge` directly. **Critical
read** before starting phase 1 task 1.3.)

- [ ] **Step 2:** Tests pinning round-trip.
- [ ] **Step 3:** Commit.

### Task 1.4: Hub × WorkspaceTree integration

- [ ] **Step 1:** A `WorkspaceTreeReplica` struct wraps a `WorkspaceTree`
   + a Hub subscriber. On `Hub::recv` it dispatches:
   - `SyncReq` → reply with `SyncResp { changes: tree.export_changes_since(since) }`
   - `SyncResp` → `tree.apply_changes(&changes)`
- [ ] **Step 2:** Two-replica convergence test. After a few ticks both
   trees report identical `current_heads()`.
- [ ] **Step 3:** Commit.

---

## Phase 2 — libp2p transport

### Task 2.1: `NetworkBehaviour` for nirios

**Files:** new `quick-shell/src/network/behaviour.rs`

- [ ] **Step 1:**

```rust
#[derive(NetworkBehaviour)]
pub struct NiriosBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    // pub kad: kad::Behaviour<…>,  // feature-gated
}
```

- [ ] **Step 2:** Construction with sensible defaults: gossipsub heart
   beat 1 s, mesh size 8.
- [ ] **Step 3:** Identity: ed25519 keypair stored at
   `/var/lib/quick-fs/peer-key`; auto-created if missing.
- [ ] **Step 4:** Smoke tests building the behaviour.
- [ ] **Step 5:** Commit.

### Task 2.2: Network actor

**Files:** new `quick-shell/src/network/actor.rs`

A tokio task owning the `Swarm<NiriosBehaviour>`. Receives commands
from the shell over an `mpsc::Sender<NetCmd>`, emits events over an
`mpsc::Sender<NetEvent>`.

- [ ] **Step 1: Cmd / Event types**

```rust
pub enum NetCmd {
    JoinWorkspace { ws: WorkspaceId, on_event: mpsc::Sender<NetEvent> },
    LeaveWorkspace { ws: WorkspaceId },
    Publish { ws: WorkspaceId, msg: NeighbourMsg },
    Connect { addr: Multiaddr },
}
pub enum NetEvent {
    PeerJoined { ws: WorkspaceId, peer: PeerId },
    PeerLeft { ws: WorkspaceId, peer: PeerId },
    Msg { ws: WorkspaceId, peer: PeerId, msg: NeighbourMsg },
}
```

- [ ] **Step 2: Actor main loop:**
   - poll the swarm
   - dispatch incoming gossipsub messages → `NetEvent::Msg`
   - on `NetCmd::Publish` → publish to the workspace topic
   - mDNS peer discovery → auto-dial + add to mesh

- [ ] **Step 3: Loopback test — two actors in one process via TCP
   loopback exchange a Publish.**

- [ ] **Step 4: Commit.**

---

## Phase 3 — Shell integration

### Task 3.1: Shell-side network handle

**Files:** new `quick-shell/src/shell/network.rs`,
modify `quick-shell/src/main.rs`

- [ ] **Step 1: Spawn the Network actor at startup.**
- [ ] **Step 2: Per active workspace, send `JoinWorkspace`.**
- [ ] **Step 3: Wire `WorkspaceTreeReplica`'s subscribe path to the
   actor's `NetEvent` channel** — translate `NetEvent::Msg` →
   `Hub::recv`.
- [ ] **Step 4: Periodic per-workspace `SyncReq` heartbeat (every 2 s)
   so reconnects converge.**

### Task 3.2: Cursor presence pump

The toolkit already throttles cursor moves to ~30 Hz and exposes them
via `app.broadcast_pointer_presence` (or similar). Pipe those into
`NetCmd::Publish { msg: PresenceMoved { … } }`.

- [ ] **Step 1: Read** `quick-ui/src/app.rs::set_presence_broadcast_scope`
   and the existing collaboration plumbing.
- [ ] **Step 2: Wire shell-side bridge.**
- [ ] **Step 3: Test with two in-process actors over loopback.**

### Task 3.3: `/connect` + `/invite` palette commands

**Files:** modify `quick-shell/src/shell/quick_settings.rs` (or wherever
palette actions live); search the codebase for existing palette command
registration.

- [ ] **Step 1: `Invite` action** — generates a serialisable token
   `(workspace_id, owner_peer_id, owner_addr, expiry, signature)`.
   Token is base64'd and copied to clipboard.
- [ ] **Step 2: `Connect` action** — prompts for a token, validates
   the signature against the embedded peer-ID, dials the peer-addr,
   joins the workspace.
- [ ] **Step 3: Tests for token round-trip.**

---

## Phase 4 — Foreign-surface streaming (depends on compositor plan)

### Task 4.1: Encoder

**Files:** new `quick-shell/src/network/encoder.rs`

- [ ] **Step 1: Pick an encoding library.** `openh264` (binding to
   Cisco's lib) is the simplest; or use `vaapi` via `ffmpeg-next` if
   we want hardware. For v1: openh264 software path; document the
   intent to swap later.
- [ ] **Step 2: API**
   ```rust
   pub struct H264Encoder { /* … */ }
   impl H264Encoder {
       pub fn new(width: u32, height: u32, fps: u32) -> Self;
       pub fn encode(&mut self, rgba: &[u8]) -> Vec<u8>;  // NALU bytes
   }
   ```
- [ ] **Step 3: Tests with a synthesised gradient frame round-trips
   through encode → decode → check PSNR ≥35 dB.**

### Task 4.2: Owner-side per-shared-surface task

When the owner shares a foreign client surface with a peer:

- [ ] **Step 1: Damage-aware loop:** every commit triggers an encode,
   modulo a 33 ms minimum interval (30 fps cap).
- [ ] **Step 2: Publish `SurfaceFrame { surface, encoded, ts }`.**

### Task 4.3: Peer-side decode

- [ ] **Step 1: New `peer_view` widget that owns an `H264Decoder`** and
   draws decoded frames into a tab's content rect.
- [ ] **Step 2: Tests with a recorded encoded stream.**

### Task 4.4: Input replay

- [ ] **Step 1:** Peer's pointer / keyboard events on a `peer_view` get
   serialised as `RemoteInput { peer, surface, event }`.
- [ ] **Step 2:** Owner's compositor injects them into the foreign client
   via smithay's `Seat::input` API.
- [ ] **Step 3:** Owner can flip a per-peer `read_only` flag (default
   false) to drop incoming input events.

---

## Phase 5 — Manual-test recipe + CI integration

### Task 5.1: Manual two-host script

**Files:** new `scripts/two-host-sync-test.sh`

- [ ] **Step 1:** Documents the recipe: launch two
   `quick-desktop --mode hosted` instances on two terminals. Owner
   issues `/invite` → copies token → user pastes into peer's `/connect`.
   Both should converge.
- [ ] **Step 2:** Take screenshots of each side via the existing
   screenshot tooling. Inspect to verify both render the same state.

### Task 5.2: CI loopback test

**Files:** new `quick-shell/tests/peer_sync_test.rs`

- [ ] **Step 1: A test that:**
   1. Spawns two `Network` actors over TCP loopback.
   2. Each actor joins a workspace.
   3. Actor A publishes a `SyncResp` carrying a fresh workspace-rename
      change.
   4. Actor B receives, applies, and asserts its `WorkspaceTree`'s
      first workspace name matches.
- [ ] **Step 2: A test for cursor presence:** A publishes `PresenceMoved`,
   B's `PresenceCtx` shows the remote cursor.
- [ ] **Step 3: A test for token round-trip + signature validation.**
- [ ] **Step 4: Run** `cargo test --test peer_sync_test`. Pass.

---

## Phase 6 — Disconnect / reconnect

### Task 6.1: Heartbeat + presence cleanup

- [ ] **Step 1: Owner publishes a heartbeat per joined workspace every 5 s.**
- [ ] **Step 2: Peer that hasn't seen a heartbeat in 15 s → eject from
   `PresenceCtx`; replace with a "disconnected" placeholder.**
- [ ] **Step 3: Tests.**

### Task 6.2: Sync after reconnect

- [ ] **Step 1: On `NetEvent::PeerJoined`** (re-join), publish a fresh
   `SyncReq { since: tree.current_heads() }` to that peer. Peer
   responds with the missing changes.
- [ ] **Step 2: Tests demonstrating two replicas drift while
   "disconnected" then converge after reconnect.**

---

## Risk register

| Risk | Mitigation |
|---|---|
| WorkspaceTree currently uses serde_json, not Automerge | Phase 1 task 1.3 has a "critical read" gate. If the doc isn't already Automerge-backed, this expands into a separate prerequisite plan. |
| libp2p mDNS doesn't work in Docker / CI | LAN auto-discovery is best-effort; loopback TCP is the CI path. |
| openh264's licensing (BSD-but-with-Cisco-patent-grant) | Document; users on patent-paranoid distros use libx264. |
| Buffer streaming bandwidth > 80 Mbps | Document as a known limitation; add bitrate clamp + adaptive resolution as follow-up. |
| Token forgery if signature check is sloppy | Use libp2p's existing signature primitives; tests that verify rejection of tampered tokens. |

## Success metrics

- Two laptops on the same Wi-Fi LAN find each other via mDNS within
  3 s of launch.
- A workspace rename on host A appears on host B within 1 s.
- A cursor moved on host A appears as a remote cursor on host B at
  ≥30 Hz with sub-100 ms perceived latency.
- An owner sharing a `firefox` surface streams 30 fps at 1080p,
  consuming <60 Mbps.
- After a 30 s network outage, both sides reconverge automatically.

