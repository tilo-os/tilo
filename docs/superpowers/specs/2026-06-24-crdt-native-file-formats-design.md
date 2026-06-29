# CRDT-Native File Formats — Design

**Status:** Approved design (brainstorming complete). Implementation plan to follow.
**Date:** 2026-06-24
**Scope owner:** qOS / nirios (`fs`, `ui`, `shell`, system apps)
**Goal for this effort (definition of done):** a **fully implemented, CRDT-native `.odt`** path —
the generic Layer-1 substrate + registration (so other apps can register formats), the Layer-2
rich-text model, FS integration, and the `.odt` codec — i.e. **Phases 0–4 below**, with
**comprehensive testing** throughout. `.docx` (Phase 5) is the documented next step *beyond* this
goal: designed-for here, implemented later.

## 1. Problem & goal

qOS wants files to become *"changes the OS understands"* rather than opaque byte blobs — so
that versioning, global undo, real-time collaboration, offline sync, and access control work as
OS-level paradigms across **any** document, not per-app. The headline target is **full `.odt`
and `.docx` support**, plus a **generic API for adding CRDT support to new filetypes** that
**both the OS/FS and applications** understand — including formats registered by third-party apps.

Today there is **no bridge** between a file and a CRDT:

- `fs/` (`fs/src/fs.rs`) is a **read-only** FUSE projection (`lookup`/`getattr`/`readdir`/`open`/`read`;
  no `write`/`create`). Content is opaque bytes fetched by `blake3` hash via the `Hydrator`.
  The FS has zero understanding of file formats.
- `ui/` (`ui/src/state/mod.rs`) wraps an `automerge::AutoCommit` with full-snapshot undo/redo,
  **per-author** undo (`undo_for_author`), and incremental sync (`save_incremental` /
  `load_incremental`). But this is *live app state* — unconnected to any on-disk file.

This spec defines the bridge: a canonical document model, a generic format-provider API, the FS
integration, and the app binding.

## 2. Locked decisions

These were settled during brainstorming and constrain everything below:

| Decision | Choice | Consequence |
|---|---|---|
| Interop bar | **Lossless-preserving** | Canonical CRDT must reconstruct the original file byte-faithfully; unmodeled content is preserved verbatim. |
| Concurrency | **Real-time multi-user (full)** | Char-level *and* structural merge, presence/remote cursors. |
| External writes | **Versioned re-import** | Foreign byte-writes become one labeled history version (snapshot diff), not char-level merge. |
| v1 formats | **WP only**: Markdown POC → `.odt` → `.docx` | No spreadsheets/presentations in v1. |
| Third-party formats | **Apps may register formats** | Codecs are untrusted; the canonical model becomes a public ABI. |
| Version history | **First-class, FS-exposed** | "Who edited what, when" + a scrub-through-time timeline. |
| History retention | **Auto-coarsen to checkpoints (schema-agnostic)** | Old change-batches fold into periodic checkpoints; policy never per-format. |
| Identity | **Keypair-derived actor→user binding (v1)** | Rich identity model is explicitly out of scope here. |

## 3. Architecture overview — two layers

The constraint *"both the OS/FS and applications understand these"* forces the conversion logic to
be **one shared thing**, and the *"third-party apps register formats"* requirement forces codecs to
be **untrusted, sandboxed**. Together these split the system into two layers:

- **Layer 1 — generic, schema-agnostic.** *A file is an automerge doc.* The OS gives any such
  doc — regardless of internal schema — history/versioning, who/when, sync, offline, global undo,
  collaboration, access control, and byte materialization via a registered codec. This is the
  "generic API for adding CRDT support to new filetypes." A format with a custom schema is still
  versioned/synced/merged for free; only *editing* needs an app that understands its schema.
- **Layer 2 — the standard rich-text schema.** One *well-known, versioned* canonical model that
  `md`/`odt`/`docx` all map onto, so they interoperate, convert to each other, and open in the same
  office/notes app. It is the "standard-library schema" for word-processing — blessed, not exclusive.

