# Nirios Concept — Feature Reference

> Catalogue of every visual/interactive detail specified by `nirios-concept/` (the React + Tailwind + framer‑motion mockup). Use this as the authoritative parity target instead of re‑reading the TSX sources for each detail.
>
> **Source tree:** `nirios-concept/src/components/`. Every citation is `<File>.tsx:<line>` relative to that directory.
>
> **Tailwind color reference (used throughout):**
>
> | Class            | Hex        | Notes |
> |------------------|------------|-------|
> | `black`          | `#000000`  | |
> | `white`          | `#ffffff`  | |
> | `slate-200`      | `#e2e8f0`  | default body text |
> | `slate-300`      | `#cbd5e1`  | |
> | `slate-400`      | `#94a3b8`  | muted labels |
> | `slate-500`      | `#64748b`  | de-emphasised text |
> | `slate-600`      | `#475569`  | |
> | `slate-700`      | `#334155`  | dividers / rails |
> | `slate-800`      | `#1e293b`  | mini-window card |
> | `slate-900`      | `#0f172a`  | primary panel |
> | `slate-950`      | `#020617`  | overview background |
> | `indigo-300`     | `#a5b4fc`  | |
> | `indigo-400`     | `#818cf8`  | accent glow / dot active |
> | `indigo-500`     | `#6366f1`  | brand accent |
> | `indigo-600`     | `#4f46e5`  | accent hover |
> | `emerald-400`    | `#34d399`  | success indicator |
> | `emerald-500`    | `#10b981`  | |
> | `amber-400`      | `#fbbf24`  | |
> | `amber-500`      | `#f59e0b`  | |
> | `red-400`        | `#f87171`  | destructive text |
> | `red-500`        | `#ef4444`  | destructive bg |
> | `blue-400`       | `#60a5fa`  | |
> | `blue-500`       | `#3b82f6`  | folder icons |
> | `purple-400`     | `#c084fc`  | |
> | `orange-400`     | `#fb923c`  | |
> | `pink-500`       | `#ec4899`  | remote caret |
> | `cyan-400`       | `#22d3ee`  | terminal link |
> | `green-400`      | `#4ade80`  | terminal caret/battery |
> | `yellow-400`     | `#facc15`  | |
>
> `bg-white/N` conventions: `bg-white/5 = rgba(255,255,255,0.05)`, `/10 = 0.10`, `/20 = 0.20`, `/30 = 0.30`. Same pattern for `bg-black/N`, `bg-indigo-500/N`, etc.

---

## Table of Contents

