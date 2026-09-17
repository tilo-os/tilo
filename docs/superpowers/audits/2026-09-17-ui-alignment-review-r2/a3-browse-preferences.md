# UI alignment review, round two — A3: Browsing & preferences (store, settings, themes)

Worker unit A3 of the 2026-09-17 second round. Base: the merged tip `381eff6a8`
("Merge ui-review/integration: UI alignment, spacing and layout review (18 units)").
Branch `ui-review/r2-a3-browse-preferences`, local only. Round one's report is
`../2026-09-16-ui-alignment-review/a3-browse-preferences.md`; its ids (S1–S12, T1–T8,
U1–U7) are referenced below.

This round had three jobs: prove round one survived the merge (including the user's
native-observer WIP in `settings_app.rs`), close the nine items left open, and look again
at 200 % and phone width now that F1's `Metrics` tokens and F2's control text scaling are
in the tree.

## Merge verification

- Rebuilt `store`, `settings`, `themes` at the merged tip and ran the settings `it` binary
  (`composition_balance`, `alignment_review`, `category_*`, `control_interaction`, `scroll`,
  `search_focus`, `no_app_title_header`, `shots_smoke`): **37 passed, 1 failed at the base**.
  `alignment_review::region_pane_keeps_a_readable_form_measure_and_stays_fluid_when_narrow`
  — `label vs dropdown centre: expected 180, got 180.5`. Cause: F2's text metrics give the
  13 px label an odd-height line box, and taffy rounds every node to whole pixels, so the
  label centred in the 40 px row lands exactly half a pixel off the dropdown's centre. Not
  a misalignment. That one assertion (and one new centre-line check of the same shape) now
  uses a documented `same_centre_line()` helper that admits exactly the engine's rounding
  step (`<= 0.5`); `close()` itself stays at `< 0.5` for the other sixteen assertions. No
  other round-one assertion moved.
- Recaptured BEFORE (store 22, themes 25, settings 82 PNGs, same recipe as round one) and
  compared with round one's AFTER: every round-one fix is intact — settings S1–S8 (card
  rhythm, caption gap, About hero, sidebar pill on the search-field edge, section header
  ink, measured label centring, Region readable measure, 40 px account buttons), store
  T1–T6 (filter band centre line, detail hero gaps, remotes/moderation header, catalogue
  rhythm), themes U1–U4 (narrow selector gutter, chip grid wrap, 40 px customizer actions,
  `children()` mirror). Evidence: `target/ui-review/{store,settings,themes}/before/`.
- 200 % control text (T8, S12 in part): Button and Dropdown labels scale in the BEFORE
  captures — F2 landed. Verified again in AFTER. Recorded as fixed by F2.

## Method

1. Source pass over the three apps for the nine open items, plus every remaining pixel
   literal, hand-painted control and fixed row/section height.
2. BEFORE captures: responsive matrix dark + light, 200 % (800 × 900, `--text-scale 2`),
   480 × 900, 800 × 700, every store scenario, every settings panel (wide dark + light,
   narrow, five panels at 200 %), every themes shot mode (wide + narrow, two at 200 %).
   Every PNG opened.
3. Fixes as widget parameters, composition and tokens (`theme.tokens.spacing`, `metrics`,
   `typography.scale`, `Spacing::BASE`/`Metrics::BASE` for compile-time constants).
   App-painted text takes theme type sizes through `TextRenderer::renderer_input_style`.
4. Geometry regressions through the production `sync → build_layout → compute_layout →
   apply_layout` cycle, now also at `text_scale = 2` with the renderer told the same scale.
5. AFTER captures with the same scripts (`target/ui-review/cap.sh`, `cap-extra.sh`) from
   the final binaries; each finding compared before → after.

One environment fact found on the way: settings' private `--shots` host
(`apps/settings/src/shots.rs`) scaled the *theme* by `--text-scale` but never told the
`TextRenderer`, while the live `ui::App`, the shared launcher and the headless harness all
do. Every 200 % settings capture from round one therefore under-reported app-painted text
(the sidebar labels, one intro paragraph). Fixed here (one line); the 200 % evidence below
is from the corrected host.

## settings

Scope: `apps/settings/src/{shots,sidebar,widgets,settings_app}.rs`, `panes/{region,
agent_models,permissions}.rs`; tests `tests/alignment_review.rs` (+3, one helper).

