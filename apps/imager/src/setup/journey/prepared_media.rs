//! Preparation stays alive across the installer-to-media-writer transition.
//! Prepared setup is ciphertext; optional home export retains its password in
//! private zeroizing custody until the streaming writer finishes.
#[path = "prepared_media/delivery_retirement.rs"]
pub(in crate::setup) mod delivery_retirement;
#[cfg(test)]
#[path = "prepared_media/native_retained.rs"]
pub(crate) mod native_retained;
use super::super::{
    media_setup::{DeliveryReceipt, MediaFilesystemGrant, PreparedSetupDelivery},
    portable_transfer::{ExportReview, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{self, Manifest},
};
use std::{
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use zeroize::Zeroizing;

use super::prepared_media::{PreparedMedia, ReturnReason};
#[derive(Clone)]
struct TaskProgress {
    cancel: Arc<AtomicBool>,
    phase: Arc<Mutex<ui::framework::JobProgress>>,
}
impl TaskProgress {
    fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            phase: Arc::new(Mutex::new(ui::framework::JobProgress {
                phase: "installer-media-phase-review".into(),
                completed: None,
                total: None,
            })),
        }
    }
    fn report(&self, key: &'static str) -> io::Result<()> {
        super::super::migration::check_cancel(&self.cancel)?;
        *self
            .phase
            .lock()
            .map_err(|_| io::Error::other("Preparation progress unavailable"))? =
            ui::framework::JobProgress {
                phase: key.into(),
                completed: None,
                total: None,
            };
        Ok(())
    }
    fn snapshot(&self) -> Option<ui::framework::JobProgress> {
        self.phase.lock().ok().map(|p| p.clone())
    }
    fn archive(
        &self,
        progress: super::super::portable_transfer::ArchiveProgress,
    ) -> io::Result<()> {
        use super::super::portable_transfer::ArchivePhase;
        super::super::migration::check_cancel(&self.cancel)?;
        let key = match progress.phase {
            ArchivePhase::Archiving => "installer-media-phase-archive",
            ArchivePhase::VerifyingArchive => "installer-media-phase-archive-verify",
            ArchivePhase::VerifyingSource => "installer-media-phase-source-verify",
            ArchivePhase::DeliveringCiphertext => "installer-media-phase-ciphertext",
        };
        *self
            .phase
            .lock()
            .map_err(|_| io::Error::other("Preparation progress unavailable"))? =
            ui::framework::JobProgress {
                phase: key.into(),
                completed: (progress.phase != ArchivePhase::VerifyingSource)
                    .then_some(progress.completed),
                total: progress.total,
            };
        Ok(())
    }
    fn byte_label(&self) -> Option<String> {
        let progress = self.snapshot()?;
        let size = ui::format::bytes_si(progress.completed?);
        if let Some(total) = progress.total {
            let total = ui::format::bytes_si(total);
            Some(super::super::strings::t_args(
                "installer-media-progress-bytes-total",
                &[("size", (&size).into()), ("total", (&total).into())],
            ))
        } else {
            Some(super::super::strings::t_args(
                "installer-media-progress-bytes",
                &[("size", (&size).into())],
            ))
        }
    }
    fn current(&self) -> Option<String> {
        self.phase.lock().ok().map(|p| p.phase.clone())
    }
}
struct PreparationWork<'a, R> {
    runner: &'a R,
    progress: &'a TaskProgress,
}

#[derive(Default)]
pub(crate) struct Delivery {
    artifact: Option<Arc<PreparedMedia>>,
    target: Option<imager_backend::imager_model::TargetDrive>,
    worker: Option<std::sync::mpsc::Receiver<io::Result<Delivered>>>,
    joined: Option<std::thread::JoinHandle<()>>,
    cancel: Option<Arc<AtomicBool>>,
    receipt: Option<DeliveryReceipt>,
    awaiting_setup_receipt: Option<DeliveryReceipt>,
    home_receipt: Option<super::super::portable_home_media::Expectation>,
    state: &'static str,
    task: Option<TaskProgress>,
    shown_phase: Option<ui::framework::JobProgress>,
}
/// Raw-writer admission checks only retirement for this exact target identity.
pub(crate) fn pending_target(stable_id: &str) -> bool {
    delivery_retirement::pending(&format!("target:{stable_id}"))
}
pub(crate) fn finish_target(stable_id: &str) -> io::Result<()> {
    delivery_retirement::finish_pending(&format!("target:{stable_id}"))
}
impl Delivery {
    /// Final host cleanup after frames stop; regular close admission defers instead.
    pub(crate) fn finish_close(&mut self) -> io::Result<()> {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        let key = self.writer_key();
        if let Some(joined) = self.joined.take() {
            if key.is_none() {
                let result = joined.join();
                drop((self.worker.take(), self.artifact.take(), self.cancel.take()));
                return result.map_err(|_| {
                    io::Error::other(crate::setup::strings::t("installer-media-worker-stopped"))
                });
            }
            delivery_retirement::retain(
                key.clone(),
                joined,
                self.worker.take(),
                self.artifact.take(),
                self.cancel.take(),
            );
        }
        match key {
            Some(key) => delivery_retirement::finish_pending(&key),
            None => Ok(()),
        }
    }

    pub fn with_artifact(artifact: Arc<PreparedMedia>) -> Self {
        Self {
            artifact: Some(artifact),
            state: "waiting-for-write",
            target: None,
            worker: None,
            joined: None,
            cancel: None,
            receipt: None,
            awaiting_setup_receipt: None,
            home_receipt: None,
            task: None,
            shown_phase: None,
        }
    }
    pub fn has_artifact(&self, artifact: &Arc<PreparedMedia>) -> bool {
        self.artifact
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, artifact))
    }
    #[cfg(test)]
    pub(crate) fn start_changed_home_fixture(&mut self, root: std::path::PathBuf) {
        assert!(!self.busy());
        let artifact = self.artifact.clone().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.worker = Some(receiver);
        self.state = "delivering";
        self.joined = Some(std::thread::spawn(move || {
            let result = (|| {
                let media = super::super::portable_home_media::MediaGrant::open(&root)?;
                artifact.home.as_ref().unwrap().lock().unwrap().deliver(
                    &media,
                    &AtomicBool::new(false),
                    &mut |_| Ok(()),
                )?;
                Err(io::Error::other(
                    "Changed-source fixture unexpectedly exported unchanged contents",
                ))
            })();
            let _ = sender.send(result);
        }));
    }
    pub fn artifact(&self) -> Option<Arc<PreparedMedia>> {
        self.artifact.clone()
    }
    pub fn mark_source_changed(&mut self) {
        self.state = "source-changed";
    }
    pub fn mark_source_unavailable(&mut self) {
        self.state = "source-unavailable";
    }
    pub fn review_reason(&self) -> Option<ReturnReason> {
        ReturnReason::from_state(self.state)
    }
    pub fn reserved_data_bytes(&self) -> u64 {
        self.artifact.as_ref().map_or(0, |artifact| {
            let ordinary = artifact.reserved_data_bytes.load(Ordering::Acquire);
            artifact
                .pending_original_declaration()
                .map_or(ordinary, |pending| {
                    ordinary.max(
                        artifact
                            .capacity_with_home(pending.sealed_reservation())
                            .unwrap_or(u64::MAX),
                    )
                })
        })
    }
    pub fn expectation(&self) -> Option<&imager_backend::imager_model::PreparedSetupExpectation> {
        self.artifact
            .as_ref()
            .map(|artifact| artifact.expectation())
    }
    pub fn requires_data_partition(&self) -> bool {
        self.artifact.as_ref().is_some_and(|artifact| {
            artifact.requires_original_export()
                || artifact.home.is_some()
                || artifact.applications.is_some()
                || artifact.browser.is_some()
        })
    }
    pub fn writing(&mut self) {
        self.target = None;
        self.receipt = None;
        self.awaiting_setup_receipt = None;
        self.home_receipt = None;
        self.state = "waiting-for-write";
    }
    pub fn written(&mut self, target: imager_backend::imager_model::TargetDrive) {
        self.target = Some(target);
        self.receipt = None;
        self.awaiting_setup_receipt = None;
        self.home_receipt = None;
        self.state = "waiting-for-filesystem";
    }
    /// Only a verified pending inbox delivery permits the boot continuation.
    pub(crate) fn awaiting_original_boot(&self) -> bool {
        self.state == "awaiting-offline-export"
            && self.receipt.is_none()
            && self.home_receipt.is_none()
            && self.artifact.as_ref().is_some_and(|artifact| {
                artifact.requires_original_export()
                    && self.awaiting_setup_receipt.as_ref().is_some_and(|receipt| {
                        receipt.sealed_bytes == artifact.expectation().sealed_bytes
                            && receipt.ciphertext_sha256 == artifact.expectation().ciphertext_sha256
                    })
            })
    }
    pub(crate) fn original_boot_drive(&self) -> Option<String> {
        self.awaiting_original_boot()
            .then_some(self.target.as_ref())
            .flatten()
            .map(|target| {
                if target.model.trim().is_empty() {
                    target.dev_path()
                } else {
                    target.model.clone()
                }
            })
    }
    pub fn pending(&self) -> bool {
        self.artifact.is_some() && self.receipt.is_none()
    }
    fn writer_key(&self) -> Option<String> {
        self.artifact
            .as_ref()
            .and_then(|artifact| artifact.pending_original_declaration())
            .map(|pending| format!("target:{}", pending.media().target_stable_id))
            .or_else(|| {
                self.target
                    .as_ref()
                    .and_then(|target| target.stable_id.as_ref())
                    .map(|id| format!("target:{id}"))
            })
            .or_else(|| {
                self.artifact
                    .as_ref()
                    .map(|artifact| format!("setup:{}", artifact.manifest.setup_binding))
            })
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
            || self
                .writer_key()
                .is_some_and(|key| delivery_retirement::pending(&key))
    }
    pub fn start(&mut self, root: std::path::PathBuf) -> Result<(), &'static str> {
        self.start_delivery(Some(root))
    }
    pub(crate) fn requires_offline_export(&self) -> bool {
        self.artifact
            .as_ref()
            .is_some_and(|artifact| artifact.requires_original_export())
    }
    /// Producer continuation after the selected image has been written and
    /// verified. The owned mount and private media admission stay in one worker
    /// until delivery, readback and unmount have all finished.
    pub(crate) fn start_awaiting_offline_export_automatic(
        &mut self,
        review: Arc<offline::OfflineMediaReview>,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-media-delivery-busy");
        }
        if let Some(reason) = self.review_reason() {
            return Err(reason.error_key());
        }
        let artifact = self
            .artifact
            .clone()
            .ok_or("installer-media-not-prepared")?;
        let target = self.target.clone().ok_or("installer-media-not-written")?;
        if !artifact.requires_original_export() {
            return Err("installer-media-delivery-failed");
        }
        let task = TaskProgress::new();
        let cancel = task.cancel.clone();
        let worker_cancel = cancel.clone();
        self.task = Some(task.clone());
        self.shown_phase = None;
        self.receipt = None;
        self.awaiting_setup_receipt = None;
        self.home_receipt = None;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.worker = Some(receiver);
        self.cancel = Some(cancel);
        self.state = "delivering";
        self.joined = Some(std::thread::spawn(move || {
            let result = task
                .report("installer-media-phase-filesystem")
                .and_then(|()| {
                    review.with_protected_delivery(&target, &artifact, &worker_cancel, |grant| {
                        artifact.deliver_awaiting_offline_inbox(grant, &worker_cancel)
                    })
                })
                .map(Delivered::AwaitingOfflineExport);
            let _ = sender.send(result);
        }));
        Ok(())
    }
    pub fn start_automatic(&mut self) -> Result<(), &'static str> {
        self.start_delivery(None)
    }
    fn start_delivery(&mut self, root: Option<std::path::PathBuf>) -> Result<(), &'static str> {
        if self.requires_offline_export() {
            return Err("installer-original-grant-required");
        }
        if self.busy() {
            return Err("installer-media-delivery-busy");
        }
        if let Some(reason) = self.review_reason() {
            return Err(reason.error_key());
        }
        let artifact = self
            .artifact
            .clone()
            .ok_or("installer-media-not-prepared")?;
        let target = self.target.clone().ok_or("installer-media-not-written")?;
        if self.receipt.is_some() {
            return Err("installer-media-already-delivered");
        }
        let task = TaskProgress::new();
        let cancel = task.cancel.clone();
        let worker_cancel = cancel.clone();
        self.task = Some(task.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.worker = Some(receiver);
        self.cancel = Some(cancel);
        self.state = "delivering";
        self.joined = Some(std::thread::spawn(move || {
            let write = |grant: &MediaFilesystemGrant| {
                artifact.deliver_all_reporting(grant, &worker_cancel, Some(&task))
            };
            let _ = task.report("installer-media-phase-filesystem");
            let result = if let Some(root) = root {
                MediaFilesystemGrant::resolve(&target, &root).and_then(|grant| write(&grant))
            } else if artifact.home.is_some()
                || artifact.applications.is_some()
                || artifact.browser.is_some()
            {
                super::super::media_setup::automatic::deliver_home(&target, &worker_cancel, write)
            } else {
                super::super::media_setup::automatic::deliver(&target, &worker_cancel, write)
            };
            let _ = sender.send(result);
        }));
        Ok(())
    }
    pub fn progress_label(&self) -> Option<String> {
        self.task.as_ref()?.byte_label()
    }
    pub fn cancel(&self) -> Result<(), &'static str> {
        self.cancel
            .as_ref()
            .ok_or("installer-media-delivery-not-running")?
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.worker else {
            return false;
        };
        let phase = self.task.as_ref().and_then(TaskProgress::snapshot);
        if self
            .joined
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            let changed = phase != self.shown_phase;
            self.shown_phase = phase;
            return changed;
        }
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let phase = self.task.as_ref().and_then(TaskProgress::snapshot);
                let changed = phase != self.shown_phase;
                self.shown_phase = phase;
                return changed;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::other("Delivery stopped"))
            }
        };
        self.worker = None;
        let result = if self
            .joined
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            Err(io::Error::other("Delivery worker stopped"))
        } else {
            result
        };
        let cancelled = self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Acquire));
        self.cancel = None;
        self.task = None;
        if cancelled {
            self.state = "cancelled";
            return true;
        }
        match result {
            Ok(Delivered::Complete { setup, home }) => {
                self.receipt = Some(setup);
                self.awaiting_setup_receipt = None;
                self.home_receipt = home;
                self.state = "delivered";
            }
            Ok(Delivered::AwaitingOfflineExport(pending)) => {
                // This is a receipt for the SMALL SETUP ONLY. No Home receipt
                // or completed-media outcome is implied by its existence.
                if self.artifact.as_ref().is_none_or(|artifact| {
                    artifact.pending_original_declaration().as_ref() != Some(&pending.declaration)
                }) {
                    self.receipt = None;
                    self.awaiting_setup_receipt = None;
                    self.home_receipt = None;
                    self.state = "failed";
                    return true;
                }
                self.receipt = None;
                self.awaiting_setup_receipt = Some(pending.setup);
                self.home_receipt = None;
                self.state = "awaiting-offline-export";
            }
            Err(error)
                if super::super::portable_home_media::source_review_error(&error).is_some() =>
            {
                self.state = if super::super::portable_home_media::source_changed(&error) {
                    "source-changed"
                } else {
                    "source-unavailable"
                };
            }
            Err(_) => self.state = "failed",
        }
        true
    }
    pub fn needs_source_review(&self) -> bool {
        ReturnReason::from_state(self.state).is_some()
    }
    pub fn failed(&self) -> bool {
        matches!(self.state, "failed" | "cancelled")
    }
    pub fn projection(&self) -> serde_json::Value {
        let progress = self.task.as_ref().and_then(TaskProgress::snapshot);
        serde_json::json!({"state":if self.state.is_empty(){"none"}else{self.state},
            "next_step":self.awaiting_original_boot().then_some("boot-original-computer"),
            "receipt":self.receipt.as_ref().map(DeliveryReceipt::expectation),
            "awaiting_setup_receipt":self.awaiting_setup_receipt.as_ref().map(DeliveryReceipt::expectation),
            "complete":self.state == "delivered" && self.receipt.is_some() && self.awaiting_setup_receipt.is_none(),
            "home_receipt":self.home_receipt,
            "phase":self.task.as_ref().and_then(|t| if t.cancel.load(Ordering::Acquire) {Some("installer-media-phase-cancelling".into())} else {progress.as_ref().map(|p|p.phase.clone())}),
            "completed":progress.as_ref().and_then(|p|p.completed),
            "total":progress.as_ref().and_then(|p|p.total),
            "cancelling":self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Acquire))})
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(joined) = self.joined.take() {
            delivery_retirement::retain(
                self.writer_key(),
                joined,
                self.worker.take(),
                self.artifact.take(),
                self.cancel.take(),
            );
        }
    }
}

