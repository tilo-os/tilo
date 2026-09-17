# F2 — Core controls and text inputs (round two)

Unit F2 of the 2026-09-17 UI alignment review, round two (19 parallel units).
Base: merged tip `381eff6a8` (`Merge ui-review/integration`, which carries round
one). Branch: `ui-review/r2-f2-controls-text-alignment`. Local only; not pushed.

## Scope

`ui/src/widget/{button, checkbox, toggle, toggle_group, segmented, slider, chip,
badge, spinner, progress_bar, gauge, number_input, search_field,
shortcut_recorder, date_picker, time_picker, color_picker, editable_label,
name_field, undo_button, text, input_hint, breadcrumb, link, icon, avatar,
divider, toolbar, text_input, block_editor, rich_text_editor, rich_text_line,
code_editor, marks}.rs`.

Touched outside the owned list: `ui/tests/f2_control_row_alignment.rs` (the F2
covenant test from round one, extended) and one line of
`ui/tests/filled_control_labels_are_readable.rs` (the style sweep gains the new
`ButtonStyle` — a sweep that skips a style it can see is the defect that file
exists to prevent).

Not touched, per the batch brief: every text-size double-scaling and
text-centring expression (unit X1); `ui/src/history_mode/*` (user WIP);
`widget/mod.rs` and `widget/layout.rs` (F1).

## Method

1. Re-read round one's F2 report and the hand-offs to F2 in A5, A6, A9, A10 and
   F1; verified each against the merged tree (see "Merge survival" below).
2. BEFORE captures from the pristine tree: settings, store, notes, remote and
   ebook (`--responsive-matrix` dark + light, 2× `--shots`, isolated profiles)
   and the five `ui-shots` fixtures — 63 PNGs, every one opened.
3. Source pass over every owned `build_layout`/`measure`/`draw`/`handle_event`
   for the round-two brief: minimum targets from `metrics`, the default
   `Button` at 2×, fixed-target leaves in overflowing rows, `Chip`/`Avatar`
   geometry, `BlockEditor` literals, `NumberInput`'s width, an outlined
   danger face, icon-only faces under the pseudo-locale, state-invariant
   geometry.
4. Fixes with tokens and measured text; **no 1× default metric changed** —
   proven by the 1× captures (all 36 app-matrix PNGs at 1× identical except
   Notes, whose heading step is the one deliberate typography change).
5. `/code-review high` over the diff; every finding dispositioned below.
6. AFTER captures from the final code, byte-compared against BEFORE; every
   changed PNG opened.

## Findings

Coordinates are logical pixels. "1×" = default theme, "2×" = `text_scale 2.0`.

