# Browser native platform boundary

The browser host keeps Tilo's widget tree, focus model, semantic actions, and
persistence authoritative. Browser APIs supply native input, composition,
clipboard, focus, accessibility, and asset loading. Two DOM text surfaces have
different jobs:

1. a private hidden editor supplies IME and ordinary canvas-focused text entry;
2. the semantic DOM projection supplies assistive technology and editable DOM
   controls, then dispatches the same semantic actions as native accessibility.

Neither surface owns a second application model. Browser events enter existing
Winit or semantic-action paths, and asynchronous work rechecks the current input
owner before committing an edit.

## Current contracts

- `crates/browser/tilo-browser-runtime/web/host.mjs` owns the canvas, hidden
  editor, input-owner invalidation, and focus handoff.
- `host-core.mjs` classifies keys and trusted clipboard or composition traffic.
- `semantics.mjs` projects roles, names, states, values, selections, live
  regions, and focus, while redacting protected values.
- `src/browser.rs` translates browser input, IME, semantic actions, and clipboard
  transfers into the real frame.
- `crates/sdk/ui-core/src/widget/text_input.rs` remains the canonical selection, composition,
  filtering, cut-ordering, and IME-geometry implementation.

Clipboard reads and writes stay within the trusted browser event. Cut deletes
only after copy succeeds. The asynchronous Clipboard API cannot silently replace
that synchronous guarantee. A revoked or replaced editor must be unable to
commit queued input, composition, or selection changes.

## Required parity work

### Directed selections

The semantic bridge must preserve DOM `selectionDirection`. A backward DOM
selection maps to the original Tilo anchor/focus order, and a canonical backward
selection is restored with `setSelectionRange(start, end, "backward")`. Tests
must cover collapsed, forward, backward, grapheme, emoji, and combining-mark
selections without exposing protected text.

### Dead-key composition

A `Dead` key must not enter the forwarding path that calls `preventDefault()`.
The resulting composition lifecycle produces one commit through the ordinary
IME path. Ownership is checked at composition start and commit, and stale
editors remain replaceable after revocation.

### Real-browser matrix

Keep unit tests as the fast path and maintain a small isolated-browser fixture
for IME, dead keys, paste/copy/cut, semantic focus, backward selection, and the
accessibility tree. It must use the shipped host and a disposable clipboard and
profile rather than a parallel editor implementation.

### Asset staging

Font loading should be measured and staged by required script coverage before a
broader runtime split. Lazy loading is valid only when missing glyphs have a
defined fallback and offline behavior remains intact. Collect request, parse or
compile, and first-presentation timing before changing the WASM boundary.

EditContext may be evaluated later as an optional input adapter. Adoption
requires a supported-browser matrix, complete composition and selection-bounds
round trips, accessibility fallback, and a demonstrated benefit over the
existing textarea adapter. It must not become a separate editor or state owner.

## Review rules

Every browser-boundary change must preserve one authoritative frame and input
owner, deny stale asynchronous commits, keep clipboard work inside trusted
events, retain protected-field redaction and grapheme indexing, and prevent
unapproved origins or document data from escaping through asset selection.
