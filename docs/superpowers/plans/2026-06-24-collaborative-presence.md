# Collaborative Presence — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two instances of the notes app, running locally on one machine, syncing one shared CRDT document live, where each peer sees who is typing what and where — pointer, caret, selection, authorship, focused-element glow, and "indicative mist" wayfinding — all via reusable `ui` presence primitives.

**Architecture:** A standalone `collab-broker` binds `QUICK_COLLAB_SOCK` and fans Plane-A messages between N peers (generalizing the existing test broker). The notes document moves from disk/markdown into the app's shared Automerge `State::doc` (page tree + per-block `Text`), reusing `TextInput`'s existing CRDT splice path. "Which page I'm viewing" leaves the shared doc and becomes per-peer presence. Remote presence (pointer/caret/selection/authorship/focus/mist) renders through reusable helpers in `ui/src/presence/render.rs`, each fed by the widget's own offset→pixel mapping (real font metrics, not the legacy 8 px constant).

**Tech Stack:** Rust (edition 2024 for `ui`; `apps/notes` is edition 2024), `automerge` 0.8, `vello`/`wgpu` render, `taffy` layout, `cosmic-text` via `ui::text_render`, `winit`, Unix domain sockets + `serde_json` framing, `serial_test` for stateful tests, `OffscreenRenderer` for snapshot tests.

## Global Constraints

- **Build only through distrobox:** every cargo command is `distrobox enter my-distrobox -- bash -c 'cargo <cmd>'`, run from the worktree root `/var/home/quinten/Development/quickdesktop/.claude/worktrees/app-collab-bridge`.
- **Scope builds/tests to `-p notes`, `-p ui`, `-p collab-broker`.** Do **not** build the workspace or `-p shell`: this worktree mixes `ui@b258352` (notes lineage) with `main`'s shell, which won't compile against each other. The demo never needs the shell.
- **No protocol changes** to `CollabRequest` / `CollabEvent` / `PresencePayload`. They already carry everything (`Pointer`, `Caret`, `Selection`, `Arrived`, `Left`).
- **Verification covenant** (repo-wide): never `#[ignore]` a failing test, never weaken an assertion, always assert on the production path.
- **Peer identity:** a peer's `user_id` comes from `QUICK_APP_ID`; its color comes from `ui::presence::user_color(user_id)`. Author marks and presence color must use the **same** `user_id`.
- **Submodule commits first, pointer bump second** (per CLAUDE.md). Work happens on branch `collab-presence` inside `ui/` and `apps/`; the superproject worktree branch is `worktree-app-collab-bridge`.

---

## File Structure

**New files**
- `apps/collab-broker/Cargo.toml`, `apps/collab-broker/src/main.rs` — the N-peer broker binary.
- `ui/src/presence/render.rs` — extend (already exists) with `draw_peer_pointer`, `draw_peer_authorship`, `draw_peer_focus_glow`, `draw_peer_mist`; fix `draw_peer_caret`/`draw_peer_selection` callers to use real metrics.
- `apps/notes/src/doc.rs` — the Automerge document model for the notes workspace (page tree + page bodies as per-block `Text`).
- `apps/notes/tests/two_peer_sync.rs` — in-memory two-peer convergence test.
- `ui/tests/real_socket_n_peer.rs` — N-peer broker socket test.

