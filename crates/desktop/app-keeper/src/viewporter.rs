//! `wp_viewporter`: a surface's source rectangle and destination size are part of the state it
//! commits, and the desktop shows the same crop and scale.
//!
//! The app's state stays in smithay's surface cache, so it is held while no desktop is attached;
//! each desktop surface gets a viewport of its own, and a fresh desktop is sent the held values.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::viewporter::{ViewportCachedState, ViewporterState};
use wayland_protocols::wp::viewporter::client::wp_viewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter;

use crate::state::State;

/// What a surface's viewport asks for.
pub(crate) type Viewport = (Option<Rectangle<f64, Logical>>, Option<Size<i32, Logical>>);

impl State {
    /// The app's viewport state of a surface, as it committed it.
    pub(crate) fn viewport_of(surface: &WlSurface) -> Viewport {
        with_states(surface, |states| {
            let mut guard = states.cached_state.get::<ViewportCachedState>();
            let state = guard.current();
            let taken = (state.src, state.dst);
            drop(guard);
            taken
        })
    }

    /// Offer the app the viewporter once a desktop offers it.
    pub(crate) fn offer_viewporter(&mut self) {
        if self.viewporter.is_none() {
            self.viewporter = Some(ViewporterState::new::<Self>(&self.dh));
        }
    }
}

smithay::delegate_viewporter!(State);

/// The desktop's viewport of one of the app's surfaces.
pub(crate) fn make_viewport(
    manager: &wp_viewporter::WpViewporter,
    surface: &wayland_client::protocol::wl_surface::WlSurface,
    qh: &wayland_client::QueueHandle<State>,
) -> wp_viewport::WpViewport {
    manager.get_viewport(surface, qh, ())
}

/// Send `(src, dst)` to the desktop's viewport.
pub(crate) fn set_viewport(viewport: &wp_viewport::WpViewport, (src, dst): Viewport) {
    match src {
        Some(src) => viewport.set_source(src.loc.x, src.loc.y, src.size.w, src.size.h),
        // The protocol unsets the whole rectangle with -1 in all four.
        None => viewport.set_source(-1.0, -1.0, -1.0, -1.0),
    }
    match dst {
        Some(dst) => viewport.set_destination(dst.w, dst.h),
        None => viewport.set_destination(-1, -1),
    }
}
