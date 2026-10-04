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
| P0 | Tab / Shift+Tab | Shared semantic nesting, selected subtrees (increment 1) | Broader convention qualification |
| P0 | Enter / Shift+Enter | SDK `apply_signal`, rich-text split/soft-break handling | Verify depth, empty-item exit and atomic undo |
| P0 | Backspace at block start | Shared outdent/unstyle before joining; both blocks' marks preserved (increment 1) | Broader input-method qualification |
| P0 | Bold, italic, underline, strike, inline code | `RichTextLine`, `marks`, rich-text conversion | Qualify multi-block formatting; retain Unicode ranges |
| P1 | Links, autolinks, paste URL on selection | Shared link popover / `insert_link` | Qualify autolinks and URL paste; avoid unsafe targets |
| P1 | Text color / highlight | Canonical marks support colors, editor mapping omits them | Theme-aware controls and round-trip representation |
| P1 | Quotes / dividers | SDK kinds and slash menu | Qualify conversion without accidental content loss |
| P1 | Code fences / language / copy | SDK code groups, keyboard handler, syntax coloring | Preserve authored whitespace, group identity, literal paste |
| P1 | Callouts / toggles | No editor kinds | Define safe canonical representation, export/import and a11y |
| P1 | Slash menu | Shared `SlashMenu` | Expand action commands and aliases |
| P1 | Markdown input shortcuts | SDK promotion and `RichTextLine` input | Audit common prefixes and deliberate differences from Notion |
| P0 | Selection / multiselect | Cross-block text selection, subtree-aware indent/duplicate/move batches | Dedicated whole-block selection mode and delete/convert commands |
| P1 | Move / drag and drop | Shared atomic selected sibling batches, per-ID group movement and semantic controls (increment 3) | Pointer batch dragging and browser qualification |
| P1 | Duplicate / delete / convert | Shared duplication of focused/selected blocks and subtrees (increment 2); canonical insert/delete/style actions | Whole-block delete/convert batches and pointer menu |
| P0 | Plain / rich / Markdown paste | Text clipboard, canonical paste vocabulary; cuts wait for accepted clipboard writes | Rich MIME negotiation, formatting/literal modes; atomic image replacement |
| P0 | Markdown import/export | Notes `markdown`, canonical page projection | Nested/inline/code/table round trips; document extension limits |
| P0 | Undo / redo | Canonical Notes actions; indent and duplicate atomicity/concurrency caller tests | Independent first-type undo-to-empty failure; remaining command qualification |
| P0 | IME / Unicode / caret | Shared line/table composition guards, Unicode joins, safe copy selection | Browser/OS input-method qualification |
| P0 | Keyboard / accessibility | Focus escape, canonical read-only refusals and semantic duplication | Pointer command menu and browser/native assistive-technology qualification |
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
- Notes `wasm32-unknown-unknown` library compilation: passed using the installed target and existing Zig shims.
- Shell test compilation initially hit the Darwin `tilo-egress` baseline issue. After integrating the existing main fix (PR #2547), shell test compilation, strict Clippy, Notes WASM compilation, 52 SDK tests and six Notes outline tests all passed again at local integration `4211afecccc4`. The independent bound-undo failure remained.
- Merged PR: [#2561](https://code.tilo.so/tilo/tilo/pulls/2561).
- Browser execution is not yet verified. Native checks use the isolated Notes
  caller and synthetic records; the user's running app and notes are untouched.

Still missing: whole-block selection mode, delete/convert/move batches, rich clipboard
modes, callouts/toggles, color/highlight controls, replace, and the qualifications
listed in the matrix. This increment is not complete Notion feature parity.


### Increment 2: canonical block duplication

Ctrl/Cmd+D and semantic actions duplicate a focused block, its nested subtree,
a complete code fence, or all blocks touched by a text selection. The SDK builds
one existing `PasteFragment` action from canonical source views, preserving
Unicode text, marks (including marks the editor does not paint), nesting and
supported attributes. Fresh code-fence identities are carried in the payload;
existing recorded paste actions keep their replay semantics. Selection clears
and focus moves to the copy, so subsequent typing cannot replace the originals.
No schema migration or canonical action change is needed.

Both per-block and selection actions are exposed through the shared accessibility
tree; Notes lists the shortcut in English and German help. Viewers and active IME
composition refuse duplication, including composition in visual table cells.
Unsupported foreign nested structures or floating-point attributes are refused
because the existing clipboard vocabulary cannot represent them losslessly.

- Red: original caller tests exposed absent duplication; later tests caught
  stale selection replacing originals, inaccessible semantic controls, invalid
  code-fence names and structural commands interrupting IME (source and table).
- All eight new mounted-Notes regressions pass, including Markdown export/import
  and distinct exported code-fence assertions. The shared SDK passes 52 block-editor and 34 rich-text tests,
  including its existing bounded-keystroke-work regressions. Independent review
  found no remaining issues after the table-composition fix.
- Full Notes nextest (each test in its own process): 137 passed, one failed,
  one existing interactive test skipped. The sole failure remains
  `canonical_actions::bound_undo_is_per_instance_and_retypes_once` at line 727.
  The same failure was reproduced on unchanged `ac1925a9b5` and the earlier
  `4211afecccc4` main integration before this increment.
- Final strict all-target Clippy for Notes, `tilo-ui` and `tilo-rich-text`, the
  required shell test compilation, and Notes WASM library compilation all pass.
  The final eight caller regressions pass again after test-helper lint fixes.
  Browser execution remains unverified.
- Repository pre-commit checks pass. The affected plan selects 96 of 260 packages
  and 16 test packages with `full=false`; no CI coverage was weakened.
- Passing existing Notes coverage also qualifies: every supported block kind and
  inline marks through byte-stable Markdown round trips (`markdown_*_roundtrip`),
  list indentation, slash conversion/list continuation (`interaction_event_path`),
  soft/hard line breaks (`soft_line_breaks`), group movement with undo/reopen and
  concurrency (`group_moves`), table editing through echo/undo/reload
  (`table_editing`), link navigation (`click_link_navigates`) and in-note find
  (`capture_runtime`). These tests do not establish all Notion shortcut or
  clipboard conventions.
- Pointer duplication menu, whole-block selection mode, batch delete/convert/move,
  rich clipboard, callout/toggle kinds and advanced formatting remain separate
  increments. This is a bounded implementation, not complete Notion parity.

- Merged PR: [#2611](https://code.tilo.so/tilo/tilo/pulls/2611), merge
  `9dd81dde4595a62a36a4b42dd70f4cbad59b3379`. Terminal CI is successful and the
  merge is verified on main.

### Increment 3: selected block movement

Native Alt+Up/Down and the accessible `notes.editor:move-up` / `move-down` actions move
selected sibling roots, complete subtrees and authored fences with one canonical
`MoveBlocks` action. Existing per-ID move controls remain explicitly scoped.
Parent boundaries refuse moves instead of silently changing nesting. Block IDs,
text, marks, nesting and fence identity are preserved.

A completed batch retains exact block IDs, including an empty final root, for
repeated movement. Ordinary text selections still exclude a column-zero end.
Empty retained rows have a themed selection fill and selected accessibility state.
Escape clears the batch and releases Tab/Shift+Tab to host focus navigation.
This retained command selection is not yet a dedicated whole-block selection mode.

If a peer interleaves a block, removes an ID or adds a descendant outside the
retained range, batch commands refuse the incomplete selection. Its text selection
and stale paint collapse, so typing cannot replace newly interleaved peer content.
A new caret, selection or typing gesture leaves this refused batch state.
The refusal stays latched if a later peer edit makes the old IDs contiguous again.

- Red: the actual Notes selected-subtree test failed before implementation.
- Initial green: seven Notes movement tests passed, covering repeated forward and
  reverse selection, column-zero exclusion, empty final roots, canonical atomic
  undo/redo, read-only/IME refusal and concurrent peer text preserved through undo.
- Independent review found missing trailing-descendant validation and stale
  selection lifecycle cases. All were fixed with shared regression tests; the
  final independent review has no remaining findings.
- Final verification: 57 SDK block-editor tests, 34 rich-text tests and all eight
  mounted Notes movement cases pass, including visual-table IME refusal. Strict
  all-target Clippy, required shell test compilation and Notes WASM compilation
  pass. Full Notes nextest: 141 passed, one existing interactive skip, and the
  same baseline first-type undo-to-empty failure documented in increment 2.
- The standard single-app browser builder now recognizes Notes through a direct
  entry function forwarding its existing component definition. The development
  WASM bundle builds successfully offline. Existing compiled MoQ dependency
  output was reused after checking its recorded hash and matching lock/build
  inputs; no dependencies were installed and no browser host was replaced.
- Installed Chrome, a fresh temporary profile and a synthetic note pass actual
  typing, semantic selected movement/duplication, Command+Z/Shift+Command+Z atomic
  undo/redo, Tab/Shift+Tab nesting, and Japanese IME structural-command refusal
  with committed text preserved. This is a bounded browser check, not full
  assistive-technology or cross-platform qualification.
- Browser shortcut limitation: `web/host-core.mjs::keyboardRoute` routes
  Alt+Arrow to the browser and excludes Ctrl/Cmd+D from application commands;
  `semanticKeyboardRoute` also keeps these chords native. The actual browser
  Alt+Down check leaves block order unchanged. Semantic commands work; fixing
  focused-editor browser routing requires coordination with the host owner.
  No keyboard assertion was weakened to claim a browser shortcut pass.

### Independent increment: clipboard refusal safety

Shared block text, image previews, visual formulas and projected table cells now
wait for the clipboard backend to accept a cut before deleting content. A refused
write preserves text, marks, selection and history. Image Copy leaves the caret
and selection untouched, including the browser host's copy-before-cut preflight.
Successful cuts retain their existing single canonical undo boundary.

- Red: actual Notes keyboard and context-menu cuts deleted selected Unicode text
  when a synthetic backend refused the write. Image-preview cases additionally
  exposed hidden source to subsequent typing. The browser preflight reproduced
  that image selection fault after the first native repair.
- Green: five actual Notes caller tests cover marked cross-block Unicode text,
  table-cell menus and image previews, refusal followed by successful retry and
  one-step undo. All 59 shared block-editor tests pass, including the new visual
  formula refusal regression and bounded-keystroke-work coverage.
- Strict all-target Notes and SDK Clippy passes. The standard offline Notes WASM
  bundle builds. Disposable Chrome passes cross-block and image-preview clipboard
  events, rejected writes, accepted retry and atomic undo. These tests use only
  synthetic clipboard event objects; no user clipboard or notes are accessed.
- Required shell test compilation fails with 22 unresolved Linux-only compositor
  imports on Darwin. The same check fails with every candidate production file
  restored to exact base `0bb7a0e8db490119be1628bc9e6638ee0076e900`.
- An adjacent actual-Notes image-paste test fails on that unchanged base: context
  menu replacement retains the old image. An immediate-commit experiment fixed
  replacement but split it into two undo actions, so the experiment was reverted.
  The full unweakened reproducer is retained for the next atomic image-paste
  increment. This change does not claim rich MIME clipboard or image-paste parity.
- Independent final review found no remaining safety-diff issues. Shared History,
  browser transport, geometry, runtime and running user applications are unchanged.
