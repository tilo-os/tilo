# CRDT-native documents in the collaborative FS — qdoc ↔ fs integration

- **Date:** 2026-06-29
- **Status:** plan (approved direction: model **B** — the FS owns the durable document CRDT)
- **Components:** `fs/` (collaborative filesystem), `qdoc/` (canonical document model + codecs),
  `editor/` + `ui/` collaboration plane (live editing UX)
- **Builds on:** `docs/superpowers/specs/2026-06-29-collab-fs-design.md`

## Problem

Three collaboration layers exist independently:

1. **`fs` — the durable substrate.** Namespace CRDT (tree) + content (content-defined chunks),
   synced **P2P across devices** via iroh, persistent. **Format-agnostic:** a `.odt` is opaque
   bytes, so two concurrent edits produce a `*.sync-conflict-*` copy — no semantic merge.
2. **`qdoc` — the document brain.** A canonical CRDT `Document` (automerge `AutoCommit`:
   blocks / text / marks, `merge()`), bound into *any* host doc via `qdoc::model::open|init(doc,
   root)`, plus a `FormatProvider`/`Registry` codec API (`import(bytes, &mut DocMut)` /
   `export(DocRef) -> Vec<u8>` for odt / docx / markdown). A *library* — no sync of its own.
3. **`editor` + `ui` collab plane — live UX.** `editor` binds a `qdoc` doc into `ui::State.doc`
   and co-edits live over the compositor broker (the notes-demo mechanism): cursors, char-level,
   **while the file is open in a collab-aware app**.

The gap: **a document's CRDT has no durable, cross-device home.** Live editing merges (plane 3),
but the durable layer (plane 1) sees opaque bytes and conflict-copies. Close the file, or edit it
from two devices offline, and the semantic merge is gone.

## Decision

**The FS owns the durable document CRDT (model B).** For `qdoc`-recognized formats, a file's
content *is* a `qdoc`-bearing automerge doc, stored and P2P-synced by `fs`'s existing automerge
engine; `fs` uses `qdoc` as the codec at the byte boundary.

**Responsibility assignment:**

| Layer | Responsibility |
|-------|----------------|
| **`fs`** | Owns durable per-file CRDTs (opaque chunked blobs **and** document automerge docs) and their P2P sync. The single source of truth on disk + across devices. Decides per-file content-kind. |
| **`qdoc`** | The document **model + codecs** the FS calls: recognize formats (`Registry::extensions`), `export` to `.odt`/`.docx` on read, `import` on opaque write, provide the canonical automerge schema (`model::open|init`). Stateless library. |
| **`editor` + `ui` broker** | Live editing UX + presence (cursors). Operates on the canonical doc the FS owns; presence rides the broker as today. |

**Rationale:** `fs` is the only component that sees every file, persists it, and already syncs
CRDTs P2P — the natural home. Documents then merge **semantically, durably, cross-device**, and
even non-collab-aware apps participate (via export/import).

**Honest caveat:** a *dumb* app that rewrites the whole `.odt` on save defeats op-granularity —
the FS must `import` the new bytes and do a coarse (state-level) merge into the canonical doc.
Fine-grained merge therefore requires **`qdoc`-aware editing** (the `editor`, editing the canonical
doc directly). The plan makes that the first-class path, with opaque import/export as a correct,
lossy-but-safe fallback. (No bytes are lost: a failed/coarse merge still yields a `*.sync-conflict-*`
document, never silent loss.)

## Architecture

### Per-file content kinds (FS content plane)

The namespace entry gains a content-kind:

```text
Entry.content =
  | Opaque(manifest_hash)     // chunked content-defined blobs (today's default; binaries, images)
  | Document(doc_id, schema)  // a qdoc-bearing automerge doc, P2P-synced as a CRDT
```

`schema` is the `qdoc` codec id (e.g. `odt`, `docx`, `markdown`), chosen from
`Registry::provider_for_ext(ext)`; `Registry::extensions()` is the source of truth for "this file
is a document." Unknown extensions stay `Opaque`.

### The document doc

A `Document(doc_id, …)` entry references a **per-file automerge doc** (separate from the namespace
doc) with `qdoc` bound at root via `qdoc::model::init`. It is synced by the FS's existing
sync-protocol machinery (the same `generate/receive_sync_message` path used for the namespace),
**on demand** — hydrated when a peer first opens/reads the file, like content blobs but as a CRDT
rather than opaque chunks.

### Data flow

- **FUSE `read`** of a document file → load/hydrate its doc → `provider_for_ext(schema).export(DocRef)`
  → `.odt`/`.docx` bytes. Any tool (`cat`, LibreOffice opening read-only, a thumbnailer) sees a
  valid file (a point-in-time materialization).
