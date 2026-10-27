//! A maximized window stays maximized until the person restores it.
//!
//! A terminal running an agent retitles itself many times a second, resizes
//! its buffer, asks to be activated, and the programs inside it open
//! short-lived windows of their own (`wl-copy` maps a one-pixel window to own
//! the clipboard, then destroys it). None of that is the person restoring the
//! window, so none of it may.
//!
//! Real Wayland clients drive the production compositor state, and the
//! production frame builder lays out and paints every frame, as the native
//! loop does. The window is maximized with the person's own shortcut.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the harness owns the compositor and frame builder for the whole scenario by design"
)]

use std::ops::{Deref, DerefMut};
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};

use automerge::{AutoCommit, ROOT};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{GroupId, TileContent};

use crate::support::wayland;

/// The largest buffer an app commits; the shared pool holds one of these.
const MAX_BUFFER: i32 = 256;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    tokens: Vec<String>,
}

wayland::test_client!(Client);

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

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore wl_keyboard::WlKeyboard);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore xdg_activation_v1::XdgActivationV1);

struct Harness {
    wl: wayland::Harness<Client>,
    fb: FrameBuilder,
    doc: AutoCommit,
    focus: Option<taffy::NodeId>,
    compositor: wl_compositor::WlCompositor,
    wm: xdg_wm_base::XdgWmBase,
    activation: xdg_activation_v1::XdgActivationV1,
    pool: wl_shm_pool::WlShmPool,
    _pool_file: std::fs::File,
    surface: wl_surface::WlSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    id: u64,
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

const TERMINAL: &str = "Alacritty";

impl Harness {
    /// One mapped terminal window, focused.
    fn with_terminal() -> Self {
        let wl = wayland::Harness::new();
        let fb = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1280, 800));
        let qh = wl.queue.handle();
        let seat = wl.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _keyboard = seat.get_keyboard(&qh, ());
        let compositor = wl.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = wl.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
        let activation = wl.bind::<xdg_activation_v1::XdgActivationV1>("xdg_activation_v1", 1);
        let shm = wl.bind::<wl_shm::WlShm>("wl_shm", 1);
        let len = MAX_BUFFER.saturating_mul(MAX_BUFFER).saturating_mul(4);
        // SAFETY: a null-terminated name and a valid flag word are the whole
        // contract of `memfd_create`.
        let raw = unsafe { libc::memfd_create(c"tilo-maximized-stays".as_ptr(), 0) };
        assert!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `memfd_create` just handed us this descriptor.
        let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        file.set_len(u64::try_from(len).unwrap()).unwrap();
        let pool = shm.create_pool(file.as_fd(), len, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(TERMINAL.into());
        toplevel.set_title("Alacritty".into());
        surface.commit();
        let mut wl = wl;
        wl.pump();
        let id = wl.foreign_id(TERMINAL);
        let mut h = Self {
            wl,
            fb,
            doc: AutoCommit::new(),
            focus: None,
            compositor,
            wm,
            activation,
            pool,
            _pool_file: file,
            surface,
            toplevel,
            id,
        };
        h.commit_buffer(&h.surface.clone(), 64);
        h.frames(10);
        assert_eq!(
            h.fb.chrome()
                .tree()
                .desktop
                .active_workspace()
                .and_then(|ws| {
                    let strip =
                        h.fb.chrome()
                            .tree()
                            .desktop
                            .active_strip
                            .get(&ws.id)
                            .copied()?;
                    h.fb.chrome().tree().desktop.active_group_for(strip)
                }),
            Some(h.group()),
            "precondition: the terminal is the focused window"
        );
        h
    }

