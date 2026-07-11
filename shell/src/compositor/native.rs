//! Native compositor mode — Phase 2 calloop event loop scaffolding.
//!
//! Phase 1 wired the `--mode native` dispatch with a stub. Phase 2 replaces
//! that stub with a real smithay event loop that:
//!
//! 1. Creates a `wayland_server::Display` and a calloop `EventLoop` over our
//!    [`state::State`].
//! 2. Constructs the `State` (which advertises `wl_compositor`, `xdg_wm_base`,
//!    `wl_shm`, `wl_seat` machinery, `wl_output` manager).
//! 3. Binds an auto-named listening socket (`wayland-1`, `wayland-2`, …) and
//!    inserts it into the loop so each accepted connection is handed back to
//!    `DisplayHandle::insert_client`.
//! 4. Runs forever — Phase 4 will tick rendering and dispatch from the loop's
//!    per-iteration callback.
//!
//! What this *doesn't* do: open a TTY, talk to DRM/KMS, render anything, or
//! forward input. Those are explicitly Phase 3+ work. The smoke test for
//! Phase 2 is just "the binary prints a listening-socket line and stays up
//! until killed".

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use taffy::NodeId;
use vello::kurbo::Rect;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

use crate::compositor::backend::{
    combined_geometry, headless, Backend, BackendEvent, DrmBackend, HeadlessBackend, OutputId,
    WinitBackend,
};
use crate::compositor::frame::FrameBuilder;
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use crate::compositor::lock_screen;
use crate::compositor::state::{ClientState, State};
use crate::compositor::test_control::{
    self, ControlState, KeyTag, NamedKeyTag, PlannedAction, parse_command, serialise_response,
};
use crate::compositor::{parse_backend_from_args, BackendKind};

/// Env-var name for the Phase 7 CI test introspection hook. When set to a
/// writable path, [`run`] serializes the shell `WorkspaceTree` to that path
/// on each frame tick as compact JSON. The test
/// (`tests/compositor_test.rs`) polls that file to assert foreign-tab
/// adoption + teardown without needing a public shell-state API.
///
/// The hook is gated on the env var being set; without it the
/// compositor never touches the filesystem from the frame loop.
const TEST_STATE_DUMP_ENV: &str = "NIRIOS_TEST_STATE_DUMP";

/// Env-var name for the real-input e2e control protocol. When set to a
/// writable path, [`run`] binds a unix socket at that path that accepts
/// newline-delimited JSON commands (see
/// `compositor::test_control::Command` for the schema). Production
/// runs never set the env var; the socket is opt-in for test harnesses.
///
/// Spec: `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`.
const TEST_CONTROL_ENV: &str = "QOS_TEST_CONTROL";

/// D-3 fix: RAII guard that unlinks a unix-socket file when dropped.
///
/// The compositor previously left its `QOS_TEST_CONTROL` socket file
/// behind on clean exit (and on panic). Subsequent runs depended on the
/// next-boot `remove_file` inside `bind_test_control_socket` to scrub
/// the stale entry; that worked for `bind`, but the orphan file lingered
/// in `XDG_RUNTIME_DIR` between sessions for any other consumer to see.
///
/// Wrapping the path in this struct ensures the file is removed both on
/// normal teardown and on the panic-unwind path (without the explicit
/// `std::process::exit(0)` at the end of `run` skipping over our
/// cleanup, since `exit` does NOT run destructors). The compositor
/// `run` function holds the guard for the entire socket lifetime and
/// explicitly `drop`s it before the final `process::exit`.
pub(crate) struct UnlinkOnDrop(pub(crate) PathBuf);

impl Drop for UnlinkOnDrop {
    fn drop(&mut self) {
        // Errors here are not actionable — the file might already be
        // gone (someone else cleaned up) or we might lack the right
        // to remove it. Either way the production path doesn't care.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind the test-control unix socket. Returns `None` if the env var is
/// unset or the bind fails — bind failures are reported via
/// `eprintln!` but don't abort the compositor (the production path
/// must never depend on the test surface).
///
/// On success, returns the listener plus an `UnlinkOnDrop` guard that
/// removes the socket file on teardown (D-3).
fn bind_test_control_socket() -> Option<(UnixListener, UnlinkOnDrop)> {
    let path = std::env::var_os(TEST_CONTROL_ENV)?;
    let path = PathBuf::from(path);
    // Best-effort cleanup of a stale socket file from a previous run —
    // `bind` would otherwise fail with EADDRINUSE.
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("nirios compositor: test_control set_nonblocking: {e}");
                return None;
            }
            eprintln!("nirios compositor test_control listening on {path:?}");
            let guard = UnlinkOnDrop(path);
            Some((listener, guard))
        }
        Err(e) => {
            eprintln!("nirios compositor: test_control bind {path:?}: {e}");
            None
        }
    }
}

/// Per-connection state for the test-control protocol. Holds a buffered
/// reader over a non-blocking `UnixStream` so partial lines accumulate
/// across iterations of the main loop.
struct TestControlConn {
    reader: BufReader<UnixStream>,
    closed: bool,
}

impl TestControlConn {
    fn new(stream: UnixStream) -> Option<Self> {
        if let Err(e) = stream.set_nonblocking(true) {
            eprintln!("nirios compositor: test_control conn set_nonblocking: {e}");
            return None;
        }
        Some(Self {
            reader: BufReader::new(stream),
            closed: false,
        })
    }
}

/// Walk-the-frame-builder `ControlState` impl. Captures a fresh
/// snapshot per `widget_registry()` call so the dispatch path sees
/// the live tree (no caching).
struct FrameBuilderControlState<'a> {
    fb: &'a FrameBuilder,
}

impl<'a> ControlState for FrameBuilderControlState<'a> {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.fb.widget_registry()
    }

    /// Serialise the live persisted [`WorkspaceTree`] — same blob
    /// the `NIRIOS_TEST_STATE_DUMP` env-var path writes on every
    /// frame tick. The chrome's `tree` field is the authoritative
    /// copy (the compositor's `State.workspace_tree` is the same
    /// type and stays in sync via the shared `Chrome` cell).
    fn workspace_tree_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        // `unwrap_or(Null)` — `WorkspaceTree`'s `Serialize` impl is
        // derived and infallible in practice, but the trait method
        // can't `expect` because that would crash the live compositor
        // on a hypothetical bad write. Nulls are still
        // schema-compliant from the wire's perspective.
        serde_json::to_value(&*chrome.tree()).unwrap_or(serde_json::Value::Null)
    }

    /// Build a structured view of [`ChromeState`] for the
    /// `state_dump` response. We construct the object manually rather
    /// than `derive(Serialize)` on `ChromeState` because the chrome
    /// owns `Rc<RefCell<…>>` handles (`Chrome`) into widgets that
    /// can't sensibly serialise; hand-rolling the object also lets us
    /// pick a wire-friendly representation (enum-as-string) for
    /// fields like `search_scope` and `ws_settings_tab` without
    /// shoving `#[serde(rename_all)]` attributes deep into the
    /// chrome's dependency graph.
    fn chrome_state_json(&self) -> serde_json::Value {
        let chrome = self.fb.chrome();
        let overlays = &chrome.overlays;
        serde_json::json!({
            "overlays": {
                "overview": overlays.overview,
                "quick_settings": overlays.quick_settings,
                "ws_settings": overlays.ws_settings,
                "collaborators": overlays.collaborators,
            },
            "search_scope": format!("{:?}", chrome.search_scope),
            "ws_settings_tab": format!("{:?}", chrome.ws_settings_tab),
            "revision": chrome.revision,
            "editing_workspace_name": chrome.editing_workspace_name,
            "pending_grants_count": chrome.pending_grants.len(),
        })
    }
}

