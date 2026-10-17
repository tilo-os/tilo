use super::super::{
    migration::{self, CopyProgress, CopyReport, PinnedDirectory, changed},
    migration_durability as durability,
    migration_journal::{Snapshot, copy_folder_with_journal, resume_folder},
    migration_review::{InventorySummary, SourceInventory},
    migration_worker::{WorkerEvent, WorkerOutcome},
    nofollow::metadata_nofollow,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

mod fidelity;
mod intent;

/// Private review: paths and snapshots stay outside collaborative app state.
#[derive(Clone)]
pub struct HomeReview {
    source: PathBuf,
    destination: PathBuf,
    journal: PathBuf,
    revision: [u8; 32],
    source_identity: durability::Identity,
    parent_identity: durability::Identity,
    pub summary: InventorySummary,
}

pub fn review(source: &Path, destination: &Path, journal: &Path) -> Result<HomeReview, String> {
    review_inner(source, destination, journal).map_err(|error| error.to_string())
}

fn review_inner(source: &Path, destination: &Path, journal: &Path) -> io::Result<HomeReview> {
    durability::supported()?;
    if source.parent().is_none()
        || !source.is_absolute()
        || !destination.is_absolute()
        || !journal.is_absolute()
    {
        return Err(changed(
            "Home transfer requires absolute source, destination and private journal paths.",
        ));
    }
    let source_pin = PinnedDirectory::open_absolute(source)?;
    reject_virtual_source(&source_pin.file)?;
    let parent = PinnedDirectory::open_absolute(
        destination
            .parent()
            .ok_or_else(|| changed("Destination must name a new directory."))?,
    )?;
    parent.validate_destination()?;
    if parent.contains_identity(&source_pin.identity) || source.starts_with(destination) {
        return Err(changed("The destination must be outside the source home."));
    }
    let name = destination
        .file_name()
        .ok_or_else(|| changed("Destination must name a new directory."))?;
    match metadata_nofollow(&parent.file, Path::new(name)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(changed(
                "Destination already exists; no existing data will be overwritten.",
            ));
        }
        Err(error) => return Err(error),
    }
    let journal_parent = PinnedDirectory::open_absolute(
        journal
            .parent()
            .ok_or_else(|| changed("Journal has no parent."))?,
    )?;
    journal_parent.validate_destination()?;
    super::super::destination_security::check_private_file(&journal_parent.file)?;
    if journal_parent.contains_identity(&source_pin.identity) || journal.starts_with(destination) {
        return Err(changed(
            "Keep the private transfer journal outside both source and destination.",
        ));
    }
    let inventory = SourceInventory::capture_home(&source_pin)?;
    Ok(HomeReview {
        source: source.to_owned(),
        destination: destination.to_owned(),
        journal: journal.to_owned(),
        revision: inventory.revision()?,
        source_identity: directory_identity(&source_pin)?,
        parent_identity: directory_identity(&parent)?,
        summary: inventory.summary()?,
    })
}

fn directory_identity(directory: &PinnedDirectory) -> io::Result<durability::Identity> {
    durability::identity(&durability::readable_directory(&directory.file)?)
}

fn reject_virtual_source(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: valid live descriptor and writable full statfs storage.
    if unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatfs initialized the complete structure.
    let kind = unsafe { status.assume_init() }.f_type as u64;
    if matches!(
        kind,
        0x9fa0
            | 0x62656572
            | 0x1cd1
            | 0x27e0eb
            | 0x63677270
            | 0x64626720
            | 0x74726163
            | 0x73636673
            | 0x62656570
            | 0xcafe4a11
            | 0xf97cff8c
    ) {
        return Err(changed(
            "Virtual OS filesystems cannot be selected as a home transfer source.",
        ));
    }
    Ok(())
}

impl HomeReview {
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// Reopen only after the caller supplies the source and destination grants
    /// again. Stored paths alone never grant filesystem access.
    pub fn recover(source: &Path, destination: &Path, journal: &Path) -> Result<Self, String> {
        intent::recover(source, destination, journal).map_err(|error| error.to_string())
    }
    pub fn verify_adoption(
        &self,
        witness: &super::promotion::AdoptionWitness,
    ) -> Result<CopyReport, String> {
        fidelity::verify_adoption(self, witness).map_err(|error| error.to_string())
    }
    pub fn verify_adoption_after_applications(
        &self,
        witness: &super::promotion::AdoptionWitness,
    ) -> Result<CopyReport, String> {
        fidelity::verify_adoption_applications(self, witness).map_err(|error| error.to_string())
    }
    pub fn verify_after_applications(&self) -> Result<CopyReport, String> {
        fidelity::verify_applications(self).map_err(|error| error.to_string())
    }
    pub fn verify_flatpak_installation(&self) -> Result<CopyReport, String> {
        fidelity::verify_flatpak_installation(self).map_err(|error| error.to_string())
    }
    pub fn verify_completed_tree(&self) -> Result<CopyReport, String> {
        fidelity::verify_completed_tree(self).map_err(|error| error.to_string())
    }
    pub fn retain_review(&self) -> Result<(), String> {
        self.validate().map_err(|error| error.to_string())?;
        intent::save(self).map_err(|error| error.to_string())
    }
    pub fn start(&self) -> Result<HomeWorker, String> {
        HomeWorker::start(self.clone(), false)
    }
    pub fn retry(&self) -> Result<HomeWorker, String> {
        HomeWorker::start(self.clone(), true)
    }

