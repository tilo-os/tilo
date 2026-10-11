//! Retained, process-local UI for the guided setup flow.

use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::Politeness;
use ui::icons::Icon;
use ui::theme::Theme;
use ui::tokens::typography::TextRole;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
use ui::widget::NumberInput;
use ui::widget::{
    Button, ButtonRole, Column, Container, ControlSize, ListRow, Row, Section, SectionHeader,
    Spacer, Text, ToggleGroup, Widget, toggle_group::Segment,
};

use super::actions::SetupEffect;
use super::controller::{SetupController, SetupPage};
use super::desktop_catalog::{DesktopRelease, PackageFormat, RuntimeEvidence};
use super::desktop_flow::{DesktopFailureKind, DesktopFlow, DesktopFlowState, DesktopSelection};
use super::folders::{self, FolderScan, SkipReason, SkippedEntry};
use super::import_view::ImportApp;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
use super::internal_download_flow::InternalDownloadFlow;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
use super::internal_install_flow::{
    self as install_flow, HostCommand, InternalInstall, Readiness, RunState,
};
use super::known_folders::{self, KnownFolder, KnownFolderKind};
use super::model::{Capabilities, Experience, FolderMode, HostOs};
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
use super::staging::helper::Action as HelperAction;
use crate::resource::{Imager, ImagerState};
use ui::LocalState;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceSurface, RunMode, SurfaceCx,
    ViewCx,
};
use ui::widget::Element;
// Page rhythm (HIG §7b): the crate's steps and the shared page sections.
use crate::page::{self, Frame, stack};

type CommandSink = Rc<RefCell<Vec<SetupCommand>>>;
type WorkerTask = Box<dyn FnOnce() + Send + 'static>;
type WorkerLauncher = fn(&str, WorkerTask) -> Result<(), String>;

const FULL_OS_UNAVAILABLE: &str = "Full Tilo is not available on this computer yet.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupCapabilities {
    pub platform: Capabilities,
    pub connected_folders: bool,
}

#[derive(Clone, Copy, Debug)]
enum SetupCommand {
    ChooseExperience(Experience),
    SetMode(FolderMode),
    SetFolderMode(usize, FolderMode),
    ClearFolderMode(usize),
    RemoveFolder(usize),
    AddSuggested(usize),
    AddFolder,
    OpenImports,
    ToggleHelp,
    Next,
    Back,
    InstallDesktop,
    OpenUsbWizard,
    CancelDesktopDownload,
    RetryDesktop,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    StartInternalDownload,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    PauseInternalDownload,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    ResumeInternalDownload,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    CancelInternalDownload,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    DiscardInternalDownload,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    ReleaseInternalDownload,
    InternalInstallUnavailable,
    /// Install, or continue the unfinished operation, with the shown plan.
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    InstallInternal,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    RestartInternal,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    RestartInternalLater,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    RetryInternal,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    RemoveInternal,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    InternalFirmwareSettings,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    InternalDisableFastStartup,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    RecheckInternal,
}

#[derive(Debug)]
enum WorkerMessage {
    KnownFolders(Vec<KnownFolder>),
    ScanFinished {
        destination: Option<PathBuf>,
        result: Result<FolderScan, String>,
    },
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    HostProbe {
        facts: super::staging::probe::HostFacts,
        records: Vec<super::staging::record::Record>,
    },
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    HostCommandFinished {
        command: HostCommand,
        result: Result<(), String>,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReviewSnapshot {
    default_mode: FolderMode,
    folder_count: usize,
}

/// Interactive retained root for setup. All choices stay process-local.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent worker, presentation, and invalidation flags do not form one state machine"
)]
pub struct SetupApp {
    controller: Rc<RefCell<SetupController>>,
    capabilities: SetupCapabilities,
    chosen_experience: Option<Experience>,
    commands: CommandSink,
    known_folders: Vec<KnownFolder>,
    worker_tx: Sender<WorkerMessage>,
    worker_rx: Receiver<WorkerMessage>,
    worker_launcher: WorkerLauncher,
    picker_rx: Option<ui::PortalChannel>,
    pending_scans: usize,
    active_scan_cancel: Option<Arc<AtomicBool>>,
    known_folder_lookup_pending: bool,
    show_help: bool,
    status: String,
    status_is_error: bool,
    composed: ui::widget::Composed,
    structure_dirty: bool,
    import_requested: bool,
    /// Whether the view shows folder imports in place of setup.
    imports: Option<LocalState<bool>>,
    /// Built with live adapters: a live window starts them on attach.
    starts_when_live: bool,
    actions: Option<ui::framework::Actions<Imager>>,
    usb: Option<LocalState<bool>>,
    desktop_flow: DesktopFlow,
    desktop_revision: u64,
    review_saver: fn(&[super::controller::FolderChoice]) -> Result<(), String>,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    internal_download_flow: Option<InternalDownloadFlow>,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    internal_download_error: Option<String>,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    internal_download_revision: u64,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    internal_install: InternalInstall,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    internal_install_revision: u64,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    host_command_pending: Option<HostCommand>,
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    internal_run_announced: RunState,
}

impl Default for SetupApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupApp {
    /// Live setup with only capabilities backed by actual adapters.
    /// Platform identity alone never enables Desktop or Full Tilo. Full Tilo
    /// becomes available only after the host probe passes, the catalog offers
    /// an internal release for this architecture and the product gate is open
    /// (design spec §5, §9). Nothing outside the process starts until it is
    /// attached to a live window (or [`Self::start_live`] is called).
    #[must_use]
    pub fn new() -> Self {
        let host = if cfg!(target_os = "windows") {
            HostOs::Windows
        } else if cfg!(target_os = "linux") {
            HostOs::Linux
        } else {
            HostOs::Other
        };
        let capabilities = SetupCapabilities {
            platform: Capabilities {
                host,
                desktop_session: false,
                internal_dual_boot: false,
            },
            connected_folders: false,
        };
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        let mut app = Self::with_parts(
            capabilities,
            launch_worker,
            InternalInstall::new(
                install_flow::StagingRuntime::live(),
                install_flow::live_gate_open(),
                host,
            ),
        );
        #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
        let mut app = Self::with_worker_launcher(capabilities, launch_worker);
        app.starts_when_live = true;
        app
    }

    /// Start what live setup reads from this computer: the Desktop host
    /// check, the familiar-folder lookup and the Full Tilo host probe.
    pub fn start_live(&mut self) {
        self.starts_when_live = false;
        self.desktop_flow = DesktopFlow::new_live();
        self.start_known_folder_lookup();
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        self.start_host_probe();
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
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        {
            Self::with_parts(
                capabilities,
                worker_launcher,
                InternalInstall::gated(capabilities.platform.host),
            )
        }
        #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
        {
            Self::with_parts(capabilities, worker_launcher)
        }
    }

    fn with_parts(
        capabilities: SetupCapabilities,
        worker_launcher: WorkerLauncher,
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        internal_install: InternalInstall,
    ) -> Self {
        let (worker_tx, worker_rx) = mpsc::channel();
        let desktop_flow = fixture_desktop_flow(capabilities.platform.desktop_session);
        #[cfg(all(any(target_os = "linux", windows, unix), test))]
        let (internal_download_flow, internal_download_error) = (
            Some(InternalDownloadFlow::unavailable_fixture(
                capabilities.platform,
            )),
            None,
        );
        #[cfg(all(any(target_os = "linux", windows), not(test)))]
        let (internal_download_flow, internal_download_error) =
            match InternalDownloadFlow::new(capabilities.platform, internal_install.link()) {
                Ok(flow) => (Some(flow), None),
                Err(error) => (None, Some(error)),
            };
        Self {
            controller: Rc::new(RefCell::new(SetupController::new(capabilities.platform))),
            capabilities,
            chosen_experience: None,
            commands: Rc::new(RefCell::new(Vec::new())),
            known_folders: unavailable_suggestions("Known-folder lookup has not run."),
            worker_tx,
            worker_rx,
            worker_launcher,
            picker_rx: None,
            pending_scans: 0,
            active_scan_cancel: None,
            known_folder_lookup_pending: false,
            show_help: false,
            status: "Choose an available experience to begin.".into(),
            status_is_error: false,
            composed: ui::widget::Composed::default(),
            structure_dirty: true,
            import_requested: false,
            imports: None,
            starts_when_live: false,
            actions: None,
            usb: None,
            desktop_flow,
            desktop_revision: 0,
            review_saver: super::desktop_continuation::persist_review,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal_download_flow,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal_download_error,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal_download_revision: 0,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal_install,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal_install_revision: 0,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            host_command_pending: None,
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal_run_announced: RunState::Idle,
        }
    }

