# UI alignment review — A3: Browsing & preferences (store, settings, themes)

Worker unit A3 of the 2026-09-16 review "every app on UI element alignment, spacing and
layouting". Base: `codex/macos-shell` @ `ad222b830` plus the coordinator's `Cargo.lock`
fix (`ec81ff904`). Branch `ui-review/a3-browse-preferences`, local only.

All three apps are Tier-A composers: they build their surfaces from shared `ui` widgets, so
the defects found were wrong widget parameters, spacers stacked on top of column gaps (a
workaround for a block-layout limitation the toolkit no longer has — an intrinsic `Column`
is a one-column Grid and honours `with_gap`), private pixel constants where the spacing
scale already has a step, and two hard-coded dark-palette colours in build-time helpers.

Conventions read first: `ui/HIG.md` §1, §7, §7a, `ui/FORM_COMPOSITION.md`,
`ui/MANUAL_LAYOUT.md`, `docs/hand-rolled-ui-audit.md`, the 2026-09-15 review evidence
(`review-3.md`, `closure-register.md`, `controller-addenda.md` — settings-region-wide-balance —
and `implementation/composition-task-3-*`). Nothing accepted by composition task 3 (store
card containment / header density, themes Customize–Full-preview switch and specimen
containment, settings preamble) was regressed; `composition_balance.rs` still passes.

## Method

1. Source pass over `apps/settings/src/{widgets,sidebar,settings_app}.rs`, `panes/*.rs`,
   `apps/store/src/{app,widgets/card}.rs`, `apps/themes/src/{app,customizer,gallery,
   preview,widgets}.rs`: Spacer/Container stacking, `with_gap` values, private consts,
   text centred on font size, hit rect vs paint, hand-rolled controls.
2. BEFORE captures with the isolated-profile recipe (`target/ui-review/a3-cap.sh`,
   `target/ui-review/a3-extra.sh`): responsive matrix dark + light, 200 % text, 480 × 900,
   800 × 700, every store `--scenario`, every settings panel via `QOS_SHOTS_PANEL`
   (note: `--viewport` bypasses the panel route, so panel captures run without it at the
   1200 × 760 default), the four settings filename routes, pseudo-locale, themes
   `QOS_THEMES_SHOT=live|scroll|customize|preview|menu|font`. 95 PNGs per tag, each opened
   and inspected.
3. Fixes with tokens (`theme.tokens.spacing.*` where a theme is in scope,
   `ui::tokens::Spacing::default()` in the theme-less settings composition helpers),
   shared composition, measured text.
4. Geometry regressions with the production `sync → build_layout → compute_layout →
   apply_layout` cycle (new `alignment_review.rs` per app).
5. AFTER captures with the same scripts; before/after compared per finding.

Capture directories (worktree `target/ui-review/<app>/{before,after}/`, 95 PNGs per
tag): `store/{before,after}/` (27 each, incl. `narrow-480`, `detail-narrow`,
`detail-light`, `wide-1000`), `settings/{before,after}/` incl. `panels/`, `panels-light/`,
`routes/` (49 each), `themes/{before,after}/` (19 each). The AFTER set was rendered from
the final binaries (built after the last source edit) and re-inspected; the store
secondary pages and compact detail hero were re-checked after the review fixes.

## settings

Scope: `apps/settings/src/widgets.rs`, `sidebar.rs`, `settings_app.rs` (one minimal
change — the WIP-flagged file), `panes/accounts.rs`; tests `tests/alignment_review.rs`
(new, registered in `tests/it.rs`), `tests/composition_balance.rs` (harness shared).

