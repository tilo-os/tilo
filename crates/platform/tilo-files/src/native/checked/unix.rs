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
    /// The identity of what `path` names now, following symbolic links; `None` when nothing is there.
    /// Rewriting a file keeps it, so a caller can compare against what it recorded.
    pub fn of_path(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::metadata(path) {
            Ok(metadata) => Self::from_metadata(&metadata).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The inode an open file reads from, so replacing the path's entry afterwards cannot name another
    /// file's identity.
    pub fn of_file(file: &std::fs::File) -> io::Result<Self> {
        Self::from_metadata(&file.metadata()?)
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
    /// Creates missing parents without following links, then retains the parent descriptor; `..` is
    /// rejected rather than resolved.
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

    /// Open and validate before `flock`: a FIFO must never make opening block. Rejects symbolic links
    /// and nonregular files.
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

    /// Read bytes and identify the same opened regular file; only absence returns `None`, links and
    /// nonregular files are refused.
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
    pub fn identity(&self) -> io::Result<Option<Identity>> {
        self.open_file()?
            .map(|file| Identity::from_metadata(&regular_metadata(&file)?))
            .transpose()
    }

    /// Atomically publish a synced sibling, then sync the retained directory; a durability error can
    /// occur after publication.
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
