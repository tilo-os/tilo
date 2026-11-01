//! Retained, process-local UI for the guided setup flow.

mod guided_desktop;
mod support_report;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::{NodeId, Size, Style, TaffyTree};
use ui::a11y::Politeness;
use ui::icons::Icon;
use ui::theme::Theme;
use ui::tokens::typography::TextRole;
use ui::widget::NumberInput;
use ui::widget::{
    Button, ButtonRole, Column, Container, ControlSize, ListRow, Row, Section, SectionHeader,
    Spacer, Text, Widget,
};

use super::actions::{HardwareProjection, SetupEffect};
use super::desktop_catalog::{DesktopRelease, PackageFormat, RuntimeEvidence};
use super::desktop_flow::{DesktopFailureKind, DesktopFlow, DesktopFlowState, DesktopSelection};
use super::internal_download_flow::InternalDownloadFlow;
pub(crate) use super::internal_install_flow::ReviewedFullOs;
use super::internal_install_flow::{
    self as install_flow, HostCommand, InternalInstall, Readiness, RunState,
};
use super::model::{Capabilities, Experience, HostOs};
use super::staging::helper::Action as HelperAction;
use crate::resource::Imager;
use crate::window_view::{Published, PublishingWindow};
use imager_backend::support_report::SupportReport;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ViewCx};
// Page rhythm (HIG §7b): the crate's steps and the shared page sections.
use crate::page::{self, Frame, stack};

/// Display facts only; final installation still reprobes through the native helper.
#[derive(Clone)]
pub(crate) struct FullOsCheck {
    pub facts: super::staging::probe::HostFacts,
    pub plan: super::staging::plan::Plan,
}

type CommandSink = Rc<RefCell<Vec<SetupCommand>>>;
type WorkerTask = Box<dyn FnOnce() + Send + 'static>;
type WorkerLauncher = fn(&str, WorkerTask) -> Result<(), String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupCapabilities {
    pub platform: Capabilities,
    pub connected_folders: bool,
}

#[derive(Clone, Copy, Debug)]
enum SetupCommand {
    Next,
    InstallDesktop,
    CancelDesktopDownload,
    RetryDesktop,
    StartInternalDownload,
    PauseInternalDownload,
    ResumeInternalDownload,
    CancelInternalDownload,
    DiscardInternalDownload,
    ReleaseInternalDownload,
    InternalInstallUnavailable,
    /// Install, or continue the unfinished operation, with the shown plan.
    InstallInternal,
    RestartInternal,
    RestartInternalLater,
    RetryInternal,
    RemoveInternal,
    InternalFirmwareSettings,
    InternalDisableFastStartup,
    RecheckInternal,
}

#[derive(Debug)]
enum WorkerMessage {
    HostProbe {
        facts: super::staging::probe::HostFacts,
        records: Vec<super::staging::record::Record>,
        report: Option<SupportReport>,
    },
    HostCommandFinished {
        command: HostCommand,
        result: Result<(), String>,
    },
}

/// Interactive retained root for setup. All choices stay process-local.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent worker, presentation, and invalidation flags do not form one state machine"
)]
pub struct SetupApp {
    capabilities: SetupCapabilities,
    /// The route the guided journey handed over for review; none before it.
    experience: Option<Experience>,
    commands: CommandSink,
    worker_tx: Sender<WorkerMessage>,
    worker_rx: Receiver<WorkerMessage>,
    worker_launcher: WorkerLauncher,
    status: String,
    status_is_error: bool,
    composed: ui::widget::Composed,
    /// Built with live adapters: a live window starts them on attach.
    starts_when_live: bool,
    actions: Option<ui::framework::Actions<Imager>>,
    /// What the window shares with the view it stands in.
    published: Rc<RefCell<Published>>,
    guided_navigation: LocalState<bool>,
    guided_desktop: Option<guided_desktop::DesktopPreparation>,
    desktop_only: bool,
    full_os_check: Option<LocalState<Option<FullOsCheck>>>,
    check_facts: Option<super::staging::probe::HostFacts>,
    support_report: Option<SupportReport>,
    desktop_flow: DesktopFlow,
    desktop_revision: u64,
    /// Offers the familiar folders at the first Tilo login once the Desktop
    /// installation starts.
    desktop_offer: fn(),
    internal_download_flow: Option<InternalDownloadFlow>,
    internal_download_error: Option<String>,
    internal_download_revision: u64,
    internal_install: InternalInstall,
    internal_install_revision: u64,
    host_command_pending: Option<HostCommand>,
    internal_run_announced: RunState,
}

impl Default for SetupApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupApp {
    /// Continue the guided journey using the existing gated Full-OS review and
    /// semantic actions. No duplicate experience/folder setup or helper launch.
    pub(crate) fn from_guided_review(reviewed: ReviewedFullOs) -> Result<Self, String> {
        let mut app = Self::new();
        app.adopt_guided_review(reviewed)?;
        Ok(app)
    }

    fn adopt_guided_review(&mut self, reviewed: ReviewedFullOs) -> Result<(), String> {
        let has_release = self
            .internal_download_flow
            .as_ref()
            .is_some_and(|flow| flow.offered_selection().is_some());
        self.internal_install
            .adopt_reviewed(reviewed, has_release)?;
        self.apply_install_capability();
        if !self.capabilities.platform.supports(Experience::FullOs) {
            return Err(super::strings::t("imager-setup-internal-error-unavailable"));
        }
        self.experience = Some(Experience::FullOs);
        self.starts_when_live = false;
        self.refresh_guided_navigation();
        self.status.clear();
        self.status_is_error = false;
        self.composed.invalidate();
        Ok(())
    }

    /// The same live navigation decision used by the outer guided controls.
    pub(crate) fn guided_navigation_guard(&self) -> LocalState<bool> {
        self.guided_navigation.clone()
    }

    fn refresh_guided_navigation(&self) {
        if self.full_os_check.is_some() {
            self.guided_navigation
                .set(self.host_command_pending.is_none());
            return;
        }
        let allowed = self.guided_desktop_can_navigate()
            && !self.internal_install.is_running()
            && !self.internal_install.can_restart()
            && self.internal_install.pending().is_none()
            && !self.internal_install.is_probing()
            && self.host_command_pending.is_none()
            && self
                .internal_download_flow
                .as_ref()
                .is_none_or(|flow| !flow.has_pending_operation());
        self.guided_navigation.set(allowed);
    }

    /// Live setup with only capabilities backed by actual adapters.
    /// Platform identity alone never enables Desktop or Full Tilo. Full Tilo
    /// becomes available only after the host probe passes, the catalog offers
    /// an internal release for this architecture and the product gate is open
    /// (design spec §5, §9). Nothing outside the process starts until it is
    /// attached to a live window (or [`Self::start_live`] is called).
    #[must_use]
    pub fn new() -> Self {
        let host = match imager_backend::setup::native_host::host() {
            imager_backend::setup::native_host::Host::Linux => HostOs::Linux,
            imager_backend::setup::native_host::Host::Windows => HostOs::Windows,
            imager_backend::setup::native_host::Host::Other => HostOs::Other,
        };
        let capabilities = SetupCapabilities {
            platform: Capabilities {
                host,
                desktop_session: false,
                internal_dual_boot: false,
            },
            connected_folders: false,
        };
        let mut app = Self::with_parts(
            capabilities,
            launch_worker,
            InternalInstall::new(
                install_flow::StagingRuntime::live(),
                install_flow::live_gate_open(),
                host,
            ),
        );
        app.starts_when_live = true;
        app
    }

    /// Review the canonical native Full Tilo pipeline without prepared identity
    /// or migration custody. Intent does not enable installation capabilities.
    #[must_use]
    pub fn for_full_os() -> Self {
        let mut app = Self::new();
        app.review_full_os();
        app
    }

    /// Read-only early compatibility review. This mode never grants installation custody.
    pub(crate) fn for_full_os_check(result: LocalState<Option<FullOsCheck>>) -> Self {
        let mut app = Self::for_full_os();
        app.full_os_check = Some(result);
        app.refresh_guided_navigation();
        app
    }