/// Stash of per-process state the test-control wire needs in order to
/// execute [`PlannedAction`]s. Mirrors the equivalent fields on
/// `ShellDriver` (`bin/shots/driver.rs`): modifier snapshot, the
/// auxiliary `AutoCommit` doc widgets mutate, the root `ObjId`, and
/// the focus cursor. The compositor doesn't drive any of these from
/// real input today (input wiring is Phase 6), so this is a clean
/// scratch context used only by the test-control surface.
struct TestControlCtx {
    modifiers: Modifiers,
    doc: AutoCommit,
    root_obj: ObjId,
    focus: Option<NodeId>,
    cursor: PhysicalPosition<f64>,
}

impl TestControlCtx {
    fn new() -> Self {
        Self {
            modifiers: Modifiers::default(),
            doc: AutoCommit::new(),
            root_obj: automerge::ROOT,
            focus: None,
            cursor: PhysicalPosition::new(0.0, 0.0),
        }
    }
}

/// Translate a [`KeyTag`] from the wire to a `winit::keyboard::Key`
/// the host event handlers actually match on. Named keys map 1:1 to
/// `NamedKey` variants; chars wrap in `Key::Character`.
fn keytag_into_winit_key(tag: KeyTag) -> Key {
    match tag {
        KeyTag::Named(n) => Key::Named(match n {
            NamedKeyTag::Enter => NamedKey::Enter,
            NamedKeyTag::Escape => NamedKey::Escape,
            NamedKeyTag::Tab => NamedKey::Tab,
            NamedKeyTag::ArrowLeft => NamedKey::ArrowLeft,
            NamedKeyTag::ArrowRight => NamedKey::ArrowRight,
            NamedKeyTag::ArrowUp => NamedKey::ArrowUp,
            NamedKeyTag::ArrowDown => NamedKey::ArrowDown,
            NamedKeyTag::Backspace => NamedKey::Backspace,
            NamedKeyTag::Delete => NamedKey::Delete,
            NamedKeyTag::Home => NamedKey::Home,
            NamedKeyTag::End => NamedKey::End,
            NamedKeyTag::PageUp => NamedKey::PageUp,
            NamedKeyTag::PageDown => NamedKey::PageDown,
            NamedKeyTag::Space => NamedKey::Space,
        }),
        KeyTag::Char(c) => Key::Character(SmolStr::new(c.to_string())),
    }
}

/// Build a `WindowEvent::KeyboardInput` for `key` with the given
/// element state. Mirrors `shots/driver.rs::key_press_event`: winit's
/// `KeyEvent` has a `pub(crate)` `platform_specific` field, so we
/// zero-init the struct and overwrite the fields the host actually
/// reads. The `text` field is populated for `Character` keys so the
/// palette + rename text-input branches see the same glyph they would
/// on real hardware.
fn keyboard_input_event(key: Key, pressed: bool) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    ev.repeat = false;
    ev.text = if let Key::Character(s) = &key {
        Some(s.clone())
    } else {
        None
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Apply a [`SetModifiers`] action by overwriting `ctx.modifiers`. The
/// host reads the snapshot inline on the next chord event, so we
/// don't need to dispatch a `WindowEvent::ModifiersChanged` to refresh
/// any per-event state. Mirrors `ShellDriver::set_modifiers`.
fn apply_modifier_mask(ctx: &mut TestControlCtx, ctrl: bool, shift: bool, alt: bool, logo: bool) {
    let mut mask = ModifiersState::empty();
    if ctrl {
        mask |= ModifiersState::CONTROL;
    }
    if shift {
        mask |= ModifiersState::SHIFT;
    }
    if alt {
        mask |= ModifiersState::ALT;
    }
    if logo {
        mask |= ModifiersState::SUPER;
    }
    ctx.modifiers = Modifiers::from(mask);
}

/// Synthesise the [`WindowEvent`] that corresponds to a single
/// [`PlannedAction`] and dispatch it through the FrameBuilder's
/// shared `dispatch_event` wire — the same wire `ShellDriver::dispatch`
/// uses for in-process scenarios. `device_id` is zeroed because
/// winit's `DeviceId` is opaque and the host's input path doesn't
/// inspect it.
fn execute_planned_action(
    action: &PlannedAction,
    fb: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
) {
    match action {
        PlannedAction::CursorMoved { x, y } => {
            ctx.cursor = PhysicalPosition::new(*x, *y);
            let event = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: ctx.cursor,
            };
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::MouseLeftButton { pressed } => {
            let event = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: if *pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
                button: MouseButton::Left,
            };
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::KeyboardKey { key, pressed } => {
            let winit_key = keytag_into_winit_key(*key);
            let event = keyboard_input_event(winit_key, *pressed);
            fb.dispatch_event(
                &event,
                &ctx.modifiers,
                &mut ctx.doc,
                &ctx.root_obj,
                &mut ctx.focus,
            );
        }
        PlannedAction::SetModifiers {
            ctrl,
            shift,
            alt,
            logo,
        } => {
            // Mutate the executor's modifier snapshot directly. The
            // host reads `ctx.modifiers` on the next chord dispatch,
            // so no event needs to flow through `dispatch_event`.
            apply_modifier_mask(ctx, *ctrl, *shift, *alt, *logo);
        }
        PlannedAction::Snapshot { path } => {
            // Re-paint the live scene into a fresh OffscreenRenderer
            // sized to the FrameBuilder's current viewport. We use
            // option 2 from the spec (re-render via OffscreenRenderer)
            // rather than tap a backend-cached `last_frame` so the
            // command works for every backend (winit / drm / future
            // headless), not just the headless one.
            //
            // Errors during creation, encoding, or write are logged
            // via `eprintln!` and swallowed — the dispatcher already
            // returned `Ok` from the wire's perspective, and crashing
            // the compositor on a stray bad path would defeat the
            // point of having a test surface. Phase 5 will route
            // these into structured wire responses.
            write_snapshot_png(fb, path);
        }
    }
}

/// Re-paint the current scene into a fresh `OffscreenRenderer` and
/// encode the resulting RGBA buffer as a PNG at `path`. Best-effort:
/// every failure path logs to stderr and returns, so an executor
/// invocation never panics or unwinds back into the calloop pump.
///
/// Split out of `execute_planned_action` so unit tests can exercise
/// the snapshot path against a hand-built `FrameBuilder` without going
/// through `PlannedAction` dispatch.
fn write_snapshot_png(fb: &mut FrameBuilder, path: &std::path::Path) {
    use std::io::Write;
    // The renderer panics if no wgpu adapter is available (e.g. CI
    // without llvmpipe); catch_unwind keeps the compositor alive in
    // that environment too.
    let (w, h) = fb.viewport();
    let (w, h) = (w as u32, h as u32);
    let scene = fb.paint();
    let render_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut renderer = ui::offscreen::OffscreenRenderer::new(w, h);
        renderer.render_rgba(&scene)
    }));
    let rgba = match render_result {
        Ok(rgba) => rgba,
        Err(_) => {
            eprintln!(
                "nirios compositor: test_control snapshot: \
                 OffscreenRenderer panicked (no wgpu adapter?)"
            );
            return;
        }
    };
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "nirios compositor: test_control snapshot: create {path:?}: {e}"
            );
            return;
        }
    };
    let mut writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut writer, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = match encoder.write_header() {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "nirios compositor: test_control snapshot: png header {path:?}: {e}"
            );
            return;
        }
    };
    if let Err(e) = png_writer.write_image_data(&rgba) {
        eprintln!(
            "nirios compositor: test_control snapshot: png write {path:?}: {e}"
        );
        return;
    }
    // Drop the PNG writer first so the underlying BufWriter holds the
    // last bytes; then flush explicitly. Without this, a swiftly-following
    // assertion on file size could race against a still-buffered tail.
    drop(png_writer);
    if let Err(e) = writer.flush() {
        eprintln!(
            "nirios compositor: test_control snapshot: flush {path:?}: {e}"
        );
    }
}

