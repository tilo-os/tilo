//! End-to-end `color-management-v1` protocol walk: an in-process
//! wayland-client wired through a `UnixStream` pair into a headless smithay
//! `State`. Walks the registry, asserts the canonical HDR support set is
//! advertised, builds a parametric BT.2020 + PQ image description, attaches
//! it to a surface, and asserts the compositor's stored `Colorimetry`
//! matches server-side.

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

use shell::collab::colorimetry::{ColorPrimaries, TransferCharacteristics};
use shell::compositor::state::{ClientState, State};
use shell::wayland::color_management::{ColorManagementSurfaceData, surface_current_colorimetry};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_compositor, wl_registry, wl_surface},
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_surface_v1,
    wp_color_manager_v1::{
        self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
    },
    wp_image_description_creator_icc_v1::{self, WpImageDescriptionCreatorIccV1},
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

// Client-side state: registry advertisements plus intermediate object
// handles the test body sends requests against.

#[derive(Default)]
struct ClientAppState {
    compositor: Option<wl_compositor::WlCompositor>,
    color_manager: Option<WpColorManagerV1>,
    supported_features: Vec<u32>,
    supported_primaries: Vec<u32>,
    supported_transfers: Vec<u32>,
    color_manager_done: bool,
    image_description_ready: bool,
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
                    let c: wl_compositor::WlCompositor =
                        registry.bind(name, version.min(4), qh, ());
                    state.compositor = Some(c);
                }
                "wp_color_manager_v1" => {
                    let m: WpColorManagerV1 = registry.bind(name, version.min(1), qh, ());
                    state.color_manager = Some(m);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for ClientAppState {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for ClientAppState {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpColorManagerV1, ()> for ClientAppState {
    fn event(
        state: &mut Self,
        _: &WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_color_manager_v1::Event::SupportedFeature { feature } => {
                if let Ok(f) = feature.into_result() {
                    state.supported_features.push(f as u32);
                }
            }
            wp_color_manager_v1::Event::SupportedPrimariesNamed { primaries } => {
                if let Ok(p) = primaries.into_result() {
                    state.supported_primaries.push(p as u32);
                }
            }
            wp_color_manager_v1::Event::SupportedTfNamed { tf } => {
                if let Ok(t) = tf.into_result() {
                    state.supported_transfers.push(t as u32);
                }
            }
            wp_color_manager_v1::Event::Done => {
                state.color_manager_done = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionCreatorParamsV1, ()> for ClientAppState {
    fn event(
        _: &mut Self,
        _: &WpImageDescriptionCreatorParamsV1,
        _: wp_image_description_creator_params_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpImageDescriptionCreatorIccV1, ()> for ClientAppState {
    fn event(
        _: &mut Self,
        _: &WpImageDescriptionCreatorIccV1,
        _: wp_image_description_creator_icc_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpImageDescriptionV1, ()> for ClientAppState {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. } => {
                state.image_description_ready = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, ()> for ClientAppState {
    fn event(
        _: &mut Self,
        _: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        _: wp_color_management_surface_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Pump the client → server → client round-trip until pending events
/// settle: both sides flush and dispatch in a bounded loop.
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
        // `roundtrip` blocks, so drain without blocking and interleave with the
        // server dispatch.
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
        "protocol walk stalled at '{label}': predicate never held \
         (features={:?}, primaries={:?}, transfers={:?}, done={}, ready={})",
        app.supported_features,
        app.supported_primaries,
        app.supported_transfers,
        app.color_manager_done,
        app.image_description_ready
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn color_management_protocol_negotiates_records_and_survives_unsupported_requests() {
    // Phase `client_negotiation_advertises_bt2020_pq_and_parametric_feature`.
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

        let display_proxy = conn.display();
        let _registry = display_proxy.get_registry(&qh, ());

        let mut app = ClientAppState::default();

        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |a| a.color_manager_done && a.compositor.is_some(),
            "wait for color_manager done",
        );

        assert!(
            app.supported_primaries.contains(&(Primaries::Srgb as u32)),
            "sRGB primaries not advertised: {:?}",
            app.supported_primaries
        );
        assert!(
            app.supported_primaries
                .contains(&(Primaries::Bt2020 as u32)),
            "BT.2020 primaries not advertised: {:?}",
            app.supported_primaries
        );
        assert!(
            app.supported_transfers
                .contains(&(TransferFunction::St2084Pq as u32)),
            "PQ transfer not advertised: {:?}",
            app.supported_transfers
        );
        assert!(
            app.supported_transfers
                .contains(&(TransferFunction::Hlg as u32)),
            "HLG transfer not advertised: {:?}",
            app.supported_transfers
        );
        assert!(
            app.supported_features
                .contains(&(Feature::Parametric as u32)),
            "Parametric feature not advertised: {:?}",
            app.supported_features
        );
        assert!(
            app.supported_features
                .contains(&(Feature::SetPrimaries as u32)),
            "SetPrimaries feature not advertised: {:?}",
            app.supported_features
        );
    }

    // Phase `unsupported_icc_new_id_disconnects_only_the_client_without_panicking_dispatch`.
    {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let mut display: Display<State> = Display::new().expect("wayland display");
        let mut server_state = State::new(event_loop.handle(), display.handle());
        let (client_side, server_side) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");

        let conn = Connection::from_socket(client_side).expect("client from socket");
        let mut event_queue = conn.new_event_queue::<ClientAppState>();
        let qh = event_queue.handle();
        let _registry = conn.display().get_registry(&qh, ());
        let mut app = ClientAppState::default();
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |state| state.color_manager_done,
            "color manager ready",
        );

        app.color_manager
            .as_ref()
            .expect("color manager")
            .create_icc_creator(&qh, ());
        conn.flush().expect("flush unsupported request");

        // This exact dispatch used to abort the compositor in wayland-backend's
        // post-callback new-id bookkeeping. A protocol error must be confined to
        // this client instead.
        display
            .dispatch_clients(&mut server_state)
            .expect("unsupported ICC request must not panic server dispatch");
        display
            .flush_clients()
            .expect("flush client protocol error");
        // Prove the error stayed client-scoped by inserting and dispatching a
        // second ordinary Wayland client after the first one was killed.
        let (survivor_side, survivor_server) = UnixStream::pair().expect("survivor socket pair");
        display
            .handle()
            .insert_client(survivor_server, Arc::new(ClientState::default()))
            .expect("insert survivor client");
        let survivor = Connection::from_socket(survivor_side).expect("survivor client");
        let survivor_queue = survivor.new_event_queue::<ClientAppState>();
        let survivor_qh = survivor_queue.handle();
        let _survivor_registry = survivor.display().get_registry(&survivor_qh, ());
        survivor.flush().expect("flush survivor request");
        display
            .dispatch_clients(&mut server_state)
            .expect("survivor dispatch remains serviceable");
    }

    // Phase `windows_scrgb_description_becomes_ready`.
    {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let mut display: Display<State> = Display::new().expect("wayland display");
        let mut server_state = State::new(event_loop.handle(), display.handle());
        let (client_side, server_side) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");
        let conn = Connection::from_socket(client_side).expect("client from socket");
        let mut event_queue = conn.new_event_queue::<ClientAppState>();
        let qh = event_queue.handle();
        let _registry = conn.display().get_registry(&qh, ());
        let mut app = ClientAppState::default();
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |state| state.color_manager_done,
            "color manager ready",
        );
        assert!(
            app.supported_features
                .contains(&(Feature::WindowsScrgb as u32)),
            "windows_scrgb feature not advertised: {:?}",
            app.supported_features
        );
        assert!(
            app.supported_transfers
                .contains(&(TransferFunction::ExtLinear as u32)),
            "ext_linear transfer not advertised: {:?}",
            app.supported_transfers
        );
        app.color_manager
            .as_ref()
            .expect("color manager")
            .create_windows_scrgb(&qh, ());
        conn.flush().expect("flush scRGB request");
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |state| state.image_description_ready,
            "scRGB description ready",
        );
    }

    // Phase `client_attaches_bt2020_pq_image_description_and_surface_data_records_it`.
    {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().expect("calloop event loop");
        let mut display: Display<State> = Display::new().expect("wayland display");
        let display_handle = display.handle();
        let mut server_state = State::new(event_loop.handle(), display_handle);

        let (client_side, server_side) = UnixStream::pair().expect("socket pair");
        let server_client = display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");

        let conn = Connection::from_socket(client_side).expect("client from socket");
        let mut event_queue: wayland_client::EventQueue<ClientAppState> = conn.new_event_queue();
        let qh = event_queue.handle();

        let _registry = conn.display().get_registry(&qh, ());
        let mut app = ClientAppState::default();

        // 1. Registry walk.
        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |a| a.color_manager_done && a.compositor.is_some(),
            "registry walk",
        );

        // 2. Build a parametric BT.2020 + PQ description.
        let manager = app.color_manager.clone().expect("color_manager bound");
        let creator = manager.create_parametric_creator(&qh, ());
        creator.set_primaries_named(Primaries::Bt2020);
        creator.set_tf_named(TransferFunction::St2084Pq);
        let desc = creator.create(&qh, ());

        pump(
            &conn,
            &mut event_queue,
            &mut app,
            &mut display,
            &mut server_state,
            |a| a.image_description_ready,
            "image description ready",
        );

        // 3. Create a surface and attach the image description.
        let compositor = app.compositor.clone().expect("wl_compositor bound");
        let surface = compositor.create_surface(&qh, ());
        let cm_surface = manager.get_surface(&surface, &qh, ());
        cm_surface.set_image_description(&desc, RenderIntent::Perceptual);
        surface.commit();

        // 4. Read the surface's `ColorManagementSurfaceData` server-side.
        let surface_protocol_id = surface.id().protocol_id();
        let _ = conn.flush();
        let deadline = tilo_test_env::hang_deadline();
        let mut server_saw_it = false;
        while Instant::now() < deadline {
            let _ = display.dispatch_clients(&mut server_state);
            let _ = display.flush_clients();
            let dh = display.handle();
            if let Ok(server_surface) = server_client
                .object_from_protocol_id::<
                    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                >(&dh, surface_protocol_id)
                && let Some((color, _hdr)) = surface_current_colorimetry(&server_surface) {
                    assert_eq!(color.primaries, ColorPrimaries::Bt2020);
                    assert_eq!(color.transfer, TransferCharacteristics::SmpteSt2084);
                    server_saw_it = true;
                    break;
                }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            server_saw_it,
            "compositor's SurfaceData did not record the attached BT.2020/PQ image description"
        );

        // SurfaceData is consumed indirectly via `surface_current_colorimetry`.
        let _ = std::marker::PhantomData::<ColorManagementSurfaceData>;
    }
}
