# Collaborative Presence — Spec (2026-06-24)

> **Status:** authoritative spec for the app-level collaboration milestone.
> Goal: **two live instances of the notes app, running locally on one
> machine, with full live collaboration** — shared document *and* rich
> per-peer presence (pointer, caret, text selection, authorship,
> focused-element glow, and wayfinding "indicative mist").
>
> The presence work is **structural**: the primitives live in the `ui`
> toolkit so every app (terminal, file-explorer, future apps, and the
> shell itself) can adopt them. The notes app is the demo vehicle, not
> the only beneficiary.
>
> Companion implementation plan: `docs/superpowers/plans/2026-06-24-collaborative-presence.md` (written next).

---

## 0. Background: two collaboration planes, one missing bridge

The stack already has two CRDT-sync planes:

- **Plane A — `ui/src/collaboration.rs`:** a Unix-socket protocol
  (`QUICK_COLLAB_SOCK`) carrying per-app `State::doc` deltas
  (`SyncIncremental` / `SyncCRDT`) and `Presence` payloads. **Every app
  already speaks it** through `ui::App`. In production *nothing binds
  the socket* — only `ui/tests/real_socket_two_peer.rs` does, as a test
  broker.
- **Plane B — `shell/src/network/replica.rs`:** libp2p gossipsub + mdns
  syncing the shell's `WorkspaceTree::doc` across LAN peers. Well-tested,
  not wired to per-app docs.

This milestone takes **Plane A** to production with a standalone broker
(Path 1 — local, ships now). The shell-as-broker bridge to Plane B
(Path 2 — cross-laptop) is a later swap-in behind the *same* protocol
and is explicitly out of scope here.

Confirmed facts the design relies on (verified 2026-06-24):

- `ui::App`'s event loop already polls the socket, applies
  `CRDTIncremental` (`app.rs` `about_to_wait`), and routes
  `RemotePresenceUpdate` into `PresenceCtx`. **Apps get document sync by
  existing in that loop** — zero per-app sync code.
- `PresenceKey` is already location-scoped: `Widget { path }` /
  `Tab { id }` / `Workspace { id }` / `App { app, sub }`.
- `PresenceCtx` already stores `pointers`, `carets`, `selections`, and
  "who is here" per key.
- `user_color(user_id)` already assigns a stable per-peer color
  (named palette + FNV-hash→HSL fallback); `PresenceCtx::color_for`
  honors server overrides.
- `PresencePayload` already has `Pointer`, `Caret`, `Selection`,
  `Arrived`, `Left`. **No protocol changes are required** for the demo.

What is missing, and what this spec adds: a **production broker**, a
**CRDT-backed notes document**, **correct + structural rendering** of
each presence type, and **authorship tracking** (Automerge marks — the
one genuinely new data-model piece).

---

## 1. The decisive split: shared vs. per-peer state

Everything hangs on this:

