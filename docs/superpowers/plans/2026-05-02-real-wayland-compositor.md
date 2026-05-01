# Real Wayland Compositor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert `quick-desktop` from a single winit Wayland *client*
into a real smithay-based Wayland *compositor* hosting both the in-tree
shell chrome and third-party Wayland clients adopted into the tab
model.

**Architecture:** Smithay 0.6+ as the protocol-handling library. The
existing `quick-ui` Vello renderer becomes the per-output frame
producer. Foreign Wayland clients map their committed buffers to tabs
in the active strip via app-id matching. The internal shell chrome
(top bar, wheel disc, command palette, modals) renders in the same
frame as a separate scene-layer.

**Tech Stack:** smithay (compositor library), wayland-server (protocol),
calloop (event loop), drm-rs / udev / libinput (output + input
backends), vello (existing renderer), wgpu (existing GPU layer).

---

## Spec coverage

This plan implements §1 of the foundation spec at
`docs/superpowers/specs/2026-05-02-nirios-foundation-design.md`.
§3 (sandbox) and §5 (peer sync) reference the Wayland bridge socket
and surface API this plan introduces; their plans depend on it.

## Out of scope

- Sandboxing protocol details — see
  `plans/2026-05-02-per-workspace-sandbox-permissions.md`.
- Buffer streaming for peers — see
  `plans/2026-05-02-peer-sync-end-to-end.md`.
- xdg-desktop-portal backend — separate spec.
- Multi-monitor (foundation-spec §4) — phase 5 of this plan stubs
  multi-output infrastructure, full implementation is a follow-up.

## Definition of done

- `quick-desktop --mode native` runs as the system Wayland compositor
  on a TTY: launches without an existing display server, enumerates
  outputs, hosts at least one foreign Wayland client (e.g. `weston-terminal`)
  adopted as a tab in the active strip.
- `quick-desktop --mode hosted` (default until phase 7 flips it)
  remains the dev-friendly Wayland-client mode.
- A new integration test `compositor_hosts_a_foreign_client` runs
  in CI by spawning `quick-desktop --mode native --headless` (uses
  `winit` only for input/output stubs in tests) and asserting an
  in-process fake client maps a toplevel that becomes a tab.
- All existing tests still pass: quick-ui 153, quick-shell --lib 125,
  concept_parity_spec 26, e2e_shell_flows 15, etc.

---

## Phase 0 — Branch + dependencies

### Task 0.1: Workspace branch + smithay dep

**Files:**
- Modify: `quick-shell/Cargo.toml`
- Modify: `Cargo.lock` (regenerated)

- [ ] **Step 1: Branch**
   ```bash
   git -C quick-shell checkout -b nirios-shell-compositor nirios-shell
   ```

- [ ] **Step 2: Add smithay**
   In `quick-shell/Cargo.toml` `[dependencies]`:
   ```toml
   smithay = { version = "0.6", features = [
       "wayland_frontend",
       "renderer_glow",       # vello can target glow if we need a fallback
       "renderer_pixman",     # software fallback
       "backend_drm",
       "backend_libinput",
       "backend_session_libseat",
       "backend_udev",
       "backend_winit",       # for hosted mode
   ] }
   calloop = "0.13"
   ```
   Pin versions to whatever cargo resolves at the time; record in the
   commit message.

- [ ] **Step 3: Verify build (no code changes yet)**
   ```bash
   distrobox enter my-distrobox -- bash -c 'cd /var/home/quinten/Development/quickdesktop && cargo build -p quick-desktop 2>&1 | tail -8'
   ```
   Expected: `Finished`. New deps add ~1 minute to first build; subsequent are cached.

- [ ] **Step 4: Commit**
   ```bash
   git -C quick-shell add Cargo.toml Cargo.lock
   git -C quick-shell commit -m "feat(deps): add smithay + calloop for compositor mode"
   ```

---

## Phase 1 — Mode selector

### Task 1.1: `--mode hosted | native` flag

**Files:**
- Modify: `quick-shell/src/main.rs`
- Create: `quick-shell/src/compositor/mod.rs` (stubs only)
- Create: `quick-shell/src/compositor/hosted.rs` (existing winit path moved here)
- Create: `quick-shell/src/compositor/native.rs` (empty stub)

- [ ] **Step 1: Add mode enum + arg parsing**