    fn check_ready(&self) -> bool {
        self.full_os_check.is_some()
            && self.internal_install.gate()
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

    fn finish_full_os_check(&mut self) -> Result<(), String> {
        if !self.check_ready() {
            return Err(super::strings::t("imager-setup-check-not-ready"));
        }
        if let (Some(result), Some(facts), Some(Ok(plan))) = (
            &self.full_os_check,
            &self.check_facts,
            self.internal_install.plan(),
        ) {
            result.set(Some(FullOsCheck {
                facts: facts.clone(),
                plan: plan.clone(),
            }));
        }
        Ok(())
    }

    fn review_full_os(&mut self) {
        self.experience = Some(Experience::FullOs);
        self.refresh_guided_navigation();
        self.status.clear();
    }

    /// Review native packages for the existing Linux account. No identity,
    /// migration, familiar-folder lookup or disk probing belongs to this route.
    #[must_use]
    pub fn for_desktop() -> Self {
        let mut app = Self::new();
        app.desktop_only = true;
        app.experience = Some(Experience::Desktop);
        app.refresh_guided_navigation();
        app.status.clear();
        app
    }

    /// Start what live setup reads from this computer: the Desktop host
    /// check, the familiar-folder lookup and the Full Tilo host probe.
    pub fn start_live(&mut self) {
        self.starts_when_live = false;
        self.desktop_flow = DesktopFlow::new_live();
        if let Some(preparation) = &self.guided_desktop {
            self.desktop_flow
                .require_account_protocol(preparation.prepared.account_custody_protocol());
            self.desktop_flow
                .require_service_session_protocol(preparation.prepared.service_session_protocol());
            self.refresh_guided_navigation();
            return;
        }
        if !self.desktop_only {
            self.start_host_probe();
        }
        self.refresh_guided_navigation();
    }

    /// Deterministic constructor for tests and screenshot fixtures.
    #[must_use]
    pub fn with_capabilities(capabilities: SetupCapabilities) -> Self {
        Self::with_worker_launcher(capabilities, launch_worker)
    }

    fn with_worker_launcher(
        capabilities: SetupCapabilities,
        worker_launcher: WorkerLauncher,
    ) -> Self {
        Self::with_parts(
            capabilities,
            worker_launcher,
            InternalInstall::gated(capabilities.platform.host),
        )
    }

    fn with_parts(
        capabilities: SetupCapabilities,
        worker_launcher: WorkerLauncher,
        internal_install: InternalInstall,
    ) -> Self {
        let (worker_tx, worker_rx) = mpsc::channel();
        let desktop_flow = fixture_desktop_flow(capabilities.platform.desktop_session);
        #[cfg(test)]
        let (internal_download_flow, internal_download_error) = (
            Some(InternalDownloadFlow::unavailable_fixture(
                capabilities.platform,
            )),
            None,
        );
        #[cfg(not(test))]
        let (internal_download_flow, internal_download_error) =
            match InternalDownloadFlow::new(capabilities.platform, internal_install.link()) {
                Ok(flow) => (Some(flow), None),
                Err(error) => (None, Some(error)),
            };
        Self {
            capabilities,
            experience: None,
            commands: Rc::new(RefCell::new(Vec::new())),
            worker_tx,
            worker_rx,
            worker_launcher,
            status: "Choose an available experience to begin.".into(),
            status_is_error: false,
            composed: ui::widget::Composed::default(),
            starts_when_live: false,
            actions: None,
            published: Rc::default(),
            guided_navigation: LocalState::new(false),
            guided_desktop: None,
            desktop_only: false,
            full_os_check: None,
            check_facts: None,
            support_report: None,
            desktop_flow,
            desktop_revision: 0,
            // Tests never look up or offer this computer's folders.
            desktop_offer: if cfg!(test) {
                || {}
            } else {
                super::desktop_continuation::offer_familiar_folders
            },
            internal_download_flow,
            internal_download_error,
            internal_download_revision: 0,
            internal_install,
            internal_install_revision: 0,
            host_command_pending: None,
            internal_run_announced: RunState::Idle,
        }
    }

    /// The reviewed route; Desktop until the journey hands one over.
    fn reviewed(&self) -> Experience {
        self.experience.unwrap_or(Experience::Desktop)
    }

    fn on_review_for(&self, experience: Experience) -> bool {
        self.experience == Some(experience)
    }

    fn setup_projection(&self) -> super::actions::SetupProjection {
        let experience = self.reviewed();
        super::actions::SetupProjection {
            experience: match experience {
                Experience::Desktop => "desktop",
                Experience::FullOs => "full_os",
            },
            next: self.check_ready(),
            desktop: super::desktop_journey::project(
                &self.desktop_flow,
                self.on_review_for(Experience::Desktop),
            ),
            internal: self.internal_projection(),
            hardware: self
                .support_report
                .as_ref()
                .map(HardwareProjection::from_report),
        }
    }

    /// Answer `effect` as its button would, after checking it against what
    /// is shown now, with the setup's status afterwards.
    ///
    /// # Errors
    /// Returns why the effect cannot run in the current setup state.
    pub fn answer(&mut self, effect: SetupEffect) -> Result<serde_json::Value, String> {
        if self.full_os_check.is_some() {
            match &effect {
                SetupEffect::ContinueSetup => {
                    self.finish_full_os_check()?;
                    return Ok(self.setup_projection().value());
                }
                SetupEffect::ReadSetupStatus
                | SetupEffect::ReadDesktopStatus
                | SetupEffect::RecheckInternal
                | SetupEffect::InternalFirmwareSettings
                | SetupEffect::InternalDisableFastStartup => {}
                _ => return Err(super::strings::t("imager-setup-check-only")),
            }
        }
        let command = match effect {
            SetupEffect::ReadSetupStatus => return Ok(self.setup_projection().value()),
            SetupEffect::ReadDesktopStatus => {
                return serde_json::to_value(self.setup_projection().desktop)
                    .map_err(|e| e.to_string());
            }
            SetupEffect::InstallDesktop
            | SetupEffect::CancelDesktopDownload
            | SetupEffect::RecheckDesktop => return self.answer_desktop(effect),
            SetupEffect::RetryDesktop => {
                if !self.guided_delivery_retry_available() || !self.retry_guided_desktop() {
                    return Err(super::strings::t("installer-desktop-unavailable"));
                }
                self.refresh_guided_navigation();
                return Ok(self.setup_projection().value());
            }
            SetupEffect::UseMigrationIntent { directory } => {
                if self.experience != Some(Experience::FullOs)
                    || self.internal_install.is_running()
                    || self.internal_install.pending().is_some()
                    || !matches!(self.internal_install.run_state(), RunState::Idle)
                {
                    return Err(super::strings::t(
                        "imager-setup-error-migration-intent-unavailable",
                    ));
                }
                let directory = zeroize::Zeroizing::new(directory);
                let intent = imager_backend::setup::migration_intent::MigrationIntent::load(
                    std::path::Path::new(directory.as_str()),
                )
                .map_err(|_| super::strings::t("imager-setup-error-migration-intent-invalid"))?;
                self.internal_install.set_migration_intent(Some(intent))?;
                return Ok(self.setup_projection().value());
            }
            SetupEffect::ContinueSetup => SetupCommand::Next,
            effect => match self.internal_operation_command(effect)? {
                Some(command) => command,
                None => return Ok(self.setup_projection().value()),
            },
        };
        self.apply_command(command);
        if self.status_is_error {
            return Err(self.status.clone());
        }
        Ok(self.setup_projection().value())
    }

    fn answer_desktop(&mut self, effect: SetupEffect) -> Result<serde_json::Value, String> {
        let state = self.setup_projection();
        let allowed = match effect {
            SetupEffect::InstallDesktop => state.desktop.actions.install,
            SetupEffect::CancelDesktopDownload => state.desktop.actions.cancel,
            SetupEffect::RecheckDesktop => state.desktop.actions.recheck,
            _ => false,
        };
        if !allowed {
            return Err(super::strings::t(
                "imager-setup-desktop-command-unavailable",
            ));
        }
        self.status_is_error = false;
        match effect {
            SetupEffect::InstallDesktop => self.start_desktop_install(),
            SetupEffect::CancelDesktopDownload => {
                if !self.desktop_flow.cancel_download() {
                    return Err(super::strings::t(
                        "imager-setup-desktop-command-unavailable",
                    ));
                }
            }
            SetupEffect::RecheckDesktop => self.desktop_flow.retry()?,
            _ => unreachable!("validated Desktop effect"),
        }
        self.composed.invalidate();
        self.refresh_guided_navigation();
        if self.status_is_error {
            return Err(self.status.clone());
        }
        Ok(self.setup_projection().value())
    }

    fn guided_delivery_retry_available(&self) -> bool {
        {
            self.guided_desktop
                .as_ref()
                .is_some_and(|p| p.failed && p.pending.is_none())
        }
    }
    fn retry_guided_desktop(&mut self) -> bool {
        if let Some(preparation) = &mut self.guided_desktop
            && preparation.failed
            && preparation.pending.is_none()
        {
            preparation.failed = false;
            self.status_is_error = false;
            self.poll_guided_desktop();
            return true;
        }
        false
    }
    fn start_desktop_install(&mut self) {
        match self.desktop_flow.start_install() {
            Ok(()) => {
                (self.desktop_offer)();
                self.status = "Rechecking this host before download…".into();
            }
            Err(error) => {
                self.status = error;
                self.status_is_error = true;
            }
        }
    }

    fn apply_command(&mut self, command: SetupCommand) {
        if self.full_os_check.is_some() {
            match command {
                SetupCommand::Next => {
                    let result = self.finish_full_os_check();
                    self.status_is_error = result.is_err();
                    if let Err(error) = result {
                        self.status = error;
                    }
                    return;
                }
                SetupCommand::RecheckInternal
                | SetupCommand::InternalFirmwareSettings
                | SetupCommand::InternalDisableFastStartup => {}
                _ => {
                    self.status = super::strings::t("imager-setup-check-only");
                    self.status_is_error = true;
                    return;
                }
            }
        }
        self.status_is_error = false;
        match command {
            SetupCommand::Next => {
                self.status = "Review is the last setup step.".into();
                self.status_is_error = true;
            }
            SetupCommand::InstallDesktop => self.start_desktop_install(),
            SetupCommand::CancelDesktopDownload => {
                if self.desktop_flow.cancel_download() {
                    self.status = "Cancelling the package download…".into();
                }
            }
            SetupCommand::RetryDesktop if self.retry_guided_desktop() => {}
            SetupCommand::RetryDesktop => match self.desktop_flow.retry() {
                Ok(()) => self.status = "Rechecking this host before retry…".into(),
                Err(error) => {
                    self.status = error;
                    self.status_is_error = true;
                }
            },
            SetupCommand::StartInternalDownload => {
                let on_full_os_review = { self.experience == Some(Experience::FullOs) };
                let result = if on_full_os_review {
                    self.internal_download_flow
                        .as_mut()
                        .ok_or_else(|| "Full Tilo download is unavailable.".to_string())
                        .and_then(|flow| {
                            let key = flow
                                .offered_selection()
                                .ok_or("No Full Tilo download is currently offered.")?;
                            flow.start_selected(key)
                        })
                } else {
                    Err("Full Tilo downloads can start only from the Full Tilo review.".into())
                };
                self.apply_internal_command_result(result, "Downloading Tilo…");
            }
            SetupCommand::PauseInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "Full Tilo download is unavailable.".to_string())
                    .and_then(InternalDownloadFlow::pause);
                self.apply_internal_command_result(result, "Pausing download…");
            }
            SetupCommand::ResumeInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "No saved download is available.".to_string())
                    .and_then(InternalDownloadFlow::resume);
                self.apply_internal_command_result(result, "Downloading Tilo…");
            }
            SetupCommand::CancelInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "Full Tilo download is unavailable.".to_string())
                    .and_then(InternalDownloadFlow::cancel);
                self.apply_internal_command_result(result, "Cancelling download…");
            }
            SetupCommand::DiscardInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "No unfinished download data is available.".to_string())
                    .and_then(InternalDownloadFlow::discard);
                self.apply_internal_command_result(result, "Discarding unfinished download data…");
            }
            SetupCommand::ReleaseInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "No verified download is held.".to_string())
                    .and_then(InternalDownloadFlow::release);
                self.apply_internal_command_result(result, "Keeping download for later…");
            }
            SetupCommand::InternalInstallUnavailable => {
                self.status =
                    "Installation has not started. Protected staging is not available yet.".into();
                self.status_is_error = true;
            }
            SetupCommand::InstallInternal
            | SetupCommand::RestartInternal
            | SetupCommand::RestartInternalLater
            | SetupCommand::RetryInternal
            | SetupCommand::RemoveInternal
            | SetupCommand::InternalFirmwareSettings
            | SetupCommand::InternalDisableFastStartup
            | SetupCommand::RecheckInternal => self.apply_internal_install_command(command),
        }
        self.composed.invalidate();
        self.refresh_guided_navigation();
    }

    fn apply_internal_command_result(&mut self, result: Result<(), String>, pending: &str) {
        match result {
            Ok(()) => {
                self.status = pending.into();
                self.status_is_error = false;
            }
            Err(error) => {
                self.status = error;
                self.status_is_error = true;
            }
        }
    }

    fn apply_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::HostProbe {
                facts,
                records,
                report,
            } => {
                if self.full_os_check.is_some() {
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
                self.apply_host_command_result(command, result);
            }
        }
        self.composed.invalidate();
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.worker_rx.try_recv() {
            self.apply_worker_message(message);
            changed = true;
        }
        changed |= self.desktop_flow.poll();
        if self.desktop_revision != self.desktop_flow.revision() {
            self.desktop_revision = self.desktop_flow.revision();
            let mut platform = self.capabilities.platform;
            platform.desktop_session = self.desktop_flow.desktop_available();
            self.capabilities.platform = platform;
            if self.on_review_for(Experience::Desktop) {
                match self.desktop_flow.state() {
                    DesktopFlowState::Installed { .. } => {
                        self.status =
                            "Tilo Desktop is installed and verified. Sign out when you are ready."
                                .into();
                        self.status_is_error = false;
                    }
                    DesktopFlowState::Failed { message, .. } => {
                        self.status = message.clone();
                        self.status_is_error = true;
                    }
                    _ => {}
                }
            }
            self.composed.invalidate();
            changed = true;
        }
        let on_full_os_review = self.on_review_for(Experience::FullOs);
        if let Some(flow) = &mut self.internal_download_flow {
            changed |= flow.poll();
            if self.internal_download_revision != flow.revision() {
                self.internal_download_revision = flow.revision();
                if on_full_os_review {
                    self.status.clone_from(&flow.presentation().title);
                    self.status_is_error = matches!(
                        flow.presentation().phase,
                        Some(super::internal_cache::coordinator::Phase::Failed)
                    );
                }
                self.composed.invalidate();
                changed = true;
            }
        }
        {
            changed |= self.poll_internal_install();
        }
        {
            changed |= self.poll_guided_desktop();
            self.refresh_guided_navigation();
        }
        changed
    }

    fn drain_commands(&mut self) -> bool {
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let changed = !commands.is_empty();
        for command in commands {
            self.apply_command(command);
        }
        changed
    }

    fn button(&self, id: &str, label: &str, command: SetupCommand) -> Button {
        let effect = match command {
            SetupCommand::InstallDesktop => Some(SetupEffect::InstallDesktop),
            SetupCommand::CancelDesktopDownload => Some(SetupEffect::CancelDesktopDownload),
            SetupCommand::RetryDesktop if self.guided_delivery_retry_available() => {
                Some(SetupEffect::RetryDesktop)
            }
            SetupCommand::RetryDesktop => Some(SetupEffect::RecheckDesktop),
            _ => None,
        };
        if let (Some(actions), Some(effect)) = (&self.actions, effect) {
            return actions.bind_effect_invocation(
                Button::new(id, label).control_size(ControlSize::Regular),
                &effect,
            );
        }
        let sink = Rc::clone(&self.commands);
        Button::new(id, label)
            .control_size(ui::widget::ControlSize::Regular)
            .on_effect(move || sink.borrow_mut().push(command))
    }

    /// Tilo Desktop on the review: what will happen, and on which computer.
    fn desktop_install_card(&self) -> Section {
        let detail = match self.desktop_flow.state() {
            DesktopFlowState::Checking => {
                "Checking this computer and the signed Tilo Desktop catalog…".to_string()
            }
            DesktopFlowState::Unavailable { host, .. } => {
                format!("Tilo Desktop isn't available for {host} yet.")
            }
            DesktopFlowState::Ready(selection) => {
                if selection.already_installed {
                    format!(
                        "Tilo Desktop {} is already installed on {}. Continue to check the Tilo session; approve the check when asked.",
                        selection.release.version, selection.host
                    )
                } else {
                    format!(
                        "Tilo Desktop {} is ready for {}. Continue to download it; approve the installation when asked.",
                        selection.release.version, selection.host
                    )
                }
            }
            DesktopFlowState::Downloading(selection) => format!(
                "Downloading and checking Tilo Desktop {} for {}. Cancelling now stops before anything is installed.",
                selection.release.version, selection.host
            ),
            DesktopFlowState::Authorizing(selection) => format!(
                "Waiting for you to approve installing Tilo Desktop {} on {}.",
                selection.release.version, selection.host
            ),
            DesktopFlowState::Preparing(selection) => format!(
                "Checking Tilo Desktop {} for {} once more before installing.",
                selection.release.version, selection.host
            ),
            DesktopFlowState::Installing(selection) => format!(
                "Installing Tilo Desktop {} on {}. Closing setup does not stop the installation.",
                selection.release.version, selection.host
            ),
            DesktopFlowState::Verifying(selection) => {
                format!("Checking the installed Tilo session on {}.", selection.host)
            }
            DesktopFlowState::Installed { selection, result } => {
                if self.guided_desktop.is_some() {
                    self.status.clone()
                } else {
                    format!(
                        "Tilo Desktop {} is installed and verified on {}. Save your work and sign out when you are ready, then choose Tilo on the login screen. Your current session stays the default.",
                        result.version, selection.host
                    )
                }
            }
            DesktopFlowState::Failed { kind, message, .. } => {
                let what = match kind {
                    DesktopFailureKind::DownloadCancelled => "The download was cancelled.",
                    DesktopFailureKind::AuthorizationCancelled => {
                        "The installation was not approved."
                    }
                    DesktopFailureKind::AuthorizationRefused => {
                        "Installing needs an administrator's approval."
                    }
                    DesktopFailureKind::WorkerLost => "Checking this computer stopped early.",
                    DesktopFailureKind::Readiness => "This computer is not ready for Tilo Desktop.",
                    DesktopFailureKind::Download => "The download failed.",
                    DesktopFailureKind::NativeTransaction => "The installation failed.",
                };
                if self.desktop_only {
                    format!("{what} {message}")
                } else {
                    format!("{what} {message} Your folder choices are kept; check again to retry.")
                }
            }
        };
        let retained = if self.desktop_only {
            super::strings::t("installer-desktop-keep-existing")
        } else {
            match self.capabilities.platform.host {
                HostOs::Windows => "Windows stays installed and remains the startup default.",
                HostOs::Linux => {
                    "Your Linux installation stays as it is and remains the default session."
                }
                HostOs::Other => "Your current system stays installed and remains the default.",
            }
            .to_owned()
        };
        Section::new(vec![Box::new(
            page::related()
                .push(Box::new(
                    Text::new(&detail)
                        .with_id("setup.desktop.status")
                        .with_live(Politeness::Polite),
                ))
                .push(Box::new(Text::new(&retained).dimmed())),
        )])
        .with_header(SectionHeader::heading("Tilo Desktop"))
    }

    fn internal_download_card(&self) -> Section {
        let mut content = page::related();
        if let Some(flow) = &self.internal_download_flow {
            let presentation = flow.presentation();
            let summary = if presentation.phase.is_none()
                && let Some(error) = flow.catalog_error()
            {
                error.to_string()
            } else {
                presentation.summary.clone()
            };
            content = content.push(Box::new(
                Text::new(&summary)
                    .with_id("setup.internal.status")
                    .with_live(Politeness::Polite),
            ));
            let mut actions = Row::new();
            let mut any = false;
            for (shown, id, label, command) in [
                (
                    flow.can_start(),
                    "setup.internal.start",
                    "Download Tilo",
                    SetupCommand::StartInternalDownload,
                ),
                (
                    flow.can_pause(),
                    "setup.internal.pause",
                    "Pause download",
                    SetupCommand::PauseInternalDownload,
                ),
                (
                    flow.can_cancel(),
                    "setup.internal.cancel",
                    "Cancel download",
                    SetupCommand::CancelInternalDownload,
                ),
                (
                    flow.can_resume(),
                    "setup.internal.resume",
                    "Resume download",
                    SetupCommand::ResumeInternalDownload,
                ),
                (
                    flow.can_release(),
                    "setup.internal.release",
                    "Keep for later",
                    SetupCommand::ReleaseInternalDownload,
                ),
                (
                    flow.can_discard(),
                    "setup.internal.discard",
                    "Discard unfinished download",
                    SetupCommand::DiscardInternalDownload,
                ),
            ] {
                if shown {
                    actions = actions.push(Box::new(self.button(id, label, command)));
                    any = true;
                }
            }
            if any {
                content = content.push(Box::new(actions));
            }
            return Section::new(vec![Box::new(content)])
                .with_header(SectionHeader::heading(presentation.title.clone()));
        }
        let detail = self.internal_download_error.as_deref().unwrap_or(
            "Tilo can't be downloaded on this computer, so installation has not started.",
        );
        content = content.push(Box::new(
            Text::new(detail)
                .with_id("setup.internal.status")
                .with_live(Politeness::Assertive),
        ));
        Section::new(vec![Box::new(content)])
            .with_header(SectionHeader::heading("Full Tilo download"))
    }

    fn review_body(&self) -> Column {
        if self.full_os_check.is_some() {
            let readiness = if self
                .support_report
                .as_ref()
                .is_some_and(SupportReport::blocks_install)
            {
                "A component needed for the desktop is unsupported. Resolve it below, then check again."
                    .to_string()
            } else {
                self.internal_readiness_text()
            };
            let mut body = stack()
                .push(Box::new(Text::new(&super::strings::t(
                    "imager-setup-check-title",
                ))))
                .push(Box::new(
                    Text::new(&readiness).with_live(Politeness::Polite),
                ));
            for section in self.hardware_report_sections() {
                body = body.push(section);
            }
            return body
                .push(Box::new(self.internal_blockers_card(None)))
                .push(Box::new(
                    self.button(
                        "setup.check.continue",
                        &super::strings::t("installer-preview-continue"),
                        SetupCommand::Next,
                    )
                    .with_enabled(self.check_ready()),
                ));
        }
        let experience = self.reviewed();
        let internal_offered =
            experience == Experience::FullOs && self.internal_install.offers_full_os();
        let mut body = page::stack();
        if experience == Experience::Desktop {
            body = body.push(Box::new(self.desktop_install_card()));
        }
        if internal_offered {
            if let Some(run) = self.internal_run_card() {
                body = body.push(Box::new(run));
            }
            if let Some(resume) = self.internal_resume_card() {
                body = body.push(Box::new(resume));
            }
            body = body
                .push(Box::new(self.internal_plan_card()))
                .push(Box::new(self.internal_blockers_card(None)));
        }
        if experience == Experience::FullOs && self.support_report.is_some() {
            // The direct Full Tilo review reads this computer automatically,
            // before any identity or migration work. Desktop has its own
            // package and session check and does not use this report.
            for section in self.hardware_report_sections() {
                body = body.push(section);
            }
        }
        if experience == Experience::FullOs {
            body = body.push(Box::new(self.internal_download_card()));
        }
        if experience == Experience::FullOs && !internal_offered {
            body = body.push(Box::new(
                Section::new(vec![Box::new(
                    Text::new(&self.internal_readiness_text()).with_live(Politeness::Polite),
                )])
                .with_header(SectionHeader::heading("Before installing")),
            ));
        }
        body
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the wizard shell declares its body, status, and state-dependent navigation together"
    )]
    fn build_composition(&self) -> Box<dyn Widget> {
        let body = self.review_body();
        let mut nav = Row::new().no_wrap();
        nav = nav.push(Box::new(Spacer::new()));
        let internal_nav =
            self.reviewed() == Experience::FullOs && self.internal_install.offers_full_os();
        if internal_nav {
            {
                nav = self.internal_nav(nav);
            }
        } else {
            let experience = self.reviewed();
            // The review's main action names what happens; once Desktop is
            // installed there is nothing left to do here.
            let primary = if experience == Experience::FullOs {
                Some((
                    "setup.internal.install-unavailable",
                    "Install Tilo",
                    SetupCommand::InternalInstallUnavailable,
                    false,
                ))
            } else {
                match self.desktop_flow.state() {
                    DesktopFlowState::Ready(_) => Some((
                        "setup.install",
                        "Install Tilo Desktop",
                        SetupCommand::InstallDesktop,
                        true,
                    )),
                    DesktopFlowState::Downloading(_) => Some((
                        "setup.cancel-download",
                        "Cancel download",
                        SetupCommand::CancelDesktopDownload,
                        true,
                    )),
                    DesktopFlowState::Failed { .. } | DesktopFlowState::Unavailable { .. } => {
                        Some((
                            "setup.retry-desktop",
                            "Check again",
                            SetupCommand::RetryDesktop,
                            true,
                        ))
                    }
                    DesktopFlowState::Installed { .. }
                        if self.guided_delivery_retry_available() =>
                    {
                        Some((
                            "setup.retry-desktop",
                            "Try again",
                            SetupCommand::RetryDesktop,
                            true,
                        ))
                    }
                    DesktopFlowState::Installed { .. } => None,
                    _ => Some((
                        "setup.install",
                        "Installing…",
                        SetupCommand::InstallDesktop,
                        false,
                    )),
                }
            };
            if let Some((id, label, command, enabled)) = primary {
                nav = nav.push(Box::new(
                    self.button(id, label, command)
                        .role(ButtonRole::Primary)
                        .with_enabled(enabled),
                ));
            }
        }

        let frame = Frame::new(body);
        frame
            .notice(page::notice(&self.status, self.status_is_error))
            .actions(nav)
            .build()
    }
}

