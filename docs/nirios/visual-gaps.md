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

### 1. ~~Content card has no real drop shadow~~ — Shipped (2026-04-21, commit `5837eee`)

- **What concept shows:** Tailwind `shadow-2xl`
  (`0 25px 50px -12px rgba(0,0,0,0.25)`) under every card, plus an
  indigo bloom `shadow-[0_0_60px_rgba(99,102,241,0.6)]` on the focused
  variant (`WindowFrame.tsx:354-369`).
- **What shipped:** `tile_host.rs` now paints a pre-baked 256 × 256
  radial Gaussian sprite (`alpha = exp(-((d - d_peak)/k)²)` with
  `k ≈ 0.35` and `d_peak ≈ 0.30`) underneath each content card. The
  dual-shoulder envelope clamps alpha to zero at both the card edge and
  the sprite's boundary, so the composite has no square halo and no
  double-darkening over the card fill. Focused tiles additionally paint
  an indigo-500 bloom driven by `WindowGroup::search_active` —
  `set_focused(focused)` on the active `TileHost` before draw. The old
  four-concentric-rounded-rect stack is removed; the new sprite reads as
  a single soft gradient instead of a stair-stepped halo. Regression
  test: `shadow_sprite_peaks_in_interior_not_centre_or_edge`.

### 2. ~~Active tab has almost no contrast against inactive tabs~~ — Shipped (2026-04-21, commit `4d6df4c`)

- **What the concept shows:** Active + focused pill has `bg-white/10` fill,
  pure `text-white`, and `border-white/20`; inactive pills are `bg-white/5`,
  `text-white/70`, `border-white/10`. With the `shadow-[0_8px_32px_rgba(0,0,0,0.2)]`
  under every pill, the active tab visually "pops forward" on the dark shell
  (`concept-features.md` §7.4).
- **What we render today:** `ui/src/widget/tabs.rs:459-471` picks
  alphas 38 (active) vs 13 (inactive) vs 26 (hover) with a border alpha of
  51/26. In `07-theme-tightened.png`, "Notes" (active) and "Files / Terminal /
  Browser" (inactive) look essentially identical — same pill height, same
  greyish fill, same title colour. The user cannot tell at a glance which
  tab is active; only the missing `×` hover-chip differs, and that's not
  rendered unless the cursor is on the pill.
- **Concrete fix:** In `ui/src/widget/tabs.rs:459`, widen the active
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

### 3. ~~No `backdrop-blur` anywhere~~ — Translucent-tint fallback shipped (2026-04-21, submodule `faf4e7c`)

- **What the concept shows:** Top bar `bg-black/80 backdrop-blur-xl` (§1.1);
  Collaborator + QuickSettings popovers `bg-slate-900/80 backdrop-blur-md`
  (§1.1, §3.1); GlobalSearch + settings modal `bg-slate-900/70 backdrop-blur-md`
  with a `backdrop-blur-[1px]` page scrim (§5.1, §4.1).
