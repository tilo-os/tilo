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

use crate::data_device::DataDevice;
use crate::outputs::KeptOutput;
use crate::pointer_constraints::PointerConstraints;
use crate::pointer_gestures::PointerGestures;
use crate::primary_selection::PrimarySelection;
use crate::relative_pointer::RelativePointers;
use crate::seat::SeatMirror;
use crate::surfaces::{Buffers, Kept, Pop, Role, Top};
use crate::text_input::TextInput;
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
    pub(crate) dmabuf: DmabufState,
    /// The dma-buf global the app is offered, once a desktop offered one.
    pub(crate) dmabuf_offer: Option<crate::dmabuf::Offered>,
    /// The app-facing globals this keeper chose to offer, once a desktop offered them.
    pub(crate) viewporter: Option<smithay::wayland::viewporter::ViewporterState>,
    pub(crate) fractional_scale:
        Option<smithay::wayland::fractional_scale::FractionalScaleManagerState>,
    pub(crate) content_type: Option<smithay::wayland::content_type::ContentTypeState>,
    /// The app-facing decoration managers, once a desktop offered them.
    pub(crate) xdg_decorations:
        Option<smithay::wayland::shell::xdg::decoration::XdgDecorationState>,
    pub(crate) kde_decorations:
        std::sync::OnceLock<smithay::wayland::shell::kde::decoration::KdeDecorationState>,
    /// The app-facing activation global, and the tokens the desktop still owes.
    pub(crate) activation_global: Option<smithay::reexports::wayland_server::backend::GlobalId>,
    pub(crate) activation_pending: crate::activation::PendingTokens,
    /// The app-facing presentation global, its clock, and the feedback the desktop owes.
    pub(crate) presentation_global: Option<smithay::reexports::wayland_server::backend::GlobalId>,
    pub(crate) presentation_clock: Option<u32>,
    pub(crate) presentation_pending: crate::presentation::Pending,
    pub(crate) presentation_flight: crate::presentation::Flights,
    /// The app-facing cursor-shape manager, the shape each app pointer named, and the desktop's
    /// device for each while attached.
    pub(crate) cursor_shape_global: Option<crate::cursor_shape::Global>,
    pub(crate) cursor_shapes: crate::cursor_shape::Shapes,
    pub(crate) cursor_shape_devices: crate::cursor_shape::Devices,
    /// The app-facing inhibitor globals, once a desktop offered them.
    pub(crate) idle_inhibit: Option<smithay::wayland::idle_inhibit::IdleInhibitManagerState>,
    pub(crate) shortcuts_inhibit_global:
        Option<smithay::reexports::wayland_server::backend::GlobalId>,
    /// The app-facing dialog global, once a desktop offered it.
    pub(crate) dialog: Option<smithay::wayland::shell::xdg::dialog::XdgDialogState>,
    /// The app-facing window-icon manager, whose sizes the desktop asked for.
    pub(crate) icons: Option<smithay::wayland::xdg_toplevel_icon::XdgToplevelIconManager>,
    pub(crate) icon_sizes: Vec<i32>,
    /// The app's colour management, kept across desktops.
    pub(crate) color: crate::color::Color,
    /// What the desktop answered each surface's own feedback request, kept while it is away.
    pub(crate) surface_feedback: crate::dmabuf::SurfaceFeedbacks,
    /// Every live surface of the app, by its id.
    pub(crate) surfaces: HashMap<smithay::reexports::wayland_server::backend::ObjectId, Kept>,
    pub(crate) next_seq: u64,
    pub(crate) buffers: Buffers,
    pub(crate) seat: SeatMirror,
    pub(crate) data_device: DataDevice,
    pub(crate) xdnd: crate::xdnd::Xdnd,
    /// The X11 server's windows, and which of its toplevels shows each.
    pub(crate) x11_windows: Vec<crate::x11_windows::X11Window>,
    pub(crate) primary: PrimarySelection,
    pub(crate) text_input: TextInput,
    pub(crate) pointer_constraints: PointerConstraints,
    pub(crate) gestures: PointerGestures,
    pub(crate) relative: RelativePointers,
    pub(crate) tablets: crate::tablet::TabletMirror,
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
            dmabuf_offer: None,
            viewporter: None,
            fractional_scale: None,
            content_type: None,
            xdg_decorations: None,
            kde_decorations: std::sync::OnceLock::new(),
            activation_global: None,
            activation_pending: HashMap::new(),
            presentation_global: None,
            presentation_clock: None,
            presentation_pending: HashMap::new(),
            presentation_flight: crate::presentation::Flights::default(),
            cursor_shape_global: None,
            cursor_shapes: HashMap::new(),
            cursor_shape_devices: HashMap::new(),
            idle_inhibit: None,
            shortcuts_inhibit_global: None,
            dialog: None,
            icons: None,
            icon_sizes: Vec::new(),
            color: crate::color::Color::default(),
            surface_feedback: crate::dmabuf::SurfaceFeedbacks::default(),
            surfaces: HashMap::new(),
            next_seq: 0,
            buffers: Buffers::default(),
            seat,
            data_device: DataDevice::new(dh),
            xdnd: crate::xdnd::Xdnd::default(),
            x11_windows: Vec::new(),
            primary: PrimarySelection::new(dh),
            text_input: TextInput::new(dh),
            pointer_constraints: PointerConstraints::new(dh),
            gestures: PointerGestures::new(dh),
            relative: RelativePointers::new(dh),
            tablets: crate::tablet::TabletMirror::default(),
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
            "xdg_activation_v1" => self.offer_activation(),
            "zwp_idle_inhibit_manager_v1" => self.offer_idle_inhibit(),
            "zwp_keyboard_shortcuts_inhibit_manager_v1" => self.offer_shortcuts_inhibit(),
            "xdg_wm_dialog_v1" => self.offer_dialog(),
            "zwp_tablet_manager_v2" => self.offer_tablet_manager(),
            _ => self.offer_decorations(interface),
        }
    }

    pub(crate) fn shm_state_mut(&mut self) -> &mut ShmState {
        &mut self.shm
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
        self.absorb_icon(surface);
        self.first_configure_while_away(surface);
        self.presentation_committed(surface);
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
        // already shown; a surface shown later asks when it is made there. smithay holds the
        // surface's state while it asks this, so nothing here may take it again.
        self.ask_surface_feedback(&surface.id());
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
