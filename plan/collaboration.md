# Collaboration & Peer Sync

This document describes the design intent of the collaboration layer: how two `shell` instances on different hosts share a workspace's CRDT state, presence, and foreign-client surfaces. The implementation is tracked in `docs/superpowers/archive/2026-05-02-peer-sync-end-to-end.md` (phases 1-6 landed). The evolving sync/host model is specified in `docs/superpowers/specs/2026-07-04-sync-host-model-design.md`.

## 1. Core philosophy

- **Compositor as orchestrator.** The shell (`shell`) owns the network stack, identity, and security policy. Apps never speak directly to peers.
- **Apps as reactive clients.** `ui` apps receive remote cursors and CRDT changes from the shell via a Unix-socket protocol (`CollabRequest`/`CollabEvent`, see `ui/src/collaboration.rs`). They do not manage P2P connections or permission enforcement themselves.
- **Workspace-centric.** Collaboration is scoped to a workspace. All apps in a workspace share the same collaborator list, base permissions, and CRDT topic.

## 2. Transport: libp2p

The shell runs a single `Swarm<NiriosBehaviour>` per process (`shell/src/network/p2p.rs`, `behaviour.rs`, `actor.rs`):

- **gossipsub** — one topic per workspace, named `/nirios/ws/<workspace_id>/v1`. Strict validation, signed messages, 1 s heartbeat, mesh size ~8.
- **mdns** — LAN auto-discovery. Peers dial discovered addresses; mesh formation is automatic.
- **noise** — transport encryption. **tcp + yamux** for the transport.
- **kad** is feature-gated for future relay/NAT-traversal work; off by default.

Identity is the machine's one ed25519 device key at `/var/lib/qos/device/keypair.bin`, auto-created if missing and shared with the filesystem so both name the same device. The libp2p `PeerId` is the multihash of the protobuf-encoded public key.

The network actor (`shell/src/network/actor.rs`) owns the swarm on a dedicated tokio task. It exchanges `NetCmd` (commands in) and `NetEvent` (events out) with the shell over `mpsc` channels.

## 3. Protocol: `NeighbourMsg`

The wire format is a serde-encoded enum (`shell/src/network/protocol.rs`):

```rust
pub enum NeighbourMsg {
    SyncReq           { since: Vec<u8>, room: String },   // change-hashes + the room asked about
    SyncResp          { changes: Vec<Vec<u8>>, room: String }, // that room's change bytes
    PresenceMoved     { peer, key, point: (f64, f64), ts },
    PresenceCleared   { peer, key },
    SurfaceFrame      { surface, encoded_h264: Vec<u8>, ts },
    RemoteInput       { peer, surface, event },
    PermissionRequest { peer, capability },
    PermissionGrant   { peer, capability },
}
```

Round-trip tests exist for every variant. Bincode is the on-wire encoding.

## 4. CRDT sync (automerge incremental) — one document per room

Inviting someone to a workspace grants them **that room**. It does not grant them a directory of every other room on the host: their names, their room ids, their owners, and above all their capability sets, which say which app holds which grant over paths the guest cannot reach.

`WorkspaceTree::doc` cannot express that boundary. It is a single automerge document holding every workspace (see `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md`), and an automerge change graph cannot be served in part: changes form a hash chain, so a subset with another room's changes removed leaves the remaining changes with dependencies the receiver never sees, and automerge parks them unapplied. A filter over the tree document is not a smaller version of this scoping — it is a version of it that does not work, however the attribution is computed.

So the unit of replication is a **room**, and each has its own document, projected from the cache by `sync_room_doc`:

```rust
fn rooms(&self) -> Vec<RoomId>;
fn room_heads(&mut self, room: RoomId) -> Vec<ChangeHash>;
fn export_room_changes_since(&mut self, room: RoomId, since: &[ChangeHash]) -> Option<Vec<Vec<u8>>>;
fn apply_room_changes(&mut self, room: RoomId, changes: &[Vec<u8>]) -> Result<bool, AutomergeError>;
```

A room document carries that room's identity, name, owner and capabilities — the set the receiving side actually reads back. `WorkspaceTree::export_changes_since` still exists for local document forks (the history pane's restore, the screenshot driver) and is **not** peer-facing.

A `WorkspaceTreeReplica` wraps the tree and a `Hub` subscriber (the in-process testing scaffold) or the live libp2p actor:

