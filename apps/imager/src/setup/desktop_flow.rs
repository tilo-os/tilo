//! UI-facing orchestration for the native Tilo Desktop installation.
//!
//! Slow host discovery, download, and helper tracking stay on workers. This
//! coordinator only consumes bounded channel events and publishes explicit UI
//! state. The privileged helper remains the final authority for installation.

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

use super::desktop_catalog::{DesktopRelease, bundled_releases};
use super::desktop_download::download_package;
use super::desktop_install::{DesktopInstallEvent, DesktopInstallRequest, InstalledDesktop};
use super::desktop_probe::{DesktopHost, DesktopReadiness, desktop_readiness, probe_desktop_host};
use super::desktop_worker::spawn_desktop_install;

const POLL_BUDGET: usize = 32;

pub type ProbeReceiver = Receiver<Result<DesktopProbeSnapshot, String>>;
pub type DownloadReceiver = Receiver<Result<PathBuf, String>>;
pub type InstallReceiver = Receiver<DesktopInstallEvent>;

#[derive(Clone, Debug)]
pub struct DesktopProbeSnapshot {
    pub host: DesktopHost,
    pub releases: Vec<DesktopRelease>,
}

#[derive(Clone, Debug)]
pub struct DesktopSelection {
    pub host: String,
    pub release: DesktopRelease,
    pub already_installed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopFailureKind {
    Readiness,
    Download,
    DownloadCancelled,
    AuthorizationCancelled,
    AuthorizationRefused,
    NativeTransaction,
    WorkerLost,
}

#[derive(Clone, Debug)]
pub enum DesktopFlowState {
    Checking,
    Unavailable {
        host: String,
        reasons: Vec<String>,
    },
    Ready(DesktopSelection),
    Downloading(DesktopSelection),
    Authorizing(DesktopSelection),
    Preparing(DesktopSelection),
    Installing(DesktopSelection),
    Verifying(DesktopSelection),
    Installed {
        selection: DesktopSelection,
        result: InstalledDesktop,
    },
    Failed {
        selection: Option<DesktopSelection>,
        kind: DesktopFailureKind,
        message: String,
    },
}

pub struct DesktopRuntime {
    pub launch_probe: fn() -> Result<ProbeReceiver, String>,
    pub launch_download: fn(DesktopRelease, Arc<AtomicBool>) -> Result<DownloadReceiver, String>,
    pub launch_install: fn(DesktopInstallRequest) -> Result<InstallReceiver, String>,
}

impl Default for DesktopRuntime {
    fn default() -> Self {
        Self {
            launch_probe: launch_live_probe,
            launch_download: launch_live_download,
            launch_install: |request| spawn_desktop_install(&request),
        }
    }
}

enum PendingOperation {
    Probe(ProbeReceiver),
    Download(DownloadReceiver),
    Install(InstallReceiver),
}

pub struct DesktopFlow {
    runtime: DesktopRuntime,
    state: DesktopFlowState,
    pending: Option<PendingOperation>,
    cancel: Option<Arc<AtomicBool>>,
    cancel_requested: bool,
    approved_selection: Option<(String, String, String)>,
    revision: u64,
    diagnostic_log: Option<PathBuf>,
}

impl DesktopFlow {
    #[must_use]
    pub fn new_live() -> Self {
        let mut flow = Self::without_start(DesktopRuntime::default());
        if let Err(error) = flow.start_check() {
            flow.fail(None, DesktopFailureKind::Readiness, error);
        }
        flow
    }

    #[must_use]
    pub fn without_start(runtime: DesktopRuntime) -> Self {
        Self {
            runtime,
            state: DesktopFlowState::Checking,
            pending: None,
            cancel: None,
            cancel_requested: false,
            approved_selection: None,
            revision: 0,
            diagnostic_log: None,
        }
    }

    /// Deterministic state for screenshots and controller-level UI tests.
    /// Live setup always uses [`Self::new_live`] and the bundled catalog.
    #[must_use]
    pub(crate) fn from_fixture_state(state: DesktopFlowState) -> Self {
        let mut flow = Self::without_start(DesktopRuntime::default());
        flow.state = state;
        flow
    }

