use crate::native::atomic::native;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Privacy {
    Inherit,
    OwnerOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishMode {
    Replace,
    IfAbsent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
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

    #[must_use]
    pub fn state(&self) -> PublicationState {
        self.state
    }

    #[must_use]
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }

    #[must_use]
    pub fn into_io_error(self) -> io::Error {
        let kind = self.source.kind();
        io::Error::new(kind, self)
    }

    /// Retries only a known post-publication durability commit: the retained file and parent handles
    /// are reused and no rename is issued. Other states return the error unchanged.
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

#[must_use]
pub fn publication_state(error: &io::Error) -> Option<PublicationState> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<PublicationError>())
        .map(PublicationError::state)
}

pub fn replace(path: &Path, bytes: &[u8], privacy: Privacy) -> Result<(), PublicationError> {
    publish_path(path, bytes, PublishMode::Replace, privacy, None)
}

/// Appends durably: written and synced before this returns. The file is one a publication made, so its
/// name and every byte appended before are durable already.
pub fn append(path: &Path, bytes: &[u8]) -> Result<(), PublicationError> {
    use std::io::Write as _;
    let not_written = |error| PublicationError::new(PublicationState::NotPublished, error);
    let unknown = |error| PublicationError::new(PublicationState::PublicationUnknown, error);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(not_written)?;
    append_checkpoint(AppendCheckpoint::BeforeWrite).map_err(not_written)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_data())
        .map_err(unknown)?;
    append_checkpoint(AppendCheckpoint::Written).map_err(unknown)
}

/// Where an append can be made to fail in a test: before anything is written, or once it is written and
/// synced but not acknowledged.
#[derive(Clone, Copy)]
enum AppendCheckpoint {
    BeforeWrite,
    Written,
}

#[cfg(feature = "test-support")]
fn append_checkpoint(at: AppendCheckpoint) -> io::Result<()> {
    use crate::native::atomic::test_support::{self, Checkpoint, CheckpointEvent};
    test_support::checkpoint(&CheckpointEvent {
        checkpoint: match at {
            AppendCheckpoint::BeforeWrite => Checkpoint::BeforePublish,
            AppendCheckpoint::Written => Checkpoint::AfterPublish,
        },
        stage: None,
    })
}

#[cfg(not(feature = "test-support"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the test-support build injects failures through the same signature"
)]
const fn append_checkpoint(_at: AppendCheckpoint) -> io::Result<()> {
    Ok(())
}

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
