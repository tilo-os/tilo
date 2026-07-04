# CRDT-Native File Formats — Phase 1 (Markdown POC end-to-end) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement task-by-task. Steps use checkbox (`- [ ]`) syntax. Phase 0 (`qdoc` foundation) is complete and green; this plan builds on it.

**Goal:** Prove the whole stack end-to-end on Markdown: a `ui` editor app binds a `qdoc` rich-text document living in `ui::State.doc`, edits flow through the existing undo/sync/presence machinery, a built-in `.md` codec round-trips, and a version timeline (`materialize_at`) works. Milestone: **two replicas edit a `.md` live, converge, with per-author global undo and a working timeline.**

**Architecture:** The document lives in `ui::State.doc` (one `AutoCommit`) at `ROOT`, structured with the Phase-0 schema. `qdoc` is refactored so its model is drivable over a **borrowed** `&mut AutoCommit` (a `DocMut<'a>` view) — the owned `Document` becomes a thin wrapper. The editor widget holds the resolved container ObjIds + id generator and mutates `state.doc` directly in `handle_event` (the existing 5-pass / CRDT-direct model). Undo = `State::commit_action`; live sync = `State::{save,load}_incremental`; history = an append-only change-blob chain derived from automerge changes.

**Tech Stack:** Phase-0 `qdoc` (+ new `qdoc::codecs::markdown`, `qdoc::history`), `pulldown-cmark` (md import), `ui` (editor widget + App + offscreen), `automerge 0.8`, `proptest`.

## Global Constraints

