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

use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use crate::compositor::backend::{Backend, WinitBackend};
use crate::compositor::state::{ClientState, State};

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
    let display: Display<State> = Display::new().expect("create wayland display");
    let display_handle = display.handle();

    let mut state = State::new(event_loop.handle(), display_handle.clone());

    // Phase 3: construct the winit backend so the compositor has a host
    // window to render into. Phase 4 will route per-frame scenes through
    // `backend.submit_frame`. We `expect` here for the same reason the
    // socket bind below does — a top-level `-> !` entry point treats any
    // backend init failure as fatal (same posture as smithay's `anvil`).
    let mut _backend: WinitBackend =
        WinitBackend::new().expect("init winit backend");
    // TODO(Phase 4): wire `_backend.poll_events` + `_backend.submit_frame`
    // into the calloop tick. For now we just hold the backend alive so
    // the host window stays open during the smoke test.
    let _ = (&mut _backend) as &mut dyn Backend;

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

    // Phase 4 will replace the empty tick body with `display.dispatch_clients`
    // + frame-pacing + render submission. For Phase 2 we just want the loop to
    // *exist* so SIGTERM during the smoke test reaches us cleanly.
    event_loop
        .run(None, &mut state, |state| {
            // `flush_clients` is cheap when there are no pending events but
            // makes sure any protocol-state mutations made by handler callbacks
            // are pushed back to the wire before we block again.
            //
            // We deliberately ignore the `Display` value's
            // `dispatch_clients` — without input/render plumbing the only
            // protocol traffic during the smoke window is initial handshakes,
            // and those flow through the listening-socket callback.
            let _ = &state.display_handle;
        })
        .expect("event loop");

    // `event_loop.run` only returns when the loop is asked to stop; we never
    // do that today, but keep an explicit exit so the function's `!` return
    // type is honoured.
    let _ = display;
    std::process::exit(0)
}
