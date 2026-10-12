//! Read-only capture of the exact source files selected by a reviewed profile.
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};

use cap_fs_ext::OpenOptionsMaybeDirExt;
use tilo_files::native::atomic::{Privacy, StorageDirectory};

use crate::Profile;

pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_SNAPSHOT_BYTES: u64 = 128 * 1024 * 1024;

/// Open precisely one profile inside the separately admitted subset root.
///
/// # Errors
/// Refuses malformed selections, foreign namespaces and redirected directories.
pub fn profile_directory(root: &File, profile: &Profile) -> io::Result<File> {
    profile.validate().map_err(|_| failed())?;
    let held = StorageDirectory::from_file(root.try_clone()?)?;
    held.validate_namespace_control()?;
    let profiles = cap_primitives::fs::open_dir_nofollow(
        root,
        std::path::Path::new(crate::PROFILES_DIRECTORY),
    )?;
    let profiles = readable_directory(&profiles)?;
    StorageDirectory::from_file(profiles.try_clone()?)?.validate_namespace_control()?;
    let selected =
        cap_primitives::fs::open_dir_nofollow(&profiles, std::path::Path::new(&profile.id))?;
    let selected = readable_directory(&selected)?;
    StorageDirectory::from_file(selected.try_clone()?)?.validate_namespace_control()?;
    Ok(selected)
}

fn readable_directory(directory: &File) -> io::Result<File> {
    let mut options = cap_primitives::fs::OpenOptions::new();
    options.read(true).maybe_dir(true);
    cap_primitives::fs::open(directory, std::path::Path::new("."), &options)
}

/// Bytes copied from one fixed legacy data file; never cookies or session stores.
pub struct CapturedFile {
    name: &'static str,
    bytes: Vec<u8>,
    stamp: Stamp,
}
impl CapturedFile {
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
/// A stable bounded snapshot. It contains no live source paths or write grants.
pub struct Snapshot {
    files: Vec<CapturedFile>,
    fingerprint: [u8; 32],
}
impl Snapshot {
    #[must_use]
    pub fn files(&self) -> &[CapturedFile] {
        &self.files
    }
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.stamp.length).sum()
    }
}
#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(failed());
        }
        Ok(Self {
            device: meta.dev(),
            inode: meta.ino(),
            owner: meta.uid(),
            mode: meta.mode(),
            length: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        })
    }
}
#[derive(Debug)]
struct ContentChanged;
impl std::fmt::Display for ContentChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Selected browser contents changed during capture")
    }
}
impl std::error::Error for ContentChanged {}
fn raced() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, ContentChanged)
}
/// True only for concurrent contents changes; grants and access failures never retry.
#[must_use]
pub fn content_changed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<ContentChanged>)
}

