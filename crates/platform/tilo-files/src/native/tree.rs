//! A directory tree copied into a place of its own, file by file: each regular file
//! shares its source's extents where the file system can (`FICLONE`), and is copied
//! where it cannot; what lands is the same either way. The copy is of what the tree
//! holds and nothing it points at: a symbolic link is made again with the same target,
//! never followed, and a directory on another file system and a special file are left
//! out. Permissions and modification times come along, and the destination must not
//! exist yet. Each entry's place in the copy is the caller's to decide
//! ([`clone_tree`]'s `place`): kept, moved to another relative path, or left out, which
//! is how a copy renames what names its source.

use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// How a regular file is copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Share the source's extents where the file system can, and copy where it cannot.
    Clone,
    /// Copy the bytes, never sharing extents.
    Copy,
}

/// What a copy made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Copied {
    pub files: u64,
    pub cloned: u64,
    pub bytes: u64,
    pub dirs: u64,
    pub links: u64,
    /// Entries left out: special files, and directories on another file system.
    pub skipped: u64,
}

/// Copies the tree at `from` to `to`, which must not exist yet, with `strategy`. `place` names each
/// entry's relative path in the copy: `None` leaves the entry out, and a directory left out is left out
/// whole. `progress` hears what the copy made so far after every file.
pub fn clone_tree(
    from: &Path,
    to: &Path,
    strategy: Strategy,
    place: &mut dyn FnMut(&Path) -> Option<PathBuf>,
    progress: &mut dyn FnMut(&Copied),
) -> io::Result<Copied> {
    let root = fs::symlink_metadata(from)?;
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", from.display()),
        ));
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists already", to.display()),
        ));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut copied = Copied::default();
    let walk = Walk {
        from,
        to,
        strategy,
        device: device_of(&root),
    };
    walk.directory(
        Path::new(""),
        Path::new(""),
        &root,
        &mut copied,
        place,
        progress,
    )?;
    Ok(copied)
}

struct Walk<'a> {
    from: &'a Path,
    to: &'a Path,
    strategy: Strategy,
    /// The file system the source's root is on: a directory on another is a mount, left out.
    device: u64,
}

impl Walk<'_> {
    /// Makes `target` from `source`, then everything in it, so a directory's mode is set before its
    /// contents arrive.
    fn directory(
        &self,
        source: &Path,
        target: &Path,
        metadata: &fs::Metadata,
        copied: &mut Copied,
        place: &mut dyn FnMut(&Path) -> Option<PathBuf>,
        progress: &mut dyn FnMut(&Copied),
    ) -> io::Result<()> {
        let made = self.to.join(target);
        fs::create_dir_all(&made)?;
        copied.dirs = copied.dirs.saturating_add(1);
        let mut entries = fs::read_dir(self.from.join(source))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        // One order on every run, so a copy that stops partway stops at the same place.
        entries.sort();
        for name in entries {
            let source = source.join(&name);
            let Some(target) = place(&source) else {
                continue;
            };
            let path = self.from.join(&source);
            let entry = fs::symlink_metadata(&path)?;
            let kind = entry.file_type();
            if kind.is_dir() {
                if device_of(&entry) != self.device {
                    copied.skipped = copied.skipped.saturating_add(1);
                    continue;
                }
                self.directory(&source, &target, &entry, copied, place, progress)?;
            } else if kind.is_file() {
                let cloned = self.file(&path, &self.to.join(&target), &entry)?;
                copied.files = copied.files.saturating_add(1);
                copied.bytes = copied.bytes.saturating_add(entry.len());
                if cloned {
                    copied.cloned = copied.cloned.saturating_add(1);
                }
                progress(copied);
            } else if kind.is_symlink() {
                let link = fs::read_link(&path)?;
                make_parent(&self.to.join(&target))?;
                symlink(&link, &self.to.join(&target))?;
                copied.links = copied.links.saturating_add(1);
            } else {
                copied.skipped = copied.skipped.saturating_add(1);
            }
        }
        // Last, so the files made in it do not move its time on.
        keep_times_and_mode(&made, metadata)
    }

    fn file(&self, source: &Path, target: &Path, metadata: &fs::Metadata) -> io::Result<bool> {
        make_parent(target)?;
        let mut input = open_source(source)?;
        let mut output = create_target(target, metadata)?;
        let cloned = self.strategy == Strategy::Clone && share_extents(&input, &output);
        if !cloned {
            io::copy(&mut input, &mut output)?;
        }
        output.set_modified(metadata.modified()?)?;
        Ok(cloned)
    }
}

fn make_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn open_source(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_source(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn create_target(path: &Path, metadata: &fs::Metadata) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(metadata.permissions().mode() & 0o7777)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn create_target(path: &Path, _metadata: &fs::Metadata) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Whether `target` now shares `source`'s extents: `FICLONE`, where the file system has it.
#[cfg(target_os = "linux")]
fn share_extents(source: &File, target: &File) -> bool {
    rustix::fs::ioctl_ficlone(target, source).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn share_extents(_source: &File, _target: &File) -> bool {
    false
}

#[cfg(unix)]
fn symlink(link: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn symlink(_link: &Path, at: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{} is a link this system cannot make", at.display()),
    ))
}

#[cfg(unix)]
fn device_of(metadata: &fs::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(not(unix))]
fn device_of(_metadata: &fs::Metadata) -> u64 {
    0
}

fn keep_times_and_mode(directory: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    fs::set_permissions(directory, metadata.permissions())?;
    let modified = metadata.modified()?;
    File::open(directory)?.set_modified(modified)
}

/// Whether the file system holding `directory` shares extents between files (`FICLONE`), which
/// [`Strategy::Clone`] falls back from.
pub fn shares_extents(directory: &Path) -> io::Result<bool> {
    let probe = directory.join(format!(".tilo-clone-probe-{}", std::process::id()));
    let target = probe.with_extension("clone");
    fs::write(&probe, b"probe")?;
    let answer = (|| {
        let source = File::open(&probe)?;
        let made = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        Ok(share_extents(&source, &made))
    })();
    let _ = fs::remove_file(&probe);
    let _ = fs::remove_file(&target);
    answer
}
