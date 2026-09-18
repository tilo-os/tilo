# A1 — devtools + browser: 1× polish (api_tester, db_client, code_editor, containers, diffmerge, git_client, browser)

Base: `fd1f84391` (Unit 0, `ux1x/unit0`). Branch: `ux1x/a1-devtools-browser`.
Evidence: `target/ux1x/a1/<app>/{before,after}/{dark,light}/` — `<state>.png` per
declared scenario / shots flag, `dev.qos.<app>-{phone,tablet,laptop,desktop}.png`
(responsive matrix), `<state>.inspection.json` + `<state>.audit.json`
(`--headless-drive` geometry through `usability.geometry_audit`). Every PNG named
below was opened with the Read tool and is described in one sentence in the
surface tables. Capture harness: `target/ux1x/a1/capture.py` (isolated `HOME`/XDG,
`QOS_SHOTS_THEME`, `QOS_SHOTS_TEXT_SCALE=1.0`, `QUICK_UI_VELLO_CPU=1`).

## Six-rule checklist

1. **Nothing touches.** git_client file rows (`changes.rs` `Metrics::row_gap`,
   `s1`) and commit rows (`history.rs` `Metrics::pitch()` = row + `s1`) no longer
   share pixels; section-header actions sit `s1` above the first row; the row
   select region ends `s1` before its chevron; branch checkout rows end `s2`
   before the delete affordance (was a 4 px overlap); off-viewport rows are no
   longer emitted as hit/semantic regions under the pinned commit box. code_editor
   `BottomPanelToggle` is inset `s1` from the bar's top and from the editor (was
   a full-height slab touching the content). Lint: zero `touching_peers` and zero
   `semantic_bounds_overlap_unresolved` on every git_client scenario, containers
   default/confirm/prompt, api_tester, db_client (see "Overlap lint").
2. **Every surface has a derived edge or a contrast step.** New helpers route
   every own edge through Unit 0: `git_client::draw::{card, edge, button, PillEdge}`,
   `containers::draw::{card, edge, separator}` wrap `ui::surface::{card, edge_on,
   separator}`; hunk bands, app bars, side panels, headers, list-column rules,
   kv-row separators, stat cards, section banners, log consoles, popups, dialogs
   and sheets are edged with `edge_on(fill)`, `ui::chrome::panel_border` /
   `panel_ground` (Panel role) or `ButtonFace`. `border_standard` remains only as a
   rule on `surfaces.base` (api_tester tab-strip rule, containers list/header
   rules on base, db_client grid outer border and structure header rule on base,
   diffmerge and git_client column dividers on base, browser address overlay —
   unchanged toolkit-owned).
3. **One rhythm.** api_tester rail: stacked controls (env → button → disclosures)
   one `rail_row_gap` = `theme.tokens.spacing.s2` (was 8 / 4 mixed); rows inside an
   open action group `rail_group_gap` = `s1` (related rows under their disclosure);
   both asserted by `rail_controls_share_one_token_gap`; a height-starved rail
   (open group in a 480 px window) hands the empty "COLLECTIONS" band to the
   footer instead of overflowing (HIG §7a).
   containers rail items `s2` apart (was 6 px). diffmerge chip clusters `s2`
   (was 6 px). git_client list rows `s1`, sections `s2`. db_client connect sheet
   sized from its content (`s6` before the action row) instead of a fixed
   320/512 px panel with dead space.
4. **Centred content.** Hand-painted buttons now paint through `ButtonFace`
   (labels centred on their measured line): git_client Fetch/Pull/Push, Commit,
   Branch, Open, Discard file, hunk Stage/Unstage/Discard; db_client Cancel/Run
   (destructive confirmation). diffmerge history preview gutter measured from
   the widest line number + `s3` (was a fixed 48 px). api_tester rail captions
   ("ENV", "COLLECTIONS") sit on their measured line boxes `s1` above what they
   label (`env_caption_h`, `caption_h` measured in `adjust_layout_with_measure`)
   instead of fixed 14 / 24 px bands — the ENV caption was only ~2.75 px above
   its picker (< `s1`).