1. [App Shell (`App.tsx`)](#1-app-shell-apptsx)
2. [TopBarWidgets (`TopBarWidgets.tsx`)](#2-topbarwidgets-topbarwidgetstsx)
3. [QuickSettings (`QuickSettings.tsx`)](#3-quicksettings-quicksettingstsx)
4. [WorkspaceSettingsModal (`WorkspaceSettingsModal.tsx`)](#4-workspacesettingsmodal-workspacesettingsmodaltsx)
5. [GlobalSearch (`GlobalSearch.tsx`)](#5-globalsearch-globalsearchtsx)
6. [DesktopView (`DesktopView.tsx`)](#6-desktopview-desktopviewtsx)
7. [WindowFrame (`WindowFrame.tsx`)](#7-windowframe-windowframetsx)
8. [OverviewView (`OverviewView.tsx`)](#8-overviewview-overviewviewtsx)
9. [CursorOverlay (`CursorOverlay.tsx`)](#9-cursoroverlay-cursoroverlaytsx)
10. [App stubs (`apps/*.tsx`)](#10-app-stubs-appstsx)

---

## 1. App Shell (`App.tsx`)

**Purpose** — Root layout: sets up the full-screen dark page, global top bar (panel), content area with desktop/overview view switching, and mounts the modals (`WorkspaceSettingsModal`, `GlobalSearch`, `QuickSettings`).

### 1.1 Structure

- Root `div` (`App.tsx:276`) — classes: `h-screen w-screen overflow-hidden bg-black flex flex-col font-sans text-slate-200 selection:bg-indigo-500/30`.
  - **Top Bar** (`App.tsx:279`): `div` with `h-10 bg-black/80 backdrop-blur-xl border-b border-white/10 flex items-center justify-between px-4 z-50 shrink-0`.
    - **Left cluster** (`App.tsx:282`) — `flex items-center gap-2`.
      - **Overview button** (`App.tsx:283-289`): `p-1.5 rounded-lg transition-colors`. Active variant: `bg-indigo-500 text-white`. Inactive: `hover:bg-white/10 text-slate-400 hover:text-white`. Icon `LayoutGrid w-4 h-4`. Title tooltip: `"Overview (Cmd+Tab)"`.
      - **Vertical divider** (`App.tsx:291`): `w-px h-4 bg-white/10 mx-1`.
      - **Workspace stepper** (`App.tsx:293`): `flex items-center gap-1 rounded-lg px-1 py-1`.
        - **Up chevron** (`App.tsx:294-300`): `text-slate-400 hover:text-white disabled:opacity-30 p-1`; `ChevronUp w-4 h-4`.
        - **Workspace name button** (`App.tsx:301-310`): `flex items-center gap-1.5 px-2 py-1 rounded-md hover:bg-white/10 transition-colors group`. Inside: `span` with `text-sm font-bold tracking-wide min-w-[100px] text-center` (workspace name); `Settings2 w-3.5 h-3.5 text-slate-500 group-hover:text-slate-300`.
        - **Down chevron** (`App.tsx:311-317`): identical styling to Up chevron.
    - **Center cluster** (`App.tsx:322`): absolutely positioned at 50% (`absolute left-1/2 -translate-x-1/2 flex items-center justify-center`). Houses `<TopBarWidgets />`.
    - **Right cluster** (`App.tsx:327`): `flex items-center gap-6`.
      - **Collaborator stack trigger** (`App.tsx:330-345`): `flex items-center gap-3 pr-6 border-r border-white/10 cursor-pointer hover:opacity-80 transition-opacity menu-trigger`. Stack: `flex -space-x-1.5`, each avatar is `w-7 h-7 rounded-full border-2 border-black object-cover` with `backgroundColor: c.cursorColor` inline.
      - **Collaborators popover** (`App.tsx:350-394`): enter `{opacity:0,y:-10,scale:0.95}` → `{opacity:1,y:0,scale:1}`; classes: `absolute top-full right-6 mt-2 w-72 bg-slate-900/80 backdrop-blur-md border border-slate-700 rounded-3xl shadow-2xl z-[70] overflow-hidden flex flex-col menu-panel`.
        - Header row (`App.tsx:356`): `p-3 border-b border-white/10 flex items-center justify-between`. Label `text-sm font-semibold text-white`; Invite button `text-xs flex items-center gap-1 bg-indigo-500 hover:bg-indigo-600 text-white px-2 py-1 rounded-md transition-colors` with `UserPlus w-3 h-3`.
        - List (`App.tsx:363`): `p-2 max-h-[300px] overflow-y-auto`. Each row: `flex items-center justify-between p-2 hover:bg-white/5 rounded-lg group transition-colors`. Avatar `w-8 h-8 rounded-full object-cover` with status pip `absolute -bottom-0.5 -right-0.5 w-3 h-3 rounded-full border-2 border-slate-900` (bg = cursorColor). Name `text-sm text-slate-200 font-medium`, role `text-xs text-slate-500`. Remove button only for non-admins, shown on group hover: `text-slate-500 hover:text-red-400 p-1.5 rounded-md hover:bg-red-500/10 opacity-0 group-hover:opacity-100 transition-all`.
      - **System tray trigger** (`App.tsx:400-407`): `flex items-center gap-4 text-slate-400 hover:text-white hover:bg-white/10 px-3 py-1.5 rounded-full transition-colors cursor-pointer menu-trigger`. Icons: `Wifi`, `Monitor`, `Battery` each `w-4 h-4`.
      - **QuickSettings popover** — rendered inside a conditional `AnimatePresence` (`App.tsx:410`), see §3.
  - **Main content area** (`App.tsx:426`): `flex-1 relative overflow-hidden`, wraps both views via `AnimatePresence mode="wait"`.

### 1.2 Measurements

| Item | Value |
|------|-------|
| Top bar height | `h-10` (= 40px) |
| Top bar padding | `px-4` (= 16px) |
| Top bar blur | `backdrop-blur-xl` (= 24px) |
| Top bar border | `border-b border-white/10` (1px, rgba 255,255,255,0.10) |
| Top bar z-index | `z-50` |
| Workspace name min width | `min-w-[100px]` |
| Avatar in top bar | `w-7 h-7`, 2px black border |
| Collaborator popover | `w-72` (= 288px), `mt-2`, `rounded-3xl`, z `z-[70]` |
| Popover list max height | `max-h-[300px]` |
| QuickSettings popover z-index | `z-[70]` |

### 1.3 Animations

- **Workspace page transition** (`App.tsx:13-31`, applied at `App.tsx:437`): variants keyed on `slideDirection`. `initial`: `y: -100%` (up) or `100%` (down), `opacity:0`, `scale:0.9`. `animate`: `y:0, opacity:1, scale:1`, transition `{ type:'spring', bounce:0.1, duration:0.5 }`. `exit`: inverse of `initial`.
- **Desktop/Overview crossfade** (`App.tsx:432-435, 469-472`): desktop enters `{opacity:0,scale:1.05}` → `{1,1}`, exits `{0,0.95}`. Overview enters `{0,0.95}` → `{1,1}`, exits `{0,1.05}`. `transition: {duration:0.2}` both.
- **Collaborators popover** (`App.tsx:351-353`): scale/opacity/Y spring (no explicit transition; uses framer default).

### 1.4 Interactions

- Click overview button → toggles `view` between `desktop` and `overview`.
- Click workspace name → opens `WorkspaceSettingsModal` (`setIsSettingsOpen`).
- Click up/down chevrons → calls `handleWorkspaceChange('up'|'down')` which sets `slideDirection` and bumps active workspace index.
- Click collaborator stack → toggles user menu.
- Click system tray → toggles QuickSettings.
- Global mousedown outside `.menu-trigger`/`.menu-panel` closes both popovers (`App.tsx:52-66`).
- Keyboard (`App.tsx:69-82`):
  - `Cmd/Meta+Tab` → toggle desktop/overview.
  - `Cmd/Meta+K` → open GlobalSearch with empty context.

### 1.5 Conditional state

- `view === 'overview'`: overview button shows `bg-indigo-500 text-white`; else muted hover style.
- Up/down chevrons set `disabled` + `disabled:opacity-30` at boundary workspaces.
- Collaborator remove button hidden unless row hover and collaborator `roleId !== 'role-admin'`.

---

## 2. TopBarWidgets (`TopBarWidgets.tsx`)

**Purpose** — Center-of-top-bar widget carousel. Rotates between three widgets (Clock, SystemMonitor, Calendar) via vertical wheel scroll.

### 2.1 Structure

- Outer `div` (`TopBarWidgets.tsx:36-39`): `relative flex items-center w-[480px] h-9 px-2 rounded-lg hover:bg-white/10 hover:shadow-[0_8px_32px_rgba(0,0,0,0.2)] transition-all duration-200 group cursor-ns-resize select-none`.
  - **Indicator column** (`TopBarWidgets.tsx:41`): `flex flex-col gap-0.5 px-3 border-r border-white/5`. One `motion.div` per widget.
  - **Widget viewport** (`TopBarWidgets.tsx:59`): `flex-1 h-full px-8 relative`. Holds an `AnimatePresence mode="wait" initial={false}` with a single child keyed by `activeIndex`.
  - **Right spacer** (`TopBarWidgets.tsx:77`): `w-10`, preserves symmetry with the dot column.

### 2.2 Indicator dots

- Classes (`TopBarWidgets.tsx:46`): `w-1 h-1 rounded-full cursor-pointer`.
- Animate state (`TopBarWidgets.tsx:48-52`):
  - Active: `backgroundColor:'#818cf8'` (= `indigo-400`), `scale:1.2`, `opacity:1`.
  - Inactive: `backgroundColor:'rgba(255,255,255,0.2)'`, `scale:1`, `opacity:0.3`.
- Transition: `{type:'spring', stiffness:500, damping:30}`.

### 2.3 Sub-widgets

#### ClockWidget (`TopBarWidgets.tsx:82-90`)

- Layout: `flex items-center justify-center w-full`.
- Time `span`: `text-sm font-bold tracking-wide text-white`. Format `HH:MM` 24h via `toLocaleTimeString('en-US',{hour:'2-digit',minute:'2-digit',hour12:false})`.
- Updates every 1000ms.

#### SystemMonitorWidget (`TopBarWidgets.tsx:92-139`)

- Container: `flex items-center justify-between w-full h-full`.
- Left cluster: `flex items-center gap-2 shrink-0`.
  - Label `text-[9px] uppercase tracking-widest text-emerald-400 font-bold` (`"CPU"`).
  - Value `text-xs font-mono text-white font-bold w-8` (e.g. `"12%"`).
- Sparkline (`TopBarWidgets.tsx:117`): `flex-1 mx-4 h-3 flex items-end gap-[1px]`. 20 bars, each `flex-1 bg-emerald-500/30 rounded-t-[1px]` with inline `height:max(10, val)%`.
- Right cluster: `flex items-center gap-4 shrink-0`.
  - RAM: label blue-400, value `text-xs font-mono text-white font-bold`.
  - TMP: label orange-400, value same. Suffix `°`.
- Updates every 2000ms with random jitter.

#### CalendarWidget (`TopBarWidgets.tsx:141-149`)

- Container: `flex items-center justify-center w-full gap-2 text-sm`.
- `"Next:"` in `text-slate-400 font-medium`.
- Event name `"Design Sync"` in `text-white font-bold tracking-wide`.
- Time `"15:30"` in `text-slate-500 font-mono text-xs`.

### 2.4 Animation — active-widget swap

- Wrapper (`TopBarWidgets.tsx:62-67`): `initial={y:8,opacity:0}` → `animate={y:0,opacity:1}` → `exit={y:-8,opacity:0}`. `transition:{duration:0.15, ease:"easeOut"}`.

### 2.5 Interactions

- `onWheel` (`TopBarWidgets.tsx:20-33`): deltaY threshold `>10`, cooldown 200ms. Positive wraps forward, negative wraps backward using modulo.
- Click on a specific dot → jump to that widget (stops propagation).

---

## 3. QuickSettings (`QuickSettings.tsx`)

**Purpose** — System tray dropdown: power/settings/screenshot actions, brightness/volume sliders, Wi-Fi/Bluetooth drill-in, power profile.

### 3.1 Structure

- Root `motion.div` (`QuickSettings.tsx:165-169`): `absolute top-full right-0 mt-2 w-80 bg-slate-900/80 backdrop-blur-md border border-slate-700 rounded-3xl shadow-2xl z-[70] overflow-hidden flex flex-col p-4 gap-4 menu-panel`.
- `AnimatePresence mode="wait"` with three keyed branches: `main`, `wifi`, `bluetooth`.
- **Main branch** (`QuickSettings.tsx:173-259`):
  - **Action row** (`QuickSettings.tsx:181`): `flex items-center justify-between gap-2`. Three buttons each `flex-1 bg-white/10 hover:bg-white/20 p-3 rounded-2xl flex justify-center items-center transition-colors`. Icons `Settings`, `Camera`, `Power`, all `w-5 h-5 text-white`. Power variant: `bg-red-500/20 hover:bg-red-500/30 text-red-400`.
  - **Sliders panel** (`QuickSettings.tsx:204`): `flex flex-col gap-3 bg-white/5 p-3 rounded-2xl`. Contains two `<Slider>` instances (brightness `Sun`, volume `Volume2`).
  - **Toggle grid** (`QuickSettings.tsx:218`): `grid grid-cols-2 gap-2`. Each tile: `bg-indigo-500 hover:bg-indigo-600 p-3 rounded-2xl flex flex-col items-start gap-1 transition-colors text-left`. Header row: icon (`Wifi`/`Bluetooth` `w-5 h-5 text-white`) + `ChevronRight w-4 h-4 text-white/70`. Body: title `text-sm font-medium text-white`, subtitle `text-xs text-indigo-200` (network/device name).
  - **Power profile row** (`QuickSettings.tsx:249`): `bg-white/5 hover:bg-white/10 p-3 rounded-2xl flex items-center justify-between transition-colors`. Icon `Battery w-5 h-5 text-green-400`. Title `Balanced` `text-sm font-medium text-white`. Subtitle `Power Profile` `text-xs text-slate-400`. Trailing `ChevronRight w-4 h-4 text-slate-400`.
- **Wi-Fi / Bluetooth branches** (`QuickSettings.tsx:101-162`): Back chevron `ChevronLeft w-5 h-5 text-white` in a `p-1 hover:bg-white/10 rounded-full` button + `text-sm font-semibold text-white` heading. List rows: `flex items-center justify-between p-2 hover:bg-white/5 rounded-xl transition-colors cursor-pointer group`. Status icon `Signal`/`Smartphone w-4 h-4`, color `text-indigo-400` when connected else `text-slate-400`. Name `text-sm` with `text-white font-medium` when connected, `text-slate-300` otherwise. Bluetooth rows also show `text-[10px] text-slate-500 uppercase tracking-wider` second line. Check mark `Check w-4 h-4 text-indigo-400` when connected.

### 3.2 Slider (internal to `QuickSettings`)

- Row: `flex items-center gap-3`.
- Icon block: `text-slate-400 shrink-0`, icon `w-4 h-4`.
- Track: `h-1.5 flex-1 bg-white/10 rounded-full overflow-hidden cursor-pointer relative`.
- Fill (`QuickSettings.tsx:86-90`): `motion.div` with `h-full bg-white rounded-full`, width animated via `{width: `${value}%`}`. Transition is `{type:'tween',duration:0}` while dragging, `{type:'spring', bounce:0, duration:0.3}` otherwise.

### 3.3 Measurements

| Item | Value |
|------|-------|
| Popover width | `w-80` (= 320px) |
| Popover padding | `p-4` |
| Section gap | `gap-4` |
| Button tile radius | `rounded-2xl` |
| Slider track height | `h-1.5` (= 6px) |
| Toggle grid columns | 2 |

### 3.4 Animations

- Root enter (`QuickSettings.tsx:166-168`): `{opacity:0,y:-10,scale:0.95}` → `{opacity:1,y:0,scale:1}`. Exit mirror.
- Branch swap: `main` slides from left (`x:-20`), others slide from right (`x:20`). No explicit timing (framer defaults).

### 3.5 Interactions

- Slider `onMouseDown` jumps to clicked X, then follows mousemove until mouseup (listens on `window`).
- Toggle tile click → `setExpandedSection('wifi'|'bluetooth')`.
- Back chevron → `setExpandedSection('none')`.
- Actions buttons: `onOpenSettings` (opens `WorkspaceSettingsModal`); others are decorative.

### 3.6 Conditional state

- Wi-Fi mock data array (`QuickSettings.tsx:24-29`): `Home_Network` connected, signal 3; three more unconnected.
- Bluetooth mock (`QuickSettings.tsx:31-36`): AirPods Pro + Logitech MX Master connected, two others not.
- Default slider values: brightness 75, volume 50.

---

## 4. WorkspaceSettingsModal (`WorkspaceSettingsModal.tsx`)

**Purpose** — Full-screen modal with three tabs (General, Roles & Permissions, Files) for managing a workspace.

### 4.1 Structure

- Backdrop (`WorkspaceSettingsModal.tsx:34-41`): `absolute inset-0 bg-black/30 backdrop-blur-[1px]`, fade in/out. Click closes modal. Container `div` (`:34`) is `fixed inset-0 z-[100] flex items-center justify-center`.
- Dialog (`:42-47`): `relative w-full max-w-4xl h-[600px] bg-slate-900/70 backdrop-blur-md border border-white/10 rounded-3xl shadow-2xl overflow-hidden flex`.
  - **Sidebar** (`:49`): `w-64 bg-black/20 border-r border-white/5 flex flex-col p-4 gap-2`.
    - **Workspace header** (`:50`): `flex items-center gap-3 px-3 py-4 mb-4`. Badge `w-8 h-8 rounded-lg bg-indigo-500 flex items-center justify-center text-white font-bold` with the workspace's first letter. Labels `text-sm font-bold text-white truncate` (name) + `text-[10px] text-slate-500 uppercase font-bold tracking-wider` (`"Workspace Settings"`).
    - **Tab buttons** (`:60-83`): `flex items-center gap-3 px-3 py-2 rounded-xl transition-all`. Active: `bg-white/10 text-white`. Inactive: `text-slate-400 hover:bg-white/5 hover:text-slate-200`. Icons `w-4 h-4`. Labels `text-sm font-medium`. Roles tab has a trailing `ChevronRight w-3 h-3 opacity-50`.
    - **Close row** (`:85-90`): pushed to bottom via `mt-auto pt-4 border-t border-white/5`. Destructive hover `hover:text-red-400 hover:bg-red-400/10 rounded-xl`.
  - **Content pane** (`:94`): `flex-1 flex flex-col overflow-hidden bg-slate-900/50`. Tab content swap:

#### General tab (`:96-150`)

- Container `p-8 space-y-8 overflow-y-auto`.
- Heading pair: `h2 text-2xl font-bold text-white` + `p text-sm text-slate-400`.
- Invite-link block: section label `text-[10px] font-bold text-slate-500 uppercase tracking-widest`. URL field `flex-1 bg-black/40 border border-white/10 rounded-xl px-4 py-3 text-sm text-slate-400 font-mono truncate`. Copy button `flex items-center gap-2 bg-indigo-500 hover:bg-indigo-600 text-white px-6 py-3 rounded-xl font-bold transition-all shadow-lg shadow-indigo-500/20 active:scale-95`.
- Collaborators grid: `grid grid-cols-1 gap-3`. Row `flex items-center justify-between bg-white/5 border border-white/5 rounded-2xl p-4 hover:border-white/10 transition-all`. Avatar `w-12 h-12 rounded-2xl bg-slate-800` + 4px status pip `absolute -bottom-1 -right-1 w-4 h-4 rounded-full border-2 border-slate-900` (bg = cursorColor). Name row `text-sm font-bold text-white flex items-center gap-2`, self-badge `text-[8px] px-1.5 py-0.5 rounded-full bg-indigo-500/20 text-indigo-400 uppercase font-black` (`"You"`). Role chip `text-[10px] font-bold px-2 py-0.5 rounded-full bg-white/5 text-slate-400` with inline `color: role.color`.

#### Roles tab (`:152-254`)

- Layout `flex h-full overflow-hidden`.
- **Role list** (`:155`): `w-48 border-r border-white/5 flex flex-col p-4 gap-2 overflow-y-auto`.
  - Row (`:157-167`): `flex items-center gap-2 px-3 py-2 rounded-xl transition-all text-left`. Active: `bg-white/10 text-white`, else `text-slate-400 hover:bg-white/5 hover:text-slate-200`. Color dot `w-2 h-2 rounded-full` (inline bg). Name `text-sm font-medium truncate`. Default badge `text-[8px] px-1.5 py-0.5 rounded-full bg-white/5 text-slate-500 uppercase font-black` (`"Default"`).
  - New-role button: `flex items-center gap-2 px-3 py-2 rounded-xl text-indigo-400 hover:bg-indigo-500/10 transition-all mt-2`, label `"+ New Role"` `text-sm font-bold`.
- **Role detail** (`:174`): `flex-1 overflow-y-auto p-8 space-y-8`.
  - Heading: `h2 text-2xl font-bold text-white flex items-center gap-3` with role name + `w-3 h-3 rounded-full` color dot. Subtitle `text-sm text-slate-400`.
  - Apps section: label `text-[10px] font-bold text-slate-500 uppercase tracking-widest`. Grid `grid-cols-2 gap-3`. Tile `flex items-center justify-between bg-white/5 border border-white/5 rounded-2xl p-4`. Icon wrapper `p-2 bg-white/5 rounded-lg text-slate-400`. Name `text-sm font-bold text-white capitalize`.
  - Toggle switch (`:199-207`): `label.relative.inline-flex.items-center.cursor-pointer`. Hidden `input.sr-only.peer`. Track `w-9 h-5 bg-slate-800 peer-checked:bg-indigo-500 rounded-full`. Thumb pseudo `::after content-[''] absolute top-[2px] left-[2px] bg-white border-gray-300 border rounded-full h-4 w-4 transition-all peer-checked:translate-x-full peer-checked:border-white`.
  - File overrides (`:213-243`): rows like apps grid; right side is a permission tri-toggle `flex bg-black/40 rounded-xl p-1 gap-1`. Buttons (`:231-238`) `px-3 py-1.5 rounded-lg text-[10px] font-black uppercase transition-all`; active `bg-indigo-500 text-white shadow-lg shadow-indigo-500/20`, else `text-slate-500 hover:text-slate-300`. Options: `none | read | write`.
  - Empty state (`:246-251`): center-stacked `Shield w-12 h-12 opacity-20` + `text-sm font-medium` label.

#### Files tab (`:256-304`)

- Container `p-8 space-y-8 overflow-y-auto`.
- Header row: title + `+ Add Files` button `px-4 py-2 bg-indigo-500 hover:bg-indigo-600 text-white rounded-xl text-sm font-bold transition-all shadow-lg shadow-indigo-500/20`.
- File row: same shell as Collaborators row. Icon slot `w-12 h-12 rounded-2xl bg-white/5 flex items-center justify-center text-slate-400 group-hover:text-indigo-400 transition-colors`; icon `w-6 h-6`. Type label `text-[10px] text-slate-500 uppercase font-bold tracking-wider`.
- Permission badges (`:284-294`): circles `w-6 h-6 rounded-full border-2 border-slate-900 flex items-center justify-center text-[8px] font-bold text-white` with role color, letter `W`/`R`. Stacked with `-space-x-2`.
- Remove button `p-2 text-slate-500 hover:text-red-400 hover:bg-red-400/10 rounded-xl transition-all` with `UserMinus w-4 h-4`.

### 4.2 Measurements

| Item | Value |
|------|-------|
| Modal max width | `max-w-4xl` (= 896px) |
| Modal height | `h-[600px]` |
| Sidebar width | `w-64` (= 256px) |
| Role list width | `w-48` (= 192px) |
| Modal z-index | `z-[100]` |
| Backdrop blur | `backdrop-blur-[1px]` |
| Dialog blur | `backdrop-blur-md` (= 12px) |

### 4.3 Animations

- Backdrop: `opacity 0 ↔ 1`.
- Dialog (`:44-46`): `{opacity:0,scale:0.95,y:20}` → `{1,1,0}`, exit same as initial. Framer default spring.

### 4.4 Interactions

- Click backdrop closes modal.
- Tab buttons switch `activeTab`.
- Selecting a role updates `selectedRoleId` (no persistence — mock).
- Toggle/permission buttons have no handlers hooked up (`onChange={() => {}}`).

### 4.5 Conditional state

- Role detail section shows empty state if `selectedRole` falsy.
- Self-badge `You` renders only when `c.id === 'me'`; remove button hidden for self.
- Default role shows `Default` pill.

---

## 5. GlobalSearch (`GlobalSearch.tsx`)

**Purpose** — Command-K palette: global/contextual search for apps, files, actions. Context-aware filters (Files/Editor/Browser) and editor-specific replace UI.

### 5.1 Structure

- Root (`GlobalSearch.tsx:100`): `fixed inset-0 z-[100] flex items-start justify-center pt-[15vh]`.
- Backdrop (`:103-107`): `absolute inset-0 bg-black/30 backdrop-blur-[1px]`. Click closes.
- Panel (`:109-113`): `w-full max-w-2xl bg-slate-900/70 backdrop-blur-md border border-white/10 rounded-3xl shadow-2xl overflow-hidden relative z-10`.
  - **Context banner** (`:115-134`, only when `contextApp`): `px-4 py-2 bg-indigo-500/20 border-b border-indigo-500/30 flex items-center justify-between`. Badge `p-1 bg-indigo-500 rounded-md text-white` with app icon `w-3 h-3`. Label `text-[10px] font-bold uppercase tracking-wider text-indigo-300`. Switch-global button `text-[10px] text-indigo-400 hover:text-indigo-300 transition-colors font-medium uppercase tracking-tighter`.
  - **Input row** (`:137-153`): `p-4 border-b border-white/10 flex flex-col gap-3`. Inside a `flex items-center gap-3`: `Search w-5 h-5 text-slate-400`, `<input>` `flex-1 bg-transparent border-none outline-none text-white text-lg placeholder:text-slate-500`. Trailing kbd pill `flex items-center gap-1 px-2 py-1 rounded-md bg-white/5 border border-white/10 text-[10px] text-slate-400 font-mono uppercase tracking-widest`, `Command w-2.5 h-2.5 + K`.
  - **Replace row** (`:155-179`, editor search only, toggled by `showReplace`): animated `{height:0,opacity:0}` → `{auto,1}`. Inner `flex items-center gap-3 pt-2 border-t border-white/5`. `Replace w-4 h-4 text-slate-500`; `<input>` `flex-1 bg-transparent border-none outline-none text-white text-sm placeholder:text-slate-600`. Buttons: `px-2 py-1 rounded bg-white/5 hover:bg-white/10 text-[10px] text-slate-300 uppercase font-bold` (Replace) and `bg-indigo-500/20 hover:bg-indigo-500/30 text-indigo-300` (All).
  - **Filters row** (`:183-244`, only when isFileSearch or isEditorSearch): `px-4 py-2 bg-white/5 border-b border-white/10 flex flex-wrap items-center gap-4`.
    - **Scope** (`:186-197`): label `text-[10px] text-slate-500 font-bold uppercase mr-1`. Buttons `px-2 py-1 rounded text-[10px] font-bold uppercase transition-all`; active `bg-indigo-500 text-white shadow-lg shadow-indigo-500/20`; inactive `text-slate-500 hover:text-slate-300 hover:bg-white/5`. Options: `workspace`, `directory`, `global`.
    - **Separator** (`:199`): `w-px h-3 bg-white/10`.
    - **File types** (`:202-221`, file search only): similar buttons; active `bg-white/20 text-white`. Icons `w-3 h-3`. Options: `Docs`, `Video`, `Images`, `Code`.
    - **Full Text toggle** (`:224-232`): button includes 1.5px status dot: active `bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]`, else `bg-slate-600`. Button bg active `bg-emerald-500/20 text-emerald-400`.
    - **Replace toggle** (`:234-242`, editor only): button bg active `bg-amber-500/20 text-amber-400`.
  - **Results list** (`:246-272`): `max-h-[400px] overflow-y-auto p-2`. Each row: `flex items-center gap-4 p-3 rounded-2xl cursor-pointer transition-colors`; selected `bg-indigo-500 text-white`; else `hover:bg-white/5 text-slate-300`. Icon block `p-2 rounded-xl`; `bg-white/20` when selected else `bg-white/5`. Title `font-semibold`, subtitle `text-xs` (selected `text-white/70` else `text-slate-500`). Empty state `p-8 text-center text-slate-500` with the query interpolated.
  - **Footer** (`:274-280`): `p-3 bg-black/20 border-t border-white/5 flex items-center justify-between text-[10px] text-slate-500 font-medium uppercase tracking-wider`. Hint chips for `↑↓ Navigate` and `Enter Select`; right label reads `Global Search` or `<AppTitle> Search`.

### 5.2 Measurements

| Item | Value |
|------|-------|
| Panel max width | `max-w-2xl` (= 672px) |
| Panel offset from top | `pt-[15vh]` |
| Panel radius | `rounded-3xl` |
| Input text size | `text-lg` |
| Results max height | `max-h-[400px]` |
| Layer z-index | `z-[100]` |

### 5.3 Animations

- Backdrop: opacity fade.
- Panel (`:110-112`): `{opacity:0,scale:0.9,y:20}` → `{1,1,0}`. Exit mirror.
- Replace row (`:159-161`): height/opacity auto transition on mount.

### 5.4 Interactions

- Auto-focus input 50ms after open (`:60`).
- Keyboard (`:79-95`): `Escape` closes; `ArrowUp/Down` cycle selection with modulo wrap; `Enter` selects — `agent`/`action` types mapped to `terminal`.
- Mouse enter on row → `setSelectedIndex`.
- Click on row → calls `onSelect(type)`.
- `onClearContext` link (`:128-133`) clears contextApp.

### 5.5 Conditional state

- `isFileSearch`, `isEditorSearch`, `isBrowserSearch` derived from `contextApp.type`.
- Filters row only renders for file/editor context.
- File-type chips only in file search.
- Replace row only in editor search and only when `showReplace` is true.
- Placeholder text changes when `contextApp` set: `Search in {title}...` else `Search for apps, files, or actions...`.

---

## 6. DesktopView (`DesktopView.tsx`)

**Purpose** — The active workspace's desktop: vertical stack of horizontal "strips" of `WindowFrame`s, with a lateral "wheel" navigator for switching strips.

### 6.1 Structure

- Root (`DesktopView.tsx:209-213`): `flex-1 relative overflow-hidden bg-gradient-to-br from-slate-900 to-slate-950 flex flex-col`. Handlers for drag-over/drop from Files app.
  - **Noise overlay** (`:216`): `absolute inset-0 bg-[url('https://grainy-gradients.vercel.app/noise.svg')] opacity-20 mix-blend-overlay pointer-events-none`.
  - **Grid overlay** (`:217`): `absolute inset-0 bg-[linear-gradient(to_right,#80808012_1px,transparent_1px),linear-gradient(to_bottom,#80808012_1px,transparent_1px)] bg-[size:24px_24px] pointer-events-none`.
  - **Sidebar Wheel** (see §6.2).
  - **Strips container** (`:326`): `flex-1 relative pl-24 overflow-hidden` with inline `perspective: 2000px`. Contains every strip; only the active one is pointer-interactive (see §6.3).
  - **Drag preview** (`:462-528`, absolute positioned while dragging a Files item).

### 6.2 Sidebar Wheel (`:219-323`)

**Purpose** — Left-edge cluster selector. Collapsed: minimal 4px-wide "pill" hugging the edge. Expanded (on hover): a 3D rotating wheel of strip labels.

- Hit area (`:220-236`): `absolute left-0 top-0 bottom-0 w-24 z-40 flex flex-col items-center justify-center cursor-grab active:cursor-grabbing`. Inline `perspective: 1000px`. Mouse enter/leave toggles `isStripsExpanded`.
- Highlight pill (`:239-248`):
  - Layout: `absolute inset-y-8 left-4 right-4 rounded-full bg-white/5 border border-white/10`.
  - Animate:
    - Collapsed — `opacity: 0.2`, `width: '4px'`, `x: -10`.
    - Expanded — `opacity: 1`, `width: 'auto'`, `x: 0`.
  - Transition `{duration:0.3}`.
- Strip items (`:250-303`): one `motion.div` per strip.
  - Classes: `absolute top-1/2 left-0 right-0 -translate-y-1/2 flex flex-col items-center gap-4 cursor-pointer` + `z-20` if active else `z-10`.
  - Wheel math (`:251-271`) per strip, based on `offset = sIdx - activeStripIndex`:
    - `angle = offset * 20` degrees.
    - `radius = 600`.
    - Collapsed (`!isStripsExpanded`): `yOffset = offset * 60`; `xOffset = 0`; `zOffset = isActive ? 0 : -100`; `rotateX = 0`; `opacity = isActive ? 1 : 0`; `scale = isActive ? 1 : 0.8`.
    - Expanded: `yOffset = sin(angle)*radius`; `xOffset = (radius - cos(angle)*radius) * 0.8`; `zOffset = |offset| * -50`; `rotateX = angle`; `opacity = 1 - |offset|*0.15`; `scale = 1 - |offset|*0.05`.
    - Note the outer `animate` uses `x: -xOffset` (strip pushed left by the arc distance).
  - Transition: `{type:'spring', stiffness:200, damping:25, mass:1.2}`.
  - Label (`:292`): `[writing-mode:vertical-rl] rotate-180 text-[10px] font-black uppercase tracking-[0.4em] text-white/40 hover:text-white/90 transition-colors select-none`.
  - Avatar stack (`:295-301`): `flex flex-col gap-2`, first 3 collaborators. Each `w-8 h-8 rounded-full border-2 border-slate-900 shadow-xl overflow-hidden ring-1 ring-white/10` with the avatar.
- Wheel rim (`:307-314`): `absolute -left-[550px] top-1/2 -translate-y-1/2 w-[1200px] h-[1200px] border border-white/5 rounded-full pointer-events-none`. Animate `opacity 0↔1`, `scale 0.8↔1`, `x -100↔0` across expanded state.
- Vertical accent (`:316-322`): inside a centred column, a `w-0.5 h-32 bg-gradient-to-b from-transparent via-indigo-500/50 to-transparent rounded-full`. Opacity only visible when expanded.

**Interactions on wheel:**

- `onMouseEnter`/`onMouseLeave`: toggle `isStripsExpanded` (`:222-223`).
- `onWheel` (`:224-228`): if `|deltaY| > 30` delegate to `handleWheelSwitch` (cooldown 300ms).
- `onPanEnd` (`:229-235`): `info.offset.y > 50` → previous strip; `< -50` → next strip.
- Click on a strip tile (`:287-290`): jump to that strip and collapse the wheel.

### 6.3 Strips container + strip transforms (`:327-457`)

Per strip:

- **Expanded view (`:339-345`)**: macOS-dock-like stack.
  - `yOffset = offset * 220`.
  - `zOffset = -400 + |offset| * -150`.
  - `rotateX = offset * -15`.
  - `opacity = 1 - |offset| * 0.2`.
  - `scale = 0.85 - |offset| * 0.05`.
- **Collapsed (`:347-362`)**:
  - Active — all zeroes, `scale 1`, `opacity 1`.
  - Inactive — `yOffset = offset > 0 ? 450 : -450`, `zOffset = -400`, `rotateX = offset>0 ? 30 : -30`, `opacity = 0.3`, `scale = 0.85`.
- Filter: `blur(0px)` when active, `blur(8px)` otherwise.
- Transition: `{type:'spring', stiffness:200, damping:25, mass:1.2}`.
- Wrapper classes (`:383`): `absolute inset-0 flex flex-col` + z-layer + pointer-events gating. Inline `transformStyle: 'preserve-3d'`, `transformOrigin: 'center center'`.
- Inner row (`:394-401`): `flex-1 overflow-x-auto overflow-y-hidden flex items-center px-12 py-12 snap-x snap-proximity hide-scrollbar relative`.
- Window group wrapper (`:404-413`): `motion.div layout`, `h-full flex items-center shrink-0 window-group-container`, enter `{opacity:0,scale:0.9,x:-20}` → `{1,1,0}`, exit `{opacity:0,scale:0.9,x:20}`, layout spring `{bounce:0, duration:0.4}`.
- DropZone before each group and one trailing (see §6.4).
- Trailing "Add strip item" tile (`:443-452`): `h-full min-w-[300px] snap-center shrink-0 flex items-center justify-center`; inside a `motion.div` sized `w-16 h-16 rounded-2xl bg-white/5 border border-white/10 flex items-center justify-center text-white/30 hover:bg-white/10 hover:text-white/60 cursor-pointer transition-colors backdrop-blur-md`. `Plus 24×24`. `whileHover scale 1.05`, `whileTap 0.95`.

### 6.4 DropZone (inline component, `:7-92`)

Slot between window groups. Widths animate by state:

| State | Width | Inner bar | Inner button |
|-------|-------|-----------|--------------|
| Idle (`!isOver && !isHovered`) | `w-8` | `bg-transparent` | hidden |
| Hovered | `w-24` | `bg-white/20` | `+` button appears |
| Drop target (`isOver`) | `w-48` | `bg-indigo-500 scale-y-110 shadow-[0_0_20px_rgba(99,102,241,0.8)]` | hidden |

- Transition (`:57`): `transition-all duration-300 ease-out`.
- Hover delay 150ms before revealing `+` (`:40-52`).
- `+` button (`:83-87`): `w-10 h-10 rounded-full bg-white/10 border border-white/20 backdrop-blur-md flex items-center justify-center text-white shadow-xl hover:bg-white/20 transition-colors`. Icon `Plus w-5 h-5`. Framer: `whileHover 1.1`, `whileTap 0.9`.
- Inner bar: `w-1 h-1/2 rounded-full transition-colors duration-300`.

### 6.5 Wheel scroll-to-switch (content area)

- `scrollContainerRef` only bound on the active strip (`:395`).
- Effect (`:118-153`): listens on wheel with `passive:false`.
  - If `|deltaX| > 5`: skip (horizontal scroll owns it).
  - If `|deltaY| > 60` AND not expanded AND cooldown `<600ms` since last switch: `preventDefault` and return.
  - Else switch active strip in direction of `deltaY`, setting cooldown.
- Sidebar `handleWheelSwitch` (`:155-166`): cooldown 300ms.

### 6.6 Drag-preview panel (`:462-528`)

Activated when Files app drags an item over the desktop.

- Wrapper inline `width:400, height:300`. Positioned at cursor minus `(200,150)`. `fixed pointer-events-none z-[200]`.
- Inner card: `bg-slate-900/40 backdrop-blur-2xl border border-white/20 rounded-3xl shadow-2xl overflow-hidden flex flex-col`.
- Fake title bar (`:476`): `h-8 bg-white/10 border-b border-white/10 flex items-center px-4 gap-2`. Three dots `w-2 h-2 rounded-full bg-{red|amber|emerald}-500/50`. App name `text-[10px] text-white/40 font-bold uppercase tracking-widest`.
- Folder variant (`:486-504`): avatar block + 3-col icon grid.
- File variant (`:506-517`): header chip `FileCode2 w-6 h-6 text-orange-400` + fake code lines (`h-2 bg-white/10 rounded-full`) with random widths and opacity decay.
- Pulse overlay (`:519`): `absolute inset-0 bg-indigo-500/5 animate-pulse pointer-events-none`.
- Tooltip under card (`:521-527`): `absolute -bottom-10 left-1/2 -translate-x-1/2 px-4 py-1.5 bg-indigo-500 text-white text-[10px] font-bold rounded-full shadow-lg uppercase tracking-widest border border-indigo-400`.

### 6.7 Focus model

- `focusedGroupId` (`:113`): set on `onMouseDownCapture` of the window wrapper (`:421`). Used by `WindowFrame` to render the focused/unfocused tab chrome.

### 6.8 Conditional state summary

| State | Visual outcome |
|-------|----------------|
| `isStripsExpanded = false` | Wheel collapsed as 4px pill, content strips collapsed (only active fully visible; others peeking rotated ±30° with blur). |
| `isStripsExpanded = true` | Wheel expands showing all strip tiles arranged on a 3D arc. Strips below/above the active one stack with stronger z-offset and rotation, blur remains on non-active. |
| Empty workspace strip | Only the trailing `+` tile renders; DropZones still present. |
| Drag preview active | Overlay card shown at cursor; dropzone state mutates strip layout width. |

---

## 7. WindowFrame (`WindowFrame.tsx`)

**Purpose** — A window group card: floating tab strip (liquid glass pills) + rounded content card with resize handle and maximize overlay.

### 7.1 Structure (restored)

- Outer wrapper (`WindowFrame.tsx:188`): `div` with inline `style={ width: isMaximized ? localWidth : 'auto', height:'100%', flexShrink:0 }`. `localWidth` default depends on `group.width`: `wide → 800`, `narrow → 400`, else `600`. `group.customWidth` overrides.
- `motion.div` container (`:189-211`): classes `flex flex-col shrink-0 gap-3`. Mode-dependent:
  - Maximized: `fixed z-50`.
  - Restored: `h-full relative`.
  - If `isDragOver && !isMaximized`: appends `scale-[1.02] transition-transform`.
  - Inline `width`: `100vw` if maximized else `localWidth` px. Height `100vh` vs `100%`. Top/left `0` vs `auto`.
  - Entry: `{opacity:0, scale:0.95, y:20}` → `{1,1,0}`. Exit `{0,0.95,y:-20}`.
  - Layout spring `{bounce:0, duration:0.4}`; opacity tween 0.2s.

#### Tab strip (restored only — `:261-352`)

- Host row: `relative h-14 z-10`.
- Scroll container (`:263-269`): `flex items-center justify-start h-full px-8 overflow-x-auto no-scrollbar snap-x snap-proximity scroll-smooth`. Inline mask image for fade edges: `linear-gradient(to right, transparent, black 40px, black calc(100% - 40px), transparent)` (both WebKit and standard).
- Per tab (`:281-297`) `motion.div`, pill:
  - `flex items-center gap-2 px-4 py-2 rounded-full text-sm font-semibold cursor-pointer transition-colors shrink-0 relative group/tab shadow-[0_8px_32px_rgba(0,0,0,0.2)] backdrop-blur-md border overflow-hidden snap-start`.
  - Active + focused: `bg-white/10 text-white border-white/20`.
  - Inactive (or unfocused): `bg-white/5 text-white/70 hover:text-white hover:bg-white/10 border-white/10`.
  - Entry `{opacity:0, y:-10, scale:0.9}` → `{1,0,1}`. Exit `{opacity:0, y:10, scale:0.9}`.
  - Layout spring `{bounce:0.2, duration:0.6}`; opacity 0.2s.
  - `draggable` true; `onDragStart` puts `{sourceGroupId, tabId}` on `text/plain`.
- Foggy collaborator indicator (`:299-310`): `absolute bottom-0 left-0 right-0 h-full pointer-events-none flex items-end justify-center opacity-50 mix-blend-screen overflow-hidden rounded-full`. Each collaborator: `w-16 h-16 rounded-full blur-xl translate-y-1/2` with `backgroundColor: cursorColor`.
- Tab icon (`:312-314`): inside `relative z-10`. Active applies `drop-shadow-md`.
- Tab title (`:315`): `relative z-10 max-w-[120px] truncate tracking-wide`.
- Actions (`:318-340`): hover-reveal row. Container classes: `relative z-10 flex items-center gap-1 overflow-hidden transition-all duration-300 ease-out max-w-0 opacity-0 group-hover/tab:max-w-[76px] group-hover/tab:opacity-100 group-hover/tab:ml-1`. Buttons `p-1 rounded-full hover:bg-white/30 transition-colors text-white/70 hover:text-white`; close variant `hover:bg-red-500/80`. Icons `w-3.5 h-3.5` (`Search`, `Maximize2`/`Minimize2`, `X`).
- Trailing `<TabDropZone isLast>` ends the row (`:344-348`).

#### Tab drop zones (`:20-62`)

- `motion.div` with widths (`:51`): `w-16` when dragged over, `w-4` if first or last, else `w-2`. Always `h-full z-20`.
- Inner dot `w-1 h-1/2 rounded-full`. When over: `bg-indigo-400 scale-y-110 shadow-[0_0_12px_rgba(99,102,241,0.8)]`. Else transparent.

#### Content card (`:354-369`)

- `motion.div` `flex-1 relative overflow-hidden bg-white shadow-2xl border flex flex-col transition-all duration-300`.
- Maximized: `rounded-none border-none`. Restored: `rounded-3xl border-white/20`.
- DragOver (restored): adds `border-indigo-500 shadow-[0_0_40px_rgba(99,102,241,0.4)]`.
- Searching (`isSearching` prop): adds `ring-4 ring-indigo-500/50 shadow-[0_0_60px_rgba(99,102,241,0.6)] z-50`.
- Unfocused (not searching): `opacity-70`; focused or searching: `opacity-100`.
- Hosts the active app body (`EditorApp`, `FilesApp`, `BrowserApp`, `TerminalApp`).

#### Resize handle (`:372-377`, restored only)

- `absolute right-0 top-14 bottom-0 w-4 cursor-col-resize z-50 hover:bg-indigo-500/20 transition-colors rounded-r-3xl`.
- `onMouseDown` starts resize; `handleMouseMove` clamps `300 ≤ newWidth ≤ 1200` (`:139, :152`). Commits to parent via `onResizeWindow`.

### 7.2 Maximized floating controls (`:213-258`)

- Outer `fixed top-0 left-0 right-0 z-[60] flex items-center justify-start h-16 px-6 gap-2 pointer-events-none`.
- Shown only when `isMaximized && isHoveringTop`; `isHoveringTop` becomes true when mouse Y ≤ 80 (`:115`).
- **Title pill** (`:223-228`): `pointer-events-auto flex items-center gap-2 px-4 py-2 rounded-full text-sm font-semibold shadow-[0_8px_32px_rgba(0,0,0,0.4)] backdrop-blur-2xl backdrop-saturate-150 border bg-neutral-900/70 text-white border-white/10`. Icon `drop-shadow-md`; title `max-w-[200px] truncate tracking-wide drop-shadow-md`.
- **Controls pill** (`:233-254`): same chrome; `px-2 py-1.5`. Three buttons `p-1.5 rounded-full hover:bg-white/20 text-white/90 hover:text-white` (Search, Restore, Close). Close hover: `hover:bg-red-500/80`. All icons `w-4 h-4`.
- Entry `{y:-100, opacity:0}` → `{0,1}`. Exit reverse. Transition `{type:'spring', damping:25, stiffness:300}`.

### 7.3 Measurements

| Item | Value |
|------|-------|
| Tab bar height | `h-14` (= 56px) |
| Tab bar horizontal padding | `px-8` (= 32px) |
| Tab mask fade | 40px each edge |
| Tab pill padding | `px-4 py-2` |
| Tab title max-width | `max-w-[120px]` |
| Tab action tray expanded max-width | `max-w-[76px]` |
| Default window width | 600 (normal) / 800 (wide) / 400 (narrow) |
| Window resize bounds | `300 – 1200` |
| Maximized top bar height | `h-16` |
| Content card radius | `rounded-3xl` |
| Maximize control pill z-index | `z-[60]` |
| Restored window z-index | default (unset) |
| Maximized window z-index | `z-50` |

### 7.4 Colors — glass tab

- Focused active pill: `rgba(255,255,255,0.10)` fill, white text, `rgba(255,255,255,0.20)` border.
- Inactive/unfocused pill: `rgba(255,255,255,0.05)` fill, `rgba(255,255,255,0.70)` text, `rgba(255,255,255,0.10)` border.
- Shadow (both): `0 8px 32px rgba(0,0,0,0.20)`.
- Border radius: full (pill).

### 7.5 Interactions

- Click tab → `setActiveTabId`.
- Drag tab → `dataTransfer` carries `{sourceGroupId, tabId}`.
- Drop on tab drop-zone → `onMoveTab(sourceGroupId, tabId, targetIndex)`.
- Drop on content card → `onMoveTab` with no index (appended).
- Search button on tab or header → `onOpenSearch(tab.id)`.
- Maximize button → `setIsMaximized` toggle. Resize drag resets `isMaximized` to false.
- Resize drag updates `localWidth` (`300-1200` clamp); on mouseup calls `onResizeWindow(newWidth)`.
- When maximized, global mousemove listens for y ≤ 80px to show floating pills.

### 7.6 Conditional state

| State | Visual change |
|-------|---------------|
| `isSingleTab` (`:185`) | declared but never read in render — all other logic still renders the tab strip. |
| `isFocused && isActive` | Primary pill styling + white text. |
| `isFocused === false && !isSearching` | Content card `opacity-70`. |
| `isSearching` | Indigo halo ring/shadow around card. |
| `isDragOver && !isMaximized` | Card border indigo + scale 1.02. |
| `isMaximized` | Hides inline tab strip (tab strip only renders `!isMaximized`), fills viewport, pill controls float when hovering top. |
| Group has ≥1 tab with active collaborators | Foggy blurred disc at tab bottom (mix-blend-screen). |

⚠️ Note: `isSingleTab` is computed but unused — the tab strip is always rendered whether single or multi tab.

---

## 8. OverviewView (`OverviewView.tsx`)

**Purpose** — Vertical workspace list view. Each workspace is a card with metadata sidebar + horizontal scroll of mini-window previews. Uses a vertical "rail" with indicator dots.

### 8.1 Structure

- Root (`OverviewView.tsx:44`): `flex-1 bg-slate-950 overflow-y-auto p-12 flex flex-col items-center`.
- Center column (`:45`): `max-w-5xl w-full space-y-12`.
- Header (`:47-50`): `text-center mb-16`. `h1 text-4xl font-bold text-white mb-4 tracking-tight`, `p text-slate-400 text-lg`.
- Rail container (`:52`): `space-y-8 relative`. Rail line (`:54`): `absolute left-8 top-0 bottom-0 w-0.5 bg-slate-800 -z-10`.
- Workspace card (`:60-68`):
  - Base classes: `relative flex items-start gap-8 p-6 rounded-3xl transition-all cursor-pointer border-2`.
  - Active: `bg-slate-900 border-indigo-500 shadow-[0_0_40px_-10px_rgba(99,102,241,0.3)]`.
  - Inactive: `bg-slate-900/50 border-transparent hover:bg-slate-900 hover:border-slate-700`.
  - `whileHover={{scale:1.01}}`.
- Active indicator dot (`:70`): `absolute -left-[35px] top-1/2 -translate-y-1/2 w-4 h-4 rounded-full border-4 border-slate-950`. Bg `bg-indigo-500` active, else `bg-slate-700`.
- **Sidebar** (`:72-126`): `w-64 shrink-0 flex flex-col gap-4`.
  - Title: `h2 text-2xl font-bold text-white mb-1`.
  - WS-ID pill (`:77`): `px-2 py-0.5 rounded-md bg-slate-800 text-slate-300 font-mono text-xs` (`"WS-{y}"`).
  - Active marker "Current": `text-indigo-400 font-medium`.
  - Perms panel (`:83-103`): `bg-slate-950 rounded-xl p-3 border border-slate-800 space-y-2`.
    - Row header: Shield `w-4 h-4 text-emerald-400` + "Permissions" + settings button `p-1 hover:bg-slate-800 rounded-md text-slate-500 hover:text-white`.
    - Badges: `flex gap-2`. "Roles" chip `bg-blue-500/20 text-blue-400 px-2 py-1 rounded-lg text-xs font-medium`. "Files" chip `bg-purple-500/20 text-purple-400`.
  - Collaborators block (`:106-125`): header `Users w-4 h-4 + text` row. Avatars `w-8 h-8 rounded-full border-2 border-slate-900` in `-space-x-2`. Plus-button `w-8 h-8 rounded-full border-2 border-slate-900 bg-slate-800 ... hover:bg-slate-700`.
    - Avatar border color overridden inline: active `#1e1e2e`, else `#0f172a` (= `slate-900`).
- **Mini window row** (`:129-133`): `flex-1 flex items-center gap-4 overflow-x-auto py-4 hide-scrollbar`. One `<MiniWindow>` per group (flattened across strips).

### 8.2 MiniWindow (`OverviewView.tsx:6-32`)

- Width class by group.width: `wide → w-32` (= 128), `narrow → w-16` (= 64), else `w-24` (= 96).
- Card (`:13`): `h-20 flex flex-col gap-1`.
- Tabs row (`:15`): `flex justify-start shrink-0 gap-0.5 px-1`.
  - Dot per tab: `h-2.5 rounded-full border border-slate-600/50`. Active tab: `bg-slate-600/80 w-4`; inactive: `bg-slate-700/50 w-2.5`.
- App card (`:25`): `flex-1 bg-slate-800 rounded-lg border border-slate-700 shadow-lg overflow-hidden flex flex-col`. Interior (`:26`): `flex-1 bg-slate-800/50 p-1`.

### 8.3 Measurements

| Item | Value |
|------|-------|
| Max width | `max-w-5xl` (= 1024px) |
| Card padding | `p-6` |
| Card radius | `rounded-3xl` |
| Rail line X | `left-8` (= 32px) |
| Rail line width | `w-0.5` (= 2px) |
| Sidebar width | `w-64` (= 256px) |
| Mini card height | `h-20` (= 80px) |
| Active indicator X | `-left-[35px]` |

### 8.4 Interactions

- Click workspace card → `onSelectWorkspace(id)` which (in `App.tsx`) sets slide direction by index delta and switches back to desktop view.
- Permissions-settings button stops propagation (`:89`).
- `+` collaborator button stops propagation (`:121`).

### 8.5 Conditional state

- Active workspace: indigo border + indigo dot + glow shadow; "Current" label appears.
- Avatar border color differs subtly between active (`#1e1e2e`) and inactive (`#0f172a`) — a known minor inconsistency ⚠️ (inline override doesn't match the `border-slate-900` class token on inactive cards; keep the class value for inactive, and the inline for active).

---

## 9. CursorOverlay (`CursorOverlay.tsx`)

**Purpose** — Floating remote-collaborator cursor overlay inside an app body.

### 9.1 Structure

- Root (`CursorOverlay.tsx:10`): `absolute inset-0 pointer-events-none z-50 overflow-hidden`.
- Filters collaborators by `activeAppId === appId && cursor`.
- Per collaborator `motion.div` (`:13-29`): `absolute flex flex-col items-start`. Transform `x/y` animated to `cursor.{x,y}`. Transition `{type:'spring', damping:25, stiffness:200}`.
  - `MousePointer2` icon `w-5 h-5 drop-shadow-md`, inline `fill: collaborator.cursorColor`, `color: 'white'`.
  - Label pill (`:23-28`): `px-2 py-0.5 rounded-full text-[10px] font-bold text-white mt-1 shadow-sm`, bg = collaborator color.

### 9.2 Interactions

None — purely presentational.

---

## 10. App stubs (`apps/*.tsx`)

**Purpose** — Static/placeholder content for the four system apps shown inside `WindowFrame`. All mount a `CursorOverlay`.

### 10.1 EditorApp (`apps/EditorApp.tsx`)

- Root: `relative w-full h-full bg-[#1e1e2e] text-[#cdd6f4] font-mono text-sm p-6 overflow-auto flex flex-col`. Base = Catppuccin Mocha palette.
- Import line (`:10`): `flex items-center gap-4 mb-6 text-[#a6adc8] border-b border-[#313244] pb-2`.
- Line rows (`:19-77`): each `flex gap-4`. Gutter `text-[#6c7086] select-none w-6 text-right`. Code indent grows with nesting (`pl-4`, `pl-8`, `pl-12`).
- Syntax colors:
  - Keyword: `#cba6f7` (mauve).
  - Identifier: `#89b4fa` (blue).
  - String: `#a6e3a1` (green).
  - Punctuation: `#89dceb` (sapphire).
  - Element tag: `#f38ba8` (pink).
  - Attribute: `#f9e2af` (yellow).
- Remote-edit highlight (`:49-54`): `bg-pink-500/30 border-b-2 border-pink-500 rounded-sm px-1 mx-1 relative`. Caret: `absolute -right-0.5 top-0 bottom-0 w-0.5 bg-pink-500 animate-pulse`.

### 10.2 TerminalApp (`apps/TerminalApp.tsx`)

- Root: `relative w-full h-full bg-black text-green-400 font-mono text-sm p-4 overflow-auto flex flex-col`.
- Static vite output: `text-blue-400` (user), `text-purple-400` (path), `text-slate-300` (meta), `text-cyan-400` (localhost link), `text-yellow-400` (hmr).
- Blinking caret (`:25`): `w-2 h-4 bg-green-400 animate-pulse`.

### 10.3 BrowserApp (`apps/BrowserApp.tsx`)

- Root (`:7`): `relative w-full h-full bg-white flex flex-col text-slate-800`.
- Chrome (`:12`): `h-12 border-b border-slate-200 flex items-center px-4 gap-4 bg-slate-50`. Three slate-300 dots. URL pill `flex-1 max-w-2xl bg-white border border-slate-200 rounded-full h-8 flex items-center px-4 gap-2 text-sm text-slate-500 shadow-sm` with `Globe w-4 h-4` + host string (switches between `localhost:3000` and `https://figma.com/file/...`).
- Content switches by title:
  - `localhost` variant: centered big block; `w-32 h-32 bg-indigo-100 rounded-3xl` housing `w-16 h-16 bg-indigo-500 rounded-xl rotate-12`; `h1 text-3xl font-bold text-slate-800 mb-2` + slate body text.
  - Figma variant (default): canvas `w-full h-64 bg-slate-50 rounded-xl border-2 border-dashed border-slate-200 text-slate-400 text-center "Design Canvas"` + 3-col `grid-cols-3` of `h-24 bg-slate-100 rounded-xl` placeholders.

### 10.4 FilesApp (`apps/FilesApp.tsx`)

- Root: `relative w-full h-full bg-white/80 backdrop-blur-xl flex text-slate-800`.
- Sidebar (`:86-141`): `motion.div` animated `width: 0` (collapsed) or `sidebarWidth` (100-400, default 180) with `{type:'spring', damping:25, stiffness:200}`. Bg `bg-slate-900/5 border-r border-black/5`.
  - Inner `p-4` container `min-w-[140px]`.
  - Section labels: `text-[10px] font-bold text-slate-400 uppercase tracking-widest mb-2`.
  - Active shortcut row (`:96`): `flex items-center justify-between px-2 py-1.5 bg-indigo-500/10 text-indigo-700 rounded-lg font-medium text-sm cursor-pointer hover:bg-indigo-500/20 transition-colors`. Dot for each active collaborator `w-2.5 h-2.5 rounded-full border border-white` with cursorColor bg.
  - Regular shortcut: `px-2 py-1.5 text-slate-600 hover:bg-black/5 rounded-lg`. Folder icon `text-blue-500/70`.
  - System row (OS Disk): `text-slate-400 cursor-not-allowed italic`.
  - Divider (`:125`): `my-3 border-t border-black/5 mx-2`.
  - Resize handle (`:136-139`): `absolute right-0 top-0 bottom-0 w-1 cursor-col-resize hover:bg-indigo-400/30 active:bg-indigo-500/50 transition-colors z-10`.
- Main content (`:144-179`): `flex-1 p-8 relative overflow-y-auto`.
  - Collapse toggle (`:146-152`): `absolute top-4 left-4 p-1.5 bg-white/50 backdrop-blur-md border border-black/5 rounded-lg text-slate-500 hover:text-slate-800 hover:bg-white/80 shadow-sm z-20 transition-all`. Icon `PanelLeftOpen`/`PanelLeftClose` `w-4 h-4`.
  - Grid (`:154`): `grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-6 mt-10`.
  - File/folder tile (`:163-178`): `motion.div draggable`, classes `flex flex-col items-center gap-3 p-4 rounded-2xl cursor-pointer transition-colors group`. `whileHover {scale:1.02, backgroundColor:'rgba(0,0,0,0.03)'}`; `whileTap {scale:0.98}`.
    - Folder icon: `Folder w-12 h-12 text-blue-500 fill-blue-500/10 group-hover:fill-blue-500/20`.
    - File icons by extension: orange `FileCode2` (package.json), slate `FileText` (README.md), purple `FileImage` (logo.png).
    - Label: `text-sm font-semibold text-slate-700`.
- Context menu (`:183-202`): `fixed z-[100] bg-white/90 backdrop-blur-2xl border border-black/5 shadow-2xl rounded-xl py-1 min-w-[160px]`. Entry `{opacity:0,scale:0.95}`→`{1,1}`. Remove button `w-full text-left px-4 py-2 text-sm text-red-600 hover:bg-red-50 transition-colors flex items-center gap-2` with `X w-3.5 h-3.5`.

### 10.5 Drag & drop protocol

- Mime-type `application/x-os-item` with JSON `{ name, type }`.
- DesktopView consumes this to infer which app to open (`code` → editor, otherwise files).

---

## Appendix A — Global CSS utilities

From `nirios-concept/src/index.css`:

- `.hide-scrollbar` — hides WebKit/Firefox/IE scrollbars.
- Import: `@import "tailwindcss";` — Tailwind v4 single-import mode.

## Appendix B — Keyboard shortcuts

| Key | Effect | Source |
|-----|--------|--------|
| `Cmd/Meta + Tab` | Toggle desktop ↔ overview | `App.tsx:71` |
| `Cmd/Meta + K` | Open GlobalSearch (empty context) | `App.tsx:75` |
| `Escape` (in search) | Close search | `GlobalSearch.tsx:80` |
| `ArrowUp/Down` (in search) | Cycle selection | `GlobalSearch.tsx:81-85` |
| `Enter` (in search) | Activate selection | `GlobalSearch.tsx:87` |

## Appendix C — Layer z-index map

| Layer | z-index |
|-------|---------|
| Grid/noise overlay | (baseline) |
| Strips inactive | `z-10` |
| Strips active | `z-20` |
| Sidebar wheel | `z-40` |
| Top bar | `z-50` |
| Cursor overlay | `z-50` |
| Maximized window content | `z-50` (container), content card also uses `z-50` when searching |
| Maximized float controls | `z-[60]` |
| User menu popover | `z-[70]` |
| QuickSettings popover | `z-[70]` |
| Modals (settings, search) | `z-[100]` |
| Files context menu | `z-[100]` |
| Drag preview | `z-[200]` |
