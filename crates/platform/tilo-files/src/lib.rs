//! Files inside capability-scoped roots, through one API on Linux, macOS and
//! the web.
//!
//! A [`Dir`] is opened from a [`tilo_paths::Location`] (an app's data,
//! config, cache or state location, a picker grant) or made in memory for
//! tests with [`Dir::memory`]. Every path below it is a [`VPath`]: relative,
//! `/`-separated, never `..`, so a `Dir` cannot name anything outside itself
//! except through symbolic links a native user created.
//!
//! # Backends
//!
//! | Location | Backend |
//! |---|---|
//! | `Location::Native` on Linux and macOS | `std::fs`, with [`native::atomic`] for crash-safe replacement and [`native::lock`] (`flock`) for locks |
//! | `VirtualRoot::Memory` on every target | a map in this process, shared by every `Dir` on the same location; [`Dir::memory`] makes a private one |
//! | `VirtualRoot::Opfs` on the web | the browser's origin-private file system, one directory per profile, and Web Locks; app `data`, `config` and `state` through the page's mirror once the host loaded it ([`web`]) |
//!
//! A location a target has no backend for (`Native` on the web, `Opfs`
//! natively) is an [`FsError::Unsupported`] from [`Dir::open`].
//!
//! # Async first
//!
//! The browser's storage answers only asynchronously on the page's thread, so
//! every operation is an `async fn`. On Linux and macOS these futures run the
//! `std::fs` call to completion when first polled; they are `Send` there and
//! not on the web, where they hold browser handles. [`blocking`] offers the
//! same operations synchronously; on the web it answers from the page's
//! mirror of the app's locations, and otherwise with
//! [`FsError::Unsupported`].
//!
//! # Watching
//!
//! [`Dir::watch`] reports changes: from memory trees, from OPFS (this tab's
//! and, through a `BroadcastChannel`, other tabs' `tilo-files` writers), and
//! natively from inotify or `FSEvents` with the `watch` feature.
//!
//! # What a write guarantees
//!
//! - [`Dir::write_atomic`] leaves either the old or the new contents after a
//!   crash. Natively: temp file, `fsync` (`F_FULLFSYNC` on macOS), rename,
//!   directory `fsync`. On the web: an OPFS writable stream, whose swap file
//!   the browser commits on close.
//! - [`Dir::create_new`] and [`Dir::rename_noreplace`] never replace an
//!   existing name. Natively they use a hard link and `renameat2`/`renameatx_np`;
//!   on the web every writer of a profile holds one Web Lock while it creates
//!   or replaces names.
//! - Writes create missing parent directories, including the `Dir`'s root.
//!
//! # Not in this version
//!
//! - Trash, owner-only directories and extended attributes report
//!   [`Availability::Unsupported`] (see [`CAPABILITIES`]); [`Dir::trash`]
//!   returns the error. The absorbed native modules still offer
//!   owner-checked private files by path ([`native::private`]).
//! - Reads and writes take whole buffers; there is no `open_read` stream.
//!   `tilo-content`'s `DirBackend` streams and hashes while receiving behind
//!   a synchronous trait, so it stays on `std::fs` until this crate streams
//!   and the content trait can await (or the web backend moves into a
//!   worker with synchronous access handles).
//! - Native operations run on the polling thread rather than `tilo_rt::io`.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]
#![cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(
        clippy::unused_async,
        reason = "native and memory storage answer at once; only the web backend awaits"
    )
)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "browser storage handles live on the page's one thread"
    )
)]

use std::fmt;
use std::path::PathBuf;

pub mod blocking;
mod caps;
mod disk;
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
mod disk_watch;
mod error;
mod local;
mod memory;
pub mod native;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod opfs;
mod watch;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod web;

pub use caps::{Availability, CAPABILITIES, Feature, Unsupported, availability};
pub use error::FsError;
pub use native::lock::LockMode;
pub use tilo_paths::{InvalidVPath, Location, VPath, VirtualRoot};
pub use watch::{Change, ChangeKind, Next, Watch};

use local::Local;

/// A directory tree an app may use, and nothing outside it.
///
/// Cloning is cheap and the clone names the same tree.
#[derive(Clone)]
pub struct Dir {
    root: Root,
}

