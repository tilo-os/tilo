//! Recovery operations retain their exact reviewed grants through write admission.
use super::{RecoveryCopy, destination};
use crate::setup::{migration, windows_inventory::Inventory};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub(crate) struct Evidence {
    copy: RecoveryCopy,
    source_id: String,
    output: PathBuf,
    independent: bool,
    complete_coverage: bool,
}
impl Evidence {
    /// Re-enter the journal verifier, including independent content rereads and
    /// source postflight. Existence of a previous success is never sufficient.
    pub(crate) fn validate(&self, cancel: &AtomicBool) -> Result<(), String> {
        self.copy.validate_source(cancel)?;
        let report = self.copy.copy(true, cancel, |_| {})?;
        if !report.complete() {
            return Err("Recovery copy no longer verifies; retain the source and retry".into());
        }
        Ok(())
    }
    pub(crate) fn validate_plan(
        &self,
        plan: &crate::setup::staging::plan::Plan,
    ) -> std::io::Result<()> {
        super::write_guard::validate(&self.copy, plan)
    }
    fn report(&self) -> Value {
        json!({"source_id":self.source_id,"output":self.output,"journal":self.copy.journal,"independent_storage":self.independent,
            "coverage_complete":self.complete_coverage,"source_must_remain":true,
            "remote_content":"unverified","replacement_authorized":false})
    }
}

