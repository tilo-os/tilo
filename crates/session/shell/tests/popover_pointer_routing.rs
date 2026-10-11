//! A shell menu that hangs over a window owns the pointer while it is open.
//!
//! The screenshot receipt's "Send…" picker drops from the top bar over
//! whatever window is below it. It used to leave the seat to that window:
//! moving onto the picker sent the motion to the client, so the picker
//! showed no hover and kept the last position the shell had seen — on the
//! Send button, outside the picker — and the click meant for a row closed
//! it instead. The tab menu, which also hangs over the window it belongs
//! to, had the same gap.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::ops::{Deref, DerefMut};

use automerge::{AutoCommit, ROOT};
use vello::kurbo::{Point, Rect};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_pointer, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use shell::compositor::frame::FrameBuilder;
use shell::shell::input_policy::chrome_captures_input;
use ui::widget::{CaptureIntent, CapturePreview, ScreenshotCommand, ScreenshotSendTarget};

use crate::support::wayland;

#[derive(Debug, PartialEq)]
enum Wire {
    Enter,
    Leave,
    Motion,
    Button,
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    wire: Vec<Wire>,
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
        let wire = match event {
            wl_pointer::Event::Enter { .. } => Wire::Enter,
            wl_pointer::Event::Leave { .. } => Wire::Leave,
            wl_pointer::Event::Motion { .. } => Wire::Motion,
            wl_pointer::Event::Button { .. } => Wire::Button,
            _ => return,
        };
        state.wire.push(wire);
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
    /// One mapped window spanning the whole strip, with a pointer bound.
    fn with_wide_window() -> (Self, u64) {
        let wl = wayland::Harness::new();
        let fb = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1280, 800));
        let mut h = Self {
            wl,
            fb,
            doc: AutoCommit::new(),
            focus: None,
        };
        let qh = h.queue.handle();
        let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let _pointer = seat.get_pointer(&qh, ());
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("org.example.Under".into());
        h.pump();
        let _ = h.fb.paint_settled();
        h.pump();
        let id = h.foreign_id("org.example.Under");
        // Full strip width, so whatever drops below the top bar is over it.
        for group in h
            .state
            .workspace_tree
            .write()
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .flat_map(|strip| strip.window_groups.iter_mut())
        {
            group.width = shell::shell::state::WindowWidth::Fraction(1000);
        }
        h.map(&surface, id);
        h.state.set_client_keyboard_focus(Some(id));
        h.pump();
        (h, id)
    }

    fn map(&mut self, surface: &wl_surface::WlSurface, surface_id: u64) {
        use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
        let (width, height) = (64, 64);
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let shm = self.bind::<wl_shm::WlShm>("wl_shm", 1);
        // SAFETY: a null-terminated name and a valid flag word are the whole
        // contract of `memfd_create`.
        let raw = unsafe { libc::memfd_create(c"tilo-popover-routing".as_ptr(), 0) };
        assert!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `memfd_create` just handed us this descriptor.
        let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        file.set_len(len as u64).unwrap();
        let qh = self.queue.handle();
        let pool = shm.create_pool(file.as_fd(), i32::try_from(len).unwrap(), &qh, ());
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        self.pump();
        let _ = self.fb.paint_settled();
        assert!(
            shell::compositor::foreign::store().is_mapped(surface_id),
            "the window is mapped and can take the pointer"
        );
    }

    fn tile(&self, surface_id: u64) -> Rect {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::shell::input_policy::foreign_tiles(&tree, self.fb.host())
            .into_iter()
            .find(|hit| hit.surface_id == surface_id)
            .expect("the window is on screen")
            .rect
    }

    /// The production order: the seat first, the shell with what it leaves.
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
        self.wl.pump();
        let _ = self.fb.paint_settled();
    }

    fn move_to(&mut self, at: Point) {
        self.input(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(at.x, at.y),
        });
    }

    fn button(&mut self, state: ElementState) {
        self.input(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        });
    }

    fn named(&self, name: &str) -> Rect {
        *self
            .fb
            .widget_registry()
            .get(name)
            .unwrap_or_else(|| panic!("{name} is on screen"))
    }

    fn sends(&mut self) -> Vec<ScreenshotSendTarget> {
        self.fb
            .take_capture_intents()
            .into_iter()
            .filter_map(|intent: CaptureIntent| match intent.screenshot_command {
                Some(ScreenshotCommand::SendLast(target)) => Some(target),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn the_screenshot_send_menu_keeps_the_pointer_until_a_row_is_picked() {
    let (mut h, window) = Harness::with_wide_window();
    let tile = h.tile(window);

    // The pointer rests on the window, which has it.
    h.move_to(tile.center());
    assert!(
        h.client.wire.contains(&Wire::Enter),
        "the window under the pointer has it before the menu opens: {:?}",
        h.client.wire
    );

    h.fb.set_capture_screenshot_finished(
        false,
        800,
        600,
        CapturePreview {
            columns: 1,
            rows: 1,
            pixels: vec![[20, 40, 60, 255]],
        },
    );
    h.fb.set_capture_screenshot_transition_progress(1_000);
    let _ = h.fb.paint_settled();

    let send = h.named("capture.send-screenshot");
    h.move_to(send.center());
    h.button(ElementState::Pressed);
    h.button(ElementState::Released);
    assert!(
        h.fb.host().capture_studio_open(),
        "the receipt is still up with its picker"
    );
    let row = h.named("capture.send-to-workspace");
    assert!(
        chrome_captures_input(&h.fb),
        "an open shell menu owns the seat, as Quick Settings does"
    );

    h.client.wire.clear();
    // Onto the row, the way a hand gets there: through the window's area.
    let path = [
        Point::new(row.center().x, send.y1 + 2.0),
        Point::new(row.center().x, row.y0 + 1.0),
        row.center(),
    ];
    for at in path {
        h.move_to(at);
        assert!(
            h.named("capture.send-to-workspace") == row,
            "moving onto the picker keeps it open at {at:?}"
        );
    }
    assert!(
        h.sends().is_empty(),
        "hovering a row sends nothing before the click"
    );

    h.button(ElementState::Pressed);
    assert_eq!(
        h.sends(),
        vec![ScreenshotSendTarget::Workspace],
        "the click lands on the row it was aimed at"
    );
    assert!(
        !h.fb
            .widget_registry()
            .contains_key("capture.send-to-workspace"),
        "a pick closes the picker"
    );
    assert!(
        !h.client
            .wire
            .iter()
            .any(|wire| matches!(wire, Wire::Enter | Wire::Motion | Wire::Button)),
        "no motion or button over the menu reached the window: {:?}",
        h.client.wire
    );
    // The press belonged to the menu, and so does its release; only then is
    // the window under the pointer offered it again.
    h.button(ElementState::Released);
    assert!(
        !h.client.wire.contains(&Wire::Button),
        "the window never sees the pick's button: {:?}",
        h.client.wire
    );
}

#[test]
fn an_open_tab_menu_keeps_the_pointer_from_the_window_under_it() {
    let (mut h, window) = Harness::with_wide_window();
    let tile = h.tile(window);
    h.move_to(tile.center());
    assert!(!chrome_captures_input(&h.fb));

    assert!(h.fb.host_mut().open_focused_tab_context_menu());
    let _ = h.fb.paint_settled();
    let menu = h.fb.host().tab_context_menu_rect().expect("the menu is up");
    assert!(
        tile.contains(menu.center()),
        "the tab menu hangs over its window ({tile:?}): {menu:?}"
    );
    assert!(
        chrome_captures_input(&h.fb),
        "an open tab menu owns the seat"
    );

    h.client.wire.clear();
    h.move_to(menu.center());
    assert!(
        h.fb.host().tab_context_menu_is_open(),
        "moving onto the menu keeps it open"
    );
    assert!(
        !h.client
            .wire
            .iter()
            .any(|wire| matches!(wire, Wire::Enter | Wire::Motion)),
        "motion over the menu does not reach the window: {:?}",
        h.client.wire
    );
}
