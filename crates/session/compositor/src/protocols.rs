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
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
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
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
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
    _cursor_shape: CursorShapeManagerState,
    _relative_pointer: RelativePointerManagerState,
    _pointer_constraints: PointerConstraintsState,
    _pointer_gestures: PointerGesturesState,
    _tablet: TabletManagerState,
    _idle_inhibit: IdleInhibitManagerState,
    _content_type: ContentTypeState,
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
            _cursor_shape: CursorShapeManagerState::new::<State>(display),
            _relative_pointer: RelativePointerManagerState::new::<State>(display),
            _pointer_constraints: PointerConstraintsState::new::<State>(display),
            _pointer_gestures: PointerGesturesState::new::<State>(display),
            _tablet: TabletManagerState::new::<State>(display),
            _idle_inhibit: IdleInhibitManagerState::new::<State>(display),
            _content_type: ContentTypeState::new::<State>(display),
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
        self.request_mode(toplevel, DecorationMode::ServerSide);
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
        if token_data.timestamp.elapsed() < ACTIVATION_TIMEOUT
            && let Some(window) = self.window_of(&surface)
        {
            self.focus_window(&window, SERIAL_COUNTER.next_serial());
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
smithay::delegate_idle_inhibit!(State);
smithay::delegate_keyboard_shortcuts_inhibit!(State);
smithay::delegate_content_type!(State);
