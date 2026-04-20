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
| Desktop view (sidebar + strips + groups) | ✅ | Top bar + left SidebarWheelChrome + one horizontal Row per strip (groups tile left-to-right). Strips stack vertically inside the content Column. Background now painted by `DesktopBackdrop` (slate-900→slate-950 diagonal gradient + 24×24 px grid at `rgba(128,128,128,0.07)`, matching `DesktopView.tsx:210-217`). Noise-SVG overlay deferred — needs image asset pipeline. |
| Overview view | ✅ | `ShellOverlayHost::draw_overview` renders a centered grid of workspace cards with preview tiles; click activates a workspace. |
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
| Center widget carousel | ✅ | 360×28 raised pill with 3-dot indicator on the left; Clock (HH:MM), SystemMonitor (CPU+sparkline+RAM+TMP), Calendar (Next event) variants draw in place. Scroll-wheel over the band cycles the active variant; clicking a dot jumps directly. |
| Collaborator avatar stack | ✅ | Up to 3 indigo-filled initial circles with 4 px overlap; 4+ peers collapse into a trailing `+N` chip on the raised surface. |
| Quick-settings tray pill | ✅ | 3-dot click toggles `overlays.quick_settings`. |

## 3. Workspace strip / sidebar wheel

| Item | Status | Notes |
|---|---|---|
| Left 96 px vertical strip | ✅ | `SidebarWheelChrome` is composed as a **floating overlay** above the wallpaper (not a column of a `SplitRow`), matching the concept's `DesktopView.tsx:219-248` `absolute left-0 top-0 bottom-0 w-24 z-40` wheel. Promoted to be an absolutely-positioned child of `ShellOverlayHost` (`with_sidebar`), offset 40 px below the top bar and 96 px wide. The opaque column fill + divider + gradient rim have been removed; the wheel now paints only a subtle `surfaces.raised @ 0.25` rounded backdrop behind the card stack plus a `border_standard @ 0.35` hairline stroke so it reads as a glassy floating pane rather than a chrome column. Event forwarding is now ordered sidebar-before-content in the overlay host so the wheel still gets first dibs on clicks landing in its rect. |
| 3D wheel rotation | ✅ | 2D vertical wheel with workspace cards, active highlight, + button. User brief explicitly accepted a 2D wheel animation in lieu of the concept's 3D "one workspace coming up from behind another" ("having an actual wheel-like 2d animation is fine too"), so treating this as parity-complete via accepted deviation. |
| Per-strip label + avatars | ✅ | Workspace name + card chrome shown; every card paints up to 3 presence dots (overflow as "+N") read from its own `Workspace::collaborators`. Inactive cards render at 55% alpha so the active card still anchors focus. Tree-level collaborators only backfill the active card when the workspace itself has no per-ws roster (demo-tree convenience). |
| Scroll/pan to switch strips | ✅ | Scroll-wheel over the sidebar accumulates line-units; crossing ±1 calls `ChromeState::switch_workspace`. Click-to-activate a workspace card + plus-button-append-and-activate are both wired. |
| Rim / gradient line on expand | ✅ | `SidebarWheelChrome::draw` paints an 8-step alpha-ramped `neutral[0]` fade at the top and bottom of the wheel (36 px each, peak ~60% alpha), so cards appear to slip into / out of the chrome as they approach the edge. A 1 px indigo @ 35% hairline rim draws at the absolute top and bottom — matching the concept's subtle focus-glow trim. |
| Current "workspace 1/1" text | ✅ | WorkspaceView renders it. |

## 4. Window groups, tabs, frames