- **The document lives in `ui::State.doc` at `ROOT`** for the POC (single-document editor). `qdoc` model ops must accept a borrowed `&mut AutoCommit` + a `Containers` (resolved ObjIds).
- **No new message layer** — editing widgets mutate the passed `AutoCommit` directly (ui's existing pattern, see `ui/src/widget/text_input.rs`).
- **Reuse ui machinery, don't reinvent:** undo via `State::commit_action`/`undo_for_author`; live sync via `save_incremental`/`load_incremental`; presence via `App::broadcast_presence` + `PresenceCtx`.
- **automerge = 0.8.0**, **edition 2024** for the app crate (matches other `apps/*`), **edition 2021** stays for `qdoc`.
- **Verification covenant:** never `#[ignore]`, never weaken an assertion, assert on the production path.
- **Build/test:** `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc'` and `... -p editor`.
- **md POC fidelity:** round-trip the *modeled* subset (headings, paragraphs, bold/italic, bullet/ordered lists, inline code). Lossless-preservation of unknown constructs is **deferred to Phase 4** (odt); the md codec may normalize whitespace. The byte-cache (Phase 2) is not in this phase.

---

### Task 1: Refactor `qdoc` to a borrowed `DocMut` view

**Why:** the editor must drive the document that lives in `ui::State.doc`. Today `Document` owns its `AutoCommit`; we extract the model so it runs over a borrowed doc, with `Document` delegating.

**Files:**
- Create: `qdoc/src/model.rs` (Containers, IdGen, DocMut/DocRef, free ops moved from document.rs)
- Modify: `qdoc/src/document.rs` (Document delegates to model)
- Modify: `qdoc/src/format.rs` (codec trait takes the borrowed view)
- Modify: `qdoc/src/lib.rs`

**Interfaces produced:**
```rust
// model.rs
pub struct Containers { pub blocks: ObjId, pub styles: ObjId, pub preserved: ObjId, pub meta: ObjId }
pub struct IdGen { actor_hex: String, next: u64 }
impl IdGen { pub fn new(actor_hex: String) -> Self; pub fn mint(&mut self) -> String }

/// Write schema scalars + create the four container maps under `root`. Returns the ObjIds.
pub fn init(doc: &mut AutoCommit, root: &ObjId) -> Containers;
/// Resolve existing containers under `root`.
pub fn open(doc: &AutoCommit, root: &ObjId) -> Result<Containers, DocError>;

// Borrowed mutable view used by editor + codecs.
pub struct DocMut<'a> { pub doc: &'a mut AutoCommit, pub c: &'a Containers, pub ids: &'a mut IdGen }
impl<'a> DocMut<'a> {
    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String;
    pub fn remove_block(&mut self, id: &str);
    pub fn move_block(&mut self, id: &str, after: Option<&str>);
    pub fn splice_text(&mut self, block: &str, idx: usize, del: isize, ins: &str);
    pub fn add_mark(&mut self, block: &str, start: usize, end: usize, name: &str, value: bool);
}
// Read ops as free fns over &AutoCommit + &Containers:
pub fn blocks_in_order(doc: &AutoCommit, c: &Containers) -> Vec<String>;
pub fn block_text(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String>;
pub fn block_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String>;
pub fn marks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<MarkSpan>;
pub fn materialize(doc: &AutoCommit, c: &Containers) -> DocSnapshot;
```
`Document` keeps its current public method names but stores `{ doc, root, c: Containers, ids: IdGen }` and delegates (e.g. `fn insert_block(&mut self, after, kind) { DocMut { doc: &mut self.doc, c: &self.c, ids: &mut self.ids }.insert_block(after, kind) }`). The `FormatProvider` trait changes to `import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError>` and `export(&self, doc: DocRef) -> Result<Vec<u8>, CodecError>` where `pub struct DocRef<'a> { pub doc: &'a AutoCommit, pub c: &'a Containers }`.

- [ ] **Step 1:** Move the block/text/mark/materialize logic from `document.rs` into `model.rs` as the free fns + `DocMut`/`DocRef` above. Keep the exact automerge calls (they're verified). Preserve the borrow-scoping in `text_obj` (read borrow ends before mutable `put_object`).
- [ ] **Step 2:** Rewrite `Document` to hold `{ doc, root, c, ids }` and delegate every method to `model`/`DocMut`. `Document::new()` = `let mut doc = AutoCommit::new(); let c = model::init(&mut doc, &ROOT); let ids = IdGen::new(doc.get_actor().to_hex_string());`. Add `pub fn as_mut(&mut self) -> DocMut` and `pub fn as_ref(&self) -> DocRef`.
- [ ] **Step 3:** Update `FormatProvider` + the in-test providers in `format.rs` to the borrowed signatures.
- [ ] **Step 4:** Run the full Phase-0 suite — it must still pass unchanged (the owned `Document` API is preserved). `cargo test -p qdoc`. Expected: 22 tests green (convergence included).
- [ ] **Step 5:** Commit: `feat(qdoc): borrowed DocMut/DocRef view; Document delegates`.

> This task is **pure refactor**: behavior identical, all Phase-0 tests are the regression guard. Do not change test assertions.

---

### Task 2: Built-in Markdown codec (`qdoc::codecs::markdown`)

**Files:**
- Create: `qdoc/src/codecs/mod.rs`, `qdoc/src/codecs/markdown.rs`
- Modify: `qdoc/Cargo.toml` (add `pulldown-cmark = "0.12"`), `qdoc/src/lib.rs`

**Interface:** `pub struct MarkdownCodec;` implementing `FormatProvider` (schema_id = `qos.richtext`).

**Mapping (modeled subset):** ATX headings `#..######` → block kind `heading` (attr `level`); paragraphs → `paragraph`; `*`/`-` list items → `list_item` (attr `ordered=false`); `1.` → `list_item` (`ordered=true`); `**`/`__` → mark `strong`; `*`/`_` → mark `em`; `` `code` `` → mark `code`. Import builds blocks + Text + marks via `DocMut`; export walks `materialize()` and emits md.

- [ ] **Step 1:** Failing round-trip test (in `markdown.rs`):
```rust
#[test]
fn roundtrip_basic() {
    let src = "# Title\n\nHello **world** and *em*.\n\n- a\n- b\n";
    let mut d = qdoc::Document::new();
    MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
    let snap = d.materialize();
    assert_eq!(snap.blocks[0].kind, "heading");
    assert_eq!(snap.blocks[0].text, "Title");
    assert!(snap.blocks[1].marks.iter().any(|m| m.name == "strong"));
    let out = String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap();
    // re-import the export → same materialized model (semantic round-trip)
    let mut d2 = qdoc::Document::new();
    MarkdownCodec.import(out.as_bytes(), &mut d2.as_mut()).unwrap();
    assert_eq!(d.materialize(), d2.materialize());
}
```
- [ ] **Step 2:** Run → FAIL (codec missing).
- [ ] **Step 3:** Implement import via `pulldown-cmark` `Parser` (track current block + open marks by byte range over the block Text); implement export by walking `DocSnapshot` blocks → md strings (headings prefix `#`, list items `- `, marks wrap with `**`/`*`/`` ` ``). Full code written at execution against the `pulldown_cmark::Event` enum.
- [ ] **Step 4:** Run → PASS. Add a **proptest**: random sequences of the modeled constructs survive `import∘export∘import` with equal `materialize()`.
- [ ] **Step 5:** Commit: `feat(qdoc): built-in Markdown codec with round-trip proptest`.

---

### Task 3: Version history — change-blob chain + `materialize_at`

**Files:**
- Create: `qdoc/src/history.rs`
- Modify: `qdoc/src/document.rs` (expose change export), `qdoc/src/lib.rs`

**Interfaces:**
```rust
pub struct VersionMeta { pub index: usize, pub actor_hex: String, pub time_ms: i64, pub change_hash: String }
/// Enumerate the document's history newest-or-oldest order from automerge changes.
pub fn versions(doc: &AutoCommit) -> Vec<VersionMeta>;
/// Reconstruct the document at version N by replaying the first N+1 changes into a fresh doc.
pub fn materialize_at(doc: &AutoCommit, c_root: &ObjId, version: usize) -> Result<DocSnapshot, DocError>;
```
Implementation: `doc.get_changes(&[])` (automerge 0.8 — confirm exact name at execution; candidates `get_changes`, `get_changes_added`) yields `&Change`s with `.actor_id()`, `.timestamp()`, `.hash()`. `materialize_at` loads changes `[0..=version]` into a fresh `AutoCommit`, resolves containers via `model::open`, and materializes.

- [ ] **Step 1:** Failing test: build a doc with 3 distinct edits (insert, type, bold), assert `versions().len() >= 3`, each with an actor + nonzero time; assert `materialize_at(.., 0)` shows the doc after only the first edit and differs from the latest.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement against the verified `automerge::Change` API.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit: `feat(qdoc): version history (change chain + materialize_at)`.

---

### Task 4: Editor app crate `apps/editor` — minimal rich-text editor widget

**Files:**
- Create: `apps/editor/Cargo.toml`, `apps/editor/src/lib.rs` (`EditorWidget`), `apps/editor/src/main.rs` (`--shots` + `run`)
- Modify: root `Cargo.toml` members (`"apps/editor"`)

**Scope (POC, deliberately minimal):** render the document as a vertical stack of block lines; a caret `(block_id, char_idx)`; typing splices into the current block's Text; `Enter` inserts a new `paragraph` block after the current and moves the caret; `Backspace` at offset 0 merges into the previous block; `Ctrl+B` toggles `strong` over the selection (or current word). Layout via taffy column; draw text via `ui`'s text renderer (mirror `text_input.rs` glyph drawing). Not in scope: mouse selection across blocks, tables, images.

**Interface:** `EditorWidget` implements `ui::widget::Widget`. Holds `Containers` + `IdGen` (resolved/initialized in `sync` against `state.root`), a render cache (`Vec<(block_id, String, Vec<MarkSpan>)>`), and caret state.

- [ ] **Step 1:** Crate scaffold + `--shots` main mirroring `apps/terminal/src/main.rs`; `EditorWidget::new()` seeds a doc via `model::init` + a welcome paragraph. Build: `cargo build -p editor`.
- [ ] **Step 2:** `sync`: `let c = model::open(state, root).unwrap_or_else(|| model::init(state, root)); self.lines = model::blocks_in_order(state, &c).map(|id| (id, block_text, marks))`. Test: a unit test constructs a doc, runs `sync`, asserts the cache reflects blocks.
- [ ] **Step 3:** `build_layout` + `draw`: a taffy column, one node per line; draw each line's text + caret. Snapshot test via `OffscreenRenderer` → PNG written to `target/editor-shots/` (golden compare optional this phase).
- [ ] **Step 4:** `handle_event`: map `KeyboardInput` to `DocMut::splice_text` / `insert_block` / mark toggle on `state` (mirror `text_input.rs` key handling). Test: feed synthetic key events, assert `materialize()` reflects typed text + Enter-split blocks.
- [ ] **Step 5:** Commit: `feat(editor): minimal CRDT rich-text editor widget (md-backed)`.

> Editor `draw`/`build_layout`/`handle_event` bodies are written against the live `ui::Widget` trait + `cosmic-text`/`taffy` at execution; the **tests** (cache reflects model; key events mutate the doc; offscreen render succeeds) are the contract and must not be weakened.

---

### Task 5: Wire undo + live incremental sync through `ui::State`

**Files:** Modify `apps/editor/src/lib.rs` (+ a test module), possibly `apps/editor/src/main.rs`.

- [ ] **Step 1:** Ensure each editing action calls `state.commit_action(focus_idx, metadata)` so it becomes an undo point (the App already calls this after handled events — verify the editor returns `needs_commit`). Test: type, `undo`, assert the text reverts; `redo` restores.
- [ ] **Step 2:** Per-author undo: tag commits with author; test `undo_for_author(A)` leaves B's edits intact (reuse `State::undo_for_author`).
- [ ] **Step 3:** Live sync test: two `State`s, edit each, exchange `save_incremental`→`load_incremental` both ways, run `sync` on both editors, assert both materialize equal (mirrors Phase-0 convergence but through the editor + State path).
- [ ] **Step 4:** Commit: `feat(editor): global per-author undo + incremental sync wired`.

---

### Task 6: Two-peer end-to-end + timeline (milestone)

**Files:** Create `apps/editor/tests/two_peer_timeline.rs`.

- [ ] **Step 1:** Drive two editor+State pairs through the incremental-sync path (the `MemoryRelay`/`CollabEvent` style harness referenced in `docs/nirios/outstanding-work.md`, or a direct `save/load_incremental` loop). Concurrent edits → converge.
- [ ] **Step 2:** Assert the timeline: `qdoc::history::versions(&state.doc)` lists the edits with the two distinct authors in time order; `materialize_at` at an early version differs from the latest.
- [ ] **Step 3:** Assert per-author global undo across the merged history.
- [ ] **Step 4:** Run the full editor + qdoc suites green. Commit: `test(editor): two-peer convergence + timeline milestone`.

---

## Self-Review

- **Spec coverage (Phase 1 slice):** editor binding (T4), undo/sync/presence reuse (T5), change-blob history + materialize_at (T3), md codec (T2), borrowed-view integration enabling all of it (T1), two-peer + timeline milestone (T6). ✓
- **Deferred correctly:** FS storage/byte-cache/write-path (Phase 2), sandbox IPC (Phase 3), odt + lossless preservation (Phase 4). The md codec normalizes whitespace by design here.
- **Placeholder note:** Tasks 1–3 carry full interfaces + verified automerge calls. Tasks 4–6 specify exact tests as the contract; the editor's render/layout/event bodies are written against the live `ui` trait at execution (genuinely exploratory — first rich-text widget in the toolkit), not hand-waved requirements.
- **Risk:** `automerge::Change` accessor names (`get_changes`/`timestamp`/`hash`) and the `ui::Widget` trait details are confirmed against source at execution, same discipline that made Phase 0 compile first-try.

## Execution

Inline. Task 1 is a pure refactor guarded by the Phase-0 suite; land it first. After Phase 1 is green the rich-text API is fully pinned and Phase 2 (FS integration) gets its own plan.