    #[must_use]
    pub fn for_shot(scenario: &str) -> Self {
        let experience_only = scenario.starts_with("setup-experience");
        let full_os = scenario.starts_with("setup-full");
        let mut app = Self::with_capabilities(SetupCapabilities {
            platform: Capabilities {
                host: if full_os {
                    HostOs::Windows
                } else {
                    HostOs::Linux
                },
                desktop_session: !experience_only,
                internal_dual_boot: !experience_only,
            },
            connected_folders: scenario == "setup-folders-connected",
        });
        if experience_only {
            app.desktop_flow = DesktopFlow::from_fixture_state(match scenario {
                "setup-experience-checking" => DesktopFlowState::Checking,
                "setup-experience-failed" => DesktopFlowState::Failed {
                    selection: None,
                    kind: DesktopFailureKind::WorkerLost,
                    message: "The check stopped before it finished.".into(),
                },
                _ => DesktopFlowState::Unavailable {
                    host: "Fedora Linux 44 (ARM64)".into(),
                    reasons: vec![
                        "No authenticated Desktop release matches this exact host.".into(),
                    ],
                },
            });
        }
        app.known_folders = KnownFolderKind::ALL
            .into_iter()
            .map(|kind| KnownFolder {
                kind,
                path: Some(PathBuf::from("/home/ada").join(kind.label())),
                unavailable_reason: None,
            })
            .collect();
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        if full_os {
            app.pose_internal_install_for_shot(scenario);
        }
        if !experience_only {
            app.apply_command(SetupCommand::ChooseExperience(if full_os {
                Experience::FullOs
            } else {
                Experience::Desktop
            }));
            app.apply_command(SetupCommand::Next);
            let mut documents = shot_scan("Documents", 38, 2_460_000_000);
            if scenario == "setup-review" {
                documents.skipped.push(SkippedEntry {
                    path: documents.source.join("Old Photos shortcut"),
                    reason: SkipReason::SymbolicLink,
                });
            }
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(documents),
            });
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(shot_scan("Pictures", 814, 6_820_000_000)),
            });
        }
        if scenario == "setup-folders-later" {
            app.apply_command(SetupCommand::SetMode(FolderMode::Later));
        } else if scenario == "setup-folders-connected" {
            app.apply_command(SetupCommand::SetMode(FolderMode::Connected));
        } else if matches!(
            scenario,
            "setup-review" | "setup-desktop-installed" | "setup-full-review" | "setup-full-restart"
        ) {
            app.apply_command(SetupCommand::Next);
        }
        if scenario == "setup-desktop-installed" {
            app.desktop_flow = DesktopFlow::from_fixture_state(DesktopFlowState::Installed {
                selection: fixture_desktop_selection(false),
                result: super::desktop_install::InstalledDesktop {
                    version: "0.1.9-1.fc44".into(),
                    session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
                },
            });
            app.status =
                "Tilo Desktop is installed and verified. Sign out when you are ready.".into();
        }
        app
    }

    /// Full Tilo review fixtures: a planned Windows disk, a verified download
    /// and either a Secure Boot blocker or a staged installer.
    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    fn pose_internal_install_for_shot(&mut self, scenario: &str) {
        let staged = scenario == "setup-full-restart";
        let blockers = if staged {
            Vec::new()
        } else {
            vec![super::staging::probe::Blocker::SecureBootOn]
        };
        let mut install = InternalInstall::new(
            install_flow::fixture::shot_runtime(blockers),
            true,
            HostOs::Windows,
        );
        if let Some(probe) = install.probe_task() {
            let (facts, records) = probe();
            install.accept_probe(facts, records, true);
        }
        if staged {
            install.set_fixture_state(RunState::Succeeded {
                action: HelperAction::Stage,
            });
        }
        self.internal_install = install;
        if let Some(flow) = &mut self.internal_download_flow {
            flow.pose_for_shot(staged);
        }
    }

    fn controller(&self) -> Ref<'_, SetupController> {
        self.controller.borrow()
    }

    #[cfg(test)]
    fn controller_handle(&self) -> Rc<RefCell<SetupController>> {
        Rc::clone(&self.controller)
    }

    #[cfg(test)]
    fn review_snapshot(&self) -> ReviewSnapshot {
        let controller = self.controller();
        ReviewSnapshot {
            default_mode: controller.default_mode(),
            folder_count: controller.plan().folders.len(),
        }
    }

    fn folder_selection_settled(&self) -> bool {
        self.picker_rx.is_none() && self.pending_scans == 0
    }

    fn on_review_for(&self, experience: Experience) -> bool {
        let controller = self.controller();
        controller.page() == SetupPage::Review && controller.plan().experience == experience
    }

    fn setup_projection(&self) -> super::actions::SetupProjection {
        let controller = self.controller();
        let page = controller.page();
        let folders = controller
            .folders()
            .iter()
            .enumerate()
            .map(|(index, folder)| super::actions::FolderProjection {
                identity: controller
                    .folder_token(index)
                    .expect("folder identities stay aligned with folder choices"),
                index,
                mode: super::actions::mode_key(folder.mode),
                overridden: folder.mode_overridden,
            })
            .collect::<Vec<_>>();
        let suggestions = self
            .known_folders
            .iter()
            .map(|known| super::actions::SuggestionProjection {
                identity: known.kind.label().to_ascii_lowercase(),
                name: known.kind.label().to_string(),
                available: known.path.is_some(),
                unavailable_reason: known.unavailable_reason.clone(),
            })
            .collect();
        let folder_count = folders.len();
        let active_folder_count = controller
            .folders()
            .iter()
            .filter(|folder| folder.mode != FolderMode::Later)
            .count();
        let chosen_experience = controller.chosen_experience();
        let next = match page {
            SetupPage::Experience => chosen_experience
                .is_some_and(|experience| self.capabilities.platform.supports(experience)),
            SetupPage::Folders => self.folder_selection_settled(),
            SetupPage::Review => false,
        };
        let on_folder_choices = page == SetupPage::Folders;
        super::actions::SetupProjection {
            page: super::actions::page_key(page),
            selection: super::actions::SelectionProjection {
                experience: chosen_experience.map(super::actions::experience_key),
                common_mode: super::actions::mode_key(controller.default_mode()),
                folder_count,
                active_folder_count,
                folders,
                suggestions,
                help_open: self.show_help,
                imports_open: self.import_requested,
            },
            desktop: super::desktop_journey::project(
                &self.desktop_flow,
                self.on_review_for(Experience::Desktop) && self.folder_selection_settled(),
            ),
            methods: super::desktop_journey::MethodProjection {
                selected: chosen_experience.map(|experience| match experience {
                    Experience::Desktop => "desktop",
                    Experience::FullOs => "alongside",
                }),
                alongside: self.capabilities.platform.supports(Experience::FullOs),
                usb: page == SetupPage::Experience
                    && !self.desktop_flow.has_pending_operation()
                    && self.usb.is_some()
                    && matches!(
                        self.capabilities.platform.host,
                        HostOs::Linux | HostOs::Windows
                    ),
            },
            review: super::actions::ReviewProjection {
                ready: page == SetupPage::Review,
            },
            availability: super::actions::AvailabilityProjection {
                desktop: page == SetupPage::Experience
                    && self.capabilities.platform.supports(Experience::Desktop),
                full_os: page == SetupPage::Experience
                    && self.capabilities.platform.supports(Experience::FullOs),
                connected: on_folder_choices && self.capabilities.connected_folders,
                next,
                back: page != SetupPage::Experience,
                folder_policies: on_folder_choices,
                folder_edits: on_folder_choices && folder_count > 0,
                help: page == SetupPage::Folders,
                open_imports: matches!(page, SetupPage::Experience | SetupPage::Review),
                choose_folder: on_folder_choices && self.folder_selection_settled(),
            },
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            internal: self.internal_projection(),
            #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
            internal: super::actions::InternalProjection {
                readiness: "gated",
                download: "none",
                run: "idle",
                ..Default::default()
            },
        }
    }

    /// Answer `effect` as its button would, after checking it against what
    /// is shown now, with the setup's status afterwards.
    ///
    /// # Errors
    /// Returns why the effect cannot run in the current setup state.
    pub fn answer(&mut self, effect: SetupEffect) -> Result<serde_json::Value, String> {
        if self.import_requested {
            return Err("Guided setup is closed while folder imports are open.".into());
        }
        let command = match effect {
            SetupEffect::ReadSetupStatus => return Ok(self.setup_projection().value()),
            SetupEffect::ReadDesktopStatus => {
                return serde_json::to_value(self.setup_projection().desktop)
                    .map_err(|e| e.to_string());
            }
            SetupEffect::InstallDesktop
            | SetupEffect::CancelDesktopDownload
            | SetupEffect::RecheckDesktop
            | SetupEffect::OpenUsbWizard => return self.answer_desktop(effect),
            SetupEffect::ChooseExperience { experience } => {
                if self.controller().page() != SetupPage::Experience {
                    return Err("Experience choices are available on the first setup page.".into());
                }
                let experience = super::actions::parse_experience(&experience)?;
                if !self.capabilities.platform.supports(experience) {
                    return Err(experience_unavailable_reason(
                        self.capabilities.platform,
                        experience,
                    ));
                }
                SetupCommand::ChooseExperience(experience)
            }
            SetupEffect::SetCommonMode { mode } => {
                if !matches!(self.controller().page(), SetupPage::Folders) {
                    return Err("Folder choices are not open on the current setup page.".into());
                }
                let mode = super::actions::parse_mode(&mode)?;
                if mode == FolderMode::Connected && !self.capabilities.connected_folders {
                    return Err("Connected folders are not available on this computer.".into());
                }
                SetupCommand::SetMode(mode)
            }
            SetupEffect::SetFolderMode { folder, mode } => {
                let index = self
                    .controller()
                    .folder_index(&folder)
                    .ok_or_else(|| "folder selection is no longer available".to_string())?;
                if !matches!(self.controller().page(), SetupPage::Folders) {
                    return Err("Folder choices are not open on the current setup page.".into());
                }
                let mode = super::actions::parse_mode(&mode)?;
                if mode == FolderMode::Connected && !self.capabilities.connected_folders {
                    return Err("Connected folders are not available on this computer.".into());
                }
                SetupCommand::SetFolderMode(index, mode)
            }
            SetupEffect::UseCommonMode { folder } => {
                let index = self
                    .controller()
                    .folder_index(&folder)
                    .ok_or_else(|| "folder selection is no longer available".to_string())?;
                if !matches!(self.controller().page(), SetupPage::Folders) {
                    return Err("Folder choices are not open on the current setup page.".into());
                }
                if !self.controller().folders()[index].mode_overridden {
                    return Err("This folder already follows the common choice.".into());
                }
                SetupCommand::ClearFolderMode(index)
            }
            SetupEffect::RemoveFolder { folder } => {
                let index = self
                    .controller()
                    .folder_index(&folder)
                    .ok_or_else(|| "folder selection is no longer available".to_string())?;
                if !matches!(self.controller().page(), SetupPage::Folders) {
                    return Err("Folder choices are not open on the current setup page.".into());
                }
                SetupCommand::RemoveFolder(index)
            }
            SetupEffect::AddSuggestedFolder { suggestion } => {
                if self.controller().page() != SetupPage::Folders
                    || !self.folder_selection_settled()
                {
                    return Err("Folder choices are not open or a scan is still running.".into());
                }
                let index = self
                    .known_folders
                    .iter()
                    .position(|known| {
                        known.kind.label().eq_ignore_ascii_case(&suggestion) && known.path.is_some()
                    })
                    .ok_or_else(|| {
                        "That suggested folder is unavailable on this computer.".to_string()
                    })?;
                SetupCommand::AddSuggested(index)
            }
            SetupEffect::ChooseFolder => {
                if self.controller().page() != SetupPage::Folders
                    || !self.folder_selection_settled()
                {
                    return Err("The folder picker is not available right now.".into());
                }
                SetupCommand::AddFolder
            }
            SetupEffect::ToggleHelp => {
                if self.controller().page() != SetupPage::Folders {
                    return Err("Folder help is available on the folder page.".into());
                }
                SetupCommand::ToggleHelp
            }
            SetupEffect::ContinueSetup => {
                if self.controller().page() == SetupPage::Folders
                    && !self.folder_selection_settled()
                {
                    return Err("Wait for the selected folders to finish scanning.".into());
                }
                SetupCommand::Next
            }
            SetupEffect::GoBackInSetup => {
                if self.controller().page() == SetupPage::Experience {
                    return Err("Setup is already on its first page.".into());
                }
                SetupCommand::Back
            }
            SetupEffect::OpenImports => {
                if self.controller().page() == SetupPage::Folders {
                    return Err(
                        "Folder imports are available on the experience and review pages.".into(),
                    );
                }
                SetupCommand::OpenImports
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            effect => match self.internal_operation_command(effect)? {
                Some(command) => command,
                None => return Ok(self.setup_projection().value()),
            },
            #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
            _ => return Err(super::strings::t("imager-setup-internal-error-unavailable")),
        };
        self.apply_command(command);
        if self.status_is_error {
            return Err(self.status.clone());
        }
        self.open_requested_imports();
        Ok(self.setup_projection().value())
    }

    fn answer_desktop(&mut self, effect: SetupEffect) -> Result<serde_json::Value, String> {
        let state = self.setup_projection();
        let allowed = match effect {
            SetupEffect::InstallDesktop => state.desktop.actions.install,
            SetupEffect::CancelDesktopDownload => state.desktop.actions.cancel,
            SetupEffect::RecheckDesktop => state.desktop.actions.recheck,
            SetupEffect::OpenUsbWizard => state.methods.usb,
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
            SetupEffect::OpenUsbWizard => {
                self.usb
                    .as_ref()
                    .ok_or_else(|| super::strings::t("imager-setup-usb-unavailable"))?
                    .set(true);
            }
            _ => unreachable!("validated Desktop effect"),
        }
        self.structure_dirty = true;
        if self.status_is_error {
            return Err(self.status.clone());
        }
        Ok(self.setup_projection().value())
    }

    fn start_desktop_install(&mut self) {
        if !self.folder_selection_settled() {
            return;
        }
        match (self.review_saver)(self.controller.borrow().folders())
            .and_then(|()| self.desktop_flow.start_install())
        {
            Ok(()) => self.status = "Rechecking this host before download…".into(),
            Err(error) => {
                self.status = error;
                self.status_is_error = true;
            }
        }
    }

    fn apply_command(&mut self, command: SetupCommand) {
        if matches!(&command, SetupCommand::Next)
            && self.controller().page() == SetupPage::Folders
            && !self.folder_selection_settled()
        {
            return;
        }
        self.status_is_error = false;
        match command {
            SetupCommand::ChooseExperience(experience) => {
                if self.controller.borrow_mut().choose_experience(experience) {
                    self.chosen_experience = Some(experience);
                    self.status = format!("{} selected.", experience_label(experience));
                } else {
                    self.status =
                        experience_unavailable_reason(self.capabilities.platform, experience);
                    self.status_is_error = true;
                }
            }
            SetupCommand::SetMode(mode) => {
                if mode == FolderMode::Connected && !self.capabilities.connected_folders {
                    self.status = "Connected folders are not available on this computer.".into();
                    self.status_is_error = true;
                } else {
                    self.controller.borrow_mut().set_mode(mode);
                    self.status = format!("{} selected for folders.", mode_label(mode));
                }
            }
            SetupCommand::SetFolderMode(index, mode) => {
                if mode == FolderMode::Connected && !self.capabilities.connected_folders {
                    self.status = "Connected folders are not available on this computer.".into();
                    self.status_is_error = true;
                } else if self.controller.borrow_mut().set_folder_mode(index, mode) {
                    self.status = format!("Folder policy changed to {}.", mode_label(mode));
                }
            }
            SetupCommand::ClearFolderMode(index) => {
                if self
                    .controller
                    .borrow_mut()
                    .clear_folder_mode_override(index)
                {
                    self.status = "Folder now follows the common choice.".into();
                }
            }
            SetupCommand::RemoveFolder(index) => {
                if self.controller.borrow_mut().remove_folder(index) {
                    self.status = "Folder removed from this setup plan.".into();
                }
            }
            SetupCommand::AddSuggested(index) => {
                let path = self
                    .known_folders
                    .get(index)
                    .and_then(|known| known.path.clone());
                if let Some(path) = path {
                    self.start_scan(path);
                } else {
                    self.status = "That suggested folder is unavailable on this computer.".into();
                    self.status_is_error = true;
                }
            }
            SetupCommand::AddFolder => {
                if self.picker_rx.is_none() {
                    self.picker_rx = Some(folders::pick_folder());
                    self.status = "Choose a folder in the system dialog.".into();
                }
            }
            SetupCommand::OpenImports => {
                self.import_requested = true;
                self.status = "Opening the standalone folder import surface…".into();
            }
            SetupCommand::ToggleHelp => {
                self.show_help = !self.show_help;
            }
            SetupCommand::Next => match self.controller.borrow_mut().next() {
                Ok(()) => self.status = "Choices saved for this review.".into(),
                Err(error) => {
                    self.status = error;
                    self.status_is_error = true;
                }
            },
            SetupCommand::Back => {
                if let Some(cancel) = self.active_scan_cancel.take() {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.controller.borrow_mut().back();
                self.status = "Your choices are retained while you make changes.".into();
            }
            SetupCommand::OpenUsbWizard => {
                if let Err(error) = self.answer_desktop(SetupEffect::OpenUsbWizard) {
                    self.status = error;
                    self.status_is_error = true;
                }
            }
            SetupCommand::InstallDesktop => self.start_desktop_install(),
            SetupCommand::CancelDesktopDownload => {
                if self.desktop_flow.cancel_download() {
                    self.status = "Cancelling the package download…".into();
                }
            }
            SetupCommand::RetryDesktop => match self.desktop_flow.retry() {
                Ok(()) => self.status = "Rechecking this host before retry…".into(),
                Err(error) => {
                    self.status = error;
                    self.status_is_error = true;
                }
            },
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            SetupCommand::StartInternalDownload => {
                let on_full_os_review = {
                    let controller = self.controller.borrow();
                    controller.page() == SetupPage::Review
                        && controller.plan().experience == Experience::FullOs
                };
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
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            SetupCommand::PauseInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "Full Tilo download is unavailable.".to_string())
                    .and_then(InternalDownloadFlow::pause);
                self.apply_internal_command_result(result, "Pausing download…");
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            SetupCommand::ResumeInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "No saved download is available.".to_string())
                    .and_then(InternalDownloadFlow::resume);
                self.apply_internal_command_result(result, "Downloading Tilo…");
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            SetupCommand::CancelInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "Full Tilo download is unavailable.".to_string())
                    .and_then(InternalDownloadFlow::cancel);
                self.apply_internal_command_result(result, "Cancelling download…");
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            SetupCommand::DiscardInternalDownload => {
                let result = self
                    .internal_download_flow
                    .as_mut()
                    .ok_or_else(|| "No unfinished download data is available.".to_string())
                    .and_then(InternalDownloadFlow::discard);
                self.apply_internal_command_result(result, "Discarding unfinished download data…");
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
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
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            SetupCommand::InstallInternal
            | SetupCommand::RestartInternal
            | SetupCommand::RestartInternalLater
            | SetupCommand::RetryInternal
            | SetupCommand::RemoveInternal
            | SetupCommand::InternalFirmwareSettings
            | SetupCommand::InternalDisableFastStartup
            | SetupCommand::RecheckInternal => self.apply_internal_install_command(command),
        }
        self.structure_dirty = true;
    }

    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
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

    fn start_scan(&mut self, source: PathBuf) {
        if self.pending_scans != 0 {
            self.status =
                "Wait for the current folder scan to finish before adding another.".into();
            self.status_is_error = true;
            self.structure_dirty = true;
            return;
        }
        if let Some(error) = self.scan_source_error(&source) {
            self.status = error;
            self.status_is_error = true;
            self.structure_dirty = true;
            return;
        }
        let source_label = source.display().to_string();
        let tx = self.worker_tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.active_scan_cancel = Some(cancel);
        self.pending_scans = self.pending_scans.saturating_add(1);
        self.status = format!("Scanning {source_label}…");
        if let Err(error) = (self.worker_launcher)(
            "imager-folder-scan",
            Box::new(move || {
                let result = folders::scan_folder_cancellable(&source, &worker_cancel);
                let _ = tx.send(WorkerMessage::ScanFinished {
                    destination: None,
                    result,
                });
            }),
        ) {
            self.active_scan_cancel = None;
            self.pending_scans = self.pending_scans.saturating_sub(1);
            self.status = format!("Folder scan could not start for {source_label}: {error}");
            self.status_is_error = true;
            self.structure_dirty = true;
        }
    }

    fn start_known_folder_lookup(&mut self) {
        self.known_folder_lookup_pending = true;
        self.status = "Looking up familiar folders…".into();
        let tx = self.worker_tx.clone();
        if let Err(error) = (self.worker_launcher)(
            "imager-known-folders",
            Box::new(move || {
                let _ = tx.send(WorkerMessage::KnownFolders(known_folders::discover()));
            }),
        ) {
            self.known_folder_lookup_pending = false;
            self.known_folders = unavailable_suggestions("Known-folder lookup could not start.");
            self.status = format!("Known-folder lookup could not start: {error}");
            self.status_is_error = true;
            self.structure_dirty = true;
        }
    }

    fn scan_source_error(&self, source: &Path) -> Option<String> {
        let source_label = source.display();
        if system_folder_reason(source).is_some() {
            return Some(format!(
                "{source_label} is a system folder and cannot be added to setup."
            ));
        }
        self.controller
            .borrow()
            .folders()
            .iter()
            .any(|folder| {
                folder.scan.source.starts_with(source) || source.starts_with(&folder.scan.source)
            })
            .then(|| format!("{source_label} is already covered by another selected folder."))
    }

    fn distinct_suggested_destination(&self, preferred: &Path) -> Result<PathBuf, String> {
        let controller = self.controller();
        if !controller.destination_conflicts(preferred) {
            return Ok(preferred.to_path_buf());
        }
        let base_name = preferred
            .file_name()
            .ok_or_else(|| "folder destination has no suggested name".to_string())?;
        let parent = preferred.parent().unwrap_or_else(|| Path::new(""));
        let max_suffix =
            controller.folders().len().checked_add(2).ok_or_else(|| {
                "too many folder destinations to suggest another name".to_string()
            })?;
        for suffix in 2..=max_suffix {
            let mut name = base_name.to_os_string();
            name.push(format!(" ({suffix})"));
            let candidate = parent.join(name);
            if !controller.destination_conflicts(&candidate) {
                return Ok(candidate);
            }
        }
        Err("folder destination overlaps another selected destination".to_string())
    }

    fn apply_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::KnownFolders(folders) => {
                self.known_folders = folders;
                self.known_folder_lookup_pending = false;
                self.status =
                    "Familiar folders are ready to choose. None are selected automatically.".into();
                self.status_is_error = false;
            }
            WorkerMessage::ScanFinished {
                destination,
                result,
            } => {
                self.active_scan_cancel = None;
                self.pending_scans = self.pending_scans.saturating_sub(1);
                match result {
                    Ok(scan) => {
                        let source = scan.source.display().to_string();
                        if let Some(error) = self.scan_source_error(&scan.source) {
                            self.status = error;
                            self.status_is_error = true;
                            self.structure_dirty = true;
                            return;
                        }
                        let skipped_count = scan.skipped_count;
                        let added = if let Some(destination) = destination {
                            self.distinct_suggested_destination(&destination).and_then(
                                |destination| {
                                    self.controller.borrow_mut().add_folder(scan, destination)
                                },
                            )
                        } else {
                            self.controller.borrow_mut().add_folder_unresolved(scan);
                            Ok(())
                        };
                        match added {
                            Ok(()) => {
                                if skipped_count == 0 {
                                    self.status =
                                        format!("{source} is ready to copy after review.");
                                } else {
                                    self.status = format!(
                                        "Folder added with an incomplete estimate: {} skipped. Review the listed paths before continuing.",
                                        skipped_count_phrase(skipped_count)
                                    );
                                }
                                self.status_is_error = false;
                            }
                            Err(error) => {
                                self.status = error;
                                self.status_is_error = true;
                            }
                        }
                    }
                    Err(error) => {
                        self.status = format!("Folder scan failed: {error}");
                        self.status_is_error = true;
                    }
                }
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            WorkerMessage::HostProbe { facts, records } => {
                let has_release = self
                    .internal_download_flow
                    .as_ref()
                    .is_some_and(|flow| flow.offered_selection().is_some());
                self.internal_install
                    .accept_probe(facts, records, has_release);
                self.apply_install_capability();
            }
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            WorkerMessage::HostCommandFinished { command, result } => {
                self.apply_host_command_result(command, result);
            }
        }
        self.structure_dirty = true;
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.worker_rx.try_recv() {
            self.apply_worker_message(message);
            changed = true;
        }
        let picker = self.picker_rx.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(error @ mpsc::TryRecvError::Disconnected) => {
                Some(ui::PortalOutcome::Failed(error.to_string()))
            }
        });
        if let Some(outcome) = picker {
            self.picker_rx = None;
            changed = true;
            match outcome {
                ui::PortalOutcome::Opened(path) => self.start_scan(path),
                ui::PortalOutcome::Cancelled => self.status = "No folder was added.".into(),
                ui::PortalOutcome::Failed(error) => {
                    self.status = format!("The folder dialog could not open: {error}");
                    self.status_is_error = true;
                }
            }
        }
        changed |= self.desktop_flow.poll();
        if self.desktop_revision != self.desktop_flow.revision() {
            self.desktop_revision = self.desktop_flow.revision();
            let mut platform = self.capabilities.platform;
            platform.desktop_session = self.desktop_flow.desktop_available();
            self.capabilities.platform = platform;
            self.controller.borrow_mut().set_capabilities(platform);
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
            self.structure_dirty = true;
            changed = true;
        }
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        let on_full_os_review = self.on_review_for(Experience::FullOs);
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
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
                self.structure_dirty = true;
                changed = true;
            }
        }
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        {
            changed |= self.poll_internal_install();
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
            SetupCommand::RetryDesktop => Some(SetupEffect::RecheckDesktop),
            SetupCommand::OpenUsbWizard => Some(SetupEffect::OpenUsbWizard),
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

    /// Why Tilo Desktop is not on offer, or that it is still being checked,
    /// as one sentence; and whether checking again can help.
    fn desktop_unavailable(&self) -> (String, bool) {
        if self.capabilities.platform.host != HostOs::Linux {
            return (
                experience_unavailable_reason(self.capabilities.platform, Experience::Desktop),
                false,
            );
        }
        match self.desktop_flow.state() {
            DesktopFlowState::Checking => (
                "Checking whether it is available for this computer…".into(),
                false,
            ),
            DesktopFlowState::Unavailable { host, .. } => {
                (format!("Not available for {host} yet."), true)
            }
            DesktopFlowState::Failed { message, .. } => (
                format!("Couldn't check whether it is available. {message}"),
                true,
            ),
            _ => (
                experience_unavailable_reason(self.capabilities.platform, Experience::Desktop),
                false,
            ),
        }
    }

    /// One experience as a choice: what it is and what it does.
    fn experience_row(&self, experience: Experience) -> ListRow {
        let sink = Rc::clone(&self.commands);
        let (id, secondary) = match experience {
            Experience::Desktop => (
                "setup.experience.desktop",
                "Adds the Tilo session to this Linux installation.",
            ),
            Experience::FullOs => (
                "setup.experience.full",
                "Installs Tilo next to your current system, which stays the startup default.",
            ),
        };
        ListRow::new(
            id,
            if experience == Experience::FullOs {
                super::strings::t("imager-setup-alongside")
            } else {
                experience_label(experience).to_owned()
            },
        )
        .icon(match experience {
            Experience::Desktop => Icon::Monitor,
            Experience::FullOs => Icon::HardDrive,
        })
        .secondary(secondary)
        .selected(self.chosen_experience == Some(experience))
        .on_activate(move || {
            sink.borrow_mut()
                .push(SetupCommand::ChooseExperience(experience));
        })
    }

    fn experience_body(&self) -> Column {
        let mut body = page::body("Choose your Tilo experience");
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        if let Some(resume) = self.internal_resume_card() {
            body = body.push(Box::new(resume));
        }
        let (desktop, recheck): (Box<dyn Widget>, bool) =
            if self.capabilities.platform.supports(Experience::Desktop) {
                (Box::new(self.experience_row(Experience::Desktop)), false)
            } else {
                let (sentence, recheck) = self.desktop_unavailable();
                (
                    Box::new(
                        ListRow::new("setup.desktop.live", experience_label(Experience::Desktop))
                            .icon(Icon::Monitor)
                            .secondary(sentence)
                            .two_line_secondary()
                            .with_enabled(false),
                    ),
                    recheck,
                )
            };
        let rows = vec![desktop];
        body = body.push(Box::new(Section::new(rows)));
        body = body.push(Box::new(self.method_choices()));
        if recheck {
            body = body.push(Box::new(self.button(
                "setup.desktop.recheck",
                "Check again",
                SetupCommand::RetryDesktop,
            )));
        }
        body
    }

    /// One way of bringing folders over, as a choice.
    fn mode_row(&self, mode: FolderMode) -> ListRow {
        let sink = Rc::clone(&self.commands);
        let (id, title, secondary) = match mode {
            FolderMode::Copy => (
                "setup.mode.copy",
                "Copy selected folders",
                "Recommended. Your originals stay where they are, and later changes don't carry over.",
            ),
            FolderMode::Connected => (
                "setup.mode.connected",
                "Keep selected folders connected",
                "Keeps a copy in Tilo and carries changes over while both systems are in use.",
            ),
            FolderMode::Later => (
                "setup.mode.later",
                "Decide later",
                "Start empty and add folders any time.",
            ),
        };
        ListRow::new(id, title)
            .secondary(secondary)
            .selected(self.controller().default_mode() == mode)
            .on_activate(move || sink.borrow_mut().push(SetupCommand::SetMode(mode)))
    }

    /// The ways a folder can come over on this computer.
    fn offered_modes(&self) -> Vec<FolderMode> {
        [FolderMode::Copy, FolderMode::Connected, FolderMode::Later]
            .into_iter()
            .filter(|mode| *mode != FolderMode::Connected || self.capabilities.connected_folders)
            .collect()
    }

    fn method_choices(&self) -> Section {
        let t = super::strings::t;
        let methods = self.setup_projection().methods;
        let mut rows: Vec<Box<dyn Widget>> =
            vec![Box::new(Text::new(&t("imager-setup-method-body")))];
        if methods.alongside {
            rows.push(Box::new(self.experience_row(Experience::FullOs)));
        } else {
            let reason = t("imager-setup-alongside-unavailable");
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            let reason = if self.internal_install.gate() {
                self.internal_readiness_text()
            } else {
                reason
            };
            rows.push(Box::new(
                ListRow::new("setup.internal.readiness", t("imager-setup-alongside"))
                    .icon(Icon::HardDrive)
                    .secondary(reason)
                    .with_enabled(false),
            ));
        }
        rows.push(Box::new(
            self.button(
                "setup.method.usb",
                &t("imager-setup-usb-open"),
                SetupCommand::OpenUsbWizard,
            )
            .with_enabled(methods.usb),
        ));
        if !methods.usb {
            rows.push(Box::new(Text::new(&t("imager-setup-usb-unavailable"))));
        }
        Section::new(rows).with_header(SectionHeader::heading(t("imager-setup-method-heading")))
    }

    /// One selected folder: what it holds, how it comes over, and removing it.
    fn selected_folder(&self, index: usize, folder: &super::controller::FolderChoice) -> Column {
        let source = folder.scan.source.display().to_string();
        let mut facts = format!(
            "{} {}, {}",
            folder.scan.file_count,
            if folder.scan.file_count == 1 {
                "file"
            } else {
                "files"
            },
            ui::format::bytes_si(folder.scan.bytes)
        );
        if folder.scan.skipped_count > 0 {
            let _ = write!(
                facts,
                ", {} skipped",
                items_phrase(folder.scan.skipped_count)
            );
        }
        let remove = Rc::clone(&self.commands);
        let mut row = ListRow::new(format!("setup.folder.{index}"), &source)
            .icon(Icon::Folder)
            .secondary(facts)
            .action(Icon::X, "Remove", move || {
                remove.borrow_mut().push(SetupCommand::RemoveFolder(index));
            });
        if folder.scan.skipped_count > 0 {
            row = row.warning();
        }
        let modes = self.offered_modes();
        let sink = Rc::clone(&self.commands);
        let choices = modes.clone();
        let choice = ToggleGroup::new(
            modes
                .iter()
                .map(|mode| Segment::new(mode_label(*mode)))
                .collect(),
            modes
                .iter()
                .position(|mode| *mode == folder.mode)
                .unwrap_or(0),
        )
        .with_id(format!("setup.folder.{index}.mode"))
        .control_size(ControlSize::Small)
        .on_change(move |chosen| {
            if let Some(mode) = choices.get(chosen) {
                sink.borrow_mut()
                    .push(SetupCommand::SetFolderMode(index, *mode));
            }
        });
        // How the folder comes over sits under it, on the page's edge with
        // the other controls, so a long path is never cut short.
        let mut controls = Row::new()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(choice));
        if folder.mode_overridden {
            controls = controls.push(Box::new(
                self.button(
                    &format!("setup.folder.{index}.follow"),
                    "Use the default",
                    SetupCommand::ClearFolderMode(index),
                )
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small),
            ));
        }
        let mut block = page::related().push(Box::new(row)).push(Box::new(controls));
        if let Some(warning) = scan_warning(&folder.scan) {
            block = block.push(Box::new(
                Text::new(&warning)
                    .with_role(TextRole::Caption)
                    .dimmed()
                    .with_id(format!("setup.folder.{index}.warnings")),
            ));
        }
        block
    }

    fn folders_body(&self) -> Column {
        let help = self
            .button(
                "setup.help",
                if self.show_help {
                    "Hide help"
                } else {
                    "Help me choose"
                },
                SetupCommand::ToggleHelp,
            )
            .role(ButtonRole::Plain);
        let modes = self
            .offered_modes()
            .into_iter()
            .map(|mode| Box::new(self.mode_row(mode)) as Box<dyn Widget>)
            .collect();
        let mut body =
            page::body("Bring your files into Tilo")
                .push(Box::new(Section::new(modes).with_header(
                    SectionHeader::heading("Every folder").action(help),
                )));
        if self.show_help {
            body = body.push(Box::new(Text::new(
                if self.capabilities.connected_folders {
                    "If you are trying Tilo, or are unsure, choose Copy. If you keep working in both systems, Connected keeps them in step. You can always decide later."
                } else {
                    "If you are trying Tilo, or are unsure, choose Copy. Your originals are never changed, and you can always decide later."
                },
            )));
        }

        let controller = self.controller();
        let chosen = |path: &Path| {
            controller
                .folders()
                .iter()
                .any(|folder| folder.scan.source == path)
        };
        let suggestions: Vec<Box<dyn Widget>> = self
            .known_folders
            .iter()
            .enumerate()
            .filter_map(|(index, known)| {
                let path = known.path.as_ref()?;
                if chosen(path) {
                    return None;
                }
                let sink = Rc::clone(&self.commands);
                let add = Rc::clone(&self.commands);
                Some(Box::new(
                    ListRow::new(
                        format!("setup.suggestion.{}", known.kind.label().to_lowercase()),
                        known.kind.label(),
                    )
                    .icon(Icon::Folder)
                    .secondary(path.display().to_string())
                    .action(Icon::Plus, "Add", move || {
                        add.borrow_mut().push(SetupCommand::AddSuggested(index));
                    })
                    .on_activate(move || {
                        sink.borrow_mut().push(SetupCommand::AddSuggested(index));
                    }),
                ) as Box<dyn Widget>)
            })
            .collect();
        let add_folder = self
            .button("setup.add-folder", "Add folder…", SetupCommand::AddFolder)
            .with_icon(Icon::FolderPlus)
            .role(ButtonRole::Plain)
            .with_enabled(self.folder_selection_settled());
        body = body.push(Box::new(Section::new(suggestions).with_header(
            SectionHeader::heading("Suggested folders").action(add_folder),
        )));

        if !controller.folders().is_empty() || self.pending_scans > 0 {
            let mut selected: Vec<Box<dyn Widget>> = controller
                .folders()
                .iter()
                .enumerate()
                .map(|(index, folder)| {
                    Box::new(self.selected_folder(index, folder)) as Box<dyn Widget>
                })
                .collect();
            if self.pending_scans > 0 {
                selected.push(Box::new(
                    Text::new(&self.status)
                        .dimmed()
                        .with_id("setup.scan")
                        .with_live(Politeness::Polite),
                ));
            }
            body = body.push(Box::new(Section::new(selected).with_header(
                SectionHeader::heading("Selected folders").count(controller.folders().len()),
            )));
        }
        drop(controller);
        body
    }

    /// The selected folders on the review, and what happens to them.
    fn review_folders(&self, experience: Experience) -> Section {
        let controller = self.controller();
        let scheduled = controller
            .folders()
            .iter()
            .enumerate()
            .filter(|(_, folder)| folder.mode != FolderMode::Later)
            .collect::<Vec<_>>();
        let mut items: Vec<Box<dyn Widget>> = scheduled
            .iter()
            .map(|(index, folder)| {
                let mut facts = format!(
                    "{}, {}",
                    mode_label(folder.mode),
                    ui::format::bytes_si(folder.scan.bytes)
                );
                if folder.scan.skipped_count > 0 {
                    let _ = write!(
                        facts,
                        ", {} skipped",
                        items_phrase(folder.scan.skipped_count)
                    );
                }
                let row = ListRow::new(
                    format!("setup.review.folder.{index}"),
                    folder.scan.source.display().to_string(),
                )
                .icon(Icon::Folder)
                .secondary(facts);
                Box::new(if folder.scan.skipped_count > 0 {
                    row.warning()
                } else {
                    row
                }) as Box<dyn Widget>
            })
            .collect();
        if scheduled.is_empty() {
            items.push(Box::new(
                Text::new("No folders are scheduled. You can add them later.")
                    .dimmed()
                    .with_id("setup.review.folders"),
            ));
        } else {
            let skipped = scheduled
                .iter()
                .map(|(_, folder)| folder.scan.skipped_count)
                .sum::<u64>();
            let estimate = controller.selected_folder_bytes().map_or_else(
                |_| "The space these folders need could not be added up.".to_string(),
                |bytes| {
                    if skipped == 0 {
                        format!(
                            "These folders need about {} in Tilo.",
                            ui::format::bytes_si(bytes)
                        )
                    } else {
                        format!(
                            "These folders need about {} in Tilo. The estimate is incomplete: it leaves out {} that could not be read.",
                            ui::format::bytes_si(bytes),
                            items_phrase(skipped)
                        )
                    }
                },
            );
            items.push(Box::new(
                Text::new(&estimate)
                    .dimmed()
                    .with_id("setup.review.estimate"),
            ));
        }
        let pending = if experience == Experience::FullOs {
            "After Tilo is installed and you sign in, setup shows where each folder goes before anything is copied."
        } else {
            "After your first Tilo login, setup shows where each folder goes before anything is copied."
        };
        items.push(Box::new(
            Text::new(pending).dimmed().with_id("setup.review.pending"),
        ));
        Section::new(items).with_header(
            SectionHeader::heading("Folders").action(
                self.button(
                    "setup.open-imports",
                    "Add folders now…",
                    SetupCommand::OpenImports,
                )
                .role(ButtonRole::Plain),
            ),
        )
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
            DesktopFlowState::Installed { selection, result } => format!(
                "Tilo Desktop {} is installed and verified on {}. Save your work and sign out when you are ready, then choose Tilo on the login screen. Your current session stays the default.",
                result.version, selection.host
            ),
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
                format!("{what} {message} Your folder choices are kept; check again to retry.")
            }
        };
        let retained = match self.capabilities.platform.host {
            HostOs::Windows => "Windows stays installed and remains the startup default.",
            HostOs::Linux => {
                "Your Linux installation stays as it is and remains the default session."
            }
            HostOs::Other => "Your current system stays installed and remains the default.",
        };
        Section::new(vec![Box::new(
            page::related()
                .push(Box::new(
                    Text::new(&detail)
                        .with_id("setup.desktop.status")
                        .with_live(Politeness::Polite),
                ))
                .push(Box::new(Text::new(retained).dimmed())),
        )])
        .with_header(SectionHeader::heading("Tilo Desktop"))
    }

    #[cfg(any(target_os = "linux", windows, all(test, unix)))]
    fn internal_download_card(&self) -> Section {
        let mut content = page::related();
        if let Some(flow) = &self.internal_download_flow {
            let presentation = flow.presentation();
            let mut summary = presentation.summary.clone();
            if presentation.phase.is_none()
                && let Some(error) = flow.catalog_error()
            {
                summary = error.to_string();
            }
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
        let experience = self.controller().plan().experience;
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        let internal_offered =
            experience == Experience::FullOs && self.internal_install.offers_full_os();
        #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
        let internal_offered = false;
        let installed = experience == Experience::Desktop
            && matches!(
                self.desktop_flow.state(),
                DesktopFlowState::Installed { .. }
            );
        let mut body = page::body(if installed {
            "Tilo Desktop is installed"
        } else {
            "Review Tilo setup"
        });
        if experience == Experience::Desktop {
            body = body.push(Box::new(self.desktop_install_card()));
        }
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
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
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        if experience == Experience::FullOs {
            body = body.push(Box::new(self.internal_download_card()));
        }
        if experience == Experience::FullOs && !internal_offered {
            body = body.push(Box::new(
                Section::new(vec![Box::new(
                    Text::new(FULL_OS_UNAVAILABLE).with_live(Politeness::Polite),
                )])
                .with_header(SectionHeader::heading("Before installing")),
            ));
        }
        body.push(Box::new(self.review_folders(experience)))
    }

    /// Hand the window to folder imports once they were asked for: the
    /// view shows them in place of setup.
    fn open_requested_imports(&self) {
        if self.import_requested
            && let Some(imports) = &self.imports
        {
            imports.set(true);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the wizard shell declares its body, status, and state-dependent navigation together"
    )]
    fn build_composition(&self) -> Box<dyn Widget> {
        let current = self.controller().page();
        let body = match current {
            SetupPage::Experience => self.experience_body(),
            SetupPage::Folders => self.folders_body(),
            SetupPage::Review => self.review_body(),
        };
        let mut nav = Row::new().no_wrap();
        if current == SetupPage::Experience {
            nav = nav.push(Box::new(self.button(
                "setup.open-imports",
                "Add folders now…",
                SetupCommand::OpenImports,
            )));
        } else {
            nav = nav.push(Box::new(
                self.button("setup.back", "Back", SetupCommand::Back)
                    .with_icon(Icon::ChevronLeft),
            ));
        }
        nav = nav.push(Box::new(Spacer::new()));
        #[cfg(any(target_os = "linux", windows, all(test, unix)))]
        let internal_nav = current == SetupPage::Review
            && self.controller().plan().experience == Experience::FullOs
            && self.internal_install.offers_full_os();
        #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
        let internal_nav = false;
        if internal_nav {
            #[cfg(any(target_os = "linux", windows, all(test, unix)))]
            {
                nav = self.internal_nav(nav);
            }
        } else if current == SetupPage::Review {
            let experience = self.controller().plan().experience;
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
                        self.folder_selection_settled(),
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
        } else {
            let next_enabled = match current {
                SetupPage::Experience => self
                    .chosen_experience
                    .is_some_and(|choice| self.capabilities.platform.supports(choice)),
                SetupPage::Folders => self.folder_selection_settled(),
                SetupPage::Review => true,
            };
            nav = nav.push(Box::new(
                self.button("setup.next", "Continue", SetupCommand::Next)
                    .role(ButtonRole::Primary)
                    .with_enabled(next_enabled),
            ));
        }

        Frame::new(body)
            .stepper(
                &["Experience", "Folders", "Review"],
                match current {
                    SetupPage::Experience => 0,
                    SetupPage::Folders => 1,
                    SetupPage::Review => 2,
                },
            )
            .notice(page::notice(&self.status, self.status_is_error))
            .actions(nav)
            .build()
    }
}