| Item | Status | Notes |
|---|---|---|
| Groups inside a horizontal strip | ✅ | Each strip's groups live in a `Row` inside the content Column, so groups tile left-to-right; multiple strips stack vertically. |
| Width presets (narrow/normal/wide/custom) | ✅ | `WindowWidth` enum + clamped 300–1200. |
| Right-edge resize handle | ✅ | `WindowGroup` renders handle, hover + drag work. |
| Maximized state | ✅ | Per-tab maximize icon on hover toggles `WindowGroup::maximized`. While maximized, the group paints a 2 px accent outline, a floating app-title pill (top-left, active tab's title), and a right-side cluster of `[Restore][×]`; the × tints `semantic.danger` on hover and closes the active tab + exits maximize in one click. Search icon still not drawn (global search is already wired via ⌘K). |
| Tab bar height (concept 56 px container / 30 px pills) | ✅ | Bar leaf is now `TAB_BAR_HEIGHT = 40 px` with pills inset `TAB_PILL_INSET_Y = 6 px` top/bot, so the pills themselves sit at 28 px (rounded-full via `pill_height / 2`) on a 40 px outer bar. Close / search / maximize hit rects inherit the pill y-range so hover affordances stay correctly centered. Presence dots hang 2 px below the pill inside the new breathing room (no overflow into the white-card frame). Concept's `h-14` (56 px) is visually taller still; we run 40 px so the pill chrome doesn't dominate the workspace area — a deliberate, documented deviation. Drop-index hit tests updated for the new geometry in `widget::tabs::tests::drop_index_for_uses_pill_midpoints`. |
| Tab pill styling | ✅ | Rounded-full pills with active/hover/inactive alpha ramp, close-glyph on hover, and a leading vector app-type icon drawn via `draw_app_icon` in `quick-ui/src/widget/tabs.rs` — one of five kurbo-built glyphs (`Terminal` = prompt chevron + underscore, `Files` = folder, `Editor` = doc with `<>`, `Browser` = globe, `Settings` = gear). Maps 1:1 to concept's lucide icons in `WindowFrame.tsx:10-18` (`getAppIcon`). Plumbed from the shell via `TabDescriptor { app_kind: Option<AppKind> }`; `TabBar::apply_layout` reserves `icon_w + icon_gap` (14 + 6 = 20 px) on the leading edge only when `app_kind.is_some()` so test/detach paths keep short pills. Covered by `widget::tabs::tests::{tab_descriptor_new_sets_app_kind_none, pill_reserves_extra_width_when_app_kind_set}`. |
| Tab layout spring | ✅ | `TabBar::reorder`, `insert_tab`, and `remove_tab` all call `snapshot_layout_for_spring`, which captures the current `tab_rects` keyed by tab id and latches `layout_anim_started`. `TabBar::draw` interpolates each pill's visual rect from its snapshot rect to `tab_rects[i]` over 180 ms with ease-out-cubic; close/max/search hit-visuals are offset by the per-tab delta so their glyphs track the moving pill. New tabs (not in the snapshot) render at target immediately so they pop in opaque — matches concept's AnimatePresence for layout-only motion. Hit-testing stays on target rects so interaction isn't delayed mid-spring. `needs_animation_frame()` ORs the spring's in-flight signal with the existing drop-zone curve. Covered by 4 unit tests (`reorder_starts_layout_spring_and_snapshots_prev_rects`, `insert_tab_triggers_layout_spring`, `remove_tab_triggers_layout_spring`, `layout_spring_clears_itself_after_duration`). |
| Tab drop zones + structural-rebuild | ✅ | `GroupRow::split_tab_into_new_group` used to insert a fresh `WindowGroup` into `self.groups` at drop time, but the taffy layout tree was only built once at App startup — the newly-inserted group had no `NodeId`, was absent from `App::node_map`, and `GroupRow::apply_layout` skipped it, so drop-to-create rendered nothing in real use despite passing unit tests. Fixed: `Widget` trait gained `take_structure_dirty(&mut self) -> bool` (default false); `GroupRow` sets its own flag after the insert; container widgets (`Column`, `Row`, `Stack`, `Container`, `Scrollable`, `TileHost`, `WindowGroup`) forward the query to their children so nested mutations bubble to the root. `App` polls once per `RedrawRequested` and once per `AboutToWait`; when dirty it drops taffy + node_map and rebuilds from `root_widget.build_layout`, then flags `layout_dirty` so the next `compute_layout` pass picks up the new tree. Regression test in `quick-ui/src/widget/group_row.rs` drives the split and asserts `take_structure_dirty()` reports true exactly once (clears on read). Drag-to-reorder itself: while a drag is in flight `TabBar::draw` paints a `TabDropZone`-style indicator in **every inter-pill gap** (N+1 bars for N pills, 2 px idle) so the user sees every legal drop location, and the gap the cursor is over animates from 2 px to 18 px with an accent-indigo glow over 140 ms via an ease-out-cubic curve — the Rust port of `nirios-concept/src/components/WindowFrame.tsx`'s `motion.div layout` `w-2`→`w-16` transition. Animation state lives on the bar (`drop_zone_target` + `drop_zone_animated_at: Option<Instant>`); `TabBar::needs_animation_frame()` returns true while the curve is still interpolating, `WindowGroup` + `GroupRow` bubble that signal up, and `ShellOverlayHost::needs_animation_frame()` ORs it with the overview crossfade so the app's `AboutToWait` pump keeps redraws flowing until the zone settles. Covered by `widget::tabs::tests::drop_zone_target_tracks_cursor_during_drag` (asserts cursor-tracks-target mid-drag + release clears target + idle state stops pumping frames). Cross-group moves land through the `GroupRow` widget in `quick-ui/src/widget/group_row.rs`: `TabBar` publishes `pending_detach` when a release falls outside its own rect, `GroupRow::handle_event` post-dispatch-sweeps sibling groups and hit-tests the release point against each `bar_rect()`, splicing the detached tab + its `TileHost` into the target (the dragged tab becomes active in its new group; the source promotes a neighbour). Drop index is resolved via `WindowGroup::drop_index_for` → `TabBar::drop_index_for` (pill-midpoint hit-tests). **Inter-group gap drops** (concept's `DropZone` between groups in `DesktopView.tsx:414`): `GroupRow::gap_index_for(release_point)` classifies a release as 0..=N when the cursor sits between two groups (or before/after the row); `GroupRow::split_tab_into_new_group(src, tab_id, gap_idx)` calls `WindowGroup::from_detached` to spawn a fresh group carrying just the dragged tab and splices it at `gap_idx`, pruning the source group when its last tab was pulled out. The degenerate "drop a single-tab group's only tab back next to itself" case is rejected so it doesn't visually flash-destroy-recreate the same slot. Covered by 5 `group_row::tests` cases. |
| Tab-level collaborator blur indicators | ✅ | See §7 — `TabBar::draw` paints per-peer colour dots at the bottom of each pill via `PresenceCtx::peers_on(PresenceKey::Tab { id })`; wired to both startup-seeded + live-refreshed presence. |
| Window frame white rounded card | ✅ | `TileHost::draw` paints `surfaces.window_card` (WHITE) with `rounded-xl` radius + `white/20` border + shadow rings (code in `quick-ui/src/widget/tile_host.rs:75-109`). Previously invisible in live shell because `GroupRow::build_layout` used `height: auto` with no `flex_grow`, so inside the parent Column it measured only as tall as its 40 px `TabBar` — forcing each `WindowGroup`'s `height: percent(1.0)` to resolve against 40 px and leaving zero for `TileHost`. Fixed by setting `flex_grow: 1.0` + `width: percent(1.0)` on the `GroupRow` flex node (commit `2d7f5fe`). `TileHost` rect went from `600×0` → `600×416`; `target/nirios-screenshots/03-desktop-card-fix.png` now shows the white rounded card with readable content. |
| Drag-over / search-active borders | ✅ | `WindowGroup::draw` paints a 1.5 px indigo-500 @ 60% alpha ring while `TabBar::drag_in_flight()` is true. Search-active ring (concept's `ring-4 ring-indigo-500/50 shadow-[0_0_60px_rgba(99,102,241,0.6)]`) is drawn as two concentric indigo strokes (8 px @ 24% alpha outer, 4 px @ 50% alpha inner) when `WindowGroup::search_active` is set — toggled by the tab-pill search glyph via `TabBar::pending_search` → `WindowGroup::take_search` drain. |

## 5. Apps

| Item | Status | Notes |
|---|---|---|
| Notes | ✅ | `NotesApp` is a CRDT-backed plaintext editor; soft-wraps, supports Enter/Backspace/Arrows/Home/End; shown as the active tile in the default Home workspace. |
| Terminal | ✅ | Real PTY via `quick-terminal`. Dark `TerminalPalette` is now tuned to the concept's `TerminalApp.tsx`: pure-black background, green-400 (#4ade80) default foreground, ANSI 1..=7 anchored on Tailwind's `-400` ramp (red/yellow/blue/purple/cyan + slate-300 neutrals), bright 8..=15 on the `-300` ramp. Palette swap on theme change is already covered by `terminal_ansi_mapping_tracks_palette_swaps`. |
| Files | ✅ | `file-explorer` crate wired in. `sidebar::build_tiers(home)` now returns a **Favorites** tier (Home + xdg-user-dirs) and a **System** tier (`/`); `files_app.rs` renders a Text label above each tier and keeps `files.sidebar.<N>` ids sequential across tiers. Matches `nirios-concept/src/components/apps/FilesApp.tsx` (lines 94/127). |
| Browser | ✅ | `BrowserStubWidget::draw` paints concept's browser chrome: white card, 48 px header with 3 slate traffic-light circles + centered globe-plus-URL address pill, and an indigo "App Preview" tile with headline/subheadline in the body. Still a stub — no real web engine — but visually matches `nirios-concept/src/components/apps/BrowserApp.tsx`. |
| Editor | ✅ | `EditorApp` in `quick-shell/src/shell/editor_app.rs`: CRDT-backed text store (own `editor_body` key so it doesn't clash with Notes), per-line rendering + 44 px line-number gutter, ArrowUp/Down with column preserve, tiny JS/TSX tokenizer (keywords → indigo-500, strings → emerald, numbers → amber, line-comments → slate-500, idents/punct → slate ramp). Ships as a fifth demo tab in the runtime workspace. |

## 6. Overlays / modals

| Item | Status | Notes |
|---|---|---|
| Overlay host in root | ✅ | `ShellOverlayHost` wraps the root widget, owns visibility flags, draws scrim + overlays. |
| Global search (⌘K) | ✅ | Overlay-host palette draws on ⌘K with scrim + a scope banner above the palette. Rows are now action-wired: `ShellOverlayHost::dispatch_palette_command` maps ids (`overview.toggle`, `settings.open`, `quick-settings.open`, `search.open`) to chrome overlay mutations; Enter on the cursor row and left-click on any visible row both dispatch and close the palette. Unknown ids close the palette too (no stale overlay). |
| Search context banner / scope radios | ✅ | Banner above the palette shows "Search in: Workspace / All" pills; click rotates `ChromeState::search_scope`; defaults to Workspace so cross-workspace leakage is opt-in. |
| Overview view modal | ✅ | Drawn on `overlays.overview`; cards clickable; Esc closes. |
| Quick settings dropdown | ✅ | Interactive: click-to-set sliders (brightness/volume 0..=1), click-to-flip toggles (Wi-Fi/Bluetooth). State lives on `ChromeState::quick_settings`. |
| Workspace settings modal | ✅ | Rename workspace: click Name input to begin editing; type / Backspace / Space / Enter / Esc semantics; border highlights indigo with caret block while editing. |
| Outside-click close | ✅ | Clicks outside the palette / quick-settings / ws-settings rect dismiss them; an in-flight name edit commits before the modal closes. |
| Collaborator dropdown | ✅ | Click on the top-bar avatar stack toggles `overlays.collaborators`; `ShellOverlayHost::draw_collaborators` paints a 260×(48+36·N) px popover with an initial-circle + name per peer. Outside-click + Esc dismiss; empty roster shows a "No peers connected" hint. |
| Maximized-window floating controls | ✅ | `WindowGroup::draw` paints title pill + `[Restore][×]` cluster on top of a maximized group; Restore + close hit-tests live on `restore_rect`/`close_rect`. Hover state on close tints `semantic.danger`. |

## 7. Collaboration chrome

| Item | Status | Notes |
|---|---|---|
| Remote-cursor overlay | ✅ | `ShellOverlayHost::draw_demo_peer` paints a single simulated peer ("Ada") with caret + name chip + Lissajous motion over the desktop. `ShellOverlayHost::draw_real_peers` closes the loop for the live path: it iterates `PresenceCtx::pointers` filtered to `PresenceKey::Workspace { id: tree.active }` — the same key `App::set_presence_broadcast_scope` writes — painting each real peer's caret + name chip in their palette colour (`presence.color_for`, so "alice" stays pink everywhere). Pointers outside this window's `rect` are clipped out so a peer wandering into the chrome never paints there. Test: `real_peers_filter_picks_up_pointers_on_active_workspace_only` asserts the key-format contract. |
| Bidirectional presence pipeline | ✅ | `CollabRequest::Presence { key, payload }` is the outbound counterpart of `CollabEvent::RemotePresenceUpdate`. `App::broadcast_presence` is the public entry point the shell / widgets call when local presence changes (cursor, caret, tab focus). The test `MemoryRelay::pump` translates Presence → RemotePresenceUpdate, attributing it to the sender's id, so the relay models what the real compositor socket will do. Round-trip tested via `multi_user_session::memory_relay_fans_out_presence_as_remote_presence_update` and `multi_user_session::app_broadcast_presence_completes_loop_to_remote_peer` — the latter drives a full alice→relay→bob flow and asserts bob's `PresenceCtx::caret_for` picks up alice's attributed caret, while alice's own context stays empty (sender-suppression). |
| CRDT incremental delta merge | ✅ | Before this landed, `CollabEvent::CRDTIncremental` was a no-op: the App branch just flipped `needs_sync` without touching the doc, so peers broadcasting via `CollabRequest::SyncIncremental` would fan out but the bytes would drop on the receiver floor. `State::load_incremental` now delegates to `AutoCommit::load_incremental` and refreshes `last_snapshot` so the undo baseline stays aligned with the merged state; the App loop wires it in (bad bytes silently ignored so a noisy peer can't brick the local doc). `State::save_incremental` is the symmetric accessor. End-to-end covered by `multi_user_session::crdt_incremental_deltas_replay_on_remote_peer`: alice mutates, serializes the delta, the relay fans out CRDTIncremental, bob merges, both docs converge on the same scalar. |
| Live pointer broadcast from App event loop | ✅ | `App::set_presence_broadcast_scope(Some(key))` turns on an outbound pointer relay wired directly into winit's `CursorMoved` / `CursorLeft` branches. Wire rate is capped at ~30 Hz via a pure `pointer_broadcast_allowed(last, now, gap)` gate (unit-tested for first-call, in-gap block, at-boundary allow, past-gap allow) so winit's vsync-rate delivery can't saturate the Unix socket. CursorLeft fans out a `Left` payload so peers stop rendering the cursor when it exits the window. Scope changes reset the throttle. Shell `main.rs` seeds the scope to `PresenceKey::Workspace { id: tree.active }` so peer cursors attribute to the workspace they were observed on. Opt-in: headless tests and `quick-ui` examples leave the scope `None` and observe no behaviour change. |
| Top-bar avatar stack | ✅ | Top-bar paints up to 3 initial avatars + `+N` overflow chip; sync-from-tree re-runs on chrome revision bump. |
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
| Glow shadows for focus / drag-over | ✅ | `WindowGroup::draw` paints a 1.5 px indigo-500 @ 60% alpha ring while a tab drag is in flight (drag-over affordance), and a two-stroke indigo glow (8 px @ 24% outer, 4 px @ 50% inner) around the whole card when `search_active` is set (focus glow). Elevation tokens cover the rest. |
| Framer-Motion-equivalent springs | ✅ | Motion tokens exist; `WorkspaceView` uses one. |

## 10. Interaction & state

| Item | Status | Notes |
|---|---|---|
| ⌘K / ⌘Tab / Esc | ✅ | ⌘K toggles palette; ⌘Tab toggles overview; Esc closes the top-most overlay. |
| Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z | ✅ | `quick-ui`'s App event loop wires undo (Ctrl+Z) + redo (Ctrl+Y or Ctrl+Shift+Z), which the shell inherits since it runs via `quick_ui::App`. |
| ⌘↑ / ⌘↓ workspace switch | ✅ | Wired through `ShellOverlayHost` → `chrome.switch_workspace`. |
| Mouse wheel over strip | ✅ | 600 ms cooldown in `StripView`. |
| Tab click | ✅ | `TabBar` handles it. |
| Tab drag-to-reorder | ✅ | Drag starts after 4 px threshold, release hit-tests position within the bar and reorders. Releases outside the bar publish `TabBar::pending_detach`, which the parent `GroupRow` consumes to move the tab into a sibling group whose `bar_rect` contains the release point. |
| Tab close | ✅ | "×" glyph inside each pill's right edge; click propagates `pending_close` → `WindowGroup` drops the host + removes the tab. |
| Group resize drag | ✅ | Handle + pointer accumulation work. |
| Sidebar wheel scroll/pan | ✅ | Click-to-activate + plus-button-create + scroll-wheel accumulator (crosses ±1 on the wheel rect) all wired. |
| Hover reveals tab actions | ✅ | Close "×" sits in the pill always; hovering the pill additionally reveals a magnifying-glass (search) + maximize-icon to its left. Click search toggles `WindowGroup::search_active`, which paints the concept's indigo ring-4 + outer glow around the whole card. Click maximize toggles `WindowGroup::maximized`. Pin / split / move still not drawn — lower-priority concept affordances. |
| Outside-click closes dropdowns | ✅ | Click outside palette / quick-settings / ws-settings rect dismisses them; ws-settings commits a pending rename before closing. |
| File drag onto workspace | ✅ | `shell::workspace_drop` ports the concept's `DesktopView.tsx:187-205` classifier (folders → Files; `.js/.ts/.tsx/.json/.md/.css/.html` → Editor; rest → Files) and splices a new tab into the active workspace's first group. Production wiring: `ShellOverlayHost::handle_event` routes winit's `WindowEvent::DroppedFile` to `ChromeState::handle_file_drop(path)`, which mutates the persisted tree via `apply_workspace_drop_from_path` and bumps `ChromeState::revision` so widgets re-sync on the next frame. Runtime-tree counterpart (`Shell::handle_file_drop`) is available for headless/integration code. Covered by 15 unit tests + 3 `nirios_e2e_test` cases (shell runtime file, shell runtime folder, chrome persisted). |
| Spring view-swap | ✅ | Ease-out-cubic crossfade in `ShellOverlayHost` (200 ms; see §1). |

---

## Highest-leverage remaining gaps

Every parity row above is ✅ — including the two accepted deviations
(§3 3D wheel → 2D per user brief; §4 tab bar 40 px vs concept's 56 px
for chrome density). Outstanding infrastructure-level items:

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

7. **Sidebar card hover-scale** — ✅ concept's `DesktopView.tsx:446`
   `whileHover={{ scale: 1.05 }}` on each workspace card in the left
   sidebar wheel ported to `SidebarWheelChrome::draw`. For every
   non-active card, when the hover bookkeeping marks the card as
   hovered we compose `transform * translate(cx, cy) * scale(1.05) *
   translate(-cx, -cy)` (scale-about-center) and route the pill fill,
   glyph text, subtitle text, per-peer presence dots, and overflow
   `+N` chip through that derived `card_xform`. The active card's
   indigo ring stays on the base transform (intentional — it keeps
   its geometric anchor while hovered neighbours lift toward the
   cursor). Opt-in by construction: non-hovered or active cards
   continue to paint on the base transform, so no regression for the
   steady-state render path.

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