    #[cfg(test)]
    pub(crate) fn with_runtime_and_state(runtime: DesktopRuntime, state: DesktopFlowState) -> Self {
        let mut flow = Self::without_start(runtime);
        flow.state = state;
        flow
    }

    #[must_use]
    pub fn state(&self) -> &DesktopFlowState {
        &self.state
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn has_pending_operation(&self) -> bool {
        self.pending.is_some()
    }

    #[must_use]
    pub fn desktop_available(&self) -> bool {
        self.selection().is_some()
            && !matches!(
                self.state,
                DesktopFlowState::Unavailable { .. }
                    | DesktopFlowState::Failed {
                        selection: None,
                        ..
                    }
            )
    }

    #[must_use]
    pub fn selection(&self) -> Option<&DesktopSelection> {
        match &self.state {
            DesktopFlowState::Ready(selection)
            | DesktopFlowState::Downloading(selection)
            | DesktopFlowState::Authorizing(selection)
            | DesktopFlowState::Preparing(selection)
            | DesktopFlowState::Installing(selection)
            | DesktopFlowState::Verifying(selection)
            | DesktopFlowState::Installed { selection, .. } => Some(selection),
            DesktopFlowState::Failed { selection, .. } => selection.as_ref(),
            DesktopFlowState::Checking | DesktopFlowState::Unavailable { .. } => None,
        }
    }

    #[must_use]
    pub fn diagnostic_log(&self) -> Option<&std::path::Path> {
        self.diagnostic_log.as_deref()
    }

    /// Begin a fresh non-mutating readiness check.
    ///
    /// # Errors
    ///
    /// Returns an error if the readiness worker cannot be launched.
    pub fn start_check(&mut self) -> Result<(), String> {
        self.cancel = None;
        self.cancel_requested = false;
        self.pending = Some(PendingOperation::Probe((self.runtime.launch_probe)()?));
        self.set_state(DesktopFlowState::Checking);
        Ok(())
    }

    /// Recheck the exact reviewed release before starting a download.
    ///
    /// # Errors
    ///
    /// Returns an error if no release is ready or the readiness worker cannot
    /// be launched.
    pub fn start_install(&mut self) -> Result<(), String> {
        let DesktopFlowState::Ready(selection) = &self.state else {
            return Err("Desktop installation is not ready for authorization.".into());
        };
        self.approved_selection = Some(selection_identity(selection));
        self.start_check()
    }

    /// Retry always re-probes; prior UI state never authorizes another attempt.
    ///
    /// # Errors
    ///
    /// Returns an error if the readiness worker cannot be launched.
    pub fn retry(&mut self) -> Result<(), String> {
        self.approved_selection = None;
        self.start_check()
    }

    pub fn cancel_download(&mut self) -> bool {
        if !matches!(self.state, DesktopFlowState::Downloading(_)) {
            return false;
        }
        let Some(cancel) = &self.cancel else {
            return false;
        };
        cancel.store(true, Ordering::Release);
        self.cancel_requested = true;
        true
    }

    /// Consume at most a small fixed number of worker events on one UI tick.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..POLL_BUDGET {
            enum Event {
                Probe(Box<Result<DesktopProbeSnapshot, String>>),
                Download(Result<PathBuf, String>),
                Install(DesktopInstallEvent),
                Lost,
                Empty,
            }
            let event = match self.pending.as_ref() {
                Some(PendingOperation::Probe(receiver)) => match receiver.try_recv() {
                    Ok(value) => Event::Probe(Box::new(value)),
                    Err(TryRecvError::Disconnected) => Event::Lost,
                    Err(TryRecvError::Empty) => Event::Empty,
                },
                Some(PendingOperation::Download(receiver)) => match receiver.try_recv() {
                    Ok(value) => Event::Download(value),
                    Err(TryRecvError::Disconnected) => Event::Lost,
                    Err(TryRecvError::Empty) => Event::Empty,
                },
                Some(PendingOperation::Install(receiver)) => match receiver.try_recv() {
                    Ok(value) => Event::Install(value),
                    Err(TryRecvError::Disconnected) => Event::Lost,
                    Err(TryRecvError::Empty) => Event::Empty,
                },
                None => Event::Empty,
            };
            match event {
                Event::Probe(value) => self.apply_probe(*value),
                Event::Download(value) => self.apply_download(value),
                Event::Install(value) => self.apply_install_event(value),
                Event::Lost => {
                    let selection = self.selection().cloned();
                    self.pending = None;
                    self.fail(
                        selection,
                        DesktopFailureKind::WorkerLost,
                        "Desktop setup worker stopped before reporting a terminal result. Recheck the host before retrying.".into(),
                    );
                }
                Event::Empty => break,
            }
            changed = true;
        }
        changed
    }

