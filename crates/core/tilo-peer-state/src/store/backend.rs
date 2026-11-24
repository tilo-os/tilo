//! What a [`Store`](super::Store) keeps its files on.
//!
//! A backend moves bytes and nothing else: every check of what the bytes say,
//! the index, coverage and retirement stay in the store above it. Paths are
//! relative to the backend's own root, `/`-separated, and name files the
//! store's layout describes ([`super`]).
//!
//! Durability is the backend's promise and only its own, kept one of two ways.
//! A backend that can wait makes each change durable before it answers
//! ([`FsBackend`]). A backend that cannot wait (a page's, whose database
//! answers later) commits every change in the order it was made, all that
//! were made before a commit at once, and answers each change at once: what
//! it changed is durable with the next commit, which [`Backend::durable`]
//! waits for. Either way nothing is acknowledged before it is durable:
//! [`Appender::sync`] fails until the bytes it is asked for are durable,
//! never answering for a sync it has not done, and a commit that holds those
//! bytes holds every change made before them.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use collab_types::signer::MaybeSendSync;
use tilo_lock::MutexRecover as _;

/// [`Backend::durable`]: resolves once every change made before it is durable.
#[cfg(not(target_family = "wasm"))]
pub type Durable = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;
/// [`Backend::durable`]: resolves once every change made before it is durable.
/// On the web it waits on the page's own promises, bound to its one thread.
#[cfg(target_family = "wasm")]
pub type Durable = Pin<Box<dyn Future<Output = io::Result<()>>>>;

/// The files a store keeps.
pub trait Backend: MaybeSendSync + std::fmt::Debug {
    /// The whole of the file at `path`; an error of kind `NotFound` when there is none.
    ///
    /// # Errors
    ///
    /// The file cannot be read.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// The file at `path`, open to read parts of it.
    ///
    /// # Errors
    ///
    /// The file cannot be opened.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadAt>>;

    /// What `dir` holds directly, each entry as `dir` joined with its name and
    /// whether it is a directory; nothing when `dir` does not exist.
    ///
    /// # Errors
    ///
    /// The directory cannot be read.
    fn list(&self, dir: &Path) -> io::Result<Vec<(PathBuf, bool)>>;

    /// Whether `path` is a directory.
    fn is_dir(&self, path: &Path) -> bool;

    /// Whether `path` is a file.
    fn is_file(&self, path: &Path) -> bool;

    /// Make `dir` and every directory above it.
    ///
    /// # Errors
    ///
    /// A directory cannot be made.
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;

    /// The file at `path` open for appending, made when it does not exist.
    ///
    /// # Errors
    ///
    /// The file cannot be opened.
    fn open_append(&self, path: &Path) -> io::Result<Box<dyn Appender>>;