```
            ┌─────────────────────────── Layer 1: generic (any schema) ──────────────────────────┐
 file  ==  automerge doc (CANONICAL)  ──►  history/version/sync/undo/collab/access-control
            │                                   ▲                         │ export (sandboxed codec)
            │ persisted as                      │ import (sandboxed codec)│
            ▼                                   │                         ▼
   [ change-blob chain ]              external write (bytes)        [ byte cache ] ──► cat / LibreOffice
   in iroh-blobs content store        => coarse "import" version    (verbatim until qOS edits)
   (the version timeline)

            └────── Layer 2: md/odt/docx all map onto one standard rich-text schema ──────┘
   FS daemon: durable history + materialization   |   shell: live collab sessions   |   app: edit + time-travel scrub
```

**Component placement (the locus split):**

- A shared **`qdoc` crate** holds *everything format-related*: the canonical model, the
  `FormatProvider`/codec trait + registry, and the built-in codecs (md/odt/docx). Both the FS daemon
  and apps link it — this is literally what makes OS and apps "understand" a format: they call the
  same code.
- The **FS daemon** owns canonical storage (iroh-blobs), byte materialization, the FUSE write path,
  versioned re-import, and the durable history/query API.
- The **shell/compositor** owns *live* editing sessions. It is already the collaboration broker
  (`CollabRequest`/`CollabEvent` over the Unix socket), the sandbox authority, and the
  portal/permission authority — a live document is just a file-backed automerge doc it brokers.
- **Codecs run sandboxed** (bubblewrap + seccomp): untrusted byte parsing isolated, output is
  validated CRDT ops only.

**Rejected alternatives:**

- *App-owned library only* — the OS wouldn't understand a file unless an app is open; external
  materialization, multi-app rendezvous on one file, and access-control ownership all break.
- *Codecs embedded in the FUSE daemon* — puts security-sensitive parsing in the privileged
  filesystem process (awful blast radius), bloats the daemon, and drags live-collab into a
  filesystem driver.

## 4. Layer 1 — the generic CRDT-file substrate

Schema-agnostic; true for every registered format, including third-party ones.

- **A file is an automerge document.** Identity on the FS is the logical path; content is the doc.
  Schema is opaque to Layer 1.
- **A format provider is a sandboxed pure transform**, registered as `(ext/MIME) → codec`:

  ```
  trait FormatProvider {
      fn detect(&self, head: &[u8]) -> Confidence;
      fn import(&self, bytes: &[u8]) -> Result<Vec<Change>, CodecError>;   // bytes  → CRDT ops
      fn export(&self, snapshot: &DocSnapshot) -> Result<Vec<u8>, CodecError>; // CRDT  → bytes
      fn schema_id(&self) -> SchemaId;
      fn schema_version(&self) -> u32;
  }
  ```

  No FS/net/other capability — bytes in, CRDT ops out. Built-in providers (md/odt/docx) live in
  `qdoc`; third-party apps register their own via manifest. Precedence:
  **built-in > user-installed > ephemeral**, with explicit user override on conflict.
- **History = an append-only chain of change-blobs** persisted in the existing iroh-blobs
  content store (dedups + syncs for free). The live doc is a compactable checkpoint; the timeline
  reads the chain.
- **Generic coarsening.** A background policy folds old change-batches into periodic checkpoints —
  operates on the automerge change graph, never per-format. Default policy: keep full per-edit
  granularity for the most recent 30 days and the last 200 versions (whichever is larger); older
  history collapses into daily checkpoints. This is one tunable config value, not per-format code.
- **Materialization byte-cache.** The original/last-exported bytes sit beside the doc. External
  reads serve the cache **verbatim**; bytes are regenerated from the CRDT only *after* a qOS edit
  dirties the doc. This keeps an untouched foreign file byte-pristine and sidesteps export
  determinism for the common case.
- **External writes = versioned re-import.** The FS write path (new — FUSE is read-only today)
  takes foreign bytes, runs `import`, and commits the diff as one labeled history entry
  (`external · user · time`). Such entries are coarse (before/after only — no keystroke-level
  authorship); native qOS edits stay fully granular.
- **History/query API lives in the FS daemon**: `list_versions`, `who/when`,
  `materialize_at(version) → DocSnapshot`. Available even when no app is open. Live editing is
  brokered by the shell; the app does the editing + scrub UI.
