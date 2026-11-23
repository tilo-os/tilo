//! Window decorations: the app asks to be decorated, the desktop says it is, and both sides
//! agree across a restart.
//!
//! `zxdg_decoration_manager_v1` and `org_kde_kwin_server_decoration_manager` are the two spellings
//! of one question. The desktop draws the frame of every window it shows, so its answer is always
//! server-side; an app that decorates itself would draw a title bar inside the desktop's tab. The
//! app's request goes up, the desktop's answer goes down and is held, and a new desktop is given
//! the request again.

use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as ServerXdgMode;
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::kde::decoration::{KdeDecorationHandler, KdeDecorationState};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1;
use wayland_protocols_misc::server_decoration::client::org_kde_kwin_server_decoration::{
    self as client_kde, OrgKdeKwinServerDecoration,
};
use wayland_protocols_misc::server_decoration::client::org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager;
use wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
    Mode as ServerKdeMode, OrgKdeKwinServerDecoration as ServerKdeDecoration,
};
use wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDefaultMode;

use crate::state::State;
use crate::surfaces::{Role, up_toplevel};
use crate::upstream::SurfaceRef;

/// The app's xdg decoration on one surface, kept while no desktop is attached.
#[derive(Default)]
pub(crate) struct Xdg {
    /// The mode the app last asked for; `None` after it unset its preference.
    requested: Option<ServerXdgMode>,
    /// The mode the desktop answered.
    mode: Option<ServerXdgMode>,
    /// The mode the app was last told, so it is not told the same one twice.
    sent: Option<ServerXdgMode>,
}

/// The app's KDE decoration on one surface, kept while no desktop is attached.
pub(crate) struct Kde {
    /// The app's object, still told the desktop's mode while the desktop is away.
    pub object: ServerKdeDecoration,
    requested: Option<ServerKdeMode>,
    mode: Option<ServerKdeMode>,
    sent: Option<ServerKdeMode>,
}

impl Kde {
    pub(crate) fn new(object: ServerKdeDecoration) -> Self {
        Self {
            object,
            requested: None,
            mode: None,
            sent: None,
        }
    }
}

fn client_xdg(mode: ServerXdgMode) -> zxdg_toplevel_decoration_v1::Mode {
    // Both spellings of the protocol have the same values.
    zxdg_toplevel_decoration_v1::Mode::try_from(u32::from(mode))
        .unwrap_or(zxdg_toplevel_decoration_v1::Mode::ClientSide)
}

fn client_kde_mode(mode: ServerKdeMode) -> client_kde::Mode {
    client_kde::Mode::try_from(u32::from(mode)).unwrap_or(client_kde::Mode::None)
}

impl State {
    /// Offer the app the decoration manager the desktop just offered, once.
    pub(crate) fn offer_decorations(&mut self, interface: &str) {
        match interface {
            "zxdg_decoration_manager_v1" if self.xdg_decorations.is_none() => {
                self.xdg_decorations = Some(XdgDecorationState::new::<Self>(&self.dh));
            }
            "org_kde_kwin_server_decoration_manager" if self.kde_decorations.get().is_none() => {
                // A client that never states a preference is decorated by the desktop, as every
                // other window is.
                self.kde_decorations.get_or_init(|| {
                    KdeDecorationState::new::<Self>(&self.dh, KdeDefaultMode::Server)
                });
            }
            _ => {}
        }
    }