    /// Write `bytes` as the file `path`, durable before this returns or with
    /// the backend's next commit.
    ///
    /// # Errors
    ///
    /// The file cannot be written or synced.
    fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// Replace the file at `path` with `bytes`, making the directories above
    /// it: a crash leaves the old bytes or the new, never part of either. The
    /// new are durable before this returns or with the backend's next commit.
    ///
    /// # Errors
    ///
    /// The file cannot be written.
    fn replace(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// Cut the file at `path` to `len` bytes, durable before this returns or
    /// with the backend's next commit.
    ///
    /// # Errors
    ///
    /// The file cannot be cut or synced.
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()>;

    /// Rename `from` to `to`, a file or a directory.
    ///
    /// # Errors
    ///
    /// The rename failed.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Remove the file at `path`.
    ///
    /// # Errors
    ///
    /// The file cannot be removed; of kind `NotFound` when there is none.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Remove `dir` and everything in it.
    ///
    /// # Errors
    ///
    /// It cannot be removed; of kind `NotFound` when there is none.
    fn remove_all(&self, dir: &Path) -> io::Result<()>;

    /// Make what `dir` holds (files made, renamed or removed in it) durable
    /// before any sync asked after this answers: durable before this returns,
    /// or with the next commit of a backend that commits in order.
    ///
    /// # Errors
    ///
    /// The directory cannot be synced.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;

    /// Resolves once every change made before it is durable: at once on a
    /// backend that makes each change durable before it answers, after its
    /// commit on one that commits later.
    fn durable(&self) -> Durable {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A file open for appending: what is appended is durable once [`Self::sync`]
/// returned for it.
pub trait Appender: MaybeSendSync {
    /// Append `bytes` at the end of the file.
    ///
    /// # Errors
    ///
    /// They were not all written.
    fn append(&self, bytes: &[u8]) -> io::Result<()>;

    /// Cut the file to `len` bytes: what part of a failed append reached it.
    ///
    /// # Errors
    ///
    /// The file cannot be cut.
    fn truncate(&self, len: u64) -> io::Result<()>;

    /// Wait until the file's first `through` bytes are durable; a backend may
    /// make more of it durable.
    ///
    /// # Errors
    ///
    /// They are not: of kind `WouldBlock` on a backend whose commit holding
    /// them has not completed yet.
    fn sync(&self, through: u64) -> io::Result<()>;
}

/// A file open for reading parts of it.
pub trait ReadAt {
    /// Fill `bytes` from `at` in the file.
    ///
    /// # Errors
    ///
    /// The file does not hold them.
    fn read_exact_at(&mut self, at: u64, bytes: &mut [u8]) -> io::Result<()>;
}

/// Every file in memory, gone when the last handle to it is: for logic tests,
/// and for a backend that keeps its files elsewhere and their image here. A
/// directory is there while a file is in it.
#[derive(Clone, Debug, Default)]
pub struct MemoryBackend {
    files: Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>,
}

impl MemoryBackend {
    /// No files.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `files`, each by its path.
    #[must_use]
    pub fn holding(files: impl IntoIterator<Item = (PathBuf, Vec<u8>)>) -> Self {
        Self {
            files: Arc::new(Mutex::new(files.into_iter().collect())),
        }
    }

    /// The file at `path`, if there is one.
    #[must_use]
    pub fn file(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.lock_recover().get(path).cloned()
    }

    /// `read` of the file at `path` where it is, without copying it, if there is one.
    pub fn with_file<T>(&self, path: &Path, read: impl FnOnce(&[u8]) -> T) -> Option<T> {
        self.files.lock_recover().get(path).map(|bytes| read(bytes))
    }

    /// The paths of every file in `dir` and below it.
    #[must_use]
    pub fn files_under(&self, dir: &Path) -> Vec<PathBuf> {
        self.files
            .lock_recover()
            .keys()
            .filter(|path| path.starts_with(dir) && *path != dir)
            .cloned()
            .collect()
    }
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} does not exist", path.display()),
    )
}

impl Backend for MemoryBackend {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.file(path).ok_or_else(|| not_found(path))
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadAt>> {
        Ok(Box::new(io::Cursor::new(self.read(path)?)))
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
        let mut found = BTreeMap::new();
        for path in self.files_under(dir) {
            let Ok(rest) = path.strip_prefix(dir) else {
                continue;
            };
            let mut parts = rest.components();
            let Some(name) = parts.next() else {
                continue;
            };
            let deeper = parts.next().is_some();
            *found.entry(dir.join(name)).or_insert(false) |= deeper;
        }
        Ok(found.into_iter().collect())
    }

    fn is_dir(&self, path: &Path) -> bool {
        !self.files_under(path).is_empty()
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.lock_recover().contains_key(path)
    }

    fn create_dir_all(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn Appender>> {
        self.files
            .lock_recover()
            .entry(path.to_path_buf())
            .or_default();
        Ok(Box::new(MemoryAppender {
            files: Arc::clone(&self.files),
            path: path.to_path_buf(),
        }))
    }

    fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace(path, bytes)
    }

    fn replace(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.files
            .lock_recover()
            .insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.files
            .lock_recover()
            .get_mut(path)
            .ok_or_else(|| not_found(path))?
            .truncate(usize::try_from(len).unwrap_or(usize::MAX));
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut files = self.files.lock_recover();
        let moved: Vec<PathBuf> = files
            .keys()
            .filter(|path| path.starts_with(from))
            .cloned()
            .collect();
        if moved.is_empty() {
            return Err(not_found(from));
        }
        for path in moved {
            let Ok(rest) = path.strip_prefix(from) else {
                continue;
            };
            let target = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            if let Some(bytes) = files.remove(&path) {
                files.insert(target, bytes);
            }
        }
        drop(files);
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.files
            .lock_recover()
            .remove(path)
            .map(drop)
            .ok_or_else(|| not_found(path))
    }