| id | surface | defect | route / viewport | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| r2-S1 | `--shots` host | renderer left at 1× while the theme carried the text scale; app-painted text under-reported at 200 % | `large-text.png` | `shots.rs:472` | sidebar labels 14 px → 28 px; the "Connect email…" paragraph 1× → 2× (`after/large-text.png`) | fixed — `set_text_scale(theme.text_scale)` |
| r2-S2 (S12) | sidebar row | fixed 40 px leaf; at 200 % a 28 px label overflowed the 32 px pill; `264/40/20/14/12` restated tokens | `large-text.png` | `sidebar.rs:45–65, 208–225` | rows measured: label line, wrapped to the pill's inner width (taken from the known or the definite available width taffy hands `measure`, so the last measure and `draw` wrap identically) + 4·`s1`, floor `min_pointer_target + 2·s1` = 40, so 1× rows are exactly 40 (`after/dark/…-laptop.png` identical to before); label type `scale.base` through `renderer_input_style`; `SIDEBAR_WIDTH = DEFAULT_SHELL_SIDEBAR_WIDTH`, `ROW_ICON_COL = icon_md`, `SHELL_FIELD_INSET = Spacing::BASE.s3` | fixed — `sidebar_rows_seat_the_pointer_target_at_1x_and_grow_with_large_text` (1× exact; 2× rows taller than 40 and shorter than two lines) |
| r2-S3 | shell breakpoint / sidebar width | 720 / 264 fixed under large text: an 800 px window at 200 % kept two panes with 28 px labels in a 264 px column | `large-text.png` 800 × 900 | `settings_app.rs:45, 424–428`, `shots.rs:431` | breakpoint and sidebar width × `text_scale` (`shell_scale`, never below 1×), read by the shell, `is_narrow()`'s pre-shell fallback, the pane width hint and the responsive-capture scenario picker alike; 800 × 900 at 200 % is the single content screen with the back arrow (`after/large-text.png`); 1× unchanged; `NAV_BREAKPOINT = DEFAULT_SHELL_BREAKPOINT` (`Metrics::compact_breakpoint`) | fixed app-side — `large_text_scales_the_shell_breakpoint_with_the_type`; the policy belongs in `AppShell::build_layout` (it has the theme and a state-preserving `pending_rebuild`) — **open, needs F1/F6** |
| r2-S4 (S9) | Region & Formats on a phone | six `choice_row`s wrapped independently: "Currency" inline, the rest stacked with a left-aligned 220 px chooser | `dark/…-phone.png` 390 × 844 | `widgets.rs:105–112`, `region.rs:131–139` | the Formats card switches as one: below a 356 px card (`COMPACT_CONTROL_W + s3 + 100 px label reserve + 2·s3`) every row is `choice_row_stacked` (label, `s1`, chooser spanning the card); at 440 and 480 every row is inline (`after/dark/…-phone.png`, `after/narrow.png`). The pane composes for the host's width hint on its first frame and its own rect after | fixed app-side — `region_choice_rows_switch_layout_as_one_card`; the per-section trailing-control policy still belongs in the toolkit's `Field` — **open, needs F3** |
| r2-S5 (S10) | theme-aware build-time spacing | lone gaps were `Container`s of fixed px; eight `hgap(8)` between row buttons stacked on the row's own gap (the S8 pattern), `hgap(10)`, `vgap(2)`, `vgap(12)` | Agent models, Permissions | `widgets.rs:66–77`, `agent_models.rs` ×8, `permissions.rs:294, 452, 484` | `gap(SpacingStep)` = `Spacer::token`, resolved against the live theme at layout time; `vgap`/`hgap` → `Spacer::fixed`; the eight stacked spacers and the pill→text spacer removed (one row gap), `vgap(2)` → `S1`, `vgap(12)` → `S3` | fixed for lone gaps; `Column::with_gap` is still a pixel (`CARD_GAP`/`ROW_GAP` unchanged) — **open, needs F1** (`with_gap(SpacingStep)`) |
| r2-S6 (S11) | sidebar ↔ shell inset | `SHELL_FIELD_INSET` restated 12 | — | `sidebar.rs:65` | names `Spacing::BASE.s3`, the token `AppShell`'s private `PAD` is built from | narrowed; export still — **open, needs F1/F6** |
| r2-S7 | page column | `24 / 20 / 16` literals | all panels | `settings_app.rs:349–353` | `PAGE_GUTTER = s6`, `CARD_GAP`, `s4`; `PAGE_INSET` (= `s3` + gutter) names what a pane's width hint subtracts | fixed |
| r2-S8 | shell search field at 200 % | placeholder stays 1× (shared `TextInput` `with_font_size(13.0)`) | `large-text.png` | `settings_app.rs:331` | unchanged | **open — F2** (`TextInput` type token) |
| r2-S9 | 200 % dropdown chevron | 5 px chevron fixed in the shared `Dropdown` | `panels-large/panel-appearance.png` | `ui/widget/dropdown.rs` | unchanged | **open — F2** |