- On `SyncReq { since, room }` naming a room this machine is in → reply `SyncResp { changes, room }`.
- On `SyncReq` naming a room this machine is **not** in → no reply at all. Nothing is exported and nothing is said about whether the room exists.
- On `SyncReq` naming **no** room — the kick the transport broadcasts on peer-join, before it knows what anyone is in — export nothing, and instead publish one `SyncReq` per room this machine is in. A blind join becomes a sync round without any layer holding a list of everybody's rooms.
- On `SyncResp { changes, room }` → `apply_room_changes`, which refuses a room this machine is not in *before* it stores anything, and refuses a payload whose own room field disagrees with the envelope.

Changes that belong to no single room never cross. The active-workspace pointer is a fact about which of this machine's rooms this machine is looking at and has no meaning on a peer; a whole-tree cache flush decomposes into per-room projections rather than travelling as one unattributable blob.

### Write authority on a room's governance

A room document's **owner** and **capability list** are its governance — `may_administer` reads the first, `decide` reads the second, and the FUSE mount enforces an export of the second. Automerge merges a map key by last-writer-wins and has no per-key ACL, so two machines both projecting their cache into one room document do not disagree about the owner, they toss a coin for it by actor id. `grant_as`/`revoke_as` gate the local path; without the same authority on the replication path a guest simply uses that instead.

So each machine records, per room, who may write its governance (`WorkspaceTree::room_authority`):

- **`Local`** — a room minted here. No peer may write its governance.
- **`Peer(p)`** — the owner an invite named. `accept_invite_into` pins it from the validated token, and the room id and owner peer id are both *inside* the ed25519 signature (§7), so a sender cannot name itself the authority over a room it does not own. Governance is taken from `p` and from no other peer, including other guests of the same room.
- **`Unpinned`** — a room joined without an invite naming an administrator. The first peer to supply governance is pinned; the second to try is refused.

`apply_room_changes` compares the governance the batch would produce against the governance it holds. Unchanged, it merges as before — an honest peer echoing what it learned is not changing it. Changed, it is accepted only from the room's authority; otherwise **nothing about the batch is stored**. Refusing wholesale rather than stripping keys is deliberate: a partial merge advances the receiver's heads past changes it did not accept, so the legitimate content becomes unreachable, and the refused ops would stay in a change graph that is exported wholesale to the next peer — relaying a claim this machine rejected, and letting any peer in the room append to its permanent history.

The bootstrap case is not an exception to that rule, because joining is the act that removes it: `set_room` surrenders the joining workspace's own governance (`Workspace::surrender_governance`), so a machine either holds none for the room and *learns* it, or holds it and is being asked to have it *overwritten*. Those are never the same act. A machine that does not govern a room also stops projecting governance into it, so the honest path never puts a competing claim on the wire.

The sender's identity reaching that decision is the authenticated `PeerId` (`PeerOrigin`), carried whole — not the `DefaultHasher` digest `peer_id_to_u64` produces, which is fine for echo-suppression and not for an equality check an attacker gets to aim at.

**What this binds is the hop, not the op.** Transport authentication says "these bytes came from `p`"; it says nothing about who authored the change inside them. So a governance change **relayed** by a third peer is refused even when it originated with the room's owner, and a delegated co-administrator's governance cannot cross at all — their edits never reach the owner to be re-issued. Binding the *op* needs a signature over the change, which is what `SignedOp` in `collab-types` is for; its validator is still dormant (`authorised_signers` is never registered). Until that path is wired, governance flows only on a direct hop from the room's owner.

**What a peer must know to be answered** is the room's id — a random 122-bit uuid minted at workspace creation, carried only inside a signed invite (§7), and now never exported to anyone outside the room. Naming it is the credential, in the same sense that holding the invite is. That is a bearer capability with no revocation and no per-peer binding; binding admission to the accepting peer's identity is the next thing to build here.

The replica path goes through `shell/src/network/replica.rs` and `hub.rs`. The cached-materialisation design means writes go through both the public cache and the backing doc, with `assert_cache_matches_doc()` running under `debug_assertions` to catch drift.

## 5. Presence

Cursor moves and focus updates are throttled to ~30 Hz inside `ui::App`, then forwarded to the shell, which publishes `PresenceMoved`/`PresenceCleared` on the workspace topic. Peers' incoming presence becomes remote cursors rendered on top of the local scene (`shell/src/shell/remote_cursors.rs` + `ui/src/collaboration.rs`).

Heartbeat + presence cleanup: a peer that hasn't published anything in 15 s is ejected from `PresenceCtx` and replaced with a "disconnected" placeholder. On `NetEvent::PeerJoined` (re-join), the shell publishes a fresh `SyncReq { since: current_heads() }` to that peer.

## 6. Foreign-surface streaming

When an owner shares a foreign Wayland client surface with peers (peer-sync Phase 4):

