# F4 — Popups, menus and overlays: alignment, spacing and the dark-theme highlight

Branch `ui-review/f4-popups-menus-overlays`, base `codex/macos-shell` @ `2d7e819b6`
(+ the one-line `Cargo.lock` fix `f9a579c9c` cherry-picked so `--locked` builds).
Reviewed 2026-09-16. Worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-aa63dbebd992f5adc`; every capture path
below is relative to its `target/ui-review/`.

Files in scope: `ui/src/widget/{context_menu,dropdown,autocomplete,slash_menu,
link_popover,command_palette,page_palette,popover,popover_widget,tooltip,toast,
modal_dialog,modal_frame,modal_prompt,permission_prompt,floating_window,file_picker}.rs`,
`ui/src/overlay/`, `ui/src/action_palette.rs`, `ui/src/status_bubble.rs`,
`ui/src/reader_hud.rs`.

## 1. The dark-theme highlight report

> "there is a broader issue that in the dark menu when you have to right click
> something, and a context menu opens or a drop down there is no highlighting."

### 1.1 Reproduction

The app `--shots` scenarios that open menus (`file-explorer --scenario context-menu`,
`monitor --scenario context-menu`, `music --scenario context`, `feeds --scenario
context-menu`, `notes --scenario menu`) all open the menu **without hovering a row**,
so they can neither show nor refute the report — see `<app>/before-dark/*.png` and
`<app>/before-light/*.png` (every menu shows plain rows in both themes).

The reproduction is therefore a rendered fixture, `ui/tests/f4_menu_highlight_render.rs`
(registered in `ui/tests/it.rs`): each menu-like widget is opened for real, the
highlight is moved onto row 1 the way a pointer (`CursorMoved` through
`Widget::handle_event`) or the keyboard (`ArrowDown`) would, the scene is rendered
offscreen over the theme's own window ground, and a text-free pixel on the highlighted
row is compared with the same pixel one row over as a CIE ΔL\* step. The probe rows carry
short labels and row 0 the longest, so a measured (label-hugging) menu never puts glyphs
under the probe. PNGs: `ui/f4-highlight-before/*.png`.

Measured **before** any change (`Theme::dark()` / `Theme::light()`):

| Surface | State | dark: highlighted vs plain → ΔL\* | light → ΔL\* |
|---|---|---|---|
| `ContextMenu` (legacy `open_at`, pointer hover) | armed row | (38,39,41) vs (13,14,17) → **11.6** | (224,224,224) vs (255,255,255) → 10.8 |
| `ContextMenu` (`open_anchored`, ArrowDown ×2) | armed row | (38,39,41) vs (13,14,17) → **11.6** | → 10.8 |
| `Dropdown` open list (pointer) | armed row | accent (88,145,232) vs (31,32,37) → 47.7 | → 52.0 |
| `Dropdown` closed control (pointer resting on it) | hover | **0.0 — not painted at all** | **0.0** |
| `Autocomplete` (pointer hover, not armed) | hover veneer | (54,55,59) vs (31,32,37) → 10.8 | → 10.8 |
| `SlashMenu` (pointer) | armed row | accent → 47.7 | → 52.0 |
| `PageJumpPalette` (ArrowDown) | armed row | accent → 47.7 | → 52.0 |

So the widgets **do** paint something in the default dark theme, and the veneer tokens
have the polarity the token doc promises (white on dark, black on light — every shipped
preset goes through `Colors::themed`, `qos_blue` hand-writes white tiers, and
`high_contrast` re-solves its tiers). The numbers also show where the report comes from:

### 1.2 Root cause

1. **`ContextMenu` was the one menu-like list that painted its armed row with the
   10 % veneer** (`surface_white_10` = `overlay_medium`, `context_menu.rs` old line
   532) while `Popover`, `Dropdown`, `SlashMenu`, `PageJumpPalette` and `LinkPopover`
   paint the same semantic — "the row Enter or a click will activate" — with a solid
   accent fill. On the context menu's ground, `surfaces.overlay` (`#0D0E11` in the
   default dark theme, the darkest surface in the palette), a 10 % white wash composites
   to `(38,39,41)`: ΔL\* 11.6 on paper, but two near-black greys on a dimmed dark-mode
   display — exactly what a user calls "no highlighting". The light theme's same veneer
   (grey on white) reads fine, which is why the report is dark-specific.
2. **The closed `Dropdown` control tracked `is_hovered` but never painted it**
   (`dropdown.rs`: set in `on_cursor_moved`, read nowhere in `draw`). Hovering any of
   Settings' 53 dropdowns gave no feedback until it was clicked, in every theme — the
   "drop down" half of the report.
3. Outside this unit (reported, not fixed here): the shell's own tile context menu
   (`shell/src/shell/mod.rs:183/252/769`, `ctx_menu`) is opened with `open_at` but never
   receives `handle_event` and is never drawn through `ContextMenu::draw`, so no hover
   can exist for it whatever the theme. **Needs S1/S2.**

### 1.3 Fix (shared widgets; geometry, metrics and defaults unchanged)

- `context_menu.rs:529-577`: the armed row is now an `accent[ACCENT_PRIMARY_IDX]` fill
  with `on_accent(ON_FILL_MIN_CONTRAST)` ink — the same fill, ink, inset (`spacing.s1`
  = the existing 4 px) and radius (`radii.sm`) as `Popover`'s highlight. One block,
  commented, so it merges next to the user's uncommitted WIP in this file.
- `dropdown.rs:709`: resting hover paints `overlay_subtle` over the raised fill (never
  while open, focused or disabled).
- `slash_menu.rs:535`, `link_popover.rs:460`: the armed row's ink was `text_primary` on
  the accent — dark-on-blue in every light theme (≈1.9:1). Now `on_accent(...)`; the
  highlight radius literal `4.0` became `radii.sm` (identical value).
- `autocomplete.rs:461`: pointer hover keeps the veneer (it is a *pre*-arm state there,
  the armed row is already accent) but reads the semantic `overlay_medium` instead of
  the legacy `surface_white_10` alias.

### 1.4 After

`ui/f4-highlight-after/*.png` (14 presets + the dropdown control), same probes:

| Surface | dark → ΔL\* | light → ΔL\* |
|---|---|---|
| `ContextMenu` pointer / keyboard | (88,145,232) vs (13,14,17) → **56.1** | (47,111,214) vs (255,255,255) → **52.0** |
| `ContextMenu`, every shipped preset | min **28.6** (`gruvboxlight`), max 73.3 (`rosepine`); ink ≥ 4.5:1 on the painted fill in all 14 | — |
| `Dropdown` closed control, resting → hovered | (31,32,37) → (42,43,48): **5.3** | (255,255,255) → (240,240,240): **5.2** |
| `Dropdown` list, `SlashMenu`, `PageJumpPalette` | 47.7 (unchanged) | 52.0 (unchanged) |
| `Autocomplete` hover veneer | 10.8 (unchanged) | 10.8 (unchanged) |

App scenarios re-captured after the change (`<app>/after-<theme>/`): identical to
`before-*` by construction for the menus (no row is hovered in those scenarios) — the
diff is the default-regression check for the measured-text centring edits below.

## 2. Alignment / spacing findings

Method: source pass over every file in scope (literal inventory, text-centring
pattern, token use), then BEFORE/AFTER captures of the app menu scenarios, the
`ui-shots` fixtures `ctrl_k_palette`, `floating_surface_shots`,
`widget_showcase_snapshot` (`ui/<scenario>/{before,after}-{dark,light}/`), Settings and
Notes defaults, all at 1200×800 in both themes, opened and compared.

"Fixed" below means the internal alignment or token expression changed with the
primitive's default geometry left identical; measured-text fixes follow the
`button.rs:739` pattern (centre on the measured line, never on the font size).

| id | surface | defect | viewport / theme | file:line | before / after | status |
|---|---|---|---|---|---|---|
| F4-01 | `ContextMenu` armed row | veneer highlight invisible on dark popover ground (§1) | fixture, dark | `context_menu.rs:529` | `ui/f4-highlight-before/f4_context_menu_hover_dark.png` / `ui/f4-highlight-after/…` | fixed |
| F4-02 | `Dropdown` closed control | `is_hovered` never painted; no hover feedback in any theme | fixture, both | `dropdown.rs:709` | — / `ui/f4-highlight-after/f4_dropdown_control_hover_{dark,light}.png` | fixed |
| F4-03 | `SlashMenu`, `LinkPopover` armed row | `text_primary` ink on accent fill, ≈1.9:1 in light themes | source, light | `slash_menu.rs:535`, `link_popover.rs:460` | `ui/f4-highlight-before/f4_slash_menu_hover_light.png` / `…after/…` | fixed |
| F4-04 | `Dropdown` list rows + "No matches" | text centred on `FONT` (15) not measured line height | source | `dropdown.rs:825,870` | `ui/f4-highlight-before/f4_dropdown_hover_dark.png` / after | fixed |
| F4-05 | `SlashMenu` rows / hint | centred on `font_size` (old :548) | source | `slash_menu.rs:557` | `f4_slash_menu_hover_*` | fixed |
| F4-06 | `LinkPopover` rows / hint | centred on `font_size` (old :456) | source | `link_popover.rs:469` | — (no fixture; unit tests) | fixed |
| F4-07 | `Autocomplete` label + detail columns | both centred on their font sizes → different baselines | source | `autocomplete.rs:507` | `f4_autocomplete_hover_*` | fixed |
| F4-08 | `PageJumpPalette` rows / "No pages match" | centred on `row_size * scale` | source | `page_palette.rs:369,416` | `f4_page_palette_cursor_*` | fixed |
| F4-09 | `Tooltip` chip label | centred on `text_size` (old :150) inside a font-size-derived box | source | `tooltip.rs:153` | `tooltip_render_test` PNGs | fixed (label only — chip box size kept, see F4-20) |
| F4-10 | `ModalDialog` footer buttons | label centred on `text_size` (old :398) | source | `modal_dialog.rs:398` | — | fixed |
| F4-11 | `PermissionPrompt` buttons | label centred on `text_size` (old :317) | source | `permission_prompt.rs:316` | — | fixed |
| F4-12 | `FloatingWindow` title | centred on `title_size` in a 38 px bar | `floating_surface_shots`, both | `floating_window.rs:890` | `ui/floating_surface_shots/before-*/floating.png` / after | fixed |
| F4-13 | `FilePicker` entry name, filename field, filter chip | centred on `row_size` | source | `file_picker.rs:1349,1464,1520` | `file_picker_render_test` PNGs | fixed |
| F4-14 | `Popover` drop shadow | four literal `rgba(0,0,0,α)` rings, same weight on light ground; HIG §9.3 wants `surface::shadow` + a token | source | `popover.rs:366` | — | fixed → `elevation.e4` |
| F4-15 | `ModalDialog` vs `FloatingWindow` `TITLE_BAR_HEIGHT` | 48 vs 38 under one name | source | `modal_dialog.rs:28`, `floating_window.rs:35` | — | documented, open: they *are* different roles — the dialog bar is `scale.lg` (16) + `s4` (16) padding and hosts a heading; the floating window bar is chrome for a 12/14 px label with a drag grip. Reconciling wants a density token (`chrome.title_bar_*`) — **needs F1** |
| F4-16 | `ContextMenu` label inset `10`, `PADDING 6`, `SEPARATOR_HEIGHT 9`, measured width `+20` | off-scale literals (10, 6, 9 are not on 4/8/12/…) | source | `context_menu.rs:161,399-402,562,581` | — | open: left in place to keep the file merge-clean against the user's WIP; separator inset `8` is `s2`, highlight inset now `s1`. HIG §9 says separators inset `s3` (12) — a default-metric change, **not made** |
| F4-17 | `Dropdown` chevron | literals `18`, `5`, `3`; row highlight is full-bleed while `ContextMenu`/`Popover` inset 4 px | source | `dropdown.rs:768-776` | — | open (default metrics) |
| F4-18 | `SlashMenu` / `LinkPopover` panel | radius `6.0` and inner inset `6.0` are off the radius/spacing scales; `radii.md` (8) or `sm` (4) would change the look | source | `slash_menu.rs:478,497`, `link_popover.rs:356,394` | — | open (default metric) |
| F4-19 | `PageJumpPalette` card | radius `12.0 * scale` = `radii.lg` (Control) where HIG §9.1 wants Panel = `radii.xl`; `row_size = 14.0` literal (= `scale.base`) | source | `page_palette.rs:274,366` | — | open (default metric); the 14 could be `scale.base` verbatim — left, it is commented as intentionally unscaled |
| F4-20 | `Tooltip` chip height | `text_size + 2·CHIP_PAD_Y` — font-size box, so label centring was off by half the ascent/descent gap; box kept, label fixed | source | `tooltip.rs:69` | — | partially fixed (see F4-09) |
| F4-21 | `Toast` text block | caption/title/body heights from font sizes (`+4` gaps), stacked inside a fixed 48 px card | source | `toast.rs:729-734` | `ui/widget_showcase_snapshot` | open — measured heights would move three lines inside a fixed card and its geometry tests pin them; needs its own pass |
| F4-22 | `Toast` action pair | `.text_size(12.0)` = `scale.sm`; layer radius `8.0` = `radii.md` | source | `toast.rs:574,579,587` | — | open (identical-value token expression; left to keep the toast diff empty this round) |
| F4-23 | `overlay::Scrim::dark()` | literal black scrim; `surfaces.scrim` exists | source | `ui/src/overlay/scrim.rs:13` | — | open — the struct has no theme access; the one consumer is `overlay/modal.rs:19`; the shell's actual modal scrim already uses `surfaces.scrim` |
| F4-24 | Ctrl+K palette rows | icon-less "Actions" titles start at x 40, icon rows at x 70 — per-group consistent, cross-group misaligned | `ctrl_k_palette`, dark 560×456 | `action_palette.rs` (`results` list) | `ui/ctrl_k_palette/before-dark/ctrlk-dark-browse.png` | open — by design per group; flag for F3 if a shared gutter is wanted |
| F4-25 | file-explorer context menu | leading separator (first row is a `Separator`), stray 1 px mark right of the menu at ≈(760,431) where the templates submenu anchors with no items | 1200×800, both | app | `file-explorer/before-{dark,light}/context-menu.png` | open — **needs A-unit (file-explorer)** |
| F4-26 | shell tile context menu | opened, never routed events, never drawn | source | `shell/src/shell/mod.rs:183,252,769` | — | open — **needs S1/S2** |
| F4-27 | toolkit | the "centre on measured line" fix is applied per call site (18 here); `rg '\(.* - f64::from\((text_size|font_size|row_size|FONT|size)\)\) / 2.0'` still finds ~40 sites across `ui/src` and `apps` (command_palette, tree_view, date_picker, segmented, breadcrumb, data_table, status_bubble …). A `TextRenderer::line_height(size)` accessor (`TextStyle::line_height_px() * text_scale`, `text_render.rs:627`) or a `draw_text_vcentered(rect, …)` would fix the class once | source | `ui/src/text_render.rs` | — | open — **needs F2 / text-render owner**; raised by the code review of this diff |
| F4-28 | `Autocomplete` status line ("Loading…" / "No matches") | centred on `text_size` | source | `autocomplete.rs:693` | — | fixed (found by the code review) |

Not defects, checked: menus anchor with a consistent inset and clamp to the host bounds
(`ContextMenu::layout_at_position` flips above when below overflows; `Tooltip::chip_rect`
flips and clamps with `CHIP_MARGIN`; `Popover::place` flips/clamps); scrim/modal insets
are symmetric (`MODAL_PAD`, `MODAL_VIEWPORT_MARGIN`); `ModalFrame` title and actions
already centre on measured text and use the shared `Button`; `Popover` rows centre on
`measure_weighted`; `CommandPalette` rows centre icon and text on the row midline and the
selected row is an accent wash + stroke + bar, visible in both themes.

## 3. Tests

Environment note: this worktree lives under `/Users/xaver/dev/qos/`, whose
`.cargo/config.toml` cargo merges by directory walk; its
`cfg(all(target_arch = "aarch64", not(target_os = "linux")))` block injects
`-C target-cpu=generic`, which trips `ring`'s Apple-silicon feature assertion. Every
cargo command here therefore carried
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."` (this tree's own
macOS flags verbatim; `RUSTFLAGS` replaces config rustflags), plus
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 --locked`.

- `cargo check -p ui --tests` — clean.
- `cargo test -p ui --test it f4_menu_highlight_render` — **9 passed, 0 failed**.
  The first version of the file measured the defect (7/7 passing with the ContextMenu
  at ΔL\* 11.6 — i.e. the floor of 4 did not distinguish the old veneer from the fix,
  which the code review of this diff pointed out). The committed version therefore
  asserts armed rows with `assert_armed_row`: the sampled fill must equal the theme's
  `accent[ACCENT_PRIMARY_IDX]` exactly, the ΔL\* step must clear 20 (the old veneer's
  11.6 fails), and the row's `on_accent` ink must reach 4.5:1 against the painted fill —
  across all 14 shipped presets. The ΔL\* ≥ 4 floor remains only for the two veneer
  cases (autocomplete pre-arm hover, closed-dropdown hover at ≥ 3). Luminance and
  contrast come from `ui::a11y`, not a re-implementation.
- Code review (`/code-review`, high then medium): five findings — the test floor above,
  an overstated comment on the autocomplete alias rename (trimmed), the per-site nature
  of the centring fix (→ F4-27), duplicated WCAG maths in the test (→ `ui::a11y`), and a
  leftover `accent[5]` in `page_palette.rs` (→ `ACCENT_PRIMARY_IDX`). All but F4-27
  fixed in this branch.
- Scoped suites, run after the change (`target/ui-review/verify.log`):

| suite | result |
|---|---|
| `--lib context_menu::` | 13 passed, 0 failed |
| `--lib dropdown::` | 26 passed, 0 failed |
| `--lib autocomplete::` | 14 passed, 0 failed |
| `--lib slash_menu::` | 20 passed, 0 failed |
| `--lib command_palette::` | 44 passed, 0 failed |
| `--lib tooltip::` | 9 passed, 0 failed |
| `--lib toast::` | 11 passed, 0 failed |
| `--lib modal_dialog::` | 18 passed, 0 failed |
| `--lib modal_frame::` | 19 passed, 0 failed |
| `--lib modal_prompt::` | 4 passed, 0 failed |
| `--lib popover` | 31 passed, 0 failed |
| `--lib permission_prompt::` | 10 passed, 0 failed |
| `--lib floating_window::` | 20 passed, 0 failed |
| `--lib file_picker::` | 28 passed, 0 failed, 1 ignored (pre-existing `#[ignore]`) |
| `--lib link_popover::` | 13 passed, 0 failed |
| `--lib page_palette::` | 11 passed, 0 failed |
| `--lib action_palette::` | 12 passed, 0 failed |
| `--lib status_bubble::` | 3 passed, 0 failed |
| `--lib reader_hud::` | 6 passed, 0 failed |
| `--test it tooltip_render_test` | 2 passed |
| `--test it file_picker_render_test` | 1 passed |
| `--test it context_menu_test` | 1 passed |
| `--test it command_palette_test` | 2 passed |
| `--test it toast_test` | 2 passed |
| `--test it overlay_test` | 2 passed |
| `--test it link_popover_renders` | 3 passed |
| `--test it tooltip_test` | 2 passed |
| `--test it block_editor_slash_menu_integration` | 3 passed |
| `--test it f4_menu_highlight_render` | **9 passed** |
| `--test it design_token_audit` | 2 passed, **2 failed — pre-existing**, see below |

