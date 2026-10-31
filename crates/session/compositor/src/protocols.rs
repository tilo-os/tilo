//! The protocols beyond the core shell that ordinary clients expect:
//! buffers (dma-buf, viewports), scaling, presentation timing, decorations,
//! activation, the primary selection, pointer shape, constraints and
//! gestures, tablets, and the inhibitors (idle, shortcuts).

use std::collections::HashSet;

use smithay::backend::allocator::Format;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::desktop::Window;
use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::Mode as KdeMode;
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration;
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeDefaultMode;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::WEnum;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::with_states;
use smithay::wayland::content_type::ContentTypeState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::backend::drm::{DrmNode, NodeType};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale};
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};
use smithay::wayland::pointer_constraints::{PointerConstraintsHandler, PointerConstraintsState};
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::selection::primary_selection::{PrimarySelectionHandler, PrimarySelectionState};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::kde::decoration::{KdeDecorationHandler, KdeDecorationState};
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::dialog::{XdgDialogHandler, XdgDialogState};
use smithay::wayland::tablet_manager::{TabletManagerState, TabletSeatHandler};
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};

use crate::state::State;

/// How long an activation token stays good: a launch that takes longer
/// than this no longer steals focus.
const ACTIVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The globals and state of these protocols.
pub(crate) struct Protocols {
    dmabuf: DmabufState,
    dmabuf_global: Option<DmabufGlobal>,
    activation: XdgActivationState,
    pub(crate) primary_selection: PrimarySelectionState,
    shortcuts_inhibit: KeyboardShortcutsInhibitState,
    /// Surfaces that keep the session from idling (a video playing).
    pub(crate) idle_inhibitors: HashSet<WlSurface>,
    _viewporter: ViewporterState,
    _fractional_scale: FractionalScaleManagerState,
    _presentation: PresentationState,
    _decoration: XdgDecorationState,
    /// `org_kde_kwin_server_decoration_manager`: the older spelling of the
    /// same question for toolkits that only speak it.
    kde_decoration: KdeDecorationState,
    _cursor_shape: CursorShapeManagerState,
    _relative_pointer: RelativePointerManagerState,
    _pointer_constraints: PointerConstraintsState,
    _pointer_gestures: PointerGesturesState,
    _tablet: TabletManagerState,
    _idle_inhibit: IdleInhibitManagerState,
    _content_type: ContentTypeState,
    _dialog: XdgDialogState,
}

impl Protocols {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        Self {
            dmabuf: DmabufState::new(),
            dmabuf_global: None,
            activation: XdgActivationState::new::<State>(display),
            primary_selection: PrimarySelectionState::new::<State>(display),
            shortcuts_inhibit: KeyboardShortcutsInhibitState::new::<State>(display),
            idle_inhibitors: HashSet::new(),
            _viewporter: ViewporterState::new::<State>(display),
            _fractional_scale: FractionalScaleManagerState::new::<State>(display),
            // CLOCK_MONOTONIC: frame times are compared with input times.
            _presentation: PresentationState::new::<State>(display, 1),
            _decoration: XdgDecorationState::new::<State>(display),
            // A client that never states a preference is decorated by the
            // desktop, like every other.
            kde_decoration: KdeDecorationState::new::<State>(display, KdeDefaultMode::Server),
            _cursor_shape: CursorShapeManagerState::new::<State>(display),
            _relative_pointer: RelativePointerManagerState::new::<State>(display),
            _pointer_constraints: PointerConstraintsState::new::<State>(display),
            _pointer_gestures: PointerGesturesState::new::<State>(display),
            _tablet: TabletManagerState::new::<State>(display),
            _idle_inhibit: IdleInhibitManagerState::new::<State>(display),
            _content_type: ContentTypeState::new::<State>(display),
            _dialog: XdgDialogState::new::<State>(display),
        }
    }
}

impl State {
    /// Offer `linux-dmabuf` with the formats the backend's renderer
    /// imports. Called once by a backend that can import dma-bufs.
    ///
    /// With the GPU the compositor renders on (`device`), clients are told
    /// to allocate there (default feedback); without it, GL clients find no
    /// GPU and fall back to software rendering.
    pub(crate) fn enable_dmabuf(&mut self, formats: Vec<Format>, device: Option<DrmNode>) {
        if self.protocols.dmabuf_global.is_some() {
            return;
        }
        let display = self.display.clone();
        let feedback = device.and_then(|node| {
            // Clients allocate on the render node of the same GPU.
            let render = node
                .node_with_type(NodeType::Render)
                .and_then(Result::ok)
                .unwrap_or(node);
            DmabufFeedbackBuilder::new(render.dev_id(), formats.iter().copied())
                .build()
                .map_err(|error| tracing::warn!(?error, "dmabuf feedback"))
                .ok()
        });
        let global = match feedback {
            Some(feedback) => self
                .protocols
                .dmabuf
                .create_global_with_default_feedback::<Self>(&display, &feedback),
            None => self
                .protocols
                .dmabuf
                .create_global::<Self>(&display, formats),
        };
        self.protocols.dmabuf_global = Some(global);
    }

    /// Whether a client keeps the session from idling.
    pub(crate) fn idle_inhibited(&self) -> bool {
        self.protocols
            .idle_inhibitors
            .iter()
            .any(smithay::reexports::wayland_server::Resource::is_alive)
    }