```rust
// quick-shell/src/compositor/mod.rs
pub enum Mode { Hosted, Native }

pub fn parse_mode_from_args() -> Mode {
    std::env::args()
        .skip(1)
        .find_map(|a| match a.as_str() {
            "--mode" => None,
            v if v.starts_with("--mode=") => Some(v.trim_start_matches("--mode=").to_string()),
            _ => None,
        })
        .or_else(|| {
            let mut iter = std::env::args().skip(1).peekable();
            while let Some(a) = iter.next() {
                if a == "--mode" { return iter.next(); }
            }
            None
        })
        .map(|m| match m.as_str() {
            "native" => Mode::Native,
            _ => Mode::Hosted,
        })
        .unwrap_or(Mode::Hosted)
}

pub mod hosted;
pub mod native;
```

- [ ] **Step 2: Move existing main into hosted::run**

   Cut the body of `quick-shell/src/main.rs`'s `main()` (everything
   after `tracing_subscriber::fmt::init()`) into
   `compositor::hosted::run()`. The `main` becomes:
   ```rust
   fn main() {
       tracing_subscriber::fmt::init();
       match quick_desktop::compositor::parse_mode_from_args() {
           quick_desktop::compositor::Mode::Hosted => quick_desktop::compositor::hosted::run(),
           quick_desktop::compositor::Mode::Native => quick_desktop::compositor::native::run(),
       }
   }
   ```

- [ ] **Step 3: Stub native::run**
   ```rust
   // quick-shell/src/compositor/native.rs
   pub fn run() -> ! {
       eprintln!("nirios compositor: native mode not yet implemented");
       std::process::exit(1);
   }
   ```

- [ ] **Step 4: Test**
   ```bash
   distrobox enter my-distrobox -- bash -c 'cd /var/home/quinten/Development/quickdesktop && cargo test -p quick-desktop --lib 2>&1 | tail -3'
   distrobox enter my-distrobox -- bash -c 'cd /var/home/quinten/Development/quickdesktop && cargo build -p quick-desktop 2>&1 | tail -3'
   ```
   Expected: 125 passed (or whatever the pre-work count is, no regression).

- [ ] **Step 5: Commit**

---

## Phase 2 — Smithay scaffolding

### Task 2.1: `CompositorState` struct

**Files:**
- Create: `quick-shell/src/compositor/state.rs`
- Modify: `quick-shell/src/compositor/mod.rs` (re-export)

The smithay convention: one big `State` struct holding all globals
and per-client state. Each protocol handler attaches to `State` via
`delegate_*` macros.

- [ ] **Step 1: Define `State`**

```rust
// quick-shell/src/compositor/state.rs
use smithay::reexports::calloop::LoopHandle;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::seat::SeatState;
use smithay::wayland::output::OutputManagerState;

pub struct State {
    pub loop_handle: LoopHandle<'static, Self>,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub output_manager_state: OutputManagerState,
    // ... shell-side state plumbed in phase 4
}

impl State {
    pub fn new(loop_handle: LoopHandle<'static, Self>) -> Self { /* … */ }
}
```

- [ ] **Step 2: Wire `delegate_compositor`, `delegate_xdg_shell`, `delegate_shm`, `delegate_seat`, `delegate_output` macros**
   Smithay's docs and the `anvil` example are the reference. Each
   delegate macro generates trait impls that route protocol messages
   to the corresponding handler.

- [ ] **Step 3: Test the empty State compiles**
   No new tests — protocol delegates are pure type plumbing.
   ```bash
   distrobox enter my-distrobox -- bash -c 'cd /var/home/quinten/Development/quickdesktop && cargo build -p quick-desktop 2>&1 | tail -5'
   ```

- [ ] **Step 4: Commit**

### Task 2.2: Calloop event loop

**Files:** modify `quick-shell/src/compositor/native.rs`

- [ ] **Step 1: Replace stub with loop init**

```rust
pub fn run() -> ! {
    let mut event_loop = calloop::EventLoop::<state::State>::try_new()
        .expect("create event loop");
    let display = wayland_server::Display::new::<state::State>().expect("display");
    let dh = display.handle();

    let mut state = state::State::new(event_loop.handle());

    // Register the wayland socket
    let listening_socket = wayland_server::ListeningSocketSource::new_auto()
        .expect("listening socket");
    let socket_name = listening_socket.socket_name().to_os_string();
    println!("nirios compositor listening on {socket_name:?}");
    event_loop.handle()
        .insert_source(listening_socket, |client_stream, _, state| {
            state.dh.insert_client(client_stream, std::sync::Arc::new(()))
                .expect("insert client");
        })
        .expect("insert listener");

    event_loop.run(None, &mut state, |_| {
        // per-iteration tick — phase 4 paints frames here
    }).expect("event loop");

    std::process::exit(0)
}
```

