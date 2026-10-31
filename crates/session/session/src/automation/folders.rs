//! Watching the folders that file-changed triggers name.
//!
//! [`FolderWatch`] watches exactly the folders it was last given
//! ([`watched_folders`](super::triggers::watched_folders), resolved from each
//! trigger's capability) and reports what changed inside them since the last
//! look, as [`FolderChange`]s for the file-changed trigger source.
//!
//! A file is reported once, and only after it has stopped changing: dropping
//! one file into a folder makes the kernel announce its creation and then
//! every chunk written into it, so a shortcut watching the folder would
//! otherwise run again and again on a file it already has. A first sighting
//! of a file restarts its `SETTLE` timer; the change it is reported as is the
//! first one seen (a file that is written and then renamed into the folder is
//! there as a creation), and a change seen while a file settles keeps it
//! settling.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tilo_files::ChangeKind;
use tilo_files::native::{ChangeWatch, WatchEvent, Watcher};

use super::triggers::{FileChange, FolderChange, WatchedFolder};

/// How long a file must go untouched before the change is reported, so one
/// write of a file is one change.
pub const SETTLE: Duration = Duration::from_millis(1500);

/// A watcher over the folders file-changed triggers name.
#[derive(Default)]
pub struct FolderWatch {
    folders: Vec<WatchedFolder>,
    watcher: Option<(Vec<ChangeWatch>, mpsc::Receiver<WatchEvent>)>,
    /// Changes seen but not yet reported, and when each file was last seen
    /// changing.
    settling: BTreeMap<FolderChange, Instant>,
}

impl std::fmt::Debug for FolderWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderWatch")
            .field("folders", &self.folders)
            .field("settling", &self.settling.len())
            .finish_non_exhaustive()
    }
}

impl FolderWatch {
    /// Watch exactly `folders` from now on. A folder that cannot be watched
    /// (it does not exist, or the watch limit is reached) is skipped.
    pub fn watch(&mut self, folders: Vec<WatchedFolder>) {
        if folders == self.folders {
            return;
        }
        self.watcher = None;
        self.folders = folders;
        self.settling.clear();
        let paths: BTreeSet<&PathBuf> = self.folders.iter().map(|folder| &folder.path).collect();
        if paths.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let mut watches = Vec::new();
        for path in paths {
            let sender = sender.clone();
            let started = Watcher::tree(path).start(move |events| {
                for event in events {
                    let _ = sender.send(event.clone());
                }
            });
            match started {
                Ok(watch) => watches.push(watch),
                Err(error) => {
                    tracing::info!(%error, path = %path.display(), "a shortcut folder is not watched");
                }
            }
        }
        self.watcher = Some((watches, receiver));
    }

    /// Every file that changed inside a watched folder and has since
    /// settled, once per file: rewriting a file, or removing it and writing
    /// it again, reports the first change seen, so one file is one run.
    pub fn changes(&mut self) -> Vec<FolderChange> {
        self.changes_at(Instant::now())
    }

    /// [`Self::changes`], with `now` deciding which files have settled.
    pub fn changes_at(&mut self, now: Instant) -> Vec<FolderChange> {
        let mut seen = Vec::new();
        if let Some((_, receiver)) = self.watcher.as_ref() {
            while let Ok(event) = receiver.try_recv() {
                let Some(change) = file_change(event.kind) else {
                    continue;
                };
                for folder in &self.folders {
                    if let Some(found) = FolderChange::inside(folder, &event.path, change) {
                        seen.push(found);
                    }
                }
            }
        }
        for found in seen {
            self.settle(found, now);
        }
        let mut changes = Vec::new();
        self.settling.retain(|change, last_seen| {
            if now.saturating_duration_since(*last_seen) < SETTLE {
                return true;
            }
            changes.push(change.clone());
            false
        });
        changes.sort();
        changes
    }

    /// Record that `found` was seen changing at `now`, keeping the change a
    /// file is first reported as.
    fn settle(&mut self, found: FolderChange, now: Instant) {
        let seen = self
            .settling
            .iter_mut()
            .find(|(settling, _)| settling.same_file() == found.same_file());
        match seen {
            Some((_, at)) => *at = now,
            None => {
                self.settling.insert(found, now);
            }
        }
    }
}

/// What a change means for the file it names. A rescan names the folder
/// itself, which is no file of it.
fn file_change(kind: ChangeKind) -> Option<FileChange> {
    match kind {
        ChangeKind::Created => Some(FileChange::Created),
        ChangeKind::Removed => Some(FileChange::Removed),
        ChangeKind::Changed => Some(FileChange::Modified),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn a_file_written_in_a_watched_folder_is_reported_inside_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let watched = dir.path().join("inbox");
        std::fs::create_dir(&watched).expect("the watched folder");
        let mut watch = FolderWatch::default();
        watch.watch(vec![WatchedFolder {
            room: "room".into(),
            capability: "inbox-read".into(),
            path: watched.clone(),
        }]);
        std::fs::write(dir.path().join("outside.txt"), b"x").expect("a file outside");
        std::fs::write(watched.join("scan.pdf"), b"x").expect("a file inside");

        let deadline = tilo_test_env::hang_deadline();
        let mut seen = Vec::new();
        while !seen.iter().any(|change: &FolderChange| {
            change.file == "scan.pdf" && change.change == FileChange::Created
        }) {
            assert!(Instant::now() < deadline, "no change reported: {seen:?}");
            seen.extend(watch.changes());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            seen.iter()
                .all(|change| change.capability == "inbox-read" && change.room == "room"),
            "{seen:?}"
        );
        assert!(
            !seen.iter().any(|change| change.file.contains("outside")),
            "nothing outside the folder is reported: {seen:?}"
        );
        assert_eq!(
            seen.len(),
            1,
            "a file is reported once, however many writes it took: {seen:?}"
        );
    }

    #[test]
    fn a_file_is_reported_once_it_stops_changing() {
        let folder = WatchedFolder {
            room: "room".into(),
            capability: "inbox-read".into(),
            path: PathBuf::from("/var/workspace/inbox"),
        };
        let start = Instant::now();
        let write = |change| {
            FolderChange::inside(&folder, Path::new("/var/workspace/inbox/scan.pdf"), change)
                .expect("inside the folder")
        };
        let mut seen = Vec::new();
        let mut watch = FolderWatch::default();
        // A file written, and written again a moment later, settles from the
        // second write: the first one's window passing reports nothing.
        watch.settle(write(FileChange::Created), start);
        watch.settle(write(FileChange::Modified), start + SETTLE / 2);
        seen.extend(watch.changes_at(start + SETTLE));
        assert!(
            seen.is_empty(),
            "the file was written again half a settle before: {seen:?} {:?}",
            watch.settling
        );
        seen.extend(watch.changes_at(start + SETTLE * 2));
        assert_eq!(
            seen,
            [write(FileChange::Created)],
            "one report, as first seen, once it stops"
        );
        assert!(watch.changes_at(start + SETTLE * 4).is_empty(), "once only");

        // A file removed and written again is the same one file.
        let mut watch = FolderWatch::default();
        watch.settle(write(FileChange::Removed), start);
        watch.settle(write(FileChange::Created), start);
        assert_eq!(
            watch.changes_at(start + SETTLE),
            [write(FileChange::Removed)]
        );
        assert!(watch.settling.is_empty());
    }
}
