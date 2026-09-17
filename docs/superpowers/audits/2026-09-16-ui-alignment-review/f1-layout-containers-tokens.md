# F1 — Layout containers, responsive shells, tokens, lint

Unit F1 of the 2026-09-16 UI alignment review (root of the framework). Branch
`ui-review/f1-layout-containers-tokens`, based on `codex/macos-shell` at
`2d7e819b6` plus the coordinator's `Cargo.lock` fix (`f9a579c9c`). Local only;
nothing pushed.

## Scope

`ui/src/widget/{row,column,stack,container,spacer,scrollable,scrollbar,layout,layout_info,manual}.rs`,
the breakpoint/width/pad constants of `{app_shell,nav_split,nav_shell,sidebar}.rs`,
`ui/src/theme.rs`, `ui/src/tokens/`, `ui/tests/{design_token_audit,tokens_test}.rs`,
the spacing/alignment sections of `ui/HIG.md`, plus the new
`ui/tests/f1_spacing_token_audit.rs` and its baseline. One-line registrations
outside that list were unavoidable and kept minimal: a re-export added to the
existing `pub use layout::{..}` line in `ui/src/widget/mod.rs`, and the new
test module in `ui/tests/it.rs` (the crate has `autotests = false`).

## Environment notes (affect every unit)

- The worktree handed to this unit was cut from the `origin/main` lineage
  (`24ee08c66`), not from `codex/macos-shell` as the brief states; none of the
  files the brief describes (`manual.rs`, `layout.rs`, `MANUAL_LAYOUT.md`, the
  2026-09-15 audits) existed there. The fresh branch was reset to the
  `codex/macos-shell` tip before any work. Other workers at `24ee08c66` are
  auditing a different tree.
