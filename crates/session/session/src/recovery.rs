//! The lock a session leaves for the next one within one authenticated login.
//!
//! The session wrapper gives every login a private directory and writes its
//! record unlocked: the login's first session starts unlocked, as the person
//! left it at the login screen. A running session arms that record locked, and
//! only its ordered end publishes the state it ended in — so a session that was
//! killed, panicked or ended without taking its display down comes back locked,
//! whoever starts it next. The record lives in the wrapper's directory, never
//! in shared state: no other login, device or remote member can reach it.

use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The state a session ran locked leaves for the next one.
const LOCKED: &[u8] = b"locked\n";
/// The state an ended session publishes for the next one.
const UNLOCKED: &[u8] = b"unlocked\n";

/// What one login's lock record says, and where it is kept.
pub struct Recovery {
    path: PathBuf,
    starts_locked: bool,
}

impl Recovery {
    /// The record of the login this process runs in, from the channel the
    /// session wrapper names in the environment, armed locked for as long as
    /// this session runs. `None` when no wrapper started this process: a
    /// session a person started by hand has no record to keep.
    ///
    /// # Errors
    /// The channel is there but cannot be trusted or written: the session must
    /// not start, or it would come back unlocked after a crash.
    pub fn from_environment() -> io::Result<Option<Self>> {
        let Some(request) = std::env::var_os("TILO_SESSION_LOGOUT_REQUEST") else {
            return Ok(None);
        };
        Self::begin(Path::new(&request)).map(Some)
    }

    /// Arm the record beside `request` and read what the session it belongs to
    /// was left as: `request` is the wrapper's logout-request file, and its
    /// directory is the login's private channel.
    ///
    /// A missing record — an older wrapper, or one whose session never ran —
    /// and a malformed one both come back locked: a lock is never lifted by
    /// what a record failed to say.
    ///
    /// # Errors
    /// The channel is not an absolute private directory of this person's, the
    /// record is linked or redirected, or it cannot be written: no session
    /// starts without its record armed.
    pub fn begin(request: &Path) -> io::Result<Self> {
        let directory = request
            .parent()
            .filter(|_| request.is_absolute())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session channel is not absolute",
                )
            })?;
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || !tilo_files::native::private::private_to_me(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "session channel is not private",
            ));
        }
        let path = directory.join("lock-state");
        let starts_locked = match tilo_files::native::private::read(&path) {
            Ok(file) => {
                if file.metadata()?.nlink() != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "linked session lock record",
                    ));
                }
                let mut record = Vec::new();
                file.take(32).read_to_end(&mut record)?;
                record != UNLOCKED
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        // Armed before the session admits anyone or shows a frame: whatever ends it from here on
        // comes back locked until it ends in order.
        tilo_files::native::private::write_atomic(&path, LOCKED)?;
        Ok(Self {
            path,
            starts_locked,
        })
    }

    /// Whether the session this record belongs to was left locked, and so starts locked.
    #[must_use]
    pub fn starts_locked(&self) -> bool {
        self.starts_locked
    }

    /// The session ended in order: publish the lock state the next start
    /// recovers. A record this fails to write stays armed, and the next start
    /// comes back locked.
    ///
    /// # Errors
    /// The record could not be written.
    pub fn finish(self, locked: bool) -> io::Result<()> {
        tilo_files::native::private::write_atomic(
            &self.path,
            if locked { LOCKED } else { UNLOCKED },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A private directory to keep a login's record in, as the wrapper makes one.
    fn channel() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a channel");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("the channel is this person's");
        directory
    }

    #[test]
    fn an_ordered_restart_keeps_the_lock_and_a_verified_unlock() {
        let directory = channel();
        let path = directory.path().join("lock-state");
        tilo_files::native::private::write_atomic(&path, UNLOCKED).expect("the wrapper's record");
        let request = directory.path().join("logout-request");
        let first = Recovery::begin(&request).expect("a fresh login");
        assert!(
            !first.starts_locked(),
            "an authenticated login starts unlocked"
        );
        first.finish(true).expect("the session ended in order");
        let second = Recovery::begin(&request).expect("a restart");
        assert!(second.starts_locked(), "a lock survives an ordered end");
        second.finish(false).expect("the session ended in order");
        let third = Recovery::begin(&request).expect("a restart");
        assert!(
            !third.starts_locked(),
            "an ordered end preserves the unlock"
        );
    }

    #[test]
    fn a_crashed_missing_or_corrupt_record_recovers_locked() {
        let directory = channel();
        let request = directory.path().join("logout-request");
        let path = directory.path().join("lock-state");
        tilo_files::native::private::write_atomic(&path, UNLOCKED).expect("the wrapper's record");
        let running = Recovery::begin(&request).expect("a session");
        // No ordered end, as after a kill.
        drop(running);
        assert!(
            Recovery::begin(&request)
                .expect("a restart")
                .starts_locked()
        );
        fs::remove_file(&path).expect("no record");
        assert!(
            Recovery::begin(&request)
                .expect("a fresh record")
                .starts_locked()
        );
        tilo_files::native::private::write_atomic(&path, b"partial").expect("a corrupt record");
        assert!(
            Recovery::begin(&request)
                .expect("a corrupt record")
                .starts_locked()
        );
    }

    #[test]
    fn the_wrapper_names_the_login_and_no_variable_names_none() {
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.remove("TILO_SESSION_LOGOUT_REQUEST");
        assert!(
            Recovery::from_environment().expect("no channel").is_none(),
            "a session started by hand keeps no record"
        );
        let directory = channel();
        let path = directory.path().join("lock-state");
        tilo_files::native::private::write_atomic(&path, UNLOCKED).expect("the wrapper's record");
        let request = directory.path().join("logout-request");
        environment.set("TILO_SESSION_LOGOUT_REQUEST", request.as_os_str());
        let started = Recovery::from_environment()
            .expect("the wrapper's channel")
            .expect("the login's record");
        assert!(!started.starts_locked(), "the login was left unlocked");
        // A session that ends without publishing its state, as after a kill.
        drop(started);
        assert!(
            Recovery::from_environment()
                .expect("the wrapper's channel")
                .expect("the login's record")
                .starts_locked(),
            "the next session starts locked"
        );
    }

    #[test]
    fn a_public_or_redirected_channel_admits_no_session() {
        let directory = tempfile::tempdir().expect("a channel");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("the channel is everyone's");
        assert!(Recovery::begin(&directory.path().join("logout-request")).is_err());
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("the channel is this person's");
        let request = directory.path().join("logout-request");
        std::os::unix::fs::symlink("untrusted", directory.path().join("lock-state"))
            .expect("a redirect");
        assert!(Recovery::begin(&request).is_err());
        let record = directory.path().join("lock-state");
        fs::remove_file(&record).expect("the redirect");
        fs::write(&record, UNLOCKED).expect("a record");
        fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).expect("a public record");
        assert!(Recovery::begin(&request).is_err());
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600))
            .expect("this person's record");
        fs::hard_link(&record, directory.path().join("redirected-record")).expect("a link");
        assert!(Recovery::begin(&request).is_err());
    }
}
