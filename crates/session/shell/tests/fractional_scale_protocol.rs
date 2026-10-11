//! End-to-end `wp_fractional_scale_v1` protocol walk, wiring an in-process
//! wayland-client through a `UnixStream` pair into a headless smithay
//! `State`, including the `CompositorHandler::commit` /
//! `FractionalScaleHandler` paths:
//!
//! 1. Walks `wl_registry` for `wl_compositor`,
//!    `wp_fractional_scale_manager_v1`, and `wp_viewporter`.
//! 2. Sets the compositor's display scale to a *fractional* 1.5x.
//! 3. Creates a `wl_surface`, mints a `wp_fractional_scale_v1`, commits.
//! 4. Asserts `preferred_scale = round(1.5 * 120) = 180` (the protocol's
//!    1/120 fixed-point encoding), confirming fractional scaling.

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
use std::sync::Arc;
use std::time::{Duration, Instant};

use shell::compositor::state::{ClientState, State};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{wl_compositor, wl_registry, wl_subcompositor, wl_subsurface, wl_surface},
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

// Client-side state: registry advertisements + the per-surface preferred scale.

#[derive(Default)]
struct ClientAppState {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    fractional_manager: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    /// Last `preferred_scale` value the compositor sent, in `1/120` units.
    preferred_scale_120: Option<u32>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ClientAppState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(4),
                        qh,
                        (),
                    ));
                }
                "wl_subcompositor" => {
                    state.subcompositor = Some(
                        registry.bind::<wl_subcompositor::WlSubcompositor, _, _>(name, 1, qh, ()),
                    );
                }
                "wp_fractional_scale_manager_v1" => {
                    state.fractional_manager =
                        Some(registry.bind::<WpFractionalScaleManagerV1, _, _>(
                            name,
                            version.min(1),
                            qh,
                            (),
                        ));
                }
                "wp_viewporter" => {
                    state.viewporter =
                        Some(registry.bind::<WpViewporter, _, _>(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(ClientAppState: ignore wl_compositor::WlCompositor);
delegate_noop!(ClientAppState: ignore wl_surface::WlSurface);
delegate_noop!(ClientAppState: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(ClientAppState: ignore wl_subsurface::WlSubsurface);
delegate_noop!(ClientAppState: ignore WpFractionalScaleManagerV1);
delegate_noop!(ClientAppState: ignore WpViewporter);

impl Dispatch<WpFractionalScaleV1, ()> for ClientAppState {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.preferred_scale_120 = Some(scale);
        }
    }
}

/// Pump the client → server → client round-trip until `predicate` holds or
/// the deadline elapses.
fn pump(
    conn: &Connection,
    ev: &mut wayland_client::EventQueue<ClientAppState>,
    app: &mut ClientAppState,
    display: &mut Display<State>,
    server_state: &mut State,
    predicate: impl Fn(&ClientAppState) -> bool,
    label: &str,
) {
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        let _ = conn.flush();
        let _ = display.dispatch_clients(server_state);
        let _ = display.flush_clients();
        let _ = ev.dispatch_pending(app);
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = ev.dispatch_pending(app);
        if predicate(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "fractional-scale walk stalled at '{label}': predicate never held \
         (compositor={}, manager={}, viewporter={}, preferred_scale_120={:?})",
        app.compositor.is_some(),
        app.fractional_manager.is_some(),
        app.viewporter.is_some(),
        app.preferred_scale_120,
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn fractional_scale_globals_are_advertised_and_follow_the_display_scale() {
    // Phase `compositor_advertises_fractional_scale_and_viewporter_globals`.
    {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let mut display: Display<State> = Display::new().expect("wayland display");
        let display_handle = display.handle();
        let mut server_state = State::new(event_loop.handle(), display_handle);

        let (client_side, server_side) = UnixStream::pair().expect("socket pair");
        let _client = display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");

        let conn = Connection::from_socket(client_side).expect("client from socket");
        let mut event_queue: wayland_client::EventQueue<ClientAppState> = conn.new_event_queue();
        let qh = event_queue.handle();
        let _registry = conn.display().get_registry(&qh, ());

        let mut app = ClientAppState::default();
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |a| a.compositor.is_some() && a.fractional_manager.is_some() && a.viewporter.is_some(),
            "registry walk for fractional-scale + viewporter",
        );

        assert!(
            app.fractional_manager.is_some(),
            "wp_fractional_scale_manager_v1 global not advertised"
        );
        assert!(
            app.viewporter.is_some(),
            "wp_viewporter global not advertised"
        );
    }

    // Phase `surface_receives_fractional_preferred_scale_driven_by_display_scale`.
    {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let mut display: Display<State> = Display::new().expect("wayland display");
        let display_handle = display.handle();
        let mut server_state = State::new(event_loop.handle(), display_handle);

        // A *fractional* 1.5x — the value the Settings app's "Display scale"
        // persists and the compositor loads at session start.
        server_state.scale = 1.5;

        let (client_side, server_side) = UnixStream::pair().expect("socket pair");
        let _client = display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");

        let conn = Connection::from_socket(client_side).expect("client from socket");
        let mut event_queue: wayland_client::EventQueue<ClientAppState> = conn.new_event_queue();
        let qh = event_queue.handle();
        let _registry = conn.display().get_registry(&qh, ());

        let mut app = ClientAppState::default();

        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |a| a.compositor.is_some() && a.fractional_manager.is_some(),
            "registry walk",
        );

        let compositor = app.compositor.clone().expect("wl_compositor bound");
        let manager = app.fractional_manager.clone().expect("manager bound");
        let surface = compositor.create_surface(&qh, ());
        let _fractional = manager.get_fractional_scale(&surface, &qh, ());
        surface.commit();

        // The commit runs `CompositorHandler::commit`, which pushes the display
        // scale as the preferred fractional scale.
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |a| a.preferred_scale_120.is_some(),
            "preferred_scale event",
        );

        // 1.5 x 120 = 180: fractional (a 1x compositor sends 120, a 2x one 240)
        // and driven end-to-end by the compositor scale.
        assert_eq!(
            app.preferred_scale_120,
            Some(180),
            "surface did not receive the fractional 1.5× preferred scale (180/120)"
        );
    }
}

/// A subsurface is drawn as part of its window, so it is asked to render at
/// the window's scale — not the session-wide fallback of a surface nobody
/// placed. A browser told 1x for the subsurface it draws its page into, in a
/// 2x window, fills only the window's top-left quarter.
#[test]
fn a_subsurface_is_asked_for_its_windows_scale() {
    let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop event loop");
    let mut display: Display<State> = Display::new().expect("wayland display");
    let mut server_state = State::new(event_loop.handle(), display.handle());
    server_state.scale = 1.0;

    let (client_side, server_side) = UnixStream::pair().expect("socket pair");
    let _client = display
        .handle()
        .insert_client(server_side, Arc::new(ClientState::default()))
        .expect("insert client");
    let conn = Connection::from_socket(client_side).expect("client from socket");
    let mut event_queue: wayland_client::EventQueue<ClientAppState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut app = ClientAppState::default();
    pump(
        &conn,
        &mut event_queue,
        &mut app,
        &mut display,
        &mut server_state,
        |a| a.compositor.is_some() && a.subcompositor.is_some() && a.fractional_manager.is_some(),
        "registry walk",
    );
    let compositor = app.compositor.clone().expect("wl_compositor bound");
    let subcompositor = app.subcompositor.clone().expect("wl_subcompositor bound");
    let manager = app.fractional_manager.clone().expect("manager bound");

    // The window, placed on a 2x output.
    let window = compositor.create_surface(&qh, ());
    window.commit();
    pump(
        &conn,
        &mut event_queue,
        &mut app,
        &mut display,
        &mut server_state,
        |_| true,
        "window surface",
    );
    let window_id = *server_state
        .surface_handles
        .keys()
        .next()
        .expect("the window's surface reached the compositor");
    server_state.surface_scales.insert(window_id, 2.0);

    // The page, drawn into a subsurface of it.
    let page = compositor.create_surface(&qh, ());
    let _subsurface = subcompositor.get_subsurface(&page, &window, &qh, ());
    let _fractional = manager.get_fractional_scale(&page, &qh, ());
    page.commit();
    window.commit();
    pump(
        &conn,
        &mut event_queue,
        &mut app,
        &mut display,
        &mut server_state,
        |a| a.preferred_scale_120.is_some(),
        "subsurface preferred_scale event",
    );
    // Let any later event (a commit's re-statement) arrive too.
    for _ in 0..3 {
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |_| true,
            "settle",
        );
    }
    assert_eq!(
        app.preferred_scale_120,
        Some(240),
        "the subsurface must render at its window's 2x, not the session-wide 1x"
    );
}
