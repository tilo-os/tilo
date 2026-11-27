//! The pages the guided setup hands its review to: Tilo Desktop for this
//! person, the read-only check of this computer before its guided pages, and
//! Full Tilo installed beside the current system.
//!
//! Each page drives the setup's own state machines (the Desktop package flow,
//! the release download and the staging install) through the journey's
//! effects, the same ones a press, an agent or a script invokes.

use std::sync::mpsc::{self, Receiver, Sender};

use hardware_report::SupportReport;

use super::InstallerPreviewEffect;
use crate::setup::actions::{HardwareProjection, InternalAvailability, InternalProjection};
use crate::setup::desktop_catalog::{DesktopRelease, PackageFormat, RuntimeEvidence};
use crate::setup::desktop_flow::{DesktopFlow, DesktopFlowState, DesktopSelection};
use crate::setup::internal_download_flow::InternalDownloadFlow;
use crate::setup::internal_install_flow::{
    self as install_flow, HostCommand, InternalInstall, Readiness, ReviewedFullOs, RunState,
};
use crate::setup::model::{Capabilities, Experience, HostOs};
use crate::setup::staging::helper::Action as HelperAction;
use crate::setup::staging::probe::HostFacts;
use crate::setup::strings::{t, t_args};
use tilo_resource::Effect as _;

/// Which page the review handed over to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Handoff {
    /// Tilo Desktop installed for this person, on this Linux.
    Desktop,
    /// This computer checked for Full Tilo before the guided pages ask
    /// anything, without installing.
    Check,
    /// Full Tilo installed beside the current system.
    Install,
}

type WorkerTask = Box<dyn FnOnce() + Send + 'static>;
type WorkerLauncher = fn(&str, WorkerTask) -> Result<(), String>;

#[derive(Debug)]
enum WorkerMessage {
    HostProbe {
        facts: HostFacts,
        records: Vec<crate::setup::staging::record::Record>,
        report: Option<SupportReport>,
    },
    HostCommandFinished {
        command: HostCommand,
        result: Result<(), String>,
    },
}

/// The page the review handed over to, with the state machines it drives.
pub(super) struct NativeSetup {
    pub(super) handoff: Handoff,
    pub(super) platform: Capabilities,
    worker_tx: Sender<WorkerMessage>,
    worker_rx: Receiver<WorkerMessage>,
    worker_launcher: WorkerLauncher,
    /// Why the last thing asked of this page did not happen, until the next
    /// thing asked of it does.
    pub(super) notice: Option<String>,
    check_facts: Option<HostFacts>,
    pub(super) support_report: Option<SupportReport>,
    pub(super) desktop_flow: DesktopFlow,
    pub(super) internal_download_flow: Option<InternalDownloadFlow>,
    pub(super) internal_download_error: Option<String>,
    pub(super) internal_install: InternalInstall,
    pub(super) host_command_pending: Option<HostCommand>,
}

impl NativeSetup {
    /// `handoff` on this computer, with the live staging and download.
    pub(super) fn new(handoff: Handoff) -> Self {
        let host = match crate::backend::setup::native_host::host() {
            crate::backend::setup::native_host::Host::Linux => HostOs::Linux,
            crate::backend::setup::native_host::Host::Windows => HostOs::Windows,
            crate::backend::setup::native_host::Host::Other => HostOs::Other,
        };
        Self::with_parts(
            handoff,
            Capabilities {
                host,
                desktop_session: false,
                internal_dual_boot: false,
            },
            launch_worker,
            InternalInstall::new(
                install_flow::StagingRuntime::live(),
                install_flow::live_install_allowed(),
                host,
            ),
        )
    }

