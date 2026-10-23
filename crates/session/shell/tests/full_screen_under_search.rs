//! Search over a window in full screen: the palette shows above it on its
//! display, the window stays in full screen at its size, the keyboard is
//! the palette's, and closing the palette gives the window back the
//! keyboard and the display. Reads a real socket, since what the client is
//! told goes on the wire.

use std::sync::{Arc, Mutex};

use shell::compositor::backend::{Output, OutputId};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::fullscreen_presentation;
use shell::compositor::output_globals::OutputGlobals;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::support::wayland;

const FULLSCREEN: u32 = 2;
const OUTPUT: (u32, u32) = (1600, 900);

/// Every configure the toplevel received: size and states.
type Configures = Arc<Mutex<Vec<(i32, i32, Vec<u32>)>>>;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configures: Configures,
}

crate::support::wayland::test_client!(Client);
delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
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
                .push((width, height, states));
        }
    }
}

struct Desk {
    wl: wayland::Harness<Client>,
    frame: FrameBuilder,
    outputs: Vec<Output>,
    globals: OutputGlobals,
}

impl Desk {
    fn new() -> Self {
        let mut wl = wayland::Harness::new();
        wl.state.output_size = (1600, 900);
        let frame = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1600, 900));
        Self {
            wl,
            frame,
            outputs: vec![Output::new(OutputId(0), OUTPUT, 1.0)],
            globals: OutputGlobals::new(),
        }
    }

    /// One turn of the compositor's frame loop, in its order: where the
    /// full-screen client is drawn, the frame, the configures it implies,
    /// and who has the keyboard.
    fn frame(&mut self) {
        fullscreen_presentation::lay_out_beneath_shell(&self.wl.state, &mut self.frame);
        let _ = self.frame.paint_settled();
        let _ = fullscreen_presentation::reconcile_overlay(
            &mut self.wl.state,
            &mut self.frame,
            &self.globals,
            &self.outputs,
        );
        let _ = self.frame.paint_settled();
        shell::compositor::foreign::flush_configures(&mut self.wl.state);
        shell::compositor::client_input::sync_keyboard_focus(&mut self.wl.state, &self.frame);
        self.wl.pump();
    }

    fn configures(&self) -> Vec<(i32, i32, Vec<u32>)> {
        self.wl
            .client
            .configures
            .lock()
            .expect("configures")
            .clone()
    }

    fn keyboard(&self) -> Option<u64> {
        self.wl.state.client_input.keyboard_focus()
    }
}

/// What the person sees and what the client was told, at one moment.
#[derive(Debug)]
struct Moment {
    /// The client drawn as the overlay above everything (and so possibly
    /// scanned out straight to the display).
    overlay: Option<u64>,
    /// The group laid out full screen in the desktop, beneath the shell.
    beneath_shell: Option<u64>,
    /// The client's full screen as the compositor holds it.
    client_full_screen: Option<u64>,
    /// Where the window's tile is when it is laid out in the desktop.
    tile: Option<Rect>,
    keyboard: Option<u64>,
    palette: bool,
}

