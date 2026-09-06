# Word Processor — current capability and honest gaps

This is a current-state release note, not a speculative backlog. Capability
claims are checked against `apps/word-processor`, `qdoc`, its codecs, focused
tests, rendered states, and the native accessibility audit. Historical wish-list
items belong in version control, not in a status document that can quietly become
false.

## Shipped

- Real page layout: pagination, margins, ruler, indentation, tab stops, line
  spacing, page breaks, headers/footers, page-number fields, columns, zoom and
  horizontal/vertical scrolling.
- Rich content: nested ordered/unordered lists, tables and table editing, images
  with persisted blobs, sizing/flipping/alt text, coarse left/right wrapping,
  footnotes/endnotes, bookmarks/cross-references, comments, tracked changes,
  highlights, super/subscript, TOC generation and named paragraph styles.
- Daily editing: system Open/Save/Save As and image pickers, drag/drop, autosave
  and recovery, undo/redo, find/replace modes, spell checking and correction,
  rich clipboard, context menus, outline navigation, templates, status/readout,
  keyboard shortcuts and IME placement.
- Formats: ODT, DOCX, Markdown, HTML and RTF import/export; PDF and EPUB export.
  PDF and EPUB import is intentionally refused with a clear error rather than
  pretending that a fixed-layout/reader format is safely editable.
- Collaboration: the document is an Automerge record routed by file identity;
  concurrent text/structure changes, authorship, caret/selection presence,
  comments, tracked changes and version history travel through the OS broker.
- Platform integration: theme/high-contrast/large-text aware chrome, Fluent
  English and German UI, responsive wrapped toolbar, narrow-window style rail,
  hierarchical resources, typed replayable actions, and a synthetic document /
  table / toolbar accessibility subtree exported through AccessKit and AT-SPI.

## Remaining release gaps

- Hyperlink targets can be added or changed from the selected-text resource
  action (including `https:`, `mailto:`, and `#bookmark` targets); imported
  links render and round-trip, and bookmarks/cross-references exist. A visual
  toolbar affordance remains a later discoverability enhancement.
- There is no legacy binary `.doc` importer, equation editor, character-style
  authoring, style inheritance/theme designer, format painter, multi-cursor edit,
  split view, minimap, grammar engine, or print-dialog integration.
- Image wrapping is paragraph-granular rather than a per-line exclusion shape;
  crop and freeform float placement are not implemented.
- Large documents reuse shaped layouts incrementally, but the materialized block
  snapshot and pagination walk still cover the whole document each frame. True
  viewport-only layout and streaming import remain performance work.
- Live typing is a framework-owned collaborative edit stream rather than one
  catalogue action per keystroke. Most discrete formatting/structure/review
  operations are typed actions; table row/column mutations, ruler drags and style
  definition edits still need migration from direct CRDT writes to explicit
  typed action payloads before the architecture is fully uniform.
- The exported accessibility subtree exposes readable block/table content and
  actionable toolbar controls. Fine-grained AT text selection/edit commands,
  keyboard roving focus within every immediate-mode panel, and an installed-VM
  AT-SPI smoke run are still required release evidence.
- Comment replies/threads, mentions, share/permission UI, and a document-scoped
  collaborator roster are not yet first-class surfaces. Basic comments,
  resolution, tracked-change decisions and remote presence are implemented.
- Error handling uses in-app toasts for runtime Open/Save failures, but launching
  directly with an invalid CLI path still exits with a launch error rather than
  opening a recoverable error state.

## Evidence commands

```sh
cargo test -p word-processor
cargo clippy -p word-processor --all-targets -- -D warnings
python3 tools/check_app_resource_action_contract.py
cargo run -p word-processor -- --native-audit
```

Representative visual states are generated with `--shots`, plus `--history`,
`--styles`, `--comments`, and `--error`. Use `--viewport WIDTHxHEIGHT` to verify
wide and narrow layouts; `QOS_SHOTS_THEME=light|high-contrast` exercises palette
variants.
