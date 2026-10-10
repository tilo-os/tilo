//! Re-reading a file when it changes, instead of on a timer.
//!
//! The capability list a mount enforces is re-read while the mount runs, so
//! a revocation takes effect without a remount. Polling it once a second
//! woke an idle daemon twice a second (the list and the workspace
//! authority built from it). [`ReloadTrigger`] waits for the host's file
//! events instead (`inotify` on Linux, `FSEvents` on macOS): a change is
//! seen in milliseconds, and an unchanged file costs nothing.
//!
//! The parent directory is watched, not the file: a producer that renames
//! a new file over the old one, deletes it or creates it again replaces
//! the inode a file watch would be bound to. The reader's own opens and
//! reads are file events too, so access events are ignored, or every
//! re-read would schedule the next one.
//!
//! Two fallbacks keep the revocation bound when events cannot be trusted:
//!
//! * If the watch cannot be set up at all (no inotify instances left, an
//!   unsupported filesystem), the trigger polls at the old interval.
//! * A long backstop re-reads and re-arms the watch anyway, for anything
//!   the kernel did not announce (the watched directory itself replaced).
//!   A lost event queue (`IN_Q_OVERFLOW`) is announced, and wakes at once.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::sync::watch;

/// How a reloader waits between two reads of its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReloadCadence {
    /// The poll interval when the file cannot be watched: today's bound.
    pub poll: Duration,
    /// How often a watched file is read and re-armed without an event.
    pub backstop: Duration,
}

impl ReloadCadence {
    /// The production cadence: poll every `poll` if the file cannot be
    /// watched, else read on each change and every [`WATCH_BACKSTOP`].
    #[must_use]
    pub const fn new(poll: Duration) -> Self {
        Self {
            poll,
            backstop: WATCH_BACKSTOP,
        }
    }
}

/// How long a watched file goes without a re-read when no event arrives.
/// Not the revocation bound — a change is read when its event arrives —
/// but the bound for a change the kernel did not report.
pub const WATCH_BACKSTOP: Duration = Duration::from_secs(60);

/// Wakes on each change of one file, from the host's file events.
pub struct FileWatch {
    watcher: Mutex<notify::RecommendedWatcher>,
    directory: PathBuf,
    changes: watch::Receiver<u64>,
}

impl std::fmt::Debug for FileWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatch")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

impl FileWatch {
    /// Watch `path` through its parent directory.
    ///
    /// # Errors
    ///
    /// The path has no file name, or the host's file events cannot be
    /// subscribed to for its directory.
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let name = path
            .file_name()
            .map(OsString::from)
            .ok_or_else(|| std::io::Error::other(format!("{} names no file", path.display())))?;
        let directory = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let canonical = std::fs::canonicalize(&directory).unwrap_or_else(|_| directory.clone());
        let (publish, changes) = watch::channel(0_u64);
        let paths = (directory.clone(), canonical);
        let mut events = notify::recommended_watcher(move |event| {
            if concerns(&event, &name, &paths) {
                publish.send_modify(|count| *count = count.wrapping_add(1));
            }
        })
        .map_err(std::io::Error::other)?;
        events
            .watch(&directory, RecursiveMode::NonRecursive)
            .map_err(std::io::Error::other)?;
        Ok(Self {
            watcher: Mutex::new(events),
            directory,
            changes,
        })
    }

    /// A receiver that sees every change from now on.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        let mut changes = self.changes.clone();
        changes.mark_unchanged();
        changes
    }

    /// Watch the directory again: a directory that was removed and created
    /// again is a new inode the old watch no longer follows.
    ///
    /// # Errors
    ///
    /// The directory cannot be watched (it is gone right now).
    pub fn rearm(&self) -> std::io::Result<()> {
        let mut watcher = self
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = watcher.unwatch(&self.directory);
        watcher
            .watch(&self.directory, RecursiveMode::NonRecursive)
            .map_err(std::io::Error::other)
    }
}

