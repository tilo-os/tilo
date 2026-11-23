//! Where copies wait before they take their names: an owner-only folder inside
//! the receiving folder, so publishing every copy is a rename within one
//! filesystem.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};

use super::publish::{self, Names};
use super::{Failure, TransferObserver, copy};

pub(crate) const STAGING_PREFIX: &str = ".tilo-transfer-";
pub(crate) const PAYLOAD: &str = "payload";

/// A private staging folder, and the copies in it.
///
/// It is reached through the receiving folder twice over: a descriptor for the
/// folder itself, so a caller that keeps one (a hosted app's Files grant, a
/// destination renamed under the receive) never needs its path, and a path for
/// the messages a person reads.
///
/// Dropping it removes the folder once it is empty; a copy retained for a
/// failed move's recovery keeps it.
pub struct Staging {
    receiving: File,
    receiving_dir: cap_std::fs::Dir,
    receiving_display: PathBuf,
    container: PathBuf,
    name: OsString,
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
    pub(crate) fn create(destination: &Path) -> io::Result<Self> {
        let receiving = File::open(destination)?;
        let receiving_dir =
            cap_std::fs::Dir::open_ambient_dir(destination, cap_std::ambient_authority())?;
        Self::allocate(receiving, receiving_dir, destination)
    }

    /// Make a private staging folder in the receiving folder held open as
    /// `receiving`, as a person sees it at `display`.
    #[cfg(unix)]
    pub fn in_folder(receiving: &File, display: &Path) -> io::Result<Self> {
        let receiving = receiving.try_clone()?;
        let receiving_dir = cap_std::fs::Dir::from_std_file(receiving.try_clone()?);
        Self::allocate(receiving, receiving_dir, display)
    }

    fn allocate(
        receiving: File,
        receiving_dir: cap_std::fs::Dir,
        display: &Path,
    ) -> io::Result<Self> {
        for _ in 0..32 {
            let name = OsString::from(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4().simple()));
            let mut builder = cap_std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use cap_std::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match receiving_dir.create_dir_with(&name, &builder) {
                Ok(()) => match receiving_dir.open_dir_nofollow(&name) {
                    Ok(directory) => {
                        return Ok(Self {
                            container: display.join(&name),
                            receiving_display: display.to_path_buf(),
                            receiving,
                            receiving_dir,
                            name,
                            directory,
                            keep: false,
                        });
                    }
                    Err(error) => {
                        let _ = receiving_dir.remove_dir(&name);
                        return Err(error);
                    }
                },
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
    pub fn take(destination: &Path, path: &Path) -> io::Result<Self> {
        if path.parent() != Some(destination) {
            return Err(io::Error::other(
                "a staging folder must be in the folder it receives into",
            ));
        }
        let receiving = File::open(destination)?;
        let receiving_dir =
            cap_std::fs::Dir::open_ambient_dir(destination, cap_std::ambient_authority())?;
        crate::create_private_dir(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("private transfer staging has no leaf name"))?
            .to_os_string();
        let directory = match receiving_dir.open_dir_nofollow(&name) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = receiving_dir.remove_dir(&name);
                return Err(error);
            }
        };
        Ok(Self {
            receiving,
            receiving_dir,
            container: path.to_path_buf(),
            receiving_display: destination.to_path_buf(),
            name,
            directory,
            keep: false,
        })
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.container
    }

    #[must_use]
    pub(crate) fn leaf(&self, name: &OsStr) -> PathBuf {
        self.container.join(name)
    }

    #[must_use]
    pub(crate) fn payload(&self) -> PathBuf {
        self.leaf(OsStr::new(PAYLOAD))
    }