/// Full Tilo installed alongside the current system (design spec §5, §8).
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
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
                self.structure_dirty = true;
                return Ok(None);
            }
            Op::InstallInternal { reviewed } => {
                review_only(self)?;
                if self.internal_install.reviewed_digest().as_deref() != Some(reviewed.as_str()) {
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
        let Some(task) = self.internal_install.probe_task() else {
            return;
        };
        let tx = self.worker_tx.clone();
        if let Err(error) = (self.worker_launcher)(
            "imager-host-probe",
            Box::new(move || {
                let (facts, records) = task();
                let _ = tx.send(WorkerMessage::HostProbe { facts, records });
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
        self.structure_dirty = true;
    }

    /// Mirror the gated readiness into the capabilities the controller and
    /// download flow check. A closed gate leaves fixture capabilities alone.
    fn apply_install_capability(&mut self) {
        if !self.internal_install.gate() {
            return;
        }
        let mut platform = self.capabilities.platform;
        platform.internal_dual_boot = self.internal_install.offers_full_os();
        self.capabilities.platform = platform;
        self.controller.borrow_mut().set_capabilities(platform);
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
            .reviewed_digest()
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
        self.structure_dirty = true;
    }

    /// Follow helper runs and stepper commits; announce finished runs.
    fn poll_internal_install(&mut self) -> bool {
        let mut changed = self.internal_install.poll();
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
            changed = true;
        }
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
            self.structure_dirty = true;
            changed = true;
        }
        if self.internal_install.take_reprobe() {
            self.start_host_probe();
            changed = true;
        }
        changed
    }

    fn internal_projection(&self) -> super::actions::InternalProjection {
        let install = &self.internal_install;
        let page = self.controller().page();
        let on_review = self.on_full_os_review();
        let ready = install.offers_full_os();
        let flow = self.internal_download_flow.as_ref();
        let running = install.is_running();
        let idle_host = self.host_command_pending.is_none();
        let resume_page = matches!(page, SetupPage::Experience | SetupPage::Review);
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
            reviewed: install.reviewed_digest(),
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
                retry: install.gate() && resume_page && pending.is_some() && !running,
                remove: install.gate() && resume_page && pending.is_some() && !running,
                firmware_settings: on_review && idle_host && install.firmware_command().is_some(),
                disable_fast_startup: on_review
                    && idle_host
                    && install.command_available(HostCommand::DisableFastStartup),
                recheck: install.gate() && resume_page && !install.is_probing() && !running,
            },
        }
    }

    fn internal_readiness_text(&self) -> String {
        let host = install_flow::host_arg(self.internal_install.host());
        match self.internal_install.readiness() {
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
                    ("needed", ui::format::bytes(needed_bytes).into()),
                    ("available", ui::format::bytes(available_bytes).into()),
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
                                    ui::format::bytes(
                                        shrink.from_bytes.saturating_sub(shrink.to_bytes),
                                    )
                                    .into(),
                                ),
                                ("from", ui::format::bytes(shrink.from_bytes).into()),
                                ("to", ui::format::bytes(shrink.to_bytes).into()),
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
                            ui::format::bytes(install_flow::target_extent_bytes(plan)).into(),
                        )],
                    ),
                    t_args(
                        "imager-setup-internal-plan-staging",
                        &[(
                            "staging",
                            ui::format::bytes(install_flow::staging_bytes(plan)).into(),
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
                                    ("done", ui::format::bytes(progress.done_bytes).into()),
                                    ("total", ui::format::bytes(progress.total_bytes).into()),
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
                    .is_ok(),
            ),
        ))
    }
}

