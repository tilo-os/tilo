# F4 — Popups, menus and overlays: round two

Branch `ui-review/r2-f4-popups-menus-overlays`, base `381eff6a8` (the merged
round-one tip, "Merge ui-review/integration"). Reviewed 2026-09-17 in the worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-a936bd119cb18d910`; every capture path
below is relative to its `target/ui-review/`. Round one's report is
[`../2026-09-16-ui-alignment-review/f4-popups-menus-overlays.md`](../2026-09-16-ui-alignment-review/f4-popups-menus-overlays.md);
its ids (`F4-nn`) are referenced, this round's are `F4r2-nn`.

Files in scope: `ui/src/widget/{context_menu,dropdown,autocomplete,slash_menu,
link_popover,command_palette,page_palette,popover,popover_widget,tooltip,toast,
modal_dialog,modal_frame,modal_prompt,permission_prompt,floating_window,file_picker}.rs`,
`ui/src/overlay/`, `ui/src/action_palette.rs`, `ui/src/status_bubble.rs`,
`ui/src/reader_hud.rs`. Changed: thirteen of those source files plus
`ui/tests/design_token_baseline.txt`; `ui/src/overlay/` is untouched (see F4r2-15). Not touched: `ui/src/history_mode/*` (user WIP), the text-size
double-scaling and text-centring expressions (unit X1) — except where the brief handed
one to F4 by name (the `ContextMenu::open_at` paint path).

## 1. What the captures showed before any change

Method: the six app `--shots` scenarios (`file-explorer context-menu|menu`, `monitor
context-menu`, `music context`, `feeds context-menu`, `notes menu|slash`, Settings
wide) at 1200×800 in dark and light, at 100 % and `--text-scale 2`, each in an isolated
profile (`target/ui-review/capture.sh`); the `ui-shots` fixtures `ctrl_k_palette`,
`floating_surface_shots` (dark + light) and `widget_showcase_snapshot`
(`target/ui-review/capture-ui.sh`). Every PNG was opened.

- **Point-placed (`open_at`) context menus are broken at 200 % text.** `file-explorer`'s
  right-click menu and its toolbar `…` menu paint their labels at **4×** — the theme's
  already text-scaled `scale.sm` sent through the renderer's own multiplier — into fixed
  28 px rows, so seven labels overprint into an unreadable block
  (`file-explorer/before-dark-2x/context-menu.png`, `before-light-2x/menu.png`). The
  anchored path (`open_anchored`, used by monitor, music, feeds, db_client, browser
  tabs) measures its rows with `renderer_input_style` and is correct at 2×
  (`music/before-dark-2x/context.png`, `feeds/before-dark-2x/context-menu.png`).
- The Notes `/` block menu (`SlashMenu`) has the same 4×-into-28 px failure
  (`notes/before-dark-2x/slash.png`). Its size expression is X1's; recorded below.
- At 100 % every menu in both themes is laid out cleanly; the round-one accent
  highlight is present wherever a row is armed (`f4_menu_highlight_render`, 9/9).
- `floating_surface_shots`: the two floating windows carry a 38 px title bar next to a
  toolkit whose every other bar is 40 (`ui/floating_surface_shots/before-*/floating.png`).
- Ctrl+K palette and showcase: unchanged from round one's observations
  (`ui/ctrl_k_palette/before/*.png`, `ui/widget_showcase_snapshot/before/*.png`).

## 2. Findings

"Fixed" means the change is on this branch with a test; every default metric that
moved is called out as such. Paths are `file:line` at the branch tip.

| id | surface | defect | viewport / theme | file:line | before / after | status |
|---|---|---|---|---|---|---|
| F4r2-01 (was F4-15) | `FloatingWindow` title bar | 38 px literal under the same name as the dialog's 48 | `floating_surface_shots`, both | `floating_window.rs:42,392,461` | `ui/floating_surface_shots/before-{dark,light}/floating.png` / `after-*` | **fixed — default moved 38 → 40**: the bar is a Bar (HIG §9.1/§9.6) and reads `metrics.bar_height` from the layout theme (`title_bar_height()`), so it follows `ui_scale`; `TITLE_BAR_HEIGHT` is now `Metrics::BASE.bar_height as f64` for `const` callers. The close control is a `bar_control_height` square centred in the bar with equal trailing/vertical inset (28 px at x1−34…x1−6 as before; y shifts by 1). The `overlay_chrome` (PiP) close and title chips are painted from `close_rect()` (less a hairline) and share its vertical band, so the tinted chip and the click target agree at every scale (they differed by 1–2 px before). Test `title_bar_is_the_bar_height_token_and_scales_with_ui_scale` |
| F4r2-02 (was F4-15) | `ModalDialog` title bar / footer | 48 / 56 / 40 literals | source | `modal_dialog.rs:43-80,171-190` | — (no shipped fixture) | **fixed — defaults unchanged at 1×**: `DialogChrome::from_theme` = heading token `scale.lg` + 2·`s4` (48, grows with `text_scale` so the bar is sized for the heading it hosts), footer `metrics.bar_height` + 2·`s2` (56), button `metrics.bar_height` (40), button inset `s4` / gap `s2`; cached because builder methods carry no theme. Tests `chrome_heights_follow_the_theme`, `content_starts_below_the_title_bar_at_a_large_text_scale`. Rationale for two roles rather than one token: the dialog bar hosts a *heading* (type-driven), the floating bar is *chrome* (density-driven). **Caveat (X1):** the heading is still painted with `draw_text(scale.lg)`, which the renderer scales again, so at 200 % the glyphs are 4× and overflow the bar sized for them — the size expression is X1's; once it paints once, the bar fits |
| F4r2-03 | `ModalDialog` content layout | `apply_layout` laid the body out with `Theme::dark()` and a fresh 1× `TextRenderer` (the `NavSplit` class of defect, README systemic #3) | any non-dark or scaled theme | `modal_dialog.rs:139,517-522` | — | fixed — `layout_theme` retained from `build_layout`/`layout_in`, renderer set to its `text_scale` |
| F4r2-04 | `ModalDialog` footer buttons | hand-rolled button painter (`draw_action_button`: fill + stroke + `Color::WHITE` ink, no hover) — HIG §8 "never hand-roll a control" | all | `modal_dialog.rs:592-630` | — | fixed — `ButtonFace` + `Button::paint` (Primary / Danger / Neutral, Medium); the label size is the theme's `scale.sm` with the renderer's multiplier taken back out (`renderer_input_style`), so it carries `ui_scale` and a custom type scale like every other `Button` label; hover from `last_cursor` gated by a `pointer_inside` flag that `CursorLeft` and (re)opening clear. Test `primary_action_paints_hover_when_the_pointer_rests_on_it` (hover ≠ rest; hover then `CursorLeft` = rest); `destructive_primary_paints_a_different_fill_than_accent` still holds. Radius follows the shared `Button` default (`radii.md`, was `radii.sm`) |
| F4r2-05 | `ModalDialog` backdrop | `Color::BLACK` at 0.7 — a second, heavier modal veil beside `ModalFrame`'s | all | `modal_dialog.rs:562` | — | fixed — `crate::chrome::fill_scrim(.., MODAL_SCRIM_ALPHA, theme)`, the one modal scrim painter `ModalFrame` uses (soft-light over the theme's scrim hue at 0.45), so a retained dialog and a sheet darken the desktop identically; `white_or_black` ratchet for the file 2 → 0 |
| F4r2-06 (was F6-27) | `ModalFrame` chrome scale | scales padding/margins/rows by `text_scale` where spacing follows `ui_scale` | text ≠ 1 | `modal_frame.rs:360` | — | **open — F4+F6 follow-up** (coordinator decision 2026-09-17): the scale factor now lives in one private `chrome_scale(theme)`; flipping it to `f64::from(theme.ui_scale)` is the one-line change. Blocked until `identity_flow/sheet.rs` consumes the new API — its `Sheet::body_width` duplicates the `text_scale` formula and `SheetSpec::modal` divides measured heights by `text_scale`, so `body_width_is_what_the_modal_layout_hands_back` and `card_centres_on_the_viewport_and_body_carries_the_measured_height` (both at `text-1.5`) fail the moment the rule changes alone |
| F4r2-07 (was F6 hand-off) | `ModalFrame` API | card-width math duplicated into `Sheet::body_width`; action height restated | source | `modal_frame.rs:369-390` | — | **API exposed; consumption pending (F6)** — `ModalFrame::card_width(viewport, spec, theme)`, `body_width(..)`, `action_height(theme)`; `layout()` routes through them, so the frame has one formula. `identity_flow/sheet.rs:338` still carries its copy until F6 replaces it with `ModalFrame::body_width(viewport, &ModalSpec::new("").max_width(max_width), theme)` and drops its duplicate test; the coordinator sequenced that after both branches merge. Tests `body_width_is_what_the_layout_hands_back` (dark/light/text-1.5/ui-1.25/small output × two max widths), `action_height_is_the_height_of_every_action_rect` |
| F4r2-08 (was F2-16) | `Dropdown` in a `Row` | `percent(100%)` + `flex_shrink: 0` claims the row; later items pushed past the edge to zero width | any | `dropdown.rs:672-697` | Settings wide dark/light: byte-identical before/after (no row there overflowed) | fixed — `flex_shrink: 1.0` with `min_size.width = PAD_X + CHEVRON_W` (the fluid item gives, like `TextInput`; fixed leaves keep their size); height from `metrics.bar_height` (40, `ui_scale`d). Test `dropdown_in_a_row_yields_width_to_fixed_siblings` (Button · Dropdown · Button in 600 px) |
| F4r2-09 (was F4-02 follow-up) | `Dropdown` closed control hover | hover change set `handled` (starves siblings of the move — `button.rs:1220` rule) and no `CursorLeft` handling, so the veneer could stick | all | `dropdown.rs:503-509,977` | — | fixed — hover is a repaint, not consumed input; `CursorLeft` clears it. Test `hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover` |
| F4r2-10 | `Dropdown` open list | opened below the control regardless of the window bottom — the one popup in the family with no flip/clamp | control near the bottom edge | `dropdown.rs:127,262-278,420-432` | — | **widget fixed; wiring needs the App owner / A3** — `list_rect` flips above when below would leave the viewport and there is room, and hit-testing shares the rect. The viewport arrives two ways: the presence viewport at paint (the route `Toolbar` uses, `toolbar.rs:614`), and a new host API `set_viewport`/`with_viewport`. The presence route only reaches a control drawn directly under the root: `Row`/`Column` clear `presence.viewport` for their children (`row.rs:496`, `column.rs:555`) and the App pops the window viewport before `draw_overlay` (`app.rs:7025`), so Settings' dropdowns (under `Row`/`Column`) still need the host to call `set_viewport`, or the App to keep the window rect pushed through the overlay pass. Test `open_list_flips_above_when_it_would_leave_the_viewport` exercises both routes through public API (`draw` with a presence viewport; `with_viewport`) |
| F4r2-11 | `Dropdown` labels, `Autocomplete` label | measured one line tall (`max_width None`) but drawn with a max width → cosmic-text wraps a long option onto a second line under the row | long options | `dropdown.rs:794,916`, `autocomplete.rs:526` | — | fixed — `elide_to_width`, drawn with `None` |
| F4r2-12 | `ContextMenu::open_at` paint | legacy branch drew `draw_text(scale.sm)` (4× at 200 %) while rows were measured with `renderer_input_style`; rows fixed at 28 | `file-explorer context-menu`, `menu`, 2× both themes | `context_menu.rs:169-201,508,531-533` | `file-explorer/before-dark-2x/context-menu.png` / `after-dark-2x/context-menu.png` (also `light-2x`, and `menu.png`) | fixed — one `draw_style` for both modes; point-placed menus keep their origin and never shrink below `MENU_WIDTH`, but rows (and, past 220, width) follow the measured labels. **Default unchanged at 1×** (`file-explorer/{before,after}-{dark,light}/context-menu.png` byte-identical; test `legacy_open_at_geometry_is_unchanged_at_1x_after_paint`). Test `legacy_open_at_rows_grow_with_the_text_scale_and_glyphs_stay_in_their_rows` asserts glyph size = the theme's 2× and every glyph line inside its row |
| F4r2-13 (was F4-16) | `ContextMenu` row geometry | `x0 + 10.0` label inset and `+ 20.0` measured width drift from the `s1` highlight inset under `ui_scale`; separator inset literal 8 | `ui_scale` ≠ 1 | `context_menu.rs:426-436,535` | — | fixed without moving the 1× default: label inset = `spacing.s1` (highlight inset) + `LABEL_GAP` 6 (a named optical gap, 10 at 1×), measured width `+ 2·label_inset`, row `h + s3`, separator inset `s2`. `PADDING 6`, `SEPARATOR_HEIGHT 9`, `MENU_WIDTH 220` and the 1 px vertical highlight inset stay (see §4) |
| F4r2-14 (was F4-21) | `Toast` text block | caption/title/body heights from font sizes `+ 4.0` | all | `toast.rs:727-750` | `ui/widget_showcase_snapshot/{before,after}/showcase.png` (differs only in the spinner, x25–54 y288–338) | fixed — measured line boxes with `spacing.s1` gaps. Test `three_line_text_block_stays_inside_the_card` (64 px card: three distinct baselines, none overlapping, all inside) |
| F4r2-15 (was F4-23) | `overlay::Scrim::dark()` | literal black scrim; the struct has no theme access | all | `overlay/scrim.rs:13` | — | open — diagnosed: `Scrim.color`/`opacity` have **no reader** anywhere in `ui`, `shell` or `apps` (`rg 'scrim\.(color|opacity)'`); the literal is dead data and every painter that darkens the screen already goes through `chrome::fill_scrim`/`surfaces.scrim`. A `Scrim::for_theme` was drafted and dropped on review (an API with no caller); the right fix is to remove the two fields or route the one consumer (`overlay/modal.rs`) through `fill_scrim` when a host starts painting it |
| F4r2-16 (was F4-14 sibling) | `PopoverWidget` shadow | four literal `Color::BLACK` rings | all | `popover_widget.rs:392` | — | fixed — `surface::shadow` with `elevation.e4` (Panel); ratchet 1 → 0 |
| F4r2-17 | `Tooltip` chip shadow | two hand-offset translucent copies of the `e2` colour | all | `tooltip.rs:97` | `tooltip_render_test` PNGs | fixed — `surface::shadow(e2)`; `channel_byte` helper removed |
| F4r2-18 | `SlashMenu`, `LinkPopover`, `PageJumpPalette` filter box | `accent[5]` literals (round one moved the palette's row to `ACCENT_PRIMARY_IDX`, three remained) | source | `slash_menu.rs:526`, `link_popover.rs:402`, `page_palette.rs:341,581` | — | fixed (identical value) |
| F4r2-19 | `FilePicker` header / rows / footer | literals `4`, `8`, `12`, `28`, `24` where `s1`, `s2`, `s3`, `bar_control_height`, `icon_lg` are meant; breadcrumb hit rects estimated as `8 + chars·7.5 + 12` and never measured | all | `file_picker.rs:48-100,776-812` | `file_picker_render_test` PNGs | fixed — `PickerMetrics` resolved from the theme in `build_layout`/`draw` and cached (the layout helper also runs from event handlers with no theme); layout **and** draw read the cache, so they agree at every `ui_scale`; `measure_crumbs` records each crumb label's measured width once per paint and `compute_crumb_rects` prefers it to the per-glyph estimate, so a wheel or `set_rect` relayout no longer reverts the hit rects to estimates |
| F4r2-20 | `FilePicker` up button | 28×24 target, below the 32 px minimum (HIG §9.6) | all | `file_picker.rs:70-73` | — | open — recorded as a default-metric change; header has room for `min_pointer_target` |
| F4r2-21 | ratchet | `ui/tests/design_token_baseline.txt` carried seven entries above the tree (three of this unit's files plus `popover`, `channel_browser`, `video_player`), so `theme_escape_hatches_do_not_grow` failed at the base | — | `design_token_baseline.txt` | — | fixed — lowered to the tree's counts (the test's own instruction) |
| F4r2-22 | `SlashMenu` (Notes `/`) at 200 % | 4× glyphs into fixed 28 px rows | `notes slash`, 2× | `slash_menu.rs:531,557` | `notes/before-dark-2x/slash.png` | **open — needs X1** (size expression), then F4 for the row height (measure as `ContextMenu` does) |
| F4r2-23 | `file-explorer` toolbar `…` menu | point-placed menu opened at the right edge runs off screen; `ContextMenu::constrain_to(viewport)` exists for exactly this and is not called | `file-explorer menu`, both, worse at 2× | app (`apps/file-explorer`) | `file-explorer/after-light-2x/menu.png` | open — **needs A9** |
| F4r2-24 (was S1-22) | Ctrl+K / palette rows at 200 % | "fixed heights overlap" | 200 % | `command_palette.rs:760-768` | — | verified not reproducible in the widget: every row constant is multiplied by `self.text_scale` (set in `build_layout` and `draw`), the paint path forces the renderer to 1× and paints theme-scaled sizes once, and the existing test at `command_palette.rs:2520` asserts glyph lines inside their rows at 2×. S1's capture predates the merge or comes from a host that never calls `build_layout`/`draw` with the scaled theme; re-check on the shell (`ux-bench`) — the `ui-shots` `ctrl_k_palette` fixture has no text-scale knob |
| F4r2-25 (was F4-24) | Ctrl+K palette gutters | icon-less "Actions" rows start at x 40, icon rows at x 70 | `ctrl_k_palette` dark | `action_palette.rs` | `ui/ctrl_k_palette/{before,after}/ctrlk-dark-browse.png` (identical) | open — per-group by design; F3 if a shared gutter is wanted |
| F4r2-26 | `Autocomplete` icon→label gap | `ICON_PX + 10.0` where `metrics.icon_label_gap` is 6 | source | `autocomplete.rs:504` | — | open (default metric) |
| F4r2-27 | `Dropdown` chevron, `SlashMenu`/`LinkPopover` panel radius 6 + inner inset 6, `PageJumpPalette` `12·scale` radius, `Tooltip` chip box from font size, `ReaderHud` 13 px literal labels, `StatusBubble` `pad_x 14` | round one's F4-17/18/19/20 family | source | see round one | — | open (default metrics; `reader_hud.rs` carries "pixel-snapshot-sensitive layout math that must not change") |

Fresh eyes at 2× (task item 3), checked and not defects: anchored menu row height and
label inset scale with the text (monitor, music, feeds at 2×, `monitor/…-2x` menu region
x70–330 y370–530 byte-identical before/after); `ContextMenu::layout_at_position` flips
above and clamps to the viewport, `Tooltip::chip_rect` flips and clamps with
`CHIP_MARGIN`, `Popover::place` flips/clamps, and `Dropdown` now flips (F4r2-10);
`PopoverWidget`'s arrow (`ARROW_SIZE_PX`) is centred on the anchor; the toast stack
(`ToastStack::draw`) stacks 48 px cards at `TOAST_GAP` with the shared painter. The
`Tooltip` widget has no arrow (the brief's "tooltip arrow" is `PopoverWidget`'s).

## 3. Round-one dark-highlight fix — re-verified after the merge

- `cargo test -p ui --test it -- f4_menu_highlight_render`: **9 passed** at the merged
  tip and again after this branch's changes (the `ContextMenu` legacy path is now
  painted through the same `draw_style` the fixture measured).
- App menu scenarios re-captured dark + light (+ 2×): every anchored menu is
  byte-identical before/after; the file-explorer menus at 1× are byte-identical; at 2×
  they now show 2× glyphs in measured rows (§2, F4r2-12).
- The merged `overlay_host` palette band: `shell` is Linux-only from this worktree
  (no `it` target, no headless compositor on macOS), so the shell-side anchoring could
  not be exercised here; the `ui` half it consumes — `CommandPalette` row/strip heights
  and `Dropdown`/`ContextMenu` anchoring — is covered by the suites below.

## 4. Deliberately not changed

- `ContextMenu::PADDING 6` (the popup family's inner pad: `Popover::PAD_Y`, the slash
  and link popovers' inner inset are 6 too), `SEPARATOR_HEIGHT 9`, `MENU_WIDTH 220`
  (legacy-only reference width) and the 1 px vertical highlight inset (a hairline, as in
  `Popover`) — default metrics; changing them is a look change for another round.
- `ModalFrame`'s scale rule (F4r2-06) — coordinated with F6, see above.
- `ModalDialog` button width `110` (`ACTION_BUTTON_WIDTH`, shared with
  `permission_prompt`) — no token; measured widths would be the `ModalFrame` approach.
- Everything X1 owns: `typography.scale.*` passed to `draw_text`/`measure`, and the
  `(h - size) / 2.0` centring expressions. The two lines this branch had to touch to
  reach a token (`modal_dialog.rs:565` `title_y`, `floating_window.rs` title `ty`) only
  rename the height term (`TITLE_BAR_HEIGHT` → `self.chrome.title_bar` /
  `self.title_bar_height()`); flag them for X1's merge.

## 5. Tests

All cargo commands carried the batch prefix
(`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache
SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`,
`--locked --no-default-features`).

| suite | before | after |
|---|---|---|
| `--lib -- context_menu:: dropdown:: autocomplete:: slash_menu:: command_palette:: tooltip:: toast:: modal_dialog:: modal_frame:: modal_prompt:: popover permission_prompt:: floating_window:: file_picker:: link_popover:: page_palette:: action_palette:: status_bubble:: reader_hud:: identity_flow::sheet::` | 317 passed, 1 ignored (pre-existing `#[ignore]` in `file_picker`) | **329 passed**, 1 ignored (12 new tests, listed in §2) |
| `--test it -- f4_ tooltip_render_test file_picker_render_test context_menu_test command_palette_test toast_test overlay_test link_popover_renders tooltip_test block_editor_slash_menu_integration f2_control_row_alignment design_token_audit` | 3 of the 6 `design_token_audit` cases failed at the base (`theme_escape_hatches_do_not_grow` — stale ratchet, 7 entries; `app_and_shell_theme_escape_hatches_do_not_grow`) | **36 passed, 1 failed** — incl. `f4_menu_highlight_render` 9/9, F2's four row-alignment tests (which exercise the `Dropdown` flex change) and `theme_escape_hatches_do_not_grow`; the one failure is the pre-existing `app_and_shell_theme_escape_hatches_do_not_grow` (`apps/browser/src/browser_tabs.rs` + `shell/src/shell/frame.rs`, round one's README #8), outside this unit |
| `cargo check -p ui --tests` | — | clean |
| `rustfmt --check` on the 14 changed source files | — | clean except the six pre-existing hunks in `modal_dialog.rs` (accessibility block and `semantic_footer_…` test, unformatted at the base) |

### Code review (`/code-review high`)

Ten findings, all against the working tree; the fixes are on this branch and each
re-ran the suites above.

1. `Dropdown` flip never saw a viewport in production (`Row`/`Column` clear the presence
   viewport; the App pops it before `draw_overlay`) and the test set the private field →
   `set_viewport`/`with_viewport` host API, test through public routes, App/A3 hand-off
   recorded (F4r2-10).
2. `FilePicker::refine_crumb_rects` undone by the renderer-less `apply_layout_helper` on
   the event path → measured widths cached per label and consumed by
   `compute_crumb_rects` (F4r2-19).
3. `FloatingWindow` overlay-chrome chips still painted from the 38 px literals while
   `close_rect()` scaled → chips derived from `close_rect()` (F4r2-01).
4. `ModalDialog` heading painted at 4× on 200 % text (X1's expression) → the guarantee
   in F4r2-02 restated as a caveat, expression untouched.
5. Footer label at `TypeScale::default().sm` ignored `ui_scale` → `renderer_input_style`
   size (F4r2-04).
6. `refresh_chrome` read footer insets from the previous `layout_theme` → insets moved
   into `DialogChrome`; `adopt_theme` sets the theme first; a chrome change at paint
   re-places the content.
7. Footer hover keyed on a stale `last_cursor` with no `CursorLeft` → `pointer_inside`
   flag cleared by `CursorLeft` and by `set_open` (F4r2-04).
8. Flat `surfaces.scrim` at 0.7 beside `ModalFrame`'s `fill_scrim` at 0.45, plus an
   unused `Scrim::for_theme` → `fill_scrim`; `scrim.rs` reverted (F4r2-05, F4r2-15).
9. `ModalFrame::body_width` had no production caller while `Sheet::body_width` still
   duplicated it → status corrected to "API exposed; consumption pending" (F4r2-07),
   per the coordinator's sequencing.
10. `FilePicker` layout `const`s from `Spacing::BASE` while draw read live tokens →
    `PickerMetrics` cached from the theme, read by both (F4r2-19).

Two candidates the review refuted: the lowered ratchet entries for files this unit did
not edit (the base was stale; the test's own message asks for the lowering), and a
per-frame `Theme` clone in `ModalDialog::draw` (already split out before the review
finished).

## 6. Capture parity

`target/ui-review/pngdiff.py` (byte compare) and `pngbbox.py` (pure-Python PNG decode,
differing-pixel bounding box, region check) over all 32 app pairs and 7 fixture pairs:

- **Byte-identical** (28): file-explorer `context-menu` at 1× (dark, light); music,
  feeds, notes (`menu`, `slash`) and Settings in every theme and scale; Ctrl+K palette
  (three PNGs); showcase scrolled.
- **Intended** (6): file-explorer `context-menu` and `menu` at 2× (F4r2-12);
  `floating_surface_shots` dark/light (F4r2-01, bbox x98–1199 y236–588 — the two
  floating windows).
- **Environmental** (5): monitor at 1× differs only in the live clock (bbox x1109–1173
  y31–43; menu region x70–210 y330–440 identical), monitor at 2× only in the clock band
  (x912–1171 y45–83; menu region identical); showcase top only in the spinner
  (x25–54 y288–338).

## 7. Limits

- macOS worktree: no `shell` headless compositor, `qos-control` or `it` target, so the
  `overlay_host` palette band and the shell's own popups are not exercised here.
- The `ui-shots` fixtures have no text-scale knob (only `floating_surface_shots` reads
  `QOS_SHOTS_THEME`), so the 200 % evidence is app-level (`--text-scale 2`) plus the
  unit tests that assert glyph lines inside rows.
- The file-explorer profile is not fully deterministic (a `Library` row appears when the
  app is launched twice into one profile); the reported pairs were captured once each
  into fresh profiles.
- No shipped fixture renders `ModalDialog`, `PopoverWidget`, `Tooltip`'s shadow or the
  `Dropdown` flip; those are covered by unit tests and `tooltip_render_test` /
  `file_picker_render_test`.
