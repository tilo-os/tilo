//! `ext-session-lock`: while a trusted client locks the session, every
//! output shows only that client's lock surfaces and all input goes to them.
//! No window, hosted surface or other client sees a pixel or a key.

use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::utils::with_renderer_surface_state;
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

/// Where a logical point is on the lock screen: the surface it lands on, where
/// the screen's own place starts, and where on the screen the point is.
pub(crate) struct LockInput {
    pub(crate) surface: WlSurface,
    pub(crate) origin: Point<f64, Logical>,
    pub(crate) at: Point<f64, Logical>,
}

pub(crate) struct Lock {
    manager: SessionLockManagerState,
    /// The lock its client put up, while that client holds the session: the
    /// lock is the client's, and ends with it.
    holder: Option<ExtSessionLockV1>,
    /// A lock asked for, confirmed once every output has drawn without the
    /// desktop.
    pending: Option<SessionLocker>,
    locked: bool,
    /// Whether the session itself asked for the lock this client holds: that
    /// lock is the session's, and a client that goes away leaves it standing
    /// for the session to show again.
    by_session: bool,
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
            by_session: false,
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

    /// Whether the lock client has drawn: a buffer is on the surface the
    /// outputs show, so what they show is the lock screen's own rather than
    /// the desk's last frame.
    pub(crate) fn lock_drawn(&self) -> bool {
        self.lock.surfaces.iter().any(|(_, surface)| {
            with_renderer_surface_state(surface.wl_surface(), |state| state.buffer().is_some())
                .unwrap_or(false)
        })
    }

    /// Whether `output` shows a lock screen now, with a frame of its own on
    /// it: what a capture of it would show.
    pub(crate) fn lock_shows(&self, output: &Output) -> bool {
        self.lock_shown_on(output).is_some_and(|(surface, _)| {
            with_renderer_surface_state(surface.wl_surface(), |state| state.buffer().is_some())
                .unwrap_or(false)
        })
    }

    /// Tell the locking client the desktop is hidden: called after a frame
    /// was drawn on every output.
    pub(crate) fn confirm_lock(&mut self) {
        if let Some(locker) = self.lock.pending.take() {
            locker.lock();
        }
    }

    /// Let go of the lock when the client that put it up is gone: what it drew
    /// and the input it took with it. A lock the session itself asked for
    /// stands — the session starts its lock screen again, and the lock ends
    /// only with the lock the person lifts — while a lock that was only the
    /// client's ends here.
    pub(crate) fn lift_lock_of_departed_client(&mut self) {
        let Some(holder) = &self.lock.holder else {
            return;
        };
        if holder.is_alive() {
            return;
        }
        if !self.lock.by_session {
            self.lift_lock();
            return;
        }
        self.lock.holder = None;
        self.mark_redraw_all();
        self.lock.locked = false;
        self.lock.by_session = false;
        self.lock.pending = None;
        self.lock.surfaces.clear();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    /// End the session's lock: the client that held it lifted it, or is gone.
    /// The scene it covered is drawn again on the next frame.
    fn lift_lock(&mut self) {
        self.lock.holder = None;
        self.mark_redraw_all();
        self.lock.locked = false;
        self.lock.by_session = false;
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

    /// The lock screen an output shows, and how much of the screen fills it:
    /// the surface made for that output, else the first one, stretched to the
    /// output's size. The lock is one screen — the session drew it once and
    /// showed it on every display — so an output a lock surface was not made
    /// for shows the whole screen, of whatever size it is, rather than the
    /// corner of one made for a larger display.
    fn lock_shown_on(&self, output: &Output) -> Option<(&LockSurface, Scale<f64>)> {
        let size = self.space.output_geometry(output)?.size;
        let surface = self.lock_surfaces_on(output).into_iter().next()?;
        let drawn = surface
            .current_state()
            .size
            .filter(|drawn| drawn.w > 0 && drawn.h > 0);
        let stretch = drawn.map_or_else(
            || Scale::from(1.0),
            |drawn| {
                Scale::from((
                    f64::from(size.w) / f64::from(drawn.w),
                    f64::from(size.h) / f64::from(drawn.h),
                ))
            },
        );
        Some((surface, stretch))
    }

    /// The lock screen under a logical point, and where on it: the surface the
    /// point lands on, where the screen's own place starts, and the point moved
    /// with the screen stretched onto the display it is on, so a person
    /// pointing at the lock screen on any display points at the place they see
    /// on it.
    pub(crate) fn lock_surface_under(&self, point: Point<f64, Logical>) -> Option<LockInput> {
        let (origin, here) = self.space.outputs().find_map(|output| {
            let geometry = self.space.output_geometry(output)?;
            geometry
                .to_f64()
                .contains(point)
                .then(|| (geometry.loc.to_f64(), output))
        })?;
        let (surface, stretch) = self.lock_shown_on(here)?;
        let at = Point::from((
            origin.x + (point.x - origin.x) * stretch.x,
            origin.y + (point.y - origin.y) * stretch.y,
        ));
        Some(LockInput {
            surface: surface.wl_surface().clone(),
            origin,
            at,
        })
    }

    /// Where on the lock screen a point on a display is, or the point itself
    /// when no lock screen is up there.
    pub(crate) fn lock_point(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        self.lock_surface_under(point)
            .map_or(point, |under| under.at)
    }

    /// The elements an output shows while locked: its lock screen only, the
    /// whole of it, stretched to fill the output when it was made for another.
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
        let Some((surface, stretch)) = self.lock_shown_on(output) else {
            return Vec::new();
        };
        let elements: Vec<WaylandSurfaceRenderElement<R>> = render_elements_from_surface_tree(
            renderer,
            surface.wl_surface(),
            (0, 0),
            scale,
            1.0,
            Kind::Unspecified,
        );
        if stretch == Scale::from(1.0) {
            return elements
                .into_iter()
                .map(OutputRenderElement::Surface)
                .collect();
        }
        elements
            .into_iter()
            .map(|element| {
                OutputRenderElement::ScaledSurface(RescaleRenderElement::from_element(
                    element,
                    Point::from((0, 0)),
                    stretch,
                ))
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
        self.lock.by_session = self.idle.session.is_locked();
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
