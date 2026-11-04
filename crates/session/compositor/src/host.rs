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

/// Whether the compositor showing hosted surfaces' frames composes on the
/// CPU, reading their pixels itself: such a surface rasterizes its frames
/// where it stands instead of drawing them on a GPU to copy them back every
/// frame. Said by the compositor as it opens its backend, as there is one
/// compositor in a process.
static COMPOSES_ON_CPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set by the compositor when it opens its backend: what
/// [`hosted_frames_on_cpu`] answers from then on.
pub(crate) fn composes_on_cpu(composes: bool) {
    COMPOSES_ON_CPU.store(composes, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the compositor that will show a hosted frame reads its pixels
/// itself, in memory, rather than sampling them as a texture on a GPU: a
/// surface's frames are then rasterized on the CPU, and a machine with a GPU
/// never copies them back from it.
#[must_use]
pub fn hosted_frames_on_cpu() -> bool {
    COMPOSES_ON_CPU.load(std::sync::atomic::Ordering::Relaxed)
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

/// A text field a hosted surface draws, when it is the one the person types
/// into: what an input method serves it with — how to compose in it, and
/// where its composition stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostTextInput {
    /// The field's content hint, as `text-input-v3` names it: what it takes
    /// and whether it may be shown, learned from or completed.
    pub hint: u32,
    /// The field's purpose, as `text-input-v3` names it: text, a name, an
    /// email, a URL, digits, a password, a terminal.
    pub purpose: u32,
    /// Where its caret stands, in the desk's logical coordinates: the
    /// candidate window and the composition go under it.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What an input method sends a hosted surface's field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostIme {
    /// The composed text is in the field: insert it, replacing any
    /// composition shown before.
    Commit(String),
    /// The composition in progress (empty when it ended), with the byte
    /// offsets its cursor and selection stand at (`None` for no selection).
    Preedit(String, Option<(usize, usize)>),
    /// The field is the one an input method serves from now on.
    Enabled,
    /// The field stopped being served: composing in it is over.
    Disabled,
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

    /// Whether this surface shows over the session's lock. Only what the
    /// session itself stands up shows then (the hardware keys' readout, and
    /// the lock view when the lock screen's app is not there), and what
    /// wants the keyboard or the pointer of those takes them: a locked
    /// session gives every other surface nothing.
    fn above_lock(&self) -> bool {
        false
    }

    /// Whether this surface is the lock itself, drawn by the session where
    /// the lock screen's app cannot draw it: the lock is what the outputs
    /// show, so nothing waits for the app's frame, and a capture of a locked
    /// display is the placeholder as it is for the app's.
    fn drawn_over_lock(&self) -> bool {
        false
    }

    /// Whether a capture may not show what this surface draws now: it shows
    /// a secret, such as what the person typed into a password field. Every
    /// copy of the desktop's pixels — a screenshot, a recording, a share
    /// stream, the portal's screenshot — draws its layers as the
    /// placeholder instead, while the person's own display keeps showing
    /// them (see [`crate::capture::protect`]).
    fn withholds_captures(&self) -> bool {
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

    /// A pointer button was pressed at this logical point, somewhere other
    /// than this surface (a window, the desk, another surface): an open
    /// popover closes.
    fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {}

    /// The pointer button that press came from is up again, at this logical
    /// point: the press is over, and so is everything it was still doing.
    fn press_ended(&mut self, _x: f64, _y: f64) {}

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

    /// The text field this surface draws that the person types into now, when
    /// it has the focus: an input method serves it as it serves a client's
    /// field — composing into it, its candidate window under the caret — and
    /// keys it does not use come back to the field.
    fn text_input(&self) -> Option<HostTextInput> {
        None
    }

    /// What the input method serving [`SurfaceHost::text_input`] sent.
    fn ime(&mut self, _event: HostIme) {}

    /// Offered every press before whoever has the keyboard gets it, the
    /// topmost surface first: a desktop chord or a hardware key this
    /// surface answers wherever the keyboard is. `locked` while the session
    /// is locked, when only keys that reveal nothing (volume, brightness)
    /// may be taken. `inhibited` while a focused client holds the desktop's
    /// shortcuts, when only what must never be given up (hardware keys,
    /// locking the session) may be taken.
    fn claim_key(&mut self, _key: &HostKey, _locked: bool, _inhibited: bool) -> KeyClaim {
        KeyClaim::Pass
    }

    /// Other clients' windows this surface pictures (thumbnails, cards),
    /// bottom first: the compositor draws their live buffers there, in
    /// front of this surface's layers. A window the session keeps private
    /// ([`crate::capture::protect`]) is drawn as the placeholder a capture
    /// of it shows instead, since the desk a picture stands on is what a
    /// screenshot, a recording and a share show.
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

    /// A frame is about to be drawn: bring the layers up to date, and say
    /// whether this surface drew or changed what it shows. A host that did
    /// not leaves the frame to whoever else has something new; a host that
    /// never knows draws every frame (`true`).
    fn prepare_frame(&mut self) -> bool {
        true
    }

    /// Keep what it holds in step without drawing any of it: what a surface the
    /// shell stands in front of still does every frame, so the changes the
    /// person and the session ask of it take effect even though nothing of it
    /// shows. Whether any of it moved, so the frame it stands under is drawn
    /// again.
    fn advance(&mut self) -> bool {
        false
    }

    /// When this surface next needs a frame with no input to bring one: now
    /// when a wake it took has not drawn yet, else the earliest deadline its
    /// content sets (a banner's exit, a caret's blink). A surface whose
    /// content only moves draws on the display's cadence the compositor gave
    /// it ([`SurfaceHost::set_frame_interval`]), not once per turn of the
    /// loop. The loop waits no longer than this; `None` leaves it to the
    /// loop's own idle.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// How long the displays this surface is shown on hold each frame: a
    /// frame of its own is due once per one of these, so the screens show as
    /// many frames as they run at and not one per turn of the loop.
    fn set_frame_interval(&mut self, _interval: std::time::Duration) {}

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
