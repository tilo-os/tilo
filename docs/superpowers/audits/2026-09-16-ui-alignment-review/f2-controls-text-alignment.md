# F2 — Core controls and text inputs: label/icon alignment

Unit F2 of the 2026-09-16 UI alignment review (18 parallel units). Base:
`codex/macos-shell` @ `2d7e819b6` plus the coordinator's `ui-review/lockfix`
(`f9a579c9c`). Branch: `ui-review/f2-controls-text-alignment`.

## Scope

`ui/src/widget/{button, checkbox, toggle, toggle_group, segmented, slider,
chip, badge, spinner, progress_bar, gauge, number_input, search_field,
shortcut_recorder, date_picker, time_picker, color_picker, editable_label,
name_field, undo_button, text, input_hint, breadcrumb, link, icon, avatar,
divider, toolbar, text_input, block_editor, rich_text_editor, rich_text_line,
code_editor, marks}.rs` — the controls every app composes, and the shared
text-editing surfaces.

Out of scope, recorded below as *Open — needs <unit>*: `widget/mod.rs` and
the Row/leaf flex policy (F1), data display (F3), popups/menus and Dropdown
(F4), app shot hosts and per-app button sizing (A-units).

One file outside the owned list was touched: `ui/tests/it.rs` gained the
two-line `#[path]`/`mod` registration for the new `f2_control_row_alignment`
test — `ui` has `autotests = false`, so a test file that is not registered
there never compiles.

## Method

1. Read `ui/HIG.md` §7/§7a/§9.6 and
   `docs/superpowers/audits/2026-09-15-framework-layout-progress.md`
   (Changes 4–6: measured Button, text scale, Checkbox double-scaling fix) so
   accepted fixes were not regressed.
2. Source pass over every owned file: every `draw`/`measure`/`build_layout`
   read for (a) vertical centring on the font size instead of the measured
   line, (b) metrics written twice, (c) hand-tuned pixel offsets, (d)
   host-dependent text sizing, (e) flex behaviour that lets a control vanish.
3. BEFORE captures: Store, Settings and Notes (`--responsive-matrix` dark +
   light, 2× large-text, 480 px narrow, Notes `heading`/`slash`; 32 PNGs)
   and ten `ui-shots` widget fixtures (32 PNGs). Every PNG opened; control
   regions cropped at 2–3× (`target/ui-review/crops/before/`).
4. Fixes with tokens, measured text and one shared helper; **no default
   metric changed** — every shipped value is preserved and proven by
   byte-identical AFTER captures at 1×.
5. Regression per fix: widget-module unit tests plus a new `FrameHarness`
   integration test over a mixed-control row.
6. AFTER captures, compared byte-for-byte against BEFORE; every changed
   file cropped and inspected.
7. `/code-review high` over the diff; each finding dispositioned below.

### Verification of the coordinator's research

| Claim | Verified? | Note |
|---|---|---|
| `breadcrumb.rs:297` centres on font size | **Yes** | fixed (F2-01) |
| `toggle_group.rs:549` and `:716` centre on font size | **No** | both use `th` from `measure_weighted`/`measure_style` — already the correct pattern; left as is |
| `button.rs` icon formula ×3, 6 px gap ×2 | Yes | consolidated (F2-06); values kept |
| Two line-height defaults (1.25 legacy / 1.4 token) | Yes | not "fixed" by offsets; every touched site measures and draws one style |
| `text_input`/`block_editor`/`rich_text_*` reworked 2026-09-16 | Yes | reviewed (caret/selection/gutter/list-prefix geometry); no clear alignment defect; all suites green |

Sites the sweep found that the research did not list: `segmented.rs:257`,
`date_picker.rs:545/565/576/597/684`, `search_field.rs` key cap (`cy - 6.5`),
the Button label as a bare `14.0`, and the Toggle collapsing to zero width in
a row that overflows.

## Findings

Coordinates are logical pixels in the named capture. "1×" = default theme.

