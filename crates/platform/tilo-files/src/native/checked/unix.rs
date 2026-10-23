use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use super::Identity;
use advisory_lock::FileLock;
use rustix::fs::{self, AtFlags, Mode, OFlags};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::NOFOLLOW
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

impl Identity {
    /// The identity of what `path` names now, following symbolic links;
    /// `None` when nothing is there. Rewriting a file or touching its times
    /// moves its change time, which no unprivileged process can set, and
    /// adding or removing an entry moves a directory's times.
    pub fn of_path(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::metadata(path) {
            Ok(metadata) => Self::from_metadata(&metadata).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            device: metadata.dev(),
            inode: metadata.ino(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

/// The parent descriptor remains valid even if an ancestor is renamed.
#[derive(Debug)]
pub struct CheckedCopy {
    directory: File,
    name: OsString,
    lock_name: OsString,
}

impl CheckedCopy {
    /// Create missing parents without following links, then retain the parent
    /// descriptor. `..` is rejected rather than allowed to escape the walk.
    ///
    /// # Errors
    /// Rejects links, parent components and paths without a file name;
    /// preserves native traversal, creation and synchronization errors.
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "checked file needs a file name",
            )
        })?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut directory = File::from(fs::open(
            if path.is_absolute() { "/" } else { "." },
            DIRECTORY_FLAGS,
            Mode::empty(),
        )?);
        for component in parent.components() {
            let name = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::Normal(name) => name,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "checked file path must not contain parent components",
                    ));
                }
            };
            let next = match fs::openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()) {
                Ok(next) => next,
                Err(rustix::io::Errno::NOENT) => {
                    match fs::mkdirat(&directory, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) => directory.sync_all()?,
                        Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    fs::openat(&directory, name, DIRECTORY_FLAGS, Mode::empty())?
                }
                Err(error) => return Err(error.into()),
            };
            directory = File::from(next);
        }
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        Ok(Self {
            directory,
            name: name.to_os_string(),
            lock_name,
        })
    }

    /// Open and validate before flock: a FIFO must never make opening block.
    ///
    /// # Errors
    /// Rejects symbolic links and nonregular locks; preserves open and lock errors.
    pub fn lock(&self) -> io::Result<FileLock> {
        let file = File::from(fs::openat(
            &self.directory,
            &self.lock_name,
            FILE_FLAGS | OFlags::RDWR | OFlags::CREATE,
            Mode::from_raw_mode(0o600),
        )?);
        regular_metadata(&file)?;
        FileLock::exclusive(file)
    }

    fn open_file(&self) -> io::Result<Option<File>> {
        match fs::openat(
            &self.directory,
            &self.name,
            FILE_FLAGS | OFlags::RDONLY,
            Mode::empty(),
        ) {
            Ok(file) => Ok(Some(File::from(file))),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Read bytes and identify the same opened regular file. Only absence
    /// returns `None`; links and nonregular files are errors.
    pub fn read(&self) -> io::Result<(Vec<u8>, Option<Identity>)> {
        let Some(mut file) = self.open_file()? else {
            return Ok((Vec::new(), None));
        };
        let metadata = regular_metadata(&file)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        Ok((contents, Some(Identity::from_metadata(&metadata)?)))
    }

    /// Validate and identify the opened inode, never metadata from a path.
    ///
    /// # Errors
    /// Rejects links and nonregular files; preserves open and metadata errors.
    pub fn identity(&self) -> io::Result<Option<Identity>> {
        self.open_file()?
            .map(|file| Identity::from_metadata(&regular_metadata(&file)?))
            .transpose()
    }

    /// Atomically publish a synced sibling, then sync the retained directory.
    /// A durability error can occur after publication; callers must forget
    /// their cached identity on any error and re-read before the next merge.
    ///
    /// # Errors
    /// Rejects unsafe existing entries; preserves creation, write, sync,
    /// rename and metadata errors. A failed temporary is removed on drop.
    pub fn write(&self, bytes: &[u8]) -> io::Result<Identity> {
        // Refuse an unsafe existing copy rather than silently repairing it.
        self.identity()?;
        let mut temporary = self.temporary()?;
        temporary.file.write_all(bytes)?;
        temporary.file.sync_all()?;
        fs::renameat(
            &self.directory,
            &temporary.name,
            &self.directory,
            &self.name,
        )?;
        temporary.published = true;
        self.directory.sync_all()?;
        Identity::from_metadata(&regular_metadata(&temporary.file)?)
    }

    fn temporary(&self) -> io::Result<Temporary<'_>> {
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = OsString::from(format!(
                ".checked-copy-{}-{sequence}.tmp",
                std::process::id()
            ));
            match fs::openat(
                &self.directory,
                &name,
                FILE_FLAGS | OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                Mode::from_raw_mode(0o600),
            ) {
                Ok(file) => {
                    return Ok(Temporary {
                        directory: &self.directory,
                        name,
                        file: File::from(file),
                        published: false,
                    });
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot create checked temporary file: remove stale .checked-copy-*.tmp files",
        ))
    }
}

