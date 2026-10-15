//! The registry identity the verification commands use: bootc's existing
//! pull credentials, copied into a private directory for the run.
//!
//! The pull identity is transport only. It cannot authorize an image or
//! change the pinned release key or the owner's grant.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::command;
use crate::release::MAX_OUTPUT;
use crate::{Error, Result, json_loads_bytes, require};

/// bootc and ostree-ext's root authfile precedence.
pub const AUTH_FILES: [&str; 3] = [
    "run/ostree/auth.json",
    "etc/ostree/auth.json",
    "usr/lib/ostree/auth.json",
];
const CREDENTIALS_LIMIT: usize = 1_048_576;

/// A private copy of the pull credentials and the environment that points
/// the verification commands at it. Dropping it removes the copy.
#[derive(Debug)]
pub struct Registry {
    directory: tempfile::TempDir,
    environment: Vec<(&'static str, OsString)>,
}

impl Registry {
    /// Copies the first present authfile under `root` (checked by `trusted`
    /// and required to be a private regular file owned by `owner`), or `{}`
    /// when there is none, into a fresh 0700 directory in `temporary`.
    ///
    /// # Errors
    /// Exposed, oversized or malformed credentials, or a failed write.
    pub fn new(
        root: &Path,
        trusted: &dyn Fn(&str) -> Result<PathBuf>,
        temporary: &Path,
        owner: u32,
    ) -> Result<Self> {
        let mut credentials = b"{}".to_vec();
        for relative in AUTH_FILES {
            let candidate = root.join(relative);
            let present = fs::metadata(&candidate).is_ok()
                || fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink());
            if !present {
                continue;
            }
            let source = trusted(relative)?;
            let stream = File::open(&source).map_err(|e| Error::os(&e, &source))?;
            let info = stream.metadata().map_err(|e| Error::os(&e, &source))?;
            require(
                info.file_type().is_file()
                    && info.uid() == owner
                    && info.nlink() == 1
                    && info.mode() & 0o777 == 0o600,
                "bootc pull credentials must be a private root-owned regular file",
            )?;
            credentials.clear();
            stream
                .take(u64::try_from(CREDENTIALS_LIMIT.saturating_add(1)).unwrap_or(u64::MAX))
                .read_to_end(&mut credentials)
                .map_err(|e| Error::os(&e, &source))?;
            require(
                credentials.len() <= CREDENTIALS_LIMIT,
                "bootc pull credentials are malformed or oversized",
            )?;
            require(
                json_loads_bytes(&credentials)?.is_object(),
                "bootc pull credentials are malformed or oversized",
            )?;
            break;
        }
        // Private like mkdtemp's: tempfile would otherwise leave it to the umask.
        let directory = tempfile::Builder::new()
            .prefix("tilo-board-verifier-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(temporary)
            .map_err(|e| Error::os(&e, temporary))?;
        let configuration = directory.path().join("config.json");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&configuration)
            .map_err(|e| Error::os(&e, &configuration))?;
        output
            .write_all(&credentials)
            .map_err(|e| Error::os(&e, &configuration))?;
        drop(output);
        let environment = vec![
            ("DOCKER_CONFIG", directory.path().as_os_str().to_owned()),
            ("REGISTRY_AUTH_FILE", configuration.into_os_string()),
        ];
        Ok(Self {
            directory,
            environment,
        })
    }

    /// The variables added to each command's environment.
    #[must_use]
    pub fn environment(&self) -> &[(&'static str, OsString)] {
        &self.environment
    }

    /// The private directory holding the copy.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Runs one verification command with these credentials.
    ///
    /// # Errors
    /// See [`command::run`].
    pub fn run(&self, arguments: &[&str]) -> Result<String> {
        command::run(
            arguments,
            &self.environment,
            Duration::from_secs(120),
            MAX_OUTPUT,
        )
    }
}
