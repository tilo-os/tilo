//! Private installer migration coordinator. Source grants and credentials never
//! enter collaborative app state. The selected data travels as a sealed Home
//! unit, admitted at the target before Flatpak mutates the new user's
//! installation. Originals stay intact.
use super::{
    flatpak_transfer::{
        self as flatpak, InstalledRef, Runner,
        portable::{OfflineRunner, PortableFlatpakSource, SealedApplications},
    },
    migration_durability::Identity,
    portable_home_media::{HomeSource, SealedHome, application_data_exclusions},
    portable_transfer::{ArchiveProgress, ExportSummary},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

mod adoption;
pub use adoption::{
    recover_portable_promotion_with_runner, recover_promotion, recover_promotion_with_runner,
};
pub mod applications;
mod durable;
pub use durable::{publish as publish_private_artifact, read as read_private_artifact};
mod storage;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct TransferRequest {
    pub source_home: PathBuf,
    /// A new directory; the admitted Home unit becomes this tree.
    pub target_home: PathBuf,
    pub private_journals: PathBuf,
    pub copy_home: bool,
    pub copy_flatpak: bool,
    /// System refs only. User refs travel in the selected home/Flatpak data.
    pub system_refs: Vec<InstalledRef>,
}
impl TransferRequest {
    /// The private workspace that holds the sealed Home unit.
    fn home_workspace(&self) -> PathBuf {
        self.private_journals.join("home")
    }
}

/// The selected system applications. They always arrive as one sealed
/// applications unit; on the same computer the unit is sealed from the
/// reviewed original deployments when the transfer runs.
#[derive(Clone)]
pub enum Applications {
    /// Reviewed original deployments, sealed into the private journals.
    Original(Arc<PortableFlatpakSource>),
    /// A unit that is already admitted.
    Sealed(SealedApplications),
}
impl Applications {
    fn covers(&self, refs: &[InstalledRef]) -> bool {
        refs.iter().all(|item| match self {
            Self::Original(source) => item.app_id().is_none() || source.selected().contains(item),
            Self::Sealed(unit) => unit
                .deployments()
                .iter()
                .any(|deployment| deployment.installed == *item),
        })
    }
}

#[derive(Clone)]
pub struct TransferReview {
    request: TransferRequest,
    /// The reviewed source, until its Home unit is sealed.
    source: Option<Arc<HomeSource>>,
    summary: TransferSummary,
    applications: Option<Applications>,
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
    attempted: bool,
    applications_complete: bool,
    sealed: Option<SealedApplications>,
    /// The admitted Home unit and the identity of its tree at admission.
    home: Option<(SealedHome, Identity)>,
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
}
impl TransferSummary {
    fn new(home: &ExportSummary, request: &TransferRequest) -> Self {
        Self {
            files: home.files,
            bytes: home.bytes,
            links: home.links,
            special_files: home.special_files,
            runtime_sockets: home.runtime_sockets,
            system_applications: request
                .system_refs
                .iter()
                .filter(|item| item.app_id().is_some())
                .count(),
        }
    }
}

#[derive(Debug, Default)]
pub struct TransferReport {
    /// The Home unit was sealed and admitted at the target.
    pub admitted: bool,
    pub flatpak: flatpak::RestoreReport,
    pub cancelled: bool,
}
impl TransferReport {
    pub fn complete(&self) -> bool {
        self.admitted && !self.cancelled && self.flatpak.failed.is_empty()
    }
}

#[derive(Debug)]
pub enum TransferEvent {
    Progress(ArchiveProgress),
    RestoringApplications,
    Finished(Result<TransferReport, String>),
}

/// Review the exact home or Flatpak data, target and system refs. Nothing is
/// sealed yet; no source is modified and no existing destination is adopted.
pub fn review(
    request: TransferRequest,
    applications: Option<Applications>,
) -> Result<TransferReview, String> {
    if !request.copy_home && !request.copy_flatpak {
        return Err("Select data to transfer".into());
    }
    validate_applications(&request, applications.as_ref())?;
    storage::validate_grants(&request)?;
    if durable::pending(&request).map_err(|error| error.to_string())? {
        return recover(&flatpak::NativeRunner, request, applications);
    }
    let source = review_source(&request)?;
    let summary = TransferSummary::new(
        &source.summary().map_err(|error| error.to_string())?,
        &request,
    );
    Ok(TransferReview {
        request,
        source: Some(source),
        summary,
        applications,
        recovered: false,
        application_recovery: false,
        verified_applications: false,
        phase: Arc::new(Mutex::new(Phase::default())),
        _source_guard: None,
    })
}

/// Renew explicit grants and reopen the sealed Home unit, re-authenticating
/// its package and the admitted tree. Saved phase labels or success reports
/// are never accepted as proof.
///
/// # Errors
/// Refuses changed grants/selections, a changed unit or admitted tree,
/// missing original-app receipts, repository trust, objects, or completed
/// application output. An interrupted signed application transaction is
/// repaired only after the admitted user data passes verification.
pub fn recover(
    runner: &impl Runner,
    request: TransferRequest,
    applications: Option<Applications>,
) -> Result<TransferReview, String> {
    validate_applications(&request, applications.as_ref())?;
    storage::validate_grants(&request)?;
    let application_recovery = durable::validate_recovery(&request)?;
    let mut phase = Phase {
        attempted: true,
        ..Phase::default()
    };
    let (source, summary) = match reopen_home(&request, application_recovery)? {
        Some(unit) => {
            let summary = TransferSummary::new(unit.summary(), &request);
            phase.home = Some((unit, storage::target_identity(&request.target_home)?));
            (None, summary)
        }
        None if application_recovery => {
            return Err("The sealed data for this transfer is missing".into());
        }
        None => {
            let source = review_source(&request)?;
            let summary = TransferSummary::new(
                &source.summary().map_err(|error| error.to_string())?,
                &request,
            );
            (Some(source), summary)
        }
    };
    let verified_applications = if application_recovery {
        applications::validate_recovery(&request)?;
        applications::verify_original_config(&request)?;
        applications::verify_output(&request, runner)?
    } else {
        false
    };
    phase.applications_complete = verified_applications;
    Ok(TransferReview {
        request,
        source,
        summary,
        applications,
        recovered: true,
        application_recovery,
        verified_applications,
        phase: Arc::new(Mutex::new(phase)),
        _source_guard: None,
    })
}

/// The whole home, or only the user's Flatpak installation and app data.
fn review_source(request: &TransferRequest) -> Result<Arc<HomeSource>, String> {
    storage::validate_target(request)?;
    let exclusions = if request.copy_home {
        Vec::new()
    } else {
        application_data_exclusions(&request.source_home).map_err(|error| error.to_string())?
    };
    HomeSource::review_with_cancel_selected(
        &request.source_home,
        &exclusions,
        &AtomicBool::new(false),
    )
    .map_err(|error| error.to_string())
}

/// Reopen the Home unit an earlier attempt sealed, if there is one.
fn reopen_home(
    request: &TransferRequest,
    applications: bool,
) -> Result<Option<SealedHome>, String> {
    let workspace = request.home_workspace();
    match std::fs::symlink_metadata(&workspace) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    SealedHome::reopen(
        &workspace,
        &request.target_home,
        applications,
        &AtomicBool::new(false),
    )
    .map_err(|error| error.to_string())
}

/// System refs arrive only in a sealed unit that carries every selected app.
fn validate_applications(
    request: &TransferRequest,
    applications: Option<&Applications>,
) -> Result<(), String> {
    match applications {
        None if request.system_refs.is_empty() => Ok(()),
        Some(applications)
            if request.copy_flatpak
                && !request.system_refs.is_empty()
                && applications.covers(&request.system_refs) =>
        {
            Ok(())
        }
        _ => Err("Selected system applications must travel in their sealed unit".into()),
    }
}

impl TransferReview {
    pub fn retain_source_guard(&mut self, guard: Arc<dyn Send + Sync>) {
        self._source_guard = Some(guard);
    }

    pub fn summary(&self) -> &TransferSummary {
        &self.summary
    }
    pub fn recovered(&self) -> bool {
        self.recovered
    }
    pub fn target_home(&self) -> &std::path::Path {
        &self.request.target_home
    }
    /// Promotion can consume only this operation's admitted Home unit.
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
            .map_err(|_| "Transfer receipts became unavailable")?;
        if self.request.copy_flatpak
            && !self.request.system_refs.is_empty()
            && !phase.applications_complete
        {
            return Err(
                "Reviewed application restoration has not been independently completed".into(),
            );
        }
        let (unit, identity) = match (&phase.home, phase.busy) {
            (Some(home), false) => home,
            _ => return Err("Selected data is not admitted yet or still busy".into()),
        };
        if storage::target_identity(unit.root())? != *identity {
            return Err("Admitted data destination changed".into());
        }
        if !self.request.copy_home {
            if !self.request.copy_flatpak {
                return Err("No application selection was reviewed".into());
            }
            unit.verify(phase.applications_complete, &AtomicBool::new(false))
                .map_err(|error| error.to_string())?;
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

    /// Synchronous orchestration, useful with a fixture CLI. Native callers use
    /// `start` to avoid blocking UI threads. Cancellation stops sealing before
    /// anything is admitted; a sealed unit is reopened on retry.
    pub fn execute(
        &self,
        runner: &impl OfflineRunner,
        cancelled: &AtomicBool,
        retry: bool,
        mut event: impl FnMut(TransferEvent),
    ) -> Result<TransferReport, String> {
        let mut report = TransferReport::default();
        if cancelled.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
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
            if !retry && phase.attempted {
                return Err("Use Retry to resume this transfer".into());
            }
            phase.busy = true;
            phase.attempted = true;
            PhaseLease(Arc::clone(&self.phase))
        };
        let unit = match self.home(cancelled, &mut event) {
            Ok(unit) => unit,
            Err(_) if cancelled.load(Ordering::Relaxed) => {
                report.cancelled = true;
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        report.admitted = true;
        if cancelled.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
        }
        if self.request.copy_flatpak {
            event(TransferEvent::RestoringApplications);
        }
        if !self.request.system_refs.is_empty() {
            self.restore_applications(runner, cancelled, &unit, &mut report)?;
        }
        Ok(report)
    }

    /// The admitted Home unit: sealed from the reviewed source on first use,
    /// reopened when an earlier attempt sealed it.
    fn home(
        &self,
        cancelled: &AtomicBool,
        event: &mut impl FnMut(TransferEvent),
    ) -> Result<SealedHome, String> {
        let admitted = self
            .phase
            .lock()
            .map_err(|_| "Transfer phase became unavailable")?
            .home
            .clone();
        if let Some((unit, identity)) = admitted {
            if storage::target_identity(unit.root())? != identity {
                return Err("Transferred directory was replaced; review again".into());
            }
            return Ok(unit);
        }
        let source = self
            .source
            .as_ref()
            .ok_or("The reviewed source is no longer available; review again")?;
        let workspace = self.request.home_workspace();
        match std::fs::symlink_metadata(&workspace) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                super::migration::PinnedDirectory::open_absolute(&self.request.private_journals)
                    .and_then(|journals| journals.create_child(std::ffi::OsStr::new("home")))
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
            Ok(_) => {}
        }
        let unit = SealedHome::seal(
            source,
            &workspace,
            &self.request.target_home,
            cancelled,
            &mut |progress| {
                event(TransferEvent::Progress(progress));
                Ok(())
            },
        )
        .map_err(|error| error.to_string())?;
        let identity = storage::target_identity(unit.root())?;
        self.phase
            .lock()
            .map_err(|_| "Transfer phase became unavailable")?
            .home = Some((unit.clone(), identity));
        Ok(unit)
    }

    /// Deploy the sealed applications unit after the admitted user data is
    /// verified again, then checkpoint the verified output.
    fn restore_applications(
        &self,
        runner: &impl OfflineRunner,
        cancelled: &AtomicBool,
        home: &SealedHome,
        report: &mut TransferReport,
    ) -> Result<(), String> {
        home.verify(true, cancelled)
            .map_err(|error| error.to_string())?;
        applications::verify_original_config(&self.request)?;
        if self.application_recovery {
            durable::applications_started(&self.request).map_err(|error| error.to_string())?;
        } else {
            applications::begin(&self.request, runner)?;
        }
        let verified = self.verified_applications
            || self
                .phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?
                .applications_complete;
        let selected = || {
            self.request
                .system_refs
                .iter()
                .map(|item| item.reference.clone())
        };
        if verified {
            if !applications::verify_output(&self.request, runner)? {
                return Err("Application output proof disappeared".into());
            }
            report.flatpak.restored.extend(selected());
            return Ok(());
        }
        let unit = self.sealed(runner, cancelled)?;
        match unit.apply(runner, &self.request.target_home, cancelled) {
            Ok(_) => report.flatpak.restored.extend(selected()),
            Err(_) if cancelled.load(Ordering::Relaxed) => {}
            Err(error) => report
                .flatpak
                .failed
                .push(("applications".into(), error.to_string())),
        }
        report.cancelled = cancelled.load(Ordering::Relaxed);
        if report.flatpak.failed.is_empty() && !report.cancelled {
            home.verify(true, cancelled)
                .map_err(|error| error.to_string())?;
            applications::checkpoint(&self.request, runner, unit.deployments())?;
            self.phase
                .lock()
                .map_err(|_| "Transfer phase became unavailable")?
                .applications_complete = true;
        }
        Ok(())
    }

    /// The admitted unit; sealed from the originals on first use and reopened
    /// from the private journals after a restart.
    fn sealed(
        &self,
        runner: &impl OfflineRunner,
        cancelled: &AtomicBool,
    ) -> Result<SealedApplications, String> {
        let sealed = self
            .phase
            .lock()
            .map_err(|_| "Transfer phase became unavailable")?
            .sealed
            .clone();
        if let Some(unit) = sealed {
            return Ok(unit);
        }
        let unit = match self
            .applications
            .as_ref()
            .ok_or("Selected system applications have no sealed unit")?
        {
            Applications::Sealed(unit) => unit.clone(),
            Applications::Original(source) => {
                let workspace = self.request.private_journals.join("applications");
                match std::fs::symlink_metadata(&workspace) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        super::migration::PinnedDirectory::open_absolute(
                            &self.request.private_journals,
                        )
                        .and_then(|journals| {
                            journals.create_child(std::ffi::OsStr::new("applications"))
                        })
                        .map_err(|error| error.to_string())?;
                    }
                    Err(error) => return Err(error.to_string()),
                    Ok(_) => {}
                }
                SealedApplications::seal(source, runner, &workspace, cancelled)
                    .map_err(|error| error.to_string())?
            }
        };
        self.phase
            .lock()
            .map_err(|_| "Transfer phase became unavailable")?
            .sealed = Some(unit.clone());
        Ok(unit)
    }

    pub fn start(&self, retry: bool) -> Result<TransferWorker, String> {
        let review = self.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let (sender, events) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("imager-user-transfer".into())
            .spawn(move || {
                let runner = flatpak::portable::CancellableOfflineRunner(Arc::clone(&flag));
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