5. **No authored-pixel exemption.** Fixed offsets that produced 1–4 were
   replaced by token expressions (`row_gap`, `RAIL_ROW_GAP`, `CHIP_GAP`, sheet
   height, toggle inset). Kept literals are listed under "Limits".
6. **Visual evidence.** Before/after PNGs, dark and light, for every touched
   surface (tables below), plus the geometry lint on every inspection.

## Surface tables (PNG before → after, dark and light)

### git_client (`.scenarios(&["changes","split","history","branches","commit","discard","empty"])` added; `--shots-narrow` now uses `ui::launch::shot_theme`)

| surface | before → after | defects fixed |
|---|---|---|
| Changes screen (`before/*/default.png` → `after/*/changes.png`) | dark: app bar and list column had invisible `border_standard` rules; tab group / Unified-Split / Syntax tracks were bare base fills on the raised bar; hunk header bands were bare raised fills; +/- chevrons and hunk buttons bare `lift(0.72)`; count badges bare; rows touched (30 px pitch). After: derived rules on bar, list column, diff header; tab/segment tracks are `draw::card`; hunk bands closed top and bottom by `edge_on(raised)`; chevrons `draw::card`; badges `PillEdge::Derived`; rows 34 px pitch (`s1` gap); Commit / hunk buttons via `ButtonFace`. Light: same edges register; no regression. | 1, 2, 3, 4 |
| Split layout (`after/*/split.png`) | new scenario; side-by-side hunk bands edged (the first capture showed Unified because the doc sync folded the preference back — fixed by pinning `settings_forced`). | 2 |
| History (`after/*/history.png`) | detail panel left rule and meta-grid rule derived (were `border_standard` on raised); commit rows 72 px pitch with the separator centred in the `s1` gap. | 1, 2 |
| Branches & Remotes (`after/*/branches.png`) | header rule derived; cards `draw::card` (was raised + `border_standard`); count pills derived edge; Fetch/Pull/Push via `ButtonFace`; checkout row no longer overlaps the delete ✕. | 1, 2, 4 |
| Commit / Discard (`after/*/commit.png`, `discard.png`) | new scenarios: enabled primary Commit, armed `danger()` discard face. | 4 |
| Empty (`after/*/empty.png`) | new scenario: disabled Open via `ButtonFace` (one opaque disabled face with an edge). | 2 |
| Narrow flap (`before/*/narrow.png` → `after/*/narrow.png`) | light theme now honoured by the custom renderer (`ui::launch::shot_theme`). | 6 |

### containers

| surface | before → after | defects fixed |
|---|---|---|
| Containers list + detail (`default.png`) | dark: rail edge, detail panel edge, toolbar and tab-strip dividers were `border_standard` on raised (invisible); status pills a bare 38-alpha tint; log console a bare `neutral[0]` fill. After: `draw::edge(raised)` rules, `draw::separator` inside raised panels, pills/chips edged in their own hue (`TINT_EDGE_ALPHA`), console `surfaces.base` + `draw::card`. Light: unchanged look with the same edges. | 2 |
| Confirm dialog (`confirm.png`), New volume prompt (`prompt.png`) | toolkit `ModalDialog`/`ModalPrompt` — unchanged (Open — needs F2). | — |
| Responsive matrix (`dev.qos.containers-phone.png` …) | log console edged on the compact drill-in. | 2 |
| Rail items | 6 px → `s2` gap (`layout` in `draw_rail_sections`). | 3 |
| Images / volumes / distrobox / create / pull (code paths, no shot flag) | banners `draw::card`; stat cards `draw::card` (was base + `border_standard`); row separators `draw::separator(.., base)`; distrobox cards `draw::card` + focus ring on hover only; create/pull modals `panel_ground` + `panel_border`; form inputs `draw::card` at rest; combobox popover `panel_ground` + `panel_border` (was `neutral[2]`); usage bar track `lift(1.0)` (was `neutral[3]`). | 2 |

### diffmerge

