//! `xdg_wm_dialog_v1`: a window the app marks modal is kept as such, so the desktop focuses it in
//! front of its parent and shows a new desktop the same hint.
//!
//! The hint is held per toplevel and sent to the surface the desktop shows; a new desktop gets it
//! when the toplevel is made again.

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::dialog::{XdgDialogHandler, XdgDialogState};
use wayland_protocols::xdg::dialog::v1::client::xdg_dialog_v1::XdgDialogV1;
use wayland_protocols::xdg::dialog::v1::client::xdg_wm_dialog_v1::XdgWmDialogV1;

use crate::state::State;
use crate::surfaces::{Role, up_toplevel};

impl State {
    /// Offer the app the dialog manager once a desktop offers it.
    pub(crate) fn offer_dialog(&mut self) {
        if self.dialog.is_none() {
            self.dialog = Some(XdgDialogState::new::<Self>(&self.dh));
        }
    }

    /// Make the desktop's dialog object for a window the app marked modal, and set the hint.
    pub(crate) fn ensure_dialog(&mut self, id: &ObjectId) {
        let Some((manager, qh)) = self
            .up
            .as_ref()
            .and_then(|up| up.dialog.clone().map(|manager| (manager, up.qh.clone())))
        else {
            return;
        };
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        if !matches!(kept.role, Role::Toplevel(_)) {
            return;
        }
        let modal = kept.modal;
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        if us.dialog.is_none()
            && modal
            && let Some(toplevel) = up_toplevel(&us.role)
        {
            us.dialog = Some(manager.get_xdg_dialog(&toplevel, &qh, ()));
        }
        if let Some(dialog) = &us.dialog
            && us.modal_sent != Some(modal)
        {
            if modal {
                dialog.set_modal();
            } else {
                dialog.unset_modal();
            }
            us.modal_sent = Some(modal);
        }
    }
}

impl XdgDialogHandler for State {
    fn modal_changed(&mut self, toplevel: ToplevelSurface, is_modal: bool) {
        let id = toplevel.wl_surface().id();
        if let Some(kept) = self.surfaces.get_mut(&id) {
            kept.modal = is_modal;
        }
        self.ensure_dialog(&id);
    }
}

wayland_client::delegate_noop!(State: ignore XdgWmDialogV1);
wayland_client::delegate_noop!(State: ignore XdgDialogV1);

smithay::delegate_xdg_dialog!(State);