    fn window_of(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .cloned()
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.protocols.dmabuf
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // The formats offered are the renderer's; a buffer that still fails
        // to import shows as nothing and is reported when drawn.
        if notifier.successful::<Self>().is_err() {
            tracing::debug!("dma-buf import: client gone");
        }
    }
}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // The scale of the output the surface is on, else the largest.
        let scale = self
            .space
            .outputs()
            .map(|output| output.current_scale().fractional_scale())
            .fold(1.0_f64, f64::max);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // The desktop draws window bars around every window.
        toplevel
            .with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ServerSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        toplevel
            .with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        XdgDecorationHandler::request_mode(self, toplevel, DecorationMode::ServerSide);
    }
}

/// `org_kde_kwin_server_decoration`: the same answer as
/// [`XdgDecorationHandler`] for clients that only speak the KDE spelling.
/// The window's frame is the tab the desktop draws, so the mode is `Server`
/// whatever the client asks for.
impl KdeDecorationHandler for State {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.protocols.kde_decoration
    }

    /// A new decoration is told the mode once, here: the protocol sends
    /// `mode` for a change, and the mode never changes.
    fn new_decoration(&mut self, _surface: &WlSurface, decoration: &OrgKdeKwinServerDecoration) {
        decoration.mode(KdeMode::Server);
    }

    /// Left unanswered on purpose. A client acknowledges a `mode` with a
    /// `request_mode`, and GTK 3 acknowledges with the mode it wants, so
    /// repeating `Server` for each request gave a window that draws its own
    /// frame (Firefox) an exchange that ended only with the window.
    fn request_mode(
        &mut self,
        _surface: &WlSurface,
        _decoration: &OrgKdeKwinServerDecoration,
        _mode: WEnum<KdeMode>,
    ) {
        // The request is the client's preference, not a fact about the
        // window: the frame stays the desktop's.
    }
}

impl Protocols {
    /// A token for an activation the session itself caused (the person
    /// chose a notification, so its app should come to the front): handing
    /// it to that app lets it raise its window the way a click on a fresh
    /// window would, without the app having to be linked to an input event
    /// it never saw.
    pub(crate) fn external_activation_token(&mut self) -> String {
        let data = XdgActivationTokenData {
            timestamp: std::time::Instant::now(),
            ..XdgActivationTokenData::default()
        };
        let (token, _) = self.activation.create_external_token(Some(data));
        token.as_str().to_owned()
    }
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.protocols.activation
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(window) = self.window_of(&surface) {
            let fresh = token_data.timestamp.elapsed() < ACTIVATION_TIMEOUT;
            self.activation_requested(&window, fresh);
        }
        self.protocols.activation.remove_token(&token);
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.protocols.primary_selection
    }
}

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {}

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
    }
}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.insert(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.remove(&surface);
    }
}

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.protocols.shortcuts_inhibit
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Remote desktops and VMs take every key while focused.
        inhibitor.activate();
    }
}

impl TabletSeatHandler for State {}

smithay::delegate_dmabuf!(State);
smithay::delegate_viewporter!(State);
smithay::delegate_fractional_scale!(State);
smithay::delegate_presentation!(State);
smithay::delegate_xdg_decoration!(State);
smithay::delegate_kde_decoration!(State);

/// A dialog a client marks modal keeps the keyboard from the window it
/// belongs to: focusing that window focuses the dialog instead.
impl XdgDialogHandler for State {
    fn modal_changed(&mut self, toplevel: ToplevelSurface, is_modal: bool) {
        let Some(parent) = toplevel.parent() else {
            return;
        };
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        if !is_modal || focused.as_ref() != Some(&parent) {
            return;
        }
        let window = self
            .space
            .elements()
            .find(|window| window.toplevel().is_some_and(|own| own == &toplevel))
            .cloned();
        if let Some(window) = window {
            self.focus_window(&window, smithay::utils::SERIAL_COUNTER.next_serial());
        }
    }
}

smithay::delegate_xdg_dialog!(State);
smithay::delegate_xdg_activation!(State);
smithay::delegate_primary_selection!(State);
smithay::delegate_cursor_shape!(State);
smithay::delegate_relative_pointer!(State);
smithay::delegate_pointer_constraints!(State);
smithay::delegate_pointer_gestures!(State);
// The tablet manager's requests go through `crate::tablet`, which notes
// the clients that take tablets; everything else is smithay's.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_manager_v2::ZwpTabletManagerV2: ()
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_seat_v2::ZwpTabletSeatV2: smithay::wayland::tablet_manager::TabletSeatUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_tool_v2::ZwpTabletToolV2: smithay::wayland::tablet_manager::TabletToolUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_v2::ZwpTabletV2: smithay::wayland::tablet_manager::TabletUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_pad_v2::ZwpTabletPadV2: smithay::wayland::tablet_manager::TabletPadUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2: smithay::wayland::tablet_manager::TabletPadUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2: smithay::wayland::tablet_manager::TabletPadUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2: smithay::wayland::tablet_manager::TabletPadUserData
] => TabletManagerState);
smithay::delegate_idle_inhibit!(State);
smithay::delegate_keyboard_shortcuts_inhibit!(State);
smithay::delegate_content_type!(State);
