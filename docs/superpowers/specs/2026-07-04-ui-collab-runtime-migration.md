# ui::collaboration → runtime-collab SDK migration

**Status:** blocked. Requires the SDK schema to catch up with the semantics `ui` currently exposes.

## Motivation

`ui::collaboration` is a bespoke serde-Unix-socket collab client baked into the toolkit. `runtime-collab` (part of the SDK apps link against) is the Cap'n Proto version, with the graceful "unavailable → Ok" contract that gives every app cross-target-Linux support for free. Migrating `ui` off its own client onto the SDK unblocks fleet-wide qpkg packaging AND stock-Fedora / Flatpak portability.

## Why "just make ui delegate to runtime-collab" doesn't fly today

**1. `ui::collaboration::{CollabRequest, CollabEvent, PresencePayload}` are the public API of the crate**, consumed by name across the fleet:

| Consumer | Files / lines |
|---|---|
| `apps/notes` | src/app.rs:25, 926 (constructs `PresencePayload::Arrived`) |
| `apps/word-processor` | src/widget.rs:5636, 5659, 5672, 9728, 9751 (`PresencePayload::{Caret, Selection}`) |
| `apps/permissions` | editor/shared.rs:379, identity.rs:88, 136, 252 (`Collaborator`) |
| `apps/collab-broker` | src/main.rs:29 (entire binary is a wrapper) |
| `ui/src/app.rs` | 74, 240, 366, 871, 889, 959–1095, 1264, 1324 (dispatches all 7 `CollabEvent` variants) |
| `shell/src/shell/collab_broker.rs` | **1437 LOC** production broker, 44+ direct references |
| Tests | `ui/tests/{live_text_sync, multi_user_session, cross_process_presence_pump, real_socket_two_peer}.rs` |

They are not internal implementation detail — they are the wire.

**2. `runtime-collab` is a semantic subset of what ui exposes.** Mapping:

| `CollabRequest` variant | SDK equivalent |
|---|---|
| `Hello { app_id, cluster_id }` | Partial: `join_channel(id, identity)` — no cluster, no perms |
| `SyncCRDT { data }` (legacy full snapshot) | **None** |
| `SyncIncremental { data }` (legacy unsigned) | **None** |
| `Presence { key, payload: Pointer }` | `PresenceHandle::publish_cursor(x, y)` |
| `Presence { key, payload: Caret / Selection / Arrived / Left }` | **None** |
| `SignedSyncIncremental { envelope }` | `ChannelHandle::send(op_bytes)` |
| `FetchScrollback { ... }` | **None** |

`CollabEvent` outbound: SDK's `stream.read()` returns opaque `Vec<u8>` with no `from_user_id`; no equivalent for `Context` (roster + permissions), `CRDTUpdate`, `CRDTIncremental`, `RemotePresenceUpdate` (non-cursor), or `ScrollbackBatch`.

**3. Threading model mismatch.** `CollaborationManager::poll()` is **synchronous non-blocking**, called from the winit event loop. `runtime-collab` futures are `!Send` and require a `tokio::task::LocalSet`. Bridging needs a shovel thread + mpsc pair, not a translator.

**4. Two brokers speak the old wire.** Even if `ui` internally switches, both brokers need to change with it:

- `apps/collab-broker` — 33-LOC binary wrapping `ui::collaboration::broker::serve_path`. Test/dev broker.
- `shell/src/shell/collab_broker.rs` — **1437 LOC** production broker running inside every shell session. What every real ui app talks to.

Neither speaks Cap'n Proto today. Migrating `ui` without upgrading them = every ui app loses collab entirely.

## Two-phase plan

### Phase 1: extend the SDK to cover ui's semantics + refit the broker

**1.1 — `runtime-collab` schema extension.**

Land in `crates/runtime/runtime-collab/schema/collab.capnp`:

