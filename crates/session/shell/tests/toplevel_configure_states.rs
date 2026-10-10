//! What a tiled window is told about itself.
//!
//! Toolkits read `xdg_toplevel.configure` states to decide how to draw:
//! without `activated` every window paints itself as the unfocused one, and
//! without the four `tiled_*` edges a client-side-decorating toolkit keeps
//! its shadow, rounded corners and resize borders inside the tile. Reads a
//! real socket, since both defects live in what goes on the wire.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use shell::compositor::frame::FrameBuilder;
use shell::shell::state::TileContent;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::support::wayland;

const ACTIVATED: u32 = 4;
const TILED: [u32; 4] = [5, 6, 7, 8];

/// Every configure each toplevel (by its app id) received: size and states.
type Configures = Arc<Mutex<HashMap<&'static str, Vec<(i32, i32, Vec<u32>)>>>>;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configures: Configures,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<xdg_toplevel::XdgToplevel, &'static str> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        app_id: &&'static str,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = event
        {
            let states = states
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            state
                .configures
                .lock()
                .expect("configures")
                .entry(*app_id)
                .or_default()
                .push((width, height, states));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);

struct Harness {
    wl: wayland::Harness<Client>,
    frame: FrameBuilder,
}

impl Deref for Harness {
    type Target = wayland::Harness<Client>;
    fn deref(&self) -> &Self::Target {
        &self.wl
    }
}

impl DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wl
    }
}

impl Harness {
    fn new() -> Self {
        let wl = wayland::Harness::new();
        // The protocol callbacks and the chrome share one tree, as in the session.
        let frame = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1600, 900));
        Self { wl, frame }
    }

    /// Open a toplevel up to the point a toolkit waits for its configure.
    fn open(
        &mut self,
        compositor: &wl_compositor::WlCompositor,
        wm_base: &xdg_wm_base::XdgWmBase,
        app_id: &'static str,
    ) -> (wl_surface::WlSurface, xdg_surface::XdgSurface, u64) {
        let qh = self.queue.handle();
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, app_id);
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.pump();
        std::mem::forget(toplevel);
        let id = self.foreign_id(app_id);
        (surface, xdg, id)
    }

    /// Make `surface`'s tile the focused one, as a click or keyboard
    /// navigation does, then run the per-tick focus reconciliation.
    fn focus(&mut self, surface: u64) {
        self.frame.with_chrome(|chrome| {
            {
                let mut tree = chrome.tree_mut();
                let workspace = tree.desktop.active;
                let (strip, group) = tree
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|ws| ws.strips.iter())
                    .find_map(|strip| {
                        strip.window_groups.iter().find_map(|group| {
                            group
                                .tabs
                                .iter()
                                .any(|tab| {
                                    matches!(tab.tile, TileContent::Foreign { surface_id, .. }
                                        if surface_id == surface)
                                })
                                .then_some((strip.id, group.id))
                        })
                    })
                    .expect("the surface has a tile");
                tree.desktop.active_strip.insert(workspace, strip);
                tree.desktop.set_active_group(strip, group);
            }
            chrome.bump();
        });
        shell::compositor::client_input::sync_keyboard_focus(&mut self.wl.state, &self.frame);
        self.pump();
    }

    fn last_states(&self, app_id: &str) -> Vec<u32> {
        self.client
            .configures
            .lock()
            .expect("configures")
            .get(app_id)
            .and_then(|seen| seen.last())
            .map_or_else(
                || panic!("{app_id} was never configured"),
                |(_, _, states)| states.clone(),
            )
    }
}