#[cfg(any(target_os = "linux", windows, all(test, unix)))]
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

fn launch_worker(name: &str, task: WorkerTask) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(task)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn unavailable_suggestions(reason: &str) -> Vec<KnownFolder> {
    KnownFolderKind::ALL
        .into_iter()
        .map(|kind| KnownFolder {
            kind,
            path: None,
            unavailable_reason: Some(reason.to_string()),
        })
        .collect()
}

pub(super) fn system_folder_reason(path: &Path) -> Option<&'static str> {
    if path.parent().is_none() {
        return Some("filesystem root");
    }

    #[cfg(unix)]
    {
        const SYSTEM_ROOTS: [&str; 13] = [
            "/bin", "/boot", "/dev", "/etc", "/lib", "/private", "/proc", "/root", "/run", "/sbin",
            "/System", "/sys", "/usr",
        ];
        let is_protected_var_path = (path == Path::new("/var") || path.starts_with("/var"))
            && !path.starts_with("/var/home");
        if SYSTEM_ROOTS
            .into_iter()
            .any(|root| path == Path::new(root) || path.starts_with(root))
            || is_protected_var_path
            || path == Path::new("/Library")
            || path.starts_with("/Library")
        {
            return Some("operating-system directory");
        }
    }

    #[cfg(windows)]
    {
        let first = path.components().find_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        });
        if first.as_deref().is_some_and(|name| {
            matches!(
                name,
                "windows" | "program files" | "program files (x86)" | "programdata"
            )
        }) {
            return Some("operating-system directory");
        }
    }

    None
}