#[derive(Clone)]
enum Root {
    Local(Local),
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    Opfs(opfs::Scope),
}

/// What kind of entry a path names. A symbolic link reports its target's
/// kind; [`Meta::symlink`] says whether the entry itself is a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    File,
    Dir,
    /// Devices, sockets, dangling links and anything else that is neither.
    Other,
}

/// One entry of [`Dir::list`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

/// What [`Dir::metadata`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Bytes in a file; zero for a directory on backends without a size.
    pub len: u64,
    /// The last modification; the Unix epoch where a backend keeps none
    /// (directories on the web).
    pub modified: tilo_clock::SystemTime,
    pub kind: Kind,
    /// Unix permission bits, where the backend has them.
    pub mode: Option<u32>,
    /// Whether the entry itself is a symbolic link.
    pub symlink: bool,
}

/// A held lock from [`Dir::lock`], released when dropped.
#[must_use = "the lock is released when it is dropped"]
pub struct Lock {
    mode: LockMode,
    _held: Held,
}

/// Each guard is held only for its release on drop.
enum Held {
    Disk {
        _lock: native::lock::FileLock,
    },
    Memory {
        _lock: memory::Held,
    },
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    Web {
        _lock: opfs::WebLock,
    },
}

impl Lock {
    pub fn mode(&self) -> LockMode {
        self.mode
    }
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lock")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

const BELOW_ROOT: &str = "the operation needs a path below the root";
const INTO_ITSELF: &str = "a directory cannot move into itself";
const UNKNOWN_ROOT: &str = "This build does not know the storage root.";

impl Dir {
    /// The tree at `location`. Nothing is created until something is
    /// written.
    ///
    /// # Errors
    ///
    /// [`FsError::Unsupported`] for a location this target has no backend
    /// for, and [`FsError::InvalidPath`] for an OPFS profile that is not one
    /// path component.
    pub fn open(location: Location) -> Result<Self, FsError> {
        let root = match location {
            Location::Native(path) => native_root(path)?,
            Location::Virtual(VirtualRoot::Memory, prefix) => {
                Root::Local(Local::Memory(memory::Scope::global(prefix)))
            }
            Location::Virtual(VirtualRoot::Opfs { profile }, prefix) => opfs_root(profile, prefix)?,
            Location::Virtual(..) => {
                return Err(FsError::Unsupported {
                    feature: Feature::Files,
                    reason: UNKNOWN_ROOT,
                });
            }
        };
        Ok(Self { root })
    }

    /// An empty tree in memory that only this `Dir` and its clones see.
    #[must_use]
    pub fn memory() -> Self {
        Self {
            root: Root::Local(Local::Memory(memory::Scope::fresh())),
        }
    }

