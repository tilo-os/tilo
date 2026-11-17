//! What setup needs from the session, and nothing more.
//!
//! The session installs one [`SessionServices`] at startup. Until it does (in a
//! test, a screenshot run, a tool), every call here does what a session
//! without that part would: nothing is woken and no app is told about a
//! browser projection.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::task::Waker;

/// The session's side of setup.
pub trait SessionServices: Send + Sync {
    /// A waker for the session loop of the calling thread: waking it makes
    /// that loop look at setup again. Made on the loop's thread.
    fn loop_waker(&self) -> Waker {
        Waker::noop().clone()
    }

    /// The verified browser projection a carried-over setup brought, which
    /// the browser is started with from now on; `None` withdraws it.
    fn browser_continuity(&self, projection: Option<PathBuf>) {
        let _ = projection;
    }

    /// Wake the session loop from any thread: the setup app has something
    /// to show.
    fn wake(&self) {}

    /// Who setup acts as: the session's person, the owner of the setup app.
    fn person(&self) -> String {
        std::env::var("TILO_USER_ID")
            .ok()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "local".to_owned())
    }
}

/// The session's services, or one that lends nothing.
struct Nothing;

impl SessionServices for Nothing {}

static SESSION: OnceLock<&'static dyn SessionServices> = OnceLock::new();

/// Install the session's services. Only the first install counts.
pub fn install(services: &'static dyn SessionServices) {
    let _ = SESSION.set(services);
}

fn services() -> &'static dyn SessionServices {
    SESSION.get().copied().unwrap_or(&Nothing)
}

/// See [`SessionServices::loop_waker`].
#[must_use]
pub fn loop_waker() -> Waker {
    services().loop_waker()
}

/// See [`SessionServices::browser_continuity`].
pub fn set_browser_continuity(projection: Option<PathBuf>) {
    services().browser_continuity(projection);
}

/// See [`SessionServices::wake`].
pub fn wake() {
    services().wake();
}

/// See [`SessionServices::person`].
#[must_use]
pub fn person() -> String {
    services().person()
}
