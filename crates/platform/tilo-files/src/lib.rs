//! Files inside capability-scoped roots, through one API on Linux, macOS and the web. A
//! [`Dir`] is opened from a [`tilo_paths::Location`] and names nothing outside it:
//! every path below it is a [`VPath`], relative and never `..`. Operations are `async`
//! because the web's storage is; [`blocking`] offers the same synchronously.
//! [`Dir::write_atomic`] leaves either the old or the new contents after a crash, and
//! [`Dir::create_new`] and [`Dir::rename_noreplace`] never replace an existing name.
//! Trash, owner-only directories and extended attributes are unsupported ([`Feature`]),
//! and reads and writes take whole buffers.

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
mod error;
mod local;
mod memory;
pub mod native;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod opfs;
mod watch;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod web;

pub use caps::{Availability, Feature, Unsupported, availability, available};
pub use error::FsError;
pub use native::lock::LockMode;
pub use tilo_paths::{InvalidVPath, Location, VPath, VirtualRoot};
pub use watch::{Change, ChangeKind, Next, Watch};

use local::Local;

/// A directory tree an app may use, and nothing outside it. Cloning is cheap and the clone names the
/// same tree.
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

/// A symbolic link reports its target's kind; [`Meta::symlink`] says whether the entry itself is a
/// link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    File,
    Dir,
    /// Devices, sockets, dangling links and anything else that is neither.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Bytes in a file; zero for a directory on backends without a size.
    pub len: u64,
    /// The Unix epoch where a backend keeps none (directories on the web).
    pub modified: tilo_clock::time::SystemTime,
    pub kind: Kind,
    pub mode: Option<u32>,
    pub symlink: bool,
}

#[must_use = "the lock is released when it is dropped"]
pub struct Lock {
    mode: LockMode,
    _held: Held,
}

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
    /// The tree at `location`; nothing is created until something is written. [`FsError::Unsupported`]
    /// for a location this target has no backend for.
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

    #[must_use]
    pub fn memory() -> Self {
        Self {
            root: Root::Local(Local::Memory(memory::Scope::fresh())),
        }
    }

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

    /// Replaces the contents so that a crash leaves either the old ones or the new ones, creating
    /// missing parents. Natively a failing final directory `fsync` happens after the new contents are
    /// visible.
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

    /// Saves a document as [`write_atomic`](Self::write_atomic) does, but natively through an existing
    /// symbolic link and keeping its permission bits, so editing a file changes only its contents. A
    /// dangling link or a file with several names is refused rather than detached, and a chosen folder
    /// that has gone is an error, never recreated.
    pub async fn save_document(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.save_document(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(_) => self.write_atomic(path, bytes).await,
        }
    }

    /// [`write_atomic`](Self::write_atomic), readable only by this user: natively the file is `0600`
    /// and its parent directory `0700`. Browser and memory storage are private already.
    pub async fn write_private(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        below_root(path)?;
        match &self.root {
            Root::Local(local) => local.write_private(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(_) => self.write_atomic(path, bytes).await,
        }
    }

    /// Publishes `bytes` only while nothing is there, creating missing parents; `false`, and nothing
    /// changed, when the name is taken.
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

    /// Moves `from` to `to`, failing rather than replacing anything at `to`, and creating missing
    /// parents of `to`. [`FsError::Unsupported`] where a backend cannot move this kind of entry:
    /// directories in browsers without `FileSystemHandle.move`, and natively on file systems without
    /// no-replace renames.
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

    /// Sorted by name; natively, names that are not UTF-8 cannot be [`VPath`] components and are left
    /// out.
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

    /// Waits for a lock on the lock file at `path`, created empty with its parents. Lock a dedicated
    /// file, not one replaced with [`write_atomic`](Self::write_atomic): natively the replacement is a
    /// new file and the lock stays on the old one. Natively the wait blocks the polling thread until
    /// `tilo_rt::io` carries file work; [`blocking::try_lock`] does not wait.
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

    /// `path` is a file, or a directory and everything below it; the watch reports paths relative to
    /// this `Dir`.
    pub fn watch(&self, path: &VPath) -> Result<Watch, FsError> {
        match &self.root {
            Root::Local(Local::Memory(scope)) => Ok(Watch::new(scope.watch(path), Box::new(()))),
            Root::Local(Local::Disk(root)) => disk_watch(root, path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Local(Local::Mirror(_)) => Err(Unsupported::of(Feature::Watch).into()),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(scope) => web::watch(scope, path),
        }
    }

    /// Not in this version.
    #[allow(
        clippy::unused_async,
        reason = "the signature is the API; backends arrive later"
    )]
    pub async fn trash(&self, path: &VPath) -> Result<(), FsError> {
        below_root(path)?;
        Err(Unsupported::of(Feature::Trash).into())
    }

    /// The operating-system path, for code that must hand one to a native API; portable code uses the
    /// operations above.
    pub fn native_path(&self, path: &VPath) -> Result<PathBuf, Unsupported> {
        match &self.root {
            Root::Local(Local::Disk(root)) => Ok(disk::resolve(root, path)),
            Root::Local(Local::Memory(_)) => Err(Unsupported {
                feature: Feature::NativePaths,
                reason: caps::MEMORY_NATIVE_PATHS,
            }),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Local(Local::Mirror(_)) => Err(Unsupported::of(Feature::NativePaths)),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Root::Opfs(_) => Err(Unsupported::of(Feature::NativePaths)),
        }
    }

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

/// The directory holding the native file at `path`, and the file's name in it: how a file a chooser, a
/// drop or the command line named is reached through [`Dir`]'s operations.
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

/// Absolute with every symbolic link resolved, as the native file system has it now: a stable identity
/// for a file a chooser named.
pub fn canonicalize(path: &std::path::Path) -> Result<PathBuf, FsError> {
    if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
        return Err(Unsupported::of(Feature::NativePaths).into());
    }
    Ok(std::fs::canonicalize(path)?)
}

/// A native path as a string that stays the same however the platform spells it: on Windows the `file:`
/// URL, which normalizes the verbatim `\\?\` prefix canonicalization adds once a file exists, elsewhere
/// the path itself. For keys derived from a document's location.
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
        return Err(Unsupported::of(Feature::NativePaths).into());
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
    native::change_watch::stream(root, path, true)
}

#[cfg(not(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
)))]
fn disk_watch(_root: &std::path::Path, _path: &VPath) -> Result<Watch, FsError> {
    Err(FsError::Unsupported {
        feature: Feature::Watch,
        reason: caps::NO_DISK_WATCH,
    })
}
