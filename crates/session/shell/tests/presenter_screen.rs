//! Presenter view on your screen, slides on the big one.
//!
//! A laptop plugged into a television: the app opens an `.audience`
//! toplevel beside its window. The slides must land on the television even
//! though the laptop panel enumerates first and has more pixels, and the
//! app's own window must move to the laptop as the presenter view — sized
//! to that screen, back in its tab when the presentation ends. Driven over
//! a real socket with real `wl_output` globals, since which screen is which
//! is exactly what goes over the wire.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use shell::compositor::backend::headless::output_fixture;
use shell::compositor::backend::{Output, OutputId, OutputIdentity};
use shell::compositor::state::{ClientState, State};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::{Display, Resource as _};
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_output, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// The shared `docked-laptop` arrangement — a double-density panel beside a
/// monitor — with the identities of a 14" eDP panel and a 65" television
/// on HDMI. The laptop has the lower id and more pixels, so neither may
/// decide.
fn laptop_and_television() -> Vec<Output> {
    let mut outputs = output_fixture("docked-laptop").expect("docked-laptop fixture");
    outputs[0].identity = OutputIdentity {
        connector: "eDP-1".into(),
        physical_mm: (302, 189),
        ..OutputIdentity::default()
    };
    outputs[1].identity = OutputIdentity {
        connector: "HDMI-A-1".into(),
        physical_mm: (1430, 800),
        ..OutputIdentity::default()
    };
    outputs
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    /// `wl_output` protocol id → (advertised name, physical millimetres).
    outputs: Vec<(u32, String, (i32, i32))>,
    /// `(xdg_toplevel protocol id, width, height)` per configure, in order.
    configures: Arc<Mutex<Vec<(u32, i32, i32)>>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Client {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = output.id().protocol_id();
        let index = state
            .outputs
            .iter()
            .position(|(known, _, _)| *known == id)
            .unwrap_or_else(|| {
                state.outputs.push((id, String::new(), (0, 0)));
                state.outputs.len().saturating_sub(1)
            });
        match event {
            wl_output::Event::Name { name } => state.outputs[index].1 = name,
            wl_output::Event::Geometry {
                physical_width,
                physical_height,
                ..
            } => state.outputs[index].2 = (physical_width, physical_height),
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.configures.lock().expect("configures").push((
                toplevel.id().protocol_id(),
                width,
                height,
            ));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);

struct Window {
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
}

struct Harness {
    display: Display<State>,
    state: State,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    // Keeps the advertised globals alive for the test's duration.
    _globals: shell::compositor::output_globals::OutputGlobals,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    fn new(outputs: &[Output]) -> Self {
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state.set_output_layout(outputs);
        let mut globals = shell::compositor::output_globals::OutputGlobals::new();
        globals.sync::<State>(&display.handle(), outputs, None);

        let (server_side, client_side) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(server_side, Arc::new(ClientState::default()))
            .expect("insert client");
        client_side
            .set_nonblocking(true)
            .expect("non-blocking client socket");
        let conn = Connection::from_socket(client_side).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        let mut harness = Self {
            display,
            state,
            queue,
            client: Client::default(),
            conn,
            _globals: globals,
            _event_loop: event_loop,
        };
        let _registry = harness
            .conn
            .display()
            .get_registry(&harness.queue.handle(), ());
        harness.pump();
        harness
    }

    fn pump(&mut self) {
        for _ in 0..6 {
            let _ = self.queue.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.client);
        }
    }

    fn bind_all<I: Proxy + 'static>(&self, interface: &str, version: u32) -> Vec<I>
    where
        Client: Dispatch<I, ()>,
    {
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        self.client
            .globals
            .iter()
            .filter(|(_, candidate, _)| candidate == interface)
            .map(|(name, _, advertised)| {
                registry.bind::<I, (), Client>(
                    *name,
                    version.min(*advertised),
                    &self.queue.handle(),
                    (),
                )
            })
            .collect()
    }

    fn open(
        &mut self,
        compositor: &wl_compositor::WlCompositor,
        wm: &xdg_wm_base::XdgWmBase,
        app_id: &str,
    ) -> Window {
        let qh = self.queue.handle();
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        self.pump();
        Window {
            surface,
            xdg_surface,
            toplevel,
        }
    }

    /// The compositor's id for the toplevel with this protocol id.
    fn surface_id(&self, window: &Window) -> u64 {
        let wanted = window.surface.id().protocol_id();
        self.state
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| toplevel.wl_surface().id().protocol_id() == wanted)
            .map(|toplevel| shell::compositor::state::surface_id(toplevel.wl_surface()))
            .expect("the compositor knows this toplevel")
    }

    fn configures_of(&self, window: &Window) -> Vec<(i32, i32)> {
        let id = window.toplevel.id().protocol_id();
        self.client
            .configures
            .lock()
            .expect("configures")
            .iter()
            .filter(|(toplevel, _, _)| *toplevel == id)
            .map(|(_, width, height)| (*width, *height))
            .collect()
    }

    fn flush_configures(&mut self) {
        shell::compositor::foreign::flush_configures(&mut self.state);
        self.pump();
    }
}

/// The configure size for a logical rectangle.
#[allow(
    clippy::cast_possible_truncation,
    reason = "output-sized values, rounded first, far inside i32"
)]
fn logical(rect: Rect) -> (i32, i32) {
    (rect.width().round() as i32, rect.height().round() as i32)
}