- Worktrees nested under `/Users/xaver/dev/qos/` inherit that checkout's
  `.cargo/config.toml`. Cargo concatenates `[target.'cfg(..)'] rustflags`
  across config files, so the parent's
  `cfg(all(target_arch = "aarch64", not(target_os = "linux")))` section adds
  `-C target-cpu=generic`, which strips `aes`/`sha2` and makes `ring 0.17.14`
  fail its `CAPS_STATIC == MIN_STATIC_FEATURES` const assertion. Every cargo
  command here was run with
  `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
  (identical to the worktree's own macOS section; `RUSTFLAGS` replaces config
  rustflags instead of merging). The prewarm worktree under `/private/tmp`
  does not inherit and built without it.

## Method

1. Read HIG §7, §7a, §9.6, `MANUAL_LAYOUT.md`, `FORM_COMPOSITION.md`, the
   2026-09-15 framework-layout progress record; verified every line reference
   in the brief against the tree.
2. BEFORE captures from binaries built on the untouched tree: Store, Settings,
   Notes (`--responsive-matrix` dark+light, 200 % text, 480 px), and the
   widget fixtures `nav_shell_shots`, `gallery_shots`, `widget_showcase_snapshot`,
   `widget_snapshot`, `scrollbar_shots`, `card-composition-shots`,
   `form-composition-shots`, `manual-composition-shots`. Every PNG opened.
3. Source pass, fixes with tokens / helpers / opt-in knobs, defaults unchanged.
4. Regression tests in the widget modules and in the theme/tokens tests.
5. AFTER captures from rebuilt binaries; byte comparison against BEFORE.

Captures: `target/ui-review/{store,settings,notes}/{before,after}/…` and
`target/ui-review/ui/<scenario>/{before,after}/…` (worktree-local; not
committed).

## Findings

| id | surface | defect | where | status |
|---|---|---|---|---|
| F1-01 | `Row` | gap always `gap_standard` on both axes, no per-instance override; `justify_content` never set; `align_items` fixed to Center/Stretch. Callers reach for a grow `Spacer` or a wrapper `Container` to get trailing alignment or a tighter cluster. | `ui/src/widget/row.rs` `build_layout` | fixed — `with_gap(px)`, `with_justify(taffy::JustifyContent)`, `with_align_items(taffy::AlignItems)`; defaults unchanged, tests pin default gap = `gap_standard` and start packing |
| F1-02 | `Column` | default inset written as `theme.padding_standard * 2.0` — arithmetic on a shortcut, not a token; four doc comments repeat it | `ui/src/widget/column.rs` `build_layout` padding block | fixed — `theme.tokens.spacing.s6`; identical on every theme and scale (test covers stock, `ui_scale` 1.5 and a custom 9 px theme) |
| F1-03 | `Spacer` | only a grow-to-fill form; a fixed gap is spelled `Container::new().with_no_padding().with_height(px)` (54 sites incl. fixtures) and never scales with the theme | `ui/src/widget/spacer.rs` | fixed — `Spacer::fixed(px)` and `Spacer::token(SpacingStep)` (resolved from the Theme in `build_layout`, so it follows `ui_scale`); `Spacer::new()` unchanged |
| F1-04 | taffy `Style` builders | no insets helper; padding is a four-field `taffy::Rect { left: length(..), .. }` at 15 sites in `ui/src/widget` and 16 in `shell`/`apps` | `ui/src/widget/layout.rs` | fixed — `insets_all(px)`, `insets_xy(h, v)`, `insets(t, r, b, l)` (negatives clamp to 0; no caller passes one), re-exported from `ui::widget`; adopted in `Row`, `Column`, `Container` |
| F1-05 | tokens | HIG §9.6 density values (bar 40, bar control 28, panel row 44, selector 28, min target 32, panel widths 320/360/560, glyph 20 in a 32 hit rect) existed only as prose; icon sizes and icon–label gap (6, from `Button`/`Tabs`) had no token | `ui/src/tokens/metrics.rs` (new), `tokens/mod.rs` | fixed — `Tokens::metrics: Metrics`, `Metrics::BASE`, scaled by `ui_scale` in `Theme::multiply_dimensions`; never by a custom theme's `spacing_px` |
| F1-06 | `Theme::from_custom_spec` | rewrote only `spacing.{s2,s3,s4}` (as `0.66×`, `1×`, `1.33×` with a 2 px floor) and set `radii.xl = 2.4 × md`; `s0/s1/s6/s8/s12/s16` stayed at stock so a 17 px theme had `s6 = 24` (< 2 × `padding_standard`), and a custom theme at the stock 8 px radius produced `xl = 19.2` instead of 24 | `ui/src/theme.rs` | fixed — whole spacing scale = `Spacing::BASE.scaled(spacing_px / 12)`; radii 1:2:3:6 from `radius_px`. **Deliberate value change** for custom themes only: `s2` 7.92→8.0 and `s4` 15.96→16.0 at 12 px; `xl` ratio 2.4→3.0; the 2 px floor on `s2` is gone (validation already bounds `spacing_px ≥ 2`). Stock themes unchanged (test `custom_theme_at_stock_values_reproduces_the_stock_scales`) |
| F1-07 | responsive shells | four incompatible constant sets: `AppShell` 264 / 720 / PAD 12 / CTRL_H 34; `NavSplit` 300 / 900; `NavShell` 280 / 340 / 700 / CHIP 36 / CHIP_INSET 12 / BACK_CHIP 104; `SidebarDrawer` 320 / 760 | `app_shell.rs`, `nav_split.rs`, `nav_shell.rs`, `sidebar.rs` | partly fixed — where the value already equals the token it now derives from `Metrics::BASE` / `Spacing::BASE`: `DEFAULT_SHELL_BREAKPOINT` = `compact_breakpoint` (720), `PAD` and `CHIP_INSET` = `s3`, `DEFAULT_NAVSHELL_SIDEBAR_WIDTH` = `sidebar_width` (280), `DEFAULT_NAVSHELL_OVERVIEW_WIDTH` = `overview_width` (340), `DEFAULT_DRAWER_WIDTH` = `drawer_width` (320). No pixel changed. **Open — product decision** (each pinned by tests and/or apps): `AppShell` sidebar 264 vs token 280; `NavSplit` 300 / 900 (`api_tester`, `slides` build on `DEFAULT_BREAKPOINT`); `NavShell` breakpoint 700 vs 720; drawer breakpoint 760; `CTRL_H`/`BACK_BTN` 34 vs `CHIP_SIZE` 36 vs `bar_height` 40 — the Back affordance is a different size in the two shells. Each is documented at its definition. |
| F1-08 | `design_token_audit.rs` | hard-fail pattern `Padding::all\(` named a type that does not exist, and `Color::rgb8?\s*\(\s*\d` half-named one (`Color::rgb8(` does not exist; `cosmic_text::Color::rgb(r, g, b)` does, at `richtext.rs:473` with variables) | `ui/tests/design_token_audit.rs` `forbidden_patterns` | fixed — `Padding::all` removed; the colour pattern corrected to the live `Color::rgb\s*\(\s*\d` (literal glyph colour) so the hard-fail set is not weakened; `to_rounded_rect(\d` and `blur:\s*\d` kept (live, zero hits — `surface.rs` hits are inside `#[cfg(test)]`); new `forbidden_patterns_match_the_shapes_they_name` pins all three. No ratcheted debt became a hard failure. |
| F1-09 | lint | no spacing/geometry ratchet: pixel constants and literal `length(N)` entered widget, chrome, shell and app code unrecorded | `ui/tests/f1_spacing_token_audit.rs`, `ui/tests/f1_spacing_token_baseline.txt` (new) | fixed — per-file monotone-decreasing ratchet over `ui/src/widget/**`, `ui/src/chrome/**`, `shell/src/**`, `apps/*/src/**`; counts `pixel_const` (module-level `const NAME: f32|f64 = <whole number ≥ 1>;`) and `taffy_length` (non-zero literal in `length(..)`); skips comments, `#[cfg(test)]` spans, `tests/`, `tests.rs`, `*_tests.rs`; arithmetic is never a literal. Baseline generated from this tree: 264 entries (241 `pixel_const`, 23 `taffy_length`; 1,312 literals in total, 17 of them in F1-owned files — the token derivations in F1-07 already removed `PAD`, `CHIP_INSET` and four defaults from the count). Regenerate with `UPDATE_F1_SPACING_BASELINE=1` after reviewing the diff. Two self-tests prove the regexes match/skip what they claim and that the walk reaches every root. |
| F1-10 | `Stack` | no alignment options | `ui/src/widget/stack.rs` | documented — deliberately a pure z-order primitive; the layer owns its geometry (a `Column`/`Row` with insets and justify, or `Container::with_centered_max_width`). No caller in `ui`, `shell` or `apps` (9 sites) needs the stack to anchor; the doc says how to add an opt-in if one does. |
| F1-11 | `Scrollable`/`Scrollbar` | `BAR_THICKNESS` 8, `BAR_MARGIN` 2, `HOVER_EXPAND` 6, `LINE_STEP` 48, `MIN_THUMB_LEN` 24 are module constants | `scrollable.rs:37-46`, `scrollbar.rs:28` | open — recorded by the new ratchet; values are internal to the shared control and consistent across themes, so left as-is rather than inventing tokens for them |

Visual findings from the BEFORE captures that belong to other units:

| id | surface | defect | viewport / theme | evidence | needs |
|---|---|---|---|---|---|
| V-01 | `NavShell` wide | the sidebar's header box starts at y ≈ 70 while the overview and detail headers start at y ≈ 12: the Back-chip band (`CHIP_INSET*2 + CHIP_SIZE` = 60) is reserved above the sidebar even when docked, so the three panes' headers do not line up | 1240×780 dark | `target/ui-review/ui/nav_shell_shots/before/nav_shell-wide.png` | F3 (NavShell drawing; constants only are F1) |
| V-02 | component gallery | the two columns do not top-align: card tops differ by 33 px (row 1), 10 px (row 2) and 86 px (row 3) because the row centres children of unequal height (`Row` default `align_items: Center`) | 1200×2300 dark and light | `target/ui-review/ui/gallery_shots/before/gallery-{dark,light}.png` | fixture `ui/examples/gallery.rs`; remedy is the new `Row::with_align_items(taffy::AlignItems::Start)` |
| V-03 | primitive account form | label centred against the whole control + help stack (documented in `FORM_COMPOSITION.md`) | 1080×800 dark | `target/ui-review/ui/form_composition_shots/before/primitive-wide.png` | fixture `ui/examples/form_composition_shots.rs`; remedy `Row::with_align_items(Start)` |
| V-04 | Settings at 200 % text | sidebar labels, section headers, the first body paragraph, card titles and button labels stay at 1× while the page title, subtitle and other paragraphs scale — mixed renderer-relative (`with_size`/default `Text`) and role-based text in one screen | 800×900 dark, `--text-scale 2` | `target/ui-review/settings/before/large-text.png` | the Settings app unit (A-group); if `Button` labels are the cause, F2 |
| V-05 | Store, Notes, card/manual/form fixtures, scrollbar states | no alignment defect found at desktop/laptop/tablet/phone, 200 % text or 480 px | all | `target/ui-review/{store,notes}/before/…`, `target/ui-review/ui/{card,manual,form}_composition_shots/before/…` | — |

## Code review (`/code-review high`) — dispositions

Ten findings; five fixed in this branch, five recorded:

| # | finding | disposition |
|---|---|---|
| R1 | Proportional custom-theme spacing now shrinks `Toolbar`'s minimum hit rect, which reads `theme.tokens.spacing.s8` (`ui/src/widget/toolbar.rs:146, :519`); the old partial rewrite left `s8` at 32 by accident | **Open — needs the Toolbar owner (F3)**: read `theme.tokens.metrics.min_pointer_target` instead of `spacing.s8` (one line, two sites). A hit target sized from a spacing step is the defect the `Metrics` family exists to remove; HIG §7b now says so. Until it lands, a custom theme with `spacing_px < 12` (the Themes app allows 6) gives Toolbar buttons a 16 px minimum. |
| R2 | `Column` inset moved from `padding_standard * 2` to `spacing.s6`; a host theme that overrides only the legacy `padding_standard` alias (preserved by `Theme::with_component_scales`) no longer moves the gutter | accepted as the migration direction (tokens are the source of truth; the alias is documented "legacy … kept for compat"); the misleading comment is corrected. No test in the tree asserts the alias-driven gutter; `ui/tests/support/application.rs:63` sets the alias but asserts nothing about Column. |
| R3 | Removing `Color::rgb8?\(\d` weakened the audit: `cosmic_text::Color::rgb(u8,u8,u8)` is live | **fixed** — pattern restored as `Color::rgb\s*\(\s*\d`, self-test extended (see F1-08) |
| R4 | `test_module_spans` treated every `#[cfg(test)]` as a braced module, exempting whatever braced item followed a `#[cfg(test)] const …;` / `mod tests;` | **fixed** in `f1_spacing_token_audit.rs`: the span is the body of the item the attribute governs (attributes and multi-line signatures walked; a `;`-terminated item exempts only itself); two new self-tests. The identical logic in `design_token_audit.rs:102` is left as is — correcting it changes which lines the colour ratchets see and would move both colour baselines under the user's uncommitted WIP on that file — **Open — needs the design_token_audit owner** (or the shared `ui/tests/support/ratchet.rs` helper R8 proposes). Baseline regenerated with the corrected finder. |
| R5 | Zero-extent `Spacer` (`fixed(0.0)`, `token(S0)`) at the origin collided with the `rect == Rect::ZERO` "never laid out" sentinel → `is_layout_dirty()` true forever | **fixed** — explicit `laid_out` flag; test `zero_extent_spacer_at_the_origin_is_not_permanently_layout_dirty` |
| R6 | `CustomThemeSpec::validate` admits `spacing_px` 2.0..=64.0, a range sized for the old formula; proportional scaling makes 2 px give `s1 = 0.67` and 64 px give `s6 = 128` | recorded — the range is the schema-v1 settings contract; tightening it would reject files that validate today. The Themes app already clamps to 6..22. **Open — product decision** (tighten `validate`, or clamp the factor). |
| R7 | HIG said the shells "derive" from scaled `Metrics` while they read the unscaled `Metrics::BASE` consts; at `ui_scale 1.5` the token says 1080 / 420 and `AppShell` still docks at 720 / 264 | **fixed in the text** (HIG §9.6 now states the consts are unscaled and that reading `theme.tokens.metrics` in `build_layout` is the shells' next step). Making the shells resolve the scaled token is drawing-logic in files this unit owns constants for only — **Open — needs F3**. |
| R8 | ~170 lines of ratchet plumbing duplicated between the two audit files inside one `it` binary that already has `ui/tests/support/` | recorded — sharing needs `ui/tests/support/mod.rs` and the WIP `design_token_audit.rs`; proposed home `ui/tests/support/ratchet.rs`. |
| R9 | `insets_all(px)` duplicates taffy's `impl FromLength for Rect<T>` (`padding: length(pad)` already works) | kept (it was asked for and reads as intent); doc now states the equivalence and that the negative clamp is the addition. |
| R10 | The ratchet fails on `stale` for shell/app-only commits from the `ui` crate's tests | same semantics as the existing `app_and_shell_theme_escape_hatches_do_not_grow`, which the brief asked this ratchet to mirror; recorded under Limits with the per-crate-baseline alternative. |

## New APIs and tokens (for other units to adopt)

- `ui::tokens::Metrics` (`theme.tokens.metrics`): `bar_height` 40, `bar_control_height` 28,
  `panel_row_height` 44, `selector_height` 28, `min_pointer_target` 32,
  `panel_width_utility` 320, `panel_width_list` 360, `panel_width_sheet` 560,
  `icon_sm` 14, `icon_md` 20, `icon_lg` 24, `icon_label_gap` 6,
  `sidebar_width` 280, `drawer_width` 320, `overview_width` 340,
  `compact_breakpoint` 720. `Metrics::BASE` for `const` contexts;
  `Metrics::scaled(f)`.
- `ui::tokens::Spacing::BASE`, `Spacing::step(SpacingStep)`, `Spacing::scaled(f)`;
  `Spacing` is now `Copy + PartialEq`. `ui::tokens::SpacingStep::{S0..S16}`.
- `ui::widget::{insets_all, insets_xy, insets}` → `taffy::Rect<LengthPercentage>`.
- `Row::with_gap(px)`, `Row::with_justify(taffy::JustifyContent)`,
  `Row::with_align_items(taffy::AlignItems)`.
- `Spacer::fixed(px)`, `Spacer::token(SpacingStep)`.
- HIG §7b (new) states the related-row vs section-gap ladder and these APIs;
  §9.6 now names the `Metrics` field for each density value.

## Tests

Scoped runs (all with `--locked --no-default-features`, `CARGO_BUILD_JOBS=1`):

- `cargo test -p ui --lib -- column:: row:: container:: spacer:: manual:: layout:: theme:: tokens::` — first run (before the review fixes) 197 passed, 0 failed; final run 198 run, 197 passed, 1 failed: `theme::configured_theme::an_absent_settings_file_still_yields_a_usable_theme` (`left: "High Contrast", right: "Dark"`). **Pre-existing race, not this branch**: that test and its sibling `a_high_contrast_preference_on_disk_is_the_theme_that_starts` both `set_var("XDG_CONFIG_HOME", …)` on the process-global environment (the sibling writes a high-contrast `settings.json` under its directory), so under parallel `cargo test` one can read the other's file — `ui/tests/it.rs` notes nextest isolates such tests per process. Neither test nor `Theme::configured` changed on this branch; six targeted reruns (`-- theme::configured_theme::`, three serial with `--test-threads=1`, three parallel) all pass 2/2. Reported verbatim, not masked.
- `cargo test -p ui --test it -- f1_spacing_token_audit:: tokens_test:: design_token_audit:: resize_reflow_test::` — final run 24 run: 22 passed, 2 failed (first run 22/20/2; the two extra passes are the new span-finder self-tests). Both failures are **pre-existing at the base tip** in files this unit did not touch; reproduced verbatim, not masked:
  - `design_token_audit::theme_escape_hatches_do_not_grow` — `src/text_render/renderer_regressions.rs [white_or_black]: 5 occurrence(s), baseline 0`
  - `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` — `apps/browser/src/browser_tab_strip.rs [literal_color]: 1 occurrence(s), baseline 0`; `shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0`; `shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0`

  Those three files and both colour baselines are untouched on this branch; the last commits to them are `09c998a2d`, `35f4cf2f2`, `ac81de14e` on `codex/macos-shell`. The 20 passing include the three new `f1_spacing_token_audit` tests, the 12 `tokens_test` tests, `design_token_audit::{no_hardcoded_style_literals_in_tree, forbidden_patterns_match_the_shapes_they_name, product_apps_use_the_shared_responsive_launcher}` and the `resize_reflow_test` cases.
- `cargo clippy -p ui --lib` — 130 errors, **zero in F1-owned files**. Four were in F1 files at first run and are fixed: three `doc_markdown` hits introduced by this unit's new doc comments (`AppShell`/`NavShell` without backticks) and one pre-existing `unnecessary_literal_bound` in `scrollable.rs:1085` (`fn name(&self) -> &str` → `&'static str`). The 130 that remain are pre-existing, outside this unit's files and reported here rather than touched: `ui/src/webview/chromium/mod.rs` 108, `ui/src/webview/mod.rs` 5, `ui/src/webview/chromium/protocol.rs` 4, `ui/src/widget/toolbar.rs` 3, `ui/src/webview/test_engine.rs` 3, `ui/src/widget/webview.rs` 2, `ui/src/widget/text_input.rs` 2, `ui/src/widget/tabs.rs` 1, `ui/src/widget/empty_state.rs` 1, `ui/src/webview/profile.rs` 1 (pedantic is `deny` workspace-wide; the base tip's chromium work carries most of it).
- `cargo check -p ui --tests` — `Finished` on the final source state (run after the last source edit).

New tests: `spacer::tests` (6, incl. the zero-extent dirty-flag case), `row::tests` (+4), `column::tests::default_inset_is_the_s6_token_on_every_scale`,
`theme::tests::{custom_theme_spacing_rescales_the_whole_scale_proportionally, custom_theme_at_stock_values_reproduces_the_stock_scales, custom_theme_radius_rescales_the_whole_scale_proportionally}`,
`tokens_test::{radii_scale_is_4_8_12_24_pill, spacing_base_is_the_default_and_steps_resolve_by_name, metrics_default_to_the_hig_density_table, ui_scale_scales_metrics_with_spacing}` (+ `gap_standard == s3` in `theme_exposes_tokens`),
`design_token_audit::forbidden_patterns_match_the_shapes_they_name`,
`f1_spacing_token_audit::{spacing_patterns_match_the_shapes_they_claim, test_module_spans_cover_only_the_test_module, test_module_spans_do_not_exempt_the_item_after_a_bodyless_test_item, test_module_spans_follow_attributes_signatures_and_same_line_modules, spacing_literals_do_not_grow}`.

## AFTER captures

Same scripts, binaries rebuilt from this branch. Content-hash comparison of
all 95 BEFORE PNGs against their AFTER twins: **91 byte-identical, 4
different, 0 missing.** The identical set includes every Store, Settings and
Notes capture (31: desktop/laptop/tablet/phone × dark/light, 200 % text,
480 px, Notes drawer) and every `nav_shell_shots`, `scrollbar_shots`,
`card-`, `form-` and `manual-composition-shots` image — i.e. every default
metric this unit touched (`Row` gap/alignment, `Column` inset, `Container`
padding, `Spacer::new`, the shell constants) renders exactly as before.

The four that differ are the fixtures containing a `Spinner` and an
indeterminate `ProgressBar`, which animate on wall-clock time. A stdlib PNG
decoder pinned the changed pixels:

| image | changed px | region | what is there |
|---|---|---|---|
| `gallery_shots/{dark,light}` | 90 / 532 | rows 649–727, x 609–1143 | "Feedback & Loading": indeterminate bar + spinner |
| `widget_showcase_snapshot/showcase.png` | 243 | rows 288–338, x 25–54 | "New: indicators" spinner |
| `widget_snapshot/gallery.png` | 147 | rows 46–76 (bar + spinner); rows 732–740, x 418–480 (TreeView `README.md` label) | animated indicators; a text row |

Re-running the same rebuilt binary (`after2`) differs from `after` in the
same four images and the same regions (gallery-dark: 1,046 px, rows
649–727; widget_snapshot: rows 46–76 and 732–741), so the differences are
run-to-run nondeterminism of the animated indicators (and one label row), not
a layout change. No other pixel in any capture moved.

## Limits

- The spacing ratchet baseline reflects this branch. Merging the other 17
  units will change counts (down where they tokenised, up where a new
  literal slipped in); the test reports both directions. Expect one
  `UPDATE_F1_SPACING_BASELINE=1` run after the merge, reviewed line by line.
- `pixel_const` uses a whole-number heuristic; a whole-number rate constant
  (`REVEAL_RATE: f64 = 14.0`) is counted and a pixel constant written as
  `12.5` is not. Both are visible in the baseline and neither can grow.
- Shell breakpoints and widths that differ from the tokens were left in place
  and documented rather than moved; unifying them changes every app on that
  shell and is recorded above for a deliberate decision.
- Adoption of `insets_*`, `Spacer::fixed/token`, `Row::with_*` and `Metrics`
  outside `Row`/`Column`/`Container`/the four shells is left to the owning
  units; this unit did not edit their files.
