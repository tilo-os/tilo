//! NTFS drives in the places list: the Windows partition next to Tilo and
//! external drives.
//!
//! Files never mounts a drive and never talks to the mount service. It asks
//! the shell which drives there are (`ToolIntent::Drives`, answered from the
//! shell's cache) and asks the shell to open one (`ToolIntent::OpenDrive`).
//! The shell asks the person in its own sheet unless they chose to remember
//! that drive, mounts it, and opens it in Files with a grant for its folder.
//! A drive already open whose folder this window can read opens here.
#[cfg(test)]
#[path = "drives_tests.rs"]
mod tests;

use super::*;
use runtime_portal::tools::DriveEntry;
use std::sync::Mutex;

/// How often the list is asked for again while the window is up. The shell
/// answers from memory; this only bounds how stale a plugged-in drive looks.
const POLL: Duration = Duration::from_secs(5);

pub(super) type DriveLister = fn() -> Result<Vec<DriveEntry>, String>;
pub(super) type DriveOpener = fn(&str) -> Result<(), String>;

fn open_through_shell(drive: &str) -> Result<(), String> {
    runtime_portal::tools::open_tool(runtime_portal::tools::ToolIntent::OpenDrive {
        drive: drive.to_owned(),
    })
}

pub(super) struct Drives {
    pub(super) shown: Vec<DriveEntry>,
    /// The latest answer of a list or open request running off the UI thread.
    listed: Arc<Mutex<Option<Vec<DriveEntry>>>>,
    refused: Arc<Mutex<Option<String>>>,
    listing: bool,
    next_poll: Option<Instant>,
    /// `None` for a window whose drives are given (scenarios, tests).
    lister: Option<DriveLister>,
    opener: DriveOpener,
}

impl Default for Drives {
    fn default() -> Self {
        Self {
            shown: Vec::new(),
            listed: Arc::default(),
            refused: Arc::default(),
            listing: false,
            next_poll: None,
            // Unit tests never reach a shell; they inject what they need.
            lister: (!cfg!(test)).then_some(runtime_portal::tools::list_drives as DriveLister),
            opener: open_through_shell,
        }
    }
}

impl Drives {
    /// When the window should look again.
    pub(super) fn next_wake(&self) -> Option<Instant> {
        let answered = self.listed.lock().is_ok_and(|slot| slot.is_some())
            || self.refused.lock().is_ok_and(|slot| slot.is_some());
        if answered {
            return Some(Instant::now());
        }
        self.lister.and(self.next_poll)
    }
}

/// The places-list row for `drive`.
pub(super) fn drive_place(drive: &DriveEntry) -> places_sidebar::DrivePlace {
    let size = crate::i18n::format_bytes(drive.size_bytes);
    let label = if drive.label.trim().is_empty() {
        if drive.internal {
            t("files-drive-unnamed-internal")
        } else {
            t("files-drive-unnamed-external")
        }
    } else {
        drive.label.clone()
    };
    places_sidebar::DrivePlace {
        id: drive.id.clone(),
        label,
        internal: drive.internal,
        open_at: drive.open_at.as_ref().map(PathBuf::from),
        detail: if drive.open_at.is_some() {
            t("files-drive-open")
        } else {
            size
        },
    }
}

impl FileExplorerWidget {
    /// Show `drives` and stop asking the shell: the scenario and test seam.
    /// The live window never starts with drives of its own.
    #[doc(hidden)]
    pub fn debug_show_drives(&mut self, drives: Vec<DriveEntry>) {
        self.drives.lister = None;
        self.drives.shown = drives;
        self.places_shown = None;
        self.structure_dirty = true;
    }