#[test]
fn wl_output_names_the_connector_and_its_millimetres() {
    let mut h = Harness::new(&laptop_and_television());
    let _outputs = h.bind_all::<wl_output::WlOutput>("wl_output", 4);
    h.pump();

    // What a client ranks screens by has to reach it.
    let mut advertised: Vec<(String, (i32, i32))> = h
        .client
        .outputs
        .iter()
        .map(|(_, name, mm)| (name.clone(), *mm))
        .collect();
    advertised.sort();
    assert_eq!(
        advertised,
        vec![
            ("HDMI-A-1".to_string(), (1430, 800)),
            ("eDP-1".to_string(), (302, 189)),
        ],
        "wl_output must carry the connector name and real millimetres"
    );
}

#[test]
fn slides_go_to_the_television_and_the_presenter_view_to_the_laptop() {
    let outputs = laptop_and_television();
    let (laptop, television) = (outputs[0].logical_rect(), outputs[1].logical_rect());
    let mut h = Harness::new(&outputs);
    let compositor = h
        .bind_all::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .pop()
        .expect("wl_compositor");
    let wm = h
        .bind_all::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .pop()
        .expect("xdg_wm_base");
    let wl_outputs = h.bind_all::<wl_output::WlOutput>("wl_output", 4);
    h.pump();

    let presenter = h.open(&compositor, &wm, "so.tilo.test.slides");
    let presenter_id = h.surface_id(&presenter);
    // The editor window happens to sit on the television when the talk starts.
    let tab = Rect::new(television.x0 + 100.0, 100.0, television.x0 + 900.0, 700.0);
    shell::compositor::foreign::store().note_placement(presenter_id, tab);
    h.flush_configures();
    let tab_size = logical(tab);
    assert_eq!(h.configures_of(&presenter).last(), Some(&tab_size));

    // winit names the app before it can ask for an output, so the first
    // placement is the compositor's own choice.
    let audience = h.open(&compositor, &wm, "so.tilo.test.slides.audience");
    let audience_id = h.surface_id(&audience);
    assert_eq!(
        h.state.output_presentation_output(audience_id),
        Some(OutputId(1)),
        "the slides go to the television, not the lowest-numbered laptop panel"
    );
    assert_eq!(
        h.state.presenter_screen(),
        Some((presenter_id, OutputId(0))),
        "the presenter view moves to the laptop"
    );
    assert_eq!(
        shell::compositor::foreign::store().placement(presenter_id),
        Some(laptop)
    );
    h.flush_configures();
    let laptop_size = logical(laptop);
    assert_eq!(
        h.configures_of(&presenter).last(),
        Some(&laptop_size),
        "the presenter view is sized to the laptop screen"
    );

    // An app that drops its own single-screen fullscreen next must not
    // pull the presenter view back into its tab.
    presenter.toplevel.unset_fullscreen();
    h.pump();
    assert_eq!(
        h.state.presenter_screen(),
        Some((presenter_id, OutputId(0)))
    );
    assert_eq!(h.configures_of(&presenter).last(), Some(&laptop_size));

    // The app then names its screen; choosing the laptop sends the
    // presenter view the other way.
    let laptop_output = wl_outputs
        .iter()
        .find(|output| {
            h.client
                .outputs
                .iter()
                .any(|(id, name, _)| *id == output.id().protocol_id() && name == "eDP-1")
        })
        .expect("the laptop's wl_output");
    audience.toplevel.set_fullscreen(Some(laptop_output));
    h.pump();
    assert_eq!(
        h.state.output_presentation_output(audience_id),
        Some(OutputId(0))
    );
    assert_eq!(
        h.state.presenter_screen(),
        Some((presenter_id, OutputId(1)))
    );
    assert_eq!(
        shell::compositor::foreign::store().placement(presenter_id),
        Some(television)
    );

    // Ending the presentation restores the window to its tab.
    audience.toplevel.destroy();
    audience.xdg_surface.destroy();
    audience.surface.destroy();
    h.pump();
    assert_eq!(h.state.presenter_screen(), None);
    assert_eq!(
        shell::compositor::foreign::store().placement(presenter_id),
        Some(tab),
        "the window is back where its tab is"
    );
    h.flush_configures();
    assert_eq!(
        h.configures_of(&presenter).last(),
        Some(&tab_size),
        "and back at its tab size"
    );
    std::mem::forget(presenter);
}

#[test]
fn one_screen_keeps_the_presenter_in_its_tab() {
    let outputs = vec![Output {
        identity: OutputIdentity {
            connector: "eDP-1".into(),
            physical_mm: (302, 189),
            ..OutputIdentity::default()
        },
        ..Output::new(OutputId(0), (2880, 1800), 1.0)
    }];
    let mut h = Harness::new(&outputs);
    let compositor = h
        .bind_all::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .pop()
        .expect("wl_compositor");
    let wm = h
        .bind_all::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .pop()
        .expect("xdg_wm_base");
    h.pump();

    let presenter = h.open(&compositor, &wm, "so.tilo.test.single.slides");
    let audience = h.open(&compositor, &wm, "so.tilo.test.single.slides.audience");
    let audience_id = h.surface_id(&audience);
    assert_eq!(
        h.state.output_presentation_output(audience_id),
        Some(OutputId(0))
    );
    assert_eq!(
        h.state.presenter_screen(),
        None,
        "with nowhere else to go the presenter window stays in its tab"
    );
    std::mem::forget((presenter, audience));
}
