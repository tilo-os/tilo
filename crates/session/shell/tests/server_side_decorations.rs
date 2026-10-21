//! Who draws the title bar.
//!
//! Tilo draws window chrome itself, so an app that draws its own draws a
//! second one inside the first; `xdg_decoration` is the wire mechanism
//! for saying so. Not only cosmetic: a client that keeps a title bar
//! keeps it out of the size the compositor gave it — configured 670x668
//! it commits 670x633 and never commits again, leaving a 35px dead band
//! of empty tab and stranding the presentation gate.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

/// The globals offered, and every decoration mode configured, in order.
#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    modes: Arc<Mutex<Vec<zxdg_toplevel_decoration_v1::Mode>>>,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event
            && let wayland_client::WEnum::Value(mode) = mode
        {
            state.modes.lock().expect("modes").push(mode);
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zxdg_decoration_manager_v1::ZxdgDecorationManagerV1);

/// Compositor and client on a socket pair, pumped by hand so the test
/// never blocks on a message that is not coming.
type Harness = crate::support::wayland::Harness<Client>;

impl Harness {
    /// One shell frame as the run loop runs it. A configure sent the instant
    /// a toplevel is created carries a decoration mode with **no size**,
    /// telling the client to pick its own (the bug that made every app open
    /// at the wrong size), so the mode is staged and rides out on the first
    /// size-bearing configure, which this pass sends.
    fn lay_out_and_configure(&mut self, tile: vello::kurbo::Rect) {
        use shell::shell::foreign;
        for toplevel in self.state.xdg_shell_state.toplevel_surfaces().to_vec() {
            let id = shell::compositor::state::surface_id(toplevel.wl_surface());
            // What `ForeignSurfaceWidget::apply_layout` publishes once the strip has
            // given the tab a rect.
            foreign::store().note_placement(id, tile);
        }
        foreign::flush_configures(&mut self.state);
        self.pump();
    }
}

/// Returns every decoration mode configured plus the globals, so a
/// failure can say whether the manager was missing or merely silent.
fn negotiate(
    ask_for: Option<zxdg_toplevel_decoration_v1::Mode>,
) -> (Vec<String>, Vec<zxdg_toplevel_decoration_v1::Mode>) {
    let mut h = Harness::new();

    let names: Vec<String> = h.client.globals.iter().map(|(_, i, _)| i.clone()).collect();
    let modes = h.client.modes.clone();

    let Some(manager) = h.try_bind::<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>(
        "zxdg_decoration_manager_v1",
        1,
    ) else {
        return (names, Vec::new());
    };
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm_base = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    h.pump();

    // The order sctk uses: create the decoration, then state a preference.
    // The compositor answers on the first, and again on the second only if
    // it changed its mind.
    let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
    h.pump();
    if let Some(mode) = ask_for {
        decoration.set_mode(mode);
        h.pump();
    }
    // The answer is staged until the strip has a size to send with it.
    h.lay_out_and_configure(vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0));

    let out = modes.lock().expect("modes").clone();
    (names, out)
}

#[test]
fn every_client_is_given_server_side_decorations() {
    // Phase `the_decoration_manager_is_advertised`.
    // A toolkit that cannot see the global never asks — it decorates
    // itself, and there is no negotiation to lose.
    {
        let (globals, _) = negotiate(None);
        assert!(
            !globals.is_empty(),
            "the client saw no globals at all — the harness failed to talk to \
             the compositor, so this test proved nothing."
        );
        assert!(
            globals.iter().any(|g| g == "zxdg_decoration_manager_v1"),
            "zxdg_decoration_manager_v1 is not advertised, so every client \
             falls back to drawing its own title bar and keeps it out of the \
             size this compositor gave it. Advertised: {globals:?}"
        );
    }

    // Phase `a_client_that_asks_for_server_side_is_given_server_side`.
    {
        let (globals, modes) = negotiate(Some(zxdg_toplevel_decoration_v1::Mode::ServerSide));
        assert!(
            !modes.is_empty(),
            "the compositor configured no decoration mode at all. Advertised: \
             {globals:?}"
        );
        assert!(
            modes
                .iter()
                .all(|m| *m == zxdg_toplevel_decoration_v1::Mode::ServerSide),
            "expected every configure to say ServerSide; got {modes:?}. A \
             client told ClientSide draws its own title bar inside the tab \
             this compositor already drew one for."
        );
    }

    // Phase `a_client_that_asks_to_decorate_itself_is_still_given_server_side`.
    {
        let (globals, modes) = negotiate(Some(zxdg_toplevel_decoration_v1::Mode::ClientSide));
        assert!(
            !modes.is_empty(),
            "the compositor configured no decoration mode at all. Advertised: \
             {globals:?}"
        );
        assert!(
            !modes.contains(&zxdg_toplevel_decoration_v1::Mode::ClientSide),
            "the compositor agreed to client-side decoration; got {modes:?}. \
             Tilo owns window chrome, so the answer to this request is no."
        );
    }
}

/// Qt5 and some GTK/Wine builds only speak the KDE spelling of the protocol.
/// It must be advertised and answered `Server`, whatever is requested, or
/// those clients draw a second title bar.
#[test]
fn a_kde_server_decoration_client_is_given_server_mode() {
    use wayland_client::WEnum;
    use wayland_protocols_misc::server_decoration::client::{
        org_kde_kwin_server_decoration, org_kde_kwin_server_decoration_manager as manager,
    };

    #[derive(Default)]
    struct Kde {
        globals: Vec<(u32, String, u32)>,
        modes: Vec<org_kde_kwin_server_decoration::Mode>,
    }
    impl Dispatch<org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration, ()> for Kde {
        fn event(
            state: &mut Self,
            _: &org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
            event: org_kde_kwin_server_decoration::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let org_kde_kwin_server_decoration::Event::Mode {
                mode: WEnum::Value(m),
            } = event
            {
                state.modes.push(m);
            }
        }
    }
    delegate_noop!(Kde: ignore wl_compositor::WlCompositor);
    delegate_noop!(Kde: ignore wl_surface::WlSurface);
    delegate_noop!(Kde: ignore manager::OrgKdeKwinServerDecorationManager);
    crate::support::wayland::test_client!(Kde: registry);

    let mut h = crate::support::wayland::Harness::<Kde>::new();
    let manager = h
        .try_bind::<manager::OrgKdeKwinServerDecorationManager>(
            "org_kde_kwin_server_decoration_manager",
            1,
        )
        .expect("org_kde_kwin_server_decoration_manager is not advertised");
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    h.pump();
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let decoration = manager.create(&surface, &qh, ());
    h.pump();
    decoration.request_mode(org_kde_kwin_server_decoration::Mode::Client);
    h.pump();
    let modes = &h.client.modes;
    assert!(!modes.is_empty(), "no mode was sent");
    assert!(
        modes
            .iter()
            .all(|m| *m == org_kde_kwin_server_decoration::Mode::Server),
        "a KDE-protocol client was told {modes:?}; the OS owns the chrome"
    );
}
