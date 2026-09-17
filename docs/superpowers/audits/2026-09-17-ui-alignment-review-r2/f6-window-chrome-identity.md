# F6 — Window-chrome widgets, identity flow, `ui/src/chrome` (round two)

Branch `ui-review/r2-f6-window-chrome-identity`, base `381eff6a8` (the merged
round-one tip) plus one cherry-pick the coordinator named, `16293e859`
(= `f11e61da6` on `codex/macos-shell`: the shell lib test target did not
compile at the base — `overlay_host.rs` read a removed `CHIP_PAD_PX` in one
palette-filter test line). Nothing else outside `ui/` was touched.

## Scope

Owned files, as in round one: `ui/src/widget/{tabs,window_group,group_row,
tile_host,strip_view,workspace_view,router,page_sidebar,resizable_split,
resize_handle,app_widget}.rs`, `ui/src/widget/identity_flow/*`,
`ui/src/chrome/{toolbar,status_bar,shell,page}.rs`. Also edited, as the
mechanics required: `ui/src/chrome/mod.rs` (one re-export line),
`ui/tests/f6_identity_flow_layout.rs` (the round-one test, extended).

Round-two brief, and what happened to each item:

1. **Identity flow: replace the hand-rolled text fields with the shared
   `TextInput`** — done at all seven call sites (below), the round-one
   `sheet::paint_field` / `LineEditState` deleted. Identity strings → fluent:
   recorded, not converted (see "Recorded, not changed").
2. **Tab action chips below the 32 px minimum (F6-22)** — done in `tabs.rs`
   without moving any pinned width; no shell test needed changing.
3. **`ToolbarMetrics`/`item_rects` published; `page_sidebar` themed
   variant** — done; the consumers are untouched by design.
4. **Round-one fixes re-verified after the merge** — all present; captures
   before/after below.

## Method

1. BEFORE captures from the merged tip, all opened and inspected:
   `target/ui-review/ui/before/identity_*_{dark,light,high-contrast,text-2x}.png`
   (7 screens + Done), `window_group_*.png`, `group_row_*.png`;
   `target/ux-bench/f6-before{,-open-files}/` at 1280×800 (desktop PASS;
   open-files FAIL with the pre-existing single check
   `Visible { target: "semantic:Files" }: missing or ambiguous target` — F6-29).
2. Source pass over the owned files against HIG §9 and the round-one report.
3. Fixes with a regression test each, in the owning module; the round-one
   `f6` integration test extended to drive the real widgets.
4. Two `code-review` passes (`high`, then `medium` after the first round of
   fixes); every functional finding fixed, every "by design" item recorded.
5. AFTER captures into `target/ui-review/ui/after/`, `target/ux-bench/f6-after*/`,
   `target/ui-review/{pdf_viewer,slides}/after/`.

