# Nirios Shell + Collaborative Toolkit — Design Spec

**Date:** 2026-04-18
**Author:** Quinten (with Claude assistance)
**Status:** Draft, awaiting implementation plan
**Source concept:** `nirios-concept/` (React/TypeScript prototype; reference for visual + interaction design only — not a fidelity target)

## 0. Purpose & guiding principles

Build a collaborative, cross-platform desktop shell and the UI toolkit that powers it, both written in pure Rust. Two sentences of intent:

1. The shell is a reimplementation of the nirios visual concept as the real UI of QuickDesktop, replacing the existing Iced-based UI in `quick-gnome-shell`.
2. The toolkit (`quick-ui`) grows into a collaboration-first, cross-platform UI framework that can compete with the web — it is the foundation for every system app, not just the shell.

**Design language parity:** the shell and all native apps share one design-token system, one motion language, one set of collaboration affordances, and one widget contract. Apps should look and feel like part of the OS.

**Collaboration is part of the contract, not an add-on.** Every new widget in this plan is built with presence hooks and a permission gate from day one, even if only a subset exercises the full collab surface in v1.

**Concept vs. product.** The nirios concept is an intentional low-fidelity prototype (only 3 workspaces, specific mock apps, cosmetic visual language). The product is more ambitious: infinite workspaces, real apps, persistent-and-potentially-CRDT-synced state. Fidelity benchmarks are this spec's targets, not the concept's.

## 1. Architecture & module layout

### Crate-level changes

- **`quick-ui`** (in-tree) — grows substantially. New primitives, a design-token module, a collaboration-contract module, an animation/spring engine, an overlay/modal subsystem, a drag-and-drop manager, a scroll area, a tabs widget, a command palette, a context-menu + tooltip + toast layer, a data-table primitive, a virtualized list, a richer text-input (presence-aware, IME plumbing). Existing widgets are *extended* to the new contract, not rewritten.
- **`quick-gnome-shell`** (submodule) — UI replaced. Its Iced `Application` is swapped for a `quick_ui::App` driving a `nirios::Shell` root widget. Non-UI code (Wayland/Smithay scaffolding, `workspace.rs`, `security/`, `network/` with libp2p, `config.rs`) remains. Iced dependencies removed from `Cargo.toml` unless any non-UI module still uses them.
- **`apps/terminal`** (submodule, already on `quick-ui`) — preserved. Gets an `AppWidget` impl and adopts the new tokens/presence hooks.
- **`apps/file-explorer`** (submodule, currently Iced) — **ported to `quick-ui`** as part of this plan, preserving every capability it has today. Its Iced code is removed once port parity is verified.
- **`apps/browser`** (submodule, Servo + Iced) — exempt. Stays as-is. Represented in the shell as a `TileContent::Stub` card ("Browser coming soon"), with an optional "open in external browser" button invoking `xdg-open`.

### New module tree inside `quick-gnome-shell`

```
src/shell/
  mod.rs              nirios::Shell root widget, app-level state
  state.rs            WorkspaceTree (Workspace → Strip → WindowGroup → Tab)
  workspace_view.rs   active workspace (strips + groups + tabs)
  strip.rs            paginated strip with 2D slide transitions
  window_group.rs     horizontal tiles, drag-resize, hosts tabs
  tab_bar.rs          tabbed bundle, drag-drop source/target
  tile.rs             TileContent enum (App | Stub | reserved: Wayland)
  topbar.rs           workspace nav, widgets, collaborators, tray
  overview.rs         all-workspaces grid view
  search.rs           Cmd+K palette
  quick_settings.rs
  workspace_settings.rs
  sidebar_wheel.rs    2.5D wheel + overscroll-create gesture
  remote_cursors.rs
  nirios_tokens.rs    nirios-specific token overrides on top of quick-ui tokens
```

### New module tree inside `quick-ui`

