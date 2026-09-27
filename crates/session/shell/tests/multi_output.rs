//! What a client sees of a multi-output desktop.
//!
//! Each case lays the compositor's outputs out from one of the named
//! headless fixtures (`TILO_HEADLESS_OUTPUTS`), advertises them through the
//! production `OutputGlobals`, and reads them back over the wire the way a
//! toolkit or xwayland-satellite does: `wl_output` geometry, mode, scale and
//! name, `zxdg_output_v1` logical rectangles, and `wl_surface.enter`/`leave`
//! as a surface moves between displays.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use shell::compositor::backend::headless::output_fixture;
use shell::compositor::backend::{Output, OutputId};
use shell::compositor::output_globals::OutputGlobals;
use shell::compositor::state::{ClientState, State};
use shell::compositor::surface_outputs::Memberships;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use vello::kurbo::{Rect, Vec2};
use wayland_client::protocol::{wl_compositor, wl_output, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

/// Everything one `wl_output` (and its `zxdg_output_v1`) told the client.
#[derive(Clone, Debug, Default)]
pub struct SeenOutput {
    pub position: Option<(i32, i32)>,
    pub transform: Option<wl_output::Transform>,
    pub mode: Option<(i32, i32, i32)>,
    pub scale: Option<i32>,
    pub name: Option<String>,
    pub logical_position: Option<(i32, i32)>,
    pub logical_size: Option<(i32, i32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceEvent {
    Enter(u32),
    Leave(u32),
}

#[derive(Default)]
pub struct Client {
    compositor: Option<wl_compositor::WlCompositor>,
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    /// Bound outputs by registry name, in advertisement order.
    outputs: BTreeMap<u32, (wl_output::WlOutput, SeenOutput)>,
    xdg_outputs: Vec<zxdg_output_v1::ZxdgOutputV1>,
    surface_events: Vec<SurfaceEvent>,
}

impl Client {
    fn seen_mut(&mut self, name: u32) -> &mut SeenOutput {
        &mut self.outputs.get_mut(&name).expect("bound output").1
    }

    fn registry_name_of(&self, output: &wl_output::WlOutput) -> u32 {
        *self
            .outputs
            .iter()
            .find(|(_, (bound, _))| bound == output)
            .expect("event names a bound output")
            .0
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "wl_output" => {
                let output = registry.bind(name, version.min(4), qh, name);
                state.outputs.insert(name, (output, SeenOutput::default()));
            }
            "zxdg_output_manager_v1" => {
                state.xdg_output_manager = Some(registry.bind(name, version.min(3), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for Client {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = state.seen_mut(*name);
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                seen.position = Some((x, y));
                if let WEnum::Value(t) = transform {
                    seen.transform = Some(t);
                }
            }
            wl_output::Event::Mode {
                width,
                height,
                refresh,
                ..
            } => seen.mode = Some((width, height, refresh)),
            wl_output::Event::Scale { factor } => seen.scale = Some(factor),
            wl_output::Event::Name { name } => seen.name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for Client {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = state.seen_mut(*name);
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                seen.logical_position = Some((x, y));
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                seen.logical_size = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_surface::WlSurface,
        event: wl_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::Enter { output } => {
                let name = state.registry_name_of(&output);
                state.surface_events.push(SurfaceEvent::Enter(name));
            }
            wl_surface::Event::Leave { output } => {
                let name = state.registry_name_of(&output);
                state.surface_events.push(SurfaceEvent::Leave(name));
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(Client: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Client: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);

/// A compositor `State` with its outputs advertised, and one client.
pub struct Harness {
    _event_loop: EventLoop<'static, State>,
    display: Display<State>,
    pub state: State,
    pub globals: OutputGlobals,
    pub outputs: Vec<Output>,
    conn: Connection,
    queue: EventQueue<Client>,
    pub client: Client,
}

impl Harness {
    /// Bring up the compositor with a named output fixture and let a client
    /// bind every `wl_output` and its `zxdg_output_v1`.
    pub fn with_fixture(fixture: &str) -> Self {
        let outputs =
            output_fixture(fixture).unwrap_or_else(|| panic!("unknown output fixture {fixture}"));
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state.set_output_layout(&outputs);
        let mut globals = OutputGlobals::new();
        globals.sync::<State>(&display.handle(), &outputs, None);

        let (client_side, server_side) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");
        let conn = Connection::from_socket(client_side).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        let _registry = conn.display().get_registry(&queue.handle(), ());

        let mut harness = Self {
            _event_loop: event_loop,
            display,
            state,
            globals,
            outputs,
            conn,
            queue,
            client: Client::default(),
        };
        let expected = harness.outputs.len();
        harness.pump("registry", |c| {
            c.compositor.is_some() && c.xdg_output_manager.is_some() && c.outputs.len() == expected
        });
        let manager = harness.client.xdg_output_manager.clone().expect("manager");
        let qh = harness.queue.handle();
        let bound: Vec<(u32, wl_output::WlOutput)> = harness
            .client
            .outputs
            .iter()
            .map(|(name, (output, _))| (*name, output.clone()))
            .collect();
        for (name, output) in bound {
            let xdg = manager.get_xdg_output(&output, &qh, name);
            harness.client.xdg_outputs.push(xdg);
        }
        harness.pump("wl_output and xdg_output state", |c| {
            c.outputs.values().all(|(_, seen)| {
                seen.mode.is_some()
                    && seen.scale.is_some()
                    && seen.logical_position.is_some()
                    && seen.logical_size.is_some()
            })
        });
        harness
    }

    /// Run client and server until `done` holds, or fail at the hang bound.
    pub fn pump(&mut self, label: &str, done: impl Fn(&Client) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while Instant::now() < deadline {
            let _ = self.conn.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.client);
            if done(&self.client) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("multi-output walk stalled at {label}");
    }

    /// What the client learned about each output, in `OutputId` order
    /// (globals are advertised in that order, so registry names follow it).
    pub fn seen(&self) -> Vec<SeenOutput> {
        self.client
            .outputs
            .values()
            .map(|(_, seen)| seen.clone())
            .collect()
    }

    /// The registry name the client bound `OutputId(id)` under.
    pub fn registry_name(&self, id: u32) -> u32 {
        *self
            .client
            .outputs
            .keys()
            .nth(usize::try_from(id).expect("small id"))
            .expect("output bound")
    }

    /// Create a client surface and return it with the compositor's id for it.
    pub fn create_surface(&mut self) -> (wl_surface::WlSurface, u64) {
        let before: Vec<u64> = self.state.surface_handles.keys().copied().collect();
        let compositor = self.client.compositor.clone().expect("wl_compositor");
        let surface = compositor.create_surface(&self.queue.handle(), ());
        surface.commit();
        self.pump("surface creation", |_| true);
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(id) = self
                .state
                .surface_handles
                .keys()
                .copied()
                .find(|id| !before.contains(id))
            {
                return (surface, id);
            }
            assert!(Instant::now() < deadline, "server never saw the surface");
            self.pump("surface creation", |_| true);
        }
    }

    /// Place a painted surface at `rect` (logical desktop coordinates) through the
    /// production membership path, and return the enter/leave events the
    /// client received for it.
    pub fn place(
        &mut self,
        memberships: &mut Memberships,
        surface_id: u64,
        rect: Rect,
    ) -> Vec<SurfaceEvent> {
        self.client.surface_events.clear();
        let changes = memberships.reconcile(&self.outputs, &[(surface_id, rect)]);
        let expected = changes.len();
        self.globals
            .send_surface_changes(&self.state.surface_handles, changes);
        self.pump("surface enter/leave", |c| {
            c.surface_events.len() >= expected
        });
        std::mem::take(&mut self.client.surface_events)
    }
}

fn output(outputs: &[Output], id: u32) -> &Output {
    outputs
        .iter()
        .find(|o| o.id == OutputId(id))
        .expect("fixture output")
}

#[test]
fn every_output_is_advertised_with_its_own_mode_scale_and_refresh() {
    let h = Harness::with_fixture("docked-laptop");
    let seen = h.seen();
    assert_eq!(seen.len(), 2);
    // The laptop panel: 2x HiDPI at 60 Hz.
    assert_eq!(seen[0].mode, Some((2560, 1600, 60_000)));
    assert_eq!(seen[0].scale, Some(2));
    // The desktop monitor keeps its own 144 Hz rather than inheriting the
    // panel's rate.
    assert_eq!(seen[1].mode, Some((2560, 1440, 144_000)));
    assert_eq!(seen[1].scale, Some(1));
    for s in &seen {
        // One global space: xdg-output and wl_output agree on where each
        // output sits.
        assert_eq!(s.logical_position, s.position);
    }
    let names: Vec<_> = seen.iter().map(|s| s.name.clone()).collect();
    assert!(names.iter().all(Option::is_some), "{names:?}");
    assert_ne!(names[0], names[1], "each output needs a distinct name");
}

#[test]
fn xdg_output_sizes_are_the_mode_over_the_scale() {
    let h = Harness::with_fixture("docked-laptop");
    let seen = h.seen();
    assert_eq!(seen[0].logical_size, Some((1280, 800)));
    assert_eq!(seen[1].logical_size, Some((2560, 1440)));
}

#[test]
fn a_rotated_output_reports_its_mode_unrotated_and_its_logical_size_rotated() {
    let h = Harness::with_fixture("portrait-plus-4k");
    let seen = h.seen();
    assert_eq!(seen[0].transform, Some(wl_output::Transform::_90));
    // `wl_output.mode` is what the connector drives; the arrangement is
    // portrait.
    assert_eq!(seen[0].mode.map(|(w, h, _)| (w, h)), Some((1920, 1080)));
    assert_eq!(seen[0].logical_size, Some((1080, 1920)));
    assert_eq!(seen[1].logical_size, Some((1920, 1080)));
    assert_eq!(seen[1].scale, Some(2));
}

#[test]
fn a_surface_enters_and_leaves_outputs_as_it_crosses_them() {
    let mut h = Harness::with_fixture("vertical-stack");
    let (_surface, id) = h.create_surface();
    let top = output(&h.outputs, 0).clone();
    let bottom = output(&h.outputs, 1).clone();
    let (top_name, bottom_name) = (h.registry_name(0), h.registry_name(1));
    let mut memberships = Memberships::default();

    let on_top = Rect::new(100.0, 100.0, 900.0, 700.0);
    assert_eq!(
        h.place(&mut memberships, id, on_top),
        vec![SurfaceEvent::Enter(top_name)]
    );

    // Straddling the seam between the monitor and the laptop below it.
    let bottom_rect = bottom.logical_rect();
    let seam = bottom_rect.y0;
    let straddle = Rect::new(
        bottom_rect.x0 + 100.0,
        seam - 200.0,
        bottom_rect.x0 + 900.0,
        seam + 200.0,
    );
    assert_eq!(
        h.place(&mut memberships, id, straddle),
        vec![SurfaceEvent::Enter(bottom_name)]
    );

    let on_bottom = straddle + Vec2::new(0.0, 400.0);
    assert!(on_bottom.y0 >= top.logical_rect().y1);
    assert_eq!(
        h.place(&mut memberships, id, on_bottom),
        vec![SurfaceEvent::Leave(top_name)]
    );
}

#[test]
fn unplugging_an_output_leaves_it_before_its_global_goes() {
    let mut h = Harness::with_fixture("docked-laptop");
    let (_surface, id) = h.create_surface();
    let external_name = h.registry_name(1);
    let external = output(&h.outputs, 1).clone();
    let mut memberships = Memberships::default();
    let external_x = external.logical_rect().x0;
    let on_external = Rect::new(external_x + 10.0, 10.0, external_x + 500.0, 500.0);
    assert_eq!(
        h.place(&mut memberships, id, on_external),
        vec![SurfaceEvent::Enter(external_name)]
    );

    let remaining = vec![output(&h.outputs, 0).clone()];
    let leaves = memberships.leave_removed_outputs(&remaining);
    h.client.surface_events.clear();
    h.globals
        .send_surface_changes(&h.state.surface_handles, leaves);
    h.pump("leave on unplug", |c| !c.surface_events.is_empty());
    assert_eq!(
        h.client.surface_events,
        vec![SurfaceEvent::Leave(external_name)]
    );
}

/// Each output's `(x, y, width, height)` in the logical space, as
/// xwayland-satellite and wlroots-style clients compute it from xdg-output.
fn logical_rects(h: &Harness) -> Vec<(i32, i32, i32, i32)> {
    h.seen()
        .iter()
        .map(|s| {
            let (x, y) = s.logical_position.expect("xdg_output logical_position");
            let (w, h) = s.logical_size.expect("xdg_output logical_size");
            (x, y, w, h)
        })
        .collect()
}

#[test]
fn xdg_output_rects_tile_the_logical_desktop_without_gaps() {
    // Mixed scales used to report physical positions beside logical sizes,
    // leaving a dead band after every HiDPI output that X11 clients under
    // xwayland-satellite placed windows into.
    for (fixture, expected) in [
        (
            "docked-laptop",
            vec![(0, 0, 1280, 800), (1280, 0, 2560, 1440)],
        ),
        (
            "vertical-stack",
            vec![(0, 0, 2560, 1440), (512, 1440, 1536, 960)],
        ),
        (
            "portrait-plus-4k",
            vec![(0, 0, 1080, 1920), (1080, 0, 1920, 1080)],
        ),
        (
            "mixed-scale-offset",
            vec![(0, 0, 1280, 800), (1280, 200, 1920, 1080)],
        ),
    ] {
        let h = Harness::with_fixture(fixture);
        let rects = logical_rects(&h);
        assert_eq!(rects, expected, "{fixture}");
        // Every output shares an edge with another: nothing floats apart.
        for (i, a) in rects.iter().enumerate() {
            let touches = rects.iter().enumerate().any(|(j, b)| {
                i != j
                    && (a.0.saturating_add(a.2) == b.0
                        || b.0.saturating_add(b.2) == a.0
                        || a.1.saturating_add(a.3) == b.1
                        || b.1.saturating_add(b.3) == a.1)
            });
            assert!(touches, "{fixture}: output {i} {a:?} leaves a gap");
        }
    }
}

#[test]
fn the_pointer_crosses_a_2x_to_1x_seam_at_an_offset_on_the_matching_logical_row() {
    use shell::compositor::backend::layout::{logical_to_scene, move_pointer, placements};
    let outputs = output_fixture("mixed-scale-offset").expect("fixture");
    let p = placements(&outputs);
    // Logical row 400 on the 2x panel: scene row 800.
    let start = (1275.0, 400.0);
    assert_eq!(logical_to_scene(&p, start), (2550.0, 800.0));
    // Ten logical pixels right: across the seam onto the 1x monitor, whose
    // top edge is 200 logical rows lower. The pointer keeps logical row 400
    // — 200 rows into the monitor — rather than landing on its row 800 or
    // being pushed off its top edge.
    let crossed = move_pointer(&p, start, (10.0, 0.0));
    assert_eq!(crossed, (1285.0, 400.0));
    let monitor = output(&outputs, 1);
    assert_eq!(
        logical_to_scene(&p, crossed),
        (
            f64::from(monitor.position.0) + 5.0,
            f64::from(monitor.position.1) + 200.0
        )
    );
    // Above the monitor's top edge there is nothing to the right of the
    // panel, so the same motion at row 100 stops at the panel's edge.
    assert_eq!(
        move_pointer(&p, (1275.0, 100.0), (10.0, 0.0)),
        (1279.0, 100.0)
    );
}
