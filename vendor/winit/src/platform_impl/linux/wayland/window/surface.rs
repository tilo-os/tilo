//! The role a window's surface has on the compositor: an ordinary toplevel
//! the desktop places, a surface that places itself on a layer of an output
//! (a session's own bars, panels and dialogs), or the session's lock screen.
//!
//! Everything about the surface itself — buffers, frame callbacks, input,
//! text input, scale — is the same for both, so the window state and the
//! event loop work through this type, and the requests a layer surface has
//! no notion of (a title, moving, maximizing, a window menu) are ignored
//! for one.

use sctk::reexports::client::protocol::wl_output::WlOutput;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::protocols::xdg::shell::client::xdg_surface::XdgSurface as XdgSurfaceProxy;
use sctk::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge as XdgResizeEdge;
use sctk::shell::wlr_layer::LayerSurface;
use sctk::shell::xdg::window::Window;
use sctk::shell::xdg::XdgSurface;
use sctk::shell::WaylandSurface;

use super::lock::LockSurface;

/// A window's surface on the compositor.
#[derive(Clone)]
pub enum Surface {
    /// A toplevel: the desktop decides where it goes and how big it is.
    Toplevel(Window),
    /// A surface on a layer of an output, placed by its own anchors.
    Layer(LayerSurface),
    /// The session's lock screen: shown on every output, taking every
    /// input, until the client lifts the lock.
    Lock(LockSurface),
}

impl Surface {
    /// The `wl_surface` of either role.
    pub fn wl_surface(&self) -> &WlSurface {
        match self {
            Self::Toplevel(window) => window.wl_surface(),
            Self::Layer(layer) => layer.wl_surface(),
            Self::Lock(lock) => lock.wl_surface(),
        }
    }

    /// Commit the surface's double buffered state.
    pub fn commit(&self) {
        match self {
            Self::Toplevel(window) => window.commit(),
            Self::Layer(layer) => layer.commit(),
            // A lock surface has no double buffered state of its own: what
            // it draws is committed with the frame that attached it.
            Self::Lock(_) => {}
        }
    }

    /// Whether the surface places itself on a layer.
    pub fn is_layer(&self) -> bool {
        matches!(self, Self::Layer(_))
    }

    /// Whether the surface is the session's lock screen.
    pub fn is_lock(&self) -> bool {
        matches!(self, Self::Lock(_))
    }

    /// The lock surface, for the requests only it has.
    pub fn lock_surface(&self) -> Option<&LockSurface> {
        match self {
            Self::Lock(lock) => Some(lock),
            _ => None,
        }
    }

    /// Whether the compositor confirmed the window's lock and gave it its
    /// surface: a window that is not the lock screen always has one.
    pub fn is_session_lock_up(&self) -> bool {
        match self {
            Self::Lock(lock) => lock.is_up(),
            _ => true,
        }
    }

    /// The toplevel, for the requests only a toplevel has.
    pub fn toplevel(&self) -> Option<&Window> {
        match self {
            Self::Toplevel(window) => Some(window),
            Self::Layer(_) | Self::Lock(_) => None,
        }
    }

    /// The toplevel's `xdg_surface`, whose window geometry a layer surface
    /// does not have.
    pub fn xdg_surface(&self) -> Option<&XdgSurfaceProxy> {
        self.toplevel().map(|window| window.xdg_surface())
    }

    /// The title, which a layer surface does not show.
    pub fn set_title(&self, title: &str) {
        if let Some(window) = self.toplevel() {
            window.set_title(title);
        }
    }

    /// The size hints the desktop reads when it places a window; a layer
    /// surface is sized by its anchors instead.
    pub fn set_min_size(&self, size: Option<(u32, u32)>) {
        if let Some(window) = self.toplevel() {
            window.set_min_size(size);
        }
    }

    /// See [`Surface::set_min_size`].
    pub fn set_max_size(&self, size: Option<(u32, u32)>) {
        if let Some(window) = self.toplevel() {
            window.set_max_size(size);
        }
    }

    /// How many buffer pixels a logical one takes, for either role.
    pub fn set_buffer_scale(&self, scale: i32) {
        let Ok(scale) = u32::try_from(scale) else {
            return;
        };
        let _ = match self {
            Self::Toplevel(window) => window.set_buffer_scale(scale),
            Self::Layer(layer) => layer.set_buffer_scale(scale),
            Self::Lock(lock) => {
                lock.wl_surface().set_buffer_scale(
                    i32::try_from(scale).unwrap_or(i32::MAX),
                );
                return;
            }
        };
    }

    /// Minimize, which a layer surface does not have.
    pub fn set_minimized(&self) {
        if let Some(window) = self.toplevel() {
            window.set_minimized();
        }
    }

    /// Maximize, which a layer surface does not have.
    pub fn set_maximized(&self) {
        if let Some(window) = self.toplevel() {
            window.set_maximized();
        }
    }

    /// Unmaximize, which a layer surface does not have.
    pub fn unset_maximized(&self) {
        if let Some(window) = self.toplevel() {
            window.unset_maximized();
        }
    }

    /// Fullscreen, which a layer surface does not have.
    pub fn set_fullscreen(&self, output: Option<&WlOutput>) {
        if let Some(window) = self.toplevel() {
            window.set_fullscreen(output);
        }
    }

    /// Unfullscreen, which a layer surface does not have.
    pub fn unset_fullscreen(&self) {
        if let Some(window) = self.toplevel() {
            window.unset_fullscreen();
        }
    }

    /// Show the desktop's window menu, which a layer surface does not have.
    pub fn show_window_menu(&self, seat: &WlSeat, serial: u32, position: (i32, i32)) {
        if let Some(window) = self.toplevel() {
            window.show_window_menu(seat, serial, position);
        }
    }

    /// Start an interactive resize, which a layer surface does not have.
    pub fn resize(&self, seat: &WlSeat, serial: u32, edges: XdgResizeEdge) {
        if let Some(window) = self.toplevel() {
            window.resize(seat, serial, edges);
        }
    }

    /// Start an interactive move, which a layer surface does not have.
    pub fn move_(&self, seat: &WlSeat, serial: u32) {
        if let Some(window) = self.toplevel() {
            window.move_(seat, serial);
        }
    }
}
