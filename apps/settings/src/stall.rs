//! A device that does not answer until the test lets it go, for proving
//! that the window never waits on one. The test looks at the window while
//! the stall is still held; a window that waited on it would sit until the
//! shared hang bound, when the stall gives up and says so.

use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct State {
    open: bool,
    gave_up: bool,
}

/// A gate every call of a stalled device waits on.
#[derive(Default)]
pub struct Stall {
    state: Mutex<State>,
    released: Condvar,
}

impl Stall {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Wait until the test lets the device answer; `false` when the hang
    /// bound passed first.
    pub fn wait(&self) -> bool {
        let (mut state, _) = self
            .released
            .wait_timeout_while(
                self.state.lock().unwrap(),
                tilo_test_env::hang_bound(),
                |state| !state.open,
            )
            .unwrap();
        let open = state.open;
        state.gave_up |= !open;
        drop(state);
        open
    }

    /// Let every call answer.
    pub fn release(&self) {
        self.state.lock().unwrap().open = true;
        self.released.notify_all();
    }

    /// Stall every call from here on again.
    pub fn hold(&self) {
        self.state.lock().unwrap().open = false;
    }

    /// Fail unless the device is still stalled: `what` came back while
    /// the device had not answered, without waiting it out.
    pub fn assert_held(&self, what: &str) {
        let held = {
            let state = self.state.lock().unwrap();
            !state.open && !state.gave_up
        };
        assert!(held, "{what} waited on the stalled device");
    }
}
