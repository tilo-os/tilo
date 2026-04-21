# Nirios Shell — Visual Parity Gaps

Comparing the current implementation (see `target/nirios-screenshots/07-theme-tightened.png`
and `target/nirios-screenshots/06-overview-open-vertical.png`) against
`docs/nirios/concept-features.md`. Prioritized highest-to-lowest visual impact.

## Gap priority legend
- Blocker — makes the shell look fundamentally different from the concept.
- High — clearly visible divergence, user would notice in a side-by-side.
- Medium — off on inspection but not obvious at a glance.
- Low — nit or small polish.

## Gaps

### 1. Content card has no real drop shadow — Blocker

- **What the concept shows:** The white window card uses Tailwind `shadow-2xl`
  (`0 25px 50px -12px rgba(0,0,0,0.25)`) plus the focused variant adds a
  `shadow-[0_0_60px_rgba(99,102,241,0.6)]` bloom (`concept-features.md` §7.1
  "Content card" at `WindowFrame.tsx:354-369`). The white card reads as
  floating above a very dark desktop with a genuinely soft, diffuse fall-off.
- **What we render today:** `quick-ui/src/widget/tile_host.rs:76-98` stacks
  four concentric offset rounded rects at alphas 24/18/10/4. Because Vello has
  no blur primitive, the union is a crisp rectangle halo — visible as a
  stepped grey band under and around the Notes card in
  `07-theme-tightened.png` (right edge ~x=720 has a hard stair-step instead
  of a soft gradient). The shadow reads "dirty / aliased" rather than "lifted".
- **Concrete fix:** Either (a) push these rects through a `GaussianBlur`
  image-filter layer (Vello supports `scene.push_blur_layer` in newer revs),
  or (b) switch to a pre-rendered radial-gradient rounded-rect sprite (one
  per radius bucket) and scale it. Tune `tile_host.rs` fallout curve from
  the current linear geometry to an exponential (`alpha ≈ exp(-d/k)`).
- **Estimated effort:** M

### 2. ~~Active tab has almost no contrast against inactive tabs~~ — Shipped (2026-04-21, commit `4d6df4c`)

- **What the concept shows:** Active + focused pill has `bg-white/10` fill,
  pure `text-white`, and `border-white/20`; inactive pills are `bg-white/5`,
  `text-white/70`, `border-white/10`. With the `shadow-[0_8px_32px_rgba(0,0,0,0.2)]`
  under every pill, the active tab visually "pops forward" on the dark shell
  (`concept-features.md` §7.4).
- **What we render today:** `quick-ui/src/widget/tabs.rs:459-471` picks
  alphas 38 (active) vs 13 (inactive) vs 26 (hover) with a border alpha of
  51/26. In `07-theme-tightened.png`, "Notes" (active) and "Files / Terminal /
  Browser" (inactive) look essentially identical — same pill height, same
  greyish fill, same title colour. The user cannot tell at a glance which
  tab is active; only the missing `×` hover-chip differs, and that's not
  rendered unless the cursor is on the pill.
- **Concrete fix:** In `quick-ui/src/widget/tabs.rs:459`, widen the active
  vs inactive fill delta (e.g. active → alpha ~52 with a subtle 1 px inner
  highlight line; inactive → alpha ~8). Also: apply `text_dim` (not
  `text_primary`) to inactive titles so the title lightness shift is the
  primary cue. Add the per-pill `0 8px 32px rgba(0,0,0,0.20)` shadow
  (currently absent — there is no `shadow` call around tabs), which also
  separates the strip from the card beneath it.
- **Estimated effort:** S
- **Shipped:** commit `4d6df4c` (2026-04-21). Fill alphas now 52 active /
  8 inactive / 26 hover; border alphas 64/18/40; inactive title → `text_dim`;
  per-pill 4-ring shadow + 1 px white inner rim on active. New regression
  test `active_pill_fill_has_stronger_alpha_than_inactive`. Also removed
  prior drift: the previous "active as a near-white card with slate-900
  text" was **not** in the concept — concept is `bg-white/10 text-white`,
  which is what we now render.

### 3. No `backdrop-blur` anywhere — overlays/chrome look like flat rectangles — High