    /// The whole file.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`], [`FsError::IsADirectory`], or the backend's
    /// failure.
    pub async fn read(&self, path: &VPath) -> Result<Vec<u8>, FsError> {
        match &self.root {
            Root::Local(local) => local.read(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => match web::view(scope) {
                Some(view) => view.read(path),
                None => scope.read(path).await,
            },
        }
    }

    /// Read this address from committed OPFS, bypassing the page mirror.
    /// Holds the existing profile writer lock and bounds allocation before
    /// reading. It neither clears failure indicators nor trusts queue counts.
    /// This is a current readback, not a promise about subsequent writes.
    ///
    /// # Errors
    /// Refuses non-OPFS storage, absent content, oversized files and read failures.
    pub async fn read_committed(&self, path: &VPath, max_bytes: usize) -> Result<Vec<u8>, FsError> {
        below_root(path)?;
        match &self.root {
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => scope.read_committed(path, max_bytes).await,
            Root::Local(_) => {
                let _ = max_bytes;
                Err(FsError::Unsupported {
                    feature: Feature::Files,
                    reason: "Committed OPFS readback requires browser persistent storage.",
                })
            }
        }
    }

    /// Replaces the file's contents with `bytes` so that a crash leaves
    /// either the old contents or the new ones, creating missing parents.
    ///
    /// # Errors
    ///
    /// [`FsError::IsADirectory`], [`FsError::NotADirectory`] for a file in
    /// the way of a parent, [`FsError::InvalidPath`] for the root, or the
    /// backend's failure. Natively a failure of the final directory `fsync`
    /// happens after the new contents are visible.
    pub async fn write_atomic(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.write_atomic(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => {
                if let Some(view) = web::view(scope) {
                    view.write_atomic_committed(path, bytes).await
                } else {
                    scope.write_atomic(path, bytes).await?;
                    web::committed_directly(scope, &web::Direct::Write(path, bytes));
                    Ok(())
                }
            }
        }
    }

    /// Saves a user's document as [`write_atomic`](Self::write_atomic) does,
    /// but natively through an existing symbolic link and keeping the
    /// file's permission bits, so saving an edited file changes only its
    /// contents. Dangling links and files with several names are refused
    /// rather than detached. Natively the folder must exist: a chosen folder
    /// that has gone (an unmounted drive) is an error, never recreated.
    ///
    /// # Errors
    ///
    /// As [`write_atomic`](Self::write_atomic); natively [`FsError::NotFound`]
    /// for a missing folder and an unwritable or unsupported existing target.
    pub async fn save_document(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.save_document(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(_) => self.write_atomic(path, bytes).await,
        }
    }

    /// [`write_atomic`](Self::write_atomic), readable only by this user:
    /// natively the file is `0600` and its parent directory `0700`. Browser
    /// and memory storage are private to the page or process already.
    ///
    /// # Errors
    ///
    /// As [`write_atomic`](Self::write_atomic).
    pub async fn write_private(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.write_private(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(_) => self.write_atomic(path, bytes).await,
        }
    }

    /// Publishes `bytes` at `path` only while nothing is there, creating
    /// missing parents. Returns `false`, and changes nothing, when the name
    /// is taken.
    ///
    /// # Errors
    ///
    /// [`FsError::InvalidPath`] for the root, or the backend's failure.
    pub async fn create_new(&self, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.create_new(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => {
                if let Some(view) = web::view(scope) {
                    view.create_new_committed(path, bytes).await
                } else {
                    let created = scope.create_new(path, bytes).await?;
                    if created {
                        web::committed_directly(scope, &web::Direct::Write(path, bytes));
                    }
                    Ok(created)
                }
            }
        }
    }

    /// Moves the file or directory at `from` to `to`, failing rather than
    /// replacing anything at `to`. Missing parents of `to` are created.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] for `from`, [`FsError::AlreadyExists`] for `to`,
    /// [`FsError::InvalidPath`] for the root or a move into itself, and
    /// [`FsError::Unsupported`] where a backend cannot move this kind of
    /// entry (directories in browsers without `FileSystemHandle.move`, and
    /// natively on file systems without no-replace renames).
    pub async fn rename_noreplace(&self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        check_rename(from, to)?;
        match &self.root {
            Root::Local(local) => local.rename_noreplace(from, to),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => {
                if let Some(view) = web::view(scope) {
                    view.rename_committed(from, to).await
                } else {
                    scope.rename_noreplace(from, to).await?;
                    web::committed_directly(scope, &web::Direct::Rename(from, to));
                    Ok(())
                }
            }
        }
    }

    /// Removes a file or an empty directory.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`], [`FsError::DirectoryNotEmpty`],
    /// [`FsError::InvalidPath`] for the root, or the backend's failure.
    pub async fn remove(&self, path: &VPath) -> Result<(), FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.remove(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => {
                if let Some(view) = web::view(scope) {
                    view.remove_committed(path).await
                } else {
                    scope.remove(path).await?;
                    web::committed_directly(scope, &web::Direct::Remove(path));
                    Ok(())
                }
            }
        }
    }