| surface | before → after | defects fixed |
|---|---|---|
| Diff (`diff.png`) | toolbar bottom rule and pane-header rules were `border_standard` on raised (invisible dark); after: `edge_on(raised)`. Light unchanged. | 2 |
| Merge (`merge.png`, `status.png`) | source-band column dividers and header bands edged (`edge_on(raised)`); selected chip ink `on_surface(col).ink` (was `from_rgb8(10,14,22)`); chip gap `s2`. | 2, 3 |
| Empty / error (`empty.png`, `error.png`) | same bar/header rules. | 2 |
| History preview (`framework_history.rs`) | raised panel now closed by `edge_on(bg_primary)`; insets `s4`; measured gutter. Not shot-reachable (history mode). | 2, 3, 4 |

### code_editor

| surface | before → after | defects fixed |
|---|---|---|
| Default / sidebar (`default.png`, `sidebar-open.png`) | sidebar↔editor rule and sidebar-toggle divider derived; tree labels `text_dim` (was `neutral[8]`). | 2 |
| Bottom panel (`search.png`, `status.png`, `indexing.png`) | top rule and tab-strip baseline derived (were `border_standard` on raised); problem messages `text_primary`. | 2 |
| Popups (completion / hover in `editor_view.rs`, find options menu in `find.rs`) | `panel_border` on `edge_outline` (was `border_standard` on overlay). Status chip `ui::surface::card`. | 2 |
| Conflict (`conflict.png`) | captured on the GPU path (see Limits); dialog already `hairline_on(raised, 51)`. | — |
| Toggle (`Show bottom panel`) | inset `s1`: no longer touches the editor (lint 1 → 0). | 1 |

### api_tester

| surface | before → after | defects fixed |
|---|---|---|
| Default (`default.png`) | rail rule, URL-bar rule, response header/body rules, kv-row separators, placeholder checkbox outline were `border_standard` (alpha-faded) on raised/base — invisible dark. After: `edge_on(raised|base)`; URL field `ui::surface::card` at rest; response body `surfaces.base` (was `neutral[1]`). Light unchanged. | 2 |
| Empty / error / focus-url / rail-open(-error) (`empty.png`, `error.png`, `focus-url.png`, `rail-open.png`, `rail-open-error.png`) | same rules; focus ring kept. | 2 |
| Auth / body chips and fields (`draw_auth`, `draw_body`) | quiet chips `card(lift(0.35))`; auth fields `card(raised)`; body field `card(raised)` at rest (was `neutral[1]` + `border_standard`); active chip ink `on_surface(accent).ink`. | 2 |
| Rail rhythm | `RAIL_ROW_GAP` (`s2`) for every stacked control. | 3 |

### db_client (`--connect`, `--confirm` shot flags added)

| surface | before → after | defects fixed |
|---|---|---|
| Default / status (`default.png`, `status.png`) | sidebar rule, tab-bar rule, sidebar toggle, Snippets button, grid header underline `border_standard` → derived; Run hint `on_surface(run_color).ink_dim` (was literal white). | 2 |
| Connect sheet (`connect.png`, new) | `panel_ground` + `panel_border` (was `bg_primary` + `border_standard`); segments and inputs `card` at rest; sheet height from content (was fixed 512 with dead space); scrim `surfaces.scrim`. | 2, 3 |
| Destructive confirmation (`confirm.png`, new) | Panel role; Cancel/Run via `ButtonFace`; scrim token. | 2, 4 |
| Snippets menu | `panel_ground` + `panel_border`. | 2 |
| Type-icon chips (`structure.rs`, `schema_tree.rs`) | glyph ink `on_surface(chip).ink` (was `from_rgb8(20,20,20)`). | 2 |

### browser (custom shot renderer)