/// Full Tilo installed alongside the current system (design spec §5, §8).
impl SetupApp {
    fn on_full_os_review(&self) -> bool {
        self.on_review_for(Experience::FullOs)
    }

    /// Validate a semantic Full Tilo operation against what is shown now and
    /// map it to the command its button dispatches (`None` when it was
    /// applied here). Stale digests and operation IDs are refused before
    /// anything runs.
    fn internal_operation_command(
        &mut self,
        operation: SetupEffect,
    ) -> Result<Option<SetupCommand>, String> {
        use SetupEffect as Op;
        if self.full_os_check.is_some()
            && !matches!(
                &operation,
                Op::RecheckInternal | Op::InternalFirmwareSettings | Op::InternalDisableFastStartup
            )
        {
            return Err(super::strings::t("imager-setup-check-only"));
        }
        let t = super::strings::t;
        let review_only = |app: &Self| {
            if app.on_full_os_review() {
                Ok(())
            } else {
                Err(t("imager-setup-internal-error-not-review"))
            }
        };
        let current_operation = |app: &Self, operation: &str| match app.internal_install.pending() {
            Some(record) if record.operation_id == operation => Ok(()),
            Some(_) => Err(t("imager-setup-internal-error-pending-changed")),
            None => Err(t("imager-setup-internal-error-no-pending")),
        };
        Ok(Some(match operation {
            Op::StartInternalDownload => {
                review_only(self)?;
                SetupCommand::StartInternalDownload
            }
            Op::PauseInternalDownload => {
                review_only(self)?;
                SetupCommand::PauseInternalDownload
            }
            Op::ResumeInternalDownload => {
                review_only(self)?;
                SetupCommand::ResumeInternalDownload
            }
            Op::CancelInternalDownload => {
                review_only(self)?;
                SetupCommand::CancelInternalDownload
            }
            Op::DiscardInternalDownload => {
                review_only(self)?;
                SetupCommand::DiscardInternalDownload
            }
            Op::ReleaseInternalDownload => {
                review_only(self)?;
                SetupCommand::ReleaseInternalDownload
            }
            Op::SetInternalTargetSize { gib } => {
                review_only(self)?;
                if !self.internal_install.offers_full_os()
                    || self.internal_install.pending().is_some()
                    || self.internal_install.is_running()
                    || self.internal_install.can_restart()
                {
                    return Err(t("imager-setup-internal-error-command-unavailable"));
                }
                let gib = u64::try_from(gib)
                    .map_err(|_| t("imager-setup-internal-error-command-unavailable"))?;
                self.internal_install.set_target_gib(gib)?;
                self.status = super::strings::t_args(
                    "imager-setup-internal-status-size",
                    &[("gib", gib.into())],
                );
                self.status_is_error = false;
                self.composed.invalidate();
                return Ok(None);
            }
            Op::InstallInternal { reviewed } => {
                review_only(self)?;
                if self.internal_install.reviewed_digest()?.as_deref() != Some(reviewed.as_str()) {
                    return Err(t("imager-setup-internal-error-plan-changed"));
                }
                SetupCommand::InstallInternal
            }
            Op::RestartInternal => SetupCommand::RestartInternal,
            Op::RestartInternalLater => SetupCommand::RestartInternalLater,
            Op::RetryInternal { operation_id } => {
                current_operation(self, &operation_id)?;
                SetupCommand::RetryInternal
            }
            Op::RemoveInternal { operation_id } => {
                current_operation(self, &operation_id)?;
                SetupCommand::RemoveInternal
            }
            Op::InternalFirmwareSettings => {
                review_only(self)?;
                SetupCommand::InternalFirmwareSettings
            }
            Op::InternalDisableFastStartup => {
                review_only(self)?;
                SetupCommand::InternalDisableFastStartup
            }
            Op::RecheckInternal => SetupCommand::RecheckInternal,
            _ => return Err(t("imager-setup-internal-error-command-unavailable")),
        }))
    }

