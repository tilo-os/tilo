//! A window whose surface is the session's lock screen: the compositor
//! shows it on every output, gives it every key and pointer event, and
//! draws nothing else until it is gone.
//!
//! The lock is the client's: it asks for one when the window is created and
//! the compositor confirms it once every output shows the lock and nothing
//! else, which is when the surface role exists and the window may draw.
//! Lifting the lock is the client's too ([`LockSurface::unlock`]), after it
//! established that the person may: the compositor takes the surface down
//! when the lock ends, however it ended.

use std::sync::{Arc, Mutex};

use sctk::compositor::Surface as CompositorSurface;
use sctk::reexports::client::protocol::wl_output::WlOutput;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Dispatch, QueueHandle};
use sctk::reexports::protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1;
use sctk::session_lock::{SessionLock, SessionLockSurface, SessionLockSurfaceData};

/// The state a window's lock surface shares between the event loop (which
/// puts the surface up when the compositor confirms the lock) and the
/// window (which draws it and ends the lock).
#[derive(Clone)]
pub struct LockSurface {
    /// The window's surface, the same object the role is made of.
    wl_surface: WlSurface,
    state: Arc<Mutex<LockState>>,
}

struct LockState {
    /// The surface before it has a role: a lock surface cannot draw until
    /// the compositor confirmed the lock and sent its size.
    wl: Option<CompositorSurface>,
    lock: SessionLock,
    /// The role, while the lock is up.
    surface: Option<SessionLockSurface>,
}

impl LockSurface {
    /// A window that asks for the session's lock.
    pub fn new(wl: CompositorSurface, lock: SessionLock) -> Self {
        let wl_surface = wl.wl_surface().clone();
        Self {
            wl_surface,
            state: Arc::new(Mutex::new(LockState {
                wl: Some(wl),
                lock,
                surface: None,
            })),
        }
    }

    /// The `wl_surface` of the window, before and after it has its role.
    pub fn wl_surface(&self) -> &WlSurface {
        &self.wl_surface
    }

    /// Whether the compositor confirmed the lock and the surface role
    /// exists: the window can draw, in the size it was configured for.
    pub fn is_up(&self) -> bool {
        self.state.lock().unwrap().surface.is_some()
    }

    /// Put the surface up on `output`, as the compositor confirmed the
    /// lock. `false` when it is up already.
    pub fn put_up<D>(&self, output: &WlOutput, queue_handle: &QueueHandle<D>) -> bool
    where
        D: Dispatch<ExtSessionLockSurfaceV1, SessionLockSurfaceData> + 'static,
    {
        let mut state = self.state.lock().unwrap();
        if state.surface.is_some() {
            return false;
        }
        let Some(wl) = state.wl.take() else {
            return false;
        };
        state.surface = Some(state.lock.create_lock_surface(wl, output, queue_handle));
        true
    }

    /// End the lock: the compositor takes every lock surface down, and the
    /// session shows its desktop again.
    pub fn unlock(&self) {
        let state = self.state.lock().unwrap();
        state.lock.unlock();
    }
}
