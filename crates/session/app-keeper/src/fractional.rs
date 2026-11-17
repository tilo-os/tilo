//! `wp_fractional_scale_v1`: the scale a desktop wants a surface drawn at, kept and re-sent.
//!
//! The desktop's answer for the surface that mirrors the app's is passed down to the app's own
//! object; the value is kept so a surface shown on another desktop is told its scale there too.

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::fractional_scale::{
    FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1;

use crate::state::State;
use crate::upstream::SurfaceRef;

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let id = surface.id();
        if let Some(kept) = self.surfaces.get_mut(&id) {
            kept.wants_fractional_scale = true;
        }
        self.request_fractional_scale(&id);
    }
}

impl State {
    /// Offer the app the manager once a desktop offers it.
    pub(crate) fn offer_fractional_scale(&mut self) {
        if self.fractional_scale.is_none() {
            self.fractional_scale = Some(FractionalScaleManagerState::new::<Self>(&self.dh));
        }
    }

    /// Ask the desktop for the scale of the surface that mirrors the app's; a fresh desktop is
    /// asked again, and a surface that asked and is not shown yet asks when it is.
    pub(crate) fn request_fractional_scale(
        &mut self,
        id: &smithay::reexports::wayland_server::backend::ObjectId,
    ) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(manager) = up.fractional_scale.clone() else {
            return;
        };
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        if !kept.wants_fractional_scale {
            return;
        }
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        if us.fractional_scale.is_some() {
            return;
        }
        us.fractional_scale =
            Some(manager.get_fractional_scale(&us.surface, &up.qh, SurfaceRef(id.clone())));
        self.reapply_fractional_scale(id);
    }

    /// The desktop said what scale it wants for a surface.
    pub(crate) fn desktop_fractional_scale(
        &mut self,
        id: &smithay::reexports::wayland_server::backend::ObjectId,
        scale: u32,
    ) {
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let scale = f64::from(scale) / 120.0;
        kept.preferred_scale = Some(scale);
        let surface = kept.surface.clone();
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale);
            });
        });
    }

    /// Tell the app the scale held for a surface again: a new object or a new desktop may not
    /// have sent it.
    pub(crate) fn reapply_fractional_scale(
        &self,
        id: &smithay::reexports::wayland_server::backend::ObjectId,
    ) {
        let Some(kept) = self.surfaces.get(id) else {
            return;
        };
        let Some(scale) = kept.preferred_scale else {
            return;
        };
        let surface = kept.surface.clone();
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale);
            });
        });
    }
}

smithay::delegate_fractional_scale!(State);

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.desktop_fractional_scale(&surface.0, scale);
        }
    }
}
