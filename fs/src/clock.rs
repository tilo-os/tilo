//! Process-wide wall clock for security decisions made by the shipped daemon.
//!
//! Production uses [`SystemTime`] unconditionally unless the daemon was
//! explicitly launched with its hidden `--test-clock-file` option.  That
//! option exists for release tests which must move an already-running daemon
//! across deadline boundaries without sleeping.  The file is required to be
//! an owner-only regular file containing one canonical decimal Unix time in
//! milliseconds.  It is re-read for every decision.  A missing, replaced,
//! oversized, or malformed file maps to `u64::MAX`, which expires credentials
//! and receipts instead of extending their authority.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

const MAX_CLOCK_FILE_BYTES: u64 = 32;
static TEST_CLOCK_FILE: OnceLock<TestClockFile> = OnceLock::new();

struct TestClockFile {
    path: PathBuf,
    // Keep the validated inode alive for the lifetime of the override.  Apart
    // from making reads race-safe, this prevents an unlink/create sequence
    // from immediately recycling the pinned inode number and defeating the
    // path identity check below.
    file: Mutex<fs::File>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// Install the daemon's explicit test clock override.
///
/// The initial value is validated before installation so a bad launch fails
/// rather than briefly running with the real clock.  Subsequent read failures
/// fail closed through [`unix_now_ms`].
///
/// # Errors
///
/// Returns an error when the path is not an absolute, owner-only regular file,
/// its value is invalid, or another test clock was already installed.
pub fn install_test_clock_file(path: &Path) -> Result<(), ClockConfigurationError> {
    let clock = TestClockFile::validate(path)?;
    TEST_CLOCK_FILE
        .set(clock)
        .map_err(|_| ClockConfigurationError::AlreadyConfigured)
}

/// Milliseconds since the Unix epoch used by authorization, share and receipt
/// decisions throughout this process.
pub fn unix_now_ms() -> u64 {
    match TEST_CLOCK_FILE.get() {
        Some(clock) => clock.read().unwrap_or(u64::MAX),
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(u64::MAX, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            }),
    }
}

pub fn unix_now_ns() -> u64 {
    unix_now_ms().saturating_mul(1_000_000)
}

pub fn unix_now_secs() -> u64 {
    unix_now_ms() / 1_000
}

impl TestClockFile {
    fn validate(path: &Path) -> Result<Self, ClockConfigurationError> {
        if !path.is_absolute() {
            return Err(ClockConfigurationError::NotAbsolute);
        }
        validate_metadata(path, None)?;
        let file = fs::File::open(path).map_err(ClockConfigurationError::Io)?;
        let metadata = file.metadata().map_err(ClockConfigurationError::Io)?;
        let clock = Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            #[cfg(unix)]
            device: {
                use std::os::unix::fs::MetadataExt;
                metadata.dev()
            },
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                metadata.ino()
            },
        };
        validate_open_file_metadata(&metadata, &clock)?;
        // Close the validation/open race: the path must still name the file
        // whose handle and identity we retain.
        validate_metadata(path, Some(&clock))?;
        clock.read()?;
        Ok(clock)
    }

    fn read(&self) -> Result<u64, ClockConfigurationError> {
        validate_metadata(&self.path, Some(self))?;
        let mut file = self.file.lock().map_err(|_| {
            ClockConfigurationError::Io(std::io::Error::other("test clock file lock poisoned"))
        })?;
        validate_open_file_metadata(&file.metadata().map_err(ClockConfigurationError::Io)?, self)?;
        file.seek(SeekFrom::Start(0))
            .map_err(ClockConfigurationError::Io)?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(MAX_CLOCK_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(ClockConfigurationError::Io)?;
        drop(file);
        if bytes.is_empty() || bytes.len() as u64 > MAX_CLOCK_FILE_BYTES {
            return Err(ClockConfigurationError::InvalidFile);
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| ClockConfigurationError::InvalidValue)?;
        if text.starts_with('+')
            || text.starts_with('-')
            || text.is_empty()
            || !text.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(ClockConfigurationError::InvalidValue);
        }
        text.parse()
            .map_err(|_| ClockConfigurationError::InvalidValue)
    }
}

fn validate_open_file_metadata(
    metadata: &fs::Metadata,
    expected: &TestClockFile,
) -> Result<(), ClockConfigurationError> {
    if !metadata.file_type().is_file() || metadata.len() > MAX_CLOCK_FILE_BYTES {
        return Err(ClockConfigurationError::InvalidFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(ClockConfigurationError::InsecurePermissions);
        }
        if metadata.dev() != expected.device || metadata.ino() != expected.inode {
            return Err(ClockConfigurationError::ReplacedFile);
        }
    }
    Ok(())
}

fn validate_metadata(
    path: &Path,
    expected: Option<&TestClockFile>,
) -> Result<fs::Metadata, ClockConfigurationError> {
    // `symlink_metadata` deliberately refuses a symlink rather than following
    // it to a file that can be swapped outside this ownership check.
    let metadata = fs::symlink_metadata(path).map_err(ClockConfigurationError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_CLOCK_FILE_BYTES {
        return Err(ClockConfigurationError::InvalidFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(ClockConfigurationError::InsecurePermissions);
        }
        if expected
            .is_some_and(|clock| metadata.dev() != clock.device || metadata.ino() != clock.inode)
        {
            return Err(ClockConfigurationError::ReplacedFile);
        }
    }
    Ok(metadata)
}

#[derive(Debug, Error)]
pub enum ClockConfigurationError {
    #[error("test clock path must be absolute")]
    NotAbsolute,
    #[error("test clock file cannot be read: {0}")]
    Io(#[source] std::io::Error),
    #[error("test clock must be a small regular file")]
    InvalidFile,
    #[error("test clock must contain only canonical decimal Unix milliseconds")]
    InvalidValue,
    #[error("test clock must be owned by the daemon user and inaccessible to group/other")]
    InsecurePermissions,
    #[error("test clock file was replaced after startup")]
    ReplacedFile,
    #[error("the process clock was already configured")]
    AlreadyConfigured,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn override_is_inode_pinned_and_each_read_fails_closed_on_bad_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock-ms");
        fs::write(&path, b"1234").unwrap();
        #[cfg(unix)]
        owner_only(&path);
        let clock = TestClockFile::validate(&path).unwrap();
        assert_eq!(clock.read().unwrap(), 1_234);

        // Truncating the same inode is the supported deterministic clock move.
        fs::write(&path, b"1235").unwrap();
        assert_eq!(clock.read().unwrap(), 1_235);
        fs::write(&path, b"12x5").unwrap();
        assert!(matches!(
            clock.read(),
            Err(ClockConfigurationError::InvalidValue)
        ));

        fs::remove_file(&path).unwrap();
        fs::write(&path, b"1236").unwrap();
        #[cfg(unix)]
        owner_only(&path);
        #[cfg(unix)]
        assert!(matches!(
            clock.read(),
            Err(ClockConfigurationError::ReplacedFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn override_refuses_relative_symlink_and_non_private_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock-ms");
        fs::write(&path, b"1").unwrap();
        assert!(matches!(
            TestClockFile::validate(Path::new("clock-ms")),
            Err(ClockConfigurationError::NotAbsolute)
        ));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            TestClockFile::validate(&path),
            Err(ClockConfigurationError::InsecurePermissions)
        ));

        owner_only(&path);
        let link = dir.path().join("clock-link");
        symlink(&path, &link).unwrap();
        assert!(matches!(
            TestClockFile::validate(&link),
            Err(ClockConfigurationError::InvalidFile)
        ));
    }
}