    pub(super) fn with_parts(
        handoff: Handoff,
        platform: Capabilities,
        worker_launcher: WorkerLauncher,
        internal_install: InternalInstall,
    ) -> Self {
        let (worker_tx, worker_rx) = mpsc::channel();
        #[cfg(test)]
        let (internal_download_flow, internal_download_error) = (
            Some(InternalDownloadFlow::unavailable_fixture(platform)),
            None,
        );
        #[cfg(not(test))]
        let (internal_download_flow, internal_download_error) =
            match InternalDownloadFlow::new(platform, internal_install.link()) {
                Ok(flow) => (Some(flow), None),
                Err(error) => (None, Some(error.to_owned())),
            };
        Self {
            handoff,
            platform,
            worker_tx,
            worker_rx,
            worker_launcher,
            notice: None,
            check_facts: None,
            support_report: None,
            desktop_flow: fixture_desktop_flow(platform.desktop_session),
            internal_download_flow,
            internal_download_error,
            internal_install,
            host_command_pending: None,
        }
    }

    /// The installation the guided review decided: its facts, plan and
    /// profile, checked again against this computer before anything runs.
    pub(super) fn from_guided_review(reviewed: ReviewedFullOs) -> Result<Self, String> {
        let mut setup = Self::new(Handoff::Install);
        let has_release = setup
            .internal_download_flow
            .as_ref()
            .is_some_and(|flow| flow.offered_selection().is_some());
        setup
            .internal_install
            .adopt_reviewed(reviewed, has_release)?;
        setup.apply_install_capability();
        if !setup.platform.supports(Experience::FullOs) {
            return Err(t("imager-setup-internal-error-unavailable"));
        }
        Ok(setup)
    }

    /// Start reading this computer: its Desktop packages, and for Full Tilo
    /// its disk and hardware.
    pub(super) fn start(&mut self) {
        self.desktop_flow = DesktopFlow::new_live();
        if self.handoff != Handoff::Desktop {
            self.start_host_probe();
        }
    }

    fn installs(&self) -> bool {
        self.handoff == Handoff::Install
    }

    /// Whether the person may leave this page: nothing it started is still
    /// running or waiting for them.
    pub(super) fn navigation_allowed(&self) -> bool {
        if self.handoff == Handoff::Check {
            return self.host_command_pending.is_none();
        }
        !self.desktop_flow.has_pending_operation()
            && !self.internal_install.is_running()
            && !self.internal_install.can_restart()
            && self.internal_install.pending().is_none()
            && !self.internal_install.is_probing()
            && self.host_command_pending.is_none()
            && self
                .internal_download_flow
                .as_ref()
                .is_none_or(|flow| !flow.has_pending_operation())
    }

    pub(super) fn check_ready(&self) -> bool {
        self.handoff == Handoff::Check
            && self.internal_install.check()
            && self.internal_install.offers_full_os()
            && self
                .internal_download_flow
                .as_ref()
                .is_some_and(|flow| flow.offered_selection().is_some())
            && !self.internal_install.is_probing()
            && self.host_command_pending.is_none()
            && self.internal_install.pending().is_none()
            && self.internal_install.blockers().is_empty()
            && self.check_facts.is_some()
            && self
                .support_report
                .as_ref()
                .is_some_and(|report| !report.blocks_install())
            && matches!(self.internal_install.plan(), Some(Ok(_)))
    }

    /// What the finished check found: the facts and plan the guided pages
    /// continue from.
    pub(super) fn checked(&self) -> Result<(HostFacts, crate::backend::plan::Plan), String> {
        match (&self.check_facts, self.internal_install.plan()) {
            (Some(facts), Some(Ok(plan))) if self.check_ready() => {
                Ok((facts.clone(), plan.clone()))
            }
            _ => Err(t("imager-setup-check-not-ready")),
        }
    }