fn failed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Selected browser data is unavailable or changed",
    )
}
fn optional(name: &str) -> bool {
    matches!(name, "History-wal" | "places.sqlite-wal")
}
fn read(
    directory: &StorageDirectory,
    owner: u32,
    name: &'static str,
    cancel: &AtomicBool,
) -> io::Result<Option<CapturedFile>> {
    let file = match directory.open_read(name.as_ref(), Privacy::Inherit) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound && optional(name) => return Ok(None),
        Err(error) => return Err(error),
    };
    let stamp = Stamp::of(&file)?;
    if stamp.owner != owner || stamp.length > MAX_FILE_BYTES {
        return Err(failed());
    }
    let mut bytes = Vec::new();
    let mut input = file.take(MAX_FILE_BYTES + 1);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(buffer.get(..count).ok_or_else(failed)?);
    }
    let current = directory
        .open_read(name.as_ref(), Privacy::Inherit)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                raced()
            } else {
                error
            }
        })?;
    let now = Stamp::of(&current)?;
    if now.owner != owner || now.mode != stamp.mode {
        return Err(failed());
    }
    if bytes.len() as u64 != stamp.length || now != stamp {
        return Err(raced());
    }
    Ok(Some(CapturedFile { name, bytes, stamp }))
}
/// Copy only the selected fixed files from a retained owner-granted directory.
/// Reads every selected file twice and reattests all names after the full pair.
/// Neither `SQLite` nor source-side locks, SHM, chmod, creation or writes occur.
///
/// # Errors
/// Refuses missing selected data, redirecting names, foreign ownership, changing
/// source state, unsupported namespace authority or explicit byte bounds.
pub fn capture(directory: File, selection: &Profile) -> io::Result<Snapshot> {
    capture_cancellable(directory, selection, &AtomicBool::new(false))
}
fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Browser capture cancelled",
        ))
    } else {
        Ok(())
    }
}
/// Capture with cooperative cancellation between bounded source reads.
///
/// # Errors
/// Refuses the same changed grants/content as `capture`, or caller cancellation.
pub fn capture_cancellable(
    directory: File,
    selection: &Profile,
    cancel: &AtomicBool,
) -> io::Result<Snapshot> {
    check_cancel(cancel)?;
    selection.validate().map_err(|_| failed())?;
    let owner = directory.metadata()?.uid();
    let directory = StorageDirectory::from_file(directory)?;
    directory.validate_namespace_control()?;
    let mut files = Vec::new();
    let mut total = 0u64;
    for &name in selection.allowed_files() {
        if let Some(file) = read(&directory, owner, name, cancel)? {
            total = total.checked_add(file.stamp.length).ok_or_else(failed)?;
            if total > MAX_SNAPSHOT_BYTES {
                return Err(failed());
            }
            files.push(file);
        }
    }
    for &name in selection.allowed_files() {
        let actual = read(&directory, owner, name, cancel)?;
        let expected = files.iter().find(|file| file.name == name);
        match (expected, actual) {
            (None, None) => {}
            (Some(expected), Some(actual))
                if expected.stamp == actual.stamp && expected.bytes == actual.bytes => {}
            (Some(expected), Some(actual)) if expected.stamp.mode != actual.stamp.mode => {
                return Err(failed());
            }
            _ => return Err(raced()),
        }
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-browser-selected-profile-snapshot-v1");
    for &name in selection.allowed_files() {
        check_cancel(cancel)?;
        let expected = files.iter().find(|file| file.name == name);
        match directory.open_read(name.as_ref(), Privacy::Inherit) {
            Ok(file) => {
                let actual = Stamp::of(&file)?;
                if actual.owner != owner
                    || expected.is_some_and(|value| value.stamp.mode != actual.mode)
                {
                    return Err(failed());
                }
                if !expected.is_some_and(|value| value.stamp == actual) {
                    return Err(raced());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if expected.is_some() || !optional(name) {
                    return Err(raced());
                }
            }
            Err(error) => return Err(error),
        }
        hash.update(name.as_bytes());
        if let Some(file) = expected {
            hash.update(&[1]);
            hash.update(&(file.bytes.len() as u64).to_le_bytes());
            hash.update(&file.bytes);
        } else {
            hash.update(&[0]);
        }
    }
    directory.validate_namespace_control()?;
    Ok(Snapshot {
        files,
        fingerprint: *hash.finalize().as_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn selection(bookmarks: bool, history: bool) -> Profile {
        Profile::new(
            "a".repeat(64),
            Provider::Chromium,
            vec!["Default".into()],
            bookmarks,
            history,
        )
        .unwrap()
    }
    #[test]
    fn cancellation_and_redirected_source_failures_are_never_content_races() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Bookmarks"), b"selected bookmarks").unwrap();
        let stopped = AtomicBool::new(true);
        let error = capture_cancellable(
            File::open(root.path()).unwrap(),
            &selection(true, false),
            &stopped,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!content_changed(&error));
        fs::remove_file(root.path().join("Bookmarks")).unwrap();
        fs::write(root.path().join("Cookies"), b"unselected credentials").unwrap();
        symlink("Cookies", root.path().join("Bookmarks")).unwrap();
        let error = capture(File::open(root.path()).unwrap(), &selection(true, false))
            .err()
            .unwrap();
        assert!(!content_changed(&error));
        assert_eq!(
            fs::read(root.path().join("Cookies")).unwrap(),
            b"unselected credentials"
        );
    }

    #[test]
    fn actual_selected_files_are_captured_readonly_without_cookies_or_shm() {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("Bookmarks", b"bookmarks".as_slice()),
            ("History", b"history"),
            ("History-wal", b"wal"),
            ("Cookies", b"unrelated credentials"),
            ("History-shm", b"runtime"),
        ] {
            fs::write(root.path().join(name), bytes).unwrap();
        }
        let before: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.clone(),
                    fs::read(&path).unwrap(),
                    fs::metadata(&path).unwrap().permissions().mode(),
                )
            })
            .collect();
        let first = capture(File::open(root.path()).unwrap(), &selection(true, false)).unwrap();
        assert_eq!(
            first
                .files()
                .iter()
                .map(CapturedFile::name)
                .collect::<Vec<_>>(),
            ["Bookmarks"]
        );
        assert_eq!(first.files()[0].bytes(), b"bookmarks");
        let second = capture(File::open(root.path()).unwrap(), &selection(true, true)).unwrap();
        assert_eq!(
            second
                .files()
                .iter()
                .map(CapturedFile::name)
                .collect::<Vec<_>>(),
            ["Bookmarks", "History", "History-wal"]
        );
        assert_eq!(second.bytes(), 19);
        assert_ne!(first.fingerprint(), second.fingerprint());
        assert_eq!(
            second.fingerprint(),
            capture(File::open(root.path()).unwrap(), &selection(true, true))
                .unwrap()
                .fingerprint()
        );
        for (path, bytes, mode) in before {
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert_eq!(fs::metadata(path).unwrap().permissions().mode(), mode);
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 5);
    }
    #[test]
    fn redirected_missing_or_oversized_selected_files_refuse_without_repair() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("secret"), b"unchanged").unwrap();
        symlink(other.path().join("secret"), root.path().join("Bookmarks")).unwrap();
        assert!(capture(File::open(root.path()).unwrap(), &selection(true, false)).is_err());
        fs::remove_file(root.path().join("Bookmarks")).unwrap();
        assert!(capture(File::open(root.path()).unwrap(), &selection(true, false)).is_err());
        let file = File::create(root.path().join("Bookmarks")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(capture(File::open(root.path()).unwrap(), &selection(true, false)).is_err());
        assert_eq!(
            fs::metadata(root.path().join("Bookmarks")).unwrap().len(),
            MAX_FILE_BYTES + 1
        );
        assert_eq!(fs::read(other.path().join("secret")).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