#[test]
fn the_focused_tile_is_activated_and_every_tile_is_tiled() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let (_first_surface, _first_xdg, first) = h.open(&compositor, &wm_base, "org.example.First");
    let (_second_surface, _second_xdg, second) =
        h.open(&compositor, &wm_base, "org.example.Second");

    for id in [first, second] {
        shell::compositor::foreign::store().note_placement(id, Rect::new(0.0, 0.0, 800.0, 600.0));
    }
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    for app in ["org.example.First", "org.example.Second"] {
        let states = h.last_states(app);
        for edge in TILED {
            assert!(
                states.contains(&edge),
                "{app}'s opening configure must carry every tiled edge — without \
                 them a toolkit draws its shadow and resize border inside the \
                 tile. States: {states:?}",
            );
        }
    }

    // Focus goes to a tile only once its client has drawn (it cannot take
    // keyboard focus unmapped); stand in for that first buffer.
    for id in [first, second] {
        shell::compositor::foreign::store().publish_rgba(id, &[0; 4], 1, 1);
    }

    h.focus(first);
    let first_states = h.last_states("org.example.First");
    assert!(
        first_states.contains(&ACTIVATED),
        "the focused tile must be configured activated, or it paints itself \
         as an unfocused window. States: {first_states:?}",
    );
    assert!(
        TILED.iter().all(|edge| first_states.contains(edge)),
        "activation must not drop the tiled edges: {first_states:?}",
    );
    assert!(!h.last_states("org.example.Second").contains(&ACTIVATED));
    assert_eq!(h.state.activated_toplevel(), Some(first));

    h.focus(second);
    let first_states = h.last_states("org.example.First");
    let second_states = h.last_states("org.example.Second");
    drop(h);
    assert!(
        !first_states.contains(&ACTIVATED),
        "the tile that lost focus must be told so, or two windows look focused \
         at once. States: {first_states:?}",
    );
    assert!(
        second_states.contains(&ACTIVATED),
        "the newly focused tile must be activated. States: {second_states:?}",
    );
    assert!(TILED.iter().all(|edge| first_states.contains(edge)));
}

/// A client's `set_window_geometry` reaches the frame store with the commit
/// that applies it, and from there crops what the tile shows.
#[test]
fn committed_window_geometry_crops_the_tile_to_the_window() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let (surface, xdg, id) = h.open(&compositor, &wm_base, "org.example.Shadowed");
    let store = shell::compositor::foreign::store();
    store.publish_rgba(id, &vec![0; 140 * 120 * 4], 140, 120);
    assert_eq!(store.frame_size(id), Some((140, 120)));

    // A libadwaita-style window: 20px of shadow on every side.
    xdg.set_window_geometry(20, 20, 100, 80);
    surface.commit();
    h.pump();
    drop(h);

    assert_eq!(
        store.frame_size(id),
        Some((100, 80)),
        "the tile must show the declared window, not the shadow around it",
    );
    let frame = store.snapshot(id).expect("frame");
    assert_eq!((frame.width, frame.height), (100, 80));
    let tile = Rect::new(300.0, 100.0, 400.0, 180.0);
    assert_eq!(
        store.surface_origin(id, tile, (300.0, 100.0)),
        (280.0, 80.0),
        "input must address the window where it is drawn: surface-local \
         (20, 20) at the tile's corner",
    );
    store.forget(id);
}

/// At 2x the declared geometry is still logical: the tile shows twice as
/// many buffer pixels, and input takes off the logical offset, not the
/// buffer-pixel one.
#[test]
fn committed_buffer_scale_puts_the_window_and_input_in_logical_pixels() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let (surface, xdg, id) = h.open(&compositor, &wm_base, "org.example.HiDpi");
    let store = shell::compositor::foreign::store();
    store.publish_rgba(id, &vec![0; 480 * 480 * 4], 480, 480);

    surface.set_buffer_scale(2);
    xdg.set_window_geometry(10, 20, 200, 200);
    surface.commit();
    h.pump();
    drop(h);

    assert_eq!(
        store.frame_size(id),
        Some((400, 400)),
        "200x200 logical at 2x, cut out from (20, 40) in the buffer",
    );
    let tile = Rect::new(300.0, 100.0, 500.0, 300.0);
    assert_eq!(store.surface_point(id, tile, (300.0, 100.0)), (10.0, 20.0));
    assert_eq!(
        store.surface_origin(id, tile, (350.0, 150.0)),
        (290.0, 80.0),
        "the window's corner is surface-local (10, 20), whatever the scale",
    );
    store.forget(id);
}