    pub(super) fn projection(&self) -> crate::setup::actions::SetupProjection {
        crate::setup::actions::SetupProjection {
            experience: match self.handoff {
                Handoff::Desktop => "desktop",
                Handoff::Check | Handoff::Install => "full_os",
            },
            next: self.check_ready(),
            desktop: crate::setup::desktop_journey::project(
                &self.desktop_flow,
                self.handoff == Handoff::Desktop,
            ),
            internal: self.internal_projection(),
            hardware: self
                .support_report
                .as_ref()
                .map(HardwareProjection::from_report),
        }
    }

    /// The page as a caller reads it: which one it is, whether the person
    /// may leave it, and what it shows.
    pub(super) fn status(&self) -> serde_json::Value {
        let mut status = self.projection().value();
        status["page"] = match self.handoff {
            Handoff::Desktop => "desktop",
            Handoff::Check => "check",
            Handoff::Install => "install",
        }
        .into();
        status["can_return"] = self.navigation_allowed().into();
        status
    }

    /// Whether `id` is one of this page's own effects.
    pub(super) fn offers(&self, id: &str) -> bool {
        let desktop = matches!(
            id,
            "install-desktop" | "cancel-desktop-download" | "recheck-desktop"
        );
        let check = matches!(
            id,
            "recheck-internal" | "internal-firmware-settings" | "internal-disable-fast-startup"
        );
        match self.handoff {
            Handoff::Desktop => desktop,
            Handoff::Check => check,
            Handoff::Install => is_native(id) && !desktop,
        }
    }

    /// Run `effect`, one of this page's own: why it did not, in words.
    pub(super) fn apply(&mut self, effect: InstallerPreviewEffect) -> Result<(), String> {
        let result = if self.offers(effect.id()) {
            match effect {
                InstallerPreviewEffect::InstallDesktop
                | InstallerPreviewEffect::CancelDesktopDownload
                | InstallerPreviewEffect::RecheckDesktop => self.apply_desktop(&effect),
                effect => self.apply_internal(effect),
            }
        } else {
            Err(t(match self.handoff {
                Handoff::Check => "imager-setup-check-only",
                Handoff::Desktop => "imager-setup-internal-error-not-review",
                Handoff::Install => "imager-setup-desktop-command-unavailable",
            }))
        };
        self.notice = result.as_ref().err().cloned();
        result
    }

    fn apply_desktop(&mut self, effect: &InstallerPreviewEffect) -> Result<(), String> {
        let desktop = crate::setup::desktop_journey::project(&self.desktop_flow, true);
        let unavailable = || t("imager-setup-desktop-command-unavailable");
        match effect {
            InstallerPreviewEffect::InstallDesktop if desktop.actions.install => {
                self.desktop_flow.start_install()
            }
            InstallerPreviewEffect::CancelDesktopDownload if desktop.actions.cancel => self
                .desktop_flow
                .cancel_download()
                .then_some(())
                .ok_or_else(unavailable),
            InstallerPreviewEffect::RecheckDesktop if desktop.actions.recheck => {
                self.desktop_flow.retry()
            }
            _ => Err(unavailable()),
        }
    }

    fn download(&mut self, missing: &str) -> Result<&mut InternalDownloadFlow, String> {
        self.internal_download_flow
            .as_mut()
            .ok_or_else(|| t(missing))
    }