**Modified files**
- root `Cargo.toml` — add `apps/notes` (done) and `apps/collab-broker` to members.
- `ui/src/widget/rich_text_line.rs` — re-enable CRDT backing (path-aware `sync`, thread `obj_id` to edits, expose `pixel_x_for_byte`/selection-rects helpers already present).
- `ui/src/widget/block_editor.rs` — `sync()` reads blocks from the doc; draw remote presence per block; expose a per-block presence position mapping.
- `ui/src/widget/text_input.rs` — add `Mark::Author` stamping at edit sites; expose caret/selection rect helpers for remote rendering (mostly present).
- `ui/src/widget/marks.rs` — add `Mark::Author(UserId)`.
- `ui/src/presence/mod.rs` — any helper needed to query "peers present on key K" and "peer caret/selection on key K".
- `ui/src/app.rs` — emit `Presence::Arrived` on connect; on inbound remote `Arrived`, reply with full `SyncCRDT`; draw remote pointer from `PresenceCtx` (round) instead of the legacy arrow.
- `apps/notes/src/app.rs` — back the workspace with `doc.rs`; move `selected` to per-peer; broadcast location presence (`Tab{page_id}` Arrived) on page switch; set the presence broadcast scope.
- `apps/notes/src/main_panel.rs` — load/save through the doc instead of disk; thread block `Text` ObjIds into the editor.
- `apps/notes/src/sidebar.rs` — draw indicative mist on rows whose `PageId` has a remote peer present.
- `apps/notes/src/main.rs` — set Automerge actor + local `UserId` from `QUICK_APP_ID`; set presence scope.

---

## Phase 0 — Integration gate (DONE during planning, verify here)

### Task 0: Baseline builds

**Files:** root `Cargo.toml` (modify — done), submodules `ui`@`collab-presence`(=b258352), `apps`@`collab-presence`(=1c7f487).

**Interfaces:**
- Produces: a worktree where `cargo build -p notes` succeeds, with `BlockEditor` at `ui/src/widget/block_editor.rs` and the notes crate at `apps/notes`.

- [ ] **Step 1: Confirm submodule state**

Run:
```bash
git -C ui rev-parse --short HEAD   # expect b258352
git -C apps rev-parse --short HEAD # expect 1c7f487
test -f ui/src/widget/block_editor.rs && echo OK
```

- [ ] **Step 2: Build notes (baseline)**

Run: `distrobox enter my-distrobox -- bash -c 'cargo build -p notes'`
Expected: compiles clean (warnings OK). If it fails, fix the workspace membership / dep issues before continuing — do not proceed on a red baseline.

- [ ] **Step 3: Run the notes app's existing tests (baseline green)**

Run: `distrobox enter my-distrobox -- bash -c 'cargo test -p notes'`
Expected: existing notes tests pass. Record the count; later phases must not regress it.

- [ ] **Step 4: Commit the workspace membership change (superproject)**

```bash
git add Cargo.toml
git commit -m "build: add apps/notes (and prep collab-broker) to workspace members"
```

---

## Phase 1 — Sync foundation

End state: `collab-broker` runs; two `cargo run -p notes` instances against it converge on one shared document (text typed in A appears in B); `selected` page is independent per peer.

### Task 1: `collab-broker` binary — N-peer fan-out

**Files:**
- Create: `apps/collab-broker/Cargo.toml`
- Create: `apps/collab-broker/src/main.rs`
- Modify: root `Cargo.toml` (add `"apps/collab-broker"` to members)
- Test: `ui/tests/real_socket_n_peer.rs` (extends the existing 2-peer test to 3 peers + late join)

**Interfaces:**
- Consumes: `ui::collaboration::{CollabRequest, CollabEvent}` (re-exported), the 4-byte-LE-length + `serde_json` frame format.
- Produces: a binary `collab-broker <socket-path>` that: binds the socket; per connection spawns a reader; for each `CollabRequest`, swallows `Hello`, maps `SyncIncremental→CRDTIncremental`, `SyncCRDT→CRDTUpdate`, `Presence→RemotePresenceUpdate{user_id}`; writes to **every other** peer; attributes `user_id` from the connection's `Hello.app_id` (fallback `peer-{n}`).

- [ ] **Step 1: Write the failing socket test (3 peers)**

Create `ui/tests/real_socket_n_peer.rs`. Model it on `ui/tests/real_socket_two_peer.rs` but parameterized to N=3, asserting a `SyncIncremental` from peer 0 is delivered to peers 1 and 2 (not echoed to 0), and a `Presence{Pointer}` from peer 1 arrives at 0 and 2 as `RemotePresenceUpdate` with peer 1's `user_id`. Reuse the broker logic but import it from the new binary's library form if extracted, OR (simpler) duplicate the loop in the test and assert behavior the binary must match. Keep the existing 2-peer test untouched.

