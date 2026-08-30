//! What the compositor actually puts on the wire.
//!
//! A client cares which globals arrive in the registry, not which
//! `*State::new` calls a constructor made — so this connects a **real**
//! `wayland-client` to a **real** `wayland-server` display driven by the
//! compositor's own `State`, and reads the registry the way GTK, Qt or SDL
//! would. GTK refuses a Wayland display without `wl_data_device_manager`
//! and silently falls back to a nonexistent X11 display with no logged
//! error, and qOS's own toolkit never asks for it — so nothing in-tree
//! noticed it was missing until the wire itself was observed.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

/// Every global the compositor advertised, by interface name.
#[derive(Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

impl Dispatch<wl_registry::WlRegistry, ()> for Seen {
    fn event(
        state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = event {
            state.0.lock().expect("globals").push(interface);
        }
    }
}

/// Bring up the compositor state on one end of a socket pair, connect a
/// client to the other, and return the interfaces the registry offered.
fn advertised_globals() -> Vec<String> {
    let event_loop: EventLoop<'static, shell::compositor::state::State> =
        EventLoop::try_new().expect("calloop");
    let mut display: Display<shell::compositor::state::State> = Display::new().expect("display");
    let mut state = shell::compositor::state::State::new(event_loop.handle(), display.handle());

    let (server_side, client_side) = UnixStream::pair().expect("socketpair");
    display
        .handle()
        .insert_client(
            server_side,
            Arc::new(shell::compositor::state::ClientState::default()),
        )
        .expect("insert client");

    let conn = Connection::from_socket(client_side).expect("client connection");
    let mut queue = conn.new_event_queue::<Seen>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    let mut seen = Seen::default();
    let sink = seen.0.clone();

    // One blocking round for the registry burst, then non-blocking drains for
    // the trailer — a second blocking dispatch would wait forever once the
    // globals arrive and the server has nothing more to send.
    let _ = queue.flush();
    display
        .dispatch_clients(&mut state)
        .expect("server dispatch");
    display.flush_clients().expect("flush");
    queue
        .blocking_dispatch(&mut seen)
        .expect("client dispatch of the registry burst");
    for _ in 0..3 {
        let _ = queue.flush();
        let _ = display.dispatch_clients(&mut state);
        let _ = display.flush_clients();
        let _ = queue.dispatch_pending(&mut seen);
    }

    sink.lock().expect("globals").clone()
}

/// Globals a toolkit needs that the compositor advertises from
/// construction. `wl_compositor`, `wl_shm` and `wl_data_device_manager`
/// are the three `gdk_wayland_display_open` checks before using a Wayland
/// display at all; the rest are what a client needs to get a window up.
///
/// **`wl_output` is deliberately absent** — the backend creates it per
/// attached monitor, so a `State` with no backend has none; the
/// xdg-output manager is checked separately below.
const REQUIRED_AT_CONSTRUCTION: &[&str] = &[
    "wl_compositor",
    "wl_shm",
    "wl_data_device_manager",
    "wl_subcompositor",
    "xdg_wm_base",
    "wl_seat",
    "zxdg_output_manager_v1",
];

#[test]
fn every_interface_a_toolkit_requires_is_advertised() {
    let globals = advertised_globals();
    assert!(
        !globals.is_empty(),
        "the client saw no globals at all — the harness failed to talk to \
         the compositor, so this test proved nothing. Fix the harness \
         rather than the assertion."
    );
    let missing: Vec<&str> = REQUIRED_AT_CONSTRUCTION
        .iter()
        .copied()
        .filter(|want| !globals.iter().any(|g| g == want))
        .collect();
    assert!(
        missing.is_empty(),
        "missing {missing:?}. A toolkit that requires these refuses the \
         display outright rather than degrading, so an application using \
         it cannot start. Advertised: {globals:?}"
    );
}

/// Named separately because it is the one that was actually absent, and a
/// specific failure message beats being one entry in a list.
#[test]
fn the_clipboard_global_is_advertised() {
    let globals = advertised_globals();
    assert!(
        globals.iter().any(|g| g == "wl_data_device_manager"),
        "wl_data_device_manager is not advertised. GTK checks for it \
         alongside wl_compositor and wl_shm and refuses the display when \
         it is absent, so every GTK application — which is most Flatpaks \
         — fails to open a window. Advertised: {globals:?}"
    );
}
