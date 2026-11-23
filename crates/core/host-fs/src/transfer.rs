//! Copying a file or folder into a folder, crash-safely, and the pieces that
//! moving one needs.
//!
//! A copy is made privately and published by a rename: the whole copy is
//! staged in an owner-only `.tilo-transfer-<id>` folder inside the receiving
//! folder, flushed, and only then does it take its name. A power cut therefore
//! never shows a partly copied file under a real name, and every name it may
//! take is one the destination does not have yet — never a replacement.
//!
//! A move first hides its source under a hidden name of its own folder (so it
//! can be put back by a rename), then removes it once the copy stands under
//! its own name. When the copy cannot be published, the source comes back out
//! of hiding; when even that is refused, the complete copy is retained and the
//! failure says where it is. Nothing is ever half of either.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

mod copy;
mod files;
mod publish;
mod source;
mod stage;
#[cfg(test)]
mod tests;

pub use files::{best_effort, best_effort_by, remove_path, sync_directory, sync_parent, trash};
pub use publish::{Names, free_name};
pub use source::{quarantine_at, quarantine_path, restore_source};
pub use stage::{Staging, staging_path};

use publish::{MAX_NUMBER, occupied};
use source::{quarantine, restore_failed_cut, rollback_quarantined_source};

pub use cap_std::fs::{Dir as SourceDirectory, OpenOptions as SourceOpenOptions};

#[derive(Debug)]
pub enum Failure {
    InvalidName,
    NoSourceName,
    AlreadyHere,
    /// A folder cannot be copied or moved into itself.
    IntoItself,
    Inspect {
        path: PathBuf,
        error: io::Error,
    },
    NamesExhausted {
        name: OsString,
        dir: PathBuf,
    },
    Refused(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => out.write_str("that is not a name a file can take"),
            Self::NoSourceName => out.write_str("the item has no name"),
            Self::AlreadyHere => out.write_str("the item is already in this folder"),
            Self::IntoItself => out.write_str("cannot copy or move a folder into itself"),
            Self::Inspect { path, error } => {
                write!(out, "{} could not be examined: {error}", path.display())
            }
            Self::NamesExhausted { name, dir } => write!(
                out,
                "every free name for {} in {} is taken",
                name.to_string_lossy(),
                dir.display()
            ),
            Self::Refused(reason) => out.write_str(reason),
        }
    }
}

impl std::error::Error for Failure {}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Refused(error.to_string())
    }
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

pub trait TransferObserver {
    /// Open a leaf of a retained source directory. A host may implement its
    /// platform's nonblocking open without putting platform flags in the app.
    fn open_source(
        &mut self,
        parent: &SourceDirectory,
        name: &OsStr,
        options: &SourceOpenOptions,
    ) -> io::Result<std::fs::File> {
        parent
            .open_with(name, options)
            .map(cap_std::fs::File::into_std)
    }

    /// Retain the staged copy's identity before any Cut deletion or
    /// publication.
    fn staged_source(&mut self, _staging: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_source_quarantine(&mut self, _source: &Path, _quarantine: &Path) -> io::Result<()> {
        Ok(())
    }

    fn allow_direct_rename(&self) -> bool {
        true
    }

    /// Check live admission before reading each source entry.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn before_copy_source(&mut self, _source: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Check live admission immediately before publication.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn before_publish(&mut self, _destination: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Check live admission immediately before deleting a hidden source.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn before_source_remove(&mut self, _source: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_source_restore(&mut self, _source: &Path, _staged: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Remove the already-admitted hidden source.
    ///
    /// # Errors
    /// Reports a filesystem removal failure, including partial deletion.
    fn remove_source(&mut self, source: &Path) -> io::Result<()> {
        remove_path(source)
    }

    /// Validate the hidden leaf before any destructive step.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn after_source_quarantine(&mut self, _source: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Validate the actual opened descriptor before reading its contents.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn opened_source(&mut self, _path: &Path, _source: &std::fs::File) -> io::Result<()> {
        Ok(())
    }

