# Nirios Shell — Concept Reference

This document describes the visual design and behavior of the Nirios shell as
prototyped in the React/TSX concept at `nirios-concept/`. It is the target we
are converging the Rust implementation toward. Section numbers match
git history so individual items can be cross-referenced.

## 1. Overall layout

A vertical workspace container with persistent chrome. Z-order, back to front:

1. Main content area — either **desktop view** (workspace with sidebar wheel +
   horizontal strips of window groups) or **overview view** (vertical list of
   workspace cards). The two views swap via an animated scale/opacity crossfade
   (0.2 s).
2. 40 px fixed **top bar** — left: overview + up/down + workspace name; center:
   rotating widget carousel (clock / system monitor / calendar); right:
   collaborator avatar stack + quick-settings tray.
3. Overlay layers — workspace-settings modal, global search (command palette),
   quick-settings dropdown, collaborator dropdown, maximized-window floating
   controls, remote-cursor overlay, toasts.

## 2. Top bar

Fixed 40 px bar with black/80 %-opacity backdrop-blur background, 1 px
white/10 % bottom border. Three clusters:

- **Left** — overview button (grid icon; toggles overview, indigo when active),
  1 px divider, up/down chevrons (disabled at boundaries), workspace-name
  button (centered text + gear icon; opens workspace settings).
- **Center** — `TopBarWidgets`, a rotating carousel of widgets. Three built-in:
  clock (HH:MM, 24 h), system monitor (CPU %, RAM %, temperature, 20-bar
  sparkline), calendar (next upcoming event). Three indicator dots on the left
  show which widget is visible; mouse wheel rotates between them (200 ms
  cooldown). Whole bar hover-highlights.
- **Right** — collaborator avatar stack (7 px, stacked, up to 3 visible +
  overflow count); quick-settings pill (Wifi / Monitor / Battery icons). Both
  clusters open dropdowns on click.

## 3. Workspace strip (sidebar wheel)

Left-edge 96 px-wide vertical wheel of strip tiles, one per workspace. Each
tile is a small card: vertical label (10 px font, font-black, uppercase, 0.4 em
tracking, rotated) + stacked collaborator avatars underneath (8 px, ringed,
shadowed).

Active tile is at full opacity/scale, z-20. Inactive tiles fade linearly with
distance (`1 - |offset| * 0.15`). On hover, the strip expands with a 3D
rotation (rotateX proportional to offset) and shows a large circular wheel
rim (1200 px, white/5 %) plus a center gradient line (transparent → indigo/50
→ transparent).

Interactions: click to switch; scroll wheel (deltaY > 30, 300 ms cooldown);
pan gesture. When collapsed, only near strips peek with rotateX ±30°.

## 4. Window groups, tabs, window frames

Windows live in **groups** inside horizontal **strips**. One strip per active
row; each strip holds many groups laid out horizontally.

### Window group

- Width presets: `narrow` (400), `normal` (600), `wide` (800), or `custom` (300–1200).
- Maximized state: fixed positioning over the viewport, no rounded corners,
  no border, no shadow. Re-entering non-maximized spring-shrinks back.
- Right-edge 4 px resize handle. Hover: `indigo-500/20`. Active:
  `indigo-500/50`. Drag clamps to 300–1200 px.

### Tab bar

- ~14 px tall (hidden when maximized).
- Horizontally scrollable with left/right mask fades (40 px gradient).
- `TabDropZone`s before the first and after the last tab (expand on drag to
  16 px wide with indigo glow).
- Each tab is a rounded-full pill:
  - **Active**: `bg-white/10`, `text-white`, border `white/20`,
    `shadow-[0_8px_32px_rgba(0,0,0,0.2)]`.
  - **Inactive**: `bg-white/5`, `text-white/70`, border `white/10`.
  - **Hover** reveals three right-edge actions: search (🔍), maximize/minimize,
    close (× red on hover). These expand the pill on hover.
  - **Collaborator indicators**: blurred colored circles behind the tab text
    (`mix-blend-screen`, 50 % opacity), one per collaborator active in that
    app. Color = collaborator cursor color.

### Window frame

- **Not maximized**: `bg-white` pure-white inner card, `rounded-3xl` (24 px),
  `border-white/20`, `shadow-2xl`. Takes all remaining group height.
- **Drag-over**: border turns `indigo-500` with a glow shadow.
- **Search-active**: `ring-4 ring-indigo-500/50` plus enhanced shadow.
- **Maximized floating controls** appear when the cursor rises above y=80:
  - Left pill: app icon + title (max 200 px, truncated).
  - Right pill: search, minimize, close.
  - Slides in with spring (stiffness 300, damping 25).

## 5. Apps (per-app visual spec)

### Editor (`EditorApp.tsx`)

Dark theme (Catppuccin Mocha `#1e1e2e` bg, `#cdd6f4` text, monospace,
`text-sm`, 24 px padding). Left gutter with gray (`#6c7086`) line numbers,
right-aligned in 24 px. Syntax highlighting: keywords `#cba6f7`, identifiers
`#89b4fa`, strings `#a6e3a1`, operators `#89dceb`. Collaborator selection on
one line: `bg-pink-500/30`, `border-b-pink-500`, plus a blinking pink caret.