/// Whether `event` may mean the watched file changed. Anything uncertain
/// (an error, a lost queue, an event without paths) counts as a change:
/// a needless re-read costs a `stat`, a missed one a stale ACL.
fn concerns(
    event: &notify::Result<notify::Event>,
    name: &std::ffi::OsStr,
    (directory, canonical): &(PathBuf, PathBuf),
) -> bool {
    let Ok(event) = event else {
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    match event.kind {
        // A writer closing the file is the end of a rewrite.
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => {}
        // Opens and reads, the reloader's own included, change nothing.
        EventKind::Access(_) => return false,
        _ => {}
    }
    if event.paths.is_empty() || matches!(event.kind, EventKind::Any | EventKind::Other) {
        return true;
    }
    event
        .paths
        .iter()
        .any(|path| path.file_name() == Some(name) || path == directory || path == canonical)
}

/// What a reloader waits on between reads: file events with a backstop, or
/// a poll when the file cannot be watched.
#[derive(Debug)]
pub struct ReloadTrigger {
    cadence: ReloadCadence,
    mode: Mode,
    /// The first [`Self::next`] returns at once: a change between the
    /// caller's first read and the watch going up must not wait for the
    /// backstop.
    primed: bool,
}

#[derive(Debug)]
enum Mode {
    Watch {
        watch: FileWatch,
        changes: watch::Receiver<u64>,
    },
    Poll,
}

impl ReloadTrigger {
    /// Watch `path`, or poll it every `cadence.poll` if it cannot be
    /// watched (logged, since the daemon then wakes on every poll).
    #[must_use]
    pub fn for_file(path: &Path, cadence: ReloadCadence) -> Self {
        let mode = match FileWatch::new(path) {
            Ok(watch) => {
                let changes = watch.subscribe();
                Mode::Watch { watch, changes }
            }
            Err(error) => {
                eprintln!(
                    "{} cannot be watched ({error}); re-reading it every {} ms instead",
                    path.display(),
                    cadence.poll.as_millis()
                );
                Mode::Poll
            }
        };
        Self {
            cadence,
            mode,
            primed: true,
        }
    }

    /// Whether changes arrive as file events (rather than by polling).
    #[must_use]
    pub const fn is_watching(&self) -> bool {
        matches!(self.mode, Mode::Watch { .. })
    }

    /// Wait until the file may have changed.
    pub async fn next(&mut self) {
        if std::mem::take(&mut self.primed) {
            return;
        }
        let lost = match &mut self.mode {
            Mode::Poll => {
                tokio::time::sleep(self.cadence.poll).await;
                false
            }
            Mode::Watch { watch, changes } => {
                tokio::select! {
                    changed = changes.changed() => changed.is_err(),
                    () = tokio::time::sleep(self.cadence.backstop) => {
                        if let Err(error) = watch.rearm() {
                            eprintln!(
                                "{} could not be watched again: {error}",
                                watch.directory.display()
                            );
                        }
                        false
                    }
                }
            }
        };
        if lost {
            // The event thread is gone, and with it every later change.
            eprintln!("a file watch ended; polling instead");
            self.mode = Mode::Poll;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, path: &Path) -> notify::Event {
        notify::Event::new(kind).add_path(path.to_path_buf())
    }

    /// Only events that can mean the watched file changed wake the
    /// reloader: never its own reads, never a sibling file.
    #[test]
    fn only_changes_to_the_file_concern_its_watch() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        let directory = PathBuf::from("/var/lib/tilo");
        let watched = (directory.clone(), directory.clone());
        let name = std::ffi::OsStr::new("fs-capabilities.json");
        let file = directory.join("fs-capabilities.json");
        let sibling = directory.join("other.json");
        let concerns = |event: notify::Result<notify::Event>| concerns(&event, name, &watched);
        let about = |kind, path: &Path| concerns(Ok(event(kind, path)));
        assert!(about(EventKind::Modify(ModifyKind::Any), &file));
        assert!(about(EventKind::Create(CreateKind::File), &file));
        assert!(about(EventKind::Remove(RemoveKind::File), &file));
        assert!(
            about(EventKind::Remove(RemoveKind::Folder), &directory),
            "the directory going away may take the file with it"
        );
        assert!(
            !about(EventKind::Access(AccessKind::Any), &file),
            "the reloader's own read must not schedule another"
        );
        assert!(!about(
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            &file
        ));
        assert!(
            about(
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
                &file
            ),
            "a writer closing the file finished a rewrite"
        );
        assert!(!about(EventKind::Modify(ModifyKind::Any), &sibling));
        assert!(
            concerns(Err(notify::Error::generic("queue lost"))),
            "an error may hide a change"
        );
        assert!(concerns(Ok(
            notify::Event::new(EventKind::Any).set_flag(notify::event::Flag::Rescan)
        )));
    }

    /// A trigger that cannot watch its file polls at the old interval.
    #[tokio::test(start_paused = true)]
    async fn an_unwatchable_file_is_polled() {
        let cadence = ReloadCadence {
            poll: Duration::from_secs(1),
            backstop: Duration::from_secs(3600),
        };
        let mut trigger = ReloadTrigger::for_file(
            Path::new("/nonexistent-tilo-dir/fs-capabilities.json"),
            cadence,
        );
        assert!(!trigger.is_watching());
        let started = tokio::time::Instant::now();
        trigger.next().await;
        assert_eq!(started.elapsed(), Duration::ZERO, "the first wait is none");
        trigger.next().await;
        assert_eq!(started.elapsed(), Duration::from_secs(1));
    }
}
