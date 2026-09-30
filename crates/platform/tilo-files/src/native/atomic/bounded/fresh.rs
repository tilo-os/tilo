//! Fresh creation under retained privacy profiles; no administrative authority.
#![expect(
    clippy::result_large_err,
    reason = "custody travels inline in the error: bounded operations request no heap allocation"
)]
#[cfg(target_os = "macos")]
use super::AdmissionError;
use super::{BoundedOwnerDirectory, OsStr, OsStrExt, StorageDirectory, io, name_check};
/// Parent provisioning admission is NOT a bounded native membership operation.
/// Keep the original writer lease/exclusion separately; this holder grants none.
#[cfg(target_os = "macos")]
pub struct OwnerOnlyProvisioning {
    directory: StorageDirectory,
}
#[cfg(target_os = "macos")]
impl OwnerOnlyProvisioning {
    /// Ordinary `OwnerOnly` validation may allocate in native directory services.
    /// It is intentionally separate from all bounded operation claims.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] retaining `directory` when it is not
    /// currently owner-only.
    pub fn admit(directory: StorageDirectory) -> Result<Self, AdmissionError> {
        match directory.validate_privacy(crate::native::atomic::Privacy::OwnerOnly) {
            Ok(()) => Ok(Self { directory }),
            Err(source) => Err(AdmissionError { source, directory }),
        }
    }
    /// Exclusively create an absent child under the already admitted parent.
    /// Caller-held exclusion and stable root permissions must span mkdir/open:
    /// there is no claim against hostile same-principal name replacement.
    /// Never adopts or normalizes an existing name. Errors never remove names.
    ///
    /// # Errors
    ///
    /// Returns a [`FreshChildError`] whose state says whether a child was
    /// created; created custody is retained for retry.
    pub fn create_child(&self, name: &OsStr) -> Result<FreshPrivateChild, FreshChildError> {
        create_child(&self.directory, name, ParentProfile::OrdinaryAdmitted)
    }
}

impl BoundedOwnerDirectory {
    /// Exclusively create an absent child using this original bounded parent.
    /// Caller retains its actual administrative owner and writer exclusion.
    /// Refusals preserve ambiguity; no existing name is adopted or removed.
    ///
    /// # Errors
    ///
    /// Returns a [`FreshChildError`] for an invalid name, a parent that fails
    /// validation, or a failed creation; created custody is retained for retry.
    pub fn create_fresh_child(&self, name: &OsStr) -> Result<FreshPrivateChild, FreshChildError> {
        name_check(name)
            .and_then(|()| self.validate())
            .map_err(|source| FreshChildError {
                source,
                custody: None,
            })?;
        create_child(&self.directory, name, ParentProfile::Bounded)
    }
}

#[derive(Clone, Copy)]
enum ParentProfile {
    #[cfg(target_os = "macos")]
    OrdinaryAdmitted,
    Bounded,
}
impl ParentProfile {
    fn validate(self, parent: &StorageDirectory) -> io::Result<()> {
        match self {
            #[cfg(target_os = "macos")]
            Self::OrdinaryAdmitted => Ok(()),
            Self::Bounded => parent.inner.validate_bounded(),
        }
    }
}