| ID | Surface | Defect | Viewport / theme | Where | Evidence | Status |
|---|---|---|---|---|---|---|
| F2-01 | Breadcrumb segments + chevrons | Centred on the 12 px font size, not the 15 px measured line → every label 1.5 px below the row centre | gallery-dark 1200×2300 @ (600–760, 462); widget_snapshot @ (24–235, 310) | `breadcrumb.rs:297` (+ separator) | `crops/{before,after}/gallery-stepper.png` | **Fixed**; one measured line height shared by labels and chevrons; test `breadcrumb_labels_and_chevrons_centre_their_measured_line` |
| F2-02 | `TriStateControl` (segmented) labels | Same drift inside the 28 px strip (measured `th` discarded) | any | `segmented.rs:257` | unit test only (not in a shipped fixture) | **Fixed**; test `tri_state_labels_centre_their_measured_line` |
| F2-03 | DatePicker header, ‹ › chevrons, weekday row, day numbers | Same drift in every band (32 / 20 / ~31 px) | widget_snapshot @ (24–248, 388–630) | `date_picker.rs:545, 565, 576, 597, 684` | `crops/{before,after}/snapshot-datepicker.png` — all text rises 1.5 px to the band centres | **Fixed**; test `picker_text_centres_its_measured_line_in_every_band` |
| F2-04 | SearchField palette key cap | Hand-tuned `cy - 6.5` for an 11 px label whose line measures 13.75 px (correct top `cy - 6.875`); five literals (20/14/10/5/7) | palette mode | `search_field.rs` `draw_kbd_chip` | `ui/search_showcase/` (fixture has no palette field → identical) | **Fixed** (measured centring, named constants, same values); test `palette_keycap_label_centres_its_measured_line` |
| F2-05 | Retained Button label size | `measure`/`draw` used a bare `14.0`. A host that scales the theme but leaves the shared renderer at 1× — the apps' `--shots` loop, `apps/settings/src/shots.rs:472` — kept labels at 14 px while role-based `Text` grew 2× | settings 800×900 @ 2× text | `button.rs:912, 1054` | `settings/{before,after}/large-text.png` @ (632–752, 470–512) | **Fixed**: size resolved from `theme.tokens.typography.scale.base` through `TextRenderer::renderer_input_style` (the Change-4/6 pattern Checkbox already uses); `draw` reads the theme it is handed, `measure` the copy captured in `build_layout`; cache invalidated when the theme's label size or padding changes. 14 px at 1×, 28 px at 2× in matched *and* unmatched hosts, with or without a `build_layout` pass. Tests `retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts`, `theme_text_scale_change_invalidates_the_cached_face` |
| F2-06 | Button geometry literals | Icon edge `(h − 14).clamp(12, 22)` ×3 (`:677, :705, :884`), icon gap `6.0` ×2 (`:723, :890`), `radius 8.0`, `(120, 40)`, `30.0`, `64.0`, stroke `2.0`/`1.0` | — | `button.rs` | AFTER 1× captures byte-identical | **Fixed** as named constants + `icon_edge()`; `ButtonFace` radius defaults to `theme.tokens.radii.md` (8 px) — identical at 1×, follows `ui_scale` like the retained widget already did. `ButtonFace`'s *label* default stays renderer-relative 14 (see review disposition 2); test `default_face_matches_the_legacy_explicit_metrics_at_one_x` |
| F2-07 | Toggle retained draw | Re-implemented the 44×24 pill / r = 10 knob / 2 px gap next to `Toggle::paint` | — | `toggle.rs:464–520` | byte-identical | **Fixed**: retained draw delegates to `Toggle::paint` via `track_rect()`; `TRACK_WIDTH/HEIGHT`, `KNOB_EDGE_GAP` named; geometry pinned by literals in `retained_draw_paints_the_documented_pill_geometry` (on/hover/focus/disabled) |
| F2-08 | Checkbox | `gap = 10.0` in `measure` and `draw`, `24×24` box literal | — | `checkbox.rs:437, 489, 501` | byte-identical | **Fixed**: `LABEL_GAP`, `BOX_EDGE`, documented against `s2/s3/s6` |
| F2-09 | ToggleGroup | `120.0`/`36.0` intrinsic-size literals; labels already measured-centred | — | `toggle_group.rs:582–586` | byte-identical | **Fixed** (`SEGMENT_WIDTH`, `STRIP_HEIGHT`) |
| F2-10 | Toolbar separator | `1.0 + 2.0 * s2` written twice; dead bootstrap `separator: 9.0` disagreed with the measured 17 | — | `toolbar.rs:78, 139, 520` | byte-identical | **Fixed**: `separator_extent(theme)`, bootstrap from default tokens; painting covered by the existing `toolbar_separator_paints_a_divider_line` |
| F2-11 | ShortcutRecorder | Hint origin `chord.center().y - 11.0` — half the hint's 22 px `HEIGHT` as a literal | — | `shortcut_recorder.rs:188` | byte-identical | **Fixed**: `input_hint::HEIGHT / 2.0` (`HEIGHT` made `pub(crate)`) |
| F2-12 | Chip | Constants `HEIGHT 28 / PAD_X 12 / GAP 6 / DOT_D 8 / X_D 14` undocumented | — | `chip.rs:41–45` | — | Documented against the density/spacing scale; no value changed |
| F2-13 | Mixed row centre line | Button (40) + Toggle (32) + Checkbox (32) + Dropdown (40) + TextInput (40) in one `Row` | dark / light / high-contrast, 2× text, after resize | — | — | **Verified** by `ui/tests/f2_control_row_alignment.rs`: centre-y within 0.5 px, every control ≥ 32×32, no overlap |
| F2-14 | Toggle in an overflowing Row | Beside a percent-width field the switch was squeezed to **zero width** (`x0 == x1`): default `flex_shrink: 1` and no measured minimum, while Button/ToggleGroup pin `flex_shrink: 0` | any row that overflows | `toggle.rs` `build_layout` | found by the harness test: `dark: toggle has no geometry: Rect { x0: 132.0, y0: 84.0, x1: 132.0, y1: 116.0 }` | **Fixed**: `flex_shrink: 0.0`; test `fixed_controls_keep_their_size_beside_a_fluid_field` (unboxed fluid `TextInput`, 1× and 2×) |
| F2-15 | Default-size Button at 2× text | With the label now truthfully 28 px, a `Button::new` (120×40) holds a ~150 px label → overflow. The live App host (matched renderer) has always drawn this; the raw capture host hid it at 14 px | settings 800×900 @ 2× | `apps/settings/src/panes/accounts.rs:80` (`Button::new` without `fit_content`/`content_sized`) | `crops/after/settings-large-row.png` | **Open — needs F2 decision + A-units**: either default-size buttons floor their width at the measured label (a default-metric change under scaled themes only) or app rows use `fit_content(40)`/`content_sized()`. Not changed here per "no default metric change" |
| F2-16 | Dropdown in a Row | `percent(100%)` width with `flex_shrink: 0` claims the whole row; every later item is pushed past the viewport to zero width (`text_input … x0: 1244 … x1: 1244` in a 1000 px row) | any | `dropdown.rs:658–661` | harness run before the fixture gave fluid controls a 220 px `Container` slot | **Open — needs F4** |
| F2-17 | Leaf/Row shrink policy | Fixed-target leaves default to taffy's `flex_shrink: 1`; a labelled Checkbox (`percent(100%)`, `flex_shrink: 1.0`, `checkbox.rs:428`) shrinks to its 42 px min-content beside a non-shrinking item | any overflowing row | `widget/mod.rs` `TaffyTreeExt::leaf_node`, `row.rs` | analysis | **Open — needs F1**: a fixed-target leaf should default to `flex_shrink: 0`, or Row should give fluid children the slack |

