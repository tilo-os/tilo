# F6 — Window-chrome widgets, identity flow, `ui/src/chrome` (alignment review)

Branch `ui-review/f6-window-chrome-identity`, base `codex/macos-shell` @ `9c9e15ba9`
plus the shared lockfix (`f9a579c9c`, one `Cargo.lock` line).

## Scope

Files owned by this unit:

- `ui/src/widget/{tabs,window_group,group_row,tile_host,strip_view,workspace_view,router,page_sidebar,resizable_split,resize_handle,app_widget}.rs`
- `ui/src/widget/identity_flow/` (8 files) — plus one new file, `sheet.rs`
- `ui/src/chrome/{toolbar,status_bar,shell,page}.rs`

Also touched, as the mechanics the review requires (flagged, not silent):

- `ui/tests/f6_identity_flow_layout.rs` (new) and one registration line in
  `ui/tests/it.rs` — `ui` sets `autotests = false`, so a new test file cannot
  compile without a `#[path]` line in the aggregator.
- `ui/tests/design_token_baseline.txt` — the ratchet fails when a file
  carries *fewer* literals than recorded ("lower it in the same commit"), so
  every colour literal removed below required its baseline line lowered.

## Method

1. Source pass against `ui/HIG.md` §7, §7a, §9 (roles, lift, borders inset
   `s3`, §9.6 type/density, §9.13 enforcement grep) — every metric and colour
   in the owned draw paths was classified as token / named const on the 4 px
   grid / off-grid / literal.
2. BEFORE captures, all inspected with the Read tool:
   - `target/ui-review/ui/before/` — `window_group_*.png`, `group_row_*.png`
     (visual-journey tests), `tab_bar.png` (high-contrast, 1.5× text), and
     `identity_*_{dark,light,high-contrast}.png` for all seven ceremony
     screens plus the Done card (new `f6` test), with 4× ffmpeg crops of the
     pill row in `zoom_*.png`.
   - `target/ux-bench/f6-before{,-open-files,-nav}/` and
     `target/ui-review/shell-before.png` — the real compositor at 1280×800.
