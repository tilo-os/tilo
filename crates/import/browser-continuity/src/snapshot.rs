//! Read-only capture of the exact source files selected by a reviewed profile.
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};

use cap_fs_ext::OpenOptionsMaybeDirExt;
use rusqlite::{Connection, OpenFlags};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
use zeroize::Zeroizing;

use crate::Profile;
use crate::sources::MAX_DATABASE_BYTES;

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
/// The identity and timestamps of one source file, compared before and after a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    /// The file's length as both reads observed it.
    #[must_use]
    pub fn length(self) -> u64 {
        self.length
    }
    /// Read the stamp of an open source file; refuses a non-regular file.
    ///
    /// # Errors
    /// Returns the metadata error, or `InvalidData` for a non-regular file.
    pub fn of(file: &File) -> io::Result<Self> {
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
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn limited() -> io::Error {
    io::Error::new(io::ErrorKind::FileTooLarge, "source exceeds the read limit")
}
fn sqlite_error(_: rusqlite::Error) -> io::Error {
    invalid("source database is unreadable or incompatible")
}

/// One source file captured under the bounded, stable read rule.
#[derive(PartialEq, Eq)]
pub struct CapturedBytes {
    stamp: Stamp,
    bytes: Zeroizing<Vec<u8>>,
}
impl CapturedBytes {
    /// The stamp both reads of this file agreed on.
    #[must_use]
    pub fn stamp(&self) -> Stamp {
        self.stamp
    }
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    #[must_use]
    pub fn into_bytes(self) -> Zeroizing<Vec<u8>> {
        self.bytes
    }
}

/// Read one named file of an owner-granted directory twice and re-attest its
/// identity after each read; `None` when the name does not exist.
///
/// # Errors
/// Refuses foreign ownership, a non-regular file, a source changed by a
/// concurrent writer, or one larger than `limit`.
pub fn capture_file(
    directory: &StorageDirectory,
    owner: u32,
    name: &str,
    limit: u64,
) -> io::Result<Option<CapturedBytes>> {
    directory.validate_namespace_control()?;
    let file = match directory.open_read(name.as_ref(), Privacy::Inherit) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stamp = Stamp::of(&file)?;
    if stamp.owner != owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "source ownership is unacceptable",
        ));
    }
    if stamp.length > limit {
        return Err(limited());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
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
    if now.owner != owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "source ownership is unacceptable",
        ));
    }
    if now.mode != stamp.mode {
        return Err(raced());
    }
    if bytes.len() as u64 > limit {
        return Err(limited());
    }
    if bytes.len() as u64 != stamp.length || now != stamp {
        return Err(raced());
    }
    Ok(Some(CapturedBytes { stamp, bytes }))
}

/// A database file and its optional `-wal`, captured together as one stable pair.
pub struct CapturedDatabase {
    database: CapturedBytes,
    wal: Option<CapturedBytes>,
}
impl CapturedDatabase {
    #[must_use]
    pub fn database(&self) -> &[u8] {
        self.database.bytes()
    }
    #[must_use]
    pub fn wal(&self) -> Option<&[u8]> {
        self.wal.as_ref().map(CapturedBytes::bytes)
    }
}

/// Capture `name` and its `-wal` from an owner-granted directory, reading both
/// twice and refusing any change between the reads.
///
/// # Errors
/// Refuses a missing database, foreign ownership, oversized files and concurrent
/// changes, without touching the source.
pub fn capture_database(
    directory: &StorageDirectory,
    owner: u32,
    name: &str,
) -> io::Result<CapturedDatabase> {
    directory.validate_namespace_control()?;
    let wal_name = format!("{name}-wal");
    let database = capture_file(directory, owner, name, MAX_DATABASE_BYTES)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "source database is missing"))?;
    let wal = capture_file(directory, owner, &wal_name, MAX_DATABASE_BYTES)?;
    let stable_database = capture_file(directory, owner, name, MAX_DATABASE_BYTES)?;
    let stable_wal = capture_file(directory, owner, &wal_name, MAX_DATABASE_BYTES)?;
    if stable_database.as_ref() != Some(&database) || stable_wal != wal {
        return Err(raced());
    }
    directory.validate_namespace_control()?;
    Ok(CapturedDatabase { database, wal })
}

/// A read-only connection to a private copy of a captured database.
pub struct OpenedDatabase {
    _temporary: tempfile::TempDir,
    connection: Connection,
}
impl OpenedDatabase {
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}
impl std::ops::Deref for OpenedDatabase {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}

/// Copy a captured database and WAL into private storage, open the copy
/// read-only and run `quick_check`.
///
/// # Errors
/// Refuses a temporary directory or copy that cannot be made, an unreadable
/// database, or one that fails the integrity check.
pub fn open_captured(name: &str, database: &CapturedDatabase) -> io::Result<OpenedDatabase> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join(name);
    private_write(&path, database.database.bytes())?;
    if let Some(wal) = &database.wal {
        private_write(&temporary.path().join(format!("{name}-wal")), wal.bytes())?;
    }
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_error)?;
    connection
        .execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN;")
        .map_err(sqlite_error)?;
    let check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(sqlite_error)?;
    if check != "ok" {
        return Err(invalid("source database integrity check failed"));
    }
    Ok(OpenedDatabase {
        _temporary: temporary,
        connection,
    })
}

fn private_write(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut file = tilo_files::native::private::create_new(path)?;
    file.write_all(bytes)
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