- **Authorship.** The automerge actor id is bound to the keypair-derived identity (the existing
  peer-salt mechanism, `shell` state), with an actor→user map in doc metadata so the FS can resolve
  names without running the app.

## 5. Layer 2 — the standard rich-text model

The public, versioned schema that `md`/`odt`/`docx` map onto. It must satisfy four things at once:
char-level real-time merge, structural merge, full named styles, and lossless round-trip. Grounded
in proven collaborative-rich-text work (Peritext marks, fractional indexing).

**5.1 Body = a sequence of blocks, ordered by fractional keys.**
Each block has a **stable id** and an **order key** (fractional / LSEQ position). Insert, delete,
and **move** all reduce to setting an order key on a block in an id-keyed map, so concurrent
structural edits merge cleanly. Concurrent *move of the same block* resolves last-writer-wins on its
key (acceptable; the alternative — automerge list move — can duplicate). This delivers "full"
structural merge without fragile tree-move semantics.

**5.2 Inline text = automerge `Text` + marks, not a run tree.**
A text block's content is one `Text` sequence (char-level CRDT → real-time typing merge). Formatting
(bold/italic/link/etc.) is **marks anchored to positions in the Text** (Peritext-style), *not* a
separate list of runs. This is the single most important correctness choice — a parallel run list
corrupts under concurrent edits; marks-over-text does not. Paragraph split/merge are block ops. We
align to automerge's native rich-text primitives (block markers + marks); the exact API is pinned by
the Markdown POC.

**5.3 Nesting = blocks containing block-sequences.**
Lists (level/marker as block attributes) and tables (table → rows → cells, each a block container)
recurse through the same block mechanism. *Honesty flag:* concurrent structural table edits (two
people restructuring the same table) are the gnarliest case — v1 models tables structurally but
same-table concurrent *structure* is expected to be coarse, refined later.

**5.4 Styles = a shared named-styles table.**
`style_id → definition`, referenced by blocks/marks. Maps almost 1:1 onto ODF and OOXML named
styles, which is what lets `.odt` and `.docx` interoperate and convert.

**5.5 The preservation layer** (what makes lossless export possible without modeling all of
OOXML/ODF). Two kinds of unmodeled content:

- **Whole opaque parts** (e.g. `vbaProject`, custom XML, themes, embedded objects, anything not yet
  interpreted) → a `preserved_parts` map, carried verbatim, re-emitted on export.
- **Positioned fidelity-atoms** — an unknown inline/block construct inside a part we *do* model →
  stored as an opaque atom holding the original XML fragment + its anchor, rendered as a
  non-editable placeholder, re-emitted verbatim. Editing *around* it preserves it.

Model the common ~90%; encapsulate the rest as positioned opaque atoms. Combined with the
byte-cache, this is how the originating tool "sees no loss."

## 6. App binding, editing, and time-travel

The win is how little is new — the existing CRDT-direct machinery generalizes.

- **A document is its own automerge doc**, separate from the app's chrome/UI-state doc. Today
  `ui::State` wraps one `AutoCommit`; we generalize so an app holds a `qdoc::Document` handle (an
  `AutoCommit` bound to a file) *alongside* its app-state doc. The 5-pass widget model already
  mutates a CRDT directly — editing widgets target the document doc exactly as they target app state
  today. **No message layer added.**
- **Undo / sync / presence come almost free.** Per-author undo (`undo_for_author`), incremental sync
  (`save_incremental` / `load_incremental`), and remote cursors already operate on an `AutoCommit`;
  point them at the document doc and global undo, real-time collab, and remote carets/marks just
  work. The `commit_action` → snapshot path becomes the **source of the change-blob chain**.
- **Open.** App asks the shell broker "open path P live." Broker loads the canonical doc from the FS
  (or joins an existing live session if peers already hold it open) and returns an `AutoCommit` +
  sync channel. On commit it persists change-blobs to the FS and fans out via the existing
  `CollabEvent` path.
