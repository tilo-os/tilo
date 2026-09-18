# CRDT-native file formats

**Date:** 2026-06-24
**Record type:** Historical design rationale. The `qdoc` crates and their tests are the source of truth for supported formats and behavior.

## Purpose

A format-aware file is represented as a canonical collaborative document rather
than an opaque byte blob. This lets version history, authorship, collaboration,
offline replication, and access control work at the operating-system boundary as
well as in an individual app.

The design deliberately has two layers:

- **Layer 1: generic document substrate.** A canonical Automerge document,
  change history, synchronization, undo, access control, and materialization.
  It works for any registered schema.
- **Layer 2: standard rich text.** A versioned schema shared by the built-in
  text and office codecs so they can render and convert through the same model.

The distinction permits a third-party format to gain generic history and sync
without requiring every app to understand its editing schema.

## Codec and storage boundary

The shared `qdoc` layer owns format schemas, codec registration, and built-in
codecs. The durable file layer owns canonical change storage, materialized bytes,
and historical queries. The shell owns live-session brokering; apps edit and
render document handles rather than inventing a separate message protocol.

A codec is a pure `bytes ↔ validated CRDT changes` transform registered by
extension or MIME type. Parsing is untrusted work: codecs run with no ambient
filesystem or network authority, bounded resources, and host-side validation of
the declared schema before changes reach a canonical document. Provider conflicts
have deterministic precedence and require an explicit user choice where needed.

Foreign byte writes are represented as a labeled, coarse re-import revision.
Native edits retain their normal granular change history. A byte cache serves an
unchanged foreign file verbatim; after native edits, export materializes bytes
from the canonical document. This balances byte preservation with a single
collaborative source of truth.

## Standard rich-text model

The model uses stable block identifiers and fractional order keys so insert,
delete, and move operations merge without fragile tree moves. Inline text uses
Automerge text plus position-anchored marks, not a parallel run list. Lists and
tables use recursively nested block sequences. A shared named-style table enables
format conversion.

Unmodelled data is preserved rather than discarded:

- Whole opaque package parts are carried verbatim.
- Unknown constructs within modelled content become anchored fidelity atoms,
  rendered as non-editable placeholders and emitted again during export.

Concurrent structural table edits remain a deliberately coarse case. The model
must state that limitation rather than pretending that a text CRDT supplies a
complete document-layout conflict resolver.

## History and application contract

Canonical change history is append-only and may be compacted into generic
checkpoints without format-specific policy. Historical materialization produces a
read-only snapshot for timeline and preview consumers. Actor identifiers are bound
to the project's identity model so the durable layer can resolve authorship.

A format-aware app binds a document handle, renders the schema it supports, and
can render a read-only historical snapshot. Undo, synchronization, presence, and
remote cursors operate on the document CRDT, separate from app chrome state.

## Verification and limits

The enduring verification contract is convergence under reordered concurrent
changes; semantic codec round trips and preservation of unsupported content;
sandbox rejection of hostile input and malformed codec output; correct re-import,
history, and cache behavior; and real multi-peer editing over the collaboration
path. UI time-travel uses headless rendering where visual behavior is relevant.

This record does not promise a particular format inventory, byte-identical export
after native edits, perfect concurrent table restructuring, or an implementation
plan for spreadsheets and presentations. Those are product and implementation
scope decisions, not properties implied by the substrate.
