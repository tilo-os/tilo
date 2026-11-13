//! Reading the folder a window shows, off the window's thread where the read can
//! block.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ui::lock::MutexRecover as _;

use super::archives::archive_rows;
use crate::fileops;
use crate::i18n::{arg, t, t_args};
use crate::model::{Entry, read_dir_sync, sort_entries};

#[derive(Debug)]
pub(super) enum Purpose {
    Show(Then),
    Update,
}

#[derive(Debug, Default)]
pub(super) enum Then {
    #[default]
    Nothing,
    Select(PathBuf),
    Rename(PathBuf),
}

pub(super) enum Source {
    Recent(Vec<PathBuf>),
    Listed(Vec<Entry>),
    HostedTrash,
    WorkspaceRoots(Vec<PathBuf>),
    Plain,
}

pub(super) enum Watched {
    Kept,
    Nothing,
    Started(Option<crate::directory_watch::StartedWatch>),
}

pub(super) struct FolderRead {
    pub(super) path: PathBuf,
    pub(super) archive: Option<Archive>,
    pub(super) shown_folder: Option<Arc<host_fs::RetainedObject>>,
    pub(super) watched: Watched,
    pub(super) listing: Result<Vec<Entry>, String>,
    pub(super) is_folder: bool,
}

pub(super) struct Archive {
    pub(super) file: PathBuf,
    pub(super) prefix: PathBuf,
    pub(super) cached: Option<Arc<Vec<crate::archive::Entry>>>,
}

pub(super) struct Job {
    pub(super) path: PathBuf,
    pub(super) source: Source,
    pub(super) watching: Option<PathBuf>,
    pub(super) entries_only: bool,
}

impl Job {
    fn run(self) -> FolderRead {
        let path = self.path;
        if self.entries_only {
            let listing = read_entries(&path, &self.source);
            return FolderRead {
                is_folder: listing.is_ok() && self.source.is_folder(),
                path,
                archive: None,
                shown_folder: None,
                watched: Watched::Kept,
                listing,
            };
        }
        // Opening the Trash for the first time (nothing deleted yet) should
        // show an empty folder, not a "no such directory" error, so
        // materialise it.
        if fileops::is_trash_dir(&path) {
            let _ = fileops::ensure_trash_dir();
        }
        let shown_folder = super::shown_selection::folder(&path);
        let location = crate::archive::location(&path);
        // Only a listing of one folder's entries is watched: Recent, the
        // Applications place, an archive and the hosted Trash (two folders)
        // are polled instead.
        let virtual_listing = location.is_some()
            || matches!(
                self.source,
                Source::Recent(_) | Source::Listed(_) | Source::HostedTrash
            );
        // Watch before reading, so a change landing between the two is
        // reported (and costs one more read) rather than lost.
        let watched = if virtual_listing {
            Watched::Nothing
        } else if self.watching.as_ref() == Some(&path) {
            Watched::Kept
        } else {
            Watched::Started(crate::directory_watch::StartedWatch::start(&path))
        };
        let archive = location.map(|(file, prefix)| Archive {
            cached: crate::archive::cached(&file),
            file,
            prefix,
        });
        let listing = if archive.is_some() {
            Ok(Vec::new())
        } else {
            read_entries(&path, &self.source)
        };
        FolderRead {
            is_folder: archive.is_none()
                && self.source.is_folder()
                && listing.is_ok()
                && path.is_dir(),
            path,
            archive,
            shown_folder,
            watched,
            listing,
        }
    }
}

impl FolderRead {
    fn failed(path: PathBuf, error: &std::io::Error) -> Self {
        Self {
            path,
            archive: None,
            shown_folder: None,
            watched: Watched::Nothing,
            listing: Err(error.to_string()),
            is_folder: false,
        }
    }
}

impl Source {
    fn is_folder(&self) -> bool {
        !matches!(self, Self::Recent(_) | Self::Listed(_))
    }
}

pub(super) fn read_entries(path: &Path, source: &Source) -> Result<Vec<Entry>, String> {
    match source {
        Source::Recent(paths) => Ok(super::open_with_recent::recent_entries_of(paths)),
        Source::Listed(entries) => Ok(entries.clone()),
        Source::HostedTrash => {
            let mut entries = read_dir_sync(path)?;
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
        }
        Source::WorkspaceRoots(roots) => crate::model::read_dir_with_workspace_roots(path, roots),
        Source::Plain => read_dir_sync(path),
    }
}

pub(super) struct FolderReads {
    pending: Option<Pending>,
    wake: ui::WakeHandle,
}

struct Pending {
    purpose: Purpose,
    done: Arc<Mutex<Option<FolderRead>>>,
}

