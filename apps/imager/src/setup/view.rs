//! The setup window: the pages from choosing Full Tilo or Desktop to the install
//! that follows.

mod support_report;

use std::collections::HashMap;
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

use super::actions::{HardwareProjection, SETUP_ROW_ID, SetupEffect};
use super::desktop_catalog::{DesktopRelease, PackageFormat, RuntimeEvidence};
use super::desktop_flow::{DesktopFailureKind, DesktopFlow, DesktopFlowState, DesktopSelection};
use super::internal_download_flow::InternalDownloadFlow;
pub(crate) use super::internal_install_flow::ReviewedFullOs;
use super::internal_install_flow::{
    self as install_flow, HostCommand, InternalInstall, Readiness, RunState,
};
use super::model::{Capabilities, Experience, HostOs};
use super::staging::helper::Action as HelperAction;
use crate::page::{self, Frame, stack};
use crate::resource::CATALOG;

use crate::hosting::{EffectAnswer, EffectCx, Effected, Hosted, Port};
use imager_backend::support_report::SupportReport;
use tilo_resource::{EffectStatus, LiveRow};
use ui::LocalState;

/// Display facts only; final installation still reprobes through the native helper.
#[derive(Clone)]
pub(crate) struct FullOsCheck {
    pub facts: super::staging::probe::HostFacts,
    pub plan: super::staging::plan::Plan,
}

type WorkerTask = Box<dyn FnOnce() + Send + 'static>;
type WorkerLauncher = fn(&str, WorkerTask) -> Result<(), String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupCapabilities {
    pub platform: Capabilities,
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent worker, presentation, and invalidation flags do not form one state machine"
)]
pub struct SetupApp {
    capabilities: SetupCapabilities,
    experience: Option<Experience>,
    worker_tx: Sender<WorkerMessage>,
    worker_rx: Receiver<WorkerMessage>,
    worker_launcher: WorkerLauncher,
    status: String,
    status_is_error: bool,
    composed: ui::widget::Composed,
    starts_when_live: bool,
    port: Port<SetupApp>,
    guided_navigation: LocalState<bool>,
    desktop_only: bool,
    full_os_check: Option<LocalState<Option<FullOsCheck>>>,
    check_facts: Option<super::staging::probe::HostFacts>,
    support_report: Option<SupportReport>,
    desktop_flow: DesktopFlow,
    desktop_revision: u64,
    internal_download_flow: Option<InternalDownloadFlow>,
    internal_download_error: Option<String>,
    internal_download_revision: u64,
    internal_install: InternalInstall,
    internal_install_revision: u64,
    host_command_pending: Option<HostCommand>,
    internal_run_announced: RunState,

    dirty: bool,
}

impl Default for SetupApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupApp {
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

    pub(crate) fn guided_navigation_guard(&self) -> LocalState<bool> {
        self.guided_navigation.clone()
    }