    fn internal_download_ready(&self) -> bool {
        self.internal_download_flow
            .as_ref()
            .is_some_and(InternalDownloadFlow::is_ready)
    }

    /// Probe the host off the UI thread; Full Tilo shows as checking until
    /// the result arrives.
    fn start_host_probe(&mut self) {
        if let Some(result) = &self.full_os_check {
            result.set(None);
            self.check_facts = None;
            self.support_report = None;
        }
        let wants_report = self.full_os_check.is_some() || self.reviewed() == Experience::FullOs;
        let Some(task) = self.internal_install.probe_task() else {
            return;
        };
        let tx = self.worker_tx.clone();
        if let Err(error) = (self.worker_launcher)(
            "imager-host-probe",
            Box::new(move || {
                let (facts, records) = task();
                let report = wants_report.then(probe_support_report);
                let _ = tx.send(WorkerMessage::HostProbe {
                    facts,
                    records,
                    report,
                });
            }),
        ) {
            self.internal_install.probe_failed();
            self.apply_install_capability();
            self.status = super::strings::t_args(
                "imager-setup-internal-command-failed",
                &[("detail", error.into())],
            );
            self.status_is_error = true;
        }
        self.composed.invalidate();
    }

    /// Mirror the gated readiness into the capabilities the review and the
    /// download flow check. A closed gate leaves fixture capabilities alone.
    fn apply_install_capability(&mut self) {
        if !self.internal_install.gate() {
            return;
        }
        let mut platform = self.capabilities.platform;
        platform.internal_dual_boot = self.internal_install.offers_full_os();
        self.capabilities.platform = platform;
        if let Some(flow) = &mut self.internal_download_flow {
            flow.set_capabilities(platform);
        }
    }

