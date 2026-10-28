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
use wayland_client::protocol::{wl_compositor, wl_subcompositor, wl_subsurface, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
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
delegate_noop!(Client: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(Client: ignore wl_subsurface::WlSubsurface);

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

/// A browser's window as Firefox commits it under a client-drawn frame: the
/// toplevel buffer is the frame with a 20px shadow margin on every side, the
/// window geometry is the rectangle inside that margin, and the page is a
/// desynchronised subsurface placed at the geometry's corner, as large as
/// the geometry.
struct ShadowedBrowser {
    root: u64,
    page: u64,
    _surface: wl_surface::WlSurface,
    _xdg: xdg_surface::XdgSurface,
    _page_surface: wl_surface::WlSurface,
    _subsurface: wl_subsurface::WlSubsurface,
}

const SHADOW: i32 = 20;
const WINDOW: (i32, i32) = (100, 80);
/// The root surface's buffer: the window and its shadow on every side.
const FRAME: (u32, u32) = (
    (WINDOW.0 + 2 * SHADOW).unsigned_abs(),
    (WINDOW.1 + 2 * SHADOW).unsigned_abs(),
);
const SHADOW_PIXEL: [u8; 4] = [255, 0, 0, 255];
const PAGE_PIXEL: [u8; 4] = [0, 128, 255, 255];
/// The page's own top-left pixel, so where it lands can be told apart.
const PAGE_CORNER_PIXEL: [u8; 4] = [255, 255, 0, 255];

impl Harness {
    fn open_shadowed_browser(&mut self) -> ShadowedBrowser {
        let compositor: wl_compositor::WlCompositor = self.bind("wl_compositor", 4);
        let subcompositor: wl_subcompositor::WlSubcompositor = self.bind("wl_subcompositor", 1);
        let wm_base: xdg_wm_base::XdgWmBase = self.bind("xdg_wm_base", 2);
        self.pump();

        let (surface, xdg, root) = self.open(&compositor, &wm_base, "org.example.Browser");
        let qh = self.queue.handle();
        let page_surface = compositor.create_surface(&qh, ());
        let subsurface = subcompositor.get_subsurface(&page_surface, &surface, &qh, ());
        subsurface.set_desync();
        subsurface.set_position(SHADOW, SHADOW);
        page_surface.commit();
        xdg.set_window_geometry(SHADOW, SHADOW, WINDOW.0, WINDOW.1);
        surface.commit();
        self.pump();

        // The compositor files a client's surfaces under the client's index
        // in the high half and the protocol id in the low half.
        let page = (root & 0xFFFF_FFFF_0000_0000) | u64::from(page_surface.id().protocol_id());
        let store = shell::compositor::foreign::store();
        let (frame_w, frame_h) = FRAME;
        let frame_pixels = frame_w.checked_mul(frame_h).expect("frame size");
        store.publish_rgba(
            root,
            &SHADOW_PIXEL.repeat(usize::try_from(frame_pixels).expect("frame size")),
            frame_w,
            frame_h,
        );
        let (page_w, page_h) = (WINDOW.0.unsigned_abs(), WINDOW.1.unsigned_abs());
        let mut page_pixels = PAGE_PIXEL.repeat(
            usize::try_from(page_w.checked_mul(page_h).expect("page size")).expect("page size"),
        );
        page_pixels[..4].copy_from_slice(&PAGE_CORNER_PIXEL);
        store.publish_rgba(page, &page_pixels, page_w, page_h);
        ShadowedBrowser {
            root,
            page,
            _surface: surface,
            _xdg: xdg,
            _page_surface: page_surface,
            _subsurface: subsurface,
        }
    }
}

#[test]
fn a_page_subsurface_at_the_window_geometry_fills_the_tile_exactly() {
    let mut h = Harness::new();
    let browser = h.open_shadowed_browser();
    drop(h);
    let store = shell::compositor::foreign::store();

    assert_eq!(
        store.frame_size(browser.root),
        Some((WINDOW.0.unsigned_abs(), WINDOW.1.unsigned_abs())),
        "the tile shows the declared window, not the shadow margin around it",
    );
    let frame = store.snapshot(browser.root).expect("frame");
    assert_eq!(
        (frame.width, frame.height),
        (WINDOW.0.unsigned_abs(), WINDOW.1.unsigned_abs())
    );
    assert_eq!(
        frame.rgba.get(..4),
        Some(&PAGE_CORNER_PIXEL[..]),
        "the page's top-left pixel must land on the window's top-left corner; \
         a subsurface drawn at its parent's origin shows the page shifted up \
         and left by the shadow margin",
    );
    let off_page: Vec<(usize, usize)> = frame
        .rgba
        .chunks_exact(4)
        .enumerate()
        .skip(1)
        .filter(|(_, pixel)| *pixel != PAGE_PIXEL)
        .map(|(index, _)| {
            let width = usize::try_from(frame.width).expect("frame width");
            (index % width, index / width)
        })
        .collect();
    assert!(
        off_page.is_empty(),
        "every pixel of the window is the page: no shadow margin and no band \
         along the right or bottom edge. {} pixels are not, the first at {:?}",
        off_page.len(),
        off_page.first(),
    );

    // Input lands where the page is drawn: the tile's corner is surface-local
    // (20, 20), which is the page's own (0, 0).
    let tile = Rect::new(300.0, 100.0, 400.0, 180.0);
    let (x, y) = store.surface_point(browser.root, tile, (300.0, 100.0));
    assert_eq!(
        store.child_at(browser.root, x, y),
        Some((browser.page, 0.0, 0.0)),
        "a click on the tile's corner must reach the page's corner",
    );
    store.forget(browser.page);
    store.forget(browser.root);
}

/// A client that dies with a subsurface still mapped (killed, or crashed)
/// has its surfaces torn down after it has gone. Re-reading the window's
/// subsurfaces from that teardown must not take a lock the walk already
/// holds: it froze the compositor until its watchdog ended the session.
#[test]
fn a_client_that_dies_with_a_subsurface_does_not_freeze_the_compositor() {
    let mut h = Harness::new();
    let browser = h.open_shadowed_browser();
    // The client goes away as a killed process does: its socket closes with
    // its surfaces still alive.
    let socket = h
        .conn
        .backend()
        .poll_fd()
        .try_clone_to_owned()
        .expect("the client's socket");
    std::os::unix::net::UnixStream::from(socket)
        .shutdown(std::net::Shutdown::Both)
        .expect("close the client's socket");
    // Tears the dead client's surfaces down, subsurface and all; a frozen
    // compositor never returns from it.
    let wl = &mut h.wl;
    for _ in 0..3 {
        let _ = wl.display.dispatch_clients(&mut wl.state);
        let _ = wl.display.flush_clients();
    }
    drop(h);
    let store = shell::compositor::foreign::store();
    assert_eq!(
        store.frame_seq(browser.root),
        0,
        "the dead client's window keeps no frame",
    );
    store.forget(browser.page);
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
    let entered = (h.state.fullscreen_group, h.last_states("org.tilo.Browser"));

    toplevel.unset_fullscreen();
    h.pump();
    let left = (h.state.fullscreen_group, h.last_states("org.tilo.Browser"));
    drop(h);
    store.forget(id);

    assert!(entered.0.is_some(), "the tab's group is the fullscreen one");
    assert!(entered.1.contains(&FULLSCREEN), "states: {:?}", entered.1);
    assert_eq!(left.0, None, "exit releases the group");
    assert!(!left.1.contains(&FULLSCREEN), "states: {:?}", left.1);
}

/// A page in full screen, then the person opens an app (from search, a
/// link, another app): the new window gets focus, so the full screen ends
/// and the client is told, and its window is back in the strip. A window
/// still opening without a frame (it may yet turn out to be the page's own
/// dialog) does not end it.
#[test]
fn opening_a_window_ends_a_full_screen_and_a_provisional_one_does_not() {
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
    h.focus(id);
    toplevel.set_fullscreen(None);
    h.pump();
    let entered = h.state.fullscreen_group;

    // Another client's toplevel arrives with no frame yet: provisional.
    let (_dialog_surface, _dialog_xdg, dialog) =
        h.open(&compositor, &wm_base, "org.example.Opening");
    let after_provisional = h.state.leave_fullscreen_when_focus_moved(None);
    let kept = h.state.fullscreen_group;

    // The person opens an app from search: it lands beside the page, focused.
    {
        let mut tree = h.state.workspace_tree.write();
        let tab = shell::shell::state::Tab {
            id: 9_100_001,
            title: "Notes".into(),
            tile: TileContent::Stub {
                kind: shell::shell::state::AppKind::Other("Notes".into()),
                label: "Notes".into(),
            },
            app_title: None,
        };
        shell::shell::tab_binding::place_tab(
            &mut tree,
            tab,
            shell::shell::tab_binding::Placement::NewGroup,
        );
    }
    let user_left = h.state.leave_fullscreen_when_focus_moved(None);
    h.pump();
    let left = (h.state.fullscreen_group, h.last_states("org.tilo.Browser"));
    drop(h);
    store.forget(id);
    store.forget(dialog);

    assert!(entered.is_some(), "fixture: the page is full screen");
    assert!(!after_provisional, "no Super+F full screen to end");
    assert_eq!(kept, entered, "a window without a frame yet ends nothing");
    assert!(!user_left);
    assert_eq!(left.0, None, "the opened window ends the full screen");
    assert!(
        !left.1.contains(&FULLSCREEN),
        "the page is told it left full screen: {:?}",
        left.1
    );
}

/// Super+F on a window, then a window opened for the person: the chrome's
/// full screen ends too. A window opened for someone else ("Beside") never
/// moves the person, so the full screen stays.
#[test]
fn an_expanded_window_ends_its_full_screen_only_for_a_window_the_person_opened() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();
    let (_surface, _xdg, id) = h.open(&compositor, &wm_base, "org.example.Expanded");
    h.focus(id);
    let group = {
        let tree = h.state.workspace_tree.read();
        shell::shell::tab_binding::group_id_for_surface(&tree, id).expect("a group")
    };
    let stub = |id: u64, title: &str| shell::shell::state::Tab {
        id,
        title: title.into(),
        tile: TileContent::Stub {
            kind: shell::shell::state::AppKind::Other(title.into()),
            label: title.into(),
        },
        app_title: None,
    };

    {
        let mut tree = h.state.workspace_tree.write();
        shell::shell::tab_binding::place_tab(
            &mut tree,
            stub(9_100_002, "Shared"),
            shell::shell::tab_binding::Placement::Beside(group),
        );
    }
    let for_someone_else = h.state.leave_fullscreen_when_focus_moved(Some(group));

    {
        let mut tree = h.state.workspace_tree.write();
        shell::shell::tab_binding::place_tab(
            &mut tree,
            stub(9_100_003, "Notes"),
            shell::shell::tab_binding::Placement::NewGroup,
        );
    }
    let for_the_person = h.state.leave_fullscreen_when_focus_moved(Some(group));
    drop(h);
    shell::compositor::foreign::store().forget(id);

    assert!(!for_someone_else, "a window opened beside for someone else");
    assert!(for_the_person, "a window the person opened");
}

/// The tab the person is in: the active workspace's active strip, that
/// strip's active group, and the tab it shows.
fn focused_tab(state: &shell::compositor::state::State) -> Option<u64> {
    active_tab_of(&state.workspace_tree.read().desktop)
}

fn active_tab_of(desktop: &shell::shell::state::DesktopTree) -> Option<u64> {
    let strip = *desktop.active_strip.get(&desktop.active)?;
    let group = desktop.active_group_for(strip)?;
    desktop
        .active_workspace()?
        .strips
        .iter()
        .find(|s| s.id == strip)?
        .window_groups
        .iter()
        .find(|g| g.id == group)
        .map(|g| g.active_tab)
}

fn stub_tab(id: u64, title: &str) -> shell::shell::state::Tab {
    shell::shell::state::Tab {
        id,
        title: title.into(),
        tile: TileContent::Stub {
            kind: shell::shell::state::AppKind::Other(title.into()),
            label: title.into(),
        },
        app_title: None,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered scenario keeps the person's steps and what each changed visible"
)]
fn launching_from_full_screen_leaves_it_and_focuses_the_new_window() {
    const FULLSCREEN: u32 = 2;
    let mut h = Harness::new();
    h.state.output_size = (1280, 900);
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, "org.tilo.Video");
    toplevel.set_app_id("org.tilo.Video".into());
    surface.commit();
    h.pump();
    let id = h.foreign_id("org.tilo.Video");
    let store = shell::compositor::foreign::store();
    store.note_placement(id, Rect::new(0.0, 0.0, 320.0, 820.0));
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    let video_group = {
        let tree = h.state.workspace_tree.read();
        shell::shell::tab_binding::group_id_for_surface(&tree, id).expect("a group")
    };

    // Beside them, in the same strip.
    h.focus(id);
    toplevel.set_fullscreen(None);
    h.pump();
    let entered_beside = h.state.fullscreen_group;
    {
        let mut tree = h.state.workspace_tree.write();
        shell::shell::tab_binding::place_tab(
            &mut tree,
            stub_tab(9_100_011, "Notes"),
            shell::shell::tab_binding::Placement::NewGroup,
        );
    }
    let super_f_left_beside = h.state.leave_fullscreen_when_focus_moved(Some(video_group));
    h.pump();
    let here = (
        h.state.fullscreen_group,
        h.last_states("org.tilo.Video"),
        focused_tab(&h.state),
    );

    // On another strip: the window opens beside one the person was in
    // there (a search result after focus moved), and the view goes with it.
    let elsewhere = {
        let mut tree = h.state.workspace_tree.write();
        let mut strip =
            shell::shell::state::Strip::new(shell::shell::state::StripId(9_100_020), "Elsewhere");
        strip.window_groups.push(shell::shell::state::WindowGroup {
            id: 9_100_021,
            tabs: vec![stub_tab(9_100_022, "Files")],
            active_tab: 9_100_022,
            width: shell::shell::state::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let id = strip.id;
        tree.desktop
            .active_workspace_mut()
            .expect("an active workspace")
            .strips
            .push(strip);
        id
    };
    h.focus(id);
    toplevel.set_fullscreen(None);
    h.pump();
    let entered_elsewhere = h.state.fullscreen_group;
    {
        let mut tree = h.state.workspace_tree.write();
        shell::shell::tab_binding::place_tab(
            &mut tree,
            stub_tab(9_100_023, "Report"),
            shell::shell::tab_binding::Placement::RightOf(9_100_021),
        );
    }
    let super_f_left_elsewhere = h.state.leave_fullscreen_when_focus_moved(Some(video_group));
    h.pump();
    let there = (
        h.state.fullscreen_group,
        h.last_states("org.tilo.Video"),
        focused_tab(&h.state),
        {
            let tree = h.state.workspace_tree.read();
            tree.desktop.active_strip.get(&tree.desktop.active).copied()
        },
    );
    drop(h);
    store.forget(id);

    assert_eq!(entered_beside, Some(video_group), "fixture: full screen");
    assert_eq!(here.0, None, "the launch ends the client's full screen");
    assert!(
        !here.1.contains(&FULLSCREEN),
        "the client is told: {:?}",
        here.1
    );
    assert!(super_f_left_beside, "and Super+F's");
    assert_eq!(here.2, Some(9_100_011), "the new window is the focused one");

    assert_eq!(
        entered_elsewhere,
        Some(video_group),
        "fixture: full screen again"
    );
    assert_eq!(there.0, None, "a launch onto another strip ends it too");
    assert!(
        !there.1.contains(&FULLSCREEN),
        "the client is told: {:?}",
        there.1
    );
    assert!(super_f_left_elsewhere, "and Super+F's");
    assert_eq!(there.3, Some(elsewhere), "the view goes to the other strip");
    assert_eq!(
        there.2,
        Some(9_100_023),
        "the new window is the focused one"
    );
}

/// A client that answers pings only while told to: an app that hangs, then
/// recovers.
#[derive(Default)]
struct Hanging {
    globals: Vec<(u32, String, u32)>,
    answering: bool,
    unanswered: Option<u32>,
}

crate::support::wayland::test_client!(Hanging: registry, configure);
delegate_noop!(Hanging: ignore wl_compositor::WlCompositor);
delegate_noop!(Hanging: ignore wl_surface::WlSurface);
delegate_noop!(Hanging: ignore xdg_toplevel::XdgToplevel);

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Hanging {
    fn event(
        state: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            if state.answering {
                base.pong(serial);
            } else {
                state.unanswered = Some(serial);
            }
        }
    }
}

/// Input reaches an app that has stopped answering: once its ping has gone
/// unanswered for the allowed time, its window shows "isn't responding" (and
/// takes the pointer for its card); Wait hides that for another period; the
/// app answering clears it by itself.
#[test]
fn an_app_that_stops_answering_shows_not_responding_until_it_answers() {
    use shell::compositor::app_liveness::{self, Request, UNANSWERED};
    let mut h: wayland::Harness<Hanging> = wayland::Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Busy".into());
    surface.commit();
    h.pump();
    let id = h.foreign_id("org.example.Busy");
    let _ = app_liveness::publish_hung(std::collections::HashSet::new());

    // A click reaches it; it never answers.
    h.state.note_app_input(id);
    h.pump();
    let pinged = h.client.unanswered;
    let start = std::time::Instant::now();
    let within = h.state.advance_app_liveness_at(start);
    let hung_early = app_liveness::is_hung(id);
    let past = start + UNANSWERED + std::time::Duration::from_millis(10);
    let turned = h.state.advance_app_liveness_at(past);
    let hung = (
        app_liveness::is_hung(id),
        shell::shell::foreign::shows_window_card(id),
    );

    // Wait: hidden for another full period.
    app_liveness::request(Request::Wait(id));
    let _ = h.state.advance_app_liveness_at(past);
    let after_wait = app_liveness::is_hung(id);
    let again = past + UNANSWERED + std::time::Duration::from_millis(10);
    let _ = h.state.advance_app_liveness_at(again);
    let hung_again = app_liveness::is_hung(id);

    // It answers.
    let serial = h.client.unanswered.take().expect("a ping is out");
    wm_base.pong(serial);
    h.pump();
    let cleared = h.state.advance_app_liveness_at(again);
    let recovered = app_liveness::is_hung(id);
    drop(h);
    shell::compositor::foreign::store().forget(id);

    assert!(pinged.is_some(), "input to the app pings it");
    assert!(!within && !hung_early, "an app gets its time to answer");
    assert!(turned, "the tiles hear of it");
    assert_eq!(hung, (true, true), "not responding, with its card");
    assert!(!after_wait, "Wait hides it");
    assert!(
        hung_again,
        "and it shows again if the app still does not answer"
    );
    assert!(cleared && !recovered, "the answer clears it by itself");
}

/// What one compositor tick shows of a window in full screen, placed as the
/// native loop places it every frame.
#[derive(Debug)]
struct FullScreenTick {
    /// The window drawn as the overlay above everything, and where.
    overlay: Option<(u64, Rect)>,
    /// The group laid out full screen in the desktop beneath the shell.
    beneath_shell: Option<u64>,
    /// Where the desktop draws the window's tile.
    tile: Option<Rect>,
    /// The full top bar stands aside.
    top_bar_aside: bool,
    /// The surface handed straight to the display, if any.
    scanout: Option<u64>,
    /// Which surface holds the keyboard.
    keyboard: Option<u64>,
    /// The client is still in its own full screen.
    still_full_screen: bool,
    palette: bool,
}

impl Harness {
    /// One tick: full screen placed (`FrameBuilder::set_client_fullscreen_group`,
    /// then the overlay as `reconcile_fullscreen_overlay` places it), a frame
    /// painted, and the keyboard reconciled.
    fn full_screen_tick(&mut self, surface: u64, output: Rect) -> FullScreenTick {
        let group = self.wl.state.client_fullscreen_group();
        self.frame.set_client_fullscreen_group(group);
        self.frame.set_client_fullscreen_surface(
            group.map(|_| (surface, "Fullscreen".to_owned(), output)),
        );
        let mut scanout = None;
        for _ in 0..2 {
            self.frame.relayout();
            scanout = self
                .frame
                .paint_with_damage()
                .scanout_candidate()
                .map(|candidate| candidate.surface);
        }
        shell::compositor::client_input::sync_keyboard_focus(&mut self.wl.state, &self.frame);
        self.pump();
        let tile = self.frame.foreign_surface_rect(surface);
        FullScreenTick {
            overlay: self.frame.fullscreen_surface_rect(),
            beneath_shell: self.frame.chrome().client_fullscreen_group(),
            tile,
            top_bar_aside: self.frame.chrome().fullscreen_covers_primary(),
            scanout,
            keyboard: self.wl.state.client_input.keyboard_focus(),
            still_full_screen: self.wl.state.client_fullscreen_group().is_some(),
            palette: self.frame.host().palette_visible(),
        }
    }
}

impl Harness {
    /// Press and release `key` on the shell, as the seat delivers it.
    fn press_and_release(
        &mut self,
        doc: &mut automerge::AutoCommit,
        key: winit::keyboard::NamedKey,
    ) {
        let mut focus = None;
        for state in [
            winit::event::ElementState::Pressed,
            winit::event::ElementState::Released,
        ] {
            let event = winit::event::WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: ui::synthetic_input::key_event(
                    winit::keyboard::Key::Named(key),
                    None,
                    state,
                ),
                is_synthetic: false,
            };
            self.frame.dispatch_event(
                &event,
                &winit::event::Modifiers::default(),
                &mut ui::widget::Edits::lend(doc),
                &automerge::ROOT,
                &mut focus,
            );
        }
    }
}

