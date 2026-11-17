//! Telling an in-place desktop restart from the login ending.
//!
//! The session wrapper restarts the compositor in place on request: the PAM session, the user
//! manager and every service stay up, and only the display process is replaced. Both runs see
//! exactly one SIGTERM, so neither can tell that from a logout on its own. Before it stops the
//! compositor, the wrapper therefore writes `restart-request` into the login's private channel —
//! the directory that already holds the logout request and the lock state — and the run that
//! follows reads it once at start, removing it. A run that ends sees the marker only when the
//! wrapper asked it to come back in place.
//!
//! A marker that is missing, unreadable, linked or not this person's is never a restart: apps
//! then end with the login, which loses nothing that a wrong guess would keep.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The marker the wrapper leaves for an in-place restart of this login's desktop.
const MARKER: &str = "restart-request";

/// Whether this run follows an in-place restart of the desktop, consuming the marker so no later
/// read sees it. Called once at start: the marker is this run's, and a request made afterwards
/// belongs to the run that ends.
#[must_use]
pub fn following_restart() -> bool {
    marker().is_some_and(|path| consume(&path))
}

/// Whether the wrapper is restarting this desktop in place right now: read at teardown, without
/// consuming the marker. A marker that cannot be trusted answers `false`, so the apps end.
#[must_use]
pub fn restarting() -> bool {
    marker().is_some_and(|path| trusted(&path).is_ok())
}

/// How the machine's own desktop ends now: an in-place restart when the wrapper asked for one,
/// else the login ends ([`crate::Options::teardown`]).
#[must_use]
pub fn teardown() -> tilo_app_launch::Teardown {
    if restarting() {
        tilo_app_launch::Teardown::InPlaceRestart
    } else {
        tilo_app_launch::Teardown::Logout
    }
}

/// The login's restart marker, from the channel the wrapper names in the environment. `None` when
/// no wrapper started this process, or the channel is not this person's private directory.
fn marker() -> Option<PathBuf> {
    let request = std::env::var_os("TILO_SESSION_LOGOUT_REQUEST")?;
    let request = PathBuf::from(request);
    match crate::recovery::channel(&request) {
        Ok(directory) => Some(directory.join(MARKER)),
        Err(error) => {
            tracing::warn!(%error, "the login's private channel is not trusted; not a restart");
            None
        }
    }
}

/// Read and remove the marker. A marker that is not a private regular file of this person's is
/// refused unread: nothing tells a restart from a logout by what a file merely exists as.
fn consume(path: &Path) -> bool {
    if trusted(path).is_err() {
        return false;
    }
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(%error, "a restart marker was read but not removed");
    }
    true
}

/// The marker file, validated as a file only this person can have written and no other name
/// shares.
fn trusted(path: &Path) -> io::Result<()> {
    let file = tilo_files::native::private::read(path)?;
    if file.metadata()?.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "linked restart marker",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    /// A private channel as the wrapper makes one, with the request file it names.
    fn channel() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("a channel");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("the channel is this person's");
        let request = directory.path().join("logout-request");
        fs::write(&request, b"").expect("the request file");
        (directory, request)
    }

    #[test]
    fn a_marker_is_a_private_file_of_this_person_nothing_else_shares() {
        let (directory, _request) = channel();
        let path = directory.path().join(MARKER);
        assert!(trusted(&path).is_err(), "a missing marker");
        fs::write(&path, b"restart\n").expect("a marker");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private");
        assert!(trusted(&path).is_ok(), "the wrapper's own marker");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("a public marker");
        assert!(trusted(&path).is_err(), "a marker anyone could write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private again");
        fs::hard_link(&path, directory.path().join("another-name")).expect("a link");
        assert!(trusted(&path).is_err(), "a marker another name shares");
        fs::remove_file(directory.path().join("another-name")).expect("the link");
        fs::remove_file(&path).expect("the marker");
        symlink("elsewhere", &path).expect("a redirect");
        assert!(trusted(&path).is_err(), "a marker that is a redirect");
    }

    tilo_test_env::scenario! {
        fn an_in_place_restart_is_read_once_at_start_and_at_teardown;

        phase the_run_that_follows_consumes_the_marker;
        {
            let mut environment = tilo_test_env::ScopedEnv::new();
            environment.remove("TILO_SESSION_LOGOUT_REQUEST");
            assert!(!following_restart(), "a session started by hand is no restart");

            let (directory, request) = channel();
            let path = directory.path().join(MARKER);
            environment.set("TILO_SESSION_LOGOUT_REQUEST", request.as_os_str());
            assert!(!restarting(), "a fresh login has no restart marker");
            assert!(!following_restart(), "and this run is no restart");
            fs::write(&path, b"restart\n").expect("the wrapper's marker");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private");
            assert!(restarting(), "the ending run sees the marker without using it");
            assert!(path.exists(), "so the run that follows can still read it");
            assert!(following_restart(), "the following run is a restart");
            assert!(!path.exists(), "and the marker is consumed");
            assert!(!following_restart(), "a second read is no restart");
            assert!(!restarting(), "and nothing is asked any more");
        }

        phase a_channel_that_is_not_this_persons_is_never_a_restart;
        {
            let mut environment = tilo_test_env::ScopedEnv::new();
            let directory = tempfile::tempdir().expect("a channel");
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
                .expect("a channel anyone can enter");
            let request = directory.path().join("logout-request");
            environment.set("TILO_SESSION_LOGOUT_REQUEST", request.as_os_str());
            let path = directory.path().join(MARKER);
            fs::write(&path, b"restart\n").expect("a marker");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private");
            assert!(!restarting(), "a public channel says nothing about a restart");
        }
    }
}
