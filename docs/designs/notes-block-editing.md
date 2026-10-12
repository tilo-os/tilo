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
| P0 | Plain / rich / Markdown paste | Text clipboard, accepted-write cuts, atomic native image replacement | Browser image transport, rich MIME negotiation, formatting/literal modes |
| P0 | Markdown import/export | Notes `markdown`, canonical page projection | Nested/inline/code/table round trips; document extension limits |
| P0 | Undo / redo | Canonical Notes actions; indent and duplicate atomicity/concurrency caller tests | Independent first-type undo-to-empty failure; remaining command qualification |
| P0 | IME / Unicode / caret | Shared line/table composition guards, Unicode joins, safe copy selection | Browser/OS input-method qualification |
| P0 | Keyboard / accessibility | Focus escape, canonical read-only refusals and semantic duplication | Pointer command menu and browser/native assistive-technology qualification |
| P1 | Tables as content | Shared source/grid table editor and canonical cells | Qualify cell navigation, paste and round-trip consistency |
| P1 | Find / replace | Shared find overlay, current-match and atomic Replace all actions | Advanced matching options and broader qualification |
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
- Main integration `ab9a8c57ae` retained both independently added SDK tests.
  All five Notes cases, all 60 SDK block-editor tests, the standard WASM build
  and both browser checks pass again. Main introduced `spellbook 0.4.2`; its
  exact pinned source fetch was approved before resuming offline validation.
- Strict all-target Clippy passes again after that integration. The mandatory
  shell check reproduces the same 22 Darwin errors; both failing compositor
  modules are unchanged between the proven base and integrated main.

### Independent increment: atomic image replacement

Clipboard images now carry a stable selection intent to the shared rich-text
surface. Notes validates the image and dispatches asset storage plus canonical
replacement as one existing action. Undo restores both text and marks in one
step; redo restores the image. No History implementation or schema changes.

- The original Notes context-menu replacement failure and unavailable-clipboard
  preview selection failure were reproduced before repair. Fourteen mounted
  Notes clipboard tests now pass, covering keyboard/menu paste, rejected bytes,
  retry, marked Unicode ranges, empty notes/rows, separator-only selections,
  column-zero endpoint identity, peer text through undo, viewer and IME refusal.
  Three existing image paste/drop/large-screenshot caller tests also pass.
- Selection planning validates stable IDs, current source and compatible kinds
  before mutation; unsupported child trees/attributes and peer-interleaved
  ranges are refused. Prefix/suffix fragments retain all canonical marks,
  including links and metadata not painted by the editor.
- All 60 shared block-editor tests, four text-selection helper tests and 37
  rich-text tests pass. Strict all-target Notes/SDK Clippy passes. Empty clipboard
  strings are unavailable; whitespace remains pasteable. Image-preview paste
  reads usable content before exposing its hidden source as a selection.
- The standard offline WASM bundle builds. Browser image transport remains
  unsupported: image-only ClipboardEvents are explicitly refused by the existing
  browser host. This increment does not claim functional browser image paste.
- Independent review found no remaining issues. Legacy fixture file storage
  validates the target before writing; live notes retain canonical asset storage.
- Four disposable Chrome checks pass: image-only refusal over selected text and
  an image preview, refused cuts with accepted retry, and atomic cut undo. No
  browser errors were recorded. Final WASM SHA-256:
  `602825102932ac61a2192ee7db55a3e18873a2a0b8126c8e67522ba333ddc35a`.
- Required shell test compilation reproduces the same 22 Darwin-only unresolved
  compositor imports documented and proven at the clipboard-safety base. The
  affected compositor modules are unchanged in this increment.

### Independent increment: slash commands from committed text

The shared editor now recognizes a single committed `/` as the same trigger as
its native slash key. This matches browser `beforeinput` delivery without changing
browser host routing. Preedit and multi-character inserts remain literal; code
blocks retain their literal slash behavior. The existing menu owns subsequent
committed filter text, keyboard navigation and conversion.

- On exact main `945d8345b5`, the new mounted Notes regression inserted `/quote`
  into the body instead of filtering the menu. It now passes with stable block
  identity and direct one-step conversion undo/redo.
- The first three focused Notes tests pass, including preedit/multi-character
  refusal, literal code slashes and Escape preserving the typed source. The
  existing Notes slash-menu focus/navigation caller also passes.
- Review found the existing native slash anchor used the old caret when replacing
  a forward selection. A new caller regression reproduced the leftover slash.
  Both input routes now capture the normalized selection start; the regression
  covers forward/reverse Unicode selections, surrounding source and stable IDs.
- This does not release the held whole-block/callout conversions: their shared
  History resurrection defect is independent of how the menu opens.
