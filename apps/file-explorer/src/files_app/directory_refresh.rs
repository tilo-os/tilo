use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::{Entry, FileExplorerWidget, fileops, read_dir_sync, sort_entries};

/// A listing read that touches no widget state, so it can run on any thread.
pub(super) type DirectoryRead = Box<dyn FnOnce() -> Result<Vec<Entry>, String> + Send>;

/// Re-reads the folder shown on a thread of its own: the window may be
/// drawn by the compositor, whose loop must never wait on the disk.
pub(super) struct DirectoryRefresh {
    wake: Arc<dyn Fn() + Send + Sync>,
    job: Option<Job>,
}

struct Job {
    path: PathBuf,
    done: Receiver<Result<Vec<Entry>, String>>,
}

impl std::fmt::Debug for DirectoryRefresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectoryRefresh")
            .field("path", &self.job.as_ref().map(|job| &job.path))
            .finish_non_exhaustive()
    }
}

impl DirectoryRefresh {
    /// A refresh whose finished reads call `wake` (the window's wake handle)
    /// from the reading thread.
    pub(super) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
            job: None,
        }
    }

    pub(super) fn start(&mut self, path: &Path, read: DirectoryRead) {
        let (sender, done) = mpsc::sync_channel(1);
        let wake = Arc::clone(&self.wake);
        let spawned = std::thread::Builder::new()
            .name("files-refresh".into())
            .spawn(move || {
                if sender.send(read()).is_ok() {
                    wake();
                }
            });
        self.job = spawned.is_ok().then(|| Job {
            path: path.to_path_buf(),
            done,
        });
    }

    /// Forget the read under way: its answer is for a listing no longer shown.
    pub(super) fn stop(&mut self) {
        self.job = None;
    }

    pub(super) fn is_reading(&self) -> bool {
        self.job.is_some()
    }

    /// The finished read and the folder it listed, if one finished.
    fn take(&mut self) -> Option<(PathBuf, Result<Vec<Entry>, String>)> {
        let job = self.job.as_ref()?;
        let read = match job.done.try_recv() {
            Ok(read) => read,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the listing read stopped".to_owned()),
        };
        self.job.take().map(|job| (job.path, read))
    }

    /// Wait for the read under way: for a test, which checks one settled
    /// listing.
    #[cfg(test)]
    fn wait(&mut self) -> Option<(PathBuf, Result<Vec<Entry>, String>)> {
        let job = self.job.take()?;
        let read = job
            .done
            .recv()
            .unwrap_or_else(|_| Err("the listing read stopped".to_owned()));
        Some((job.path, read))
    }
}

impl FileExplorerWidget {
    /// How `path`'s listing is read, built from what the window knows now.
    pub(super) fn directory_reader(&self, path: &Path) -> DirectoryRead {
        if crate::recent::is_location(path) {
            let paths = self.recent_paths();
            return Box::new(move || Ok(super::open_with_recent::recent_entries_of(&paths)));
        }
        if crate::applications::is_location(path) {
            let entries = self.applications_entries();
            return Box::new(move || Ok(entries));
        }
        let path = path.to_path_buf();
        if self.hosted_clipboard() && fileops::is_trash_dir(&path) {
            return Box::new(move || {
                let mut entries = read_dir_sync(&path)?;
                if let Some(user_trash) = host_fs::trash::user_trash_dir()
                    && user_trash != path
                {
                    match std::fs::symlink_metadata(&user_trash) {
                        Ok(_) => entries.extend(read_dir_sync(&user_trash)?),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.to_string()),
                    }
                }
                Ok(entries)
            });
        }
        if self.startup_accounts.is_some() {
            let roots = self.workspace_roots.clone();
            Box::new(move || crate::model::read_dir_with_workspace_roots(&path, &roots))
        } else {
            Box::new(move || read_dir_sync(&path))
        }
    }

    pub(super) fn read_directory(&self, path: &Path) -> Result<Vec<Entry>, String> {
        self.directory_reader(path)()
    }

    /// Read the folder shown again, off the window's thread.
    pub(super) fn start_directory_refresh(&mut self) {
        let read = self.directory_reader(&self.current_path);
        self.directory_refresh.start(&self.current_path, read);
    }

    /// Show the folder's listing once its read finished; whether the view
    /// changed.
    pub(super) fn take_directory_refresh(&mut self) -> bool {
        let Some((path, read)) = self.directory_refresh.take() else {
            return false;
        };
        self.apply_directory_refresh(&path, read)
    }

    /// Wait for the listing read under way and show it; whether the view
    /// changed.
    #[cfg(test)]
    pub(super) fn settle_directory_refresh(&mut self) -> bool {
        let Some((path, read)) = self.directory_refresh.wait() else {
            return false;
        };
        self.apply_directory_refresh(&path, read)
    }

    fn apply_directory_refresh(&mut self, path: &Path, read: Result<Vec<Entry>, String>) -> bool {
        if path != self.current_path {
            return false;
        }
        if self.renaming.is_some() || self.pin_drag.is_some() {
            // Replacing the rows under a rename or a drag would move them;
            // the listing is read again once it ends.
            self.directory_changed = true;
            return false;
        }
        // A transient unmount or reconnect must not replace a still-useful
        // view with an error page. Explicit Refresh remains the path that
        // surfaces directory errors to the user.
        read.is_ok_and(|entries| self.show_refreshed_entries(entries))
    }

    /// Show `entries` as the folder's listing while preserving stable-record
    /// selection, scroll and operation status; whether the view changed.
    pub(super) fn show_refreshed_entries(&mut self, mut entries: Vec<Entry>) -> bool {
        let selected = self
            .selection
            .selected()
            .into_iter()
            .filter_map(|index| self.entries.get(index))
            .map(|entry| (entry.record_id, entry.path.clone()))
            .collect::<Vec<_>>();
        let primary = self
            .selection
            .primary()
            .and_then(|index| self.entries.get(index))
            .map(|entry| (entry.record_id, entry.path.clone()));
        if !self.show_hidden {
            entries.retain(|entry| !entry.name.starts_with('.'));
        }
        if !self.is_recent_view() {
            sort_entries(self.sort_by, self.sort_order, &mut entries);
        }
        if entries == self.entries {
            return false;
        }
        let matches_key = |entry: &Entry, key: &(Option<[u8; 16]>, PathBuf)| {
            key.0.map_or_else(
                || entry.path == key.1,
                |record| entry.record_id == Some(record),
            )
        };
        let selected_indices = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                selected
                    .iter()
                    .any(|key| matches_key(entry, key))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let primary_index = primary
            .as_ref()
            .and_then(|key| entries.iter().position(|entry| matches_key(entry, key)));
        self.entries = entries;
        self.selection
            .replace_indices(selected_indices, primary_index);
        self.retain_shown_selection();
        self.structure_dirty = true;
        true
    }
}
