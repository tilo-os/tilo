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
| Desktop view (sidebar + strips + groups) | 🟧 | Top bar + white-card window-groups render; no left wheel, no true horizontal strip. |
| Overview view | 🟡 | `OverviewView` struct exists; visibility flag set by ⌘Tab; not drawn yet (overlay host only hosts palette). |
| Z-ordered overlay host | 🟧 | `ShellOverlayHost` owns a `CommandPalette` and draws scrim + popover on ⌘K. Overview / quick-settings / ws-settings not yet wired into the host. |
| View-swap animation | 🟥 | No crossfade between desktop ↔ overview. |

## 2. Top bar

| Item | Status | Notes |
|---|---|---|
| 40 px fixed bar | ✅ | `TopBarChrome` renders a 40 px overlay-bg bar; wired into `build_nirios_root`. |
| Overview toggle button | 🟧 | Grid icon drawn on left cluster; click-geometry + toggle still TODO. |
| Up/down chevrons | 🟧 | Drawn with greyed-out state when at workspace bounds; click wiring still TODO. |
| Workspace-name button + gear | 🟧 | Pill + gear glyph drawn; click → `OpenWorkspaceSettings` still TODO. |
| Center widget carousel | 🟧 | Clock HH:MM renders; SystemMonitor / Calendar variants not drawn yet. |
| Collaborator avatar stack | 🟧 | Avatars draw (up to 3) with initial + indigo fill; overflow count not shown. |
| Quick-settings tray pill | 🟧 | Pill + 3-dot glyph drawn; click → `OpenQuickSettings` still TODO. |

## 3. Workspace strip / sidebar wheel

| Item | Status | Notes |
|---|---|---|
| Left 96 px vertical strip | 🟥 | No widget drawn on the left edge. |
| 3D wheel rotation | 🟡 | `SidebarWheel` widget exists with overscroll commit; not in root. |
| Per-strip label + avatars | 🟥 | No rendering path. |
| Scroll/pan to switch strips | 🟧 | `StripView::try_paginate` cools 600 ms; wheel works *on the strip area*, not on a side wheel. |
| Rim / gradient line on expand | 🟥 | No render. |
| Current "workspace 1/1" text | ✅ | WorkspaceView renders it (placeholder top-of-shell, not matching concept). |

## 4. Window groups, tabs, frames

| Item | Status | Notes |
|---|---|---|
| Groups inside a horizontal strip | 🟧 | Today groups are stacked vertically inside a Column; concept wants horizontal. |
| Width presets (narrow/normal/wide/custom) | ✅ | `WindowWidth` enum + clamped 300–1200. |
| Right-edge resize handle | ✅ | `WindowGroup` renders handle, hover + drag work. |
| Maximized state | 🟥 | No maximize / minimize / floating top-bar. |
| Tab bar height ~14 px | 🟧 | TabBar is 28 px (concept 14 px). |
| Tab pill styling | 🟧 | Rounded-full pills (white/15 active, white/5 inactive) with dim text. No hover-expand action buttons yet. |
| Tab drop zones | 🟥 | No reorder-drop zones. |
| Tab-level collaborator blur indicators | 🟥 | No indicators. |
| Window frame white rounded card | ✅ | `TileHost` paints `surfaces.window_card` (WHITE) with `rounded-xl` radius + `white/20` border. Stub text renders dark-on-white. |
| Drag-over / search-active borders | 🟥 | No border state transitions. |

## 5. Apps

| Item | Status | Notes |
|---|---|---|
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
| Overview view modal | 🟡 | ⌘Tab sets flag; not drawn. |
| Quick settings dropdown | 🟡 | `QuickSettings` holds brightness/volume + nav stack (Wifi / BT drill-ins); not drawn. |
| Workspace settings modal | 🟡 | `WorkspaceSettings` holds tabs (General/Roles/Files), Modal + FocusTrap wrappers; not drawn. |
| Collaborator dropdown | 🟥 | No widget. |
| Maximized-window floating controls | 🟥 | No maximize path yet. |

## 7. Collaboration chrome

| Item | Status | Notes |
|---|---|---|
| Remote-cursor overlay | 🟡 | `RemoteCursorOverlay` + `SimulatedFeed` exist; neither gets a draw call. |
| Top-bar avatar stack | 🟡 | TopBar helpers read tree collaborators; no render. |
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
| ⌘K / ⌘Tab / Esc | 🟧 | ⌘K toggles the palette overlay; Esc closes it. ⌘Tab (overview) still a flag with no visible overlay. |
| ⌘↑ / ⌘↓ workspace switch | ✅ | `WorkspaceView` springs. |
| Mouse wheel over strip | ✅ | 600 ms cooldown in `StripView`. |
| Tab click | ✅ | `TabBar` handles it. |
| Tab drag-to-reorder | 🟧 | Drag starts (`pending_drag` after 4 px); no drop zones rendered; no cross-group move. |
| Group resize drag | ✅ | Handle + pointer accumulation work. |
| Sidebar wheel scroll/pan | 🟥 | Widget never receives events. |
| Hover reveals tab actions | 🟥 | No pill / no action buttons. |
| Outside-click closes dropdowns | 🟥 | No dropdowns drawn. |
| File drag onto workspace | 🟥 | — |
| Spring view-swap | 🟥 | — |

---

## Highest-leverage gaps

Sorted by visual impact per unit of work:

1. **Overlay stack in the root** — unlocks everything 🟡: top bar, overview,
   search, quick settings, workspace settings, toasts, remote cursors. Without
   this none of those can render.
2. **Top bar wired in** — the single most visible chrome change. Also gives
   the user real entry points for ⌘Tab / ⌘K / workspace settings.
3. **Window frame white-card styling + pill tabs** — changes the look of every
   tile from "flat rectangle on dark" to concept-matching white cards.
4. **Overview view render** — second-most-visible chrome. ⌘Tab currently a
   no-op.
5. **Sidebar wheel render** — left-edge wheel is iconic to the concept.
6. **Global search / quick settings / workspace settings draw paths** — three
   moderately-sized overlay renders; widgets are already built.
7. **Editor app** — only fully-missing app (Terminal / Files / Browser-stub
   all exist).
8. **Collaboration chrome wiring** — remote cursor overlay, tab presence
   blurs, top-bar avatar stack.
9. **Theme tuning** — nudge default palette to the concept's slate/indigo.
10. **Detail polish** — tab drop zones, maximize state, drag-file-to-open,
    outside-click-to-close, etc.