    /// Make the desktop's decoration objects for a surface that is shown now, and send the app's
    /// held request to each.
    pub(crate) fn ensure_decorations(&mut self, id: &ObjectId) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        if !up.globals_bound {
            return;
        }
        let qh = up.qh.clone();
        let xdg_manager = up.xdg_decoration.clone();
        let kde_manager = up.kde_decoration.clone();
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let is_toplevel = matches!(kept.role, Role::Toplevel(_));
        let held_xdg = kept.xdg_decoration.as_ref();
        let held_kde = kept.kde_decoration.as_ref();
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        if let (Some(manager), true, Some(held)) = (xdg_manager, is_toplevel, held_xdg)
            && us.xdg_decoration.is_none()
            && let Some(toplevel) = up_toplevel(&us.role)
        {
            let made = manager.get_toplevel_decoration(&toplevel, &qh, SurfaceRef(id.clone()));
            if let Some(mode) = held.requested {
                made.set_mode(client_xdg(mode));
            }
            us.xdg_decoration = Some(made);
        }
        if let (Some(manager), Some(held)) = (kde_manager, held_kde)
            && us.kde_decoration.is_none()
        {
            let made = manager.create(&us.surface, &qh, SurfaceRef(id.clone()));
            if let Some(mode) = held.requested {
                made.request_mode(client_kde_mode(mode));
            }
            us.kde_decoration = Some(made);
        }
    }

    /// The app asked for a decoration mode on its xdg toplevel: pass the request up. The answer
    /// comes down when the desktop configured the window.
    fn request_xdg_decoration(&mut self, id: &ObjectId) {
        self.ensure_decorations(id);
        let requested = self
            .surfaces
            .get(id)
            .and_then(|kept| kept.xdg_decoration.as_ref())
            .and_then(|held| held.requested);
        if let Some(Some(us)) = self.surfaces.get_mut(id).map(|kept| kept.up.as_mut())
            && let (Some(decoration), Some(mode)) = (&us.xdg_decoration, requested)
        {
            decoration.set_mode(client_xdg(mode));
        }
    }

    /// The app no longer prefers a decoration mode.
    fn unset_xdg_decoration(&mut self, id: &ObjectId) {
        if let Some(kept) = self.surfaces.get_mut(id)
            && let Some(held) = kept.xdg_decoration.as_mut()
        {
            held.requested = None;
        }
        if let Some(Some(us)) = self.surfaces.get_mut(id).map(|kept| kept.up.as_mut())
            && let Some(decoration) = &us.xdg_decoration
        {
            decoration.unset_mode();
        }
    }

    /// The desktop answered the decoration mode of the window `id`: hold it and tell the app,
    /// whose next configure carries it.
    pub(crate) fn desktop_xdg_decoration(&mut self, id: &ObjectId, mode: ServerXdgMode) {
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let Some(held) = kept.xdg_decoration.as_mut() else {
            return;
        };
        held.mode = Some(mode);
        if held.sent == Some(mode) {
            return;
        }
        held.sent = Some(mode);
        let Role::Toplevel(top) = &kept.role else {
            return;
        };
        top.toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        if top.toplevel.is_initial_configure_sent() {
            top.toplevel.send_pending_configure();
        }
    }

    /// The app asked for a mode through the KDE spelling of the same question.
    fn request_kde_decoration(&mut self, id: &ObjectId) {
        self.ensure_decorations(id);
        let requested = self
            .surfaces
            .get(id)
            .and_then(|kept| kept.kde_decoration.as_ref())
            .and_then(|held| held.requested);
        if let Some(Some(us)) = self.surfaces.get_mut(id).map(|kept| kept.up.as_mut())
            && let (Some(decoration), Some(mode)) = (&us.kde_decoration, requested)
        {
            decoration.request_mode(client_kde_mode(mode));
        }
    }

    /// The desktop told the window `id` how it is decorated: hold it and pass it to the app.
    pub(crate) fn desktop_kde_decoration(&mut self, id: &ObjectId, mode: ServerKdeMode) {
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let Some(held) = kept.kde_decoration.as_mut() else {
            return;
        };
        held.mode = Some(mode);
        if held.sent == Some(mode) {
            return;
        }
        held.sent = Some(mode);
        if held.object.is_alive() {
            held.object.mode(mode);
        }
    }

    /// The app destroyed its KDE decoration object; the desktop's goes with it.
    pub(crate) fn kde_decoration_released(&mut self, id: &ObjectId) {
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        kept.kde_decoration = None;
        if let Some(us) = kept.up.as_mut()
            && let Some(decoration) = us.kde_decoration.take()
        {
            decoration.release();
        }
    }
}

/// The app's decoration objects and their requests on its xdg toplevels.
impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        let id = toplevel.wl_surface().id();
        if let Some(kept) = self.surfaces.get_mut(&id) {
            kept.xdg_decoration.get_or_insert_with(Xdg::default);
        }
        self.ensure_decorations(&id);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: ServerXdgMode) {
        let id = toplevel.wl_surface().id();
        let Some(kept) = self.surfaces.get_mut(&id) else {
            return;
        };
        let held = kept.xdg_decoration.get_or_insert_with(Xdg::default);
        held.requested = Some(mode);
        self.request_xdg_decoration(&id);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.unset_xdg_decoration(&toplevel.wl_surface().id());
    }
}

impl KdeDecorationHandler for State {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        // Only a desktop offering the global creates it, and its objects exist only after that.
        self.kde_decorations
            .get_or_init(|| KdeDecorationState::new::<Self>(&self.dh, KdeDefaultMode::Server))
    }

    fn new_decoration(&mut self, surface: &WlSurface, decoration: &ServerKdeDecoration) {
        let id = surface.id();
        if let Some(kept) = self.surfaces.get_mut(&id) {
            kept.kde_decoration = Some(Kde::new(decoration.clone()));
        }
        self.ensure_decorations(&id);
    }

    /// The client's preference, passed up. It is not echoed back: the desktop's answer comes as
    /// the decoration's `mode` event, and echoing a request made toolkits argue about it.
    fn request_mode(
        &mut self,
        surface: &WlSurface,
        _decoration: &ServerKdeDecoration,
        mode: WEnum<ServerKdeMode>,
    ) {
        let id = surface.id();
        let Some(kept) = self.surfaces.get_mut(&id) else {
            return;
        };
        if let Some(held) = kept.kde_decoration.as_mut() {
            held.requested = match mode {
                WEnum::Value(mode) => Some(mode),
                WEnum::Unknown(_) => None,
            };
        }
        self.request_kde_decoration(&id);
    }

    fn release(&mut self, _decoration: &ServerKdeDecoration, surface: &WlSurface) {
        self.kde_decoration_released(&surface.id());
    }
}

impl Dispatch<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure {
            mode: WEnum::Value(mode),
        } = event
            && let Ok(mode) = ServerXdgMode::try_from(u32::from(mode))
        {
            state.desktop_xdg_decoration(&surface.0, mode);
        }
    }
}

impl Dispatch<OrgKdeKwinServerDecoration, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &OrgKdeKwinServerDecoration,
        event: client_kde::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let client_kde::Event::Mode {
            mode: WEnum::Value(mode),
        } = event
            && let Ok(mode) = ServerKdeMode::try_from(u32::from(mode))
        {
            state.desktop_kde_decoration(&surface.0, mode);
        }
    }
}

wayland_client::delegate_noop!(State: ignore wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1);
wayland_client::delegate_noop!(State: ignore OrgKdeKwinServerDecorationManager);

smithay::delegate_xdg_decoration!(State);
smithay::delegate_kde_decoration!(State);
