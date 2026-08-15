//! What the compositor actually puts on the wire.
//!
//! A client does not care which `*State::new` calls a constructor made;
//! it cares which globals arrive in the registry. So this connects a
//! **real** `wayland-client` to a **real** `wayland-server` display
//! driven by the compositor's own `State`, and reads the registry the
//! way GTK, Qt or SDL would.
//!
//! The regression it exists to catch is specific and was live: qOS's
//! own toolkit never asks for `wl_data_device_manager`, so nothing
//! in-tree noticed it was missing — and GTK refuses to use a Wayland
//! display at all without it. Every GTK application on the machine,
//! which is most Flatpaks, connected, was refused, and fell back to an
//! X11 display that does not exist. Nothing in the compositor logged an
//! error; the app looked broken.
//!
//! A check that could not observe the wire could not have caught that,
//! which is the whole reason this test talks to a socket.

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
        _: &(),
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
    let mut display: Display<shell::compositor::state::State> =
        Display::new().expect("display");
    let mut state =
        shell::compositor::state::State::new(event_loop.handle(), display.handle());

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

    // Pump both sides until the registry has been drained. Three rounds
    // is ample for a bind-and-advertise exchange and bounds the test if
    // something goes wrong.
    for _ in 0..3 {
        let _ = queue.flush();
        display.dispatch_clients(&mut state).expect("server dispatch");
        display.flush_clients().expect("flush");
        let _ = queue.blocking_dispatch(&mut seen);
    }

    let out = sink.lock().expect("globals").clone();
    out
}

/// The set GTK's `gdk_wayland_display_open` checks for before it will
/// use a Wayland display at all. Missing any one of them and it warns
/// "the Wayland compositor does not provide one or more of the required
/// interfaces" and falls back to X11.
const GTK_REQUIRED: &[&str] = &[
    "wl_compositor",
    "wl_shm",
    "wl_data_device_manager",
    "wl_subcompositor",
    "xdg_wm_base",
    "wl_seat",
    "wl_output",
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
    let missing: Vec<&str> = GTK_REQUIRED
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

/// Named separately because it is the one that was actually absent, and
/// a specific failure message beats being one entry in a list.
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
