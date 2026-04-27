# Nirios — Concept Coverage Audit

Snapshot: 2026-04-27. Component-by-component delta between
`nirios-concept/src/` and the shipped implementation on `nirios-shell`.
This audit cross-references the concept TSX line-by-line against the
Rust ports listed in `parity-status.md`. The goal is to quantify the
gap the user describes as "not at all" close — many ✅ rows in
`parity-status.md` are functionally present but visually/structurally
diverge from the concept in ways listed below.

Source of truth for the concept: `nirios-concept/src/components/*.tsx`.
Source of truth for the port: `quick-shell/src/shell/*.rs` and
`quick-ui/src/widget/*.rs`.

## Component matrix

| Concept file | Rust port | Visual ⊨ | Functional ⊨ | Notes |
|---|---|---|---|---|
| `WindowFrame.tsx` | `quick-ui/src/widget/{tabs,window_group}.rs` + `tile_host.rs` | 🟡 | 🟡 | Tab pill + close + maximize present. Per-tab "Search in App" hover icon present. Concept's `motion.div` spawn/exit, `shadow-[0_8px_32px]`, foggy-collaborator gradient missing or approximated as flat dots. |
| `WindowFrame.tsx` (maximized chrome) | `quick-ui/src/widget/window_group.rs` | 🟡 | 🟡 | Title pill + restore + close drawn. **No** mouse-edge-trigger reveal — ours always draws while maximized; concept slides them in only when `clientY ≤ 80` (`WindowFrame.tsx:114-123`). |
| `OverviewView.tsx` | `quick-shell/src/shell/overlay_host.rs::draw_overview` | 🟧 | 🟧 | Vertical workspace list shipped. Title text undersized + no `tracking-tight`; mini-window previews are stub rectangles, not the dual-tab-row + slate-800 card the concept paints; permissions panel paints a tiny shield + 2 chips, omits the "Settings2" gear button + collaborator avatars + invite "+" button rendered per row in concept (`:113-124`); rail dot uses `border-4` ring in concept, ours fakes it with two concentric fills. |
| `QuickSettings.tsx` | `quick-shell/src/shell/quick_settings.rs` (state only) + `overlay_host.rs::draw_quick_settings` | 🟥 | 🟧 | Concept renders a 3-button quick-action row (Settings/Camera/Power), 2 sliders inside a `bg-white/5 p-3 rounded-2xl` card, **two indigo-filled toggle cards** with chevrons + connection-name subline, drill-in lists for WiFi/BT, and a Power-Profile row. **Ours** draws a flat title + 2 grey progress-bar sliders + 2 monochrome iOS-style toggle pills. Drill-in `nav_stack` exists in `QuickSettings` state but isn't rendered. No power/camera/settings buttons. No Wi-Fi/Bluetooth list. |
| `TopBarWidgets.tsx` (carousel) | `quick-shell/src/shell/topbar_chrome.rs` (center band) | 🟧 | 🟡 | Clock/SystemMonitor/Calendar wired with scroll-wheel + dot click. Concept widget is **480 px** wide pill with hover background; ours is 360 px. Concept's vertical 3-dot indicator uses `bg-indigo-400` (`text-indigo-400` ≈ `accent[6]`); ours uses `accent[5]` (indigo-500) — the concept's lighter shade is by design to read against the dark bar. SystemMonitor sparkline in concept is `bg-emerald-500/30`; ours uses `theme.accent_primary` (indigo) — wrong colour family. CPU label in concept is `text-emerald-400 font-bold uppercase tracking-widest`; ours uses indigo-500 Semibold (gap #7 partially fixed but colour family still wrong). |
| `WorkspaceSettingsModal.tsx` | `quick-shell/src/shell/workspace_settings.rs` + `overlay_host.rs::draw_ws_settings` | 🟥 | 🟥 | This is the **largest gap in the codebase**. Concept is a 4xl-wide × 600 px modal with a 256 px **left sidebar nav** (workspace icon + name header, 3 nav rows: General / Roles & Permissions / Files, a footer "Close Settings" row), and a per-tab right pane with elaborate content (collaborator grid w/ avatars + role pills + remove button, role list w/ swatches + permissions toggle grid + per-file permission radios). **Our `draw_ws_settings`** is 520×360 px, **no sidebar**, the four "tabs" are flat text pills with the first one always active, the only interactive surface is a **Name** input (rename workspace). The whole modal is essentially a rename dialog with stub tabs. The `WorkspaceSettings` Rust struct has `apps_table` + `files_table` + `select_role` API but **none of it is rendered** by `draw_ws_settings`. |
| `GlobalSearch.tsx` | `quick-shell/src/shell/search.rs` + `quick-ui/src/widget/command_palette.rs` | 🟥 | 🟧 | Concept palette is `max-w-2xl` rounded-3xl with: context banner (top, indigo), search-icon + input + Cmd-K kbd chip, scope/filetype/full-text filter strip (per-context), `max-h-[400px]` results list with selected row highlighted indigo, footer with ↑↓/Enter hints + scope label. **Ours** is a flat raised-surface card with `radii.md` (8 px, concept uses 24 px / `rounded-3xl`), no leading search icon, no Cmd-K chip, no filter strip, no footer hints, selected row uses `neutral[4]` (mid-grey) instead of `bg-indigo-500`. Context-aware filtering exists in `search.rs::set_context_kind` but isn't surfaced visually. |
| `CursorOverlay.tsx` | `quick-shell/src/shell/remote_cursors.rs` | 🟧 | ✅ | Caret + name chip drawn. Concept uses `MousePointer2` lucide outline filled with peer color (white outer stroke); ours draws a small filled triangle BezPath (no outline). Concept name chip is `bg-{peerColor} text-white px-2 py-0.5 rounded-full text-[10px] font-bold`; ours uses neutral `surfaces.raised` with peer-colour underline — completely different visual. |
| `DesktopView.tsx` | `quick-shell/src/shell/{mod,workspace_drop,desktop_backdrop,sidebar_wheel_disc}.rs` | 🟧 | 🟡 | Backdrop gradient + grid + noise: ✅. **Wheel sidebar superseded by user spec** (per `parity-status.md` §3 row 1, reaffirmed in user memory): we ship `SidebarWheelDisc`, a 240 px in-plane disc, not the concept's `w-24` left-pinned 3D macOS-Dock-style strip column with avatar stack and `[writing-mode:vertical-rl]` workspace labels. This is intentional but it is the single largest visible departure from the concept. Per-strip 3D rotateX stacking with `filter: blur(8px)` on inactive strips is not ported (and probably can't be without `push_blur_layer`). DropZone with hover-reveal "+" button is implemented in `GroupRow`. Drag-preview (`dragPreview`) showing a mini-window mock at cursor position when dragging files is **absent**. |
| `apps/EditorApp.tsx` | `quick-shell/src/shell/editor_app.rs` | 🟧 | ✅ | Concept paints a **frozen Catppuccin Mocha snippet** (`bg-[#1e1e2e]`) with hand-coloured tokens, a single hot-pink highlighted span emulating a peer-edit + animated caret. Ours is a **real CRDT-backed editor on a white card** with token-coloured syntax mapped to theme tokens (indigo keywords, emerald strings, amber numbers). Functionally far ahead; visually nowhere near the dark Catppuccin look. The Editor tile lives on a white window-card per concept's `bg-white` outer card, so this is at least consistent. |
| `apps/TerminalApp.tsx` | `apps/terminal/` submodule (real PTY) | ✅ | ✅ | Real terminal backed by `quick-terminal`. Palette (per `parity-status.md`) tuned to concept's green-400 / blue-400 / purple-400 / etc. Visually well-aligned for the stub frame. |
| `apps/FilesApp.tsx` | `apps/file-explorer/` submodule | 🟡 | 🟡 | Sidebar tiers with Favorites + System present (`parity-status.md` §5). Concept's `bg-white/80 backdrop-blur-xl` translucent panel + per-folder hover scale + draggable file/folder grid w/ `Folder fill-blue-500/10` and per-shortcut context-menu remove are not all confirmed in the in-tree port (out of scope to deep-audit a submodule from here). |
| `apps/BrowserApp.tsx` | `quick-shell/src/shell/browser_stub.rs` | 🟡 | 🟧 | White card + chrome strip + 3 grey traffic-lights + globe-pill URL bar + indigo App Preview tile + headline. Visually close-ish. Functionally a stub — no actual web content; concept itself is a stub too, so this row is not a real gap. |