#[derive(Default)]
pub(crate) struct Controller {
    worker: Option<std::sync::mpsc::Receiver<io::Result<Arc<PreparedMedia>>>>,
    prepared: Option<Arc<PreparedMedia>>,
    joined: Option<std::thread::JoinHandle<()>>,
    state: &'static str,
    task: Option<TaskProgress>,
    shown_phase: Option<ui::framework::JobProgress>,
    exported: Arc<Mutex<Option<serde_json::Value>>>,
    capacity_review: Arc<Mutex<Option<(String, super::super::staging::plan::Plan)>>>,
}
impl Controller {
    /// A gated unprivileged worker exercises the real cancel and join path.
    #[cfg(test)]
    pub(super) fn start_cancellation_fixture(
        &mut self,
        scratch: std::path::PathBuf,
    ) -> super::preparation_navigation_tests::WorkerControl {
        use super::preparation_navigation_tests::WorkerControl;
        let (probe, probed) = std::sync::mpsc::channel();
        let (receipt, cancelled) = std::sync::mpsc::channel();
        let (finish, finishing) = std::sync::mpsc::channel();
        let (sender, receiver) = std::sync::mpsc::channel();
        let task = TaskProgress::new();
        task.archive(super::super::portable_transfer::ArchiveProgress {
            phase: super::super::portable_transfer::ArchivePhase::Archiving,
            completed: 4096,
            total: Some(8192),
        })
        .unwrap();
        self.task = Some(task.clone());
        self.state = "preparing";
        self.worker = Some(receiver);
        self.joined = Some(std::thread::spawn(move || {
            probed.recv().unwrap();
            let error = super::super::migration::check_cancel(&task.cancel).unwrap_err();
            receipt.send(error.kind()).unwrap();
            finishing.recv().unwrap();
            std::fs::remove_file(scratch).unwrap();
            assert!(sender.send(Err(error)).is_ok());
        }));
        WorkerControl {
            probe,
            cancelled,
            finish,
        }
    }

    pub(in crate::setup) fn start_with_selections(
        &mut self,
        identity: Arc<PreparedIdentity>,
        accounts: Option<super::accounts::PreparedAccounts>,
        selections: MediaSelections,
        password: Zeroizing<String>,
        profile: Option<prepared_setup::PublicInstallProfile>,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-identity-busy");
        }
        *self
            .capacity_review
            .lock()
            .map_err(|_| "installer-media-prepare-failed")? = None;
        self.prepared = None;
        *self
            .exported
            .lock()
            .map_err(|_| "installer-media-prepare-failed")? = None;
        self.state = "preparing";
        let (sender, receiver) = std::sync::mpsc::channel();
        self.worker = Some(receiver);
        let task = TaskProgress::new();
        self.task = Some(task.clone());
        self.joined = Some(std::thread::spawn(move || {
            let binding = uuid::Uuid::new_v4().to_string();
            let runner = super::super::flatpak_transfer::portable::CancellableOfflineRunner(
                task.cancel.clone(),
            );
            let result = PreparedMedia::prepare_with_work(
                &identity,
                accounts,
                selections,
                password,
                binding,
                profile,
                &PreparationWork {
                    runner: &runner,
                    progress: &task,
                },
            );
            let _ = sender.send(result);
        }));
        Ok(())
    }
    pub fn review_computer_belongings(
        &mut self,
        prepared: Arc<PreparedMedia>,
        intent: super::super::migration_intent::MigrationIntent,
        target_bytes: u64,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-transfer-busy");
        }
        if target_bytes < intent.application_minimum() {
            return Err("installer-preview-storage-unavailable");
        }
        *self
            .exported
            .lock()
            .map_err(|_| "installer-media-prepare-failed")? = None;
        self.state = "reviewing";
        let held_review = self.capacity_review.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let task = TaskProgress::new();
        self.task = Some(task.clone());
        self.worker = Some(receiver);
        // No caller supplies a Plan or disk. The canonical elevated probe
        // rediscovers this namespace's system disk; registration separately
        // reattests the actual original source against the computed plan.
        self.joined = Some(std::thread::spawn(move || {
            let result = (|| {
                task.report("installer-media-phase-review")?;
                let facts = super::super::staging::capacity_probe::review_with_cancel(&task.cancel);
                super::super::migration::check_cancel(&task.cancel)?;
                let basis = facts
                    .basis
                    .as_ref()
                    .ok_or_else(|| io::Error::other("Actual system disk geometry unavailable"))?;
                let (operation, plan) = {
                    let mut held = held_review.lock().map_err(|_| {
                        io::Error::other("Actual capacity review custody unavailable")
                    })?;
                    if let Some((operation, plan)) = &*held {
                        if &plan.basis != basis
                            || plan
                                .target_extent
                                .lbas()
                                .checked_mul(basis.disk.sector_bytes)
                                != Some(target_bytes)
                        {
                            return Err(io::Error::other(
                                "Prepared capacity changed; review belongings again",
                            ));
                        }
                        (operation.clone(), plan.clone())
                    } else {
                        let plan = super::super::staging::plan::compute(
                            basis,
                            super::storage::requirements(target_bytes),
                        )
                        .map_err(|_| {
                            io::Error::other("Actual source capacity cannot hold preparation")
                        })?;
                        let operation = uuid::Uuid::new_v4().to_string();
                        // Preserve the exact observed plan/nonces before any
                        // fallible actual registration, including on retry.
                        *held = Some((operation.clone(), plan.clone()));
                        (operation, plan)
                    }
                };
                let reservation = plan
                    .target_extent
                    .lbas()
                    .checked_mul(plan.basis.disk.sector_bytes)
                    .ok_or_else(|| io::Error::other("Actual capacity overflows"))?;
                let bound = prepared.prepare_computer_applications(
                    &intent,
                    &operation,
                    &plan,
                    reservation,
                    &task.cancel,
                )?;
                // A completed registration cannot turn changed geometry into
                // a ready export, even if its ciphertext was already produced.
                let current =
                    super::super::staging::capacity_probe::review_with_cancel(&task.cancel);
                super::super::migration::check_cancel(&task.cancel)?;
                if current.basis.as_ref() != Some(basis) {
                    return Err(io::Error::other(
                        "System disk changed during preparation review",
                    ));
                }
                bound.validate_for_write(&task.cancel)?;
                Ok(bound)
            })();
            let _ = sender.send(result);
        }));
        Ok(())
    }

    pub fn start_belongings_export(
        &mut self,
        prepared: Arc<PreparedMedia>,
        destination: std::path::PathBuf,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-transfer-busy");
        }
        *self
            .exported
            .lock()
            .map_err(|_| "installer-media-prepare-failed")? = None;
        self.state = "exporting";
        let (sender, receiver) = std::sync::mpsc::channel();
        let task = TaskProgress::new();
        self.task = Some(task.clone());
        self.worker = Some(receiver);
        let exported = self.exported.clone();
        self.joined = Some(std::thread::spawn(move || {
            let result = (|| {
                task.report("installer-media-phase-setup")?;
                let info = prepared.export_belongings(&destination, &task.cancel)?;
                *exported
                    .lock()
                    .map_err(|_| io::Error::other("Export receipt unavailable"))? =
                    Some(serde_json::to_value(info).map_err(io::Error::other)?);
                Ok(prepared)
            })();
            let _ = sender.send(result);
        }));
        Ok(())
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub fn cancel(&self) -> Result<(), &'static str> {
        let task = self.task.as_ref().ok_or("installer-media-not-preparing")?;
        task.cancel.store(true, Ordering::Release);
        Ok(())
    }
    pub fn phase_key(&self) -> Option<String> {
        self.task.as_ref().map(|t| {
            if t.cancel.load(Ordering::Acquire) {
                "installer-media-phase-cancelling".into()
            } else {
                t.current()
                    .unwrap_or_else(|| "installer-media-phase-review".into())
            }
        })
    }
    pub fn progress_label(&self) -> Option<String> {
        self.task.as_ref()?.byte_label()
    }
    pub fn take_progress_changed(&mut self) -> bool {
        let mut phase = self.task.as_ref().and_then(TaskProgress::snapshot);
        if let Some(snapshot) = &mut phase {
            snapshot.phase = self.phase_key().unwrap_or_else(|| snapshot.phase.clone());
        }
        let changed = phase != self.shown_phase;
        self.shown_phase = phase;
        changed
    }
    pub fn poll(&mut self) -> Option<Result<Arc<PreparedMedia>, &'static str>> {
        if self
            .joined
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return None;
        }
        let result = match self.worker.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::other("Preparation stopped"))
            }
        };
        self.worker = None;
        let result = if self
            .joined
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            Err(io::Error::other("Preparation worker stopped"))
        } else {
            result
        };
        let cancelled = self
            .task
            .as_ref()
            .is_some_and(|t| t.cancel.load(Ordering::Acquire));
        self.task = None;
        if cancelled {
            self.state = "cancelled";
            return Some(Err("installer-media-preparation-cancelled"));
        }
        match result {
            Ok(prepared) => {
                self.prepared = Some(prepared.clone());
                self.state = "prepared";
                Some(Ok(prepared))
            }
            Err(error)
                if super::super::portable_home_media::source_review_error(&error).is_some() =>
            {
                let reason = if super::super::portable_home_media::source_changed(&error) {
                    ReturnReason::ReviewedHomeChanged
                } else {
                    ReturnReason::ReviewedHomeUnavailable
                };
                self.state = reason.state();
                Some(Err(reason.error_key()))
            }
            Err(error)
                if error.get_ref().is_some_and(|inner| {
                    inner.is::<super::super::service_sessions::source::SourceError>()
                }) =>
            {
                self.state = "session-reconnect";
                Some(Err("installer-service-session-reconnect"))
            }
            Err(error) => {
                // Preserve the worker's cause in test output; the public surface
                // deliberately exposes only the localized, non-secret error key.
                #[cfg(test)]
                eprintln!("installer media preparation failed: {error}");
                #[cfg(not(test))]
                let _ = error;
                self.state = "failed";
                Some(Err("installer-media-prepare-failed"))
            }
        }
    }
    pub fn require_source_review(&mut self, reason: ReturnReason) {
        self.prepared = None;
        self.state = reason.state();
    }
    pub fn review_reason(&self) -> Option<ReturnReason> {
        ReturnReason::from_state(self.state)
    }
    pub fn needs_source_review(&self) -> bool {
        ReturnReason::from_state(self.state).is_some()
    }
    pub fn active_home_projection(&self) -> Option<serde_json::Value> {
        self.prepared
            .as_ref()
            .and_then(|prepared| prepared.home_projection())
            .map(|mut projection| {
                if self.needs_source_review() {
                    projection["capture"] = "unavailable".into();
                }
                projection
            })
    }
    pub fn projection(&self) -> serde_json::Value {
        let progress = self.task.as_ref().and_then(TaskProgress::snapshot);
        serde_json::json!({"state":if self.state.is_empty(){"idle"}else{self.state},
            "artifact":self.prepared.as_ref().map(|prepared|prepared.projection()),
            "exported":if !self.busy() && self.state == "prepared" {self.exported.lock().ok().and_then(|held|held.clone())} else {None},
            "phase":if self.task.as_ref().is_some_and(|t|t.cancel.load(Ordering::Acquire)){Some("installer-media-phase-cancelling".to_string())}else{progress.as_ref().map(|p|p.phase.clone())},
            "completed":progress.as_ref().and_then(|p|p.completed),
            "total":progress.as_ref().and_then(|p|p.total),
            "cancelling":self.task.as_ref().is_some_and(|t| t.cancel.load(Ordering::Acquire))})
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.cancel.store(true, Ordering::Release);
        }
    }
}