fn moment(desk: &Desk, group: u64) -> Moment {
    Moment {
        overlay: desk.frame.fullscreen_surface_rect().map(|(id, _)| id),
        beneath_shell: desk.frame.fullscreen_group(),
        client_full_screen: desk.wl.state.fullscreen_group,
        tile: desk.frame.host().active_tile_rect(group),
        keyboard: desk.keyboard(),
        palette: desk.frame.host().palette_visible(),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered scenario keeps the person's steps and what each changed visible"
)]
fn search_over_a_full_screen_window_shows_above_it_and_gives_it_back() {
    let mut desk = Desk::new();
    let compositor: wl_compositor::WlCompositor = desk.wl.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = desk.wl.bind("xdg_wm_base", 2);
    desk.wl.pump();

    let qh = desk.wl.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg: xdg_surface::XdgSurface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.tilo.Video".into());
    surface.commit();
    desk.wl.pump();
    let id = desk.wl.foreign_id("org.tilo.Video");
    let store = shell::compositor::foreign::store();
    // Its first frame: a window takes the keyboard once it has drawn.
    store.publish_rgba(id, &[0; 4], 1, 1);
    let group = {
        let tree = desk.wl.state.workspace_tree.read();
        shell::shell::tab_binding::group_id_for_surface(&tree, id).expect("a group")
    };
    desk.frame.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            let workspace = tree.desktop.active;
            let strip = tree
                .desktop
                .workspaces
                .iter()
                .flat_map(|ws| ws.strips.iter())
                .find(|strip| strip.window_groups.iter().any(|g| g.id == group))
                .map(|strip| strip.id)
                .expect("the window has a strip");
            tree.desktop.active_strip.insert(workspace, strip);
            tree.desktop.set_active_group(strip, group);
        }
        chrome.bump();
    });
    desk.frame();

    toplevel.set_fullscreen(None);
    desk.wl.pump();
    desk.frame();
    let full_screen = moment(&desk, group);
    let told = desk.configures();

    // Search opens over it.
    desk.frame.host_mut().set_palette_visible(true);
    desk.frame.host_mut().force_palette_settled();
    desk.frame();
    desk.frame();
    let searching = moment(&desk, group);
    let told_while_searching = desk.configures();
    let top_bar_while_searching = desk.frame.chrome().is_fullscreen();
    let palette = desk.frame.host().palette_rect_for_tests();

    // Escape: the palette goes, nothing was opened.
    desk.frame.host_mut().set_palette_visible(false);
    desk.frame();
    desk.frame();
    let back = moment(&desk, group);
    let told_after = desk.configures();
    drop(desk);
    store.forget(id);

    let output = Rect::new(0.0, 0.0, f64::from(OUTPUT.0), f64::from(OUTPUT.1));
    let (w, h, states) = told.last().cloned().expect("the client was configured");
    assert_eq!(
        (w, h),
        (1600, 900),
        "fixture: full screen covers the display"
    );
    assert!(states.contains(&FULLSCREEN), "fixture: {states:?}");
    assert_eq!(
        full_screen.client_full_screen,
        Some(group),
        "{full_screen:?}"
    );
    assert_eq!(
        full_screen.overlay,
        Some(id),
        "in full screen the window is drawn above the desktop: {full_screen:?}"
    );
    assert_eq!(full_screen.keyboard, Some(id), "{full_screen:?}");

    assert!(searching.palette, "{searching:?}");
    assert_eq!(
        searching.overlay, None,
        "nothing of the window is drawn above the palette, nor scanned out past it: {searching:?}"
    );
    assert_eq!(
        searching.beneath_shell,
        Some(group),
        "the window is laid out full screen beneath the palette: {searching:?}"
    );
    assert_eq!(
        searching.tile,
        Some(output),
        "beneath the palette the window still covers the whole display: {searching:?}"
    );
    assert!(
        top_bar_while_searching,
        "the desktop's top bar stays away while searching over a full screen"
    );
    assert!(
        palette.width() > 0.0 && output.contains(palette.center()),
        "the palette is on the window's display: {palette:?}"
    );
    assert_eq!(
        searching.client_full_screen,
        Some(group),
        "searching does not end the full screen: {searching:?}"
    );
    assert_ne!(
        searching.keyboard,
        Some(id),
        "the keyboard is the palette's while it is open: {searching:?}"
    );
    for (w, h, states) in told_while_searching.iter().skip(told.len()) {
        assert!(
            states.contains(&FULLSCREEN) && (*w, *h) == (1600, 900),
            "the window is told nothing but its full screen while search is open: \
             {w}x{h} {states:?}"
        );
    }

    assert!(!back.palette, "{back:?}");
    assert_eq!(
        back.client_full_screen,
        Some(group),
        "closing search keeps the full screen: {back:?}"
    );
    assert_eq!(
        back.overlay,
        Some(id),
        "the window is drawn above the desktop again: {back:?}"
    );
    assert_eq!(back.beneath_shell, None, "{back:?}");
    assert_eq!(
        back.keyboard,
        Some(id),
        "the full-screen window has the keyboard back: {back:?}"
    );
    for (w, h, states) in told_after.iter().skip(told.len()) {
        assert!(
            states.contains(&FULLSCREEN) && (*w, *h) == (1600, 900),
            "the window stays in full screen at its size: {w}x{h} {states:?}"
        );
    }
}