- The compositor's per-surface task encodes committed buffers to H.264 via openh264 (`shell/src/network/encoder.rs`), at a 33 ms minimum interval (30 fps cap).
- Encoded NALUs ride `NeighbourMsg::SurfaceFrame` on the workspace topic.
- Peers decode in a `peer_view` widget (`shell/src/shell/peer_view.rs`) with an `H264Decoder`. Decoded frames blit into the tab's content rect.
- Peer pointer/keyboard events on a `peer_view` get serialised as `RemoteInput { peer, surface, event }`. The owner's compositor replays them via `smithay::input::Seat::input`, going through real focus rules.
- The owner can flip a per-peer `read_only` flag (default false) to drop incoming input events.

Note: openh264 is the software path for v1; hardware encoding (VAAPI via ffmpeg-next) is a follow-up.

## 7. Invite tokens

Invites are minted by `make_invite_token` and validated by `validate_invite_token` in `shell/src/network/invite.rs`. Wire format (base64-url, no padding, no JSON pretty-printing):

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

### Where the broker runs

`shell::shell::collab_broker` runs **inside the compositor process**, bound by
the native entry point before the wayland socket is published — an app resolves
its collab descriptor once at spawn, so a broker that arrives later is one that
app never gets.

It is not a systemd user unit, and the reason is not convenience. The broker's
validation path reads the room's governance, the session's identity and the
libp2p handle; §1 puts all three in the shell. A unit would re-export them
across an IPC boundary to reach the same decisions, and would order the session
from outside the only process that knows when the session is ready. (This OS
also never starts `graphical-session.target`, so a unit ordered against it would
simply not run.)

The socket is `$XDG_RUNTIME_DIR/qos/services/collab.sock` —
`launcher::services::collab_socket()`, the same function the launcher calls to
open a descriptor and pass it into the sandbox as `QOS_SVC_COLLAB_FD`. The app
never learns the path; it gets a connected fd. One owner for the name, because
two spellings of it on opposite sides of the sandbox boundary is what let
collaboration sit switched off without anyone noticing.

A session that cannot bind it says so in the chrome (`ChromeState::
set_collab_unavailable`), and refuses to unlink a socket another live session
owns. `collab` stays `optional@1` in app manifests: a broker failure must not
stop a calculator from opening, so the app degrades and the **OS** reports it —
an app cannot tell "no collaborators" from "no collaboration", and the roster is
chrome's to draw either way.

### Native `ui` apps

The toolkit's collaboration plumbing (`ui/src/collaboration.rs`) connects to the shell over a Unix-socket protocol. `CollabRequest::Sync { changes }` flows from app to shell; `CollabEvent::{RemoteCursorMoved, SyncIncremental, …}` flows back. The shell forwards `SyncIncremental` to the network actor, which publishes it.

Inside the app:

- `state.collab.cursors` is populated from `RemoteCursorMoved`.
- Local automerge changes auto-flush via the existing hook in `ui::App`.
- A `permissions()` helper lets widgets check capability grants for the current workspace.
- Remote cursors render natively inside the vello scene for zero-latency feedback.

### The unit of a channel is a record, not an app

An app's traffic is addressed by `(app_id, cluster_id, record_id)` — the triple
it announces on `CollabRequest::Hello`, carried on the peer wire as fields of
`NeighbourMsg::AppCrdt*` and, on the capnp transport, inside the channel id
(`<cluster>#record:<id>`, parsed back by `CrdtRoute::from_channel_key`).

A **record** is one document with one identity (`ui::record`; a file's identity
is its path). Two people editing two different files in one room announce two
different records, so the broker no longer hands each of them the other's
document. Chrome traffic — presence, cursors, roster — stays room-wide, because
who else is here is a fact about the room.

Two ways to make the record the routing key were open, and the second is what
is built:

- **The broker understands record boundaries.** It would have to materialise
  every app's document to know which record a change belongs to — the broker
  holds no `AutoCommit` today and would need one per app per room — and it still
  could not split a change graph, because a change names its dependencies by
  hash (see §4). So it would end up re-deriving, from opaque bytes, a fact the
  sender already knows.
- **Apps announce a channel per record.** The broker keeps routing opaque bytes
  and never materialises anything; what changed is that a connection says which
  document its bytes are for. A process with two records open holds two
  channels. `ui::App` re-announces when the record it has checked out changes.

Fail-closed both ways: an app that has adopted records is not handed the traffic
of one that has not, and vice versa, because those are not the same document.
Apps that have not adopted records keep the `(app, room)` addressing they always
had.

### Meeting a document you have no history for

Two people who each *start* the same app hold two automerge documents
with nothing in common. A delta names its dependencies by hash (§4), so
one sent between them refers to changes the receiver has never seen and
automerge parks it: presence crosses, the caret moves, and the page
stays blank. Something has to carry the state the deltas are relative
to, once.