```rust
// ui/tests/real_socket_n_peer.rs (skeleton — fill framing helpers from real_socket_two_peer.rs)
use ui::collaboration::{CollabEvent, CollabRequest};
// ... UnixListener broker spawning 3 clients ...
#[test]
fn three_peer_fanout_excludes_sender() {
    // peer0 sends SyncIncremental{data: b"X"}
    // assert peer1 and peer2 receive CRDTIncremental{data: b"X"}
    // assert peer0 receives nothing
}
#[test]
fn presence_attributes_sender_user_id() {
    // peer1 sends Presence{ key: App{..}, payload: Pointer{1.0,2.0} }
    // assert peer0 + peer2 receive RemotePresenceUpdate{ user_id == peer1 id, Pointer{1,2} }
}
```

- [ ] **Step 2: Run it, watch it fail**

Run: `distrobox enter my-distrobox -- bash -c 'cargo test -p ui --test real_socket_n_peer'`
Expected: FAIL (broker not implemented / test broker not matching attribution).

- [ ] **Step 3: Implement the broker binary**

`apps/collab-broker/Cargo.toml`:
```toml
[package]
name = "collab-broker"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "collab-broker"
path = "src/main.rs"

[dependencies]
ui = { path = "../../ui" }
serde_json = "1.0"
```

