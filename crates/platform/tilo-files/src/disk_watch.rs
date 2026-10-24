//! Watching native directories: inotify on Linux, `FSEvents` on macOS, through
//! `notify`. Only with the `watch` feature, so crates that persist through
//! this one without watching stay free of it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    FUSE_SUPER_MAGIC,
    0x0000_6969,
    0x0000_517B,
    0xFF53_4D42,
    0xFE53_4D42,
    0x0102_1997,
    0x00C3_6400,
    0x6B41_4653,
    0x5346_414F,
];

/// `statfs` magic of FUSE.
#[cfg(target_os = "linux")]
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// [`crate::native::watch_sees_every_change`].
#[cfg(target_os = "linux")]
pub(crate) fn sees_every_change(dir: &Path) -> bool {
    let Ok(stat) = rustix::fs::statfs(dir) else {
        return false;
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
    )]
    let magic = stat.f_type as u32;
    if magic == FUSE_SUPER_MAGIC {
        return signals_every_change(dir);
    }
    !REMOTE_FILESYSTEMS.contains(&magic)
}

/// Whether the FUSE filesystem holding `dir` says it tells the kernel of
/// every change, by answering [`crate::native::CHANGE_SIGNAL_XATTR`] there.
#[cfg(target_os = "linux")]
fn signals_every_change(dir: &Path) -> bool {
    let mut value = [0_u8; 8];
    rustix::fs::getxattr(dir, crate::native::CHANGE_SIGNAL_XATTR, &mut value[..])
        .is_ok_and(|len| value.get(..len) == Some(b"1".as_slice()))
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
    // Taken before the watch starts, so a change made between the two is
    // never mistaken for earlier history.
    #[cfg(target_os = "macos")]
    let started = Started::of(&target);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        #[cfg(target_os = "macos")]
        if started
            .as_ref()
            .is_some_and(|started| started.explains(&event))
        {
            return;
        }
        deliver(&sink, &root, &rescan, event);
    })
    .map_err(error)?;
    watcher.watch(&target, mode).map_err(error)?;
    Ok(Watch::new(inbox, Box::new(watcher)))
}

/// The watched directory as it was when its watch started.
///
/// `FSEvents` numbers an event when its daemon gets to it, not when the
/// change was made, so a stream started "from now" can still be handed what
/// happened to the watched directory just before: its own creation above
/// all, when it was made a moment earlier. Reported, that history would wake
/// the watch's owner for a change it has read already, since whoever starts
/// a watch reads the directory once after it.
///
/// So an event on the directory itself is dropped while the directory is
/// still the one that was watched, with the mode and owner it had then. A
/// removal, a replacement or a new mode or owner is reported. Changes to its
/// entries move its times too, but are reported by their own names, so the
/// times are not compared: a late report of the directory's making must not
/// pass as a change because an entry was created since. The one change to
/// the directory itself left unreported is to its extended attributes,
/// which only a Linux FUSE mount uses as a signal
/// ([`crate::native::CHANGE_SIGNAL_XATTR`]), and inotify reports only what
/// happens after its watch is added.
#[cfg(target_os = "macos")]
struct Started {
    /// The directory as watched.
    dir: PathBuf,
    /// The same, canonical, as `FSEvents` names it.
    canonical: PathBuf,
    identity: Identity,
}

#[cfg(target_os = "macos")]
impl Started {
    fn of(dir: &Path) -> Option<Self> {
        Some(Self {
            identity: Identity::of(dir)?,
            dir: dir.to_path_buf(),
            canonical: dir.canonicalize().ok()?,
        })
    }

    /// Whether `event` reports only what the directory went through before
    /// its watch started.
    fn explains(&self, event: &notify::Result<Event>) -> bool {
        let Ok(event) = event else {
            return false;
        };
        !event.need_rescan()
            && !event.paths.is_empty()
            && event
                .paths
                .iter()
                .all(|path| *path == self.dir || *path == self.canonical)
            && Identity::of(&self.dir).as_ref() == Some(&self.identity)
    }
}

/// Which directory a path names, and who may do what in it.
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    born: Option<std::time::SystemTime>,
    mode: u32,
    owner: (u32, u32),
}

#[cfg(target_os = "macos")]
impl Identity {
    fn of(dir: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata(dir).ok()?;
        Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            born: metadata.created().ok(),
            mode: metadata.mode(),
            owner: (metadata.uid(), metadata.gid()),
        })
    }
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

/// The paths an event names and what each is now. A write's hidden
/// temporary sibling is left out; the rename that publishes it reports the
/// real name.
///
/// Every kind is read from the filesystem when the event is delivered, not
/// from the event itself. Events arrive after the fact, and under load long
/// after: by the time a write's rename is delivered the file may already be
/// removed again. Mixing the two views reorders history: a rename looked up
/// late says `Removed`, and the next event of the same burst, taken at its
/// word, says `Changed` after it, for a file that is gone. Read at delivery,
/// each change is the path's state at a later moment than the one before
/// it, and any change after the last look raises another event, so the last
/// change a watch reports for a path is what the path is. `FSEvents`, which
/// coalesces and whose flags cannot be taken at their word either, needs
/// the same.
fn changes(event: &Event) -> Vec<(PathBuf, ChangeKind)> {
    if matches!(event.kind, EventKind::Access(_)) {
        return Vec::new();
    }
    event
        .paths
        .iter()
        .filter(|path| !is_temporary(path))
        .map(|path| (path.clone(), state_now(path)))
        .collect()
}