Totals: **0 ✅ • 7 🟡 • 4 🟥 • 0 🟧** for visual fidelity (counting
🟧 as 🟡 and the wheel-disc deviation as 🟥 because it's the most
visible departure). Functional fidelity: **2 ✅ • 6 🟡 • 4 🟥**.

## Per-component deltas

### WindowFrame.tsx → tabs.rs / window_group.rs / tile_host.rs

- Concept tab pill (`WindowFrame.tsx:294-296`):
  `flex items-center gap-2 px-4 py-2 rounded-full text-sm font-semibold
   shadow-[0_8px_32px_rgba(0,0,0,0.2)] backdrop-blur-md border
   bg-white/10|bg-white/5 border-white/20|border-white/10`.
  Ours (`tabs.rs:339-357`) paints rgba alpha tiers `(52, 64) / (26, 40) /
  (8, 18)` — close enough, but `font-semibold` is honoured only in the
  draw call (good, gap #7 fixed). `shadow-[0_8px_32px]` is approximated
  by 4 down-translated rings (`tabs.rs:316-338`); reads softer than the
  concept's true large-blur shadow.
- Concept tab spawn animation (`WindowFrame.tsx:284-288`):
  `initial={{ opacity: 0, y: -10, scale: 0.9 }}` →
  `animate={{ opacity: 1, y: 0, scale: 1 }}`.
  Ours (`tabs.rs:46-51`, `INSERT_FADE_*`) animates **opacity + y** only;
  scale is skipped. Documented deviation; acceptable.
- Concept foggy collaborator indicator on tab pill
  (`WindowFrame.tsx:299-310`): a `w-16 h-16 rounded-full blur-xl`
  translated half-down per peer + `mix-blend-screen`. Reads as a
  glowing tinted halo at the pill bottom.
  Ours (`tabs.rs:419-438`) paints small filled circles at low alpha;
  no blur, no `mix-blend-screen`. Not foggy at all — flat dots.
- Concept "Search in App" hover button (`WindowFrame.tsx:319-325`)
  uses lucide `Search` glyph + `hover:bg-white/30`.
  Ours (`tabs.rs:511-535`) draws a kurbo magnifier outline. ✅
  reasonable parity.
- Concept maximized floating controls (`WindowFrame.tsx:213-258`) only
  appear on `mouseY ≤ 80`. Ours draws them unconditionally while
  maximized — a regressed affordance, the concept's deliberate "ambient
  maximized chrome" feel is lost.
- Concept active card focus (`WindowFrame.tsx:359-360`):
  `ring-4 ring-indigo-500/50 shadow-[0_0_60px_rgba(99,102,241,0.6)]`
  while `isSearching`. Ours (per `parity-status.md` row 4) paints
  two concentric indigo strokes — close, but lacks the soft 60 px
  bloom; on Vello this is the precomputed `indigo_bloom_sprite`
  (`tile_host.rs:100-103`) which is not currently composed in the
  search-active path. That sprite is wired only to "focused tile"
  (`TileHost::focused`).

### OverviewView.tsx → overlay_host.rs::draw_overview

- Concept h1 (`OverviewView.tsx:48`): `text-4xl font-bold text-white
  mb-4 tracking-tight`. 36 px / 700 / negative letter-spacing.
  Ours (`overlay_host.rs:1903-1920`) uses the largest token step
  `tokens.typography.scale.x4l` = 40 px (close) at `TextWeight::Bold`
  — but `tracking-tight` (~ -0.025em) is unimplemented; tokens have
  no letter-spacing axis. Weight + size land; tracking does not.
- Concept subtitle (`:49`): `text-slate-400 text-lg` (18 px Medium).
  Ours (`overlay_host.rs:1925`) uses `lg = 16 px` Regular weight.
  **Two pixels short and one weight tier off.**
- Concept active card border (`:63`): `border-2 border-indigo-500
  shadow-[0_0_40px_-10px_rgba(99,102,241,0.3)]`.
  Ours (`overlay_host.rs:1992-2036`) does the four-ring soft bloom
  + 1.5 px stroke. Approximation acceptable; "shadow-[0_0_40px_-10px]"
  has the offset compressed inside the rect, which we approximate with
  4 outward rings. Visually softer than concept; passes for "glow".
- Concept active rail dot (`:70`): `w-4 h-4 rounded-full border-4
  border-slate-950 bg-indigo-500`. The 4 px slate-950 ring is what
  separates the dot from the rail. Ours (`overlay_host.rs:2068-2087`)
  paints a slate-950 outer disc + smaller indigo inner disc — same
  result optically, ✅.
- Concept WS-N pill (`:77`): `px-2 py-0.5 rounded-md bg-slate-800
  text-slate-300 font-mono text-xs`. Ours (`overlay_host.rs:2104-2133`)
  uses `colors.neutral[2]` (slate-800) ✅, but the **pill text uses
  `colors.neutral[7]` (slate-300) at a non-monospace weight** — concept
  is `font-mono`. The text renderer has a `font_mono` axis (per
  `quick-ui/src/text_render.rs`); the call here doesn't request it.
- Concept permissions panel (`:83-103`): full row with shield + label
  + Settings2 gear button + 2 badge chips ("N Roles" / "N Files") at
  `bg-blue-500/20 text-blue-400` + `bg-purple-500/20 text-purple-400`.
  Ours (`overlay_host.rs` ~ `:2146-…`) paints a single chip and a
  static shield (without text "Permissions"). Loses the **role-count
  / file-count** badges — material loss of information density.
- Concept collaborator stack (`:111-124`): up to N avatars with
  `-space-x-2`, then a `+` button to invite. Per concept this lives
  on every workspace row in the overview. Ours doesn't render
  collaborators per row at all in the overview path; only the top-bar
  has avatars.
- Concept mini-windows (`:6-32`): two-row mock of pill-tabs above a
  `bg-slate-800 rounded-lg shadow-lg` "card body". Ours paints a
  flat rectangle (`draw_mini_window` at `overlay_host.rs:1776`)
  without the dual-row pill-strip-on-top affordance. Visually
  this loses the "this is a window with tabs" reading at a glance.

### QuickSettings.tsx → overlay_host.rs::draw_quick_settings

- Concept popover (`QuickSettings.tsx:169`): `w-80 bg-slate-900/80
  backdrop-blur-md border border-slate-700 rounded-3xl shadow-2xl
  p-4 gap-4`. ~320 px wide, 24 px radius.
  Ours uses `radii.lg` = 12 px. **Half the concept radius.**
- Concept top row (`:181-201`): three equal-width buttons `flex-1
  bg-white/10` with **Settings / Camera / red-tinted Power** glyphs.
  Ours has **none of these** — no row of action buttons.
- Concept slider container (`:204-215`): `bg-white/5 p-3 rounded-2xl`
  *card* containing two horizontally-laid-out sliders, each a
  `h-1.5 rounded-full` thin bar with `bg-white` fill, prefixed by a
  Sun/Volume2 icon in `text-slate-400`.
  Ours (`overlay_host.rs:2907-2926`) paints two stand-alone progress
  bars 6 px tall on `neutral[2]` (slate-800), no enclosing card,
  no leading icons, no `bg-white` filled ramp — just `accent[5]`
  (indigo) bars. Wrong colour, no icons, no card.
- Concept toggle grid (`:218-246`): two **indigo-500-filled** rounded
  `bg-indigo-500 hover:bg-indigo-600` buttons in a 2-col grid, each
  containing the Wi-Fi/Bluetooth glyph, a chevron, a label, and a
  current-connection sub-line ("Home_Network" / "AirPods Pro").
  Ours paints a label + an iOS-style on/off pill (white knob travels
  along an indigo track when on) — completely different idiom.
- Concept Power Profile row (`:249-258`) is missing entirely.
- Concept WiFi/BT drill-in lists (`:101-162`) — no entries; the
  `nav_stack` field on `QuickSettings` is dead state.

### TopBarWidgets.tsx → topbar_chrome.rs

- Concept band width (`:37`): `w-[480px] h-9`. Ours (`topbar_chrome.rs:542-543`):
  `band_w = 360.0`, `band_h = 28.0`. **120 px narrower, 8 px shorter**;
  the band feels much tighter than concept's airy widget.
- Concept hover (`:37`): `hover:bg-white/10 hover:shadow-[0_8px_32px]
  transition-all`. Ours: no hover state on the band.
- Concept dots (`:48-54`): `bg-indigo-400` (= `accent[6]` in our ramp)
  for active. Ours uses `colors.accent[5]` (indigo-500) — one shade
  too saturated.
- Concept SystemMonitor sparkline (`TopBarWidgets.tsx:120-124`):
  bars are `bg-emerald-500/30`. **Ours uses `theme.accent_primary`
  (indigo-500)** — wrong colour family entirely.
- Concept SystemMonitor labels: CPU `text-emerald-400`, RAM
  `text-blue-400`, TMP `text-orange-400`. Ours: CPU + RAM
  `colors.accent[5]` (indigo), TMP `text_dim`. **The "different
  semantic colour per metric" cue is lost.**

### WorkspaceSettingsModal.tsx → workspace_settings.rs / draw_ws_settings

This is the **most visually broken component** relative to concept:

- Concept modal (`:43`): `max-w-4xl h-[600px] bg-slate-900/70
  backdrop-blur-md border border-white/10 rounded-3xl flex`.
  Width up to ~896 px; ours is fixed 520×360. Concept's interior
  splits left-sidebar (256 px) + right-content. Ours is a flat
  single column.
- Concept left sidebar (`:49-91`): workspace identity card on top
  (8×8 indigo square w/ first letter, "Workspace Settings" caption),
  then 3 nav buttons (General / Roles & Permissions / Files), then
  a footer "Close Settings" row. **None of this is rendered.**
- Concept General tab (`:95-149`): `Invite Link` field + Copy
  button + `Collaborators (N)` grid where each row is an avatar
  + name + role chip + Remove button. **Our General tab is just
  a Name input.** Invite link API exists (`workspace_settings.rs:147
  invite_link`) but is never drawn.
- Concept Roles tab (`:152-254`): role list w/ colored swatches +
  isDefault chip; per-role app-permissions toggle grid (5 apps × on/off)
  + per-role file-permissions radio strip (none/read/write).
  **Not drawn.** `apps_table` + `files_table` exist as `DataTable`
  state but `draw_ws_settings` ignores them.
- Concept Files tab (`:257-303`): grid of files w/ icon + name +
  type badge + assigned-roles avatar pile + Remove. **Not drawn.**

So this row is functionally **stubbed** — the rename input works,
nothing else does. `parity-status.md` row 6 "Workspace settings modal
✅" is **stale**.

### GlobalSearch.tsx → command_palette.rs + search.rs

- Concept palette card (`:113`): `max-w-2xl bg-slate-900/70
  backdrop-blur-md border border-white/10 rounded-3xl`.
  Ours (`command_palette.rs:239`): `RoundedRect::from_rect(rect,
  theme.tokens.radii.md as f64)` = **8 px** vs concept's 24 px.
- Concept input row (`:138-153`): leading lucide `Search` glyph +
  input + trailing `Cmd-K` kbd chip in `bg-white/5 border
  border-white/10`. Ours: just placeholder "Search…" text in a
  neutral inset rect. **No search icon, no kbd chip.**
- Concept context banner (`:115-135`): when scoped to an app, paints
  a `bg-indigo-500/20 border-b border-indigo-500/30` strip with the
  app icon, "Searching in {title}", and a "Switch to Global" link.
  Ours has a `draw_search_banner` (`overlay_host.rs:2774`) that
  paints two scope pills (Workspace/All) — **not the concept's
  app-context indicator**. Different concept entirely.
- Concept filter strip (`:184-244`): scope chips + filetype multi-select
  + Full-Text toggle + Replace toggle. **Not drawn**; the only
  filtering is the two scope pills.
- Concept result row (`:251`):
  `${idx === selectedIndex ? 'bg-indigo-500 text-white' : 'hover:bg-white/5
  text-slate-300'}` — selected row is **fully indigo**.
  Ours (`command_palette.rs:308-316`) highlights with
  `theme.tokens.colors.neutral[4]` (mid-grey). Wrong colour.
- Concept icon container per row (`:258`): `p-2 rounded-xl bg-white/20`
  on selected, `bg-white/5` otherwise. Ours: no icon at all on rows.
- Concept footer (`:274-280`): kbd chips for ↑↓ Navigate / Enter
  Select + scope label on the right. **Not drawn.**

### CursorOverlay.tsx → remote_cursors.rs / draw_real_peers / draw_demo_peer

- Concept caret (`:19-22`): lucide `MousePointer2` SVG outline,
  `fill: peerColor; color: white` — the white outer stroke is what
  makes peer cursors readable on any background.
  Ours (`remote_cursors.rs:184-195`, `overlay_host.rs:2643-2648`):
  small filled BezPath triangle, no outline. Reads weaker on light
  backgrounds; concept's outlined arrow always reads.
- Concept name chip (`:23-28`): `bg-{peerColor} text-white px-2 py-0.5
  rounded-full text-[10px] font-bold`.
  Ours (`remote_cursors.rs:200-251`): chip uses neutral
  `surfaces.raised` background with peer-coloured underline + 1 px
  border. **The chip background should be the peer colour itself.**
  This is a clear visual departure: in concept the chip *is* the
  peer's badge; here it's a generic raised-surface popover.

### DesktopView.tsx → mod.rs / sidebar_wheel_disc.rs / etc.

- Concept Global Nav Wheel (`:219-323`): `absolute left-0 top-0
  bottom-0 w-24 z-40` — a 96 px-wide column of vertical-text
  workspace labels stacked along an arc, using `perspective: 1000px`
  + `rotateX` per item to give a 3D wheel feel; the active workspace
  is `text-white/90`, inactives at `text-white/40`; per-workspace
  collaborator avatar stack to the right of the label. The strip
  container uses `perspective: 2000px` + per-strip `rotateX` +
  `filter: blur(8px)` on inactive strips for the macOS-Dock peek.
  Ours: `SidebarWheelDisc` (`sidebar_wheel_disc.rs`), a 240 px
  in-plane circular disc with radial spokes and an indigo arc on
  the active rim. **This is intentional per user spec** (memory:
  "Spokes = workspaces, disc replaces old sidebar outright"), but
  it is the single biggest *visible* departure — every screenshot
  comparison will diverge here.
- Concept DropZone hover (`:54-90`): the gap between groups expands
  from `w-8` → `w-24` on hover and pops a `w-10 h-10 rounded-full
  bg-white/10 border-white/20 backdrop-blur-md` floating "+" button
  with whileHover scale 1.1. Ours (`tabs.rs::DROP_ZONE_*`) animates
  inter-pill gaps **inside a tab bar**, not inter-group gaps in a
  strip. The concept's between-group `+` floats over the workspace,
  ours has the equivalent but smaller and not styled the same.
- Concept DragPreview (`:461-530`): when dragging a file over the
  workspace, paints a **mock window mini-preview at the cursor**
  showing what would open (Files explorer card or Editor mock).
  **Absent in our shell.** A concept-iconic affordance.

### apps/EditorApp.tsx → editor_app.rs

- Concept palette (frozen): `bg-[#1e1e2e] text-[#cdd6f4]` (Catppuccin
  Mocha dark) with hand-coloured spans
  (`#cba6f7` keywords, `#89b4fa` idents, `#a6e3a1` strings,
  `#f9e2af` attrs, `#6c7086` line-numbers, `#313244` divider).
  Ours runs on **the white window-card surface** with theme tokens
  (slate-900 body, indigo-500 keywords, emerald strings, amber
  numbers). White-on-light vs concept's dark Catppuccin is the
  most jarring visual delta — though the concept itself is just
  decorative TSX, ours is a functional editor on the OS-mandated
  Theme. Trade-off per `CLAUDE.md` quick-ui rules.
- Concept "highlighted text by Alice" (`:46-57`):
  `bg-pink-500/30 border-b-2 border-pink-500` span + animated caret.
  Ours: presence-driven peer caret + selection painted via
  `presence::draw_peer_caret/_selection` — functionally a real
  peer mark, not a frozen mock. ✅ functional improvement.

### apps/BrowserApp.tsx → browser_stub.rs

- Concept browser body, `localhost` variant (`:27-33`): centred
  `w-32 h-32 bg-indigo-100 rounded-3xl` outer + `w-16 h-16
  bg-indigo-500 rounded-xl rotate-12` inner + `text-3xl font-bold
  text-slate-800 "App Preview"` headline + slate-500 sub.
  Ours (`browser_stub.rs:241-307`): hardcoded same composition,
  rendered. Reasonably close; **concept's `rotate-12` on the inner
  square is dropped** (we paint axis-aligned). Subtle but adds the
  "playful" reading of the concept.

## Stale ✅ in parity-status.md

Spot-check of 5 ✅ rows against the actual code:

1. **§6 "Workspace settings modal ✅"** — **stale**. Modal renders only
   a Name input + four flat tab labels with no working tab switch, no
   sidebar, no role/file/collab grids. The Rust state struct
   (`workspace_settings.rs`) has `apps_table` / `files_table` /
   `select_role` API never wired to a draw path. Should be 🟧 Partial.
2. **§4 "Width presets (narrow/normal/wide/custom) ✅"** — code lines
   up (`WindowWidth` enum + 300–1200 clamp). ✅ accurate.
3. **§5 "Browser ✅"** — close-ish; concept's `rotate-12` inner tile is
   missing but the row's "Still a stub" caveat already flags this.
   ✅ accurate enough.
4. **§7 "Top-bar avatar stack ✅"** — checked code in
   `topbar_chrome.rs:824-862`. Per-peer color, 2 px black separator
   ring, +N overflow chip — all present. ✅ accurate.
5. **§9 "Glow shadows for focus / drag-over ✅"** — drag-over ring
   accurate. **Search-active glow** claim says 8 px @ 24% outer + 4 px
   @ 50% inner; the concept calls for `shadow-[0_0_60px
   rgba(99,102,241,0.6)]` which `tile_host.rs` actually has a
   precomputed `indigo_bloom_sprite` for, but the shell's
   search-active path appears to use only the two strokes (no sprite
   composition). Suggest re-grading to 🟧 Partial — the concept's
   60 px bloom is documented but not wired in the search-active
   draw path.
6. **§3 "Per-strip label + avatars ✅"** (re-check, since user is
   most sensitive to the wheel area): the disc paints workspace
   labels at spoke tips, not per-strip labels in the concept's
   `[writing-mode:vertical-rl]` lockup. The ✅ here is honest about
   *content* (label + avatars present) but the visual idiom is
   fundamentally different — appropriate to mark as superseded
   (already does in row 1; row 2 is fine).

## Highest-impact deltas (top 5 user-noticeable)

1. **WorkspaceSettingsModal is a rename dialog with stub tabs.**
   Concept has a 4xl-wide left-sidebar-+-content modal with a
   General tab (invite link + collaborator grid), Roles tab (role
   list + per-role app/file permission grids), Files tab (file
   grid + assigned-role avatars). Ours renders only a Name input.
   *Fix hint:* split `draw_ws_settings` into per-tab branches that
   consume the existing `apps_table` / `files_table`; add a 256 px
   sidebar nav block on the left.
2. **Sidebar wheel is a circular disc, not a left-pinned 3D
   workspace dock.** This is intentional per user spec but is the
   single biggest pixel delta against the concept screenshots —
   any side-by-side will diverge here.
   *Fix hint:* not a fix — the concept disc is the locked design
   per user memory. Document this loudly in the audit so the user
   stops grading against the concept's `DesktopView.tsx:219-323`.
3. **GlobalSearch palette is missing every concept affordance**:
   no search icon, no Cmd-K kbd chip, no filter strip (scope /
   filetype / full-text), no footer hints, selected row is grey
   not indigo, radius is 8 px not 24 px.
   *Fix hint:* extend `command_palette.rs::draw` to paint the
   leading lucide search glyph, an inline kbd chip on the right,
   and a footer band; bump `radii.md` → `radii.xl` for the palette
   surface; switch the selected-row fill to `accent[5]`.
4. **QuickSettings is a flat list, not the concept's grid.** No
   action-button row (Settings / Camera / Power), no slider card,
   no indigo-filled toggle cards with sub-line, no Power Profile
   row, no Wi-Fi/BT drill-in. Renders as a stub.
   *Fix hint:* rebuild `draw_quick_settings` from the concept
   layout: 3-col action row → slider card → 2-col toggle card grid
   → power profile row. Wire `QuickSettings::nav_stack` to a
   second branch that draws the WiFi/BT lists.
5. **TopBar SystemMonitor uses indigo throughout instead of
   emerald (CPU) / blue (RAM) / orange (TMP).** The semantic-colour
   cue per metric is the concept's whole reason for drawing the
   strip; without it the strip reads as decorative noise.
   *Fix hint:* swap `theme.accent_primary` → `colors.semantic.success`
   for the sparkline + CPU label; add per-metric colour mapping in
   `topbar_chrome.rs:632-711`.

## Top 3 functional deltas (gestures concept does, we don't)

1. **Maximized "auto-hide / mouse-edge reveal" controls.** Concept
   `WindowFrame.tsx:108-123` only shows the floating title pill +
   restore/close buttons when `clientY ≤ 80`; otherwise the chrome
   is hidden. Ours always draws them while maximized. Loses the
   "true full-bleed" feel.
2. **DragPreview mock window at cursor when dragging a file.**
   Concept `DesktopView.tsx:461-530` paints a mini Files-explorer
   or Editor mock under the cursor as the user drags. Ours just
   shows the OS native drag image. A concept-iconic affordance.
3. **QuickSettings drill-in / back-chevron.** Concept's `nav_stack`
   pushes a Wi-Fi-list level on click, with a back-chevron header
   (`QuickSettings.tsx:108-128`). Our `QuickSettings::nav_stack`
   field exists but isn't rendered — clicking the WiFi/BT toggles
   only flips the boolean in `ChromeState::quick_settings`.
