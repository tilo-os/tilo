//! Whether this process runs inside a Tilo login, and where that login's
//! compositor publishes its endpoints: the private [`SESSION_DIR`] under the
//! login's runtime directory. The compositor and every client that looks for
//! it (the runtime's agent door, `tilo-control`, the runtime portal) resolve
//! these paths here, so they cannot drift apart.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The desktop name a Tilo login announces in `XDG_CURRENT_DESKTOP`.
pub const TILO_DESKTOP: &str = "tilo";
/// The session's private directory under `XDG_RUNTIME_DIR`, holding the
/// compositor's sockets and the facts it publishes for the login.
pub const SESSION_DIR: &str = "tilo-session";
/// The agent door's file name in [`SESSION_DIR`].
pub const AGENT_SOCKET_FILE: &str = "agent.sock";
/// The device admin's control socket's file name in [`SESSION_DIR`].
pub const ADMIN_CONTROL_SOCKET_FILE: &str = "admin-control.sock";
/// File in [`SESSION_DIR`] naming the room of the workspace in front: the
/// room a command started now (an app run from a terminal, or by
/// `tilo-app dev`) joins, as an app the shell starts joins the room it names
/// with the launcher's `--workspace-id`.
pub const WORKSPACE_ID_FILE: &str = "workspace-id";

/// The sockets a Tilo compositor serves in [`SESSION_DIR`] for the life of
/// its session. Either one's presence is the session's own marker.
const SESSION_SOCKETS: [&str; 2] = [AGENT_SOCKET_FILE, ADMIN_CONTROL_SOCKET_FILE];

/// [`SESSION_DIR`] under `runtime_dir`; `None` when there is no runtime
/// directory or it is not absolute, which leaves no private place for the
/// session's files (guessing `/tmp` would put them somewhere world-writable).
#[must_use]
pub fn session_dir(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    let runtime_dir = PathBuf::from(runtime_dir?);
    runtime_dir
        .is_absolute()
        .then(|| runtime_dir.join(SESSION_DIR))
}

/// Whether an `XDG_CURRENT_DESKTOP` value names a Tilo login. The value is a
/// colon list (a Tilo session exports `tilo:GNOME`), so Tilo is one entry of
/// it, never necessarily the whole value.
#[must_use]
pub fn names_tilo(current_desktop: Option<&OsStr>) -> bool {
    current_desktop
        .and_then(OsStr::to_str)
        .is_some_and(|value| {
            value
                .split(':')
                .any(|part| part.eq_ignore_ascii_case(TILO_DESKTOP))
        })
}

/// The directory this login's runtime files live in: `XDG_RUNTIME_DIR` when
/// it is set and absolute, else the account's `/run/user/<uid>` where
/// `exists` says it is there. A terminal multiplexer, `ssh` or a service
/// manager can each start a process without the variable, and the directory
/// is where the session put its sockets all the same.
#[must_use]
pub fn login_runtime_dir_from(
    runtime_dir: Option<OsString>,
    uid: u32,
    exists: impl FnOnce(&Path) -> bool,
) -> Option<PathBuf> {
    let named = runtime_dir
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    named.or_else(|| {
        let conventional = PathBuf::from(format!("/run/user/{uid}"));
        exists(&conventional).then_some(conventional)
    })
}

/// This process's login runtime directory ([`login_runtime_dir_from`]).
#[cfg(unix)]
#[must_use]
pub fn login_runtime_dir() -> Option<PathBuf> {
    login_runtime_dir_from(
        crate::var("XDG_RUNTIME_DIR"),
        rustix::process::geteuid().as_raw(),
        Path::is_dir,
    )
}

/// This process's login runtime directory: none outside Unix.
#[cfg(not(unix))]
#[must_use]
pub fn login_runtime_dir() -> Option<PathBuf> {
    None
}

/// Whether `runtime_dir` holds a Tilo session's marker: one of the
/// compositor's own sockets in its private [`SESSION_DIR`]. Only a
/// compositor of this account creates them, in a directory only this
/// account can write, so they say "this login is a Tilo desktop" without any
/// environment variable a launcher may have dropped on the way.
#[cfg(unix)]
#[must_use]
pub fn has_session_marker(runtime_dir: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    let session = runtime_dir.join(SESSION_DIR);
    SESSION_SOCKETS.iter().any(|name| {
        std::fs::symlink_metadata(session.join(name))
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    })
}

/// Whether `runtime_dir` holds a Tilo session's marker: never outside Unix.
#[cfg(not(unix))]
#[must_use]
pub fn has_session_marker(_runtime_dir: &Path) -> bool {
    let _ = SESSION_SOCKETS;
    false
}

