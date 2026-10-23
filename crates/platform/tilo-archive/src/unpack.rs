//! Extracting an archive below one folder, and nowhere else.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::stream::{Flow, walk};
use crate::{Entry, Kind, Observer, cancelled, link_stays_inside, safe_relative, unsafe_name};

/// What an extraction wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    pub files: usize,
    pub directories: usize,
    /// Symbolic and hard links.
    pub links: usize,
    /// Device nodes, pipes and sockets the archive holds, never created;
    /// and, where the host has no symbolic links, the archive's links.
    pub skipped: usize,
    pub bytes: u64,
}

/// The largest read or write of a file's contents at once.
const CHUNK: usize = 1 << 20;

/// The most permission an extracted file or folder gets: `rwxr-xr-x`, never
/// setuid, setgid, sticky or writable by others.
const MODE_CAP: u32 = 0o755;

/// Extract the archive at `archive` into `root`, an empty folder the caller
/// made for it, and nowhere else.
///
/// Every entry lands below `root`:
/// - a name that is absolute or climbs out with `..` refuses the archive
///   ([`crate::is_unsafe`]), before anything of that entry is written;
/// - links are made last, after every file and folder, so nothing is ever
///   written through one; a symlink must point inside `root` without
///   following anything, and may not sit below another symlink; a hard link
///   must name a file extracted before it;
/// - device nodes, pipes and sockets are skipped;
/// - permissions keep at most `rwxr-xr-x`, and the owner can always read
///   and write what was extracted.
///
/// A later entry of the same name replaces an earlier file, as tar does.
/// On an error, what was written so far stays in `root` for the caller to
/// remove with it.
///
/// # Errors
///
/// As [`walk`]; an unsafe name or link ([`crate::is_unsafe`]); a file and a
/// folder of the same name; the observer's [`Observer::wrote`] error or
/// cancel ([`crate::is_cancelled`]); or the filesystem's error.
pub fn extract_into(archive: &Path, root: &Path, observer: &dyn Observer) -> io::Result<Extracted> {
    let mut done = Extracted::default();
    let mut directories: Vec<(PathBuf, Option<u32>)> = Vec::new();
    let mut hard_links: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut symlinks: Vec<(PathBuf, PathBuf)> = Vec::new();
    walk(archive, observer, &mut |entry, contents| {
        match entry.kind {
            Kind::Directory => {
                make_directory(root, &entry.path)?;
                directories.push((entry.path.clone(), entry.mode));
                done.directories = done.directories.saturating_add(1);
            }
            Kind::File => {
                let written = write_file(root, entry, contents, observer)?;
                done.files = done.files.saturating_add(1);
                done.bytes = done.bytes.saturating_add(written);
            }
            Kind::HardLink => {
                let target = entry
                    .link
                    .as_deref()
                    .map(safe_relative)
                    .transpose()?
                    .flatten()
                    .ok_or_else(|| unsafe_name(&entry.path))?;
                hard_links.push((entry.path.clone(), target));
            }
            Kind::Symlink => {
                let target = entry.link.clone().unwrap_or_default();
                if !link_stays_inside(&entry.path, &target) {
                    return Err(unsafe_name(&entry.path));
                }
                symlinks.push((entry.path.clone(), target));
            }
            Kind::Special => done.skipped = done.skipped.saturating_add(1),
        }
        Ok(Flow::Continue)
    })?;
    for (path, target) in hard_links {
        link_file(root, &path, &target, observer)?;
        done.links = done.links.saturating_add(1);
    }
    for (path, target) in symlinks {
        if make_symlink(root, &path, &target)? {
            done.links = done.links.saturating_add(1);
        } else {
            done.skipped = done.skipped.saturating_add(1);
        }
    }
    // Deepest first, so a folder made read-only does not stop its parent's
    // permissions being set.
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, mode) in directories {
        if let Some(mode) = mode {
            set_mode(&root.join(path), (mode & MODE_CAP) | 0o700)?;
        }
    }
    Ok(done)
}