use super::prepared_media::{MediaSelections, PortableAppsSelection};
#[path = "prepared_media/fresh.rs"]
mod fresh;
#[path = "prepared_media/offline.rs"]
pub(crate) mod offline;
#[cfg(target_os = "linux")]
#[path = "prepared_media/original_copy.rs"]
mod original_copy;
#[cfg(test)]
#[path = "prepared_media/recovery_tests.rs"]
mod recovery_tests;

struct PreparedBrowser {
    source: super::browser::SelectedSources,
    plan: imager_backend::browser_selection::Plan,
    expectation: super::super::browser_transfer::Expectation,
    archive: Mutex<super::super::portable_transfer::ImportReview>,
}
struct PreparedApplications {
    source: Arc<super::super::flatpak_transfer::portable::PortableFlatpakSource>,
    expectation: super::super::flatpak_transfer::portable::Expectation,
    archive: Mutex<super::super::portable_transfer::ImportReview>,
}
pub(crate) struct NativePreparedMedia {
    _custody: imager_backend::session::BackendSession,
    delivery: Mutex<PreparedSetupDelivery>,
    public_key: [u8; 32],
    account_ids: Vec<String>,
    sealed_bytes: u64,
    reserved_data_bytes: AtomicU64,
    expectation: imager_backend::imager_model::PreparedSetupExpectation,
    install_profile: Option<prepared_setup::PublicInstallProfile>,
    retained: Option<super::super::migration_intent::MigrationIntent>,
    pending_applications_intent: Option<super::super::migration_intent::MigrationIntent>,
    home: Option<Arc<Mutex<HomePreparation>>>,
    home_preview:
        Arc<Mutex<Option<(super::super::portable_transfer::ExportSummary, &'static str)>>>,
    applications: Option<Arc<PreparedApplications>>,
    browser: Option<Arc<PreparedBrowser>>,
    manifest: Manifest,
    current_identity: Option<Arc<tilo_identity_host::enrolled_transport::CurrentDeviceReference>>,
    browser_password: Option<Zeroizing<Vec<u8>>>,
    original_password: Zeroizing<Vec<u8>>,
    browser_frozen: bool,
    delivery_started: std::sync::atomic::AtomicBool,
    recovery: Mutex<
        Vec<(
            Arc<super::super::migration_recovery::Evidence>,
            super::super::staging::plan::Plan,
        )>,
    >,
    transfer_generation: Mutex<Option<Arc<PreparedMedia>>>,
}
struct HomePreparation {
    source: Arc<super::super::portable_home_media::HomeSource>,
    password: Zeroizing<Vec<u8>>,
    binding: String,
    receipt: Option<super::super::portable_home_media::Expectation>,
    reservation: u64,
}
impl HomePreparation {
    fn deliver(
        &mut self,
        media: &super::super::portable_home_media::MediaGrant,
        cancel: &AtomicBool,
        observer: &mut super::super::portable_transfer::Observer<'_>,
    ) -> io::Result<super::super::portable_home_media::Expectation> {
        if let Some(receipt) = &self.receipt {
            self.source.validate_existing_reporting(
                media,
                receipt,
                &self.binding,
                &self.password,
                cancel,
                observer,
            )?;
        } else {
            self.receipt = Some(self.source.export_to_reporting(
                media,
                &self.binding,
                &self.binding,
                self.password.clone(),
                cancel,
                observer,
            )?);
        }
        let receipt = self
            .receipt
            .as_ref()
            .ok_or_else(|| io::Error::other("Portable home receipt missing"))?;
        media.publish_expectation(receipt, cancel)?;
        Ok(receipt.clone())
    }
}
/// This result deliberately has no Home expectation or ready state.
/// Its ciphertext still requires the native offline exporter before installation.
pub(in crate::setup) struct AwaitingOfflineExport {
    pub setup: DeliveryReceipt,
    pub declaration: prepared_setup::OriginalSelectionPlan,
}
enum Delivered {
    Complete {
        setup: DeliveryReceipt,
        home: Option<super::super::portable_home_media::Expectation>,
    },
    AwaitingOfflineExport(AwaitingOfflineExport),
}
impl PreparedMedia {
    fn pending_original_declaration(&self) -> Option<prepared_setup::OriginalSelectionPlan> {
        self.manifest
            .offline_home
            .clone()
            .map(prepared_setup::OriginalSelectionPlan::Home)
            .or_else(|| {
                self.manifest
                    .offline_applications
                    .clone()
                    .map(prepared_setup::OriginalSelectionPlan::Applications)
            })
    }
    fn requires_original_export(&self) -> bool {
        self.manifest.offline_home.is_some() || self.manifest.offline_applications.is_some()
    }
    #[cfg(test)]
    pub(in crate::setup) fn prepare(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        password: Zeroizing<String>,
        binding: String,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_with_home(identity, accounts, None, password, binding)
    }
    #[cfg(test)]
    pub(in crate::setup) fn prepare_with_home(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        home: Option<Arc<super::super::portable_home_media::HomeSource>>,
        password: Zeroizing<String>,
        binding: String,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_for_install(identity, accounts, home, password, binding, None)
    }
    #[cfg(test)]
    pub(crate) fn prepare_profiled_for_test(
        identity: &PreparedIdentity,
        password: Zeroizing<String>,
        binding: String,
        profile: prepared_setup::PublicInstallProfile,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_for_install(identity, None, None, password, binding, Some(profile))
    }
    #[cfg(test)]
    pub(in crate::setup) fn prepare_for_install(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        home: Option<Arc<super::super::portable_home_media::HomeSource>>,
        password: Zeroizing<String>,
        binding: String,
        profile: Option<prepared_setup::PublicInstallProfile>,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_with_migration(identity, accounts, home, None, password, binding, profile)
    }
    #[cfg(test)]
    pub(in crate::setup) fn prepare_with_migration(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        home: Option<Arc<super::super::portable_home_media::HomeSource>>,
        retained: Option<super::super::migration_intent::MigrationIntent>,
        password: Zeroizing<String>,
        binding: String,
        profile: Option<prepared_setup::PublicInstallProfile>,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_with_selections(
            identity,
            accounts,
            MediaSelections {
                home_applications: false,
                home,
                retained,
                planned_direct: None,
                applications: None,
                browser: None,
                sessions: None,
            },
            password,
            binding,
            profile,
        )
    }
    #[cfg(test)]
    pub(in crate::setup) fn prepare_with_selections(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        selections: MediaSelections,
        password: Zeroizing<String>,
        binding: String,
        profile: Option<prepared_setup::PublicInstallProfile>,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_with_runner(
            identity,
            accounts,
            selections,
            password,
            binding,
            profile,
            &super::super::flatpak_transfer::portable::NativeOfflineRunner,
        )
    }
    #[cfg(test)]
    fn prepare_with_runner(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        selections: MediaSelections,
        password: Zeroizing<String>,
        binding: String,
        profile: Option<prepared_setup::PublicInstallProfile>,
        runner: &impl super::super::flatpak_transfer::portable::OfflineRunner,
    ) -> io::Result<Arc<Self>> {
        Self::prepare_with_work(
            identity,
            accounts,
            selections,
            password,
            binding,
            profile,
            &PreparationWork {
                runner,
                progress: &TaskProgress::new(),
            },
        )
    }
    fn prepare_with_work(
        identity: &PreparedIdentity,
        accounts: Option<super::accounts::PreparedAccounts>,
        selections: MediaSelections,
        password: Zeroizing<String>,
        binding: String,
        profile: Option<prepared_setup::PublicInstallProfile>,
        work: &PreparationWork<'_, impl super::super::flatpak_transfer::portable::OfflineRunner>,
    ) -> io::Result<Arc<Self>> {
        work.progress.report("installer-media-phase-review")?;
        let cancel = &*work.progress.cancel;
        let MediaSelections {
            home_applications,
            home,
            retained,
            planned_direct,
            applications,
            browser,
            sessions,
        } = selections;
        if let Some(profile) = &profile {
            profile.validate()?;
        }
        if let Some(home) = &home {
            home.validate_with_cancel(cancel)?;
        }
        imager_backend::startup_password::validate(&password).map_err(io::Error::other)?;
        let custody = imager_backend::session::BackendSession::create(
            imager_backend::session::SessionIntent::PrepareMedia,
        )?;
        let root = custody.path().to_path_buf();
        use std::os::unix::fs::PermissionsExt;
        let tree = root.join("setup");
        std::fs::create_dir(&tree)?;
        std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o700))?;
        let account_ids = accounts
            .as_ref()
            .map_or_else(Vec::new, super::accounts::PreparedAccounts::selected_ids);
        if applications.is_some() {
            work.progress.report("installer-media-phase-applications")?;
        }
        let applications = applications
            .map(|selected| {
                prepare_applications(selected, &root, password.as_bytes(), &binding, work, cancel)
                    .map(Arc::new)
            })
            .transpose()?;
        if browser.is_some() {
            work.progress.report("installer-media-phase-browser")?;
        }
        let browser = browser
            .map(|selection| {
                prepare_browser(
                    selection,
                    &root,
                    password.as_bytes(),
                    &binding,
                    identity.public_key(),
                    cancel,
                )
                .map(Arc::new)
            })
            .transpose()?;
        let mut manifest =
            Manifest::new(binding.clone(), identity.public_key(), account_ids.clone())?;
        manifest.portable_flatpak = applications.as_ref().map(|apps| apps.expectation.clone());
        manifest.browser = browser
            .as_ref()
            .map(|browser| prepared_setup::BrowserHandoff {
                plan: browser.plan.clone(),
                payload: browser.expectation.clone(),
            });
        manifest.install_profile.clone_from(&profile);
        manifest.retained_home = retained
            .as_ref()
            .map(|intent| {
                let plan = intent.for_media()?;
                // Whole-Home Copy stays in the retained native intent until the
                // actual staging operation is registered and password-sealed.
                if plan.categories.home
                    && plan.home_policy == super::super::migration_handoff::HomePolicy::Copy
                {
                    Ok(None)
                } else {
                    prepared_setup::RetainedHomePlan::new(plan).map(Some)
                }
            })
            .transpose()?
            .flatten();
        if manifest.retained_home.as_ref().is_some_and(|home| {
            let source = home.as_plan();
            source.categories.home
                && source.home_policy
                    == super::super::migration_handoff::HomePolicy::AccessReadWrite
        }) {
            // Original writable Files consent is independent of Apps copying.
            // The canonical owner/root exchange binds even a Files-only setup.
            manifest.retained_caller = Some(
                retained
                    .as_ref()
                    .ok_or_else(|| io::Error::other("Shared original Files source grant missing"))?
                    .register_retained_caller(&binding, &identity.public_key(), cancel)?,
            );
        }
        if let Some(plan) = &planned_direct {
            if let Some(retained) = &manifest.retained_home {
                manifest.planned_btrfs_resize =
                    prepared_setup::PlannedBtrfsResize::from_review(retained, plan)?;
            }
        }
        if manifest.retained_home.is_some() && home.is_some() {
            return Err(io::Error::other(
                "Shared original files and portable home copying cannot both be selected",
            ));
        }
        if home.is_some() {
            manifest.portable_home = Some(prepared_setup::PortableHomePlan {
                schema: 1,
                selection: prepared_setup::PortableHomeSelection::FullHome,
                applications: home_applications,
            });
        }
        work.progress.report("installer-media-phase-identity")?;
        if let Some(device_public_key) = identity.current_device_public_key() {
            manifest.identity_kind =
                prepared_setup::IdentityKind::CurrentDevice { device_public_key };
            identity.export_current(
                &tree.join(prepared_setup::IDENTITY_FILE),
                password.as_bytes(),
                &binding,
            )?;
        } else if let Some(device_public_key) = identity.enrolled_device_public_key() {
            manifest.identity_kind =
                prepared_setup::IdentityKind::EnrolledDevice { device_public_key };
            identity.export_enrolled(
                &tree.join(prepared_setup::IDENTITY_FILE),
                password.as_bytes(),
                &binding,
            )?;
        } else {
            identity.export(&tree.join(prepared_setup::IDENTITY_FILE), password.clone())?;
        }
        work.progress.report("installer-media-phase-accounts")?;
        if let Some(accounts) = accounts.filter(|_| !account_ids.is_empty()) {
            accounts
                .export_password_sealed(
                    &tree.join(prepared_setup::ACCOUNTS_FILE),
                    password.as_bytes(),
                    binding.as_bytes(),
                )
                .map_err(|_| io::Error::other("Selected accounts could not be prepared"))?;
        }
        if !account_ids.is_empty() {
            if super::super::account_transfer::PasswordSealedAccounts::inspect_prepared_consumer(
                &tree.join(prepared_setup::ACCOUNTS_FILE),
                password.as_bytes(),
                binding.as_bytes(),
                &account_ids,
            )
            .map_err(|_| io::Error::other("Selected accounts could not be inspected"))?
            {
                manifest.account_custody_protocol = Some(1);
            }
        }
        if let Some(sources) = sessions {
            let captured = sources.capture(identity.public_key(), binding.clone())?;
            let (artifact, declaration) = captured.seal(password.as_bytes())?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(tree.join(prepared_setup::SERVICE_SESSIONS_FILE))?;
            file.write_all(artifact.as_bytes())?;
            file.sync_all()?;
            manifest.service_sessions = Some(declaration);
        }
        work.progress.report("installer-media-phase-setup")?;
        manifest.write(&tree)?;
        let package = root.join("prepared-setup.sealed");
        ExportReview::review(
            &tree,
            &package,
            Purpose::PreparedSetup,
            prepared_setup::LIMITS,
        )?
        .export_reporting(
            Zeroizing::new(password.as_bytes().to_vec()),
            cancel,
            &mut |progress| work.progress.archive(progress),
        )?;
        // Authenticate before permitting any media mutation, not only when the
        // target later tries to import it. The held descriptor binds the bytes.
        work.progress.report("installer-media-phase-verify")?;
        let delivery = PreparedSetupDelivery::review(&package, password.as_bytes(), cancel)?;
        let expectation = delivery.expectation();
        let sealed_bytes = delivery.info().sealed_bytes;
        if let Some(intent) = &retained {
            let plan = intent.for_media()?;
            let expected = if plan.categories.home
                && plan.home_policy == super::super::migration_handoff::HomePolicy::Copy
            {
                None
            } else {
                Some(prepared_setup::RetainedHomePlan::new(plan)?)
            };
            if expected != manifest.retained_home {
                return Err(io::Error::other("The retained-files preparation changed"));
            }
        }
        let home_reservation = home
            .as_ref()
            .map(|source| {
                // Zero is unassigned private preparation state, never an
                // authenticated export budget or a completed archive estimate.
                if source.original_only() {
                    Ok(0)
                } else {
                    source.sealed_reservation()
                }
            })
            .transpose()?
            .unwrap_or(0);
        let home_preview = home
            .as_ref()
            .filter(|source| !source.original_only())
            .map(|source| {
                source
                    .summary()
                    .map(|summary| (summary, source.capture_status()))
            })
            .transpose()?;
        let mut reserved_data_bytes = sealed_bytes;
        for bytes in [
            home_reservation,
            applications
                .as_ref()
                .map_or(0, |apps| apps.expectation.sealed_bytes),
            browser
                .as_ref()
                .map_or(0, |browser| browser.expectation.sealed_bytes),
            128 * 1024, // Bounded public expectations, selection and ownership receipts.
        ] {
            reserved_data_bytes = reserved_data_bytes
                .checked_add(bytes)
                .ok_or_else(|| io::Error::other("Prepared media capacity overflows"))?;
        }
        std::fs::remove_dir_all(&tree)?;
        Ok(Arc::new(Self::from_native(NativePreparedMedia {
            _custody: custody,
            delivery: Mutex::new(delivery),
            public_key: identity.public_key(),
            account_ids,
            sealed_bytes,
            reserved_data_bytes: AtomicU64::new(reserved_data_bytes),
            expectation,
            install_profile: profile,
            retained,
            pending_applications_intent: None,
            applications,
            browser_password: browser
                .as_ref()
                .map(|_| Zeroizing::new(password.as_bytes().to_vec())),
            browser,
            manifest,
            current_identity: identity.current_source(),
            original_password: Zeroizing::new(password.as_bytes().to_vec()),
            browser_frozen: false,
            delivery_started: std::sync::atomic::AtomicBool::new(false),
            recovery: Mutex::new(Vec::new()),
            transfer_generation: Mutex::new(None),
            home_preview: Arc::new(Mutex::new(home_preview)),
            home: home.map(|source| {
                Arc::new(Mutex::new(HomePreparation {
                    source,
                    password: Zeroizing::new(password.as_bytes().to_vec()),
                    binding,
                    receipt: None,
                    reservation: home_reservation,
                }))
            }),
        })))
    }
    fn capacity_with_home(&self, home: u64) -> io::Result<u64> {
        [
            self.sealed_bytes,
            // Offline completion retains the immutable pending parent while
            // publishing a separately encrypted amended setup, never another Home.
            if self.requires_original_export() {
                offline::amended_setup_reservation()?
            } else {
                0
            },
            home,
            self.applications
                .as_ref()
                .map_or(0, |apps| apps.expectation.sealed_bytes),
            self.browser
                .as_ref()
                .map_or(0, |browser| browser.expectation.sealed_bytes),
            128 * 1024,
        ]
        .into_iter()
        .try_fold(0u64, |total, value| {
            total
                .checked_add(value)
                .ok_or_else(|| io::Error::other("Prepared media capacity overflows"))
        })
    }
    fn deliver_awaiting_offline_inbox(
        &self,
        grant: &super::super::media_setup::inbox::ProtectedInboxGrant<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<AwaitingOfflineExport> {
        let declaration = self
            .pending_original_declaration()
            .ok_or_else(|| io::Error::other("No authenticated offline export was requested"))?;
        self.validate_for_write(cancel)?;
        if self.retained.is_some() {
            return Err(io::Error::other(
                "Shared-home custody cannot become offline copy",
            ));
        }
        if self.manifest.portable_flatpak.as_ref()
            != self.applications.as_ref().map(|apps| &apps.expectation)
            || self
                .manifest
                .browser
                .as_ref()
                .map(|browser| &browser.payload)
                != self.browser.as_ref().map(|browser| &browser.expectation)
        {
            return Err(io::Error::other(
                "Selected payload custody differs from sealed declaration",
            ));
        }
        self.delivery_started.store(true, Ordering::Release);
        grant.validate_pending(declaration.media(), &self.expectation)?;
        let media = grant.portable_home_grant()?;
        let app_bytes = self.applications.as_ref().map_or(Ok(0), |apps| {
            apps.source
                .validate_pending_offline_scope(declaration.source())?;
            media.application_allocation(&apps.expectation, cancel)
        })?;
        let browser_bytes = self.browser.as_ref().map_or(Ok(0), |browser| {
            for (source, _) in browser.source.profiles() {
                source.validate()?;
            }
            browser
                .archive
                .lock()
                .map_err(|_| io::Error::other("Browser custody unavailable"))?
                .verify_held_ciphertext(cancel)?;
            media.browser_allocation(&browser.expectation, cancel)
        })?;
        let amended_setup = offline::amended_setup_reservation()?;
        let required = declaration
            .sealed_reservation()
            .checked_add(self.sealed_bytes)
            .and_then(|bytes| bytes.checked_add(amended_setup))
            .and_then(|bytes| bytes.checked_add(app_bytes))
            .and_then(|bytes| bytes.checked_add(browser_bytes))
            .and_then(|bytes| bytes.checked_add(128 * 1024))
            .ok_or_else(|| io::Error::other("Offline media reservation overflows"))?;
        if required > media.limits()?.plaintext_bytes {
            return Err(io::Error::other(
                "Offline export medium has insufficient reserved capacity",
            ));
        }
        super::super::migration::check_cancel(cancel)?;
        if let Some(apps) = &self.applications {
            let mut archive = apps
                .archive
                .lock()
                .map_err(|_| io::Error::other("Portable application custody unavailable"))?;
            media.deliver_applications_reporting(
                &mut archive,
                &apps.expectation,
                cancel,
                &mut |_| Ok(()),
            )?;
        }
        if let Some(browser) = &self.browser {
            let mut archive = browser
                .archive
                .lock()
                .map_err(|_| io::Error::other("Browser custody unavailable"))?;
            media.deliver_browser_reporting(
                &mut archive,
                &browser.expectation,
                cancel,
                &mut |_| Ok(()),
            )?;
        }
        let setup = {
            let mut delivery = self
                .delivery
                .lock()
                .map_err(|_| io::Error::other("Prepared setup custody unavailable"))?;
            delivery.deliver_protected_inbox_reporting(grant, cancel, &mut |_| Ok(()))?
        };
        if let Some(apps) = &self.applications {
            media.publish_applications_selection(&setup, &apps.expectation, cancel)?;
        }
        if let Some(browser) = &self.browser {
            media.publish_browser_selection(&setup, &browser.expectation, cancel)?;
        }
        grant.validate_pending(declaration.media(), &self.expectation)?;
        Ok(AwaitingOfflineExport { setup, declaration })
    }
    /// Export exactly the reviewed ciphertext through a held private parent.
    /// This publishes no installation authority; consumers authenticate it again.
    fn export_belongings(
        &self,
        destination: &std::path::Path,
        cancel: &AtomicBool,
    ) -> io::Result<super::super::portable_transfer::PackageInfo> {
        use std::os::fd::AsRawFd;
        self.validate_for_write(cancel)?;
        let parent = super::super::migration::PinnedDirectory::open_absolute(
            destination.parent().ok_or(io::ErrorKind::InvalidInput)?,
        )?;
        parent.validate_destination()?;
        super::super::destination_security::check_private_file(&parent.file)?;
        if let Some(source) = &self.retained {
            source.validate_artifact_destination(&parent)?;
        }
        let name = destination.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        if name == std::ffi::OsStr::new(".") || name == std::ffi::OsStr::new("..") {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let held_parent =
            std::path::PathBuf::from(format!("/proc/self/fd/{}", parent.file.as_raw_fd()));
        let mut temporary = tempfile::NamedTempFile::new_in(&held_parent)?;
        let info = {
            let mut delivery = self
                .delivery
                .lock()
                .map_err(|_| io::Error::other("Prepared custody unavailable"))?;
            delivery.copy_preparation_ciphertext(temporary.as_file_mut(), cancel)?;
            delivery.info().clone()
        };
        temporary.as_file().sync_all()?;
        parent.validate_destination()?;
        self.validate_for_write(cancel)?;
        if let Some(source) = &self.retained {
            source.validate_artifact_destination(&parent)?;
        }
        // The atomic no-replacement publication also stays under the held
        // directory, even if a same-user actor changes a pathname ancestor.
        temporary
            .persist_noclobber(held_parent.join(name))
            .map_err(|error| error.error)?;
        super::super::migration_durability::sync_directory(&parent.file)?;
        parent.validate_destination()?;
        self.validate_for_write(cancel)?;
        Ok(info)
    }

    pub(in crate::setup) fn native_matches_reviewed_belongings(
        &self,
        identity: &PreparedIdentity,
        profile: &prepared_setup::PublicInstallProfile,
        migration: Option<&super::super::migration_intent::MigrationIntent>,
    ) -> bool {
        self.public_key == identity.public_key()
            && self.install_profile.as_ref() == Some(profile)
            && self.retained.as_ref() == migration
    }

    pub(in crate::setup) fn native_bind_recovery(
        &self,
        evidence: Vec<Arc<super::super::migration_recovery::Evidence>>,
        plan: &super::super::staging::plan::Plan,
    ) -> io::Result<()> {
        if self.delivery_started.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "Recovery review is frozen after delivery begins",
            ));
        }
        for item in &evidence {
            item.validate_plan(plan)?;
        }
        *self
            .recovery
            .lock()
            .map_err(|_| io::Error::other("Recovery custody unavailable"))? = evidence
            .into_iter()
            .map(|item| (item, plan.clone()))
            .collect();
        Ok(())
    }
    pub(crate) fn validate_recovery_for_stage(
        &self,
        actual_plan: &super::super::staging::plan::Plan,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        for (_, reviewed_plan) in self
            .recovery
            .lock()
            .map_err(|_| io::Error::other("Recovery custody unavailable"))?
            .iter()
        {
            if reviewed_plan != actual_plan {
                return Err(io::Error::other(
                    "Installation write ranges changed after recovery review",
                ));
            }
        }
        self.validate_recovery(cancel)
    }
    fn review_recovery(&self) -> io::Result<()> {
        for (evidence, plan) in self
            .recovery
            .lock()
            .map_err(|_| io::Error::other("Recovery custody unavailable"))?
            .iter()
        {
            evidence.validate_plan(plan)?;
        }
        Ok(())
    }
    fn validate_recovery(&self, cancel: &AtomicBool) -> io::Result<()> {
        for (evidence, plan) in self
            .recovery
            .lock()
            .map_err(|_| io::Error::other("Recovery custody unavailable"))?
            .iter()
        {
            evidence.validate_plan(plan)?;
            evidence.validate(cancel).map_err(io::Error::other)?;
        }
        Ok(())
    }
    pub fn native_validate_for_write(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.validate_recovery(cancel)?;
        if let Some(identity) = &self.current_identity {
            identity.validate()?;
        }
        self.delivery
            .lock()
            .map_err(|_| io::Error::other("Prepared setup custody unavailable"))?
            .verify_reviewed_ciphertext(cancel)?;
        #[cfg(target_os = "linux")]
        if let Some(copy) = &self.manifest.original_copy {
            copy.bytes()?;
            self.retained
                .as_ref()
                .ok_or_else(|| io::Error::other("Original staging custody missing"))?
                .validate_staging_copy(copy.source())?;
        }
        #[cfg(target_os = "linux")]
        if self.manifest.computer_applications.is_some() {
            let main = self
                .retained
                .as_ref()
                .ok_or_else(|| io::Error::other("Computer Files custody missing"))?;
            let apps = main
                .computer_applications()?
                .ok_or_else(|| io::Error::other("Computer Apps custody missing"))?;
            let copy = self
                .manifest
                .installed_original_copy_declaration()?
                .ok_or_else(|| io::Error::other("Computer Apps declaration missing"))?;
            apps.validate_staging_copy(copy.source())?;
            if self
                .manifest
                .retained_home
                .as_ref()
                .map(prepared_setup::RetainedHomePlan::as_plan)
                != Some(&main.for_media()?)
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        if let Some(pending) = &self.manifest.offline_applications {
            let intent = self
                .pending_applications_intent
                .as_ref()
                .ok_or_else(|| io::Error::other("Pending selected applications grant missing"))?;
            if self.home.is_some()
                || self.retained.is_some()
                || self.applications.is_some()
                || intent
                    .for_pending_applications()?
                    .original_inventory_selection()?
                    .0
                    != *pending.source()
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            if let Some(browser) = &self.browser {
                for (source, _) in browser.source.profiles() {
                    source.validate()?;
                }
                browser
                    .archive
                    .lock()
                    .map_err(|_| io::Error::other("Browser custody unavailable"))?
                    .verify_held_ciphertext(cancel)?;
            }
            super::super::migration::check_cancel(cancel)?;
            return Ok(());
        }
        if let Some(pending) = &self.manifest.offline_home {
            let home = self
                .home
                .as_ref()
                .ok_or_else(|| io::Error::other("Pending original home grant missing"))?
                .lock()
                .map_err(|_| io::Error::other("Home custody unavailable"))?;
            home.source.validate_offline_scope(pending.source())?;
            offline::validate_payload_scope(self, pending.source(), cancel)?;
            return Ok(());
        }
        if let Some(home) = &self.home {
            let mut home = home
                .lock()
                .map_err(|_| io::Error::other("Home custody unavailable"))?;
            if home.source.original_only() {
                return Err(io::Error::other(
                    "Original source requires authenticated offline preparation",
                ));
            }
            if home.receipt.is_none() && home.source.immutable_capture() {
                // Actual transfer begins with the latest approved contents, not
                // the epoch inspected during earlier personal setup steps.
                let latest = home.source.review_latest(cancel)?;
                let reservation = latest.sealed_reservation()?;
                let total = self.capacity_with_home(reservation)?;
                super::super::migration::check_cancel(cancel)?;
                let summary = latest.summary()?;
                let mut preview = self
                    .home_preview
                    .lock()
                    .map_err(|_| io::Error::other("Home preview unavailable"))?;
                let previous = std::mem::replace(&mut home.source, latest);
                home.reservation = reservation;
                *preview = Some((summary, home.source.capture_status()));
                self.reserved_data_bytes.store(total, Ordering::Release);
                drop(preview);
                previous.release_capture();
            }
            home.source.validate()?;
            // Generations may share the Home lease while their authenticated
            // setup/browser packages differ. Never subtract a shared latest
            // reservation from a generation's older cached total.
            self.reserved_data_bytes.store(
                self.capacity_with_home(home.reservation)?,
                Ordering::Release,
            );
        }
        if let Some(apps) = &self.applications {
            apps.source.validate()?;
            apps.archive
                .lock()
                .map_err(|_| io::Error::other("Application custody unavailable"))?
                .verify_held_ciphertext(cancel)?;
        }
        if let Some(browser) = &self.browser {
            for (source, _) in browser.source.profiles() {
                source.validate()?;
            }
            browser
                .archive
                .lock()
                .map_err(|_| io::Error::other("Browser custody unavailable"))?
                .verify_held_ciphertext(cancel)?;
        }
        Ok(())
    }
    pub(in crate::setup) fn desktop_manifest(&self) -> io::Result<&Manifest> {
        if let Some(identity) = &self.current_identity {
            identity.validate()?;
        }
        self.staging_artifact()?;
        Ok(&self.manifest)
    }
    fn deliver_reporting(
        &self,
        grant: &MediaFilesystemGrant,
        cancel: &AtomicBool,
        observer: &mut super::super::portable_transfer::Observer<'_>,
    ) -> io::Result<DeliveryReceipt> {
        if let Some(intent) = &self.retained {
            intent.for_media()?;
        }
        self.delivery
            .lock()
            .map_err(|_| io::Error::other("Prepared setup custody is unavailable"))?
            .deliver_or_verify_reporting(grant, cancel, observer)
    }
    fn home_projection(&self) -> Option<serde_json::Value> {
        // Drop the Home mutex before taking preview: refresh uses Home→preview.
        let original_only = self
            .home
            .as_ref()
            .and_then(|home| home.lock().ok())
            .is_some_and(|home| home.source.original_only());
        if original_only {
            return Some(
                serde_json::json!({"capture":"awaiting-original-boot", "contents_reviewed":false,
                "sealed_budget":self.manifest.offline_home.as_ref().map(|plan| plan.sealed_reservation())}),
            );
        }
        let preview = self.home_preview.lock().ok()?;
        preview.as_ref().map(|(summary, capture)| {
            serde_json::json!({
                "capture":capture,"files":summary.files,"bytes":summary.bytes
            })
        })
    }
    fn native_projection(&self) -> serde_json::Value {
        serde_json::json!({"public_key":hex::encode(self.public_key),
            "account_ids":self.account_ids,"sealed_bytes":self.sealed_bytes,
            "setup_binding":self.manifest.setup_binding,
            "source_operation":self.manifest.retained_home.as_ref().map(|home|home.as_plan().operation_id.clone()),
            "pending_computer_applications_scope":self.manifest.pending_computer_applications_scope().ok().flatten(),
            "portable_home":self.home.is_some(),
            "awaiting_offline_export":self.requires_original_export(),
            "browser":self.browser.is_some(),
            "shared_original_files":self.retained.as_ref().is_some_and(|intent| intent.source_plan().categories.home),
            "independent_applications":self.manifest.offline_applications.is_some() || self.applications.is_some() || self.retained.as_ref().is_some_and(|intent| intent.source_plan().categories.flatpak)})
    }
    fn deliver_all_reporting(
        &self,
        grant: &MediaFilesystemGrant,
        cancel: &AtomicBool,
        task: Option<&TaskProgress>,
    ) -> io::Result<Delivered> {
        if self.requires_original_export() {
            return Err(io::Error::other(
                "Original export is awaiting explicit offline continuation",
            ));
        }
        let phase = |key| -> io::Result<()> {
            super::super::migration::check_cancel(cancel)?;
            if let Some(task) = task {
                task.report(key)?;
            }
            Ok(())
        };
        phase("installer-media-phase-review")?;
        self.delivery_started.store(true, Ordering::Release);
        let media = (self.home.is_some() || self.applications.is_some() || self.browser.is_some())
            .then(|| grant.portable_home_grant())
            .transpose()?;
        if let Some(media) = &media {
            let app_bytes = self.applications.as_ref().map_or(Ok(0), |apps| {
                media.application_allocation(&apps.expectation, cancel)
            })?;
            let browser_bytes = self.browser.as_ref().map_or(Ok(0), |browser| {
                media.browser_allocation(&browser.expectation, cancel)
            })?;
            if let Some(browser) = &self.browser {
                for (source, _) in browser.source.profiles() {
                    source.validate()?;
                }
                browser
                    .archive
                    .lock()
                    .map_err(|_| io::Error::other("Browser custody unavailable"))?
                    .verify_held_ciphertext(cancel)?;
            }
            let home_bytes = if let Some(home) = &self.home {
                let home = home
                    .lock()
                    .map_err(|_| io::Error::other("Portable home custody unavailable"))?;
                if let Some(receipt) = &home.receipt {
                    home.source.validate_existing(
                        media,
                        receipt,
                        &home.binding,
                        &home.password,
                        cancel,
                    )?;
                    0
                } else {
                    home.source.archive_estimate()?
                }
            } else {
                0
            };
            let required = app_bytes
                .checked_add(home_bytes)
                .and_then(|bytes| bytes.checked_add(browser_bytes))
                .and_then(|bytes| bytes.checked_add(self.sealed_bytes))
                .ok_or_else(|| io::Error::other("Combined portable media allocation overflows"))?;
            if required > media.limits()?.plaintext_bytes {
                return Err(io::Error::other(
                    "The selected medium has insufficient room for home, apps, browser and prepared setup",
                ));
            }
            if let Some(apps) = &self.applications {
                apps.source.validate()?;
                let mut archive = apps
                    .archive
                    .lock()
                    .map_err(|_| io::Error::other("Portable application custody unavailable"))?;
                phase("installer-media-phase-applications")?;
                media.deliver_applications_reporting(
                    &mut archive,
                    &apps.expectation,
                    cancel,
                    &mut |p| task.map_or(Ok(()), |t| t.archive(p)),
                )?;
            }
            if let Some(browser) = &self.browser {
                let mut archive = browser
                    .archive
                    .lock()
                    .map_err(|_| io::Error::other("Browser custody unavailable"))?;
                phase("installer-media-phase-browser")?;
                media.deliver_browser_reporting(
                    &mut archive,
                    &browser.expectation,
                    cancel,
                    &mut |p| task.map_or(Ok(()), |t| t.archive(p)),
                )?;
            }
        }
        if self.home.is_some() {
            phase("installer-media-phase-home")?;
        }
        let home = if let Some(preparation) = &self.home {
            let mut preparation = preparation
                .lock()
                .map_err(|_| io::Error::other("Portable home custody is unavailable"))?;
            let media = grant.portable_home_grant()?;
            Some(preparation.deliver(&media, cancel, &mut |progress| {
                task.map_or(Ok(()), |task| task.archive(progress))
            })?)
        } else {
            None
        };
        phase("installer-media-phase-publish")?;
        let setup = self.deliver_reporting(grant, cancel, &mut |p| {
            task.map_or(Ok(()), |t| t.archive(p))
        })?;
        if let Some(home) = &home {
            grant
                .portable_home_grant()?
                .publish_setup_selection(&setup, home, cancel)?;
        }
        if let Some(apps) = &self.applications {
            grant
                .portable_home_grant()?
                .publish_applications_selection(&setup, &apps.expectation, cancel)?;
        }
        if let Some(browser) = &self.browser {
            grant.portable_home_grant()?.publish_browser_selection(
                &setup,
                &browser.expectation,
                cancel,
            )?;
        }
        Ok(Delivered::Complete { setup, home })
    }
    pub fn native_expectation(&self) -> &imager_backend::imager_model::PreparedSetupExpectation {
        &self.expectation
    }
    pub(crate) fn install_profile(&self) -> Option<&prepared_setup::PublicInstallProfile> {
        self.install_profile.as_ref()
    }
    pub(crate) fn native_account_custody_protocol(&self) -> Option<u32> {
        self.manifest.account_custody_protocol
    }

    pub(crate) fn native_service_session_protocol(&self) -> Option<u32> {
        self.manifest
            .service_sessions
            .as_ref()
            .map(|sessions| sessions.consumer_protocol)
    }
    pub(crate) fn native_applications_protocol(&self) -> Option<u32> {
        if self.manifest.offline_applications.is_some() {
            return Some(2);
        }

        self.manifest
            .portable_flatpak
            .as_ref()
            .and_then(|applications| (applications.consumer_protocol == 2).then_some(2))
    }
    pub(crate) fn native_home_archive_protocol(&self) -> Option<u32> {
        (self.manifest.portable_home.is_some() || self.manifest.portable_flatpak.is_some())
            .then_some(super::super::home_admission::ARCHIVE_PROTOCOL)
    }

    pub(in crate::setup) fn staging_artifact(
        &self,
    ) -> io::Result<super::super::staging::prepared_setup::PreparedArtifact> {
        // UI-side artifact projection checks range custody only. Independent
        // byte rereads run on the download worker before creating the actual
        // stage request, and on media writers at final write admission.
        self.review_recovery()?;
        if self.requires_original_export() {
            return Err(io::Error::other(
                "Pending offline export is not an installable artifact",
            ));
        }
        if self.home.is_some() || self.applications.is_some() {
            return Err(io::Error::other(
                "Portable home is not a direct-install artifact",
            ));
        }
        self.delivery
            .lock()
            .map_err(|_| io::Error::other("Prepared setup custody is unavailable"))?
            .verify_reviewed_ciphertext(&AtomicBool::new(false))?;
        let artifact = super::super::staging::prepared_setup::PreparedArtifact {
            source_path: self
                ._custody
                .path()
                .join("prepared-setup.sealed")
                .to_str()
                .ok_or_else(|| io::Error::other("Prepared setup path is not UTF-8"))?
                .to_owned(),
            expectation: self.expectation.clone(),
            browser: self.browser_staging_artifact()?,
        };
        Ok(artifact)
    }
    fn browser_staging_artifact(
        &self,
    ) -> io::Result<Option<super::super::staging::browser::BrowserArtifact>> {
        let Some(browser) = &self.browser else {
            return Ok(None);
        };
        for (source, _) in browser.source.profiles() {
            source.validate()?;
        }
        browser
            .archive
            .lock()
            .map_err(|_| io::Error::other("Browser custody unavailable"))?
            .verify_held_ciphertext(&AtomicBool::new(false))?;
        let artifact = super::super::staging::browser::BrowserArtifact {
            source_path: self
                ._custody
                .path()
                .join(&browser.expectation.filename)
                .to_str()
                .ok_or_else(|| io::Error::other("Browser custody path is not UTF-8"))?
                .into(),
            expectation: browser.expectation.clone(),
        };
        if !artifact.valid() {
            return Err(io::Error::other("Browser staging artifact is invalid"));
        }
        Ok(Some(artifact))
    }
    #[cfg(test)]
    pub(crate) fn package(&self) -> std::path::PathBuf {
        self._custody.path().join("prepared-setup.sealed")
    }
}