- **What shipped:** Vello 0.2.1 has no `push_blur_layer` primitive, so we
  ship the alpha-tint half of the spec today and revisit convolution when
  the Vello bump lands. Pinned constants: `TOP_BAR_SURFACE_ALPHA = 0.80`,
  `POPOVER_SURFACE_ALPHA = 0.80` (collaborators + QuickSettings),
  `PALETTE_SURFACE_ALPHA = 0.70` (GlobalSearch via theme override so
  ui's shared `CommandPalette` stays untouched),
  `MODAL_SURFACE_ALPHA = 0.70` (WorkspaceSettings), `MODAL_SCRIM_ALPHA = 0.20`
  (page scrim, down from 0.30). Regression tests:
  `topbar_chrome_surface_alpha_is_80_percent` + `global_search_surface_alpha_is_70_percent`
  pin the accessors so future reverts to opaque flip red without a pixel snapshot.
- **Remaining work (deferred):** real Gaussian convolution lands when Vello
  grows a blur primitive — the alpha-tint delta already makes the chrome
  read as translucent over the gradient, so the user-facing gap is closed
  enough to drop from "High" to "parked".

### 4. ~~Overview active-row glow is a crisp rectangle "pale halo"~~ — Shipped (2026-04-21, commit `4b12846`)

- **What concept shows:** indigo bloom
  `shadow-[0_0_40px_-10px_rgba(99,102,241,0.3)]` fading into `slate-950`.
- **What shipped:** `overlay_host.rs` now paints four concentric rings at
  alphas 0.10 / 0.06 / 0.03 / 0.01 and spreads 6 / 14 / 28 / 48 px, using
  `accent[5]` (indigo-500). The near-card ring stays below the 2 px indigo
  border, and the outer ring softly blends into the scrim — no visible
  rectangular edge. Since Vello has no blur primitive, the ring-stack
  emulates a gaussian fade; cheap to paint (4 rounded-rect fills per
  active row).

### 5. ~~Icons are kurbo primitives, not lucide glyphs~~ — Shipped (2026-04-21, commits `e2fad05` + `59be5eb`)

- **What concept shows:** lucide-react glyphs throughout: `LayoutGrid`,
  `Settings2`, `Wifi` / `Monitor` / `Battery`, `Shield`, `Maximize2` /
  `Minimize2`, `X`, `ChevronUp` / `Down`, `Plus` — all at 1.5 px stroke.
- **What shipped:** new `quick_ui::icons` module (`e2fad05`) embedding
  lucide's BezPaths on the 24 × 24 viewBox plus a pure `draw_icon(scene,
  transform, rect, icon, color, stroke_px)` helper with round caps and
  round joins. Shell chrome (`59be5eb`) now routes through it for the
  top-bar overview grid, workspace-name gear, tray Wifi/Monitor/Battery
  triad, and the overview permissions shield. Closes gap #11 (the
  emerald-square shield stub) as a side effect. Unit tests pin
  non-empty subpaths per icon; `shell_chrome_test` asserts the overview
  button routes through `Icon::LayoutGrid`. Remaining call sites (tab
  maximize, tab-kind leading icons) can ride a later batch.

### 6. ~~Sidebar wheel card uses solid indigo fill~~ — Superseded (2026-04-21)

The sidebar-wheel-card gap no longer applies. The user's 2026-04-21 spec
deliberately diverges from the concept's 96 px permanent sidebar in
favour of an in-plane **wheel disc** (`shell/src/shell/sidebar_wheel_disc.rs`,
commits `2a7ec2b` + `2337b21` + `65d105d`). Spokes render as radial
labels around a central hub, not as rectangular cards. The concept's
vertical-rl rotated labels are closest in spirit to the spokes on the
horizontal axis, but the wheel's rotated-layout requirement means we do
not follow the concept's card styling here.

See `docs/nirios/wheel-disc-design.md` for the full design reference.

### 7. ~~Text is rendered with heavy-looking weight~~ — Shipped (2026-04-21, commits `7dd9fa1` + `9a018e3`)

- **What concept shows:** Tailwind-aligned weight tiers —
  `font-medium` (500), `font-semibold` (600), `font-bold` (700).
- **What shipped:** `quick_ui::TextWeight` enum with Regular / Medium /
  Semibold / Bold variants that proxy through to
  `cosmic_text::fontdb::Weight` (commit `7dd9fa1`). `TextRenderer` now
  exposes `draw_text_weighted` and `measure_weighted`; legacy
  `draw_text` defaults to Regular so untouched call sites keep
  behaviour. Consumer adoption (commit `9a018e3` on the shell side)
  covers: clock = Bold, workspace-name pill = Semibold, tab titles =
  Semibold, overview header = Bold, overview subtitle = Regular,
  collaborator avatar initials = Bold, overflow "+N" = Semibold,
  system-monitor labels = Semibold / values = Bold, calendar prefix +
  time = Medium / event = Semibold. Weight accessors on
  `TopBarChrome` pin the tiers for regression tests.

### 8. ~~Top-bar center pill misses the `/` separation + format details~~ — Shipped (2026-04-21, commit `3b786c2`)

- **What concept shows:** the vertical 3-dot column has an `indigo-400`
  active dot visibly larger + brighter than the `rgba(255,255,255,0.2)`
  inactive dots.
- **What shipped:** `topbar_chrome.rs` now paints the active dot at
  `1.6×` base radius (indigo `accent[5]`) and brightens the inactive dots
  by bumping their opacity. Regression test
  `topbar_chrome_active_center_dot_is_larger_than_inactive` pins the
  geometric contrast.

### 9. ~~Collaborator avatars are solid indigo, not personalised/gradient~~ — Shipped (2026-04-21, commit `730e6c4`)

- **What concept shows:** each avatar uses its peer's `cursorColor`
  inline, with `border-2 border-black` between overlapping circles.
- **What shipped:** `topbar_chrome.rs` caches a `Vec<Color>` alongside
  `collaborator_initials` in `sync_from_tree`. Each entry resolves a peer
  via `Collaborator.cursor_color` (falls through to
  `quick_ui::presence::user_color` hash when unset), so the canonical
  four `me / alice / bob / charlie` stay at their concept-matched hues
  (indigo / pink / orange / emerald) and unknown ids get a stable hash
  colour. Draw site paints a 9 px black disc under a 7.5 px peer-colour
  disc, producing the 2 px black ring between stacked circles. Covered
  by `topbar_chrome_maps_two_peers_to_two_distinct_avatar_hues`.

### 10. ~~Desktop backdrop grid is there but the noise-overlay is missing~~ — Shipped (2026-04-21, commit `722ad07`)

- **What concept shows:** `bg-[url('noise.svg')] opacity-20 mix-blend-overlay`
  (`DesktopView.tsx:216`) on top of the gradient.
- **What shipped:** `desktop_backdrop.rs` bakes a tileable noise texture
  (deterministic 64×64 pattern) and tiles it over the gradient beneath
  the grid lines. Breaks the smooth banding visible in the old
  `07-theme-tightened.png` capture without eating a full-res asset in
  the binary.

### 11. ~~Permissions shield is still a stub square in overview~~ — Shipped (2026-04-21, commit `59be5eb`, via gap #5)

- **What concept shows:** `Shield` lucide icon at `w-4 h-4
  text-emerald-400` leading the "Permissions" label.
- **What shipped:** the emerald-square stub in `overlay_host.rs` is
  gone — the `quick_ui::icons::Icon::Shield` BezPath is stroked at
  `colors.status.success` directly from the overview permissions row,
  matching lucide's proportions and 1.5 px stroke.

### 12. ~~Overview header typography is not large / bold enough~~ — Shipped (2026-04-21, commit `687e7ee`)

- **What concept shows:** `h1 text-4xl font-bold text-white mb-4
  tracking-tight`.
- **What shipped:** overview header in `overlay_host.rs` now renders at
  ≥32 px with `font-bold` weight, pinned by the
  `overview_header_font_size_is_at_least_32px` regression test.

### 13. ~~Scrim for overview mode is absent / indistinct~~ — Shipped (2026-04-21, commit `687e7ee`)

- **What concept shows:** overview lives on `bg-slate-950` (`#020617`)
  so entering it feels like "the desktop dimmed behind a plate".
- **What shipped:** overview render path in `overlay_host.rs` now paints
  a full-bleed slate-950 scrim before placing the cards, giving the
  distinct "entered a darker room" transition instead of showing the
  gradient through.

---

## Recommended prioritization for the next ralph iteration

Gaps #1, #2, #4, #5, #7, #8, #9, #10, #11, #12, #13 have shipped; #6
was superseded by the wheel disc. The **only remaining open gap** is:

- **#3 Backdrop blur** — High. Needs either Vello's
  `scene.push_blur_layer` (requires bumping Vello; verify the pinned
  version supports it) or a translucent-tint fallback that at least
  honours the `/80` vs `/70` alpha split on the top bar, popovers,
  GlobalSearch, and the settings modal. The radial-sprite trick from
  #1 does not generalise here because the blur has to track whatever's
  under the chrome — a frozen sprite can't.

Next structural unlocks beyond visual parity live in
`docs/nirios/outstanding-work.md`: real-socket collaboration E2E,
presence dots on wheel-disc spokes, tab maximize/minimize lucide
glyph swap.