    fn apply_internal(&mut self, effect: InstallerPreviewEffect) -> Result<(), String> {
        use InstallerPreviewEffect as E;
        let current_operation =
            |setup: &Self, operation: &str| match setup.internal_install.pending() {
                Some(record) if record.operation_id == operation => Ok(()),
                Some(_) => Err(t("imager-setup-internal-error-pending-changed")),
                None => Err(t("imager-setup-internal-error-no-pending")),
            };
        match effect {
            E::StartInternalDownload => {
                let flow = self.download("imager-setup-error-download-unavailable")?;
                let key = flow
                    .offered_selection()
                    .ok_or_else(|| t("imager-setup-error-download-not-offered"))?;
                flow.start_selected(key)
            }
            E::PauseInternalDownload => self
                .download("imager-setup-error-download-unavailable")?
                .pause(),
            E::ResumeInternalDownload => self
                .download("imager-setup-error-no-saved-download")?
                .resume(),
            E::CancelInternalDownload => self
                .download("imager-setup-error-download-unavailable")?
                .cancel(),
            E::DiscardInternalDownload => self
                .download("imager-setup-error-no-unfinished-download")?
                .discard(),
            E::ReleaseInternalDownload => self
                .download("imager-setup-error-no-verified-download")?
                .release(),
            E::SetInternalTargetSize { gib } => {
                let install = &self.internal_install;
                if !install.offers_full_os()
                    || install.pending().is_some()
                    || install.is_running()
                    || install.can_restart()
                {
                    return Err(t("imager-setup-internal-error-command-unavailable"));
                }
                let gib = u64::try_from(gib)
                    .map_err(|_| t("imager-setup-internal-error-command-unavailable"))?;
                self.internal_install.set_target_gib(gib)
            }
            E::InstallInternal { reviewed } => {
                if self.internal_install.reviewed_digest()?.as_deref() != Some(reviewed.as_str()) {
                    return Err(t("imager-setup-internal-error-plan-changed"));
                }
                self.start_internal_install(&reviewed)
            }
            E::RestartInternal if self.internal_install.can_restart() => {
                self.launch_host_command(HostCommand::Restart)
            }
            E::RestartInternal => Err(t("imager-setup-internal-error-no-restart")),
            E::RestartInternalLater => self.internal_install.defer_restart(),
            E::RetryInternal { operation_id } => {
                current_operation(self, &operation_id)?;
                self.launch_helper(HelperAction::Arm)
            }
            E::RemoveInternal { operation_id } => {
                current_operation(self, &operation_id)?;
                self.launch_helper(HelperAction::Remove)
            }
            E::InternalFirmwareSettings => {
                let command = self
                    .internal_install
                    .firmware_command()
                    .ok_or_else(|| t("imager-setup-internal-error-remedy-unavailable"))?;
                self.launch_host_command(command)
            }
            E::InternalDisableFastStartup
                if self
                    .internal_install
                    .command_available(HostCommand::DisableFastStartup) =>
            {
                self.launch_host_command(HostCommand::DisableFastStartup)
            }
            E::InternalDisableFastStartup => {
                Err(t("imager-setup-internal-error-remedy-unavailable"))
            }
            E::RecheckInternal
                if self.internal_install.check()
                    && !self.internal_install.is_probing()
                    && !self.internal_install.is_running() =>
            {
                self.start_host_probe();
                Ok(())
            }
            _ => Err(t("imager-setup-internal-error-command-unavailable")),
        }
    }

    fn internal_download_ready(&self) -> bool {
        self.internal_download_flow
            .as_ref()
            .is_some_and(InternalDownloadFlow::is_ready)
    }

    fn start_host_probe(&mut self) {
        if self.handoff == Handoff::Check {
            self.check_facts = None;
            self.support_report = None;
        }
        let support_report = std::sync::Arc::clone(&self.internal_install.runtime().support_report);
        let Some(task) = self.internal_install.probe_task() else {
            return;
        };
        let tx = self.worker_tx.clone();
        if let Err(error) = (self.worker_launcher)(
            "imager-host-probe",
            Box::new(move || {
                let (facts, records) = task();
                let report = Some(support_report());
                let _ = tx.send(WorkerMessage::HostProbe {
                    facts,
                    records,
                    report,
                });
            }),
        ) {
            self.internal_install.probe_failed();
            self.apply_install_capability();
            self.notice = Some(t_args(
                "imager-setup-internal-command-failed",
                &[("detail", error.into())],
            ));
        }
    }

    fn apply_install_capability(&mut self) {
        if !self.internal_install.check() {
            return;
        }
        self.platform.internal_dual_boot = self.internal_install.offers_full_os();
        if let Some(flow) = &mut self.internal_download_flow {
            flow.set_capabilities(self.platform);
        }
    }

