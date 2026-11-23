//! What wakes the program holding a shell when what the person, the chrome or an agent
//! commands of it waits for its next pass, and what saves and shares a changed tree.

use std::rc::Rc;
use std::sync::Arc;

/// The wake installed on the shell's handle ([`crate::desktop::state::DesktopHandle::set_wake`]), called for
/// every change that waits for the shell's next pass.
///
/// Work the shell starts off its passes (a search source answering on a worker) captures the
/// shell's own wake scope, which calls the same wake: its answer asks for the pass that shows it.
#[derive(Clone, Default)]
pub struct Waker {
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    scope: Option<Rc<ui::WakeScope>>,
}

impl Waker {
    /// Ask for the shell's next pass, when something asked to be woken: what
    /// moved shows in a frame even when the shell draws nothing of its own.
    pub fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    /// Call `wake` for every change from here on, and for every answer to work the shell
    /// starts inside [`Self::enter`].
    pub fn set(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        let mut scope = {
            let _outside = ui::WakeScope::enter_detached();
            ui::WakeScope::new()
        };
        let woken = Arc::clone(&wake);
        if scope.bind(move |_| woken()).is_ok() {
            self.scope = Some(Rc::new(scope));
        }
        self.wake = Some(wake);
    }

    /// The shell's own wake scope while the guard lives: work started meanwhile asks for the
    /// shell's next pass when it is done.
    pub fn enter(&self) -> Option<ui::WakeScopeGuard> {
        self.scope.as_ref().map(|scope| scope.enter())
    }
}

/// What saves and shares a tree the shell changed ([`crate::desktop::state::DesktopHandle::set_publish`]).
pub type Publish = Rc<dyn Fn(&crate::desktop::state::DesktopHandle)>;
