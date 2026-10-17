//! Private installer migration coordinator. Source grants and credentials never
//! enter collaborative app state. A selected home copy finishes and verifies
//! before Flatpak mutates the new user's installation. Originals stay intact.
use super::{
    flatpak_transfer::{self as flatpak, ApprovedRemote, InstalledRef, Runner},
    home_transfer::{self, HomeReview},
    migration::{CopyProgress, CopyReport},
    migration_worker::{WorkerEvent, WorkerOutcome},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Duration,
};

mod adoption;
pub use adoption::{
    recover_portable_promotion_with_runner, recover_promotion, recover_promotion_with_runner,
};
pub mod applications;
mod durable;
pub use durable::{publish as publish_private_artifact, read as read_private_artifact};
mod namespace;
mod storage;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct TransferRequest {
    pub source_home: PathBuf,
    pub target_home: PathBuf,
    pub private_journals: PathBuf,
    pub copy_home: bool,
    pub copy_flatpak: bool,
    /// System refs only. User refs travel in the selected home/Flatpak data copy.
    pub system_refs: Vec<InstalledRef>,
}

#[derive(Clone)]
pub struct TransferReview {
    request: TransferRequest,
    copies: Vec<HomeReview>,
    remotes: Vec<storage::PreparedRemote>,
    preparation_failures: Vec<(String, String)>,
    recovered: bool,
    application_recovery: bool,
    verified_applications: bool,
    phase: Arc<Mutex<Phase>>,
    // A source mount grant stays live in the worker's cloned review until all
    // file descriptors and finalization have finished, even after UI teardown.
    _source_guard: Option<Arc<dyn Send + Sync>>,
}
#[derive(Default)]
struct Phase {
    busy: bool,
    applications_complete: bool,
    attempted: std::collections::BTreeSet<usize>,
    complete:
        std::collections::BTreeMap<usize, (CopyReport, super::migration_durability::Identity)>,
}
struct PhaseLease(Arc<Mutex<Phase>>);
impl Drop for PhaseLease {
    fn drop(&mut self) {
        if let Ok(mut phase) = self.0.lock() {
            phase.busy = false;
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct TransferSummary {
    pub files: u64,
    pub bytes: u64,
    pub links: u64,
    pub special_files: u64,
    /// Runtime socket endpoints are recreated by apps, rather than copied.
    pub runtime_sockets: u64,
    pub system_applications: usize,
    pub repository_failures: usize,
}

#[derive(Debug, Default)]
pub struct TransferReport {
    pub copies: Vec<CopyReport>,
    pub flatpak: flatpak::RestoreReport,
    pub cancelled: bool,
}
impl TransferReport {
    pub fn complete(&self) -> bool {
        !self.cancelled
            && self.copies.iter().all(CopyReport::complete)
            && self.flatpak.failed.is_empty()
    }
}

#[derive(Debug)]
pub enum TransferEvent {
    CopyProgress(CopyProgress),
    RestoringApplications,
    Finished(Result<TransferReport, String>),
}

/// Review the exact home/subtrees, target and system refs. Flatpak-only review
/// reserves a fresh private target skeleton because each subtree has its own
/// pinned parent. No source is modified and no existing destination is adopted.
#[cfg(target_os = "linux")]
pub fn review(runner: &impl Runner, request: TransferRequest) -> Result<TransferReview, String> {
    if !request.copy_home && !request.copy_flatpak {
        return Err("Select data to transfer".into());
    }
    storage::validate_grants(&request)?;
    if durable::pending(&request).map_err(|error| error.to_string())? {
        return recover(runner, request);
    }
    let copies = if request.copy_home {
        vec![home_transfer::review(
            &request.source_home,
            &request.target_home,
            &request.private_journals.join("home-import"),
        )?]
    } else {
        let copies = storage::review_flatpak_data(&request)?;
        namespace::save(&request)?;
        copies
    };
    let (remotes, preparation_failures) = if request.copy_flatpak {
        storage::prepare_remotes(runner, &request)?
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(TransferReview {
        request,
        copies,
        remotes,
        preparation_failures,
        recovered: false,
        application_recovery: false,
        verified_applications: false,
        phase: Arc::new(Mutex::new(Phase::default())),
        _source_guard: None,
    })
}

/// Renew explicit grants and revalidate actual retained home/fidelity receipts.
/// Saved phase labels or success reports are never accepted as copy proof.
///
/// # Errors
/// Refuses changed grants/selections, missing copy or original-app receipts,
/// changed user data, repository trust, objects, or completed application output.
/// An interrupted signed application transaction is repaired only after the
/// original copied user data passes independent fidelity verification.
#[cfg(target_os = "linux")]
pub fn recover(runner: &impl Runner, request: TransferRequest) -> Result<TransferReview, String> {
    storage::validate_grants(&request)?;
    let application_recovery = durable::validate_recovery(&request)?;
    let copies = if request.copy_home {
        vec![HomeReview::recover(
            &request.source_home,
            &request.target_home,
            &request.private_journals.join("home-import"),
        )?]
    } else {
        namespace::recover(&request)?
    };
    let (remotes, preparation_failures) = if request.copy_flatpak {
        storage::prepare_remotes(runner, &request)?
    } else {
        (Vec::new(), Vec::new())
    };
    let mut phase = Phase::default();
    let verified_applications = if application_recovery {
        applications::validate_recovery(&request)?;
        let reports = verify_copies_after_applications(&request, &copies)?;
        applications::verify_original_config(&request)?;
        let approved = storage::publish_remotes(&request.private_journals, &remotes)?;
        let verified = applications::verify_output(&request, runner, &approved)?;
        for (index, (copy, report)) in copies.iter().zip(reports).enumerate() {
            phase.complete.insert(
                index,
                (report, storage::target_identity(copy.destination())?),
            );
        }
        verified
    } else {
        false
    };
    phase.applications_complete = verified_applications;
    phase.attempted.extend(0..copies.len());
    Ok(TransferReview {
        request,
        copies,
        remotes,
        preparation_failures,
        recovered: true,
        application_recovery,
        verified_applications,
        phase: Arc::new(Mutex::new(phase)),
        _source_guard: None,
    })
}

fn verify_copies_after_applications(
    request: &TransferRequest,
    copies: &[HomeReview],
) -> Result<Vec<CopyReport>, String> {
    if request.copy_home {
        Ok(vec![
            copies
                .first()
                .ok_or("Original whole-home proof is missing")?
                .verify_after_applications()?,
        ])
    } else {
        namespace::verify(request, copies)
    }
}

impl TransferReview {
    pub fn retain_source_guard(&mut self, guard: Arc<dyn Send + Sync>) {
        self._source_guard = Some(guard);
    }

    pub fn summary(&self) -> Result<TransferSummary, String> {
        let mut result = TransferSummary::default();
        for copy in &self.copies {
            result.files = result
                .files
                .checked_add(copy.summary.files)
                .ok_or("File count exceeds bounds")?;
            result.bytes = result
                .bytes
                .checked_add(copy.summary.bytes)
                .ok_or("Byte count exceeds bounds")?;
            result.links = result
                .links
                .checked_add(copy.summary.links)
                .ok_or("Link count exceeds bounds")?;
            result.special_files = result
                .special_files
                .checked_add(copy.summary.special_files)
                .ok_or("Special-file count exceeds bounds")?;
            result.runtime_sockets = result
                .runtime_sockets
                .checked_add(copy.summary.runtime_sockets)
                .ok_or("Runtime-socket count exceeds bounds")?;
        }
        result.system_applications = self
            .request
            .system_refs
            .iter()
            .filter(|item| item.app_id().is_some())
            .count();
        result.repository_failures = self.preparation_failures.len();
        Ok(result)
    }
    pub fn recovered(&self) -> bool {
        self.recovered
    }
    pub fn target_home(&self) -> &std::path::Path {
        &self.request.target_home
    }
    /// Promotion can consume only this operation's completed whole-home copy.
    pub fn review_promotion(
        &self,
        home: &std::path::Path,
        journal: &std::path::Path,
    ) -> Result<super::home_transfer::promotion::PromotionReview, String> {
        self.review_promotion_inner(home, journal, None)
    }
    pub fn review_portable_promotion(
        &self,
        home: &std::path::Path,
        journal: &std::path::Path,
        grant: &super::flatpak_transfer::portable::ApplicationStateGrant,
    ) -> Result<super::home_transfer::promotion::PromotionReview, String> {
        if self.request.copy_home || !self.request.copy_flatpak {
            return Err("Portable application authority cannot authorize home adoption".into());
        }
        self.review_promotion_inner(home, journal, Some(grant))
    }
    fn review_promotion_inner(
        &self,
        home: &std::path::Path,
        journal: &std::path::Path,
        grant: Option<&super::flatpak_transfer::portable::ApplicationStateGrant>,
    ) -> Result<super::home_transfer::promotion::PromotionReview, String> {
        let protected = grant
            .map(|grant| grant.permits(&self.request.source_home, home))
            .transpose()?
            .unwrap_or(false);
        let source = super::migration::PinnedDirectory::open_absolute(&self.request.source_home)
            .map_err(|error| error.to_string())?;
        source.validate().map_err(|error| error.to_string())?;
        for target in [home, journal] {
            if !target.is_absolute()
                || target.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
                || target.starts_with(&self.request.source_home)
                || (self.request.source_home.starts_with(target) && !(protected && target == home))
            {
                return Err("Home adoption cannot write into or overlap the original home".into());
            }
        }
        let phase = self
            .phase
            .lock()
            .map_err(|_| "Copy receipts became unavailable")?;
        if self.request.copy_flatpak
            && !self.request.system_refs.is_empty()
            && !phase.applications_complete
        {
            return Err(
                "Reviewed application restoration has not been independently completed".into(),
            );
        }
        if phase.busy || phase.complete.len() != self.copies.len() {
            return Err("Selected copy is incomplete or still busy".into());
        }
        for (index, copy) in self.copies.iter().enumerate() {
            let (report, identity) = phase
                .complete
                .get(&index)
                .ok_or("Selected copy receipt missing")?;
            if !report.complete() || storage::target_identity(copy.destination())? != *identity {
                return Err("Verified copy destination changed".into());
            }
        }
        if !self.request.copy_home {
            if !self.request.copy_flatpak {
                return Err("No application selection was reviewed".into());
            }
            namespace::verify_promotion(&self.request, &self.copies, phase.applications_complete)?;
            if phase.applications_complete {
                applications::verify_checkpoint(&self.request)?;
            }
        }
        super::home_transfer::promotion::PromotionReview::review(
            &self.request.target_home,
            home,
            journal,
        )?
        .with_application_consent(self.request.copy_flatpak)
    }

    pub fn copy_count(&self) -> usize {
        self.copies.len()
    }

    /// Synchronous orchestration, useful with a fixture CLI. Native callers use
    /// `start` to avoid blocking UI threads. Cancellation drains the copy worker
    /// before reporting terminal state; partial verified output is retained.
    pub fn execute(
        &self,
        runner: &impl Runner,
        cancelled: &AtomicBool,
        retry: bool,
        mut event: impl FnMut(TransferEvent),
    ) -> Result<TransferReport, String> {
        let mut report = TransferReport::default();
        if cancelled.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
        }
        if !self.request.copy_home {
            namespace::validate_grants(&self.request)?;
        }
        for copy in &self.copies {
            copy.retain_review()?;
        }
        durable::save(&self.request).map_err(|error| error.to_string())?;
        let _lease = {
            let mut phase = self
                .phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?;
            if phase.busy {
                return Err("This transfer is already running".into());
            }
            if !retry && !phase.attempted.is_empty() {
                return Err("Use Retry to resume this transfer".into());
            }
            phase.busy = true;
            PhaseLease(Arc::clone(&self.phase))
        };
        if self.application_recovery {
            let proofs = verify_copies_after_applications(&self.request, &self.copies)?;
            applications::verify_original_config(&self.request)?;
            let mut phase = self
                .phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?;
            for (index, (copy, proof)) in self.copies.iter().zip(proofs).enumerate() {
                phase.complete.insert(
                    index,
                    (proof, storage::target_identity(copy.destination())?),
                );
            }
        }
        report = self.execute_copies(cancelled, &mut event)?;
        if report.cancelled || !report.copies.iter().all(CopyReport::complete) {
            return Ok(report);
        }
        if cancelled.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
        }
        if self.request.copy_flatpak {
            if !self.request.system_refs.is_empty() {
                verify_copies_after_applications(&self.request, &self.copies)?;
                applications::verify_original_config(&self.request)?;
            }
            if !self.request.system_refs.is_empty() {
                if self.application_recovery {
                    durable::applications_started(&self.request)
                        .map_err(|error| error.to_string())?;
                } else {
                    applications::begin(&self.request, runner)?;
                }
            }
            event(TransferEvent::RestoringApplications);
            report.flatpak.failed.clone_from(&self.preparation_failures);
            let approved: Vec<ApprovedRemote> =
                storage::publish_remotes(&self.request.private_journals, &self.remotes)?;
            let selected: Vec<_> = self
                .request
                .system_refs
                .iter()
                .filter(|item| {
                    !self
                        .preparation_failures
                        .iter()
                        .any(|(reference, _)| reference == &item.reference)
                })
                .cloned()
                .collect();
            let verified_applications = self.verified_applications
                || self
                    .phase
                    .lock()
                    .map_err(|_| "Transfer phase became unavailable")?
                    .applications_complete;
            let result = if verified_applications {
                if !applications::verify_output(&self.request, runner, &approved)? {
                    return Err("Application output proof disappeared".into());
                }
                flatpak::RestoreReport {
                    restored: selected.iter().map(|item| item.reference.clone()).collect(),
                    failed: Vec::new(),
                }
            } else if self.application_recovery {
                flatpak::repair_system_refs(
                    runner,
                    &self.request.target_home,
                    &selected,
                    &approved,
                )?
            } else {
                flatpak::restore_system_refs(
                    runner,
                    &self.request.target_home,
                    &selected,
                    &approved,
                    None,
                )?
            };
            report.flatpak.restored.extend(result.restored);
            report.flatpak.failed.extend(result.failed);
            report.cancelled = cancelled.load(Ordering::Relaxed);
            if !selected.is_empty() && report.flatpak.failed.is_empty() && !report.cancelled {
                verify_copies_after_applications(&self.request, &self.copies)?;
                applications::checkpoint(&self.request, runner, &approved)?;
                self.phase
                    .lock()
                    .map_err(|_| "Transfer phase became unavailable")?
                    .applications_complete = true;
            }
        }
        Ok(report)
    }

    fn execute_copies(
        &self,
        cancelled: &AtomicBool,
        event: &mut impl FnMut(TransferEvent),
    ) -> Result<TransferReport, String> {
        let mut report = TransferReport::default();
        for (index, copy) in self.copies.iter().enumerate() {
            let completed = self
                .phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?
                .complete
                .get(&index)
                .cloned();
            if let Some((verified, identity)) = completed {
                if storage::target_identity(copy.destination())? != identity {
                    return Err("Transferred directory was replaced; review again".into());
                }
                report.copies.push(verified);
                continue;
            }
            let attempted = self
                .phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?
                .attempted
                .contains(&index);
            let mut worker = if attempted {
                copy.retry()?
            } else {
                copy.start()?
            };
            self.phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?
                .attempted
                .insert(index);
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    worker.cancel();
                }
                match worker.try_next() {
                    Some(WorkerEvent::Progress(progress)) => {
                        event(TransferEvent::CopyProgress(progress))
                    }
                    Some(WorkerEvent::Finished(WorkerOutcome::Complete(result))) => {
                        let identity = storage::target_identity(copy.destination())?;
                        self.phase
                            .lock()
                            .map_err(|_| "Transfer phase became unavailable")?
                            .complete
                            .insert(index, (result.clone(), identity));
                        report.copies.push(result);
                        break;
                    }
                    Some(WorkerEvent::Finished(WorkerOutcome::Incomplete(result))) => {
                        report.cancelled = result.cancelled || cancelled.load(Ordering::Relaxed);
                        report.copies.push(result);
                        return Ok(report);
                    }
                    Some(WorkerEvent::Finished(WorkerOutcome::Failed(error))) => return Err(error),
                    Some(WorkerEvent::Finished(WorkerOutcome::Interrupted)) => {
                        return Err("Data transfer stopped before completion".into());
                    }
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        Ok(report)
    }

    pub fn start(&self, retry: bool) -> Result<TransferWorker, String> {
        let review = self.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let (sender, events) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("imager-user-transfer".into())
            .spawn(move || {
                let runner = flatpak::CancellableNativeRunner(Arc::clone(&flag));
                let result = review.execute(&runner, &flag, retry, |event| {
                    let _ = sender.send(event);
                });
                let _ = sender.send(TransferEvent::Finished(result));
            })
            .map_err(|error| error.to_string())?;
        Ok(TransferWorker {
            cancelled,
            events,
            terminal: false,
            thread: Some(thread),
        })
    }
}

pub struct TransferWorker {
    thread: Option<std::thread::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    events: Receiver<TransferEvent>,
    terminal: bool,
}
impl TransferWorker {
    /// Drain thread-local source leases after observing a terminal event.
    /// Headless helpers call this before exiting; UI callers retain nonblocking Drop.
    ///
    /// # Errors
    /// Refuses joining an active worker or reports a panicked worker.
    pub fn join_finished(&mut self) -> Result<(), String> {
        if !self.terminal {
            return Err("The transfer worker has not finished".into());
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "The transfer worker stopped during cleanup")?;
        }
        Ok(())
    }
    pub fn cancel(&self) -> bool {
        !self.terminal && !self.cancelled.swap(true, Ordering::Relaxed)
    }
    pub fn try_next(&mut self) -> Option<TransferEvent> {
        if self.terminal {
            return None;
        }
        match self.events.try_recv() {
            Ok(event) => {
                self.terminal = matches!(event, TransferEvent::Finished(_));
                Some(event)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.terminal = true;
                Some(TransferEvent::Finished(Err(
                    "User transfer worker stopped".into()
                )))
            }
        }
    }
}
impl Drop for TransferWorker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