fn create_child(
    directory: &StorageDirectory,
    name: &OsStr,
    parent_profile: ParentProfile,
) -> Result<FreshPrivateChild, FreshChildError> {
    let parent = (|| {
        name_check(name)?;
        Ok(StorageDirectory {
            inner: directory.inner.try_clone()?,
        })
    })()
    .map_err(|source| FreshChildError {
        source,
        custody: None,
    })?;
    let mut custody = FreshPrivateChild {
        parent,
        parent_profile,
        child: None,
        name: [0; 200],
        length: name.as_bytes().len(),
        created: false,
        submitted: false,
    };
    // `name_check` bounds the name by the buffer; refuse rather than panic.
    let Some(slot) = custody.name.get_mut(..custody.length) else {
        return Err(FreshChildError {
            source: io::ErrorKind::InvalidInput.into(),
            custody: None,
        });
    };
    slot.copy_from_slice(name.as_bytes());
    let result = (|| {
        custody.parent_profile.validate(&custody.parent)?;
        custody.submitted = true;
        if let Err(error) = custody.parent.inner.mkdir_fresh_private(name) {
            if error.kind() == io::ErrorKind::AlreadyExists {
                custody.submitted = false;
            }
            return Err(error);
        }
        #[cfg(test)]
        checkpoint(FreshCheckpoint::AfterMkdir)?;
        custody.created = true;
        custody.child = Some(StorageDirectory {
            inner: custody.parent.inner.open_directory(name)?,
        });
        Ok(())
    })();
    match result {
        Ok(()) => Ok(custody),
        Err(source) => Err(FreshChildError {
            source,
            custody: Some(custody),
        }),
    }
}
/// Move-only custody obtainable only after this API's exclusive mkdir succeeds.
/// No raw File/StorageDirectory adoption or automatic name removal exists.
pub struct FreshPrivateChild {
    parent: StorageDirectory,
    parent_profile: ParentProfile,
    child: Option<StorageDirectory>,
    name: [u8; 200],
    length: usize,
    created: bool,
    submitted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshChildState {
    NotCreated,
    CreationUnknown,
    CreatedUncommitted,
}
pub struct FreshChildError {
    source: io::Error,
    custody: Option<FreshPrivateChild>,
}
impl FreshChildError {
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }
    pub fn state(&self) -> FreshChildState {
        if self.custody.as_ref().is_some_and(|c| c.created) {
            FreshChildState::CreatedUncommitted
        } else if self.custody.as_ref().is_some_and(|c| c.submitted) {
            FreshChildState::CreationUnknown
        } else {
            FreshChildState::NotCreated
        }
    }
    /// Retry normalization and commit of retained created custody.
    ///
    /// # Errors
    ///
    /// Returns `self` unchanged when no created child is retained, or a new
    /// error when normalization or commit fails again.
    pub fn retry_normalize_and_commit(mut self) -> Result<BoundedOwnerDirectory, Self> {
        if self
            .custody
            .as_ref()
            .is_some_and(|c| c.created && c.child.is_some())
        {
            // The predicate above proves the same existing custody is present.
            if let Some(custody) = self.custody.take() {
                return custody.normalize_and_commit();
            }
        }
        Err(self)
    }
}
impl std::fmt::Debug for FreshChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FreshChildError")
            .field("source", &self.source)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}
impl FreshPrivateChild {
    /// Normalize only this newly-created, still-empty child: a non-inheriting
    /// ACL on macOS, private mode with preserved POSIX ACLs on Linux. Sync child
    /// and original parent and verify the exact name before returning. Failure
    /// retains native custody; dropping it leaves an explicitly incomplete child.
    ///
    /// # Errors
    ///
    /// Returns a [`FreshChildError`] retaining this custody when the binding
    /// changed, the child is not empty, or a barrier fails.
    pub fn normalize_and_commit(mut self) -> Result<BoundedOwnerDirectory, FreshChildError> {
        let result = self.normalize_inner();
        match result {
            Ok(()) => match self.child.take() {
                Some(directory) => Ok(BoundedOwnerDirectory { directory }),
                None => Err(FreshChildError {
                    source: io::ErrorKind::InvalidData.into(),
                    custody: Some(self),
                }),
            },
            Err(source) => Err(FreshChildError {
                source,
                custody: Some(self),
            }),
        }
    }
    fn binding(&self) -> io::Result<()> {
        self.parent_profile.validate(&self.parent)?;
        let child = self.child.as_ref().ok_or(io::ErrorKind::InvalidData)?;
        let name = OsStr::from_bytes(
            self.name
                .get(..self.length)
                .ok_or(io::ErrorKind::InvalidData)?,
        );
        let named = self.parent.inner.open_directory(name)?;
        if named.identity()? != child.inner.identity()? {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }
    fn normalize_inner(&self) -> io::Result<()> {
        if !self.created {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.binding()?;
        let child = self.child.as_ref().ok_or(io::ErrorKind::InvalidData)?;
        child.inner.normalize_fresh_empty()?;
        self.binding()?;
        #[cfg(test)]
        checkpoint(FreshCheckpoint::Child)?;
        child.inner.sync()?;
        self.binding()?;
        #[cfg(test)]
        checkpoint(FreshCheckpoint::Parent)?;
        self.parent.inner.sync()?;
        self.binding()?;
        child.inner.validate_bounded()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FreshCheckpoint {
    AfterMkdir,
    Child,
    Parent,
}
#[cfg(test)]
std::thread_local! { static FAILURE: std::cell::Cell<Option<FreshCheckpoint>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn checkpoint(at: FreshCheckpoint) -> io::Result<()> {
    if FAILURE.with(|slot| slot.get() == Some(at)) {
        Err(io::Error::from_raw_os_error(5))
    } else {
        Ok(())
    }
}
#[cfg(test)]
pub(super) fn with_failure<T>(at: FreshCheckpoint, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<FreshCheckpoint>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURE.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(FAILURE.with(|slot| slot.replace(Some(at))));
    f()
}

impl std::fmt::Display for FreshChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for FreshChildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
