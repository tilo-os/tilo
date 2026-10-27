//! The freedesktop Trash, by its directory layout alone.
//!
//! A Trash directory holds `files/<name>`, the trashed item, and
//! `info/<name>.trashinfo`, where it came from and when
//! (<https://specifications.freedesktop.org/trash-spec/latest/>). Everything
//! here works from those two directories and the item: no mount table, which
//! a sandboxed app cannot read. Whether an item can go into a Trash is the
//! rename's answer (`EXDEV`: another filesystem, or another mount of one),
//! not a lookup.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// Why an item did not go into any of the Trashes it was offered.
#[derive(Debug)]
pub(super) enum MoveError {
    /// It is on another filesystem than every one of them.
    OtherFilesystem,
    Io(io::Error),
}

impl From<io::Error> for MoveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Move `path` into the first of `trashes` (each a Trash's `files`
/// directory) on its filesystem, preparing that Trash with `prepare` first
/// (the first is prepared already by the caller, later ones only when
/// reached). Returns the entry it became in `files`.
pub(super) fn move_in(
    path: &Path,
    trashes: &[PathBuf],
    prepare: &dyn Fn(&Path) -> io::Result<()>,
) -> Result<PathBuf, MoveError> {
    let original = absolute(path)?;
    // The item itself, not what a symbolic link names.
    std::fs::symlink_metadata(&original)?;
    for (index, files) in trashes.iter().enumerate() {
        if index > 0 {
            prepare(files)?;
        }
        match move_into(&original, files) {
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {}
            other => return other.map_err(MoveError::Io),
        }
    }
    Err(MoveError::OtherFilesystem)
}

/// `path` as an absolute path whose folders are resolved, its own name kept
/// (a trashed symbolic link is the link).
fn absolute(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nothing to trash"))?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok(parent.canonicalize()?.join(name))
}

/// Move `original` into the Trash whose `files` directory is `files`: the
/// record first, then the item under the record's name, so a crash leaves at
/// worst a record without an item, never an item nobody can restore. A name
/// already taken gets `.2`, `.3`, … as other implementations do.
fn move_into(original: &Path, files: &Path) -> io::Result<PathBuf> {
    let info = info_dir(files)?;
    let leaf = original
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nothing to trash"))?;
    let record = record(original);
    for attempt in 1_u32.. {
        let mut name = leaf.to_os_string();
        if attempt > 1 {
            name.push(format!(".{attempt}"));
        }
        let info_path = info.join(info_name(&name));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let entry = files.join(&name);
        let moved = file
            .write_all(record.as_bytes())
            .and_then(|()| crate::rename_noreplace(original, &entry));
        match moved {
            Ok(()) => return Ok(entry),
            Err(error) => {
                let _ = std::fs::remove_file(&info_path);
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
    }
    Err(io::Error::other("no free name in Trash"))
}

/// The `.trashinfo` record of `original`, trashed now.
fn record(original: &Path) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode(original.as_os_str()),
        local_now()
    )
}

/// The Trash's `info` directory beside `files`.
fn info_dir(files: &Path) -> io::Result<PathBuf> {
    if files.file_name() != Some(OsStr::new("files")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a Trash files directory",
        ));
    }
    files
        .parent()
        .map(|trash| trash.join("info"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Trash root is unavailable"))
}

fn info_name(name: &OsStr) -> OsString {
    let mut info = name.to_os_string();
    info.push(".trashinfo");
    info
}

/// The record of `entry` (an item in a Trash's `files` directory).
fn info_of(entry: &Path) -> io::Result<PathBuf> {
    let files = entry
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a Trash entry"))?;
    let name = entry
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a Trash entry"))?;
    Ok(info_dir(files)?.join(info_name(name)))
}

/// Where the item `entry` was trashed from, by its record. A relative `Path`
/// is relative to the folder holding the Trash, as in a Trash at a volume's
/// top.
fn original_of(entry: &Path, info: &Path) -> io::Result<PathBuf> {
    let text = std::fs::read(info)?;
    let encoded = text
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"Path="))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Trash record has no Path"))?;
    let path = PathBuf::from(OsString::from_vec(decode(encoded.trim_ascii_end())));
    if path.is_absolute() {
        return Ok(path);
    }
    entry
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|top| top.join(&path))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Trash record has no base"))
}

