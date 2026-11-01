//! `ext-session-lock`: while a trusted client locks the session, every
//! output shows only that client's lock surfaces and all input goes to them.
//! No window, hosted surface or other client sees a pixel or a key.

use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::{Kind, RenderElementStates};
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree,
    surface_presentation_feedback_flags_from_states, take_presentation_feedback_surface_tree,
};
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind as FeedbackKind;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource as _};
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Scale};
use smithay::wayland::seat::WaylandFocus as _;
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};

use crate::render::OutputRenderElement;
use crate::state::{ClientState, State};

pub(crate) struct Lock {
    manager: SessionLockManagerState,
    /// The lock its client put up, while that client holds the session: the
    /// lock is the client's, and ends with it.
    holder: Option<ExtSessionLockV1>,
    /// A lock asked for, confirmed once every output has drawn without the
    /// desktop.
    pending: Option<SessionLocker>,
    locked: bool,
    surfaces: Vec<(Output, LockSurface)>,
}

impl Lock {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        // Only clients the session started as trusted may lock it.
        let manager = SessionLockManagerState::new::<State, _>(display, |client: &Client| {
            client
                .get_data::<ClientState>()
                .is_some_and(ClientState::trusted)
        });
        Self {
            manager,
            holder: None,
            pending: None,
            locked: false,
            surfaces: Vec::new(),
        }
    }
}

impl State {
    /// Whether the session is locked (or a lock is being put up): by a
    /// lock client, or by the session itself (idle, a key, a command), when
    /// its lock screen shows.
    pub(crate) fn locked(&self) -> bool {
        self.lock.locked || self.idle.session.is_locked()
    }

    /// Whether the session's own lock screen is drawing: a hosted lock
    /// screen with layers to show. A lock client's surfaces are its own,
    /// and show without it.
    pub(crate) fn lock_screen_drawn(&self) -> bool {
        self.hosts
            .iter()
            .any(|host| host.lock_screen() && !host.layers().is_empty())
    }

    /// Whether the window with the keyboard is fullscreen: it covers its
    /// display and, being a game or a video, owns the input that plays it.
    pub(crate) fn fullscreen(&self) -> bool {
        let Some(focus) = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
        else {
            return false;
        };
        self.space.elements().any(|window| {
            let focused = window
                .wl_surface()
                .is_some_and(|surface| surface.as_ref() == &focus)
                || window
                    .x11_surface()
                    .is_some_and(|x11| x11.wl_surface() == Some(focus.clone()));
            focused
                && (window.toplevel().is_some_and(|toplevel| {
                    toplevel.current_state().states.contains(
                        smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
                    )
                }) || window
                    .x11_surface()
                    .is_some_and(smithay::xwayland::X11Surface::is_fullscreen))
        })
    }

    /// Tell the locking client the desktop is hidden: called after a frame
    /// was drawn on every output.
    pub(crate) fn confirm_lock(&mut self) {
        if let Some(locker) = self.lock.pending.take() {
            locker.lock();
        }
    }

    /// Lift the lock when the client that put it up is gone: a lock ends with
    /// its client, and one held for a client that can no longer draw or
    /// answer leaves the session locked to nothing — no screen to show, and no
    /// way to lift it.
    pub(crate) fn lift_lock_of_departed_client(&mut self) {
        let Some(holder) = &self.lock.holder else {
            return;
        };
        if holder.is_alive() {
            return;
        }
        self.lift_lock();
    }

    /// End the session's lock: the client that held it lifted it, or is gone.
    /// The scene it covered is drawn again on the next frame.
    fn lift_lock(&mut self) {
        self.lock.holder = None;
        self.mark_redraw_all();
        self.lock.locked = false;
        self.lock_lifted();
        self.lock.pending = None;
        self.lock.surfaces.clear();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    /// Tell the lock screen its frame was shown: a client draws its next
    /// frame after the one it committed is on screen.
    pub(crate) fn lock_frame_drawn(
        &self,
        output: &Output,
        time: std::time::Duration,
        feedback: &mut OutputPresentationFeedback,
        states: Option<&RenderElementStates>,
    ) {
        for surface in self.lock_surfaces_on(output) {
            let surface = surface.wl_surface();
            send_frames_surface_tree(
                surface,
                output,
                time,
                Some(std::time::Duration::ZERO),
                |_, _| Some(output.clone()),
            );
            take_presentation_feedback_surface_tree(
                surface,
                feedback,
                |_, _| Some(output.clone()),
                |surface, _| {
                    states.map_or_else(FeedbackKind::empty, |states| {
                        surface_presentation_feedback_flags_from_states(surface, states)
                    })
                },
            );
        }
    }

    /// The lock surfaces `output` shows: its own, or the first one's when it
    /// has none of its own.
    fn lock_surfaces_on(&self, output: &Output) -> Vec<&LockSurface> {
        let own: Vec<&LockSurface> = self
            .lock
            .surfaces
            .iter()
            .filter(|(of, _)| of == output)
            .map(|(_, surface)| surface)
            .collect();
        if own.is_empty() {
            self.lock
                .surfaces
                .first()
                .map(|(_, surface)| surface)
                .into_iter()
                .collect()
        } else {
            own
        }
    }

    /// The lock surface under a logical point and its origin.
    pub(crate) fn lock_surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.lock.surfaces.iter().find_map(|(output, surface)| {
            let geometry = self.space.output_geometry(output)?;
            geometry
                .to_f64()
                .contains(point)
                .then(|| (surface.wl_surface().clone(), geometry.loc.to_f64()))
        })
    }

    /// The elements an output shows while locked: its lock surface only.
    /// An output no lock surface was made for shows the first one's: a
    /// client that locks with one window still covers every output, rather
    /// than leaving one of them blank for whoever looks at it.
    pub(crate) fn lock_elements<R>(
        &self,
        renderer: &mut R,
        output: &Output,
    ) -> Vec<OutputRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        let scale = Scale::from(output.current_scale().fractional_scale());
        self.lock_surfaces_on(output)
            .into_iter()
            .flat_map(|surface| {
                render_elements_from_surface_tree(
                    renderer,
                    surface.wl_surface(),
                    (0, 0),
                    scale,
                    1.0,
                    Kind::Unspecified,
                )
            })
            .collect()
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.lock.manager
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        self.lock.holder = Some(confirmation.ext_session_lock().clone());
        self.lock.locked = true;
        self.lock_shown();
        self.lock.pending = Some(confirmation);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    fn unlock(&mut self) {
        self.lift_lock();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else {
            return;
        };
        if let Some(mode) = output.current_mode() {
            let scale = output.current_scale().fractional_scale();
            let size = mode.size.to_f64().to_logical(scale).to_i32_round::<i32>();
            #[allow(clippy::cast_sign_loss, reason = "output sizes are positive")]
            surface.with_pending_state(|state| {
                state.size = Some((size.w as u32, size.h as u32).into());
            });
        }
        surface.send_configure();
        let focus = surface.wl_surface().clone();
        self.lock.surfaces.push((output, surface));
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(focus), SERIAL_COUNTER.next_serial());
        }
    }
}

smithay::delegate_session_lock!(State);