3. Fixes expressed through `theme.tokens.*`, `ChromeRole`/`ModalFrame`, the
   shared faces (`Button::paint`, `Checkbox::paint`, `ToggleGroup::paint`)
   and measured text; pinned metrics left unchanged (see "Off-grid metrics
   kept").
4. Regression per fix as unit tests in the owning module (pure geometry
   functions extracted where the draw path had none) and the `f6`
   integration test.
5. AFTER captures into `target/ui-review/ui/after/` and
   `target/ux-bench/f6-after*/`, compared against BEFORE.

Build-environment notes (not code changes, recorded so the numbers are
reproducible): the worktree was cut from `24ee08c66`, a commit that is *not*
an ancestor of `codex/macos-shell`; it was reset to the branch tip before any
edit. Cargo merges the parent checkout's `.cargo/config.toml`
(`/Users/xaver/dev/qos`, on `main`), whose non-Linux aarch64 block passes
`-C target-cpu=generic` and breaks `ring`'s static-feature assertion; every
cargo command therefore ran with
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
(replaces the merged list; identical rustc arguments to the tip's own macOS
block) and `CFLAGS_aarch64_apple_darwin="-fstack-protector-strong"`.

## Findings

Status: **fixed** = changed here with a test; **kept** = deliberate,
documented; **open** = outside this unit or outside this review's scope.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| F6-01 | Identity: every screen | Screen requested a fixed leaf (720×580, 800×600, 900×640…) so the scrim stopped at that box and the card centred in the window's top-left quadrant, not the viewport | 1280×800, all themes | `identity_flow/*.rs` `build_layout` | `ui/before/identity_create_identity_dark.png` → `ui/after/…` | fixed — `percent(1.0)` leaf; `f6::identity_screens_fill_the_viewport` |
| F6-02 | Identity: Done card | `Style::default()` leaf → zero width; card centred on x = 0, half off-screen | 1280×800 dark | `identity_flow/mod.rs:2677` | `ui/before/identity_done_dark.png` → after | fixed — viewport leaf + sheet layout |
| F6-03 | Identity: all screens | ~400 absolute offsets (`card.y0 + 240.0`, `card.y0 + 130.0`, `x0 + 16.0, y0 + 12.0` label origins), seven card sizes, four button heights (36/40/44/56), text centred with `(h - 13.0)/2` | all | `invite_mint.rs:424`, `create_identity.rs:322-350`, … | before/after per screen | fixed — new `sheet.rs`: `ModalFrame` chrome + `Rows` cursor on `spacing.*`, `Role` type roles measured with the style drawn |
| F6-04 | Identity: invite mint | Invite URL painted unbounded, running off the card and off the window; QR generated but never drawn | 1280×800 | `invite_mint.rs:412-437` | `ui/before/identity_invite_mint_dark.png` → after | fixed — QR centred (`s16×4` square), link elided to the row |
| F6-05 | Identity: invite accept | URL field text ran past the field | 1280×800 | `invite_accept.rs:440-478` | before/after | fixed — field clips to its shape |
| F6-06 | Identity: create | Accent tiles centred on the card while their label sat on the left edge | all | `create_identity.rs:339-347` | before/after | fixed — tiles start on the label's edge (`plan_stacks_rows_…`) |
| F6-07 | Identity: buttons | Hand-painted buttons: literal white ink on accent, radius 8 literal, danger red `rgb(220,80,80)`, disabled = base fill | all | every screen | — | fixed — `ModalFrame::paint_actions` (`Button::paint`, measured widths, platform order) |
| F6-08 | Identity: check row / segments / code boxes | Hand-drawn checkbox (18 px, radius 3, label `+2.0`), hand-drawn segmented control, 56×64 code boxes | all | `recovery_share.rs`, `invite_mint.rs`, `pair_device.rs` | before/after | fixed — `Checkbox::paint` (`s4` box), `ToggleGroup::paint` (`radii.md`), `s12` square cells |
| F6-09 | Identity: recovery share vs verify | Same grid, different row heights (36 vs 50) and slot offsets | all | `recovery_share.rs:367`, `recovery_verify.rs:355` | before/after | fixed — one `WordGrid`, one control-tall row |
| F6-10 | Identity: scrim + card | `rgba(0,0,0,180)` scrim ×8, `surfaces.raised` card, radius 12 literal | all | every screen | — | fixed — `fill_scrim` + `radii.lg` via `ModalFrame` |
| F6-11 | Identity: addressability | `IdentityFlow` exposed no children: no screen reachable by `test_id` from the harness / a11y tree | — | `identity_flow/mod.rs` | — | fixed — `children()`/`children_mut()`/`test_id` |
| F6-12 | Tab pill | Leading glyph at `x0 + 10` while the title inset is `PILL_PAD_X` = 12 → glyph 2 px inside the text edge, icon→title gap 8 instead of `ICON_GAP` 6 | all | `tabs.rs:635-659`, `:914-923` (closing pill) | `ui/before/zoom_wg_fresh_bar.png` → after | fixed — `leading_glyph_rect`/`title_x0`; title x unchanged so no elision drift |
| F6-13 | Tab pill action chips | Hovered maximize/search painted a 14×24 capsule while the close chip paints an 18 px disc | all | `tabs.rs:832,858` | — | fixed — `chip_hover_ring` (one `CLOSE_W` disc); centre-line test |
| F6-14 | Tab pill | Hover washes `rgba(255,255,255,28|64)` theme-blind (invisible on light palettes); `rgba(0,0,0,0)` | light | `tabs.rs:769-771,822` | — | fixed — `Colors::hairline_on(ground, α)`; `chip_hover_tint_is_derived…` |
| F6-15 | Tab pill | Focus keyline literals `8.0 / 2.5 / 0.5 / radius 1.0`; pinned marker `±7.0`; new-tab glyph `11.0` | — | `tabs.rs:457-460, 601-606, 2600` | — | fixed — `spacing.s2`, named consts, `square_at` |
| F6-16 | Window group resize grip | Bars at `x1 − 3 ± 2`, width 2, radius 1 literal; height clamp `24..80` literal | — | `window_group.rs:1151-1163` | — | fixed — centred on `RESIZE_HANDLE_PX / 2`, bars fill the band exactly, clamp `s6..s16+s4` |
| F6-17 | Maximized chrome | Icon slot `+8..+22, y+4..y−4` (14×14 inside a 22 row but label at `+28`), restore→close gap `4.0`, title pad `24`, close 22 wide | — | `window_group.rs:1277-1318` | — | fixed — square `MAXIMIZED_ICON_PX` slot on the centre line, label `s2` after it, gap `spacing.s1`, close chip square; `maximized_chrome_chips_share_one_row…` |
| F6-18 | Group row reorder capsule | End offsets `±5.0`, thickness `4.0` | — | `group_row.rs:1959-1976` | — | fixed — `HANDLE_SLOT_PX / 2`, `2 × HANDLE_VISIBLE_PX` |
| F6-19 | `chrome/status_bar.rs` | Literal `#9a9ca4`, size 13, gutter 18, `(h − 15)/2` | all | `status_bar.rs:49-65` | — | fixed — `text_dim`, `scale.sm`, `spacing.s4`, measured line; long left readout elides; tests |
| F6-20 | `chrome/toolbar.rs` | Literal white ink on accent; radius 6; label `cy − 9`; decorations at `+18/+9` | all | `toolbar.rs:99-172` | — | fixed (minimal) — `on_accent(ON_FILL_MIN_CONTRAST)`, `radii.md`, measured line + weight |
| F6-21 | `page_sidebar.rs` | 9 consts off-grid (10/22/30/38), header at `(PAD+2, +9)`, sizes 11/12/12.5, radii 3/5/6/7/8/9, `Color::BLACK` washes as shadows ×2 | pdf_viewer / slides | `page_sidebar.rs:89-97, 513-766` | consumer shots (see E2E) | fixed — grid consts, `label_size` (`scale.sm`), measured centring, `radii.sm/md/lg`, `surface::shadow(e4)` |
| F6-22 | Tab pill action chips | Maximize/search pointer targets are 14×24 — below the 32 px minimum (§9.6); widening them changes `CHIP_CLUSTER_W` and every pill width the layout tests pin | — | `tabs.rs` `chip_rects` | — | open — needs a deliberate cluster-width change with S1 (shell pins pill widths) |
| F6-23 | Maximized chrome | Row is 22 px; §9.6 Selector height is 28 | — | `window_group.rs` `MAXIMIZED_CHIP_H` | — | kept — overlays app content; recorded off-grid |
| F6-24 | `chrome/toolbar.rs` | `TOOLBAR_HEIGHT` 50, left pad 14, gap 6, button 34, label 15 stay | — | `toolbar.rs` | — | kept — `apps/word-processor/src/widget.rs:712-727` mirrors this width model literally; needs A-unit (word processor) to consume a shared `ToolbarMetrics` (the user's WIP adds one) |
| F6-25 | Identity strings | Every label is an English literal, not a fluent key | — | `identity_flow/*` | — | open — i18n pass, not alignment; needs the unit owning `ui` locales |
| F6-26 | Identity: text fields | **`identity_flow` hand-rolls text inputs (pre-existing)** — `LineEditState` + a painted box; this review consolidated the seven per-screen copies into one `sheet::paint_field` that mirrors `TextInput`'s look and paints selection/caret only where the old screens already did (create-identity name, the three verify blanks); no input behaviour was added. Still HIG §8 / CLAUDE.md "never hand-roll a text input". | — | call sites: `create_identity.rs` display name; `recovery_verify.rs` blanks ×3; `invite_accept.rs` invite link; `pair_device.rs` payload textarea; `pair_device.rs` six digit cells | — | **open — F6 follow-up**: migrate to the shared `TextInput` via `TextInput::apply_layout_external(rect)` with forwarded `draw`/`handle_event` (the `ModalPrompt` precedent), one screen at a time, keeping `display_name()`/`set_answer()`-style accessors for the protocol tests |
| F6-30 | Identity: QR painter | Review finding: the rect painter dropped `QrWidget`'s error path — a payload the encoder refused painted a blank white square | — | `sheet.rs` `paint_qr` | — | fixed — paints the widget's `QR error: …` in a field cell, as `QrWidget::draw` does |
| F6-31 | Identity: body vs. actions | Review regression in the first sheet version: rows were not confined to the card, so at 2× text the acknowledgement row ran under the action buttons and took Continue's click | 1280×800, text 2× | `sheet.rs`, `recovery_share.rs` | — | fixed — body paints under a clip, rows scroll (`BodyScroll`: wheel, PageUp/PageDown) and a row's pointer target is its part inside the body; `at_two_x_text_the_ack_row_never_overlaps_the_actions_and_scrolls_into_view` |
| F6-32 | Identity: hit rects | Review regression in the first sheet version: geometry resolved only in `draw`, so a click before the first paint, or after a state change that reshaped the body, hit stale or zero rects | — | every screen | — | fixed — one `resolve` runs from `apply_layout` (against a layout-pass renderer at the theme's text scale) and again from `draw`; `hit_rects_are_resolved_by_apply_layout_without_a_paint`, `resolve_agrees_between_the_layout_renderer_and_a_frame_renderer` |
| F6-33 | Identity: invite link | Review regression in the first sheet version: the minted URL was elided to one line with no way to read it | — | `invite_mint.rs` | after capture | fixed — wrapped by character (`wrap_chars`) into as many rows as it needs; `plan_shows_a_centred_qr_and_link_only_once_minted` asserts the concatenated rows equal the URL |
| F6-27 | `ModalFrame` | Scales its own padding by `text_scale` while spacing tokens scale by `ui_scale` only; body rows here follow the Theme rule (`spacing` unscaled by text) | text 1.5× | `modal_frame.rs:361-380` | — | open — needs F4 |
| F6-28 | `examples/identity_flow_demo.rs` | Cannot seed: `mint_invite` fails with "invite redemption broker unavailable" outside `cfg(test)` — the demo exits before showing a screen | — | `identity_flow_demo.rs:184` | — | open — pre-existing; the `f6` test configures a loopback broker instead |
| F6-29 | ux-bench `open-files` | `visible semantic:Files` → `missing or ambiguous target: semantic:Files` at base, before any change here | 1280×800 | shell | `target/ux-bench/f6-before-open-files/report.json` | open — pre-existing, needs S1/S2 |

## Off-grid metrics kept (and why)

`tabs.rs` (documented in the metrics block above the consts):
`TAB_PILL_INSET_Y` 6 (bar 44 is pinned by the shell — `foreign.rs`, `screenshot_test`), `PILL_GAP` 6, `ICON_GAP` 6, `CLOSE_W` 18, `ACTION_W` 14,
`ICON_W` 14, `CLOSE_INSET` 3, `TITLE_CHIP_GAP` 10, `CHIP_FADE_PX` 10,
`PILL_SQUEEZE_FLOOR_PX` 54, `NEW_TAB_GLYPH_PX` 11. Lucide glyph sizes and
optical gaps around a 32 px pill; rounding any of them moves every pill width
the layout tests pin. On-grid and expressed as such: `PILL_PAD_X` 12 (`s3`),
`CLOSE_HIT_PX` 32 (`s8`), `ACTION_GAP`/`ACTION_HIT_INSET_Y` 4 (`s1`).

`window_group.rs`: `MAXIMIZED_CHIP_H` 22 (F6-23), `MAXIMIZED_TITLE_CHIP_MIN_W`
80, `MAXIMIZED_RESTORE_CHIP_W` 76, `RESIZE_HANDLE_PX` 6 (pinned by tests),
`GRIP_BAR_PX` 2. `resize_handle.rs`: `HANDLE_SLOT_PX` 8 (`s2`),
`HANDLE_HIT_HALF_PX` 8, `HANDLE_VISIBLE_PX` 2 / `HANDLE_HOVER_PX` 3 — the
handle is centred on the gap by construction (`set_neighbour_rects`).
`page_sidebar.rs`: `SCROLLBAR_W` 6 (matches the shared scrollbar).
`chrome/toolbar.rs`: see F6-24. `status_bar.rs`: 28 = §9.6 control height.

Colour literals deliberately kept (counted in the ratchet): `tabs.rs` and
`window_group.rs` opaque-white × glyph over the danger fill — pinned by
`close_hover_glyph_is_full_white` and `maximized_close_chip_uses_danger_token_on_hover`;
`sheet.rs` QR black/white — the scanner contract, same as `qr.rs`.

## Tests

Every cargo command ran as
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." cargo … --locked --no-default-features`
(both of the coordinator's workarounds for the merged parent `.cargo/config.toml`;
`--no-default-features` is a no-op for `ui`, which declares no default features).

| command | result |
|---|---|
| `cargo test -p ui --lib -- tabs:: window_group:: group_row:: identity_flow:: chrome:: page_sidebar:: resize_handle:: tile_host:: resizable_split::` | **352 passed, 0 failed** (includes the new tests: `tabs::{leading_glyph_sits_on_the_pill_inset_and_the_title_follows_by_icon_gap, every_chip_shares_the_centre_line_and_one_hover_shape, chip_hover_tint_is_derived_against_the_ground}`, `window_group::maximized_chrome_chips_share_one_row_and_a_token_gap`, `identity_flow::sheet::*` (13, incl. `body_scroll_clamps_and_hides_rows_outside_the_body`, `resolve_agrees_between_the_layout_renderer_and_a_frame_renderer`, `wrap_chars_breaks_an_unbreakable_token_into_lines_that_fit`), one `plan_*` test per screen (7), `create_identity::hit_rects_are_resolved_by_apply_layout_without_a_paint`, `recovery_share::at_two_x_text_the_ack_row_never_overlaps_the_actions_and_scrolls_into_view`, `chrome::status_bar::*` (3)) |
| `cargo test -p ui --test it -- f6_identity_flow_layout` (dark, light, high-contrast, text 2×) | 2 passed, 0 failed; 32 captures written |
| `cargo test -p ui --test it -- design_token_audit f6_identity_flow_layout group_row_visual_journey window_group_visual_journey screenshot_test tabs_test window_group_test strip_view_test workspace_view_test tile_app_widget` | 28 passed, **2 failed — both pre-existing at base**, see below |
| `cargo test -p ui --test it -- design_token_audit f6_identity_flow_layout` (after lowering the baseline) | 4 passed, same 2 pre-existing failures; this unit's files no longer appear in either |
| `cargo test -p shell --lib -- tab` (shell inline tests pinning tab/group/strip chrome) | **144 passed, 0 failed** |
| `cargo check -p ui --tests` | clean, 0 warnings |
| `cargo check -p shell --lib --bin ux-bench --bin shell --bin qos-control` | clean |
| `cargo check -p shell --lib --bins` | fails on `shell/src/bin/click_test_client.rs` only: `unresolved import smithay_client_toolkit` / `wayland_client` — a Wayland client binary with Linux-only dependencies, unbuildable on macOS before and after this change |
| `cargo build -p pdf_viewer -p slides --bins` + `--shots` scenarios | built; 6/6 shots rendered (dark + light) |

Code review: the `code-review` skill was invoked twice (`high`, then
`medium`); its forked runs reported through the coordinator rather than
inline. Findings and resolutions:

- *Removed behaviour, three regressions in the first sheet version* — body
  rows not confined to the card (F6-31), hit rects only populated in `draw`
  (F6-32), invite URL elided (F6-33): all three fixed, each with a test.
- *Reuse: `sheet::paint_field` is a hand-rolled text input* — resolved as
  option (b): the consolidation stays, it paints nothing the seven original
  copies did not, and F6-26 is the top-level Open follow-up with the call
  sites and the `TextInput::apply_layout_external` migration path.
- *Reuse: `paint_qr` dropped `QrWidget`'s error path* — fixed (F6-30).
- A manual pass over the rest of the diff found one cleanup (a redundant
  double `text_rect` in `paint_field`'s multiline branch), fixed.

Pre-existing failures, verbatim (files outside this unit; they fail at the
base commit before any change here):

```
design_token_audit::theme_escape_hatches_do_not_grow
  src/text_render/renderer_regressions.rs [white_or_black]: 5 occurrence(s), baseline 0
design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow
  apps/browser/src/browser_tab_strip.rs [literal_color]: 1 occurrence(s), baseline 0
  shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0
  shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0
```

Because the growth assertion fires first, the ratchet cannot print its
"lower these" list; the counts for this unit's files were recomputed with the
test's own rules (`target/ui-review/ratchet_counts.py`, a transcription of
`ratcheted_counts`) and the baseline lowered to match: `status_bar` 1→0,
`toolbar` 1→0, `tabs` 5→2, `page_sidebar` white_or_black 2→0, the eight
`identity_flow` files 28→0 in total, plus `identity_flow/sheet.rs literal_color 2`
(new file; the QR contract, net −26 for the module).

The `f6` integration test failed before the fix with 21 "screen is 720×580,
must fill the 1280×800 viewport" (and sibling sizes) messages and passes
after; the identity screens were not addressable through `targets()` at all
until `IdentityFlow` exposed its children.

## E2E captures

All PNGs were opened and inspected with the Read tool.

| what | before | after |
|---|---|---|
| Identity screens ×7 + Done, dark/light/high-contrast (+ text 2× after), 1280×800 | `target/ui-review/ui/before/identity_*.png` | `target/ui-review/ui/after/identity_*.png` — `identity_recovery_{share,verify}_text-2x.png` show the word grid scrolling under the body clip with the action row clear |
| Tab pill row (fresh window group, group row left/right, maximized chip), 4× crops | `target/ui-review/ui/before/zoom_*.png` (+ `window_group_*.png`, `group_row_*.png`, `tab_bar.png`) | `target/ui-review/ui/after/zoom_*.png` (+ same set) |
| Compositor: desktop, open-files (tabs), navigation-layout | `target/ux-bench/f6-before{,-open-files,-nav}/` — desktop PASS, nav PASS, open-files FAIL (F6-29) | `target/ux-bench/f6-after{,-open-files,-nav}/` — desktop PASS, nav PASS, open-files FAIL with the identical single check |
| Compositor single frame | `target/ui-review/shell-before.png` | `target/ui-review/shell-after.png` |
| Compositor pill row, 8× crop | `target/ui-review/shell_pill_before.png` | `target/ui-review/shell_pill_after.png` — glyph moved 2 px onto the text inset, title and × unchanged |
| `page_sidebar` consumers (pdf_viewer `page-bubble`, slides `overview`/`narrow`), dark + light | none (see Limits) | `target/ui-review/{pdf_viewer,slides}/after/*.png` |

What changed visibly: the identity sheets went from a top-left 720×580 scrim
with a hand-drawn card to a viewport scrim with a centred `ModalFrame` card,
measured rows, real buttons, a drawn QR and an elided link; the tab pill's app
glyph moved from `x0+10` to `x0+12` with the title fixed; the maximized-chrome
row and grip are pixel-identical in placement except the restore icon now
sits on the chip's centre line. Nothing pinned by the shell moved:
`TAB_BAR_HEIGHT` 44, `TAB_PILL_INSET_Y` 6, `NEW_TAB_CHIP_PX` 32,
`RESIZE_HANDLE_PX` 6, `HANDLE_SLOT_PX` 8, pill widths (title x unchanged).

## Limits

- No BEFORE shots for the `page_sidebar` consumers: producing them would have
  meant a second full app build from the base tree on a memory-constrained
  host; the rail's geometry changes are pinned by its own unit tests instead.
- `apply_layout` has no text renderer, so the sheet measures its rows there
  against a thread-local layout renderer (`sheet::with_layout_renderer`, one
  font system per thread, re-scaled to the theme's text scale each pass).
  The frame renderer shapes with the same fonts, and
  `resolve_agrees_between_the_layout_renderer_and_a_frame_renderer` pins
  that the two produce identical geometry; a host that swapped fonts between
  the two would break that agreement.
- A body that overflows scrolls by wheel and PageUp/PageDown; there is no
  scrollbar painted for it yet (the sheet has no retained `Scrollable`), so
  an overflowing body is discoverable by wheel, not by a visible affordance.
- `ModalFrame` is F4's; its 20 px pad and 34 px control height are inherited
  as-is (the control height is read from the layout, never restated).
- The identity screens remain a `ui-lab` surface: the shell's real onboarding
  is `shell/src/onboarding_view.rs`, which reuses only the crypto half of
  `identity_flow`.
- `tabs.rs` keeps the action-chip pointer targets at 14×24 (F6-22); widening
  them is a deliberate cluster-width change that moves pinned pill widths.
