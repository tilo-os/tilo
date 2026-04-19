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
| Desktop view (sidebar + strips + groups) | 🟧 | Top bar + white-card window-groups + left SidebarWheelChrome render. Groups still stack vertically (concept wants horizontal). |
| Overview view | ✅ | `ShellOverlayHost::draw_overview` renders a centered grid of workspace cards with preview tiles; click activates a workspace. |
| Z-ordered overlay host | ✅ | `ShellOverlayHost` draws palette, overview, quick-settings, workspace-settings + scrim above content. |
| View-swap animation | 🟥 | No crossfade between desktop ↔ overview. |

## 2. Top bar

| Item | Status | Notes |
|---|---|---|
| 40 px fixed bar | ✅ | `TopBarChrome` renders a 40 px overlay-bg bar; wired into `build_nirios_root`. |
| Overview toggle button | ✅ | Grid icon click toggles `overlays.overview`. |
| Up/down chevrons | ✅ | Clicks switch workspace via `chrome.switch_workspace(±1)`; greyed at bounds. |
| Workspace-name button + gear | ✅ | Gear click toggles `overlays.ws_settings`. |
| Center widget carousel | 🟧 | Clock HH:MM renders; SystemMonitor / Calendar variants not drawn yet. |
| Collaborator avatar stack | ✅ | Up to 3 indigo-filled initial circles with 4 px overlap; 4+ peers collapse into a trailing `+N` chip on the raised surface. |
| Quick-settings tray pill | ✅ | 3-dot click toggles `overlays.quick_settings`. |

## 3. Workspace strip / sidebar wheel

| Item | Status | Notes |
|---|---|---|
| Left 96 px vertical strip | ✅ | `SidebarWheelChrome` renders on the left of the split row. |
| 3D wheel rotation | 🟧 | 2D vertical wheel with workspace cards, active highlight, + button. No 3D tilt (acceptable per user brief). |
| Per-strip label + avatars | 🟧 | Workspace name + card chrome shown; collaborator avatars per workspace not yet. |
| Scroll/pan to switch strips | ✅ | Scroll-wheel over the sidebar accumulates line-units; crossing ±1 calls `ChromeState::switch_workspace`. Click-to-activate + plus-button-create-workspace remain. |
| Rim / gradient line on expand | 🟥 | No render. |
| Current "workspace 1/1" text | ✅ | WorkspaceView renders it. |

## 4. Window groups, tabs, frames

| Item | Status | Notes |
|---|---|---|
| Groups inside a horizontal strip | 🟧 | Today groups are stacked vertically inside a Column; concept wants horizontal. |
| Width presets (narrow/normal/wide/custom) | ✅ | `WindowWidth` enum + clamped 300–1200. |
| Right-edge resize handle | ✅ | `WindowGroup` renders handle, hover + drag work. |
| Maximized state | 🟥 | No maximize / minimize / floating top-bar. |
| Tab bar height ~14 px | 🟧 | TabBar is 28 px (concept 14 px). |
| Tab pill styling | ✅ | Rounded-full pills with active/hover/inactive alpha ramp, close-glyph on hover. |
| Tab drop zones | 🟧 | Drag-to-reorder works via `press_origin` + on-release hit-test inside the bar; no visual drop-zone indicator (cross-group move not supported). |
| Tab-level collaborator blur indicators | 🟥 | No indicators. |
| Window frame white rounded card | ✅ | `TileHost` paints `surfaces.window_card` (WHITE) with `rounded-xl` radius + `white/20` border. Stub text renders dark-on-white. |
| Drag-over / search-active borders | 🟥 | No border state transitions. |

## 5. Apps

| Item | Status | Notes |
|---|---|---|
| Notes | ✅ | `NotesApp` is a CRDT-backed plaintext editor; soft-wraps, supports Enter/Backspace/Arrows/Home/End; shown as the active tile in the default Home workspace. |
| Terminal | 🟧 | Real PTY via `quick-terminal`. Visual styling driven by `Theme`; matches concept palette if theme tuned. |
| Files | 🟧 | `file-explorer` crate wired in. Has a sidebar + grid, but not yet matched to concept's Favorites/Project-Root/System tiers. |
| Browser | 🟧 | `BrowserStubWidget` shows "Browser coming soon"; concept wants white chrome + traffic lights + address bar preview. |
| Editor | 🟥 | No editor app exists. Concept has one with Catppuccin palette + syntax stubs + collab selection. |

## 6. Overlays / modals