Conventions observed: every row type keeps label and control on one centre line (S7's
test now tolerates taffy's half-pixel); `ROW_HEIGHT` is `min_pointer_target + 2·s1`;
`CARD_GAP` 20 stays the documented off-scale step; `ROW_GAP` is `Spacing::BASE.s3`.

## store

Scope: `apps/store/src/app.rs`, `widgets/card.rs`; tests `tests/alignment_review.rs` (+2).

| id | surface | defect | route / viewport | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| r2-T1 (T7) | status pill | bottom-right pill and the hover band that reveals it sat where every card puts its actions: 800 × 700 covered the Chess card's **Install**; 1366 × 768 clipped the Notes card at **Open**; 200 % covered the Files card's **Details** — so moving the pointer to a card action *revealed* the pill over it | `compact.png`, `dark/…-laptop.png`, `large-text.png` | `app.rs:2842–2900` | pill at the window's bottom-**left**, on the catalogue's leading edge (`(w − 1120)/2` on a roomy desktop, `padding_standard` otherwise), one `s4` above the bottom; hover band `s6` above and `min_pointer_target` beside it; label `scale.sm` through `renderer_input_style`; the end-of-list landing zone is the pill band (pill height + 2·`s4`, `status_bubble_band`) instead of `72.0`. `after/compact.png`: pill on the VLC card's blank corner, Install clear | fixed — `status_pill_never_floats_over_a_card_action` (four viewports, no `Button` intersects), `status_pill_landing_zone_follows_the_text_scale` |
| r2-T2 (T8) | 200 % control text | Button / Dropdown labels | `large-text.png` | shared widgets | scale in BEFORE and AFTER | fixed by F2 — verified |
| r2-T3 | spacing literals | off-scale 7 / 10 / 5 / 6 (section header, detail summary, facts, remotes) and on-scale 12 / 8 / 4 literals in header, detail, safety, remotes, card grids | all | `app.rs` ×16, `card.rs:63` | `s1` / `s2` / `s3` from the theme in scope (CardGrid gutters `f64::from(s2)`) | fixed |
| r2-T4 | 200 % pill height | the shared pill pads from the renderer-relative size, so 24 px glyphs sit in a ~30 px pill at every scale | `after/large-text.png` | `ui/status_bubble.rs:118–124` | unchanged; `status_bubble_band` reserves the height the pill *should* have (theme-scaled `sm` + the helper's padding), documented as generous until the helper pads from the theme size — the band test compares reservations, not the painted pill | **open — F-unit** (`pill_size`/`draw_pill` should take the theme-scaled size; export one `pill_height` for callers) |
| r2-T5 | stacked layout scrollbar | overlay bar paints over the card column's right border at 800 px (`with_content_gutter(0.0)`) | `compact.png` | `app.rs:2805` | unchanged — a gutter would break the card/header right edge | recorded |
| r2-T6 | card artwork at 200 % | 36 px artwork beside a 2× title | `large-text.png` | `card.rs` | unchanged (an image, not text) | recorded |

## themes

Scope: `apps/themes/src/{app,widgets,gallery,preview,customizer}.rs`; tests
`tests/alignment_review.rs` (+4, one helper).

| id | surface | defect | route / viewport | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| r2-U1 (U5) | gallery toggle | hand-painted 34 px rect with a cursor hit-test in `handle_overlay_shortcuts`; at 200 % / 480 the actions row painted over it | `narrow.png`, `mode-customize-large.png` | `app.rs:64–66, 345–410, 1253–1265` | a shared icon-only `Button` in the top bar's leading slot, `bar_height` square: `Icon::Menu` / "Show themes" while the gallery is hidden, `Icon::X` / "Hide themes" once it is more than half revealed (two Buttons posting one request, swapped in `sync` with a single tree rebuild, since `Button` has no `set_icon`); click via `on_click`, keyboard and assistive routes via the Button; scrim tap excludes it; in `children()`/`children_mut()` | fixed — `gallery_toggle_is_a_shared_button_on_the_bar_control_height` (geometry, tree membership, spoken name in both states) |
| r2-U2 | top bar | 34 px controls, 18 px padding, `56.0`; no wrap, so 200 % actions overflowed | `large-text.png`, `mode-customize-large.png` | `app.rs:51, 209–219, 791–840` | controls `bar_height`; padding `s4` (= the content gutter, so toggle and rows share an edge); gap `s2`; `TOPBAR_H = bar_height + 2·s2` (56, 1× exact); the bar wraps its actions onto their own right-aligned line(s) inside the window and its measured rect drives the scrim, flap origin and paint (`after/mode-customize-large.png`) | fixed — `top_bar_wraps_its_actions_under_large_text_instead_of_covering_the_toggle` |
| r2-U3 (U6) | gallery rows | fixed 58 px rows: name and tag collided at 200 %, names ran past 272 px; `11/51/62/9/33/10` literals; 34 px header band | `large-text.png` | `widgets.rs:33, 136–260`, `gallery.rs:28–30` | rows measured (name `scale.base` + `s1` + tag `scale.sm`, wrapped to the row's text width, floor 58 → 1× exact); `bar_height` swatch square centred; `s3` insets; badge `scale.xs` sized from its line; `name_rect` follows the drawn line; gallery `PAD` → `s4`, `GAP` → `s2`, header band `max(34, s3 + sm line + s1)`, count `scale.sm`. `after/large-text.png`: two-line names, tags below, header in its band | fixed — `gallery_rows_keep_their_1x_height_and_grow_with_large_text` |
| r2-U4 (U6) | preview specimen | fixed section heights and in-row offsets collapsed at 200 % | `mode-live-large.png` | `preview.rs:30–150, 690–760, 749–870, 946–990` | text-driven heights (title bar, section labels, bodies, list rows, badges) × renderer text scale, clamped at 1× in one place (`geometry_scale`) for both the allocated pane and the drawn specimen; list row glyph offsets, avatar and badge × `rowh / LIST_ROW_H`; the toggle, checkbox, their labels, the slider origin and the semantic chips × the same factor; `narrow_intrinsic_height(text_scale)`; charts and pane margin fixed. `after/mode-live-large.png`: each section in its own band, list rows two lines | fixed — `specimen_and_customizer_reflow_with_large_text` |
| r2-U5 | customizer | header, chip caption and control labels collided at 200 %; `16/11/12` literals | `mode-customize-large.png` | `customizer.rs:38–59, 623–700, 702–745` | header offsets, caption and the label offset × `text_scale`; inter-control gap × scale (the labels ride in it); sizes `lg`/`sm` through `renderer_input_style`; `intrinsic_height_for_width_scaled`, `chip_grid_height_scaled` (1× APIs kept for the round-one tests) | fixed — same test |
| r2-U6 (U7) | named off-scale values | gallery `PAD` 14, `GAP` 8, `NARROW_CONTENT_GUTTER` 16, bar padding 18, `TOGGLE_SIZE` 34 | — | — | → `s4`, `s2`, `s4`, `s4`, `bar_height`. Kept and documented: `customizer::GAP` 22 (label above track), `PANE_MARGIN` 22, `GALLERY_W` 272, `CUSTOMIZER_W` 344, `BREAKPOINT` 840, `ROW_H` 58 | fixed where on-scale |
| r2-U7 | `ToggleGroup` at 200 % | segment labels overflow their segments | `mode-live-large.png` | `ui/widget/toggle_group.rs` | unchanged | **open — F2/F3** |
| r2-U8 | `ColorPicker` readouts at 200 % | hex / rgb readouts run off the picker | `mode-customize-large.png` | `ui/widget/color_picker.rs` | unchanged | **open — F2/F5** |
| r2-U9 | keyboard focus order | `ui::App::collect_focusable` pairs `children()` positionally with the root's taffy children (`[topbar, content, gallery?]`); the app's `children()` lists the bar's buttons individually, so the pairing was already wrong before this round and the toggle shifts it by one more | — | `app.rs` `children()`, `ui/src/app.rs:5236` | unchanged — needs either a composite top-bar widget in the app or a `children()`/taffy contract in `ui` | **open — pre-existing; F6 + themes follow-up** |
| r2-U10 | gallery list | 14 rows exceed 768 px and the list does not scroll | `dark/…-laptop.png` | `gallery.rs` | unchanged, pre-existing, not alignment | recorded |

## Tests

Every cargo command ran as `RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G
CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo …
--locked --no-default-features`; no cargo config was edited. Results from the final code:

- `cargo test -p settings --test it` — **41 passed** (category_nav 16, category_sidebar 1,
  control_interaction 3, no_app_title_header 2, scroll 1, search_focus 3, shots_smoke 1,
  composition_balance 3, alignment_review 8 = 5 round one + 3 new), 0 failed.
- `cargo test -p settings --lib -- sidebar widgets panes::region` — 15 passed.
- `cargo test -p settings --test accounts_mail_loop --test prefs_persistence` — 1 + 6 passed.
- `cargo test -p store --test alignment_review` — **6 passed** (4 + 2 new);
  `cargo test -p store --lib` — 103 passed.
- `cargo test -p themes --test alignment_review` — **7 passed** (3 + 4 new);
  `cargo test -p themes --lib` — 52 passed.
- `cargo check -p store -p settings -p themes --tests` — clean, no warnings.

New regressions run the production layout cycle at `text_scale = 2` with the renderer told
the same scale (`lay_out_scaled`, `lay_out_widget_scaled`), so a 200 % geometry claim is
asserted, not screenshotted. Two assertions were written and then corrected against the
tree rather than against the intent: the store pill's "leads every button" (the compact
masthead's own leading control shares the edge — now `<=`), and the region caption gap
(taffy rounds adjacent edges independently — now `s1` within one pixel, stated as such).

## Code review

The shared `code-review` skill (effort high) ran on the finished diff and returned ten
findings. Fixed before commit: the specimen's text-scale clamp was applied in `new` but not
in `minimum_pane_height` (below 1× the pane would be allocated shorter than the specimen
drawn — one `geometry_scale` now serves both); the badges and controls specimen rows kept 1×
shapes around scaled glyphs (scaled); the settings responsive-capture scenario picker and the
pre-shell `is_narrow()` / width-hint fallbacks compared against the unscaled breakpoint and
sidebar width while the shell used the scaled ones (one `shell_scale` now feeds all four);
`SidebarRow::measure` and `ThemeRow::measure` fell back to a constant width when
`known.width` was `None`, which taffy's final pass hands a leaf, so the sticking height could
be wrapped at a different width than `draw` (both now take the definite available width
first; the settings 2× test asserts rows are one line, not two); the shared `close()` helper
had been loosened for every assertion in the file (restored to `< 0.5`; the one merge-caused
half-pixel case uses a dedicated, documented `same_centre_line`); the open-state close glyph
the hand-painted toggle used to draw had been lost (restored by swapping a Menu/X Button
pair); the `status_bubble_band` doc claimed the shared pill's rule while the shared pill pads
from the renderer-relative size (doc corrected, toolkit item recorded as r2-T4); dead
`region::build`, `vgap`, `hgap` removed; `footer_label`/pill size computed once per frame.
Recorded rather than fixed (outside the unit or pre-existing): `AppShell` should own the
large-text breakpoint policy (r2-S3); `App::collect_focusable`'s positional pairing (r2-U9);
five app-side `renderer_input_style(..).size` twins that a `ui` helper should replace — the
same `renderer_type_size` consolidation round one's index already lists.

## Limits

- No workspace-wide build or test; suites scoped as listed.
- The shots hosts are the evidence path on macOS (no compositor, no `qos-control`).
  Settings' host now matches the shared one for text scale; the store and themes hosts
  already did.
- The Region stacking threshold is an app-side composition rule (`choice_rows_stack_below`)
  because the toolkit's `Field` has no per-section trailing-control policy yet (F3). The
  settings shell's large-text policy (breakpoint × text scale) is likewise app-side until
  `AppShell` resolves the scaled `Metrics` itself (HIG §9.6 names this as the shells' next
  step).
- `Button` has no `set_icon`, so the gallery toggle's two states are two Buttons swapped
  with one tree rebuild when the flap crosses half-open; a `set_icon` (F2) would make it one.
- App-painted text takes theme type sizes through `renderer_input_style(..).size` and the
  legacy `measure`/`draw_text` path in five places across the three apps; the documented
  pairing is `measure_style`/`draw_style` with the converted style, and `ui` already carries
  two crate-private size-only twins. One exported helper would replace all of them (F2/F3).
- Capture directories: `target/ui-review/{store,themes,settings}/{before,after}/`
  (store 22, themes 25, settings 82 PNGs per tag); helper scripts `target/ui-review/cap.sh`,
  `cap-extra.sh`.
