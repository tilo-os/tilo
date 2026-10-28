//! The size a window opens at.
//!
//! An `xdg_toplevel.configure` carrying 0x0 means "pick your own size" —
//! a client that gets one draws a complete frame at whatever it picked,
//! so the strip's real size arrives as a *correction* one frame too late
//! and every app opens visibly undersized before snapping. The strip
//! doesn't know a tab's size until after layout, which is after the
//! toplevel is created, so the opening configure is sent from the pass
//! that runs after layout. Reads a real socket, since the defect lives
//! entirely in what goes on the wire and when.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

/// Every `xdg_toplevel.configure` size the client was sent, in order.
type Configures = Arc<Mutex<Vec<(i32, i32)>>>;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configures: Configures,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The event under test: a real toolkit reads exactly this to decide how
        // big to render, and treats 0x0 as "my choice".
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state
                .configures
                .lock()
                .expect("configures")
                .push((width, height));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore zxdg_decoration_manager_v1::ZxdgDecorationManagerV1);
delegate_noop!(Client: ignore zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1);

/// Compositor and client on a socket pair, pumped by hand so the test
/// never blocks on a message that is not coming.
type Harness = crate::support::wayland::Harness<Client>;

impl Harness {
    /// The way `ForeignSurfaceWidget::apply_layout` publishes once the strip
    /// has laid the tab out.
    fn lay_tabs_out(&self, tile: Rect) {
        for toplevel in self.state.xdg_shell_state.toplevel_surfaces().to_vec() {
            let id = shell::compositor::state::surface_id(toplevel.wl_surface());
            shell::shell::foreign::store().note_placement(id, tile);
        }
    }

    /// The pass the run loop runs after every layout.
    fn flush_configures(&mut self) {
        shell::shell::foreign::flush_configures(&mut self.state);
        self.pump();
    }
}

/// Bring a client to the point a toolkit reaches before it draws —
/// everything but the buffer, which is what it needs the configure for.
fn open_a_window(h: &mut Harness) -> Configures {
    let configures = h.client.configures.clone();

    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm_base = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let manager = h.bind::<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>(
        "zxdg_decoration_manager_v1",
        1,
    );
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id("org.tilo.test.opening-size".into());
    h.pump();

    // Toolkits negotiate decorations in the same round trip that creates the
    // toplevel, so drive it here and cover that route too.
    let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
    decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
    h.pump();

    // Leaked on purpose: destroying these would tear the toplevel down
    // before the test has read what the compositor said about it.
    std::mem::forget((surface, xdg_surface, toplevel, decoration, wm_base, manager));
    configures
}

#[test]
fn clients_are_configured_with_their_laid_out_tab_size_or_eventually_anyway() {
    // Phase `the_first_configure_a_client_receives_carries_its_tab_size`.
    {
        let mut h = Harness::new();
        let configures = open_a_window(&mut h);

        h.lay_tabs_out(Rect::new(0.0, 0.0, 940.0, 712.0));
        h.flush_configures();

        let seen = configures.lock().expect("configures").clone();
        let first = *seen.first().expect(
            "the client was never configured at all, so it can never map a \
             window — whatever else is true, that is worse than the defect \
             this test guards.",
        );
        assert_ne!(
            first,
            (0, 0),
            "the first configure carried no size. In xdg-shell that tells the \
             client to choose its own, so it commits a full frame at a size \
             the strip never asked for and is corrected afterwards — the \
             two-stage open this test exists to prevent. Configures: {seen:?}",
        );
        assert_eq!(
            first,
            (940, 712),
            "the first configure must carry the tab's laid-out size, not some \
             other size the client then has to be moved off. Configures: \
             {seen:?}",
        );
    }

    // Phase `nothing_is_configured_before_the_strip_has_laid_the_tab_out`.
    // Asserts both halves, so it can't pass by the client being
    // disconnected or the harness failing to talk at all.
    {
        let mut h = Harness::new();
        let configures = open_a_window(&mut h);

        // No tab rect published, so the strip has not laid this window out and
        // there is no size to send.
        h.flush_configures();
        let before = configures.lock().expect("configures").clone();
        assert!(
            before.is_empty(),
            "the compositor configured a client whose tab it had not laid out \
             yet, so the only size it could have sent is no size: {before:?}",
        );

        h.lay_tabs_out(Rect::new(0.0, 0.0, 800.0, 600.0));
        h.flush_configures();
        let after = configures.lock().expect("configures").clone();
        assert_eq!(
            after,
            vec![(800, 600)],
            "laying the tab out must produce exactly one configure, carrying \
             that tab's size. Anything else means the silence above was the \
             harness failing rather than the compositor waiting.",
        );
    }

    // Phase `a_toplevel_whose_tab_is_never_laid_out_is_configured_anyway`.
    // A tab that's never laid out (collapsed under a maximized sibling, or
    // off-screen) must not leave its client unable to map forever.
    {
        let mut h = Harness::new();
        let configures = open_a_window(&mut h);

        // `flush_configures` only sends the backstop once
        // `OPENING_CONFIGURE_GRACE` (250 ms) has elapsed. Poll that production
        // path instead of sleeping past a guessed 400 ms.
        tilo_test_env::wait_until(
            "a never-laid-out toplevel received its backstop configure",
            || {
                h.flush_configures();
                configures
                    .lock()
                    .expect("configures")
                    .first()
                    .is_some_and(|&(w, height)| w > 0 && height > 0)
            },
        );
        let seen = configures.lock().expect("configures").clone();
        let first = *seen.first().expect(
            "a toplevel the strip never laid out was never configured, so its \
             client can never map and the window never opens at all.",
        );
        assert!(
            first.0 > 0 && first.1 > 0,
            "the backstop configure must carry a real size — sending no size \
             would hand the client back the same choice the whole path exists \
             to take away from it. Configures: {seen:?}",
        );
    }
}

/// A slow host once laid a new tab out tens of thousands of pixels wide.
/// The client was configured to that width, could not create a drawing
/// surface past the GPU's texture limit, closed its window and exited, and
/// every app opened on the device vanished the same way. However large a tab
/// is laid out, the client is asked for a size it can render at its scale.
#[test]
fn a_runaway_tab_is_configured_to_a_size_the_client_can_render() {
    let mut h = Harness::new();
    h.state.scale = 2.0;
    let configures = open_a_window(&mut h);

    h.lay_tabs_out(Rect::new(0.0, 0.0, 780.0, 792.0));
    h.flush_configures();
    for width in [5089.0, 36736.0] {
        h.lay_tabs_out(Rect::new(0.0, 0.0, width, 792.0));
        h.flush_configures();
    }

    let seen = configures.lock().expect("configures").clone();
    assert_eq!(seen.first(), Some(&(780, 792)), "configures: {seen:?}");
    let limit = shell::shell::foreign::CLIENT_TEXTURE_LIMIT_PX / 2;
    assert!(
        seen.iter()
            .all(|&(w, height)| w <= limit && height <= limit),
        "a configure asked the client for more than {limit} logical px at \
         scale 2, past what it can render: {seen:?}",
    );
    assert_eq!(
        seen.last(),
        Some(&(limit, 792)),
        "the runaway width is held at the largest the client can draw: {seen:?}",
    );
}