impl FolderReads {
    pub(super) fn new(wake: ui::WakeHandle) -> Self {
        Self {
            pending: None,
            wake,
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn start(&mut self, job: Job, purpose: Purpose) -> Option<(Purpose, FolderRead)> {
        if !ui::storage::waiting::forbidden() {
            self.pending = None;
            let read = job.run();
            return Some((purpose, read));
        }
        let path = job.path.clone();
        let done = Arc::new(Mutex::new(None));
        let landed = Arc::clone(&done);
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("files-read".into())
            .spawn(move || {
                let read = job.run();
                *landed.lock_recover() = Some(read);
                wake.wake();
            });
        match spawned {
            Ok(_) => {
                self.pending = Some(Pending { purpose, done });
                None
            }
            Err(error) => {
                self.pending = None;
                Some((purpose, FolderRead::failed(path, &error)))
            }
        }
    }

    pub(super) fn take(&mut self) -> Option<(Purpose, FolderRead)> {
        let read = self.pending.as_ref()?.done.lock_recover().take()?;
        let pending = self.pending.take()?;
        Some((pending.purpose, read))
    }
}

impl super::FilesState {
    pub(super) fn folder_job(&self, path: PathBuf, entries_only: bool) -> Job {
        let source = if crate::recent::is_location(&path) {
            Source::Recent(
                self.recent_list()
                    .items()
                    .iter()
                    .map(|item| item.path.clone())
                    .collect(),
            )
        } else if crate::applications::is_location(&path) {
            Source::Listed(self.applications_entries())
        } else if self.hosted_clipboard() && fileops::is_trash_dir(&path) {
            Source::HostedTrash
        } else if self.startup_accounts.is_some() {
            Source::WorkspaceRoots(self.workspace_roots.clone())
        } else {
            Source::Plain
        };
        Job {
            path,
            source,
            watching: self.directory_watch.target().map(Path::to_path_buf),
            entries_only,
        }
    }

    #[cfg(test)]
    pub(super) fn read_directory(&self, path: &Path) -> Result<Vec<Entry>, String> {
        let job = self.folder_job(path.to_path_buf(), true);
        read_entries(&job.path, &job.source)
    }

    pub(super) fn load(&mut self, path: PathBuf) {
        self.load_then(path, Then::Nothing);
    }

    pub(super) fn load_then(&mut self, path: PathBuf, then: Then) {
        let path = self.place_of(path);
        self.leave_cloud_browse();
        self.typing.reset();
        self.selection.clear();
        self.retain_shown_selection();
        self.error = None;
        self.last_click = None;
        self.status = None;
        self.scroll_offset.set((0.0, 0.0));
        self.list_slot.reset_scroll();
        self.invalidate_open_with();
        self.close_app_chooser();
        self.directory_changed = false;
        self.next_metadata_refresh = None;
        self.last_directory_read = ui::time::now();
        self.read_shown_folder(path, then);
    }

    pub(super) fn read_shown_folder(&mut self, path: PathBuf, then: Then) {
        let showing = path == self.current_path && !self.is_loading && self.error.is_none();
        let job = self.folder_job(path.clone(), false);
        if let Some((purpose, read)) = self.folder_reads.start(job, Purpose::Show(then)) {
            self.apply_read(purpose, read);
            return;
        }
        if !showing {
            self.is_loading = true;
            self.entries.clear();
            self.shown_folder = None;
            self.shown_is_folder = false;
            self.archive_listing.stop();
            self.current_path = path;
        }
        self.structure_dirty = true;
    }

    pub(super) fn show_read(&mut self, read: FolderRead, then: Then) {
        let FolderRead {
            path,
            archive,
            shown_folder,
            watched,
            listing,
            is_folder,
        } = read;
        let recent = crate::recent::is_location(&path);
        let in_archive = archive.is_some();
        self.shown_folder = shown_folder;
        self.shown_is_folder = is_folder;
        match watched {
            Watched::Kept => {}
            Watched::Nothing => {
                let _ = self.directory_watch.point_at(None);
            }
            Watched::Started(started) => {
                let _ = self.directory_watch.adopt(&path, started);
            }
        }
        let loaded = if let Some(archive) = archive {
            let Some(entries) = archive.cached else {
                // A compressed tar is decoded front to back to list it,
                // which can take minutes: the window keeps running and
                // shows the folder once it is read.
                self.is_loading = true;
                self.archive_listing.start(&archive.file);
                self.current_path = path;
                self.structure_dirty = true;
                return;
            };
            crate::archive::children_of(&entries, &archive.prefix)
                .map(|rows| archive_rows(&archive.file, rows))
                .map_err(|error| error.to_string())
        } else {
            self.archive_listing.stop();
            listing
        };
        match loaded {
            Ok(mut entries) => {
                if !self.show_hidden {
                    entries.retain(|e| !e.name.starts_with('.'));
                }
                if !recent {
                    sort_entries(self.sort_by, self.sort_order, &mut entries);
                }
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                // Drop stale entries so the error view isn't backed by the old
                // directory's contents.
                self.entries.clear();
                self.shown_folder = None;
                self.error = Some(t_args("files-directory-read-failed", &[arg("error", e)]));
            }
        }
        // Always record the path we attempted, even on error, so a Refresh
        // ("Try again") retries the same target rather than reloading the
        // previous valid directory and silently clearing the error.
        self.current_path = path;
        if self.error.is_none() && in_archive {
            self.status = Some(t("files-archive-read-only"));
        }
        self.is_loading = false;
        self.structure_dirty = true;
        self.refresh_external_search_catalogs();
        match then {
            Then::Nothing => {}
            Then::Select(path) => {
                let _ = self.select_shown(&path);
            }
            Then::Rename(path) => {
                if self.select_shown(&path) {
                    self.begin_rename();
                }
            }
        }
    }

    pub(super) fn select_shown(&mut self, path: &Path) -> bool {
        let Some(index) = self
            .displayed_entries()
            .iter()
            .position(|entry| entry.path == path)
        else {
            return false;
        };
        self.selection.select_only(index);
        self.retain_shown_selection();
        true
    }

    pub(super) fn take_folder_read(&mut self) -> bool {
        let Some((purpose, read)) = self.folder_reads.take() else {
            return false;
        };
        self.apply_read(purpose, read);
        true
    }

    pub(super) fn apply_read(&mut self, purpose: Purpose, read: FolderRead) -> bool {
        match purpose {
            Purpose::Show(then) => {
                self.show_read(read, then);
                true
            }
            // Rows do not move under a rename or a drag: the change is read
            // again once they end.
            Purpose::Update if self.renaming.is_some() || self.pin_drag.is_some() => {
                self.directory_changed = true;
                false
            }
            Purpose::Update => self.update_from(read.listing),
        }
    }
}
