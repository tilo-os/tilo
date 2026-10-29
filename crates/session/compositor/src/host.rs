//! Surface apps the compositor hosts in its own process.
//!
//! The top bar, launcher, quick settings and every other shell surface are
//! apps; the compositor holds none of their code. A [`SurfaceHost`] is how one
//! of them reaches the screen: it hands the compositor buffers to show above
//! the windows and receives the input that lands on them. The in-process host
//! drives an ordinary toolkit app and rasterizes its frames into those
//! buffers; nothing here knows how the pixels were made.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;

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

    /// A frame is about to be drawn: bring the layers up to date.
    fn prepare_frame(&mut self) {}

    /// The outputs' logical rectangles `(x, y, w, h)`, now and whenever
    /// one is added, removed or moved: a surface covering the desktop
    /// (the top bar, the chrome above the windows) follows them.
    fn outputs_changed(&mut self, _outputs: &[(i32, i32, i32, i32)]) {}
}
