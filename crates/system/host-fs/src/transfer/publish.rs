//! The names a copy may take in its receiving folder: never one that is
//! already there.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Failure, TransferObserver};

/// The most numbered names one folder is asked for before a transfer gives up.
pub(crate) const MAX_NUMBER: u32 = 10_000;

/// A copy's name and the numbered names it may take instead, so a transfer
/// never replaces what is already there.
pub(crate) struct Names {
    original: OsString,
    stem: OsString,
    extension: Option<OsString>,
}

impl Names {
    /// The names for `name`: itself, then its stem with a number before the
    /// extension.
    #[must_use]
    pub fn new(name: &OsStr) -> Self {
        let path = Path::new(name);
        Self {
            original: name.to_os_string(),
            stem: path.file_stem().unwrap_or_default().to_os_string(),
            extension: path.extension().map(OsStr::to_os_string),
        }
    }

    /// The name the `suffix`th try takes: the name itself first, then
    /// `report 2.txt`, `report 3.txt`.
    #[must_use]
    pub fn for_suffix(&self, suffix: u32) -> OsString {
        if suffix <= 1 {
            return self.original.clone();
        }
        let mut name = self.stem.clone();
        name.push(format!(" {suffix}"));
        if let Some(extension) = &self.extension {
            name.push(".");
            name.push(extension);
        }
        name
    }
}

/// Whether something already holds the directory entry `path`.
///
/// # Errors
/// Returns why an entry could not be examined.
pub(crate) fn occupied(path: &Path) -> Result<bool, Failure> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Failure::Inspect {
            path: path.to_path_buf(),
            error,
        }),
    }
}

/// The first name `name` may take in `folder` without replacing anything, or
/// `None` when every numbered name is taken.
///
/// # Errors
/// Returns why a candidate could not be examined.
pub fn free_name(folder: &Path, name: &OsStr) -> Result<Option<PathBuf>, Failure> {
    let names = Names::new(name);
    for number in 1..MAX_NUMBER {
        let candidate = folder.join(names.for_suffix(number));
        if !occupied(&candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// Publish the copy at `staged` under `target`, never replacing a name that
/// was taken meanwhile.
///
/// # Errors
/// Returns [`io::ErrorKind::AlreadyExists`] when `target` is taken, and what
/// the filesystem says otherwise.
pub(crate) fn publish(staged: &Path, target: &Path) -> io::Result<()> {
    crate::rename_noreplace(staged, target)
}

/// Publish the copy at `staged` under `names`' first free name in `dest_dir`.
pub(crate) fn publish_numbered(
    staged: &Path,
    dest_dir: &Path,
    names: &Names,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, Failure> {
    for suffix in 1..MAX_NUMBER {
        let destination = dest_dir.join(names.for_suffix(suffix));
        if occupied(&destination)? {
            continue;
        }
        observer.before_publish(&destination)?;
        match publish(staged, &destination) {
            Ok(()) => {
                super::files::best_effort(
                    "making a published name survive a crash",
                    &destination,
                    || super::sync_parent(&destination),
                );
                return Ok(destination);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Failure::Refused(error.to_string())),
        }
    }
    Err(Failure::NamesExhausted {
        name: names.original.clone(),
        dir: dest_dir.to_path_buf(),
    })
}
