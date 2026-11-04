//! Where copies wait before they take their names: an owner-only folder inside
//! the receiving folder, so publishing every copy is a rename within one
//! filesystem.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Failure, TransferObserver, copy};

/// The folder a transfer stages its copies in, inside the receiving folder.
const STAGING_PREFIX: &str = ".tilo-transfer-";
/// The leaf a one-shot transfer stages its one copy at.
pub(crate) const PAYLOAD: &str = "payload";

/// A private staging folder, and the copies in it.
///
/// Dropping it removes the folder once it is empty; a copy retained for a
/// failed move's recovery keeps it.
pub struct Staging {
    container: PathBuf,
    directory: cap_std::fs::Dir,
    keep: bool,
}

/// A fresh name for a staging folder in `destination`, for a caller that must
/// write it down before the folder exists.
#[must_use]
pub fn staging_path(destination: &Path) -> PathBuf {
    destination.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4().simple()))
}

impl Staging {
    /// Make a private staging folder in `destination`.
    ///
    /// # Errors
    /// The folder cannot be created or opened.
    pub(crate) fn create(destination: &Path) -> io::Result<Self> {
        for _ in 0..32 {
            match Self::take(destination, &staging_path(destination)) {
                Ok(staging) => return Ok(staging),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate private transfer staging",
        ))
    }

    /// Take the private staging folder at `path`, which lies in `destination`:
    /// a caller that writes the paste down first chooses the name.
    ///
    /// # Errors
    /// The folder cannot be created or opened, and a `path` outside
    /// `destination` is refused.
    pub fn take(destination: &Path, path: &Path) -> io::Result<Self> {
        use cap_fs_ext::DirExt as _;

        if path.parent() != Some(destination) {
            return Err(io::Error::other(
                "a staging folder must be in the folder it receives into",
            ));
        }
        let receiving =
            cap_std::fs::Dir::open_ambient_dir(destination, cap_std::ambient_authority())?;
        crate::create_private_dir(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("private transfer staging has no leaf name"))?;
        let directory = match receiving.open_dir_nofollow(name) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = fs::remove_dir(path);
                return Err(error);
            }
        };
        Ok(Self {
            container: path.to_path_buf(),
            directory,
            keep: false,
        })
    }

    /// The staging folder itself.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.container
    }

    /// The path the copy `name` takes in the staging folder.
    #[must_use]
    pub(crate) fn leaf(&self, name: &OsStr) -> PathBuf {
        self.container.join(name)
    }

    /// The path a one-shot transfer's copy takes here.
    #[must_use]
    pub(crate) fn payload(&self) -> PathBuf {
        self.leaf(OsStr::new(PAYLOAD))
    }

    /// Copy the tree at `source` into this folder as its one payload.
    ///
    /// # Errors
    /// As [`Self::stage`].
    pub(crate) fn stage_payload(
        &self,
        source: &Path,
        observer: &mut impl TransferObserver,
    ) -> io::Result<()> {
        self.stage(source, Path::new(PAYLOAD), observer)
    }

    /// Copy the tree at `source` to `name` in this folder. Nothing is flushed
    /// here: [`Self::sync`] commits the whole batch at once.
    ///
    /// # Errors
    /// Refuses lost admission and I/O failures.
    pub fn stage(
        &self,
        source: &Path,
        name: &Path,
        observer: &mut impl TransferObserver,
    ) -> io::Result<()> {
        copy::copy_tree_to(&self.directory, name, source, observer)
    }

    /// Flush every copy staged here and the folder's own entry, so they are
    /// whole on disk before any of them is published.
    ///
    /// # Errors
    /// Returns what the filesystem says.
    pub fn sync(&self) -> io::Result<()> {
        super::files::sync_filesystem(&self.container)?;
        crate::transfer::sync_directory(&self.container)
    }

    /// Keep the folder and what is in it, whatever happens to this handle: a
    /// complete copy is the way back from a move that could not finish.
    pub fn keep(&mut self) {
        self.keep = true;
    }

    /// The transfer succeeded: the copy was published and only an empty
    /// folder is left to remove.
    pub fn finish_success(mut self) {
        self.keep = true;
        if let Err(error) = fs::remove_dir_all(&self.container) {
            tracing::warn!(
                path = %self.container.display(),
                %error,
                "an empty transfer staging folder could not be removed"
            );
        }
    }

    /// The transfer failed with `error`: take the copy away, keeping it and
    /// saying where when it cannot be taken away.
    pub(crate) fn refuse(mut self, error: impl Into<Failure>) -> Failure {
        let error = error.into();
        match super::remove_path(&self.container) {
            Ok(()) => {
                self.keep = true;
                error
            }
            Err(cleanup_error) => {
                self.keep = true;
                Failure::Refused(format!(
                    "{error}; private staging retained at {}: {cleanup_error}",
                    self.container.display()
                ))
            }
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.keep {
            if std::thread::panicking() {
                tracing::warn!(
                    path = %self.container.display(),
                    "a transfer worker unwound; complete recovery copy retained"
                );
            }
            return;
        }
        if let Err(error) = super::remove_path(&self.container) {
            tracing::warn!(
                path = %self.container.display(),
                %error,
                "transfer staging cleanup failed"
            );
        }
    }
}