| Concern | Where it lives | Why |
| --- | --- | --- |
| Page tree (page metas: id, title, parent, order) | **Shared** Automerge doc | structure must converge across peers |
| Per-page block content (each block's text + marks) | **Shared** Automerge doc | the actual collaborative document |
| **Which page a peer is viewing** | **Per-peer** (presence) | peers must read *different* pages independently |
| Caret position | **Per-peer** (presence) | |
| Text selection | **Per-peer** (presence) | |
| Pointer position | **Per-peer** (presence) | |

Today the notes app stores `selected` (active page) **inside the shared
`WorkspaceData`**. If we synced that, both peers would be yanked to the
same page — which makes **indicative mist impossible** (there would be no
"other page a peer is on"). Therefore:

> **`selected` moves OUT of the shared doc into per-peer local state**,
> and "where peer B is" becomes a presence fact:
> `Presence { key: Tab { id: page_id }, payload: Arrived }`.

This is what lets B edit page X while you read page Y and still see where
B is.

---

## 2. CRDT-backing the notes document (heaviest piece)

Today: page bodies are markdown files on disk; `BlockEditor::sync()` and
`RichTextLine::sync()` are no-ops; the editor owns local `String`s. The
workspace tree is stored as `serde_json(WorkspaceData)` spliced into a
single Automerge `Text` blob (a serialization shim, not a live CRDT
structure).

Target doc shape (in the app's shared `State::doc`):

```
root
├── "pages"  (Map, keyed by PageId string)
│   └── <page-id> (Map)
│       ├── "title"  (Text)               # collaborative title
│       ├── "parent" (Str|null)           # PageId or null
│       ├── "order"  (Int)
│       └── "blocks" (List)
│           └── [i] (Map)
│               ├── "kind" (Str + params) # paragraph/heading(n)/bullet/checklist/code/divider
│               └── "text" (Text)         # the block's text; carries author marks (§3)
```

Changes:

- **`BlockEditor::sync()`** reads the active page's `blocks` list from the
  doc and rebuilds/refreshes blocks. Block identity is positional today
  (`Vec<Block>`, no stable id); the `blocks` List gives us CRDT-stable
  element identity for free, so concurrent inserts/deletes at different
  positions converge without index races.
- **Edit sites already CRDT-aware:** `TextInput::insert_str` /
  `delete_selection` / backspace / delete already call `splice_text`
  *when given an `obj_id`*. We thread the active block's `Text` `ObjId`
  through `RichTextLine` → `TextInput` so edits hit the CRDT. (Position
  unit stays char-index for the Automerge calls, byte-offset for the
  in-memory `String`, exactly as the existing code already converts.)
- **Workspace tree** moves from the JSON-in-Text shim to the live
  `pages` Map, so page create/rename/reorder/delete converge as CRDT ops.
- **Disk** becomes an *optional* autosave of a doc snapshot (debounced),
  not the sync mechanism. The existing `PageSaver`/`Saver` debounce hooks
  are repurposed to snapshot the doc.

Non-goal: rich markdown round-tripping through disk for *sync*. Markdown
import/export stays as a load/export convenience.

---

## 3. Authorship — "what they wrote", marker-style

- Extend the marks model (`ui/.../marks.rs`) with `Mark::Author(UserId)`.
- Stamp it at the same insert sites as text edits, persisted as
  **Automerge marks** on each block's `Text` object
  (`put_mark`/`marks`) so authorship **syncs automatically** with the
  document — no separate channel.
- Set the Automerge **actor id** + a local `UserId` at startup, derived
  from `QUICK_APP_ID` (the same identity the broker attributes presence
  to), so author marks and presence colors agree.
- Render: a run whose author ≠ me gets a translucent **marker underlay**
  in `color_for(author)` (drawn under the glyphs so text stays legible).
- Deletes erase text; they do not need an author mark. Marks on deleted
  ranges are removed by Automerge with the spliced characters.

This is the only new *data-model* surface; it is coupled to §2 (it needs
the block `Text` objects to exist).

---

## 4. Five presence primitives — structural, reusable

All live in `ui/src/presence/render.rs` (two already exist:
`draw_peer_caret`, `draw_peer_selection`). Each resolves the peer color
via `PresenceCtx::color_for(user)`:

1. **`draw_peer_pointer(scene, theme, user, (x,y))`** — a **round filled
   dot** + soft halo + name label, in the peer's color. Replaces today's
   arrow `BezPath` in `app.rs`, and reads from **`PresenceCtx` pointers**
   (the structural path) instead of the legacy `state.collab.cursors`
   map. Drawn in window/scene space, keyed by the app/workspace presence
   key.
2. **`draw_peer_caret(scene, theme, user, rect)`** — exists, but is
   **fixed to use real metrics**: the widget supplies the caret `Rect`
   from `RichTextLine::pixel_x_for_byte(theme, offset)` + the block's `y`.
   This deletes the fake `advance = 8.0px` constant currently used for
   remote carets.
3. **`draw_peer_selection(scene, theme, user, rects)`** — exists; the
   widget supplies the run of `Rect`s (one per visual line the selection
   spans), again from real metrics.
4. **`draw_peer_authorship(scene, theme, user, rects)`** — new; marker
   underlay for foreign-authored runs (§3).
5. **`draw_peer_focus_glow(scene, theme, user, rect)`** — new; a glow /
   outline around the widget a peer is focused in. Keyed by
   `PresenceKey::Widget { path }`, which `TextInput` already exposes via
   `presence_key()`.
6. **`draw_indicative_mist(scene, theme, user, rect)`** — new; a soft
   radial "mist" on a navigational element (§5).

### The structural contract (so other apps adopt it cheaply)

A widget participates in remote presence by providing two things:

- **`presence_key() -> Option<PresenceKey>`** — already on `TextInput`.
- **an offset→`Rect` mapping** — already on `RichTextLine`
  (`pixel_x_for_byte`), so the framework can place remote carets /
  selections / authorship without per-widget bespoke math.

The framework (a draw-time helper invoked from `Widget::draw`, reading
`PresenceCtx` for the widget's key) does the rest. Terminal,
file-explorer, and the shell can opt in by implementing the same two
hooks. This is the "structural and reusable" requirement, satisfied at
the toolkit layer rather than per app.

---

## 5. Indicative mist — wayfinding to a peer's location

Definition (user's words): *"colored mist in the color of another user on
the UI element that leads to where that user currently is — another
screen inside the same application, another tab inside a window group,
etc."*

- **In-app (demoed with two notes instances):** peer B's location
  presence is `Tab { id: page_X }`. In the **sidebar `draw`**, for each
  page row whose `PageId` has a remote peer present (lookup in
  `PresenceCtx`), paint `draw_indicative_mist` on that `row_rect` in the
  peer's color. That is the wayfinding glow to "the screen B is on."
  The sidebar already has stable `PageId`s per row and `row_rects[i]`
  for placement.
- **Cross-app / window-group tabs (shell-level):** the *same primitive*,
  fed by the shell's window-group chrome + Plane B presence. **Out of
  scope to demo** in this milestone (it requires the shell running both
  apps inside one group — not reproducible with two standalone notes
  instances), but the primitive and the `PresenceKey::Tab`/`App` contract
  are designed so the shell can call it. Documented as the generalization;
  not built here.

---

## 6. Transport: the `collab-broker` binary

A standalone binary (new crate `apps/collab-broker`, or a `ui` example
bin — decided in the plan) that:

- Binds `QUICK_COLLAB_SOCK` (path from arg/env), accepts **N** peer
  connections (generalizing `real_socket_two_peer.rs` from 2→N).
- Assigns each connection a **stable `user_id`** (from the `Hello`
  `app_id`, falling back to `peer-{n}`) → stable color via
  `user_color()`.
- Fans `SyncIncremental` → `CRDTIncremental`, `SyncCRDT` → `CRDTUpdate`,
  and `Presence` → `RemotePresenceUpdate { user_id, .. }` to all **other**
  connected peers (never echo to sender).
- Cleans up on EOF.

### Late-join convergence (no Automerge knowledge in the broker)

A late joiner must receive current document state. We do this with
**existing protocol messages**, keeping the broker dumb:

1. On connect, a peer broadcasts `Presence { key: App{..}, Arrived }`.
2. Peers already in the room, on receiving a remote `Arrived`, reply with
   a full `SyncCRDT { data: doc.save() }`.
3. The broker fans that to the new peer, which `State::load`s it.

Small additions required: emit `Arrived` on connect, and handle inbound
remote `Arrived` by sending a full snapshot. Both are app/`ui::App`-level,
not broker-level.

### Running the demo (one machine)

```bash
collab-broker /tmp/notes.sock &
QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=alice cargo run -p notes &
QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=bob   cargo run -p notes
```

Two windows: edits in either converge; alice sees bob's pointer (round,
his color), his caret + selection in his color, the text he wrote tinted
in his color, a glow on the block he's editing, and — when he's on a
different page — mist on that page's sidebar row.

---

## 7. Integration / base branch

This work needs `ui-app-polish`'s `ui` (BlockEditor, marks, RichTextLine)
and `apps` (notes), but the worktree was created off `main` per the
original instruction. Verified 2026-06-24: **both `ui` branches carry the
identical collab/presence infra** (`PresencePayload` byte-for-byte the
same; both have `real_socket_two_peer.rs`); `ui-app-polish` merely adds
`BlockEditor` on top.

**Decision: rebase this effort onto `ui-app-polish`'s submodule state:**

- `ui` → `b258352`, `apps` → `1c7f487`, superproject ref `0efe33e`.

**Plan step 0 (verification gate):** confirm whether `main`'s three
newest `ui` commits (`a5aeddf` strip `scroll_dirty` cache fix, `9cc9600`
`UserEvent::Wheel`, `13ebdfd` horizontal-wheel pan) are absent from
`ui-app-polish`; if so and they matter for rendering/scroll, cherry-pick
them. They are scroll/wheel fixes, likely orthogonal to this milestone,
but must be checked before building.

---

## 8. Testing strategy

Assert on the production path; never weaken an assertion (repo covenant).

- **Broker:** extend `real_socket_two_peer.rs` to `real_socket_N_peer` —
  3 peers, verify fan-out to all-but-sender, `Hello` swallowed, late
  joiner receives full state via the `Arrived`→`SyncCRDT` handshake.
- **CRDT notes document:** two in-memory `MemoryRelay` peers edit
  *different* blocks and *different* pages → both converge on document
  content, while `selected` (per-peer) stays independent.
- **Authorship:** author marks survive `save` / `load_incremental`
  round-trips; a run authored by peer B resolves to B's color.
- **Presence rendering:** `OffscreenRenderer` snapshot per primitive —
  remote caret, selection, authorship underlay, focus glow, and
  indicative mist — asserting the expected colored pixels at the rects
  computed from real metrics (not the legacy 8px constant).
- **Position mapping:** unit test `pixel_x_for_byte` round-trips against
  `byte_for_pixel_x` so remote caret placement matches local caret
  placement for the same offset.

---

## 9. Phasing (one milestone, landed in order)

Each phase is independently testable; we can stop at any green point.

1. **Sync foundation** — `collab-broker` + CRDT-back `BlockEditor`
   (page tree + block text in the doc) + move `selected` to per-peer.
   Exit: two notes instances sync text live.
2. **Core presence** — remote **caret + selection** (real metrics) +
   round **pointer**, via the structural primitives.
3. **Authorship** — `Mark::Author`, stamped + persisted as Automerge
   marks, rendered as the marker underlay.
4. **Wayfinding** — **focus glow** + **indicative mist** (in-app page
   case). Document the shell/window-group generalization.

---

## 10. Scope honesty

This is large — realistically several days, not one or two. The deep
parts are §2 (CRDT-backing BlockEditor) and §3 (authorship marks); the
five render primitives are individually small *once the data plumbing
exists*. The window-group-tab mist is deliberately **designed but not
built** here. No protocol changes are needed; the heavy lifting is in the
notes document model and the toolkit-level presence rendering.

---

## 11. Open questions resolved during brainstorming

- **Demo app:** notes (not terminal — terminal spawns one PTY per
  instance, so two instances writing the same grid garble; and a grid has
  no text caret / authorship surface). *Resolved: notes.*
- **Notes base:** reuse the committed `ui-app-polish` BlockEditor, rewrite
  storage disk→Automerge. *Resolved: reuse.*
- **Milestone size:** all five presence features in one milestone (phased
  internally). *Resolved: all five.*
- **`selected` shared vs per-peer:** per-peer. *Resolved.*
- **Window-group mist:** designed, not built this milestone. *Resolved.*