    fn commit_buffer(&mut self, surface: &wl_surface::WlSurface, side: i32) {
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
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, side, side);
        surface.commit();
        self.pump();
    }

    /// One frame the way the native loop runs it: clients, apps, layout,
    /// paint.
    fn frame(&mut self) {
        self.wl.pump();
        self.fb.pump_apps(&mut self.doc, &ROOT);
        let _ = self.fb.paint_settled();
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    fn group(&self) -> GroupId {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::compositor::tab_binding::group_id_for_surface(&tree, self.id)
            .expect("the terminal has a group")
    }

    /// Whether the terminal's group is maximized: in the desktop's tree, on
    /// screen, and as the window expanded over the display.
    fn maximized(&mut self) -> (bool, bool, bool) {
        let group = self.group();
        let in_tree = {
            let chrome = self.fb.chrome();
            let tree = chrome.tree();
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|ws| ws.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .find(|g| g.id == group)
                .is_some_and(|g| g.maximized)
        };
        let strip = {
            let chrome = self.fb.chrome();
            let tree = chrome.tree();
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|ws| ws.strips.iter())
                .find(|s| s.window_groups.iter().any(|g| g.id == group))
                .map(|s| s.id)
                .expect("the terminal is on a strip")
        };
        let shown = self
            .fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .and_then(|row| row.groups.iter().find(|g| g.id == group))
            .is_some_and(|g| g.maximized);
        let expanded = self.fb.chrome().maximized_group() == Some(group);
        (in_tree, shown, expanded)
    }

    /// Local input in production order: the seat first, the shell with what
    /// it leaves.
    fn input(&mut self, event: &WindowEvent, modifiers: Modifiers) {
        let to_client = shell::compositor::client_input::route_input(
            &mut self.wl.state,
            &mut self.fb,
            event,
            &modifiers,
        );
        if !to_client {
            self.fb.dispatch_event(
                event,
                &modifiers,
                &mut ui::widget::Edits::lend(&mut self.doc),
                &ROOT,
                &mut self.focus,
            );
        }
        self.frame();
    }

    fn person_presses_super(&mut self, key: NamedKey) {
        let super_key = Modifiers::from(ModifiersState::SUPER);
        for state in [ElementState::Pressed, ElementState::Released] {
            let event = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
                Key::Named(key),
                None,
                state,
            ));
            self.input(&event, super_key);
        }
        self.frames(10);
    }

    /// The person presses Super+Up: their own maximize.
    fn person_maximizes(&mut self) {
        self.person_presses_super(NamedKey::ArrowUp);
        assert_eq!(
            self.maximized(),
            (true, true, true),
            "precondition: Super+Up maximizes the focused terminal"
        );
    }

    fn assert_still_maximized(&mut self, after: &str) {
        self.frames(10);
        let (in_tree, shown, expanded) = self.maximized();
        assert!(
            in_tree && shown && expanded,
            "the person maximized the terminal and never restored it, but after {after} \
             it is no longer maximized (desktop tree: {in_tree}, on screen: {shown}, \
             expanded over the display: {expanded})"
        );
    }

    /// How far the terminal's strip is panned.
    fn scroll_x(&mut self) -> f64 {
        let group = self.group();
        let strip = {
            let chrome = self.fb.chrome();
            let tree = chrome.tree();
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|ws| ws.strips.iter())
                .find(|s| s.window_groups.iter().any(|g| g.id == group))
                .map(|s| s.id)
                .expect("the terminal is on a strip")
        };
        self.fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .expect("the strip has a row")
            .scroll_x()
    }

    /// Three fingers sideways over the maximized window, released past the
    /// end of the strip, as the touchpad reports it.
    fn person_pans_the_strip(&mut self) {
        let start = std::time::Instant::now();
        for (ms, travel) in [
            (16, 40.0),
            (32, 120.0),
            (48, 260.0),
            (64, 420.0),
            (80, 300.0),
            (96, -150.0),
        ] {
            let at = start
                .checked_add(std::time::Duration::from_millis(ms))
                .unwrap();
            let _ = self
                .fb
                .host_mut()
                .update_active_strip_pan(travel, ms == 16, at);
            self.frame();
            let scroll = self.scroll_x();
            assert!(
                scroll.abs() <= 0.5,
                "three fingers slid the maximized terminal {scroll} px aside"
            );
        }
        let _ = self.fb.host_mut().end_active_strip_pan(
            start
                .checked_add(std::time::Duration::from_millis(120))
                .unwrap(),
            false,
        );
        self.frames(60);
    }

    /// The terminal's own traffic: a retitle on every repaint of the agent's
    /// status line, and a resized buffer now and then.
    fn terminal_retitles_and_redraws(&mut self, rounds: usize) {
        for round in 0..rounds {
            self.toplevel
                .set_title(format!("✳ Claude Code — working ({round})"));
            if round.checked_rem(10) == Some(0) {
                let side = i32::try_from(round.checked_rem(150).unwrap_or(0))
                    .unwrap()
                    .saturating_add(64);
                self.commit_buffer(&self.surface.clone(), side);
            } else {
                self.surface.commit();
                self.pump();
            }
            self.frame();
        }
    }

    /// The terminal asks to be activated with no input behind the request.
    fn terminal_asks_for_activation(&mut self) {
        let qh = self.queue.handle();
        let token = self.activation.get_activation_token(&qh, ());
        token.set_surface(&self.surface);
        token.set_app_id(TERMINAL.into());
        token.commit();
        self.pump();
        if let Some(token) = self.wl.client.tokens.pop() {
            self.activation.activate(token, &self.surface);
            self.pump();
        }
    }

    /// What `wl-copy` does where it has no clipboard-manager protocol: map a
    /// one-pixel window of its own, take the selection once the window has
    /// the keyboard, and destroy the window.
    fn a_program_in_the_terminal_copies(&mut self) {
        let qh = self.queue.handle();
        let surface = self.compositor.create_surface(&qh, ());
        let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title("wl-clipboard".into());
        toplevel.set_app_id("io.github.bugaevc.wl-clipboard".into());
        surface.commit();
        self.pump();
        self.commit_buffer(&surface, 1);
        self.frames(3);
        toplevel.destroy();
        xdg.destroy();
        surface.destroy();
        self.pump();
    }

    /// The terminal opens a dialog of its own and closes it again.
    fn terminal_opens_a_dialog(&mut self) {
        let qh = self.queue.handle();
        let surface = self.compositor.create_surface(&qh, ());
        let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
        let dialog = xdg.get_toplevel(&qh, ());
        dialog.set_parent(Some(&self.toplevel));
        dialog.set_app_id(TERMINAL.into());
        dialog.set_title("Dialog".into());
        surface.commit();
        self.pump();
        self.frames(3);
        dialog.destroy();
        xdg.destroy();
        surface.destroy();
        self.pump();
    }

    /// A program starts a window and gives up before it ever shows a frame.
    fn a_window_opens_and_never_shows(&mut self) {
        let qh = self.queue.handle();
        let surface = self.compositor.create_surface(&qh, ());
        let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("org.example.GaveUp".into());
        surface.commit();
        self.pump();
        self.frames(3);
        toplevel.destroy();
        xdg.destroy();
        surface.destroy();
        self.pump();
    }
}