- **What the concept shows:** Top bar uses `bg-black/80 backdrop-blur-xl`
  (§1.1). Collaborator popover and QuickSettings popover use
  `bg-slate-900/80 backdrop-blur-md` (§1.1, §3.1). GlobalSearch and the
  settings modal use `bg-slate-900/70 backdrop-blur-md` with a
  `backdrop-blur-[1px]` scrim over the whole page (§5.1, §4.1). The
  universal feel is "frosted glass over the desktop", not opaque fill.
- **What we render today:** `quick-shell/src/shell/topbar_chrome.rs:271` just
  fills `colors.surfaces.overlay` — a flat dark grey. No convolution, no
  translucency over the gradient behind. In `07-theme-tightened.png` the
  top bar reads as a separate opaque plate, and in `06-overview-open-vertical.png`
  the inactive workspace card underneath "Design Sprint" also looks painted,
  not frosted. `quick-ui/src/tokens/elevation.rs` defines `blur` values but
  nothing consumes them as an actual filter; `Grep "GaussianBlur|blur_layer"`
  in `quick-shell/` and `quick-ui/` returns zero matches.
- **Concrete fix:** Introduce a single helper `push_frosted_layer(scene,
  rect, radius, tint, blur_px)` in `quick-ui/src/widget/mod.rs` that uses
  `scene.push_blur_layer` (now in Vello main) with a fall-back to
  "darker translucent tint of the wallpaper" when unavailable. Call it from
  `topbar_chrome::draw` before the top-bar rect fill, and from every
  popover surface (QuickSettings, collaborators, search, modal). Until the
  blur primitive lands we should at least honour the `/80` vs `/70` alpha
  split so the bar isn't fully opaque.
- **Estimated effort:** L (if Vello blur) / M (translucent fallback first)

### 4. Overview active-row glow is a crisp rectangle "pale halo" — High

- **What the concept shows:** Active workspace card has
  `border-indigo-500 shadow-[0_0_40px_-10px_rgba(99,102,241,0.3)]` (§8.1
  `OverviewView.tsx:60-68`). That shadow spreads gently and softly from the
  card perimeter, fading into the `slate-950` background — no visible edge.
- **What we render today:** `quick-shell/src/shell/overlay_host.rs:1704-1731`
  paints a *single* rounded rect 12 px outside the card filled with
  `indigo-500 @ alpha 0.18`. In `06-overview-open-vertical.png` this is
  directly visible as a pale, slightly bluish-grey rectangle sitting
  *outside* the indigo border of the Home row, with a sharp outer edge —
  the exact effect the user called "pale gray halo outside the ring". It
  reads as a bug rather than an intentional glow.
- **Concrete fix:** Same blur-layer helper as #3, OR replace the single
  fill with four concentric rings at 0.10 / 0.06 / 0.03 / 0.01 alpha and
  `spread` values 6/14/28/48 so the edge fades rather than stops. Keep the
  indigo hue but clamp the near-card ring at a much lower alpha (~0.08)
  so it doesn't compete with the border.
- **Estimated effort:** S

### 5. Icons are kurbo primitives, not lucide glyphs — High

- **What the concept shows:** All icons are lucide-react: the top-bar
  overview grid is `LayoutGrid`, workspace chevrons are `ChevronUp/Down`,
  the workspace name has `Settings2`, QuickSettings trigger shows actual
  `Wifi`/`Monitor`/`Battery` at `w-4 h-4` with a 1.5 px stroke
  (§1.1). Tab close is an `X`; tab icons are app-type lucide icons. Tab
  maximize is `Maximize2`/`Minimize2`. Permissions in overview uses a
  `Shield` glyph (§8.1).
- **What we render today:** `topbar_chrome.rs:289-304` paints the overview
  glyph as four filled rectangles (a 2×2 quad of solid squares), not a
  stroked grid. The "gear" next to the workspace name (`:386-396`) is a
  stroked circle with a filled dot inside — not a gear at all. The tray
  pill shows three filled dots (`:600-603`) instead of Wifi/Monitor/Battery
  glyphs. In overview, `overlay_host.rs:1870-1884` draws the permissions
  shield as an *emerald square* with a code comment acknowledging the stub.
  Tab `×` is a text glyph (works OK) but tab maximize is a drawn square
  outline. Side-by-side, our chrome has a distinctly "geometric toy"
  quality vs the concept's crisp vector-icon shell.
