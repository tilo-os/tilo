//! End-to-end `color-management-v1` protocol walk.
//!
//! Wires an in-process wayland-client connection through a `UnixStream`
//! pair into a headless smithay `State`, then:
//!
//! 1. Walks the `wl_registry` for `wl_compositor` + `wp_color_manager_v1`.
//! 2. Collects the compositor's advertised `supported_primaries_named`,
//!    `supported_tf_named`, and `supported_feature` events — asserts the
//!    canonical HDR set (`Srgb + Bt2020`, `Bt1886 + St2084Pq + Hlg`,
//!    `Parametric + SetPrimaries`) is present.
//! 3. Builds a parametric image description with BT.2020 primaries +
//!    PQ transfer, calls `create`, waits for `ready2`.
//! 4. Creates a `wl_surface`, mints a `wp_color_management_surface_v1`
//!    for it, attaches the image description, and commits.
//! 5. Server-side, drains dispatch, then looks up the compositor's
//!    `ColorManagementSurfaceData` for that surface and asserts the
//!    stored `Colorimetry` matches BT.2020 + PQ.

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
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

// -----------------------------------------------------------------------------
// Client-side state — collects the registry advertisements and drives
// the protocol walk. Exposes the intermediate object handles so the test
// body can send explicit requests.
// -----------------------------------------------------------------------------

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
        _: &(),
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
        _: &(),
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
        _: &(),
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
        _: &(),
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
        _: &(),
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
        _: &(),
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

impl Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, ()>
    for ClientAppState
{
    fn event(
        _: &mut Self,
        _: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        _: wp_color_management_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// -----------------------------------------------------------------------------
// Helpers to drive both sides.
// -----------------------------------------------------------------------------

/// Pump the client → server → client round-trip enough times for the
/// pending events to settle. Both sides flush + read/dispatch in a
/// bounded loop with a small sleep so events queued at either end
/// propagate.
fn pump(
    conn: &Connection,
    ev: &mut wayland_client::EventQueue<ClientAppState>,
    app: &mut ClientAppState,
    display: &mut Display<State>,
    server_state: &mut State,
    predicate: impl Fn(&ClientAppState) -> bool,
    label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let _ = conn.flush();
        let _ = display.dispatch_clients(server_state);
        let _ = display.flush_clients();
        // Drain events without blocking. `roundtrip` blocks; we
        // interleave with the server dispatch so it can complete.
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
fn client_negotiation_advertises_bt2020_pq_and_parametric_feature() {
    // Server side: minimal State (compositor + color_manager globals).
    let event_loop: EventLoop<'static, State> =
        EventLoop::try_new().expect("calloop event loop");
    let mut display: Display<State> = Display::new().expect("wayland display");
    let display_handle = display.handle();
    let mut server_state = State::new(event_loop.handle(), display_handle);

    // Client ⇄ server via a UnixStream pair.
    let (client_side, server_side) = UnixStream::pair().expect("socket pair");
    let _client = display
        .handle()
        .insert_client(server_side, Arc::new(ClientState::default()))
        .expect("insert client");

    let conn = Connection::from_socket(client_side).expect("client from socket");
    let mut event_queue: wayland_client::EventQueue<ClientAppState> =
        conn.new_event_queue();
    let qh = event_queue.handle();

    let display_proxy = conn.display();
    let _registry = display_proxy.get_registry(&qh, ());

    let mut app = ClientAppState::default();

    // Registry walk + color-manager advertisement.
    pump(
        &conn,
        &mut event_queue,
        &mut app,
        &mut display,
        &mut server_state,
        |a| a.color_manager_done && a.compositor.is_some(),
        "wait for color_manager done",
    );

    // Sanity: compositor advertised the canonical HDR set.
    assert!(
        app.supported_primaries.contains(&(Primaries::Srgb as u32)),
        "sRGB primaries not advertised: {:?}",
        app.supported_primaries
    );
    assert!(
        app.supported_primaries.contains(&(Primaries::Bt2020 as u32)),
        "BT.2020 primaries not advertised: {:?}",
        app.supported_primaries
    );
    assert!(
        app.supported_transfers.contains(&(TransferFunction::St2084Pq as u32)),
        "PQ transfer not advertised: {:?}",
        app.supported_transfers
    );
    assert!(
        app.supported_transfers.contains(&(TransferFunction::Hlg as u32)),
        "HLG transfer not advertised: {:?}",
        app.supported_transfers
    );
    assert!(
        app.supported_features.contains(&(Feature::Parametric as u32)),
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

#[test]
fn client_attaches_bt2020_pq_image_description_and_surface_data_records_it() {
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
    let mut event_queue: wayland_client::EventQueue<ClientAppState> =
        conn.new_event_queue();
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

    // 4. Drain the server dispatch loop, then look up the surface
    //    server-side by protocol id and read its
    //    `ColorManagementSurfaceData`.
    let surface_protocol_id = surface.id().protocol_id();
    let _ = conn.flush();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut server_saw_it = false;
    while Instant::now() < deadline {
        let _ = display.dispatch_clients(&mut server_state);
        let _ = display.flush_clients();
        let dh = display.handle();
        if let Ok(server_surface) = server_client
            .object_from_protocol_id::<
                smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            >(&dh, surface_protocol_id)
        {
            if let Some((color, _hdr)) = surface_current_colorimetry(&server_surface) {
                assert_eq!(color.primaries, ColorPrimaries::Bt2020);
                assert_eq!(color.transfer, TransferCharacteristics::SmpteSt2084);
                server_saw_it = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        server_saw_it,
        "compositor's SurfaceData did not record the attached BT.2020/PQ image description"
    );

    // Suppress unused warning on the SurfaceData type import — we
    // consume it indirectly through `surface_current_colorimetry`.
    let _ = std::marker::PhantomData::<ColorManagementSurfaceData>;
}