    /// Create the folder `relative` in staging, and the folders above it,
    /// owner-only. Existing folders are opened, never replaced.
    pub fn create_directory(&self, relative: &Path) -> io::Result<()> {
        let mut current = self.directory.try_clone()?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(io::Error::other("a received folder is not portable"));
            };
            let mut builder = cap_std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use cap_std::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match current.create_dir_with(name, &builder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            current = current.open_dir_nofollow(name)?;
        }
        Ok(())
    }

    pub fn create_file(&self, relative: &Path) -> io::Result<File> {
        let name = relative
            .file_name()
            .ok_or_else(|| io::Error::other("a received file has no name"))?;
        let parent = match relative.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => self.open_directory(parent)?,
            _ => self.directory.try_clone()?,
        };
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        parent
            .open_with(name, &options)
            .map(cap_std::fs::File::into_std)
    }

    fn open_directory(&self, relative: &Path) -> io::Result<cap_std::fs::Dir> {
        let mut current = self.directory.try_clone()?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(io::Error::other("a received folder is not portable"));
            };
            current = current.open_dir_nofollow(name)?;
        }
        Ok(current)
    }

    /// Write `bytes` into this folder as its one payload, owner-only.
    pub(crate) fn stage_bytes(
        &self,
        bytes: &[u8],
        observer: &mut impl TransferObserver,
    ) -> io::Result<()> {
        use std::io::Write as _;

        let mut output = self.create_file(Path::new(PAYLOAD))?;
        let chosen = observer.destination_permissions(output.metadata()?.permissions());
        output.set_permissions(chosen)?;
        output.write_all(bytes)
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
    pub fn stage(
        &self,
        source: &Path,
        name: &Path,
        observer: &mut impl TransferObserver,
    ) -> io::Result<()> {
        copy::copy_tree_to(&self.directory, name, source, observer)
    }

    /// Flush every write staged here, so the copies are whole on disk before
    /// any of them is published.
    pub fn sync(&self) -> io::Result<()> {
        super::files::sync_filesystem(&self.receiving)?;
        #[cfg(unix)]
        super::files::sync_child_directory(&self.receiving, &self.name)?;
        #[cfg(not(unix))]
        super::sync_directory(&self.container)?;
        Ok(())
    }

    /// Publish the staged payload under `name`, or its first free numbered
    /// name, in the receiving folder.
    ///
    /// # Errors
    /// As [`super::transfer`]: never a replacement, and says when no name is
    /// free.
    #[cfg(unix)]
    pub fn publish_payload(
        &mut self,
        name: &OsStr,
        observer: &mut impl TransferObserver,
    ) -> Result<OsString, Failure> {
        publish::publish_numbered_at(
            OsStr::new(PAYLOAD),
            &self.directory,
            &self.receiving_dir,
            &self.receiving,
            &self.receiving_display,
            &Names::new(name),
            observer,
        )
    }

    /// Publish the staging folder itself under `name`, or its first free
    /// numbered name, in the receiving folder: a received folder keeps its
    /// staged tree exactly as it was made.
    ///
    /// # Errors
    /// As [`Self::publish_payload`].
    #[cfg(unix)]
    pub fn publish_tree(
        &mut self,
        name: &OsStr,
        observer: &mut impl TransferObserver,
    ) -> Result<OsString, Failure> {
        let published = publish::publish_numbered_at(
            &self.name,
            &self.receiving_dir,
            &self.receiving_dir,
            &self.receiving,
            &self.receiving_display,
            &Names::new(name),
            observer,
        )?;
        self.keep = true;
        Ok(published)
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
        if let Err(error) = self.remove() {
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
        self.keep = true;
        match self.remove() {
            Ok(()) => error,
            Err(cleanup_error) => Failure::Refused(format!(
                "{error}; private staging retained at {}: {cleanup_error}",
                self.container.display()
            )),
        }
    }

    fn remove(&self) -> io::Result<()> {
        match self.receiving_dir.remove_dir_all(&self.name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
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
        if let Err(error) = self.remove() {
            tracing::warn!(
                path = %self.container.display(),
                %error,
                "transfer staging cleanup failed"
            );
        }
    }
}
