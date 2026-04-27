# Nirios Shell — Parity Status

Current state of the Rust implementation vs. the concept described in
`docs/nirios/concept.md`. Status legend:

- ✅ **Done** — behaves as concept describes.
- 🟡 **Built but not wired** — widget exists in `quick-ui`/shell, no render path.
- 🟧 **Partial** — minimum stubbed-in, visually off from concept.
- 🟥 **Missing** — no implementation yet.

## 1. Overall layout

| Item | Status | Notes |
|---|---|---|
| Desktop view (sidebar + strips + groups) | ✅ | Top bar + left in-plane `SidebarWheelDisc` + one horizontal Row per strip (groups tile left-to-right). Strips stack vertically inside the content Column. Background now painted by `DesktopBackdrop` (slate-900→slate-950 diagonal gradient + 24×24 px grid at `rgba(128,128,128,0.07)`, matching `DesktopView.tsx:210-217`). Noise-SVG overlay deferred — needs image asset pipeline. |
| Overview view | 🟧 Partial | Per `concept-coverage.md` §"OverviewView.tsx → overlay_host.rs::draw_overview": vertical workspace list shipped, but several concept affordances are still stubbed. (1) Header h1 in concept (`OverviewView.tsx:48`) is `text-4xl font-bold tracking-tight`; our `overlay_host.rs:1903-1920` honours size + weight but `tracking-tight` (-0.025em) is unimplemented (no letter-spacing token). (2) Subtitle (`:49`) is `text-slate-400 text-lg` (18 px Medium); ours (`:1925`) uses 16 px Regular — two pixels short, one weight tier off. (3) WS-N pill text (concept `:77`) is `font-mono`; our `:2104-2133` paints with the default proportional face despite `text_render.rs` exposing a `font_mono` axis. (4) Permissions panel: concept (`:83-103`) renders shield + label + Settings2 gear button + 2 badge chips ("N Roles" / "N Files") at `bg-blue-500/20` / `bg-purple-500/20`; ours paints a single chip + static shield, omits role-count / file-count badges and the gear button — material loss of information density. (5) Collaborator stack per row (concept `:111-124`): up to N avatars `-space-x-2` + invite "+" button on every row; ours doesn't render workspace collaborators in the overview path at all. (6) Mini-window previews: concept `:6-32` paints a dual-tab-row pill strip atop a slate-800 card body; our `draw_mini_window` (`overlay_host.rs:1776`) paints a flat rectangle without the tab-strip-on-top affordance. Active-row 4-ring bloom + rail dot remain ✅ approximations. |
| Z-ordered overlay host | ✅ | `ShellOverlayHost` draws palette, overview, quick-settings, workspace-settings + scrim above content. |
| View-swap animation | ✅ | `ShellOverlayHost` crossfades desktop ↔ overview via a sequential two-phase animation matching the concept's `AnimatePresence mode="wait"` wrapper (`App.tsx:427-487`). Total duration 400 ms. Phase 1 (t ∈ [0, 0.5]): outgoing view fades + shrinks from `scale 1.0 → 0.95` anchored to scene centre. Phase 2 (t ∈ [0.5, 1.0]): incoming view fades + zooms from `0.95 → 1.0`. Each half eased with smoothstep `p*p*(3-2p)`; the split happens in the pure helper `compose_overlay_transition(t, target)`. Leaving overview reverses the phase order (overview shrink-out then desktop zoom-in). `ShellOverlayHost::needs_animation_frame()` returns true while `transition_started.is_some()` so `App` pumps redraws in `AboutToWait` until the curve settles — without this the overview stalled at `t ≈ 0` after the click and the button appeared non-functional. 6 unit tests cover idle / at-rest composition / half-split scales at t=0.25 & 0.75 / reverse-phase scales / mid-flight re-toggle plus a full-root click E2E and an animation-frame-pump E2E. |
| Workspace slide-swap animation | ✅ | `ShellOverlayHost::tick_workspace_slide` latches a 260 ms ease-out-cubic vertical slide when the chrome's active workspace id diverges from `last_active_ws`. Direction is resolved by comparing workspace indices on the tree: later → slide in from below (+y), earlier → from above (-y). The draw path applies the offset to the content widget only via `Affine::translate`, so overlay stack + scrim + demo peer stay fixed while the workspace body slides. `needs_animation_frame()` bubbles the in-flight signal so the event loop keeps pumping redraws until the slide settles to zero. 3 unit tests cover start-on-switch, self-clears-after-duration, and direction-follows-index. |

## 2. Top bar

| Item | Status | Notes |
|---|---|---|
| 40 px fixed bar | ✅ | `TopBarChrome` renders a 40 px overlay-bg bar; wired into `build_nirios_root`. |
| Overview toggle button | ✅ | Grid icon click toggles `overlays.overview`. |
| Up/down chevrons | ✅ | Clicks switch workspace via `chrome.switch_workspace(±1)`; greyed at bounds. |
| Workspace-name button + gear | ✅ | Gear click toggles `overlays.ws_settings`. |
| Center widget carousel | 🟧 Partial | Per `concept-coverage.md` §"TopBarWidgets.tsx → topbar_chrome.rs": all three variants + dot-click + scroll-wheel cycling are wired, but visual idiom diverges materially. (1) Band size: concept `TopBarWidgets.tsx:37` is `w-[480px] h-9`; ours `topbar_chrome.rs:542-543` is 360×28 — 120 px narrower, 8 px shorter. (2) Hover state: concept has `hover:bg-white/10 hover:shadow-[0_8px_32px]`; ours has none. (3) Indicator dots: concept `:48-54` uses `bg-indigo-400` (= `accent[6]` in our ramp); ours uses `accent[5]` (indigo-500) — one shade too saturated for the dark bar. (4) SystemMonitor sparkline: concept `:120-124` is `bg-emerald-500/30`; ours uses `theme.accent_primary` (indigo) — wrong colour family. (5) SystemMonitor labels: concept paints CPU `text-emerald-400`, RAM `text-blue-400`, TMP `text-orange-400`; ours uses CPU+RAM `accent[5]` (indigo) + TMP `text_dim`, so the per-metric semantic-colour cue is lost (which is the whole point of the strip). |
| Collaborator avatar stack | ✅ | Up to 3 **per-peer-coloured** initial circles with 4 px overlap and 2 px black separator rings between stacked avatars; each fill comes from `Collaborator::cursor_color` (hex override) or the `quick_ui::presence::user_color` palette-pinned fallback (indigo/pink/orange/emerald for `me`/`alice`/`bob`/`charlie`, stable FNV→HSL for unknown ids). 4+ peers collapse into a trailing `+N` chip on the raised surface, still in the neutral `surfaces.raised` tint so it reads as "count" not "person". Gap #9 shipped in `730e6c4`. |
| Quick-settings tray pill | ✅ | 3-dot click toggles `overlays.quick_settings`. |

## 3. Workspace strip / sidebar wheel