Controls checked and found correct (no change): Chip label (measured `lh`),
Checkbox label (measured, Body role), Badge/Avatar (measured `th`), Link (rect
= measured text), NumberInput step glyphs and value (measured), TimePicker
(measured), InputHint key caps and separators (measured), Slider, Spinner,
ProgressBar, Gauge (no text bands), EditableLabel (computed `size × 1.25`,
matching TextInput's fallback band), TextInput single-line band (shaped
`line_height`), TextInput code gutter numbers (same style and line origins as
the text), BlockEditor bullet/checklist prefixes (line-rect midpoint) and
numbered prefixes (RichTextLine's resolved 1.25 line), RichTextLine
caret/selection (delegated to the inner TextInput — one geometry).

## Helper API added

`ui/src/widget/text.rs`:

```rust
pub fn centered_text_top(band_y0: f64, band_height: f64, measured_line_height: f64) -> f64
```

Top edge of one text line centred in a band; pass the `.1` of
`TextRenderer::measure*` (the measured line), never the font size. Not
clamped: a line taller than its band overhangs symmetrically. Documented with
the drift formula `(line_height − 1) × size / 2` (1.5 px at 12 px, 1.75 px at
14 px). Used by breadcrumb, segmented, date_picker and search_field; reachable
inside the `widget` tree as `crate::widget::text::centered_text_top`.

Test-only companion `assert_glyph_run_centred_in_band(scene, run_index, text,
size, band_y0, band_height, renderer, color)` (`#[cfg(test)] pub(crate)`)
asserts a painted glyph run against a reference `draw_text` at the measured
centre; the four site tests use it.

**Open — needs F1:** `widget/mod.rs` declares `mod text;` privately and
re-exports only `Text`. Add `pub use text::centered_text_top;` so F3/F4/shell
sites can adopt it (the inline `rect.y0 + (rect.height() − measured_h) / 2.0`
works meanwhile). The reviewer's stronger suggestion — a
`TextRenderer::draw_text_centered(band, …)` that measures and centres
internally so no caller *can* centre on the font size — belongs to the
`text_render.rs` owner.

Also added inside `button.rs`: `renderer_label_size(theme_size, renderer)` —
the one conversion from a theme type step to the renderer-relative size the
legacy `measure`/`draw_text` entry points consume.

## Code review dispositions (`/code-review high`)

1. *`Button::draw` used the cached `label_size`; a host that lays out
   externally never runs `build_layout`, so the label halved under a scaled
   theme.* **Fixed** — `draw` reads `theme.tokens.typography.scale.base`
   directly; the cache serves only `measure`. Covered by the external-layout
   case in `retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts`.
2. *`ButtonFace` default label size made theme-derived while twelve
   immediate-mode callers size their rects with renderer-relative
   `measure(label, 14.0)`.* **Accepted and reverted** — the face default is
   renderer-relative 14 again (documented why); only its radius reads the
   theme. Identical at 1× and in every matched host.
3. *Thirteen identical font-size-centring sites remain outside F2; helper not
   reachable outside `ui`.* **Open**, listed under *needs F3/F4/F1*.
4. *Toggle parity test compared `draw` to the `paint` it now calls.*
   **Fixed** — replaced by literal expected geometry
   (`retained_draw_paints_the_documented_pill_geometry`).
5. *Toggle-only `flex_shrink` patch; fixture boxes fluid controls.*
   **Partly accepted** — added the unboxed toolbar fixture
   (`fixed_controls_keep_their_size_beside_a_fluid_field`), which is the row
   that exposed the collapse; the Dropdown and leaf-default policy are
   recorded as F2-16/F2-17 for F4/F1.
6. *Per-widget cache invalidation on theme change.* **Kept**, extended to
   `content_padding`; the relayout-only `FrameHarness::set_theme` path needs it.
7. *Breadcrumb re-measured every label per frame; dead `sep_w`.* **Fixed** —
   one measured line height for the whole trail; dead measure removed.
8. *f64→f32 shims and four duplicated test blocks; toolbar bootstrap test
   asserted a value nothing reads.* **Fixed** — Toggle constants are `f32`
   widened losslessly; the four tests share `assert_glyph_run_centred_in_band`;
   the toolbar test was dropped (the refactor is covered by the existing
   divider test). `px_f32` in `button.rs` stays: it keeps `icon_edge()` a single
   formula for both the f64 painters and the f32 `measure`.
9. *`DEFAULT_SIZE` doc called 120 px a minimum.* **Fixed** (reworded; points
   at `FIT_MIN_WIDTH`).

## Open — needs another unit

| Where | Defect | Unit |
|---|---|---|
| `data_table.rs:1218`, `tree_view.rs:2221, 2306` | `(h − f64::from(text_size)) / 2.0` — font-size centring drift | F3 |
| `autocomplete.rs:501, 689`, `command_palette.rs:1226`, `modal_dialog.rs:398`, `link_popover.rs:421, 456`, `permission_prompt.rs:317`, `slash_menu.rs:548, 560`, `tooltip.rs:150` | same drift; adopt `centered_text_top` | F4 |
| `dropdown.rs:658–661` | `percent(100%)` + `flex_shrink: 0` pushes every later row item to zero width (F2-16) | F4 |
| `widget/mod.rs` | re-export `text::centered_text_top`; fixed-target leaf shrink default (F2-17) | F1 |
| `apps/settings/src/shots.rs:472` (and Notes' equivalent) | raw `--shots` host builds `TextRenderer::new()` without `set_text_scale(theme.text_scale)`; legacy-size widgets (sidebar rows, TextInput placeholder, search field) stay 14 px in the 2× capture while `Text` scales. Store already uses `FrameHarness` (its 2× capture is byte-identical before/after) | A-units / coordinator |
| `apps/settings/src/panes/accounts.rs:80` and every `Button::new` row action | default 120×40 button overflows at 2× text (F2-15) | A-units, F2 decision |
| `block_editor.rs:1334–1337` | drag handle is a fixed 16×20 px rect 22 px left of the text, not scaled with `text_scale`, under the 32 px target | F2 (open; behaviour change) |
| `block_editor.rs:3410` | numbered-prefix gap `- 6.0` not scaled with `text_scale` | F2 (open; sub-pixel at 2×) |
| `block_editor.rs:67–76` | `BlockKind::font_size` 16/28/22/18/14 off the theme type scale (14/16/20/24/32) and not read from `Theme` | F2 (open; needs a typography decision — changes every Notes page) |
| `button.rs` `FIT_SIDE_PADDING = 15` vs `content_sized` `padding_standard = 12` | the two auto-width modes inset the label differently | F2 (open; metric change) |
| Legacy `TextStyle::new` 1.25 vs token 1.4 line height | Button/Chip/Breadcrumb lines are 17.5 px, Checkbox (Body role) 19.6 px; rows still align because both centre | F1 typography |

## Tests

All `ui` test/check commands ran as
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 cargo … --locked --no-default-features`
(coordinator option (a) for the `ring` const-assert; additionally the
committed `.cargo/config.toml` Linux-only linker flags were neutralised
per-invocation with `RUSTFLAGS=""` on this macOS host — the config file is
untouched).

| Command | Result |
|---|---|
| `cargo test -p ui --lib -- button:: toggle:: segmented:: chip:: checkbox:: slider:: breadcrumb:: text:: block_editor:: rich_text_line:: rich_text_editor:: toolbar:: date_picker:: toggle_group:: search_field:: input_hint:: shortcut_recorder:: code_editor::` | **362 passed, 0 failed, 2 ignored** (both pre-existing `#[ignore]` visual audits; none added) |
| `cargo test -p ui --lib -- text_input::` (the `text::` filter does not match this path) | **105 passed, 0 failed** |
| `cargo test -p ui --test it -- f2_control_row_alignment` | **4 passed** (three themes, 2× text, after resize, unboxed fluid field) |
| `cargo test -p ui --test it -- filled_control_labels_are_readable` | **5 passed** |
| `cargo test -p ui --test it -- typography_visual_regression` | **1 failed — pre-existing baseline failure, reported verbatim, not touched:** `multilingual_gallery_matches_scale_matrix … panicked at ui/tests/typography_visual_regression.rs:307:5: dark-display-1-text-1 changed dimensions: actual 1160x1073, baseline 1160x824` (same for `light-display-1-text-1`, `dark/light-display-1_25-text-1` 1450x1341 vs 1450x1030, `dark/light-display-1_5-text-1_25` 1740x1788 vs 1740x1415). The gallery has grown taller than every stored baseline — a dimension mismatch unrelated to control alignment; no text in this unit's widgets appears in that gallery |
| `cargo check -p ui --tests` (gate) | **clean** — `Finished`, no errors, no warnings |

New tests added (12): `text::centered_text_top_splits_the_slack_evenly`,
`text::centered_text_top_uses_the_measured_line_not_the_font_size`,
`breadcrumb::breadcrumb_labels_and_chevrons_centre_their_measured_line`,
`segmented::tri_state_labels_centre_their_measured_line`,
`date_picker::picker_text_centres_its_measured_line_in_every_band`,
`search_field::palette_keycap_label_centres_its_measured_line`,
`button::default_face_matches_the_legacy_explicit_metrics_at_one_x`,
`button::retained_label_follows_the_theme_scale_in_matched_and_unmatched_hosts`,
`button::theme_text_scale_change_invalidates_the_cached_face`,
`toggle::retained_draw_paints_the_documented_pill_geometry`, and the four in
`ui/tests/f2_control_row_alignment.rs`.

The one warning `cargo test` prints for the `ui` lib-test target is the
macOS debug-linker message (`linker stderr: ld: __eh_frame section too large
(max 16MB)…`, `#[warn(linker_messages)]`), as the coordinator anticipated; it
is not from this diff.

## Captures

`target/ui-review/<app>/{before,after}/…` for store, settings, notes;
`target/ui-review/ui/<scenario>/{before,after}/` for the ten widget fixtures;
2–3× crops in `target/ui-review/crops/{before,after}/`.

64 BEFORE PNGs (32 app, 32 fixture) against the AFTER set taken from the
final code (`target/f2after.sh`): **53 byte-identical, 11 changed**, every
change inspected:

| Changed capture | What moved | Why |
|---|---|---|
| `ui/gallery_shots/{gallery-dark,gallery-light}.png` | breadcrumb trail rises 1.5 px (`crops/{before,after}/gallery-stepper.png`, trail baseline 185 → 182 at 2×) | F2-01 |
| `ui/widget_showcase_snapshot/{showcase,showcase-scrolled}.png` | breadcrumb trail, same | F2-01 |
| `ui/widget_snapshot/gallery.png` | breadcrumb + every DatePicker label/number rises 1.5 px into its band centre (`crops/{before,after}/snapshot-datepicker.png`) | F2-01, F2-03 |
| `ui/fractional_scale_shots/gallery-device-{1.0,1.5}.png`, `gallery-text-{small,large}.png`, `gallery-ui1.25-device1.5.png` | breadcrumb trail only; the Buttons & Controls panel is pixel-identical (`crops/{before,after}/frac10-controls.png`) | F2-01 |
| `settings/large-text.png` (800×900, dark, 2× text) | the four "Add account" labels render at 28 px instead of 14 px, as the live App host already did; this exposes F2-15 (default 120×40 frame too narrow for the label — `crops/after/settings-large-row.png`) | F2-05 |

Identical, as required: all 1× Store/Settings/Notes matrices (dark + light,
desktop/laptop/tablet/phone), all 480 px narrow shots, Notes `heading`/`slash`,
`store/large-text.png` (Store's shots host is `FrameHarness`, i.e. a matched
renderer — proof that the theme-driven label reproduces the legacy 28 px
exactly), `notes/large-text.png`, and the text_shots, textinput_ext,
rich_text_editor, undo_controls, search_showcase and denied_state fixtures.

## Limits

- The Settings responsive matrix lands on *External Accounts*, whose only
  controls are Buttons; toggles/dropdowns/segmented controls in other panes
  were reviewed through the `ui-shots` fixtures instead.
- `ui/tests/typography_visual_regression.rs` is a known baseline failure on
  this branch (per the coordinator); not touched, not re-baselined.
- No PIL on the host; crops were made with `sips` (bilinear upscale), so
  crop pixels are for reading, not for measuring — measurements come from
  the glyph-run assertions in the tests.
- The worktree had been cut from `origin/main` (`24ee08c66`) rather than
  `codex/macos-shell`; it was reset to `2d7e819b6` before any change (the
  research line numbers match that tip).
