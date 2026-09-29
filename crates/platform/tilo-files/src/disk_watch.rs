//! Watching native directories: inotify on Linux, `FSEvents` on macOS, through
//! `notify`. Only with the `watch` feature, so crates that persist through
//! this one without watching stay free of it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};

use crate::watch::{Change, ChangeKind, Inbox, Watch};
use crate::{FsError, VPath};

pub(crate) fn watch(root: &Path, path: &VPath) -> Result<Watch, FsError> {
    let target = crate::disk::resolve(root, path);
    let inbox = Arc::new(Inbox::default());
    let root = root.to_path_buf();
    let rescan = path.clone();
    let sink = Arc::clone(&inbox);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let Ok(event) = event else {
            sink.push(Change {
                path: rescan.clone(),
                kind: ChangeKind::Rescan,
            });
            return;
        };
        for (path, kind) in changes(&event) {
            if let Some(path) = relative(&root, &path) {
                sink.push(Change { path, kind });
            }
        }
    })
    .map_err(error)?;
    watcher
        .watch(&target, RecursiveMode::Recursive)
        .map_err(error)?;
    Ok(Watch::new(inbox, Box::new(watcher)))
}

fn error(error: notify::Error) -> FsError {
    match error.kind {
        notify::ErrorKind::PathNotFound => FsError::NotFound,
        notify::ErrorKind::Io(io) => io.into(),
        _ => FsError::Io(error.to_string()),
    }
}

/// The paths an event names and what happened to each. A write's hidden
/// temporary sibling is left out; the rename that publishes it reports the
/// real name.
fn changes(event: &Event) -> Vec<(PathBuf, ChangeKind)> {
    let settled = |path: &PathBuf| {
        if path.exists() {
            ChangeKind::Changed
        } else {
            ChangeKind::Removed
        }
    };
    let named: Vec<(PathBuf, ChangeKind)> = match event.kind {
        EventKind::Access(_) => Vec::new(),
        EventKind::Remove(_) => event
            .paths
            .iter()
            .map(|path| (path.clone(), ChangeKind::Removed))
            .collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            let mut paths = event.paths.iter();
            paths
                .next()
                .map(|from| (from.clone(), ChangeKind::Removed))
                .into_iter()
                .chain(paths.map(|to| (to.clone(), ChangeKind::Changed)))
                .collect()
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => event
            .paths
            .iter()
            .map(|path| (path.clone(), ChangeKind::Removed))
            .collect(),
        // FSEvents coalesces; whether a name exists now is the truth.
        _ => event
            .paths
            .iter()
            .map(|path| (path.clone(), settled(path)))
            .collect(),
    };
    named
        .into_iter()
        .filter(|(path, _)| !is_temporary(path))
        .collect()
}

/// `native::atomic`'s temporary names: `.<name>.<pid>.<sequence>.tmp`.
fn is_temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
        && path.extension() == Some(std::ffi::OsStr::new("tmp"))
}

/// `path` below `root` as a [`VPath`]; `None` outside it or for a name that
/// is not UTF-8. `FSEvents` reports canonical paths (`/private/var/...` for
/// `/var/...`), so a canonical root is tried too.
fn relative(root: &Path, path: &Path) -> Option<VPath> {
    let below: PathBuf = match path.strip_prefix(root) {
        Ok(below) => below.to_path_buf(),
        Err(_) => path
            .strip_prefix(root.canonicalize().ok()?)
            .ok()?
            .to_path_buf(),
    };
    let parts: Option<Vec<&str>> = below
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    VPath::new(&parts?.join("/")).ok()
}
