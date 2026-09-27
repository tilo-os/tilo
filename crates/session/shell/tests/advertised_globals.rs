//! The globals the compositor advertises on the wire.
//!
//! A client cares which globals arrive in the registry, not which
//! `*State::new` calls a constructor made — so this connects a
//! `wayland-client` to a `wayland-server` display driven by the compositor's
//! `State`, and reads the registry the way GTK, Qt, or SDL would. GTK refuses
//! a Wayland display without `wl_data_device_manager` and silently falls back
//! to a nonexistent X11 display with no logged
//! error, and Tilo's own toolkit never asks for it — so nothing in-tree
//! noticed it was missing until the wire itself was observed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

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
    advertised_globals_to(Arc::new(shell::compositor::state::ClientState::default()))
}

/// [`advertised_globals`] for a client accepted with `client` state — the
/// privileged socket, the OSK socket, or an ordinary app.
fn advertised_globals_to(client: Arc<shell::compositor::state::ClientState>) -> Vec<String> {
    let event_loop: EventLoop<'static, shell::compositor::state::State> =
        EventLoop::try_new().expect("calloop");
    let mut display: Display<shell::compositor::state::State> = Display::new().expect("display");
    let mut state = shell::compositor::state::State::new(event_loop.handle(), display.handle());

    let (server_side, client_side) = UnixStream::pair().expect("socketpair");
    display
        .handle()
        .insert_client(server_side, client)
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
    // Toolkits (GTK 4.14+, Qt 6.8+, SDL3, winit) name their cursor through
    // it instead of loading an Xcursor theme the sandbox may not contain.
    "wp_cursor_shape_manager_v1",
    // Games, emulators and remote-desktop clients lock the pointer and read
    // raw deltas; SDL and Proton refuse mouse-look without these.
    "zwp_pointer_constraints_v1",
    "zwp_relative_pointer_manager_v1",
    // Krita, GIMP, Xournal++ and every GTK/Qt app read pen pressure and
    // tilt only through tablet-v2.
    "zwp_tablet_manager_v2",
    // virt-manager, RDP clients and Moonlight capture Super through it.
    "zwp_keyboard_shortcuts_inhibit_manager_v1",
    "wp_content_type_manager_v1",
];

#[test]
fn every_toolkit_interface_and_the_clipboard_global_is_advertised() {
    // Phase `every_interface_a_toolkit_requires_is_advertised`.
    {
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

    // Phase `the_idle_inhibit_global_is_advertised`.
    // Video players and presentations keep the screen on with this; without it
    // they fall back to nothing and the session locks mid-film.
    {
        let globals = advertised_globals();
        assert!(
            globals.iter().any(|g| g == "zwp_idle_inhibit_manager_v1"),
            "zwp_idle_inhibit_manager_v1 is not advertised. Advertised: {globals:?}"
        );
    }

    // Phase `the_toplevel_icon_global_is_advertised`.
    // Windows no desktop entry describes set their own tab icon through it.
    {
        let globals = advertised_globals();
        assert!(
            globals.iter().any(|g| g == "xdg_toplevel_icon_manager_v1"),
            "xdg_toplevel_icon_manager_v1 is not advertised. Advertised: {globals:?}"
        );
    }
    // Phase `the_clipboard_global_is_advertised`.
    // Keep this check separate so its failure identifies the clipboard global.
    {
        let globals = advertised_globals();
        assert!(
            globals.iter().any(|g| g == "wl_data_device_manager"),
            "wl_data_device_manager is not advertised. GTK checks for it \
             alongside wl_compositor and wl_shm and refuses the display when \
             it is absent, so every GTK application — which is most Flatpaks \
             — fails to open a window. Advertised: {globals:?}"
        );
    }
}

/// Display configuration and panel power can black out every screen, so
/// only the privileged socket (session tools such as kanshi) sees them; an
/// app on the public socket must not.
#[test]
fn display_configuration_globals_reach_only_privileged_clients() {
    const DISPLAY_CONTROL: [&str; 2] = ["zwlr_output_manager_v1", "zwlr_output_power_manager_v1"];
    let public = advertised_globals();
    let privileged = advertised_globals_to(shell::compositor::state::ClientState::privileged_arc());
    for interface in DISPLAY_CONTROL {
        assert!(
            !public.iter().any(|g| g == interface),
            "{interface} is advertised on the public socket. Advertised: {public:?}"
        );
        assert!(
            privileged.iter().any(|g| g == interface),
            "{interface} is missing for privileged clients. Advertised: {privileged:?}"
        );
    }
}