/// Whether a process with these facts runs inside a Tilo login: its
/// environment names the desktop, **or** its account's runtime directory
/// holds the session's own marker. The environment is a hint that can only
/// add: a process that lost it (tmux, ssh, a service) is still inside the
/// login its runtime directory belongs to.
#[must_use]
pub fn in_tilo_login(
    current_desktop: Option<&OsStr>,
    runtime_dir: Option<&Path>,
    marker: impl FnOnce(&Path) -> bool,
) -> bool {
    names_tilo(current_desktop) || runtime_dir.is_some_and(marker)
}

/// Whether this process runs inside a Tilo login ([`in_tilo_login`]).
#[must_use]
pub fn is_tilo_login() -> bool {
    in_tilo_login(
        crate::var("XDG_CURRENT_DESKTOP").as_deref(),
        login_runtime_dir().as_deref(),
        has_session_marker,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tilo_login_is_one_entry_of_the_colon_list() {
        for value in ["tilo", "tilo:GNOME", "GNOME:tilo", "Tilo"] {
            assert!(names_tilo(Some(OsStr::new(value))), "{value}");
        }
        for value in ["", "GNOME", "tilox:GNOME", "KDE"] {
            assert!(!names_tilo(Some(OsStr::new(value))), "{value}");
        }
        assert!(!names_tilo(None));
    }

    #[test]
    fn a_process_that_lost_its_environment_is_still_inside_its_login() {
        let runtime = Path::new("/run/user/1000");
        // tmux, ssh and services drop `XDG_CURRENT_DESKTOP`; the session's
        // own sockets in the runtime directory still say whose login this is.
        assert!(in_tilo_login(None, Some(runtime), |_| true));
        assert!(in_tilo_login(Some(OsStr::new("")), Some(runtime), |_| true));
        assert!(in_tilo_login(
            Some(OsStr::new("GNOME")),
            Some(runtime),
            |_| true
        ));
        // The environment alone is enough too, and the marker never subtracts.
        assert!(in_tilo_login(Some(OsStr::new("tilo:GNOME")), None, |_| {
            false
        }));
        assert!(in_tilo_login(
            Some(OsStr::new("tilo")),
            Some(runtime),
            |_| false
        ));
        // A foreign desktop with no Tilo session in its runtime directory.
        assert!(!in_tilo_login(
            Some(OsStr::new("GNOME")),
            Some(runtime),
            |_| false
        ));
        assert!(!in_tilo_login(None, None, |_| true));
    }

    #[test]
    fn the_runtime_directory_falls_back_to_the_accounts_own() {
        let present = |_: &Path| true;
        assert_eq!(
            login_runtime_dir_from(Some("/run/user/7".into()), 1000, present),
            Some(PathBuf::from("/run/user/7"))
        );
        for lost in [None, Some(OsString::new()), Some("relative/run".into())] {
            assert_eq!(
                login_runtime_dir_from(lost.clone(), 1000, present),
                Some(PathBuf::from("/run/user/1000")),
                "{lost:?}"
            );
            assert_eq!(login_runtime_dir_from(lost, 1000, |_| false), None);
        }
    }

    #[test]
    fn the_session_directory_lives_under_an_absolute_runtime_dir_only() {
        assert_eq!(
            session_dir(Some("/run/user/1000".into())),
            Some(PathBuf::from("/run/user/1000/tilo-session"))
        );
        assert_eq!(session_dir(Some("run/user".into())), None);
        assert_eq!(session_dir(Some(OsString::new())), None);
        assert_eq!(session_dir(None), None);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_compositors_socket_is_the_session_marker() {
        use std::os::unix::net::UnixListener;
        let runtime = std::env::temp_dir().join(format!(
            "tilo-paths-marker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&runtime);
        std::fs::create_dir(&runtime).unwrap();
        assert!(!has_session_marker(&runtime), "no session directory");
        let session = runtime.join(SESSION_DIR);
        std::fs::create_dir(&session).unwrap();
        assert!(!has_session_marker(&runtime), "an empty directory");
        std::fs::write(session.join(AGENT_SOCKET_FILE), b"").unwrap();
        assert!(!has_session_marker(&runtime), "a regular file");
        std::fs::remove_file(session.join(AGENT_SOCKET_FILE)).unwrap();
        let admin = UnixListener::bind(session.join(ADMIN_CONTROL_SOCKET_FILE)).unwrap();
        assert!(has_session_marker(&runtime), "the admin socket");
        drop(admin);
        std::fs::remove_dir_all(&runtime).unwrap();
    }
}