    /// Creates the directory and any missing parents; an existing directory
    /// is success.
    ///
    /// # Errors
    ///
    /// A file in the way, or the backend's failure.
    pub async fn create_dir_all(&self, path: &VPath) -> Result<(), FsError> {
        match &self.root {
            Root::Local(local) => local.create_dir_all(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => {
                if let Some(view) = web::view(scope) {
                    view.create_dir_committed(path).await
                } else {
                    scope.create_dir_all(path).await?;
                    web::committed_directly(scope, &web::Direct::CreateDir(path));
                    Ok(())
                }
            }
        }
    }

    /// The directory's entries, sorted by name. Natively, names that are not
    /// UTF-8 cannot be [`VPath`] components and are left out.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] (including a root nothing was written to yet),
    /// [`FsError::NotADirectory`], or the backend's failure.
    pub async fn list(&self, path: &VPath) -> Result<Vec<Entry>, FsError> {
        match &self.root {
            Root::Local(local) => local.list(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => match web::view(scope) {
                Some(view) => view.list(path),
                None => scope.list(path).await,
            },
        }
    }

    /// Size, modification time and kind of the entry.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`], or the backend's failure.
    pub async fn metadata(&self, path: &VPath) -> Result<Meta, FsError> {
        match &self.root {
            Root::Local(local) => local.metadata(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => match web::view(scope) {
                Some(view) => view.metadata(path),
                None => scope.metadata(path).await,
            },
        }
    }

    /// Waits for a lock on the lock file at `path`, created empty (with its
    /// parents) when missing. Other processes, and on the web other tabs of
    /// the origin, that lock the same path through `tilo-files` exclude one
    /// another as `mode` says.
    ///
    /// Lock a dedicated file, not one you replace with
    /// [`write_atomic`](Self::write_atomic): natively a replacement is a new
    /// file, and the lock stays on the old one.
    ///
    /// Natively the wait blocks the polling thread until `tilo_rt::io`
    /// carries file work; use [`blocking::try_lock`] to avoid waiting.
    ///
    /// # Errors
    ///
    /// [`FsError::InvalidPath`] for the root, [`FsError::Unsupported`] in a
    /// browser without Web Locks, or the backend's failure.
    pub async fn lock(&self, path: &VPath, mode: LockMode) -> Result<Lock, FsError> {
        below_root(path)?;
        let held = match &self.root {
            Root::Local(local) => local.lock(path, mode).await?,
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => Held::Web {
                _lock: scope.lock(path, mode).await?,
            },
        };
        Ok(Lock { mode, _held: held })
    }

    /// Watches `path` (a file, or a directory and everything below it) for
    /// changes. The watch reports paths relative to this `Dir`.
    ///
    /// # Errors
    ///
    /// [`FsError::Unsupported`] with [`Feature::Watch`] for native
    /// directories in a build without the `watch` feature; natively,
    /// [`FsError::NotFound`] when `path` does not exist yet.
    pub fn watch(&self, path: &VPath) -> Result<Watch, FsError> {
        match &self.root {
            Root::Local(Local::Memory(scope)) => Ok(Watch::new(scope.watch(path), Box::new(()))),
            Root::Local(Local::Disk(root)) => disk_watch(root, path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Local(Local::Mirror(_)) => Err(caps::unsupported(Feature::Watch).into()),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => web::watch(scope, path),
        }
    }

    /// Moves the entry to the user's trash. Not in this version.
    ///
    /// # Errors
    ///
    /// [`FsError::Unsupported`] with [`Feature::Trash`] on every target.
    #[allow(
        clippy::unused_self,
        clippy::unused_async,
        reason = "the signature is the API; backends arrive later"
    )]
    pub async fn trash(&self, path: &VPath) -> Result<(), FsError> {
        below_root(path)?;
        Err(caps::unsupported(Feature::Trash).into())
    }

    /// The operating-system path of `path`, for code that must hand one to a
    /// native API. Portable code uses the operations above.
    ///
    /// # Errors
    ///
    /// [`Unsupported`] for browser and in-memory storage.
    pub fn native_path(&self, path: &VPath) -> Result<PathBuf, Unsupported> {
        match &self.root {
            Root::Local(Local::Disk(root)) => Ok(disk::resolve(root, path)),
            Root::Local(Local::Memory(_)) => Err(Unsupported {
                feature: Feature::NativePaths,
                reason: caps::MEMORY_NATIVE_PATHS,
            }),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Local(Local::Mirror(_)) => Err(caps::unsupported(Feature::NativePaths)),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(_) => Err(caps::unsupported(Feature::NativePaths)),
        }
    }

    /// The synchronous backend: native and memory trees, and on the web
    /// OPFS locations the page mirrors.
    #[cfg_attr(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        allow(
            clippy::unnecessary_wraps,
            reason = "only the web has locations without a synchronous backend"
        )
    )]
    fn local(&self) -> Result<Local, FsError> {
        match &self.root {
            Root::Local(local) => Ok(local.clone()),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => web::view(scope)
                .map(Local::Mirror)
                .ok_or(FsError::Unsupported {
                    feature: Feature::Blocking,
                    reason: caps::WEB_BLOCKING,
                }),
        }
    }
}