| Item | Status | Notes |
|---|---|---|
| Left 96 px vertical strip | 🟧 Superseded | The user rejected the permanent sidebar pattern (2026-04-21): *"No permanent sidebar, but workspaces appear as rotated axes of the wheel disc, meaning you can go to the left and drag it to move between workspaces."* `SidebarWheelChrome` is no longer mounted in the live shell; `SidebarWheelDisc` (below) takes its place. The module file is preserved as reference but the `w-24 z-40` column is gone. |
| 3D wheel rotation | ✅ | **`SidebarWheelDisc`** (`quick-shell/src/shell/sidebar_wheel_disc.rs`, commit `2337b21`). 240 px circular disc, mounted in-plane with the workspace content (not as a chrome column). Each workspace appears as a radial **spoke**; labels are painted at the spoke's tip, with the active spoke in `text_primary` + a 24° indigo arc glow on the rim. Press-and-drag rotates spokes continuously (`rotation_deg = origin + dy * deg_per_pixel`); release snaps `rotation_deg` to the nearest spoke and commits a new `active_idx`. Central hub dot + crosshair reads as a grabbable control. Unit tests: `new_starts_with_active_idx_zero_and_rotation_zero`, `drag_and_release_snaps_to_nearest_spoke`, `drag_without_release_keeps_rotation_continuous`, `single_strip_disc_never_snaps_past_itself`. Snapshot: `target/nirios-screenshots/wheel-disc-01-baseline.png`. CRDT write-back wired in commit `65d105d` — `with_chrome(Rc<RefCell<ChromeState>>)` + `chrome.borrow_mut().tree.set_active(ws_id)` on snap, covered by `snap_publishes_active_workspace_to_chrome`. Labels in (90°, 270°) auto-rotate by π so glyphs stay upright. A dedicated **Plus slot** below the hub (commit `dc8956a`) calls `ChromeState::add_workspace` on press — the "add workspace" affordance the concept's wheel had as a trailing "+" card. |
| Per-strip label + avatars | ✅ | Workspace name + card chrome shown; every card paints up to 3 presence dots (overflow as "+N") read from its own `Workspace::collaborators`. Inactive cards render at 55% alpha so the active card still anchors focus. Tree-level collaborators only backfill the active card when the workspace itself has no per-ws roster (demo-tree convenience). |
| Scroll/pan to switch strips | ✅ | Scroll-wheel over the sidebar accumulates line-units; crossing ±1 calls `ChromeState::switch_workspace`. Click-to-activate a workspace card + plus-button-append-and-activate are both wired. |
| Rim / gradient line on expand | 🟧 Superseded | The concept's edge-fade was a feature of the 96 px rectangular `SidebarWheelChrome` column. The new `SidebarWheelDisc` is a circular widget without vertical edges; the equivalent rim-emphasis is the 24° accent arc glow painted at the active spoke's angle (`sidebar_wheel_disc.rs`). Module `sidebar_wheel_chrome.rs` remains in-tree for reference but is not mounted. |
| Current "workspace 1/1" text | ✅ | WorkspaceView renders it. |

## 4. Window groups, tabs, frames