`design_token_audit` fails at the base commit too; the two failures name files this
branch does not touch (verbatim):

```
design_token_audit::theme_escape_hatches_do_not_grow
  1 file(s) gained a hard-coded colour the Theme should own.
  src/text_render/renderer_regressions.rs [white_or_black]: 5 occurrence(s), baseline 0
design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow
  3 app/shell file(s) gained theme-blind styling. Use shared Theme tokens.
  apps/browser/src/browser_tab_strip.rs [literal_color]: 1 occurrence(s), baseline 0
  shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0
  shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0
```

The other two audit tests (`no_hardcoded_style_literals_in_tree`, and the baseline
comparison for `ui/src`) pass with this diff — the `Popover` shadow change *removes* a
literal-colour occurrence.

Capture parity (byte compare, `target/ui-review/pngdiff.py` for the non-identical
pairs): every app menu scenario, Settings and Notes defaults and the Ctrl+K palette
fixtures are byte-identical before/after in both themes, except: `monitor` — 659 px in
x1108–1173 y31–43, the live clock; `floating_surface_shots` — the two floating-window
titles (F4-12, intended); `widget_showcase_snapshot` — 110/115 px in x25–54 y288–338,
the animated spinner in the "indicators" row.

## 4. Limits

- Widget-level proof only: the report comes from the live macOS shell, which I cannot
  drive from this worktree. The fixture routes pointer/keyboard events through
  `Widget::handle_event` exactly as the App does, but whether the live host forwards
  `CursorMoved` into an app's open context menu is not covered here (file-explorer,
  monitor, music, feeds and notes do forward it in source).
- The user's uncommitted WIP in `context_menu.rs` (+12) and `command_palette.rs` (+186)
  is not in this tree; the context-menu edit is confined to the row-paint block so it
  merges without touching anything else, and `command_palette.rs` was not edited.
- `Popover`'s new shadow is the tokenised `e4` tier, which is a visible change from the
  old ring stack; no shipped fixture renders a `Popover`, so it is covered by its unit
  tests, not a PNG.
- Default metrics were not changed anywhere (F4-15 … F4-22 stay open by that rule).