/// Drain any pending bytes from `conn`'s buffer. For each complete
/// line (terminated by `\n`), parse it as a [`test_control::Command`],
/// dispatch against `frame_builder`, execute any `PlannedAction`s the
/// dispatcher returned, and write the JSON response back to the
/// connection. Partial lines stay buffered for the next tick.
///
/// On EOF / connection error / write failure, marks the connection
/// closed so the outer loop can drop it.
///
/// Phase 2b — `click_at` and `click_named` execute their planned
/// actions through `FrameBuilder::dispatch_event` (same wire as
/// `ShellDriver::dispatch`). Chrome-mutating widgets (overview
/// button, wheel-disc plus slot, …) react identically to real input.
/// D-10 fix: cap on the per-line size for the test-control protocol.
/// `BufRead::read_line` will buffer the entire line into the `String`
/// regardless of size; a malicious or buggy client could feed an
/// unbounded byte stream with no newline and drive the compositor
/// to OOM. 64 KiB is comfortably above any legitimate command
/// (`click_named` carries a widget id + a couple of floats — well
/// under a kilobyte) and below any meaningful memory pressure.
pub(crate) const TEST_CONTROL_MAX_LINE_BYTES: usize = 64 * 1024;

/// D-10: read up to one `\n`-terminated line from `reader`, capping at
/// `TEST_CONTROL_MAX_LINE_BYTES`. Returns:
///
/// * `Ok(Some(line))` — a complete `\n`-terminated line was read; the
///   trailing newline is included so the caller can keep using
///   `parse_command` unchanged.
/// * `Ok(None)` — EOF reached without seeing a newline (peer closed
///   cleanly mid-line).
/// * `Err(ReadLineError::TooLarge)` — the cap fired before a newline
///   was seen. Caller must close the connection.
/// * `Err(ReadLineError::Io(e))` — underlying I/O failure (incl.
///   `WouldBlock`, which the caller handles specially).
pub(crate) enum ReadLineError {
    TooLarge,
    Io(std::io::Error),
}

pub(crate) fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    out: &mut String,
    max_bytes: usize,
) -> Result<Option<()>, ReadLineError> {
    let mut buf = Vec::with_capacity(256);
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) => return Err(ReadLineError::Io(e)),
        };
        if available.is_empty() {
            // EOF. If we accumulated some bytes without a newline,
            // return them as a best-effort line (caller can decide
            // what to do); otherwise signal EOF via None.
            if buf.is_empty() {
                return Ok(None);
            }
            match String::from_utf8(buf) {
                Ok(s) => {
                    out.push_str(&s);
                    return Ok(Some(()));
                }
                Err(e) => {
                    return Err(ReadLineError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e,
                    )));
                }
            }
        }
        // Find the newline (if any) in the available slice.
        let (consumed, found_nl) = match available.iter().position(|&b| b == b'\n') {
            Some(idx) => (idx + 1, true),
            None => (available.len(), false),
        };
        if buf.len() + consumed > max_bytes {
            // Cap reached. Consume up to the cap so we don't leave
            // garbage in the reader, then bail.
            let remaining = max_bytes.saturating_sub(buf.len());
            buf.extend_from_slice(&available[..remaining]);
            reader.consume(remaining);
            return Err(ReadLineError::TooLarge);
        }
        buf.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if found_nl {
            match String::from_utf8(buf) {
                Ok(s) => {
                    out.push_str(&s);
                    return Ok(Some(()));
                }
                Err(e) => {
                    return Err(ReadLineError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e,
                    )));
                }
            }
        }
    }
}

fn pump_test_control_conn(
    conn: &mut TestControlConn,
    frame_builder: &mut FrameBuilder,
    ctx: &mut TestControlCtx,
) {
    loop {
        let mut line = String::new();
        match read_bounded_line(&mut conn.reader, &mut line, TEST_CONTROL_MAX_LINE_BYTES) {
            Ok(None) => {
                conn.closed = true;
                return;
            }
            Ok(Some(())) => {
                let (resp, actions) = match parse_command(&line) {
                    Ok(cmd) => {
                        let state = FrameBuilderControlState { fb: frame_builder };
                        test_control::dispatch_command(&cmd, &state)
                    }
                    Err(err_resp) => (err_resp, Vec::new()),
                };
                for action in &actions {
                    execute_planned_action(action, frame_builder, ctx);
                }
                // Re-run layout so the next `list_targets` (or paint
                // tick) sees rects post-mutation. Chrome state
                // changes (overlay open, workspace add, …) can change
                // structural layout; without a relayout here the
                // next command sees stale rects from the previous
                // frame.
                if !actions.is_empty() {
                    frame_builder.relayout();
                }
                let mut out = serialise_response(&resp);
                out.push('\n');
                if let Err(e) = conn.reader.get_mut().write_all(out.as_bytes()) {
                    eprintln!("nirios compositor: test_control write: {e}");
                    conn.closed = true;
                    return;
                }
            }
            Err(ReadLineError::TooLarge) => {
                tracing::warn!(
                    "test_control: client sent oversized line; dropping connection"
                );
                conn.closed = true;
                return;
            }
            Err(ReadLineError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return;
            }
            Err(ReadLineError::Io(e)) => {
                eprintln!("nirios compositor: test_control read: {e}");
                conn.closed = true;
                return;
            }
        }
    }
}

/// Track D2: turn a [`SessionTransition`] into a real backend effect.
///
/// `Blanked` / `Woke` drive the backend's DPMS (no-op on winit /
/// headless); `Locked` / `Unlocked` are pure state the paint path reads
/// via `SessionManager::is_locked`, so they need no backend call here.
fn apply_session_transition(transition: SessionTransition, backend: &mut dyn Backend) {
    match transition {
        SessionTransition::Blanked => backend.set_dpms(false),
        SessionTransition::Woke => backend.set_dpms(true),
        SessionTransition::Locked | SessionTransition::Unlocked => {}
    }
}

/// Track D2: is `event` the Super+L lock chord (on key *press*)?
///
/// The modifier mask comes from the loop's running `Modifiers` snapshot
/// (the same one `dispatch_event` uses), not the key event, because
/// winit reports modifiers via a separate `ModifiersChanged` event.
fn is_lock_chord(event: &WindowEvent, modifiers: &Modifiers) -> bool {
    if let WindowEvent::KeyboardInput { event, .. } = event {
        if event.state == ElementState::Pressed && modifiers.state().super_key() {
            if let Key::Character(s) = &event.logical_key {
                return s.eq_ignore_ascii_case("l");
            }
        }
    }
    false
}

/// Track D2: is `event` the explicit unlock action (Enter or Escape on
/// key *press*)?
///
/// This is the minimum "explicit unlock action" the D2 spec requires for
/// this pass; wiring real PAM authentication behind it is a documented
/// follow-up. Only key presses count so an auto-repeat release can't
/// race the transition.
fn is_unlock_input(event: &WindowEvent) -> bool {
    if let WindowEvent::KeyboardInput { event, .. } = event {
        if event.state == ElementState::Pressed {
            return matches!(
                event.logical_key,
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape)
            );
        }
    }
    false
}