| Item | Status | Notes |
|---|---|---|
| Overlay host in root | ✅ | `ShellOverlayHost` wraps the root widget, owns visibility flags, draws scrim + overlays. |
| Global search (⌘K) | 🟧 | Overlay-host palette draws on ⌘K with scrim; rows don't wire to anything yet (concept's `GlobalSearch` scope/context banner missing). |
| Search context banner / scope radios | 🟥 | No context-sensitive search chrome. |
| Overview view modal | ✅ | Drawn on `overlays.overview`; cards clickable; Esc closes. |
| Quick settings dropdown | ✅ | Interactive: click-to-set sliders (brightness/volume 0..=1), click-to-flip toggles (Wi-Fi/Bluetooth). State lives on `ChromeState::quick_settings`. |
| Workspace settings modal | ✅ | Rename workspace: click Name input to begin editing; type / Backspace / Space / Enter / Esc semantics; border highlights indigo with caret block while editing. |
| Outside-click close | ✅ | Clicks outside the palette / quick-settings / ws-settings rect dismiss them; an in-flight name edit commits before the modal closes. |
| Collaborator dropdown | 🟥 | No widget. |
| Maximized-window floating controls | 🟥 | No maximize path yet. |

## 7. Collaboration chrome

| Item | Status | Notes |
|---|---|---|
| Remote-cursor overlay | ✅ | `ShellOverlayHost::draw_demo_peer` paints a single simulated peer ("Ada") with caret + name chip + Lissajous motion over the desktop. |
| Top-bar avatar stack | ✅ | Top-bar paints up to 3 initial avatars + `+N` overflow chip; sync-from-tree re-runs on chrome revision bump. |
| Tab collaborator blur indicators | 🟥 | — |
| File-explorer per-folder presence dots | 🟥 | — |
| Editor collaborator caret+selection | 🟥 | (Editor itself missing.) |

## 8. Data model

| Item | Status | Notes |
|---|---|---|
| Workspace / Strip / WindowGroup / Tab | ✅ | `quick-gnome-shell/src/shell/state.rs`, mirrors concept. |
| Roles + RolePermissions | ✅ | `Role`, `WorkspaceFile` exist. |
| Collaborator (id, name, cursor color, etc.) | 🟧 | Only `id` + `name` in current Rust `Collaborator`. Missing `cursorColor`, `avatar`, `roleId`, `cursor`, `activeAppId`. |
| App types | 🟧 | `AppKind::{Terminal,Files,Browser,…}` exists; no Editor. |
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
| Per-collaborator fixed cursor colors | 🟥 | Concept pins me/alice/bob/charlie; Rust hashes UserId → HSL. |
| Radii 8/12/16/24/full | ✅ | Tokens available. |
| Glow shadows for focus / drag-over | 🟧 | Elevation tokens exist; drag-over glows not implemented. |
| Framer-Motion-equivalent springs | ✅ | Motion tokens exist; `WorkspaceView` uses one. |

## 10. Interaction & state

| Item | Status | Notes |
|---|---|---|
| ⌘K / ⌘Tab / Esc | ✅ | ⌘K toggles palette; ⌘Tab toggles overview; Esc closes the top-most overlay. |
| ⌘↑ / ⌘↓ workspace switch | ✅ | Wired through `ShellOverlayHost` → `chrome.switch_workspace`. |
| Mouse wheel over strip | ✅ | 600 ms cooldown in `StripView`. |
| Tab click | ✅ | `TabBar` handles it. |
| Tab drag-to-reorder | ✅ | Drag starts after 4 px threshold, release hit-tests position within the bar and reorders. |
| Tab close | ✅ | "×" glyph inside each pill's right edge; click propagates `pending_close` → `WindowGroup` drops the host + removes the tab. |
| Group resize drag | ✅ | Handle + pointer accumulation work. |
| Sidebar wheel scroll/pan | 🟧 | Click-to-activate + plus-button-create-workspace wired; scroll-wheel gesture on the wheel itself still TODO. |
| Hover reveals tab actions | 🟧 | Close "×" appears on hover; no full action row (pin / split / move). |
| Outside-click closes dropdowns | 🟥 | Only Esc closes. |
| File drag onto workspace | 🟥 | — |
| Spring view-swap | 🟥 | — |

---

## Highest-leverage remaining gaps

Sorted by visual/functional impact per unit of work:

1. **Editor app** — concept has a Catppuccin-tinged editor with gutter +
   selection highlight; we don't yet.
2. **Maximize state** — concept has a floating top-bar over a maximized
   window-group; no path in the runtime tree yet.
3. **Horizontal window-group strips** — groups currently stack vertically;
   concept wants a horizontal row per strip.
4. **Tab cross-group drag** — today reorder stays within a group; concept
   shows cross-group move via drop zones.
5. **Search context banner / scope radios** — global search is palette-only
   today.
6. **Screenshot-driven visual E2E** — blocked on host missing the "Window
   Calls Extended" GNOME extension; 8 headless E2E tests cover the
   non-visual wiring.
