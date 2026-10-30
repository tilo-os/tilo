//! Surface apps the compositor hosts in its own process.
//!
//! The top bar, launcher, quick settings and every other shell surface are
//! apps; the compositor holds none of their code. A [`SurfaceHost`] is how one
//! of them reaches the screen: it hands the compositor buffers to show above
//! the windows and receives the input that lands on them. The in-process host
//! drives an ordinary toolkit app and draws its frames into those buffers;
//! nothing here knows how the pixels were made.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Buffer, Rectangle};

pub use smithay::input::pointer::CursorIcon;

use crate::input::HostKey;
use crate::keys::KeyClaim;

/// A whole-pixel rectangle of a surface: what one draw changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// Its left edge, in pixels.
    pub x: u32,
    /// Its top edge, in pixels.
    pub y: u32,
    /// How wide it is, in pixels.
    pub width: u32,
    /// How tall it is, in pixels.
    pub height: u32,
}

impl Region {
    /// The rectangle at `(x, y)` of `width` by `height` pixels.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// The pixels a hosted surface shows: premultiplied RGBA, kept as the buffer
/// the compositor draws, so a host only hands over what changed.
#[derive(Clone, Debug)]
pub struct HostFrame {
    buffer: MemoryRenderBuffer,
    size: (u32, u32),
}

impl HostFrame {
    /// An empty frame of `size` physical pixels.
    #[must_use]
    pub fn new(size: (u32, u32)) -> Self {
        Self {
            buffer: MemoryRenderBuffer::new(
                smithay::backend::allocator::Fourcc::Abgr8888,
                (
                    i32::try_from(size.0).unwrap_or(i32::MAX),
                    i32::try_from(size.1).unwrap_or(i32::MAX),
                ),
                1,
                smithay::utils::Transform::Normal,
                None,
            ),
            size,
        }
    }

    /// The frame as the compositor draws it.
    #[must_use]
    pub const fn buffer(&self) -> &MemoryRenderBuffer {
        &self.buffer
    }

    /// A frame showing `buffer`, a surface of `size` physical pixels: for a
    /// host that made its own.
    #[must_use]
    pub const fn from_buffer(buffer: MemoryRenderBuffer, size: (u32, u32)) -> Self {
        Self { buffer, size }
    }

    /// The size of the surface in physical pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Copy the `regions` of `pixels` (premultiplied RGBA, `width * 4` bytes
    /// a row of this frame's surface) into the frame, telling it what
    /// changed.
    pub fn set_pixels(&mut self, pixels: &[u8], regions: &[Region]) {
        let width = self.size.0 as usize;
        let drawn = self.buffer.render().draw(|memory: &mut [u8]| {
            for region in regions {
                for row in region.y..region.y.saturating_add(region.height) {
                    let start = (row as usize)
                        .saturating_mul(width)
                        .saturating_add(region.x as usize)
                        .saturating_mul(4);
                    let end = start.saturating_add((region.width as usize).saturating_mul(4));
                    if let (Some(to), Some(from)) =
                        (memory.get_mut(start..end), pixels.get(start..end))
                    {
                        to.copy_from_slice(from);
                    }
                }
            }
            Ok::<_, std::convert::Infallible>(
                regions
                    .iter()
                    .filter_map(|region| {
                        Some(Rectangle::<i32, Buffer>::new(
                            (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
                            (
                                i32::try_from(region.width).ok()?,
                                i32::try_from(region.height).ok()?,
                            )
                                .into(),
                        ))
                    })
                    .collect(),
            )
        });
        if let Err(never) = drawn {
            match never {}
        }
    }
}

/// One buffer a hosted surface shows, at a position in the logical layout.
#[derive(Debug, Clone)]
pub struct HostLayer {
    /// The buffer's top-left corner in the logical layout.
    pub location: (i32, i32),
    /// The size it covers in the logical layout: the buffer is scaled to
    /// fit, so a host draws at the output's (possibly fractional) scale.
    pub size: (i32, i32),
    /// The pixels; the buffer's own render context records what changed.
    pub frame: HostFrame,
}

/// A drag a hosted app starts: bytes per format (files as
/// `text/uri-list`) and, optionally, an image under the pointer with its
/// hotspot (logical pixels from its top left).
#[derive(Debug, Clone)]
pub struct HostDrag {
    pub offer: Vec<(String, Vec<u8>)>,
    pub icon: Option<(
        HostFrame,
        smithay::utils::Point<i32, smithay::utils::Logical>,
    )>,
}

/// A hosted surface app, as the compositor sees it.
pub trait SurfaceHost {
    /// The layers this surface shows now, bottom first. Hosts draw above
    /// every window ([`Self::below_windows`] aside), later hosts above
    /// earlier ones.
    fn layers(&self) -> Vec<HostLayer>;