- [ ] **Step 2: Smoke test**
   ```bash
   distrobox enter my-distrobox -- bash -c 'cd /var/home/quinten/Development/quickdesktop && timeout 3 cargo run -p quick-desktop --bin quick-desktop -- --mode native 2>&1 | head -5'
   ```
   Expected: `nirios compositor listening on "wayland-1"` (or similar)
   then exits when timeout hits. Connection refused isn't expected
   because no client connects in the smoke window.

- [ ] **Step 3: Commit**

---

## Phase 3 — Backend abstraction

### Task 3.1: Hosted (winit) backend for native mode

**Files:**
- Create: `quick-shell/src/compositor/backend/mod.rs`
- Create: `quick-shell/src/compositor/backend/winit.rs`

Even in `--mode native`, we want a "host on top of an existing
compositor" path for development. Smithay has `backend_winit`
exactly for this. Phase 5 adds a `drm-udev` backend for running
on a TTY without a parent compositor.

- [ ] **Step 1: Define a `Backend` trait**
   ```rust
   pub trait Backend {
       fn submit_frame(&mut self, scene: &vello::Scene, output: OutputId);
       fn poll_events(&mut self, callback: impl FnMut(BackendEvent));
       fn outputs(&self) -> &[Output];
   }
   ```
- [ ] **Step 2: Implement `WinitBackend`**
   Mirror `anvil`'s winit backend.
- [ ] **Step 3: Wire it into `native::run`**
- [ ] **Step 4: Test that the binary launches inside an existing
   Wayland session and shows a black window**
   ```bash
   /usr/bin/python3 scripts/screenshot-app.py --launch './target/debug/quick-desktop --mode native' --title 'nirios compositor' --out /tmp/nirios-native-startup.png --settle 3.0
   ```
- [ ] **Step 5: Commit**

---

## Phase 4 — Shell chrome rendered as scene layers

### Task 4.1: Adapt `ShellOverlayHost::draw` for compositor frames

**Files:** modify `quick-shell/src/compositor/native.rs`

The compositor's per-frame loop:

1. Build a `Scene` from the ShellOverlayHost (existing widget tree)
2. For each foreign Wayland surface in the active workspace,
   blit its committed buffer into the scene
3. Submit the scene to the backend

- [ ] **Step 1: Build the per-frame scene**
   Reuse `quick-shell/src/bin/shots/driver.rs`'s frame production:
   ```rust
   driver.relayout();
   let scene = driver.paint();
   ```
   The driver already wires the chrome state, taffy tree, and Vello
   scene production. Lift its API out of `bin/shots` into a public
   `quick_desktop::compositor::frame` module so both shots and
   native mode share it.

- [ ] **Step 2: Wire frame production into the loop's tick**
- [ ] **Step 3: Verify the black window now shows the shell**
   Same screenshot harness as 3.4. Expected output: same as
   `target/post-shadow/desktop.png`.
- [ ] **Step 4: Commit**

### Task 4.2: Foreign-surface blit

**Files:** modify `quick-shell/src/compositor/state.rs` and `frame.rs`

When a foreign client commits a buffer:

- [ ] **Step 1: Track committed buffers per surface**
   In the `CompositorHandler::commit` callback, pull the latest
   `wl_buffer` and store it on the surface's user-data.

- [ ] **Step 2: During scene-build, for each foreign tab in the active
   strip, look up the committed buffer + draw it into the scene at
   the tab's content-area rect**

   Initially: SHM-buffer-only path (skip dmabuf). Vello can render
   a `peniko::Image` from raw RGBA bytes; convert SHM (likely
   ARGB8888 → RGBA8888 little-endian).

- [ ] **Step 3: Test with a real foreign client**

   ```bash
   # Terminal 1: launch the compositor in hosted-on-wayland mode
   ./target/debug/quick-desktop --mode native &
   # Terminal 2: spawn a wayland client into it
   WAYLAND_DISPLAY=wayland-1 weston-terminal &
   # Take a screenshot
   /usr/bin/python3 scripts/screenshot-app.py --title 'nirios compositor' --out /tmp/nirios-with-foreign.png --settle 5.0
   ```
   Expected: weston-terminal renders inside a tab in the active strip.

- [ ] **Step 4: Commit**

---

## Phase 5 — DRM/udev backend for TTY

### Task 5.1: `DrmBackend` implementation

**Files:** new `quick-shell/src/compositor/backend/drm.rs`