- **Extended presence:**
  ```capnp
  interface PresenceHandle {
    publishCursor    @0 (x :Float32, y :Float32) -> ();
    publishCaret     @1 (keyBytes :Data, pos :UInt64) -> ();
    publishSelection @2 (keyBytes :Data, start :UInt64, end :UInt64) -> ();
    publishArrived   @3 (keyBytes :Data) -> ();
    publishLeft      @4 (keyBytes :Data) -> ();
    subscribeAll     @5 () -> (stream :PresenceStream);
  }
  interface PresenceStream {
    read @0 () -> (event :PresenceEvent);
  }
  struct PresenceEvent {
    userId    @0 :Text;
    keyBytes  @1 :Data;      # postcard of ui::PresenceKey
    payload   @2 :PresencePayload;
  }
  ```
  `keyBytes` stays opaque (a postcard of `ui::PresenceKey`) so the schema doesn't couple to `ui`.

- **Roster / context:** `subscribeContext() -> ContextStream` yielding `(collaborators, permissions)`, delivered whenever the broker updates the roster.

- **Signed op stream must carry attribution:** either extend `Stream.read()` to `-> (op :Data, fromUserId :Text)` or fold attribution into the postcard envelope's outer shell.

- **Legacy unsigned paths:** either expose `sendUnsignedSnapshot` / `sendUnsignedIncremental` OR require callers to move to signed ops. **Recommend the latter** — it's what the security model wanted anyway.

- **Scrollback:** `fetchScrollback(artifactId, beforeSeq, max) -> ScrollbackBatch`.

**1.2 — Broker refit.** Two options:

- **A. Rewrite `shell/src/shell/collab_broker.rs` to serve the extended capnp interface.** 3–5 days of careful surgery. Keeps fanout semantics identical. Biggest chunk of work.
- **B. Bridge process** (~200 LOC): accepts capnp from apps, forwards to the old shell broker over its existing serde-JSON socket. Cheaper. Adds a hop.

Option A is cleaner; option B ships faster.

### Phase 2: `ui::collaboration` becomes a shim

Once Phase 1 lands:

- `ui::collaboration::CollaborationManager` becomes a synchronous shim over `runtime-collab`.
- Shovel thread bridges tokio `LocalSet` ↔ the sync `send`/`poll` API `ui::app.rs` calls.
- Public `CollabRequest` / `CollabEvent` / `PresencePayload` shape preserved. **Every existing caller keeps compiling unchanged.**

### Phase 3: opportunistic per-app migration (optional)

Each app can drop the legacy shape when convenient:

| App | Effort |
|---|---|
| `apps/notes` | ~1 h (2 lines) |
| `apps/word-processor` | ~2 h (5 sites in widget.rs) |
| `apps/permissions` | ~1 h (4 sites in constructor helpers) |
| `apps/collab-broker` | ~30 min (delete or repoint) |
| `ui/src/app.rs` | ~4–6 h (7 event variants, mixed with app-facing side effects — best left last) |

Delete `ui::collaboration` legacy wrappers once every caller has moved.

## What breaks if we try to short-circuit

If we force Milestone A ("wire types stay, but `CollaborationManager` only routes what the SDK supports today") — the closest possible thing to preserving the API without extending the SDK:

- `apps/notes` — presence `Arrived` silently becomes a no-op; join badges never appear.
- `apps/word-processor` — remote caret / selection rendering silently stops.
- `apps/permissions` — roster stops arriving; identity picker shows empty list.
- `apps/collab-broker` — still compiles; nothing connects to it.
- `ui` internals — 6 of 7 `CollabEvent` variants dispatched in `app.rs` never fire.
- `shell::shell::collab_broker` — still compiles; deaf.
- Tests: `ui/tests/{live_text_sync, multi_user_session, real_socket_two_peer, cross_process_presence_pump}.rs`, `apps/notes/tests/two_peer_sync.rs` — all break.

That is the "bigger than scoped" bomb the STOP clause guarded against.