| id | surface | defect | route / viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| S1 | content pane, every card | `cards()`/`rows()` interleaved `vgap` spacers with `with_gap(0.0)` — a workaround for block layout the intrinsic Column no longer needs; two ways to express one gap | all panels | `widgets.rs:48-61,227-240` | geometry unchanged; `settings/after/panels/*` | fixed — one `stack(items, gap)` with `with_gap(CARD_GAP / ROW_GAP)` |
| S2 | described rows (Appearance, Accessibility, Notifications, Mouse & Keyboard, Storage, Devices & Sync) | caption sat 12 px (default column gap) under its label row and 12 px above the next row: equidistant, so it did not read as belonging to its label (Accessibility: label y 178, caption 213, next label 249) | `panel-appearance.png`, `panel-accessibility.png`, 1200 × 760 dark/light | `widgets.rs:126-140` | `before/panels/panel-appearance.png` caption 459 → `after/…` 449 (4 px under the toggle, next row a full ROW_GAP away) | fixed — caption gap `s1` |
| S3 | About hero | `hero()` text block claimed the whole row and wrapped *under* the icon; title→subtitle 27 px (3 px spacer + 2 × 12 px gap); icon colour frozen from `Theme::dark().accent_primary` | `panel-about.png` 1200 × 760 | `widgets.rs:197-213` | `before/panels/panel-about.png` (icon 135–160, title 190, subtitle 237) → `after/…` (title beside icon at y 141, subtitle 4 px below) | fixed — flex-grow container beside the icon, `s1` gap, theme-primary ink |
| S4 | sidebar | selection pill inset 20 px more than the shell's search field on each side (pill 32–232 vs field 12–252); section headers at x 24 aligned with neither the pill nor the label; header rhythm was TITLE_GAP/SECTION_GAP_ABOVE/BELOW stacked on the column's default 12 px gap (63 / 44 / 26 px) | all wide captures, `settings-narrow-list.png` | `sidebar.rs:42-89,459-530` | `before/dark/dev.qos.settings-laptop.png` → `after/…`: pill 12–252 = search field, headers at the icon column (x 24), header→rows `s2`, rows `s1` apart, sections `s6`, list starts `s2` below the search band | fixed — section blocks; private consts replaced by scale steps, `ROW_HEIGHT`/`ROW_ICON_COL`/`SIDEBAR_WIDTH` kept named |
| S5 | sidebar section header | colour frozen from `Theme::dark().text_dim` (low contrast on light) | `panels-light/panel-region_formats.png` | `sidebar.rs:534-540` | `before/panels-light/…` light grey on white → `after/…` theme dim ink | fixed — `.dimmed()` |
| S6 | sidebar row label | vertical centring derived from `size × 1.25` | all | `sidebar.rs:87-89,142` | same position at 1×; now measured | fixed — `TextRenderer::measure` line box passed to `RowGeometry::compute` |
| S7 | Region & Formats (wide) | labels at x 312, dropdowns at x 932: ~600 px label/control drift at full detail width (controller addendum settings-region-wide-balance) | `panel-region_formats.png` 1200 × 760 dark/light | `settings_app.rs:327-332` | `before/panels/panel-region_formats.png` (312 / 932) → `after/…` (408 / 836) inside the toolkit's 720 px form measure, centred; narrow unchanged | fixed — `readable_content_measure()` returns the FormPage readable width for Region; External Accounts keeps 1040 |
| S8 | External Accounts, account entries | maintenance buttons 34 px next to a 40 px "Add account" and 40 px dropdowns; Edit → Sign in again 32 px apart (8 px `hgap` stacked on the row's 12 px gap); same stacking in Remove-confirm and re-auth forms | `routes/settings-accountdemo.png`, `settings-accountdemo-narrow.png` | `panes/accounts.rs:681-735,524,891` | `before/routes/settings-accountdemo.png` → `after/…` (all buttons 40, one row gap) | fixed — `ACTION_BUTTON_H`, `hgap` removed |
| S9 | Region & Formats on a phone | `choice_row` rows wrap independently: "Currency" stays label-left/control-right while longer labels stack label-above (control left-aligned) in the same card | `dark/dev.qos.settings-phone.png` 390 × 844 | `widgets.rs:105-112` | unchanged | open — needs F3: `Field`/`Section` measured placement with a trailing-control option, so a whole section switches together (an app-side fix would hand-roll the form policy) |
| S10 | composition helpers | `Spacing::default()` is the only scale reachable before a theme resolves; a custom theme's retuned `s2`–`s4` reach widget padding but not these gaps | all | `widgets.rs:26-38` | n/a | open — needs F1: theme access (or theme-aware gap tokens) for build-time composition |
| S11 | sidebar ↔ shell | the shell's field inset (`app_shell.rs` private `PAD`) is mirrored as `sidebar::SHELL_FIELD_INSET` | all | `sidebar.rs:63` | n/a | open — needs F1/F6: export the inset so the app and its test import it |
| S12 | 200 % text | sidebar labels, dropdown values and several intro lines do not scale | `large-text.png`, `panel-appearance-large.png` | shared `Dropdown`, `Text::with_size` | unchanged | open — text-geometry task 22 / F2 (not alignment) |

Conventions observed: label and trailing control share one centre line in every row
type (Toggle 32, Dropdown 40, value text); all trailing edges meet the card's inner right
edge; `CARD_GAP` (20) stays a named, deliberately off-scale step between `s4` and `s6`;
`ROW_GAP` = `s3`; page heading eyebrow/title/subtitle now `s1` apart.

## store

Scope: `apps/store/src/app.rs`; `apps/store/src/widgets/card.rs` reviewed, unchanged
(header/body/footer gutters and footer `Actions` baselines were correct after task 3).
Tests: `apps/store/tests/alignment_review.rs` (new).

| id | surface | defect | route / viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| T1 | browse band | source summary centred against the label+dropdown stack, ~10 px above the dropdowns' value line | `wide.png`, `wide-1000.png`, `light/…-desktop.png`, `compact.png` | `app.rs:1223-1261,2397-2465` | `before/wide.png` summary y 215 vs dropdown centre 225 → `after/wide.png` both 221 | fixed — `build_filter_band`: labels row + controls row with identical flex cells (`FILTER_CELL_MIN_W`), so the summary sits on the pickers' centre line |
| T2 | detail hero (compact) | 14 px spacer stacked on the column's 24 px gap → 62 px between artwork and title | `detail-narrow.png` 480 × 900 | `app.rs:1549-1554` | `before/detail-narrow.png` artwork bottom 148 → title 215 (62) → `after/…` 24 | fixed — single `s6` gap; obsolete 44 px wrapper band around the back control removed |
| T3 | detail hero (wide) | 12 px spacer stacked on the 16 px gap → 44 px between back control and artwork | `detail.png` 1200 × 800 | `app.rs:1566-1573` | 48 → 24 (`after/detail.png`) | fixed — `s6` |
| T4 | Flatpak remotes | "Configured remotes" heading flush on its first card (0 px) while the form card had a 12 px spacer; header composed of 0-gap column + 8/8/12 px spacers | `remotes.png` 1200 × 800 | `app.rs:2035-2054,2176-2195` | `before/remotes.png` heading 375 / card 384 → `after/…` heading→card `s2`, form→heading `s3`, header `s6` above body | fixed — `secondary_page_header` + `secondary_page`, heading grouped with its cards |
| T5 | Trust & moderation | header layered as `with_gap(10)` + 6 px spacers; title 24 vs remotes 26; note field label/input/caption spaced by 10 + 6 + 10 | `moderation*.png` | `app.rs:1853-1890` | `before/moderation-case.png` → `after/…` one header composition shared with remotes, field block `s2` | fixed |
| T6 | catalogue sections | grid gap 14 and caption→grid 14 (off-scale), heading→caption 4 + 0-gap column; "At a glance" facts block 0-gap + 10/14/14 spacers; header title/subtitle 10 px spacer + 14 gap | `wide.png`, `detail.png` | `app.rs:2670-2712,1690-1705,2330-2362` | grid/section rhythm `s1`/`s3`/`s6`, header `s2`/`s3` inside an `s6` band | fixed — tokens |
| T7 | status bubble | transient "N apps available" pill overlays the last card's action when the list is scrolled to top | `wide.png` | `draw_status_bubble` | unchanged | open (design choice: floating transient; a landing zone exists at scroll end) |
| T8 | 200 % text | buttons/dropdown values do not scale with card text | `large-text.png` | shared widgets | unchanged | open — F2 |

Conventions observed: 3-column desktop grid gutter and section-heading rhythm all on
`s3`; sibling cards' footers share one baseline (task 3); page content bounded to 760 /
1120 px as before; secondary pages share one header (back `s3` above a 24 px title, `s2`
above the explanation, `s6` above the body).

## themes

Scope: `apps/themes/src/app.rs`, `customizer.rs`; `gallery.rs`, `widgets.rs`,
`preview.rs` reviewed. Tests: `apps/themes/tests/alignment_review.rs` (new).

| id | surface | defect | route / viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| U1 | narrow Customize / Full preview switch | sat flush on the top bar's hairline (top margin 0; left/right had the 16 px content gutter) | `narrow-customize.png`, `narrow-live.png` 480 × 900 | `app.rs:1572-1583` | `before/narrow-customize.png` tabs y 56 → `after/…` 72 (one gutter below the bar); scroll extent includes the margin | fixed |
| U2 | customizer colour chips | fixed `GRID_H = 152` (two chip rows) — at narrow width all 11 chips fit one row, leaving a ~50 px dead band between chips and the "Accent · #…" caption | `narrow-customize.png`, `narrow-scroll.png` | `customizer.rs:33,265-275,526-542` | caption 252 → 232 directly under the chips (`after/narrow-customize.png`); docked 2-row height unchanged (152) | fixed — `chips_per_row` / `chip_grid_height(width)`; app tells the panel its width; `intrinsic_height()` follows |
| U3 | customizer actions | Duplicate / Reset / Delete 34 px under a 40 px font dropdown | all | `customizer.rs:41,303` | `after/dark/dev.qos.themes-laptop.png` 40 px | fixed — `ACTIONS_H = 40` = dropdown height |
| U4 | customizer `children()` | action buttons missing from the read-only tree (present in `children_mut`) — invisible to a11y/inspection | n/a | `customizer.rs:556-563` | n/a | fixed — mirrored |
| U5 | top bar hamburger/close toggle | hand-painted (`draw_toggle` + rect hit-test), 34 px | narrow captures | `app.rs:340-410` | unchanged | open — needs F2/F6: shared icon `Button` with the shell's overlay-toggle semantics (keyboard + a11y routes live in `handle_overlay_shortcuts`) |
| U6 | gallery rows / preview specimen at 200 % | fixed 58 px rows and fixed specimen geometry do not reflow (names overflow the 272 px gallery) | `large-text.png` | `widgets.rs:33`, `preview.rs:30-41` | unchanged | open — text-geometry task 22 |
| U7 | named off-scale values | `customizer::GAP` 22, `gallery::PAD` 14, `preview::PANE_MARGIN` 22, `TOGGLE_SIZE` 34, top-bar padding 18 | — | — | documented, unchanged | recorded (deliberate; `GAP` documented as between `s4`/`s6` because sliders carry their labels 18 px above the track) |

## Tests

Environment note (coordinator): this worktree sits under `/Users/xaver/dev/qos/`, whose
`.cargo/config.toml` is merged and appends `-C target-cpu=generic` for aarch64 macOS,
which fails `ring`'s CPU const-assert. Every cargo command was run as
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 RUSTFLAGS="-C
force-frame-pointers=yes --remap-path-prefix=/build=." CFLAGS_aarch64_apple_darwin=
"-fstack-protector-strong" CXXFLAGS_aarch64_apple_darwin="-fstack-protector-strong"
cargo … --locked --no-default-features`; no cargo config was edited. All new tests were
run, not just checked.

Results (final code):

- `cargo test -p settings --test it` — 38 passed (category_nav 16, category_sidebar 1,
  control_interaction 3, no_app_title_header 2, scroll 1, search_focus 3, shots_smoke 1,
  composition_balance 3, alignment_review 5), 0 failed.
- `cargo test -p settings --lib -- sidebar widgets panes::region` — 15 passed
  (incl. new `pill_shares_the_shell_search_fields_left_edge`), 0 failed.
- `cargo test -p settings --test accounts_mail_loop --test prefs_persistence` — 1 + 6 passed.
- `cargo test -p store --test alignment_review` — 4 passed; `cargo test -p store --lib` —
  103 passed, 0 failed.
- `cargo test -p themes --test alignment_review` — 3 passed; `cargo test -p themes --lib` —
  52 passed, 0 failed.
- `cargo check -p settings -p store -p themes --tests` — clean (no warnings).

Pre-existing failures: none observed in the scoped suites.

## Limits

- The whole-settings and whole-store test binaries were run scoped as listed; no
  workspace-wide build or test was run (host memory constraint).
- Store/themes hosts have no `tests/common` module; their headless harness is duplicated
  per crate (the settings copy is shared through `composition_balance::lay_out_at`). A
  `ui::headless` walk/geometry helper would remove all three copies (F-unit).
- Phone-width Region wrap inconsistency (S9), theme-aware build-time spacing (S10), the
  shell field inset export (S11) and the themes hamburger (U5) need framework work and are
  left open; text-scale residuals (S12, T8, U6) belong to task 22.
- `--headless-drive` target dumps were not needed: every alignment claim is asserted by
  the new geometry tests against widget rects.
