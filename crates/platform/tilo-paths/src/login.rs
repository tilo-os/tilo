//! Whether this process runs inside a Tilo login, and where that login's compositor
//! publishes its endpoints: the private [`SESSION_DIR`] under the login's runtime
//! directory.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub const TILO_DESKTOP: &str = "tilo";
pub const SESSION_DIR: &str = "tilo-session";
pub const AGENT_SOCKET_FILE: &str = "agent.sock";
pub const ENVIRONMENT_DOORS_DIR: &str = "environments";
pub const GUEST_AGENT_SOCKET: &str = "/run/tilo/agent.sock";
pub const AGENT_SOCKET_ENV: &str = "TILO_AGENT_SOCK";
pub const ADMIN_CONTROL_SOCKET_FILE: &str = "admin-control.sock";
/// The development test-control socket's file name in [`SESSION_DIR`]: what
/// `tilo-control` and the tools that drive a desktop reach for unless they are
/// told another path.
pub const TEST_CONTROL_SOCKET_FILE: &str = "test-control.sock";
pub const WORKSPACE_ID_FILE: &str = "workspace-id";
/// File in [`SESSION_DIR`] holding the bare name of the running session's
/// Wayland socket, so `WAYLAND_DISPLAY=$(cat …)` is exactly right. The
/// session that bound the socket publishes it for the programs this login
/// starts and the tools beside it, which find the display without reading
/// the compositor's log.
pub const WAYLAND_DISPLAY_FILE: &str = "wayland-display";
/// Prefix of the socket in [`SESSION_DIR`] on which this session says whether
/// its first use is over: the installed folder importer asks before it
/// continues a saved import, so nothing it carries lands under an identity
/// ceremony still under way.
pub const IMPORT_READINESS_SOCKET_PREFIX: &str = "import-readiness";
pub const IMPORTER_DIR: &str = "tilo-imager";
pub const IMPORTER_CONTINUATION_FILE: &str = "desktop-continuation.json";

const SESSION_SOCKETS: [&str; 2] = [AGENT_SOCKET_FILE, ADMIN_CONTROL_SOCKET_FILE];

#[must_use]
pub fn session_dir(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    let runtime_dir = PathBuf::from(runtime_dir?);
    runtime_dir
        .is_absolute()
        .then(|| runtime_dir.join(SESSION_DIR))
}

#[cfg(unix)]
#[must_use]
pub fn import_readiness_socket(
    runtime_dir: Option<OsString>,
    display: &OsStr,
    wayland: u64,
) -> Option<PathBuf> {
    Some(session_dir(runtime_dir)?.join(import_readiness_socket_file(display, wayland)?))
}

#[cfg(unix)]
#[must_use]
pub fn import_readiness_socket_file(display: &OsStr, wayland: u64) -> Option<String> {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = display.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 || bytes.contains(&b'/') {
        return None;
    }
    let mut key = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(key, "{byte:02x}");
    }
    Some(format!(
        "{IMPORT_READINESS_SOCKET_PREFIX}-{key}-{wayland}.sock"
    ))
}

#[must_use]
pub fn importer_migrations_dir() -> Option<PathBuf> {
    Some(
        crate::data_local_dir()?
            .join(IMPORTER_DIR)
            .join("migrations"),
    )
}

#[must_use]
pub fn importer_continuation() -> Option<PathBuf> {
    Some(importer_migrations_dir()?.join(IMPORTER_CONTINUATION_FILE))
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

#[cfg(unix)]
#[must_use]
pub fn login_runtime_dir() -> Option<PathBuf> {
    login_runtime_dir_from(
        crate::var("XDG_RUNTIME_DIR"),
        rustix::process::geteuid().as_raw(),
        Path::is_dir,
    )
}

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

/// Whether a process with these facts runs inside a Tilo login: its environment names
/// the desktop, or its account's runtime directory holds the session's own marker. The
/// environment is a hint that can only add.
#[must_use]
pub fn in_tilo_login(
    current_desktop: Option<&OsStr>,
    runtime_dir: Option<&Path>,
    marker: impl FnOnce(&Path) -> bool,
) -> bool {
    names_tilo(current_desktop) || runtime_dir.is_some_and(marker)
}

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
    fn the_import_readiness_socket_names_the_session_by_its_display() {
        assert_eq!(
            import_readiness_socket(Some("/run/user/1000".into()), OsStr::new("wayland-1"), 42),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/import-readiness-7761796c616e642d31-42.sock"
            ))
        );
        assert_eq!(
            import_readiness_socket(None, OsStr::new("wayland-1"), 42),
            None
        );
        let too_long = "n".repeat(33);
        for impossible in ["", "../escape", "wayland-1/other", too_long.as_str()] {
            assert_eq!(
                import_readiness_socket(Some("/run/user/1000".into()), OsStr::new(impossible), 1),
                None,
                "{impossible:?}"
            );
        }
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
