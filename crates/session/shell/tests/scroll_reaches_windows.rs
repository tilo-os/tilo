//! A wheel or a two-finger scroll over a window scrolls that window.
//!
//! The pointer router latches an implicit grab on the first button down and
//! lets it go on the last button up. It counted held buttons, so a release
//! that never reached it (a source lost it) left the count above zero for
//! good: the grab stayed with the shell, no window got pointer focus again,
//! and every wheel and click after that went to the shell instead of the
//! window under the pointer. On the device that is what "I can no longer
//! scroll in any of my terminal windows" was: `state-dump` showed
//! `pointer_grab: chrome`, `pointer_buttons_down: 1` and no pointer focus,
//! with nothing held. Alacritty and Tilo's own terminal are both Wayland
//! clients in their own process, so this one wire covers both.
//!
//! Real Wayland clients drive the production compositor state, and the
//! production frame builder lays out and paints every frame, as the native
//! loop does; input goes to the seat first and the shell gets what it
//! leaves, in production order.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ROOT};
use vello::kurbo::{Point, Rect};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_pointer, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{
    ElementState, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};

use shell::compositor::frame::FrameBuilder;

use crate::support::wayland;

/// The buffer the window commits, in pixels a side.
const BUFFER: i32 = 64;

/// How long the window's tile must keep its place to count as laid out.
const QUIET: Duration = Duration::from_millis(400);

/// What the window heard on its `wl_pointer`.
#[derive(Debug, PartialEq)]
enum Heard {
    Enter,
    Leave,
    Button(wl_pointer::ButtonState),
    Scroll(wl_pointer::Axis),
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    heard: Vec<Heard>,
}

wayland::test_client!(Client);

impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let heard = match event {
            wl_pointer::Event::Enter { .. } => Heard::Enter,
            wl_pointer::Event::Leave { .. } => Heard::Leave,
            wl_pointer::Event::Button {
                state: WEnum::Value(pressed),
                ..
            } => Heard::Button(pressed),
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                ..
            } => Heard::Scroll(axis),
            _ => return,
        };
        state.heard.push(heard);
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

struct Harness {
    wl: wayland::Harness<Client>,
    fb: FrameBuilder,
    doc: AutoCommit,
    focus: Option<taffy::NodeId>,
    id: u64,
    _surface: wl_surface::WlSurface,
    _toplevel: xdg_toplevel::XdgToplevel,
    _pool: wl_shm_pool::WlShmPool,
    _pool_file: std::fs::File,
}

