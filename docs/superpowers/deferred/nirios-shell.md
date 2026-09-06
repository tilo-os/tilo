# Nirios Shell — Deferred Scope & Follow-ups

Tracker for work intentionally excluded from the 2026-04-18 Nirios shell design (see `docs/superpowers/archive/2026-04-18-nirios-shell-design.md`). Design decisions in the main plan must stay compatible with the items here; none of them are "never" — all are "next".

## Index

1. Full CRDT shell state (option (c))
2. High-fidelity animations (3D strip stack, layout transitions, true circular wheel)
3. Full compositor integration (options (ii) and (iii))
4. Cross-platform backends (cocoa, win32)
5. Full collaborative-toolkit horizon (shared canvas, follow-mode, shared clipboard, etc.)
6. Native `quick-ui` browser
7. Accessibility (AT-SPI) and RTL layout
8. Filesystem-backed + CRDT-synced persistence
9. Full IME (preedit rendering, candidate windows)
10. Gesture abstraction (touch, stylus)

## Details

### 1. Full CRDT shell state
`WorkspaceTree` lives in Automerge; distributed edits with conflict semantics (two users dragging the same tab), permission enforcement wire-side.
**Design constraint kept in v1:** state access goes through a `WorkspaceTree` API surface that is replaceable with a CRDT-backed impl without touching shell screens.

### 2. High-fidelity animations
3D strip transitions (`perspective` + `rotateX` + scale + blur, macOS-Dock-style stacking); layout animations for tab add/remove; shared-element transitions for overview; `SidebarWheel` true circular geometry with sin/cos radial layout.
**Design constraint kept in v1:** animation engine is spring-physics-based from day one, so upgrading from 2D slide/fade to 3D/layout-animation is additive.

### 3. Full compositor integration
`TileContent::WaylandSurface` implementation with nested smithay compositor, `xdg-shell` clients hosted in tabs, buffer-to-wgpu-texture pipeline (shm + dmabuf), seat/input routing, coordinate transforms, tile-drag with live surface handoff, eventual DRM/KMS backend for real-session use.
**Design constraint kept in v1:** `TileContent` enum has the `WaylandSurface` variant reserved; `TileHost` layout is surface-agnostic.

### 4. Cross-platform backends
Windowing-backend trait introduced in v1, winit impl only. Cocoa + win32 impls deferred.
**Design constraint kept in v1:** no direct winit types leak past `quick-ui/src/backend/`.

### 5. Full collaborative-toolkit horizon
Live data-table co-editing; shared-canvas drawing; follow-mode / remote-viewport sharing; shared clipboard; concurrent selection visualization; live video/voice embed.
**Design constraint kept in v1:** every widget has `presence_key` + `permission_scope` hooks; extending any of the above to use them does not require trait changes.

### 6. Native browser
A `quick-ui`-based browser replacing the Servo/iced `quick-browser`. Out of scope.
**Design constraint kept in v1:** browser stub uses same `TileContent::Stub` mechanism that any future app can use.

**Later status:** completed by `apps/browser`; the shell now hydrates the real
`BrowserWidget` contract and production launches its packaged WPE process. The
paragraph above remains as the v1 historical constraint, not current behavior.

### 7. Accessibility & RTL
AT-SPI integration on Linux, full keyboard nav audit, high-contrast theme, RTL layouts.
**Design constraint kept in v1:** no hard-coded left/right directional assumptions in layout widgets where they can be avoided; focus model is already directional-neutral.

### 8. Persistence
v1 persists `WorkspaceTree` as JSON at `~/.config/quick-desktop/state.json`. Filesystem-backed (stored inside `quick-fs-fuse`) + CRDT-synced persistence deferred.
**Design constraint kept in v1:** serialization boundary lives in `WorkspaceTree`, not in shell screens.

### 9. Full IME
`rich_text` lays preedit plumbing groundwork; complete compositional-input rendering + candidate windows deferred.
**Design constraint kept in v1:** `rich_text` widget exposes a preedit slot even if nothing fills it yet.

### 10. Gesture abstraction
Touch and stylus support. v1 handles mouse wheel + drag only.
**Design constraint kept in v1:** `input/` module exposes a pointer abstraction, not raw winit mouse events, so pointer-source can be extended without rewriting consumers.
