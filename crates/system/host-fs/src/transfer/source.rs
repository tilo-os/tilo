//! Hiding a move's source, and the ways back: putting it where it was, or —
//! when even that is refused — keeping the complete copy a person can take it
//! from.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use super::{Failure, Staging, TransferObserver, copy, publish, sync_parent};

/// The hidden name a move's source takes in its own folder while its copy is
/// published, so putting it back is a rename.
const QUARANTINE_PREFIX: &str = ".tilo-transfer-source-";

/// Hide `source` under a free hidden name of its own folder.
pub(crate) fn quarantine(
    source: &Path,
    observer: &mut impl TransferObserver,
) -> io::Result<std::path::PathBuf> {
    for _ in 0..32 {
        let hidden = quarantine_path(source)?;
        match quarantine_at(source, &hidden, observer) {
            Ok(hidden) => return Ok(hidden),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a hidden name beside the source",
    ))
}

/// A hidden name beside `source`, for a caller that must write it down
/// before the source is hidden.
pub fn quarantine_path(source: &Path) -> io::Result<std::path::PathBuf> {
    let parent = source
        .parent()
        .ok_or_else(|| io::Error::other("a moved item has no folder"))?;
    Ok(parent.join(format!("{QUARANTINE_PREFIX}{}", uuid::Uuid::new_v4())))
}

/// [`quarantine`] to the hidden name `hidden`, which the caller chose and
/// wrote down before any destructive step.
pub fn quarantine_at(
    source: &Path,
    hidden: &Path,
    observer: &mut impl TransferObserver,
) -> io::Result<std::path::PathBuf> {
    if hidden.parent() != source.parent() {
        return Err(io::Error::other(
            "a hidden source must be in the source's own folder",
        ));
    }
    observer.before_source_quarantine(source, hidden)?;
    crate::rename_noreplace(source, hidden)?;
    Ok(hidden.to_path_buf())
}

/// Put a hidden source back where it was: under its name, or beside a name
/// something took again meanwhile, so neither is lost. A source that is no
/// longer hidden is nothing to put back.
pub fn restore_source(
    hidden: &Path,
    source: &Path,
    observer: &mut impl TransferObserver,
) -> Result<(), Failure> {
    if std::fs::symlink_metadata(hidden).is_err() {
        return Ok(());
    }
    observer.before_source_restore(source, hidden)?;
    if std::fs::symlink_metadata(source).is_ok() {
        let folder = source
            .parent()
            .ok_or_else(|| Failure::Refused(format!("{} has no folder", source.display())))?;
        let name = source.file_name().ok_or(Failure::NoSourceName)?;
        let beside = publish::free_name(folder, name)?.ok_or_else(|| {
            Failure::Refused(format!(
                "{} has its name back and no other is free",
                source.display()
            ))
        })?;
        crate::rename_noreplace(hidden, &beside).map_err(|error| {
            Failure::Refused(format!(
                "{} could not be put back: {error}",
                beside.display()
            ))
        })?;
        best_effort_sync(&beside);
        return Ok(());
    }
    crate::rename_noreplace(hidden, source).map_err(|error| {
        Failure::Refused(format!(
            "{} could not be put back: {error}",
            source.display()
        ))
    })?;
    best_effort_sync(source);
    Ok(())
}

fn best_effort_sync(path: &Path) {
    super::files::best_effort("making a put-back name survive a crash", path, || {
        sync_parent(path)
    });
}

/// A source whose deletion or publication was refused goes back, unless the
/// caller's authority to put it back was refused too; then the copy is what
/// the person still has.
pub(crate) fn rollback_quarantined_source(
    source: &Path,
    hidden: &Path,
    staging: Staging,
    observer: &mut impl TransferObserver,
    original_error: String,
) -> Failure {
    match restore_source(hidden, source, observer) {
        Ok(()) => staging.refuse(original_error),
        Err(error) => {
            let mut kept = staging;
            kept.keep();
            Failure::Refused(format!(
                "{original_error}; source retained at {}: {error}; staged data retained at {}",
                hidden.display(),
                kept.path().display()
            ))
        }
    }
}

/// Put a move's source back from what was published: the copy is copied back
/// over the source, and the hidden source is taken away. When that fails, the
/// copy stays where it is and the failure says so.
pub(crate) fn restore_failed_cut(
    source: &Path,
    hidden: &Path,
    staging: Staging,
    original_error: impl Into<Failure>,
    observer: &mut impl TransferObserver,
) -> Failure {
    let original = original_error.into();
    let original_error = original.to_string();
    let staged = staging.payload();
    let mut recovery = RecoveryObserver {
        observer,
        source,
        staged: &staged,
    };
    let restored = recovery
        .before_copy_source(&staged)
        .and_then(|()| copy::copy_tree(&staged, source, &mut recovery));
    match restored {
        Ok(()) => {
            if let Err(error) = observer.before_source_restore(source, &staged) {
                let mut kept = staging;
                kept.keep();
                return Failure::Refused(format!(
                    "{original_error}; recovery cleanup refused: {error}; staged data retained at {}",
                    kept.path().display()
                ));
            }
            let hidden_error = if hidden.as_os_str().is_empty() {
                None
            } else {
                super::remove_path(hidden).err()
            };
            let mut error = staging.refuse(original);
            if let Some(hidden_error) = hidden_error
                && let Failure::Refused(text) = &mut error
            {
                let _ = write!(
                    text,
                    "; partial hidden source retained at {}: {hidden_error}",
                    hidden.display()
                );
            }
            error
        }
        Err(restore_error) => {
            let mut kept = staging;
            kept.keep();
            Failure::Refused(format!(
                "{original_error}; source restoration failed: {restore_error}; staged data retained at {}",
                kept.path().display()
            ))
        }
    }
}

/// Restoring a source is a copy back that must answer the caller's live
/// authority as it goes, not only at its start.
struct RecoveryObserver<'a, T> {
    observer: &'a mut T,
    source: &'a Path,
    staged: &'a Path,
}

impl<T: TransferObserver> TransferObserver for RecoveryObserver<'_, T> {
    fn before_copy_source(&mut self, _: &Path) -> io::Result<()> {
        self.observer
            .before_source_restore(self.source, self.staged)
    }

    fn open_source(
        &mut self,
        parent: &super::SourceDirectory,
        name: &std::ffi::OsStr,
        options: &super::SourceOpenOptions,
    ) -> io::Result<std::fs::File> {
        self.observer.open_source(parent, name, options)
    }

    fn copy_bytes(
        &mut self,
        source: &mut std::fs::File,
        destination: &mut std::fs::File,
    ) -> io::Result<u64> {
        use std::io::{Read as _, Write as _};
        let mut buffer = vec![0; 256 * 1024];
        let mut total = 0_u64;
        loop {
            self.observer
                .before_source_restore(self.source, self.staged)?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                return Ok(total);
            }
            destination.write_all(
                buffer
                    .get(..count)
                    .ok_or_else(|| io::Error::other("invalid recovery read length"))?,
            )?;
            total = total.saturating_add(u64::try_from(count).map_err(io::Error::other)?);
        }
    }
}