/// A window in full screen, then search (Super+Space): the palette shows
/// over it on its display, the window stays full screen and the top bar
/// stays hidden; nothing of the window is drawn above the palette or handed
/// straight to the display. Esc closes the palette, and the keyboard goes
/// back to the window, drawn over everything and scanned out again.
#[test]
fn search_shows_over_a_full_screen_window_and_esc_gives_it_the_keyboard_back() {
    search_over_a_full_screen_window(false);
}

/// The same with the window also put full screen by the person (Super+F)
/// before the app inside it went full screen, as a video page does: search
/// still shows over it rather than opening unseen beneath the window.
#[test]
fn search_shows_over_a_full_screen_page_in_a_super_f_window() {
    search_over_a_full_screen_window(true);
}

fn search_over_a_full_screen_window(super_f: bool) {
    const FULLSCREEN: u32 = 2;
    const SIZE: (u32, u32) = (1600, 900);
    let output = Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1));
    let mut h = Harness::new();
    h.state.output_size = (1600, 900);
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, "org.tilo.Movie");
    toplevel.set_app_id("org.tilo.Movie".into());
    surface.commit();
    h.pump();
    let id = h.foreign_id("org.tilo.Movie");
    let store = shell::compositor::foreign::store();
    store.note_placement(id, Rect::new(0.0, 0.0, 800.0, 820.0));
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    // Its full-screen frame: opaque and the size of the display.
    let pixels = [10, 20, 30, 255].repeat(SIZE.0.saturating_mul(SIZE.1) as usize);
    store.publish_rgba(id, &pixels, SIZE.0, SIZE.1);
    h.focus(id);
    if super_f {
        // Super+F parks the focused group.
        let group = {
            let tree = h.state.workspace_tree.read();
            let desktop = &tree.desktop;
            let strip = *desktop.active_strip.get(&desktop.active).expect("a strip");
            let group = desktop.active_group_for(strip);
            drop(tree);
            group
        };
        assert!(group.is_some(), "fixture: the window's group is focused");
        h.frame
            .with_chrome(|chrome| chrome.set_user_fullscreen_group(group));
    }
    toplevel.set_fullscreen(None);
    h.pump();
    // The extent `reconcile_fullscreen_overlay` gives the full-screen client.
    store.note_fullscreen_extent(id, (1600, 900));
    let group = h.state.client_fullscreen_group();
    let before = h.full_screen_tick(id, output);

    // Super+Space.
    let mut doc = automerge::AutoCommit::new();
    h.frame
        .host_mut()
        .open_global_search(&doc, &automerge::ROOT);
    let searching = h.full_screen_tick(id, output);
    let states_searching = h.last_states("org.tilo.Movie");

    // Esc.
    h.press_and_release(&mut doc, winit::keyboard::NamedKey::Escape);
    let after = h.full_screen_tick(id, output);
    let states_after = h.last_states("org.tilo.Movie");
    drop(h);
    store.forget(id);

    assert!(group.is_some(), "fixture: the window is full screen");
    assert_eq!(before.overlay, Some((id, output)), "{before:?}");
    assert_eq!(before.beneath_shell, None, "{before:?}");
    assert_eq!(before.scanout, Some(id), "alone on top: {before:?}");
    assert_eq!(before.keyboard, Some(id), "{before:?}");

    assert!(searching.palette, "Super+Space opens search: {searching:?}");
    assert_eq!(
        searching.overlay, None,
        "nothing of the window is drawn above the palette: {searching:?}"
    );
    assert_eq!(searching.beneath_shell, group, "{searching:?}");
    let tile = searching.tile.expect("the desktop draws the window");
    assert!(
        tile.x0 <= output.x0
            && tile.y0 <= output.y0
            && tile.x1 >= output.x1
            && tile.y1 >= output.y1,
        "it still fills its display beneath the palette: {tile:?}"
    );
    assert!(
        searching.top_bar_aside,
        "the top bar stays hidden: {searching:?}"
    );
    assert_eq!(
        searching.scanout, None,
        "the palette is composited over it: {searching:?}"
    );
    assert_eq!(
        searching.keyboard, None,
        "search has the keyboard: {searching:?}"
    );
    assert!(searching.still_full_screen, "{searching:?}");
    assert!(
        states_searching.contains(&FULLSCREEN),
        "the window is never told it left full screen: {states_searching:?}"
    );

    assert!(!after.palette, "Esc closes search: {after:?}");
    assert_eq!(after.overlay, Some((id, output)), "{after:?}");
    assert_eq!(after.beneath_shell, None, "{after:?}");
    assert_eq!(after.scanout, Some(id), "scanned out again: {after:?}");
    assert_eq!(after.keyboard, Some(id), "the keyboard is back: {after:?}");
    assert!(after.still_full_screen, "{after:?}");
    assert!(states_after.contains(&FULLSCREEN), "{states_after:?}");
}
