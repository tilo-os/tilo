//! What setup needs from the session that hosts it, and nothing more.
//!
//! The session installs one [`SessionHost`] at startup. Until it does (in a
//! test, a screenshot run, a tool), every call here does what a session
//! without that part would: nothing is woken and no app is told about a
//! browser projection.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::task::Waker;

/// The session's side of setup.
pub trait SessionHost: Send + Sync {
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

/// The session's host, or one that lends nothing.
struct Nothing;

impl SessionHost for Nothing {}

static HOST: OnceLock<&'static dyn SessionHost> = OnceLock::new();

/// Install the session's host. Only the first install counts.
pub fn install(host: &'static dyn SessionHost) {
    let _ = HOST.set(host);
}

fn host() -> &'static dyn SessionHost {
    HOST.get().copied().unwrap_or(&Nothing)
}

/// See [`SessionHost::loop_waker`].
#[must_use]
pub fn loop_waker() -> Waker {
    host().loop_waker()
}

/// See [`SessionHost::browser_continuity`].
pub fn set_browser_continuity(projection: Option<PathBuf>) {
    host().browser_continuity(projection);
}

/// See [`SessionHost::wake`].
pub fn wake() {
    host().wake();
}

/// See [`SessionHost::person`].
#[must_use]
pub fn person() -> String {
    host().person()
}
