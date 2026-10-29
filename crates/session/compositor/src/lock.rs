//! `ext-session-lock`: while a trusted client locks the session, every
//! output shows only that client's lock surfaces and all input goes to them.
//! No window, hosted surface or other client sees a pixel or a key.

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Scale};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};

use crate::render::OutputRenderElement;
use crate::state::{ClientState, State};

pub(crate) struct Lock {
    manager: SessionLockManagerState,
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
            pending: None,
            locked: false,
            surfaces: Vec::new(),
        }
    }
}

impl State {
    /// Whether the session is locked (or a lock is being put up).
    pub(crate) fn locked(&self) -> bool {
        self.lock.locked
    }

    /// Tell the locking client the desktop is hidden: called after a frame
    /// was drawn on every output.
    pub(crate) fn confirm_lock(&mut self) {
        if let Some(locker) = self.lock.pending.take() {
            locker.lock();
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
        self.lock
            .surfaces
            .iter()
            .filter(|(of, _)| of == output)
            .flat_map(|(_, surface)| {
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
        self.lock.locked = true;
        self.lock_shown();
        self.lock.pending = Some(confirmation);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    fn unlock(&mut self) {
        self.lock.locked = false;
        self.lock_lifted();
        self.lock.pending = None;
        self.lock.surfaces.clear();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
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