```
src/
  animation/          spring, tween, easing, motion tokens
  tokens/             design-token system (colors, type, spacing, radii, elevation, motion)
  overlay/            modal, popover, drawer, scrim, focus trap
  dnd/                drag-drop manager, typed payloads, drop-target registration
  presence/           PresenceKey, PresenceCtx, remote cursor/selection plumbing
  permissions/        PermissionScope, PermissionContext, gated-widget helpers
  input/              pointer/gesture abstraction (mouse today, room for touch/stylus)
  backend/            windowing-backend trait; winit impl (only backend in v1)
  clipboard/          clipboard provider trait
  widget/
    scroll.rs         Scrollable with momentum, scroll-into-view, sticky
    tabs.rs           Tabs/TabBar with drag-reorder + detach
    context_menu.rs
    tooltip.rs
    toast.rs
    command_palette.rs  searchable/fuzzy command list
    tree.rs
    data_table.rs     sortable, resizable columns, virtualized
    virtual_list.rs
    rich_text.rs      multi-line rich/monospace text input with selection + IME preedit
    (existing widgets stay in place; they all implement the extended trait)
```

### Dependency direction

`quick-ui` is the base. `apps/terminal` and (post-port) `apps/file-explorer` depend on `quick-ui`. `quick-gnome-shell` depends on `quick-ui` and on the two native apps so it can embed them as `AppWidget` instances inside shell tiles. `apps/browser` is not a shell dependency.

### Run modes

- **Dev mode (primary for this plan):** `cargo run -p quick-desktop` opens a winit window with the nirios shell; `AppWidget` instances run as in-process widget trees in tabs.
- **Compositor mode (deferred):** the smithay Wayland backend continues to exist and is not rewired to the new shell UI. `TileContent::WaylandSurface` is a reserved variant with a `todo!()` impl so the future rewire is mechanical. See §10.

## 2. Widget contract extensions

The existing `Widget` trait in `quick-ui/src/widget/mod.rs` gains the following additions. All existing widgets implement the defaults (no behavior change) and the new widgets opt in where it matters.

```rust
pub trait Widget {
    // ... existing methods (sync, build_layout, apply_layout, draw, handle_event, ...)

    /// Stable identity used to look up presence and permissions.
    /// Typically derived from the widget's CRDT path.
    fn presence_key(&self) -> Option<PresenceKey> { None }

    /// Which permission, if any, gates this widget.
    fn permission_scope(&self) -> Option<PermissionScope> { None }

    /// Declares what drop payloads this widget accepts.
    fn accepts_drop(&self, _payload: &DragPayload) -> bool { false }
    fn on_drop(&mut self, _payload: DragPayload, _state: &mut AutoCommit, _path: &ObjId) {}

    /// Declares whether this widget owns pointer capture while a gesture is active.
    fn wants_pointer_capture(&self) -> bool { false }
}
```

`draw` gains a `PresenceCtx` alongside `Theme`, so widgets can render peer carets, peer selections, hover ghosts, and "who's editing this" avatars using shared helpers. Widgets opting out read neither.