fn regular_metadata(file: &File) -> io::Result<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "checked file and lock must be regular files",
        ));
    }
    Ok(metadata)
}

struct Temporary<'a> {
    directory: &'a File,
    name: OsString,
    file: File,
    published: bool,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::unlinkat(self.directory, &self.name, AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn missing_is_empty_and_arbitrary_bytes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let copy = CheckedCopy::open(&root.join("settings.json")).unwrap();
        assert_eq!(copy.read().unwrap(), (Vec::new(), None));
        for contents in [b"".as_slice(), b"{", b"[]", b"null", b"\xff"] {
            let identity = copy.write(contents).unwrap();
            assert_eq!(copy.read().unwrap(), (contents.to_vec(), Some(identity)));
            assert_eq!(copy.identity().unwrap(), Some(identity));
        }
    }

    #[test]
    fn links_in_ancestors_copy_and_lock_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        symlink("real", root.join("alias")).unwrap();
        assert!(CheckedCopy::open(&root.join("alias/settings.json")).is_err());
        let path = root.join("real/settings.json");
        let copy = CheckedCopy::open(&path).unwrap();
        std::fs::write(root.join("target"), b"{}").unwrap();
        symlink("../target", &path).unwrap();
        symlink("../target", root.join("real/settings.json.lock")).unwrap();
        assert!(copy.read().is_err());
        assert!(copy.identity().is_err());
        assert!(copy.write(b"{}").is_err());
        assert!(copy.lock().is_err());
        assert_eq!(std::fs::read(root.join("target")).unwrap(), b"{}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fifo_copy_and_lock_are_rejected_without_waiting_for_a_peer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("settings.json");
        fs::mknodat(
            fs::CWD,
            &path,
            fs::FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        fs::mknodat(
            fs::CWD,
            root.join("settings.json.lock"),
            fs::FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let copy = CheckedCopy::open(&path).unwrap();
        assert!(copy.read().is_err());
        assert!(copy.identity().is_err());
        assert!(copy.lock().is_err());
    }

    #[test]
    fn renamed_parent_stays_anchored_for_read_lock_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let parent = root.join("original");
        let copy = CheckedCopy::open(&parent.join("settings.json")).unwrap();
        let moved = root.join("moved");
        std::fs::rename(&parent, &moved).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("settings.json"), b"{\"foreign\":true}").unwrap();
        let lock = copy.lock().unwrap();
        let document = b"{\"value\":true}".to_vec();
        let identity = copy.write(&document).unwrap();
        assert_eq!(copy.read().unwrap(), (document, Some(identity)));
        assert_eq!(copy.identity().unwrap(), Some(identity));
        assert!(moved.join("settings.json.lock").is_file());
        assert!(!parent.join("settings.json.lock").exists());
        assert_eq!(
            std::fs::read(parent.join("settings.json")).unwrap(),
            b"{\"foreign\":true}"
        );
        drop(lock);
    }

    #[test]
    fn ordinary_writer_lock_contends_with_checked_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let copy = CheckedCopy::open(&root.join("settings.json")).unwrap();
        let lock = copy.lock().unwrap();
        let peer = File::options()
            .read(true)
            .write(true)
            .open(root.join("settings.json.lock"))
            .unwrap();
        assert!(matches!(
            FileLock::try_exclusive(peer),
            Err(advisory_lock::TryLockError::WouldBlock)
        ));
        drop(lock);
        let peer = File::options()
            .read(true)
            .write(true)
            .open(root.join("settings.json.lock"))
            .unwrap();
        assert!(FileLock::try_exclusive(peer).is_ok());
    }
}