- All four new Notes cases, the existing Notes focus case, all 60 shared
  block-editor cases and four menu tests pass. Strict all-target Notes/UI Clippy
  passes. Required shell compilation hits the same 22 Darwin-only unresolved
  Linux compositor imports. Independent review found no remaining issues.
- The standard offline WASM build and three disposable Chrome checks pass:
  slash opening/filtering/conversion with direct undo/redo, Unicode with a soft
  break, and forward-selection replacement preserving surrounding source and
  one-step conversion undo/redo. No browser errors were recorded. WASM SHA-256:
  `6a41395b0a98eae232a3c026b8647eb7324770f6499981ca31cfdd4963961231`.


### Independent increment: replace the current find match

The shared Find overlay adds a replacement field and Replace button. Tab moves
between the two inputs, Enter in the replacement field replaces one match, F3
navigates matches, and Escape closes the overlay. Pointer and semantic actions
use the same command. The shared rich-text surface validates the captured block
and match before one existing canonical `SpliceText` action; no schema or History
changes are involved. Empty replacement deletes the match. Replacement is
single-line (tabs allowed); Replace all and advanced matching options remain open.

- Red: the mounted Notes caller had no find-query/replacement semantic targets.
- Six actual Notes regressions pass: Unicode replacement, one-step undo/redo,
  keyboard and pointer activation, marked suffix preservation, empty replacement,
  peer text surviving undo, stale peer target refusal, viewer and IME safety,
  and exclusive focus after navigation or replacing the final match.
- This operation's concurrent-undo test passes independently of the unpublished
  formatting increment's mark-inheritance failure. It edits a single existing
  text object and does not recreate blocks.
- Strict all-target Notes and SDK Clippy passes. Remaining SDK/browser results
  are recorded after validation; no browser pass is inferred from compilation.
- All 60 block-editor tests (including bounded typing work), both SearchField
  tests and all 37 rich-text tests pass. Independent final review has no findings.
- The standard offline WASM bundle builds. Disposable Chrome passes actual
  Ctrl/Cmd+F entry, semantic Unicode replacement, empty replacement, atomic
  undo/redo, and Tab/Enter replacement input. No browser errors were recorded.
- Visual inspection found the default button footprint overlapping the input;
  an explicit theme-scaled button size fixes it. The native pointer caller now
  asserts the controls do not overlap; that regression and strict Clippy pass.
- Required shell test compilation reproduces the same 22 baseline Darwin
  compositor import errors. No compositor or History implementation was changed.


### Independent increment: replace all find matches

The shared replacement field supports a Replace all button and Shift+Enter.
Both keyboard and semantic `notes.find:replace-all` activation use one canonical
`ReplaceMatches` action. It validates disjoint character ranges and the aggregate
text budget before mutation, then splices back to front without recreating blocks.
This extends the action vocabulary without migrating stored documents or changing
History. Matching retains the existing case-insensitive literal search behavior;
regular expressions and whole-word options remain open.

- The original real Notes caller regression failed because no Replace all target
  existed. Twelve current Notes find/replace tests pass, including single and batch
  replacement, Unicode, stable block IDs, one-step undo/redo, shortcut modifiers,
  pointer activation, empty replacements, surviving link marks, IME/viewer refusal,
  stale matches and newly inserted peer blocks, concurrent text surviving undo,
  exclusive input focus, and replacements that themselves contain the query.
- Two canonical tests cover shuffled Unicode character ranges, one replay envelope,
  missing blocks, out-of-bounds/empty/reversed/overlapping/duplicate ranges, batch
  size and aggregate insertion limits. Invalid batches leave document heads unchanged.
- Each search captures one linear-size body snapshot. Replace all refuses if its
  order or projected content changed; refreshing the query captures the new body.
  Character offset conversion advances once through each source paragraph instead
  of repeatedly scanning its prefix. The user must explicitly refresh a stale search.
- Independent review found no remaining issues after adding keyboard activation and
  reflecting the canonical insertion budget in button eligibility.
- All 60 SDK block-editor tests and all 37 rich-text tests pass, including bounded
  typing-work regressions. Strict all-target Clippy and repository checks pass.
  Required shell test compilation reproduces the same 22 Darwin-only unresolved
  Linux imports as the verified previous base; those compositor files are unchanged.
- The standard offline Notes WASM bundle builds. Three disposable Chrome checks
  pass for Unicode Replace all with stable IDs and one-step undo/redo, empty
  replacements, and Shift+Enter when the replacement contains the query. No
  browser errors were recorded. Final WASM SHA-256:
  `82ab5a487728c603e2f1981a6f09bcb7d1c82f2ec5602a10260c3982f67d3962`.
  Shift+Enter is the advertised cross-platform shortcut; the shared browser host
  retains Ctrl/Cmd+Enter inside native text inputs, so no host routing was changed.