Every cargo command ran as
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo … --locked --no-default-features`.

## Findings

Status: **fixed** = changed here with a test; **recorded** = deliberate or
outside this unit, written down precisely.

| id | surface | defect | file (before) | before → after | status |
|---|---|---|---|---|---|
| F6-r2-01 | Identity: every text field | Hand-rolled input (`LineEditState` + `sheet::paint_field`): no IME, no context menu, no accessibility node, no `test_id`; hand-rolled Ctrl+A/C/X/V; pair-device payload typed byte by byte with its own control-char filter | `create_identity.rs` name; `invite_accept.rs` link; `pair_device.rs` payload + six digit cells; `recovery_verify.rs` blanks ×3 | `ui/before/identity_{create_identity,invite_accept,pair_device,recovery_verify}_dark.png` → `ui/after/…` (pixel-identical chrome at rest: the shared widget paints the same raised fill, `radii.md`, focus edge and `padding_standard` inset the sheet mirrored) plus `ui/after/identity_*_after_{paste,typing,set_value}.png` | fixed — each field is a `TextInput` seated by `sheet::place_field` / `place_code_cell`, drawn and fed events by the screen, and a **child node of the screen** (`Display::None`, registered in `node_map`) so the host's focus order, the a11y tree and `named_target_nodes` reach it; `IdentityFlow` became a real parent node so `children()` and `taffy.children()` line up |
| F6-r2-02 | Identity: hit rects | The pointer target of a field was the screen's whole row while the widget's own rect was the same row — a row scrolled under the action row must not take a click | all four screens | — | fixed — the widget takes the full row (the body clip trims its paint); the screen forwards a press only through `BodyScroll::visible(body, row)` |
| F6-r2-03 | Identity: pair-device code cells at 2× text | The `s12` (48 px) cell did not grow with the `Code` glyph, which filled the frame | `pair_device.rs plan` | `ui/before/identity_pair_device_text-2x.png` → `ui/after/identity_pair_device_text-2x.png` | fixed — `sheet::code_cell_side` = `s12.max(line_height(Code) + 2·s2)`; `code_cell_grows_with_the_text_scale` |
| F6-r2-04 | Identity: recovery verify Tab order | Tab walked the blanks only inside the screen's own key handler, which a host with focus traversal never delivers | `recovery_verify.rs` | — | fixed — `children()` and the minted nodes are in reading order (ascending word position), so the host's Tab/Shift+Tab walks the grid the way the eye does; `next_slot_in_visual_order` kept for a host that leaves Tab to widgets; each blank is named "Word N" for a screen reader |
| F6-r2-05 | Identity: accessibility | Screens exposed no text-input nodes; an agent could not set a name or a link | all four | — | fixed — every field has an accessible name from its label; `SetValue` re-parses the invite link on the next layout pass (`an_edit_through_the_shared_field_reparses_the_link`); `SetSelection` is *not* an edit (`sheet::edited` compares text, never the widget's dirty flag) |
| F6-r2-06 | Tab pill action chips (F6-22) | Maximize/search pointer targets 14×24 px | `tabs.rs chip_rects` | `ux-bench/f6-before-open-files/files-open.png` → `ux-bench/f6-after-open-files/files-open.png` (pixel-identical: nothing painted moved) | fixed — `TabBar::action_hit_rect`: `tokens.metrics.min_pointer_target` (32) square centred on the glyph, clamped to the bar; hover and press resolve **glyph first, then targets by priority** (close, maximize, search); the close target is clamped off a painted maximize glyph; painted chips and pinned pill widths unchanged |
| F6-r2-07 | Tab pill action chips | The enlarged targets overlap inside one pill by construction (glyph pitch 18 px < 32 px), so the exclusive part of each target is 27–28 px | `tabs.rs` | — | recorded for S1 — a true 32 px exclusive target needs `ACTION_GAP`/`ACTION_W` to change, which moves every pinned pill width; no shell test pins the chip rects (`grep ACTION_W\|CHIP_CLUSTER_W\|max_rects\|search_rects shell/src` → only `TAB_BAR_HEIGHT` in `foreign.rs:1960`) |
| F6-r2-08 | `chrome/toolbar.rs` | Width model `14 / 6 / 14 / 34 / 15` restated as literals in `draw` and mirrored by `apps/word-processor/src/widget.rs:716-734` | `toolbar.rs draw` | — | fixed (stage one) — `ToolbarMetrics::standard()`, `item_advance`, `button_height`, `item_rects`, `draw_with_metrics`; `draw` paints exactly `item_rects`; labels measured once per frame. The word processor still carries its copy — A7 adopts in a later round (values kept identical, pinned by `item_rects_are_what_draw_paints_and_follow_the_advance_model`) |
| F6-r2-09 | `page_sidebar.rs` | `begin_frame` runs without a `Theme`, so eleven metrics were consts | `page_sidebar.rs:99-115` | consumer shots below (unchanged by construction) | fixed (stage one) — `RailMetrics` (`DEFAULT` = the consts; `from_theme` reads `spacing.s1/s2/s3/s6/s8`, `metrics.bar_height`), `begin_frame_with_theme`; every use site reads `self.m`; the old `begin_frame` keeps `DEFAULT` so pdf_viewer/slides are pixel-unchanged. `ADD_H` 36 → `bar_height` 40 in the themed path only; `MENU_W` 184 and `SCROLLBAR_W` 6 stay consts (no token) |
| F6-r2-10 | Identity strings | English literals, not fluent keys | `identity_flow/*` | — | recorded — see below |
| F6-r2-11 | Round-one fixes | Re-verified after the merge: viewport-filling sheets, body scroll under overflow at 2×, hit rects from `apply_layout`, tab glyph/close alignment, status bar and toolbar tokens | — | `ui/before/*` = merged tip, all present; `identity_screens_fill_the_viewport` green | verified |

### Code-review findings (fixed before commit)

The `high` pass and the coordinator's relay of its Angle-A findings, then a
`medium` pass over the fixed diff. Functional findings, all fixed with a test:

1. *Pair device: a digit typed into a filled cell after a caret move
   appended and spread.* Fixed: the focused cell keeps its digit selected —
   in `settle`, on every pass, whichever door focused it (click, host Tab,
   spread) — so the next keystroke replaces (`typing_advances_a_paste_spreads_and_backspace_steps_back`
   covers ArrowRight/End then a digit, and a host-focused cell).
2. *`settle(&mut None)` moved widget focus from the layout/draw pass.* Fixed:
   `settle(Option<&mut Option<NodeId>>)` — digits still settle from the
   layout pass (where an accessibility edit is noticed) but focus moves only
   in the event path with the host's real `focus_target`.
3. *`SetSelection` treated as an edit.* Fixed: `sheet::edited(input, last)`
   compares text; used by all four screens.
4. *Recovery verify forwarded Release/right-click only to the focused blank.*
   Fixed: `forward_all` — each field gates itself, so a drag ends and a menu
   opens where the pointer is (the `f6` test drags in a blank, Tabs away,
   releases, and checks the old blank's selection no longer follows the
   pointer).
5. *Close target vs. maximize glyph.* Fixed: glyph-first resolution plus the
   clamp in `apply_layout`; `close_target_stays_off_the_maximize_glyph_and_glyphs_win_over_targets`
   asserts on a live hover-revealed pill.
6. *(medium) Host Tab into a filled cell was never re-selected* → covered by 1.
7. *(medium) `reseed()` replaced the blanks and dropped widget focus* → fixed,
   `reset_blanks` re-focuses the same slot; `reseed_clears_answers_and_picks_new_challenge`
   pins it.
8. *(medium) Pair screen no longer opened with cell 0 focused* → fixed in
   `new()`; the `f6` test asserts the host adopts it.
9. *(medium) Toolbar labels shaped twice per frame* → fixed, one `lay_out`
   pass feeds both `item_rects` and `draw_with_metrics`.

Recorded from the review, not changed: `ToolbarMetrics`/`item_rects` and
`RailMetrics`/`begin_frame_with_theme` have no callers yet — that is this
round's brief (publish the model; A7 and the pdf_viewer/slides owners adopt
it without editing their files here); their tests pin that the published
values equal the ones the consumers mirror.

## Recorded, not changed

- **Fluent keys for identity strings.** The toolkit exposes a bundle
  (`ui::i18n::I18nManager`, `ui/locales/{en-US,de-DE}.ftl`, `i18n!`), but
  `ui` never installs it: `I18nManager::install_system` is called by app
  hosts (the `qos-dev` generated `install()`), and `lookup` falls back to
  the raw key. `Button::paint`'s `i18n!(face.label)` therefore passes literal
  labels through unchanged. Converting the identity strings to keys would
  render `identity-create-title` in `ui-lab` and in the harness until a host
  installs the catalogs. Path: keys + `en-US`/`de-DE` entries in
  `ui/locales`, `i18n!` at the paint sites, and `ui-lab`/the shell onboarding
  host installing the catalogs — one change across owners.
- **Chip target exclusivity** (F6-r2-07) — for S1.
- **`design_token_audit`** fails at the merged tip in files outside this
  unit, before and after this change (this unit's files appear in neither
  list):
  `theme_escape_hatches_do_not_grow`: baselines above the tree for
  `autocomplete.rs [theme_blind_white] 1→0`, `channel_browser.rs [literal_color] 1→0`,
  `context_menu.rs [theme_blind_white] 1→0`, `popover.rs [literal_color] 1→0`,
  `video_player.rs [literal_color] 1→0`;
  `app_and_shell_theme_escape_hatches_do_not_grow`: `apps/browser/src/browser_tabs.rs`
  (`literal_color` 1, `white_or_black` 1), `shell/src/shell/frame.rs`
  (`literal_color` 1, `white_or_black` 2). `ui/tests/design_token_baseline.txt`
  needed no change for this unit: `sheet.rs literal_color 2` (the QR
  contract) is unchanged and the deleted painter carried no literal.
- **`cargo clippy -p ui`** does not pass at the merged tip (212 errors under
  the workspace `pedantic = deny`, and `--no-deps` is required because
  `collab-types` itself fails `missing_errors_doc`). Every clippy error in
  code this round wrote was fixed; the ones left in owned files are on
  untouched lines: `tabs.rs:2426` (`nonminimal_bool`), `add_device_host.rs:524`
  (`single_match_else`), `invite_mint.rs:560` (`redundant_closure`),
  `identity_flow/mod.rs:2123` (`draw_done_card`, 102/100 lines),
  `sheet.rs:664` (`paint_check_row`, 8 arguments, warning).
- **ModalFrame::body_width / action_height** — F4 had not exposed them in
  this tree; `Sheet::body_width` and `Sheet::control_height` remain the
  duplicates (unchanged from round one).
- **`LineEditState` deleted** with its seven copies; nothing outside
  `identity_flow` referenced it (`grep -rn LineEditState ui shell apps crates`).

## Tests

| command | result |
|---|---|
| `cargo test -p ui --lib -- identity_flow:: tabs:: chrome:: page_sidebar:: window_group:: group_row:: tile_host:: resize_handle:: resizable_split::` | **369 passed, 0 failed** (round one: 352). New: `sheet::{embedded_fields_take_their_row_and_code_cells_centre_their_digit, code_cell_grows_with_the_text_scale}`, `create_identity::clicking_and_typing_drive_the_shared_field_and_enter_submits`, `invite_accept::an_edit_through_the_shared_field_reparses_the_link`, `recovery_verify::children_and_nodes_walk_the_blanks_in_reading_order` (+ focus kept across reseed), `pair_device::typing_advances_a_paste_spreads_and_backspace_steps_back`, `tabs::{action_chips_have_a_minimum_pointer_target_centred_on_the_glyph, overlapping_action_targets_resolve_to_one_chip_by_priority, close_target_stays_off_the_maximize_glyph_and_glyphs_win_over_targets}`, `chrome::toolbar::item_rects_are_what_draw_paints_and_follow_the_advance_model`, `page_sidebar::themed_metrics_read_the_spacing_scale_the_constants_restate` |
| `cargo test -p ui --test it -- f6_identity_flow_layout window_group_visual_journey group_row_visual_journey` | **9 passed, 0 failed**; the new `identity_fields_are_shared_text_inputs_reached_by_tab_typing_and_paste` drives Tab (host focus order, in reading order on the grid), typing after End/ArrowRight/Home, Ctrl+A / Ctrl+V through the widget's own shortcuts against an in-process `ClipboardBackend` (never the developer's clipboard), a11y `SetValue` and `SetSelection`, and a drag that ends after Tab — reading every result back through `SemanticExpectation::named(test_id).value(..)` and the focused node's accessible name |
| `cargo test -p ui --test it -- design_token_audit` | 4 passed, 2 failed — both pre-existing, files outside this unit (verbatim above) |
| `cargo check -p ui --tests` (final code) | clean, 0 warnings |
| `cargo clippy -p ui --lib --tests --no-deps` | 212 errors at the tip, none in lines this round wrote (see above) |
| `cargo check -p shell --lib --bin ux-bench --bin shell` | clean, 0 warnings |
| `cargo test -p shell --lib -- tab` (after the cherry-pick; read-only check that the shell's pins still hold) | **151 passed, 0 failed** (round one: 144) — no shell test needed changing for the chip targets |
| `cargo build -p pdf_viewer -p slides --bins` + `--shots` scenarios | built; 6/6 shots rendered (dark + light) |

## E2E captures

All PNGs were opened and inspected with the Read tool.

| what | before | after |
|---|---|---|
| Identity screens ×7 + Done, dark/light/high-contrast/text-2× | `target/ui-review/ui/before/identity_*.png` | `target/ui-review/ui/after/identity_*.png` — at rest indistinguishable from before except the 2× pair-device cells, which now hold their digits with an `s2` breath |
| Identity fields after driving them through the harness | — | `target/ui-review/ui/after/identity_create_identity_after_paste.png` ("Carol" pasted over a select-all), `identity_recovery_verify_after_typing.png` (the word typed into the Tab-focused blank, focus edge on it), `identity_pair_device_after_paste.png` (9 typed, 2468 pasted and spread, "payload" typed after Tab), `identity_invite_accept_after_set_value.png` (a link set by `SetValue` and then broken by an "x" typed after Home: the caret sits after the x, the error row "identity-flow decoding failed: missing qos://invite/ prefix" is drawn and Accept is disabled — the edit re-parsed on the next pass, exactly as typing did before) |
| Tab bar, window group, group row visual journeys | `target/ui-review/ui/before/{window_group,group_row}_*.png` | `target/ui-review/ui/after/…` — identical (nothing painted changed) |
| Compositor: desktop, open-files (1280×800) | `target/ux-bench/f6-before{,-open-files}/` — desktop PASS, open-files FAIL (F6-29, pre-existing) | `target/ux-bench/f6-after{,-open-files}/` — desktop PASS; open-files FAIL with the identical single check (`Visible { target: "semantic:Files" }: missing or ambiguous target`); `desktop.png` and `files-open.png` visually identical to before (the Files pill's glyph, title and × have not moved — only the hover-reveal pointer targets changed) |
| `page_sidebar` consumers (pdf_viewer `page-bubble`, slides `overview`/`narrow`), dark + light, 1200×800 | not retaken: the consumers call the unchanged `begin_frame`, whose geometry is `RailMetrics::DEFAULT` = the previous constants, pinned by `themed_metrics_read_the_spacing_scale_the_constants_restate` | `target/ui-review/pdf_viewer/after/page-bubble-{dark,light}.png` (rail header, numbered thumbnails on the `s6` gutter, Import button), `target/ui-review/slides/after/{overview,narrow}-{dark,light}.png` (the narrow scenario's slide rail; overview has no rail) — all six inspected, laid out as in round one |

## Limits

- The embedded fields are placed by the sheet, not by taffy: their nodes
  are `Display::None` leaves that exist so the host's walks line up with
  `children()`. A host that positions widgets from taffy layout alone would
  see zero-size rects for them; every host in this tree drives
  `apply_layout`, where the sheet seats them.
- The pair-device digit cells are plain-chrome `TextInput`s inside the
  sheet's `paint_cell` frame (the shared widget has no centred alignment);
  the frame restates the widget's own field chrome, and the digit widget
  is centred on the cell by measuring the widest digit.
- Focus moved by a digit spread reaches the host only through the event
  path; a spread triggered by an accessibility `SetValue` settles the
  digits but leaves focus where the host put it (recorded as designed, per
  the review).
- `TextInput` measures its single line against the frame renderer; the
  layout-pass renderer (`sheet::with_layout_renderer`) seats it by rect
  only, so no double shaping.
- macOS: `cargo clippy -p ui` needs `--no-deps` here (a `collab-types`
  `missing_errors_doc` error stops the dependency build under clippy).
- The shell lib test target needed the coordinator's one-line cherry-pick
  (`16293e859`) to compile at all; it is committed on this branch ahead of
  this unit's commit and touches only a test line in `overlay_host.rs`.
- Git identity: this checkout has no `user.email`; the two commits were
  made with `-c user.name="Xaver Emmerling" -c user.email="xaver@Mac.lan"`,
  the identity every prior commit on the branch carries.