This is the heaviest single task. Mirrors `anvil/src/udev.rs`.

- [ ] **Step 1: Acquire a session via `libseat`**
- [ ] **Step 2: Enumerate DRM devices via udev**
- [ ] **Step 3: For each output: configure mode, allocate buffers,
   set up a page-flip event source on the calloop loop**
- [ ] **Step 4: Wire libinput for keyboard / pointer / touch events
   into the same calloop loop**
- [ ] **Step 5: Smoke test on a VT**

   On a host with a free TTY (Ctrl+Alt+F3, log in, then):
   ```bash
   ./target/release/quick-desktop --mode native --backend drm
   ```
   Expected: the shell renders fullscreen on the TTY's monitor.

- [ ] **Step 6: Commit**

---

## Phase 6 — Tab-binding for foreign clients

### Task 6.1: app-id → tab matching

**Files:** new `quick-shell/src/compositor/tab_binding.rs`

- [ ] **Step 1: Surface adoption rule**

When `XdgShellHandler::new_toplevel` fires:
1. Read the toplevel's app-id (may not be set yet — wait for first commit).
2. Search the active workspace's strips for a tab with
   `desired_app_id == Some(app_id)`. If found, adopt.
3. Else, create a new `Tab::Foreign { app_id, surface_id }` in the
   active group of the active strip.

- [ ] **Step 2: New state on Tab**

   Add to `quick-shell/src/shell/state.rs::Tab`:
   ```rust
   pub enum TabContent {
       Native(/* existing */),
       Foreign { app_id: String, surface_id: SurfaceId },
   }
   ```

- [ ] **Step 3: Tests**

   `compositor_adopts_a_new_toplevel_into_active_group` — create a
   fake client (smithay test utilities), commit a toplevel, assert
   the workspace gained a tab.

- [ ] **Step 4: Commit**

### Task 6.2: Surface lifecycle → tab lifecycle

- [ ] **Step 1: On `xdg_toplevel.destroy`** mark the corresponding tab `closing` (existing animation runs).
- [ ] **Step 2: On `xdg_toplevel.set_app_id`** if the tab's app-id was previously unset, attempt re-adoption.
- [ ] **Step 3: Test** — close a foreign client, assert the tab disappears after the close animation.
- [ ] **Step 4: Commit**

---

## Phase 7 — Default-mode flip + rollout

### Task 7.1: Default to native

**Files:** `quick-shell/src/compositor/mod.rs`

- [ ] **Step 1:** flip `parse_mode_from_args`'s default to `Mode::Native`.
- [ ] **Step 2:** bump the live-binary docs.
- [ ] **Step 3:** keep `--mode hosted` as an explicit flag for development.
- [ ] **Step 4:** commit.

### Task 7.2: CI integration test

**Files:** new `quick-shell/tests/compositor_test.rs`

- [ ] **Step 1: A test that:**
   1. Spawns `quick-desktop --mode native --backend winit` headless.
   2. Connects a fake `wayland-client` with a toplevel.
   3. Asserts the toplevel becomes a tab in the active group.
   4. Closes the client; asserts the tab is reaped.
- [ ] **Step 2:** wire into the workspace-wide `cargo test`.

### Task 7.3: Final ledger run + parity-status update

- [ ] **Step 1:** `cargo test --workspace`. All counts hold or grow.
- [ ] **Step 2:** Update `docs/superpowers/specs/2026-04-XX-parity-status.md`
   to mark §1 of the foundation spec as ✅, with the test name as evidence.
- [ ] **Step 3:** commit; bump submodule pointer.

---

## Risk register

| Risk | Mitigation |
|---|---|
| Smithay version churn | Pin to a specific minor version; vendor patches if needed. |
| DRM backend platform-specific bugs (Intel vs Mesa vs NVIDIA) | Phase 5 ships behind an explicit flag (`--backend drm`). Default backend stays winit until phase 7's verification on hardware. |
| Vello + foreign-surface blit perf | Profile early. If SHM blit is the bottleneck, fall back to wgpu native textures for foreign surfaces (smithay supports this). |
| Sandbox interactions block phase 6 | Sandbox plan gates per-workspace bridge sockets. Until that lands, foreign clients all share the compositor's main socket — that's fine for development; security is added by the sandbox plan. |

## Success metrics

- A user can run `quick-desktop --mode native` on bare metal, the
  shell renders, they spawn `weston-terminal` into a tab, and it just
  works.
- The tab-drag, scroll-snap, and palette flows still function.
- 60 fps frame rate on a 1080p single-output system.