- **Time-travel = a read-only binding.** App requests `materialize_at(version)` → renders the
  snapshot through its normal draw pass with editing disabled. Scrubbing steps `version` along the
  chain; diff-highlight compares two materializations (automerge patch), colored by author.
- **File-explorer preview (separate UX track, designed-for not designed-here).** The TimeShift-style
  scrubbable preview is the *same* `materialize_at` + a **headless offscreen-render hook** (reusing
  the existing `ui` offscreen renderer, `ui/src/offscreen.rs`) — so the explorer rasterizes
  historical versions without launching the editor. This spec keeps that API path open; the explorer
  UX itself is out of scope.
- **Contract for a "format-aware" app**: bind to a `qdoc::Document`, render the standard rich-text
  model, and support a read-only render of a materialized snapshot. The office/notes apps implement
  it; any third-party app adopting the standard schema gets time-travel for free.

## 7. Codec sandbox, IPC, and registration (the trust boundary)

- **Codec runs in the existing per-workspace bubblewrap + seccomp sandbox** — no net, no FS, bounded
  memory/time/output (decompression-ratio, entity-expansion, output-size limits). A hostile-input
  pure function.
- **IPC.** Host (FS or shell) spawns the sandbox and speaks a tiny framed protocol over a pipe:
  `Import { bytes } → { changes | error }`, `Export { doc_snapshot } → { bytes | error }`. The codec
  never gets a live doc handle or any capability — only the bytes/snapshot it needs. Output
  `changes` are **validated** (well-formed automerge changes against the declared schema) before
  touching the canonical doc.
- **Registration.** An app manifest declares
  `provides_format = [{ ext, mime, schema_id, codec_entry }]`. Registered in the same
  automerge-backed workspace tree the shell already maintains; ephemeral apps register per-session,
  "installed" apps persist. Precedence **built-in > installed > ephemeral**, user override on
  collision — surfaced through the existing permission-prompt spine (claiming a sensitive type is a
  grantable capability).
- **Why safe.** Parsing untrusted `.docx` is the classic RCE surface; here the parser is a black box
  whose *only* outputs are validated CRDT ops. A malicious codec can emit garbage content but cannot
  escape, read other files, or reach the network. Reuses qOS's sandbox + portal spine — no new
  boundary invented.

## 8. Phasing (model frozen before the hard codecs)

Each phase is independently testable and shippable. **The model freezes at end of Phase 1**, so the
expensive codec work (Phases 4–5) builds on something proven.

- **Phase 0 — `qdoc` skeleton + canonical model.** Layer-2 schema types, the codec trait, the
  registry. Unit-tested model + merge semantics. No FS/app wiring.
- **Phase 1 — Markdown POC, end-to-end, in-process.** md codec (trivial), editor binding in a `ui`
  app, undo/sync/presence on the document doc, change-blob chain + `materialize_at`. Proves
  Layer-1 + Layer-2 core, pins the automerge rich-text API.
  *Milestone:* two users edit a `.md` live, global undo, working timeline.
- **Phase 2 — FS integration.** Canonical storage in iroh-blobs, materialization byte-cache, the
  **FUSE write path**, versioned re-import, history-query API in the FS daemon.
  *Milestone:* a `.md` on `/var/workspace`; external append re-imports as a version; `cat` reads
  materialized bytes.
- **Phase 3 — Codec sandbox + third-party registration.** Move codecs behind sandbox IPC; manifest
  registration + precedence + permission prompt.
  *Milestone:* md codec runs sandboxed; a dummy third-party format registers and gets history/sync
  for free.
- **Phase 4 — `.odt` codec.** Full ODF import/export + preservation layer (ODF first — clean,
  well-specified).
  *Milestone:* round-trip a real LibreOffice `.odt` losslessly; edit collaboratively.
- **Phase 5 — `.docx` codec.** OOXML import/export + preservation (the messiest). *Beyond this
  effort's goal — designed-for, implemented later.*
  *Milestone:* round-trip a real Word `.docx` losslessly; `.odt` ↔ `.docx` convert through the
  shared model.

### 8.1 Testing strategy (first-class — applies to every phase)

