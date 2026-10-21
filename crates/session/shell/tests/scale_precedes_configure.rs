//! A window moved to a display of another density hears that display's
//! scale before the size it is to fill there.
//!
//! xwayland-satellite sizes an X window as the configured size times the
//! `wp_fractional_scale_v1` preferred scale it holds when the configure
//! arrives; a later scale event only moves its viewport. So a configure
//! that reaches it ahead of the new display's scale leaves the window one
//! move behind: blurry on the denser display, an oversized buffer back on
//! the other. Any client that sizes from configure reads it the same way.
//!
//! Each frame runs the run loop's own passes in its order — the per-tick
//! scale push, layout, `publish_layout_to_clients`, paint, then
//! `foreign::flush_configures` — over a two-display layout at 1.0 and 1.25,
//! and reads the wire on a real client socket.

use automerge::{AutoCommit, ROOT};
use shell::compositor::backend::headless::parse_output_spec;
use shell::compositor::backend::{Output, OutputIdentity, combined_geometry, output_geometry};
use shell::compositor::native::{publish_layout_to_clients, push_surface_scales};
use shell::compositor::output_globals::OutputGlobals;
use shell::compositor::state::State;
use shell::compositor::surface_outputs::Memberships;
use shell::shell::frame::FrameBuilder;
use shell::shell::state::{GroupId, StripId, TileContent};
use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

use crate::support::wayland;

const APP_ID: &str = "org.example.ScaleFollowsDisplay";

/// The two events whose order is under test, as the client received them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wire {
    /// `wp_fractional_scale_v1.preferred_scale`, in 1/120 units.
    Scale(u32),
    /// `xdg_toplevel.configure` size.
    Configure(i32, i32),
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    wire: Vec<Wire>,
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
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.wire.push(Wire::Configure(width, height));
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.wire.push(Wire::Scale(scale));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore WpFractionalScaleManagerV1);

/// The VM acceptance arrangement: a 1.0 display beside a 1.25 one.
fn outputs() -> Vec<Output> {
    let mut outputs = parse_output_spec("1920x1080@1,2560x1600@1.25").expect("output spec");
    for (output, (connector, serial)) in outputs.iter_mut().zip([("DP-1", "A-1"), ("DP-2", "B-1")])
    {
        output.identity = OutputIdentity {
            connector: connector.into(),
            make: "Tilo".into(),
            model: "Test Display".into(),
            serial: serial.into(),
            physical_mm: (0, 0),
        };
    }
    outputs
}

/// The compositor state, the chrome sharing its tree and the output
/// globals, wired as the session wires them.
struct Desk {
    wl: wayland::Harness<Client>,
    fb: FrameBuilder,
    outputs: Vec<Output>,
    globals: OutputGlobals,
    memberships: Memberships,
    doc: AutoCommit,
}

impl Desk {
    fn new() -> Self {
        let outputs = outputs();
        let wl = wayland::Harness::<Client>::with_state(|handle, display| {
            let mut state = State::new(handle, display);
            state.set_output_layout(&outputs);
            state
        });
        let mut globals = OutputGlobals::new();
        globals.sync::<State>(&wl.display.handle(), &outputs, None);
        let mut fb = FrameBuilder::with_handle(
            wl.state.workspace_tree.clone(),
            false,
            combined_geometry(&outputs),
        );
        fb.set_output_geometry(output_geometry(&outputs));
        Self {
            wl,
            fb,
            outputs,
            globals,
            memberships: Memberships::default(),
            doc: AutoCommit::new(),
        }
    }

    /// One tick of `compositor::native::run` that paints, in its order.
    fn frame(&mut self) {
        self.wl.pump();
        let state = &mut self.wl.state;
        state.client_input.refresh_from_tree(&state.workspace_tree);
        push_surface_scales(state, &self.fb, &[]);
        self.fb.pump_apps(&mut self.doc, &ROOT);
        self.fb.relayout();
        publish_layout_to_clients(
            &mut self.memberships,
            state,
            &self.fb,
            &[],
            &self.outputs,
            &self.globals,
        );
        let _ = self.fb.paint();
        shell::shell::foreign::flush_configures(state);
        self.wl.pump();
    }

    /// Frames until `done` holds, then a few more so anything the loop
    /// would still send late has reached the wire.
    fn settle_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.frame();
            if done(self) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never settled: {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        for _ in 0..4 {
            self.frame();
        }
    }