    /// The `drives` screenshot scenario: made-up drives, the Windows
    /// partition next to Tilo and an external stick already open. No shell is
    /// asked.
    pub(super) fn debug_show_sample_drives(&mut self) {
        self.debug_show_drives(vec![
            DriveEntry {
                id: "nvme0n1p3:1".into(),
                label: "Windows".into(),
                size_bytes: 511_000_000_000,
                internal: true,
                open_at: None,
            },
            DriveEntry {
                id: "sdb1:7".into(),
                label: String::new(),
                size_bytes: 64_000_000_000,
                internal: false,
                open_at: Some(self.current_path.to_string_lossy().into_owned()),
            },
        ]);
    }

    /// Route opening a drive through `opener` instead of the shell.
    #[cfg(test)]
    pub(super) fn set_drive_opener(&mut self, opener: DriveOpener) {
        self.drives.opener = opener;
    }

    /// Take in answers that arrived, and ask again when due. Returns whether
    /// what the window shows changed.
    pub(super) fn poll_drives(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(listed) = self
            .drives
            .listed
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.drives.listing = false;
            if listed != self.drives.shown {
                self.drives.shown = listed;
                self.places_shown = None;
                changed = true;
            }
        }
        if let Some(refusal) = self
            .drives
            .refused
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.status = Some(t_args("files-drive-open-failed", &[arg("error", refusal)]));
            changed = true;
        }
        let Some(lister) = self.drives.lister else {
            return changed;
        };
        if self.drives.listing || self.drives.next_poll.is_some_and(|due| now < due) {
            return changed;
        }
        self.drives.next_poll = Some(now + POLL);
        self.drives.listing = true;
        let slot = Arc::clone(&self.drives.listed);
        let spawned = std::thread::Builder::new()
            .name("files-drives".into())
            .spawn(move || {
                // No session to ask is no drives, not an error to show.
                let drives = lister().unwrap_or_default();
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(drives);
                }
            });
        if spawned.is_err() {
            self.drives.listing = false;
        }
        changed
    }

    /// Open the drive with this id: here when it is already open and this
    /// window can read its folder, otherwise through the shell, which asks
    /// the person first unless they chose to remember the drive.
    pub(super) fn open_drive(&mut self, id: &str) -> Result<(), String> {
        let drive = self
            .drives
            .shown
            .iter()
            .find(|drive| drive.id == id)
            .cloned()
            .ok_or_else(|| t("files-drive-gone"))?;
        if let Some(open_at) = drive.open_at.as_deref().map(PathBuf::from)
            && std::fs::read_dir(&open_at).is_ok()
        {
            self.open_path(open_at);
            return Ok(());
        }
        let opener = self.drives.opener;
        let refused = Arc::clone(&self.drives.refused);
        let id = drive.id;
        std::thread::Builder::new()
            .name("files-open-drive".into())
            .spawn(move || {
                if let Err(error) = opener(&id)
                    && let Ok(mut slot) = refused.lock()
                {
                    *slot = Some(error);
                }
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// One resource per drive, so an agent or assistive tool can list them
    /// and open one; opening still goes through the shell's sheet.
    pub(super) fn live_drive_resources(&self) -> Vec<Resource> {
        if !ui::bound_dispatch::host_person_invocation_permitted() {
            return Vec::new();
        }
        self.drives
            .shown
            .iter()
            .map(|drive| {
                let place = drive_place(drive);
                let mut resource = Resource::new(
                    format!("files.drive:{}", drive.id),
                    "files.drive",
                    place.label,
                );
                let where_ = if drive.internal {
                    t("files-drive-internal")
                } else {
                    t("files-drive-external")
                };
                resource.presentation = Some(tilo_actions::ResourcePresentation {
                    detail: Some(format!(
                        "{where_} — {}",
                        crate::i18n::format_bytes(drive.size_bytes)
                    )),
                    badge: drive.open_at.is_some().then(|| t("files-drive-open")),
                    preview: None,
                });
                resource.actions.push(crate::effects::row_action(
                    &crate::effects::FilesEffect::OpenDrive {
                        drive: drive.id.clone(),
                    },
                    "open",
                    t("files-drive-open-action"),
                ));
                resource
            })
            .collect()
    }
}