/// Put each of `entries` back where it was trashed from. An entry that is no
/// longer in Trash (no item or no record) is skipped; returns how many came
/// back.
pub(super) fn restore(entries: &[PathBuf]) -> io::Result<usize> {
    let mut restored = 0_usize;
    for entry in entries {
        let info = info_of(entry)?;
        if std::fs::symlink_metadata(entry).is_err() {
            continue;
        }
        let original = match original_of(entry, &info) {
            Ok(original) => original,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if let Some(parent) = original.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::rename_noreplace(entry, &original).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(
                    error.kind(),
                    format!("{} already exists", original.display()),
                )
            } else {
                error
            }
        })?;
        remove_record(&info)?;
        restored = restored.saturating_add(1);
    }
    Ok(restored)
}

/// Delete each of `entries` and its record for good. An entry without a
/// record is not Trash's to delete and is skipped; returns how many went.
pub(super) fn purge(entries: &[PathBuf]) -> io::Result<usize> {
    let mut purged = 0_usize;
    for entry in entries {
        let info = info_of(entry)?;
        if std::fs::symlink_metadata(&info).is_err() {
            continue;
        }
        remove_item(entry)?;
        remove_record(&info)?;
        purged = purged.saturating_add(1);
    }
    Ok(purged)
}

/// Delete everything in the Trash whose `files` directory is `files`, and
/// every record left in its `info`. Returns how many items went.
pub(super) fn empty(files: &Path) -> io::Result<usize> {
    let info = info_dir(files)?;
    let mut purged = 0_usize;
    match std::fs::read_dir(files) {
        Ok(items) => {
            for item in items {
                remove_item(&item?.path())?;
                purged = purged.saturating_add(1);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match std::fs::read_dir(&info) {
        Ok(records) => {
            for record in records {
                remove_record(&record?.path())?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(purged)
}

fn remove_item(entry: &Path) -> io::Result<()> {
    let removed = match std::fs::symlink_metadata(entry) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(entry),
        Ok(_) => std::fs::remove_file(entry),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn remove_record(info: &Path) -> io::Result<()> {
    match std::fs::remove_file(info) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// `path` as a URI path: every byte but unreserved characters and `/`
/// percent-encoded, as the spec asks of a record's `Path`.
fn encode(path: &OsStr) -> String {
    let mut out = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn decode(encoded: &[u8]) -> Vec<u8> {
    fn hex(digit: u8) -> Option<u8> {
        char::from(digit)
            .to_digit(16)
            .and_then(|value| u8::try_from(value).ok())
    }
    let mut out = Vec::with_capacity(encoded.len());
    let mut rest = encoded;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%'
            && let [high, low, after @ ..] = tail
            && let (Some(high), Some(low)) = (hex(*high), hex(*low))
        {
            out.push(high.wrapping_mul(16) | low);
            rest = after;
        } else {
            out.push(byte);
            rest = tail;
        }
    }
    out
}

/// Local time now as the spec's `DeletionDate` wants it, `YYYY-MM-DDThh:mm:ss`.
fn local_now() -> String {
    // SAFETY: `time` with a null pointer only returns the current time.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: an all-zero `tm` is a valid value of a plain C struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers refer to live values of the right types;
    // `localtime_r` is the reentrant form and keeps no reference.
    if unsafe { libc::localtime_r(&raw const now, &raw mut tm) }.is_null() {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        i64::from(tm.tm_year).saturating_add(1900),
        tm.tm_mon.saturating_add(1),
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "host-fs-trash-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("data/Trash/files")).unwrap();
            std::fs::create_dir_all(dir.join("data/Trash/info")).unwrap();
            std::fs::create_dir_all(dir.join("home/Downloads")).unwrap();
            Self(dir)
        }

        fn files(&self) -> PathBuf {
            self.0.join("data/Trash/files")
        }

        fn downloads(&self) -> PathBuf {
            self.0.join("home/Downloads").canonicalize().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unprepared(_: &Path) -> io::Result<()> {
        Err(io::Error::other("only the first Trash was needed"))
    }

    /// A sandboxed app cannot read the mount table, and the `trash` crate
    /// needs it for every move ("Neither '/proc/mounts' nor '/etc/mtab'
    /// could be opened."), so Files could not move anything to Trash. The
    /// move here is the record, then a rename, and a restore puts the item
    /// back by its record alone.
    #[test]
    fn an_item_goes_to_trash_and_comes_back_by_its_record_alone() {
        let scratch = Scratch::new("round-trip");
        let film = scratch.downloads().join("holiday film #1.mkv");
        std::fs::write(&film, b"frames").unwrap();

        let entry = move_in(&film, &[scratch.files()], &unprepared).unwrap();
        assert_eq!(entry, scratch.files().join("holiday film #1.mkv"));
        assert!(!film.exists());
        assert_eq!(std::fs::read(&entry).unwrap(), b"frames");
        let record = std::fs::read_to_string(
            scratch
                .0
                .join("data/Trash/info/holiday film #1.mkv.trashinfo"),
        )
        .unwrap();
        assert!(record.starts_with("[Trash Info]\nPath=/"), "{record}");
        assert!(
            record.contains("/home/Downloads/holiday%20film%20%231.mkv\n"),
            "{record}"
        );
        assert!(record.contains("\nDeletionDate=2"), "{record}");

        assert_eq!(restore(std::slice::from_ref(&entry)).unwrap(), 1);
        assert_eq!(std::fs::read(&film).unwrap(), b"frames");
        assert!(!entry.exists());
        assert_eq!(
            std::fs::read_dir(scratch.0.join("data/Trash/info"))
                .unwrap()
                .count(),
            0,
            "the record goes with the restore"
        );
        assert_eq!(
            restore(std::slice::from_ref(&entry)).unwrap(),
            0,
            "an entry no longer in Trash is not restored twice"
        );
    }

    #[test]
    fn a_taken_name_gets_a_numbered_one_and_restore_never_replaces() {
        let scratch = Scratch::new("names");
        let notes = scratch.downloads().join("notes.txt");
        std::fs::write(&notes, b"first").unwrap();
        let first = move_in(&notes, &[scratch.files()], &unprepared).unwrap();
        std::fs::write(&notes, b"second").unwrap();
        let second = move_in(&notes, &[scratch.files()], &unprepared).unwrap();
        assert_eq!(first, scratch.files().join("notes.txt"));
        assert_eq!(second, scratch.files().join("notes.txt.2"));

        std::fs::write(&notes, b"third").unwrap();
        let error = restore(std::slice::from_ref(&first)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&notes).unwrap(), b"third");
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
    }

    #[test]
    fn purge_and_empty_delete_items_and_their_records() {
        let scratch = Scratch::new("purge");
        let folder = scratch.downloads().join("album");
        std::fs::create_dir_all(folder.join("inner")).unwrap();
        std::fs::write(folder.join("inner/photo.png"), b"pixels").unwrap();
        let link = scratch.downloads().join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        let folder_entry = move_in(&folder, &[scratch.files()], &unprepared).unwrap();
        let link_entry = move_in(&link, &[scratch.files()], &unprepared).unwrap();
        assert!(
            std::fs::symlink_metadata(&link_entry)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a trashed link is the link"
        );

        assert_eq!(purge(std::slice::from_ref(&folder_entry)).unwrap(), 1);
        assert!(!folder_entry.exists());
        assert!(!scratch.0.join("data/Trash/info/album.trashinfo").exists());

        assert_eq!(empty(&scratch.files()).unwrap(), 1);
        assert_eq!(std::fs::read_dir(scratch.files()).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(scratch.0.join("data/Trash/info"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn a_relative_record_is_relative_to_the_folder_holding_the_trash() {
        let scratch = Scratch::new("relative");
        let entry = scratch.files().join("report.pdf");
        std::fs::write(&entry, b"pages").unwrap();
        std::fs::write(
            scratch.0.join("data/Trash/info/report.pdf.trashinfo"),
            "[Trash Info]\nPath=papers/report%20final.pdf\nDeletionDate=2026-10-22T00:20:53\n",
        )
        .unwrap();
        assert_eq!(restore(&[entry]).unwrap(), 1);
        assert_eq!(
            std::fs::read(scratch.0.join("data/papers/report final.pdf")).unwrap(),
            b"pages"
        );
    }

    #[test]
    fn paths_survive_the_records_encoding() {
        for path in ["/a/b c/ü€#%?.txt", "/plain/name-1_2.~x"] {
            let encoded = encode(OsStr::new(path));
            assert!(encoded.is_ascii());
            assert_eq!(decode(encoded.as_bytes()), path.as_bytes());
        }
        assert_eq!(decode(b"100%"), b"100%", "a stray percent sign is kept");
    }
}