/// Make the folder `path` below `root`. Only folders this extraction made
/// can be on the way: links come last.
fn make_directory(root: &Path, path: &Path) -> io::Result<()> {
    let full = root.join(path);
    match fs::symlink_metadata(&full) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(conflict(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(&full),
        Err(error) => Err(error),
    }
}

fn conflict(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "the archive holds both a file and a folder called “{}”",
            path.display()
        ),
    )
}

/// The parent folder of `path` below `root`, made when missing.
fn make_parent(root: &Path, path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => make_directory(root, parent),
        _ => Ok(()),
    }
}

/// Clear `path` below `root` for a new file or link: an earlier file or
/// link of that name goes, a folder refuses.
fn clear(root: &Path, path: &Path) -> io::Result<()> {
    let full = root.join(path);
    match fs::symlink_metadata(&full) {
        Ok(meta) if meta.is_dir() => Err(conflict(path)),
        Ok(_) => fs::remove_file(&full),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_file(
    root: &Path,
    entry: &Entry,
    contents: &mut dyn Read,
    observer: &dyn Observer,
) -> io::Result<u64> {
    make_parent(root, &entry.path)?;
    clear(root, &entry.path)?;
    let full = root.join(&entry.path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full)?;
    let written = copy(contents, &mut file, observer)?;
    if let Some(mode) = entry.mode {
        set_mode(&full, (mode & MODE_CAP) | 0o600)?;
    }
    if let Some(modified) = entry.modified {
        // A time the filesystem cannot keep is not worth failing over.
        let _ = file.set_modified(modified);
    }
    Ok(written)
}

/// Copy `from` to `to` in chunks, telling the observer and stopping when it
/// asks.
fn copy(from: &mut dyn Read, to: &mut File, observer: &dyn Observer) -> io::Result<u64> {
    let mut buffer = vec![0_u8; CHUNK];
    let mut copied = 0_u64;
    loop {
        if observer.cancelled() {
            return Err(cancelled());
        }
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok(copied),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        observer.wrote(read as u64)?;
        to.write_all(buffer.get(..read).unwrap_or_default())?;
        copied = copied.saturating_add(read as u64);
    }
}

/// Make `path` a second name for the file `target` extracted before it; a
/// copy where the filesystem cannot link.
fn link_file(root: &Path, path: &Path, target: &Path, observer: &dyn Observer) -> io::Result<()> {
    let source = root.join(target);
    if !fs::symlink_metadata(&source)?.is_file() {
        return Err(unsafe_name(path));
    }
    make_parent(root, path)?;
    clear(root, path)?;
    let full = root.join(path);
    if fs::hard_link(&source, &full).is_ok() {
        return Ok(());
    }
    let mut from = File::open(&source)?;
    let mut to = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full)?;
    copy(&mut from, &mut to, observer).map(drop)
}

/// Make the symlink `path` to `target`, already known to point inside;
/// `false` where the host has none. A symlink below another symlink is
/// refused: its target was checked from where its name says, not from where
/// the outer link would put it.
fn make_symlink(root: &Path, path: &Path, target: &Path) -> io::Result<bool> {
    let mut walked = PathBuf::new();
    if let Some(parent) = path.parent() {
        for part in parent.components() {
            walked.push(part);
            match fs::symlink_metadata(root.join(&walked)) {
                Ok(meta) if meta.is_dir() => {}
                Ok(meta) if meta.file_type().is_symlink() => return Err(unsafe_name(path)),
                Ok(_) => return Err(conflict(&walked)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(root.join(&walked))?;
                }
                Err(error) => return Err(error),
            }
        }
    }
    clear(root, path)?;
    symlink(target, &root.join(path))
}

#[cfg(unix)]
fn symlink(target: &Path, at: &Path) -> io::Result<bool> {
    std::os::unix::fs::symlink(target, at).map(|()| true)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _at: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}