| ID | Surface | Defect | Where | Evidence | Status |
|---|---|---|---|---|---|
| R2-F2-01 | `Toolbar` minimum hit rect | Sized from `spacing.s8`, so a dense custom-theme `spacing_px` shrank the pointer target (F1's R1 hand-off; HIG §7b) | `toolbar.rs` `recompute_visible`, `build_layout`, bootstrap | unit test with `Spacing::BASE.scaled(0.5)`: buttons stayed 16 px before | **Fixed** — `metrics.min_pointer_target` at all three sites; test `minimum_hit_rect_reads_the_pointer_target_not_the_spacing_scale` |
| R2-F2-02 | Default `Button::new` (120×40) at 2× text | The 28 px label's 35 px line sat in a 40 px face and "Add account" (~150 px) painted outside the 120 px width (round-one F2-15) | `button.rs` `build_layout` pinned `length×length`, so taffy never measured | `settings/before/large.png` (632–766, 470–512): label outside the face | **Fixed** — new `adjust_layout_with_measure`: the default footprint is `DEFAULT_SIZE` floored at the measured label — height `max(40, line + padding_standard)` (the `content_sized` rule, so both auto-height modes agree), width `max(120, label + icon + 2×FIT_SIDE_PADDING)`. 1× stays exactly 120×40 for every label under 90 px; 2× → 47 tall; explicit `with_size`/`fit_content`/`fill_width` heights untouched. Tests: `default_footprint_is_exactly_the_shipped_120_by_40_at_one_x`, `default_footprint_grows_from_the_measured_line_at_two_x`, `a_wide_label_widens_the_default_button_instead_of_overflowing_it`, `an_explicit_size_is_never_grown_by_the_measured_footprint`, and `f2_control_row_alignment::default_buttons_hold_their_label_at_two_x_and_stay_120_by_40_at_one_x` on the production `FrameHarness` path |
| R2-F2-03 | Single-line `TextInput` at 2× | Fixed 40 px band; the 16 px face shapes a 40 px line at 2× — zero slack, glyphs touching the field's edges | `text_input.rs` `build_layout` | `store/before/large.png` search field (y 148–188) → `store/after/large.png` (148–204) | **Fixed** — band = `max(40, scaled line + 2×ML_PAD_Y)`: 40 at 1× (20 px line), 56 at 2×; the band centres its text (`text_band`) so nothing else moves. Test `single_line_band_is_40_at_one_x_and_grows_with_the_scaled_line` |
| R2-F2-04 | `Chip` (retained) width | Both axes pinned `length×length`, so taffy never called `measure` (confirmed in taffy 0.10 `compute_leaf_layout`): every retained chip was laid out at a 0.6 em-per-character estimate; `measure` also reached for `Theme::dark()` | `chip.rs` `build_layout`, `measure` | `ui/gallery_shots/{before,after}/gallery-*.png` chip row; test | **Fixed** — `adjust_layout_with_measure` pins the measured pill (label size captured in `build_layout`); `measure` shares the cache. Test `chip_width_is_measured_not_estimated` |
| R2-F2-05 | `Chip` pill height at 2× | 28 px pill fixed while the label line grows past it | `chip.rs` | test | **Fixed** — pill = `max(28, measured line + 2×s1)`; 28 at 1×, 38 at 2× (measured at the renderer-relative size, see X1 note). Test `pill_is_28_at_one_x_and_follows_its_measured_label_at_two_x` |
| R2-F2-06 | `Chip` pointer target | Hit-tested on the 28 px pill; no control may be under 32 px, 48 for a coarse pointer | `chip.rs` `handle_event`, `cursor_shape`, × hover | test: a click 1 px above the pill did nothing | **Fixed** — `hit_rect()`/`remove_hit_rect()` grow vertically (never sideways: the × would reach under the label) to `max(metrics.min_pointer_target, interaction.minimum_target())`; `set_interaction_environment` added. Test `hit_rect_meets_the_minimum_pointer_target_while_the_pill_stays_28` |
| R2-F2-07 | Fixed-target leaves in an overflowing row | `badge`, `spinner`, `input_hint`, `shortcut_recorder`, `date_picker`, `time_picker`, `color_picker`, `number_input` were `length×length` leaves on taffy's default `flex_shrink: 1` — the shape that squeezed `Toggle` to zero width in round one (F2-14); F1 did **not** land a leaf-level default (`TaffyTreeExt::leaf_node` is a passthrough) | eight `build_layout`s | round-one F2-14 evidence (`x0 == x1`) | **Fixed** in my files — `flex_shrink: 0.0` on each. The class-level fix (a fixed-leaf `Style` constructor in `widget/mod.rs`) stays with F1 |
| R2-F2-08 | `SearchField` in a `Row` | `percent(100%)` with `flex_shrink: 0` claimed the whole row and pushed every later sibling past the viewport (`text_input … x0: 1941 … x1: 1941` in a 1200 px row) — the F2-16 shape recorded against `Dropdown`, found in my own file by the new dense-row test | `search_field.rs` `build_layout` | `every_control_kind_in_one_row…` failure message | **Fixed** — `flex_shrink: 1.0` with a floor of the two gutters plus two pointer targets of text, so it gives but never vanishes |
| R2-F2-09 | Outlined danger face | No shared style for a quiet destructive action; remote hand-paints "Delete" (A5's RM-8) | `button.rs` | remote `draw_form_footer` | **Fixed** — `ButtonStyle::DangerOutline` (+ `Button::danger_outline()`, `ButtonFace::danger_outline()`): transparent at rest, danger ink as both outline and label, `bg_hover`/`bg_active` under the pointer so the ink is measurable on every ground (a translucent danger tint has no measurable ground). In the readability sweep for all three themes and four states. Tests `face_colors_danger_outline_is_transparent_at_rest_and_outlined_in_its_ink`, `every_button_style_label_clears_its_floor_in_every_theme` (5 styles). **Open — needs A5**: remote adopts it |
| R2-F2-10 | Icon-only `ButtonFace` under `--pseudo-locale` | `i18n!("")` returns `⟦⟧`, painting a bracket pair beside the glyph (A10's VE-O5) — and in `accessibility()` the non-empty `⟦⟧` hid the `accessible_name`/tooltip fallback, so the button was *spoken* as brackets | `button.rs` paint, measure, draw, accessibility | tests | **Fixed** — `localized_label()` keeps an empty key empty at all four sites. Tests `icon_only_face_paints_no_label_under_the_pseudo_locale`, `icon_only_button_speaks_its_tooltip_under_the_pseudo_locale` |
| R2-F2-11 | Button geometry across states | Hover / press / focus / disabled must recolour, never move | `button.rs` | test over 5 styles × 5 states: label origin identical | **Verified** — `interaction_states_do_not_move_the_face_or_its_label` (the focus ring is stroked on the outline, not inset) |
| R2-F2-12 | `NumberInput` preferred width | A9's ebook stepper measures the field's widest value itself and assumes the field's type size (E2) | `number_input.rs` | — | **Fixed** — `pub fn preferred_width(&self, theme, text) -> f64`: widest of current/min/max formatted with precision + suffix, beside the steppers, the same rule the widget applies to itself (`required_width` shared). Test `preferred_width_fits_the_widest_value_in_range`. **Open — needs A9**: the ebook slot adopts it (no caller yet, by design of the split) |
| R2-F2-13 | `BlockEditor` drag handle | Fixed 16×20 px rect 22 px left of the text with 2.5 px dots pitched 6×5 — none scaled (A6's N3/C18); numbered-prefix gap a bare `6.0` | `block_editor.rs` `block_drag_handle_rect`, `draw_block_drag_affordance`, `numbered_prefix_origin` | `notes/{before,after}/large.png` gutter (x 8–26) | **Fixed** — `HandleMetrics` from `theme.tokens` (`s4` width, `icon_md` height, `icon_label_gap`, `min_pointer_target`); height and dot cluster follow the text scale (column pitch capped to stay inside the fixed-width gutter column); hit rect grows to the pointer target where the block is that tall and never past the block's own line (adjacent code lines butt together); the ordinal's gap is `theme.tokens.metrics.icon_label_gap × text_scale` |
| R2-F2-14 | `BlockKind::font_size` | 16 / 28 / 22 / 18 / 14 — the headings off every step of the type scale | `block_editor.rs` | `notes/{before,after}/dark/dev.qos.notes-desktop.png` ("This week", "Notes") | **Fixed (typography decision)** — body `lg` 16, H1 `x3l` 32, H2 `x2l` 24, H3+ `xl` 20, code `base` 14. Every Notes page's headings change (28→32, 22→24, 18→20); no app consumes the numbers (mail maps headings to its own `TextRole`). `heading_paragraph_inline_code_and_numbered_prefix_scale_once` now asserts the `x3l` step once and doubled, instead of the old literal |
| R2-F2-15 | Mixed control kinds in one `Row` | Default + outlined buttons, chip, number stepper, segmented group, search field and a fluid field: one centre line at 1× and 2×, no overlap, ≥ 32 px for everything but the 28 px chip (its target is the grown `hit_rect`) | `f2_control_row_alignment.rs` | — | **Verified** — `every_control_kind_in_one_row_shares_the_centre_line_at_one_and_two_x` (dark, light, dark @ 2×); it is the test that found R2-F2-08 |
| R2-F2-16 | `Avatar::draw_at` initials | `0.4 × disc` handed to the size-based renderer APIs, scaled again at 2× (A6's K8) | `avatar.rs:146` (and `paint_initials`, `draw_overflow_at`) | — | **Open — X1** (size expression). Disc, ring and hit geometry are rect-derived and correct; nothing else to change here |
| R2-F2-17 | `Chip::draw` / `draw_at` label size | `typography.scale.sm` (pre-scaled) fed to `measure`/`draw_text` (A6's K7); `label_size_for_height` likewise | `chip.rs` `draw`, `draw_at`, `label_size_for_height` | — | **Open — X1**. The layout path this round added (`intrinsic_size`) measures at the renderer-relative size via F3's `renderer_type_size`, so the pill is right (38 at 2×) while the painted glyph is still doubled until X1 lands the same conversion in `draw` |
| R2-F2-18 | `SearchField` height | `HEIGHT = 38`, the one control off the 40 px bar height in the mixed row (centred by the Row, so no visible drift) | `search_field.rs:33` | — | **Open — recorded** (a default-metric change) |
| R2-F2-19 | Settings `--shots` host | Lays out with raw `taffy.compute_layout_with_measure` (`apps/settings/src/shots.rs:497`), never the shared solver, so it cannot show R2-F2-02: `settings/after/large.png` is byte-identical to BEFORE with the overflowing labels. Store's host is `FrameHarness` and does show R2-F2-03 | — | `settings/{before,after}/large.png` | **Open — A3 / coordinator**: route the settings host through `compute_widget_layout` (round one flagged the same host's unmatched renderer) |

Controls checked and found correct (no change): `Toggle`, `Checkbox`,
`ToggleGroup`, `Segmented`, `Slider` (fluid by design), `Breadcrumb`,
`DatePicker`/`TimePicker` bands, `InputHint`, `Link`, `Icon`, `Divider`,
`UndoButton`, `EditableLabel`, `RichTextLine`/`RichTextEditor` caret and
selection geometry, `CodeEditor` gutter.

## Merge survival (round one's F2 changes at `381eff6a8`)

| Round-one change | Present? | Proof |
|---|---|---|
| `text::centered_text_top` + its two tests | yes | `ui/src/widget/text.rs:37`; `text::` tests in the lib run |
| Breadcrumb / TriState / DatePicker / SearchField measured-line centring | yes | `breadcrumb::`, `segmented::`, `date_picker::`, `search_field::` tests pass (in the 449) |
| Button label from the theme (`renderer_label_size`) | yes | `retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts` passes |
| Toggle `flex_shrink: 0` + 44 px pill | yes | `fixed_controls_keep_their_size_beside_a_fluid_field` passes |
| Button/Checkbox/ToggleGroup/Toolbar named constants | yes | in tree |
| `ui/tests/it.rs` registration of `f2_control_row_alignment` | yes | lines 52–53 |
| F1's `Tokens::metrics` | yes | `ui/src/tokens/metrics.rs`; consumed now by Toolbar, Chip, BlockEditor, SearchField |
| F1's fixed-leaf shrink default (F2-17 hand-off) | **no** | `TaffyTreeExt::leaf_node` is a passthrough → R2-F2-07 |

## Code review dispositions (`/code-review high`, 10 findings)

1. *`Chip::intrinsic_size` measured at the pre-scaled step → 68 px pill at 2×
   while the doc promised 38.* **Fixed** — measures through F3's
   `renderer_type_size`; the test now pins 38. `draw`'s expression is X1's
   (R2-F2-17).
2. *Chip target read only `metrics.min_pointer_target`, never the coarse-pointer
   48.* **Fixed** — `interaction` field + `set_interaction_environment`;
   target = max of both.
3. *× hover highlight tested the unexpanded rect while press/cursor used the
   grown one.* **Fixed** — one rect.
4. *Handle `half_height` never capped at the block, so butting code lines'
   handles overlapped and `.find` picked the wrong line.* **Fixed** —
   `.min(line.height() / 2)`; comment now true.
5. *Chip footprint cache not invalidated on `pad_y`.* **Fixed** — bits
   comparison like Button's `padding_standard`.
6. *Pin-leaf dance duplicated in Button ×2, Chip, NumberInput.* **Recorded for
   F1** — the shared helper belongs in `widget/layout.rs`/`mod.rs`, not this
   unit's files.
7. *`flex_shrink: 0.0` hand-pasted into eight leaves instead of a fixed-leaf
   constructor.* **Recorded for F1** (same file); the per-widget pins fix real
   collapses now.
8. *`numbered_prefix_origin` used `Metrics::BASE`, not the theme.* **Fixed** —
   the caller passes `ctx.theme.tokens.metrics.icon_label_gap`.
   `BlockKind::font_size` keeps `TypeScale::default()` deliberately: it is the
   unscaled step, and every consumer multiplies by `text_scale` (making it
   theme-resolved would double-scale — X1's territory).
9. *`preferred_width` has no caller.* **Accepted as is** — the brief asks for
   the framework API for A9's stepper; the unit test is its contract
   (R2-F2-12).
10. *`Chip::measure` unreachable yet re-measuring.* **Fixed** — shares the
    footprint cache; kept as the `auto`-width fallback.

## Tests

All commands ran as
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo … --locked --no-default-features`.

| Command | Result |
|---|---|
| `cargo test -p ui --lib -- button:: toggle chip:: avatar:: toolbar:: text_input:: block_editor:: number_input:: breadcrumb:: badge:: spinner:: input_hint:: shortcut_recorder:: date_picker:: time_picker:: color_picker:: search_field::` | **426 passed, 0 failed, 2 ignored** (both the pre-existing `#[ignore]` visual audits; none added) |
| `cargo test -p ui --test it -- f2_ filled_control_labels_are_readable` | **11 passed, 0 failed** (6 row-alignment, 5 readability) |
| `cargo test -p ui --test it -- f1_spacing_token_audit design_token_audit` | 3 pre-existing failures at the merge tip, reported verbatim, untouched: `theme_escape_hatches_do_not_grow` (baseline above tree for `autocomplete.rs`, `channel_browser.rs`, `context_menu.rs`, `popover.rs`, `video_player.rs`), `app_and_shell_theme_escape_hatches_do_not_grow` (`apps/browser/src/browser_tabs.rs`, `shell/src/shell/frame.rs`), `spacing_literals_do_not_grow` (baseline above tree for `apps/word-processor/src/zoom_control.rs`, `shell/src/shell/editor_app.rs`). None names a file this unit touched; the ratchets for my files hold |
| `cargo check -p ui --tests` (gate) | **clean** — no errors, no warnings (the `it` target included) |

New tests (18): `toolbar::minimum_hit_rect_reads_the_pointer_target_not_the_spacing_scale`;
`button::{default_footprint_is_exactly_the_shipped_120_by_40_at_one_x,
default_footprint_grows_from_the_measured_line_at_two_x,
a_wide_label_widens_the_default_button_instead_of_overflowing_it,
an_explicit_size_is_never_grown_by_the_measured_footprint,
icon_only_face_paints_no_label_under_the_pseudo_locale,
icon_only_button_speaks_its_tooltip_under_the_pseudo_locale,
face_colors_danger_outline_is_transparent_at_rest_and_outlined_in_its_ink,
interaction_states_do_not_move_the_face_or_its_label}`;
`chip::{chip_width_is_measured_not_estimated,
pill_is_28_at_one_x_and_follows_its_measured_label_at_two_x,
hit_rect_meets_the_minimum_pointer_target_while_the_pill_stays_28}`;
`text_input::single_line_band_is_40_at_one_x_and_grows_with_the_scaled_line`;
`number_input::preferred_width_fits_the_widest_value_in_range`;
`f2_control_row_alignment::{every_control_kind_in_one_row_shares_the_centre_line_at_one_and_two_x,
default_buttons_hold_their_label_at_two_x_and_stay_120_by_40_at_one_x}`.
One expectation updated, not weakened:
`block_editor::heading_paragraph_inline_code_and_numbered_prefix_scale_once`
asserts the H1 step through `BlockKind::font_size()` (= `x3l`) instead of the
retired literal 28. No `#[ignore]` added.

## Captures

`target/ui-review/<app>/{before,after}/{dark,light}/dev.qos.<app>-{desktop,laptop,tablet,phone}.png`
and `…/{before,after}/large.png` (800×900, dark, 2× text) for settings, store,
notes, remote, ebook; `target/ui-review/ui/<scenario>/{before,after}/*.png` for
`widget_showcase_snapshot`, `gallery_shots`, `textinput_ext_shots`,
`rich_text_editor_shots`, `undo_controls_shots`. Scripts: `target/ui-review/{capture,capture-fixtures,compare}.sh`.

63 pairs: **49 byte-identical, 14 changed**, each opened:

| Changed | What moved | Why |
|---|---|---|
| `notes/{dark,light}/*` (8) and `notes/large.png` | H2 blocks 22 → 24 px (44 → 48 at 2×); drag grips scale with the text at 2× and sit inside the 32 px gutter | R2-F2-14, R2-F2-13 |
| `store/large.png` | search field band 40 → 56 px with 8 px of slack a side; the list below shifts 16 px | R2-F2-03 (Store's host is `FrameHarness`; its buttons are `fit_content`/`content_sized` so R2-F2-02 does not show here) |
| `ui/gallery_shots/gallery-{dark,light}.png` | chip pills at their measured width; indeterminate progress bar at another animation phase | R2-F2-04; animation |
| `ui/widget_showcase_snapshot/showcase.png` | indeterminate progress bar phase only | animation, not geometry |
| `ui/textinput_ext_shots/writing-assistance.png` | the context menu gained a "Paste" row | host clipboard state at capture time, not this diff |

Identical, as required: every 1× settings/store/remote/ebook matrix PNG (dark
and light, four viewports), `settings/large.png` (see R2-F2-19), `remote/large.png`,
`ebook/large.png`, `showcase-scrolled.png`, all `rte-*`, `undo-*`, `multiline`,
`secret`, `structured-fields`.

## Open — needs another unit

| Where | What | Unit |
|---|---|---|
| `chip.rs` `draw`/`draw_at`/`label_size_for_height`; `avatar.rs` `draw_at`/`paint_initials`/`draw_overflow_at` | size expressions scaled twice; the chip's layout path now converts once via `renderer_type_size` — `draw` should too | X1 |
| `widget/mod.rs`, `widget/layout.rs` | fixed-leaf `Style` constructor (`flex_shrink: 0` by construction) and a shared `pin_leaf_size(taffy, node, Size)` for Button/Chip/NumberInput | F1 |
| `apps/remote` `draw_form_footer` | hand-painted outlined Delete → `ButtonFace::new(rect, "Delete").danger_outline()` | A5 |
| `apps/ebook` size stepper slot | `NumberInput::preferred_width(theme, text)` | A9 |
| `apps/settings/src/shots.rs:497` | raw taffy layout; route through the shared solver so its 2× capture can observe the framework | A3 / coordinator |
| `search_field.rs:33` | `HEIGHT = 38` vs the 40 px bar height | F2, next round (default-metric change) |

## Limits

- macOS: no `shots` binary, no compositor; app evidence is the shared
  `ui::launch` `--shots`/`--responsive-matrix` host plus `ui-shots` fixtures.
- Remote's shipped start state has no connection, so its edit-mode Delete is
  not in any capture; the ebook default view does not open the Fonts stepper.
  Both APIs are proven by unit and `it` tests rather than pixels.
- The settings host cannot observe R2-F2-02 (R2-F2-19); the production path
  is proven on `FrameHarness` in `f2_control_row_alignment`.
- The chip's painted glyph is still double-scaled at 2× until X1 lands; the
  pill's geometry is already the post-fix one.