### Files (`FilesApp.tsx`)

White background (`bg-white/80` + backdrop blur). Two-pane:

- **Sidebar** — 180 px, collapsible to 0. `bg-slate-900/5`, right border
  `black/5`. Sections:
  - "Favorites" header (10 px, slate-400, uppercase).
  - "Project Root" (indigo-500/10 bg, indigo-700 text, active state).
  - Shortcut folders (blue folder icon).
  - "System" divider → "OS Disk" disabled (italic, slate-400).
  - Right-click context menu: "Remove Shortcut".
  - Drop target for dragging folders in.
  - 1 px right resize handle (indigo-400/30 hover, indigo-500/50 active).
- **Main** — `p-8`, 2–5 column responsive grid. Folder icons
  (`fill-blue-500/10`), file icons type-specific (`FileCode2` orange,
  `FileText` gray, `FileImage` purple). Hover: scale 1.02 + `bg-black/3`.
  Tap: scale 0.98. Drag provides custom preview and data payload.

### Browser (`BrowserApp.tsx`)

White chrome with three traffic-light dots (slate-300), rounded-full address
bar (white bg, slate-200 border, Globe icon + URL). Content area
`bg-slate-100`. Preview state: centered large indigo icon + "App Preview" +
"Running locally on port 3000". Design-tool state (Figma): white card with
header bar + dashed "Design Canvas" box + 3-col placeholder grid.

### Terminal (`TerminalApp.tsx`)

Pure black (`#000`) background, `text-green-400` default, monospace, `p-4`.
Prompt colors: user@host blue-400, path purple-400, `$` green-400, URLs
cyan-400, warnings yellow-400. Blinking green caret line (2 × 4 px,
animate-pulse). Mock content is a vite dev-server log.

## 6. Overlays / modals

### Global search (command palette)

- Trigger: ⌘K, or per-tab search button.
- Fixed overlay, `pt-[15vh]`, centered. Backdrop: `bg-black/30 backdrop-blur-sm`.
  Modal: `max-w-2xl`, `bg-slate-900/70 backdrop-blur-md`, `rounded-3xl`,
  `shadow-2xl`. Enter animation: scale 0.9 → 1, opacity + y-fade (200 ms).
- Optional context banner at top when opened inside an app: indigo/20 bg with
  icon + "Searching in [AppTitle]" + "Switch to Global" button on the right.
- Search row: 🔍 + `placeholder="Search for apps, files, or actions..."` (or
  context-specific), right-side `⌘K` key hint.
- Conditional panels per context: editor → replace row, scope radios
  (workspace/directory/global), full-text toggle (green glow), replace toggle
  (amber glow). Files → file-type filter chips.
- Results list (max-h 400, overflow-y): icon + title + subtitle, selected
  state `bg-indigo-500`; empty state text.
- Footer: "↑↓ Navigate", "Enter Select" hints; right-side context label.
- Keys: ↑/↓ navigate, Enter select, Esc close.

### Overview view

Full-screen `bg-slate-950`. Header: "Workspace Overview" (text-4xl bold) +
subtitle. Vertical list (`space-y-8`), connector line `bg-slate-800` on the
left. Each workspace card:

- Inactive: `bg-slate-900/50`, transparent border.
- Active: `bg-slate-900`, `border-indigo-500`, glow shadow.
- Active indicator dot on the left (−35 px, 4 px, bordered `slate-950`).
- Left column (w-64): workspace name (text-2xl bold), `WS-{y}` monospace
  badge, "Current" label if active, permissions box (shield icon, role/file
  counts), collaborators (Users icon + count + stacked avatars).
- Right column: horizontally scrolling **mini-windows** — tab dots + h-20
  slate-800 rounded-lg cards; widths map to narrow/normal/wide (w-16, w-24, w-32).

### Quick settings

Dropdown anchored under the tray icons, `w-80`, `bg-slate-900/80 backdrop-blur-md`,
`rounded-3xl`. Root level:

- **Top row (3 buttons)**: Settings, Screenshot (camera icon), Power
  (red-500/20 tint).
- **Sliders block**: Brightness (sun), Volume (volume2). `h-1.5 bg-white/10`
  track with indigo fill, draggable 0–100.
- **Toggles grid (2 col)**: Wi-Fi (bg-indigo-500 when connected, shows network
  name), Bluetooth (shows paired device name). Both show a chevron-right.
- **Power profile row**: battery icon, "Balanced" + "Power Profile" subtext,
  chevron-right.

Wi-Fi / Bluetooth drill-ins: chevron-left back button, list of
networks/devices (Signal or Smartphone icon, name + status), connected item
marked with a check and indigo-400 accent.

### Workspace settings modal

