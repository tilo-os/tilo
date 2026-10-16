//! A sideways two-finger swipe over a window that has no use for it moves
//! the strip; a window that scrolls sideways keeps it.
//!
//! The scroll reaches the window first. Tilo's toolkit hands back the first
//! event of a sideways scroll nothing in the app used, over
//! `tilo_scroll_surface_v1` (`ui::gesture::SidewaysScroll` decides that);
//! the compositor then ends the scroll on the window and pans the strip with
//! all of it. Each window here plays the toolkit's part.

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
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::platform::wayland::scroll_protocol::{tilo_scroll_manager_v1, tilo_scroll_surface_v1};

use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{StripId, WindowWidth};

use crate::support::wayland;

/// What one window heard of a scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Wire {
    Horizontal,
    Stop,
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    /// The surface the pointer is in, by protocol id.
    entered: Option<u32>,
    /// What each surface heard, by protocol id.
    wire: Vec<(u32, Wire)>,
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
            wl_pointer::Event::Enter { surface, .. } => {
                state.entered = Some(surface.id().protocol_id());
                return;
            }
            wl_pointer::Event::Axis {
                axis: wayland_client::WEnum::Value(wl_pointer::Axis::HorizontalScroll),
                ..
            } => Wire::Horizontal,
            wl_pointer::Event::AxisStop {
                axis: wayland_client::WEnum::Value(wl_pointer::Axis::HorizontalScroll),
                ..
            } => Wire::Stop,
            _ => return,
        };
        if let Some(surface) = state.entered {
            state.wire.push((surface, heard));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore tilo_scroll_manager_v1::TiloScrollManagerV1);
delegate_noop!(Client: ignore tilo_scroll_surface_v1::TiloScrollSurfaceV1);

/// One window of the strip: its surface, its tab and its scroll object.
struct Window {
    surface: wl_surface::WlSurface,
    id: u64,
    scroll: tilo_scroll_surface_v1::TiloScrollSurfaceV1,
}

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
    /// Two windows side by side, together wider than the strip, so it has
    /// room to pan.
    fn two_windows() -> (Self, Window, Window) {
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
        let manager = h.bind::<tilo_scroll_manager_v1::TiloScrollManagerV1>(
            "tilo_scroll_manager_v1",
            1,
        );
        let table = h.open(&manager, "org.example.Table");
        let notes = h.open(&manager, "org.example.Notes");
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
            group.width = WindowWidth::Fraction(700);
        }
        h.map(&table);
        h.map(&notes);
        (h, table, notes)
    }

    fn open(
        &mut self,
        manager: &tilo_scroll_manager_v1::TiloScrollManagerV1,
        app_id: &str,
    ) -> Window {
        let qh = self.queue.handle();
        let compositor = self.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = self.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        let scroll = manager.get_scroll_surface(&surface, &qh, ());
        self.pump();
        let _ = self.fb.paint_settled();
        self.pump();
        let id = self.foreign_id(app_id);
        Window {
            surface,
            id,
            scroll,
        }
    }

    fn map(&mut self, window: &Window) {
        use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
        let (width, height) = (64_i32, 64_i32);
        let stride = width.checked_mul(4).unwrap();
        let len = usize::try_from(stride.checked_mul(height).unwrap()).unwrap();
        let shm = self.bind::<wl_shm::WlShm>("wl_shm", 1);
        // SAFETY: a null-terminated name and a valid flag word are the whole
        // contract of `memfd_create`.
        let raw = unsafe { libc::memfd_create(c"tilo-sideways-scroll".as_ptr(), 0) };
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
        window.surface.attach(Some(&buffer), 0, 0);
        window.surface.damage(0, 0, width, height);
        window.surface.commit();
        self.pump();
        let _ = self.fb.paint_settled();
        assert!(
            shell::compositor::foreign::store().is_mapped(window.id),
            "the window is mapped and can take the pointer"
        );
    }

    /// Where `window` is on screen, as far as it is.
    fn tile(&self, window: &Window) -> Rect {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::shell::input_policy::foreign_tiles(&tree, self.fb.host())
            .into_iter()
            .find(|hit| hit.surface_id == window.id)
            .expect("the window is on screen")
            .rect
            .intersect(Rect::new(0.0, 0.0, 1280.0, 800.0))
    }

    fn strip(&self) -> StripId {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        tree.desktop.active_workspace().unwrap().strips[0].id
    }

    fn strip_scroll(&mut self) -> f64 {
        let strip = self.strip();
        self.fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .expect("the strip has a row")
            .scroll_x()
    }

    /// The production order: the seat first, the shell with what it leaves,
    /// then the requests the clients sent in answer.
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
        let _ = self.wl.state.take_handed_back_scrolls(&mut self.fb);
    }

    fn move_to(&mut self, at: Point) {
        self.input(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(at.x, at.y),
        });
    }

    /// Two fingers moving content toward the strip's later windows; they
    /// lift with a step of nothing, as libinput reports it.
    fn swipe(&mut self, phase: TouchPhase) {
        let (dx, dy) = if phase == TouchPhase::Ended {
            (0.0, 0.0)
        } else {
            (-40.0, 2.0)
        };
        self.input(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(dx, dy)),
            phase,
        });
    }

    /// What `window` heard since the last call.
    fn heard(&mut self, window: &Window) -> Vec<Wire> {
        let id = window.surface.id().protocol_id();
        let wire = std::mem::take(&mut self.client.wire);
        wire.into_iter()
            .filter(|(surface, _)| *surface == id)
            .map(|(_, heard)| heard)
            .collect()
    }
}