fn experience_label(experience: Experience) -> &'static str {
    match experience {
        Experience::Desktop => "Tilo Desktop",
        Experience::FullOs => "Full Tilo",
    }
}

fn experience_unavailable_reason(capabilities: Capabilities, experience: Experience) -> String {
    match experience {
        Experience::Desktop if capabilities.host == HostOs::Windows => {
            "Tilo Desktop is available only on supported Linux installations.".into()
        }
        Experience::Desktop if capabilities.host == HostOs::Linux => {
            "Tilo Desktop isn't available for this Linux installation yet.".into()
        }
        Experience::Desktop => "Tilo Desktop is unavailable on this operating system.".into(),
        Experience::FullOs => "Full Tilo is not available on this computer yet.".into(),
    }
}

fn mode_label(mode: FolderMode) -> &'static str {
    match mode {
        FolderMode::Copy => "Copy",
        FolderMode::Connected => "Connected",
        FolderMode::Later => "Decide later",
    }
}

/// The entries a folder's scan left out, one per line under a sentence
/// that says what that means for the estimate.
fn scan_warning(scan: &FolderScan) -> Option<String> {
    if scan.skipped_count == 0 {
        return None;
    }
    let mut warning = format!(
        "The file count and size leave out {} that couldn't be read:",
        items_phrase(scan.skipped_count)
    );
    for skipped in &scan.skipped {
        let reason = match &skipped.reason {
            SkipReason::SymbolicLink => "a symbolic link".to_string(),
            SkipReason::ReadError(error) => format!("couldn't be read ({error})"),
            SkipReason::UnsupportedFileType => "not a regular file".to_string(),
        };
        let _ = write!(warning, "\n{}, {reason}", skipped.path.display());
    }
    if scan.skipped_count > scan.skipped.len() as u64 {
        let hidden = scan.skipped_count - scan.skipped.len() as u64;
        let _ = write!(warning, "\n{} more", items_phrase(hidden));
    }
    Some(warning)
}

/// "1 item", "3 items".
fn items_phrase(count: u64) -> String {
    if count == 1 {
        "1 item".to_string()
    } else {
        format!("{count} items")
    }
}

fn skipped_count_phrase(count: u64) -> String {
    if count == 1 {
        "1 entry was".to_string()
    } else {
        format!("{count} entries were")
    }
}

fn shot_scan(name: &str, file_count: u64, bytes: u64) -> FolderScan {
    FolderScan {
        source: PathBuf::from("/home/ada").join(name),
        file_count,
        bytes,
        empty_directory_count: 0,
        empty_directories: Vec::new(),
        skipped_count: 0,
        skipped: Vec::new(),
    }
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

impl Drop for SetupApp {
    fn drop(&mut self) {
        if let Some(cancel) = self.active_scan_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Widget for SetupApp {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.poll_background();
        self.drain_commands();
        self.open_requested_imports();
        self.composed.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.poll_background() || self.drain_commands();
        self.open_requested_imports();
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
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "SetupApp"
    }

    fn needs_idle_sync(&self) -> bool {
        self.known_folder_lookup_pending
            || self.pending_scans > 0
            || self.picker_rx.is_some()
            || self.desktop_flow.has_pending_operation()
            || {
                #[cfg(any(target_os = "linux", windows, all(test, unix)))]
                {
                    self.internal_download_flow
                        .as_ref()
                        .is_some_and(InternalDownloadFlow::has_pending_operation)
                        || self.internal_install.is_probing()
                        || self.internal_install.is_running()
                        || self.host_command_pending.is_some()
                }
                #[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
                {
                    false
                }
            }
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) || self.composed.take_structure_dirty()
    }
}

/// The screenshot scenarios guided setup stages.
pub const SCENARIOS: &[&str] = &[
    "setup-experience-checking",
    "setup-experience",
    "setup-experience-failed",
    "setup-folders-copy",
    "setup-folders-connected",
    "setup-folders-later",
    "setup-review",
    "setup-desktop-installed",
    "setup-full-review",
    "setup-full-restart",
];

impl ResourceSurface<Imager, SetupEffect> for SetupApp {
    fn present(&mut self, _state: &Rc<ImagerState>) {}

    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        self.actions = Some(cx.actions());
        if cx.mode().is_live() && self.starts_when_live {
            self.start_live();
        }
    }

    /// Pose the scenario's setup: its capabilities, folders and page. The
    /// fixtures are deterministic and never read this computer.
    fn stage(&mut self, scenario: Option<&str>) {
        let Some(scenario) = scenario.filter(|name| SCENARIOS.contains(name)) else {
            return;
        };
        let imports = self.imports.take();
        let actions = self.actions.take();
        let usb = self.usb.take();
        *self = Self::for_shot(scenario);
        self.imports = imports;
        self.actions = actions;
        self.usb = usb;
        self.structure_dirty = true;
    }

    fn effect(&mut self, effect: SetupEffect, _cx: &mut EffectCx<Imager, Self>) -> EffectAnswer {
        self.answer(effect).map(Effected::value)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        super::actions::statuses(&self.setup_projection())
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        vec![super::actions::row(&self.setup_projection())]
    }
}

/// Guided setup in a view of the imager resource. Asking for folder imports
/// shows them in its place. A live window checks this computer; a capture
/// or a past version starts from a fixture and reads nothing from it.
pub fn view(cx: &mut ViewCx<Imager>) -> Element {
    mount(cx, setup_for, |_| ImportApp::new())
}

/// The setup `mode` builds: live adapters in a live window, a fixture
/// otherwise.
fn setup_for(mode: RunMode) -> SetupApp {
    if mode.is_live() {
        SetupApp::new()
    } else {
        SetupApp::with_capabilities(SetupCapabilities {
            platform: Capabilities {
                host: HostOs::Linux,
                desktop_session: false,
                internal_dual_boot: false,
            },
            connected_folders: false,
        })
    }
}

/// [`view`] with the setup and the imports each run mode builds.
pub(crate) fn mount(
    cx: &mut ViewCx<Imager>,
    setup: fn(RunMode) -> SetupApp,
    imports: fn(RunMode) -> ImportApp,
) -> Element {
    let writing = cx.local(false);
    let writer_shown = writing.clone();
    cx.switch(&writer_shown, move |cx, writing_now| {
        if *writing_now {
            return cx.surface(crate::ImagerApp::new());
        }
        let importing = cx.local(false);
        let shown = importing.clone();
        let writing = writing.clone();
        cx.switch(&shown, move |cx, importing_now| {
            if *importing_now {
                cx.surface(imports(cx.mode()))
            } else {
                let mut app = setup(cx.mode());
                app.imports = Some(importing.clone());
                app.usb = Some(writing.clone());
                cx.surface(app)
            }
        })
    })
}