#[test]
fn a_maximized_window_stays_maximized_until_the_person_restores_it() {
    let mut h = Harness::with_terminal();
    h.person_maximizes();

    h.person_pans_the_strip();
    h.assert_still_maximized("the person swiped sideways with three fingers");
    assert!(
        h.scroll_x().abs() <= 0.5,
        "the strip came to rest away from home under a maximized window"
    );

    h.terminal_retitles_and_redraws(200);
    h.assert_still_maximized("the terminal retitled and redrew itself 200 times");

    h.terminal_asks_for_activation();
    h.assert_still_maximized("the terminal asked to be activated");

    for copy in 0..3 {
        h.a_program_in_the_terminal_copies();
        h.assert_still_maximized(&format!(
            "a program in the terminal copied with wl-copy ({copy})"
        ));
    }

    h.terminal_opens_a_dialog();
    h.assert_still_maximized("the terminal opened a dialog and closed it");

    h.a_window_opens_and_never_shows();
    h.assert_still_maximized("a window opened and went away before it showed");

    let still_one_window = {
        let chrome = h.fb.chrome();
        let tree = chrome.tree();
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|ws| ws.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .filter(|tab| matches!(tab.tile, TileContent::Foreign { .. }))
            .count()
    };
    assert_eq!(still_one_window, 1, "the copy's window is gone again");

    // Until the person restores it: then it stays restored, whatever opens
    // and closes after.
    h.person_presses_super(NamedKey::ArrowDown);
    assert_eq!(
        h.maximized(),
        (false, false, false),
        "Super+Down restores the terminal"
    );
    h.a_program_in_the_terminal_copies();
    h.frames(10);
    assert_eq!(
        h.maximized(),
        (false, false, false),
        "a window coming and going never maximizes what the person restored"
    );
}
