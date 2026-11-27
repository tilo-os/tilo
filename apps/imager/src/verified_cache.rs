//! A private, verified, content-addressed file cache, and the trusted downloads that fill it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tilo_net::http::fetch;

/// The person's sentence for a cache failure.
pub(crate) fn message(noun: &str, error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::Interrupted => format!("{noun} download cancelled."),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            format!("The {noun} does not match its signed size and SHA-256.")
        }
        _ => format!("{noun} cache: {error}"),
    }
}

fn mismatch() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "the cache entry did not verify")
}

/// A downloader that trusts only `origin`, every redirect checked against it.
pub(crate) fn downloader(
    origin: reqwest::Url,
    timeout: Duration,
    agent: &str,
) -> Result<fetch::Downloader, String> {
    fetch::Downloader::new(origin, timeout, Some(agent))
        .map_err(|error| format!("Could not initialize the downloader: {error}"))
}

/// The person's sentence for a download that did not produce the verified file.
pub(crate) fn fetch_error(noun: &str, error: fetch::Error) -> String {
    match error {
        fetch::Error::Cancelled => format!("{noun} download cancelled."),
        fetch::Error::Digest => format!("The {noun} download did not match its signed catalog."),
        fetch::Error::Size { .. } => format!("The {noun} download has an unexpected size."),
        fetch::Error::Incomplete(_) | fetch::Error::Paused => {
            format!("The {noun} download was interrupted before it completed.")
        }
        fetch::Error::Io(error) => format!("Could not save the {noun}: {error}"),
        fetch::Error::Client(detail) => format!("Could not initialize the downloader: {detail}"),
        fetch::Error::Trusted(detail) | fetch::Error::Protocol(detail) => {
            format!("Could not download the {noun}: {detail}")
        }
        fetch::Error::Hook(detail) => format!("Could not save the {noun}: {detail}"),
    }
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(path)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != tilo_process::effective_uid() {
        return Err(io::Error::other(format!(
            "{} must be a directory owned by this user",
            path.display()
        )));
    }
    if metadata.mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn trusted(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.is_file()
        && metadata.mode() & 0o777 == 0o600
        && metadata.nlink() == 1
        && metadata.uid() == tilo_process::effective_uid()
}

#[cfg(not(unix))]
fn trusted(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
}

/// A cache directory, held to the identity it was opened with.
pub(crate) struct Cache {
    path: PathBuf,
    identity: (u64, u64, u32),
}

impl Cache {
    /// Create `path` private to this user when it is absent, and pin its identity.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        create_private(path)?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(path)?;
            (metadata.dev(), metadata.ino(), 0o700)
        };
        #[cfg(not(unix))]
        let identity = (0, 0, 0);
        Ok(Self {
            path: path.to_path_buf(),
            identity,
        })
    }

    /// `name` when its bytes are exactly `size` long and hash to `sha256`; absent when there is
    /// no such entry. An entry that does not verify is reported, not removed.
    pub fn verified(
        &self,
        name: &str,
        size: u64,
        sha256: &str,
        cancel: &AtomicBool,
    ) -> io::Result<Option<PathBuf>> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            libc::O_NOFOLLOW | libc::O_NONBLOCK,
        );
        let mut file = match options.open(self.path.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        self.verify(&mut file, size, sha256, cancel)?;
        self.checked(name).map(Some)
    }

    /// A fresh staging file beside the entry it will become, removed unless published.
    pub fn staging(&self) -> io::Result<Staging> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let path = self.path.join(format!(
                ".download-{}-{}.part",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            match options.open(&path) {
                Ok(file) => return Ok(Staging { path, file }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("could not allocate cache staging"))
    }

    /// Verify `staging`, publish it as `name`, and return the verified entry. An entry another
    /// process published first is verified and kept instead of replaced.
    pub fn publish(
        &self,
        mut staging: Staging,
        name: &str,
        size: u64,
        sha256: &str,
        cancel: &AtomicBool,
    ) -> io::Result<PathBuf> {
        self.verify(&mut staging.file, size, sha256, cancel)?;
        staging.file.sync_all()?;
        match fs::hard_link(&staging.path, self.path.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let mut published = OpenOptions::new().read(true).open(self.path.join(name))?;
                self.verify(&mut published, size, sha256, cancel)?;
            }
            Err(error) => return Err(error),
        }
        drop(staging);
        self.checked(name)
    }

    fn verify(
        &self,
        file: &mut File,
        size: u64,
        sha256: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !trusted(&file.metadata()?) {
            return Err(mismatch());
        }
        file.seek(SeekFrom::Start(0))?;
        let digest = crate::backend::write::hash_source_exact(
            file,
            size,
            &|| cancel.load(Ordering::Acquire),
            |_| {},
        )?;
        if digest == sha256 {
            Ok(())
        } else {
            Err(mismatch())
        }
    }

    fn checked(&self, name: &str) -> io::Result<PathBuf> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(&self.path)?;
            if !metadata.is_dir()
                || (metadata.dev(), metadata.ino(), metadata.mode() & 0o777) != self.identity
            {
                return Err(io::Error::other(format!(
                    "{} changed during the download",
                    self.path.display()
                )));
            }
        }
        Ok(self.path.join(name))
    }
}

pub(crate) struct Staging {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