    fn validate_source(&self) -> io::Result<Arc<PinnedDirectory>> {
        let source = PinnedDirectory::open_absolute(&self.source)?;
        if directory_identity(&source)? != self.source_identity
            || SourceInventory::capture_home(&source)?.revision()? != self.revision
        {
            return Err(changed(
                "Source home changed after review; review the transfer again",
            ));
        }
        Ok(source)
    }
    fn validate(&self) -> io::Result<Arc<PinnedDirectory>> {
        let source = self.validate_source()?;
        let parent = PinnedDirectory::open_absolute(
            self.destination
                .parent()
                .ok_or_else(|| changed("Destination has no parent"))?,
        )?;
        parent.validate_destination()?;
        if directory_identity(&parent)? != self.parent_identity {
            return Err(changed("Reviewed destination parent was replaced"));
        }
        Ok(source)
    }
    pub fn recover_adoption(
        source: &Path,
        destination: &Path,
        journal: &Path,
        witness: &super::promotion::AdoptionWitness,
        prefix: &Path,
    ) -> Result<Self, String> {
        intent::recover_adoption(source, destination, journal, witness, prefix)
            .map_err(|e| e.to_string())
    }
    pub fn verify_flatpak_adoption(
        &self,
        witness: &super::promotion::AdoptionWitness,
    ) -> Result<CopyReport, String> {
        fidelity::verify_flatpak_adoption(self, witness).map_err(|e| e.to_string())
    }
    pub fn transfer(
        &self,
        retry: bool,
        cancel: &AtomicBool,
        progress: impl FnMut(CopyProgress),
    ) -> Result<CopyReport, String> {
        let source = self.validate().map_err(|error| error.to_string())?;
        if retry {
            let saved = Self::recover(&self.source, &self.destination, &self.journal)?;
            if saved.revision != self.revision
                || saved.source_identity != self.source_identity
                || saved.parent_identity != self.parent_identity
            {
                return Err("Retry does not match the saved whole-home review.".into());
            }
        } else {
            intent::save(self).map_err(|error| error.to_string())?;
        }
        let state_path = self.journal.with_extension("home-finalization");
        if retry && fs::symlink_metadata(&state_path).is_ok() {
            return fidelity::resume(self, &source, &state_path, cancel)
                .map_err(|error| error.to_string());
        }
        let mut report = if retry && fs::symlink_metadata(&self.journal).is_ok() {
            resume_folder(&self.journal, cancel, progress)?
        } else {
            copy_folder_with_journal(
                &self.source,
                &self.destination,
                &self.journal,
                cancel,
                progress,
            )?
        };
        // A byte-phase failure or cancellation must never trigger finalization.
        if !report.scan_completed || report.cancelled || !report.failed.is_empty() {
            return Ok(report);
        }
        self.validate().map_err(|error| error.to_string())?;
        fidelity::begin(self, &source, &state_path, &mut report, cancel)
            .map_err(|error| error.to_string())?;
        Ok(report)
    }
}

pub struct HomeWorker {
    cancel: Arc<AtomicBool>,
    events: Receiver<WorkerEvent>,
    terminal: bool,
    finished: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<CopyProgress>>>,
}
impl HomeWorker {
    fn start(review: HomeReview, retry: bool) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, events) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let progress = Arc::new(Mutex::new(None));
        let worker_progress = Arc::clone(&progress);
        std::thread::Builder::new()
            .name("imager-home-transfer".into())
            .spawn(move || {
                let result = review.transfer(retry, &worker_cancel, move |progress| {
                    if let Ok(mut latest) = worker_progress.lock() {
                        *latest = Some(progress);
                    }
                });
                let outcome = match result {
                    Ok(report) if report.complete() => WorkerOutcome::Complete(report),
                    Ok(report) => WorkerOutcome::Incomplete(report),
                    Err(error) => WorkerOutcome::Failed(error),
                };
                let _ = sender.send(WorkerEvent::Finished(outcome));
                worker_finished.store(true, Ordering::Release);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            cancel,
            events,
            terminal: false,
            finished,
            progress,
        })
    }
    pub fn cancel(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
            && !self.terminal
            && !self.cancel.swap(true, Ordering::Relaxed)
    }
    pub fn try_next(&mut self) -> Option<WorkerEvent> {
        if self.terminal {
            return None;
        }
        match self.events.try_recv() {
            Ok(event) => {
                self.terminal = matches!(event, WorkerEvent::Finished(_));
                Some(event)
            }
            Err(TryRecvError::Empty) => {
                self.progress.lock().ok()?.take().map(WorkerEvent::Progress)
            }
            Err(TryRecvError::Disconnected) => {
                self.terminal = true;
                Some(WorkerEvent::Finished(WorkerOutcome::Interrupted))
            }
        }
    }
    #[cfg(test)]
    fn finish(&mut self) -> WorkerOutcome {
        loop {
            match self
                .events
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap()
            {
                WorkerEvent::Progress(_) => {}
                WorkerEvent::Finished(outcome) => {
                    self.terminal = true;
                    return outcome;
                }
                _ => panic!("Unexpected persistence event"),
            }
        }
    }
}
impl Drop for HomeWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
