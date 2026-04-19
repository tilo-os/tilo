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
| Desktop view (sidebar + strips + groups) | ✅ | Top bar + left SidebarWheelChrome + one horizontal Row per strip (groups tile left-to-right). Strips stack vertically inside the content Column. |
| Overview view | ✅ | `ShellOverlayHost::draw_overview` renders a centered grid of workspace cards with preview tiles; click activates a workspace. |
| Z-ordered overlay host | ✅ | `ShellOverlayHost` draws palette, overview, quick-settings, workspace-settings + scrim above content. |
| View-swap animation | ✅ | `ShellOverlayHost` crossfades desktop ↔ overview via ease-out-cubic over 200 ms. Desktop content + simulated peer render inside a `push_layer(alpha = 1 - t)` and scrim + overview grid render inside a `push_layer(alpha = t)`; a mid-flight re-toggle latches `transition_from` from the current `overview_t` so it doesn't pop. 3 unit tests cover idle/full/mid-flight cases. |

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
| Left 96 px vertical strip | ✅ | `SidebarWheelChrome` renders on the left of the split row. |
| 3D wheel rotation | 🟧 | 2D vertical wheel with workspace cards, active highlight, + button. No 3D tilt (acceptable per user brief). |
| Per-strip label + avatars | 🟧 | Workspace name + card chrome shown; active card paints up to 3 presence dots at the bottom (overflow as "+N"). Per-workspace presence isn't modeled yet — tree-level collaborators attribute to the active card as a "people are here" signal. |
| Scroll/pan to switch strips | ✅ | Scroll-wheel over the sidebar accumulates line-units; crossing ±1 calls `ChromeState::switch_workspace`. Click-to-activate a workspace card + plus-button-append-and-activate are both wired. |
| Rim / gradient line on expand | 🟥 | No render. |
| Current "workspace 1/1" text | ✅ | WorkspaceView renders it. |

## 4. Window groups, tabs, frames

