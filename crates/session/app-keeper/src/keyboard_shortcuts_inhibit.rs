//! `zwp_keyboard_shortcuts_inhibit_manager_v1`: an app that wants every key, a game or a remote
//! viewer, asks the desktop to stop acting on its own shortcuts while its window has focus. The
//! request is held per window and made again on a new desktop; while no desktop is attached the
//! app's inhibitor is inactive.

use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, DisplayHandle, Dispatch, GlobalDispatch, New, Resource as _,
};
use wayland_client::{Connection, Dispatch as ClientDispatch, QueueHandle};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1;
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibitor_v1 as client_inhibitor;
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::server::zwp_keyboard_shortcuts_inhibit_manager_v1 as server_manager;
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::server::zwp_keyboard_shortcuts_inhibitor_v1 as server_inhibitor;

use crate::state::State;
use crate::upstream::SurfaceRef;

/// The app's inhibitor on one surface.
pub(crate) struct Held {
    /// The app's object, told when the desktop lets the window take the keys.
    pub app: server_inhibitor::ZwpKeyboardShortcutsInhibitorV1,
    /// Whether the app was last told the inhibitor is active.
    pub active: bool,
}

/// Which surface an app inhibitor belongs to.
pub(crate) struct InhibitorData {
    surface: WlSurface,
}

impl State {
    /// Offer the app the shortcuts-inhibit manager once a desktop offers it.
    pub(crate) fn offer_shortcuts_inhibit(&mut self) {
        if self.shortcuts_inhibit_global.is_none() {
            self.shortcuts_inhibit_global = Some(self.dh.create_global::<
                Self,
                server_manager::ZwpKeyboardShortcutsInhibitManagerV1,
                (),
            >(1, ()));
        }
    }

    /// Make the desktop's inhibitor for a surface that holds one and is shown now.
    pub(crate) fn ensure_shortcuts_inhibitor(&mut self, id: &ObjectId) {
        let Some((manager, qh, seat)) = self.up.as_ref().and_then(|up| {
            up.shortcuts_inhibit
                .clone()
                .map(|manager| (manager, up.qh.clone(), up.seat.clone()))
        }) else {
            return;
        };
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        if kept.shortcuts_inhibitor.is_none() {
            return;
        }
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        let Some(seat) = seat else {
            return;
        };
        if us.shortcuts_inhibitor.is_none() {
            us.shortcuts_inhibitor =
                Some(manager.inhibit_shortcuts(&us.surface, &seat, &qh, SurfaceRef(id.clone())));
        }
    }

    /// The desktop said whether the window takes the keys: tell the app when that changed.
    pub(crate) fn desktop_shortcuts_inhibited(&mut self, id: &ObjectId, active: bool) {
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let Some(held) = kept.shortcuts_inhibitor.as_mut() else {
            return;
        };
        if held.active == active {
            return;
        }
        held.active = active;
        if !held.app.is_alive() {
            return;
        }
        if active {
            held.app.active();
        } else {
            held.app.inactive();
        }
    }

    /// The desktop went away: nothing takes the keys now, and the app is told so. A new desktop
    /// gets the inhibitor again.
    pub(crate) fn hold_shortcuts_inhibit(&mut self) {
        for kept in self.surfaces.values_mut() {
            let Some(held) = kept.shortcuts_inhibitor.as_mut() else {
                continue;
            };
            if held.active {
                held.active = false;
                if held.app.is_alive() {
                    held.app.inactive();
                }
            }
        }
    }

    fn app_shortcuts_inhibited(
        &mut self,
        surface: &WlSurface,
        inhibitor: server_inhibitor::ZwpKeyboardShortcutsInhibitorV1,
    ) {
        let id = surface.id();
        if let Some(kept) = self.surfaces.get_mut(&id) {
            kept.shortcuts_inhibitor = Some(Held {
                app: inhibitor,
                active: false,
            });
        }
        self.ensure_shortcuts_inhibitor(&id);
    }

    fn app_shortcuts_released(&mut self, surface: &WlSurface) {
        let id = surface.id();
        let Some(kept) = self.surfaces.get_mut(&id) else {
            return;
        };
        kept.shortcuts_inhibitor = None;
        if let Some(us) = kept.up.as_mut()
            && let Some(inhibitor) = us.shortcuts_inhibitor.take()
        {
            inhibitor.destroy();
        }
    }
}

impl GlobalDispatch<server_manager::ZwpKeyboardShortcutsInhibitManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<server_manager::ZwpKeyboardShortcutsInhibitManagerV1>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<server_manager::ZwpKeyboardShortcutsInhibitManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &server_manager::ZwpKeyboardShortcutsInhibitManagerV1,
        request: server_manager::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let server_manager::Request::InhibitShortcuts { id, surface, .. } = request {
            let inhibitor = data_init.init(
                id,
                InhibitorData {
                    surface: surface.clone(),
                },
            );
            state.app_shortcuts_inhibited(&surface, inhibitor);
        }
    }
}

impl Dispatch<server_inhibitor::ZwpKeyboardShortcutsInhibitorV1, InhibitorData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &server_inhibitor::ZwpKeyboardShortcutsInhibitorV1,
        request: server_inhibitor::Request,
        data: &InhibitorData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, server_inhibitor::Request::Destroy) {
            state.app_shortcuts_released(&data.surface);
        }
    }
}

impl ClientDispatch<client_inhibitor::ZwpKeyboardShortcutsInhibitorV1, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &client_inhibitor::ZwpKeyboardShortcutsInhibitorV1,
        event: client_inhibitor::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            client_inhibitor::Event::Active => state.desktop_shortcuts_inhibited(&surface.0, true),
            client_inhibitor::Event::Inactive => {
                state.desktop_shortcuts_inhibited(&surface.0, false);
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore ZwpKeyboardShortcutsInhibitManagerV1);
