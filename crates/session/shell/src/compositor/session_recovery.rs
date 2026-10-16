//! Lock handoff within one authenticated wrapper login, never a shared CRDT.
//!
//! A running compositor leaves a locked recovery record. Only ordered teardown,
//! after presentation and control endpoints stop, publishes the final lock state.
//! Thus SIGKILL, panic and incomplete teardown recover locked. A new PAM login
//! gets a new private directory and an explicitly unlocked record from its wrapper.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const LOCKED: &[u8] = b"locked\n";
const UNLOCKED: &[u8] = b"unlocked\n";

pub(super) struct SessionRecovery {
    path: PathBuf,
}

impl SessionRecovery {
    pub(super) fn from_env() -> io::Result<Option<(Self, bool)>> {
        std::env::var_os("TILO_SESSION_LOGOUT_REQUEST")
            .map(|request| Self::begin(Path::new(&request)))
            .transpose()
    }

    fn begin(request: &Path) -> io::Result<(Self, bool)> {
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
        // Missing (including an older wrapper) and malformed records fail closed.
        let locked = match tilo_files::native::private::read(&path) {
            Ok(file) => {
                if file.metadata()?.nlink() != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "linked session recovery record",
                    ));
                }
                let mut record = Vec::new();
                file.take(32).read_to_end(&mut record)?;
                record != UNLOCKED
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        // Mandatory before admission or presentation: failed arming stops startup.
        tilo_files::native::private::write_atomic(&path, LOCKED)?;
        Ok((Self { path }, locked))
    }

    pub(super) fn finish(self, locked: bool) -> io::Result<()> {
        tilo_files::native::private::write_atomic(
            &self.path,
            if locked { LOCKED } else { UNLOCKED },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn orderly_restart_preserves_lock_and_verified_unlock() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("lock-state");
        tilo_files::native::private::write_atomic(&path, UNLOCKED).unwrap();
        let request = directory.path().join("logout-request");
        let (first, locked) = SessionRecovery::begin(&request).unwrap();
        assert!(!locked, "authenticated fresh login starts unlocked");
        first.finish(true).unwrap();
        let (second, locked) = SessionRecovery::begin(&request).unwrap();
        assert!(locked, "lock survives ordered teardown");
        second.finish(false).unwrap();
        let (_, locked) = SessionRecovery::begin(&request).unwrap();
        assert!(
            !locked,
            "ordered teardown preserves successful authentication"
        );
    }

    #[test]
    fn crash_missing_or_corrupt_record_recovers_locked() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let request = directory.path().join("logout-request");
        let path = directory.path().join("lock-state");
        tilo_files::native::private::write_atomic(&path, UNLOCKED).unwrap();
        let (running, _) = SessionRecovery::begin(&request).unwrap();
        drop(running); // no successful ordered teardown, as after SIGKILL
        let (_, locked) = SessionRecovery::begin(&request).unwrap();
        assert!(locked);
        fs::remove_file(&path).unwrap();
        let (_, locked) = SessionRecovery::begin(&request).unwrap();
        assert!(locked);
        tilo_files::native::private::write_atomic(&path, b"partial").unwrap();
        let (_, locked) = SessionRecovery::begin(&request).unwrap();
        assert!(locked);
    }

    #[test]
    fn public_or_redirected_channel_cannot_admit_a_session() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(SessionRecovery::begin(&directory.path().join("logout-request")).is_err());
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink("untrusted", directory.path().join("lock-state")).unwrap();
        assert!(SessionRecovery::begin(&directory.path().join("logout-request")).is_err());
        let record = directory.path().join("lock-state");
        fs::remove_file(&record).unwrap();
        fs::write(&record, UNLOCKED).unwrap();
        fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SessionRecovery::begin(&directory.path().join("logout-request")).is_err());
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&record, directory.path().join("redirected-record")).unwrap();
        assert!(SessionRecovery::begin(&directory.path().join("logout-request")).is_err());
    }
}