/// Entry point for the smithay-on-Wayland compositor.
///
/// Diverges (`-> !`) so the caller in `main` can use it next to
/// [`super::hosted::run`] without juggling return types. Any unrecoverable
/// error is `panic`-ed via `expect`; this is a top-level binary entry point,
/// so an `unwrap`-style style is appropriate (and matches smithay's own
/// examples).
pub fn run() -> ! {
    // Calloop drives the whole compositor; smithay is callback-oriented and
    // expects every wakeup to fan out from a single `EventLoop::<State>`.
    let mut event_loop: EventLoop<'static, State> =
        EventLoop::try_new().expect("create calloop event loop");

    // The `Display` owns the wayland-server protocol state. We hold it in a
    // local so `dispatch_clients`/`flush_clients` can be called from the
    // per-iteration tick once Phase 4 needs to wake clients up.
    //
    // For Phase 2 the loop only fires when the listening socket has a new
    // connection, so the `Display` exists primarily to mint `DisplayHandle`s.
    //
    // Phase 7: the loop body must also call `display.dispatch_clients` +
    // `display.flush_clients` each tick — without that, the per-iteration
    // calloop dispatch never pulls bytes off accepted client sockets and
    // the client side of `wl_display.get_registry` blocks forever.
    // Mirrors smithay 0.7's `examples/minimal.rs` and `examples/seat.rs`
    // loop bodies.
    let mut display: Display<State> = Display::new().expect("create wayland display");
    let display_handle = display.handle();

    let mut state = State::new(event_loop.handle(), display_handle.clone());

    // Phase 3: construct the winit backend so the compositor has a host
    // window to render into. Phase 4 routes per-frame scenes through
    // `backend.submit_frame`. We `expect` here for the same reason the
    // socket bind below does — a top-level `-> !` entry point treats any
    // backend init failure as fatal (same posture as smithay's `anvil`).
    //
    // Phase 5 Task 5.1: `--backend drm|winit` selects which `Backend`
    // impl runs. `Drm` is currently a skeleton — it errors at
    // construction with a "not yet implemented" message — but the
    // dispatch is wired so the follow-up TTY work plugs in without
    // touching this entry point.
    let backend_kind = parse_backend_from_args();
    let mut backend: Box<dyn Backend> = match backend_kind {
        BackendKind::Winit => {
            let mut b = WinitBackend::new().expect("init winit backend");
            // Match the FrameBuilder's internal theme so the host
            // window clears to chrome bg before the first frame
            // paints. Runtime theme changes would re-call this; for
            // now `Theme::dark()` is the only one in play and the
            // FrameBuilder picks the same constant.
            b.set_base_color(ui::theme::Theme::dark().bg_app);
            Box::new(b)
        }
        BackendKind::Drm => match DrmBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => {
                eprintln!("nirios compositor: --backend drm failed: {e}");
                std::process::exit(1);
            }
        },
        BackendKind::Headless => {
            // Initial size comes from `NIRIOS_HEADLESS_SIZE` (with a
            // shots-default fallback) — keeps the headless backend
            // env-var-driven like `QOS_TEST_CONTROL` /
            // `NIRIOS_TEST_STATE_DUMP` instead of adding a third CLI
            // flag for size. Skip `set_base_color` because headless
            // has no window — `HeadlessBackend::submit_frame` clears
            // to the same constant the winit backend uses.
            let (w, h) = headless::resolve_initial_size();
            match HeadlessBackend::new(w, h) {
                Ok(b) => Box::new(b),
                Err(e) => {
                    eprintln!("nirios compositor: --backend headless failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    };

    // Phase 4: per-frame scene production lifted out of `bin/shots`.
    // The compositor and the offscreen tooling share `FrameBuilder`,
    // so the chrome rendered into the host window is byte-identical
    // to the screenshots `cargo run --bin shots` produces.
    // Size the scene to the bounding box spanning every output (Track
    // B3): the compositor paints one scene across the combined geometry
    // and each output presents its own sub-rect. For a single output this
    // equals that output's size, so single-monitor layout is unchanged.
    let initial_size = match combined_geometry(backend.outputs()) {
        (0, 0) => (1600, 900),
        size => size,
    };
    let mut frame_builder = FrameBuilder::new(initial_size);
    // Seed the size used for fullscreen configures so an early
    // `set_fullscreen` request fills the output even before the first resize.
    state.output_size = (initial_size.0 as i32, initial_size.1 as i32);

    // `new_auto` walks `wayland-1..wayland-32` and binds the first free name.
    // The smoke test in the plan asserts on the line below ("nirios compositor
    // listening on …"), so the format here is part of the contract.
    let listening_socket =
        ListeningSocketSource::new_auto().expect("bind listening wayland socket");
    let socket_name = listening_socket.socket_name().to_os_string();
    eprintln!("nirios compositor listening on {socket_name:?}");

    event_loop
        .handle()
        .insert_source(listening_socket, move |client_stream, _, state| {
            // Each accepted connection becomes a wayland-server `Client`. We
            // attach a `ClientState` so `CompositorHandler::client_compositor_state`
            // can fish out the per-client `CompositorClientState` later.
            if let Err(err) = state
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                eprintln!("nirios compositor: failed to insert client: {err}");
            }
        })
        .expect("insert listening socket source into event loop");

    // Phase 7 test introspection: when `NIRIOS_TEST_STATE_DUMP` is set, the
    // compositor serializes its `WorkspaceTree` to that path on every frame
    // tick. The integration test in `tests/compositor_test.rs` reads this
    // file to assert foreign-tab adoption + reaping. Production runs never
    // set the env var, so the dump is a true zero-cost noop there.
    let test_dump_path = std::env::var_os(TEST_STATE_DUMP_ENV);

    // Real-input e2e Phase 1b: opt-in unix socket that accepts
    // newline-delimited JSON commands (see `compositor::test_control`).
    // Bound to whatever path `QOS_TEST_CONTROL` points at; unset
    // means the socket is never created (zero-cost in production).
    // Each loop tick polls the listener for new connections and drains
    // pending bytes from existing ones.
    let (test_control_listener, _test_control_unlink) = match bind_test_control_socket() {
        Some((l, g)) => (Some(l), Some(g)),
        None => (None, None),
    };
    let mut test_control_conns: Vec<TestControlConn> = Vec::new();
    // Phase 2b: per-process scratch context for executing PlannedActions
    // off the test-control socket. The compositor doesn't (yet) route
    // real winit input to the host, so this is the only place a
    // `Modifiers` snapshot / `AutoCommit` doc / focus cursor lives in
    // the live binary. Production runs never set the env var that
    // enables the socket, so this is zero-cost in shipped builds.
    let mut test_control_ctx = TestControlCtx::new();

    // Phase 4 tick body. Each iteration:
    //
    //   1. Drain backend events (resize, close, redraw, focus). A
    //      `CloseRequested` flips a flag we read after the closure to
    //      tell the calloop loop to stop.
    //   2. Run a layout pass on the frame builder.
    //   3. Paint the scene.
    //   4. Submit the scene to the backend's primary output.
    //   5. (Test-only) dump shell state if `NIRIOS_TEST_STATE_DUMP` is set.
    //
    // `submit_frame` is currently a no-op (vello + smithay glow interop
    // is the next follow-up); the host window therefore stays blank.
    // The scene is still produced every frame so smoke tests can
    // confirm the layout pipeline runs without a GPU surface.
    // Track D2: idle / DPMS-blank / session-lock state machine. Pure
    // logic (see `compositor::idle`); the loop below turns its
    // transitions into `backend.set_dpms` calls and lock-overlay paints.
    // Config comes from `QOS_IDLE_BLANK_SECS` / `QOS_IDLE_LOCK_SECS`
    // (defaults: blank after 5 min, no auto-lock).
    let mut session = SessionManager::new(IdleConfig::from_env(), Instant::now());

    // Track D1: the `org.freedesktop.Notifications` D-Bus service. The
    // compositor owns the notification daemon so sandboxed apps' toasts
    // render as OS-drawn chrome. The bridge is the `Send + Sync` mailbox
    // between the async zbus service (driven by a dedicated tokio
    // runtime) and the synchronous chrome pump in this loop. Failure to
    // reach the session bus (none present / another daemon owns the
    // name) is downgraded to a warning — the compositor must keep
    // running, and shell-internal notifications still work without the
    // external service.
    let notif_bridge = crate::notifications::NotificationBridge::new();
    frame_builder.with_chrome(|st| st.set_notification_bridge(notif_bridge.clone()));
    // Held for the lifetime of `run` so the runtime's worker threads keep
    // driving zbus dispatch + the signal-emitter task in the background.
    let mut _notif_runtime: Option<tokio::runtime::Runtime> = None;
    let mut _notif_handle: Option<crate::notifications::ServerHandle> = None;
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("nirios-notify")
        .build()
    {
        Ok(rt) => {
            let bridge = notif_bridge.clone();
            let res: Result<crate::notifications::ServerHandle, zbus::Error> =
                rt.block_on(async move {
                    let conn = zbus::ConnectionBuilder::session()?.build().await?;
                    let service = crate::notifications::NotificationService::new(
                        bridge.clone(),
                        // v1: a fresh CallerMap. The per-workspace bridge
                        // accept path populates unique-name → workspace
                        // entries once that lands (same shape the portal's
                        // CallerMap uses); until then unsandboxed callers
                        // simply carry no workspace attribution.
                        crate::portal::CallerMap::new(),
                    );
                    let handle =
                        crate::notifications::serve_notifications(conn.clone(), service).await?;
                    crate::notifications::spawn_signal_emitter(conn, bridge);
                    Ok(handle)
                });
            match res {
                Ok(h) => {
                    tracing::info!(
                        "notifications: serving {} on session bus",
                        crate::notifications::BUS_NAME
                    );
                    _notif_handle = Some(h);
                    _notif_runtime = Some(rt);
                }
                Err(e) => {
                    tracing::warn!(
                        "notifications: serve failed ({e}); continuing with \
                         shell-internal notifications only"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!("notifications: tokio runtime build failed ({e}); D-Bus service off");
        }
    }

    let mut close_requested = false;
    let frame_period = Duration::from_millis(16);
    // Reused per-iteration scratch so backend events can be processed
    // *after* `poll_events` returns — the input arm needs `&mut backend`
    // (for `set_dpms`) which can't be borrowed inside the poll closure.
    let mut pending_events: Vec<BackendEvent> = Vec::new();
    // Redraw gating. The loop used to `paint()` + `submit_frame()` on
    // every tick regardless of whether anything changed, pinning a CPU
    // core at 100% (and driving needless GPU work) even on a completely
    // static desktop. We now repaint only when there's a reason to:
    //   - a backend event arrived (input / resize / focus / redraw),
    //   - the widget tree has an animation in flight,
    //   - the chrome revision bumped (a state mutation: overlay toggle,
    //     notification, tab switch, …),
    //   - a session transition fired (blank / lock / unlock),
    //   - or the keepalive interval elapsed (a safety net so any missed
    //     dirty source still refreshes within a second).
    // Layout still runs every tick so input hit-testing stays current;
    // only the expensive paint + GPU submit are gated.
    let keepalive = Duration::from_millis(1000);
    let mut last_paint = Instant::now();
    let mut last_revision = frame_builder.chrome_revision();
    let mut force_first_paint = true;
    // Optional paint-rate diagnostics (`QOS_PAINT_STATS=1`).
    let paint_stats = std::env::var("QOS_PAINT_STATS").is_ok();
    let mut stats_since = Instant::now();
    let (mut frames_total, mut frames_painted) = (0u32, 0u32);
    let (mut cause_events, mut cause_revision, mut cause_anim) = (0u32, 0u32, 0u32);
    let (mut cause_session, mut cause_keepalive) = (0u32, 0u32);
    while !close_requested {
        backend.poll_events(&mut |event| pending_events.push(event));
        let had_backend_events = !pending_events.is_empty();
        let mut session_changed = false;
        for event in pending_events.drain(..) {
            match event {
                BackendEvent::CloseRequested => {
                    close_requested = true;
                }
                BackendEvent::OutputResized { id: _, size, scale: _ } => {
                    if size.0 > 0 && size.1 > 0 {
                        frame_builder.set_viewport(size);
                        // Keep the size used for fullscreen configures in sync
                        // so a fullscreened client fills the current output.
                        state.output_size = (size.0 as i32, size.1 as i32);
                    }
                }
                BackendEvent::Input(wev) => {
                    // Track D2: any input is activity — reset the idle
                    // timer and wake the display if it was blanked.
                    for t in session.note_activity(Instant::now()) {
                        apply_session_transition(t, backend.as_mut());
                    }

                    if session.is_locked() {
                        // Locked: input is captured. Only the explicit
                        // unlock action (Enter / Escape) is honoured;
                        // every other event is swallowed so app/chrome
                        // content stays hidden and inert behind the
                        // overlay. This is the lock's security boundary
                        // in lieu of an `ext-session-lock-v1` client.
                        if is_unlock_input(&wev) {
                            for t in session.request_unlock(Instant::now()) {
                                apply_session_transition(t, backend.as_mut());
                            }
                        }
                    } else if is_lock_chord(&wev, &test_control_ctx.modifiers) {
                        // Super+L locks immediately.
                        for t in session.request_lock(Instant::now()) {
                            apply_session_transition(t, backend.as_mut());
                        }
                    } else {
                        // Dispatch real input through the same wire the
                        // headless `execute_planned_action` path uses.
                        frame_builder.dispatch_event(
                            &wev,
                            &test_control_ctx.modifiers,
                            &mut test_control_ctx.doc,
                            &test_control_ctx.root_obj,
                            &mut test_control_ctx.focus,
                        );
                    }
                }
                BackendEvent::ModifiersChanged(modifiers) => {
                    // Carry the modifier mask forward so the next event
                    // dispatched into `FrameBuilder::dispatch_event` (or
                    // the lock-chord check above) sees the right
                    // Ctrl/Shift/Alt/Super state.
                    test_control_ctx.modifiers = modifiers;
                }
                BackendEvent::Redraw | BackendEvent::Focus(_) => {
                    // Redraw is implicit because we re-paint every tick;
                    // Focus changes don't gate input dispatch here.
                }
            }
        }

        // Track D2: advance the idle timers and apply any timeout-driven
        // transitions (blank, auto-lock) for this tick.
        for t in session.tick(Instant::now()) {
            apply_session_transition(t, backend.as_mut());
            session_changed = true;
        }

        // Track D1: pump the D-Bus notification bridge into the live
        // toast stack and mirror the lock state into the chrome so the
        // stack can suppress content while locked. Cheap no-ops when no
        // bridge is installed / state is unchanged.
        frame_builder.with_chrome(|st| {
            st.set_session_locked(session.is_locked());
            st.pump_dbus_notifications();
        });

        // Mirror the compositor's live fullscreen state (set by the
        // `XdgShellHandler` fullscreen/unfullscreen path) into the rendered
        // scene, so the promoted group takes over the whole output and a later
        // exit restores the exact prior tiling. The overlay host guards against
        // a group id it doesn't host — the live client tree and the rendered
        // tree are not yet unified (Phase 4), so an unknown id is a safe no-op
        // rather than blanking the shell.
        frame_builder.set_fullscreen(state.fullscreen_group);

        // Decide whether this frame needs repainting *before* doing the
        // expensive layout pass. Both queries below are cheap and don't
        // need a fresh layout: `needs_animation_frame` just walks the
        // widget tree, and the chrome revision is a plain counter bumped
        // by state mutations (input handlers, the notification pump, the
        // fullscreen mirror above). Skipping `relayout` + `paint` entirely
        // on idle frames is what lets the loop fall to near-zero CPU at
        // rest instead of burning a core re-laying-out an unchanged tree.
        // Hit-testing stays correct because input is only dispatched on
        // ticks that carry a backend event, and those force a repaint (and
        // thus a relayout) below.
        let revision = frame_builder.chrome_revision();
        let revision_changed = revision != last_revision;
        let animating = frame_builder.needs_animation_frame();
        let keepalive_due = last_paint.elapsed() >= keepalive;
        let should_paint = force_first_paint
            || had_backend_events
            || session_changed
            || revision_changed
            || animating
            || keepalive_due;

        // Diagnostic: `QOS_PAINT_STATS=1` logs, every ~2 s, how many
        // frames painted vs idled and which condition forced the repaints.
        // Lets us confirm the render loop actually idles (and pinpoint any
        // perpetual-animation source) without guessing.
        if paint_stats {
            frames_total += 1;
            if should_paint {
                frames_painted += 1;
                if had_backend_events { cause_events += 1; }
                if session_changed { cause_session += 1; }
                if revision_changed { cause_revision += 1; }
                if animating { cause_anim += 1; }
                if keepalive_due && !(had_backend_events || session_changed || revision_changed || animating) {
                    cause_keepalive += 1;
                }
            }
            if stats_since.elapsed() >= Duration::from_secs(2) {
                eprintln!(
                    "paint-stats: {frames_painted}/{frames_total} painted | \
                     events={cause_events} revision={cause_revision} anim={cause_anim} \
                     session={cause_session} keepalive={cause_keepalive}"
                );
                frames_total = 0; frames_painted = 0;
                cause_events = 0; cause_revision = 0; cause_anim = 0;
                cause_session = 0; cause_keepalive = 0;
                stats_since = Instant::now();
            }
        }

        if should_paint {
            // Lay out only when we're about to paint. Any structural change
            // (spliced group, tab switch, resize) is applied here, right
            // before the scene is built.
            frame_builder.relayout();
            // When locked, paint the compositor-owned lock overlay instead
            // of the chrome — app content is never submitted while locked.
            let scene = if session.is_locked() {
                let theme = frame_builder.theme().clone();
                let viewport = frame_builder.viewport();
                // Count-only summary on the lock screen — no bodies leak.
                let notif_count = frame_builder.chrome().notification_count();
                lock_screen::draw_lock_scene(
                    viewport,
                    &theme,
                    frame_builder.text_renderer_mut(),
                    notif_count,
                )
            } else {
                frame_builder.paint()
            };
            backend.submit_frame(&scene, OutputId(0));
            last_paint = Instant::now();
            // Re-read after `relayout`: its `resync_strips_from_tree` pass
            // can bump the revision, and we must capture that post-paint
            // value so the next idle tick doesn't see a phantom change.
            last_revision = frame_builder.chrome_revision();
            force_first_paint = false;
        }

        // Real-input e2e Phase 1b: accept any new test-control
        // connections, then drain pending bytes from each open one
        // (each call may complete zero or more commands, parses
        // them via `test_control::parse_command`, and writes JSON
        // responses back). Closed connections (EOF or error) get
        // collected at the bottom of the loop. The listener stays
        // non-blocking so the frame-pacing loop never stalls on
        // test traffic.
        if let Some(listener) = &test_control_listener {
            loop {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if let Some(conn) = TestControlConn::new(stream) {
                            test_control_conns.push(conn);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        eprintln!("nirios compositor: test_control accept: {e}");
                        break;
                    }
                }
            }
            for conn in test_control_conns.iter_mut() {
                pump_test_control_conn(conn, &mut frame_builder, &mut test_control_ctx);
            }
            test_control_conns.retain(|c| !c.closed);
        }

        // Phase 7 fix: drain wayland client traffic + push pending
        // protocol updates back out. `dispatch_clients` advances each
        // client's request queue (registry, get_compositor, get_xdg_surface,
        // commit, …) and routes them through the `*Handler` impls.
        // `flush_clients` pumps server-originated events (configure, …)
        // back out to clients. Mirrors smithay 0.7's `examples/minimal.rs`.
        if let Err(err) = display.dispatch_clients(&mut state) {
            eprintln!("nirios compositor: dispatch_clients error: {err}");
            break;
        }
        if let Err(err) = display.flush_clients() {
            eprintln!("nirios compositor: flush_clients error: {err}");
            break;
        }

        // Pump any pending calloop sources (the listening socket
        // included) and sleep up to one frame. The calloop loop itself
        // dispatches sources on each iteration; this is what lets
        // newly-accepted connections fan out into `insert_client` calls.
        if let Err(err) = event_loop.dispatch(frame_period, &mut state) {
            eprintln!("nirios compositor: event loop dispatch error: {err}");
            break;
        }
        let _ = &state.display_handle;

        // Phase 7 test introspection: serialize the live `WorkspaceTree`
        // to the requested path. We write to a tempfile-in-the-same-dir
        // and rename to make readers see a consistent snapshot (the test
        // polls the file concurrently with the frame loop writing it).
        if let Some(path) = test_dump_path.as_ref() {
            if let Ok(json) = serde_json::to_vec(&state.workspace_tree) {
                let final_path = std::path::Path::new(path);
                let tmp_path = {
                    let mut p = final_path.as_os_str().to_os_string();
                    p.push(".tmp");
                    std::path::PathBuf::from(p)
                };
                if std::fs::write(&tmp_path, &json).is_ok() {
                    let _ = std::fs::rename(&tmp_path, final_path);
                }
            }
        }
    }

    // `event_loop.run` only returns when the loop is asked to stop; we never
    // do that today, but keep an explicit exit so the function's `!` return
    // type is honoured. `display` is dropped here, severing its clients.
    drop(display);
    // D-3: explicitly drop the test-control socket guard so its
    // `Drop` impl unlinks the socket file before we hand control to
    // `process::exit` (which skips destructors). On the panic path
    // the unwind already runs `Drop` so we get cleanup either way.
    drop(_test_control_unlink);
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    //! Phase 2b unit coverage for the test-control execution path.
    //!
    //! The integration test (`tests/control_socket_test.rs`) needs a
    //! Wayland display and is `#[ignore]` by default; these unit tests
    //! exercise the same `execute_planned_action` → `FrameBuilder::dispatch_event`
    //! wire without spinning up a process, so a CI run that can't open
    //! winit still gates against regressions here.
    use super::*;

    /// Sanity: `CursorMoved` planned action updates the context's cached
    /// cursor and dispatches through the FrameBuilder without panicking.
    /// The cursor cache matters because subsequent `MouseInput` events
    /// (which don't carry positions on winit) get hit-tested against
    /// `last_cursor` inside the host.
    #[test]
    fn cursor_moved_action_updates_ctx_cursor() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut ctx = TestControlCtx::new();
        execute_planned_action(
            &PlannedAction::CursorMoved { x: 250.0, y: 75.0 },
            &mut fb,
            &mut ctx,
        );
        assert_eq!(ctx.cursor.x, 250.0);
        assert_eq!(ctx.cursor.y, 75.0);
    }

    /// The full click sequence (`CursorMoved` + button-down + button-up)
    /// dispatches all three events without panicking. This is the
    /// minimum bar: Phase 2a returns these three actions for a
    /// `click_at` command; Phase 2b's contract is that all three reach
    /// `ShellOverlayHost::handle_event` and the host doesn't crash on
    /// any of them.
    #[test]
    fn full_click_sequence_dispatches_without_panic() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut ctx = TestControlCtx::new();
        let actions = vec![
            PlannedAction::CursorMoved { x: 800.0, y: 450.0 },
            PlannedAction::MouseLeftButton { pressed: true },
            PlannedAction::MouseLeftButton { pressed: false },
        ];
        for action in &actions {
            execute_planned_action(action, &mut fb, &mut ctx);
        }
        // No panic + relayout still works = wire intact.
        fb.relayout();
        let scene = fb.paint();
        assert!(!scene.encoding().is_empty());
    }

    /// Click sequence aimed at the wheel-disc's plus slot mutates the
    /// chrome state — proves the planned actions reach the host's
    /// click handlers, not just dispatch into a void.
    ///
    /// The bootstrap workspace tree seeds three demo workspaces; pressing
    /// the wheel-disc plus slot calls `chrome.add_workspace("Untitled")`,
    /// which grows that count to four. Asserting on the count is more
    /// robust than asserting on internal widget state because the chrome
    /// is the contract every consumer sees.
    #[test]
    fn click_on_wheel_plus_slot_grows_workspace_count() {
        let mut fb = FrameBuilder::new((1600, 900));
        // Paint once so the wheel-disc populates its plus_rect — the
        // press handler hit-tests against it, and it's `Rect::ZERO`
        // until the first draw pass runs.
        let _ = fb.paint();
        // Find the wheel disc rect by its opt-in `test_id`. We then
        // approximate the plus slot's position: it's a 28 px disc
        // centred on the visible footprint, 8 px below the visible
        // arc's bottom tip (see `sidebar_wheel_disc::draw`). Rather
        // than re-derive that arithmetic, click slightly below the
        // wheel's centre — close enough to land inside the plus rect
        // on the bootstrap layout.
        let registry = fb.widget_registry();
        let wheel_rect = registry
            .get("wheel_disc")
            .copied()
            .expect("wheel_disc test_id resolves on bootstrap tree");

        let initial_count = fb.chrome().tree().workspaces.len();

        // Walk down the wheel rect's vertical axis in 4 px increments
        // until a click lands on the plus slot (adds a workspace).
        // The plus slot is near the bottom of the wheel disc; this
        // sweep avoids hard-coding the exact y the wheel's `draw`
        // pass uses internally.
        let mut ctx = TestControlCtx::new();
        let cx = (wheel_rect.x0 + wheel_rect.x1) / 2.0;
        let mut found = false;
        let mut y = wheel_rect.y0 + 8.0;
        while y < wheel_rect.y1 {
            execute_planned_action(
                &PlannedAction::CursorMoved { x: cx, y },
                &mut fb,
                &mut ctx,
            );
            execute_planned_action(
                &PlannedAction::MouseLeftButton { pressed: true },
                &mut fb,
                &mut ctx,
            );
            execute_planned_action(
                &PlannedAction::MouseLeftButton { pressed: false },
                &mut fb,
                &mut ctx,
            );
            if fb.chrome().tree().workspaces.len() > initial_count {
                found = true;
                break;
            }
            y += 4.0;
        }
        assert!(
            found,
            "sweep of clicks down the wheel disc's vertical axis \
             should hit the plus slot and grow the workspace count \
             from {initial_count}"
        );
    }

    // ===================================================================
    // Phase 3 — drag / key / type / modifiers executor coverage
    // ===================================================================

    /// `SetModifiers` action mutates `ctx.modifiers` directly. The
    /// host reads the snapshot on the next chord, so this is the
    /// state the harness depends on for chord injection (Ctrl+K, …).
    #[test]
    fn set_modifiers_action_updates_ctx_mask() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut ctx = TestControlCtx::new();
        execute_planned_action(
            &PlannedAction::SetModifiers {
                ctrl: true,
                shift: false,
                alt: false,
                logo: true,
            },
            &mut fb,
            &mut ctx,
        );
        assert!(ctx.modifiers.state().control_key());
        assert!(ctx.modifiers.state().super_key());
        assert!(!ctx.modifiers.state().shift_key());
        assert!(!ctx.modifiers.state().alt_key());
    }

    /// A keyboard press+release pair dispatches through the host
    /// without panicking. The minimum bar is the same as
    /// `full_click_sequence_dispatches_without_panic` — the wire
    /// stays intact for both press and release.
    #[test]
    fn keyboard_key_press_release_dispatches_without_panic() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut ctx = TestControlCtx::new();
        for pressed in [true, false] {
            execute_planned_action(
                &PlannedAction::KeyboardKey {
                    key: KeyTag::Named(NamedKeyTag::Escape),
                    pressed,
                },
                &mut fb,
                &mut ctx,
            );
        }
        fb.relayout();
        let _ = fb.paint();
    }

    // ===================================================================
    // Phase 4 — snapshot executor coverage
    // ===================================================================

    /// Sanity: `Snapshot` action against a fresh FrameBuilder writes a
    /// real PNG to disk. Asserts both the magic byte signature and a
    /// non-trivial byte count — a corrupted/early-terminated PNG would
    /// pass the magic check but be tiny, so the >1KB floor catches the
    /// "wrote only a header" failure mode.
    ///
    /// Skipped on environments without a wgpu adapter (CI without
    /// llvmpipe) because `OffscreenRenderer::new` panics; the executor
    /// itself catches that panic so the assertion would silently pass
    /// without producing a file — we explicitly check for the file's
    /// existence and skip the rest gracefully if the renderer was
    /// unavailable.
    #[test]
    fn execute_snapshot_writes_a_png_at_path() {
        let tmpdir =
            tempfile::tempdir().expect("create tempdir for snapshot test");
        let path = tmpdir.path().join("test.png");
        let mut fb = FrameBuilder::new((640, 360));
        let mut ctx = TestControlCtx::new();
        execute_planned_action(
            &PlannedAction::Snapshot { path: path.clone() },
            &mut fb,
            &mut ctx,
        );
        // If the renderer wasn't available the executor would have
        // logged + returned without creating the file. Treat that as
        // a soft skip — mirrors `tests/sidebar_wheel_disc_snapshot.rs::
        // try_new_renderer`.
        if !path.exists() {
            eprintln!(
                "execute_snapshot_writes_a_png_at_path: no wgpu adapter, \
                 skipping pixel assertions"
            );
            return;
        }
        let bytes = std::fs::read(&path).expect("read snapshot PNG");
        // PNG magic: \x89PNG\r\n\x1a\n.
        assert_eq!(
            &bytes[..8],
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
            "expected PNG magic at start of {path:?}"
        );
        assert!(
            bytes.len() > 1024,
            "expected non-trivial PNG output (>1KB); got {} bytes",
            bytes.len()
        );
    }

    /// Hostile path inputs — the executor must log the error and
    /// return without panicking. The dispatcher's `Ok` response was
    /// already on the wire; the compositor must not die because a
    /// client typo asked for `/nonexistent/dir/x.png`. Phase 5 will
    /// upgrade these logs into structured wire errors.
    #[test]
    fn execute_snapshot_with_bogus_dir_does_not_crash() {
        let mut fb = FrameBuilder::new((640, 360));
        let mut ctx = TestControlCtx::new();
        // Path under a nonexistent parent — File::create will fail with
        // ENOENT; the executor must log and return.
        execute_planned_action(
            &PlannedAction::Snapshot {
                path: std::path::PathBuf::from("/nonexistent/dir/x.png"),
            },
            &mut fb,
            &mut ctx,
        );
        // No panic == pass. Sanity-check the FrameBuilder is still
        // usable after the failed snapshot (we didn't leave it in a
        // half-painted state).
        fb.relayout();
        let _ = fb.paint();
    }

    // ===================================================================
    // Phase 5 — state_dump
    // ===================================================================

    /// `FrameBuilderControlState::chrome_state_json` returns an object
    /// with an `overlays` key carrying the four flags. The shape
    /// matches what `qos-control state-dump` clients query, so a
    /// regression here would silently break the CLI.
    #[test]
    fn state_dump_chrome_includes_overlays_field() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb };
        let chrome = state.chrome_state_json();
        let overlays = chrome
            .get("overlays")
            .expect("chrome_state_json should carry `overlays` key");
        // The four bool flags should all be reachable through the
        // `overlays` sub-object — addressing them by string mirrors
        // how the CLI / shell-test harness will read them.
        for key in ["overview", "quick_settings", "ws_settings", "collaborators"] {
            assert!(
                overlays.get(key).and_then(|v| v.as_bool()).is_some(),
                "overlays should carry boolean `{key}`; got {overlays:?}"
            );
        }
    }

    /// `FrameBuilderControlState::workspace_tree_json` exposes a
    /// `workspaces` array on the bootstrap layout. The CLI's
    /// `state-dump` smoke depends on this — without it the JSON would
    /// still be valid but the consumer would have no entrypoint to
    /// the workspace list.
    #[test]
    fn state_dump_workspace_tree_includes_workspaces_array() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb };
        let tree = state.workspace_tree_json();
        let workspaces = tree
            .get("workspaces")
            .and_then(|v| v.as_array())
            .expect("workspace_tree_json should carry `workspaces` array");
        // The bootstrap tree seeds at least one demo workspace —
        // matches the assertions in `bootstrap_default` tests in
        // `shell/state.rs`.
        assert!(
            !workspaces.is_empty(),
            "bootstrap workspace tree should not be empty"
        );
    }

    /// Dispatching `Command::StateDump` against a `FrameBuilderControlState`
    /// returns `Response::State` and produces no actions. The
    /// integration round-trip lives in `tests/control_socket_test.rs`;
    /// this unit-level smoke catches regressions that don't need a
    /// spawned compositor.
    #[test]
    fn dispatch_state_dump_against_frame_builder_returns_state() {
        let fb = FrameBuilder::new((1600, 900));
        let state = FrameBuilderControlState { fb: &fb };
        let (resp, actions) =
            test_control::dispatch_command(&test_control::Command::StateDump, &state);
        assert!(actions.is_empty());
        let test_control::Response::State { workspace_tree, chrome } = resp else {
            panic!("expected Response::State; got something else");
        };
        // Sanity: both blobs are non-null and shaped like objects.
        assert!(workspace_tree.is_object(), "workspace_tree should be an object");
        assert!(chrome.is_object(), "chrome should be an object");
    }

    /// Cmd+Tab via the test_control wire opens the overview overlay —
    /// proves modifiers + named-key chords reach the host's chord
    /// handler exactly like `ShellDriver::press_key` does for
    /// in-process scenarios.
    #[test]
    fn cmd_tab_chord_via_test_control_opens_overview() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint();
        assert!(
            !fb.chrome().overlays.overview,
            "bootstrap layout starts with overview closed"
        );
        let mut ctx = TestControlCtx::new();
        // Plant the Super modifier, then dispatch Tab press+release.
        execute_planned_action(
            &PlannedAction::SetModifiers {
                ctrl: false,
                shift: false,
                alt: false,
                logo: true,
            },
            &mut fb,
            &mut ctx,
        );
        execute_planned_action(
            &PlannedAction::KeyboardKey {
                key: KeyTag::Named(NamedKeyTag::Tab),
                pressed: true,
            },
            &mut fb,
            &mut ctx,
        );
        execute_planned_action(
            &PlannedAction::KeyboardKey {
                key: KeyTag::Named(NamedKeyTag::Tab),
                pressed: false,
            },
            &mut fb,
            &mut ctx,
        );
        assert!(
            fb.chrome().overlays.overview,
            "Super+Tab via test_control should toggle overview on"
        );
    }

    /// D-3 contract: dropping an `UnlinkOnDrop` removes the file at
    /// its path. The compositor's `run` holds a guard around the
    /// test-control socket file so it gets unlinked on clean exit
    /// + panic-unwind. Previously the socket lingered until the next
    /// boot's `bind_test_control_socket` scrubbed it.
    #[test]
    fn unlink_on_drop_removes_socket_file_on_drop() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let path = tmpdir.path().join("nirios-test-control.sock");
        // Use an arbitrary regular-file stand-in; the guard removes
        // anything `remove_file` accepts, so we don't need a real
        // unix socket to exercise the contract.
        std::fs::write(&path, b"").expect("create stand-in file");
        assert!(path.exists(), "test setup: file should exist after write");
        let guard = UnlinkOnDrop(path.clone());
        drop(guard);
        assert!(
            !path.exists(),
            "UnlinkOnDrop must remove the file at its path on drop \
             (D-3); got lingering file at {path:?}"
        );
    }

    /// D-10 contract: `read_bounded_line` caps a malicious / buggy
    /// client's input at `TEST_CONTROL_MAX_LINE_BYTES`. The previous
    /// implementation called `BufRead::read_line` with no limit, so a
    /// 100k-byte newline-less stream would buffer in full and could
    /// drive the compositor toward OOM.
    ///
    /// We construct an in-memory reader of 100k bytes with no
    /// newline and assert the bounded reader returns `TooLarge`
    /// rather than allocating unbounded. The caller (`pump_…`) closes
    /// the connection on this error; that contract is exercised
    /// transitively by the existing test_control integration tests
    /// — here we focus on the new helper's bound.
    #[test]
    fn pump_drops_oversized_lines() {
        use std::io::BufReader;
        let stream: Vec<u8> = vec![b'A'; 100_000];
        let mut reader = BufReader::new(&stream[..]);
        let mut out = String::new();
        let res = read_bounded_line(&mut reader, &mut out, TEST_CONTROL_MAX_LINE_BYTES);
        match res {
            Err(ReadLineError::TooLarge) => {
                // Expected. `out` is intentionally still empty here
                // (we don't surface the truncated bytes to the parser
                // because they'd parse as garbage anyway).
                assert!(
                    out.is_empty(),
                    "read_bounded_line must not hand a truncated line \
                     to the parser; got {} bytes",
                    out.len()
                );
            }
            Err(ReadLineError::Io(e)) => panic!(
                "expected TooLarge on a 100k newline-less stream; got io: {e}"
            ),
            Ok(opt) => panic!(
                "expected TooLarge on a 100k newline-less stream; got Ok({:?})",
                opt.is_some()
            ),
        }
    }

    // ===================================================================
    // Track D2 — session lock chord / unlock detection
    // ===================================================================

    fn mods_with_super(on: bool) -> Modifiers {
        let mut mask = ModifiersState::empty();
        if on {
            mask |= ModifiersState::SUPER;
        }
        Modifiers::from(mask)
    }

    /// Super+L on key press is the lock chord.
    #[test]
    fn super_l_press_is_lock_chord() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("l")), true);
        assert!(is_lock_chord(&ev, &mods_with_super(true)));
        // Upper-case 'L' (Shift+Super+L) also counts.
        let ev_upper = keyboard_input_event(Key::Character(SmolStr::new("L")), true);
        assert!(is_lock_chord(&ev_upper, &mods_with_super(true)));
    }

    /// Without Super held, plain `l` is not the lock chord (it types).
    #[test]
    fn l_without_super_is_not_lock_chord() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("l")), true);
        assert!(!is_lock_chord(&ev, &mods_with_super(false)));
    }

    /// The lock chord only fires on press, not release — otherwise the
    /// key-up would re-trigger and toggle twice per keystroke.
    #[test]
    fn super_l_release_is_not_lock_chord() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("l")), false);
        assert!(!is_lock_chord(&ev, &mods_with_super(true)));
    }

    /// A non-keyboard event (cursor move) is never the lock chord even
    /// with Super held.
    #[test]
    fn cursor_move_is_not_lock_chord() {
        let ev = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(10.0, 10.0),
        };
        assert!(!is_lock_chord(&ev, &mods_with_super(true)));
    }

    /// Enter and Escape on press are the explicit unlock action.
    #[test]
    fn enter_and_escape_press_are_unlock_actions() {
        let enter = keyboard_input_event(Key::Named(NamedKey::Enter), true);
        let escape = keyboard_input_event(Key::Named(NamedKey::Escape), true);
        assert!(is_unlock_input(&enter));
        assert!(is_unlock_input(&escape));
    }

    /// A plain character keypress does not unlock — only the deliberate
    /// Enter/Escape gesture does.
    #[test]
    fn char_key_is_not_unlock_action() {
        let ev = keyboard_input_event(Key::Character(SmolStr::new("a")), true);
        assert!(!is_unlock_input(&ev));
    }

    /// Enter on *release* is not an unlock action — press is the gesture.
    #[test]
    fn enter_release_is_not_unlock_action() {
        let ev = keyboard_input_event(Key::Named(NamedKey::Enter), false);
        assert!(!is_unlock_input(&ev));
    }

    /// `apply_session_transition` maps Blanked/Woke to the backend's
    /// `set_dpms`; Locked/Unlocked are pure state and touch no backend.
    /// We assert via a spy backend that records `set_dpms` calls.
    #[test]
    fn apply_transition_drives_backend_dpms() {
        use crate::compositor::backend::{Backend, BackendEvent, Output};
        use vello::Scene;

        #[derive(Default)]
        struct SpyBackend {
            dpms_calls: Vec<bool>,
            outputs: Vec<Output>,
        }
        impl Backend for SpyBackend {
            fn submit_frame(&mut self, _scene: &Scene, _output: OutputId) {}
            fn poll_events(&mut self, _cb: &mut dyn FnMut(BackendEvent)) {}
            fn outputs(&self) -> &[Output] {
                &self.outputs
            }
            fn set_dpms(&mut self, on: bool) {
                self.dpms_calls.push(on);
            }
        }

        let mut backend = SpyBackend::default();
        apply_session_transition(SessionTransition::Blanked, &mut backend);
        apply_session_transition(SessionTransition::Woke, &mut backend);
        apply_session_transition(SessionTransition::Locked, &mut backend);
        apply_session_transition(SessionTransition::Unlocked, &mut backend);
        // Blanked → set_dpms(false), Woke → set_dpms(true); Locked /
        // Unlocked produce no backend calls.
        assert_eq!(backend.dpms_calls, vec![false, true]);
    }
}

