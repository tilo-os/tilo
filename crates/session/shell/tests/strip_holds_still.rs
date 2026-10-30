//! The strip moves only when the person moves it.
//!
//! Apps keep talking to the compositor while nobody touches anything: a
//! terminal retitles with every command, a browser resizes its buffer, a
//! window changes its size limits, an app asks to be activated, opens a
//! dialog and closes it. None of that is the person asking to look somewhere
//! else, so none of it may pan the strip: the viewport stays where the last
//! deliberate act left it, whether that was stepping focus to a window or a
//! free three-finger pan that left two windows cut by the edges. And a click
//! into a window that is partly on screen focuses it where it is, without
//! sliding it out from under the pointer.
//!
//! Real Wayland clients drive the production compositor state, and the
//! production frame builder lays out and paints every frame, as the native
//! loop does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};

use vello::kurbo::{Point, Rect};
use wayland_client::protocol::{
    wl_compositor, wl_pointer, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use shell::shell::state::{GroupId, StripId, WindowWidth};
use ui::widget::Widget as _;

use crate::support::wayland::{self, Toplevel, create_toplevel};

/// Each window's share of the strip: three overflow the 1280 px display, so
/// there is always something off screen to pan to.
const WIDTH_PERMILLE: u32 = 600;

/// The largest buffer an app commits; the shared pool holds one of these.
const MAX_BUFFER: i32 = 256;

/// Rounds of unprompted app activity in each idle run.
const IDLE_ROUNDS: usize = 40;

/// How long nothing may move before the strip counts as at rest: longer than
/// an eased pan (idle gate plus curve) and a width spring take.
const QUIET: Duration = Duration::from_millis(800);

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    tokens: Vec<String>,
}

wayland::test_client!(Client: registry, ping, configure, surface, toplevel, shm, buffer, seat);

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.tokens.push(token);
        }
    }
}

delegate_noop!(Client: ignore wl_pointer::WlPointer);
delegate_noop!(Client: ignore xdg_activation_v1::XdgActivationV1);

/// One app window: its surface, role objects and compositor-side id.
struct Window {
    app_id: String,
    surface: wl_surface::WlSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    id: u64,
}

struct Harness {
    f: wayland::Framed<Client>,
    compositor: wl_compositor::WlCompositor,
    wm: xdg_wm_base::XdgWmBase,
    activation: xdg_activation_v1::XdgActivationV1,
    pool: wl_shm_pool::WlShmPool,
    _pool_file: std::fs::File,
    windows: Vec<Window>,
    strip: StripId,
}

impl Deref for Harness {
    type Target = wayland::Framed<Client>;
    fn deref(&self) -> &Self::Target {
        &self.f
    }
}

impl DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.f
    }
}

impl Harness {
    /// Four mapped app windows side by side in one strip, wider together
    /// than the display, with a pointer bound.
    fn with_windows() -> Self {
        let f = wayland::Framed::<Client>::new();
        let qh = f.queue.handle();
        let seat = f.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _pointer = seat.get_pointer(&qh, ());
        let compositor = f.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = f.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
        let activation = f.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
        let shm = f.bind::<wl_shm::WlShm>("wl_shm", 1);
        let len = MAX_BUFFER.saturating_mul(MAX_BUFFER).saturating_mul(4);
        let (pool, file) = wayland::shm_pool(&shm, &qh, &vec![0; usize::try_from(len).unwrap()]);
        let mut h = Self {
            f,
            compositor,
            wm,
            activation,
            pool,
            _pool_file: file,
            windows: Vec::new(),
            strip: StripId(0),
        };
        for n in 0..4 {
            let app_id = format!("org.example.Still{n}");
            let Toplevel {
                surface, toplevel, ..
            } = create_toplevel(&h.compositor, &h.wm, &qh, &app_id);
            toplevel.set_title(format!("Window {n}"));
            surface.commit();
            h.pump();
            let id = h.foreign_id(&app_id);
            h.windows.push(Window {
                app_id,
                surface,
                toplevel,
                id,
            });
            h.commit_buffer(n, 64);
            h.settle();
        }
        let ids: Vec<u64> = h.windows.iter().map(|w| w.id).collect();
        let strip_id = {
            let handle = h.state.workspace_tree.clone();
            let mut tree = handle.write();
            let strip = tree
                .desktop
                .workspaces
                .iter_mut()
                .flat_map(|ws| ws.strips.iter_mut())
                .find(|strip| {
                    strip.window_groups.iter().any(|g| {
                        g.tabs.iter().any(|t| {
                            matches!(t.tile, shell::shell::state::TileContent::Foreign { surface_id, .. } if ids.contains(&surface_id))
                        })
                    })
                })
                .expect("the windows were adopted into a strip");
            // Only the four app windows, so every group is one of them.
            strip.window_groups.retain(|g| {
                g.tabs.iter().any(|t| {
                    matches!(t.tile, shell::shell::state::TileContent::Foreign { surface_id, .. } if ids.contains(&surface_id))
                })
            });
            for group in &mut strip.window_groups {
                group.width = WindowWidth::Fraction(WIDTH_PERMILLE);
            }
            let id = strip.id;
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
            id
        };
        h.strip = strip_id;
        h.settle();
        assert_eq!(h.groups().len(), 4, "four windows, one group each");
        h
    }