- The Agent adapter maps every batch replacement span to the original block and
  character range for its interaction anchor. A focused regression covers two
  blocks, Unicode replacement text and unrelated focus. Strict Agent library
  Clippy passes; independent review found no remaining issues in the adapter.
  The focused Agent test and all-target check cannot run through their shell
  dependency on Darwin: the same 22 unresolved Linux compositor imports stop
  compilation before the new assertion. Linux CI must execute that regression.


### Browser rich clipboard transport foundation

Added an optional paired `RichTextOffer` to the shared clipboard API and event
scope, and implemented it in the browser DataTransfer adapter. Both formats
come from one event. HTML and its captured plain fallback each have a 1 MiB
UTF-8 cap; the browser checks size before copying strings into WASM memory.
Accepted rich offers consume the event once; missing, empty or oversized offers
leave the existing plain read available. Nested scopes restore their outer
state, and diagnostics report lengths rather than clipboard contents.

- Eight native event-scope tests pass; strict clipboard all-target Clippy passes.
- The production JavaScript bound checker passes 1,809 UTF-8 boundary/seeded
  cases plus seven invalid-value/error cases, including surrogate handling.
- Four disposable Chrome/WASM transport checks pass, including a real trusted
  copy/paste event, paired Unicode content, one-read behavior and size refusal.
  The temporary fixture export was removed before the production build.
- The final production Notes WASM bundle builds; three real Notes browser
  regressions pass with zero browser errors, and the fixture export is absent.
- The required shell test compile still fails on the 22 established Darwin
  compositor/Wayland import errors at this main base; no assertions weakened.
- Extra strict WASM lint checks reproduce four advisory-lock errors and 31
  browser-runtime errors with unchanged baseline browser source. None reports
  the changed clipboard adapter; this is not a passing WASM lint qualification.
- No browser host, geometry, authority, History or Wayland implementation changed
  in this increment. Browser/Wayland file ownership is now confirmed separately.

This is transport support, not released formatted Notes paste. Its SDK decoder
and canonical caller remain on the History-held rich-paste branch. The current
browser host also rejects HTML-only offers before dispatch; changing that host
preflight requires its owner's scoped follow-up. Real Notes rich-paste
undo/redo and full-browser caller qualification remain release gates.

### Wayland paired clipboard transport (held for protocol qualification)

The existing publication worker now requests HTML and its optional plain
fallback from one focus-authorized `wl_data_offer`. Both formats have the shared
1 MiB cap; nonblocking reads do at most eight 8 KiB syscalls per callback, validate
UTF-8 at EOF, and expire after 40 ms. Replacement, keyboard Leave, seat removal,
worker exit and backend teardown invalidate the cached offer and pending reads.
An epoch prevents late publication after invalidation. Native backend forwarding
uses the attached Wayland connection and does not acquire additional privileges.
Existing image transfer limits and publication behavior are unchanged.

Linux all-target compilation and strict Clippy pass with the installed target
and Zig toolchain, offline and locked. Four production cache/reader tests compile;
four new isolated compositor tests cover paired Unicode, completed-cache focus
loss, plain replacement, NULL selection and deadline pipe closure/late-write
refusal. Linux execution of these tests is still required before release.
A separate private-socket protocol fixture now supplies four deterministic tests
for selection-before-Enter, same-client surface switches, both-pipe cancellation
on Leave/NULL, multi-seat authority, and the public 40 ms deadline. It waits for
both seats to bind and uses a worker-queue protocol sync as its event barrier.
Cancellation tests have one absolute failure deadline earlier than the private
read budget, so timer expiry cannot make them pass. Both public constructors
retain 40 ms. The fixture uses the already-locked Wayland server crate as a
Linux-only dev-dependency; no dependency versions changed or downloads were
required. Linux execution is still pending; these client-worker tests complement
the real-compositor authorization tests. The local Linux shell cross-check stops
in glib-sys because no Linux pkg-config sysroot is configured on this Mac.
This transport does not release the History-held Notes rich-paste consumer.

The first deterministic Linux run (`dd629311eb`, focused run 67474) executed all
eight requested worker/cache cases: seven passed and pending NULL cancellation
failed. SCTK 0.20 clears a NULL selection without invoking its selection callback.
The adapter now forwards raw data-device events through SCTK and additionally
processes NULL for rich text, preserving SCTK child initialization and the
existing image handler. The both-pipe closure and late-write assertions remain
unchanged; fixed-source execution is required before lifting the hold.