    fn apply_probe(&mut self, result: Result<DesktopProbeSnapshot, String>) {
        self.pending = None;
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.approved_selection = None;
                self.fail(None, DesktopFailureKind::Readiness, error);
                return;
            }
        };
        let host = host_label(&snapshot.host);
        match desktop_readiness(&snapshot.host, &snapshot.releases) {
            DesktopReadiness::Ready(release) => {
                let selection = DesktopSelection {
                    host,
                    release,
                    already_installed: false,
                };
                let approved = self.approved_selection.take();
                if approved.as_ref() == Some(&selection_identity(&selection)) {
                    self.begin_download(selection);
                } else {
                    self.set_state(DesktopFlowState::Ready(selection));
                }
            }
            DesktopReadiness::AlreadyInstalled(release) => {
                let selection = DesktopSelection {
                    host,
                    release,
                    already_installed: true,
                };
                let approved = self.approved_selection.take();
                if approved.as_ref() == Some(&selection_identity(&selection)) {
                    self.begin_download(selection);
                } else {
                    self.set_state(DesktopFlowState::Ready(selection));
                }
            }
            DesktopReadiness::Blocked(reasons) => {
                self.approved_selection = None;
                self.set_state(DesktopFlowState::Unavailable { host, reasons });
            }
        }
    }

    fn begin_download(&mut self, selection: DesktopSelection) {
        let cancel = Arc::new(AtomicBool::new(false));
        match (self.runtime.launch_download)(selection.release.clone(), Arc::clone(&cancel)) {
            Ok(receiver) => {
                self.cancel = Some(cancel);
                self.cancel_requested = false;
                self.pending = Some(PendingOperation::Download(receiver));
                self.set_state(DesktopFlowState::Downloading(selection));
            }
            Err(error) => self.fail(Some(selection), DesktopFailureKind::Download, error),
        }
    }

    #[cfg(test)]
    fn begin_download_for_test(&mut self) {
        if let Some(selection) = self.selection().cloned() {
            self.begin_download(selection);
        }
    }

    fn apply_download(&mut self, result: Result<PathBuf, String>) {
        self.pending = None;
        let selection = self.selection().cloned();
        self.cancel = None;
        let cancel_requested = std::mem::take(&mut self.cancel_requested);
        match (selection, result) {
            (Some(selection), Ok(_)) if cancel_requested => self.fail(
                Some(selection),
                DesktopFailureKind::DownloadCancelled,
                "Desktop package download cancelled before native authorization began.".into(),
            ),
            (Some(selection), Ok(package_input)) => {
                let request = DesktopInstallRequest {
                    schema: 1,
                    release_id: selection.release.id.clone(),
                    package_input,
                };
                match (self.runtime.launch_install)(request) {
                    Ok(receiver) => {
                        self.pending = Some(PendingOperation::Install(receiver));
                        self.set_state(DesktopFlowState::Authorizing(selection));
                    }
                    Err(error) => self.fail(
                        Some(selection),
                        DesktopFailureKind::NativeTransaction,
                        error,
                    ),
                }
            }
            (Some(selection), Err(error)) => {
                let kind = if cancel_requested || error.to_ascii_lowercase().contains("cancel") {
                    DesktopFailureKind::DownloadCancelled
                } else {
                    DesktopFailureKind::Download
                };
                self.fail(Some(selection), kind, error);
            }
            (None, _) => self.fail(
                None,
                DesktopFailureKind::WorkerLost,
                "Desktop download returned without a reviewed release.".into(),
            ),
        }
    }

    fn apply_install_event(&mut self, event: DesktopInstallEvent) {
        let selection = self.selection().cloned();
        match (selection, event) {
            (Some(selection), DesktopInstallEvent::Preparing) => {
                self.set_state(DesktopFlowState::Preparing(selection));
            }
            (Some(selection), DesktopInstallEvent::Installing) => {
                self.set_state(DesktopFlowState::Installing(selection));
            }
            (Some(selection), DesktopInstallEvent::Verifying) => {
                self.set_state(DesktopFlowState::Verifying(selection));
            }
            (Some(_), DesktopInstallEvent::DiagnosticLog(path)) => {
                self.diagnostic_log = Some(path);
                self.revision = self.revision.wrapping_add(1);
            }
            (Some(selection), DesktopInstallEvent::Complete(result)) => {
                self.pending = None;
                self.set_state(DesktopFlowState::Installed { selection, result });
            }
            (Some(selection), DesktopInstallEvent::Failed(error)) => {
                self.pending = None;
                self.fail(
                    Some(selection),
                    DesktopFailureKind::NativeTransaction,
                    error,
                );
            }
            (Some(selection), DesktopInstallEvent::AuthorizationCancelled) => {
                self.pending = None;
                self.fail(
                    Some(selection),
                    DesktopFailureKind::AuthorizationCancelled,
                    "Authorization was cancelled before a native package transaction began.".into(),
                );
            }
            (Some(selection), DesktopInstallEvent::AuthorizationRefused) => {
                self.pending = None;
                self.fail(
                    Some(selection),
                    DesktopFailureKind::AuthorizationRefused,
                    "Authorization was refused before a native package transaction began.".into(),
                );
            }
            (None, _) => self.fail(
                None,
                DesktopFailureKind::WorkerLost,
                "Desktop helper returned progress without a reviewed release.".into(),
            ),
        }
    }

    fn fail(
        &mut self,
        selection: Option<DesktopSelection>,
        kind: DesktopFailureKind,
        message: String,
    ) {
        self.set_state(DesktopFlowState::Failed {
            selection,
            kind,
            message,
        });
    }

    fn set_state(&mut self, state: DesktopFlowState) {
        self.state = state;
        self.revision = self.revision.wrapping_add(1);
    }
}