**Migration note.** Extending the `draw` signature and adding the new trait methods is a breaking change to every existing `Widget` impl. Phase A step 1 performs a mechanical migration of every widget in the workspace (`quick-ui`'s built-ins, terminal app, and any in-tree consumers) as part of landing the token + presence/permission scaffolding.

The framework resolves `permission_scope` against a `PermissionContext` before dispatching events — denied widgets render in a disabled/read-only style and reject mutations. The context is single-user local in v1; peer-enforced in deferred scope.

## 3. Design-token system

A single `Tokens` struct, attached to `Theme`, that every widget reads from. Shell and apps share it. No hard-coded styling in widget code.

- **Colors:** 11-stop neutral ramp (`neutral_0` … `neutral_1000`), one accent ramp, one set of semantic colors (`success`, `warning`, `danger`, `info`), one set of surfaces (`surface_base`, `surface_raised`, `surface_overlay`), one set of "glass" surface tokens (translucent + backdrop blur radius).
- **Typography:** 8-step type scale (`xs`, `sm`, `base`, `lg`, `xl`, `2xl`, `3xl`, `4xl`), weight scale, two families (sans, mono). Line heights expressed as multipliers.
- **Spacing:** 4-pixel base scale (`0, 1=4, 2=8, 3=12, 4=16, 6=24, 8=32, 12=48, 16=64`).
- **Radii:** `sm, md, lg, xl, full`.
- **Elevation:** 5-step shadow scale + the "glass" token pair.
- **Motion:** named spring presets (`subtle`, `snappy`, `bouncy`), ease curves (`linear`, `standard`, `emphasized`), duration steps (`fast`, `base`, `slow`).

Two themes ship in v1: `dark` (default) and `light`. Existing quick-ui themes (Dracula/Nord/Gruvbox/Solarized) are re-expressed as tokens so they still work.

`nirios_tokens.rs` in the shell overrides motion and elevation where the OS chrome needs more dramatic presence (e.g. overview-view scale, sidebar wheel perspective), but it extends rather than replaces the base tokens.

## 4. Collaboration contract

### Presence

- `PresenceKey` — enum of stable identifiers: `Widget { path: Vec<Segment> }`, `Tab { id }`, `Workspace { id }`, custom `App { app: AppKind, sub: String }`.
- `PresenceCtx` per frame: `who's here`, `whose pointer is over this key`, `whose selection intersects`. Built from remote events delivered by `CollaborationManager` (already exists; `RemoteCursorMoved` generalizes to `RemotePresenceUpdate` carrying a `PresenceKey` + payload).
- Default rendering helpers: peer caret (colored line + small label), peer selection (colored highlight at reduced opacity), peer pointer (avatar ghost with name), peer "focus" (subtle border). Widgets call these helpers from `draw` when they have a `presence_key`.
- Rich-text widgets natively render peer carets + selections (since Automerge already carries text structure and peer carets are simple positions).

### Permissions

- `PermissionScope { resource: ResourceId, op: Op }`. `ResourceId` is one of `App(AppKind)`, `File(FileId)`, `Workspace(WorkspaceId)`. `Op` is `View | Edit | Admin`.
- `PermissionContext` is provided by the root app; widgets read it during event dispatch. Denied widgets render in their "disabled/read-only" visual and reject mutations.
- v1 is single-user locally-enforced. The editor in WorkspaceSettings produces real `Role` and `RolePermissions` data that's stored in the workspace state; wire-side enforcement across peers is deferred.

### Per-user undo

The existing `UndoEntry` in `quick-ui/src/state/mod.rs` gains an optional `author: UserId` field. In multi-user mode, undo skips entries authored by other users; in single-user mode, the field is ignored. v1 plumbs the field but operates single-user.

### Follow-mode, shared clipboard, concurrent selection, voice/video

All deferred. See §10.

## 5. Shell data model & screens

### Data model (in-memory for v1; persisted as JSON; migration to CRDT deferred)

```rust
pub struct WorkspaceTree {
    pub workspaces: Vec<Workspace>,        // ordered; "infinite" via dynamic append/prepend
    pub active: WorkspaceId,
    pub active_strip: HashMap<WorkspaceId, StripId>,
    pub collaborators: Vec<Collaborator>,
}

pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub strips: Vec<Strip>,
    pub roles: Vec<Role>,
    pub workspace_files: Vec<WorkspaceFile>,
}

pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub window_groups: Vec<WindowGroup>,
}

pub struct WindowGroup {
    pub id: GroupId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
    pub width: WindowWidth,     // Narrow(400) | Normal(600) | Wide(800) | Custom(px in 300..=1200)
}

pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub tile: TileContent,
}

pub enum TileContent {
    App(Box<dyn AppWidget>),
    Stub { kind: AppKind, label: String },
    // reserved: Wayland(SurfaceId)
}
```

### Screens

- **ShellRoot** — `Column([TopBar, Stack([WorkspaceView(active), OverviewView])])` with overview toggle.
- **WorkspaceView** — `Row([SidebarWheel(strips), StripView(active_strip)])`.
- **StripView** — horizontal scroll area of `WindowGroup`s, paginated by wheel with cooldown.
- **WindowGroup** — `Column([TabBar, TileHost])`. Right-edge drag handle resizes.
- **TopBar** — `Row([LeftNav, CenterWidgets, RightTray])`. `LeftNav` = overview toggle + workspace up/down arrows + workspace-name button opens WorkspaceSettings. `CenterWidgets` = clock + system monitor (CPU/RAM sparkline) + calendar widget, cycled by wheel. `RightTray` = collaborator avatars + QuickSettings trigger.
- **OverviewView** — vertical list of workspace mini-cards (each card shows strips as miniatures, not live renders in v1). Click activates.
- **GlobalSearch** — command palette: text input + result list + context-aware filters (e.g. filter to files+editor when opened from a file context). Opens with Cmd+K; closes on Escape or click outside.
- **QuickSettings** — popover: brightness + volume sliders, WiFi/Bluetooth toggles (with multi-level drill-in), buttons for Settings (opens WorkspaceSettings), Camera, Power.
- **WorkspaceSettings** — modal with three tabs: **General** (workspace name, invite link + copy, collaborators list with remove), **Roles & Permissions** (left role list, right panel with app-permission checkboxes + per-file permission matrix using the `DataTable` primitive), **Files** (attached workspace files + visual per-role permission summary).
- **SidebarWheel** — left-side 2.5D wheel showing strips of the active workspace along a vertical circular path. Overscroll at the top/bottom commits a new workspace (with "New session" hint during the gesture).
- **RemoteCursors** — overlay layer rendering peer cursors over the active tile, driven by `PresenceCtx`.

## 6. Interactions & gestures

- **Cmd+K** → open `GlobalSearch` (focus-trapped).
- **Cmd+Tab** → toggle `OverviewView`.
- **Cmd+Up / Cmd+Down** → switch workspace (with slide transition).
- **Wheel on strip area** → paginate strips with 600 ms cooldown; direction follows sign.
- **Drag on window-group right edge** → resize; clamped to `[300, 1200]` px.
- **Drag a tab** → `DragPayload::Tab { group, tab }`. Drop targets: `TabDropZone` between tabs (reorder/insert within any group), "+"-zone at strip edge (new group).
- **Wheel on `SidebarWheel`** → switch strips within the current workspace; at the top/bottom ends, continued scroll past a threshold triggers **overscroll-create**. A growing "New session" hint appears during the gesture. Release past the commit threshold creates and activates a new workspace; release before the threshold snaps back.
- **Right-click anywhere** → context menu via the toolkit primitive (shell provides a default set; widgets/apps can extend).
- **Hover** → tooltip, with a small reveal delay.
- **Toast** notifications for transient info (e.g. "Workspace created").

## 7. App integration

### `AppWidget` trait

```rust
pub trait AppWidget: Widget {
    fn app_kind(&self) -> AppKind;
    fn app_title(&self) -> &str;
    fn app_icon(&self) -> IconDescriptor;
    fn permission_hint(&self) -> Option<AppKind>;  // governs role gating
    fn commands(&self) -> Vec<AppCommand> { vec![] }  // command-palette entries
}
```

Tiles host an `AppWidget` via a `TileHost` widget. The shell owns layout (`build_layout`/`apply_layout`) and delegates to the app for content.

### File-explorer port

New `apps/file-explorer/src/` structure: `lib.rs`, `files_app.rs` implementing `AppWidget`.

**Pre-port audit step.** Before porting, inventory the current Iced implementation's user-visible behavior (sidebar, navigation, selection, drag-drop, context menu, sort, filter) and capture it as an explicit feature list + test expectations in the port's test suite. The inventoried behavior is the parity bar; any ambiguity in the current code resolves to "the port is the new source of truth."

**Expected feature list** (to be refined by the audit):

- Sidebar: drag-resize, collapse/expand toggle, shortcut list with drag-reorder + drop-new shortcut from the file grid, right-click remove.
- Main view: file items with icon + title + size + modified. List mode uses `DataTable`; icon mode uses a custom grid.
- Selection: whatever the Iced version supports today (single + multi at minimum).
- Context menu on a file: Open, Reveal, Rename, Delete (to trash), Copy, Move.
- Drag items from grid → drop to sidebar shortcuts or folders in the grid (move/copy) as currently supported.
- Breadcrumbs and path-bar navigation.
- Sort and filter as currently supported.

Existing `walkdir`, `trash`, `open`, `infer`, `mime_guess`, `humansize` crates stay.

### Terminal preservation

- Existing PTY + VT parser code unchanged.
- Minimal refactor: implement `AppWidget` around the existing widget tree; adopt `Tokens::font_mono` and terminal-palette token overrides.
- Existing integration tests remain green.

### Browser stub

- `TileContent::Stub { kind: AppKind::Browser, label: "Browser" }` rendered as a card with icon + headline + "Open in external browser" button invoking `xdg-open`.
- No Servo work in this plan.

## 8. Phasing & ordering

Approach 3 from the brainstorm: toolkit + shell co-developed, app port last, collab contract hardened at close.

### Phase A — Toolkit + shell, co-developed (each step demoable)

1. Design-token module + `Theme` extension. Port existing widgets to consume tokens.
2. Animation/spring engine + motion tokens.
3. Overlay subsystem (modal + popover + scrim + focus trap).
4. Drag-drop manager + typed payloads + `Tabs`/`TabBar` widget with reorder + detach.
5. `Scrollable` (vertical + horizontal, momentum, scroll-into-view, sticky).
6. `TileContent` + `TileHost` + `WindowGroup` with resize; `Strip` model + paginated `StripView`; wheel paginate with cooldown.
7. `WorkspaceView` + `WorkspaceTree` state + workspace up/down switch with slide.
8. `OverviewView`.
9. `CommandPalette` primitive + `GlobalSearch` using it.
10. `QuickSettings` popover (multi-level nav + slider polish).
11. `DataTable` primitive + `WorkspaceSettings` modal using it.
12. `TopBar` (workspace nav, cycling widgets, collaborators, tray).
13. `SidebarWheel` + overscroll-create gesture.
14. `PresenceCtx`-driven remote-cursor overlay.
15. `ContextMenu`, `Tooltip`, `Toast` primitives (land with their first shell consumer).

### Phase B — Apps

1. `AppWidget` trait + terminal refactor to implement it.
2. File-explorer port (sidebar, grid, context menu, drag-drop, sort/filter, `DataTable` list mode). Iced code removed after parity tests pass.
3. Browser stub tile with external-browser fallback.

### Phase C — Collaboration contract hardening

1. Generalize `PresenceCtx` coverage to every widget; verify opt-in chrome helpers work uniformly.
2. `PermissionContext` + gated-widget helpers; wire into `WorkspaceSettings` editor.
3. Per-user-undo extension: `UndoEntry::author`, single-user mode operates as before.
4. Design-token audit: shell chrome + ported file-explorer + terminal all consume tokens; no hard-coded styling remains.

## 9. Testing & verification

- **`quick-ui` unit tests** per new primitive (construction, layout measurement, basic event handling).
- **Headless render tests** in `quick-ui`: feed canned events, capture Vello scene, assert structural invariants (bounds, layer counts, token-derived values) rather than pixels.
- **Shell interaction tests** — new `tests/shell_interaction.rs` in `quick-gnome-shell` driving `nirios::Shell` with synthetic events. Required assertions: workspace switch navigates; Cmd+K opens search; tab drag between groups moves state; tab drag to "+"-zone creates a new group; overscroll past threshold appends a workspace; strip paginate switches active strip with cooldown honored.
- **Port-parity tests** for file-explorer — each existing capability has a matching test in the `quick-ui` port before the Iced code is removed.
- **Collab tests** — simulated multi-user session with two `App` instances sharing `CollaborationManager` over an in-memory pipe. Assertions: peer cursor positions render at the right screen coords under layout changes; presence chrome updates on peer join/leave; permission-gated widget rejects mutation and renders read-only.
- **Visual smoke** — `cargo run -p quick-desktop` in dev mode is the primary integration check throughout Phase A and B. Mandatory run after each step in §8.
- **Compositor headless test** (`cargo test --test interaction_test` in `quick-gnome-shell`) continues to run unchanged — the shell-UI tests are an additional suite.

## 9a. Error handling & failure modes

- **Toolkit APIs.** Widget trait methods return values, not `Result`. Errors that cross the widget boundary (CRDT sync failure, IPC disconnect, asset load failure) are funnelled through a small `ShellError` enum surfaced as toasts via the toast primitive. No panic paths in event handlers.
- **Collaboration IPC.** `CollaborationManager` already reconnects on Unix-socket disconnect; surface a presence-degraded indicator in the topbar when the socket is down. Presence/permission queries fall back to single-user defaults.
- **AppWidget load failure.** A tile whose backing `AppWidget` fails to construct renders a `TileContent::Stub` with an error label plus a retry button. The shell never unwinds on an app failure.
- **Persistence.** Reading/writing `~/.config/quick-desktop/state.json` tolerates corruption by falling back to a default workspace + a toast. Writes are atomic (tmpfile + rename).
- **Overscroll-create.** If the workspace create operation fails (rare — it's in-memory in v1), the gesture snaps back with a toast rather than leaving a half-created workspace.
- **File-explorer port.** All fallible OS operations (trash, rename, move, copy) surface as toasts; the existing Iced app's failure semantics are the minimum bar.

## 10. Deferred scope & follow-ups

The following are intentionally out of scope for this plan but are tracked so design decisions here stay compatible. A companion file at `docs/superpowers/deferred/nirios-shell.md` carries the running list.

1. **Full CRDT shell state.** `WorkspaceTree` lives in Automerge; distributed edits with conflict semantics (two users dragging the same tab); permissions enforced wire-side across peers.
2. **High-fidelity animations.** 3D strip transitions (`perspective` + `rotateX` + scale + blur, macOS-Dock-style stacking); layout animations for tab add/remove; shared-element transitions for overview; `SidebarWheel` true circular geometry with sin/cos radial layout. v1 ships 2D slide/fade + spring physics only.
3. **Full compositor integration.** `TileContent::WaylandSurface` implementation with nested smithay compositor, `xdg-shell` clients hosted in tabs, buffer-to-wgpu-texture pipeline (shm + dmabuf), seat/input routing, coordinate transforms, tile-drag with live surface handoff, eventual DRM/KMS backend for running as the real session compositor.
4. **Cross-platform backends.** Windowing-backend trait is introduced but only the winit impl ships; cocoa/win32 impls deferred.
5. **Full collaborative-toolkit horizon.** Live data-table co-editing; shared-canvas drawing with peer strokes; follow-mode / remote-viewport sharing; shared clipboard with permission prompts; concurrent selection visualization in file-explorer; live video/voice chat embed.
6. **Native browser.** A `quick-ui`-based browser replacing Servo-based `quick-browser` is out of scope.
7. **Accessibility & RTL.** Widget trait hooks (focus, tab order) exist; AT-SPI integration and RTL layout are deferred.
8. **Persistence.** v1 persists `WorkspaceTree` as JSON at `~/.config/quick-desktop/state.json`. Filesystem-backed + CRDT-synced persistence is deferred.
9. **Full IME.** `rich_text` lays preedit plumbing groundwork; complete compositional-input rendering + candidate windows deferred.
10. **Gesture abstraction beyond mouse.** Touch and stylus support deferred; wheel + drag are the only gestures in v1.

## 11. Risks & open questions

- **Scope size.** This plan is intentionally large (approach 3 from brainstorm, everything in one plan). If the plan grows past a single-plan threshold during writing-plans, the natural split points are: Phase A steps 1–7 (foundation), Phase A steps 8–15 (screens), Phase B (apps), Phase C (collab). Splitting is a plan-level decision, not a spec-level one.
- **`quick-ui` API churn.** Rapid co-development of toolkit + shell (Phase A) will break toolkit APIs frequently. Absorbing the churn inside one plan is a feature, not a bug — the goal is for APIs to stabilize under real use before the file-explorer port starts (Phase B).
- **File-explorer port parity.** Exact parity requires a per-feature test suite. If any existing behavior is ill-defined in the current Iced code, it is captured as a test expectation in the port, and the port is the new source of truth.
- **Design-token coverage.** Existing `quick-ui` widgets hard-code some styling; audit in Phase C guarantees no hard-coded values remain.
- **Infinite workspaces + persistence.** "Infinite" in v1 means dynamic append/prepend of empty workspaces via overscroll-create or the workspace-management UI; total count is bounded only by memory/disk. Truly unbounded + lazy-loaded is a filesystem-backed-persistence concern, which is deferred.