    fn remove_all(&self, dir: &Path) -> io::Result<()> {
        let mut files = self.files.lock_recover();
        let before = files.len();
        files.retain(|path, _| !path.starts_with(dir) || path == dir);
        let removed = files.len() < before;
        drop(files);
        if removed { Ok(()) } else { Err(not_found(dir)) }
    }

    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }
}

struct MemoryAppender {
    files: Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>,
    path: PathBuf,
}

impl Appender for MemoryAppender {
    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        self.files
            .lock_recover()
            .entry(self.path.clone())
            .or_default()
            .extend_from_slice(bytes);
        Ok(())
    }

    fn truncate(&self, len: u64) -> io::Result<()> {
        if let Some(file) = self.files.lock_recover().get_mut(&self.path) {
            file.truncate(usize::try_from(len).unwrap_or(usize::MAX));
        }
        Ok(())
    }

    fn sync(&self, _through: u64) -> io::Result<()> {
        Ok(())
    }
}

impl ReadAt for io::Cursor<Vec<u8>> {
    fn read_exact_at(&mut self, at: u64, bytes: &mut [u8]) -> io::Result<()> {
        use io::{Read as _, Seek as _};
        self.seek(io::SeekFrom::Start(at))?;
        self.read_exact(bytes)
    }
}

/// The files of a directory on this device's disk: synced with `fsync`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct FsBackend {
    root: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
mod fs_backend {
    use super::{Appender, Backend, FsBackend, ReadAt};
    use std::fs;
    use std::io::{self, Read as _, Seek as _, Write as _};
    use std::path::{Path, PathBuf};

    impl FsBackend {
        /// The files under `root`.
        #[must_use]
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self { root: root.into() }
        }

        fn at(&self, path: &Path) -> PathBuf {
            self.root.join(path)
        }
    }

    impl Backend for FsBackend {
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            fs::read(self.at(path))
        }

        fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadAt>> {
            Ok(Box::new(fs::File::open(self.at(path))?))
        }

        fn list(&self, dir: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
            let mut found = Vec::new();
            let entries = match fs::read_dir(self.at(dir)) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
                Err(error) => return Err(error),
            };
            for entry in entries {
                let entry = entry?;
                found.push((dir.join(entry.file_name()), entry.path().is_dir()));
            }
            found.sort();
            Ok(found)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.at(path).is_dir()
        }

        fn is_file(&self, path: &Path) -> bool {
            self.at(path).is_file()
        }

        fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
            fs::create_dir_all(self.at(dir))
        }

        fn open_append(&self, path: &Path) -> io::Result<Box<dyn Appender>> {
            Ok(Box::new(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.at(path))?,
            ))
        }

        fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
            let mut file = fs::File::create(self.at(path))?;
            file.write_all(bytes)?;
            file.sync_all()
        }

        fn replace(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
            tilo_files::native::atomic::write_creating_parent(&self.at(path), bytes)
        }

        fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
            let file = fs::OpenOptions::new().write(true).open(self.at(path))?;
            file.set_len(len)?;
            file.sync_all()
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            fs::rename(self.at(from), self.at(to))
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            fs::remove_file(self.at(path))
        }

        fn remove_all(&self, dir: &Path) -> io::Result<()> {
            fs::remove_dir_all(self.at(dir))
        }

        fn sync_dir(&self, dir: &Path) -> io::Result<()> {
            tilo_files::native::atomic::sync_directory(&self.at(dir))
        }
    }

    impl Appender for fs::File {
        fn append(&self, bytes: &[u8]) -> io::Result<()> {
            (&*self).write_all(bytes)
        }

        fn truncate(&self, len: u64) -> io::Result<()> {
            self.set_len(len)
        }

        fn sync(&self, _through: u64) -> io::Result<()> {
            self.sync_data()
        }
    }

    impl ReadAt for fs::File {
        fn read_exact_at(&mut self, at: u64, bytes: &mut [u8]) -> io::Result<()> {
            self.seek(io::SeekFrom::Start(at))?;
            self.read_exact(bytes)
        }
    }
}
