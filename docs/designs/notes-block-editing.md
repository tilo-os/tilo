# Notes block editing

Audit base: `ac1925a9b5` (2026-10-04). Scope is text and block editing in the
existing Notes app. Databases, AI, accounts and sharing products are outside scope.
Existing code is evidence of a path, not a claim of verified user behavior.

## Sources and conventions

- [Notion editing basics](https://www.notion.com/help/writing-and-editing-basics):
  block handles, conversion, formatting, nesting and slash menus.
- [Notion keyboard reference](https://www.notion.com/help/keyboard-shortcuts):
  Tab nests right, Shift+Tab outdents, Shift+Enter breaks within a block,
  block selection/movement, formatting and link shortcuts.
- [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/): block and inline syntax,
  lists, fences, escaping and links. Notes already uses some extensions; do not
  silently reinterpret existing documents to claim CommonMark conformance.

## Feature and gap matrix

P0 = essential editing correctness; P1 = common feature; P2 = advanced extension.
Unchecked entries need implementation or caller-level qualification.

| Priority | Feature | Existing path | Remaining work |
|---|---|---|---|
| P0 | Paragraphs, H1–H3 | SDK `BlockEditor::try_convert_to`, Notes `notes_editor` | Qualify keyboard conversion, preserve marks/selection |
| P0 | Bullets, numbered lists, tasks | SDK `BlockKind`, Notes Markdown codec | Verify nested completion and numbering |
| P0 | Tab / Shift+Tab | SDK `try_tab_indent`, `try_indent` | Ordinary prose nesting and selected blocks |
| P0 | Enter / Shift+Enter | SDK `apply_signal`, rich-text split/soft-break handling | Verify depth, empty-item exit and atomic undo |
| P0 | Backspace at block start | SDK `apply_signal`, `merge_with_previous` | Outdent/unstyle before joining; preserve both blocks' marks |
| P0 | Bold, italic, underline, strike, inline code | `RichTextLine`, `marks`, rich-text conversion | Qualify multi-block formatting; retain Unicode ranges |
| P1 | Links, autolinks, paste URL on selection | Shared link popover / `insert_link` | Qualify autolinks and URL paste; avoid unsafe targets |
| P1 | Text color / highlight | Canonical marks support colors, editor mapping omits them | Theme-aware controls and round-trip representation |
| P1 | Quotes / dividers | SDK kinds and slash menu | Qualify conversion without accidental content loss |
| P1 | Code fences / language / copy | SDK code groups, keyboard handler, syntax coloring | Preserve authored whitespace, group identity, literal paste |
| P1 | Callouts / toggles | No editor kinds | Define safe canonical representation, export/import and a11y |
| P1 | Slash menu | Shared `SlashMenu` | Expand action commands and aliases |
| P1 | Markdown input shortcuts | SDK promotion and `RichTextLine` input | Audit common prefixes and deliberate differences from Notion |
| P0 | Selection / multiselect | `DocumentSelection`, cross-block text selection | Whole-block selection mode and subtree-aware batch commands |
| P1 | Move / drag and drop | SDK group movement, semantic move controls | Qualify selected groups and boundaries |
| P1 | Duplicate / delete / convert | Canonical insert/delete/style actions; partial editor controls | Reachable whole-block commands preserving metadata |
| P0 | Plain / rich / Markdown paste | Text clipboard and canonical paste vocabulary | Rich clipboard negotiation, formatting and literal/plain modes |
| P0 | Markdown import/export | Notes `markdown`, canonical page projection | Nested/inline/code/table round trips; document extension limits |
| P0 | Undo / redo | `RichTextSurface` → Notes `EditBody` → shared framework history | One structural command = one boundary; per-author concurrent edits |
| P0 | IME / Unicode / caret | Shared text input and projection | Composition must not trigger structural shortcuts; stable selections |
| P0 | Keyboard / accessibility | Shared widget semantic tree and resource actions | Focus escape, read-only refusals, discoverable new commands |
| P1 | Tables as content | Shared source/grid table editor and canonical cells | Qualify cell navigation, paste and round-trip consistency |
| P1 | Find / replace | Shared `open_find`; Notes search provider | Replace absent; selection-safe Unicode replacement |
| P2 | Block anchors / mentions / backlinks | Notes resources, links and backlinks | Qualify reusable block links and cross-note references |

## Implementation sequence

1. [x] Structural indentation for prose and selected blocks, with subtree depth,
   source round trips, canonical Notes actions, undo and concurrent-edit tests.
2. [x] Safe block-start Backspace and merge formatting preservation.
3. [ ] Whole-block duplicate/delete/convert and keyboard/semantic affordances.
4. [ ] Clipboard/inline shortcuts and remaining common block types.
5. [ ] Advanced formatting and replace after their lossless representations exist.

Keep changes in the shared SDK/model, with Notes-only construction and help in
Notes. Reuse canonical `nesting`/list `level` attributes and existing actions;
no schema migration, whole-document replacement, or app-local editor fork.
Existing first-list-item indentation and the depth cap stay compatible.
Synthetic fixtures only. Preserve the running desktop and original notes.

## Verification ledger

Each increment records actual red/green tests, native Notes caller coverage,
WASM compile/browser checks if available, and remaining blockers here. Tests
alone do not establish complete Notion parity.

### Increment 1: structural editing and safety

Implemented in the shared SDK: prose nesting, selection-wide subtree indentation,
common depth limits, Backspace outdent/unstyle before joining, preserving inline
marks on both sides of a Unicode join, composition-key protection, and Escape
then Tab/Shift+Tab to return to host focus navigation. Notes help covers the new
keyboard behavior. Existing canonical nesting attributes and body actions persist
these edits; no schema migration or document replacement is required.

- Red: five actual Notes caller regressions failed before the fixes; four SDK
  regressions failed while 46 existing shared editor tests passed.
- Green: six Notes caller tests pass in the full serial suite; 52 shared editor
  tests pass, including subtree selection, modifier-key focus escape and the
  existing bounded-keystroke-work regression. Independent review is complete.
- Full native Notes suite: 129 passed, one failed, one existing interactive test
  ignored with `--test-threads=1`. The failure is
  `canonical_actions::bound_undo_is_per_instance_and_retypes_once` at line 727.
- Controlled main comparison: all tracked sources were restored to the original
  `ac1925a9b5` for a full test run, then the feature patch was restored. The same
  bound-undo failure reproduces. Parallel runs also expose the existing slash
  conversion revision-count failure (line 204 of `format_preservation`) on both
  versions; it passes isolated and serial. Main additionally failed its timed
  navigation capture under load. No tests or assertions were weakened.
- Native `cargo check --offline --locked -p notes -p tilo-ui --all-targets`: passed.
- Strict `cargo clippy --offline --locked -p notes -p tilo-ui --all-targets -- -D warnings`: passed.
- WASM compilation and the repository-required shell test compile: pending.
- Browser execution is not yet verified. Native checks use the isolated Notes
  caller and synthetic records; the user's running app and notes are untouched.

Still missing: whole-block selection/duplicate/batch actions, rich clipboard
modes, callouts/toggles, color/highlight controls, replace, and the qualifications
listed in the matrix. This increment is not complete Notion feature parity.
