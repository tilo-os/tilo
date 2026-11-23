//! `zwp_idle_inhibit_manager_v1`: while the app has an inhibitor on a window, the desktop shown
//! with that window must not go idle. The inhibitor is held per surface, made again on a new
//! desktop, and nothing inhibits while no desktop is attached.

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::state::State;

impl State {
    /// Offer the app the idle-inhibit manager once a desktop offers it.
    pub(crate) fn offer_idle_inhibit(&mut self) {
        if self.idle_inhibit.is_none() {
            self.idle_inhibit = Some(IdleInhibitManagerState::new::<Self>(&self.dh));
        }
    }

    /// Make the desktop's inhibitor for a surface that holds one and is shown now.
    pub(crate) fn ensure_idle_inhibit(&mut self, id: &ObjectId) {
        let Some((manager, qh)) = self.up.as_ref().and_then(|up| {
            up.idle_inhibit
                .clone()
                .map(|manager| (manager, up.qh.clone()))
        }) else {
            return;
        };
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        if !kept.idle_inhibitor {
            return;
        }
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        if us.idle_inhibitor.is_none() {
            us.idle_inhibitor = Some(manager.create_inhibitor(&us.surface, &qh, ()));
        }
    }
}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        let id = surface.id();
        if let Some(kept) = self.surfaces.get_mut(&id) {
            kept.idle_inhibitor = true;
        }
        self.ensure_idle_inhibit(&id);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        let id = surface.id();
        let Some(kept) = self.surfaces.get_mut(&id) else {
            return;
        };
        kept.idle_inhibitor = false;
        if let Some(us) = kept.up.as_mut()
            && let Some(inhibitor) = us.idle_inhibitor.take()
        {
            inhibitor.destroy();
        }
    }
}

impl
    Dispatch<
        wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        (),
    > for State
{
    fn event(
        _: &mut Self,
        _: &wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        _: wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1);

smithay::delegate_idle_inhibit!(State);