#[test]
fn a_sideways_swipe_moves_the_strip_only_over_a_window_without_use_for_it() {
    let (mut h, table, notes) = Harness::two_windows();
    let start = h.strip_scroll();

    // The table scrolls sideways: it keeps the whole swipe.
    h.move_to(h.tile(&table).center());
    for phase in [TouchPhase::Started, TouchPhase::Moved, TouchPhase::Moved] {
        h.swipe(phase);
    }
    h.swipe(TouchPhase::Ended);
    let heard = h.heard(&table);
    assert_eq!(
        heard.iter().filter(|wire| **wire == Wire::Horizontal).count(),
        3,
        "the table hears every sideways step: {heard:?}"
    );
    let _ = h.fb.paint_settled();
    assert!(
        (h.strip_scroll() - start).abs() < 0.5,
        "the strip stays where it is under a window that scrolls"
    );

    // Notes has no use for it: on the first step it hands the swipe back,
    // as Tilo's toolkit does, and the strip pans with all of it.
    h.move_to(h.tile(&notes).center());
    h.swipe(TouchPhase::Started);
    assert_eq!(h.heard(&notes), vec![Wire::Horizontal], "the first step reaches it");
    notes.scroll.horizontal_unused();
    h.pump();
    assert!(h.state.take_handed_back_scrolls(&mut h.fb), "the strip takes the swipe");
    assert_eq!(
        h.heard(&notes),
        vec![Wire::Stop],
        "the swipe ends on the window that handed it back"
    );
    h.swipe(TouchPhase::Moved);
    h.swipe(TouchPhase::Moved);
    assert!(
        h.heard(&notes).is_empty(),
        "the rest of the swipe is the strip's alone"
    );
    let _ = h.fb.paint_settled();
    let panned = h.strip_scroll();
    assert!(
        panned > start + 100.0,
        "three steps of 40 px pan the strip toward its later windows: {start} -> {panned}"
    );
    h.swipe(TouchPhase::Ended);
    assert!(h.heard(&notes).is_empty());

    // The next swipe over the table is the table's again.
    h.move_to(h.tile(&table).center());
    h.swipe(TouchPhase::Started);
    assert_eq!(h.heard(&table), vec![Wire::Horizontal]);
    h.swipe(TouchPhase::Ended);
}