- **Concrete fix:** Embed the lucide SVG paths (MIT-licensed) into a
  `quick_ui::icons` module as `kurbo::BezPath` constants; add
  `draw_icon(scene, transform, rect, Icon::LayoutGrid, color)` taking a 1.5 px
  stroke width. Swap the top-bar grid (`topbar_chrome.rs:289-304`), the
  gear (`:386-396`), the tray icons (`:600-603`), the permissions shield
  (`overlay_host.rs:1870-1884`) and the tab maximize glyph (`tabs.rs:~580`)
  to use it.
- **Estimated effort:** L (one-time cost, reused forever)

### 6. ~~Sidebar wheel card uses solid indigo fill~~ — Superseded (2026-04-21)

The sidebar-wheel-card gap no longer applies. The user's 2026-04-21 spec
deliberately diverges from the concept's 96 px permanent sidebar in
favour of an in-plane **wheel disc** (`quick-shell/src/shell/sidebar_wheel_disc.rs`,
commits `2a7ec2b` + `2337b21` + `65d105d`). Spokes render as radial
labels around a central hub, not as rectangular cards. The concept's
vertical-rl rotated labels are closest in spirit to the spokes on the
horizontal axis, but the wheel's rotated-layout requirement means we do
not follow the concept's card styling here.

See `docs/nirios/wheel-disc-design.md` for the full design reference.

### 7. Text is rendered with heavy-looking weight — Medium

- **What the concept shows:** Body is Tailwind default (`font-sans` →
  system-ui stack) with deliberate weight tiers: `font-medium`/`font-semibold`
  for most chrome, `font-bold` for titles, `font-mono` for values
  (clock / system-monitor). Most chrome text is `font-semibold` + light
  tracking.
- **What we render today:** In `07-theme-tightened.png` the "Notes" card
  heading, the tab titles, and the top-bar "Home 1/1" all read significantly
  heavier than the concept — almost as if every font were bold. The likely
  cause: `text_primary` is a near-white and the text renderer uses a single
  weight. Check if `cosmic-text` is being asked for a specific weight; if
  not, we'll get the platform's default regular but it may not be the same
  family as the concept's system-ui (Inter/SF/Segoe depending on OS).
- **Concrete fix:** Explicitly request weight 500 (medium) for most labels
  and 700 (bold) for `h1`/`h2` in the overview + top-bar title in
  `text_render::TextRenderer`. Also verify fallback fonts match — the
  concept selects `sans-serif` (browser picks Arial/Helvetica/Segoe UI
  depending on platform); we should target Inter or DejaVu Sans explicitly.
- **Estimated effort:** S

### 8. Top-bar center pill misses the `/` separation + format details — Medium

- **What the concept shows:** The center band has a `rounded-lg hover:bg-white/10`
  hover state and the clock is `text-sm font-bold tracking-wide text-white`
  with format `HH:MM` — and on the *left* of the band a 3-dot *vertical*
  indicator column where the active dot is `indigo-400` scaled 1.2× (§2.1,
  §2.2).
- **What we render today:** `topbar_chrome.rs:398-433` does paint a vertical
  3-dot indicator. Good. But in `07-theme-tightened.png` the band shows
  "23:17" centered inside a wide pill — the dots *are* barely visible on
  the far left (tiny). The contrast between the active dot and inactive
  dots is too small (our `dot_size * 1.4` active vs `dot_size * 1.0`
  inactive on the same `text_dim` colour). The concept's active dot uses
  `indigo-400` which contrasts much more against the inactive
  `rgba(255,255,255,0.2)`.
- **Concrete fix:** At `topbar_chrome.rs:423`, confirm the active colour
  actually resolves to a saturated indigo (currently `colors.accent[5]`);
  if the theme maps accent[5] to indigo-500, that's fine, but verify by
  eye — the 07 screenshot's active dot looks grey, not indigo. Also
  increase active radius to 1.6× and add a subtle opacity bump for
  non-active dots (0.3 → 0.35) so they read without shouting.
- **Estimated effort:** S

### 9. Collaborator avatars are solid indigo, not personalised/gradient — Medium

- **What the concept shows:** Each avatar uses the collaborator's own
  `cursorColor` as `backgroundColor` (inline style, §1.1) with a
  `border-2 border-black` separator between overlapping avatars. Each
  peer has a distinct hue — pink, purple, emerald, etc.