fn selection_identity(selection: &DesktopSelection) -> (String, String, String) {
    (
        selection.release.id.clone(),
        selection.release.version.clone(),
        selection.release.sha256.clone(),
    )
}

/// The host as people know it: "Fedora Linux 44 (x86-64)".
fn host_label(host: &DesktopHost) -> String {
    match (&host.os_release, &host.architecture) {
        (Ok(os), Ok(architecture)) => {
            let name = os.name.as_deref().unwrap_or(&os.id);
            let architecture = match architecture.as_str() {
                "x86_64" | "amd64" => "x86-64",
                "aarch64" | "arm64" => "ARM64",
                other => other,
            };
            match os.version_id.as_deref() {
                Some(version) => format!("{name} {version} ({architecture})"),
                None => format!("{name} ({architecture})"),
            }
        }
        _ => host.operating_system.clone(),
    }
}

fn launch_live_probe() -> Result<ProbeReceiver, String> {
    let handle = probe_desktop_host().map_err(|error| error.to_string())?;
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("desktop-readiness-coordinator".into())
        .spawn(move || {
            let result = handle
                .join()
                .map_err(|_| "Desktop readiness worker stopped unexpectedly.".to_string())
                .and_then(|host| {
                    bundled_releases().map(|releases| DesktopProbeSnapshot { host, releases })
                });
            let _ = sender.send(result);
        })
        .map_err(|error| format!("Cannot start Desktop readiness coordinator: {error}"))?;
    Ok(receiver)
}