    /// Attach a fresh `side`-px square buffer to window `n` and commit it.
    fn commit_buffer(&mut self, n: usize, side: i32) {
        let qh = self.queue.handle();
        let buffer = self.pool.create_buffer(
            0,
            side,
            side,
            side.saturating_mul(4),
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        let surface = &self.windows[n].surface;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, side, side);
        surface.commit();
        self.pump();
    }

    /// Frames until the strip has stopped moving: no change of pan or of
    /// any window's place for longer than an eased pan or a width spring
    /// takes, so whatever the last act started has landed. (Foreign tiles
    /// waiting on client frames keep the loop's animation demand up, so that
    /// is no signal of rest here.)
    fn settle(&mut self) {
        let deadline = tilo_test_env::hang_deadline();
        let mut last = self.layout_signature();
        let mut quiet_since = Instant::now();
        loop {
            self.frame();
            let now = self.layout_signature();
            if now != last {
                last = now;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= QUIET {
                return;
            }
            assert!(Instant::now() < deadline, "the strip never came to rest");
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// The pan and every window's laid-out rect, rounded to whole pixels.
    fn layout_signature(&mut self) -> Vec<i64> {
        let strip = self.strip;
        let Some(row) = self.fb.host_mut().group_row_mut_for_tests(strip) else {
            return Vec::new();
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "on-screen pixel coordinates are far inside i64"
        )]
        let px = |v: f64| v.round() as i64;
        let mut out = vec![px(row.scroll_x())];
        for g in &row.groups {
            let r = g.get_rect();
            out.extend([px(r.x0), px(r.x1)]);
        }
        out
    }

    fn scroll_x(&mut self) -> f64 {
        let strip = self.strip;
        self.fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .expect("the strip has a row")
            .scroll_x()
    }

    fn groups(&self) -> Vec<GroupId> {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|ws| ws.strips.iter())
            .find(|strip| strip.id == self.strip)
            .map(|strip| strip.window_groups.iter().map(|g| g.id).collect())
            .unwrap_or_default()
    }

    fn group_of(&self, n: usize) -> GroupId {
        let id = self.windows[n].id;
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::shell::tab_binding::group_id_for_surface(&tree, id).expect("the window has a group")
    }

    fn active_group(&self) -> Option<GroupId> {
        self.fb.chrome().tree().desktop.active_group_for(self.strip)
    }

    /// The on-screen rect of window `n`'s tile.
    fn tile(&self, n: usize) -> Rect {
        let id = self.windows[n].id;
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::shell::input_policy::foreign_tiles(&tree, self.fb.host())
            .into_iter()
            .find(|hit| hit.surface_id == id)
            .map_or(Rect::ZERO, |hit| hit.rect)
    }

    fn row_rect(&mut self) -> Rect {
        let strip = self.strip;
        self.fb
            .host_mut()
            .group_row_rect_for_tests(strip)
            .expect("the strip has a row")
    }

    /// Local input in production order: the seat first, the shell with what
    /// it leaves.
    fn input(&mut self, event: &WindowEvent) {
        let modifiers = Modifiers::default();
        self.route(event, modifiers);
        self.frame();
    }

    fn click(&mut self, at: Point) {
        self.input(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(at.x, at.y),
        });
        for state in [ElementState::Pressed, ElementState::Released] {
            self.input(&WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            });
        }
    }

    /// One round of what apps do on their own: every window retitles,
    /// commits a buffer of another size and changes its size limits; the
    /// windows without focus ask to be activated without any input behind
    /// the request; and now and then one opens a dialog and closes it.
    fn apps_act_alone(&mut self, round: usize) {
        let qh = self.queue.handle();
        let focused = self.wl.state.client_input.keyboard_focus();
        for n in 0..self.windows.len() {
            let wobble = round
                .saturating_mul(7)
                .saturating_add(n.saturating_mul(13))
                .checked_rem(200)
                .unwrap_or(0);
            let side = i32::try_from(wobble).unwrap().saturating_add(48);
            {
                let w = &self.windows[n];
                w.toplevel
                    .set_title(format!("{} — round {round}", w.app_id));
                let half = side.checked_div(2).unwrap_or(side);
                w.toplevel.set_min_size(half, half);
                w.toplevel.set_max_size(0, 0);
            }
            self.commit_buffer(n, side);
            let w = &self.windows[n];
            if focused != Some(w.id) {
                let token = self.activation.get_activation_token(&qh, ());
                token.set_surface(&w.surface);
                token.set_app_id(w.app_id.clone());
                token.commit();
                self.pump();
                if let Some(token) = self.wl.client.tokens.pop() {
                    let w = &self.windows[n];
                    self.activation.activate(token, &w.surface);
                    self.pump();
                }
            }
        }
        if round.checked_rem(8) == Some(3) {
            let n = round.checked_rem(self.windows.len()).unwrap_or(0);
            let surface = self.compositor.create_surface(&qh, ());
            let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
            let dialog = xdg.get_toplevel(&qh, ());
            dialog.set_parent(Some(&self.windows[n].toplevel));
            dialog.set_app_id(self.windows[n].app_id.clone());
            dialog.set_title("Dialog".into());
            surface.commit();
            self.pump();
            self.frame();
            dialog.destroy();
            xdg.destroy();
            surface.destroy();
            self.pump();
        }
    }

    /// Run the apps on their own for a while and require the strip to stay
    /// exactly where it is, frame after frame.
    fn assert_holds_still_while_apps_act(&mut self, what: &str) {
        let rest = self.scroll_x();
        let focus = self.active_group();
        for round in 0..IDLE_ROUNDS {
            self.apps_act_alone(round);
            for _ in 0..3 {
                self.frame();
                let now = self.scroll_x();
                assert!(
                    (now - rest).abs() <= 0.5,
                    "{what}: the strip panned from {rest} to {now} in idle round {round} \
                     while only the apps acted"
                );
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        self.settle();
        let now = self.scroll_x();
        assert!(
            (now - rest).abs() <= 0.5,
            "{what}: the strip came to rest at {now}, not where the person left it ({rest})"
        );
        assert_eq!(
            self.active_group(),
            focus,
            "{what}: an app moved the focus without any input"
        );
    }
}

#[test]
fn focus_stepped_to_a_window_stays_framed_while_apps_act() {
    let mut h = Harness::with_windows();
    let groups = h.groups();
    let strip = h.strip;
    let first = groups[0];
    let tab = h.fb.chrome().tree().desktop.workspaces[0]
        .strips
        .iter()
        .find(|s| s.id == strip)
        .and_then(|s| s.window_groups.iter().find(|g| g.id == first))
        .map(|g| g.active_tab)
        .unwrap();
    let _ = h.fb.host_mut().focus_window_on_strip(strip, first, tab);
    h.settle();
    assert!(h.fb.host_mut().step_focus_along_strip(2));
    h.settle();
    assert_eq!(h.active_group(), Some(groups[2]));
    assert!(
        h.scroll_x() > 0.0,
        "precondition: stepping to the third window pans the strip"
    );
    h.assert_holds_still_while_apps_act("after stepping focus");
}

#[test]
fn a_free_three_finger_pan_stays_where_the_fingers_left_it() {
    let mut h = Harness::with_windows();
    // Leave the strip between rest positions, with windows cut on both
    // edges: exactly the framing a geometry-driven correction would
    // "fix".
    let before = h.scroll_x();
    // One report, released at once: the fingers rest, nothing is thrown.
    let at = Instant::now();
    assert!(h.fb.host_mut().update_active_strip_pan(-173.0, true, at));
    let _ = h.fb.host_mut().end_active_strip_pan(at, false);
    h.settle();
    let panned = h.scroll_x();
    assert!(
        (panned - (before - 173.0)).abs() <= 0.5,
        "precondition: the pan stays where the fingers left it ({before} → {panned})"
    );
    h.assert_holds_still_while_apps_act("after a free pan");
}

#[test]
fn clicking_into_a_partly_visible_window_focuses_it_in_place() {
    let mut h = Harness::with_windows();
    let row = h.row_rect();
    let focused = h.active_group();
    let partly = (0..h.windows.len()).find(|&n| {
        let tile = h.tile(n);
        let shown = tile.intersect(row);
        Some(h.group_of(n)) != focused
            && shown.width() > 40.0
            && shown.height() > 40.0
            && (tile.x0 < row.x0 || tile.x1 > row.x1)
    });
    let n = partly.expect("precondition: a window without focus is cut by an edge");
    let shown = h.tile(n).intersect(row);
    let before = h.scroll_x();
    h.click(shown.center());
    h.settle();
    assert_eq!(
        h.active_group(),
        Some(h.group_of(n)),
        "the click focuses the window it landed on"
    );
    let after = h.scroll_x();
    assert!(
        (after - before).abs() <= 0.5,
        "a click into a partly visible window slid the strip from {before} to {after}, \
             dragging the window out from under the pointer"
    );
    h.assert_holds_still_while_apps_act("after a click");
}