- **What we render today:** `topbar_chrome.rs:613-641` paints every avatar
  with `colors.accent[5]` (a single indigo). In `07-theme-tightened.png`
  we see only one "Ada" remote cursor (as a coloured dot next to the name
  pill), but the top-bar stack would all be monochrome indigo. The
  `Collaborator::color` field is presumably populated but unused here.
- **Concrete fix:** Read `Collaborator::color` (or hash the name via
  `presence.color_for`) and pass it to the circle fill. Add the black
  border ring between stacked circles.
- **Estimated effort:** S

### 10. Desktop backdrop grid is there but the noise-overlay is missing — Low

- **What the concept shows:** `bg-[url('noise.svg')] opacity-20 mix-blend-overlay`
  (`DesktopView.tsx:216`) on top of the gradient — subtle, but responsible
  for the "not a flat gradient" look in the concept.
- **What we render today:** `desktop_backdrop.rs:52-100` paints the gradient
  + 24 px grid lines only; a code comment at `:8` says "noise skipped —
  requires an image resource". In `07-theme-tightened.png` the background
  between grid lines reads as a flat smooth gradient (visible banding
  along the right edge), whereas the concept's is gently stippled.
- **Concrete fix:** Bake a small tileable noise PNG (~64×64) into the
  binary via `include_bytes!`, upload it as a Vello image once, and tile
  it with `opacity 0.08` over the backdrop. Same fix pattern already used
  elsewhere for static resources.
- **Estimated effort:** M

### 11. Permissions shield is still a stub square in overview — Low

- **What the concept shows:** `Shield` lucide icon at `w-4 h-4
  text-emerald-400` leading the "Permissions" label (§8.1 `OverviewView.tsx:83-103`).
- **What we render today:** `overlay_host.rs:1870-1884` has an explicit
  code comment: *"Shield glyph stubbed as a small emerald square"*. In
  `06-overview-open-vertical.png` you can see a 10 px filled square
  labelled "Permissions" — clearly wrong.
- **Concrete fix:** Covered by gap #5 (icon embedding); this is the
  canonical first consumer.
- **Estimated effort:** S (once #5 lands)

### 12. Overview header typography is not large / bold enough — Low

- **What the concept shows:** Header is `h1 text-4xl font-bold text-white
  mb-4 tracking-tight` + `p text-slate-400 text-lg`.
- **What we render today:** In `06-overview-open-vertical.png` the
  "Workspaces" title looks about 28–30 px and has only modest boldness —
  close but thinner than `font-bold text-4xl` (32 px / Tailwind's 36 px).
  The subtitle "Your workspaces, side by side" looks right.
- **Concrete fix:** In `overlay_host.rs` overview header, use
  `tokens.typography.scale.xxl` (or `h1`) with weight 700. If our scale
  doesn't include a size large enough, widen `xxl` in `quick-ui`
  `typography.rs`.
- **Estimated effort:** S

### 13. Scrim for overview mode is absent / indistinct — Low

- **What the concept shows:** Not a hard scrim for the overview view
  itself (it fully replaces the desktop), but the view is set on
  `bg-slate-950` (`#020617`) so there's a distinct "darker than desktop"
  shift when entering overview.
- **What we render today:** `06-overview-open-vertical.png` uses what looks
  like the same desktop gradient background, not the darker slate-950
  flat. The overview reads as "same wall with cards on it" rather than
  "entered a darker room".
- **Concrete fix:** In overlay_host's overview render path, force the root
  fill to `colors.neutral[0]` (slate-950) instead of whatever currently
  shows the desktop gradient through.
- **Estimated effort:** S

---

## Recommended prioritization for the next ralph iteration

The three **highest-impact** gaps are #1 (no real shadow), #2 (active-tab
contrast), and #3 (no backdrop blur). Of these, **gap #2** is the cheapest
to ship and has the most "instant wow" return — changing two alpha values
and the inactive title colour in `quick-ui/src/widget/tabs.rs` immediately
makes "which tab am I on" obvious, which is the first thing the eye lands
on. Gap #4 (overview active-row halo) is similarly cheap and fixes a
clearly visible bug. Shadow-fidelity (#1) is a bigger undertaking because
it touches the Vello layer-blur or sprite path.
