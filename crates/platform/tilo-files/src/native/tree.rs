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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    /// A tree with a little of everything a workspace's files hold.
    fn tree() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("a tree");
        let at = root.path();
        fs::create_dir_all(at.join("src/deep")).unwrap();
        fs::write(at.join("README.md"), b"# Plan\n").unwrap();
        fs::write(at.join("src/main.rs"), b"fn main() {}\n").unwrap();
        fs::write(at.join("src/deep/data.bin"), vec![7_u8; 1 << 20]).unwrap();
        fs::write(at.join("run.sh"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(at.join("run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink("src/main.rs", at.join("main")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", at.join("outside")).unwrap();
        std::os::unix::net::UnixListener::bind(at.join("socket")).unwrap();
        root
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "a place, as `clone_tree` asks of every entry"
    )]
    fn keep(path: &Path) -> Option<PathBuf> {
        Some(path.to_path_buf())
    }

    fn same(from: &Path, to: &Path) {
        for file in ["README.md", "src/main.rs", "src/deep/data.bin", "run.sh"] {
            assert_eq!(
                fs::read(from.join(file)).unwrap(),
                fs::read(to.join(file)).unwrap(),
                "{file} holds what its source holds"
            );
            assert_eq!(
                fs::metadata(from.join(file)).unwrap().modified().unwrap(),
                fs::metadata(to.join(file)).unwrap().modified().unwrap(),
                "{file} keeps its time"
            );
        }
        assert_eq!(
            fs::metadata(to.join("run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            fs::read_link(to.join("main")).unwrap(),
            Path::new("src/main.rs")
        );
        assert_eq!(
            fs::read_link(to.join("outside")).unwrap(),
            Path::new("/etc/hostname"),
            "a link is made again, never followed"
        );
        assert!(
            fs::symlink_metadata(to.join("socket")).is_err(),
            "a socket is no file to copy"
        );
    }

    #[test]
    fn a_copy_holds_what_the_tree_holds_and_shares_nothing_with_it() {
        let source = tree();
        let into = tempfile::tempdir().unwrap();
        let to = into.path().join("copy");
        let copied =
            clone_tree(source.path(), &to, Strategy::Copy, &mut keep, &mut |_| {}).unwrap();
        same(source.path(), &to);
        assert_eq!(
            (copied.files, copied.cloned, copied.links, copied.skipped),
            (4, 0, 2, 1)
        );
        assert_eq!(copied.bytes, 7 + 13 + (1 << 20) + 10);
        // The copy is a tree of its own: changing it leaves the source as it was.
        fs::write(to.join("README.md"), b"# Changed\n").unwrap();
        assert_eq!(
            fs::read(source.path().join("README.md")).unwrap(),
            b"# Plan\n"
        );
    }

    /// Where the file system shares extents, every file of the copy does; where it does not, the clone
    /// falls back to copying and the copy is the same. Both are the one answer a caller gets.
    #[test]
    fn a_clone_shares_extents_where_it_can_and_copies_where_it_cannot() {
        let source = tree();
        let to = source.path().with_extension("clone");
        let sharing = shares_extents(source.path()).unwrap();
        let mut heard = 0;
        let copied = clone_tree(source.path(), &to, Strategy::Clone, &mut keep, &mut |_| {
            heard += 1;
        })
        .unwrap();
        same(source.path(), &to);
        assert_eq!(copied.files, 4);
        assert_eq!(heard, 4, "progress after every file");
        if sharing {
            assert_eq!(copied.cloned, 4, "every file shares its source's extents");
        } else {
            assert_eq!(copied.cloned, 0, "nothing is shared here, so all is copied");
        }
        fs::write(to.join("src/main.rs"), b"changed").unwrap();
        assert_eq!(
            fs::read(source.path().join("src/main.rs")).unwrap(),
            b"fn main() {}\n",
            "a clone written to leaves its source"
        );
        fs::remove_dir_all(&to).unwrap();
    }

    #[test]
    fn the_caller_places_and_leaves_out_entries() {
        let source = tree();
        let into = tempfile::tempdir().unwrap();
        let to = into.path().join("copy");
        let copied = clone_tree(
            source.path(),
            &to,
            Strategy::Clone,
            &mut |path| {
                if path == Path::new("src/deep") {
                    return None;
                }
                Some(PathBuf::from(
                    path.to_string_lossy().replace("README", "NOTES"),
                ))
            },
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(fs::read(to.join("NOTES.md")).unwrap(), b"# Plan\n");
        assert!(!to.join("README.md").exists());
        assert!(!to.join("src/deep").exists(), "a directory left out whole");
        assert_eq!(copied.files, 3);
    }

    #[test]
    fn a_copy_never_mixes_into_what_exists() {
        let source = tree();
        let into = tempfile::tempdir().unwrap();
        let error = clone_tree(
            source.path(),
            into.path(),
            Strategy::Copy,
            &mut keep,
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }
}