// View fixtures depend on the Unix installer and cache worker test adapters.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::path::PathBuf;

    use crate::setup::controller::FolderDestination;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use ui::a11y::Action;
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::{Effect, Resource};

    use crate::setup::import_actions::ImportEffect;
    use ui::headless::{FrameHarness, SemanticExpectation};

    use super::*;
    use crate::setup::actions::SETUP_ROW_ID;
    use crate::setup::folders::{FolderScan, SkipReason, SkippedEntry};
    use crate::setup::internal_bundle;
    use crate::setup::internal_cache::coordinator::{Phase as InternalPhase, Worker};
    use crate::setup::internal_download::tests::{Server, response};
    use crate::setup::model::{Capabilities, Experience, FolderMode, HostOs};
    use ui::time::Duration;

    #[cfg(windows)]
    const TEST_SOURCE_ROOT: &str = r"C:\setup-source";
    #[cfg(not(windows))]
    const TEST_SOURCE_ROOT: &str = "/setup-source";
    #[cfg(windows)]
    const TEST_DEST_ROOT: &str = r"C:\tilo-home";
    #[cfg(not(windows))]
    const TEST_DEST_ROOT: &str = "/home/tilo";
    #[cfg(windows)]
    const TEST_SYSTEM_ROOT: &str = r"C:\Windows";
    #[cfg(not(windows))]
    const TEST_SYSTEM_ROOT: &str = "/etc";

    include!("desktop_journey_tests.rs");

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

    fn accepted_without_completion_worker(_: &str, _: WorkerTask) -> Result<(), String> {
        Ok(())
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

    #[derive(Clone, Copy)]
    enum PendingFolderSelection {
        Scan,
        Picker,
    }

    fn stale_ready_desktop_review(
        pending: PendingFolderSelection,
    ) -> (SetupApp, Option<mpsc::Sender<ui::PortalOutcome>>) {
        let mut app = SetupApp::with_worker_launcher(
            supported_fixture().capabilities,
            accepted_without_completion_worker,
        );
        app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
        app.apply_command(SetupCommand::Next);
        let picker_tx = match pending {
            PendingFolderSelection::Scan => {
                app.start_scan(PathBuf::from(TEST_SOURCE_ROOT).join("Documents"));
                None
            }
            PendingFolderSelection::Picker => {
                let (tx, rx) = mpsc::channel();
                app.picker_rx = Some(rx);
                Some(tx)
            }
        };
        app.controller.borrow_mut().next().unwrap();
        assert!(app.controller().folders().is_empty());
        app.desktop_flow = DesktopFlow::with_runtime_and_state(
            super::super::desktop_flow::DesktopRuntime {
                launch_probe: disconnected_desktop_probe,
                launch_download: disconnected_desktop_download,
                launch_install: disconnected_desktop_install,
            },
            DesktopFlowState::Ready(fixture_desktop_selection(false)),
        );
        (app, picker_tx)
    }

    fn scan(name: &str, bytes: u64) -> FolderScan {
        FolderScan {
            source: PathBuf::from(TEST_SOURCE_ROOT).join(name),
            file_count: 2,
            bytes,
            empty_directory_count: 0,
            empty_directories: Vec::new(),
            skipped_count: 0,
            skipped: Vec::new(),
        }
    }

    fn scan_with_warnings(name: &str, bytes: u64) -> FolderScan {
        let source = PathBuf::from(TEST_SOURCE_ROOT).join(name);
        FolderScan {
            source: source.clone(),
            file_count: 2,
            bytes,
            empty_directory_count: 0,
            empty_directories: Vec::new(),
            skipped_count: 2,
            skipped: vec![
                SkippedEntry {
                    path: source.join("private"),
                    reason: SkipReason::ReadError("permission denied".into()),
                },
                SkippedEntry {
                    path: source.join("linked"),
                    reason: SkipReason::SymbolicLink,
                },
            ],
        }
    }

    /// Held scans, pending and disconnected pickers and stale desktop installs.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn folder_selection_blocks_navigation_until_scans_and_pickers_settle() {
        // Phase `folder_selection_held_scans_block_both_experiences_until_every_terminal_result`.
        {
            for experience in [Experience::Desktop, Experience::FullOs] {
                let mut app = SetupApp::with_worker_launcher(
                    supported_fixture().capabilities,
                    accepted_without_completion_worker,
                );
                app.apply_command(SetupCommand::ChooseExperience(experience));
                app.apply_command(SetupCommand::Next);
                assert_eq!(app.controller().page(), SetupPage::Folders);

                app.start_scan(PathBuf::from(TEST_SOURCE_ROOT).join("Documents"));
                app.start_scan(PathBuf::from(TEST_SOURCE_ROOT).join("Pictures"));
                assert_eq!(app.pending_scans, 1);
                assert!(app.status.contains("current folder scan"));
                assert!(app.picker_rx.is_none());
                let composition = app.build_composition();
                assert!(retained_widget_disabled(composition.as_ref(), "setup.next"));

                let scanning_status = app.status.clone();
                app.apply_command(SetupCommand::Next);
                assert_eq!(app.controller().page(), SetupPage::Folders);
                assert!(app.controller().folders().is_empty());
                assert_eq!(app.status, scanning_status);

                app.apply_worker_message(WorkerMessage::ScanFinished {
                    destination: None,
                    result: Err("test scan failed".into()),
                });
                assert_eq!(app.pending_scans, 0);
                assert!(app.status.contains("test scan failed"));
                assert!(app.status_is_error);

                app.start_scan(PathBuf::from(TEST_SOURCE_ROOT).join("Pictures"));
                assert_eq!(app.pending_scans, 1);
                app.apply_worker_message(WorkerMessage::ScanFinished {
                    destination: None,
                    result: Ok(scan("Pictures", 12)),
                });
                assert_eq!(app.pending_scans, 0);
                assert_eq!(app.controller().folders().len(), 1);
                let composition = app.build_composition();
                assert!(!retained_widget_disabled(
                    composition.as_ref(),
                    "setup.next"
                ));

                app.apply_command(SetupCommand::Next);
                assert_eq!(app.controller().page(), SetupPage::Review);
            }
        }

        // Phase `folder_selection_pending_picker_blocks_next_until_cancelled`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            app.apply_command(SetupCommand::Next);
            let (picker_tx, picker_rx) = mpsc::channel();
            app.picker_rx = Some(picker_rx);

            let composition = app.build_composition();
            assert!(retained_widget_disabled(composition.as_ref(), "setup.next"));
            app.apply_command(SetupCommand::Next);
            assert_eq!(app.controller().page(), SetupPage::Folders);

            picker_tx.send(ui::PortalOutcome::Cancelled).unwrap();
            assert!(app.poll_background());
            assert!(app.picker_rx.is_none());
            let composition = app.build_composition();
            assert!(!retained_widget_disabled(
                composition.as_ref(),
                "setup.next"
            ));
            app.apply_command(SetupCommand::Next);
            assert_eq!(app.controller().page(), SetupPage::Review);
        }

        // Phase `folder_selection_disconnected_picker_recovers_with_existing_failure_path`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            app.apply_command(SetupCommand::Next);
            let (picker_tx, picker_rx) = mpsc::channel::<ui::PortalOutcome>();
            drop(picker_tx);
            app.picker_rx = Some(picker_rx);

            app.apply_command(SetupCommand::Next);
            assert_eq!(app.controller().page(), SetupPage::Folders);
            assert!(app.poll_background());
            assert!(app.picker_rx.is_none());
            assert!(app.status.starts_with("The folder dialog could not open:"));
            assert!(app.status_is_error);
            assert!(app.controller().folders().is_empty());

            app.apply_command(SetupCommand::Next);
            assert_eq!(app.controller().page(), SetupPage::Review);
        }

        // Phase `folder_selection_stale_desktop_install_is_disabled_and_rejected_until_settled`.
        {
            for pending in [PendingFolderSelection::Scan, PendingFolderSelection::Picker] {
                let (render_app, _render_picker_tx) = stale_ready_desktop_review(pending);
                let composition = render_app.build_composition();
                assert!(retained_widget_disabled(
                    composition.as_ref(),
                    "setup.install"
                ));

                let (mut command_app, picker_tx) = stale_ready_desktop_review(pending);
                command_app.apply_command(SetupCommand::InstallDesktop);
                assert!(matches!(
                    command_app.desktop_flow.state(),
                    DesktopFlowState::Ready(_)
                ));
                assert!(!command_app.desktop_flow.has_pending_operation());

                match pending {
                    PendingFolderSelection::Scan => {
                        command_app.apply_worker_message(WorkerMessage::ScanFinished {
                            destination: None,
                            result: Err("test scan failed".into()),
                        });
                    }
                    PendingFolderSelection::Picker => {
                        picker_tx
                            .unwrap()
                            .send(ui::PortalOutcome::Cancelled)
                            .unwrap();
                        assert!(command_app.poll_background());
                    }
                }
                assert!(command_app.controller().folders().is_empty());
                command_app.apply_command(SetupCommand::InstallDesktop);
                assert!(matches!(
                    command_app.desktop_flow.state(),
                    DesktopFlowState::Checking
                ));
                assert!(command_app.desktop_flow.has_pending_operation());
            }
        }
    }

    /// Experience availability, folder modes, back navigation and keyboard/assistive activation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn experience_commands_route_through_the_controller() {
        // Phase `unavailable_experience_command_cannot_proceed`.
        {
            let mut app = SetupApp::with_capabilities(SetupCapabilities {
                platform: Capabilities {
                    host: HostOs::Windows,
                    desktop_session: false,
                    internal_dual_boot: false,
                },
                connected_folders: false,
            });

            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);

            assert_eq!(app.controller().page(), SetupPage::Experience);
            assert!(app.status.contains("available"));
        }

        // Phase `copy_connected_and_later_commands_change_review`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            app.apply_command(SetupCommand::Next);
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan("Documents", 4096)),
            });

            for mode in [FolderMode::Copy, FolderMode::Connected, FolderMode::Later] {
                app.apply_command(SetupCommand::SetMode(mode));
                app.apply_command(SetupCommand::Next);
                assert_eq!(app.controller().page(), SetupPage::Review);
                assert_eq!(app.review_snapshot().default_mode, mode);
                assert_eq!(
                    app.review_snapshot().folder_count,
                    usize::from(mode != FolderMode::Later)
                );
                app.apply_command(SetupCommand::Back);
            }
        }

        // Phase `back_retains_experience_and_folder_settings`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Pictures")),
                result: Ok(scan("Pictures", 12)),
            });
            app.apply_command(SetupCommand::SetMode(FolderMode::Copy));
            app.apply_command(SetupCommand::Next);
            app.apply_command(SetupCommand::Back);

            assert_eq!(app.controller().page(), SetupPage::Folders);
            assert_eq!(app.controller().plan().experience, Experience::FullOs);
            assert_eq!(app.controller().default_mode(), FolderMode::Copy);
            assert_eq!(app.controller().folders().len(), 1);
        }

        // Phase `initial_unavailable_experience_can_activate_the_real_import_surface`.
        // Catches exposing later import only after unavailable installation steps.
        {
            let app = SetupApp::with_capabilities(SetupCapabilities {
                platform: Capabilities {
                    host: HostOs::Other,
                    desktop_session: false,
                    internal_dual_boot: false,
                },
                connected_folders: false,
            });
            let mut window = launch(app, || ImportApp::for_shot("import-complete"));
            let harness = window.harness();

            harness
                .expect(
                    &SemanticExpectation::named("setup.internal.readiness")
                        .value_contains("Not available"),
                )
                .unwrap();
            harness
                .expect(&SemanticExpectation::named("setup.open-imports").enabled(true))
                .unwrap();
            harness
                .perform_named("setup.open-imports", &Action::Activate)
                .unwrap();
            window.settle();

            window
                .harness()
                .expect(&SemanticExpectation::named("import.pick-source").enabled(true))
                .unwrap();
        }

        // Phase `enter_and_space_keyboard_events_reach_controller_commands`.
        {
            let app = supported_fixture();
            let observed = app.controller_handle();
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (1180, 820));

            harness
                .perform_named("setup.experience.full", &Action::Focus)
                .unwrap();
            harness.key("Enter");

            assert_eq!(observed.borrow().plan().experience, Experience::FullOs);

            harness.perform_named("setup.next", &Action::Focus).unwrap();
            harness.key("Space");
            assert_eq!(observed.borrow().page(), SetupPage::Folders);
        }

        // Phase `assistive_activation_reaches_controller_command`.
        {
            let app = supported_fixture();
            let observed = app.controller_handle();
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (1180, 820));

            harness
                .perform_named("setup.experience.full", &Action::Activate)
                .unwrap();

            assert_eq!(observed.borrow().plan().experience, Experience::FullOs);
        }
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
            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);
            app.apply_command(SetupCommand::SetMode(FolderMode::Later));
            app.apply_command(SetupCommand::Next);
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
            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);
            app.apply_command(SetupCommand::SetMode(FolderMode::Later));
            app.apply_command(SetupCommand::Next);

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
            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);
            app.apply_command(SetupCommand::SetMode(FolderMode::Later));
            app.apply_command(SetupCommand::Next);

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

        // Phase `full_os_review_keeps_folder_copy_pending`.
        // Catches treating the setup plan's suggested Tilo path as a mounted target
        // before Full Tilo installation and target-side volume resolution exist.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan("Documents", 12)),
            });
            app.apply_command(SetupCommand::Next);
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (1180, 820));

            harness
                .expect(
                    &SemanticExpectation::named("setup.review.pending")
                        .name_contains("before anything is copied"),
                )
                .unwrap();
        }
    }

    /// Failed scans, warnings, distinct destinations and launch failures.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn scan_results_warnings_and_launch_failures_are_truthful() {
        // Phase `failed_scan_worker_never_becomes_a_completed_folder`.
        {
            let mut app = supported_fixture();
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Err("permission denied while reading a long redirected folder path".into()),
            });

            assert!(app.controller().folders().is_empty());
            assert!(app.status.contains("permission denied"));
            assert!(!app.status.contains("ready to copy"));
        }

        // Phase `partial_scan_warnings_persist_on_folder_and_review_screens`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            app.apply_command(SetupCommand::Next);
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan_with_warnings("Documents", 4096)),
            });

            assert!(app.status.contains("incomplete estimate"));
            assert!(app.status.contains("2 entries"));

            app.apply_command(SetupCommand::Next);
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));
            harness
                .expect(
                    &SemanticExpectation::named("setup.review.folder.0")
                        .value_contains("2 items skipped"),
                )
                .unwrap();
            harness
                .expect(
                    &SemanticExpectation::named("setup.review.estimate")
                        .name_contains("incomplete"),
                )
                .unwrap();

            harness
                .perform_named("setup.back", &Action::Activate)
                .unwrap();
            harness
                .expect(
                    &SemanticExpectation::named("setup.folder.0.warnings").name_contains(
                        PathBuf::from(TEST_SOURCE_ROOT)
                            .join("Documents/private")
                            .display()
                            .to_string(),
                    ),
                )
                .unwrap();
            harness
                .expect(
                    &SemanticExpectation::named("setup.folder.0.warnings").name_contains(
                        PathBuf::from(TEST_SOURCE_ROOT)
                            .join("Documents/linked")
                            .display()
                            .to_string(),
                    ),
                )
                .unwrap();
        }

        // Phase `concurrent_same_basename_completions_receive_distinct_destinations`.
        {
            let mut app = supported_fixture();
            let destination = PathBuf::from(TEST_DEST_ROOT).join("Documents");
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(destination.clone()),
                result: Ok(scan("account-a/Documents", 12)),
            });
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(destination),
                result: Ok(scan("account-b/Documents", 8)),
            });

            let folders = app.controller();
            assert_eq!(folders.folders().len(), 2);
            assert_eq!(
                folders.folders()[0].destination,
                FolderDestination::Resolved(PathBuf::from(TEST_DEST_ROOT).join("Documents"))
            );
            assert_eq!(
                folders.folders()[1].destination,
                FolderDestination::Resolved(PathBuf::from(TEST_DEST_ROOT).join("Documents (2)"))
            );
        }

        // Phase `worker_launch_failures_clear_pending_state`.
        {
            let mut scan_app = SetupApp::with_worker_launcher(
                supported_fixture().capabilities,
                fail_worker_launch,
            );
            scan_app.start_scan(PathBuf::from(TEST_SOURCE_ROOT).join("Documents"));

            assert_eq!(scan_app.pending_scans, 0);
            assert!(scan_app.folder_selection_settled());
            assert!(!scan_app.needs_idle_sync());
            assert!(scan_app.status.contains("could not start"));

            let mut lookup_app = SetupApp::with_worker_launcher(
                supported_fixture().capabilities,
                fail_worker_launch,
            );
            lookup_app.start_known_folder_lookup();

            assert!(!lookup_app.known_folder_lookup_pending);
            assert!(!lookup_app.needs_idle_sync());
            assert!(lookup_app.status.contains("could not start"));
        }
    }

    fn fail_worker_launch(_: &str, _: WorkerTask) -> Result<(), String> {
        Err("test launcher refused the worker".into())
    }

    /// Protected, overlapping, system and home folders and mode feedback.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn folder_sources_are_validated_before_scanning_and_show_mode_feedback() {
        // Phase `protected_and_overlapping_sources_are_rejected_before_worker_launch`.
        {
            let mut protected = SetupApp::with_worker_launcher(
                supported_fixture().capabilities,
                fail_worker_launch,
            );
            protected.start_scan(PathBuf::from(TEST_SYSTEM_ROOT));

            assert_eq!(protected.pending_scans, 0);
            assert!(protected.status.contains("system folder"));

            let mut overlapping = SetupApp::with_worker_launcher(
                supported_fixture().capabilities,
                fail_worker_launch,
            );
            overlapping.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan("Documents", 12)),
            });
            overlapping.start_scan(PathBuf::from(TEST_SOURCE_ROOT).join("Documents/Projects"));

            assert_eq!(overlapping.pending_scans, 0);
            assert_eq!(overlapping.controller().folders().len(), 1);
            assert!(overlapping.status.contains("already covered"));
        }

        // Phase `overlapping_scan_is_deduplicated_with_an_explanation`.
        {
            let mut app = supported_fixture();
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan("Documents", 12)),
            });
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Projects")),
                result: Ok(scan("Documents/Projects", 8)),
            });

            assert_eq!(app.controller().folders().len(), 1);
            assert!(app.status.contains("already covered"));
        }

        // Phase `system_folder_scan_is_refused`.
        {
            let mut app = supported_fixture();
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("system")),
                result: Ok(FolderScan {
                    source: PathBuf::from(TEST_SYSTEM_ROOT),
                    file_count: 4,
                    bytes: 80,
                    empty_directory_count: 0,
                    empty_directories: Vec::new(),
                    skipped_count: 0,
                    skipped: Vec::new(),
                }),
            });

            assert!(app.controller().folders().is_empty());
            assert!(app.status.contains("system folder"));
        }

        // Phase `var_home_user_folder_is_accepted`.
        #[cfg(unix)]
        {
            let mut app = supported_fixture();
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(FolderScan {
                    source: PathBuf::from("/var/home/tilo/Documents"),
                    file_count: 4,
                    bytes: 80,
                    empty_directory_count: 0,
                    empty_directories: Vec::new(),
                    skipped_count: 0,
                    skipped: Vec::new(),
                }),
            });

            assert_eq!(app.controller().folders().len(), 1);
            assert!(app.status.contains("ready to copy"));
        }

        // Phase `inherited_and_overridden_folder_modes_have_visible_selection_feedback`.
        {
            let mut inherited = supported_fixture();
            inherited.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            inherited.apply_command(SetupCommand::Next);
            inherited.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan("Documents", 12)),
            });
            // A folder that follows the default offers no way back to it.
            let composition = inherited.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.folder.0.mode"),
                1
            );
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.folder.0.follow"),
                0
            );

            let mut overridden = supported_fixture();
            overridden.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            overridden.apply_command(SetupCommand::Next);
            overridden.apply_worker_message(WorkerMessage::ScanFinished {
                destination: Some(PathBuf::from(TEST_DEST_ROOT).join("Documents")),
                result: Ok(scan("Documents", 12)),
            });
            overridden.apply_command(SetupCommand::SetFolderMode(0, FolderMode::Connected));
            assert!(overridden.controller().folders()[0].mode_overridden);
            let observed = overridden.controller_handle();
            let mut harness =
                FrameHarness::new(ui::State::new(), Box::new(overridden), (1180, 820));
            harness
                .expect(&SemanticExpectation::named("setup.folder.0.follow").enabled(true))
                .unwrap();
            harness
                .perform_named("setup.folder.0.follow", &Action::Activate)
                .unwrap();
            assert!(!observed.borrow().folders()[0].mode_overridden);
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
        // Phase `initial_worker_loss_can_recheck_and_recover_navigation_in_the_same_app`.
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
            app.controller.borrow_mut().add_folder_unresolved(shot_scan(
                "Documents",
                38,
                2_460_000_000,
            ));
            assert!(
                app.controller
                    .borrow_mut()
                    .set_folder_mode(0, FolderMode::Later)
            );
            app.desktop_flow =
                DesktopFlow::without_start(super::super::desktop_flow::DesktopRuntime {
                    launch_probe: recoverable_desktop_probe,
                    launch_download: disconnected_desktop_download,
                    launch_install: disconnected_desktop_install,
                });
            app.desktop_flow.start_check().unwrap();
            assert!(app.poll_background());
            let observed = app.controller_handle();
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));

            harness
                .expect(
                    &SemanticExpectation::named("setup.desktop.live")
                        .value_contains("worker stopped"),
                )
                .unwrap();
            harness
                .expect(&SemanticExpectation::named("setup.desktop.recheck").enabled(true))
                .unwrap();
            harness
                .perform_named("setup.desktop.recheck", &Action::Activate)
                .unwrap();
            harness
                .expect(&SemanticExpectation::named("setup.experience.desktop").enabled(true))
                .unwrap();
            harness
                .perform_named("setup.experience.desktop", &Action::Activate)
                .unwrap();
            harness
                .perform_named("setup.next", &Action::Activate)
                .unwrap();

            assert_eq!(observed.borrow().page(), SetupPage::Folders);
            assert_eq!(observed.borrow().folders()[0].mode, FolderMode::Later);
            assert!(observed.borrow().folders()[0].mode_overridden);
            assert_eq!(
                observed.borrow().folders()[0].destination,
                FolderDestination::Unresolved
            );
            assert_eq!(RECOVERY_PROBES.load(Ordering::SeqCst), 2);
        }

        // Phase `desktop_review_shows_exact_host_version_and_pending_destination_before_authorization`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            app.apply_command(SetupCommand::Next);
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(scan("Documents", 4096)),
            });
            app.apply_command(SetupCommand::Next);
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
                .expect(
                    &SemanticExpectation::named("setup.review.pending")
                        .name_contains("shows where each folder goes"),
                )
                .unwrap();
            harness
                .expect(&SemanticExpectation::named("setup.install").enabled(true))
                .unwrap();
            let labels = harness
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_string))
                .collect::<Vec<_>>();
            let desktop = labels
                .iter()
                .position(|label| label == "Tilo Desktop")
                .unwrap();
            let folders = labels.iter().position(|label| label == "Folders").unwrap();
            assert!(desktop < folders);
        }

        // Phase `installed_review_leads_with_login_guidance_and_completion_status`.
        {
            let app = SetupApp::for_shot("setup-desktop-installed");
            let mut harness = FrameHarness::new(ui::State::new(), Box::new(app), (640, 640));

            assert!(
                harness
                    .a11y_node_named("Tilo Desktop is installed")
                    .is_some()
            );
            harness
                .expect(
                    &SemanticExpectation::named("setup.desktop.status")
                        .name_contains("choose Tilo on the login screen"),
                )
                .unwrap();
            let labels = harness
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_string))
                .collect::<Vec<_>>();
            assert!(
                labels
                    .iter()
                    .position(|label| label == "Tilo Desktop")
                    .unwrap()
                    < labels.iter().position(|label| label == "Folders").unwrap()
            );
        }

        // Phase `native_failure_and_retry_retain_folder_choice_and_mode`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::Desktop));
            app.apply_command(SetupCommand::Next);
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(scan("Documents", 4096)),
            });
            app.apply_command(SetupCommand::SetMode(FolderMode::Copy));
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
            assert_eq!(app.controller().folders().len(), 1);
            assert_eq!(app.controller().default_mode(), FolderMode::Copy);
        }

        // Phase `desktop_review_ignores_unselected_full_os_download_status`.
        {
            let mut app = SetupApp::for_shot("setup-review");
            app.desktop_revision = app.desktop_flow.revision();
            app.internal_download_revision = u64::MAX;
            let expected = app.status.clone();

            assert!(app.poll_background());
            assert_eq!(app.status, expected);
        }

        // Phase `full_os_review_ignores_unselected_desktop_install_status`.
        {
            let mut app = supported_fixture();
            app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
            app.apply_command(SetupCommand::Next);
            app.apply_command(SetupCommand::Next);
            assert_eq!(app.controller().page(), SetupPage::Review);
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
            assert!(harness.a11y_node_named("Review Tilo setup").is_some());
            assert!(
                harness
                    .a11y_node_named("Tilo Desktop is installed")
                    .is_none()
            );
        }
    }

    /// Embedded import routing and issue pages.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn embedded_import_routes_the_live_child_and_its_public_pages() {
        // Phase `opened_imports_replace_setup_in_the_published_tree`.
        {
            let mut app = launch(supported_fixture(), || {
                ImportApp::for_shot("import-first-login-later")
            });
            assert!(find_row(&app.harness().resources(), SETUP_ROW_ID).is_some());
            app.effect(&SetupEffect::OpenImports).unwrap();
            app.settle();

            let resources = app.harness().resources();
            assert!(find_row(&resources, SETUP_ROW_ID).is_none());
            let imports = find_row(&resources, "imager.import").expect("the imports row");
            assert!(
                app.effect(&SetupEffect::OpenImports).is_err(),
                "a retained setup effect must not reach the closed setup"
            );
            let folder = imports
                .children
                .iter()
                .find(|resource| resource.kind == "imager.import.folder")
                .unwrap()
                .clone();
            app.call_on(folder.id.as_str(), "set-folder-copy", serde_json::json!({}))
                .unwrap();

            let updated = app.harness().resources();
            let folder = find_row(&updated, folder.id.as_str()).unwrap();
            assert!(
                folder
                    .presentation
                    .as_ref()
                    .and_then(|presentation| presentation.detail.as_deref())
                    .is_some_and(|detail| detail.contains("Copy"))
            );
        }

        // Phase `opened_imports_publicly_observe_completed_issue_pages`.
        {
            let mut app = launch(supported_fixture(), || {
                ImportApp::for_shot("import-issue-pages")
            });
            app.effect(&SetupEffect::OpenImports).unwrap();
            app.settle();
            let read = |app: &mut TestApp<SetupFlow>| {
                app.effect(&ImportEffect::ReadStatus)
                    .unwrap()
                    .value
                    .unwrap()
            };
            let resources = app.harness().resources();
            let operation = find_row(&resources, "imager.import")
                .unwrap()
                .children
                .iter()
                .find(|resource| resource.kind == "imager.import.operation")
                .cloned()
                .unwrap_or_else(|| {
                    panic!("issue-page fixture has no operation: {}", read(&mut app))
                });
            app.call_on(operation.id.as_str(), "next-issues", serde_json::json!({}))
                .unwrap();

            let deadline = tilo_test_env::hang_deadline();
            let observed = loop {
                app.settle();
                let observed = read(&mut app);
                if observed["operations"][0]["issues"]
                    .as_array()
                    .is_some_and(|issues| issues.len() == 8)
                {
                    break observed;
                }
                assert!(
                    ui::time::now() < deadline,
                    "completed issue page was not publicly observable"
                );
                std::thread::yield_now();
            };
            assert_eq!(observed["operations"][0]["next_issues_available"], false);
            assert_eq!(read(&mut app), observed);
        }
    }

    thread_local! {
        static NEXT_SETUP: std::cell::RefCell<Option<SetupApp>> =
            const { std::cell::RefCell::new(None) };
        static NEXT_IMPORTS: std::cell::Cell<Option<fn() -> ImportApp>> =
            const { std::cell::Cell::new(None) };
    }

    /// The imager mounting guided setup as its view, over the setup and the
    /// imports [`launch`] hands it.
    struct SetupFlow;

    impl ResourceApp for SetupFlow {
        type Resource = Imager;
        const ID: &'static str = "so.tilo.imager";
        const TITLE: &'static str = "Disk Imager";

        fn view(cx: &mut ViewCx<Imager>) -> Element {
            mount(
                cx,
                |_| {
                    NEXT_SETUP
                        .with(|next| next.borrow_mut().take())
                        .expect("a setup to mount")
                },
                |_| {
                    NEXT_IMPORTS
                        .with(std::cell::Cell::get)
                        .expect("imports to mount")()
                },
            )
        }
    }

    /// `setup` in a live window of the imager; asking for imports shows
    /// `imports`.
    fn launch(setup: SetupApp, imports: fn() -> ImportApp) -> TestApp<SetupFlow> {
        NEXT_SETUP.with(|next| *next.borrow_mut() = Some(setup));
        NEXT_IMPORTS.with(|next| next.set(Some(imports)));
        TestApp::<SetupFlow>::new().unwrap()
    }

    fn find_row(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find_row(&resource.children, id)
            }
        })
    }

    /// The imager mounting guided setup, as a screenshot captures it.
    struct SetupShots;

    impl ResourceApp for SetupShots {
        type Resource = Imager;
        const ID: &'static str = "so.tilo.imager";
        const TITLE: &'static str = "Disk Imager";

        fn scenarios() -> &'static [&'static str] {
            SCENARIOS
        }

        fn view(cx: &mut ViewCx<Imager>) -> Element {
            view(cx)
        }
    }

    #[test]
    fn every_setup_scenario_stages_its_page_without_reading_this_computer() {
        for (scenario, badge) in [
            ("setup-experience-checking", "Experience"),
            ("setup-experience", "Experience"),
            ("setup-experience-failed", "Experience"),
            ("setup-folders-copy", "Folders"),
            ("setup-folders-connected", "Folders"),
            ("setup-folders-later", "Folders"),
            ("setup-review", "Review"),
            ("setup-desktop-installed", "Review"),
            ("setup-full-review", "Review"),
            ("setup-full-restart", "Review"),
        ] {
            let mut app = TestApp::<SetupShots>::capture(Some(scenario)).unwrap();
            let resources = app.harness().resources();
            let setup = find_row(&resources, SETUP_ROW_ID).unwrap_or_else(|| panic!("{scenario}"));
            assert_eq!(
                setup.presentation.as_ref().unwrap().badge.as_deref(),
                Some(badge),
                "{scenario}"
            );
            let inspection = app.harness().inspection_json().unwrap();
            if badge != "Experience" {
                assert!(inspection.contains("Pictures"), "{scenario}");
            }
            if scenario == "setup-desktop-installed" {
                assert!(inspection.contains("installed and verified"), "{scenario}");
            }
        }
    }

    /// Setup as the imager resource publishes it: the live row, carrying
    /// the setup effects as callers see them now.
    fn setup_resource(app: &SetupApp) -> tilo_actions::Resource {
        let mut root = app.live_rows().remove(0).into_resource();
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

    fn setup_action(app: &SetupApp, id: &str) -> tilo_actions::ActionSpec {
        setup_resource(app)
            .actions
            .into_iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("missing setup effect {id}"))
    }

    fn folder_action(app: &SetupApp, index: usize, id: &str) -> tilo_actions::ActionSpec {
        child_resource(&setup_resource(app), "imager.setup.selection").children[index]
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap()
            .clone()
    }

    fn setup_args(
        entries: &[(&str, serde_json::Value)],
    ) -> serde_json::Map<String, serde_json::Value> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect()
    }

    /// Invoke `action` with `arguments` as a caller does: the effect it
    /// encodes reaches the setup.
    fn invoke_spec(
        app: &mut SetupApp,
        action: &tilo_actions::ActionSpec,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let encoded = action
            .build_json(arguments)
            .map_err(|error| error.to_string())?;
        invoke_encoded_setup(app, &encoded)
    }

    fn invoke_setup(
        app: &mut SetupApp,
        id: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let action = setup_action(app, id);
        invoke_spec(app, &action, &arguments)
    }

    fn invoke_folder_setup(
        app: &mut SetupApp,
        index: usize,
        id: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let action = folder_action(app, index, id);
        invoke_spec(app, &action, &arguments)
    }

    fn invoke_setup_raw(
        app: &mut SetupApp,
        effect: SetupEffect,
    ) -> Result<serde_json::Value, String> {
        app.answer(effect)
    }

    fn child_resource<'a>(
        resource: &'a tilo_actions::Resource,
        kind: &str,
    ) -> &'a tilo_actions::Resource {
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
        app.answer(SetupEffect::from_payload(&encoded.payload)?)
    }

    /// Semantic operations, suggested and retained folder actions.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn semantic_setup_operations_validate_choices_identities_and_arguments() {
        // Phase `semantic_operations_choose_navigate_retain_and_invalidate_review`.
        {
            let mut app = supported_fixture();
            let selected = invoke_setup(
                &mut app,
                "choose-experience",
                setup_args(&[("experience", serde_json::json!("desktop"))]),
            )
            .unwrap();
            assert_eq!(selected["selection"]["experience"], "desktop");
            assert_eq!(
                invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap()["page"],
                "folders"
            );
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(scan("Documents", 4096)),
            });
            invoke_folder_setup(
                &mut app,
                0,
                "set-folder-mode",
                setup_args(&[("mode", serde_json::json!("later"))]),
            )
            .unwrap();
            let cleared =
                invoke_folder_setup(&mut app, 0, "use-common-mode", serde_json::Map::new())
                    .unwrap();
            assert_eq!(cleared["selection"]["folders"][0]["mode"], "copy");
            assert_eq!(cleared["selection"]["folders"][0]["overridden"], false);
            invoke_folder_setup(
                &mut app,
                0,
                "set-folder-mode",
                setup_args(&[("mode", serde_json::json!("later"))]),
            )
            .unwrap();
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(scan("Pictures", 2048)),
            });
            let removed =
                invoke_folder_setup(&mut app, 1, "remove-folder", serde_json::Map::new()).unwrap();
            assert_eq!(removed["selection"]["folder_count"], 1);
            let review = invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            assert_eq!(review["page"], "review");
            assert_eq!(review["review"]["ready"], true);
            assert_eq!(review["selection"]["folder_count"], 1);
            assert_eq!(review["selection"]["active_folder_count"], 0);
            assert!(
                invoke_setup_raw(
                    &mut app,
                    SetupEffect::SetCommonMode {
                        mode: "copy".into()
                    }
                )
                .is_err()
            );
            invoke_setup(&mut app, "go-back-in-setup", serde_json::Map::new()).unwrap();
            let edited = invoke_setup(
                &mut app,
                "set-common-mode",
                setup_args(&[("mode", serde_json::json!("copy"))]),
            )
            .unwrap();
            assert_eq!(edited["page"], "folders");
            assert_eq!(edited["review"]["ready"], false);
            assert_eq!(edited["selection"]["folders"][0]["mode"], "later");
            let back = invoke_setup(&mut app, "go-back-in-setup", serde_json::Map::new()).unwrap();
            assert_eq!(back["page"], "experience");
            assert_eq!(back["selection"]["experience"], "desktop");
            assert_eq!(back["selection"]["folder_count"], 1);
        }

        // Phase `semantic_operations_refuse_unavailable_choices_without_mutating_state`.
        {
            let mut app = SetupApp::with_capabilities(SetupCapabilities {
                platform: Capabilities {
                    host: HostOs::Linux,
                    desktop_session: true,
                    internal_dual_boot: false,
                },
                connected_folders: false,
            });
            let error = invoke_setup_raw(
                &mut app,
                SetupEffect::ChooseExperience {
                    experience: "full_os".into(),
                },
            )
            .unwrap_err();
            assert!(error.contains("not available"), "{error}");
            assert_eq!(app.controller().chosen_experience(), None);
            invoke_setup(
                &mut app,
                "choose-experience",
                setup_args(&[("experience", serde_json::json!("desktop"))]),
            )
            .unwrap();
            invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            let error = invoke_setup_raw(
                &mut app,
                SetupEffect::SetCommonMode {
                    mode: "connected".into(),
                },
            )
            .unwrap_err();
            assert!(error.contains("not available"), "{error}");
            assert_eq!(app.controller().default_mode(), FolderMode::Copy);
            assert_eq!(app.controller().page(), SetupPage::Folders);
        }

        // Phase `semantic_operations_refuse_stale_identities_and_malformed_arguments`.
        {
            let mut app = supported_fixture();
            invoke_setup(
                &mut app,
                "choose-experience",
                setup_args(&[("experience", serde_json::json!("desktop"))]),
            )
            .unwrap();
            invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            app.apply_worker_message(WorkerMessage::ScanFinished {
                destination: None,
                result: Ok(scan("Documents", 4096)),
            });
            let before = app.controller().folders().to_vec();
            for operation in [
                SetupEffect::SetFolderMode {
                    folder: "missing".into(),
                    mode: "copy".into(),
                },
                SetupEffect::UseCommonMode {
                    folder: "missing".into(),
                },
                SetupEffect::RemoveFolder {
                    folder: "missing".into(),
                },
            ] {
                assert!(
                    invoke_setup_raw(&mut app, operation)
                        .unwrap_err()
                        .contains("folder selection")
                );
                assert_eq!(app.controller().folders(), before.as_slice());
            }
            let set_mode = folder_action(&app, 0, "set-folder-mode");
            assert!(set_mode.build_json(&serde_json::Map::new()).is_err());
            assert!(
                set_mode
                    .build_json(&setup_args(&[("mode", serde_json::json!(7))]))
                    .is_err()
            );
            assert_eq!(app.controller().folders(), before.as_slice());
        }

        // Phase `suggested_folder_action_uses_live_choice_and_rechecks_availability`.
        {
            let mut app = supported_fixture();
            app.worker_launcher = accepted_without_completion_worker;
            invoke_setup(
                &mut app,
                "choose-experience",
                setup_args(&[("experience", serde_json::json!("desktop"))]),
            )
            .unwrap();
            invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            app.known_folders = vec![KnownFolder {
                kind: KnownFolderKind::Documents,
                path: Some(PathBuf::from(TEST_SOURCE_ROOT).join("Documents")),
                unavailable_reason: None,
            }];
            let root = setup_resource(&app);
            let suggestion = child_resource(&root, "imager.setup.suggestions")
                .children
                .iter()
                .find(|child| child.kind == "imager.setup.suggestion")
                .unwrap();
            assert!(
                suggestion
                    .actions
                    .iter()
                    .find(|action| action.id == "add-suggested-folder")
                    .unwrap()
                    .contract
                    .availability
                    .is_available()
            );
            let encoded = suggestion
                .actions
                .iter()
                .find(|action| action.id == "add-suggested-folder")
                .unwrap()
                .build_json(&serde_json::Map::new())
                .unwrap();
            invoke_encoded_setup(&mut app, &encoded).unwrap();
            assert_eq!(app.pending_scans, 1);
            assert!(invoke_encoded_setup(&mut app, &encoded).is_err());
        }

        // Phase `retained_folder_action_uses_identity_after_earlier_folder_is_removed`.
        {
            let mut app = supported_fixture();
            invoke_setup(
                &mut app,
                "choose-experience",
                setup_args(&[("experience", serde_json::json!("desktop"))]),
            )
            .unwrap();
            invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            for (name, bytes) in [("A", 1), ("B", 2), ("C", 3)] {
                app.apply_worker_message(WorkerMessage::ScanFinished {
                    destination: None,
                    result: Ok(scan(name, bytes)),
                });
            }

            let root = setup_resource(&app);
            let folders = &child_resource(&root, "imager.setup.selection").children;
            let remove_a = folders[0]
                .actions
                .iter()
                .find(|action| action.id == "remove-folder")
                .unwrap()
                .clone();
            let set_b = folders[1]
                .actions
                .iter()
                .find(|action| action.id == "set-folder-mode")
                .unwrap()
                .clone();
            let remove_a = match &remove_a.input {
                tilo_actions::ActionInput::Immediate(encoded) => encoded.as_ref().clone(),
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("remove must be immediate")
                }
            };
            let set_b = match &set_b.input {
                tilo_actions::ActionInput::Prompted { build, .. } => build("later"),
                tilo_actions::ActionInput::Immediate(_)
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("set-mode must prompt for one mode")
                }
            };
            assert_ne!(set_b.type_id, "tilo.invalid_arguments");

            invoke_encoded_setup(&mut app, &remove_a).unwrap();
            invoke_encoded_setup(&mut app, &set_b).unwrap();
            assert_eq!(app.controller().folders().len(), 2);
            assert_eq!(app.controller().folders()[0].scan.bytes, 2);
            assert_eq!(app.controller().folders()[0].mode, FolderMode::Later);
            assert_eq!(app.controller().folders()[1].scan.bytes, 3);
            assert_eq!(app.controller().folders()[1].mode, FolderMode::Copy);
        }
    }

    /// Setup actions and resource projection.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn setup_resource_follows_visible_pages_and_projects_folder_selection() {
        // Phase `setup_actions_follow_visible_pages_and_import_takeover`.
        {
            let mut app = supported_fixture();
            assert!(
                setup_action(&app, "choose-experience")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(
                setup_action(&app, "open-imports")
                    .contract
                    .availability
                    .is_available()
            );
            invoke_setup(
                &mut app,
                "choose-experience",
                setup_args(&[("experience", serde_json::json!("desktop"))]),
            )
            .unwrap();
            invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            assert!(
                !setup_action(&app, "choose-experience")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(
                !setup_action(&app, "open-imports")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(
                setup_action(&app, "choose-folder")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(
                invoke_setup_raw(
                    &mut app,
                    SetupEffect::ChooseExperience {
                        experience: "desktop".into()
                    }
                )
                .is_err()
            );
            invoke_setup(&mut app, "continue-setup", serde_json::Map::new()).unwrap();
            assert!(
                !setup_action(&app, "set-common-mode")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(
                !setup_action(&app, "choose-folder")
                    .contract
                    .availability
                    .is_available()
            );
            assert!(
                setup_action(&app, "open-imports")
                    .contract
                    .availability
                    .is_available()
            );
            let opened = invoke_setup(&mut app, "open-imports", serde_json::Map::new()).unwrap();
            assert_eq!(opened["selection"]["imports_open"], true);
            assert!(
                invoke_setup_raw(&mut app, SetupEffect::ContinueSetup).is_err(),
                "setup is closed once imports are asked for"
            );
        }

        // Phase `setup_resource_projects_availability_help_and_import_navigation`.
        {
            let app = SetupApp::with_capabilities(SetupCapabilities {
                platform: Capabilities {
                    host: HostOs::Linux,
                    desktop_session: true,
                    internal_dual_boot: false,
                },
                connected_folders: false,
            });
            let mut window = launch(app, || ImportApp::for_shot("import-complete"));
            let root_action = |window: &mut TestApp<SetupFlow>, id: &str| {
                find_row(&window.harness().resources(), Imager::KIND)
                    .expect("the imager resource")
                    .actions
                    .into_iter()
                    .find(|action| action.id == id)
                    .unwrap_or_else(|| panic!("missing {id}"))
            };
            let resource = find_row(&window.harness().resources(), SETUP_ROW_ID).unwrap();
            assert_eq!(resource.kind, "imager.setup");
            assert_eq!(
                resource.presentation.as_ref().unwrap().badge.as_deref(),
                Some("Experience")
            );
            let detail = resource
                .presentation
                .as_ref()
                .unwrap()
                .detail
                .as_deref()
                .unwrap();
            assert!(detail.contains("Experience page"), "{detail}");
            let status = window
                .effect(&SetupEffect::ReadSetupStatus)
                .unwrap()
                .value
                .unwrap();
            assert_eq!(
                status["availability"]["full_os"],
                serde_json::json!(false),
                "the published setup status refuses Full Tilo"
            );
            assert_eq!(
                status["availability"]["connected"],
                serde_json::json!(false),
                "the published setup status refuses connected folders"
            );
            for id in ["choose-experience", "open-imports", "read-setup-status"] {
                let action = root_action(&mut window, id);
                assert_eq!(
                    action.operation_type.as_deref(),
                    Some(SetupEffect::OPERATION)
                );
                assert!(action.contract.availability.is_available(), "{id}");
            }
            assert!(
                !root_action(&mut window, "continue-setup")
                    .contract
                    .availability
                    .is_available(),
                "the published effects follow the setup's state"
            );
            assert!(
                window
                    .effect(&SetupEffect::ChooseExperience {
                        experience: "full_os".into()
                    })
                    .is_err(),
                "an unavailable experience is refused"
            );
            let chosen = window
                .effect(&SetupEffect::ChooseExperience {
                    experience: "desktop".into(),
                })
                .unwrap()
                .value
                .unwrap();
            assert_eq!(chosen["availability"]["desktop"], true);
            assert_eq!(chosen["availability"]["full_os"], false);
            assert_eq!(chosen["availability"]["connected"], false);
            assert_eq!(chosen["availability"]["next"], true);
            window.effect(&SetupEffect::ContinueSetup).unwrap();
            let folders = window
                .effect(&SetupEffect::ToggleHelp)
                .unwrap()
                .value
                .unwrap();
            assert_eq!(folders["selection"]["help_open"], true);
            assert_eq!(folders["page"], "folders");
            let resources = window.harness().resources();
            let selection = find_row(&resources, "imager.setup.selection").unwrap();
            let detail = selection
                .presentation
                .as_ref()
                .unwrap()
                .detail
                .as_deref()
                .unwrap();
            assert!(detail.contains("help open"), "{detail}");
            assert!(detail.contains("imports closed"), "{detail}");
            assert!(
                !root_action(&mut window, "open-imports")
                    .contract
                    .availability
                    .is_available()
            );
            window.effect(&SetupEffect::ContinueSetup).unwrap();
            let opened = window
                .effect(&SetupEffect::OpenImports)
                .unwrap()
                .value
                .unwrap();
            assert_eq!(opened["selection"]["imports_open"], true);
            window.settle();

            let resources = window.harness().resources();
            assert!(find_row(&resources, SETUP_ROW_ID).is_none());
            assert_eq!(
                find_row(&resources, "imager.import").unwrap().kind,
                "imager.import"
            );
            assert_eq!(
                window
                    .effect(&ImportEffect::ReadStatus)
                    .unwrap()
                    .value
                    .unwrap()["operations"][0]["state"],
                "complete"
            );
            assert!(
                window.effect(&SetupEffect::ToggleHelp).is_err(),
                "a retained setup effect must not reach the closed setup"
            );
        }

        // Phase `setup_resource_browses_complete_path_free_folder_selection`.
        {
            let mut app = supported_fixture();
            for (name, bytes) in [("Documents", 4096), ("Pictures", 2048)] {
                app.apply_worker_message(WorkerMessage::ScanFinished {
                    destination: None,
                    result: Ok(scan(name, bytes)),
                });
            }
            app.controller
                .borrow_mut()
                .set_folder_mode(1, FolderMode::Later);

            let root = setup_resource(&app);
            let selection = child_resource(&root, "imager.setup.selection");
            let detail = selection
                .presentation
                .as_ref()
                .unwrap()
                .detail
                .as_deref()
                .unwrap();
            assert_eq!(
                detail,
                "no experience chosen yet, copy by default, 2 folders, 1 to bring over. With help closed and imports closed."
            );
            assert_eq!(selection.children.len(), 2);
            assert_eq!(selection.children[0].kind, "imager.setup.folder");
            assert_eq!(selection.children[1].kind, "imager.setup.folder");
            assert!(
                selection.children[0]
                    .presentation
                    .as_ref()
                    .unwrap()
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("Copy, the common choice")
            );
            assert!(
                selection.children[1]
                    .presentation
                    .as_ref()
                    .unwrap()
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("Decide later, chosen for this folder")
            );
            for folder in &selection.children {
                assert!(!folder.label.contains("Documents"));
                assert!(!folder.label.contains("Pictures"));
                assert_eq!(folder.actions.len(), 3);
                assert!(folder.actions.iter().all(|action| {
                    action.operation_type.as_deref() == Some(SetupEffect::OPERATION)
                }));
                assert_eq!(
                    folder
                        .actions
                        .iter()
                        .find(|action| action.id == "set-folder-mode")
                        .unwrap()
                        .params()
                        .len(),
                    1
                );
            }
            let review = child_resource(&root, "imager.setup.review");
            assert!(
                review
                    .presentation
                    .as_ref()
                    .unwrap()
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("Review ready: No.")
            );
            let availability = child_resource(&root, "imager.setup.availability");
            assert!(
                availability
                    .presentation
                    .as_ref()
                    .unwrap()
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("Available experiences: desktop Yes, full OS Yes.")
            );
        }
    }

    fn retained_widget_count(root: &dyn Widget, id: &str) -> usize {
        usize::from(root.test_id() == Some(id))
            + root
                .children()
                .into_iter()
                .map(|child| retained_widget_count(child, id))
                .sum::<usize>()
    }

    fn retained_value(root: &dyn Widget, id: &str) -> Option<String> {
        ui::widget::find_test_id(root, id)?.accessibility()?.value
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
        app.apply_command(SetupCommand::ChooseExperience(Experience::FullOs));
        app.apply_command(SetupCommand::Next);
        app.apply_command(SetupCommand::SetMode(FolderMode::Later));
        app.apply_command(SetupCommand::Next);
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
            let composition = app.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.experience.full"),
                0,
                "Full Tilo is not offered until the probe passes"
            );
            assert!(
                retained_value(composition.as_ref(), "setup.internal.readiness")
                    .unwrap()
                    .starts_with("Checking whether Tilo can be installed")
            );
            app.start_host_probe();
            assert!(app.poll_background());
            assert!(app.capabilities.platform.supports(Experience::FullOs));
            let composition = app.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.experience.full"),
                1,
                "Full Tilo is offered once the probe passes"
            );
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
            let composition = blocked.app.build_composition();
            assert!(
                retained_value(composition.as_ref(), "setup.internal.readiness")
                    .unwrap()
                    .contains("MBR")
            );
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
                assert_eq!(launches[0].plan_sha256, staged.digest());
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

        // Phase `full_os_shot_fixtures_pose_a_plan_blocker_and_restart`.
        {
            let review = SetupApp::for_shot("setup-full-review");
            assert!(review.on_full_os_review());
            let composition = review.build_composition();
            assert_eq!(
                retained_widget_count(composition.as_ref(), "setup.internal.firmware-settings"),
                1
            );
            assert!(retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.install"
            ));
            assert!(
                retained_name(composition.as_ref(), "setup.internal.plan")
                    .unwrap()
                    .contains("Windows (C:) shrinks by")
            );
            let staged = SetupApp::for_shot("setup-full-restart");
            let composition = staged.build_composition();
            assert!(!retained_widget_disabled(
                composition.as_ref(),
                "setup.internal.restart"
            ));
        }
    }
}
