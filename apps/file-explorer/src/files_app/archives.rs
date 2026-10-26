//! Archives in the window: browsing one as a read-only folder, read off
//! the window's thread; opening a file inside it; and Extract here,
//! Extract to… and Compress, queued as background file operations.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};
use ui::lock::MutexRecover as _;

use super::{FileExplorerWidget, OpFollowUp, TRANSFER_SHOWN_AFTER, clash, hex_prefix};
use crate::config;
use crate::i18n::{arg, t, t_args};
use crate::model::{Entry, sort_entries};

/// Space kept clear at the bottom of the file viewport for the transient
/// item/status pill. The pill includes its hover shadow, so this is larger
/// than its measured text capsule: no file label or target may sit beneath it.
/// A read-only copy of the file `member` of `archive`, made once per
/// archive version in Files' cache, so opening it again reuses it.
pub(super) fn archive_member_copy(archive: &Path, member: &Path) -> std::io::Result<PathBuf> {
    let copy = archive_member_path(archive, member)?;
    if copy.is_file() {
        return Ok(copy);
    }
    if let Some(folder) = copy.parent() {
        std::fs::create_dir_all(folder)?;
    }
    crate::archive::extract_member(archive, member, &copy)?;
    Ok(copy)
}

/// The copy of `member` opened before, while the archive is unchanged.
pub(super) fn archive_member_cached(archive: &Path, member: &Path) -> Option<PathBuf> {
    archive_member_path(archive, member)
        .ok()
        .filter(|copy| copy.is_file())
}

/// Where Files keeps its copy of `member` of this version of `archive`.
fn archive_member_path(archive: &Path, member: &Path) -> std::io::Result<PathBuf> {
    let modified = std::fs::metadata(archive)?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs());
    let digest = Sha256::digest(format!("{}\0{modified}", archive.display()).as_bytes());
    let folder = config::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("archive-members")
        .join(hex_prefix(&digest, 12))
        .join(member.parent().unwrap_or_else(|| Path::new("")));
    let name = member
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file"))?;
    Ok(folder.join(name))
}

/// Reading files out of archives to open them, off the window's thread.
/// A file read out of an archive: its path as shown, and its copy or why
/// it could not be read.
type MemberRead = (PathBuf, std::io::Result<PathBuf>);

pub(super) struct ArchiveMembers {
    wake: Arc<dyn Fn() + Send + Sync>,
    done: Arc<std::sync::Mutex<Vec<MemberRead>>>,
}

impl ArchiveMembers {
    pub(super) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
            done: Arc::default(),
        }
    }

    /// Read `member` of `archive` into its copy on a thread.
    pub(super) fn start(&self, archive: PathBuf, member: PathBuf) {
        let done = Arc::clone(&self.done);
        let wake = Arc::clone(&self.wake);
        let shown = archive.join(&member);
        let spawned = std::thread::Builder::new()
            .name("files-archive-member".into())
            .spawn(move || {
                let copy = archive_member_copy(&archive, &member);
                done.lock_recover().push((archive.join(&member), copy));
                wake();
            });
        if let Err(error) = spawned {
            self.done.lock_recover().push((shown, Err(error)));
        }
    }

    /// The reads finished since last asked: the member's path as shown, and
    /// its copy or why it could not be read.
    pub(super) fn take(&self) -> Vec<MemberRead> {
        std::mem::take(&mut *self.done.lock_recover())
    }
}

impl std::fmt::Debug for ArchiveMembers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveMembers").finish_non_exhaustive()
    }
}