| surface | before → after | defects fixed |
|---|---|---|
| Light theme proof (`after/light/settings.png`, `library-*.png`, `default.png`) | `shot_theme` already honoured `QOS_SHOTS_THEME`; it now also honours `QOS_SHOTS_TEXT_SCALE` through `ui::launch::shot_text_scale` (same precedence as every other app). | 6 |
| Tab strip overflow paddles (`tabs.png`) | bare raised fills → `ui::surface::card`. | 2 |
| Fullscreen hint (`browser_tabs.rs`) | `from_rgba8(16,16,20,242)` + `Color::WHITE` → `Container::with_background()` + themed text (the pre-existing app_and_shell ratchet failure on these two literals). | 2 |
| Toolbar / library / settings | toolkit Buttons, Cards, TextInputs — Open — needs F1/F2. | — |

## Pair table (gap px before → after, from inspection JSON or layout constants)

| surface | element A | element B | axis | before → after | step |
|---|---|---|---|---|---|
| git_client changes | file row | next file row | y | 0 → 4 | `s1` |
| git_client changes | "Unstage all" header action | first file row | y | 0 → 4 | `s1` |
| git_client changes | row select region | stage/unstage chevron | x | 2 → 4 | `s1` |
| git_client changes | last row of a section | next section header | y | 8 → 8 | `s2` |
| git_client changes | Changes / History / Branches pills | each other | x | 6 → 6 | inside one selector track |
| git_client changes | Diff-layout track | Syntax toggle | x | 12 → 12 | `s3` |
| git_client history | commit row | next commit row | y | 0 → 4 | `s1` |
| git_client branches | checkout row | delete ✕ | x | −4 (overlap) → 8 | `s2` |
| containers | rail item | rail item | y | 6 → 8 | `s2` |
| containers | list row card | list row card | y | 8 → 8 | `s2` |
| containers | collapse chevron | first rail item | y | 12 → 12 | `s3` |
| api_tester rail | env picker | New collection | y | 8 → 8 | `s2` |
| api_tester rail | Save request | Collection actions | y | 4 → 8 | `s2` |
| api_tester rail | disclosure | disclosure | y | 4 → 8 | `s2` |
| api_tester rail | action row | action row (open group) | y | 4 → 4 | `s1` |
| api_tester rail | "ENV" caption | environment picker | y | 2.75 → 4 | `s1` (caption ↔ its control) |
| api_tester rail | "COLLECTIONS" caption | first nav row | y | 11.5 → 4 | `s1` (caption ↔ its rows; rows carry their own 7 px leading) |
| api_tester bar | GET / URL / Send | each other | x | 8 → 8 | `s2` |
| code_editor | Show bottom panel | editor | y | 0 → 4 | `s1` |
| diffmerge merge | Ours / Theirs / Both chips | each other | x | 6 → 8 | `s2` (`theme.tokens.spacing.s2`; compact `s1`) |
| db_client connect | last field | Cancel / Save row | y | 148 (fixed panel) → 24 | `s6` |
| db_client connect | Cancel | Save | x | 10 → 10 | unchanged (see Limits) |

## Overlap lint (`usability.geometry_audit`, touching_peers + semantic_bounds_overlap_unresolved)

| app / state | before | after |
|---|---|---|
| git_client changes / split / commit / discard (sample) | not capturable (no scenarios); rows touched by construction (0 px pitch gap) | 0 / 0 (dark and light) |
| git_client history | — | 0 / 0 |
| git_client branches | — | 2 overlaps (checkout row ∩ delete) → 0 |
| git_client empty | — | 0 |
| git_client default (live repository = this dirty worktree) | 0 (clean tree, 6 controls) | 4 overlaps of scrolled-off rows under the commit box → 0 after culling (non-deterministic state; scenarios are the evidence) |
| containers default / confirm / prompt | 0 | 0 |
| api_tester default | 0 (2 other: unlabeled ToggleGroup, 10 px divider target — pre-existing) | 0 (same 2 other) |
| db_client default | 0 | 0 |
| code_editor default | 1 touching (toggle ↔ editor) | 0 |
| diffmerge merge / status | 0 (12 `target_size_24` pre-existing) | 0 |
| diffmerge diff / error / empty | 22 / 1 `semantic_bounds_overlap_unresolved` | unchanged — duplicate semantic projections (widget node and parent-projected child of the same `Canvas`), not painted overlaps; see Limits |

## Ratchet (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

