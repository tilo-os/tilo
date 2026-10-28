use crate::native::atomic::native;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

/// Requested privacy semantics for a file or explicitly validated held directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Privacy {
    /// Use the platform's ordinary inherited creation policy.
    Inherit,
    /// Restrict access to the effective owner.
    OwnerOnly,
}

/// Namespace publication behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishMode {
    /// Atomically replace an existing target.
    Replace,
    /// Publish only when the target name is absent.
    IfAbsent,
}

/// What is known about a failed publication operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
    /// Publication was not submitted or is proven not to have happened.
    NotPublished,
    /// Publication happened, but its durability commit did not complete.
    PublishedDurabilityUnknown,
    /// Publication may or may not have happened.
    PublicationUnknown,
}

pub(crate) struct CommitGuard {
    inner: native::CommitGuard,
}

impl CommitGuard {
    pub(crate) fn new(inner: native::CommitGuard) -> Self {
        Self { inner }
    }

    pub(crate) fn commit(&self) -> io::Result<()> {
        self.inner.commit()
    }
}

/// An I/O failure annotated with the publication phase in which it occurred.
pub struct PublicationError {
    state: PublicationState,
    source: io::Error,
    pending_commit: Option<CommitGuard>,
}

impl PublicationError {
    pub(crate) fn new(state: PublicationState, source: io::Error) -> Self {
        Self {
            state,
            source,
            pending_commit: None,
        }
    }

    pub(crate) fn pending(source: io::Error, guard: CommitGuard) -> Self {
        Self {
            state: PublicationState::PublishedDurabilityUnknown,
            source,
            pending_commit: Some(guard),
        }
    }

    /// Return what is known about namespace publication.
    #[must_use]
    pub fn state(&self) -> PublicationState {
        self.state
    }

    /// Return the original I/O failure.
    #[must_use]
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }

    /// Wrap this typed error in an [`io::Error`] without losing its phase.
    #[must_use]
    pub fn into_io_error(self) -> io::Error {
        let kind = self.source.kind();
        io::Error::new(kind, self)
    }

    /// Retry only a known post-publication durability commit.
    ///
    /// The retained native file and parent handles are reused. No rename is
    /// issued. Other states return this error unchanged.
    pub fn retry_commit(mut self) -> Result<(), Self> {
        let Some(guard) = self.pending_commit.as_ref() else {
            return Err(self);
        };
        match guard.commit() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.source = error;
                Err(self)
            }
        }
    }
}

impl fmt::Debug for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicationError")
            .field("state", &self.state)
            .field("source", &self.source)
            .field("has_pending_commit", &self.pending_commit.is_some())
            .finish()
    }
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl Error for PublicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Recover publication phase information from the compatibility I/O wrapper.
#[must_use]
pub fn publication_state(error: &io::Error) -> Option<PublicationState> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<PublicationError>())
        .map(PublicationError::state)
}

/// Durably replace `path` through an opened parent directory.
///
/// # Errors
///
/// Returns a [`PublicationError`] carrying the known publication state when
/// the parent cannot be opened or staging, rename or a barrier fails.
pub fn replace(path: &Path, bytes: &[u8], privacy: Privacy) -> Result<(), PublicationError> {
    publish_path(path, bytes, PublishMode::Replace, privacy, None)
}

/// Durably publish `path` only when the target name is absent.
///
/// # Errors
///
/// Returns a [`PublicationError`]; an existing target is reported as
/// `AlreadyExists` without publication.
pub fn publish_if_absent(
    path: &Path,
    bytes: &[u8],
    privacy: Privacy,
) -> Result<(), PublicationError> {
    publish_path(path, bytes, PublishMode::IfAbsent, privacy, None)
}

pub(crate) fn replace_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), PublicationError> {
    #[cfg(windows)]
    {
        validate_windows_mode(mode)?;
        publish_path(path, bytes, PublishMode::Replace, Privacy::OwnerOnly, None)
    }
    #[cfg(unix)]
    publish_path(
        path,
        bytes,
        PublishMode::Replace,
        Privacy::Inherit,
        Some(mode),
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn save_with_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), PublicationError> {
    let name = path.file_name().ok_or_else(|| {
        PublicationError::new(
            PublicationState::NotPublished,
            io::ErrorKind::InvalidInput.into(),
        )
    })?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = crate::native::atomic::StorageDirectory::open(parent)
        .map_err(|error| PublicationError::new(PublicationState::NotPublished, error))?;
    directory.publish_atomic_save(name, bytes, mode)
}

#[cfg(windows)]
pub(crate) fn validate_windows_mode(mode: u32) -> Result<(), PublicationError> {
    if mode == 0o600 {
        return Ok(());
    }
    Err(PublicationError::new(
        PublicationState::NotPublished,
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows exact modes support only 0o600 (OwnerOnly)",
        ),
    ))
}

fn publish_path(
    path: &Path,
    bytes: &[u8],
    mode: PublishMode,
    privacy: Privacy,
    exact_mode: Option<u32>,
) -> Result<(), PublicationError> {
    let name = path.file_name().ok_or_else(|| {
        PublicationError::new(
            PublicationState::NotPublished,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication path has no file name",
            ),
        )
    })?;
    let parent = path.parent().map_or_else(
        || Path::new("."),
        |parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        },
    );
    let directory = crate::native::atomic::StorageDirectory::open(parent)
        .map_err(|source| PublicationError::new(PublicationState::NotPublished, source))?;
    directory.publish_with_mode(name, bytes, mode, privacy, exact_mode)
}