    /// The strip, group and tab showing `surface`.
    fn tile_of(&self, surface: u64) -> (StripId, GroupId, u64) {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        let tile = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|ws| ws.strips.iter())
            .find_map(|strip| {
                strip.window_groups.iter().find_map(|group| {
                    group
                        .tabs
                        .iter()
                        .find(|tab| {
                            matches!(tab.tile, TileContent::Foreign { surface_id, .. }
                                if surface_id == surface)
                        })
                        .map(|tab| (strip.id, group.id, tab.id))
                })
            })
            .expect("the client's toplevel was adopted into a tab");
        drop(tree);
        tile
    }

    fn display_of(&self, group: GroupId) -> Option<u32> {
        self.fb.chrome().tree().desktop.outputs.display_of(group)
    }

    fn scale_of(&self, display: u32) -> f64 {
        self.outputs
            .iter()
            .find(|output| output.id.0 == display)
            .map(|output| f64::from(output.scale))
            .expect("display")
    }
}

/// The preferred scale the client held, in 1/120 units, when it read the
/// last configure in `wire` — the scale satellite multiplies that size by.
fn scale_at_last_configure(wire: &[Wire]) -> (u32, (i32, i32)) {
    let last = wire
        .iter()
        .rposition(|event| matches!(event, Wire::Configure(..)))
        .expect("the client was configured");
    let Wire::Configure(width, height) = wire[last] else {
        unreachable!()
    };
    let held = wire[..last]
        .iter()
        .rev()
        .find_map(|event| match event {
            Wire::Scale(scale) => Some(*scale),
            Wire::Configure(..) => None,
        })
        .expect("a preferred scale preceded the configure");
    (held, (width, height))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fixture scales are small positive multiples of 1/120"
)]
fn wire_scale(scale: f64) -> u32 {
    (scale * 120.0).round() as u32
}

#[test]
fn a_window_moved_between_displays_hears_the_new_scale_before_its_new_size() {
    let mut desk = Desk::new();
    let compositor = desk
        .wl
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm_base = desk.wl.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 2);
    let manager = desk
        .wl
        .bind::<WpFractionalScaleManagerV1>("wp_fractional_scale_manager_v1", 1);
    desk.wl.pump();

    let qh = desk.wl.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let fractional = manager.get_fractional_scale(&surface, &qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(APP_ID.into());
    surface.commit();
    desk.wl.pump();
    let id = desk.wl.foreign_id(APP_ID);

    desk.settle_until("the opening configure", |desk| {
        desk.wl
            .client
            .wire
            .iter()
            .any(|event| matches!(event, Wire::Configure(w, h) if *w > 0 && *h > 0))
    });
    let (strip, group, tab) = desk.tile_of(id);
    let home = desk
        .display_of(group)
        .expect("a new window opens on a display");
    let other = u32::from(home == 0);
    assert_ne!(
        desk.scale_of(home),
        desk.scale_of(other),
        "the fixture must put the two displays at different scales, or the \
         order under test cannot be observed",
    );

    for (to, back) in [(other, home), (home, other)] {
        // The move chord acts on the focused window.
        let _ = desk.fb.host_mut().focus_window_on_strip(strip, group, tab);
        desk.frame();
        assert_eq!(
            desk.fb.chrome().tree().desktop.active_group_for(strip),
            Some(group),
            "the window to move is focused",
        );
        let sent_before_move = desk.wl.client.wire.len();
        let direction = if to > back { 1 } else { -1 };
        assert!(
            desk.fb.host_mut().move_focused_group_to_display(direction),
            "the window moves from display {back} to display {to}",
        );
        let want = desk.scale_of(to);
        desk.settle_until("the moved window is laid out on its new display", |desk| {
            desk.display_of(group) == Some(to)
                && (desk.wl.state.surface_scale(id) - want).abs() < f64::EPSILON
        });

        let wire = desk.wl.client.wire.clone();
        assert!(
            wire[sent_before_move..]
                .iter()
                .any(|event| matches!(event, Wire::Configure(..))),
            "moving to display {to} must configure the window to its new tile \
             size, or nothing here observed the order under test. Wire since \
             the move: {:?}",
            &wire[sent_before_move..],
        );
        let (held, size) = scale_at_last_configure(&wire);
        assert_eq!(
            held,
            wire_scale(want),
            "the window's last configure ({size:?}) on display {to} reached \
             the client while it still held scale {held}/120 rather than that \
             display's {}/120. A client that sizes from configure — \
             xwayland-satellite multiplies the size by the scale it holds — \
             then renders at the previous display's scale until something \
             configures it again. Wire since the move: {:?}",
            wire_scale(want),
            &wire[sent_before_move..],
        );
    }

    drop(desk);
    drop((
        fractional, toplevel, xdg, surface, manager, wm_base, compositor,
    ));
    shell::shell::foreign::store().forget(id);
}