    /// Whether this surface lies under every window rather than above
    /// them: the desktop itself (its wallpaper, window bars and strips).
    /// It takes the pointer only where no window is.
    fn below_windows(&self) -> bool {
        false
    }

    /// Whether this surface is the lock screen: while the session is
    /// locked only it (and a lock client's surfaces) is drawn and given
    /// the pointer and the keyboard; until then it is never drawn.
    fn lock_screen(&self) -> bool {
        false
    }

    /// Whether the pointer at this logical point belongs to this surface
    /// rather than to what lies beneath it.
    fn accepts_pointer(&self, x: f64, y: f64) -> bool;

    /// The pointer moved to this logical point over the surface.
    fn pointer_motion(&mut self, x: f64, y: f64);

    /// The pointer left the surface.
    fn pointer_leave(&mut self) {}

    /// A pointer button (evdev code) changed state over the surface.
    fn pointer_button(&mut self, button: u32, pressed: bool);

    /// A pointer button was pressed somewhere other than this surface (a
    /// window, the desk, another surface): an open popover closes.
    fn pressed_elsewhere(&mut self) {}

    /// Scroll over the surface, in logical pixels.
    fn pointer_axis(&mut self, _horizontal: f64, _vertical: f64) {}

    /// Whether keys should come to this surface instead of the focused
    /// window (an open launcher or menu).
    fn wants_keyboard(&self) -> bool {
        false
    }

    /// A key, while [`SurfaceHost::wants_keyboard`] holds, or one this
    /// surface claimed (its press, repeats and release).
    fn key(&mut self, _key: &HostKey) {}

    /// Offered every press before whoever has the keyboard gets it, the
    /// topmost surface first: a desktop chord or a hardware key this
    /// surface answers wherever the keyboard is. `locked` while the session
    /// is locked, when only keys that reveal nothing (volume, brightness)
    /// may be taken.
    fn claim_key(&mut self, _key: &HostKey, _locked: bool) -> KeyClaim {
        KeyClaim::Pass
    }

    /// Other clients' windows this surface pictures (thumbnails, cards),
    /// bottom first: the compositor draws their live buffers there, in
    /// front of this surface's layers.
    fn window_pictures(&self) -> Vec<crate::WindowPicture> {
        Vec::new()
    }

    /// The regions this surface draws translucent material in, which take
    /// a blur of what lies below them ([`crate::backdrop`]).
    fn backdrops(&self) -> Vec<crate::BackdropRequest> {
        Vec::new()
    }

    /// Blurred images of what lies below [`SurfaceHost::backdrops`]'
    /// regions, in their order; `None` where nothing changed since the
    /// last.
    fn set_backdrops(&mut self, _images: Vec<Option<crate::BackdropImage>>) {}

    /// A frame is about to be drawn: bring the layers up to date. `true`
    /// when the layer drew something this frame.
    fn prepare_frame(&mut self) -> bool {
        false
    }

    /// The outputs' logical rectangles `(x, y, w, h)`, now and whenever
    /// one is added, removed or moved: a surface covering the desktop
    /// (the top bar, the chrome above the windows) follows them.
    fn outputs_changed(&mut self, _outputs: &[(i32, i32, i32, i32)]) {}

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