/// An archive's entries as the rows of the folder they are in.
pub(super) fn archive_rows(archive: &Path, rows: Vec<crate::archive::Entry>) -> Vec<Entry> {
    rows.into_iter()
        .map(|entry| {
            let name = entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let synthetic_path = archive.join(&entry.path);
            let is_dir = entry.kind == crate::archive::Kind::Directory;
            Entry {
                name,
                path: synthetic_path.clone(),
                is_dir,
                size: entry.size,
                modified: entry.modified.map(chrono::DateTime::<chrono::Local>::from),
                mime: if is_dir {
                    "inode/directory".into()
                } else {
                    tilo_mime::from_path_or_octet_stream(&synthetic_path).to_string()
                },
                sync_state: None,
                content_hash: None,
                record_id: None,
                availability: None,
                share: None,
                history: None,
                app: None,
                rich_metadata: None,
                observed: None,
            }
        })
        .collect()
}

impl FileExplorerWidget {
    /// The archives the selection holds.
    pub(super) fn selected_archives(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|path| crate::archive::is_supported(path) && path.is_file())
            .collect()
    }

    pub(super) fn apply_extract_here(&mut self) -> bool {
        let archives = self.selected_archives();
        if archives.is_empty() {
            self.status = Some(t("files-archive-select"));
            return true;
        }
        for archive in archives {
            if let Some(folder) = archive.parent().map(Path::to_path_buf) {
                self.extract_into(vec![archive], folder);
            }
        }
        true
    }

    pub(super) fn apply_extract_current_archive(&mut self) -> bool {
        let Some((archive, _)) = crate::archive::location(&self.current_path) else {
            return self.apply_extract_here();
        };
        if let Some(folder) = archive.parent().map(Path::to_path_buf) {
            self.extract_into(vec![archive], folder);
        }
        true
    }

    /// `FileCommand::ExtractTo`: ask for the folder, then extract there.
    pub(super) fn apply_extract_to(&mut self) -> bool {
        let archives = match crate::archive::location(&self.current_path) {
            Some((archive, _)) => vec![archive],
            None => self.selected_archives(),
        };
        if archives.is_empty() {
            self.status = Some(t("files-archive-select"));
            return true;
        }
        let chooser = ui::portal::PickRequest::writable_directory(
            &t("files-archive-extract-to-title"),
            &t("files-archive-extract-to-accept"),
        )
        .send();
        self.extract_chooser = Some((archives, chooser));
        true
    }

    /// The folder picked for Extract to…, once the picker answered.
    pub(super) fn poll_extract_chooser(&mut self) -> bool {
        let Some(reply) =
            self.extract_chooser
                .as_ref()
                .and_then(|(_, chooser)| match chooser.try_recv() {
                    Ok(reply) => Some(reply),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(
                        ui::portal::PortalOutcome::Failed(t("files-archive-extract-to-closed")),
                    ),
                })
        else {
            return false;
        };
        let Some((archives, _)) = self.extract_chooser.take() else {
            return false;
        };
        match reply {
            ui::portal::PortalOutcome::Opened(folder) => self.extract_into(archives, folder),
            ui::portal::PortalOutcome::Failed(error) => {
                self.notify_failure(t("files-archive-extract-failed-title"), &error, None);
            }
            ui::portal::PortalOutcome::Cancelled => {}
        }
        self.structure_dirty = true;
        true
    }

    /// Extract `archives` into `folder`, asking first when a name they
    /// would most likely take there is in use.
    pub(super) fn extract_into(&mut self, archives: Vec<PathBuf>, folder: PathBuf) {
        let clashing: Vec<(PathBuf, String)> = archives
            .iter()
            .map(|archive| (archive.clone(), crate::archive::likely_name(archive)))
            .filter(|(_, name)| std::fs::symlink_metadata(folder.join(name)).is_ok())
            .collect();
        if clashing.is_empty() {
            self.queue_extractions(&archives, &folder, crate::file_ops::IfExists::KeepBoth);
        } else {
            self.clash = Some(clash::PendingClash::extraction(archives, clashing, folder));
            self.structure_dirty = true;
        }
    }

    pub(super) fn queue_extractions(
        &mut self,
        archives: &[PathBuf],
        folder: &Path,
        if_exists: crate::file_ops::IfExists,
    ) {
        let group = self
            .dispatcher
            .as_ref()
            .map(ui::framework::Dispatcher::begin_group);
        for archive in archives {
            let name = archive.file_name().map_or_else(
                || archive.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            self.queue(
                crate::file_ops::FileOp::Extract {
                    archive: archive.display().to_string(),
                    folder: folder.display().to_string(),
                    if_exists,
                },
                OpFollowUp::Extract { name },
            );
        }
        drop(group);
    }

    pub(super) fn apply_compress_selection(&mut self, packing: crate::archive::Packing) -> bool {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = Some(t("files-archive-select-compress"));
            return true;
        }
        let base = match paths.as_slice() {
            [only] => crate::archive::stem(only),
            _ => "Archive".to_owned(),
        };
        let name = format!("{base}.{}", packing.extension());
        self.queue(
            crate::file_ops::FileOp::Compress {
                paths: paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                folder: self.current_path.display().to_string(),
                name,
            },
            OpFollowUp::Compress,
        );
        true
    }

    /// An extraction partway through, as its card shows it (headless shots).
    #[doc(hidden)]
    pub fn debug_show_extract_progress(&mut self) {
        let archive = self
            .displayed_entries()
            .iter()
            .find(|entry| crate::archive::is_supported(&entry.path))
            .map_or_else(|| self.current_path.clone(), |entry| entry.path.clone());
        // Running as a real one would, so the window follows it as it
        // follows any: the capture's process ends with the frame.
        let transfer = crate::file_ops::LocalTransfer {
            done: (1_610_612_736_u64).into(),
            total: (4_617_089_843_u64).into(),
            kind: crate::file_ops::TransferKind::Extract,
            count: 1,
            folder: archive.display().to_string(),
            started: ui::time::now().checked_sub(TRANSFER_SHOWN_AFTER),
            ..crate::file_ops::LocalTransfer::default()
        };
        self.show_transfer(&transfer);
        crate::file_ops::run_for_capture(transfer);
    }

    /// Show the archive being browsed once its entries were read; whether
    /// anything changed.
    pub(super) fn take_archive_listing(&mut self) -> bool {
        let Some((archive, read)) = self.archive_listing.take() else {
            return false;
        };
        let Some((current, prefix)) = crate::archive::location(&self.current_path) else {
            return false;
        };
        if current != archive {
            return false;
        }
        let rows = read.and_then(|entries| crate::archive::children_of(&entries, &prefix));
        match rows {
            Ok(rows) => {
                let mut entries = archive_rows(&archive, rows);
                if !self.show_hidden {
                    entries.retain(|entry| !entry.name.starts_with('.'));
                }
                sort_entries(self.sort_by, self.sort_order, &mut entries);
                self.entries = entries;
                self.status = Some(t("files-archive-read-only"));
            }
            Err(error) => {
                self.entries.clear();
                self.shown_folder = None;
                self.error = Some(t_args(
                    "files-directory-read-failed",
                    &[arg("error", error.to_string())],
                ));
            }
        }
        self.is_loading = false;
        self.structure_dirty = true;
        self.refresh_external_search_catalogs();
        true
    }

    /// Wait for the archive being browsed to be read, and show it: for a
    /// test or a capture, which show one settled frame.
    pub(crate) fn wait_for_archive(&mut self) {
        let until = Instant::now() + Duration::from_secs(20);
        while self.is_loading && Instant::now() < until {
            if !self.take_archive_listing() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// Wait for the files being read out of archives, and open them.
    #[cfg(test)]
    pub(crate) fn wait_for_archive_members(&mut self) {
        let until = Instant::now() + Duration::from_secs(20);
        while !self.open_read_members() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Open the files read out of archives since last asked; whether any
    /// were.
    pub(super) fn open_read_members(&mut self) -> bool {
        let read = self.archive_members.take();
        if read.is_empty() {
            return false;
        }
        for (shown, copy) in read {
            match copy {
                Ok(copy) => self.open_queue.push_back(copy),
                Err(error) => self.notify_open_failed(&shown, &error.to_string()),
            }
        }
        self.structure_dirty = true;
        true
    }
}