That frame is `CollabRequest::SyncCRDT` — a whole document — and it
crosses the peer wire as `NeighbourMsg::AppCrdtSnapshot`, routed by the
same `(app_id, cluster_id, record_id)` triple as a delta and admitted by
the same room check.

**It goes out on arrival, not on every commit.** The toolkit produces a
`SyncCRDT` on *every commit that changes the document* — it feeds
same-host fan-out and the undo snapshot — so republishing each one would
put a whole document on the room topic per commit: unbounded, growing
with the session, and redundant with the delta that already carries the
same edit to anyone who shares the history.

The publish is therefore gated on somebody needing one. A peer's
`Arrived` passes through the local broker's own drain on its way to
local apps, so the broker sees the request even though the frame that
answers it is indistinguishable from a commit's: the arrival arms the
room (`ShellNetwork::note_snapshot_request`), the next whole document
for it goes out, and the ones after stay local until somebody else
arrives. Which snapshot is released does not matter — any full state
serves a peer that has none.

That makes the rate once per peer that arrives rather than once per
keystroke, the same rare, single-shot shape as
`ScrollbackRequest`/`ScrollbackResponse`, and it is what makes a
broadcast medium tolerable for this at all.

It is deliberately **not** addressed to the joiner. Merging a snapshot
is idempotent and commutative, so a peer that missed one converges from
somebody else's copy; addressing it would need the *app* to name a
recipient and would buy nothing but a narrower path to the same state.

**Bounded, and honest about the bound.** A document has no upper size
and a gossipsub RPC has one — 64 KiB, the libp2p default this shell does
not raise. `publish_app_crdt_snapshot` refuses above
`MAX_APP_SNAPSHOT_BYTES` (48 KiB, leaving the envelope and gossipsub's
own framing room under that ceiling) rather than handing the transport a
message it would reject. A document past the cap does not converge, in a
`warn` line naming the size; it does not half-converge, and it does not
fail somewhere unattributable.

**Adoption is one-directional, and that is a real limit.** The arrival
arms the room on the host that was *already there*, so its document
crosses to the joiner and the joiner adopts it. Nothing arms the
reverse: the joiner's own document — the one it created before it met
anybody — never crosses, so the host has no history for it and parks the
joiner's deltas. Two people converge on the document that was open
first, and edits made by the second before it adopted are not merged in.

That is enough for someone joining a document another person is already
editing, which is the common case and the one the demo shows. It is not
enough for two people who both typed before meeting. Closing it means
the toolkit sending its own state once upon adopting a foreign document
— a decision about the app protocol, where the two documents actually
are, rather than about routing. Arming the reverse direction here
instead would look equivalent and is not: with both sides armed, every
commit re-arms the other, and the room is back to a whole document per
commit.

Raising the ceiling would move that failure rather than remove it, and
would do it on a medium where every peer in the room pays for every
byte. Past the cap the right carrier is a direct peer-to-peer stream —
the substrate already exists in `shell/src/network/surface_stream.rs`,
which video frames use for exactly the same two reasons: they are large,
and they concern one peer rather than the room. Wiring a snapshot onto
it is the next thing to build here.

### Legacy / foreign Wayland clients

For non-collaboration-aware Wayland clients (anything bound through the per-workspace bridge socket):

- **Compositor-side cursors.** The shell draws remote cursors over the foreign-client window — the client itself sees only its single local pointer.
- **Input multiplexing.** The compositor decides whose input reaches the foreign client; default policy is "host always wins" with a per-peer `read_only` toggle for guests.
- **Environment emulation.** The launcher gives the app one writable directory at `/run/qos/app` — per room, per package — with `HOME` and the `XDG_*` variables pointed inside it, plus a read-only system profile it did not choose. The app sees no path outside the sandbox, and none of what it does see was named by its manifest.

## 9. Disconnect / reconnect

- Owner publishes a heartbeat per joined workspace every 5 s.
- Peer that hasn't seen a heartbeat in 15 s → ejected from `PresenceCtx`.
- On reconnect: `NetEvent::PeerJoined` triggers a fresh `SyncReq { since: current_heads() }`; missing automerge changes flow in via `SyncResp`.

Tests in `shell/tests/peer_sync_test.rs` exercise the convergence path.

## Where to read first

- `shell/src/network/{actor.rs, behaviour.rs, hub.rs, replica.rs, protocol.rs, invite.rs}` — the live wiring.
- `docs/superpowers/archive/2026-05-02-peer-sync-end-to-end.md` — phase-by-phase implementation log.
- `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md` — why the cache + backing doc.
- `ui/src/collaboration.rs` — the in-process channel between toolkit and shell.