impl fmt::Debug for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.root {
            Root::Local(Local::Disk(root)) => f.debug_tuple("Dir").field(root).finish(),
            Root::Local(Local::Memory(scope)) => {
                f.debug_tuple("Dir::Memory").field(&scope.prefix()).finish()
            }
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Local(Local::Mirror(_)) => f.debug_tuple("Dir::Mirror").finish(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => f.debug_tuple("Dir::Opfs").field(scope).finish(),
        }
    }
}

/// The directory holding the native file at `path`, and the file's name in
/// it: how a file a native chooser, a drop or the command line named is
/// reached through [`Dir`]'s operations (usually [`blocking`]).
///
/// # Errors
///
/// [`FsError::InvalidPath`] for a path with no file name, or one that is not
/// UTF-8; [`FsError::Unsupported`] on the web, where no file has a native
/// path.
pub fn file_at(path: &std::path::Path) -> Result<(Dir, VPath), FsError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| VPath::new(name).ok())
        .ok_or(FsError::InvalidPath("the path names no file"))?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((Dir::open(Location::Native(parent))?, name))
}

/// `path` made absolute with every symbolic link resolved, as the native file
/// system has it now: a stable identity for a file a chooser named.
///
/// # Errors
///
/// [`FsError::NotFound`] when something on the path does not exist, and
/// [`FsError::Unsupported`] on the web.
pub fn canonicalize(path: &std::path::Path) -> Result<PathBuf, FsError> {
    if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
        return Err(caps::unsupported(Feature::NativePaths).into());
    }
    Ok(std::fs::canonicalize(path)?)
}

/// A native path as a string that stays the same however the platform spells
/// it: on Windows the `file:` URL, which normalizes the verbatim `\\?\` prefix
/// canonicalization adds once a file exists; elsewhere the path itself. For
/// keys derived from a document's location (a recovery slot's name).
#[must_use]
pub fn path_identity(path: &std::path::Path) -> String {
    #[cfg(windows)]
    {
        url::Url::from_file_path(path).map_or_else(
            |()| path.to_string_lossy().into_owned(),
            |url| url.to_string(),
        )
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

fn below_root(path: &VPath) -> Result<(), FsError> {
    if path.is_root() {
        Err(FsError::InvalidPath(BELOW_ROOT))
    } else {
        Ok(())
    }
}

fn check_rename(from: &VPath, to: &VPath) -> Result<(), FsError> {
    below_root(from)?;
    below_root(to)?;
    let inside = to
        .as_str()
        .strip_prefix(from.as_str())
        .is_some_and(|rest| rest.starts_with('/'));
    if inside {
        Err(FsError::InvalidPath(INTO_ITSELF))
    } else {
        Ok(())
    }
}

fn native_root(path: PathBuf) -> Result<Root, FsError> {
    if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
        return Err(caps::unsupported(Feature::NativePaths).into());
    }
    Ok(Root::Local(Local::Disk(path)))
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn opfs_root(profile: String, prefix: VPath) -> Result<Root, FsError> {
    opfs::Scope::new(profile, prefix).map(Root::Opfs)
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn opfs_root(_profile: String, _prefix: VPath) -> Result<Root, FsError> {
    Err(FsError::Unsupported {
        feature: Feature::Files,
        reason: "The origin-private file system exists only in browsers.",
    })
}

#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
fn disk_watch(root: &std::path::Path, path: &VPath) -> Result<Watch, FsError> {
    disk_watch::watch(root, path)
}

#[cfg(not(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
)))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature of the backend a `watch` build has"
)]
fn disk_watch(_root: &std::path::Path, _path: &VPath) -> Result<Watch, FsError> {
    Err(FsError::Unsupported {
        feature: Feature::Watch,
        reason: caps::NO_DISK_WATCH,
    })
}
