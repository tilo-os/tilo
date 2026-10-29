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
}

static HOST: OnceLock<&'static dyn SessionHost> = OnceLock::new();

/// Install the session's host. Only the first install counts.
pub fn install(host: &'static dyn SessionHost) {
    let _ = HOST.set(host);
}

fn host() -> Option<&'static dyn SessionHost> {
    HOST.get().copied()
}

/// See [`SessionHost::loop_waker`].
#[must_use]
pub fn loop_waker() -> Waker {
    host().map_or_else(|| Waker::noop().clone(), SessionHost::loop_waker)
}

/// See [`SessionHost::browser_continuity`].
pub fn set_browser_continuity(projection: Option<PathBuf>) {
    if let Some(host) = host() {
        host.browser_continuity(projection);
    }
}