    /// Optionally provide a target captured from the exact retained symlink.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn symlink_target(&mut self, _path: &Path) -> io::Result<Option<PathBuf>> {
        Ok(None)
    }

    /// Choose permissions for a freshly created destination file in private
    /// staging. Ordinary transfers preserve the source permissions.
    fn destination_permissions(&mut self, source: std::fs::Permissions) -> std::fs::Permissions {
        source
    }

    /// Copy bytes while observing cancellation and live admission.
    ///
    /// # Errors
    /// Stops the transfer on lost authority, cancellation or I/O failure.
    fn copy_bytes(
        &mut self,
        source: &mut std::fs::File,
        destination: &mut std::fs::File,
    ) -> io::Result<u64> {
        io::copy(source, destination)
    }
}

impl TransferObserver for () {}

/// Copy `source` into `dest_dir` under its own name, or the first free
/// numbered one; with `remove_source`, move it.
pub fn transfer(
    source: &Path,
    dest_dir: &Path,
    remove_source: bool,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, Failure> {
    let name = source.file_name().ok_or(Failure::NoSourceName)?;
    let name = name.to_os_string();
    transfer_named(source, dest_dir, &name, remove_source, observer)
}

/// [`transfer`] to one explicit leaf name.
///
/// # Errors
/// As [`transfer`].
pub fn transfer_named(
    source: &Path,
    dest_dir: &Path,
    name: &OsStr,
    remove_source: bool,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, Failure> {
    let names = admit(source, dest_dir, name, remove_source)?;

    if remove_source && observer.allow_direct_rename() {
        for suffix in 1..MAX_NUMBER {
            let destination = dest_dir.join(names.for_suffix(suffix));
            if occupied(&destination)? {
                continue;
            }
            observer.before_publish(&destination)?;
            match crate::rename_noreplace(source, &destination) {
                Ok(()) => {
                    best_effort("making a moved name survive a crash", &destination, || {
                        sync_parent(&destination)
                    });
                    return Ok(destination);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => break,
            }
        }
    }

    let staging = Staging::create(dest_dir)?;
    if let Err(error) = staging.stage_payload(source, observer) {
        return Err(staging.refuse(error));
    }
    if let Err(error) = observer.staged_source(&staging.payload()) {
        return Err(staging.refuse(error));
    }
    if let Err(error) = staging.sync() {
        return Err(staging.refuse(error));
    }
    finish(source, dest_dir, &names, remove_source, staging, observer)
}

/// Write `bytes` to `dest_dir` under `name`, or the first free numbered one.
///
/// # Errors
/// Refuses invalid names, collisions on every numbered name, and I/O
/// failures; a failure leaves nothing under a real name.
pub fn transfer_bytes(
    bytes: &[u8],
    dest_dir: &Path,
    name: &OsStr,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, Failure> {
    let names = admit_name(dest_dir, name)?;
    let staging = Staging::create(dest_dir)?;
    if let Err(error) = staging.stage_bytes(bytes, observer) {
        return Err(staging.refuse(error));
    }
    if let Err(error) = staging.sync() {
        return Err(staging.refuse(error));
    }
    match publish::publish_numbered(&staging.payload(), dest_dir, &names, observer) {
        Ok(destination) => {
            staging.finish_success();
            Ok(destination)
        }
        Err(error) => Err(staging.refuse(error)),
    }
}

/// Receive `bytes` into the folder held open as `receiving` under `name`, or
/// its first free numbered name. `display` is that folder as a person sees it.
///
/// # Errors
/// As [`transfer_bytes`]: a failure leaves nothing under a real name.
#[cfg(unix)]
pub fn receive_bytes(
    receiving: &std::fs::File,
    display: &Path,
    name: &OsStr,
    bytes: &[u8],
    observer: &mut impl TransferObserver,
) -> Result<OsString, Failure> {
    let mut staging = Staging::in_folder(receiving, display)?;
    if let Err(error) = staging.stage_bytes(bytes, observer) {
        return Err(staging.refuse(error));
    }
    if let Err(error) = staging.sync() {
        return Err(staging.refuse(error));
    }
    staging.publish_payload(name, observer)
}

/// Receive the file or folder at `source` into the folder held open as
/// `receiving` under `name`, or its first free numbered name. The source is
/// left where it is.
///
/// # Errors
/// As [`receive_bytes`], and when the source cannot be read.
#[cfg(unix)]
pub fn receive_file(
    receiving: &std::fs::File,
    display: &Path,
    name: &OsStr,
    source: &Path,
    observer: &mut impl TransferObserver,
) -> Result<OsString, Failure> {
    let mut staging = Staging::in_folder(receiving, display)?;
    if let Err(error) = staging.stage_payload(source, observer) {
        return Err(staging.refuse(error));
    }
    if let Err(error) = staging.sync() {
        return Err(staging.refuse(error));
    }
    staging.publish_payload(name, observer)
}

/// Whether `name` may be received into `dest_dir`: one leaf, and a
/// destination whose first name can be examined. The names it may take.
fn admit_name(dest_dir: &Path, name: &OsStr) -> Result<Names, Failure> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(Failure::InvalidName);
    }
    let names = Names::new(name);
    // An inaccessible destination is refused before any staging is made.
    occupied(&dest_dir.join(names.for_suffix(1)))?;
    Ok(names)
}

/// Whether `source` may go to `dest_dir` as `name`: one leaf, not into
/// itself, not a move into the folder it is already in, and a destination
/// whose first name can be examined. The names it may be published under.
fn admit(
    source: &Path,
    dest_dir: &Path,
    name: &OsStr,
    remove_source: bool,
) -> Result<Names, Failure> {
    let names = admit_name(dest_dir, name)?;
    ensure_destination_is_outside_source(source, dest_dir)?;
    let source_parent = source
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok());
    let destination_dir = std::fs::canonicalize(dest_dir).ok();
    if remove_source && source_parent.is_some() && source_parent == destination_dir {
        return Err(Failure::AlreadyHere);
    }
    Ok(names)
}

/// Refuse a recursive copy or move before any destination exists. Resolving
/// the destination catches a symlink that points back into `source`.
fn ensure_destination_is_outside_source(source: &Path, dest_dir: &Path) -> Result<(), Failure> {
    if !std::fs::symlink_metadata(source)?.is_dir() {
        return Ok(());
    }
    let source = std::fs::canonicalize(source)?;
    let destination = std::fs::canonicalize(dest_dir)?;
    if destination.starts_with(&source) {
        return Err(Failure::IntoItself);
    }
    Ok(())
}

/// The second half of a transfer whose copy is staged: for a move, hide the
/// source and remove it; then publish the copy under the first free name.
fn finish(
    source: &Path,
    dest_dir: &Path,
    names: &Names,
    remove_source: bool,
    mut staging: Staging,
    observer: &mut impl TransferObserver,
) -> Result<PathBuf, Failure> {
    if remove_source {
        // From hiding onward, unwinding must retain the complete copy: the
        // source may already be partially or wholly deleted. Normal error and
        // success paths explicitly restore or clean this container.
        staging.keep();
        let quarantine = match quarantine(source, observer) {
            Ok(path) => path,
            Err(error) => return Err(staging.refuse(error)),
        };
        if let Err(error) = observer
            .after_source_quarantine(&quarantine)
            .and_then(|()| observer.before_source_remove(&quarantine))
        {
            return Err(rollback_quarantined_source(
                source,
                &quarantine,
                staging,
                observer,
                error.to_string(),
            ));
        }
        let deletion =
            crate::mark_private(&quarantine).and_then(|()| observer.remove_source(&quarantine));
        if let Err(error) = deletion {
            return Err(restore_failed_cut(
                source,
                &quarantine,
                staging,
                error.to_string(),
                observer,
            ));
        }
    }

    match publish::publish_numbered(&staging.payload(), dest_dir, names, observer) {
        Ok(destination) => {
            staging.finish_success();
            Ok(destination)
        }
        Err(publish_error) if remove_source => Err(restore_failed_cut(
            source,
            Path::new(""),
            staging,
            publish_error,
            observer,
        )),
        Err(error) => Err(staging.refuse(error)),
    }
}