| Item | Status | Notes |
|---|---|---|
| Groups inside a horizontal strip | ✅ | Each strip's groups live in a `Row` inside the content Column, so groups tile left-to-right; multiple strips stack vertically. |
| Width presets (narrow/normal/wide/custom) | ✅ | `WindowWidth` enum + clamped 300–1200. |
| Right-edge resize handle | ✅ | `WindowGroup` renders handle, hover + drag work. |
| Maximized state | 🟧 Partial | Floating chrome pill + `[Search][Restore][×]` cluster are drawn while maximized, glyphs use `quick_ui::icons::draw_icon` on a 24×24 viewBox (commit `62c5c53`). Per `concept-coverage.md` §"WindowFrame.tsx (maximized chrome)" + Top-3 functional deltas #1: concept `WindowFrame.tsx:108-123` only reveals the floating title pill + restore/close buttons when `clientY ≤ 80`; otherwise the chrome auto-hides for a true full-bleed reading. Ours always draws them while maximized — the concept's deliberate "ambient maximized chrome" feel is lost (regressed affordance). |
| Tab bar height (concept 56 px container / 30 px pills) | ✅ | Bar leaf is now `TAB_BAR_HEIGHT = 40 px` with pills inset `TAB_PILL_INSET_Y = 6 px` top/bot, so the pills themselves sit at 28 px (rounded-full via `pill_height / 2`) on a 40 px outer bar. Close / search / maximize hit rects inherit the pill y-range so hover affordances stay correctly centered. Presence dots hang 2 px below the pill inside the new breathing room (no overflow into the white-card frame). Concept's `h-14` (56 px) is visually taller still; we run 40 px so the pill chrome doesn't dominate the workspace area — a deliberate, documented deviation. Drop-index hit tests updated for the new geometry in `widget::tabs::tests::drop_index_for_uses_pill_midpoints`. |
| Tab pill styling | 🟧 Partial | Rounded-full pills + active/hover/inactive alpha ramp + close glyph + leading app-type icon (`draw_app_icon` in `quick-ui/src/widget/tabs.rs` — Terminal/Files/Editor/Browser/Settings glyphs map 1:1 to concept's lucide icons in `WindowFrame.tsx:10-18`). Concept tab spawn motion (`WindowFrame.tsx:284-288`) is `opacity 0→1, y -10→0, scale 0.9→1`; ours (`tabs.rs:46-51`, `INSERT_FADE_*`) animates opacity + y only (scale skipped — documented deviation, see Highest-leverage gap #2). Per `concept-coverage.md` §"WindowFrame.tsx → tabs.rs / window_group.rs / tile_host.rs" the bigger gap is the **foggy collaborator indicator**: concept `WindowFrame.tsx:299-310` paints a `w-16 h-16 rounded-full blur-xl` translated half-down per peer with `mix-blend-screen` — reads as a glowing tinted halo at the pill bottom. Ours (`tabs.rs:419-438`) paints small flat low-alpha filled circles — no blur, no `mix-blend-screen`, "not foggy at all". Pill-shadow approximation (`tabs.rs:316-338`) uses 4 down-translated rings vs concept's true `shadow-[0_8px_32px_rgba(0,0,0,0.2)]` — softer than the real blur. |
| Tab layout spring | ✅ | `TabBar::reorder`, `insert_tab`, and `remove_tab` all call `snapshot_layout_for_spring`, which captures the current `tab_rects` keyed by tab id and latches `layout_anim_started`. `TabBar::draw` interpolates each pill's visual rect from its snapshot rect to `tab_rects[i]` over 180 ms with ease-out-cubic; close/max/search hit-visuals are offset by the per-tab delta so their glyphs track the moving pill. New tabs (not in the snapshot) render at target immediately so they pop in opaque — matches concept's AnimatePresence for layout-only motion. Hit-testing stays on target rects so interaction isn't delayed mid-spring. `needs_animation_frame()` ORs the spring's in-flight signal with the existing drop-zone curve. Covered by 4 unit tests (`reorder_starts_layout_spring_and_snapshots_prev_rects`, `insert_tab_triggers_layout_spring`, `remove_tab_triggers_layout_spring`, `layout_spring_clears_itself_after_duration`). |
| Tab drop zones + structural-rebuild | ✅ | `GroupRow::split_tab_into_new_group` used to insert a fresh `WindowGroup` into `self.groups` at drop time, but the taffy layout tree was only built once at App startup — the newly-inserted group had no `NodeId`, was absent from `App::node_map`, and `GroupRow::apply_layout` skipped it, so drop-to-create rendered nothing in real use despite passing unit tests. Fixed: `Widget` trait gained `take_structure_dirty(&mut self) -> bool` (default false); `GroupRow` sets its own flag after the insert; container widgets (`Column`, `Row`, `Stack`, `Container`, `Scrollable`, `TileHost`, `WindowGroup`) forward the query to their children so nested mutations bubble to the root. `App` polls once per `RedrawRequested` and once per `AboutToWait`; when dirty it drops taffy + node_map and rebuilds from `root_widget.build_layout`, then flags `layout_dirty` so the next `compute_layout` pass picks up the new tree. Regression test in `quick-ui/src/widget/group_row.rs` drives the split and asserts `take_structure_dirty()` reports true exactly once (clears on read). Drag-to-reorder itself: while a drag is in flight `TabBar::draw` paints a `TabDropZone`-style indicator in **every inter-pill gap** (N+1 bars for N pills, 2 px idle) so the user sees every legal drop location, and the gap the cursor is over animates from 2 px to 18 px with an accent-indigo glow over 140 ms via an ease-out-cubic curve — the Rust port of `nirios-concept/src/components/WindowFrame.tsx`'s `motion.div layout` `w-2`→`w-16` transition. Animation state lives on the bar (`drop_zone_target` + `drop_zone_animated_at: Option<Instant>`); `TabBar::needs_animation_frame()` returns true while the curve is still interpolating, `WindowGroup` + `GroupRow` bubble that signal up, and `ShellOverlayHost::needs_animation_frame()` ORs it with the overview crossfade so the app's `AboutToWait` pump keeps redraws flowing until the zone settles. Covered by `widget::tabs::tests::drop_zone_target_tracks_cursor_during_drag` (asserts cursor-tracks-target mid-drag + release clears target + idle state stops pumping frames). Cross-group moves land through the `GroupRow` widget in `quick-ui/src/widget/group_row.rs`: `TabBar` publishes `pending_detach` when a release falls outside its own rect, `GroupRow::handle_event` post-dispatch-sweeps sibling groups and hit-tests the release point against each `bar_rect()`, splicing the detached tab + its `TileHost` into the target (the dragged tab becomes active in its new group; the source promotes a neighbour). Drop index is resolved via `WindowGroup::drop_index_for` → `TabBar::drop_index_for` (pill-midpoint hit-tests). **Inter-group gap drops** (concept's `DropZone` between groups in `DesktopView.tsx:414`): `GroupRow::gap_index_for(release_point)` classifies a release as 0..=N when the cursor sits between two groups (or before/after the row); `GroupRow::split_tab_into_new_group(src, tab_id, gap_idx)` calls `WindowGroup::from_detached` to spawn a fresh group carrying just the dragged tab and splices it at `gap_idx`, pruning the source group when its last tab was pulled out. The degenerate "drop a single-tab group's only tab back next to itself" case is rejected so it doesn't visually flash-destroy-recreate the same slot. Covered by 5 `group_row::tests` cases. |
| Tab-level collaborator blur indicators | ✅ | See §7 — `TabBar::draw` paints per-peer colour dots at the bottom of each pill via `PresenceCtx::peers_on(PresenceKey::Tab { id })`; wired to both startup-seeded + live-refreshed presence. |
| Window frame white rounded card | ✅ | `TileHost::draw` paints `surfaces.window_card` (WHITE) with `rounded-xl` radius + `white/20` border + shadow rings (code in `quick-ui/src/widget/tile_host.rs:75-109`). Previously invisible in live shell because `GroupRow::build_layout` used `height: auto` with no `flex_grow`, so inside the parent Column it measured only as tall as its 40 px `TabBar` — forcing each `WindowGroup`'s `height: percent(1.0)` to resolve against 40 px and leaving zero for `TileHost`. Fixed by setting `flex_grow: 1.0` + `width: percent(1.0)` on the `GroupRow` flex node (commit `2d7f5fe`). `TileHost` rect went from `600×0` → `600×416`; `target/nirios-screenshots/03-desktop-card-fix.png` now shows the white rounded card with readable content. |
| Drag-over / search-active borders | 🟧 Partial | Drag-over: 1.5 px indigo-500 @ 60% alpha ring while `TabBar::drag_in_flight()` ✅. Search-active glow: per `concept-coverage.md` §"WindowFrame.tsx" + spot-check #5, concept's `WindowFrame.tsx:359-360` is `ring-4 ring-indigo-500/50 shadow-[0_0_60px_rgba(99,102,241,0.6)]` — a 60 px soft bloom around the whole card. Ours paints two concentric indigo strokes (8 px @ 24% outer, 4 px @ 50% inner) — close on the ring but **lacks the 60 px bloom**. The precomputed `indigo_bloom_sprite` (`tile_host.rs:100-103`) exists but is wired only to `TileHost::focused`; the search-active draw path doesn't compose it. Should be wired in to recover concept parity. |

## 5. Apps

| Item | Status | Notes |
|---|---|---|
| Notes | ✅ | `NotesApp` is a CRDT-backed plaintext editor; soft-wraps, supports Enter/Backspace/Arrows/Home/End; shown as the active tile in the default Home workspace. |
| Terminal | ✅ | Real PTY via `quick-terminal`. Dark `TerminalPalette` is now tuned to the concept's `TerminalApp.tsx`: pure-black background, green-400 (#4ade80) default foreground, ANSI 1..=7 anchored on Tailwind's `-400` ramp (red/yellow/blue/purple/cyan + slate-300 neutrals), bright 8..=15 on the `-300` ramp. Palette swap on theme change is already covered by `terminal_ansi_mapping_tracks_palette_swaps`. |
| Files | 🟧 Partial | Sidebar tiers + sequential ids match concept `FilesApp.tsx:94/127` ✅. Per `concept-coverage.md` row "apps/FilesApp.tsx → apps/file-explorer/" the concept also paints a `bg-white/80 backdrop-blur-xl` translucent panel + per-folder hover scale + draggable file/folder grid w/ `Folder fill-blue-500/10` and per-shortcut context-menu remove that aren't all confirmed in our submodule port (audit was scoped out of deep-auditing the submodule from the superproject). |
| Browser | ✅ | `BrowserStubWidget::draw` paints concept's browser chrome: white card, 48 px header with 3 slate traffic-light circles + centered globe-plus-URL address pill, and an indigo "App Preview" tile with headline/subheadline in the body. Still a stub — no real web engine — but visually matches `nirios-concept/src/components/apps/BrowserApp.tsx`. Concept's `rotate-12` on the inner indigo square (`BrowserApp.tsx:27-33`) is the only documented delta — minor; audit (`concept-coverage.md` spot-check #3) marks this row "✅ accurate enough". |
| Editor | 🟧 Partial | Functionally ✅ — full CRDT-backed editor with token-coloured syntax + arrow-key column-preserve + presence-driven peer caret/selection (a real improvement over concept's frozen mock). Per `concept-coverage.md` §"apps/EditorApp.tsx → editor_app.rs" the **visual delta is the most jarring in the system**: concept paints a `bg-[#1e1e2e] text-[#cdd6f4]` Catppuccin Mocha dark snippet with hand-coloured spans (`#cba6f7` keywords, `#89b4fa` idents, `#a6e3a1` strings, `#f9e2af` attrs, `#6c7086` line-numbers, `#313244` divider). Ours runs on the **white window-card surface** with theme tokens (slate-900 body, indigo keywords, emerald strings, amber numbers) — white-on-light vs concept's dark Catppuccin. Trade-off per `CLAUDE.md`'s OS-mandated theme rule. |

## 6. Overlays / modals

| Item | Status | Notes |
|---|---|---|
| Overlay host in root | ✅ | `ShellOverlayHost` wraps the root widget, owns visibility flags, draws scrim + overlays. |
| Global search (⌘K) | 🟧 Partial | ⌘K opens the overlay-host palette + scrim + scope banner; Enter / left-click dispatch on action ids (`overview.toggle`, `settings.open`, etc.) and close. Per `concept-coverage.md` §"GlobalSearch.tsx → command_palette.rs + search.rs": (1) palette card radius is `radii.md` = 8 px (`command_palette.rs:239`); concept `:113` is `rounded-3xl` = 24 px — **palette is 1/3 the concept radius**. (2) Input row: concept `:138-153` has leading lucide `Search` glyph + input + trailing `Cmd-K` kbd chip in `bg-white/5`; ours has just placeholder text in a neutral inset rect — no search icon, no kbd chip. (3) Filter strip: concept `:184-244` adds scope chips + filetype multi-select + Full-Text toggle + Replace toggle; ours draws nothing. (4) Selected row: concept `:251` is `bg-indigo-500 text-white`; ours (`command_palette.rs:308-316`) highlights with `neutral[4]` (mid-grey) — wrong colour. (5) Per-row icon container: concept `:258` is `p-2 rounded-xl bg-white/20` on selected; ours has no icon at all on rows. (6) Footer: concept `:274-280` paints kbd chips for ↑↓ Navigate / Enter Select + scope label; ours has none. |
| Search context banner / scope radios | 🟧 Partial | Workspace/All pills do toggle `ChromeState::search_scope` ✅. Per `concept-coverage.md` §"GlobalSearch.tsx" the concept context banner (`GlobalSearch.tsx:115-135`) is a different idiom entirely: when scoped to an app it paints a `bg-indigo-500/20 border-b border-indigo-500/30` strip above the input with the **app icon + "Searching in {title}" label + a "Switch to Global" link**. Ours (`overlay_host.rs:2774` `draw_search_banner`) paints two scope pills outside the palette card — same data shape, completely different concept affordance. |
| Overview view modal | ✅ | Drawn on `overlays.overview`; cards clickable; Esc closes. |
| Quick settings dropdown | 🟧 Partial | Sliders + toggles flip state ✅ functionally, but per `concept-coverage.md` §"QuickSettings.tsx → overlay_host.rs::draw_quick_settings" + Highest-impact delta #4 the visual idiom is fundamentally different. (1) Popover: concept `QuickSettings.tsx:169` is `w-80 rounded-3xl` (24 px); ours uses `radii.lg` = 12 px — **half the concept radius**. (2) Top action row: concept `:181-201` paints 3 equal-width buttons (Settings / Camera / red-tinted Power); ours has none. (3) Slider container: concept `:204-215` wraps both sliders in a `bg-white/5 p-3 rounded-2xl` card with leading Sun/Volume2 icons + `h-1.5` thin `bg-white` ramps; ours (`overlay_host.rs:2907-2926`) paints two stand-alone 6 px progress bars on slate-800 with `accent[5]` indigo fill — wrong colour, no card, no leading icons. (4) Toggle grid: concept `:218-246` is two **indigo-500-filled** rounded buttons (`bg-indigo-500 hover:bg-indigo-600`) with glyph + chevron + label + connection-name sub-line ("Home_Network" / "AirPods Pro"); ours paints label + iOS-style on/off pill — completely different idiom. (5) Power Profile row (`:249-258`): missing entirely. (6) WiFi/BT drill-in lists (`:101-162`): the `nav_stack` field on `QuickSettings` is dead state — clicking toggles only flips the boolean. Top-3 functional delta #3. |
| Workspace settings modal | 🟧 Partial | **Largest gap in the codebase per `concept-coverage.md` §"WorkspaceSettingsModal.tsx" + Highest-impact delta #1.** Rename Name input works ✅. Everything else is stubbed: (1) Modal frame: concept `WorkspaceSettingsModal.tsx:43` is `max-w-4xl h-[600px] bg-slate-900/70 rounded-3xl flex` (≈896×600); ours (`overlay_host.rs::draw_ws_settings`) is fixed 520×360 with no internal split. (2) Left sidebar: concept `:49-91` renders a 256 px sidebar (workspace identity card top w/ 8×8 indigo first-letter square + "Workspace Settings" caption, 3 nav buttons General / Roles & Permissions / Files, footer "Close Settings" row); ours has no sidebar at all — the four "tabs" are flat text pills with the first one always active. (3) General tab: concept `:99-149` renders `Invite Link` field + Copy button + `Collaborators (N)` grid (avatar + name + role chip + Remove per row); our `workspace_settings.rs:340-380` only draws the Name input. The `invite_link` API in `workspace_settings.rs:147` is never drawn. (4) Roles tab: concept `:152-254` renders role list with colored swatches + isDefault chip + per-role app-permissions toggle grid (5 apps × on/off) + per-role file-permissions radio strip (none/read/write); not drawn — `apps_table` + `files_table` `DataTable` state in `workspace_settings.rs:120-180` is unused by the draw path. (5) Files tab: concept `:257-303` renders file grid with icon + name + type badge + assigned-roles avatar pile + Remove; not drawn. The whole modal is essentially a rename dialog with stub tabs (spot-check #1 in audit). |
| Outside-click close | ✅ | Clicks outside the palette / quick-settings / ws-settings rect dismiss them; an in-flight name edit commits before the modal closes. |
| Collaborator dropdown | ✅ | Click on the top-bar avatar stack toggles `overlays.collaborators`; `ShellOverlayHost::draw_collaborators` paints a 260×(48+36·N) px popover with an initial-circle + name per peer. Outside-click + Esc dismiss; empty roster shows a "No peers connected" hint. |
| Maximized-window floating controls | ✅ | `WindowGroup::draw` paints title pill + `[Restore][×]` cluster on top of a maximized group; Restore + close hit-tests live on `restore_rect`/`close_rect`. Hover state on close tints `semantic.danger`. |

## 7. Collaboration chrome

| Item | Status | Notes |
|---|---|---|
| Remote-cursor overlay | 🟧 Partial | Demo peer ("Ada") + real-peer fan-out + workspace-key filtering all wired ✅ functionally. Per `concept-coverage.md` §"CursorOverlay.tsx → remote_cursors.rs" the visual idiom diverges: (1) Caret glyph: concept `CursorOverlay.tsx:19-22` is the lucide `MousePointer2` SVG outline `fill: peerColor; color: white` — the white outer stroke is what makes peer cursors readable on any background. Ours (`remote_cursors.rs:184-195`, `overlay_host.rs:2643-2648`) paints a small filled BezPath triangle with **no outline** — reads weaker on light backgrounds. (2) Name chip: concept `:23-28` is `bg-{peerColor} text-white px-2 py-0.5 rounded-full text-[10px] font-bold` — **the chip background IS the peer colour itself, the chip is the peer's badge**. Ours (`remote_cursors.rs:200-251`) uses neutral `surfaces.raised` background with peer-coloured underline + 1 px border — generic raised-surface popover, not a peer-coloured badge. Clear visual departure. |
| Bidirectional presence pipeline | ✅ | `CollabRequest::Presence { key, payload }` is the outbound counterpart of `CollabEvent::RemotePresenceUpdate`. `App::broadcast_presence` is the public entry point the shell / widgets call when local presence changes (cursor, caret, tab focus). The test `MemoryRelay::pump` translates Presence → RemotePresenceUpdate, attributing it to the sender's id, so the relay models what the real compositor socket will do. Round-trip tested via `multi_user_session::memory_relay_fans_out_presence_as_remote_presence_update` and `multi_user_session::app_broadcast_presence_completes_loop_to_remote_peer` — the latter drives a full alice→relay→bob flow and asserts bob's `PresenceCtx::caret_for` picks up alice's attributed caret, while alice's own context stays empty (sender-suppression). |
| CRDT incremental delta merge | ✅ | Before this landed, `CollabEvent::CRDTIncremental` was a no-op: the App branch just flipped `needs_sync` without touching the doc, so peers broadcasting via `CollabRequest::SyncIncremental` would fan out but the bytes would drop on the receiver floor. `State::load_incremental` now delegates to `AutoCommit::load_incremental` and refreshes `last_snapshot` so the undo baseline stays aligned with the merged state; the App loop wires it in (bad bytes silently ignored so a noisy peer can't brick the local doc). `State::save_incremental` is the symmetric accessor. End-to-end covered by `multi_user_session::crdt_incremental_deltas_replay_on_remote_peer`: alice mutates, serializes the delta, the relay fans out CRDTIncremental, bob merges, both docs converge on the same scalar. |
| Live pointer broadcast from App event loop | ✅ | `App::set_presence_broadcast_scope(Some(key))` turns on an outbound pointer relay wired directly into winit's `CursorMoved` / `CursorLeft` branches. Wire rate is capped at ~30 Hz via a pure `pointer_broadcast_allowed(last, now, gap)` gate (unit-tested for first-call, in-gap block, at-boundary allow, past-gap allow) so winit's vsync-rate delivery can't saturate the Unix socket. CursorLeft fans out a `Left` payload so peers stop rendering the cursor when it exits the window. Scope changes reset the throttle. Shell `main.rs` seeds the scope to `PresenceKey::Workspace { id: tree.active }` so peer cursors attribute to the workspace they were observed on. Opt-in: headless tests and `quick-ui` examples leave the scope `None` and observe no behaviour change. |
| Top-bar avatar stack | ✅ | Top-bar paints up to 3 **per-peer-coloured** initial avatars + `+N` overflow chip, with a 2 px black separator ring between stacked circles (concept's `border-2 border-black`). Each fill resolves via `Collaborator::cursor_color` → `parse_hex_color` first, then the palette-pinned `user_color` fallback (`me`/`alice`/`bob`/`charlie` → indigo/pink/orange/emerald). `sync_from_tree` caches the resolved `Vec<Color>` in lockstep with `collaborator_initials`, re-runs on chrome revision bump. Gap #9 shipped in `730e6c4`. |
| Tab collaborator blur indicators | ✅ | `TabBar::draw` reads `PresenceCtx::peers_on(PresenceKey::Tab { id })`; for up to 4 peers it paints low-alpha color dots (stable FNV-style hash → HSL hue) at the bottom of each pill. Shell-side wiring landed via `Collaborator::active_tab: Option<u64>` + `shell::sync_tab_presence_from_tree(&tree, &mut ctx)` (called from `main.rs` before the app loop starts), so any workspace collaborator with an `active_tab` attribution now materialises as a dot on the corresponding tab pill on first draw. Live refresh is wired too: `App::apply_remote_presence` routes `PresenceKey::Tab` `Arrived` payloads through `PresenceCtx::set_tab_focus`, which evicts the user from every other `Tab` key before registering them on the new one — so a peer switching tabs mid-session moves their dot instead of leaving ghosts. |
| File-explorer per-folder presence dots | ✅ | `SidebarFolderRow` (in `apps/file-explorer/src/sidebar_row.rs`) replaces the plain Button for each favorites/system entry and paints up to 3 peer-coloured 3 px dots (overlap `-space-x-1`) on the right of the pill, keyed on `PresenceKey::App { app: FileExplorer, sub: <path> }`. Active row paints indigo-500/10 background + full-alpha label; inactive rows fade to 55%. `peer_dot_colors` honours `PresenceCtx::color_for` (palette + per-peer overrides). Mirrors `FilesApp.tsx:101-111`. |
| Editor collaborator caret+selection | ✅ | `EditorApp::draw` reads `PresenceCtx::carets` / `selections` keyed by `PresenceKey::Widget { [Field("editor_body")] }`. Char-index carets are mapped to byte offsets via `char_indices`, then positioned with `tr.measure` on the prefix of the owning line; multi-line selections emit one rect per intersecting row so `draw_peer_selection` paints a clean per-line band. Presence colours unify with tab dots + remote cursors via `presence::color_for` → `user_color`, so alice stays pink everywhere. |

## 8. Data model

| Item | Status | Notes |
|---|---|---|
| Workspace / Strip / WindowGroup / Tab | ✅ | `quick-shell/src/shell/state.rs`, mirrors concept. |
| Roles + RolePermissions | ✅ | `Role`, `WorkspaceFile` exist. |
| Collaborator (id, name, cursor color, etc.) | ✅ | Rust `Collaborator` now carries `id`, `name`, `active_tab: Option<u64>` (concept's `activeAppId`), `cursor_color: Option<String>` (hex override; falls through to `user_color` palette), `avatar: Option<String>` (URL or initials fragment) and `role_id: Option<String>` (references the workspace's `Role` table). Builder chain: `Collaborator::new` / `with_active_tab` / `with_cursor_color` / `with_avatar` / `with_role`; all new fields are `#[serde(default)]` so pre-bump persisted trees still load. 8 unit tests in `shell::state::collaborator_tests`. Runtime `cursor` position is modelled out-of-struct via `PresenceCtx::pointers`. |
| App types | ✅ | `AppKind::{Terminal,Files,Editor,Browser,Settings}` — Editor now maps to `EditorApp`. |
| CRDT sync for apps | ✅ | Automerge `State::doc`, per-app `sync(doc, root)`. |

## 9. Visual tokens

The `quick-ui` theme exposes accent / semantic / neutral ramps, typography
scale, spacing, radii, elevation, motion presets, and a terminal palette.
The **default dark theme is now tuned to the concept's slate-900 / indigo-500
palette** — slate/indigo ramps match Tailwind's exact hex codes and the
surface alphas match the concept's `/70` and `/80` conventions.

| Item | Status | Notes |
|---|---|---|
| Accent indigo-500/600 | ✅ | Default dark theme's accent ramp is indigo-950→indigo-50 (index 5 = `#6366f1`); concept's indigo-400 / 500 / 600 sit at indices 6 / 5 / 4 and line up to `#818cf8` / `#6366f1` / `#4f46e5` exactly. |
| Slate-900 / slate-950 surface scheme | ✅ | Dark theme: `surfaces.base` = slate-900 (`#0f172a`), `surfaces.raised` = slate-800 (`#1e293b`), `surfaces.overlay` = `rgba(0,0,0,0.8)` (black/80, top-bar backdrop), `surfaces.glass` = `rgba(15,23,42,0.8)` (slate-900/80, overlay panels). Neutral ramp = slate-950→slate-50 at exact Tailwind hex values. |
| Per-collaborator fixed cursor colors | ✅ | `quick_ui::presence::user_color` pins the concept palette (`me` → indigo-500, `alice` → pink-500, `bob` → orange-500, `charlie` → emerald-500) and falls through to FNV→HSL for unknown ids. Tabs' presence dots and the shell's `SimulatedFeed` (now built via `new_with_user_id("alice", "Alice")`) both read from the same function, so one peer keeps one colour across every affordance. Arbitrary per-peer overrides are now wired too: `Collaborator::cursor_color` (hex) is seeded into `PresenceCtx::color_overrides` by `sync_tab_presence_from_tree`, and every render helper (`TabBar`, `draw_peer_caret`/`_selection`/`_pointer`/`_focus`) consults `PresenceCtx::color_for` before falling through to the palette. |
| Radii aligned to Tailwind steps | ✅ | `Radii::default()` now mirrors the concept's Tailwind radius scale: `sm=4` (`rounded`), `md=8` (`rounded-lg`), `lg=12` (`rounded-xl`), `xl=24` (`rounded-3xl`), `full=9999`. The window card (`TileHost` → `radii.xl`) now paints at 24 px to match the concept's `rounded-3xl` anchor. Only five slots exist, so the 16 px (`rounded-2xl`) step is intentionally absent. |
| Glow shadows for focus / drag-over | ✅ | Drag-over ring (1.5 px indigo-500 @ 60% while drag in flight) ✅. Search-active 60 px bloom **is wired**: `WindowGroup::draw` (`window_group.rs:541-548`) calls `host.set_focused(self.search_active)` which routes through `TileHost::focused` and composes the precomputed `indigo_bloom_sprite` (`tile_host.rs:396-411`). The visible two strokes are an additional `ring-4` on top of the bloom — concept TSX layers both (`shadow-[0_0_60px rgba(99,102,241,0.6)]` AND `ring-4 ring-indigo-500/50` in `WindowFrame.tsx:357-360`). Initially marked Partial after the 2026-04-28 audit; corrected back to ✅ after the spec-test pass discovered the bloom IS composed (see `concept-coverage.md` row §9 self-correction). |
| Framer-Motion-equivalent springs | ✅ | Motion tokens exist; `WorkspaceView` uses one. |

## 10. Interaction & state

| Item | Status | Notes |
|---|---|---|
| ⌘K / ⌘Tab / Esc | ✅ | ⌘K toggles palette; ⌘Tab toggles overview; Esc closes the top-most overlay. |
| Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z | ✅ | `quick-ui`'s App event loop wires undo (Ctrl+Z) + redo (Ctrl+Y or Ctrl+Shift+Z), which the shell inherits since it runs via `quick_ui::App`. **Widget metadata + focus survive the round-trip** (commit `b6aa0d0`): `State::undo`/`redo`/`undo_for_author` now accept `current_focused_idx` + `current_metadata` and push them onto the opposite stack, so Ctrl+Shift+Z rehydrates not just the text body but the caret position too. Caller in `App::run` reads `root_widget.get_state()` (non-mutating) + resolves the current focused node through `focusable_order` index lookup before calling. Window-management gestures (close/reorder/maximize) are still **outside undo scope** — `TabBar::sync` is empty, `WindowGroup::handle_event` mutates widget state directly without touching `state.doc`, so `commit_action()` finds no ops and `undo_stack` stays empty for those gestures. Promoting tab structure into the CRDT is the structural fix; tracked in `outstanding-work.md`. |
| ⌘↑ / ⌘↓ workspace switch | ✅ | Wired through `ShellOverlayHost` → `chrome.switch_workspace`. |
| Mouse wheel over strip | ✅ | 600 ms cooldown in `StripView`. |
| Tab click | ✅ | `TabBar` handles it. |
| Tab drag-to-reorder | ✅ | Drag starts after 4 px threshold, release hit-tests position within the bar and reorders. Releases outside the bar publish `TabBar::pending_detach`, which the parent `GroupRow` consumes to move the tab into a sibling group whose `bar_rect` contains the release point. |
| Tab close | ✅ | "×" glyph inside each pill's right edge. Click triggers a deferred 180 ms fade-out instead of synchronous removal: `TabBar::start_closing(id)` puts the pill into `closing_tabs` (snapshotting its desc + pill_rect + close_rect + started_at), `WindowGroup::closing_hosts` keeps the host alive for the same window, both layers fade their α 1→0 with a scale 1→0.95 + y 0→-20 transform matching the concept's `exit={{ opacity: 0, scale: 0.95, y: -20 }}` (commit `6ad564a`). Active-id shifts to the neighbour at close-start so the user never sees a "ghost active" frame. Hit-testing skips closing pills so a stale press can't reactivate a fading tab. Once the last tab fades out, `WindowGroup::start_closing()` latches a 180 ms group-level fade-out (α 1→0, scale 1→0.95) and `GroupRow` prunes the group only after `close_complete()` returns true (commit `3c733e6`). Maximized-mode close routes through the same deferred flow (`0c9a55d`); cross-group drops onto an `is_closing()` group fall through to new-group creation so the dragged tab isn't eaten by the fade. Close-glyph hover paints `theme.tokens.colors.semantic.danger.with_alpha_factor(0.8)` (red-500/80, concept's `hover:bg-red-500/80`) with full-white glyph promotion — commit `ee7f27f`. |
| Group resize drag | ✅ | Handle + pointer accumulation work. |
| Sidebar wheel scroll/pan | ✅ | Click-to-activate + plus-button-create + scroll-wheel accumulator (crosses ±1 on the wheel rect) all wired. |
| Hover reveals tab actions | ✅ | Close "×" sits in the pill always; hovering the pill additionally reveals a magnifying-glass (search) + maximize-icon to its left. Click search toggles `WindowGroup::search_active`, which paints the concept's indigo ring-4 + outer glow around the whole card. Click maximize toggles `WindowGroup::maximized`. Pin / split / move still not drawn — lower-priority concept affordances. |
| Outside-click closes dropdowns | ✅ | Click outside palette / quick-settings / ws-settings rect dismisses them; ws-settings commits a pending rename before closing. |
| File drag onto workspace | 🟧 Partial | Drop classifier + tree splicing wired ✅ functionally. Per `concept-coverage.md` §"DesktopView.tsx → mod.rs / sidebar_wheel_disc.rs / etc." + Top-3 functional delta #2: concept `DesktopView.tsx:461-530` paints a **DragPreview mock window** at the cursor position while a file is being dragged over the workspace — a mini Files-explorer or Editor mock showing what would open. Concept-iconic affordance, **absent in our shell** — we just show the OS-native drag image. |
| Spring view-swap | ✅ | Ease-out-cubic crossfade in `ShellOverlayHost` (200 ms; see §1). |

---

## Honest summary

Tally per the 2026-04-28 concept-coverage audit
(`docs/nirios/concept-coverage.md`). Numbers below come from that
audit's component matrix, NOT from the per-row table above (which is
larger because it splits sub-affordances).

- **Visual fidelity:** 0 ✅ / 7 🟡 / 4 🟥 / 0 🟧 across 11 concept components (matrix row "Totals", line 33).
- **Functional fidelity:** 2 ✅ / 6 🟡 / 4 🟥 across the same 11 components (matrix row "Totals", line 35).

The five highest-impact deltas (`concept-coverage.md` §"Highest-impact"):

1. **WorkspaceSettingsModal is a rename dialog with stub tabs** (§6 above).
2. **Sidebar wheel is a circular disc, not a left-pinned 3D dock** — intentional per user brief, but the single biggest pixel delta against concept screenshots.
3. **GlobalSearch palette is missing every concept affordance** (no leading icon, no Cmd-K kbd chip, no filter strip, grey selected row, 8 px instead of 24 px radius).
4. **QuickSettings is a flat list, not the concept's grid** (no 3-button action row, no slider card, no indigo-filled toggle cards, no Power Profile, no WiFi/BT drill-in).
5. **TopBar SystemMonitor uses indigo for everything instead of emerald (CPU) / blue (RAM) / orange (TMP)** — the per-metric semantic-colour cue is the strip's whole reason to exist.

The historical "Highest-leverage remaining gaps" section pre-dates the
audit and is preserved below for context, but the per-component matrix
(in `concept-coverage.md`) is now the source of truth — the rows in
this doc were corrected on 2026-04-28 to match the audit findings, with
several previously-✅ rows downgraded to 🟧 Partial:

- §1 Overview view (was ✅; mini-window previews + permissions chip-counts + per-row collaborator stack missing)
- §2 Center widget carousel (was ✅; band size + sparkline colour + per-metric semantic colours wrong)
- §4 Maximized state (was ✅; auto-hide on `clientY > 80` not implemented)
- §4 Tab pill styling (was ✅; foggy collaborator halo + true `shadow-[0_8px_32px]` not present)
- §4 Drag-over / search-active borders (was ✅; 60 px bloom not composed in search-active path)
- §5 Files (was ✅; submodule-deep audit deferred)
- §5 Editor (was ✅ visually; runs on white card vs concept's Catppuccin dark — trade-off per OS theme rule)
- §6 Global search (⌘K) (was ✅; palette is a flat 8 px-radius card with no concept affordances)
- §6 Search context banner / scope radios (was ✅; concept banner is an in-palette indigo strip, ours is two pills outside the card — different idiom)
- §6 Quick settings dropdown (was ✅; entire concept layout absent — see delta #4)
- §6 Workspace settings modal (was ✅; rename dialog only — see delta #1)
- §7 Remote-cursor overlay (was ✅; concept caret is outlined `MousePointer2` + name chip is peer-coloured background, ours is plain triangle + raised-surface popover)
- §10 File drag onto workspace (was ✅; DragPreview mock window absent — see functional delta #2)
- ~~§9 Glow shadows for focus / drag-over~~ — **reverted to ✅** after the spec-test pass found the bloom IS composed via `set_focused(self.search_active)` → `TileHost::focused`. The audit's original Partial claim was incorrect; corrected in the same commit that landed the spec tests.

## Highest-leverage remaining gaps (historical / pre-audit)

> **Note (2026-04-28):** This list was written when every row above
> was ✅. The 2026-04-28 concept-coverage audit downgraded ~15 rows
> to 🟧 Partial; the live source of truth for "what's left" is now
> `docs/nirios/concept-coverage.md` (specifically its
> "Highest-impact deltas" and "Top 3 functional deltas" sections).
> The infrastructure-level items below remain valid — they just
> don't represent the full gap set anymore.

1. **Screenshot-driven visual E2E** — ✅ **infra landed** in
   `quick-ui/tests/support/offscreen.rs`: a headless wgpu + vello
   renderer that copies texture bytes back to the CPU (handling the
   256-byte row-pitch alignment) so tests can assert pixel invariants
   or write PNGs without winit/a compositor. Smoke tests in
   `screenshot_test.rs` exercise the pipeline (solid bg + rect, PNG
   writeback with magic-byte verification, full `Container` widget,
   `TabBar` geometry guard). **Two journey-style E2E tests** now layer
   on top: `window_group_visual_journey.rs` drives a `WindowGroup`
   through four states (fresh → search-active → maximized →
   post-close) and asserts each produces a distinct FNV-1a pixel hash
   — so the indigo focus ring, the maximized floating title pill + ×,
   and the tab-close / layout-spring path all become pixel-regression
   guarded. `group_row_visual_journey.rs` does the same for
   cross-group chrome (baseline → transfer-tab → split-into-new-group)
   to cover the drag-routing and group-count-change paint paths.
   Every journey test skips gracefully without a wgpu adapter and
   writes one PNG per state when `QUICK_UI_SCREENSHOT_DIR` is set.
   A golden-image diff harness (with reference-set provenance +
   per-DPI bases + tolerance) is the next incremental step and is
   deferred as its own scope.

2. **Per-tab fade-in animation** — ✅ concept's `<motion.div>` tab
   spawn (`WindowFrame.tsx:284-288`) ported as a per-id `inserted_at`
   map on `TabBar`. `insert_tab` stamps a timestamp, `draw` wraps the
   pill paint in a `push_layer` with ease-out-cubic alpha + a
   vertical settle from `-10` to `0`, and `needs_animation_frame`
   reports true while any unexpired entry exists so `App` keeps
   pumping redraws over the 220 ms curve. Opt-in by construction —
   `TabBar::new` does not seed the map, so headless / screenshot
   tests stay deterministic as long as they don't trigger an insert.

3. **Per-window spawn fade-in animation** — ✅ concept's per-window
   `<motion.div>` spawn (`WindowFrame.tsx:201-205`, `initial={{ opacity: 0, scale: 0.95, y: 20 }}` → `animate={{ opacity: 1, scale: 1, y: 0 }}`)
   ported as a single `spawned_at: Option<Instant>` on `WindowGroup`
   plus the public `start_spawn_animation()` entry point. `draw`
   wraps the whole group paint in a `push_layer` alpha fade with an
   ease-out-cubic curve over 320 ms and applies both a 20 px → 0
   vertical settle and a 0.95 → 1.0 scale (about the group's center)
   via composed `Affine` transforms. `needs_animation_frame` returns
   true while the curve is in flight so redraws keep flowing.
   Wire-up: `GroupRow::split_tab_into_new_group` kicks off the fade
   on the freshly-detached group so tear-offs materialise instead
   of snapping in. Opt-in by construction — `WindowGroup::new` /
   `from_detached` leave `spawned_at = None`, so baseline tests /
   offscreen renders stay deterministic. Covered by
   `window_group::tests::start_spawn_animation_records_timestamp_and_pumps_frames`
   and `spawn_fade_window_clears_after_duration`.

3a. **Per-tile spawn fade-in animation** — ✅ commit `9fa86fd` adds
   `TileHost::start_spawn_animation()` for the inner-content layer
   (180 ms ease-out-cubic, α 0→1, scale 0.95→1.0). Caller wire-up
   in `WindowGroup::insert_detached_tab` (commit `f290b89`) covers
   both `GroupRow::move_tab_between_groups` and the drop-via-release
   path so any time a dragged-out tab lands inside an existing group
   its tile fades in. `split_tab_into_new_group` is deliberately
   skipped (group-level fade already plays — double-fade would be
   wrong). Cold-boot `WindowGroup::new` is also skipped (launch
   would blink).

3b. **Per-tile + per-group close fade-out** — ✅ commit `3c733e6`.
   `WindowGroup` gains `closing_at: Option<Instant>` + `start_closing()`
   + `close_complete()`; `draw` wraps the whole group paint in a
   `push_layer` alpha fade composed with a center-anchored scale
   (1.0→0.95) when `is_closing()`. Mutually exclusive with the spawn
   fade (close checked first, spawn falls through). `GroupRow` adds
   a per-tick prune sweep that drops groups whose `close_complete()`
   returns true and flags `structure_dirty`. Edge cases resolved in
   `0c9a55d`: maximized-close routes through the same deferred flow
   (no abrupt path); cross-group drops onto `is_closing()` siblings
   fall through to `split_tab_into_new_group` so the dragged tab
   never lands in a fading corpse. 6 new tests cover the full sweep.

4. **Shadow-2xl beneath window card** — ✅ concept's `shadow-2xl`
   on the white window card (`WindowFrame.tsx:357` `bg-white shadow-2xl`)
   ≈ `0 25px 50px -12px rgba(0, 0, 0, 0.25)`. Vello has no native
   gaussian blur, so `TileHost::draw` paints four progressively-larger,
   downward-translated, alpha-diminishing rounded rects behind the
   white card (24/18/10/4 alpha over 2/6/14/24 px spread offsets).
   Their union reads as a soft ~30 % drop shadow that matches the
   concept's perceived depth without needing a blur shader.

5. **Quick-settings popover entry animation** — ✅ concept's
   `QuickSettings.tsx:165-168` motion.div, `initial={{ opacity: 0, y: -10, scale: 0.95 }}` → `animate={{ opacity: 1, y: 0, scale: 1 }}`.
   `ShellOverlayHost` tracks `qs_opened_at: Option<Instant>` +
   `qs_was_open: bool` for rising-edge detection, wraps the popover
   paint in a `push_layer` alpha fade for 180 ms with ease-out-cubic,
   and composes a transform that scales 0.95 → 1.0 about the panel's
   top-center anchor plus a -10 → 0 px vertical settle. Idle draws
   short-circuit the matrix concat. `needs_animation_frame` ORs the
   in-flight signal so the app keeps pumping redraws until the
   curve settles; the timestamp clears on close so the next open
   replays the motion.

6. **Modal entry animations (ws-settings + palette)** — ✅ the two
   remaining concept `motion.div` panel wraps are now live:
   `WorkspaceSettingsModal.tsx:42-46` (start scale 0.95, y=20) and
   `GlobalSearch.tsx:109-113` (start scale 0.9, y=20). Both share a
   module-level `modal_open_curve(opened_at, start_scale)` helper
   that returns `(alpha, dy, scale)` sampled on ease-out-cubic over
   `MODAL_OPEN_ANIM_MS = 220 ms`. Each overlay adds its own
   rising-edge bookkeeping (`ws_settings_opened_at`/`_was_open`,
   `palette_opened_at`/`palette_was_visible`) and draws the scrim at
   full alpha while only the panel fades — matching the concept's
   "modal lifts out of the scrim" feel. `needs_animation_frame`
   includes both flags. Covered by 5 unit tests
   (`modal_curve_tests::{modal_open_curve_at_rest_when_no_timestamp,
   modal_open_curve_starts_from_initial_values,
   modal_open_curve_settles_at_rest_after_duration,
   palette_rising_edge_stamps_opened_at_and_pumps_frames,
   ws_settings_close_clears_opened_at}`).

7. **Sidebar card hover-scale** — 🟧 Superseded along with
   `SidebarWheelChrome`. The old rectangular workspace cards had a
   `scale(1.05)` hover lift; the new `SidebarWheelDisc` uses radial
   spokes instead of cards, so per-card hover-scale doesn't map onto
   the new widget. If a later iteration wants a hover cue on the disc,
   it would naturally apply to the spoke label (e.g. brightening
   `text_dim` → `text_primary`), not to any rectangular chrome.

8. **Overview card hover-scale** — ✅ concept's `OverviewView.tsx:66`
   `whileHover={{ scale: 1.01 }}` on each workspace card in the
   overview grid ported to `ShellOverlayHost::draw_overview`. Each
   card's rect is tested against `self.last_cursor`; when the cursor
   is inside, `card_xform = transform * T(cx,cy) * S(1.01) *
   T(-cx,-cy)` and the card fill/stroke, name, subtitle, preview
   tiles + tab strips, empty-workspace dashed placeholder + label
   all route through it. Subtler than the sidebar (1.01 vs 1.05) to
   match the concept's overview aesthetic where cards already sit in
   a spacious grid. `hit_test_overview` remains the single source of
   truth for overview card geometry; a new unit test
   (`overview_hit_test_returns_card_index_when_cursor_is_over_card`)
   guards the hit-test so a later refactor of the grid can't silently
   break the hover path.
