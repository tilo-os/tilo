//! The plain directory a workspace's folder is mounted over. What was written into it while
//! nothing was mounted there is the person's: it is taken into the workspace before the mount
//! covers it, never hidden underneath.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::fs::QuickFS;

/// What one entry underneath was when it was found: a file taken in is removed only as it was.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Found {
    dir: bool,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl Found {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dir: meta.is_dir(),
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }

    fn mtime_ms(self) -> u64 {
        self.modified
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

/// Moves what the plain directory `root` holds into the workspace `fs` shows, one way: each
/// folder and file written through the path the mount writes through, then removed from `root`.
/// `staging` is where each file is copied while it is written. Returns how many files were taken.
///
/// # Errors
///
/// A file could not be read or written into the workspace, one changed while it was taken in, or
/// `root` holds something no workspace takes (a symbolic link, a special file, a name that is not
/// UTF-8): it is named, and stays where it is.
pub fn take_in(fs: &QuickFS, root: &Path, staging: &Path) -> io::Result<usize> {
    let found = scan(root)?;
    if found.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut taken = 0_usize;
    // Parents before their children: a path sorts after every path it starts with.
    for (path, entry) in &found {
        let rel = Path::new(path);
        if entry.dir {
            fs.apply_local_dir(rel).map_err(crate::errno::error)?;
            continue;
        }
        let at = root.join(path);
        let written = fs::copy(&at, staging).and_then(|_| {
            if Found::of(&fs::symlink_metadata(&at)?) != *entry {
                return Err(changed(&at));
            }
            fs.write_path(rel, staging, entry.mtime_ms())
        });
        let _ = fs::remove_file(staging);
        written?;
        taken = taken.saturating_add(1);
    }
    // Children before their parents; a file only as it was taken in. A folder's own stamp changes
    // as its children leave it.
    for (path, entry) in found.iter().rev() {
        let at = root.join(path);
        let removed = if entry.dir {
            fs::remove_dir(&at)
        } else if Found::of(&fs::symlink_metadata(&at)?) != *entry {
            return Err(changed(&at));
        } else {
            fs::remove_file(&at)
        };
        removed.map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "{} is in the workspace now, and cannot be removed from the folder underneath it: {error}",
                    at.display()
                ),
            )
        })?;
    }
    Ok(taken)
}

/// Every folder and plain file below `root`, by `/`-separated path. Anything else is refused,
/// named: the mount would hide it.
fn scan(root: &Path) -> io::Result<Vec<(String, Found)>> {
    let mut out = Vec::new();
    let mut stack = vec![(PathBuf::from(root), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let at = entry.path();
            let meta = fs::symlink_metadata(&at)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err(untakeable(&at));
            };
            if !(meta.is_dir() || meta.is_file()) {
                return Err(untakeable(&at));
            }
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if meta.is_dir() {
                stack.push((at, path.clone()));
            }
            out.push((path, Found::of(&meta)));
        }
    }
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(out)
}

fn changed(at: &Path) -> io::Error {
    io::Error::other(format!(
        "{} changed while it was taken into the workspace",
        at.display()
    ))
}

fn untakeable(at: &Path) -> io::Error {
    io::Error::other(format!(
        "{} is in the folder before the workspace is shown there, and a workspace takes no such entry: move it out to show the folder",
        at.display()
    ))
}

/// The plain directory a folder is about to be mounted over, opened before the mount: what is
/// written into it after the mount's take-in looked, and before the mount covered it, is still
/// read through it.
pub struct Underneath(fs::File);

impl Underneath {
    /// Open the directory `root`, before anything is mounted over it.
    ///
    /// # Errors
    ///
    /// `root` could not be opened as a directory.
    pub fn open(root: &Path) -> io::Result<Self> {
        let dir = fs::File::open(root)?;
        if !dir.metadata()?.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a folder",
                root.display()
            )));
        }
        Ok(Self(dir))
    }

    /// Whether anything is in it now, mounted over or not.
    ///
    /// # Errors
    ///
    /// The directory could not be read.
    pub fn holds_anything(&self) -> io::Result<bool> {
        for entry in rustix::fs::Dir::read_from(&self.0)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