fn launch_live_download(
    release: DesktopRelease,
    cancel: Arc<AtomicBool>,
) -> Result<DownloadReceiver, String> {
    let cache = tilo_paths::cache_dir()
        .ok_or("No private cache directory is available for the Desktop package.")?
        .join("tilo-imager-desktop");
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("desktop-package-download".into())
        .spawn(move || {
            let _ = sender.send(download_package(&release, &cache, &cancel));
        })
        .map_err(|error| format!("Cannot start Desktop package download: {error}"))?;
    Ok(receiver)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use super::*;
    use crate::setup::desktop_catalog::{DesktopRelease, PackageFormat, RuntimeEvidence};
    use crate::setup::desktop_install::{
        DesktopInstallEvent, DesktopInstallRequest, InstalledDesktop,
    };
    use crate::setup::desktop_probe::{
        DesktopHost, InstalledPackage, OsRelease, PackageManager, SessionSupport,
    };

    fn release(version: &str) -> DesktopRelease {
        let sha256 = "a".repeat(64);
        DesktopRelease {
            id: "fedora-44-aarch64".into(),
            version: version.into(),
            distro_id: "fedora".into(),
            distro_version: "44".into(),
            architecture: "aarch64".into(),
            package_format: PackageFormat::Rpm,
            url: "https://code.tilo.so/tilo/releases/fixture.rpm".into(),
            byte_size: 7,
            sha256: sha256.clone(),
            verified_runtime: Some(RuntimeEvidence {
                schema: 1,
                reference: "fixture evidence".into(),
                package_sha256: sha256,
                graphical_login: true,
                return_to_prior_session: true,
                identity_setup: true,
                selected_folder_import: true,
                source_intact: true,
            }),
        }
    }

    fn host(installed: InstalledPackage) -> DesktopHost {
        DesktopHost {
            operating_system: "linux".into(),
            os_release: Ok(OsRelease {
                id: "fedora".into(),
                version_id: Some("44".into()),
                name: Some("Fedora Linux".into()),
            }),
            architecture: Ok("aarch64".into()),
            package_manager: Ok(PackageManager::Dnf),
            immutable: Ok(false),
            sandboxed: Ok(false),
            session: Ok(SessionSupport {
                display_manager: "gdm.service".into(),
                other_wayland_sessions: vec!["gnome.desktop".into()],
            }),
            installed_package: Ok(installed),
        }
    }

    fn disconnected_probe() -> Result<ProbeReceiver, String> {
        let (_tx, rx) = mpsc::channel();
        Ok(rx)
    }

    fn disconnected_download(
        _: DesktopRelease,
        _: Arc<AtomicBool>,
    ) -> Result<DownloadReceiver, String> {
        let (_tx, rx) = mpsc::channel();
        Ok(rx)
    }

    fn disconnected_install(_: DesktopInstallRequest) -> Result<InstallReceiver, String> {
        let (_tx, rx) = mpsc::channel();
        Ok(rx)
    }

    static INSTALL_LAUNCHES: AtomicUsize = AtomicUsize::new(0);

    fn counting_install(_: DesktopInstallRequest) -> Result<InstallReceiver, String> {
        INSTALL_LAUNCHES.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        drop(tx);
        Ok(rx)
    }

    fn runtime() -> DesktopRuntime {
        DesktopRuntime {
            launch_probe: disconnected_probe,
            launch_download: disconnected_download,
            launch_install: disconnected_install,
        }
    }

    fn ready_flow(installed: InstalledPackage) -> DesktopFlow {
        let mut flow = DesktopFlow::without_start(runtime());
        flow.apply_probe(Ok(DesktopProbeSnapshot {
            host: host(installed),
            releases: vec![release("0.1.9-1.fc44")],
        }));
        flow
    }

    /// Readiness, recheck, download, progress and authorization outcomes.
    #[test]
    fn desktop_install_rechecks_downloads_and_reports_distinct_outcomes() {
        // Phase `readiness_publishes_exact_host_version_and_already_installed_status`.
        {
            let flow = ready_flow(InstalledPackage::Installed {
                version: "0.1.9-1.fc44".into(),
            });

            let DesktopFlowState::Ready(selection) = flow.state() else {
                panic!("matching installed package should still be ready for verification")
            };
            assert_eq!(selection.host, "Fedora Linux 44 (ARM64)");
            assert_eq!(selection.release.version, "0.1.9-1.fc44");
            assert!(selection.already_installed);
        }

        // Phase `install_rechecks_and_requires_review_when_selected_version_changes`.
        {
            let mut flow = ready_flow(InstalledPackage::Absent);
            flow.start_install().unwrap();
            assert!(matches!(flow.state(), DesktopFlowState::Checking));

            flow.apply_probe(Ok(DesktopProbeSnapshot {
                host: host(InstalledPackage::Absent),
                releases: vec![release("0.2.0-1.fc44")],
            }));

            let DesktopFlowState::Ready(selection) = flow.state() else {
                panic!("changed release must return to review")
            };
            assert_eq!(selection.release.version, "0.2.0-1.fc44");
            assert!(!flow.has_pending_operation());
        }

        // Phase `matching_recheck_downloads_then_tracks_native_progress_to_completion`.
        {
            let mut flow = ready_flow(InstalledPackage::Absent);
            flow.start_install().unwrap();
            flow.apply_probe(Ok(DesktopProbeSnapshot {
                host: host(InstalledPackage::Absent),
                releases: vec![release("0.1.9-1.fc44")],
            }));
            assert!(matches!(flow.state(), DesktopFlowState::Downloading(_)));

            flow.apply_download(Ok(PathBuf::from("/tmp/verified.rpm")));
            assert!(matches!(flow.state(), DesktopFlowState::Authorizing(_)));
            flow.apply_install_event(DesktopInstallEvent::Preparing);
            assert!(matches!(flow.state(), DesktopFlowState::Preparing(_)));
            flow.apply_install_event(DesktopInstallEvent::Installing);
            assert!(matches!(flow.state(), DesktopFlowState::Installing(_)));
            flow.apply_install_event(DesktopInstallEvent::Verifying);
            assert!(matches!(flow.state(), DesktopFlowState::Verifying(_)));
            flow.apply_install_event(DesktopInstallEvent::Complete(InstalledDesktop {
                version: "0.1.9-1.fc44".into(),
                session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
            }));
            assert!(matches!(flow.state(), DesktopFlowState::Installed { .. }));
        }

        // Phase `authorization_outcomes_and_package_failure_are_distinct_and_retry_rechecks`.
        {
            for (event, expected) in [
                (
                    DesktopInstallEvent::AuthorizationCancelled,
                    DesktopFailureKind::AuthorizationCancelled,
                ),
                (
                    DesktopInstallEvent::AuthorizationRefused,
                    DesktopFailureKind::AuthorizationRefused,
                ),
                (
                    DesktopInstallEvent::Failed("package manager failed".into()),
                    DesktopFailureKind::NativeTransaction,
                ),
            ] {
                let mut flow = ready_flow(InstalledPackage::Absent);
                flow.apply_install_event(event);
                assert!(matches!(
                    flow.state(),
                    DesktopFlowState::Failed { kind, .. } if *kind == expected
                ));
                flow.retry().unwrap();
                assert!(matches!(flow.state(), DesktopFlowState::Checking));
            }
        }
    }

    /// Cancellation and disconnection.
    #[test]
    fn desktop_download_cancellation_and_worker_loss_are_reported_truthfully() {
        // Phase `download_cancellation_is_not_reported_as_native_transaction_cancellation`.
        {
            let mut flow = ready_flow(InstalledPackage::Absent);
            flow.begin_download_for_test();
            assert!(flow.cancel_download());
            flow.apply_download(Err("Desktop package download cancelled.".into()));

            assert!(matches!(
                flow.state(),
                DesktopFlowState::Failed {
                    kind: DesktopFailureKind::DownloadCancelled,
                    ..
                }
            ));
        }

        // Phase `accepted_download_cancellation_wins_over_a_queued_success`.
        {
            INSTALL_LAUNCHES.store(0, Ordering::SeqCst);
            let mut flow = DesktopFlow::without_start(DesktopRuntime {
                launch_probe: disconnected_probe,
                launch_download: disconnected_download,
                launch_install: counting_install,
            });
            flow.apply_probe(Ok(DesktopProbeSnapshot {
                host: host(InstalledPackage::Absent),
                releases: vec![release("0.1.9-1.fc44")],
            }));
            flow.begin_download_for_test();

            assert!(flow.cancel_download());
            flow.apply_download(Ok(PathBuf::from("/tmp/already-queued.rpm")));

            assert_eq!(INSTALL_LAUNCHES.load(Ordering::SeqCst), 0);
            assert!(matches!(
                flow.state(),
                DesktopFlowState::Failed {
                    kind: DesktopFailureKind::DownloadCancelled,
                    ..
                }
            ));
        }

        // Phase `a_disconnected_operation_reports_worker_loss_without_blocking`.
        {
            let mut flow = DesktopFlow::without_start(runtime());
            flow.start_check().unwrap();

            assert!(flow.poll());
            assert!(matches!(
                flow.state(),
                DesktopFlowState::Failed {
                    kind: DesktopFailureKind::WorkerLost,
                    ..
                }
            ));
        }
    }
}
