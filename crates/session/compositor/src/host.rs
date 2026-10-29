//! Surface apps the compositor hosts in its own process.
//!
//! The top bar, launcher, quick settings and every other shell surface are
//! apps; the compositor holds none of their code. A [`SurfaceHost`] is how one
//! of them reaches the screen: it hands the compositor buffers to show above
//! the windows and receives the input that lands on them. The in-process host
//! drives an ordinary toolkit app and rasterizes its frames into those
//! buffers; nothing here knows how the pixels were made.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;

pub use smithay::input::pointer::CursorIcon;

use crate::input::HostKey;

/// One buffer a hosted surface shows, at a position in the logical layout.
#[derive(Debug, Clone)]
pub struct HostLayer {
    /// The buffer's top-left corner in the logical layout.
    pub location: (i32, i32),
    /// The size it covers in the logical layout: the buffer is scaled to
    /// fit, so a host draws at the output's (possibly fractional) scale.
    pub size: (i32, i32),
    /// The pixels; the buffer's own render context records what changed.
    pub buffer: MemoryRenderBuffer,
}

/// A drag a hosted app starts: bytes per format (files as
/// `text/uri-list`) and, optionally, an image under the pointer with its
/// hotspot (logical pixels from its top left).
#[derive(Debug, Clone)]
pub struct HostDrag {
    pub offer: Vec<(String, Vec<u8>)>,
    pub icon: Option<(
        MemoryRenderBuffer,
        smithay::utils::Point<i32, smithay::utils::Logical>,
    )>,
}

/// A hosted surface app, as the compositor sees it.
pub trait SurfaceHost {
    /// The layers this surface shows now, bottom first. Hosts draw above
    /// every window, later hosts above earlier ones.
    fn layers(&self) -> Vec<HostLayer>;

    /// Whether the pointer at this logical point belongs to this surface
    /// rather than to what lies beneath it.
    fn accepts_pointer(&self, x: f64, y: f64) -> bool;

    /// The pointer moved to this logical point over the surface.
    fn pointer_motion(&mut self, x: f64, y: f64);

    /// The pointer left the surface.
    fn pointer_leave(&mut self) {}

    /// A pointer button (evdev code) changed state over the surface.
    fn pointer_button(&mut self, button: u32, pressed: bool);

    /// Scroll over the surface, in logical pixels.
    fn pointer_axis(&mut self, _horizontal: f64, _vertical: f64) {}

    /// Whether keys should come to this surface instead of the focused
    /// window (an open launcher or menu).
    fn wants_keyboard(&self) -> bool {
        false
    }

    /// A key, while [`SurfaceHost::wants_keyboard`] holds.
    fn key(&mut self, _key: &HostKey) {}

    /// Other clients' windows this surface pictures (thumbnails, cards),
    /// bottom first: the compositor draws their live buffers there, in
    /// front of this surface's layers.
    fn window_pictures(&self) -> Vec<crate::WindowPicture> {
        Vec::new()
    }

    /// A frame is about to be drawn: bring the layers up to date.
    fn prepare_frame(&mut self) {}

    /// The pointer shape while the pointer is over the surface (a text
    /// beam over a field, a hand over a link).
    fn cursor(&self) -> CursorIcon {
        CursorIcon::Default
    }

    /// A drag (from a Wayland window, an X11 window or a hosted surface)
    /// is over the surface at this logical point, offering these formats
    /// (files as `text/uri-list`): the format the surface takes if it is
    /// dropped here, or `None` to refuse it. Asked again as it moves.
    fn drop_offered(&mut self, _mimes: &[String], _x: f64, _y: f64) -> Option<String> {
        None
    }

    /// The drag left the surface without a drop.
    fn drop_left(&mut self) {}

    /// The drag was dropped on the surface: its data in the format
    /// [`SurfaceHost::drop_offered`] chose, once it has been read.
    fn drop_data(&mut self, _mime: &str, _data: Vec<u8>) {}

    /// A drag the app started while handling the input just routed to it
    /// (a press held and moved on something draggable); the compositor
    /// carries it like [`crate::Compositor::start_host_drag`].
    fn take_drag(&mut self) -> Option<HostDrag> {
        None
    }

    /// A drag this surface started ([`crate::Compositor::start_drag`])
    /// ended: `dropped` when a target took it.
    fn drag_ended(&mut self, _dropped: bool) {}
}