impl Harness {
    /// One mapped terminal window on the desktop, with a pointer bound.
    fn with_window() -> Self {
        let mut wl = wayland::Harness::<Client>::new();
        let fb = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1280, 800));
        let qh = wl.queue.handle();
        let seat = wl.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _pointer = seat.get_pointer(&qh, ());
        let compositor = wl.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = wl.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
        let shm = wl.bind::<wl_shm::WlShm>("wl_shm", 1);
        let len = BUFFER.saturating_mul(BUFFER).saturating_mul(4);
        // SAFETY: a null-terminated name and a valid flag word are the whole
        // contract of `memfd_create`.
        let raw = unsafe { libc::memfd_create(c"tilo-scroll-reaches-windows".as_ptr(), 0) };
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
        toplevel.set_app_id("org.example.Terminal".into());
        toplevel.set_title("Terminal".into());
        surface.commit();
        wl.pump();
        let id = wl.foreign_id("org.example.Terminal");
        let buffer = pool.create_buffer(
            0,
            BUFFER,
            BUFFER,
            BUFFER.saturating_mul(4),
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, BUFFER, BUFFER);
        surface.commit();
        wl.pump();
        let mut h = Self {
            wl,
            fb,
            doc: AutoCommit::new(),
            focus: None,
            id,
            _surface: surface,
            _toplevel: toplevel,
            _pool: pool,
            _pool_file: file,
        };
        h.settle();
        h
    }

    /// One frame the way the native loop runs it: clients, apps, layout,
    /// paint.
    fn frame(&mut self) {
        self.wl.pump();
        self.fb.pump_apps(&mut self.doc, &ROOT);
        let _ = self.fb.paint_settled();
    }

    /// Frames until the window's tile is on screen and keeps its place.
    fn settle(&mut self) {
        let deadline = tilo_test_env::hang_deadline();
        let mut last = self.tile();
        let mut quiet_since = Instant::now();
        loop {
            self.frame();
            let now = self.tile();
            if now != last || now.area() <= 0.0 {
                last = now;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= QUIET {
                return;
            }
            assert!(Instant::now() < deadline, "the window never came to rest");
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// Every app window's tile on screen.
    fn tiles(&self) -> Vec<(u64, Rect)> {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::shell::input_policy::foreign_tiles(&tree, self.fb.host())
            .into_iter()
            .map(|hit| (hit.surface_id, hit.rect))
            .collect()
    }

    /// The window's tile on screen.
    fn tile(&self) -> Rect {
        self.tiles()
            .into_iter()
            .find_map(|(id, rect)| (id == self.id).then_some(rect))
            .unwrap_or(Rect::ZERO)
    }

    /// A point on the screen that no app window covers: the shell's own.
    fn shell_point(&self) -> Point {
        let tiles = self.tiles();
        (0..80)
            .flat_map(|row| (0..128).map(move |col| (col, row)))
            .map(|(col, row)| Point::new(f64::from(col) * 10.0 + 5.0, f64::from(row) * 10.0 + 5.0))
            .find(|p| !tiles.iter().any(|(_, rect)| rect.contains(*p)))
            .expect("some of the screen is the shell's")
    }

    /// Local input in production order: the seat first, the shell with what
    /// it leaves.
    fn input(&mut self, event: &WindowEvent) {
        let modifiers = Modifiers::default();
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

    fn move_to(&mut self, at: Point) {
        self.input(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(at.x, at.y),
        });
    }

    fn left(&mut self, state: ElementState) {
        self.input(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        });
    }

    /// Two fingers moving up the touchpad, then lifting.
    fn two_finger_scroll(&mut self) {
        for (dy, phase) in [(-12.0, TouchPhase::Moved), (0.0, TouchPhase::Ended)] {
            self.input(&WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
                phase,
            });
        }
    }

    /// One notch of a mouse wheel.
    fn wheel_notch(&mut self) {
        self.input(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        });
    }

    /// What the window heard since last asked.
    fn heard(&mut self) -> Vec<Heard> {
        self.wl.pump();
        std::mem::take(&mut self.wl.client.heard)
    }
}

fn scrolled(heard: &[Heard]) -> bool {
    heard.contains(&Heard::Scroll(wl_pointer::Axis::VerticalScroll))
}

// USER REQUIREMENT (2026-10-21): "btw I can no longer scroll in any of my
// terminal windows (alacritty or tilo native terminal) here"
#[test]
fn scrolling_over_a_terminal_window_scrolls_it_even_after_a_release_went_missing() {
    let mut h = Harness::with_window();
    let window = h.tile().center();

    // The window under the pointer gets the wheel and the touchpad scroll.
    h.move_to(window);
    let _ = h.heard();
    h.wheel_notch();
    assert!(
        scrolled(&h.heard()),
        "a wheel notch over the window scrolls it"
    );
    h.two_finger_scroll();
    assert!(
        scrolled(&h.heard()),
        "a two-finger scroll over the window scrolls it"
    );

    // A press on the shell whose release never reaches the router: a source
    // lost it (a pen lifted over another app, a release made while the
    // session was locked). The router saw the press, as on the device.
    let shell = h.shell_point();
    h.move_to(shell);
    let modifiers = Modifiers::default();
    let _ = shell::compositor::client_input::route_input(
        &mut h.wl.state,
        &mut h.fb,
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        &modifiers,
    );
    h.frame();

    // Back over the window, a click goes to the window, and so does every
    // scroll after it: the next press of the button ended the lost one.
    h.move_to(window);
    let _ = h.heard();
    h.left(ElementState::Pressed);
    h.left(ElementState::Released);
    let clicked = h.heard();
    assert!(
        clicked.contains(&Heard::Button(wl_pointer::ButtonState::Pressed))
            && clicked.contains(&Heard::Button(wl_pointer::ButtonState::Released)),
        "a click into the window reaches it after a lost release: {clicked:?}"
    );
    h.wheel_notch();
    assert!(
        scrolled(&h.heard()),
        "a wheel notch over the window scrolls it after a lost release"
    );
    h.two_finger_scroll();
    assert!(
        scrolled(&h.heard()),
        "a two-finger scroll over the window scrolls it after a lost release"
    );
}
