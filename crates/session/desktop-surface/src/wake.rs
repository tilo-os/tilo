//! What wakes the host holding a desk when what the person, the chrome or an agent
//! commands of it waits for its next pass.

use std::rc::Rc;

/// The wake a host installed on the desk's handle ([`super::DesktopHandle::set_wake`]),
/// run for every change that waits for the desk's next pass.
#[derive(Clone, Default)]
pub(crate) struct Waker {
    wake: Option<Rc<dyn Fn()>>,
}

impl Waker {
    /// Ask the host holding the desk for its next pass, when one asked to be woken: what
    /// moved shows in a frame even when the desk draws nothing of its own.
    pub(crate) fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    /// Have `wake` run for every change from here on.
    pub(crate) fn set(&mut self, wake: Rc<dyn Fn()>) {
        self.wake = Some(wake);
    }
}