/// Super+F on a tiled client: the fullscreen configure sizes it to the
/// output, and the promoted tile still laid out beneath must not size it
/// back to the tile. Leaving fullscreen hands the tile size back.
#[test]
fn a_fullscreen_client_keeps_the_output_size_over_its_tile() {
    const FULLSCREEN: u32 = 2;
    let mut h = Harness::new();
    h.state.output_size = (1280, 900);
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, "org.example.Terminal");
    toplevel.set_app_id("org.example.Terminal".into());
    surface.commit();
    h.pump();
    let id = h.foreign_id("org.example.Terminal");
    let store = shell::compositor::foreign::store();
    let column = Rect::new(0.0, 0.0, 320.0, 820.0);
    store.note_placement(id, column);
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();

    toplevel.set_fullscreen(None);
    h.pump();
    let last = |h: &Harness| {
        h.client.configures.lock().expect("configures")["org.example.Terminal"]
            .last()
            .cloned()
            .expect("configured")
    };
    let (width, height, states) = last(&h);
    assert!(states.contains(&FULLSCREEN), "states: {states:?}");
    assert_eq!(
        (width, height),
        (1280, 900),
        "fullscreen is the output, not the column"
    );

    // The promoted tile reports its own rect every frame.
    store.note_placement(id, Rect::new(0.0, 0.0, 320.0, 900.0));
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    let after_tile = last(&h);
    assert_eq!(
        (after_tile.0, after_tile.1),
        (width, height),
        "the tile must not resize a fullscreen client to its column"
    );

    toplevel.unset_fullscreen();
    h.pump();
    store.note_placement(id, column);
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    let restored = last(&h);
    drop(h);
    store.forget(id);
    assert!(
        !restored.2.contains(&FULLSCREEN),
        "states: {:?}",
        restored.2
    );
    assert_eq!(
        (restored.0, restored.1),
        (320, 820),
        "leaving fullscreen gives the client its tile back"
    );
}

/// A page's fullscreen (the Browser's `xdg_toplevel.set_fullscreen`) takes
/// the output: the compositor records the tab's window group as the
/// fullscreen one (which is what paints the output-sized overlay over the
/// bars), configures the client `fullscreen` at the output's size, and the
/// client's exit (Esc) drops both again.
#[test]
fn a_page_fullscreen_request_takes_the_output_and_exit_gives_it_back() {
    const FULLSCREEN: u32 = 2;
    let mut h = Harness::new();
    h.state.output_size = (1280, 900);
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, "org.tilo.Browser");
    toplevel.set_app_id("org.tilo.Browser".into());
    surface.commit();
    h.pump();
    let id = h.foreign_id("org.tilo.Browser");
    let store = shell::compositor::foreign::store();
    store.note_placement(id, Rect::new(0.0, 0.0, 320.0, 820.0));
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    assert_eq!(h.state.fullscreen_group, None);

    toplevel.set_fullscreen(None);
    h.pump();
    let entered = (
        h.state.fullscreen_group,
        h.last_states("org.tilo.Browser"),
    );

    toplevel.unset_fullscreen();
    h.pump();
    let left = (
        h.state.fullscreen_group,
        h.last_states("org.tilo.Browser"),
    );
    drop(h);
    store.forget(id);

    assert!(entered.0.is_some(), "the tab's group is the fullscreen one");
    assert!(entered.1.contains(&FULLSCREEN), "states: {:?}", entered.1);
    assert_eq!(left.0, None, "exit releases the group");
    assert!(!left.1.contains(&FULLSCREEN), "states: {:?}", left.1);
}
