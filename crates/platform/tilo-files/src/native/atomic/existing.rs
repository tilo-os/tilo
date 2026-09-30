//! Commit custody for an existing Unix file/name; deliberately not publication.
use crate::native::atomic::{FileIdentity, Privacy, StorageDirectory, native};
use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io,
};

struct Custody {
    parent: native::Directory,
    file: File,
    name: OsString,
    expected: FileIdentity,
    privacy: native::FilePolicy,
}

/// Failure committing an already existing object. Drop never removes anything.
pub struct ExistingCommitError {
    source: io::Error,
    changed: bool,
    custody: Option<Custody>,
}
impl ExistingCommitError {
    pub fn source_changed(&self) -> bool {
        self.changed
    }
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.source.kind(), self)
    }
    /// Revalidate and repeat data/name barriers using retained authority only.
    ///
    /// # Errors
    ///
    /// Returns `self` unchanged without retained custody, or with the new error
    /// when revalidation or a barrier fails again.
    pub fn retry_commit(mut self) -> Result<(), Self> {
        let Some(custody) = &self.custody else {
            return Err(self);
        };
        match custody.commit() {
            Ok(()) => Ok(()),
            Err((source, changed)) => {
                self.source = source;
                self.changed = changed;
                Err(self)
            }
        }
    }
}
impl fmt::Debug for ExistingCommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExistingCommitError")
            .field("source", &self.source)
            .field("source_changed", &self.changed)
            .field("retained", &self.custody.is_some())
            .finish()
    }
}
impl fmt::Display for ExistingCommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}
impl Error for ExistingCommitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
fn observed(error: io::Error) -> (io::Error, bool) {
    let changed = matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    );
    (error, changed)
}
fn unchanged(
    actual: &FileIdentity,
    expected: &FileIdentity,
    privacy: native::FilePolicy,
) -> Result<(), (io::Error, bool)> {
    if actual == expected {
        Ok(())
    } else {
        Err((
            privacy.error(io::ErrorKind::Other, "existing file binding changed"),
            true,
        ))
    }
}
impl Custody {
    fn validate(&self) -> Result<(), (io::Error, bool)> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if matches!(self.privacy, native::FilePolicy::BoundedOwner) {
            self.parent.validate_bounded().map_err(|e| (e, false))?;
        }
        unchanged(
            &FileIdentity::of(&self.file).map_err(|e| (e, false))?,
            &self.expected,
            self.privacy,
        )?;
        let named = self
            .parent
            .open_read_policy(&self.name, self.privacy)
            .map_err(observed)?;
        unchanged(
            &FileIdentity::of(&named).map_err(|e| (e, false))?,
            &self.expected,
            self.privacy,
        )
    }
    fn commit(&self) -> Result<(), (io::Error, bool)> {
        self.validate()?;
        checkpoint(false).map_err(|e| (e, false))?;
        self.file.sync_all().map_err(|e| (e, false))?;
        self.validate()?;
        checkpoint(true).map_err(|e| (e, false))?;
        self.parent.sync().map_err(|e| (e, false))?;
        self.validate()
    }
}
pub(crate) fn commit(
    parent: &StorageDirectory,
    name: &OsStr,
    expected: &FileIdentity,
    privacy: Privacy,
) -> Result<(), ExistingCommitError> {
    commit_policy(
        parent,
        name,
        expected,
        native::FilePolicy::Ordinary(privacy),
    )
}
pub(crate) fn commit_policy(
    parent: &StorageDirectory,
    name: &OsStr,
    expected: &FileIdentity,
    privacy: native::FilePolicy,
) -> Result<(), ExistingCommitError> {
    let prepared = (|| {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if matches!(privacy, native::FilePolicy::BoundedOwner) {
            crate::native::atomic::bounded::name_check(name)?;
            parent.inner.validate_bounded()?;
        }
        let parent = parent.inner.try_clone()?;
        let file = parent.open_writable_policy(name, privacy, false, false)?;
        Ok::<_, io::Error>(Custody {
            parent,
            file,
            name: name.to_owned(),
            expected: expected.clone(),
            privacy,
        })
    })();
    let custody = prepared.map_err(|source| {
        let (source, changed) = observed(source);
        ExistingCommitError {
            source,
            changed,
            custody: None,
        }
    })?;
    custody
        .commit()
        .map_err(|(source, changed)| ExistingCommitError {
            source,
            changed,
            custody: Some(custody),
        })
}
#[cfg_attr(
    not(feature = "test-support"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the test-support build injects failures through the same signature"
    )
)]
fn checkpoint(namespace: bool) -> io::Result<()> {
    #[cfg(feature = "test-support")]
    {
        crate::native::atomic::test_support::existing_checkpoint(if namespace {
            crate::native::atomic::test_support::ExistingCheckpoint::BeforeNamespaceFlush
        } else {
            crate::native::atomic::test_support::ExistingCheckpoint::BeforeDataFlush
        })
    }
    #[cfg(not(feature = "test-support"))]
    {
        let _ = namespace;
        Ok(())
    }
}
