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
    watch_mode(root, path, RecursiveMode::Recursive)
}

/// Watches the entries directly in `dir`, not below its subdirectories:
/// [`crate::native::watch_entries`].
pub(crate) fn watch_entries(dir: &Path) -> Result<Watch, FsError> {
    watch_mode(dir, &VPath::root(), RecursiveMode::NonRecursive)
}

/// Filesystems whose entries can change without passing through this
/// kernel, by `statfs` magic: FUSE, NFS, SMB, CIFS, SMB2, 9P, Ceph and the
/// two AFS implementations.
#[cfg(target_os = "linux")]
const REMOTE_FILESYSTEMS: [u32; 9] = [
    0x6573_5546,
    0x0000_6969,
    0x0000_517B,
    0xFF53_4D42,
    0xFE53_4D42,
    0x0102_1997,
    0x00C3_6400,
    0x6B41_4653,
    0x5346_414F,
];

/// [`crate::native::watch_sees_every_change`].
#[cfg(target_os = "linux")]
pub(crate) fn sees_every_change(dir: &Path) -> bool {
    rustix::fs::statfs(dir).is_ok_and(|stat| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
        )]
        let magic = stat.f_type as u32;
        !REMOTE_FILESYSTEMS.contains(&magic)
    })
}

/// Elsewhere the filesystem's type is not told apart, so a watch is trusted
/// for local edits only.
#[cfg(not(target_os = "linux"))]
pub(crate) fn sees_every_change(_dir: &Path) -> bool {
    false
}

fn watch_mode(root: &Path, path: &VPath, mode: RecursiveMode) -> Result<Watch, FsError> {
    let target = crate::disk::resolve(root, path);
    let inbox = Arc::new(Inbox::default());
    let root = root.to_path_buf();
    let rescan = path.clone();
    let sink = Arc::clone(&inbox);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        deliver(&sink, &root, &rescan, event);
    })
    .map_err(error)?;
    watcher.watch(&target, mode).map_err(error)?;
    Ok(Watch::new(inbox, Box::new(watcher)))
}

/// Queue what one native event says changed. A failed event, and a native
/// queue overflow (inotify's `IN_Q_OVERFLOW`, `FSEvents`' dropped events),
/// both mean changes were missed: the watch reports a rescan of `rescan`, the
/// watched path. The overflow arrives as an event without paths, so it must
/// be recognised by its flag; otherwise it would vanish without a trace.
fn deliver(sink: &Inbox, root: &Path, rescan: &VPath, event: notify::Result<Event>) {
    let event = match event {
        Ok(event) if !event.need_rescan() => event,
        _ => {
            sink.push(Change {
                path: rescan.clone(),
                kind: ChangeKind::Rescan,
            });
            return;
        }
    };
    for (path, kind) in changes(&event) {
        if let Some(path) = relative(root, &path) {
            sink.push(Change { path, kind });
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::Flag;

    #[test]
    fn a_native_queue_overflow_asks_for_a_rescan() {
        let inbox = Arc::new(Inbox::default());
        let watched = VPath::new("w").unwrap();
        deliver(
            &inbox,
            Path::new("/root"),
            &watched,
            Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
        );
        let watch = Watch::new(inbox, Box::new(()));
        assert_eq!(
            watch.try_next(),
            Some(Change {
                path: watched,
                kind: ChangeKind::Rescan
            })
        );
        assert_eq!(watch.try_next(), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_watch_on_a_local_directory_sees_every_change() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sees_every_change(dir.path()));
        assert!(!sees_every_change(&dir.path().join("missing")));
    }

    #[test]
    fn an_entries_watch_sees_replaced_files_but_not_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let watch = watch_entries(dir.path()).unwrap();
        crate::native::atomic::write(&dir.path().join("sub/inner.json"), b"below").unwrap();
        crate::native::atomic::write(&dir.path().join("top.json"), b"one").unwrap();
        crate::native::atomic::write(&dir.path().join("top.json"), b"two").unwrap();
        let top = VPath::new("top.json").unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            while let Some(change) = watch.try_next() {
                seen.push(change);
            }
            if seen.iter().any(|change| change.path == top) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            seen.iter()
                .any(|change| change.path == top && change.kind == ChangeKind::Changed),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .all(|change| !change.path.as_str().starts_with("sub/")),
            "{seen:?}"
        );
    }
}