    fn launch_host_command(&mut self, command: HostCommand) -> Result<(), String> {
        if self.host_command_pending.is_some() {
            return Err(super::strings::t(
                "imager-setup-internal-error-command-unavailable",
            ));
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
            .ok_or_else(|| super::strings::t("imager-setup-internal-error-no-pending"))?;
        let task = self.internal_install.helper_task(action, &operation)?;
        if let Err(error) = (self.worker_launcher)("imager-staging-helper", task) {
            self.internal_install
                .helper_task_failed(action, error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn start_internal_install(&mut self) -> Result<(), String> {
        if !self.on_full_os_review() {
            return Err(super::strings::t("imager-setup-internal-error-not-review"));
        }
        let reviewed = self
            .internal_install
            .reviewed_digest()?
            .ok_or_else(|| super::strings::t("imager-setup-internal-error-no-plan"))?;
        let ready = self.internal_download_ready();
        self.internal_install.begin_stage(&reviewed, ready)?;
        let consumed = self
            .internal_download_flow
            .as_mut()
            .ok_or_else(|| super::strings::t("imager-setup-internal-error-download"))
            .and_then(InternalDownloadFlow::consume);
        if let Err(error) = consumed {
            self.internal_install.abort_stage("download", error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn apply_internal_install_command(&mut self, command: SetupCommand) {
        let t = super::strings::t;
        let (result, pending) = match command {
            SetupCommand::InstallInternal => (
                self.start_internal_install(),
                t("imager-setup-internal-status-installing"),
            ),
            SetupCommand::RestartInternal => (
                if self.internal_install.can_restart() {
                    self.launch_host_command(HostCommand::Restart)
                } else {
                    Err(t("imager-setup-internal-error-no-restart"))
                },
                t("imager-setup-internal-restarting"),
            ),
            SetupCommand::RestartInternalLater => (
                self.internal_install.defer_restart(),
                t("imager-setup-internal-restart-deferred"),
            ),
            SetupCommand::RetryInternal => (
                self.launch_helper(HelperAction::Arm),
                t("imager-setup-internal-status-retrying"),
            ),
            SetupCommand::RemoveInternal => (
                self.launch_helper(HelperAction::Remove),
                t("imager-setup-internal-status-removing"),
            ),
            SetupCommand::InternalFirmwareSettings => (
                self.internal_install
                    .firmware_command()
                    .ok_or_else(|| t("imager-setup-internal-error-remedy-unavailable"))
                    .and_then(|command| self.launch_host_command(command)),
                t("imager-setup-internal-status-firmware"),
            ),
            SetupCommand::InternalDisableFastStartup => (
                if self
                    .internal_install
                    .command_available(HostCommand::DisableFastStartup)
                {
                    self.launch_host_command(HostCommand::DisableFastStartup)
                } else {
                    Err(t("imager-setup-internal-error-remedy-unavailable"))
                },
                t("imager-setup-internal-status-fast-startup"),
            ),
            SetupCommand::RecheckInternal => (
                if self.internal_install.gate()
                    && !self.internal_install.is_probing()
                    && !self.internal_install.is_running()
                {
                    self.start_host_probe();
                    Ok(())
                } else {
                    Err(t("imager-setup-internal-error-command-unavailable"))
                },
                t("imager-setup-internal-status-checking"),
            ),
            _ => return,
        };
        self.apply_internal_command_result(result, &pending);
    }

    fn apply_host_command_result(&mut self, command: HostCommand, result: Result<(), String>) {
        self.host_command_pending = None;
        match result {
            Ok(()) => {
                self.status_is_error = false;
                if command == HostCommand::DisableFastStartup {
                    self.start_host_probe();
                    self.status = super::strings::t("imager-setup-internal-status-checking");
                } else {
                    self.status = super::strings::t("imager-setup-internal-restarting");
                }
            }
            Err(error) => {
                self.status = super::strings::t_args(
                    "imager-setup-internal-command-failed",
                    &[("detail", error.into())],
                );
                self.status_is_error = true;
            }
        }
        self.composed.invalidate();
    }

    /// Follow helper runs and stepper commits; announce finished runs.
    fn poll_internal_install(&mut self) -> bool {
        let mut changed = self.internal_install.poll();
        changed |= self.abort_stage_on_download_failure();
        if self.internal_install_revision != self.internal_install.revision() {
            self.internal_install_revision = self.internal_install.revision();
            let host = self.internal_install.host();
            let announced = match self.internal_install.run_state() {
                RunState::Succeeded {
                    action: HelperAction::Stage | HelperAction::Arm,
                } => Some((
                    super::strings::t_args(
                        "imager-setup-internal-done",
                        &[("host", install_flow::host_arg(host).into())],
                    ),
                    false,
                )),
                RunState::Succeeded {
                    action: HelperAction::Remove,
                } => Some((super::strings::t("imager-setup-internal-removed"), false)),
                RunState::Failed { failure, .. } => {
                    Some((install_flow::failure_text(failure, host), true))
                }
                _ => None,
            };
            if let Some((status, is_error)) = announced
                && self.internal_run_announced != *self.internal_install.run_state()
            {
                self.internal_run_announced = self.internal_install.run_state().clone();
                self.status = status;
                self.status_is_error = is_error;
            }
            self.composed.invalidate();
            changed = true;
        }
        if self.internal_install.take_reprobe() {
            self.start_host_probe();
            changed = true;
        }
        changed
    }

    /// End a running stage whose download failed; returns whether it did.
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
            .map(str::to_string)
        {
            self.internal_install.abort_stage("download", detail);
            true
        } else {
            false
        }
    }

    fn internal_projection(&self) -> super::actions::InternalProjection {
        let install = &self.internal_install;
        let on_review = self.on_full_os_review() && self.full_os_check.is_none();
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
        super::actions::InternalProjection {
            readiness: match install.readiness() {
                Readiness::Gated => "gated",
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
            // Without a digest, the projection cannot offer an install action.
            // The invocation path propagates the serialization error.
            reviewed: if self.full_os_check.is_some() {
                None
            } else {
                install.reviewed_digest().ok().flatten()
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
            availability: super::actions::InternalAvailability {
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
                retry: self.full_os_check.is_none()
                    && install.gate()
                    && pending.is_some()
                    && !running,
                remove: self.full_os_check.is_none()
                    && install.gate()
                    && true
                    && pending.is_some()
                    && !running,
                firmware_settings: self.on_full_os_review()
                    && idle_host
                    && install.firmware_command().is_some(),
                disable_fast_startup: self.on_full_os_review()
                    && idle_host
                    && install.command_available(HostCommand::DisableFastStartup),
                recheck: install.gate() && true && !install.is_probing() && !running,
            },
        }
    }

    fn internal_readiness_text(&self) -> String {
        let host = install_flow::host_arg(self.internal_install.host());
        match self.internal_install.readiness() {
            Readiness::Gated if self.full_os_check.is_some() => {
                super::strings::t("imager-setup-check-gated")
            }
            Readiness::Gated => super::strings::t("imager-setup-internal-gated"),
            Readiness::Checking => {
                super::strings::t_args("imager-setup-internal-checking", &[("host", host.into())])
            }
            Readiness::NoRelease => super::strings::t("imager-setup-internal-no-release"),
            Readiness::Unsupported(blockers) => super::strings::t_args(
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
            Readiness::Ready => {
                super::strings::t_args("imager-setup-internal-ready", &[("host", host.into())])
            }
        }
    }

    /// A text on the left that grows, and its control on the right.
    fn detail_row(text: Text, control: Option<Box<dyn Widget>>) -> Row {
        let mut row = Row::new().push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(text)),
                )),
        ));
        if let Some(control) = control {
            row = row.push(control);
        }
        row
    }

    fn internal_plan_card(&self) -> Section {
        let t = super::strings::t;
        let t_args = super::strings::t_args;
        let install = &self.internal_install;
        let host = install_flow::host_arg(install.host());
        let pending_plan = install.pending().map(|record| &record.plan);
        let plan = pending_plan.map(Ok).or_else(|| {
            install
                .plan()
                .map(|plan| plan.as_ref().map_err(Clone::clone))
        });
        let summary = match plan {
            None => t("imager-setup-internal-plan-checking"),
            Some(Err(super::staging::plan::PlanError::InsufficientSpace {
                needed_bytes,
                available_bytes,
            })) => t_args(
                "imager-setup-internal-plan-error-space",
                &[
                    ("needed", ui::format::memory(needed_bytes).into()),
                    ("available", ui::format::memory(available_bytes).into()),
                ],
            ),
            Some(Err(super::staging::plan::PlanError::Unsupported(detail))) => t_args(
                "imager-setup-internal-plan-error-other",
                &[("detail", detail.into())],
            ),
            Some(Ok(plan)) => {
                let space = plan.shrink.as_ref().map_or_else(
                    || t("imager-setup-internal-plan-free"),
                    |shrink| {
                        t_args(
                            "imager-setup-internal-plan-shrink",
                            &[
                                ("host", host.into()),
                                (
                                    "shrink",
                                    ui::format::memory(
                                        shrink.from_bytes.saturating_sub(shrink.to_bytes),
                                    )
                                    .into(),
                                ),
                                ("from", ui::format::memory(shrink.from_bytes).into()),
                                ("to", ui::format::memory(shrink.to_bytes).into()),
                            ],
                        )
                    },
                );
                [
                    space,
                    t_args(
                        "imager-setup-internal-plan-target",
                        &[(
                            "target",
                            ui::format::memory(install_flow::target_extent_bytes(plan)).into(),
                        )],
                    ),
                    t_args(
                        "imager-setup-internal-plan-staging",
                        &[(
                            "staging",
                            ui::format::memory(install_flow::staging_bytes(plan)).into(),
                        )],
                    ),
                ]
                .join("\n")
            }
        };
        let mut content = stack().push(Box::new(
            Text::new(&summary)
                .with_id("setup.internal.plan")
                .with_live(Politeness::Polite),
        ));
        if pending_plan.is_none()
            && let Some((min, max)) = install.bounds_gib()
        {
            let editable = !install.is_running() && !install.can_restart();
            content = content
                .push(Box::new(
                    Text::new(&t("imager-setup-internal-size-label")).with_role(TextRole::Label),
                ))
                .push(Box::new(
                    NumberInput::from_state(install.target_state())
                        .with_range(min as f64, max as f64)
                        .with_step(1.0)
                        .with_suffix("GiB")
                        .with_id("setup.internal.target-size")
                        .with_accessible_name(t("imager-setup-internal-size-label"))
                        .with_enabled(editable),
                ))
                .push(Box::new(
                    Text::new(&t_args(
                        "imager-setup-internal-size-help",
                        &[
                            ("min", min.into()),
                            ("max", max.into()),
                            ("host", host.into()),
                        ],
                    ))
                    .dimmed(),
                ));
        }
        content = content.push(Box::new(Text::new(&t_args(
            "imager-setup-internal-expect",
            &[("host", host.into())],
        ))));
        if install_flow::BUILD_GATE != Some("1") {
            content = content.push(Box::new(
                Text::new(&t("imager-setup-internal-unverified"))
                    .with_id("setup.internal.unverified")
                    .dimmed(),
            ));
        }
        Section::titled(
            t("imager-setup-internal-plan-title"),
            vec![Box::new(content)],
        )
    }

    fn internal_blockers_card(&self, extra: Option<&str>) -> Section {
        let t = super::strings::t;
        let install = &self.internal_install;
        let idle_host = self.host_command_pending.is_none();
        let mut content = stack();
        if install.blockers().is_empty() {
            content = content.push(Box::new(
                Text::new(&t("imager-setup-internal-blockers-none"))
                    .with_id("setup.internal.blockers"),
            ));
        }
        for blocker in install.blockers() {
            let control: Option<Box<dyn Widget>> = match install.remedy(*blocker) {
                Some(HostCommand::FirmwareSettings { .. }) => Some(Box::new(
                    self.button(
                        "setup.internal.firmware-settings",
                        &t("imager-setup-internal-remedy-firmware"),
                        SetupCommand::InternalFirmwareSettings,
                    )
                    .with_enabled(idle_host),
                )),
                Some(HostCommand::DisableFastStartup) => Some(Box::new(
                    self.button(
                        "setup.internal.disable-fast-startup",
                        &t("imager-setup-internal-remedy-fast-startup"),
                        SetupCommand::InternalDisableFastStartup,
                    )
                    .with_enabled(idle_host),
                )),
                _ => None,
            };
            content = content.push(Box::new(Self::detail_row(
                Text::new(&install_flow::blocker_text(*blocker, install.host()))
                    .with_id(format!(
                        "setup.internal.blocker.{}",
                        install_flow::blocker_key(*blocker)
                    ))
                    .with_live(Politeness::Polite),
                control,
            )));
        }
        if let Some(extra) = extra {
            content = content.push(Box::new(Text::new(extra)));
        }
        content = content.push(Box::new(
            self.button(
                "setup.internal.recheck",
                &t("imager-setup-internal-recheck"),
                SetupCommand::RecheckInternal,
            )
            .with_enabled(!install.is_probing() && !install.is_running()),
        ));
        Section::titled(
            t("imager-setup-internal-blockers-title"),
            vec![Box::new(content)],
        )
    }

    fn internal_resume_card(&self) -> Option<Section> {
        let t = super::strings::t;
        let install = &self.internal_install;
        install.pending()?;
        let enabled = !install.is_running();
        Some(Section::titled(
            t("imager-setup-internal-resume-title"),
            vec![Box::new(
                stack()
                    .push(Box::new(
                        Text::new(&t("imager-setup-internal-resume"))
                            .with_id("setup.internal.resume"),
                    ))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(
                                self.button(
                                    "setup.internal.retry",
                                    &t("imager-setup-internal-resume-retry"),
                                    SetupCommand::RetryInternal,
                                )
                                .with_enabled(enabled),
                            ))
                            .push(Box::new(
                                self.button(
                                    "setup.internal.remove",
                                    &t("imager-setup-internal-resume-remove"),
                                    SetupCommand::RemoveInternal,
                                )
                                .with_enabled(enabled),
                            )),
                    )),
            )],
        ))
    }

    fn internal_run_card(&self) -> Option<Section> {
        let t = super::strings::t;
        let install = &self.internal_install;
        let host = install.host();
        let (title, detail, politeness) = match install.run_state() {
            RunState::Idle => return None,
            RunState::Running { progress, .. } => (
                t("imager-setup-internal-run-title"),
                progress.as_ref().map_or_else(
                    || t("imager-setup-internal-run-waiting"),
                    |progress| {
                        let step = t(&format!(
                            "imager-setup-internal-step-{}",
                            install_flow::step_key(progress.step)
                        ));
                        if progress.total_bytes == 0 {
                            super::strings::t_args(
                                "imager-setup-internal-run-step",
                                &[("step", step.into())],
                            )
                        } else {
                            super::strings::t_args(
                                "imager-setup-internal-run-progress",
                                &[
                                    ("step", step.into()),
                                    ("done", ui::format::memory(progress.done_bytes).into()),
                                    ("total", ui::format::memory(progress.total_bytes).into()),
                                ],
                            )
                        }
                    },
                ),
                Politeness::Polite,
            ),
            RunState::Succeeded {
                action: HelperAction::Remove,
            } => (
                t("imager-setup-internal-run-title"),
                t("imager-setup-internal-removed"),
                Politeness::Polite,
            ),
            RunState::Succeeded { .. } => {
                let mut detail = super::strings::t_args(
                    "imager-setup-internal-done",
                    &[("host", install_flow::host_arg(host).into())],
                );
                if install.restart_deferred() {
                    detail.push('\n');
                    detail.push_str(&t("imager-setup-internal-restart-deferred"));
                }
                (
                    t("imager-setup-internal-done-title"),
                    detail,
                    Politeness::Polite,
                )
            }
            RunState::Failed { failure, .. } => (
                t("imager-setup-internal-run-title"),
                install_flow::failure_text(failure, host),
                Politeness::Assertive,
            ),
        };
        Some(Section::titled(
            &title,
            vec![Box::new(
                Text::new(&detail)
                    .with_id("setup.internal.run")
                    .with_live(politeness),
            )],
        ))
    }

    /// Review navigation for an offered Full Tilo: Install, or Restart now /
    /// Restart later once the installer is set to start.
    fn internal_nav(&self, mut nav: Row) -> Row {
        let t = super::strings::t;
        let install = &self.internal_install;
        if install.can_restart() {
            if !install.restart_deferred() {
                nav = nav.push(Box::new(self.button(
                    "setup.internal.restart-later",
                    &t("imager-setup-internal-restart-later"),
                    SetupCommand::RestartInternalLater,
                )));
            }
            return nav.push(Box::new(
                self.button(
                    "setup.internal.restart",
                    &t("imager-setup-internal-restart-now"),
                    SetupCommand::RestartInternal,
                )
                .role(ButtonRole::Primary)
                .with_enabled(self.host_command_pending.is_none()),
            ));
        }
        let label = if install.is_running() {
            t("imager-setup-internal-installing")
        } else if install.pending().is_some() {
            t("imager-setup-internal-continue")
        } else {
            t("imager-setup-internal-install")
        };
        nav.push(Box::new(
            self.button(
                "setup.internal.install",
                &label,
                SetupCommand::InstallInternal,
            )
            .role(ButtonRole::Primary)
            .with_enabled(
                install
                    .install_blocker(self.internal_download_ready())
                    .is_ok()
                    && !self
                        .support_report
                        .as_ref()
                        .is_some_and(SupportReport::blocks_install),
            ),
        ))
    }
}

const fn download_phase_key(phase: super::internal_cache::coordinator::Phase) -> &'static str {
    use super::internal_cache::coordinator::Phase;
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

fn probe_support_report() -> SupportReport {
    #[cfg(all(test, target_os = "linux"))]
    if let Some(report) = super::journey::main_story_tests::fixture_support_report() {
        return report;
    }
    imager_backend::support_report::SupportReport::probe()
}

/// The component report supplied for another computer. Important limitations
/// are expanded; working components stay compact.
pub(crate) fn another_computer_report_sections(report: &SupportReport) -> Vec<Box<dyn Widget>> {
    support_report::sections(report, support_report::Placement::AnotherComputer)
}

fn launch_worker(name: &str, task: WorkerTask) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(task)
        .map(drop)
        .map_err(|error| error.to_string())
}

pub(super) use imager_backend::setup::native_host::system_folder_reason;