- **FUSE `write`** by a non-aware app → `import(new_bytes, &mut DocMut)` into a fork, then
  `Document::merge` into the canonical doc → broadcast the delta. Coarse but convergent.
- **`editor` (qdoc-aware)** → opens the **canonical doc directly** via an FS API (a control
  socket), binds with `qdoc::model::open`, edits with fine-grained ops; edits are CRDT ops on the
  FS-owned doc → durable + P2P-synced immediately. Live cursors/presence ride the `ui` broker.
- **Sync** → document docs sync over the FS P2P plane (durable, cross-device). The broker plane
  remains the live in-cluster presence overlay; P4 bridges them so an in-editor edit and an
  FS-level change are one stream.

## Module changes (`fs/`)

| file | change |
|------|--------|
| `Cargo.toml` | add `qdoc = { path = "../qdoc" }` |
| `namespace.rs` | entry gains `content_kind` (opaque vs document) + `doc_id`/`schema`; create_file picks the kind from a `qdoc::Registry` |
| `docstore.rs` (**new**) | owns per-file `qdoc`-bearing automerge docs: create / load / save / merge / sync-state; export/import via the `Registry` |
| `sync.rs` | extend the sync-protocol to sync document docs (keyed by `doc_id`) in addition to the namespace doc; hydrate-on-open |
| `fs.rs` | `read` → export; `write`/`flush`/`release` for document files → import + merge (instead of chunked ingest) |
| `main.rs` | build a `qdoc::Registry` (register builtin odt/docx/markdown providers); a control socket for qdoc-aware apps (P3) |

`qdoc` and `editor` need no changes for P0–P2 (the FS consumes the existing `FormatProvider` +
`model::open|init` API); P3 adds a small FS-side open API the `editor` calls.

## Phased plan

Each phase lands behind tests (the verification covenant applies — assert on the real path) and
keeps the existing opaque-file behavior intact for non-document files.

- **P0 — recognition.** `fs` depends on `qdoc`; build a `Registry`; tag namespace entries
  `document{schema}` by extension. *Test:* `.odt` → `Document(odt)`, `.png` → `Opaque`.
- **P1 — document docs + sync.** `docstore.rs`: per-file `qdoc` automerge doc; sync it over the
  existing protocol; hydrate-on-open. *Test:* two in-process nodes — A creates a document doc, B
  opens it and converges (extend the existing two-node harness).
- **P2 — transparent export/import (the headline).** FUSE read → `export`; dumb write →
  `import`+`merge`. *Test:* write a `.odt` via the codec on A, read valid `.odt` on B; two peers
  edit the *same* doc → **semantic merge**, not a conflict copy (the property `fs` lacks today);
  e2e: `serve`/`mount`, edit a `.odt` on both, converge.
- **P3 — first-class qdoc-aware path.** FS control socket to open the canonical doc; `editor` binds
  to it. *Test:* editor fine-grained edits land durably in `fs` and sync.
- **P4 — unify sync planes.** Bridge FS-P2P (durable/cross-device) and `ui` broker (live presence)
  for one document. *Test:* an in-editor edit appears on a remote device's mount and vice versa.
- **P5 — history & GC.** Surface `qdoc::Document::versions()` as file history; compact automerge
  history + GC superseded chunks/doc snapshots (the one remaining item from the collab-fs spec).

P0–P2 deliver CRDT-merging `.odt`/`.docx` that any app can open, synced across devices.

## Risks & open questions

- **Import/merge fidelity (P2):** whole-file re-import + state-merge is coarse. Mitigate by making
  the qdoc-aware path (P3) first-class; measure how often dumb-write merges happen in practice.
- **Multi-doc sync scale (P1):** one automerge doc per open document + the namespace doc. Sync
  on-demand (hydrate-on-open), not all-always; reuse the incremental sync-protocol.
- **Two sync planes (P4):** the broker plane and FS-P2P must not double-apply or echo; converge on
  one (likely the FS-owned doc as canonical, broker as a presence-only overlay).
- **Determinism of `doc_id`:** generate like namespace node ids (`author/counter`) so peers agree.
- **Conflict UX:** when a coarse merge can't reconcile, fall back to a `*.sync-conflict-*` document
  (never silent loss) — same guarantee as the opaque plane.

## Out of scope (for now)

Live GUI polish of the `editor` (deferred per `editor`'s own notes); installed/ephemeral codec
tiers beyond the builtin odt/docx/markdown providers; non-document binary merge.
