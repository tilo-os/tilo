//! Existing caller-private capture request/rendezvous, shared with the Shell.
//! It conveys no identity, source or filesystem authority. QPE admission is
//! required before consuming a request or transferring capabilities.
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    sync::Arc,
};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments and returns the process's real UID.
    unsafe { libc::getuid() }
}

/// Each caller supplies its existing held directory ancestry, preserving its
/// original admission/validation rather than deriving custody from a pathname.
pub trait DirectoryPin: Send + Sync {
    fn file(&self) -> &File;
    /// Open a readable handle to the held directory.
    ///
    /// # Errors
    /// Refuses unavailable directory evidence or failure to open a readable handle.
    fn readable_file(&self) -> io::Result<File>;
    /// Revalidate the caller's held directory ancestry.
    ///
    /// # Errors
    /// Refuses changed or unavailable held namespace evidence.
    fn validate(&self) -> io::Result<()>;
    /// Revalidate the held destination before publishing caller-private files.
    ///
    /// # Errors
    /// Refuses an inadmissible destination or changed held namespace evidence.
    fn validate_destination(&self) -> io::Result<()>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
impl NativePath {
    fn encode(path: &Path) -> Self {
        Self::Unix(path.as_os_str().as_bytes().to_vec())
    }
    fn decode(&self) -> io::Result<std::path::PathBuf> {
        match self {
            Self::Unix(bytes) if bytes.len() <= 4096 && !bytes.contains(&0) => {
                Ok(std::ffi::OsString::from_vec(bytes.clone()).into())
            }
            _ => Err(refused()),
        }
    }
}

/// Exact existing request wire representation; `caller_pid` is only a claim
/// until the receiving root helper compares it with the actual QPE sender.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rendezvous {
    path: NativePath,
    caller_pid: u32,
}
pub struct Listener {
    socket: UnixListener,
    parent: Arc<dyn DirectoryPin>,
}
impl Rendezvous {
    /// # Errors
    /// Refuses a non-private caller directory or a changed held namespace.
    pub fn create(directory: &Path, parent: Arc<dyn DirectoryPin>) -> io::Result<(Self, Listener)> {
        parent.validate_destination()?;
        let facts = parent.file().metadata()?;
        if facts.uid() != current_uid() || facts.mode() & 0o077 != 0 {
            return Err(refused());
        }
        let address = format!("/proc/self/fd/{}/capture.sock", parent.file().as_raw_fd());
        let socket = UnixListener::bind(&address)?;
        std::fs::set_permissions(address, std::fs::Permissions::from_mode(0o600))?;
        parent.validate()?;
        Ok((
            Self {
                path: NativePath::encode(&directory.join("capture.sock")),
                caller_pid: std::process::id(),
            },
            Listener { socket, parent },
        ))
    }
    pub fn caller_pid(&self) -> u32 {
        self.caller_pid
    }
    /// # Errors
    /// Refuses foreign path, non-private socket or changed pinned directory.
    pub fn connect(
        &self,
        directory: &Path,
        parent: &dyn DirectoryPin,
        caller: u32,
    ) -> io::Result<UnixStream> {
        let path = self.path.decode()?;
        if path.parent() != Some(directory)
            || path.file_name() != Some(std::ffi::OsStr::new("capture.sock"))
            || self.caller_pid <= 1
        {
            return Err(refused());
        }
        parent.validate()?;
        let facts = parent.file().metadata()?;
        if facts.uid() != caller || facts.mode() & 0o077 != 0 {
            return Err(refused());
        }
        let address = format!("/proc/self/fd/{}/capture.sock", parent.file().as_raw_fd());
        let facts = std::fs::symlink_metadata(&address)?;
        if !facts.file_type().is_socket() || facts.uid() != caller || facts.mode() & 0o077 != 0 {
            return Err(refused());
        }
        let stream = UnixStream::connect(address)?;
        parent.validate()?;
        Ok(stream)
    }
}
impl Listener {
    pub fn socket(&self) -> &UnixListener {
        &self.socket
    }
    /// Revalidate the listener's held directory ancestry.
    ///
    /// # Errors
    /// Refuses changed or unavailable held namespace evidence.
    pub fn validate(&self) -> io::Result<()> {
        self.parent.validate()
    }
}

/// # Errors
/// Uses the original atomic, owner-only, no-clobber publication. Refuses an
/// oversized request or changed private parent; existing requests stay intact.
pub fn publish_request_bytes(
    parent: &dyn DirectoryPin,
    bytes: &[u8],
    maximum: usize,
) -> io::Result<File> {
    use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
    parent.validate_destination()?;
    let held = StorageDirectory::from_file(parent.readable_file()?)?;
    held.validate_privacy(Privacy::OwnerOnly)?;
    if bytes.len() > maximum {
        return Err(io::ErrorKind::InvalidData.into());
    }
    held.publish(
        std::ffi::OsStr::new("request.json"),
        bytes,
        PublishMode::IfAbsent,
        Privacy::OwnerOnly,
    )
    .map_err(io::Error::other)?;
    parent.validate()?;
    held.open_read(std::ffi::OsStr::new("request.json"), Privacy::OwnerOnly)
}

/// Root uses its actual existing pinned parent and observed filesystem owner;
/// the decoded request remains untrusted until its executing Shell is admitted.
/// # Errors
/// Refuses a non-private, foreign, non-regular or oversized request.
pub fn read_request_bytes(
    parent: &dyn DirectoryPin,
    caller: u32,
    maximum: usize,
) -> io::Result<Vec<u8>> {
    use tilo_files::native::atomic::{Privacy, StorageDirectory};
    parent.validate()?;
    let facts = parent.file().metadata()?;
    if facts.uid() != caller || facts.mode() & 0o077 != 0 {
        return Err(refused());
    }
    let held = StorageDirectory::from_file(parent.readable_file()?)?;
    let file = held.open_read(std::ffi::OsStr::new("request.json"), Privacy::Inherit)?;
    let facts = file.metadata()?;
    let bound = u64::try_from(maximum).map_err(io::Error::other)?;
    if !facts.is_file() || facts.uid() != caller || facts.mode() & 0o077 != 0 || facts.len() > bound
    {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    file.take(bound.checked_add(1).ok_or_else(refused)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(refused());
    }
    parent.validate()?;
    Ok(bytes)
}