Backdrop: `bg-black/30 backdrop-blur-[1px]`. Modal: `max-w-4xl h-[600px]`,
`bg-slate-900/70`, `rounded-3xl`. Two panes:

- **Sidebar (w-64)** — workspace badge (indigo-500 circle with initial letter),
  name (bold, truncated), "Workspace Settings" subtext, tabs: General, Roles &
  Permissions, Workspace Files. Close (×) at bottom.
- **General** — Invite Link (monospace URL in dark box + Copy indigo button),
  Collaborators grid: per-collab card (avatar + name + role badge +
  Remove/UserMinus).
- **Roles & Permissions** — left role list (colored dots, + New Role), right
  panel: "Allowed Applications" grid of app toggles, "File Access Overrides"
  per-file none/read/write button group (indigo-500 when selected).
- **Workspace Files** — file cards with type-specific icon, name + mime
  subtext, per-role permission chips (W/R), remove button.

### Cursor overlay

Absolute layer (`inset-0 pointer-events-none z-50 overflow-hidden`). Per
remote collaborator with `activeAppId === app.id` and a `cursor` position:

- `MousePointer2` icon (5 × 5, drop-shadow) in their cursor color with a
  white outline.
- Label pill under cursor (2 × 0.5 rounded-full, text-10 bold white, bg =
  cursor color).
- Spring-animated motion (stiffness 200, damping 25).

## 7. Collaboration chrome

- Stacked avatars in the top-bar tray (−1.5 x-space, 7 px, ring border).
- Collaborator dropdown: list with avatar + cursor-color dot, name, role,
  remove button (only for non-owners).
- Tab collaborator indicators: blurred color circles behind tab text.
- File-explorer per-folder presence dots (2.5 px, white ring) with hover
  tooltip.
- Editor collaborator selection line + animated caret.
- Remote pointer overlay over the active app.

## 8. Data model (`types.ts`)

- `Workspace { id, name, y, strips, roles, workspaceFiles, collaborators }`.
- `Strip { id, name, windowGroups }`.
- `WindowGroup { id, tabs, activeTabId, width: 'narrow'|'normal'|'wide',
  customWidth? }`.
- `AppInstance { id, type, title }`; `AppType = 'editor'|'files'|'settings'|
  'browser'|'terminal'`.
- `Role { id, name, color, isDefault?, permissions: { apps: AppType[],
  files: Record<id, 'read'|'write'|'none'> } }`.
- `Collaborator { id, name, color, cursorColor, avatar, roleId?, cursor?,
  activeAppId? }`.
- `WorkspaceFile { id, name, type: 'text'|'video'|'document'|'image'|'code' }`.

`data.ts` seeds 4 collaborators (me/alice/bob/charlie), 5 mock files, 3 roles
(Admin/Editor/Viewer), 3 workspaces (Personal / Project Alpha [active] /
Client Review).

## 9. Visual tokens

- **Palette**: dark shell on slate-900 / slate-950 / black; file explorer and
  app inner cards on white. Indigo-500/600 for active accents. Per-collaborator
  fixed cursor colors (me indigo, alice pink, bob orange, charlie emerald).
- **Semantic**: red-500/-400 danger, emerald-400/-500 success,
  yellow-400/amber-500 warning, cyan-400/blue-400 info.
- **Text**: white → slate-200 → slate-400 → slate-500/-600 tiers.
- **Radii**: 8 / 12 / 16 / 24 px + full.
- **Shadows**: `shadow-sm` → `shadow-2xl` + bespoke glow shadows for indigo
  focus states.
- **Blur**: `backdrop-blur-md`/`-xl` for overlays; `blur-sm`/`-xl` for
  collaborator chroma.
- **Animation**: Framer-Motion springs (stiffness 200, damping 25) and 0.2–
  0.5 s tween fades.

## 10. Interaction & state

**Shortcuts**

- ⌘Tab — toggle overview.
- ⌘K — open global search.
- ↑/↓, Enter, Esc inside search.

**Mouse**

- Click top-bar chevrons → switch workspace.
- Click workspace name → open workspace settings.
- Hover left wheel → expand; scroll / pan → switch strips (300 ms cooldown).
- Click tab → activate; drag tab → reorder / new-group drop zone.
- Click max / min / close on tab → maximize / minimize / close.
- Drag group right edge → resize (300–1200 px).
- ⌘K inside tab → search in that app context; context bar with "Switch to
  Global".
- Click tray avatars / quick-settings icons → dropdowns; outside click → close.
- Sidebar in Files: drag resize handle / click collapse / right-click items.
- Drag file onto workspace → opens Files/Editor app.

**Animations**

- View switch: 0.2 s scale+fade between desktop and overview.
- Workspace switch: spring slide direction matches ↑/↓ (spring, bounce 0.1).
- Maximize: instant pop to fullscreen; top-bar float slides in on hover.
- Tabs: new tabs enter y/scale spring; closing tabs y/scale exit.
- Sidebar wheel: expanding does radial/rotate transform; inactive tiles
  fade and blur.
- Cursor overlay: spring motion for remote pointers.