    fn launch_host_command(&mut self, command: HostCommand) -> Result<(), String> {
        if self.host_command_pending.is_some() {
            return Err(t("imager-setup-internal-error-command-unavailable"));
        }
        let run = std::sync::Arc::clone(&self.internal_install.runtime().host_command);
        let tx = self.worker_tx.clone();
        (self.worker_launcher)(
            "imager-host-command",
            Box::new(move || {
                let result = run(command);
                let _ = tx.send(WorkerMessage::HostCommandFinished { command, result });
            }),
        )?;
        self.host_command_pending = Some(command);
        Ok(())
    }

    fn launch_helper(&mut self, action: HelperAction) -> Result<(), String> {
        let operation = self
            .internal_install
            .pending()
            .map(|record| record.operation_id.clone())
            .ok_or_else(|| t("imager-setup-internal-error-no-pending"))?;
        let task = self.internal_install.helper_task(action, &operation)?;
        if let Err(error) = (self.worker_launcher)("imager-staging-helper", task) {
            self.internal_install
                .helper_task_failed(action, error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn start_internal_install(&mut self, reviewed: &str) -> Result<(), String> {
        let ready = self.internal_download_ready();
        self.internal_install.begin_stage(reviewed, ready)?;
        let consumed = self
            .download("imager-setup-internal-error-download")
            .and_then(InternalDownloadFlow::consume);
        if let Err(error) = consumed {
            self.internal_install.abort_stage("download", error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn apply_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::HostProbe {
                facts,
                records,
                report,
            } => {
                if self.handoff == Handoff::Check {
                    self.check_facts = Some(facts.clone());
                }
                if report.is_some() {
                    self.support_report = report;
                }
                let has_release = self
                    .internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.offered_selection().is_some());
                self.internal_install
                    .accept_probe(facts, records, has_release);
                self.apply_install_capability();
            }
            WorkerMessage::HostCommandFinished { command, result } => {
                self.host_command_pending = None;
                match result {
                    Ok(()) => {
                        self.notice = None;
                        if command == HostCommand::DisableFastStartup {
                            self.start_host_probe();
                        }
                    }
                    Err(error) => {
                        self.notice = Some(t_args(
                            "imager-setup-internal-command-failed",
                            &[("detail", error.into())],
                        ));
                    }
                }
            }
        }
    }

    /// Take up what the page's workers and state machines reported. Whether
    /// anything changed.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.worker_rx.try_recv() {
            self.apply_worker_message(message);
            changed = true;
        }
        changed |= self.desktop_flow.poll();
        if let Some(flow) = &mut self.internal_download_flow {
            changed |= flow.poll();
        }
        changed |= self.internal_install.poll();
        changed |= self.abort_stage_on_download_failure();
        if self.internal_install.take_reprobe() {
            self.start_host_probe();
            changed = true;
        }
        changed
    }

    fn abort_stage_on_download_failure(&mut self) -> bool {
        if matches!(
            self.internal_install.run_state(),
            RunState::Running {
                action: HelperAction::Stage,
                ..
            }
        ) && let Some(detail) = self
            .internal_download_flow
            .as_ref()
            .and_then(InternalDownloadFlow::consume_failure)
        {
            self.internal_install.abort_stage("download", detail);
            true
        } else {
            false
        }
    }

    /// Whether a worker of this page is still to report.
    pub(super) fn needs_idle_sync(&self) -> bool {
        self.desktop_flow.has_pending_operation()
            || self
                .internal_download_flow
                .as_ref()
                .is_some_and(InternalDownloadFlow::has_pending_operation)
            || self.internal_install.is_probing()
            || self.internal_install.is_running()
            || self.host_command_pending.is_some()
    }

    pub(super) fn internal_projection(&self) -> InternalProjection {
        let install = &self.internal_install;
        let on_review = self.installs();
        let full_os = self.handoff != Handoff::Desktop;
        let ready = install.offers_full_os();
        let flow = self.internal_download_flow.as_ref();
        let running = install.is_running();
        let idle_host = self.host_command_pending.is_none();
        let pending = install.pending();
        let plan = match install.plan() {
            Some(Ok(plan)) => Some(plan),
            _ => None,
        };
        let (run, run_action, failure) = match install.run_state() {
            RunState::Idle => ("idle", None, None),
            RunState::Running { action, .. } => ("running", Some(*action), None),
            RunState::Succeeded { action } => ("succeeded", Some(*action), None),
            RunState::Failed { action, failure } => (
                "failed",
                Some(*action),
                Some(failure.code.clone().unwrap_or_else(|| "failed".into())),
            ),
        };
        let bounds = install.bounds_gib();
        let editable = self.handoff != Handoff::Check;
        InternalProjection {
            readiness: match install.readiness() {
                Readiness::NotEnabled => "not-enabled",
                Readiness::Checking => "checking",
                Readiness::NoRelease => "no-release",
                Readiness::Unsupported(_) => "unsupported",
                Readiness::Ready => "ready",
            },
            on_review,
            download: flow
                .and_then(|flow| flow.presentation().phase)
                .map_or("none", download_phase_key),
            target_gib: ready.then(|| install.target_bytes() >> 30),
            min_gib: bounds.map(|(min, _)| min),
            max_gib: bounds.map(|(_, max)| max),
            reviewed: if editable {
                install.reviewed_digest().ok().flatten()
            } else {
                None
            },
            shrink_bytes: plan
                .and_then(|plan| plan.shrink.as_ref())
                .map(|shrink| shrink.from_bytes.saturating_sub(shrink.to_bytes)),
            staging_bytes: plan.map(install_flow::staging_bytes),
            blockers: install
                .blockers()
                .iter()
                .map(|blocker| install_flow::blocker_key(*blocker))
                .collect(),
            run,
            run_action: run_action.map(|action| match action {
                HelperAction::Stage => "stage",
                HelperAction::Arm => "arm",
                HelperAction::Remove => "remove",
            }),
            failure,
            pending_operation: pending.map(|record| record.operation_id.clone()),
            availability: InternalAvailability {
                start_download: on_review && flow.is_some_and(InternalDownloadFlow::can_start),
                pause_download: on_review && flow.is_some_and(InternalDownloadFlow::can_pause),
                resume_download: on_review && flow.is_some_and(InternalDownloadFlow::can_resume),
                cancel_download: on_review && flow.is_some_and(InternalDownloadFlow::can_cancel),
                discard_download: on_review && flow.is_some_and(InternalDownloadFlow::can_discard),
                release_download: on_review && flow.is_some_and(InternalDownloadFlow::can_release),
                set_target_size: on_review
                    && ready
                    && bounds.is_some()
                    && pending.is_none()
                    && !running
                    && !install.can_restart(),
                install: on_review
                    && install
                        .install_blocker(self.internal_download_ready())
                        .is_ok(),
                restart: on_review && install.can_restart() && idle_host,
                restart_later: on_review && install.can_restart() && !install.restart_deferred(),
                retry: editable && install.check() && pending.is_some() && !running,
                remove: editable && install.check() && pending.is_some() && !running,
                firmware_settings: full_os && idle_host && install.firmware_command().is_some(),
                disable_fast_startup: full_os
                    && idle_host
                    && install.command_available(HostCommand::DisableFastStartup),
                recheck: install.check() && !install.is_probing() && !running,
            },
        }
    }

    pub(super) fn internal_readiness_text(&self) -> String {
        let host = install_flow::host_arg(self.internal_install.host());
        match self.internal_install.readiness() {
            Readiness::NotEnabled if self.handoff == Handoff::Check => {
                t("imager-setup-check-not-enabled")
            }
            Readiness::NotEnabled => t("imager-setup-internal-not-enabled"),
            Readiness::Checking => {
                t_args("imager-setup-internal-checking", &[("host", host.into())])
            }
            Readiness::NoRelease => t("imager-setup-internal-no-release"),
            Readiness::Unsupported(blockers) => t_args(
                "imager-setup-internal-unsupported",
                &[(
                    "reasons",
                    blockers
                        .iter()
                        .map(|blocker| {
                            install_flow::blocker_text(*blocker, self.internal_install.host())
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into(),
                )],
            ),
            Readiness::Ready => t_args("imager-setup-internal-ready", &[("host", host.into())]),
        }
    }
}

/// Whether `id` is one of the effects a setup page answers itself.
pub(super) fn is_native(id: &str) -> bool {
    matches!(
        id,
        "install-desktop"
            | "cancel-desktop-download"
            | "recheck-desktop"
            | "start-internal-download"
            | "pause-internal-download"
            | "resume-internal-download"
            | "cancel-internal-download"
            | "discard-internal-download"
            | "release-internal-download"
            | "set-internal-target-size"
            | "install-internal"
            | "restart-internal"
            | "restart-internal-later"
            | "retry-internal"
            | "remove-internal"
            | "internal-firmware-settings"
            | "internal-disable-fast-startup"
            | "recheck-internal"
    )
}

const fn download_phase_key(
    phase: crate::setup::internal_cache::coordinator::Phase,
) -> &'static str {
    use crate::setup::internal_cache::coordinator::Phase;
    match phase {
        Phase::Authenticating => "authenticating",
        Phase::Acquiring => "acquiring",
        Phase::Preparing => "preparing",
        Phase::Downloading => "downloading",
        Phase::Ready => "ready",
        Phase::Consuming => "consuming",
        Phase::Consumed => "consumed",
        Phase::Paused => "paused",
        Phase::Cancelled => "cancelled",
        Phase::Failed => "failed",
        Phase::Released => "released",
        Phase::Discarded => "discarded",
    }
}

fn launch_worker(name: &str, task: WorkerTask) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(task)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The release a page shows before this computer's packages are read: a
/// shot's, a test's.
pub(super) fn fixture_desktop_release() -> DesktopRelease {
    let sha256 = "a".repeat(64);
    DesktopRelease {
        id: "fedora-44-aarch64-fixture".into(),
        version: "0.1.9-1.fc44".into(),
        distro_id: "fedora".into(),
        distro_version: "44".into(),
        architecture: "aarch64".into(),
        package_format: PackageFormat::Rpm,
        url: "https://code.tilo.so/tilo/releases/fixture.rpm".into(),
        byte_size: 7,
        sha256: sha256.clone(),
        verified_runtime: Some(RuntimeEvidence {
            schema: 1,
            reference: "nonproduction screenshot fixture".into(),
            package_sha256: sha256,
            graphical_login: true,
            return_to_prior_session: true,
            identity_setup: true,
            selected_folder_import: true,
            source_intact: true,
        }),
    }
}

pub(super) fn fixture_desktop_selection(already_installed: bool) -> DesktopSelection {
    DesktopSelection {
        host: "Fedora Linux 44 (ARM64)".into(),
        release: fixture_desktop_release(),
        already_installed,
    }
}

fn fixture_desktop_flow(available: bool) -> DesktopFlow {
    let state = if available {
        DesktopFlowState::Ready(fixture_desktop_selection(false))
    } else {
        DesktopFlowState::Unavailable {
            host: "Fedora Linux 44 (ARM64)".into(),
            reasons: vec!["No Tilo Desktop release matches this computer yet.".into()],
        }
    };
    DesktopFlow::from_fixture_state(state)
}

#[cfg(test)]
#[cfg(unix)]
#[path = "native_tests.rs"]
mod tests;