fn prepare_browser(
    source: super::browser::SelectedSources,
    root: &std::path::Path,
    password: &[u8],
    binding: &str,
    identity: [u8; 32],
    cancel: &AtomicBool,
) -> io::Result<PreparedBrowser> {
    use super::super::browser_transfer::{self, ExportRequest};
    let plan = source.plan(binding.into(), identity)?;
    let operation = uuid::Uuid::new_v4().to_string();
    let package = root.join(format!("browser-{operation}.sealed"));
    let expectation = browser_transfer::export(
        &source,
        &plan,
        ExportRequest {
            staging_parent: root,
            package: &package,
            operation: &operation,
            password: Zeroizing::new(password.to_vec()),
        },
        cancel,
    )?;
    let archive = browser_transfer::review(&expectation, &plan, &package, password, cancel)?;
    Ok(PreparedBrowser {
        source,
        plan,
        expectation,
        archive: Mutex::new(archive),
    })
}

fn prepare_applications(
    selection: PortableAppsSelection,
    root: &std::path::Path,
    password: &[u8],
    binding: &str,
    work: &PreparationWork<'_, impl super::super::flatpak_transfer::portable::OfflineRunner>,
    cancel: &AtomicBool,
) -> io::Result<PreparedApplications> {
    use super::super::{
        flatpak_transfer::portable::StageGrant,
        migration::PinnedDirectory,
        portable_transfer::{ImportReview, Purpose},
    };
    let pin = PinnedDirectory::open_absolute(root)?;
    pin.create_child(std::ffi::OsStr::new("applications-stage"))?;
    pin.create_child(std::ffi::OsStr::new("applications-journals"))?;
    let allocation = super::super::portable_home_media::reviewed_directory_limits(pin.clone())?
        .plaintext_bytes
        / 2;
    let stage = root.join("applications-stage");
    let journals = root.join("applications-journals");
    let staged = selection.source.stage(
        work.runner,
        StageGrant {
            root: &stage,
            journals: &journals,
            allocation_bytes: allocation,
            include_user_state: selection.include_user_state,
        },
        cancel,
    )?;
    let operation = uuid::Uuid::new_v4().to_string();
    let package = root.join(format!("applications-{operation}.sealed"));
    let limits = super::super::portable_home_media::reviewed_directory_limits(pin)?;
    let expectation = staged.seal_reporting(
        &package,
        &operation,
        binding,
        Zeroizing::new(password.to_vec()),
        limits,
        cancel,
        &mut |progress| work.progress.archive(progress),
    )?;
    let archive = ImportReview::review(
        &package,
        password,
        Purpose::FlatpakApplications,
        limits,
        cancel,
    )?;
    staged.validate()?;
    std::fs::remove_dir_all(&stage)?;
    std::fs::remove_dir_all(&journals)?;
    Ok(PreparedApplications {
        source: selection.source,
        expectation,
        archive: Mutex::new(archive),
    })
}