/// Owns explicit recovery grants, cancellation and verified-copy evidence.
/// Copy completion never authorizes replacement of the original system.
#[derive(Default)]
pub struct Flow {
    reviewed: Option<Arc<Evidence>>,
    verified: BTreeMap<String, Arc<Evidence>>,
    worker: Option<mpsc::Receiver<Result<migration::CopyReport, String>>>,
    cancel: Arc<AtomicBool>,
    picker: Option<(String, ui::PortalChannel)>,
    state: &'static str,
    error: Option<String>,
}
impl Flow {
    /// Whether a chooser or copy worker still owns the current operation.
    pub fn busy(&self) -> bool {
        self.worker.is_some() || self.picker.is_some()
    }
    /// Open the platform folder chooser for an explicitly identified source.
    pub fn choose(&mut self, source_id: String) -> Result<(), String> {
        if self.busy() {
            return Err("Recovery selection is busy".into());
        }
        self.picker = Some((source_id, crate::setup::folders::pick_folder()));
        Ok(())
    }
    /// Admit a completed chooser response against the current source inventory.
    pub fn poll_selection(&mut self, inventory: Option<&Inventory>) -> bool {
        let Some((source_id, channel)) = &self.picker else {
            return false;
        };
        let outcome = match channel.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                ui::PortalOutcome::Failed("Recovery folder chooser disconnected".into())
            }
        };
        let source_id = source_id.clone();
        self.picker = None;
        let result = match outcome {
            ui::PortalOutcome::Opened(path) => inventory
                .ok_or_else(|| "Windows source inventory unavailable".to_string())
                .and_then(|inventory| self.review_directory(inventory, &source_id, &path)),
            ui::PortalOutcome::Cancelled => return true,
            ui::PortalOutcome::Failed(error) => Err(error),
        };
        if let Err(error) = result {
            self.error = Some(error);
        }
        true
    }
    fn validate_destination(inventory: &Inventory, directory: &Path) -> Result<(), String> {
        let parent =
            migration::PinnedDirectory::open_absolute(directory).map_err(|e| e.to_string())?;
        parent.validate_destination().map_err(|e| e.to_string())?;
        for source in &inventory.roots {
            let pin = migration::PinnedDirectory::open_absolute(&source.path)
                .map_err(|e| e.to_string())?;
            if pin.contains_identity(&parent.identity) || parent.contains_identity(&pin.identity) {
                return Err(
                    "Recovery output and journal must be outside every granted Windows source"
                        .into(),
                );
            }
        }
        Ok(())
    }
    /// Validate all source boundaries before creating private recovery records.
    pub fn review_directory(
        &mut self,
        inventory: &Inventory,
        source_id: &str,
        directory: &Path,
    ) -> Result<(), String> {
        crate::setup::migration_worker::durable_copy_available()?;
        if self.busy() {
            return Err("Recovery copy is running".into());
        }
        if !inventory.roots.iter().any(|r| r.source_id == source_id) {
            return Err("Unknown Windows source identity".into());
        }
        Self::validate_destination(inventory, directory)?;
        // The receipt directory is a private sibling of the output, not its
        // ancestor. Check all source grants before creating even this metadata.
        let parent =
            migration::PinnedDirectory::open_absolute(directory).map_err(|e| e.to_string())?;
        let name = format!(".tilo-recovery-records-{}", uuid::Uuid::new_v4());
        let records = parent
            .create_child(std::ffi::OsStr::new(&name))
            .map_err(|e| e.to_string())?;
        crate::setup::migration_durability::readable_directory(&records.file)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        crate::setup::migration_durability::readable_directory(&parent.file)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        self.review(
            inventory,
            source_id,
            directory,
            &directory.join(name).join("journal"),
        )
    }
    /// Review a fresh private output below the selected destination directory.
    pub fn review(
        &mut self,
        inventory: &Inventory,
        source_id: &str,
        directory: &Path,
        journal: &Path,
    ) -> Result<(), String> {
        if !directory.is_absolute() || directory.parent().is_none() {
            return Err("Select an explicit recovery folder, not a filesystem root".into());
        }
        let output = directory.join(format!("tilo-recovery-{}", uuid::Uuid::new_v4()));
        self.review_output(inventory, source_id, &output, journal)
    }
    /// Reattach exact grants after restarting the installer. The journal is a
    /// hint only; explicit retry will reverify every source and output receipt.
    pub fn review_existing(
        &mut self,
        inventory: &Inventory,
        source_id: &str,
        output: &Path,
        journal: &Path,
    ) -> Result<(), String> {
        if !journal.try_exists().map_err(|e| e.to_string())? {
            return Err("Select the saved recovery journal".into());
        }
        self.review_output(inventory, source_id, output, journal)?;
        self.state = "interrupted";
        Ok(())
    }
    fn review_output(
        &mut self,
        inventory: &Inventory,
        source_id: &str,
        output: &Path,
        journal: &Path,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("Recovery copy is running".into());
        }
        let directory = output
            .parent()
            .ok_or("Select the exact recovery output directory")?;
        Self::validate_destination(inventory, directory)?;
        Self::validate_destination(
            inventory,
            journal
                .parent()
                .ok_or("Select a private recovery journal directory")?,
        )?;
        let source = inventory
            .roots
            .iter()
            .find(|r| r.source_id == source_id)
            .ok_or("Unknown exact Windows source grant")?;
        // No mount-root inference: the supplied directory itself is the grant.
        // Recovery output gets its own private child and cannot replace a folder.
        if !directory.is_absolute() || directory.parent().is_none() {
            return Err("Select an explicit recovery folder, not a filesystem root".into());
        }
        let pin =
            migration::PinnedDirectory::open_absolute(&source.path).map_err(|e| e.to_string())?;
        use cap_fs_ext::MetadataExt;
        if format!("{}:{}", pin.identity.dev(), pin.identity.ino()) != source_id {
            return Err("Windows source changed; select the exact source again".into());
        }
        let copy = RecoveryCopy::review(&source.path, output, journal)?;
        let independent = destination::physical(&source.path)
            .zip(destination::physical(directory))
            .is_some_and(|(source, output)| source.independent_of(&output));
        self.verified.remove(source_id);
        self.reviewed = Some(Arc::new(Evidence {
            copy,
            source_id: source_id.into(),
            output: output.into(),
            independent,
            complete_coverage: source.gaps.is_empty()
                && source
                    .entries
                    .iter()
                    .all(|e| e.copy_eligible && e.bytes.is_some()),
        }));
        self.state = "reviewed";
        self.error = None;
        Ok(())
    }
    /// Start copying, or explicitly resume through the durable receipt verifier.
    pub fn start(&mut self, resume: bool) -> Result<(), String> {
        if self.busy() {
            return Err("Recovery copy is running".into());
        }
        let evidence = self
            .reviewed
            .clone()
            .ok_or("Review a recovery folder first")?;
        if !resume && self.state != "reviewed" {
            return Err("Use explicit retry to resume verified output".into());
        }
        // Cancellation may precede journal creation. A retry can start fresh
        // only if neither journal nor output exists; existing output always
        // requires the durable receipt verifier and never gets overwritten.
        let resume = if resume
            && !evidence
                .copy
                .journal
                .try_exists()
                .map_err(|e| e.to_string())?
        {
            if evidence.output.try_exists().map_err(|e| e.to_string())? {
                return Err(
                    "Recovery output exists without its journal; review a new folder".into(),
                );
            }
            false
        } else {
            resume
        };
        self.verified.remove(&evidence.source_id);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.state = "copying";
        self.error = None;
        std::thread::spawn(move || {
            let _ = tx.send(evidence.copy.copy(resume, &cancel, |_| {}));
        });
        Ok(())
    }
    /// Request cancellation without removing verified output or original files.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.picker = None;
    }
    /// Consume a completed worker result; only complete reports become evidence.
    pub fn poll(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Recovery worker interrupted; retry to reverify receipts".into())
            }
        };
        self.worker = None;
        match result {
            Ok(report) if report.complete() => {
                self.state = "verified";
                if let Some(evidence) = &self.reviewed {
                    self.verified
                        .insert(evidence.source_id.clone(), evidence.clone());
                }
            }
            Ok(report) => {
                self.state = if report.cancelled {
                    "cancelled"
                } else {
                    "incomplete"
                };
            }
            Err(error) => {
                self.state = "interrupted";
                self.error = Some(error);
            }
        }
        true
    }
    /// Recovery never relaxes placement or permits Windows replacement. An
    /// interrupted selected operation must be resolved before preparing a stage.
    pub(crate) fn evidence(&self) -> Result<Vec<Arc<Evidence>>, String> {
        if self.busy()
            || self
                .reviewed
                .as_ref()
                .is_some_and(|e| !self.verified.contains_key(&e.source_id))
        {
            return Err("Finish and verify the selected recovery copy before review".into());
        }
        Ok(self.verified.values().cloned().collect())
    }
    /// Return device-local review state, including the explicitly granted paths.
    /// This report is not a public log or destructive-write authorization.
    pub fn report(&self) -> Value {
        json!({"durable_copy_available":crate::setup::migration_worker::durable_copy_available().is_ok(),"state":if self.state.is_empty() { "not-selected" } else {self.state},"busy":self.busy(),"error":self.error,
            "reviewed":self.reviewed.as_ref().map(|e| e.report()),"verified":self.verified.values().map(|e| e.report()).collect::<Vec<_>>(),
            "source_must_remain":true,"remote_content":"unverified","replacement_authorized":false})
    }
}
impl Drop for Flow {
    fn drop(&mut self) {
        self.cancel();
    }
}
