//! The app's side: smithay's protocol state and the handlers that turn every request into held
//! state and push it to the desktop.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::input::{SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::{
    wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface,
};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource as _};
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::dmabuf::{
    DmabufFeedback, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::shell::xdg::{
    Configure, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_shm, delegate_xdg_shell,
};

use crate::outputs::KeptOutput;
use crate::seat::SeatMirror;
use crate::surfaces::{Buffers, Kept, Pop, Role, Top};
use crate::upstream::Upstream;
use crate::wire::{FromKeeper, Window};

/// What one app connection is to the keeper.
#[derive(Default)]
pub(crate) struct ClientState {
    pub compositor: CompositorClientState,
}

impl ClientData for ClientState {
    fn disconnected(&self, _client: ClientId, reason: DisconnectReason) {
        if let DisconnectReason::ProtocolError(error) = reason {
            tracing::warn!(
                ?error,
                "an app connection broke the protocol and was closed"
            );
        }
    }
}

/// Everything the keeper holds for its app, and the desktop connection it mirrors it to.
pub struct State {
    pub(crate) dh: DisplayHandle,
    compositor: CompositorState,
    shm: ShmState,
    xdg_shell: XdgShellState,
    _output_manager: OutputManagerState,
    smithay_seat: SeatState<Self>,
    dmabuf: DmabufState,
    pub(crate) dmabuf_global: Option<DmabufGlobal>,
    /// The app-facing globals this keeper chose to offer, once a desktop offered them.
    pub(crate) viewporter: Option<smithay::wayland::viewporter::ViewporterState>,
    pub(crate) fractional_scale:
        Option<smithay::wayland::fractional_scale::FractionalScaleManagerState>,
    pub(crate) content_type: Option<smithay::wayland::content_type::ContentTypeState>,
    /// What the desktop answered each surface's own feedback request, kept while it is away.
    pub(crate) surface_feedback: crate::dmabuf::SurfaceFeedbacks,
    /// Every live surface of the app, by its id.
    pub(crate) surfaces: HashMap<smithay::reexports::wayland_server::backend::ObjectId, Kept>,
    pub(crate) next_seq: u64,
    pub(crate) buffers: Buffers,
    pub(crate) seat: SeatMirror,
    pub(crate) outputs: Vec<KeptOutput>,
    /// The desktop, while one is attached.
    pub(crate) up: Option<Upstream>,
    /// Counts attaches: an upstream serial is only meaningful in the generation it came from.
    pub(crate) generation: u64,
    pub(crate) app_id_prefix: Option<String>,
    pub(crate) next_tag: u64,
    pub(crate) placements: HashMap<String, String>,
    pub(crate) windows_changed: bool,
    pub(crate) fault: Option<String>,
    /// The desktop's pings and the app connections that owe them an answer.
    pub(crate) pings: crate::pings::Pings,
    /// App connections whose state broke the desktop's protocol once already.
    pub(crate) faulted: HashSet<ClientId>,
    /// Messages for every connected session.
    pub(crate) outbox: Vec<FromKeeper>,
    /// Set once the first desktop answered with its globals: the app may start.
    pub(crate) ready_once: bool,
}

impl State {
    pub(crate) fn new(dh: &DisplayHandle, app_id_prefix: Option<String>) -> Self {
        let compositor_state = CompositorState::new::<Self>(dh);
        let shm_state = ShmState::new::<Self>(dh, Vec::new());
        let xdg_state = XdgShellState::new::<Self>(dh);
        let output_state = OutputManagerState::new_with_xdg_output::<Self>(dh);
        let seat = SeatMirror::new(dh);
        Self {
            dh: dh.clone(),
            compositor: compositor_state,
            shm: shm_state,
            xdg_shell: xdg_state,
            _output_manager: output_state,
            smithay_seat: SeatState::new(),
            dmabuf: DmabufState::new(),
            dmabuf_global: None,
            viewporter: None,
            fractional_scale: None,
            content_type: None,
            surface_feedback: crate::dmabuf::SurfaceFeedbacks::default(),
            surfaces: HashMap::new(),
            next_seq: 0,
            buffers: Buffers::default(),
            seat,
            outputs: Vec::new(),
            up: None,
            generation: 0,
            app_id_prefix,
            next_tag: 0,
            placements: HashMap::new(),
            windows_changed: false,
            fault: None,
            pings: crate::pings::Pings::default(),
            faulted: HashSet::new(),
            outbox: Vec::new(),
            ready_once: false,
        }
    }

    /// Offer the app a global the desktop just offered, once.
    pub(crate) fn offer_global(&mut self, interface: &str) {
        match interface {
            "wp_viewporter" => self.offer_viewporter(),
            "wp_fractional_scale_manager_v1" => self.offer_fractional_scale(),
            "wp_content_type_manager_v1" if self.content_type.is_none() => {
                self.content_type =
                    Some(smithay::wayland::content_type::ContentTypeState::new::<Self>(&self.dh));
            }
            _ => {}
        }
    }

    pub(crate) fn shm_state_mut(&mut self) -> &mut ShmState {
        &mut self.shm
    }

    pub(crate) fn dmabuf_state_mut(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    /// The app id the desktop is given for what the app set.
    pub(crate) fn upstream_app_id(&self, app_id: Option<&str>) -> Option<String> {
        let app_id = app_id.map(str::trim).filter(|id| !id.is_empty());
        match &self.app_id_prefix {
            Some(prefix) => Some(format!("{prefix}{}", app_id.unwrap_or("window"))),
            None => app_id.map(str::to_owned),
        }
    }

    /// The app's windows, oldest first.
    pub(crate) fn windows(&self) -> Vec<Window> {
        let mut windows: Vec<(u64, Window)> = self
            .surfaces
            .values()
            .filter_map(|kept| {
                let Role::Toplevel(top) = &kept.role else {
                    return None;
                };
                let (title, app_id) = top.title_and_app_id();
                Some((
                    top.number,
                    Window {
                        tag: top.tag.clone(),
                        app_id: self.upstream_app_id(app_id.as_deref()),
                        title,
                        placement: self.placements.get(&top.tag).cloned(),
                    },
                ))
            })
            .collect();
        windows.sort_by_key(|(number, _)| *number);
        windows.into_iter().map(|(_, window)| window).collect()
    }

    fn new_kept(&mut self, surface: &WlSurface) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.surfaces
            .insert(surface.id(), Kept::new(surface.clone(), seq));
    }

    fn set_role(&mut self, surface: &WlSurface, role: Role) {
        if !self.surfaces.contains_key(&surface.id()) {
            self.new_kept(surface);
        }
        if let Some(kept) = self.surfaces.get_mut(&surface.id()) {
            kept.role = role;
        }
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        static FALLBACK: OnceLock<CompositorClientState> = OnceLock::new();
        client.get_data::<ClientState>().map_or_else(
            || FALLBACK.get_or_init(CompositorClientState::default),
            |data| &data.compositor,
        )
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.new_kept(surface);
    }

    fn new_subsurface(&mut self, surface: &WlSurface, parent: &WlSurface) {
        self.set_role(surface, Role::Sub(crate::surfaces::Sub::new(parent.id())));
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.absorb(surface);
        self.first_configure_while_away(surface);
        self.push(&surface.id());
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        self.forget_surface(&surface.id());
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, buffer: &WlBuffer) {
        self.buffers.client_destroyed(&buffer.id());
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    fn new_surface_feedback(
        &mut self,
        surface: &WlSurface,
        _global: &DmabufGlobal,
    ) -> Option<DmabufFeedback> {
        // The request itself is the moment to ask the desktop too, when the surface is
        // already shown; a surface shown earlier asked when it was made.
        self.request_surface_feedback(&surface.id());
        self.surface_feedback
            .get(&surface.id())
            .and_then(|entry| entry.built.clone())
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // The desktop imports it when the buffer is attached there; the keeper reads nothing of
        // it, so every well-formed buffer is accepted here.
        if let Err(error) = notifier.successful::<Self>() {
            tracing::warn!(?error, "a dma-buf's client went away while it was imported");
        }
    }
}

impl OutputHandler for State {}

/// smithay's xdg-shell names a seat handler; the keeper's seat is its own (`seat.rs`), which
/// passes the desktop's events on as they came, so this one holds no seat.
impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.smithay_seat
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.next_tag = self.next_tag.wrapping_add(1);
        let number = self.next_tag;
        let wl_surface = surface.wl_surface().clone();
        self.set_role(
            &wl_surface,
            Role::Toplevel(Box::new(Top::new(surface, number))),
        );
        self.windows_changed = true;
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let wl_surface = surface.wl_surface().clone();
        self.set_role(
            &wl_surface,
            Role::Popup(Box::new(Pop::new(surface, positioner))),
        );
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        self.toplevel_request(surface.wl_surface(), |up, toplevel| {
            if let Some(seat) = &up.seat {
                toplevel._move(seat, serial.into());
            }
        });
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        let edges = crate::surfaces::client_resize_edge(edges);
        self.toplevel_request(surface.wl_surface(), |up, toplevel| {
            if let (Some(seat), Some(edges)) = (&up.seat, edges) {
                toplevel.resize(seat, serial.into(), edges);
            }
        });
    }

    fn show_window_menu(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        location: smithay::utils::Point<i32, smithay::utils::Logical>,
    ) {
        self.toplevel_request(surface.wl_surface(), |up, toplevel| {
            if let Some(seat) = &up.seat {
                toplevel.show_window_menu(seat, serial.into(), location.x, location.y);
            }
        });
    }

    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, serial: Serial) {
        self.popup_grab(surface.wl_surface(), serial.into());
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.with_top(surface.wl_surface(), |top| top.maximized = true);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.with_top(surface.wl_surface(), |top| top.maximized = false);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .map(|output| output.name());
        self.with_top(surface.wl_surface(), |top| top.fullscreen = (true, output));
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.with_top(surface.wl_surface(), |top| top.fullscreen = (false, None));
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.toplevel_request(surface.wl_surface(), |_, toplevel| toplevel.set_minimized());
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        let serial = match configure {
            Configure::Toplevel(configure) => configure.serial,
            Configure::Popup(configure) => configure.serial,
        };
        self.client_acked(&surface.id(), serial.into());
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        self.popup_reposition(surface.wl_surface(), positioner, token);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.role_destroyed(&surface.wl_surface().id());
        self.windows_changed = true;
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.role_destroyed(&surface.wl_surface().id());
    }

    fn client_pong(&mut self, client: smithay::wayland::shell::xdg::ShellClient) {
        self.app_ponged(&client);
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.with_top(surface.wl_surface(), |_| {});
        self.windows_changed = true;
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.with_top(surface.wl_surface(), |_| {});
        self.windows_changed = true;
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        self.with_top(surface.wl_surface(), |_| {});
    }
}

impl State {
    /// Change what a toplevel asked for and send what changed to the desktop at once.
    fn with_top(&mut self, surface: &WlSurface, change: impl FnOnce(&mut Top)) {
        let id = surface.id();
        if let Some(Role::Toplevel(top)) = self.surfaces.get_mut(&id).map(|kept| &mut kept.role) {
            change(top);
        }
        self.push_toplevel_meta(&id);
    }
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
smithay::delegate_content_type!(State);
