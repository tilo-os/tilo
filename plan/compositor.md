# Spatial Compositor Design

## Architecture

The compositor is a single Rust binary (`quick-desktop`) with two main threads:

1. **UI Thread**: Runs the Iced application loop, renders the spatial canvas, handles user input
2. **Wayland Thread**: Runs the Smithay Wayland server, manages client connections, captures pixel buffers

Communication between threads via `tokio::sync::mpsc` channels sending `WaylandEvent` messages.

## Core Data Model

```
WorkspaceState
├── zones: Vec<Zone>           # Rectangular regions on the canvas
│   ├── id, name, position, size
│   ├── project_root: Option<String>
│   └── decks: Vec<Deck>      # Containers within a zone
│       ├── id, position, size, active_tab
│       └── tabs: Vec<Tab>    # Individual app windows
│           ├── title, command
│           ├── buffer: Option<Vec<u8>>     # Captured pixels
│           └── buffer_size: Option<(u32, u32)>
├── users: Vec<UserCursor>     # Remote collaborator cursors
├── offset: Point              # Canvas pan offset
└── focused_zone/deck: Option<usize>
```

## Rendering Pipeline

### Current State (Iced Canvas)
- Infinite grid background drawn with canvas paths
- Zones rendered as bordered rectangles with labels
- Decks rendered within zones with tab strips
- Tabs show command text or "[PIXELS CAPTURED]" placeholder

### Target State (Phase 1)
- Captured pixel buffers rendered as actual images via `iced::widget::image::Handle::from_rgba()`
- Images positioned at deck coordinates on the canvas
- Smooth panning and zoom transitions

### Pixel Capture Path
```
Wayland client commits surface -> Smithay CompositorHandler::commit()
  -> Access SHM buffer via with_buffer_contents()
  -> Copy RGBA pixels to Vec<u8>
  -> Send WaylandEvent::Frame { app_id, pixels, width, height }
  -> Main thread matches app_id to Tab, stores buffer
  -> Canvas re-renders with actual image
```

## Input Architecture

### Local Input
```
Physical keyboard/mouse -> Iced event -> Message enum
  -> Update workspace state (focus, pan, shortcuts)
  -> Forward to focused Wayland client's seat (Phase 1)
```

### Remote Input (Phase 3)
```
Network InputEvent -> Validate (region check, rate limit, key filter)
  -> Transform coordinates (remote canvas -> local surface)
  -> Inject via Smithay seat into target Wayland client
```

### Keyboard Shortcuts
| Shortcut | Action |
|----------|--------|
| Cmd+Enter | Create new zone |
| Cmd+Backspace | Delete focused zone |
| Cmd+T | New tab in focused deck |
| Cmd+W | Close focused tab |
| (Future) Cmd+S | Share focused zone |

## The Dual-State Problem

Two Smithay state implementations currently exist:

1. **`state.rs`** (215 lines): Full implementation with Space, XDG shell, layer shell, seat, data device, output handlers. Never instantiated in `main.rs`.

2. **`network/wayland.rs`** (85 lines): Minimal implementation with just compositor + SHM. This is what `main.rs` actually uses.

### Resolution Plan (Phase 1)
- Merge the comprehensive handler delegation from `state.rs` into the WaylandServer in `network/wayland.rs`
- Delete `state.rs` after migration
- The unified server needs: XDG shell (for proper window management), seat (for input forwarding), and the existing compositor + SHM handlers

## Wayland Session Modes

### Nested Mode (Development)
- quick-desktop runs as a Wayland client of an existing compositor
- Iced creates a window via winit on the host display
- The internal Smithay server uses a separate socket for child apps
- `WAYLAND_DISPLAY` is set to the internal socket for spawned apps

### Standalone Mode (Production)
- quick-desktop IS the compositor, started by greetd on bare DRM/KMS
- Iced/winit initializes directly on the GPU via wgpu DRM backend
- All Wayland clients connect to the single Smithay server
- Fallback: wrap with `cage` if bare DRM init fails

## Damage-Aware Streaming (Phase 3)

Since the compositor owns all Wayland surfaces, it knows exactly which pixels changed:

```
Smithay damage event: "Surface X changed at rect (100, 100, 200, 50)"
  -> If no damage: send nothing (zero bandwidth for static content)
  -> If damage: capture only the damaged region
    -> PipeWire DMA-BUF (zero-copy from GPU)
    -> GStreamer encode with ROI (Region of Interest)
      - Static areas: high quality, low frequency (QP=0, sharp text)
      - Dynamic areas: force I-frame, encode immediately
    -> AV1 codec (screen content coding tools for sharp text)
    -> WebRTC transport to remote peer
```
