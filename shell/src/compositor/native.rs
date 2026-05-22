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
use std::sync::Arc;
use std::time::Duration;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use vello::kurbo::Rect;

use crate::compositor::backend::{Backend, BackendEvent, DrmBackend, OutputId, WinitBackend};
use crate::compositor::frame::FrameBuilder;
use crate::compositor::state::{ClientState, State};
use crate::compositor::test_control::{
    self, ControlState, parse_command, serialise_response,
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
const TEST_CONTROL_ENV: &str = "NIRIOS_TEST_CONTROL";

/// Bind the test-control unix socket. Returns `None` if the env var is
/// unset or the bind fails — bind failures are reported via
/// `eprintln!` but don't abort the compositor (the production path
/// must never depend on the test surface).
fn bind_test_control_socket() -> Option<UnixListener> {
    let path = std::env::var_os(TEST_CONTROL_ENV)?;
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
            Some(listener)
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
}

/// Drain any pending bytes from `conn`'s buffer. For each complete
/// line (terminated by `\n`), parse it as a [`test_control::Command`],
/// dispatch against `frame_builder`, and write the JSON response back
/// to the connection. Partial lines stay buffered for the next tick.
///
/// On EOF / connection error / write failure, marks the connection
/// closed so the outer loop can drop it.
///
/// Phase 1b — only read-only commands (`list_targets`) are fully
/// implemented end-to-end. Phase 2a's `click_at` / `click_named`
/// commands still parse + dispatch through the pure-logic layer (see
/// `test_control::PlannedAction`), but their planned actions are
/// **not yet executed** — that wireup is Phase 2b (needs ShellOverlayHost
/// `handle_event` plumbing with AutoCommit / ObjId / focus / NodeId
/// pieces that currently live in `ShellDriver` and aren't available
/// here yet).
fn pump_test_control_conn(conn: &mut TestControlConn, frame_builder: &FrameBuilder) {
    loop {
        let mut line = String::new();
        match conn.reader.read_line(&mut line) {
            Ok(0) => {
                conn.closed = true;
                return;
            }
            Ok(_) => {
                let state = FrameBuilderControlState { fb: frame_builder };
                let (resp, actions) = match parse_command(&line) {
                    Ok(cmd) => test_control::dispatch_command(&cmd, &state),
                    Err(err_resp) => (err_resp, Vec::new()),
                };
                if !actions.is_empty() {
                    eprintln!(
                        "nirios compositor: test_control received {} planned \
                         action(s); execution is Phase 2b (not yet wired)",
                        actions.len()
                    );
                }
                let mut out = serialise_response(&resp);
                out.push('\n');
                if let Err(e) = conn.reader.get_mut().write_all(out.as_bytes()) {
                    eprintln!("nirios compositor: test_control write: {e}");
                    conn.closed = true;
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return;
            }
            Err(e) => {
                eprintln!("nirios compositor: test_control read: {e}");
                conn.closed = true;
                return;
            }
        }
    }
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
            b.set_base_color(quick_ui::theme::Theme::dark().bg_app);
            Box::new(b)
        }
        BackendKind::Drm => match DrmBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => {
                eprintln!("nirios compositor: --backend drm failed: {e}");
                std::process::exit(1);
            }
        },
    };

    // Phase 4: per-frame scene production lifted out of `bin/shots`.
    // The compositor and the offscreen tooling share `FrameBuilder`,
    // so the chrome rendered into the host window is byte-identical
    // to the screenshots `cargo run --bin shots` produces.
    let initial_size = backend
        .outputs()
        .first()
        .map(|o| o.size)
        .unwrap_or((1600, 900));
    let mut frame_builder = FrameBuilder::new(initial_size);

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
    // Bound to whatever path `NIRIOS_TEST_CONTROL` points at; unset
    // means the socket is never created (zero-cost in production).
    // Each loop tick polls the listener for new connections and drains
    // pending bytes from existing ones.
    let test_control_listener = bind_test_control_socket();
    let mut test_control_conns: Vec<TestControlConn> = Vec::new();

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
    let mut close_requested = false;
    let frame_period = Duration::from_millis(16);
    while !close_requested {
        backend.poll_events(&mut |event| match event {
            BackendEvent::CloseRequested => {
                close_requested = true;
            }
            BackendEvent::OutputResized { id: _, size, scale: _ } => {
                if size.0 > 0 && size.1 > 0 {
                    frame_builder.set_viewport(size);
                }
            }
            BackendEvent::Input | BackendEvent::Redraw | BackendEvent::Focus(_) => {
                // Phase 4 doesn't route input through the seat layer
                // yet (Phase 6) and `Redraw` is implicit because we
                // re-paint every tick.
            }
        });

        frame_builder.relayout();
        let scene = frame_builder.paint();
        backend.submit_frame(&scene, OutputId(0));

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
                pump_test_control_conn(conn, &frame_builder);
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
    std::process::exit(0)
}