    fn refresh_guided_navigation(&self) {
        if self.full_os_check.is_some() {
            self.guided_navigation
                .set(self.host_command_pending.is_none());
            return;
        }
        let allowed = !self.desktop_flow.has_pending_operation()
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
        };
        let mut app = Self::with_parts(
            capabilities,
            launch_worker,
            InternalInstall::new(
                install_flow::StagingRuntime::live(),
                install_flow::live_install_allowed(),
                host,
            ),
        );
        app.starts_when_live = true;
        app
    }

    #[must_use]
    pub fn for_full_os() -> Self {
        let mut app = Self::new();
        app.review_full_os();
        app
    }

    pub(crate) fn for_full_os_check(result: LocalState<Option<FullOsCheck>>) -> Self {
        let mut app = Self::for_full_os();
        app.full_os_check = Some(result);
        app.refresh_guided_navigation();
        app
    }

    fn check_ready(&self) -> bool {
        self.full_os_check.is_some()
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

    #[must_use]
    pub fn for_desktop() -> Self {
        let mut app = Self::new();
        app.desktop_only = true;
        app.experience = Some(Experience::Desktop);
        app.refresh_guided_navigation();
        app.status.clear();
        app
    }

    pub fn start_live(&mut self) {
        self.starts_when_live = false;
        self.desktop_flow = DesktopFlow::new_live();
        if !self.desktop_only {
            self.start_host_probe();
        }
        self.refresh_guided_navigation();
    }

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
            InternalInstall::guarded(capabilities.platform.host),
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
                Err(error) => (None, Some(error.to_owned())),
            };
        Self {
            dirty: true,
            capabilities,
            experience: None,
            worker_tx,
            worker_rx,
            worker_launcher,
            status: super::strings::t("imager-setup-status-choose-experience"),
            status_is_error: false,
            composed: ui::widget::Composed::default(),
            starts_when_live: false,
            port: Port::detached(),
            guided_navigation: LocalState::new(false),
            desktop_only: false,
            full_os_check: None,
            check_facts: None,
            support_report: None,
            desktop_flow,
            desktop_revision: 0,
            internal_download_flow,
            internal_download_error,
            internal_download_revision: 0,
            internal_install,
            internal_install_revision: 0,
            host_command_pending: None,
            internal_run_announced: RunState::Idle,
        }
    }

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

    fn start_desktop_install(&mut self) {
        match self.desktop_flow.start_install() {
            Ok(()) => {
                self.status = super::strings::t("imager-setup-status-rechecking-before-download");
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
                self.status = super::strings::t("imager-setup-status-review-last");
                self.status_is_error = true;
            }
            SetupCommand::InstallDesktop => self.start_desktop_install(),
            SetupCommand::CancelDesktopDownload => {
                if self.desktop_flow.cancel_download() {
                    self.status = super::strings::t("imager-setup-status-cancelling-package");
                }
            }
            SetupCommand::RetryDesktop => match self.desktop_flow.retry() {
                Ok(()) => {
                    self.status = super::strings::t("imager-setup-status-rechecking-before-retry");
                }
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
                        .ok_or_else(|| {
                            crate::setup::strings::t("imager-setup-error-download-unavailable")
                        })
                        .and_then(|flow| {
                            let key = flow.offered_selection().ok_or(super::strings::t(
                                "imager-setup-error-download-not-offered",
                            ))?;
                            flow.start_selected(key)
                        })
                } else {
                    Err(super::strings::t("imager-setup-error-download-review-only"))
                };
                let pending = super::strings::t("imager-setup-status-downloading");
                self.apply_internal_command_result(result, &pending);
            }
            SetupCommand::PauseInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| {
                        crate::setup::strings::t("imager-setup-error-download-unavailable")
                    })
                    .and_then(InternalDownloadFlow::pause);
                let pending = super::strings::t("imager-setup-status-pausing-download");
                self.apply_internal_command_result(result, &pending);
            }
            SetupCommand::ResumeInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| super::strings::t("imager-setup-error-no-saved-download"))
                    .and_then(InternalDownloadFlow::resume);
                let pending = super::strings::t("imager-setup-status-downloading");
                self.apply_internal_command_result(result, &pending);
            }
            SetupCommand::CancelInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| {
                        crate::setup::strings::t("imager-setup-error-download-unavailable")
                    })
                    .and_then(InternalDownloadFlow::cancel);
                let pending = super::strings::t("imager-setup-status-cancelling-download");
                self.apply_internal_command_result(result, &pending);
            }
            SetupCommand::DiscardInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| super::strings::t("imager-setup-error-no-unfinished-download"))
                    .and_then(InternalDownloadFlow::discard);
                let pending = super::strings::t("imager-setup-status-discarding-download");
                self.apply_internal_command_result(result, &pending);
            }
            SetupCommand::ReleaseInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| super::strings::t("imager-setup-error-no-verified-download"))
                    .and_then(InternalDownloadFlow::release);
                let pending = super::strings::t("imager-setup-status-keeping-download");
                self.apply_internal_command_result(result, &pending);
            }
            SetupCommand::InternalInstallUnavailable => {
                self.status = super::strings::t("imager-setup-status-staging-unavailable");
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
                        self.status = super::strings::t("imager-desktop-installed-sign-out");
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
                    self.status = super::strings::t(flow.presentation().title_key);
                    self.status_is_error = matches!(
                        flow.presentation().phase,
                        Some(super::internal_cache::coordinator::Phase::Failed)
                    );
                }
                self.composed.invalidate();
                changed = true;
            }
        }
        changed |= self.poll_internal_install();
        self.refresh_guided_navigation();
        changed
    }

    /// The published effect a press of `command`'s button asks: the same one an agent or a
    /// script invokes, so a press is checked and answered as theirs is. A command with none
    /// (the install that cannot run here) is a button that is never enabled.
    fn press_effect(&self, command: SetupCommand) -> Option<SetupEffect> {
        let operation_id = || {
            self.internal_install
                .pending()
                .map(|record| record.operation_id.clone())
                .unwrap_or_default()
        };
        Some(match command {
            SetupCommand::Next => SetupEffect::ContinueSetup,
            SetupCommand::InstallDesktop => SetupEffect::InstallDesktop,
            SetupCommand::CancelDesktopDownload => SetupEffect::CancelDesktopDownload,
            SetupCommand::RetryDesktop => SetupEffect::RecheckDesktop,
            SetupCommand::StartInternalDownload => SetupEffect::StartInternalDownload,
            SetupCommand::PauseInternalDownload => SetupEffect::PauseInternalDownload,
            SetupCommand::ResumeInternalDownload => SetupEffect::ResumeInternalDownload,
            SetupCommand::CancelInternalDownload => SetupEffect::CancelInternalDownload,
            SetupCommand::DiscardInternalDownload => SetupEffect::DiscardInternalDownload,
            SetupCommand::ReleaseInternalDownload => SetupEffect::ReleaseInternalDownload,
            SetupCommand::InstallInternal => SetupEffect::InstallInternal {
                reviewed: self
                    .internal_install
                    .reviewed_digest()
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            },
            SetupCommand::RestartInternal => SetupEffect::RestartInternal,
            SetupCommand::RestartInternalLater => SetupEffect::RestartInternalLater,
            SetupCommand::RetryInternal => SetupEffect::RetryInternal {
                operation_id: operation_id(),
            },
            SetupCommand::RemoveInternal => SetupEffect::RemoveInternal {
                operation_id: operation_id(),
            },
            SetupCommand::InternalFirmwareSettings => SetupEffect::InternalFirmwareSettings,
            SetupCommand::InternalDisableFastStartup => SetupEffect::InternalDisableFastStartup,
            SetupCommand::RecheckInternal => SetupEffect::RecheckInternal,
            SetupCommand::InternalInstallUnavailable => return None,
        })
    }

    fn button(&self, id: &str, key: &str, command: SetupCommand) -> Button {
        let button =
            Button::new(id, &crate::setup::strings::t(key)).control_size(ControlSize::Regular);
        match self.press_effect(command) {
            Some(effect) => button.on_click(self.port.presses().on(effect)),
            None => button.with_enabled(false),
        }
    }

    fn desktop_install_card(&self) -> Section {
        let detail = match self.desktop_flow.state() {
            DesktopFlowState::Checking => CATALOG.t("imager-desktop-checking"),
            DesktopFlowState::Unavailable { host, .. } => CATALOG.t_args(
                "imager-desktop-unavailable",
                &[("host", ui::i18n::ArgValue::Text(host.clone()))],
            ),
            DesktopFlowState::Ready(selection) => {
                let version = (
                    "version",
                    ui::i18n::ArgValue::Text(selection.release.version.clone()),
                );
                let host = ("host", ui::i18n::ArgValue::Text(selection.host.clone()));
                CATALOG.t_args(
                    if selection.already_installed {
                        "imager-desktop-already-installed"
                    } else {
                        "imager-desktop-ready"
                    },
                    &[version, host],
                )
            }
            DesktopFlowState::Downloading(selection) => CATALOG.t_args(
                "imager-desktop-downloading",
                &[
                    (
                        "version",
                        ui::i18n::ArgValue::Text(selection.release.version.clone()),
                    ),
                    ("host", ui::i18n::ArgValue::Text(selection.host.clone())),
                ],
            ),
            DesktopFlowState::Authorizing(selection) => CATALOG.t_args(
                "imager-desktop-authorizing",
                &[
                    (
                        "version",
                        ui::i18n::ArgValue::Text(selection.release.version.clone()),
                    ),
                    ("host", ui::i18n::ArgValue::Text(selection.host.clone())),
                ],
            ),
            DesktopFlowState::Preparing(selection) => CATALOG.t_args(
                "imager-desktop-preparing",
                &[
                    (
                        "version",
                        ui::i18n::ArgValue::Text(selection.release.version.clone()),
                    ),
                    ("host", ui::i18n::ArgValue::Text(selection.host.clone())),
                ],
            ),
            DesktopFlowState::Installing(selection) => CATALOG.t_args(
                "imager-desktop-installing",
                &[
                    (
                        "version",
                        ui::i18n::ArgValue::Text(selection.release.version.clone()),
                    ),
                    ("host", ui::i18n::ArgValue::Text(selection.host.clone())),
                ],
            ),
            DesktopFlowState::Verifying(selection) => CATALOG.t_args(
                "imager-desktop-verifying",
                &[("host", ui::i18n::ArgValue::Text(selection.host.clone()))],
            ),
            DesktopFlowState::Installed { selection, result } => CATALOG.t_args(
                "imager-desktop-installed",
                &[
                    ("version", ui::i18n::ArgValue::Text(result.version.clone())),
                    ("host", ui::i18n::ArgValue::Text(selection.host.clone())),
                ],
            ),
            DesktopFlowState::Failed { kind, message, .. } => {
                let what = CATALOG.t(match kind {
                    DesktopFailureKind::DownloadCancelled => "imager-desktop-download-cancelled",
                    DesktopFailureKind::AuthorizationCancelled => "imager-desktop-not-approved",
                    DesktopFailureKind::AuthorizationRefused => "imager-desktop-needs-admin",
                    DesktopFailureKind::WorkerLost => "imager-desktop-stopped-early",
                    DesktopFailureKind::Readiness => "imager-desktop-not-ready",
                    DesktopFailureKind::Download => "imager-desktop-download-failed",
                    DesktopFailureKind::NativeTransaction => "imager-desktop-install-failed",
                });
                if self.desktop_only {
                    CATALOG.t_args(
                        "imager-desktop-failed",
                        &[
                            ("what", ui::i18n::ArgValue::Text(what)),
                            ("detail", ui::i18n::ArgValue::Text(message.clone())),
                        ],
                    )
                } else {
                    CATALOG.t_args(
                        "imager-desktop-failed-retry",
                        &[
                            ("what", ui::i18n::ArgValue::Text(what)),
                            ("detail", ui::i18n::ArgValue::Text(message.clone())),
                        ],
                    )
                }
            }
        };
        let retained = if self.desktop_only {
            super::strings::t("installer-desktop-keep-existing")
        } else {
            CATALOG.t(match self.capabilities.platform.host {
                HostOs::Windows => "imager-retained-windows",
                HostOs::Linux => "imager-retained-linux",
                HostOs::Other => "imager-retained-other",
            })
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
        .with_header(SectionHeader::heading(CATALOG.t("imager-section-desktop")))
    }

    fn internal_download_card(&self) -> Section {
        let mut content = page::related();
        if let Some(flow) = &self.internal_download_flow {
            let presentation = flow.presentation();
            let summary = if presentation.phase.is_none()
                && let Some(error) = flow.catalog_error()
            {
                super::strings::t(error)
            } else {
                super::strings::t(presentation.sentence_key)
            };
            content = content.push(Box::new(
                Text::new(&summary)
                    .with_id("setup.internal.status")
                    .with_live(Politeness::Polite),
            ));
            let mut actions = Row::new();
            let mut any = false;
            for (shown, id, key, command) in [
                (
                    flow.can_start(),
                    "setup.internal.start",
                    "imager-setup-action-start-download",
                    SetupCommand::StartInternalDownload,
                ),
                (
                    flow.can_pause(),
                    "setup.internal.pause",
                    "imager-setup-action-pause-download",
                    SetupCommand::PauseInternalDownload,
                ),
                (
                    flow.can_cancel(),
                    "setup.internal.cancel",
                    "imager-setup-action-cancel-download",
                    SetupCommand::CancelInternalDownload,
                ),
                (
                    flow.can_resume(),
                    "setup.internal.resume",
                    "imager-setup-action-resume-download",
                    SetupCommand::ResumeInternalDownload,
                ),
                (
                    flow.can_release(),
                    "setup.internal.release",
                    "imager-setup-action-release-download",
                    SetupCommand::ReleaseInternalDownload,
                ),
                (
                    flow.can_discard(),
                    "setup.internal.discard",
                    "imager-setup-action-discard-download",
                    SetupCommand::DiscardInternalDownload,
                ),
            ] {
                if shown {
                    actions = actions.push(Box::new(self.button(id, key, command)));
                    any = true;
                }
            }
            if any {
                content = content.push(Box::new(actions));
            }
            return Section::new(vec![Box::new(content)]).with_header(SectionHeader::heading(
                super::strings::t(presentation.title_key),
            ));
        }
        let detail = self.internal_download_error.as_deref().map_or_else(
            || super::strings::t("imager-desktop-cannot-download"),
            super::strings::t,
        );
        content = content.push(Box::new(
            Text::new(&detail)
                .with_id("setup.internal.status")
                .with_live(Politeness::Assertive),
        ));
        Section::new(vec![Box::new(content)]).with_header(SectionHeader::heading(
            CATALOG.t("imager-section-full-download"),
        ))
    }

    fn review_body(&self) -> Column {
        if self.full_os_check.is_some() {
            let readiness = if self
                .support_report
                .as_ref()
                .is_some_and(SupportReport::blocks_install)
            {
                CATALOG.t("imager-desktop-unsupported-component")
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
                        "installer-preview-continue",
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
                .with_header(SectionHeader::heading(
                    CATALOG.t("imager-section-before-installing"),
                )),
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
            let primary = if experience == Experience::FullOs {
                Some((
                    "setup.internal.install-unavailable",
                    "imager-setup-internal-install",
                    SetupCommand::InternalInstallUnavailable,
                    false,
                ))
            } else {
                match self.desktop_flow.state() {
                    DesktopFlowState::Ready(_) => Some((
                        "setup.install",
                        "imager-setup-desktop-install",
                        SetupCommand::InstallDesktop,
                        true,
                    )),
                    DesktopFlowState::Downloading(_) => Some((
                        "setup.cancel-download",
                        "imager-setup-desktop-cancel",
                        SetupCommand::CancelDesktopDownload,
                        true,
                    )),
                    DesktopFlowState::Failed { .. } | DesktopFlowState::Unavailable { .. } => {
                        Some((
                            "setup.retry-desktop",
                            "imager-setup-desktop-recheck",
                            SetupCommand::RetryDesktop,
                            true,
                        ))
                    }
                    DesktopFlowState::Installed { .. } => None,
                    _ => Some((
                        "setup.install",
                        "imager-setup-internal-installing",
                        SetupCommand::InstallDesktop,
                        false,
                    )),
                }
            };
            if let Some((id, key, command, enabled)) = primary {
                nav = nav.push(Box::new(
                    self.button(id, key, command)
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

impl SetupApp {
    fn on_full_os_review(&self) -> bool {
        self.on_review_for(Experience::FullOs)
    }

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

    fn start_host_probe(&mut self) {
        if let Some(result) = &self.full_os_check {
            result.set(None);
            self.check_facts = None;
            self.support_report = None;
        }
        let wants_report = self.full_os_check.is_some() || self.reviewed() == Experience::FullOs;
        let support_report = std::sync::Arc::clone(&self.internal_install.runtime().support_report);
        let Some(task) = self.internal_install.probe_task() else {
            return;
        };
        let tx = self.worker_tx.clone();
        if let Err(error) = (self.worker_launcher)(
            "imager-host-probe",
            Box::new(move || {
                let (facts, records) = task();
                let report = wants_report.then(|| support_report());
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

    fn apply_install_capability(&mut self) {
        if !self.internal_install.check() {
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
                if self.internal_install.check()
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
                    && install.check()
                    && pending.is_some()
                    && !running,
                remove: self.full_os_check.is_none()
                    && install.check()
                    && true
                    && pending.is_some()
                    && !running,
                firmware_settings: self.on_full_os_review()
                    && idle_host
                    && install.firmware_command().is_some(),
                disable_fast_startup: self.on_full_os_review()
                    && idle_host
                    && install.command_available(HostCommand::DisableFastStartup),
                recheck: install.check() && true && !install.is_probing() && !running,
            },
        }
    }

    fn internal_readiness_text(&self) -> String {
        let host = install_flow::host_arg(self.internal_install.host());
        match self.internal_install.readiness() {
            Readiness::NotEnabled if self.full_os_check.is_some() => {
                super::strings::t("imager-setup-check-not-enabled")
            }
            Readiness::NotEnabled => super::strings::t("imager-setup-internal-not-enabled"),
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
        if install_flow::BUILD_ALLOW != Some("1") {
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
                        "imager-setup-internal-remedy-firmware",
                        SetupCommand::InternalFirmwareSettings,
                    )
                    .with_enabled(idle_host),
                )),
                Some(HostCommand::DisableFastStartup) => Some(Box::new(
                    self.button(
                        "setup.internal.disable-fast-startup",
                        "imager-setup-internal-remedy-fast-startup",
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
                "imager-setup-internal-recheck",
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
                                    "imager-setup-internal-resume-retry",
                                    SetupCommand::RetryInternal,
                                )
                                .with_enabled(enabled),
                            ))
                            .push(Box::new(
                                self.button(
                                    "setup.internal.remove",
                                    "imager-setup-internal-resume-remove",
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

    fn internal_nav(&self, mut nav: Row) -> Row {
        let install = &self.internal_install;
        if install.can_restart() {
            if !install.restart_deferred() {
                nav = nav.push(Box::new(self.button(
                    "setup.internal.restart-later",
                    "imager-setup-internal-restart-later",
                    SetupCommand::RestartInternalLater,
                )));
            }
            return nav.push(Box::new(
                self.button(
                    "setup.internal.restart",
                    "imager-setup-internal-restart-now",
                    SetupCommand::RestartInternal,
                )
                .role(ButtonRole::Primary)
                .with_enabled(self.host_command_pending.is_none()),
            ));
        }
        let key = if install.is_running() {
            "imager-setup-internal-installing"
        } else if install.pending().is_some() {
            "imager-setup-internal-continue"
        } else {
            "imager-setup-internal-install"
        };
        nav.push(Box::new(
            self.button("setup.internal.install", key, SetupCommand::InstallInternal)
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
        self.dirty = true;
        self.poll_background();
        self.composed.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.poll_background();
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

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "SetupApp"
    }

    fn needs_idle_sync(&self) -> bool {
        self.desktop_flow.has_pending_operation() || {
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

/// `app` as a window the guided setup shows on `port`: started when the setup is live.
pub(crate) fn setup_window(
    mut app: SetupApp,
    port: Port<SetupApp>,
    live: bool,
) -> ui::widget::Element {
    app.port = port;
    if live && app.starts_when_live {
        app.start_live();
    }
    Box::new(Hosted(app))
}

impl crate::hosting::Window for SetupApp {
    type Effect = SetupEffect;

    fn port(&self) -> &Port<Self> {
        &self.port
    }

    fn effect(&mut self, effect: SetupEffect, _cx: &mut EffectCx<'_, Self>) -> EffectAnswer {
        self.answer(effect).map(Effected::value)
    }

    fn statuses(&self) -> Vec<EffectStatus> {
        super::actions::statuses(&self.setup_projection())
    }

    fn rows(&self) -> Vec<LiveRow> {
        vec![super::actions::row(&self.setup_projection())]
    }

    fn row_statuses(&self) -> Vec<(String, Vec<EffectStatus>)> {
        let setup = self.setup_projection();
        let mut by_row = vec![
            (
                format!("{SETUP_ROW_ID}.internal"),
                super::actions::internal_row_status(&setup.internal),
            ),
            (
                "imager.setup.desktop".to_owned(),
                super::desktop_journey::row_status(&setup.desktop),
            ),
        ];
        by_row.retain(|(_, statuses)| !statuses.is_empty());
        by_row
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use tilo_resource::Effect;
    use ui::a11y::Action;

    use ui::headless::{Driver, SemanticExpectation};

    use super::*;
    use crate::hosting::testing::TestHost;
    use crate::setup::actions::SETUP_ROW_ID;
    use crate::setup::internal_bundle;
    use crate::setup::internal_cache::coordinator::{Phase as InternalPhase, Worker};
    use crate::setup::internal_download::tests::{Server, response};
    use crate::setup::model::{Capabilities, Experience, HostOs};
    use ui::time::Duration;

    include!("desktop_journey_tests.rs");
    include!("hosted_journey_tests.rs");
    include!("setup_buttons_language_tests.rs");

    tilo_test_env::scenario! {
        #[cfg(target_os = "linux")]
        fn a_desktop_only_install_checks_its_packages_first;

        phase desktop_only_records_intent_without_inventing_support;
        {
            let app = SetupApp::for_desktop();
            assert_eq!(app.experience, Some(Experience::Desktop));
            assert!(!app.capabilities.platform.desktop_session);
            assert!(!app.internal_install.is_probing());
            assert!(!app.setup_projection().desktop.actions.install);
        }

        phase desktop_only_install_checks_packages_and_blocks_navigation;
        {
            let mut app = SetupApp::for_desktop();
            app.desktop_flow = DesktopFlow::with_runtime_and_state(
                crate::setup::desktop_flow::DesktopRuntime {
                    launch_probe: || {
                        let (_sender, receiver) = mpsc::channel();
                        Ok(receiver)
                    },
                    launch_download: |_, _| Err("unexpected download".into()),
                    launch_install: |_| Err("unexpected native install".into()),
                },
                DesktopFlowState::Ready(fixture_desktop_selection(false)),
            );
            assert!(!app.capabilities.platform.desktop_session);
            assert!(app.setup_projection().desktop.actions.install);
            app.answer(SetupEffect::InstallDesktop).unwrap();
            assert!(app.desktop_flow.has_pending_operation());
            assert!(matches!(
                app.desktop_flow.state(),
                DesktopFlowState::Checking
            ));
            assert!(!app.guided_navigation.get());
            assert!(app.answer(SetupEffect::InstallDesktop).is_err());
            assert!(!app.internal_install.is_probing());
        }
    }

    tilo_test_env::scenario! {
        fn guided_setup_refuses_what_the_review_did_not_admit;

        phase guided_navigation_follows_actual_download_and_staging_and_refuses_legacy_back;
        {
            let mut fixture = guarded_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
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

        phase unsupported_host_keeps_internal_state_but_refuses_installation_actions;
        {
            let mut app = SetupApp::with_capabilities(SetupCapabilities {
                platform: Capabilities {
                    host: HostOs::Other,
                    desktop_session: false,
                    internal_dual_boot: false,
                },
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
    }

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
        })
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn full_os_download_is_distinct_and_reaches_only_the_real_worker() {
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
            let mut harness = Driver::new(
                ui::State::new(),
                Box::new(crate::hosting::Hosted(app)),
                (960, 720),
            );

            harness
                .expect(
                    &SemanticExpectation::named("setup.internal.status")
                        .name_contains("No Tilo download is offered right now."),
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
            let mut ready = Driver::new(
                ui::State::new(),
                Box::new(app.internal_download_card()),
                (640, 320),
            );
            ready
                .expect(
                    &SemanticExpectation::named("setup.internal.release")
                        .name_contains("Keep download for later")
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
            assert_eq!(app.status, "Start the download from the Full Tilo review.");
            assert!(
                app.internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.presentation().phase.is_none())
            );
            assert!(server.requests().is_empty());
        }

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
                crate::setup::strings::t("imager-setup-error-download-start-unavailable")
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
            let mut harness = Driver::new(
                ui::State::new(),
                Box::new(crate::hosting::Hosted(app)),
                (640, 640),
            );

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
            let mut harness = Driver::new(
                ui::State::new(),
                Box::new(crate::hosting::Hosted(app)),
                (640, 640),
            );

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
            let harness = Driver::new(
                ui::State::new(),
                Box::new(crate::hosting::Hosted(app)),
                (640, 640),
            );
            assert!(
                harness
                    .a11y_node_named("Tilo Desktop is installed")
                    .is_none()
            );
        }
    }

    fn launch(setup: SetupApp) -> TestHost {
        TestHost::hosting(|store, wake| {
            setup_window(setup, Port::new(store.clone(), wake.clone()), true)
        })
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

    fn setup_resource(app: &SetupApp) -> tilo_actions::ResourceSpec {
        let mut root = crate::hosting::Window::rows(app).remove(0).into_resource();
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
        let setup = app.setup_projection();
        if let Some(internal) = root
            .children
            .iter_mut()
            .find(|child| child.kind == "imager.setup.internal")
        {
            for (id, code, reason) in
                super::super::actions::internal_refusals_for_test(&setup.internal)
            {
                refuse(internal, id, code, reason);
            }
        }
        if let Some(desktop) = root
            .children
            .iter_mut()
            .find(|child| child.kind == "imager.setup.desktop")
        {
            for (command, _, open) in super::super::desktop_journey::commands(&setup.desktop) {
                if !open {
                    refuse(
                        desktop,
                        command.id,
                        "desktop_command_unavailable",
                        super::super::strings::t("imager-setup-desktop-command-unavailable"),
                    );
                }
            }
        }
        root
    }

    /// The host's row status, applied to the child a test built without
    /// publishing it.
    fn refuse(child: &mut tilo_actions::ResourceSpec, id: &str, code: &str, reason: String) {
        if let Some(at) = child.actions.iter().position(|action| action.id == id) {
            let spec = child.actions.remove(at);
            child.actions.insert(at, spec.unavailable(code, reason));
        }
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

        let mut blocked = guarded_app(
            install_flow::tests::Fakes::new(vec![super::super::staging::probe::Blocker::NotGpt]),
            HostOs::Windows,
        );
        blocked.app.review_full_os();
        blocked.app.start_host_probe();
        blocked.app.poll_background();
        assert!(invoke_internal(&mut blocked.app, "install-internal").is_err());
        assert!(blocked.fakes.launches.lock().unwrap().is_empty());

        let mut fixture = guarded_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Windows);
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
        let root = find_row(&[window.published()], SETUP_ROW_ID).unwrap();
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
        assert!(launches[0].profile.is_empty());
        assert_eq!(launches[0].plan_sha256, launches[0].plan.digest().unwrap());
        assert_eq!(fixture.fakes.manifests.lock().unwrap()[0], fixture.bytes);
        assert!(!guard.get());
    }

    tilo_test_env::scenario! {
        fn the_full_os_check_changes_nothing_and_reads_fresh_facts;

        phase full_os_check_refuses_mutations_even_with_forged_public_actions;
        {
            let mut fixture = guarded_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
            let result = LocalState::new(None);
            let app = &mut fixture.app;
            app.review_full_os();
            app.full_os_check = Some(result.clone());
            app.start_host_probe();
            wait_for_app(app, "the full OS check is ready", SetupApp::check_ready);
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
            let mut fixture = guarded_app(pending, HostOs::Linux);
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

        phase full_os_check_rechecks_fresh_facts_and_does_not_reuse_continue_result;
        {
            let fakes = install_flow::tests::Fakes::new(Vec::new());
            let facts = Arc::new(std::sync::Mutex::new(fakes.facts.clone()));
            let mut fixture = guarded_app(fakes, HostOs::Linux);
            let mut runtime = fixture.fakes.runtime();
            let current = Arc::clone(&facts);
            runtime.probe = Arc::new(move || current.lock().unwrap().clone());
            fixture.app.internal_install = InternalInstall::new(runtime, true, HostOs::Linux);
            let result = LocalState::new(None);
            let app = &mut fixture.app;
            app.review_full_os();
            app.full_os_check = Some(result.clone());
            app.start_host_probe();
            wait_for_app(app, "the full OS check is ready", SetupApp::check_ready);
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
            wait_for_app(app, "the recheck has read the facts", |app| {
                !app.internal_install.is_probing()
            });
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

        phase full_os_check_closed_check_is_build_limitation_and_keeps_back_available;
        {
            let result = LocalState::new(None);
            let mut app = SetupApp::for_full_os_check(result);
            app.internal_install = InternalInstall::guarded(HostOs::Linux);
            assert!(!app.check_ready());
            assert!(app.guided_navigation_guard().get());
            assert_eq!(
                app.internal_readiness_text(),
                super::super::strings::t("imager-setup-check-not-enabled")
            );
            assert!(!app.internal_projection().availability.install);
        }
    }

    fn run_inline(_: &str, task: WorkerTask) -> Result<(), String> {
        task();
        Ok(())
    }

    struct HeldFixture {
        app: SetupApp,
        fakes: install_flow::tests::Fakes,
        bytes: Vec<u8>,
        _server: Server,
        _root: tempfile::TempDir,
        _preference: tempfile::TempDir,
    }

    fn guarded_app(fakes: install_flow::tests::Fakes, host: HostOs) -> HeldFixture {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        setup_fixture(server, fakes, host, 3)
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
}
