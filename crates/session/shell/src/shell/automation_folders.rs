//! Watching the folders that file-changed triggers name.
//!
//! [`FolderWatch`] watches exactly the folders it was last given
//! ([`watched_folders`](crate::shell::automation_triggers::watched_folders),
//! resolved from each trigger's capability) and reports what changed inside
//! them since the last look, as
//! [`FolderChange`]s for the file-changed trigger source.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;

use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::shell::automation_triggers::{FileChange, FolderChange, WatchedFolder};

/// A watcher over the folders file-changed triggers name.
#[derive(Default)]
pub struct FolderWatch {
    folders: Vec<WatchedFolder>,
    watcher: Option<(RecommendedWatcher, mpsc::Receiver<notify::Event>)>,
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
        let paths: BTreeSet<&PathBuf> = self.folders.iter().map(|folder| &folder.path).collect();
        if paths.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                let _ = sender.send(event);
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                tracing::warn!(%error, "shortcut folders cannot be watched");
                return;
            }
        };
        for path in paths {
            if let Err(error) = watcher.watch(path, RecursiveMode::Recursive) {
                tracing::info!(%error, path = %path.display(), "a shortcut folder is not watched");
            }
        }
        self.watcher = Some((watcher, receiver));
    }

    /// Every file changed inside a watched folder since the last look, once
    /// per file and kind of change.
    pub fn changes(&mut self) -> Vec<FolderChange> {
        let Some((_, receiver)) = &self.watcher else {
            return Vec::new();
        };
        let mut changes: Vec<FolderChange> = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            let Some(change) = file_change(event.kind) else {
                continue;
            };
            for path in &event.paths {
                for folder in &self.folders {
                    if let Some(found) = FolderChange::inside(folder, path, change)
                        && !changes.contains(&found)
                    {
                        changes.push(found);
                    }
                }
            }
        }
        changes
    }
}

/// What a filesystem event means for the files it names, when it changes
/// them.
fn file_change(kind: EventKind) -> Option<FileChange> {
    match kind {
        EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            Some(FileChange::Created)
        }
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            Some(FileChange::Removed)
        }
        EventKind::Modify(ModifyKind::Metadata(_))
        | EventKind::Access(_)
        | EventKind::Any
        | EventKind::Other => None,
        EventKind::Modify(_) => Some(FileChange::Modified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
}
