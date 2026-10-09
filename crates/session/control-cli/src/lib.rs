//! Where a running Tilo desktop's control sockets live.
//!
//! Both are in the session's private runtime directory, never a shared one: a
//! well-known name under `/tmp` could be bound first by anyone, capturing every
//! command a client sends.
//!
//! - The device admin's control socket, [`admin_socket_path`]: the action
//!   model, served by every build in the device admin's session once first
//!   login is complete.
//! - The development test-control socket, [`default_socket_path`] or
//!   `$TILO_TEST_CONTROL`: the action model plus synthetic input and
//!   screenshots, bound only by development builds that are told where.

use std::ffi::OsString;
use std::path::PathBuf;

/// Environment variable naming the test-control socket; the compositor
/// listens there only when it is set and development overrides are compiled
/// in.
pub const TEST_CONTROL_ENV: &str = "TILO_TEST_CONTROL";

/// Directory under `$XDG_RUNTIME_DIR` shared with the other per-session
/// endpoints the compositor publishes.
pub const TEST_CONTROL_RUNTIME_SUBDIR: &str = "tilo-session";

/// File name of the default test-control socket inside
/// [`TEST_CONTROL_RUNTIME_SUBDIR`].
pub const TEST_CONTROL_SOCKET_FILE: &str = "test-control.sock";

/// File name of the device admin's control socket inside
/// [`TEST_CONTROL_RUNTIME_SUBDIR`]. Unlike the test-control socket it needs no
/// environment variable and no development build.
pub const ADMIN_CONTROL_SOCKET_FILE: &str = "admin-control.sock";

/// `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`, or `None` when the
/// runtime directory is unset or not absolute.
#[must_use]
pub fn default_socket_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    in_session_dir(runtime_dir, TEST_CONTROL_SOCKET_FILE)
}

/// `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock`, or `None` when the
/// runtime directory is unset or not absolute.
#[must_use]
pub fn admin_socket_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    in_session_dir(runtime_dir, ADMIN_CONTROL_SOCKET_FILE)
}

fn in_session_dir(runtime_dir: Option<OsString>, file: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(runtime_dir?);
    dir.is_absolute()
        .then(|| dir.join(TEST_CONTROL_RUNTIME_SUBDIR).join(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_live_under_an_absolute_runtime_dir_only() {
        assert_eq!(
            default_socket_path(Some("/run/user/1000".into())),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/test-control.sock"
            ))
        );
        assert_eq!(
            admin_socket_path(Some("/run/user/1000".into())),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/admin-control.sock"
            ))
        );
        for runtime in [None, Some(OsString::new()), Some("run/user".into())] {
            assert_eq!(default_socket_path(runtime.clone()), None);
            assert_eq!(admin_socket_path(runtime), None);
        }
    }
}

mod client;
#[cfg(target_os = "linux")]
pub use client::KernelPeer;
pub use client::{ControlClient, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};