| Item | Status | Notes |
|---|---|---|
| Groups inside a horizontal strip | ✅ | Each strip's groups live in a `Row` inside the content Column, so groups tile left-to-right; multiple strips stack vertically. |
| Width presets (narrow/normal/wide/custom) | ✅ | `WindowWidth` enum + clamped 300–1200. |
| Right-edge resize handle | ✅ | `WindowGroup` renders handle, hover + drag work. |
| Maximized state | ✅ | Per-tab maximize icon on hover toggles `WindowGroup::maximized`. While maximized, the group paints a 2 px accent outline, a floating app-title pill (top-left, active tab's title), and a right-side cluster of `[Restore][×]`; the × tints `semantic.danger` on hover and closes the active tab + exits maximize in one click. Search icon still not drawn (global search is already wired via ⌘K). |
| Tab bar height ~14 px | 🟧 | TabBar is 28 px (concept 14 px). |
| Tab pill styling | ✅ | Rounded-full pills with active/hover/inactive alpha ramp, close-glyph on hover. |
| Tab drop zones | ✅ | Drag-to-reorder works + 2 px indigo bar renders at the drop position while drag is in flight (left of target when moving backward, right of target when moving forward). Cross-group moves land through the new `GroupRow` widget in `quick-ui/src/widget/group_row.rs`: `TabBar` publishes `pending_detach` when a release falls outside its own rect, `GroupRow::handle_event` post-dispatch-sweeps sibling groups and hit-tests the release point against each `bar_rect()`, splicing the detached tab + its `TileHost` into the target (the dragged tab becomes active in its new group; the source promotes a neighbour). Drop index is resolved via `WindowGroup::drop_index_for` → `TabBar::drop_index_for`, which walks `tab_rects()` and compares the release x to each pill's midpoint so a drop between pill X and pill Y lands at index Y rather than always falling to index 0. |
| Tab-level collaborator blur indicators | 🟥 | No indicators. |
| Window frame white rounded card | ✅ | `TileHost` paints `surfaces.window_card` (WHITE) with `rounded-xl` radius + `white/20` border. Stub text renders dark-on-white. |
| Drag-over / search-active borders | 🟧 | `WindowGroup::draw` paints a 1.5 px indigo-500 @ 60% alpha ring while `TabBar::drag_in_flight()` is true; matches the concept's drag-over state for the source group. Search-active ring not yet tied to ⌘K scope. |

## 5. Apps

| Item | Status | Notes |
|---|---|---|
| Notes | ✅ | `NotesApp` is a CRDT-backed plaintext editor; soft-wraps, supports Enter/Backspace/Arrows/Home/End; shown as the active tile in the default Home workspace. |
| Terminal | 🟧 | Real PTY via `quick-terminal`. Visual styling driven by `Theme`; matches concept palette if theme tuned. |
| Files | 🟧 | `file-explorer` crate wired in. Has a sidebar + grid, but not yet matched to concept's Favorites/Project-Root/System tiers. |
| Browser | 🟧 | `BrowserStubWidget` shows "Browser coming soon"; concept wants white chrome + traffic lights + address bar preview. |
| Editor | ✅ | `EditorApp` in `quick-gnome-shell/src/shell/editor_app.rs`: CRDT-backed text store (own `editor_body` key so it doesn't clash with Notes), per-line rendering + 44 px line-number gutter, ArrowUp/Down with column preserve, tiny JS/TSX tokenizer (keywords → indigo-500, strings → emerald, numbers → amber, line-comments → slate-500, idents/punct → slate ramp). Ships as a fifth demo tab in the runtime workspace. |

## 6. Overlays / modals

| Item | Status | Notes |
|---|---|---|
| Overlay host in root | ✅ | `ShellOverlayHost` wraps the root widget, owns visibility flags, draws scrim + overlays. |
| Global search (⌘K) | 🟧 | Overlay-host palette draws on ⌘K with scrim + a scope banner above the palette; palette rows still don't wire to actions. |
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
| Remote-cursor overlay | ✅ | `ShellOverlayHost::draw_demo_peer` paints a single simulated peer ("Ada") with caret + name chip + Lissajous motion over the desktop. |
| Top-bar avatar stack | ✅ | Top-bar paints up to 3 initial avatars + `+N` overflow chip; sync-from-tree re-runs on chrome revision bump. |
| Tab collaborator blur indicators | ✅ | `TabBar::draw` reads `PresenceCtx::peers_on(PresenceKey::Tab { id })`; for up to 4 peers it paints low-alpha color dots (stable FNV-style hash → HSL hue) at the bottom of each pill. Shell-side wiring landed via `Collaborator::active_tab: Option<u64>` + `shell::sync_tab_presence_from_tree(&tree, &mut ctx)` (called from `main.rs` before the app loop starts), so any workspace collaborator with an `active_tab` attribution now materialises as a dot on the corresponding tab pill on first draw. Live refresh is wired too: `App::apply_remote_presence` routes `PresenceKey::Tab` `Arrived` payloads through `PresenceCtx::set_tab_focus`, which evicts the user from every other `Tab` key before registering them on the new one — so a peer switching tabs mid-session moves their dot instead of leaving ghosts. |
| File-explorer per-folder presence dots | 🟥 | — |
| Editor collaborator caret+selection | ✅ | `EditorApp::draw` reads `PresenceCtx::carets` / `selections` keyed by `PresenceKey::Widget { [Field("editor_body")] }`. Char-index carets are mapped to byte offsets via `char_indices`, then positioned with `tr.measure` on the prefix of the owning line; multi-line selections emit one rect per intersecting row so `draw_peer_selection` paints a clean per-line band. Presence colours unify with tab dots + remote cursors via `presence::color_for` → `user_color`, so alice stays pink everywhere. |

## 8. Data model

| Item | Status | Notes |
|---|---|---|
| Workspace / Strip / WindowGroup / Tab | ✅ | `quick-gnome-shell/src/shell/state.rs`, mirrors concept. |
| Roles + RolePermissions | ✅ | `Role`, `WorkspaceFile` exist. |
| Collaborator (id, name, cursor color, etc.) | 🟧 | Rust `Collaborator` now carries `id`, `name`, and `active_tab: Option<u64>` (concept's `activeAppId`), plus `new` / `with_active_tab` constructors. Still missing `cursorColor`, `avatar`, `roleId`, `cursor`. |
| App types | ✅ | `AppKind::{Terminal,Files,Editor,Browser,Settings}` — Editor now maps to `EditorApp`. |
| CRDT sync for apps | ✅ | Automerge `State::doc`, per-app `sync(doc, root)`. |

## 9. Visual tokens

The `quick-ui` theme exposes accent / semantic / neutral ramps, typography
scale, spacing, radii, elevation, motion presets, and a terminal palette.
It is structurally complete for theming but the **default dark theme is not
tuned to the concept's slate-900/indigo-500 palette** — it leans more neutral.

| Item | Status | Notes |
|---|---|---|
| Accent indigo-500/600 | ✅ | Default dark theme's accent ramp is indigo-950→indigo-50 (index 5 = `#6366f1`). |
| Slate-900 / slate-950 surface scheme | ✅ | Dark theme: slate-900 base, slate-800 raised, black/80 top-bar overlay, slate-900/70 glass. Neutral ramp = slate-950→slate-50. |
| Per-collaborator fixed cursor colors | ✅ | `quick_ui::presence::user_color` pins the concept palette (`me` → indigo-500, `alice` → pink-500, `bob` → orange-500, `charlie` → emerald-500) and falls through to FNV→HSL for unknown ids. Tabs' presence dots and the shell's `SimulatedFeed` (now built via `new_with_user_id("alice", "Alice")`) both read from the same function, so one peer keeps one colour across every affordance. |
| Radii 8/12/16/24/full | ✅ | Tokens available. |
| Glow shadows for focus / drag-over | 🟧 | Elevation tokens exist; drag-over glows not implemented. |
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
| Hover reveals tab actions | 🟧 | Close "×" sits in the pill always; hovering the pill additionally reveals a maximize-icon to its left (1 px outlined square). Concept's full action row (search + pin / split / move) still not drawn. |
| Outside-click closes dropdowns | ✅ | Click outside palette / quick-settings / ws-settings rect dismisses them; ws-settings commits a pending rename before closing. |
| File drag onto workspace | 🟥 | — |
| Spring view-swap | ✅ | Ease-out-cubic crossfade in `ShellOverlayHost` (200 ms; see §1). |

---

## Highest-leverage remaining gaps

Sorted by visual/functional impact per unit of work:

1. **File-explorer tier matching** — concept's Favorites / Project-Root /
   System sidebar tiers; Rust widget exists but doesn't split the listing.
2. **Screenshot-driven visual E2E** — blocked on host missing the "Window
   Calls Extended" GNOME extension; 16 headless E2E tests cover the
   non-visual wiring.
