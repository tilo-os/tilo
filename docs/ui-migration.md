# UI migration guide

How an app moves onto Tilo's unified design system. `docs/design-language.md`
says what Tilo looks like; `crates/sdk/ui-core/HIG.md` says how screens are
built. This guide is the mechanical part: what to delete, what replaces it,
and how to prove the app is done. Where this guide and the design language
disagree, the design language wins.

Apply the checklist to one app per change. Each section after it maps an old
API or hand-drawn pattern to its replacement.

## App migration checklist

Work through every step for the app (`apps/<name>`). Do not touch other apps
or the toolkit; a missing capability is reported, not forked into the app.

1. **Read first.** `docs/design-language.md`, then the sections of this guide
   for every API the app uses.
2. **Roles, not appearance.** Every piece of text gets a `TextRole`
   ([Typography](#typography)); every button a `ButtonRole` (`Primary` for the
   one main action of a view, `Destructive` for anything that deletes or
   revokes, `Secondary` for the rest, `Plain` for inline text actions) and a
   `ControlSize`; every list a list style (`Sidebar`, `Inset`, `Plain`); every
   area of the window a region (`Content`, `Sidebar`, `Inspector`). Remove
   every colour, font size, radius, padding and border width the app passes
   to a widget.
3. **No own ground.** Delete any fill of the whole window or of a whole pane
   (`surfaces.base`, `surfaces.window_card`, `bg_app`, `bg_primary`, a literal
   colour). The framework paints the window ground and each region's tier
   ([Ground & regions](#ground--regions)).
4. **Navigation and selection.** Sidebars, master/detail and drawers come from
   the shared shell and list ([Navigation](#navigation)). A selected row, tab
   or segment is the system lift, never an accent fill and never painted by
   the app.
5. **Tokens for what remains.** Genuine content drawing (charts, documents,
   maps, media, canvases) keeps its painting code but reads every value from
   the theme: `theme.tokens.spacing.s1`–`s16`, `theme.tokens.metrics.*`,
   `theme.tokens.radii.*`, theme colours. Delete module-level pixel and size
   constants.
6. **Icons.** Replace every Unicode glyph and emoji used as an icon (`✕`, `×`,
   `⟳`, `▾`, `🔍`, …) with `ui::icons`.
7. **Copy.** Sentence case everywhere. No all-caps labels, no `A · B · C`
   metadata chains, no taglines or subtitles under headings, no window title
   drawn by the app, no developer notes. A button names what happens
   ("Delete message", not "OK"). An empty state is one sentence and at most
   one action. Demo and fixture content is realistic and complete.
8. **Empty your shards.** Every audit keeps a shard per app at
   `crates/sdk/ui-core/tests/baselines/<audit>/apps-<name>.txt` for `theme`,
   `app-paint`, `spacing`, `typography` and `copy`
   ([Ratchets](#ratchets-what-each-key-asks-you-to-replace) maps every key to
   its fix). The goal is to delete every one of them. An entry may remain
   only for genuine content drawing or for a hit that is not UI (a volume
   label in fixture data, a protocol token), and it keeps its true count.
   Never reshape code so a pattern stops matching (HIG §9.13). Lower the
   shards as described in [Updating shards](#updating-shards).
9. **Shots.** Render the app and look at every screen it has, dark, light
   and narrow:

   ```bash
   cargo run -p <pkg> --bin <bin> -- \
     --shots target/look/<dir>/x.png --viewport 1280x800
   TILO_SHOTS_THEME=light cargo run -p <pkg> --bin <bin> -- \
     --shots target/look/<dir>/x-light.png --viewport 1280x800
   cargo run -p <pkg> --bin <bin> -- \
     --shots target/look/<dir>/x-narrow.png --viewport 800x600
   ```

   When a cargo drop-in renders on a remote host, bring `target/look/<dir>`
   back, and give each app its own `<dir>` so fetches never overwrite each
   other. Open the PNGs and compare them with the design language: one
   ground per region, no accent outside presence and status, sentence-case
   copy, no glyph icons, a sidebar that collapses at 800 px.
10. **Verify once.** `cargo clippy -p <pkg> --all-targets`,
    `cargo test -p <pkg>`, and
    `cargo test -p tilo-ui --test it do_not_grow --success-output immediate`
    (every ratchet passes and prints the shard text to commit).

### Updating shards

A shard lists `<path relative to the repository root> <key> <count>` lines for
one unit (`apps-<name>`, `ui-core` or `shell`). A missing shard allows zero.

- **A remote cargo drop-in** neither forwards the environment nor brings a
  test's writes back. Through it run
  `cargo test -p tilo-ui --test it do_not_grow --success-output immediate`.
  Every ratchet whose tree dropped below its shard prints a `notice:` with
  the stale entries and then either `<shard path> now reads:` followed by the
  complete new shard text, or `delete <shard path>` when the unit reached
  zero. Write exactly that text to the file (or delete it). Do not edit
  other units' shards.
- **cargo** (CI, or a local toolchain):
  `UPDATE_RATCHET_BASELINES=1 RATCHET_SCOPE=apps/<name> cargo test -p tilo-ui --test it do_not_grow`
  rewrites only that app's shards and deletes the ones that reached zero.
  It refuses to raise a count unless `RATCHET_ALLOW_GROWTH=1` is set, and
  an app migration never sets it.

A ratchet test that fails lists each file that grew, the key, and what to use
instead. Fix the code; do not raise the shard.

## Ratchets: what each key asks you to replace

Find the key in the failing test or in the app's shard, then apply the
replacement. "Delete" means the line goes away because a widget or the
framework now does the job.

### `theme` and `app-paint` (colour, ground, shape, paint)

| Key | Matches | Replace with |
|---|---|---|
| `literal_color` | `Color::from_rgb8(12, …)`, `Color::from_rgba8(0x…, …)` | A theme colour. Status: `danger`/`warning`/`success` ink. Presence: the participant's presence colour. User content (album art, calendar colours, photo data) stays and keeps its count. |
| `white_or_black` | `Color::WHITE`, `Color::BLACK` | The theme's ink tiers, the inverted primary label, or the scrim token. |
| `theme_blind_white` | `surface_white_*`, `border_white_*` | `Colors::lift()` for a raised fill, `Colors::hairline_on(ground, …)` for an edge; in app chrome, delete and use the widget. |
| `literal_color_new` | `Color::new([0.2, …])` | Same as `literal_color`. |
| `ramp_slot` | `.neutral[3]`, `accent[5]` | `surfaces.content` / `sidebar` / `inspector` / `raised` / `overlay` and the text tiers. Selection is the lift: delete the fill and use the list or segmented widget. |
| `legacy_alias` | `.bg_app`, `.bg_primary`, `.bg_hover`, `.bg_active` | Ground fills: delete. Hover and active states: delete; the widget paints them. |
| `desktop_ground` | `surfaces.base`, `surfaces.window_card` | Delete the fill and declare the region; `surfaces.base` is the desktop behind tiles. |
| `literal_radius` | `.to_rounded_rect(8.0)`, `RoundedRect::from_rect(r, 6.0)`, `fill_round(s, r, 4.0, …)`, `.radius(12.0)` | Controls and rows: delete; the widget's role gives the radius (control 8, item 6, chip full). Content drawing: `theme.tokens.radii.*`, concentric (inner = outer − inset, minimum 4). |
| `literal_alpha` | `with_alpha(0.5)`, `with_alpha_factor(x * 0.4)`, `multiply_alpha(0.6)` | The theme's state, scrim or material token; hover/press washes come from the widget. |
| `raw_paint` | `scene.fill(`, `scene.stroke(` | Chrome (bars, rows, buttons, chips, cards, separators, backgrounds): compose `ui` widgets and delete the painter. Content drawing (plots, documents, maps, canvases, media) stays, reading theme values. |
| `glyph_icon` | a string or char literal with `✕ × ⟳ ↻ ⌫ ⇅ « » ‹ › ▾ ▸ ▴ ◍ ◎ ◷ ⚓ ➤ ⏰` or an emoji | The matching `ui::icons` icon (an icon-only button carries an accessible label). A `×` that is real text ("1280×800") stays. |
| `transparent_shot` | `render_png(…, …TRANSPARENT…)` | Render shots through the framework so they land on the window ground (see [Window chrome & shots](#window-chrome--shots)). |

### `spacing`

| Key | Matches | Replace with |
|---|---|---|
| `pixel_const` | `const PAD: f64 = 12.0;` | Delete the constant. Gaps and insets: `theme.tokens.spacing.sN` (4 → `s1`, 8 → `s2`, 12 → `s3`, 16 → `s4`, 24 → `s6`, 32 → `s8`, 48 → `s12`, 64 → `s16`; round to the nearest step). Control, bar and row heights, icon sizes, panel widths: `theme.tokens.metrics.*`, or delete when the widget sizes itself. |
| `taffy_length` | `length(12.0)` in a `Style` | `insets_all(spacing.sN)` / `insets_xy(h, v)` or `length(spacing.sN)`. |
| `literal_gap` | `.with_gap(12.0)`, `.with_padding(16)`, `.with_row_gap(…)`, `.with_column_gap(…)` | `.with_gap(spacing.sN)` etc., or delete it when the composition (form, section, list) already spaces its children. |
| `literal_spacer` | `Spacer::fixed(12.0)` | Delete it when the gap belongs to the container; otherwise `Spacer::fixed(spacing.sN)`. |

### `typography`

| Key | Matches | Replace with |
|---|---|---|
| `literal_font_size` | `TextStyle::new(13.0)`, `.with_size(20.0)`, `.text_size(14.0)`, `.with_font_size(…)`, `.set_font_size(…)` | A `TextRole` ([Typography](#typography)). By size: 34 → `Display`, 26–28 → `LargeTitle`, 20–22 → `Title`, 16–18 → `Heading`, 14–15 semibold → `Subheading`, 14 → `Body`, 13 → `Label` (`Code` for monospace), 12 and below → `Caption`; long-form reading → `Reading`. Bold or semibold emphasis inside a role → `.strong()`, tabular figures → `.numeric()`. |
| `legacy_size_api` | `.draw_text(`, `.draw_text_weighted(`, `.measure(`, `.measure_weighted(`, `.measure_styled(`, `.elide_to_width(`, `.elide_to_width_weighted(` | A `Text` widget, or the role-based measure/draw/elide calls ([Typography](#typography)). |
| `font_size_const` | `const TITLE_FONT: f32 = 15.0;` | Delete the constant; name the role where the text is made. |
| `type_step_arithmetic` | `typography.scale.sm - 1.0` | The role whose size fits; never a size between roles. |
| `raw_type_step` | `typography.scale.*`, `TypeScale::default()` | A `TextRole`. |
| `renderer_scale_helper` | `renderer_input_style`, `renderer_type_size` | Delete; a role is scaled once, by the theme. |

### `copy`

| Key | Matches | Replace with |
|---|---|---|
| `all_caps_label` | a displayed literal opening with an all-caps word (`"COLLECTIONS"`, `"REC  {}"`) | Sentence case (`"Collections"`). A section header uses the `Subheading` role (`Heading` for a top-level section). Acronyms (`PDF`, `HTTP`, `GET`) are not counted. A hit that is data rather than UI (a volume label in fixtures) stays. |
| `middle_dot_join` | a literal containing ` · ` (`format!("{a} · {b}")`, `.join(" · ")`) | The first fact as the row title and the rest as a `Caption` secondary line; a `PropertyList` for label/value facts; table columns for repeated facts. |
| `to_uppercase` | `.to_uppercase()` | Displayed text: delete the call and write the source string in sentence case. Comparisons: `eq_ignore_ascii_case` or `to_lowercase`. Content whose form is upper case (hex digits, a licence plate) stays. |

### `chrome-border` (toolkit and shell only)

`declared_border` counts `.border_standard`, `.border_strong`,
`overlay_border_strong` and `colors.neutral[` in toolkit and shell chrome.
Derive the edge from the ground: `tilo_ui::surface::{edge_on, card,
separator}` or `tilo_ui::chrome::{panel_border, chrome_hairline,
selection_tier}` (HIG §9.5).

## Typography

## Ground & regions

## Navigation

## Controls

## Content widgets

## Search

## Window chrome & shots