fn fixture_desktop_release() -> DesktopRelease {
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
        account_custody: None,
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

fn fixture_desktop_selection(already_installed: bool) -> DesktopSelection {
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

impl Widget for SetupApp {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.poll_background();
        self.drain_commands();
        self.composed.sync(state, path);
        self.publish();
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.poll_background() || self.drain_commands();
        if let Some(composed) = self.composed.element_mut() {
            changed |= composed.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "SetupApp"
    }

    fn needs_idle_sync(&self) -> bool {
        self.desktop_flow.has_pending_operation()
            || {
                {
                    self.guided_desktop
                        .as_ref()
                        .is_some_and(|p| p.pending.is_some())
                }
            }
            || {
                {
                    self.internal_download_flow
                        .as_ref()
                        .is_some_and(InternalDownloadFlow::has_pending_operation)
                        || self.internal_install.is_probing()
                        || self.internal_install.is_running()
                        || self.host_command_pending.is_some()
                }
            }
    }
}

impl SetupApp {
    fn effect(&mut self, effect: SetupEffect, _cx: &mut EffectCx<Imager>) -> EffectAnswer {
        self.answer(effect).map(Effected::value)
    }
}

/// `app` as a view of the imager resource: a live window starts the
/// setup it was made to start.
pub(crate) fn setup_window(cx: &ViewCx<Imager>, mut app: SetupApp) -> ui::widget::Element {
    app.actions = Some(cx.actions());
    if cx.mode().is_live() && app.starts_when_live {
        app.start_live();
    }
    crate::window_view::window_view(cx, app, SetupApp::effect)
}

impl PublishingWindow for SetupApp {
    fn published(&self) -> &Rc<RefCell<Published>> {
        &self.published
    }

    fn statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = super::actions::statuses(&self.setup_projection());
        statuses.push(super::actions::desktop_preparation_status(
            self.guided_delivery_retry_available(),
        ));
        statuses
    }

    fn rows(&self) -> Vec<LiveRow> {
        let mut rows = vec![super::actions::row(&self.setup_projection())];
        if self.reviewed() == Experience::Desktop {
            rows.push(super::actions::desktop_preparation_row(
                self.guided_delivery_retry_available(),
            ));
        }
        rows
    }
}

// View fixtures depend on the Unix installer and cache worker test adapters.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::path::{Path, PathBuf};

    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use ui::a11y::Action;
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Effect;

    use ui::headless::{FrameHarness, SemanticExpectation};

    use super::*;
    use crate::setup::actions::SETUP_ROW_ID;
    use crate::setup::internal_bundle;
    use crate::setup::internal_cache::coordinator::{Phase as InternalPhase, Worker};
    use crate::setup::internal_download::tests::{Server, response};
    use crate::setup::model::{Capabilities, Experience, HostOs};
    use ui::time::Duration;

    include!("desktop_journey_tests.rs");

    #[test]
    fn migration_intent_cannot_be_attached_outside_full_os_review() {
        let mut app = SetupApp::new();
        let before = app.setup_projection().value();
        assert!(
            app.answer(SetupEffect::UseMigrationIntent {
                directory: "/must-not-be-opened".into()
            })
            .is_err()
        );
        assert_eq!(app.setup_projection().value(), before);
    }

    #[test]
    fn guided_navigation_follows_actual_download_and_staging_and_refuses_legacy_back() {
        let mut fixture = gated_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
        let app = &mut fixture.app;
        open_full_review(app);
        app.refresh_guided_navigation();
        let guard = app.guided_navigation_guard();
        assert!(guard.get());
        assert!(retained_name(app.build_composition().as_ref(), "setup.back").is_none());
        invoke_internal(app, "start-internal-download").unwrap();
        assert!(!guard.get());
        wait_for_app(app, "download ready", SetupApp::internal_download_ready);
        assert!(guard.get());
        invoke_internal(app, "install-internal").unwrap();
        assert!(!guard.get());
        wait_for_app(app, "staged", |app| {
            matches!(
                app.internal_install.run_state(),
                RunState::Succeeded {
                    action: HelperAction::Stage
                }
            )
        });
        assert!(!guard.get());
    }

    /// Put `app` on the review of `experience`, as the guided journey hands
    /// a reviewed route to setup.
    fn review(app: &mut SetupApp, experience: Experience) {
        match experience {
            Experience::Desktop | Experience::FullOs => app.experience = Some(experience),
        }
        app.refresh_guided_navigation();
    }

    fn supported_fixture() -> SetupApp {
        SetupApp::with_capabilities(SetupCapabilities {
            platform: Capabilities {
                host: HostOs::Linux,
                desktop_session: true,
                internal_dual_boot: true,
            },
            connected_folders: true,
        })
    }

    #[test]
    fn unsupported_host_keeps_internal_state_but_refuses_installation_actions() {
        let mut app = SetupApp::with_capabilities(SetupCapabilities {
            platform: Capabilities {
                host: HostOs::Other,
                desktop_session: false,
                internal_dual_boot: false,
            },
            connected_folders: false,
        });
        let projection = app.setup_projection();
        assert_eq!(projection.internal, app.internal_projection());
        assert!(!app.internal_install.offers_full_os());
        assert!(app.answer(SetupEffect::StartInternalDownload).is_err());
        assert!(
            app.answer(SetupEffect::InstallInternal {
                reviewed: String::new(),
            })
            .is_err()
        );
    }

    fn retained_widget_disabled(root: &dyn Widget, id: &str) -> bool {
        fn visit(widget: &dyn Widget, id: &str, matches: &mut Vec<bool>) {
            if widget.test_id() == Some(id) {
                let node = widget
                    .accessibility()
                    .unwrap_or_else(|| panic!("{id} has no accessibility node"));
                matches.push(node.state.disabled);
            }
            for child in widget.children() {
                visit(child, id, matches);
            }
        }

        let mut matches = Vec::new();
        visit(root, id, &mut matches);
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one retained {id} widget"
        );
        matches[0]
    }

    /// Full OS review download surface and command routing.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn full_os_download_is_distinct_and_reaches_only_the_real_worker() {
        // Phase `full_os_review_has_a_distinct_download_surface_and_no_install_action`.
        // Catches routing Full Tilo through the Desktop installer or implying
        // installation is ready while the product gate is closed.
        {
            let mut app = supported_fixture();
            review(&mut app, Experience::FullOs);
            let composition = app.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.install"),
                0
            );
            assert!(
                !internal_action(&app, "install-internal")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(invoke_internal(&mut app, "install-internal").is_err());
            assert_eq!(app.internal_install.run_state(), &RunState::Idle);
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (960, 720));

            harness
                .expect(
                    &SemanticExpectation::named("setup.internal.status")
                        .name_contains("Full Tilo download"),
                )
                .unwrap();
            harness
                .expect(
                    &SemanticExpectation::named("setup.internal.install-unavailable")
                        .name_contains("Install Tilo")
                        .enabled(false),
                )
                .unwrap();
        }

        // Phase `full_os_download_command_routes_to_the_real_internal_worker_only`.
        // Catches a Full Tilo button dispatching the Desktop installer instead of the internal worker.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let preference = tempfile::tempdir().unwrap();
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut app = supported_fixture();
            app.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
                app.capabilities.platform,
                preference.path().join("selection.json"),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            ));
            review(&mut app, Experience::FullOs);

            app.apply_command(SetupCommand::StartInternalDownload);
            assert!(matches!(
                app.desktop_flow.state(),
                DesktopFlowState::Ready(_)
            ));
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if app
                    .internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.presentation().phase == Some(InternalPhase::Ready))
                {
                    break;
                }
                assert!(ui::time::now() < deadline, "internal download timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(app.status, "Download ready");
            assert!(!app.needs_idle_sync());
            let mut ready = FrameHarness::new(
                ui::State::new(),
                Box::new(app.internal_download_card()),
                (640, 320),
            );
            ready
                .expect(
                    &SemanticExpectation::named("setup.internal.release")
                        .name_contains("Keep for later")
                        .enabled(true),
                )
                .unwrap();

            app.apply_command(SetupCommand::ReleaseInternalDownload);
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if app
                    .internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.presentation().phase == Some(InternalPhase::Released))
                {
                    break;
                }
                assert!(ui::time::now() < deadline, "internal release timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(app.status, "Kept for later");
        }

        // Phase `full_os_download_command_is_refused_outside_full_os_review`.
        // Catches a forged or stale command starting a Full Tilo download away from its review.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let preference = tempfile::tempdir().unwrap();
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut app = supported_fixture();
            app.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
                app.capabilities.platform,
                preference.path().join("selection.json"),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            ));

            app.apply_command(SetupCommand::StartInternalDownload);

            assert!(app.status_is_error);
            assert_eq!(
                app.status,
                "Full Tilo downloads can start only from the Full Tilo review."
            );
            assert!(
                app.internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.presentation().phase.is_none())
            );
            assert!(server.requests().is_empty());
        }

        // Phase `duplicate_full_os_start_command_is_rejected_without_an_extra_request`.
        // Catches two routed Start commands replacing the active internal download before recompose.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
            let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
            let (continue_tx, continue_rx) = mpsc::sync_channel(0);
            let mut request = 0;
            let server = Server::on_request(
                vec![
                    response(200, "", &bytes),
                    response(200, "", &vec![b'E'; 4096]),
                    response(200, "", &vec![b'R'; 8192]),
                    response(200, "", &bytes),
                    response(200, "", &vec![b'E'; 4096]),
                    response(200, "", &vec![b'R'; 8192]),
                ],
                move || {
                    request += 1;
                    if request == 2 {
                        blocked_tx.send(()).unwrap();
                        continue_rx.recv().unwrap();
                    }
                },
            );
            let root = tempfile::tempdir().unwrap();
            let preference = tempfile::tempdir().unwrap();
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut app = supported_fixture();
            app.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
                app.capabilities.platform,
                preference.path().join("selection.json"),
                worker,
                vec![
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                ],
                "x86_64",
            ));
            review(&mut app, Experience::FullOs);

            app.apply_command(SetupCommand::StartInternalDownload);
            tilo_test_env::recv("the download to block", &blocked_rx);
            let requests = server.requests().len();
            app.apply_command(SetupCommand::StartInternalDownload);
            let duplicate_status = app.status.clone();
            let duplicate_is_error = app.status_is_error;
            let requests_after = server.requests().len();
            continue_tx.send(()).unwrap();

            assert!(duplicate_is_error);
            assert_eq!(
                duplicate_status,
                "A Full Tilo download cannot start in its current state."
            );
            assert_eq!(requests_after, requests);
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.poll_background();
                if app
                    .internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.presentation().phase == Some(InternalPhase::Ready))
                {
                    break;
                }
                assert!(ui::time::now() < deadline, "internal download timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(server.requests().len(), 3);
        }
    }

    fn disconnected_desktop_probe() -> Result<super::super::desktop_flow::ProbeReceiver, String> {
        let (_tx, rx) = mpsc::channel();
        Ok(rx)
    }

    fn disconnected_desktop_download(
        _: DesktopRelease,
        _: Arc<AtomicBool>,
    ) -> Result<super::super::desktop_flow::DownloadReceiver, String> {
        let (_tx, rx) = mpsc::channel();
        Ok(rx)
    }

    fn disconnected_desktop_install(
        _: super::super::desktop_install::DesktopInstallRequest,
    ) -> Result<super::super::desktop_flow::InstallReceiver, String> {
        let (_tx, rx) = mpsc::channel();
        Ok(rx)
    }

    static RECOVERY_PROBES: AtomicUsize = AtomicUsize::new(0);

    fn recoverable_desktop_probe() -> Result<super::super::desktop_flow::ProbeReceiver, String> {
        let (tx, rx) = mpsc::channel();
        if RECOVERY_PROBES.fetch_add(1, Ordering::SeqCst) > 0 {
            tx.send(Ok(super::super::desktop_flow::DesktopProbeSnapshot {
                host: super::super::desktop_probe::DesktopHost {
                    operating_system: "linux".into(),
                    os_release: Ok(super::super::desktop_probe::OsRelease {
                        id: "fedora".into(),
                        version_id: Some("44".into()),
                        name: Some("Fedora Linux".into()),
                    }),
                    architecture: Ok("aarch64".into()),
                    package_manager: Ok(super::super::desktop_probe::PackageManager::Dnf),
                    immutable: Ok(false),
                    sandboxed: Ok(false),
                    session: Ok(super::super::desktop_probe::SessionSupport {
                        display_manager: "gdm.service".into(),
                        other_wayland_sessions: vec!["gnome.desktop".into()],
                    }),
                    installed_package: Ok(super::super::desktop_probe::InstalledPackage::Absent),
                },
                releases: vec![fixture_desktop_release()],
            }))
            .unwrap();
        }
        drop(tx);
        Ok(rx)
    }

    /// Worker loss, desktop review, installed guidance, retry and cross-experience status.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_review_shows_exact_status_and_recovers_from_failures() {
        {
            RECOVERY_PROBES.store(0, Ordering::SeqCst);
            let mut app = SetupApp::with_capabilities(SetupCapabilities {
                platform: Capabilities {
                    host: HostOs::Linux,
                    desktop_session: false,
                    internal_dual_boot: false,
                },
                connected_folders: false,
            });
            review(&mut app, Experience::Desktop);
            app.desktop_flow =
                DesktopFlow::without_start(super::super::desktop_flow::DesktopRuntime {
                    launch_probe: recoverable_desktop_probe,
                    launch_download: disconnected_desktop_download,
                    launch_install: disconnected_desktop_install,
                });
            app.desktop_flow.start_check().unwrap();
            assert!(app.poll_background());
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));

            harness
                .expect(&SemanticExpectation::named("setup.retry-desktop").enabled(true))
                .unwrap();
            harness
                .perform_named("setup.retry-desktop", &Action::Activate)
                .unwrap();
            assert_eq!(RECOVERY_PROBES.load(Ordering::SeqCst), 2);
        }

        {
            let mut app = supported_fixture();
            review(&mut app, Experience::Desktop);
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));

            harness
                .expect(
                    &SemanticExpectation::named("setup.desktop.status")
                        .name_contains("0.1.9-1.fc44"),
                )
                .unwrap();
            harness
                .expect(
                    &SemanticExpectation::named("setup.desktop.status")
                        .name_contains("Fedora Linux 44 (ARM64)"),
                )
                .unwrap();
            harness
                .expect(&SemanticExpectation::named("setup.install").enabled(true))
                .unwrap();
        }

        {
            let mut app = supported_fixture();
            review(&mut app, Experience::Desktop);
            let selection = fixture_desktop_selection(false);
            app.desktop_flow = DesktopFlow::with_runtime_and_state(
                super::super::desktop_flow::DesktopRuntime {
                    launch_probe: disconnected_desktop_probe,
                    launch_download: disconnected_desktop_download,
                    launch_install: disconnected_desktop_install,
                },
                DesktopFlowState::Failed {
                    selection: Some(selection),
                    kind: DesktopFailureKind::NativeTransaction,
                    message: "package manager failed".into(),
                },
            );

            app.apply_command(SetupCommand::RetryDesktop);

            assert!(matches!(
                app.desktop_flow.state(),
                DesktopFlowState::Checking
            ));
        }

        {
            let mut app = supported_fixture();
            review(&mut app, Experience::FullOs);
            assert_eq!(app.experience, Some(Experience::FullOs));
            app.internal_download_revision =
                app.internal_download_flow.as_ref().unwrap().revision();
            app.desktop_flow = DesktopFlow::from_fixture_state(DesktopFlowState::Installed {
                selection: fixture_desktop_selection(false),
                result: super::super::desktop_install::InstalledDesktop {
                    version: "0.1.9-1.fc44".into(),
                    session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
                },
            });
            app.desktop_revision = u64::MAX;
            let expected = app.status.clone();

            assert!(app.poll_background());
            assert_eq!(app.status, expected);
            let harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));
            assert!(
                harness
                    .a11y_node_named("Tilo Desktop is installed")
                    .is_none()
            );
        }
    }

    thread_local! {
        static NEXT_SETUP: std::cell::RefCell<Option<SetupApp>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The imager showing the setup [`launch`] hands it.
    struct SetupFlow;

    impl ResourceApp for SetupFlow {
        type Resource = Imager;
        const ID: &'static str = "so.tilo.imager";
        const TITLE: &'static str = "Disk Imager";

        fn view(cx: &mut ui::framework::ViewCx<Imager>) -> ui::widget::Element {
            let setup = NEXT_SETUP
                .with(|next| next.borrow_mut().take())
                .expect("a setup to mount");
            setup_window(cx, setup)
        }
    }

    /// `setup` in a live window of the imager.
    fn launch(setup: SetupApp) -> TestApp<SetupFlow> {
        NEXT_SETUP.with(|next| *next.borrow_mut() = Some(setup));
        TestApp::<SetupFlow>::new().unwrap()
    }

    fn find_row(
        resources: &[tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find_row(&resource.children, id)
            }
        })
    }

    /// Setup as the imager resource publishes it: the live row, carrying
    /// the setup effects as callers see them now.
    fn setup_resource(app: &SetupApp) -> tilo_actions::ResourceSpec {
        let mut root = app.rows().remove(0).into_resource();
        let blocked = super::super::actions::blocked(&app.setup_projection());
        root.actions = SetupEffect::specs()
            .into_iter()
            .map(
                |spec| match blocked.iter().find(|blocked| blocked.id == spec.id) {
                    Some(blocked) => spec.unavailable(blocked.code, blocked.reason.clone()),
                    None => spec,
                },
            )
            .collect();
        root
    }

    fn setup_args(
        entries: &[(&str, serde_json::Value)],
    ) -> serde_json::Map<String, serde_json::Value> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect()
    }

    fn invoke_setup_raw(
        app: &mut SetupApp,
        effect: SetupEffect,
    ) -> Result<serde_json::Value, String> {
        app.answer(effect)
    }

    fn child_resource<'a>(
        resource: &'a tilo_actions::ResourceSpec,
        kind: &str,
    ) -> &'a tilo_actions::ResourceSpec {
        resource
            .children
            .iter()
            .find(|child| child.kind == kind)
            .unwrap_or_else(|| panic!("missing child resource kind {kind}"))
    }

    fn invoke_encoded_setup(
        app: &mut SetupApp,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<serde_json::Value, String> {
        assert_eq!(encoded.type_id, SetupEffect::OPERATION);
        app.answer(SetupEffect::from_payload(&encoded.payload).map_err(|error| error.to_string())?)
    }

    fn retained_widget_count(root: &dyn Widget, id: &str) -> usize {
        usize::from(root.test_id() == Some(id))
            + root
                .children()
                .into_iter()
                .map(|child| retained_widget_count(child, id))
                .sum::<usize>()
    }

    fn retained_name(root: &dyn Widget, id: &str) -> Option<String> {
        ui::widget::find_test_id(root, id)?.accessibility()?.name
    }

    fn internal_action(app: &SetupApp, id: &str) -> tilo_actions::ActionSpec {
        child_resource(&setup_resource(app), "imager.setup.internal")
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("missing internal action {id}"))
            .clone()
    }

    fn invoke_internal(app: &mut SetupApp, id: &str) -> Result<serde_json::Value, String> {
        let encoded = internal_action(app, id)
            .build_json(&serde_json::Map::new())
            .map_err(|error| error.to_string())?;
        invoke_encoded_setup(app, &encoded)
    }

    #[test]
    fn native_full_os_review_keeps_probe_refusal_and_verified_consumption() {
        let app = SetupApp::for_full_os();
        assert!(app.on_full_os_review());
        assert!(!app.capabilities.platform.internal_dual_boot);
        assert!(
            !internal_action(&app, "install-internal")
                .contract
                .availability
                .is_available()
        );

        let mut blocked = gated_app(
            install_flow::tests::Fakes::new(vec![super::super::staging::probe::Blocker::NotGpt]),
            HostOs::Windows,
        );
        blocked.app.review_full_os();
        blocked.app.start_host_probe();
        blocked.app.poll_background();
        assert!(invoke_internal(&mut blocked.app, "install-internal").is_err());
        assert!(blocked.fakes.launches.lock().unwrap().is_empty());

        let mut fixture = gated_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Windows);
        let app = &mut fixture.app;
        app.review_full_os();
        assert!(invoke_internal(app, "install-internal").is_err());
        app.start_host_probe();
        app.poll_background();
        assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 1);
        assert!(invoke_internal(app, "install-internal").is_err());
        invoke_internal(app, "start-internal-download").unwrap();
        wait_for_app(
            app,
            "verified native download",
            SetupApp::internal_download_ready,
        );
        let guard = app.guided_navigation_guard();
        assert!(guard.get());
        let mut window = launch(fixture.app);
        assert!(
            window
                .effect(&SetupEffect::InstallInternal {
                    reviewed: "stale-review".into(),
                })
                .is_err()
        );
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
        let root = find_row(&window.harness().resources(), SETUP_ROW_ID).unwrap();
        let internal = child_resource(&root, "imager.setup.internal");
        window
            .call_on(
                internal.id.as_str(),
                "install-internal",
                serde_json::json!({}),
            )
            .unwrap();
        assert!(!guard.get());
        let deadline = tilo_test_env::hang_deadline();
        loop {
            window.settle();
            if window
                .with_widget::<SetupApp, _>(|app| !app.internal_install.is_running())
                .unwrap()
            {
                break;
            }
            assert!(ui::time::now() < deadline, "native staging must complete");
        }
        window
            .with_widget::<SetupApp, _>(|app| {
                assert!(matches!(
                    app.internal_install.run_state(),
                    RunState::Succeeded {
                        action: HelperAction::Stage
                    }
                ))
            })
            .unwrap();
        let launches = fixture.fakes.launches.lock().unwrap();
        assert_eq!(launches.len(), 1);
        assert!(launches[0].prepared_setup.is_none());
        assert!(launches[0].migration.is_none());
        assert!(launches[0].profile.is_empty());
        assert_eq!(launches[0].plan_sha256, launches[0].plan.digest().unwrap());
        assert_eq!(fixture.fakes.manifests.lock().unwrap()[0], fixture.bytes);
        assert!(!guard.get());
    }

    #[test]
    fn full_os_check_refuses_mutations_even_with_forged_public_actions() {
        let mut fixture = gated_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
        let result = LocalState::new(None);
        let app = &mut fixture.app;
        app.review_full_os();
        app.full_os_check = Some(result.clone());
        app.start_host_probe();
        app.poll_background();
        assert!(app.check_ready());
        for effect in [
            SetupEffect::StartInternalDownload,
            SetupEffect::ResumeInternalDownload,
            SetupEffect::ReleaseInternalDownload,
            SetupEffect::DiscardInternalDownload,
            SetupEffect::InstallInternal {
                reviewed: app.internal_install.reviewed_digest().unwrap().unwrap(),
            },
            SetupEffect::RetryInternal {
                operation_id: "forged-operation".into(),
            },
            SetupEffect::RemoveInternal {
                operation_id: "forged-operation".into(),
            },
            SetupEffect::RestartInternal,
            SetupEffect::SetInternalTargetSize { gib: 120 },
            SetupEffect::InstallDesktop,
        ] {
            assert!(invoke_setup_raw(app, effect).is_err());
        }
        // Invoke the actual resource contracts too, rather than relying on
        // buttons being hidden or action availability metadata being honored.
        for id in [
            "start-internal-download",
            "install-internal",
            "remove-internal",
        ] {
            assert!(invoke_internal(app, id).is_err(), "{id}");
        }
        assert!(!app.internal_projection().availability.start_download);
        assert!(!app.internal_projection().availability.install);
        assert!(!app.internal_projection().availability.remove);
        assert!(!app.internal_download_ready());
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
        invoke_setup_raw(app, SetupEffect::ContinueSetup).unwrap();
        assert!(result.get().is_some());
        assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 1);
        for command in [
            SetupCommand::StartInternalDownload,
            SetupCommand::InstallInternal,
            SetupCommand::RemoveInternal,
            SetupCommand::RetryInternal,
        ] {
            app.apply_command(command);
            assert!(app.status_is_error);
        }
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());

        let mut pending = install_flow::tests::Fakes::new(Vec::new());
        pending
            .records
            .push(install_flow::tests::record("actual-operation"));
        let mut fixture = gated_app(pending, HostOs::Linux);
        fixture.app.review_full_os();
        fixture.app.full_os_check = Some(LocalState::new(None));
        fixture.app.start_host_probe();
        fixture.app.poll_background();
        assert!(fixture.app.internal_install.pending().is_some());
        assert!(
            fixture
                .app
                .answer(SetupEffect::RemoveInternal {
                    operation_id: "actual-operation".into(),
                })
                .is_err()
        );
        assert!(
            fixture
                .app
                .answer(SetupEffect::RetryInternal {
                    operation_id: "actual-operation".into(),
                })
                .is_err()
        );
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
    }

    #[test]
    fn full_os_check_rechecks_fresh_facts_and_does_not_reuse_continue_result() {
        let fakes = install_flow::tests::Fakes::new(Vec::new());
        let facts = Arc::new(std::sync::Mutex::new(fakes.facts.clone()));
        let mut fixture = gated_app(fakes, HostOs::Linux);
        let mut runtime = fixture.fakes.runtime();
        let current = Arc::clone(&facts);
        runtime.probe = Arc::new(move || current.lock().unwrap().clone());
        fixture.app.internal_install = InternalInstall::new(runtime, true, HostOs::Linux);
        let result = LocalState::new(None);
        let app = &mut fixture.app;
        app.review_full_os();
        app.full_os_check = Some(result.clone());
        app.start_host_probe();
        app.poll_background();
        invoke_setup_raw(app, SetupEffect::ContinueSetup).unwrap();
        assert!(result.get().is_some());
        facts
            .lock()
            .unwrap()
            .blockers
            .push(super::super::staging::probe::Blocker::NotGpt);
        invoke_setup_raw(app, SetupEffect::RecheckInternal).unwrap();
        assert!(result.get().is_none());
        assert!(invoke_setup_raw(app, SetupEffect::ContinueSetup).is_err());
        app.poll_background();
        assert!(!app.check_ready());
        assert!(invoke_setup_raw(app, SetupEffect::ContinueSetup).is_err());
        assert!(
            app.internal_readiness_text()
                .contains(&install_flow::blocker_text(
                    super::super::staging::probe::Blocker::NotGpt,
                    HostOs::Linux
                ))
        );
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
    }

    #[test]
    fn full_os_check_closed_gate_is_build_limitation_and_keeps_back_available() {
        let result = LocalState::new(None);
        let mut app = SetupApp::for_full_os_check(result);
        app.internal_install = InternalInstall::gated(HostOs::Linux);
        assert!(!app.check_ready());
        assert!(app.guided_navigation_guard().get());
        assert_eq!(
            app.internal_readiness_text(),
            super::super::strings::t("imager-setup-check-gated")
        );
        assert!(!app.internal_projection().availability.install);
    }

    fn run_inline(_: &str, task: WorkerTask) -> Result<(), String> {
        task();
        Ok(())
    }

    struct GatedFixture {
        app: SetupApp,
        fakes: install_flow::tests::Fakes,
        bytes: Vec<u8>,
        _server: Server,
        _root: tempfile::TempDir,
        _preference: tempfile::TempDir,
    }

    /// Setup with the product gate open, a fake host and helper, and a real
    /// fixture download worker whose consumer is the staging consumer.
    fn gated_app(fakes: install_flow::tests::Fakes, host: HostOs) -> GatedFixture {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let root = tempfile::tempdir().unwrap();
        let preference = tempfile::tempdir().unwrap();
        let capabilities = SetupCapabilities {
            platform: Capabilities {
                host,
                desktop_session: true,
                internal_dual_boot: false,
            },
            connected_folders: false,
        };
        let install = InternalInstall::new(fakes.runtime(), true, host);
        let worker = Worker::fixture_for_test(
            root.path(),
            server.transport(),
            install_flow::staging_consumer(install.link()),
        );
        let mut app = SetupApp::with_parts(capabilities, run_inline, install);
        app.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
            capabilities.platform,
            preference.path().join("selection.json"),
            worker,
            vec![
                vec![server.release(&bytes)],
                vec![server.release(&bytes)],
                vec![server.release(&bytes)],
            ],
            "x86_64",
        ));
        GatedFixture {
            app,
            fakes,
            bytes,
            _server: server,
            _root: root,
            _preference: preference,
        }
    }

    fn open_full_review(app: &mut SetupApp) {
        app.start_host_probe();
        app.poll_background();
        review(app, Experience::FullOs);
        assert!(app.on_full_os_review(), "{}", app.status);
    }

    fn wait_for_app(app: &mut SetupApp, what: &str, done: impl Fn(&SetupApp) -> bool) {
        tilo_test_env::wait_until(what, || {
            app.poll_background();
            done(app)
        });
    }

    /// Probe gate, staging, stale digests, blockers, resume cards and shot fixtures.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn full_os_install_stages_only_the_current_reviewed_plan() {
        // Phase `full_os_is_checking_until_the_probe_passes_with_the_gate_open`.
        // Catches Full Tilo being offered before the host probe reports, and the
        // gate, probe and catalog not all being required.
        {
            let mut fixture = gated_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
            let app = &mut fixture.app;
            assert!(!app.capabilities.platform.supports(Experience::FullOs));
            assert!(
                app.internal_readiness_text()
                    .starts_with("Checking whether Tilo can be installed")
            );
            app.start_host_probe();
            assert!(app.poll_background());
            assert!(app.capabilities.platform.supports(Experience::FullOs));
            assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 1);

            let mut blocked = gated_app(
                install_flow::tests::Fakes::new(vec![
                    super::super::staging::probe::Blocker::NotGpt,
                ]),
                HostOs::Linux,
            );
            blocked.app.start_host_probe();
            blocked.app.poll_background();
            assert!(
                !blocked
                    .app
                    .capabilities
                    .platform
                    .supports(Experience::FullOs)
            );
            assert!(blocked.app.internal_readiness_text().contains("MBR"));
        }

        // Phase `gated_full_os_install_stages_the_reviewed_plan_and_offers_restart`.
        // Catches Install enabling before the verified download is held, the
        // helper receiving anything but the reviewed plan and verified paths, or
        // a success that does not offer the restart.
        {
            let mut fixture = gated_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
            let app = &mut fixture.app;
            open_full_review(app);
            let composition = app.build_composition();
            assert!(retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.install"
            ));
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.install-unavailable"),
                0
            );
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.target-size"),
                1
            );
            assert!(
                !internal_action(app, "install-internal")
                    .contract
                    .availability
                    .is_available()
            );

            invoke_internal(app, "start-internal-download").unwrap();
            wait_for_app(app, "download ready", SetupApp::internal_download_ready);
            let composition = app.build_composition();
            assert!(!retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.install"
            ));
            let reviewed_plan = app.internal_install.plan().unwrap().clone().unwrap();
            let projection = invoke_internal(app, "install-internal").unwrap();
            assert_eq!(projection["internal"]["run"], "running");
            wait_for_app(app, "staging outcome", |app| {
                !app.internal_install.is_running()
            });
            assert_eq!(
                app.internal_install.run_state(),
                &RunState::Succeeded {
                    action: HelperAction::Stage
                }
            );
            {
                let launches = fixture.fakes.launches.lock().unwrap();
                assert_eq!(launches.len(), 1);
                assert_eq!(launches[0].action, HelperAction::Stage);
                // Review ran on provisional installer sizes; the stage request
                // carries the planner's layout for the verified bundle's sizes,
                // with the same Tilo size and no further shrink than reviewed.
                let staged = &launches[0].plan;
                assert_eq!(launches[0].plan_sha256, staged.digest().unwrap());
                assert_eq!(
                    staged.target_extent.lbas(),
                    reviewed_plan.target_extent.lbas()
                );
                let kept = |p: &super::super::staging::plan::Plan| {
                    p.shrink.as_ref().map_or(u64::MAX, |x| x.to_bytes)
                };
                assert!(kept(staged) >= kept(&reviewed_plan));
                assert_eq!(
                    launches[0].component_paths.staging_root,
                    "/verified/staging-root.img"
                );
                assert_eq!(fixture.fakes.manifests.lock().unwrap()[0], fixture.bytes);
            }
            wait_for_app(app, "download consumed", |app| {
                app.internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.presentation().phase == Some(InternalPhase::Consumed))
            });
            let composition = app.build_composition();
            assert!(!retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.restart"
            ));
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.restart-later"),
                1
            );
            assert!(
                retained_name(composition.as_ref(), "setup.internal.run")
                    .unwrap()
                    .contains("stays the startup default")
            );
            assert!(invoke_internal(app, "install-internal").is_err());

            invoke_internal(app, "restart-internal-later").unwrap();
            let composition = app.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.restart-later"),
                0
            );
            invoke_internal(app, "restart-internal").unwrap();
            app.poll_background();
            assert_eq!(
                *fixture.fakes.commands.lock().unwrap(),
                vec![HostCommand::Restart]
            );
        }

        // Phase `install_action_refuses_a_plan_digest_from_before_a_size_change`.
        // Catches an install invoked with a digest read before the plan changed.
        {
            let mut fixture = gated_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
            let app = &mut fixture.app;
            open_full_review(app);
            invoke_internal(app, "start-internal-download").unwrap();
            wait_for_app(app, "download ready", SetupApp::internal_download_ready);
            let stale = internal_action(app, "install-internal")
                .build_json(&serde_json::Map::new())
                .unwrap();
            let size = internal_action(app, "set-internal-target-size")
                .build_json(&setup_args(&[("gib", serde_json::json!(100))]))
                .unwrap();
            invoke_encoded_setup(app, &size).unwrap();
            assert_eq!(app.internal_install.target_bytes(), 100 << 30);
            let out_of_range = internal_action(app, "set-internal-target-size")
                .build_json(&setup_args(&[("gib", serde_json::json!(10))]))
                .unwrap();
            assert!(invoke_encoded_setup(app, &out_of_range).is_err());
            assert_eq!(app.internal_install.target_bytes(), 100 << 30);

            let error = invoke_encoded_setup(app, &stale).unwrap_err();
            assert!(error.contains("plan changed"), "{error}");
            assert!(fixture.fakes.launches.lock().unwrap().is_empty());
            assert!(app.internal_download_ready());
            invoke_internal(app, "install-internal").unwrap();
            assert!(app.internal_install.is_running());
        }

        // Phase `blockers_show_remedies_and_keep_install_disabled`.
        // Catches remedies offered for blockers that do not have them, Install
        // enabling while a blocker remains, or a remedy action running the wrong
        // host command.
        {
            use super::super::staging::probe::Blocker;
            let mut fixture = gated_app(
                install_flow::tests::Fakes::new(vec![Blocker::SecureBootOn, Blocker::Hibernation]),
                HostOs::Windows,
            );
            let app = &mut fixture.app;
            open_full_review(app);
            invoke_internal(app, "start-internal-download").unwrap();
            wait_for_app(app, "download ready", SetupApp::internal_download_ready);
            let composition = app.build_composition();
            assert!(retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.install"
            ));
            assert!(!retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.firmware-settings"
            ));
            assert!(!retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.disable-fast-startup"
            ));
            assert!(invoke_internal(app, "install-internal").is_err());
            invoke_internal(app, "internal-disable-fast-startup").unwrap();
            app.poll_background();
            invoke_internal(app, "internal-firmware-settings").unwrap();
            app.poll_background();
            assert_eq!(
                *fixture.fakes.commands.lock().unwrap(),
                vec![
                    HostCommand::DisableFastStartup,
                    HostCommand::FirmwareSettings {
                        suspend_bitlocker: false
                    }
                ]
            );
            assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 2);
            assert!(fixture.fakes.launches.lock().unwrap().is_empty());

            let mut battery = gated_app(
                install_flow::tests::Fakes::new(vec![Blocker::OnBattery]),
                HostOs::Linux,
            );
            open_full_review(&mut battery.app);
            let composition = battery.app.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.blocker.on-battery"),
                1
            );
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.firmware-settings"),
                0
            );
            assert!(
                !internal_action(&battery.app, "internal-firmware-settings")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(invoke_internal(&mut battery.app, "internal-firmware-settings").is_err());
            assert!(battery.fakes.commands.lock().unwrap().is_empty());
        }

        // Phase `unfinished_record_shows_a_resume_card_and_acts_on_that_record_only`.
        // Catches the resume card acting on a record other than the one shown,
        // and a removal that leaves stale records on screen.
        {
            use super::super::staging::probe::Blocker;
            let mut fakes = install_flow::tests::Fakes::new(vec![Blocker::PendingOperation]);
            fakes.records = vec![install_flow::tests::record("op-1")];
            let mut fixture = gated_app(fakes, HostOs::Linux);
            let app = &mut fixture.app;
            app.start_host_probe();
            app.poll_background();
            review(app, Experience::FullOs);
            let composition = app.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.resume"),
                1
            );
            let stale = SetupEffect::RemoveInternal {
                operation_id: "op-0".into(),
            };
            assert!(
                invoke_setup_raw(app, stale)
                    .unwrap_err()
                    .contains("no longer the current")
            );
            assert!(fixture.fakes.launches.lock().unwrap().is_empty());

            invoke_internal(app, "retry-internal").unwrap();
            app.poll_background();
            assert_eq!(
                app.internal_install.run_state(),
                &RunState::Succeeded {
                    action: HelperAction::Arm
                }
            );
            invoke_internal(app, "remove-internal").unwrap();
            app.poll_background();
            let launches = fixture.fakes.launches.lock().unwrap();
            assert_eq!(
                launches
                    .iter()
                    .map(|request| (request.action, request.operation_id.as_str()))
                    .collect::<Vec<_>>(),
                vec![(HelperAction::Arm, "op-1"), (HelperAction::Remove, "op-1")]
            );
            assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 2);
        }
    }
}