`apps/collab-broker/src/main.rs` — generalize the test broker: accept a socket path arg; loop `accept()`; assign each connection an index and a `user_id` from its `Hello.app_id`; one reader thread per peer pushing `(idx, frame)` onto an `mpsc`; a main loop that decodes `CollabRequest`, maps to `CollabEvent` (swallow `Hello`; attribute `Presence` with that peer's `user_id`), serializes, and writes the framed bytes to every peer except `idx`. Use a shared `Arc<Mutex<Vec<Option<UnixStream>>>>` for the write side; drop on EOF. Reuse `write_frame`/`read_frame` shapes from `real_socket_two_peer.rs` (4-byte LE length + json).

- [ ] **Step 4: Add to workspace + build**

Add `"apps/collab-broker"` to root `Cargo.toml` members.
Run: `distrobox enter my-distrobox -- bash -c 'cargo build -p collab-broker'`
Expected: builds.

- [ ] **Step 5: Run the socket test green**

Run: `distrobox enter my-distrobox -- bash -c 'cargo test -p ui --test real_socket_n_peer'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git -C ui add tests/real_socket_n_peer.rs && git -C ui commit -m "test(ui): N-peer broker fan-out + presence attribution"
git -C apps add collab-broker && git -C apps commit -m "feat(collab-broker): standalone N-peer QUICK_COLLAB_SOCK broker"
git add Cargo.toml && git commit -m "build: add apps/collab-broker to workspace"
```

### Task 2: Notes document model (`doc.rs`) — page tree in Automerge

**Files:**
- Create: `apps/notes/src/doc.rs`
- Modify: `apps/notes/src/lib.rs` (add `mod doc;`)
- Test: in `apps/notes/src/doc.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `automerge::{AutoCommit, ObjId, ObjType, ROOT}`, `crate::model::{PageId, PageMeta}`.
- Produces:
  - `pub struct NotesDoc;` with associated functions over `&mut AutoCommit`:
    - `fn pages_map(doc) -> ObjId` (get-or-create `root["pages"]` Map)
    - `fn upsert_page_meta(doc, &PageMeta)` (writes `title` Text, `parent`, `order`)
    - `fn list_pages(doc) -> Vec<PageMeta>`
    - `fn page_blocks_list(doc, PageId) -> ObjId` (get-or-create `pages[id]["blocks"]` List)
    - `fn block_text_obj(doc, blocks_list, index) -> ObjId` (the `Text` for block i)
  - These give later tasks stable ObjIds to splice into.

- [ ] **Step 1: Write failing test — page tree round-trips through the doc**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use crate::model::PageMeta;
    use uuid::Uuid;

    #[test]
    fn pages_round_trip() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        NotesDoc::upsert_page_meta(&mut doc, &PageMeta { id, title: "Hello".into(), parent: None, order: 0 });
        let pages = NotesDoc::list_pages(&doc);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, id);
        assert_eq!(pages[0].title, "Hello");
    }

    #[test]
    fn two_docs_merge_pages() {
        let mut a = AutoCommit::new();
        let mut b = AutoCommit::new();
        let ida = Uuid::new_v4();
        let idb = Uuid::new_v4();
        NotesDoc::upsert_page_meta(&mut a, &PageMeta { id: ida, title: "A".into(), parent: None, order: 0 });
        NotesDoc::upsert_page_meta(&mut b, &PageMeta { id: idb, title: "B".into(), parent: None, order: 1 });
        let delta = a.save();
        b.merge(&mut AutoCommit::load(&delta).unwrap()).unwrap();
        let titles: Vec<_> = NotesDoc::list_pages(&b).into_iter().map(|p| p.title).collect();
        assert!(titles.contains(&"A".to_string()) && titles.contains(&"B".to_string()));
    }
}
```

- [ ] **Step 2: Run, watch fail**

Run: `distrobox enter my-distrobox -- bash -c 'cargo test -p notes doc::tests'`
Expected: FAIL (NotesDoc undefined).

- [ ] **Step 3: Implement `doc.rs`**

Implement `NotesDoc` with the get-or-create helpers. Store `title` as a `Text` object (so titles are collaborative), `parent` as a string-or-absent scalar, `order` as an integer. `list_pages` iterates the `pages` Map keys, reading each child Map's fields. (Match the patterns in `apps/notes/src/storage.rs::encode_workspace` for Automerge calls.)

- [ ] **Step 4: Run green**

Run: `distrobox enter my-distrobox -- bash -c 'cargo test -p notes doc::tests'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C apps add notes/src/doc.rs notes/src/lib.rs
git -C apps commit -m "feat(notes): Automerge document model for the page tree"
```

### Task 3: Re-enable CRDT backing in `RichTextLine`/`TextInput`

**Files:**
- Modify: `ui/src/widget/rich_text_line.rs` (path-aware `sync`; thread `obj_id` to edits)
- Modify: `ui/src/widget/text_input.rs` (ensure `sync(state, obj_id)` reads the bound Text; expose a setter to bind a block's `Text` ObjId/path)
- Test: `ui/src/widget/rich_text_line.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `automerge::{AutoCommit, ObjId}`; `NotesDoc` block ObjIds (from Task 2) are passed in by the editor (Task 4).
- Produces:
  - `RichTextLine::bind_text(&mut self, obj_id: ObjId)` — bind this line to an Automerge `Text`.
  - `RichTextLine::sync(&mut self, state)` pulls the bound Text into the in-memory string.
  - Edits (`insert_str`/`delete_selection`/backspace/delete) splice into the bound Text.

- [ ] **Step 1: Write failing test — a bound line mirrors the Automerge Text**

```rust
#[test]
fn bound_line_reads_text_from_doc() {
    let mut doc = automerge::AutoCommit::new();
    let tid = doc.put_object(&automerge::ROOT, "t", automerge::ObjType::Text).unwrap();
    doc.splice_text(&tid, 0, 0, "hello").unwrap();
    let mut line = RichTextLine::paragraph("");
    line.bind_text(tid.clone());
    line.sync(&mut doc);
    assert_eq!(line.text(), "hello");
}
```

- [ ] **Step 2: Run, watch fail.** `cargo test -p ui rich_text_line` → FAIL.

- [ ] **Step 3: Implement.** Add a `bound: Option<ObjId>` to `RichTextLine`; `bind_text` sets it; `sync` (no longer a no-op) reads `state.text(&obj_id)` into the inner `TextInput`'s string and clamps the caret; route the inner `TextInput`'s edit calls with `Some(&obj_id)` so they splice. (The inner `TextInput::insert_str`/`delete_selection` already accept `Option<&ObjId>`.)

- [ ] **Step 4: Run green.** `cargo test -p ui rich_text_line` → PASS.

- [ ] **Step 5: Commit.**
```bash
git -C ui add src/widget/rich_text_line.rs src/widget/text_input.rs
git -C ui commit -m "feat(ui): RichTextLine can bind to an Automerge Text (re-enable CRDT path)"
```

### Task 4: `BlockEditor::sync` reads blocks from the doc; edits hit per-block Text

**Files:**
- Modify: `ui/src/widget/block_editor.rs` (`sync` reads the blocks List; bind each block's RichTextLine; handle add/remove)
- Test: `ui/src/widget/block_editor.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: a blocks-List `ObjId` (the editor is told where its page's blocks live).
- Produces:
  - `BlockEditor::bind_blocks(&mut self, list: ObjId)` — bind the editor to a `pages[id]["blocks"]` List.
  - `BlockEditor::sync(&mut self, state)` rebuilds/refreshes blocks from that List, binding each block's text.
  - Local edits splice into the per-block Text (via Task 3).

- [ ] **Step 1: Failing test — editor mirrors a doc-built block list**

```rust
#[test]
fn editor_syncs_blocks_from_doc() {
    let mut doc = automerge::AutoCommit::new();
    let list = doc.put_object(&automerge::ROOT, "blocks", automerge::ObjType::List).unwrap();
    // one paragraph block: { kind:"paragraph", text:"hi" }
    let b0 = doc.insert_object(&list, 0, automerge::ObjType::Map).unwrap();
    doc.put(&b0, "kind", "paragraph").unwrap();
    let t0 = doc.put_object(&b0, "text", automerge::ObjType::Text).unwrap();
    doc.splice_text(&t0, 0, 0, "hi").unwrap();
    let mut ed = BlockEditor::new();
    ed.bind_blocks(list);
    ed.sync(&mut doc);
    assert_eq!(ed.blocks().len(), 1);
    assert_eq!(ed.blocks()[0].text(), "hi");
}
```

- [ ] **Step 2: Run, watch fail.** `cargo test -p ui block_editor` → FAIL.

- [ ] **Step 3: Implement `sync`.** Read the List length; for each index read the block Map's `kind` + `text` ObjId; create/refresh the `Block` and `bind_text` its line; drop trailing blocks if the list shrank. Keep block identity positional in-memory but driven by the List. New local blocks (Enter key) `insert_object` into the List; deletes `delete` from the List. Thread the bound List through `MainPanel`/`NotesApp` in Task 6.

- [ ] **Step 4: Run green.** `cargo test -p ui block_editor` → PASS.

- [ ] **Step 5: Commit.**
```bash
git -C ui add src/widget/block_editor.rs
git -C ui commit -m "feat(ui): BlockEditor.sync reads blocks + text from the CRDT doc"
```

### Task 5: Connect-time presence + late-join full-state handshake in `ui::App`

**Files:**
- Modify: `ui/src/app.rs` (emit `Arrived` on connect; reply with full `SyncCRDT` on inbound remote `Arrived`)
- Test: `ui/tests/` (in-memory relay test, model on existing `support::MemoryRelay`)

**Interfaces:**
- Consumes: `CollaborationManager`, `PresencePayload::Arrived`, `State::save`.
- Produces: a new peer that connects after document creation receives current state.

- [ ] **Step 1: Failing test — late joiner converges**

Using the in-memory relay support: peer A creates content, peer B "arrives", assert B ends with A's content after the Arrived→SyncCRDT exchange is pumped.

- [ ] **Step 2: Run, watch fail.**

- [ ] **Step 3: Implement.** On socket connect (where `Hello` is sent), also `broadcast_presence(App-scope, Arrived)`. In the `RemotePresenceUpdate{ Arrived }` arm of `apply_remote_presence`/the poll loop, send `CollabRequest::SyncCRDT { data: self.state.save() }`. Guard against storms with a simple "only reply if we have non-empty content" check.

- [ ] **Step 4: Run green.**

- [ ] **Step 5: Commit.**
```bash
git -C ui add src/app.rs tests/
git -C ui commit -m "feat(ui): emit Arrived on connect + reply full state to late joiners"
```

### Task 6: Wire notes app onto the doc; move `selected` to per-peer; run two instances

**Files:**
- Modify: `apps/notes/src/app.rs` (build workspace from `NotesDoc`; `selected` local; on page switch broadcast `Tab{page_id}` Arrived; set presence scope)
- Modify: `apps/notes/src/main_panel.rs` (`load_page` binds the page's blocks List from the doc instead of parsing disk markdown)
- Modify: `apps/notes/src/main.rs` (set actor + `UserId` from `QUICK_APP_ID`; ensure sync loop runs)
- Test: `apps/notes/tests/two_peer_sync.rs`

**Interfaces:**
- Consumes: Task 2 `NotesDoc`, Task 4 `BlockEditor::bind_blocks`, Task 5 handshake.
- Produces: a runnable two-instance demo with live text sync.

- [ ] **Step 1: Failing test — two in-memory peers converge on body text, independent selection**

```rust
// apps/notes/tests/two_peer_sync.rs
// Build two NotesApp-like states sharing via MemoryRelay (or drive doc.rs directly +
// BlockEditor). Peer A types into page P; pump relay; assert peer B's page P body
// contains the text. Assert each peer's local `selected` can differ.
```

- [ ] **Step 2: Run, watch fail.**

- [ ] **Step 3: Implement.** Replace `Workspace.selected`-in-doc with a local `selected: Option<PageId>` field on `NotesApp`. `sync()` rebuilds the sidebar's page list from `NotesDoc::list_pages`. `apply_event(Select)` sets local `selected`, binds `MainPanel` to that page's blocks List, and `broadcast_presence(Tab{page_id}, Arrived)`. `main.rs` sets `doc.set_actor(...)` and a `UserId` from `QUICK_APP_ID`, and `app.set_presence_broadcast_scope(Some(App{Notes, ""}))`.

- [ ] **Step 4: Run green (test).**

- [ ] **Step 5: Manual two-instance smoke**

```bash
distrobox enter my-distrobox -- bash -c 'cargo build -p notes -p collab-broker'
./target/debug/collab-broker /tmp/notes.sock &
QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=alice ./target/debug/notes &
QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=bob   ./target/debug/notes
```
Expected: typing in one window appears in the other; each can view a different page. (If no display is available, assert via the two-peer test instead and note it.)

- [ ] **Step 6: Commit.**
```bash
git -C apps add notes/src
git -C apps commit -m "feat(notes): back document with Automerge doc; per-peer selected; live two-peer text sync"
```

---

## Phase 2 — Core presence (caret, selection, round pointer)

### Task 7: Round remote pointer from `PresenceCtx`

**Files:**
- Modify: `ui/src/presence/render.rs` (add `draw_peer_pointer`)
- Modify: `ui/src/app.rs` (draw pointers from `PresenceCtx.pointers`/`remote_cursors`, round, replacing the arrow BezPath)
- Test: `ui/src/presence/render.rs` `#[cfg(test)]` (offscreen) or a pixel assertion test.

**Interfaces:**
- Produces: `pub fn draw_peer_pointer(scene, theme, user: &UserId, ctx: &PresenceCtx, pos: (f32,f32))` — filled circle + halo + name label in `ctx.color_for(user)`.

- [ ] Step 1: Failing offscreen test — a peer pointer paints the peer's color near `pos`.
- [ ] Step 2: Run, watch fail.
- [ ] Step 3: Implement `draw_peer_pointer` (a `vello` circle via `kurbo::Circle`); replace the arrow in `app.rs` and source positions from `PresenceCtx`.
- [ ] Step 4: Run green.
- [ ] Step 5: Commit (`feat(ui): round color-coded remote pointer from PresenceCtx`).

### Task 8: Remote caret + selection with real metrics in `BlockEditor`

**Files:**
- Modify: `ui/src/widget/block_editor.rs` (draw remote caret/selection per block using `RichTextLine::pixel_x_for_byte`)
- Modify: `ui/src/widget/text_input.rs` (delete the fake `advance = 8.0` remote path; use `char_x_positions`)
- Modify: `apps/notes` — broadcast local `Caret`/`Selection` presence on caret/selection change, keyed by the focused block's widget key.
- Test: offscreen snapshot in `ui` asserting a remote caret rect lands at the same x as the local caret for the same offset.

**Interfaces:**
- Consumes: `PresenceCtx.carets`/`selections` keyed by the block's `PresenceKey::Widget`.
- Produces: remote carets/selections rendered at real glyph positions.

- [ ] Step 1: Failing test — remote caret x == local caret x for offset N (via `pixel_x_for_byte`).
- [ ] Step 2: Run, watch fail.
- [ ] Step 3: Implement: in `BlockEditor::draw`, for each block with a presence key, look up remote carets/selections and draw via `presence.draw_peer_caret(scene, theme, user, rect)` where `rect` comes from `block.line.pixel_x_for_byte` + block y. Remove the `advance=8.0` code in `text_input.rs`. Add the local broadcast of `Caret{pos}`/`Selection{start,end}` in the notes edit/caret-move path.
- [ ] Step 4: Run green.
- [ ] Step 5: Commit (`feat(ui): real-metric remote caret + selection in BlockEditor`).

---

## Phase 3 — Authorship

### Task 9: `Mark::Author` data model + stamping

**Files:**
- Modify: `ui/src/widget/marks.rs` (add `Author(UserId)`)
- Modify: `ui/src/widget/text_input.rs` (stamp author at insert sites; persist as Automerge mark on the bound Text)
- Test: `ui/src/widget/text_input.rs` `#[cfg(test)]` — author mark survives `save`/`load` and resolves per range.

**Interfaces:**
- Consumes: a local `UserId` set on the editor (from `QUICK_APP_ID`).
- Produces: `Mark::Author(UserId)` ranges queryable for rendering.

- [ ] Step 1: Failing test — insert "abc" as alice → range 0..3 has `Author("alice")` after doc round-trip.
- [ ] Step 2: Run, watch fail.
- [ ] Step 3: Implement: add the variant; thread a `local_author: Option<UserId>` into `TextInput`; at `insert_str` splice sites also `state.mark(...)` (Automerge marks API) over the inserted range with the author; reconstruct on `sync` via `state.marks(&obj_id)`.
- [ ] Step 4: Run green.
- [ ] Step 5: Commit (`feat(ui): Mark::Author stamped + persisted as Automerge marks`).

### Task 10: Render authorship underlay

**Files:**
- Modify: `ui/src/presence/render.rs` (`draw_peer_authorship`)
- Modify: `ui/src/widget/block_editor.rs` (paint foreign-author runs under glyphs)
- Test: offscreen — a run authored by bob shows bob's color underlay; my own runs show none.

- [ ] Step 1: Failing offscreen test.
- [ ] Step 2: Run, watch fail.
- [ ] Step 3: Implement `draw_peer_authorship(scene, theme, user, rects)`; in `BlockEditor::draw`, group consecutive chars by author mark, build rects via `pixel_x_for_byte`, skip runs authored by the local user.
- [ ] Step 4: Run green.
- [ ] Step 5: Commit (`feat(ui): authorship marker underlay in BlockEditor`).

---

## Phase 4 — Wayfinding (focus glow + indicative mist)

### Task 11: Focused-element glow

**Files:**
- Modify: `ui/src/presence/render.rs` (`draw_peer_focus_glow`)
- Modify: `ui/src/widget/block_editor.rs` (glow the block a remote peer is focused in, via `PresenceKey::Widget`)
- Test: offscreen — peer present on block 2's widget key → block 2 has a colored glow ring.

- [ ] Step 1: Failing offscreen test.
- [ ] Step 2: Run, watch fail.
- [ ] Step 3: Implement glow helper (stroked rounded rect in peer color, low alpha) + lookup of "who is focused here" from `PresenceCtx`.
- [ ] Step 4: Run green.
- [ ] Step 5: Commit (`feat(ui): remote focused-element glow`).

### Task 12: Indicative mist on sidebar rows (in-app page wayfinding)

**Files:**
- Modify: `ui/src/presence/render.rs` (`draw_peer_mist`)
- Modify: `apps/notes/src/sidebar.rs` (mist on rows whose `PageId` has a remote peer present via `PresenceKey::Tab{id}`)
- Test: `apps/notes` offscreen — peer present on page X's `Tab` key → that row glows with mist; verify the demo end-to-end visual via offscreen render.

**Interfaces:**
- Consumes: `PresenceCtx` "who is present on `Tab{page_id}`"; the sidebar's `row_rects[i]` + `flat_rows[i].id`.
- Produces: the wayfinding glow that closes the "who is where" loop.

- [ ] Step 1: Failing offscreen test — `Tab{page_X}` has remote peer ⇒ row for X painted with mist in peer color.
- [ ] Step 2: Run, watch fail.
- [ ] Step 3: Implement `draw_peer_mist` (soft radial / blurred fill, low alpha); in `Sidebar::draw`, for each row, query peers on `Tab{row.id}` and paint mist on `row_rects[i]`.
- [ ] Step 4: Run green.
- [ ] Step 5: Commit (`feat(notes): indicative mist on sidebar rows for remote peers' pages`).

### Task 13: End-to-end demo verification + docs

**Files:**
- Create: `apps/collab-broker/README.md` (how to run the demo)
- Modify: `docs/superpowers/specs/2026-06-24-collaborative-presence-design.md` (mark window-group mist as the documented shell extension)

- [ ] Step 1: Run the full two-instance demo (broker + 2 notes), confirm: text sync, round pointer, caret, selection, authorship underlay, focus glow, and page-switch mist all visible. If headless, produce offscreen snapshots of each.
- [ ] Step 2: Write the README run recipe.
- [ ] Step 3: Commit (`docs: collab demo run recipe + window-group mist as shell extension`).

### Task 14: Superproject pointer bump

- [ ] Step 1: Bump submodule pointers (`ui`, `apps`) in the superproject and commit.
```bash
git add ui apps
git commit -m "chore: bump ui + apps for collaborative presence (notes demo)"
```

---

## Self-Review

**Spec coverage:** §0 transport → Task 1; §1 shared/per-peer → Task 6; §2 CRDT doc → Tasks 2-4,6; §3 authorship → Tasks 9-10; §4 five primitives → Tasks 7,8,10,11,12; §5 indicative mist → Task 12 (+window-group documented in Task 13); §6 broker + late join → Tasks 1,5; §7 integration → Task 0; §8 testing → tests in every task; §9 phasing → phases 1-4.

**Placeholder scan:** Phase 1 tasks carry full code/tests. Phases 2-4 give exact files, interfaces, test intent, and step structure; the implementer reads the real widget code (now present in-worktree) and fills the offscreen-snapshot bodies modeled on existing `OffscreenRenderer` tests — these are render primitives where the exact pixel-assert helper is taken from the existing snapshot tests, not invented.

**Type consistency:** `user_id: String` everywhere on the wire; `UserId(String)` in `presence`; `PageId = Uuid`; `bind_text(ObjId)` / `bind_blocks(ObjId)` consistent between Tasks 3, 4, 6; `draw_peer_*` helpers all take `(scene, theme, user, …)`.