`design_token_audit`: 7 passed, 1 failed — the failure is the pre-existing
`app_and_shell_theme_escape_hatches_do_not_grow` on **`shell/src/shell/frame.rs`**
only (`[literal_color]: 1, baseline 0`; `[white_or_black]: 2, baseline 0`), a shell
file outside this unit. The other half of that pre-existing failure,
`apps/browser/src/browser_tabs.rs` (one `from_rgba8` literal and one `Color::WHITE`),
is fixed by this unit and no longer reported. No `grew` line names any A1 file.
`declared_borders_in_chrome_do_not_grow`, `theme_escape_hatches_do_not_grow`,
`no_hardcoded_style_literals_in_tree`, `product_apps_use_the_shared_responsive_launcher`
pass.

`f1_spacing_token_audit::spacing_literals_do_not_grow`: no `grew` line; one **stale**
line for an owned file, expected and pasted for the coordinator's re-baseline:

```
apps/diffmerge/src/merge_view.rs [pixel_const]: now 13, baseline still 15 — lower it to 13
```
(`CHIP_GAP` / `CHIP_GAP_COMPACT` removed; the gaps now read `theme.tokens.spacing.{s2,s1}`.)
An earlier iteration of this unit added a `RAIL_ROW_GAP` module constant to
`apps/api_tester/src/app.rs` (`grew` 14 → 15); it was replaced by the theme-fed
`rail_row_gap` / `rail_group_gap` fields, so that line is gone.

## Tests

All with the inline env and `--no-default-features --locked`. `cargo build -p <app>` for
all seven apps: green (only the two pre-existing `ui` warnings). `cargo check -p <app>
--tests` for all seven: green.

| crate | `cargo test -p` | notes |
|---|---|---|
| git_client | lib 44 passed (+2 new: `file_rows_keep_a_token_gap_on_every_preset`, `commit_rows_keep_a_token_gap_on_every_preset`); `tests/repo_ops.rs` 26 passed | |
| containers | lib 54 passed (+2 new: `card_edges_register_on_every_containers_ground`, `tinted_pills_and_chips_are_edged_in_their_own_hue`); `tests/interaction.rs` 20 passed | |
| diffmerge | lib 66 passed; bin 1 passed | |
| db_client | lib 86 passed; `tests/live_roundtrip.rs` 4 passed | |
| browser | lib 224 passed, 3 ignored; bin 1; `tests/private_launch.rs` 3; `tests/chrome_alignment.rs` 5 passed, 1 failed | `labelled_toolbar_controls_grow_with_the_text_and_the_tiers_follow` ("browser.site.info leaves the row's one control height at 2x") **fails identically with the three A1 browser files swapped for their `fd1f84391` versions** (`target/ux1x/a1/browser_base/result.txt`): pre-existing, toolkit/site_controls height at 2×, not touched here |
| api_tester | lib 74 passed, 2 ignored (+1 new: `rail_controls_share_one_token_gap`); `tests/a11y.rs` 13 passed | `composition_narrow_escape_paging_and_open_resize_are_reachable` failed twice on the way here (the `s2` rhythm cost 20 px at 800×480: first an open action group overflowed the rail, then the nav list lost a row and one Next page no longer reached `POST Login`); fixed in production code, not in the test — the empty COLLECTIONS band yields to the footer (HIG §7a) and the captions sit on measured line boxes — the assertions are untouched |
| code_editor | lib 226 passed, 1 failed (pre-existing), 1 ignored; `tests/language_server.rs` 0 (no tests); `retained_dock_trigger_shares_tab_chrome_and_uses_pointer_and_keyboard_routes` updated: the trigger is asserted `s1` inset from the chrome row's top and `2·s1` shorter than the row (both before and after the resize), justified by `after/dark/default.png` where it no longer touches the editor | `app::tests::a_document_from_a_previous_session_still_stats_the_file` fails at base and after (`/var` vs `/private/var` temp path on macOS) |

## Code review (`code-review`, medium)