"Lots of testing" is an explicit goal, and this repo's verification covenant applies: **never weaken
an assertion, never `#[ignore]` a failing test, always assert on the production code path** (see
`shell/tests/concept_parity_spec.rs`). Each phase ships with the tests that prove it before moving on.

- **Model & merge (Phases 0–1).** Unit tests for the Layer-2 schema. **Property-based convergence
  tests**: apply randomized concurrent op-sets across N actors in different orders → all replicas
  converge to the same materialized document (the core CRDT guarantee). Targeted tests for the known
  hard cases: concurrent same-block move (LWW), paragraph split/merge, mark overlap.
- **Codec round-trip (Phases 1, 4, 5).** **Property/fuzz round-trip**: `export(import(bytes))` is
  *semantically* equal to `bytes` for a corpus of real documents; for untouched docs the byte-cache
  guarantees byte-identical reads. A **golden-file corpus** of real LibreOffice `.odt` files
  (and later Word `.docx`) committed to the repo, asserted on import→model→export. Preservation
  tests: a doc with unmodeled parts/constructs, edited elsewhere, re-exports those parts verbatim.
- **Sandbox & security (Phase 3).** Malicious-input tests: zip bombs, deeply-nested / billion-laughs
  XML, oversized output, malformed parts — the codec must fail safely within resource limits and
  never escape the sandbox. Validation tests: malformed `changes` from a codec are rejected before
  touching the canonical doc.
- **FS integration (Phase 2).** Write-path tests (partial writes, truncation, `release`/`fsync`
  re-import trigger), versioned-re-import attribution, `materialize_at(version)` correctness, history
  enumeration, and the byte-cache invalidation rule (regenerate only after a qOS edit).
- **End-to-end collaboration (Phases 1–4).** Two-peer live-edit tests over the real collaboration
  socket (extending the existing `MemoryRelay`/`CollabEvent` harness): concurrent edits converge,
  global per-author undo isolates users, remote cursors/marks render, timeline reflects authorship.
- **Visual / headless (where applicable).** Editor + time-travel scrub rendered via the existing
  `shots --mode offscreen` path for golden-image checks; never screenshot a live device.

## 9. Non-goals / out of scope (this spec)

- Spreadsheets (`.ods`/`.xlsx`) and presentations — future format-providers on the same Layer-1 API.
- The file-explorer scrub/preview UX polish (we design the *API* for it, not the UX).
- A rich identity model — v1 uses keypair-derived actor binding only.
- Hardening concurrent structural *table* edits beyond coarse behavior.
- Byte-for-byte identical re-serialization in all cases — the bar is *semantic* losslessness
  (originating tool sees no loss), backed by the byte-cache for untouched files.

## 10. Key risks / things to validate

- **Automerge rich-text API fit.** The marks-over-text + block-markers model must map onto
  automerge's current primitives. Phase 1 (Markdown POC) is the spike that confirms this *before*
  any codec work.
- **Structural merge edge cases.** Fractional-key ordering handles insert/delete/move; concurrent
  same-block move (LWW) and concurrent table-structure edits (coarse) are accepted limitations to
  revisit.
- **Export determinism vs. lossless.** Mitigated by the byte-cache (serve original bytes until first
  qOS edit) + the preservation layer (verbatim re-emit of unmodeled fragments).
- **History storage growth.** Mitigated by the content-addressed change-blob chain + generic
  coarsening to checkpoints.
- **FUSE write path.** A genuinely new capability for `fs/` (currently read-only); needs careful
  design for partial writes, truncation, and the re-import trigger point (e.g. on `release`/`fsync`).

## 11. Glossary

- **Canonical doc** — the automerge document that *is* the file (Layer-1 source of truth).
- **Byte cache** — materialized byte form served to external tools; verbatim until a qOS edit.
- **Change-blob chain** — append-only history of automerge changes in the content store; the timeline.
- **Format provider / codec** — sandboxed pure transform `bytes ↔ CRDT changes` for a given type.
- **Fidelity-atom** — positioned opaque placeholder holding unmodeled original XML for lossless export.
- **Materialize-at** — reconstruct the document model at a historical version (powers time-travel + preview).
- **Standard rich-text schema** — the blessed Layer-2 model `md`/`odt`/`docx` share.