/// Whether `path` names an entry now. A dangling symlink is still an entry,
/// and a name that cannot be looked up for any other reason may have changed.
fn state_now(path: &Path) -> ChangeKind {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ChangeKind::Removed,
        _ => ChangeKind::Changed,
    }
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

    fn drain(inbox: Arc<Inbox>) -> Vec<Change> {
        let watch = Watch::new(inbox, Box::new(()));
        std::iter::from_fn(|| watch.try_next()).collect()
    }

    fn event(kind: EventKind, paths: &[&Path]) -> Event {
        paths.iter().fold(Event::new(kind), |event, path| {
            event.add_path(path.to_path_buf())
        })
    }

    /// Two atomic writes and a removal, all done before the watch thread got
    /// to the first event, as happens under load. The rename that published
    /// each write is delivered after the file is gone; it must not say the
    /// file changed after an earlier event already said it was removed.
    #[test]
    fn a_file_removed_before_its_writes_are_delivered_is_never_reported_changed() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = root.join("a.md");
        let inbox = Arc::new(Inbox::default());
        let watched = VPath::root();
        for seq in 0..2 {
            let temporary = root.join(format!(".a.md.42.{seq}.tmp"));
            let burst = [
                event(EventKind::Create(CreateKind::File), &[&temporary]),
                event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                    &[&temporary],
                ),
                event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                    &[&file],
                ),
                event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                    &[&temporary, &file],
                ),
            ];
            for native in burst {
                deliver(&inbox, root, &watched, Ok(native));
            }
        }
        deliver(
            &inbox,
            root,
            &watched,
            Ok(event(EventKind::Remove(RemoveKind::File), &[&file])),
        );
        assert_eq!(
            drain(inbox),
            vec![Change {
                path: VPath::new("a.md").unwrap(),
                kind: ChangeKind::Removed
            }]
        );
    }

    /// The other way round: a removal delivered after the name was created
    /// again reports the file that is there, so the last change is the truth.
    #[test]
    fn a_removal_delivered_after_the_name_returned_reports_the_file() {
        use notify::event::RemoveKind;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        std::fs::write(&file, b"back").unwrap();
        let inbox = Arc::new(Inbox::default());
        deliver(
            &inbox,
            dir.path(),
            &VPath::root(),
            Ok(event(EventKind::Remove(RemoveKind::File), &[&file])),
        );
        assert_eq!(
            drain(inbox),
            vec![Change {
                path: VPath::new("a.md").unwrap(),
                kind: ChangeKind::Changed
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
        assert_eq!(state_now(&link), ChangeKind::Changed);
        assert_eq!(state_now(&dir.path().join("missing")), ChangeKind::Removed);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_watch_on_a_local_directory_sees_every_change() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sees_every_change(dir.path()));
        assert!(!sees_every_change(&dir.path().join("missing")));
    }

    /// Every change `watch` reports up to and including the creation of
    /// `name` in `dir`, which it makes. A watch reports in the order the
    /// changes were made, so anything left over from before the watch began
    /// is among them.
    fn changes_until_made(watch: &Watch, dir: &Path, name: &str) -> Vec<Change> {
        std::fs::create_dir(dir.join(name)).unwrap();
        let made = VPath::new(name).unwrap();
        let mut seen = Vec::new();
        tilo_test_env::wait_until("the watch to report the made entry", || {
            while let Some(change) = watch.try_next() {
                seen.push(change);
            }
            seen.iter().any(|change| change.path == made)
        });
        seen
    }

    /// `FSEvents` can hand a watch the creation of the directory it watches
    /// when the directory was made a moment before: the event is numbered
    /// when its daemon gets to it. That is no change since the watch began.
    #[test]
    fn a_watch_on_a_just_made_directory_reports_nothing_of_its_making() {
        for _ in 0..50 {
            let dir = tempfile::tempdir().unwrap();
            let watch = watch_entries(dir.path()).unwrap();
            let seen = changes_until_made(&watch, dir.path(), "made");
            assert!(seen.iter().all(|change| !change.path.is_root()), "{seen:?}");
        }
    }

    /// What the directory itself goes through after the watch began is
    /// reported, as a change of the watched path.
    #[cfg(unix)]
    #[test]
    fn a_change_to_the_watched_directory_itself_is_reported() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let watch = watch_entries(dir.path()).unwrap();
        // Past any report of the directory's making.
        let _ = changes_until_made(&watch, dir.path(), "made");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        tilo_test_env::wait_until("the watch to report the directory's new mode", || {
            std::iter::from_fn(|| watch.try_next()).any(|change| change.path.is_root())
        });
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
        tilo_test_env::wait_until("the replaced file to be watched", || {
            while let Some(change) = watch.try_next() {
                seen.push(change);
            }
            seen.iter().any(|change| change.path == top)
        });
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