Four findings, all fixed before commit: (1) git_client row hit/semantic regions are
now clamped to the list viewport (`Rect::intersect(area)`), so a row straddling the
list/commit-box boundary cannot reach under the pinned commit box; (2) diffmerge
chip gaps read `theme.tokens.spacing.{s2,s1}` at the use site instead of a
hard-coded 8; (3) browser's local `shot_theme` copy is deleted — every custom shot
handler calls `ui::launch::shot_theme(args)`; (4) git_client's now-unused declared
outline helper `draw::stroke_round` is deleted.

## Open — needs other units

- **F1** Button Neutral/Disabled rest edge is still the declared `border_standard`: git_client `after/dark/changes.png` ("Unstage hunk", disabled "Commit 3 files"), `after/dark/empty.png` (disabled "Open"), diffmerge `after/dark/diff.png` (Prev / Next / Load sample / Open files), browser `after/dark/default.png` toolbar, containers `after/dark/confirm.png` (Cancel) — `ui/src/widget/button.rs:520/538`.
- **F1** SearchField rest edge: containers `after/dark/default.png` "Search containers…" — `ui/src/widget/search_field.rs:361`.
- **F2** ModalDialog / ModalPrompt panel border and body ↔ actions rhythm: containers `after/dark/confirm.png`, `after/dark/prompt.png` — `ui/src/widget/modal_dialog.rs:581`.
- **F2** Card rest edge: browser `after/dark/library-history.png` — `ui/src/widget/card_composition.rs:165` / `container.rs:310`.
- **F1** Segmented / ToggleGroup track edge: api_tester `after/dark/default.png` Params/Headers/Auth/Body, Pretty/Raw/Preview — `ui/src/widget/toggle_group.rs:469`.

## Limits

- git_client `--headless-drive` without `--scenario` drives the live root, which reads the real repository — this worktree — so its control set changes with the working tree; the sample scenarios are the deterministic evidence.
- diffmerge `semantic_bounds_overlap_unresolved` (diff / error: 22, empty: 1) come from the same `Canvas` node being walked twice (the widget's own semantics and its parent's projected child) and the hunk buttons living inside it; `target_size_24` on the 20 px hunk rows and on git_client's 14 px hunk buttons / 20×22 chevrons is pre-existing target-size debt, outside rules 1–6.
- code_editor `--scenario conflict` and `--responsive-matrix` panic inside vello's CPU rasteriser (`tile_alloc.rs:52`, bump-buffer overflow) under `QUICK_UI_VELLO_CPU=1`; both were captured on the GPU path instead. The BEFORE `conflict.png` could not be produced (the base binary had been rebuilt before the GPU workaround was found).
- code_editor lib test `app::tests::a_document_from_a_previous_session_still_stats_the_file` fails at base and after (macOS `/var` vs `/private/var` temp-path canonicalisation); untouched.
- Remaining literals (none added): `Color::WHITE` as ink on accent fills (git_client tab / segment / hamburger labels and head pill, containers primary buttons), ref-pill tints via `from_rgba8`, `neutral[5]/[6]/[7]` for hash / punctuation ink, `theme_ext.rs` `Color::BLACK` mix; db_client Cancel↔Save 10 px; many authored layout offsets in api_tester `compute_layout`, db_client `connect_modal.rs` and git_client `app.rs` remain literal where they did not produce 1–4.
- The 7-copy `framework_history.rs` fix is applied to diffmerge only (this unit's copy).

## Files changed

apps/api_tester/src/{app.rs,navrow.rs}; apps/browser/src/{browser_tab_strip.rs,browser_tabs.rs,main.rs};
apps/code_editor/src/{app.rs,bottom_panel.rs,editor_view.rs,file_tree.rs,find.rs};
apps/containers/src/{draw.rs,panels.rs}; apps/db_client/src/{app.rs,connect_modal.rs,grid.rs,lib.rs,main.rs,schema_tree.rs,structure.rs};
apps/diffmerge/src/{app.rs,diff_view.rs,framework_history.rs,merge_view.rs};
apps/git_client/src/{app.rs,changes.rs,diffview.rs,draw.rs,history.rs,lib.rs,main.rs,panels.rs}.