#[cfg(test)]
#[path = "prepared_media/capacity_tests.rs"]
mod capacity_tests;
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    fn reseal_pending_fixture(
        value: &mut PreparedMedia,
        identity: &PreparedIdentity,
        root: &std::path::Path,
        password: &str,
    ) -> (std::path::PathBuf, DeliveryReceipt) {
        let stage = root.join("pending-stage");
        std::fs::create_dir(&stage).unwrap();
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
        identity
            .export(
                &stage.join(prepared_setup::IDENTITY_FILE),
                Zeroizing::new(password.into()),
            )
            .unwrap();
        value.manifest.write(&stage).unwrap();
        let package = root.join("pending.sealed");
        ExportReview::review(
            &stage,
            &package,
            Purpose::PreparedSetup,
            prepared_setup::LIMITS,
        )
        .unwrap()
        .export(
            Zeroizing::new(password.as_bytes().to_vec()),
            &AtomicBool::new(false),
        )
        .unwrap();
        let authenticated = prepared_setup::PreparedSetupReview::open(
            &package,
            password.as_bytes(),
            &root.join("pending-review"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(authenticated.manifest(), &value.manifest);
        assert!(authenticated.require_completed_export().is_err());
        let reviewed =
            PreparedSetupDelivery::review(&package, password.as_bytes(), &AtomicBool::new(false))
                .unwrap();
        value.expectation = reviewed.expectation();
        value.sealed_bytes = reviewed.info().sealed_bytes;
        value.delivery = Mutex::new(reviewed);
        let receipt = DeliveryReceipt {
            relative_path: std::path::Path::new(super::super::super::media_setup::DIRECTORY)
                .join(super::super::super::media_setup::FILE),
            sealed_bytes: value.expectation.sealed_bytes,
            ciphertext_sha256: value.expectation.ciphertext_sha256.clone(),
        };
        (package, receipt)
    }
    fn pending_media_fixture(
        root: &std::path::Path,
    ) -> (
        Arc<PreparedMedia>,
        AwaitingOfflineExport,
        std::path::PathBuf,
    ) {
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let caller = crate::setup::home_capture::original_caller::fixture_source_owner(&home);
        let operation = uuid::Uuid::new_v4().to_string();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let password = "several unrelated destination installation words";
        let mut artifact = PreparedMedia::prepare(
            &identity,
            None,
            Zeroizing::new(password.into()),
            operation.clone(),
        )
        .unwrap();
        // A deterministic user-authenticated request fixture; kernel source
        // authority is qualified only by the opt-in GPT-backed native test.
        let source: super::super::super::migration_handoff::Plan = serde_json::from_value(serde_json::json!({
            "schema": 1, "operation_id": operation,
            "categories": {"home": true, "flatpak": false, "accounts": false},
            "home": {"volume": {
                "disk_guid": "11111111-1111-4111-8111-111111111111",
                "disk_bytes": 64u64 * 1024 * 1024, "sector_bytes": 512,
                "partition_guid": "22222222-2222-4222-8222-222222222222",
                "first_lba": 2048, "last_lba": 65535,
                "filesystem_uuid": "33333333-3333-4333-8333-333333333333",
                "filesystem": "ext4"
            }, "relative": crate::setup::migration_store::NativePath::encode(std::path::Path::new("home/person")),
            "inode": 1234, "original_uid": caller.uid(), "subvolume": null}
        })).unwrap();
        let (_media_directory, _media_path, raw_media) = crate::media_layout::tests::disk();
        let media = crate::media_layout::provision(
            &raw_media,
            4 * 1024 * 1024,
            64 * 1024 * 1024,
            "reviewed-medium",
            &|| false,
        )
        .unwrap();
        let declaration = prepared_setup::OfflineHomePlan::new(
            source,
            operation,
            identity.public_key(),
            media,
            caller,
            1024 * 1024,
        )
        .unwrap();
        // Build genuine authenticated pending ciphertext. The fixture supplies
        // the proposed reseal boundary; it does not simulate physical media.
        let value = Arc::get_mut(&mut artifact).unwrap();
        value.manifest.portable_home = Some(prepared_setup::PortableHomePlan {
            schema: 1,
            selection: prepared_setup::PortableHomeSelection::FullHome,
            applications: false,
        });
        value.manifest.offline_home = Some(declaration.clone());
        let (package, receipt) = reseal_pending_fixture(value, &identity, root, password);
        (
            artifact,
            AwaitingOfflineExport {
                setup: receipt,
                declaration: prepared_setup::OriginalSelectionPlan::Home(declaration),
            },
            package,
        )
    }
    #[test]
    fn awaiting_offline_export_public_state_never_becomes_completed_delivery() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _facts = super::super::main_story_tests::source_fixture(&root);
        let (artifact, awaiting, package) = pending_media_fixture(&root);
        let admitted = artifact.capacity_with_home(1024 * 1024).unwrap();
        let actual_pending_bytes = std::fs::metadata(&package).unwrap().len();
        assert_eq!(actual_pending_bytes, artifact.expectation.sealed_bytes);
        assert_eq!(
            admitted,
            actual_pending_bytes
                + 1024 * 1024
                + prepared_setup::LIMITS.sealed_bound().unwrap()
                + 128 * 1024
        );

        artifact
            .reserved_data_bytes
            .store(admitted, Ordering::Release);
        let mut delivery = Delivery::with_artifact(artifact.clone());
        // Existing reviewed allocation already includes Home: defer it, never
        // add a second copy of the same Home reservation.
        assert_eq!(delivery.reserved_data_bytes(), admitted);
        assert_eq!(
            delivery.writer_key(),
            Some(format!(
                "target:{}",
                artifact
                    .manifest
                    .offline_home
                    .as_ref()
                    .unwrap()
                    .media()
                    .target_stable_id
            ))
        );
        assert!(!delivery.awaiting_original_boot());
        let (sender, receiver) = std::sync::mpsc::channel();
        delivery.worker = Some(receiver);
        delivery.state = "delivering";
        assert!(!delivery.awaiting_original_boot());
        sender
            .send(Ok(Delivered::AwaitingOfflineExport(awaiting)))
            .unwrap();
        assert!(delivery.poll());
        assert!(!delivery.busy());
        assert!(delivery.pending());
        assert!(!delivery.failed());
        assert!(delivery.requires_data_partition());
        // Actual authenticated pending ciphertext cannot be mistaken for the
        // completed delivery protocol by a retry or manual-output action.
        assert_eq!(
            delivery.start_automatic(),
            Err("installer-original-grant-required")
        );
        assert_eq!(
            delivery.start(root.join("manual-output")),
            Err("installer-original-grant-required")
        );
        assert_eq!(delivery.projection()["state"], "awaiting-offline-export");
        assert_eq!(
            delivery.reserved_data_bytes(),
            artifact.capacity_with_home(1024 * 1024).unwrap()
        );
        assert!(delivery.awaiting_original_boot());
        // A state label or a receipt for different bytes cannot authorize the
        // user's boot continuation, even while the declaration remains pending.
        let original_hash = delivery
            .awaiting_setup_receipt
            .as_ref()
            .unwrap()
            .ciphertext_sha256
            .clone();
        delivery
            .awaiting_setup_receipt
            .as_mut()
            .unwrap()
            .ciphertext_sha256 = "0".repeat(64);
        assert!(!delivery.awaiting_original_boot());
        delivery
            .awaiting_setup_receipt
            .as_mut()
            .unwrap()
            .ciphertext_sha256 = original_hash;
        assert!(delivery.awaiting_original_boot());
        let state = delivery.projection();
        assert_eq!(state["state"], "awaiting-offline-export");
        assert_eq!(state["complete"], false);
        assert!(state["receipt"].is_null());
        assert!(state["home_receipt"].is_null());
        assert_eq!(
            state["awaiting_setup_receipt"]["sealed_bytes"],
            artifact.expectation().sealed_bytes
        );
        assert!(artifact.staging_artifact().is_err());
        assert!(artifact.desktop_manifest().is_err());
        assert_eq!(
            std::fs::read(&package).unwrap().len() as u64,
            artifact.expectation().sealed_bytes
        );
        assert!(!root.join("pending-stage/home.receipt.json").exists());
    }
    fn empty_selections() -> MediaSelections {
        MediaSelections {
            home_applications: false,
            home: None,
            retained: None,
            planned_direct: None,
            applications: None,
            browser: None,
            sessions: None,
        }
    }
    fn settle_preparation(controller: &mut Controller) -> Result<Arc<PreparedMedia>, &'static str> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(result) = controller.poll() {
                return result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "production preparation did not exit"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    fn cancellation_keeps_worker_custody_until_exit_and_retry_preserves_identity() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let identity = Arc::new(identity);
        let key = identity.public_key();
        let password = "a sufficiently long preparation password";
        let mut controller = Controller::default();
        controller
            .start_with_selections(
                identity.clone(),
                None,
                empty_selections(),
                Zeroizing::new(password.into()),
                None,
            )
            .unwrap();
        controller.cancel().unwrap();
        assert!(
            controller.busy(),
            "cancel must retain the old worker until actual exit"
        );
        assert_eq!(
            controller.projection()["phase"],
            "installer-media-phase-cancelling"
        );
        assert_eq!(controller.projection()["cancelling"], true);
        assert!(
            controller
                .start_with_selections(
                    identity.clone(),
                    None,
                    empty_selections(),
                    Zeroizing::new(password.into()),
                    None
                )
                .is_err()
        );
        assert!(matches!(
            settle_preparation(&mut controller),
            Err("installer-media-preparation-cancelled")
        ));
        assert!(!controller.busy());
        assert!(controller.joined.is_none());
        assert_eq!(controller.projection()["state"], "cancelled");
        assert!(controller.projection()["artifact"].is_null());
        assert_eq!(identity.public_key(), key);
        controller
            .start_with_selections(
                identity,
                None,
                empty_selections(),
                Zeroizing::new(password.into()),
                None,
            )
            .unwrap();
        let prepared = settle_preparation(&mut controller).unwrap();
        let authenticated = prepared_setup::PreparedSetupReview::open(
            &prepared.package(),
            password.as_bytes(),
            &root.join("verified-retry"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(authenticated.manifest().identity_public_key, key);
        assert!(authenticated.manifest().account_ids.is_empty());
        assert_eq!(controller.projection()["state"], "prepared");
    }

    fn assert_legacy_wrappers(identity: &PreparedIdentity, password: &str, root: &std::path::Path) {
        for (index, prepared) in [
            PreparedMedia::prepare(
                identity,
                None,
                Zeroizing::new(password.into()),
                "compatibility".into(),
            )
            .unwrap(),
            PreparedMedia::prepare_with_home(
                identity,
                None,
                None,
                Zeroizing::new(password.into()),
                "home-wrapper".into(),
            )
            .unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(prepared.install_profile().is_none());
            let review = prepared_setup::PreparedSetupReview::open(
                &prepared.package(),
                password.as_bytes(),
                &root.join(format!("compatibility-{index}")),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(review.manifest().install_profile.is_none());
        }
    }
    #[test]
    fn prepared_install_profile_is_authenticated_and_staging_binds_exact_ciphertext() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        let password = "a sufficiently long installation password";
        let mut draft = imager_backend::imager_model::InstallProfile::default();
        draft.user.username = "ada".into();
        draft.user.full_name = "Ada Lovelace".into();
        let profile = prepared_setup::PublicInstallProfile::from_draft(&draft).unwrap();
        let prepared = PreparedMedia::prepare_for_install(
            &identity,
            None,
            None,
            Zeroizing::new(password.into()),
            "authenticated-install-profile".into(),
            Some(profile.clone()),
        )
        .unwrap();
        let artifact = prepared.staging_artifact().unwrap();
        assert_eq!(
            std::path::Path::new(&artifact.source_path),
            prepared.package()
        );
        let ciphertext = std::fs::read(&artifact.source_path).unwrap();
        assert_eq!(artifact.expectation, *prepared.expectation());
        assert_eq!(artifact.expectation.sealed_bytes, ciphertext.len() as u64);
        assert_eq!(
            artifact.expectation.ciphertext_sha256,
            hex::encode(Sha256::digest(&ciphertext))
        );
        let review = prepared_setup::PreparedSetupReview::open(
            &prepared.package(),
            password.as_bytes(),
            &root.join("authenticated-review"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(review.manifest().install_profile.as_ref(), Some(&profile));
        assert_eq!(review.manifest().identity_public_key, identity.public_key());
        assert_eq!(
            review.manifest().setup_binding,
            "authenticated-install-profile"
        );
        assert_eq!(prepared.install_profile(), Some(&profile));
        assert_eq!(std::fs::read(&artifact.source_path).unwrap(), ciphertext);
        assert_legacy_wrappers(&identity, password, &root);
        let mut changed = ciphertext;
        changed[20] ^= 1;
        std::fs::write(&artifact.source_path, &changed).unwrap();
        assert!(prepared.staging_artifact().is_err());
        assert!(
            prepared_setup::PreparedSetupReview::open(
                &prepared.package(),
                password.as_bytes(),
                &root.join("changed-review"),
                &AtomicBool::new(false),
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&artifact.source_path).unwrap(), changed);
    }
    #[test]
    fn selected_portable_applications_are_sealed_declared_and_delivered_without_plaintext() {
        use super::super::super::{
            flatpak_transfer::{
                Installation, InstalledRef,
                portable::{self, PortableFlatpakSource},
            },
            portable_home_media::MediaGrant,
        };
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let runner = portable::tests::fixture(&root);
        let selected = InstalledRef {
            reference: "app/org.example.Paint/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: Installation::System("default".into()),
            name: "Paint".into(),
        };
        let source =
            PortableFlatpakSource::review(&runner, &runner.source_home, vec![selected]).unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let password = "a sufficiently long transport password";
        let prepared = PreparedMedia::prepare_with_runner(
            &identity,
            None,
            MediaSelections {
                home_applications: false,
                home: None,
                retained: None,
                planned_direct: None,
                applications: Some(PortableAppsSelection {
                    source,
                    include_user_state: false,
                }),
                browser: None,
                sessions: None,
            },
            Zeroizing::new(password.into()),
            "portable-apps-binding".into(),
            None,
            &runner,
        )
        .unwrap();
        assert_eq!(prepared.home_archive_protocol(), Some(2));
        assert!(!prepared._custody.path().join("applications-stage").exists());
        assert!(
            !prepared
                ._custody
                .path()
                .join("applications-journals")
                .exists()
        );
        let setup = prepared_setup::PreparedSetupReview::open(
            &prepared.package(),
            password.as_bytes(),
            &root.join("setup-authenticated"),
            &AtomicBool::new(false),
        )
        .unwrap();
        let expected = setup.manifest().portable_flatpak.as_ref().unwrap();
        assert!(setup.manifest().portable_home.is_none());
        assert_eq!(expected.setup_binding, "portable-apps-binding");
        let storage = root.join("medium");
        std::fs::create_dir(&storage).unwrap();
        std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700)).unwrap();
        let media = MediaGrant::open(&storage).unwrap();
        let apps = prepared.applications.as_ref().unwrap();
        let mut archive = apps.archive.lock().unwrap();
        media
            .deliver_applications(&mut archive, expected, &AtomicBool::new(false))
            .unwrap();
        let first = std::fs::read(storage.join(&expected.filename)).unwrap();
        assert!(
            !first
                .windows(b"fixture signed objects".len())
                .any(|bytes| bytes == b"fixture signed objects")
        );
        media
            .deliver_applications(&mut archive, expected, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            std::fs::read(storage.join(&expected.filename)).unwrap(),
            first
        );
        std::fs::write(storage.join(&expected.filename), b"changed ciphertext").unwrap();
        assert!(
            media
                .deliver_applications(&mut archive, expected, &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(
            std::fs::read(storage.join(&expected.filename)).unwrap(),
            b"changed ciphertext"
        );
    }
    #[test]
    fn home_delivery_is_bound_to_prepared_identity_and_retry_preserves_ciphertext() {
        use super::super::super::{
            portable_home_import::{HomeImportGrants, PortableHomeReview},
            portable_home_media::{HomeSource, MediaGrant},
        };
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.join("source");
        let storage = root.join("media");
        let home = root.join("new-home");
        for directory in [&source, &storage, &home] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(source.join("document"), b"my belongings").unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        let password = "a sufficiently long transport password";
        let prepared = PreparedMedia::prepare_with_home(
            &identity,
            None,
            Some(HomeSource::review(&source).unwrap()),
            Zeroizing::new(password.into()),
            "prepared-home-binding".into(),
        )
        .unwrap();
        let setup = prepared_setup::PreparedSetupReview::open(
            &prepared.package(),
            password.as_bytes(),
            &root.join("setup-review"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(setup.manifest().portable_home.is_some());
        prepared
            .validate_for_write(&AtomicBool::new(false))
            .unwrap();
        let reservation = prepared.reserved_data_bytes.load(Ordering::Acquire);
        let media = MediaGrant::open(&storage).unwrap();
        let mut delivery = prepared.home.as_ref().unwrap().lock().unwrap();
        let first = delivery
            .deliver(&media, &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap();
        let ciphertext = std::fs::read(storage.join(&first.filename)).unwrap();
        let second = delivery
            .deliver(&media, &AtomicBool::new(false), &mut |_| Ok(()))
            .unwrap();
        assert_eq!(first.ciphertext_sha256, second.ciphertext_sha256);
        assert_eq!(
            std::fs::read(storage.join(&first.filename)).unwrap(),
            ciphertext
        );
        let workspace = home.join(".local/state/tilo/portable-home/fixture");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
        let custody = home.join(".config/tilo");
        std::fs::create_dir_all(&custody).unwrap();
        std::fs::set_permissions(&custody, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(
            home.join(".local/state/tilo"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        setup
            .import_identity(&custody, password.as_bytes())
            .unwrap();
        let mut imported = PortableHomeReview::review(
            HomeImportGrants {
                package: storage.join(&first.filename),
                home,
                workspace,
            },
            &setup,
            &first,
            password.as_bytes(),
            &AtomicBool::new(false),
        )
        .unwrap();
        imported
            .prepare(
                Zeroizing::new(password.as_bytes().to_vec()),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(source.join("document")).unwrap(),
            b"my belongings"
        );
        assert!(!prepared.projection().to_string().contains(password));
        assert!(!prepared.projection().to_string().contains("my belongings"));
        drop(delivery);
        prepared
            .validate_for_write(&AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            prepared.reserved_data_bytes.load(Ordering::Acquire),
            reservation
        );
        let cancelled = AtomicBool::new(true);
        let stopped = prepared.validate_for_write(&cancelled).unwrap_err();
        assert_eq!(stopped.kind(), io::ErrorKind::Interrupted);
        assert_eq!(
            std::fs::read(source.join("document")).unwrap(),
            b"my belongings"
        );
        assert_eq!(
            std::fs::read(storage.join(&first.filename)).unwrap(),
            ciphertext
        );
        assert_eq!(
            prepared.reserved_data_bytes.load(Ordering::Acquire),
            reservation
        );
        std::fs::write(source.join("document"), b"later old-system edit").unwrap();
        let error = prepared
            .validate_for_write(&AtomicBool::new(false))
            .unwrap_err();
        assert!(super::super::super::portable_home_media::source_changed(
            &error
        ));
        assert_eq!(
            prepared.reserved_data_bytes.load(Ordering::Acquire),
            reservation
        );
        assert_eq!(
            std::fs::read(storage.join(&first.filename)).unwrap(),
            ciphertext
        );
        let retained = prepared.home.as_ref().unwrap().lock().unwrap();
        assert_eq!(
            serde_json::to_value(retained.receipt.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&first).unwrap()
        );
        assert_eq!(
            std::fs::read(source.join("document")).unwrap(),
            b"later old-system edit"
        );
    }
    fn verify_fixture_device_import(
        authenticated: &tilo_identity_host::enrolled_transport::AuthenticatedEnrolledDevice,
        root: &std::path::Path,
        certificate: &[u8],
    ) {
        let system = root.join("private-import-machine");
        let identity = root.join("private-import-identity");
        for directory in [&system, &identity] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let imported = authenticated.import_into(&system, &identity).unwrap();
        assert_eq!(imported.user_public_key, authenticated.public_key());
        assert_eq!(
            imported.device_public_key,
            authenticated.device_public_key()
        );
        assert_eq!(
            std::fs::read(system.join("device/cert.bin")).unwrap(),
            certificate
        );
        let paths = tilo_user_identity::persist::IdentityPaths::under(&system);
        assert_eq!(
            tilo_user_identity::persist::load_device_key(&paths)
                .unwrap()
                .verifying_key()
                .to_bytes(),
            authenticated.device_public_key()
        );
        assert!(!paths.user_privkey.exists());
        let record = runtime_identity::load_from_path(imported.identity_record).unwrap();
        assert_eq!(record.group_public_key(), authenticated.public_key());
        assert!(!record.is_single_device());
    }

    fn verify_fixture_root_publication(
        reviewed: &prepared_setup::PreparedSetupReview,
        root: &std::path::Path,
        password: &[u8],
        certificate: &[u8],
    ) {
        let target = root.join("installed-machine");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let published = reviewed.publish_enrolled_system(&target, password);
        // SAFETY: geteuid has no preconditions. Both authority branches assert
        // the production result; neither skips the positive private import.
        if rustix::process::geteuid().as_raw() == 0 {
            published.unwrap();
            assert_eq!(
                std::fs::read(target.join("device/cert.bin")).unwrap(),
                certificate
            );
        } else {
            assert!(published.is_err());
            assert!(!target.join("device-private/keypair.bin").exists());
        }
    }

    #[test]
    fn enrolled_desktop_delivers_and_imports_into_the_same_private_host_tree() {
        let (custody, mut pairing) = super::super::pairing::certified_fixture();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        while pairing.busy() {
            assert!(std::time::Instant::now() < deadline);
            pairing.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let identity = pairing.prepared().unwrap();
        let root = custody.path().canonicalize().unwrap();
        let source = identity
            .custody_directory()
            .parent()
            .unwrap()
            .join("future-device");
        let source_key = std::fs::read(source.join("device-private/keypair.bin")).unwrap();
        let source_cert = std::fs::read(source.join("device/cert.bin")).unwrap();
        let source_record = identity.custody_directory().join("member.json");
        let source_record_bytes = std::fs::read(&source_record).unwrap();
        let source_fleet = identity
            .custody_directory()
            .join("fleets")
            .join(format!("{}.json", hex::encode(identity.public_key())));
        let source_fleet_bytes = std::fs::read(&source_fleet).unwrap();
        let home = root.join("desktop-home");
        let config = home.join(".config/tilo");
        let device = home.join(".local/share/tilo");
        for directory in [&home, &config, &device] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(home.join("document"), b"same host document").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", &device);
        let password = "a sufficiently long enrolled desktop preparation password";
        let prepared = PreparedMedia::prepare(
            &identity,
            None,
            Zeroizing::new(password.into()),
            "enrolled-desktop".into(),
        )
        .unwrap();
        let receipt =
            crate::setup::desktop_delivery::deliver(prepared, &home, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(receipt.identity_public_key, identity.public_key());
        let package = home.join(".local/state/tilo/prepared-setup/prepared-setup.sealed");
        let target = config.join("identity");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        // The actual helper requires the caller's quarantine parent to be a
        // private grant, independently of the delivered package's private home.
        let quarantines = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&root)
            .unwrap();
        let run = |suffix: &str, secret: &[u8], host: &std::path::Path| {
            let args = [
                std::ffi::OsString::from("--package"),
                package.clone().into_os_string(),
                "--quarantine".into(),
                quarantines.path().join(suffix).into_os_string(),
                "--identity-dir".into(),
                target.clone().into_os_string(),
                "--config-root".into(),
                config.clone().into_os_string(),
                "--host-device-root".into(),
                host.to_path_buf().into_os_string(),
            ];
            let mut output = Vec::new();
            let result = crate::setup::prepared_setup_helper::run(
                args,
                &mut std::io::Cursor::new(secret),
                &mut output,
            );
            (result, output)
        };
        assert!(run("wrong-password", b"wrong password", &device).0.is_err());
        assert!(!device.join("device-private/keypair.bin").exists());
        assert!(!target.join("primary.json").exists());
        let (result, first) = run("first-import", password.as_bytes(), &device);
        result.unwrap();
        let persona = std::fs::read(target.join("primary.json")).unwrap();
        assert_eq!(
            std::fs::read(device.join("device-private/keypair.bin")).unwrap(),
            source_key
        );
        assert_eq!(
            std::fs::read(device.join("device/cert.bin")).unwrap(),
            source_cert
        );
        let signer = runtime_identity::load_from_path(target.join("primary.json")).unwrap();
        assert_eq!(signer.group_public_key(), identity.public_key());
        assert!(!signer.is_single_device());
        let (result, retry) = run("retry-import", password.as_bytes(), &device);
        result.unwrap();
        assert_eq!(first, retry);
        assert_eq!(std::fs::read(target.join("primary.json")).unwrap(), persona);
        let wrong_root = root.join("unconfigured-device");
        assert!(
            run("wrong-root", password.as_bytes(), &wrong_root)
                .0
                .is_err()
        );
        assert!(!wrong_root.exists());
        let primary = target.join("primary.json");
        let other_person =
            runtime_identity::public_member_record("Existing other person", [1, 2, 3], [6; 32])
                .unwrap();
        std::fs::write(&primary, &other_person).unwrap();
        let (result, refused) = run("conflicting-person", password.as_bytes(), &device);
        assert!(result.is_err());
        assert!(refused.is_empty());
        assert_eq!(std::fs::read(&primary).unwrap(), other_person);
        assert_eq!(
            std::fs::read(device.join("device-private/keypair.bin")).unwrap(),
            source_key
        );
        std::fs::write(&primary, &persona).unwrap();
        // A conflicting pre-existing key must be refused, never replaced or
        // accepted by generating a substitute identity. Exact retries above
        // exercise the canonical same-key import rather than a test fallback.
        let key = device.join("device-private/keypair.bin");
        let mut conflicting = source_key.clone();
        conflicting[0] ^= 1;
        std::fs::write(&key, &conflicting).unwrap();
        let (result, refused) = run("conflicting-key", password.as_bytes(), &device);
        assert!(result.is_err());
        assert!(refused.is_empty());
        assert_eq!(std::fs::read(&key).unwrap(), conflicting);
        assert_eq!(std::fs::read(target.join("primary.json")).unwrap(), persona);
        assert_eq!(
            std::fs::read(source.join("device-private/keypair.bin")).unwrap(),
            source_key
        );
        assert_eq!(
            std::fs::read(source.join("device/cert.bin")).unwrap(),
            source_cert
        );
        assert_eq!(std::fs::read(&source_record).unwrap(), source_record_bytes);
        assert_eq!(std::fs::read(&source_fleet).unwrap(), source_fleet_bytes);
        assert_eq!(
            std::fs::read(home.join("document")).unwrap(),
            b"same host document"
        );
    }

    #[test]
    fn media_preparation_exports_actual_enrolled_device_without_root_backup_fallback() {
        let (custody, mut pairing) = super::super::pairing::certified_fixture();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        while pairing.busy() {
            assert!(std::time::Instant::now() < deadline);
            pairing.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let identity = pairing.prepared().unwrap();
        let device = identity.enrolled_device_public_key().unwrap();
        let root = custody.path().canonicalize().unwrap();
        let source = identity
            .custody_directory()
            .parent()
            .unwrap()
            .join("future-device/device/cert.bin");
        let certificate = std::fs::read(&source).unwrap();
        let password = "a sufficiently long enrolled transport password";
        let media = PreparedMedia::prepare(
            &identity,
            None,
            Zeroizing::new(password.into()),
            "actual-enrolled-operation".into(),
        )
        .unwrap();
        let quarantine = root.join("reviewed-setup");
        let reviewed = prepared_setup::PreparedSetupReview::open(
            &media.package(),
            password.as_bytes(),
            &quarantine,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            reviewed.manifest().identity_kind,
            prepared_setup::IdentityKind::EnrolledDevice {
                device_public_key: device
            }
        );
        assert_eq!(
            reviewed.manifest().identity_public_key,
            identity.public_key()
        );
        let sealed = quarantine.join("home").join(prepared_setup::IDENTITY_FILE);
        assert!(sealed.is_file());
        assert!(runtime_identity::verify_portable_backup(&sealed, password.as_bytes()).is_err());
        let authentication = |binding: &str, root_key, device_key| {
            tilo_identity_host::enrolled_transport::AuthenticatedEnrolledDevice::open(
                &sealed,
                password.as_bytes(),
                binding,
                root_key,
                device_key,
                1,
            )
        };
        assert!(authentication("wrong-operation", identity.public_key(), device).is_err());
        assert!(authentication("actual-enrolled-operation", [0; 32], device).is_err());
        assert!(
            authentication("actual-enrolled-operation", identity.public_key(), [0; 32]).is_err()
        );
        let authenticated =
            authentication("actual-enrolled-operation", identity.public_key(), device).unwrap();
        verify_fixture_device_import(&authenticated, &root, &certificate);
        verify_fixture_root_publication(&reviewed, &root, password.as_bytes(), &certificate);
        assert_eq!(std::fs::read(&source).unwrap(), certificate);
        assert!(!media._custody.path().join("setup").exists());
        assert!(!media.projection().to_string().contains(password));
    }

    #[test]
    fn media_preparation_exports_the_actual_identity_and_holds_only_ciphertext() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let root = tempfile::tempdir().unwrap();
        let private = root.path().canonicalize().unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let identity = PreparedIdentity::create(
            &private.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        let password = "a sufficiently long transport password";
        let media = PreparedMedia::prepare(
            &identity,
            None,
            Zeroizing::new(password.into()),
            "media-operation".into(),
        )
        .unwrap();
        assert_eq!(media.public_key, identity.public_key());
        assert!(media.account_ids.is_empty());
        assert!(!media._custody.path().join("setup").exists());
        let quarantine = root.path().join("review");
        let reviewed = prepared_setup::PreparedSetupReview::open(
            &media.package(),
            password.as_bytes(),
            &quarantine,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            reviewed.manifest().identity_public_key,
            identity.public_key()
        );
        assert_eq!(reviewed.manifest().setup_binding, "media-operation");
        assert!(
            prepared_setup::PreparedSetupReview::open(
                &media.package(),
                b"wrong password",
                &root.path().join("wrong"),
                &AtomicBool::new(false),
            )
            .is_err()
        );
        let mut delivery = Delivery::with_artifact(media.clone());
        assert_eq!(
            delivery.start(root.path().join("ungranted")),
            Err("installer-media-not-written")
        );
        let target = crate::model::sample_targets()[1].clone();
        delivery.written(target);
        assert_eq!(delivery.projection()["state"], "waiting-for-filesystem");
        delivery.writing();
        assert_eq!(delivery.projection()["state"], "waiting-for-write");
        assert_eq!(
            delivery.start(root.path().join("ungranted")),
            Err("installer-media-not-written")
        );
        assert!(delivery.pending());
        drop(delivery);
        let package = media.package();
        let held = media.clone();
        drop(media);
        assert!(package.exists());
        drop(held);
        assert!(!package.exists());
    }
}

#[cfg(test)]
#[path = "prepared_media/browser_tests.rs"]
mod browser_tests;

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn protected_account_media_fixture(
    root: &std::path::Path,
) -> (
    Arc<PreparedMedia>,
    crate::setup::staging::prepared_setup::PreparedArtifact,
) {
    let accounts = super::accounts::checkpoint_fixture_for_journey(root);
    let identity = PreparedIdentity::create(
        &root.join("prepared-identity"),
        "Ada",
        [10, 20, 30],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let prepared = PreparedMedia::prepare(
        &identity,
        accounts.prepared_handle(),
        Zeroizing::new("original chosen preparation password".into()),
        "actual-protected-media-fixture".into(),
    )
    .unwrap();
    let artifact = prepared.staging_artifact().unwrap();
    (prepared, artifact)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn home_media_fixture(root: &std::path::Path) -> Arc<PreparedMedia> {
    let source = root.join("original-home");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("document"), b"unchanged original document").unwrap();
    let identity = PreparedIdentity::create(
        &root.join("prepared-identity"),
        "Ada",
        [10, 20, 30],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    PreparedMedia::prepare_with_home(
        &identity,
        None,
        Some(super::super::portable_home_media::HomeSource::review(&source).unwrap()),
        Zeroizing::new("original chosen preparation password".into()),
        "actual-home-media-fixture".into(),
    )
    .unwrap()
}

#[cfg(test)]
#[path = "prepared_media/session_tests.rs"]
mod session_tests;

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn selected_applications_media_fixture(root: &std::path::Path) -> Arc<PreparedMedia> {
    use super::super::flatpak_transfer::{
        Installation, InstalledRef,
        portable::{self, PortableFlatpakSource},
    };
    use std::os::unix::fs::PermissionsExt;
    let fake = portable::tests::fixture(root);
    let runner = portable::tests::UserSubset(&fake);
    let selected_data = fake.source_home.join(".var/app/org.example.Paint");
    std::fs::create_dir_all(&selected_data).unwrap();
    std::fs::set_permissions(&selected_data, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        selected_data.join("settings"),
        b"actual selected app settings",
    )
    .unwrap();
    let selected = InstalledRef {
        reference: "app/org.example.Paint/x86_64/stable".into(),
        origin: "flathub".into(),
        installation: Installation::User,
        name: "Paint".into(),
    };
    let source =
        PortableFlatpakSource::review_selected_state(&runner, &fake.source_home, vec![selected])
            .unwrap();
    let identity = PreparedIdentity::create(
        &root.join("selected-app-identity"),
        "Ada",
        [10, 20, 30],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let artifact = PreparedMedia::prepare_with_runner(
        &identity,
        None,
        MediaSelections {
            home_applications: false,
            home: None,
            retained: None,
            planned_direct: None,
            applications: Some(PortableAppsSelection {
                source,
                include_user_state: true,
            }),
            browser: None,
            sessions: None,
        },
        Zeroizing::new("actual selected apps preparation password".into()),
        "actual-selected-apps-binding".into(),
        None,
        &runner,
    )
    .unwrap();
    assert_eq!(artifact.applications_protocol(), Some(2));
    assert!(artifact.manifest.portable_home.is_none());
    artifact
}

impl PreparedMedia {
    fn from_native(native: NativePreparedMedia) -> Self {
        Self {
            native_payload: super::native_ownership::FrontendNativeCustody::new(native),
            api: super::prepared_media::PreparedApi {
                projection: Self::native_projection,
                expectation: Self::native_expectation,
                account_custody_protocol: Self::native_account_custody_protocol,
                service_session_protocol: Self::native_service_session_protocol,
                applications_protocol: Self::native_applications_protocol,
                home_archive_protocol: Self::native_home_archive_protocol,
                validate_for_write: Self::native_validate_for_write,
                install_profile_draft: |prepared| {
                    prepared
                        .install_profile()
                        .map(|profile| profile.to_draft())
                        .transpose()
                },
            },
        }
    }
}
impl std::ops::Deref for PreparedMedia {
    type Target = NativePreparedMedia;
    fn deref(&self) -> &Self::Target {
        self.native_payload
            .get()
            .expect("prepared media retains its native owner")
    }
}

impl std::ops::DerefMut for PreparedMedia {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.native_payload
            .get_mut()
            .expect("prepared media retains its native owner")
    }
}

impl super::prepared_media::DeliveryState for Delivery {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn finish_close(&mut self) -> std::io::Result<()> {
        Delivery::finish_close(self)
    }
    fn has_artifact(&self, artifact: &Arc<PreparedMedia>) -> bool {
        Delivery::has_artifact(self, artifact)
    }
    fn artifact(&self) -> Option<Arc<PreparedMedia>> {
        Delivery::artifact(self)
    }
    fn mark_source_changed(&mut self) {
        Delivery::mark_source_changed(self)
    }
    fn mark_source_unavailable(&mut self) {
        Delivery::mark_source_unavailable(self)
    }
    fn review_reason(&self) -> Option<ReturnReason> {
        Delivery::review_reason(self)
    }
    fn reserved_data_bytes(&self) -> u64 {
        Delivery::reserved_data_bytes(self)
    }
    fn expectation(&self) -> Option<&imager_backend::imager_model::PreparedSetupExpectation> {
        Delivery::expectation(self)
    }
    fn requires_data_partition(&self) -> bool {
        Delivery::requires_data_partition(self)
    }
    fn writing(&mut self) {
        Delivery::writing(self)
    }
    fn written(&mut self, target: imager_backend::imager_model::TargetDrive) {
        Delivery::written(self, target)
    }
    fn awaiting_original_boot(&self) -> bool {
        Delivery::awaiting_original_boot(self)
    }
    fn original_boot_drive(&self) -> Option<String> {
        Delivery::original_boot_drive(self)
    }
    fn pending(&self) -> bool {
        Delivery::pending(self)
    }
    fn busy(&self) -> bool {
        Delivery::busy(self)
    }
    fn start(&mut self, root: std::path::PathBuf) -> Result<(), &'static str> {
        Delivery::start(self, root)
    }
    fn requires_offline_export(&self) -> bool {
        Delivery::requires_offline_export(self)
    }
    fn start_automatic(&mut self) -> Result<(), &'static str> {
        Delivery::start_automatic(self)
    }
    fn progress_label(&self) -> Option<String> {
        Delivery::progress_label(self)
    }
    fn cancel(&self) -> Result<(), &'static str> {
        Delivery::cancel(self)
    }
    fn poll(&mut self) -> bool {
        Delivery::poll(self)
    }
    fn needs_source_review(&self) -> bool {
        Delivery::needs_source_review(self)
    }
    fn failed(&self) -> bool {
        Delivery::failed(self)
    }
    fn projection(&self) -> serde_json::Value {
        Delivery::projection(self)
    }
}
impl std::ops::Deref for super::prepared_media::Delivery {
    type Target = Delivery;
    fn deref(&self) -> &Self::Target {
        self.native
            .get()
            .expect("native adapter installed")
            .as_any()
            .downcast_ref()
            .expect("matching native adapter")
    }
}
impl std::ops::DerefMut for super::prepared_media::Delivery {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.native
            .get_mut()
            .expect("native adapter installed")
            .as_any_mut()
            .downcast_mut()
            .expect("matching native adapter")
    }
}

impl super::prepared_media::ControllerState for Controller {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn busy(&self) -> bool {
        Controller::busy(self)
    }
    fn cancel(&self) -> Result<(), &'static str> {
        Controller::cancel(self)
    }
    fn phase_key(&self) -> Option<String> {
        Controller::phase_key(self)
    }
    fn progress_label(&self) -> Option<String> {
        Controller::progress_label(self)
    }
    fn take_progress_changed(&mut self) -> bool {
        Controller::take_progress_changed(self)
    }
    fn poll(&mut self) -> Option<Result<Arc<PreparedMedia>, &'static str>> {
        Controller::poll(self)
    }
    fn require_source_review(&mut self, reason: ReturnReason) {
        Controller::require_source_review(self, reason)
    }
    fn review_reason(&self) -> Option<ReturnReason> {
        Controller::review_reason(self)
    }
    fn active_home_projection(&self) -> Option<serde_json::Value> {
        Controller::active_home_projection(self)
    }
    fn projection(&self) -> serde_json::Value {
        Controller::projection(self)
    }
}
impl std::ops::Deref for super::prepared_media::Controller {
    type Target = Controller;
    fn deref(&self) -> &Self::Target {
        self.native
            .get()
            .expect("native adapter installed")
            .as_any()
            .downcast_ref()
            .expect("matching native adapter")
    }
}
impl std::ops::DerefMut for super::prepared_media::Controller {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.native
            .get_mut()
            .expect("native adapter installed")
            .as_any_mut()
            .downcast_mut()
            .expect("matching native adapter")
    }
}
