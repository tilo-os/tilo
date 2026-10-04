//! Windows Apps: the installed Windows apps beside the selected app, and the
//! managed Windows appliance.
//!
//! The window is a retained [`NavShell`]: the overview lists the apps and the
//! appliance, the content shows the selection. Controls queue an [`Intent`];
//! the app applies intents against the selected model in one ordered place
//! and swaps the columns when what they show changes.
use crate::actions;
use crate::appliance_owner::{
    ApplianceOwnerClient, AppliancePowerState, ApplianceStatus, EgressPurpose, LifecycleAction,
};
use crate::model::{AppStatus, ApplianceState, Catalogue, Route, VmInstallConfirmation};
use crate::provider::{CompatibilityProvider, EmptyProvider, LiveProvider, SampleProvider};
use crate::strings;
use automerge::{AutoCommit, ObjId};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::rc::Rc;
use ui::a11y::Action as AccessibilityAction;
use ui::icons::Icon;
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::SpacingStep;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    AppModel, Banner, Button, ButtonRole, Column, Density, Element, Form, FormSection, ListRow,
    ModalDialog, ModelPath, NavPane, NavShell, Property, PropertyList, Row, Scrollable, Section,
    SectionHeader, Spacer, Text, ToggleGroup, Widget,
};
use ui::{TextKind, TextRole};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

fn installer_pick_request() -> PickRequest {
    PickRequest::open("Add a Windows application", "Install")
        .thread_name("tilo-windows-installer-picker")
        .filter(
            FileFilter::new("Windows applications")
                .glob("*.exe")
                .glob("*.msi")
                .mimetype("application/vnd.microsoft.portable-executable")
                .mimetype("application/x-msi"),
        )
        .all_files()
}

fn windows_iso_pick_request() -> PickRequest {
    PickRequest::open("Choose Windows installation media", "Use this ISO")
        .thread_name("tilo-windows-iso-picker")
        .filter(
            FileFilter::new("Windows installation media")
                .glob("*.iso")
                .mimetype("application/x-cd-image")
                .mimetype("application/x-iso9660-image"),
        )
        .all_files()
}

fn windows_document_pick_request() -> PickRequest {
    PickRequest::open("Open a file in Windows", "Open read-only")
        .thread_name("tilo-windows-document-picker")
        .all_files()
}

/// What the content column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    /// The selected catalogue app (or the empty catalogue).
    App,
    /// The managed Windows appliance.
    Appliance,
    /// An inspected installer waiting for its destination.
    PendingInstall,
}

/// A request a control in the view made. Controls only queue intents; the
/// app applies them against the current selection in [`WindowsApp::apply_intents`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    SelectApp(usize),
    SelectAppliance,
    SelectPending,
    Route(Route),
    ResetRoute,
    Launch,
    OpenFile,
    VmFallback,
    Install,
    ConfirmCapsule,
    ConfirmVm,
    Publish,
    Retry,
    Escalate,
    RemoveLauncher,
    Uninstall,
    DeleteData,
    ConfirmDelete,
    CancelDelete,
    CancelConfirmation,
    ChooseIso,
    Recovery,
    Lifecycle(LifecycleAction),
    ConnectActivation,
    ConnectUpdate,
    RevokeConnection,
    ToggleApplianceDetails,
}

type Intents = Rc<RefCell<Vec<Intent>>>;

/// The one status line the window shows above its content.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice {
    text: String,
    error: bool,
}

impl Notice {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

/// A step waiting for the person's explicit confirmation in a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Delete,
    Escalate,
    Uninstall { vm: bool },
}

/// Everything the overview column shows; it is rebuilt when this changes.
#[derive(Clone, Debug, PartialEq)]
struct OverviewKey {
    apps: Vec<(String, String, AppStatus, Route)>,
    appliance: (String, bool),
    pending: Option<String>,
    page: Page,
    selected: usize,
    pushed: bool,
}

/// Everything the content column shows; it is rebuilt when this changes.
#[derive(Clone, Debug, PartialEq)]
struct ContentKey {
    page: Page,
    app: Option<crate::model::WindowsApp>,
    vm: ApplianceState,
    pending: Option<VmInstallConfirmation>,
    appliance: ApplianceStatus,
    appliance_checked: bool,
    lease: Option<String>,
    busy: bool,
    notice: Option<Notice>,
    details: bool,
}

fn queue(intents: &Intents, intent: Intent) -> impl FnMut() + 'static {
    let intents = Rc::clone(intents);
    move || intents.borrow_mut().push(intent)
}

fn action_button(
    id: &str,
    label_key: &str,
    role: ButtonRole,
    intents: &Intents,
    intent: Intent,
) -> Button {
    Button::new(id, &strings::t(label_key), None)
        .role(role)
        .on_effect(queue(intents, intent))
}

fn body(text: &str) -> Text {
    Text::new(text).with_role(TextRole::Body)
}

fn secondary(text: &str) -> Text {
    Text::new(text).with_role(TextRole::Body).dimmed()
}

fn caption(text: &str) -> Text {
    Text::new(text).with_role(TextRole::Caption).dimmed()
}

/// A titled group of a page, a group gap from the next.
fn section(title_key: &str, items: Vec<Element>) -> FormSection {
    items.into_iter().fold(
        FormSection::new(strings::t(title_key)),
        FormSection::content,
    )
}

/// A content page: a head block under the column title, then its sections.
#[derive(Default)]
struct PageParts {
    head: Vec<Element>,
    sections: Vec<FormSection>,
}

fn row(children: Vec<Element>) -> Row {
    children.into_iter().fold(Row::new(), Row::push)
}

fn appliance_summary(status: &ApplianceStatus, checked: bool) -> String {
    if !checked {
        strings::t("windows-appliance-checking-short")
    } else if status.is_provisioned() {
        status.summary_label().to_owned()
    } else {
        strings::t("windows-appliance-not-set-up")
    }
}

pub struct WindowsApp {
    provider: Box<dyn CompatibilityProvider>,
    pub(crate) catalogue: Catalogue,
    actions: crate::resource::ActionSink,
    selected: usize,
    page: Page,
    notice: Option<Notice>,
    intents: Intents,
    shell: NavShell,
    dialog: Option<ModalDialog<Text>>,
    dialog_for: Option<Confirm>,
    overview_key: Option<OverviewKey>,
    content_key: Option<ContentKey>,
    /// The content column should come to the front of a narrow window's
    /// push stack at the next rebuild.
    push_content: bool,
    appliance_details: bool,
    cursor: Point,
    overview_scroll: Rc<Cell<(f32, f32)>>,
    content_scroll: Rc<Cell<(f32, f32)>>,
    portal_rx: Option<OpenFileChannel>,
    document_portal_rx: Option<OpenFileChannel>,
    document_target: Option<String>,
    iso_portal_rx: Option<OpenFileChannel>,
    appliance_owner: ApplianceOwnerClient,
    confirm_escalate_to_vm: Option<String>,
    confirm_vendor_uninstall: Option<String>,
    confirm_delete_data: Option<String>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_waits_for_appliance: bool,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    pending_control_invocation: Option<tilo_actions::ControlInvocation>,
    projected_route_policy: HashMap<String, Option<Route>>,
    /// The apps as platform search finds them.
    search_index: crate::search::AppsSearch,
}

impl WindowsApp {
    pub fn new_live() -> Self {
        let provider: Box<dyn CompatibilityProvider> = match LiveProvider::connect() {
            Ok(provider) => Box::new(provider),
            Err(detail) => Box::new(EmptyProvider::with_detail(detail)),
        };
        Self::with_parts(provider, ApplianceOwnerClient::new())
    }
    pub fn new_live_open(path: &Path) -> Self {
        let mut app = Self::new_live();
        app.queue_import_path(path);
        app
    }
    pub fn new_offline() -> Self {
        Self::with_provider(Box::new(SampleProvider::new()))
    }
    /// Deterministic visual states for responsive review and release evidence.
    /// These never connect to the compatibility service or the appliance.
    pub fn new_offline_scenario(scenario: &str) -> Self {
        let mut app = Self::new_offline();
        app.stage_scenario(Some(scenario));
        app
    }

    /// The scenarios a capture can stage (`--shots --scenario NAME`).
    pub const SCENARIOS: &'static [&'static str] = &[
        "wide",
        "detail",
        "vm-detail",
        "running",
        "error",
        "error-compact",
        "confirm-delete",
        "appliance",
        "appliance-setup",
        "pending-install",
    ];

    /// The window size a scenario is captured at, when it is not the
    /// default.
    #[must_use]
    pub fn scenario_viewport(scenario: &str) -> Option<(u32, u32)> {
        match scenario {
            "detail" | "error-compact" | "vm-detail" | "confirm-delete" => Some((480, 800)),
            "running" | "error" | "pending-install" => Some((1000, 760)),
            _ => None,
        }
    }

    /// Replace the service and the appliance with the offline catalogue and
    /// show `scenario` (`None` for the plain capture). Captures only: a live
    /// window keeps its service.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let scenario = scenario.unwrap_or("wide");
        let mut provider = SampleProvider::new();
        if matches!(scenario, "error" | "error-compact") {
            provider.seed_setup_failure();
        }
        if scenario == "pending-install" {
            provider.seed_install_confirmation();
        }
        self.provider = Box::new(provider);
        self.appliance_owner = ApplianceOwnerClient::offline(if scenario == "appliance-setup" {
            ApplianceStatus::unavailable(
                "The Windows appliance command failed: command process failed: \
                 No such file or directory (os error 2)",
            )
        } else {
            ApplianceStatus::sample_ready()
        });
        self.catalogue = self.provider.catalogue();
        self.search_index.sync(&self.catalogue);
        self.projected_route_policy = self
            .catalogue
            .apps
            .iter()
            .map(|app| (app.id.clone(), None))
            .collect();
        self.select(Page::App, 0);
        self.push_content = false;
        match scenario {
            "detail" | "error-compact" => self.push_content = true,
            "running" => self.selected = 1,
            "vm-detail" => {
                self.selected = 2;
                self.push_content = true;
            }
            "appliance" | "appliance-setup" => {
                self.page = Page::Appliance;
                self.push_content = true;
            }
            "pending-install" => self.page = Page::PendingInstall,
            "confirm-delete" => {
                self.push_content = true;
                self.confirm_delete_data = self
                    .catalogue
                    .apps
                    .first()
                    .map(|first| first.capsule.clone());
            }
            _ => {}
        }
        self.rebuild();
    }

    pub fn with_provider(provider: Box<dyn CompatibilityProvider>) -> Self {
        Self::with_parts(
            provider,
            ApplianceOwnerClient::offline(ApplianceStatus::sample_ready()),
        )
    }

    fn with_parts(
        mut provider: Box<dyn CompatibilityProvider>,
        appliance_owner: ApplianceOwnerClient,
    ) -> Self {
        let catalogue = provider.catalogue();
        let search_index = crate::search::AppsSearch::new(&catalogue);
        let intents: Intents = Rc::default();
        let projected_route_policy = catalogue
            .apps
            .iter()
            .map(|app| (app.id.clone(), None))
            .collect();
        let install = Button::icon_only(
            "windows-install",
            Icon::Plus,
            &strings::t("windows-install"),
        )
        .on_effect(queue(&intents, Intent::Install));
        let shell = NavShell::new()
            .with_id("windows-shell")
            .overview_header(strings::t("windows-apps-title"), vec![Box::new(install)])
            .content_header(strings::t("windows-app-title"), Vec::new());
        let mut app = Self {
            provider,
            catalogue,
            actions: crate::resource::ActionSink::default(),
            selected: 0,
            page: Page::App,
            notice: None,
            intents,
            shell,
            dialog: None,
            dialog_for: None,
            overview_key: None,
            content_key: None,
            push_content: false,
            appliance_details: false,
            cursor: Point::ZERO,
            overview_scroll: Rc::default(),
            content_scroll: Rc::default(),
            portal_rx: None,
            document_portal_rx: None,
            document_target: None,
            iso_portal_rx: None,
            appliance_owner,
            confirm_escalate_to_vm: None,
            confirm_vendor_uninstall: None,
            confirm_delete_data: None,
            resource_invocation: None,
            resource_waits_for_appliance: false,
            resource_result: None,
            pending_control_invocation: None,
            projected_route_policy,
            search_index,
        };
        app.rebuild();
        app
    }

    fn queue_live_control(&mut self, resource: impl Into<String>, action: &str) -> bool {
        if !self.provider.action_native_controls() {
            return false;
        }
        self.pending_control_invocation = Some(tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new(resource, action),
            arguments: serde_json::Map::new(),
        });
        self.notice = Some(Notice::info(strings::t("windows-request-sent")));
        true
    }

    /// Turn a control's intent into the same inert address search and
    /// agents use. The application host re-resolves it against the fresh
    /// tree, applies permission and confirmation policy, records Effects in
    /// history, and only then calls `start_operation`.
    fn queue_live_control_intent(&mut self, intent: Intent) -> bool {
        if !self.provider.action_native_controls() {
            return false;
        }
        let appliance = match intent {
            Intent::ChooseIso => Some("import-iso"),
            Intent::Recovery => Some("recovery"),
            Intent::Lifecycle(LifecycleAction::Start) => Some("start"),
            Intent::Lifecycle(LifecycleAction::Restart) => Some("restart"),
            Intent::Lifecycle(LifecycleAction::Suspend) => Some("suspend"),
            Intent::Lifecycle(LifecycleAction::Resume) => Some("resume"),
            Intent::Lifecycle(LifecycleAction::Shutdown) => Some("shutdown"),
            Intent::ConnectActivation => Some("temporary-network"),
            Intent::ConnectUpdate => Some("update-network"),
            Intent::RevokeConnection => Some("revoke-network"),
            _ => None,
        };
        if let Some(action) = appliance {
            return self.queue_live_control("windows.appliance", action);
        }
        match intent {
            Intent::Install => return self.queue_live_control("windows.manager", "install"),
            Intent::ConfirmCapsule => {
                return self
                    .queue_live_control("windows.pending-install", "install-private-capsule");
            }
            Intent::ConfirmVm => {
                return self
                    .queue_live_control("windows.pending-install", "install-windows-appliance");
            }
            _ => {}
        }
        let Some(app) = self.selected_app() else {
            return false;
        };
        let resource = format!("windows.app:{}", app.id);
        let action = match intent {
            Intent::Route(route) => format!("route-{}", route.short().to_ascii_lowercase()),
            Intent::ResetRoute => "reset-route".into(),
            Intent::OpenFile => "open-file".into(),
            Intent::Launch => "launch".into(),
            Intent::VmFallback => "launch-vm-fallback".into(),
            Intent::Publish => "publish".into(),
            Intent::Retry => "retry-install".into(),
            Intent::Escalate => "escalate-install".into(),
            Intent::RemoveLauncher => "remove-launcher".into(),
            Intent::Uninstall => "uninstall".into(),
            Intent::DeleteData => "delete-data".into(),
            _ => return false,
        };
        self.queue_live_control(resource, &action)
    }
    fn selected_app(&self) -> Option<&crate::model::WindowsApp> {
        self.catalogue.apps.get(self.selected)
    }
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Windows>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Adopt collaborative route overrides; never clear the local catalogue.
    ///
    /// An empty [`crate::resource::WindowsState`] means "no published
    /// overrides yet" — keep whatever the provider already projected so a
    /// local set-route that has not reached the document is not wiped.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::WindowsState) {
        if state.routes.is_empty() {
            return;
        }
        for (app_id, route) in &state.routes {
            let desired = Some(*route);
            if self.projected_route_policy.get(app_id) == Some(&desired) {
                continue;
            }
            match self.provider.set_route_override(app_id, *route) {
                Ok(()) => {
                    self.projected_route_policy.insert(app_id.clone(), desired);
                }
                Err(error) => self.notice = Some(Notice::error(error)),
            }
        }
        self.refresh();
        self.rebuild();
    }

    pub(crate) fn commit_route(&mut self, doc: &mut AutoCommit, action: &actions::WindowsAction) {
        self.actions.dispatch(doc, action);
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`]
    /// with live catalogue children overlaid.
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Windows::read(doc).unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Windows>(
            actions::MANAGER_RESOURCE_ID,
            "Windows manager",
            &state,
        );
        // Live catalogue rows carry launch and maintenance. Document route
        // overrides stay on the published children and are merged onto the
        // matching app so a shared route is not dropped.
        let published = std::mem::take(&mut root.children);
        let mut children = actions::live_catalogue_extras(&self.catalogue);
        for document in published {
            if document.id.as_str() == "windows.appliance" {
                continue;
            }
            if let Some(existing) = children.iter_mut().find(|child| child.id == document.id) {
                for action in document.actions {
                    if existing.actions.iter().any(|have| have.id == action.id) {
                        continue;
                    }
                    existing.actions.push(action);
                }
            } else {
                children.push(document);
            }
        }
        root.children = children;
        vec![root]
    }

    pub(crate) fn project_shared_route_policy(&mut self, state: &AutoCommit) {
        let ids = self
            .catalogue
            .apps
            .iter()
            .map(|app| app.id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let desired = actions::route_override(state, &id);
            if self.projected_route_policy.get(&id) == Some(&desired) {
                continue;
            }
            let result = match desired {
                Some(route) => self.provider.set_route_override(&id, route),
                None => self.provider.reset_route_override(&id),
            };
            match result {
                Ok(()) => {
                    self.projected_route_policy.insert(id, desired);
                }
                Err(error) => self.notice = Some(Notice::error(error)),
            }
        }
    }
    fn finish_resource_operation(&mut self, result: Result<(), String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.resource_result = Some(match result {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "windows.operation_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    fn cancel_resource_operation(&mut self, message: &str) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: Some(message.into()),
        });
    }

    /// Show the outcome of a request: its error, or `done` when it was
    /// accepted.
    fn report(&mut self, result: Result<(), String>, done: &str) {
        self.notice = Some(match result {
            Ok(()) => Notice::info(strings::t(done)),
            Err(error) => Notice::error(error),
        });
    }

    fn cancel_delete_confirmation(&mut self) {
        self.confirm_delete_data = None;
        self.notice = None;
    }

    fn confirm_delete_data(&mut self) {
        let Some(capsule) = self.confirm_delete_data.take() else {
            return;
        };
        let result = self.provider.delete_data(&capsule);
        self.report(result, "windows-data-deleted");
        self.refresh();
    }

    fn queue_import_path(&mut self, path: &Path) {
        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if source_name.is_empty() {
            self.notice = Some(Notice::error(strings::t("windows-invalid-installer-name")));
            return;
        }
        match File::open(path) {
            Ok(source) => {
                let result = self.provider.install(source, source_name);
                self.report(result, "windows-installing");
            }
            Err(error) => {
                self.notice = Some(Notice::error(format!(
                    "Could not open the selected installer: {error}"
                )));
            }
        }
        self.refresh();
    }
    /// Queue an installer from the descriptor the portal worker opened. The
    /// portal path is intentionally not available here, so this UI cannot
    /// reopen a name after the user has approved it.
    fn queue_import_file(&mut self, source: File, source_name: &str) {
        if source_name.is_empty() {
            self.notice = Some(Notice::error(strings::t("windows-invalid-installer-name")));
            return;
        }
        let result = self.provider.install(source, source_name);
        self.report(result, "windows-installing");
        self.refresh();
    }
    fn picker_failed(&mut self, error: &str, what: &str) {
        self.finish_resource_operation(Err(error.to_owned()));
        self.notice = Some(Notice::error(format!("Could not open the {what}: {error}")));
    }
    fn picker_disconnected(&mut self) {
        self.finish_resource_operation(Err(strings::t("windows-picker-disconnected")));
        self.notice = Some(Notice::error(strings::t("windows-picker-disconnected")));
    }
    fn drain_portal(&mut self) {
        let Some(receiver) = self.portal_rx.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(OpenFileOutcome::Opened(source)) => {
                self.portal_rx = None;
                let source_name = source.display_name().to_string();
                self.queue_import_file(source.into_file(), &source_name);
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.portal_rx = None;
                self.cancel_resource_operation("installer selection was cancelled");
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.portal_rx = None;
                self.picker_failed(&error, "file picker");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                self.picker_disconnected();
            }
        }
    }
    fn drain_iso_portal(&mut self) {
        let Some(receiver) = self.iso_portal_rx.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(OpenFileOutcome::Opened(source)) => {
                self.iso_portal_rx = None;
                let result = self.appliance_owner.import_iso(source.into_file());
                self.report(result, "windows-iso-importing");
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.iso_portal_rx = None;
                self.cancel_resource_operation("Windows ISO selection was cancelled");
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.iso_portal_rx = None;
                self.picker_failed(&error, "Windows ISO picker");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.iso_portal_rx = None;
                self.picker_disconnected();
            }
        }
    }

    fn drain_document_portal(&mut self) {
        let Some(receiver) = self.document_portal_rx.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(OpenFileOutcome::Opened(source)) => {
                self.document_portal_rx = None;
                let target = self.document_target.take();
                let result = target
                    .ok_or_else(|| "The Windows application selection expired.".to_string())
                    .and_then(|entry_id| {
                        self.provider
                            .launch_vm_with_file(&entry_id, source.into_file())
                    });
                self.report(result, "windows-file-launch-queued");
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.document_portal_rx = None;
                self.document_target = None;
                self.cancel_resource_operation("document selection was cancelled");
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.document_portal_rx = None;
                self.document_target = None;
                self.picker_failed(&error, "file picker");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.document_portal_rx = None;
                self.document_target = None;
                self.picker_disconnected();
            }
        }
    }
    pub(crate) fn refresh(&mut self) {
        if let Some(notice) = self.appliance_owner.drain() {
            self.notice = Some(Notice::info(notice));
        }
        if self.resource_invocation.is_some()
            && self.resource_waits_for_appliance
            && let Some(result) = self.appliance_owner.take_action_result()
        {
            self.finish_resource_operation(result.map(|_| ()));
        }
        let had_pending = self.catalogue.vm_install_confirmation.is_some();
        let catalogue = self.provider.catalogue();
        if catalogue.apps != self.catalogue.apps {
            self.search_index.sync(&catalogue);
        }
        self.catalogue = catalogue;
        if let Some(notice) = self.provider.take_notice() {
            self.notice = Some(Notice::info(notice));
        }
        if self.resource_invocation.is_some()
            && !self.resource_waits_for_appliance
            && let Some(result) = self.provider.take_operation_result()
        {
            self.finish_resource_operation(result.map(|_| ()));
        }
        if self.selected >= self.catalogue.apps.len() {
            self.selected = 0;
        }
        match (
            had_pending,
            self.catalogue.vm_install_confirmation.is_some(),
        ) {
            // A freshly inspected installer asks where it goes: show it.
            (false, true) => self.select(Page::PendingInstall, self.selected),
            (_, false) if self.page == Page::PendingInstall => self.page = Page::App,
            _ => {}
        }
    }

    /// Show `page` (and, for an app, the app at `index`). Pending
    /// confirmations and the last notice belong to the previous selection.
    fn select(&mut self, page: Page, index: usize) {
        self.page = page;
        self.selected = index;
        self.notice = None;
        self.confirm_escalate_to_vm = None;
        self.confirm_vendor_uninstall = None;
        self.confirm_delete_data = None;
        self.appliance_details = false;
        self.content_scroll.set((0.0, 0.0));
        self.push_content = true;
    }

    /// Apply every intent the controls queued since the last call, in order.
    pub(crate) fn apply_intents(&mut self, doc: &mut AutoCommit) {
        let pending = std::mem::take(&mut *self.intents.borrow_mut());
        for intent in pending {
            self.apply_intent(intent, doc);
        }
    }

    fn apply_intent(&mut self, intent: Intent, doc: &mut AutoCommit) {
        match intent {
            Intent::SelectApp(index) if index < self.catalogue.apps.len() => {
                self.select(Page::App, index);
            }
            Intent::SelectAppliance => self.select(Page::Appliance, self.selected),
            Intent::SelectPending if self.catalogue.vm_install_confirmation.is_some() => {
                self.select(Page::PendingInstall, self.selected);
            }
            Intent::SelectApp(_) | Intent::SelectPending => {}
            Intent::ToggleApplianceDetails => self.appliance_details = !self.appliance_details,
            Intent::CancelDelete => self.cancel_delete_confirmation(),
            Intent::ConfirmDelete => self.confirm_delete_data(),
            Intent::CancelConfirmation => {
                self.confirm_escalate_to_vm = None;
                self.confirm_vendor_uninstall = None;
                self.notice = None;
            }
            other => {
                if !self.queue_live_control_intent(other) {
                    self.apply_appliance_intent(other);
                    self.apply_install_intent(other);
                    self.apply_app_intent(other, doc);
                }
            }
        }
    }

    fn apply_appliance_intent(&mut self, intent: Intent) {
        if self.appliance_owner.busy() {
            return;
        }
        match intent {
            Intent::ChooseIso if self.iso_portal_rx.is_none() => {
                self.iso_portal_rx = Some(windows_iso_pick_request().send_open_file());
                self.notice = Some(Notice::info(strings::t("windows-choose-iso")));
            }
            Intent::Recovery => {
                let result = self.appliance_owner.open_recovery();
                self.report(result, "windows-recovery-starting");
            }
            Intent::Lifecycle(action) => {
                let result = self.appliance_owner.lifecycle(action);
                self.report(
                    result,
                    match action {
                        LifecycleAction::Start => "windows-start-starting",
                        LifecycleAction::Restart => "windows-restart-starting",
                        LifecycleAction::Suspend => "windows-suspend-starting",
                        LifecycleAction::Resume => "windows-resume-starting",
                        LifecycleAction::Shutdown => "windows-shutdown-starting",
                    },
                );
            }
            Intent::ConnectActivation | Intent::ConnectUpdate => {
                let purpose = if intent == Intent::ConnectActivation {
                    EgressPurpose::Activation
                } else {
                    EgressPurpose::WindowsUpdate
                };
                let result = self.appliance_owner.open_temporary_egress(purpose);
                self.report(result, "windows-temporary-connection-starting");
            }
            Intent::RevokeConnection => {
                if let Err(error) = self.appliance_owner.revoke_temporary_egress() {
                    self.notice = Some(Notice::error(error));
                }
            }
            _ => {}
        }
    }

    fn apply_install_intent(&mut self, intent: Intent) {
        match intent {
            Intent::ConfirmCapsule if self.catalogue.vm_install_confirmation.is_some() => {
                let result = self.provider.confirm_capsule_install();
                self.report(result, "windows-capsule-setup-started");
                self.refresh();
            }
            Intent::ConfirmVm if self.catalogue.vm_install_confirmation.is_some() => {
                let result = self.provider.confirm_vm_install();
                self.report(result, "windows-vm-setup-started");
                self.refresh();
            }
            Intent::Install if self.portal_rx.is_none() => {
                self.portal_rx = Some(installer_pick_request().send_open_file());
                self.notice = Some(Notice::info(strings::t("windows-choose-installer")));
            }
            _ => {}
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered dispatcher applies every app intent against the selected model"
    )]
    fn apply_app_intent(&mut self, intent: Intent, doc: &mut AutoCommit) {
        let Some(app) = self.selected_app().cloned() else {
            return;
        };
        if self.page != Page::App {
            return;
        }
        match intent {
            Intent::OpenFile
                if self.document_portal_rx.is_none()
                    && app.route == Route::Vm
                    && app.portal_file_open_available =>
            {
                self.document_target = Some(app.id);
                self.document_portal_rx = Some(windows_document_pick_request().send_open_file());
                self.notice = Some(Notice::info(strings::t("windows-choose-document")));
            }
            Intent::Route(route)
                if app.route_override_available
                    && route != app.route
                    && (route != Route::Vm || app.launch_available) =>
            {
                self.commit_route(
                    doc,
                    &actions::WindowsAction::SetRoute {
                        app_id: app.id,
                        route,
                    },
                );
                self.project_shared_route_policy(doc);
                self.refresh();
                if self.notice.is_none() {
                    self.notice = Some(Notice::info(strings::t("windows-route-set")));
                }
            }
            Intent::ResetRoute if app.route_override_available && app.uses_override() => {
                self.commit_route(doc, &actions::WindowsAction::ResetRoute { app_id: app.id });
                self.project_shared_route_policy(doc);
                self.refresh();
                if self.notice.is_none() {
                    self.notice = Some(Notice::info(strings::t("windows-route-reset")));
                }
            }
            Intent::Launch if app.launch_available => {
                let result = if app.route == Route::Vm {
                    self.provider.launch_vm_fallback(&app.id)
                } else {
                    self.provider.launch(&app.id)
                };
                self.report(result, "windows-launch-queued");
            }
            Intent::VmFallback if app.route != Route::Vm && app.vm_fallback_available => {
                let result = self.provider.launch_vm_fallback(&app.id);
                self.report(result, "windows-vm-launch-queued");
            }
            Intent::Publish if app.can_publish_vm_candidate => {
                let result = self.provider.publish_vm_candidate(&app.id);
                self.report(result, "windows-vm-candidate-publishing");
                self.refresh();
            }
            Intent::Retry if app.can_retry_install => {
                let result = self.provider.retry_install(&app.capsule);
                self.report(result, "windows-retrying");
            }
            Intent::Escalate if app.can_escalate_install_to_vm => {
                if self.confirm_escalate_to_vm.as_deref() == Some(app.capsule.as_str()) {
                    self.confirm_escalate_to_vm = None;
                    let result = self.provider.escalate_install_to_vm(&app.capsule);
                    self.report(result, "windows-escalation-started");
                    self.refresh();
                } else {
                    self.confirm_escalate_to_vm = Some(app.capsule);
                    self.confirm_vendor_uninstall = None;
                    self.confirm_delete_data = None;
                    self.notice = None;
                }
            }
            Intent::RemoveLauncher if app.can_remove_launcher => {
                let result = self.provider.remove_launcher(&app.id);
                self.report(result, "windows-launcher-removed");
            }
            Intent::Uninstall if app.can_vendor_uninstall => {
                let vm_application = app.route == Route::Vm;
                let management_id = if vm_application { app.id } else { app.capsule };
                if self.confirm_vendor_uninstall.as_deref() == Some(management_id.as_str()) {
                    self.confirm_vendor_uninstall = None;
                    let result = if vm_application {
                        self.provider.vendor_uninstall_vm(&management_id)
                    } else {
                        self.provider.vendor_uninstall(&management_id)
                    };
                    self.report(
                        result,
                        if vm_application {
                            "windows-vm-uninstalling"
                        } else {
                            "windows-uninstalling"
                        },
                    );
                } else {
                    self.confirm_vendor_uninstall = Some(management_id);
                    self.confirm_escalate_to_vm = None;
                    self.confirm_delete_data = None;
                    self.notice = None;
                }
            }
            Intent::DeleteData if app.can_delete_data && self.confirm_delete_data.is_none() => {
                self.confirm_delete_data = Some(app.capsule);
                self.confirm_escalate_to_vm = None;
                self.confirm_vendor_uninstall = None;
                self.notice = None;
            }
            _ => {}
        }
    }

    fn invoke_portal_operation(&mut self, type_id: &str, payload: &[u8]) -> Result<bool, String> {
        match type_id {
            "windows.install" => {
                let path: String =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if path.trim().is_empty() {
                    if self.portal_rx.is_some() {
                        return Err("an installer picker is already open".into());
                    }
                    self.portal_rx = Some(installer_pick_request().send_open_file());
                } else {
                    let path = std::path::PathBuf::from(path);
                    let source_name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| "installer path has no valid file name".to_string())?;
                    let source = File::open(&path)
                        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
                    self.provider.install(source, source_name)?;
                }
            }
            "windows.import-iso" => {
                let path: String =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if path.trim().is_empty() {
                    if self.iso_portal_rx.is_some() {
                        return Err("a Windows ISO picker is already open".into());
                    }
                    self.iso_portal_rx = Some(windows_iso_pick_request().send_open_file());
                } else {
                    let source = File::open(&path)
                        .map_err(|error| format!("could not open {path}: {error}"))?;
                    self.appliance_owner.import_iso(source)?;
                }
            }
            "windows.confirm-capsule-install" => self.provider.confirm_capsule_install()?,
            "windows.confirm-vm-install" => self.provider.confirm_vm_install()?,
            "windows.open-file" => {
                let args: Vec<String> =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                let app_id = args
                    .first()
                    .ok_or_else(|| "missing Windows app id".to_string())?;
                let file_path = args.get(1).map_or("", String::as_str);
                if file_path.trim().is_empty() {
                    if self.document_portal_rx.is_some() {
                        return Err("a document picker is already open".into());
                    }
                    self.document_target = Some(app_id.clone());
                    self.document_portal_rx =
                        Some(windows_document_pick_request().send_open_file());
                } else {
                    let source = File::open(file_path)
                        .map_err(|error| format!("could not open {file_path}: {error}"))?;
                    self.provider.launch_vm_with_file(app_id, source)?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn invoke_appliance_operation(&mut self, type_id: &str) -> Result<bool, String> {
        match type_id {
            "windows.recovery" => self.appliance_owner.open_recovery()?,
            "windows.start" => self.appliance_owner.lifecycle(LifecycleAction::Start)?,
            "windows.restart" => self.appliance_owner.lifecycle(LifecycleAction::Restart)?,
            "windows.suspend" => self.appliance_owner.lifecycle(LifecycleAction::Suspend)?,
            "windows.resume" => self.appliance_owner.lifecycle(LifecycleAction::Resume)?,
            "windows.shutdown" => self.appliance_owner.lifecycle(LifecycleAction::Shutdown)?,
            "windows.temporary-network" => self
                .appliance_owner
                .open_temporary_egress(EgressPurpose::Activation)?,
            "windows.update-network" => self
                .appliance_owner
                .open_temporary_egress(EgressPurpose::WindowsUpdate)?,
            "windows.revoke-network" => self.appliance_owner.revoke_temporary_egress()?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn invoke_application_operation(
        &mut self,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if !matches!(
            type_id,
            "windows.launch"
                | "windows.launch-vm-fallback"
                | "windows.publish"
                | "windows.retry-install"
                | "windows.escalate-install"
                | "windows.remove-launcher"
                | "windows.uninstall"
                | "windows.delete-data"
        ) {
            return Ok(false);
        }
        let id: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let app = self
            .catalogue
            .app(&id)
            .cloned()
            .or_else(|| {
                self.catalogue
                    .apps
                    .iter()
                    .find(|app| app.capsule == id)
                    .cloned()
            })
            .ok_or_else(|| "Windows application is no longer available".to_string())?;
        match type_id {
            "windows.launch" if app.launch_available => {
                if app.route == Route::Vm {
                    self.provider.launch_vm_fallback(&app.id)?;
                } else {
                    self.provider.launch(&app.id)?;
                }
            }
            "windows.launch-vm-fallback" if app.vm_fallback_available => {
                self.provider.launch_vm_fallback(&app.id)?;
            }
            "windows.publish" if app.can_publish_vm_candidate => {
                self.provider.publish_vm_candidate(&app.id)?;
            }
            "windows.retry-install" if app.can_retry_install => {
                self.provider.retry_install(&app.capsule)?;
            }
            "windows.escalate-install" if app.can_escalate_install_to_vm => {
                self.provider.escalate_install_to_vm(&app.capsule)?;
            }
            "windows.remove-launcher" if app.can_remove_launcher => {
                self.provider.remove_launcher(&app.id)?;
            }
            "windows.uninstall" if app.can_vendor_uninstall => {
                if app.route == Route::Vm {
                    self.provider.vendor_uninstall_vm(&app.id)?;
                } else {
                    self.provider.vendor_uninstall(&app.capsule)?;
                }
            }
            "windows.delete-data" if app.can_delete_data => {
                self.provider.delete_data(&app.capsule)?;
            }
            _ => return Err("Windows operation is no longer available".into()),
        }
        self.notice = Some(Notice::info(strings::t("windows-operation-requested")));
        Ok(true)
    }

    fn invoke_effect_operation(&mut self, type_id: &str, payload: &[u8]) -> Result<bool, String> {
        if self.invoke_portal_operation(type_id, payload)?
            || self.invoke_appliance_operation(type_id)?
        {
            return Ok(true);
        }
        self.invoke_application_operation(type_id, payload)
    }
}

// --- view --------------------------------------------------------------------

impl WindowsApp {
    fn appliance_busy(&self) -> bool {
        self.appliance_owner.busy()
            || self.iso_portal_rx.is_some()
            || self.portal_rx.is_some()
            || self.document_portal_rx.is_some()
    }

    /// Whether the window is narrow enough that the overview and the content
    /// are separate screens.
    fn pushed(&self) -> bool {
        !self.shell.geometry().stack.is_empty()
    }

    fn confirmation(&self) -> Option<Confirm> {
        if self.confirm_delete_data.is_some() {
            Some(Confirm::Delete)
        } else if self.confirm_escalate_to_vm.is_some() {
            Some(Confirm::Escalate)
        } else if self.confirm_vendor_uninstall.is_some() {
            Some(Confirm::Uninstall {
                vm: self
                    .selected_app()
                    .is_some_and(|app| app.route == Route::Vm),
            })
        } else {
            None
        }
    }

    fn overview_key(&self) -> OverviewKey {
        let status = self.appliance_owner.status();
        OverviewKey {
            apps: self
                .catalogue
                .apps
                .iter()
                .map(|app| (app.id.clone(), app.name.clone(), app.status, app.route))
                .collect(),
            appliance: (
                appliance_summary(status, self.appliance_owner.has_status()),
                status.power_state == AppliancePowerState::Running,
            ),
            pending: self
                .catalogue
                .vm_install_confirmation
                .as_ref()
                .map(|pending| pending.display_name.clone()),
            page: self.page,
            selected: self.selected,
            pushed: self.pushed(),
        }
    }

    fn content_key(&self) -> ContentKey {
        let appliance = self.appliance_owner.status().clone();
        ContentKey {
            page: self.page,
            app: self.selected_app().cloned(),
            vm: self.catalogue.vm.clone(),
            pending: self.catalogue.vm_install_confirmation.clone(),
            lease: appliance
                .temporary_egress
                .as_ref()
                .map(crate::appliance_owner::TemporaryEgressLease::expiry_label),
            appliance,
            appliance_checked: self.appliance_owner.has_status(),
            busy: self.appliance_busy(),
            notice: self.notice.clone(),
            details: self.appliance_details,
        }
    }

    /// Swap whichever columns no longer show the model.
    pub(crate) fn rebuild(&mut self) {
        let overview = self.overview_key();
        if self.overview_key.as_ref() != Some(&overview) {
            self.shell.set_overview(Some(self.overview()));
            self.overview_key = Some(overview);
        }
        let content = self.content_key();
        if self.content_key.as_ref() != Some(&content) {
            self.shell.set_content(self.content());
            self.shell.set_content_title(self.content_title());
            self.content_key = Some(content);
        }
        let confirmation = self.confirmation();
        if self.dialog_for != confirmation {
            self.dialog = confirmation.map(|confirm| self.dialog(confirm));
            self.dialog_for = confirmation;
        }
        if std::mem::take(&mut self.push_content) {
            self.shell.show(NavPane::Content);
        }
    }

    fn overview(&self) -> Element {
        let pushed = self.pushed();
        let mut list = Column::new().with_intrinsic_height();
        if let Some(pending) = &self.catalogue.vm_install_confirmation {
            let mut row = ListRow::new("windows-pending-install", &pending.display_name)
                .secondary(strings::t("windows-pending-secondary"))
                .selected(self.page == Page::PendingInstall)
                .on_activate(queue(&self.intents, Intent::SelectPending));
            if pushed {
                row = row.disclosure();
            }
            list = list.push(Box::new(Section::new(vec![Box::new(row)]).with_header(
                SectionHeader::group(strings::t("windows-pending-section")),
            )));
        }
        if self.catalogue.apps.is_empty() {
            list = list.push(Box::new(secondary(&strings::t("windows-no-apps"))));
        } else {
            let rows = self
                .catalogue
                .apps
                .iter()
                .enumerate()
                .map(|(index, app)| -> Element {
                    let mut row = ListRow::new(format!("windows-app-{}", app.id), &app.name)
                        .secondary(app.status.label())
                        .meta(app.route.label())
                        .selected(self.page == Page::App && index == self.selected)
                        .on_activate(queue(&self.intents, Intent::SelectApp(index)));
                    // Every app row carries a status dot, so the route
                    // beside it keeps one trailing edge down the list.
                    row = match app.status {
                        AppStatus::Running => row.active(),
                        AppStatus::NeedsAttention => row.warning(),
                        AppStatus::Failed => row.error(),
                        AppStatus::Ready
                        | AppStatus::Installing
                        | AppStatus::Uninstalling
                        | AppStatus::Uninstalled => row.idle(),
                    };
                    if pushed {
                        row = row.disclosure();
                    }
                    Box::new(row)
                })
                .collect();
            list = list.push(Box::new(Section::new(rows)));
        }
        let status = self.appliance_owner.status();
        let mut appliance = ListRow::new("windows-appliance", strings::t("windows-appliance-row"))
            .secondary(appliance_summary(status, self.appliance_owner.has_status()))
            .selected(self.page == Page::Appliance)
            .on_activate(queue(&self.intents, Intent::SelectAppliance));
        appliance = if status.power_state == AppliancePowerState::Running {
            appliance.active()
        } else {
            appliance.idle()
        };
        if pushed {
            appliance = appliance.disclosure();
        }
        list = list.push(Box::new(
            Section::new(vec![Box::new(appliance)]).with_header(SectionHeader::group(strings::t(
                "windows-appliance-section",
            ))),
        ));
        Box::new(
            Scrollable::new_vertical()
                .with_semantic_key("windows-overview")
                .with_persisted_offset(Rc::clone(&self.overview_scroll))
                .with_cursor(self.cursor)
                .with_child(Box::new(list)),
        )
    }

    fn content_title(&self) -> String {
        match self.page {
            Page::App => self
                .selected_app()
                .map(|app| app.name.clone())
                .unwrap_or_default(),
            Page::Appliance => strings::t("windows-appliance-title"),
            Page::PendingInstall => self
                .catalogue
                .vm_install_confirmation
                .as_ref()
                .map(|pending| pending.display_name.clone())
                .unwrap_or_default(),
        }
    }

    fn content(&self) -> Element {
        let mut page = PageParts::default();
        if let Some(notice) = &self.notice {
            page.head.push(Box::new(if notice.error {
                Banner::error(&notice.text)
            } else {
                Banner::info(&notice.text)
            }));
        }
        match self.page {
            Page::App => match self.selected_app() {
                Some(app) => self.app_page(app, &mut page),
                None => self.empty_page(&mut page),
            },
            Page::Appliance => self.appliance_page(&mut page),
            Page::PendingInstall => self.pending_page(&mut page),
        }
        let mut column = page
            .head
            .into_iter()
            .fold(Column::new().with_intrinsic_height(), Column::push);
        if !page.sections.is_empty() {
            // Two sibling gaps around an empty spacer: the sections stand a
            // group gap below the head block, as they stand from each other.
            column = column.push(Box::new(Spacer::token(SpacingStep::S0)));
            let form = page.sections.into_iter().fold(
                Form::new().with_density(Some(Density::Compact)),
                Form::section,
            );
            column = column.push(Box::new(form));
        }
        Box::new(
            Scrollable::new_vertical()
                .with_semantic_key("windows-content")
                .with_persisted_offset(Rc::clone(&self.content_scroll))
                .with_cursor(self.cursor)
                .with_child(Box::new(column)),
        )
    }

    fn empty_page(&self, page: &mut PageParts) {
        let message = if self.catalogue.vm.ready {
            strings::t("windows-empty-body")
        } else {
            self.catalogue.vm.detail.clone()
        };
        page.head.push(Box::new(
            ui::widget::EmptyState::new(strings::t("windows-empty-title")).with_message(message),
        ));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the detail page reads top to bottom as the sections it shows"
    )]
    fn app_page(&self, app: &crate::model::WindowsApp, page: &mut PageParts) {
        let intents = &self.intents;
        let vm = app.route == Route::Vm;
        // A failed setup leads with what went wrong and the ways out of it;
        // retrying is then the page's main action.
        let recovering = app.status == AppStatus::Failed;
        let retry_leads = recovering && app.can_retry_install;

        if recovering {
            let mut banner = Banner::error(strings::t("windows-setup-failed-title"));
            if let Some(detail) = app
                .installation_detail
                .as_ref()
                .or(app.launch_unavailable_reason.as_ref())
            {
                banner = banner.with_message(detail);
            }
            page.head.push(Box::new(banner));
            let mut recovery: Vec<Element> = Vec::new();
            if app.can_retry_install {
                recovery.push(Box::new(action_button(
                    "windows-retry-install",
                    "windows-retry-install",
                    ButtonRole::Primary,
                    intents,
                    Intent::Retry,
                )));
            }
            if app.can_escalate_install_to_vm {
                recovery.push(Box::new(action_button(
                    "windows-escalate-install-to-vm",
                    "windows-escalate-install-to-vm",
                    ButtonRole::Secondary,
                    intents,
                    Intent::Escalate,
                )));
            }
            if !recovery.is_empty() {
                page.head.push(Box::new(row(recovery)));
            }
        }

        // The ways to open the app. A setup that failed has nothing to open.
        if !recovering || app.launch_available {
            let mut actions: Vec<Element> = vec![Box::new(
                action_button(
                    "windows-launch",
                    "windows-launch",
                    if retry_leads {
                        ButtonRole::Secondary
                    } else {
                        ButtonRole::Primary
                    },
                    intents,
                    Intent::Launch,
                )
                .with_enabled(app.launch_available),
            )];
            if vm {
                let mut open_file = action_button(
                    "windows-open-file-in-vm",
                    "windows-open-file-in-vm",
                    ButtonRole::Secondary,
                    intents,
                    Intent::OpenFile,
                )
                .with_enabled(app.portal_file_open_available && self.document_portal_rx.is_none());
                if let Some(reason) = app
                    .portal_file_open_unavailable_reason
                    .as_ref()
                    .filter(|_| !app.portal_file_open_available)
                {
                    open_file = open_file.tooltip(reason.clone());
                }
                actions.push(Box::new(open_file));
            } else if app.vm_fallback_available {
                actions.push(Box::new(action_button(
                    "windows-open-in-vm",
                    "windows-open-in-vm",
                    ButtonRole::Secondary,
                    intents,
                    Intent::VmFallback,
                )));
            }
            page.head.push(Box::new(row(actions)));
            if let Some(reason) = app
                .launch_unavailable_reason
                .as_ref()
                .filter(|_| !app.launch_available && !recovering)
            {
                page.head.push(Box::new(caption(reason)));
            }
        }

        // How it opens, and why.
        let mut open_with: Vec<Element> = Vec::new();
        if app.route_override_available {
            let selected = Route::ALL
                .iter()
                .position(|route| *route == app.route)
                .unwrap_or(0);
            let routes = {
                let intents = Rc::clone(intents);
                ToggleGroup::new(
                    Route::ALL
                        .iter()
                        .map(|route| Segment::new(strings::t(route.button_key())))
                        .collect(),
                    selected,
                )
                .with_id("windows-route")
                .on_change(move |index| {
                    if let Some(route) = Route::ALL.get(index) {
                        intents.borrow_mut().push(Intent::Route(*route));
                    }
                })
            };
            let mut choice: Vec<Element> = vec![Box::new(routes)];
            if app.uses_override() {
                choice.push(Box::new(action_button(
                    "windows-use-recommendation",
                    "windows-use-recommendation",
                    ButtonRole::Plain,
                    intents,
                    Intent::ResetRoute,
                )));
            }
            open_with.push(Box::new(row(choice)));
        } else {
            open_with.push(Box::new(body(app.route.label())));
            if let Some(reason) = &app.route_override_unavailable_reason {
                open_with.push(Box::new(caption(reason)));
            }
        }
        open_with.push(Box::new(secondary(&app.reason)));
        page.sections.push(section("windows-open-with", open_with));

        // What it can reach.
        let mut access: Vec<Element> = Vec::new();
        if vm {
            access.push(Box::new(secondary(&strings::t(
                "windows-shared-session-trust",
            ))));
        }
        let mut reach: Vec<Property> = app
            .grants
            .iter()
            .map(|grant| Property::new(&grant.label, &grant.detail))
            .collect();
        if vm {
            let shared = &self.catalogue.vm;
            if shared.shared_session_active {
                reach.push(Property::new(
                    strings::t("windows-shared-access"),
                    shared.shared_session_grants.join(", "),
                ));
                reach.push(Property::new(
                    strings::t("windows-shared-with"),
                    shared.shared_session_contributors.join(", "),
                ));
            } else {
                reach.push(Property::new(
                    strings::t("windows-shared-access"),
                    strings::t("windows-no-redirects"),
                ));
            }
        }
        if !reach.is_empty() {
            access.push(Box::new(PropertyList::new(reach)));
        }
        let boundary = if app.fallback_detail.trim().is_empty() {
            app.route.security_summary()
        } else {
            app.fallback_detail.as_str()
        };
        access.push(Box::new(caption(boundary)));
        page.sections.push(section("windows-access", access));

        // Where it comes from.
        let mut about = Vec::new();
        if !recovering {
            about.push(Property::new(
                strings::t("windows-status"),
                app.status.label(),
            ));
        }
        about.extend([
            Property::new(strings::t("windows-publisher"), &app.publisher),
            Property::new(strings::t("windows-source"), &app.catalogue_source),
            Property::new(strings::t("windows-compatibility"), app.confidence.label()),
            Property::new(strings::t("windows-runner"), &app.runner),
        ]);
        if let Some(detail) = app.installation_detail.as_ref().filter(|_| !recovering) {
            about.push(Property::new(strings::t("windows-installation"), detail));
        }
        page.sections.push(section(
            "windows-about",
            vec![Box::new(PropertyList::new(about))],
        ));

        // Maintenance. Recovering a failed setup leads the page instead.
        let mut manage: Vec<Element> = Vec::new();
        for (available, key, role, intent) in [
            (
                app.can_publish_vm_candidate,
                "windows-publish-vm-candidate",
                ButtonRole::Secondary,
                Intent::Publish,
            ),
            (
                app.can_retry_install && !recovering,
                "windows-retry-install",
                ButtonRole::Secondary,
                Intent::Retry,
            ),
            (
                app.can_escalate_install_to_vm && !recovering,
                "windows-escalate-install-to-vm",
                ButtonRole::Secondary,
                Intent::Escalate,
            ),
            (
                app.can_remove_launcher,
                "windows-remove-launcher",
                ButtonRole::Secondary,
                Intent::RemoveLauncher,
            ),
            (
                app.can_vendor_uninstall,
                "windows-vendor-uninstall",
                ButtonRole::Destructive,
                Intent::Uninstall,
            ),
            (
                app.can_delete_data,
                "windows-delete-data",
                ButtonRole::Destructive,
                Intent::DeleteData,
            ),
        ] {
            if available {
                manage.push(Box::new(action_button(key, key, role, intents, intent)));
            }
        }
        if !manage.is_empty() {
            page.sections.push(section(
                "windows-manage-installation",
                vec![Box::new(row(manage))],
            ));
        }
    }

    fn pending_page(&self, page: &mut PageParts) {
        let Some(pending) = &self.catalogue.vm_install_confirmation else {
            return;
        };
        let intents = &self.intents;
        if !pending.detail.trim().is_empty() {
            page.head.push(Box::new(body(&pending.detail)));
        }
        // The two destinations, the recommended one first and labelled.
        let vm_recommended = pending.recommended_route == Route::Vm;
        let destination = |id: &str, title: &str, detail: &str, icon: Icon, intent: Intent| {
            ListRow::new(id, strings::t(title))
                .icon(icon)
                .secondary(strings::t(detail))
                .disclosure()
                .on_activate(queue(intents, intent))
        };
        let mut capsule = destination(
            "windows-confirm-capsule-install",
            "windows-install-private-capsule",
            "windows-install-private-capsule-detail",
            Icon::Shield,
            Intent::ConfirmCapsule,
        );
        let mut appliance = destination(
            "windows-confirm-vm-install",
            "windows-install-windows-appliance",
            "windows-install-windows-appliance-detail",
            Icon::Monitor,
            Intent::ConfirmVm,
        );
        let recommended = strings::t("windows-recommended");
        let rows: Vec<Element> = if vm_recommended {
            appliance = appliance.meta(recommended);
            vec![Box::new(appliance), Box::new(capsule)]
        } else {
            capsule = capsule.meta(recommended);
            vec![Box::new(capsule), Box::new(appliance)]
        };
        page.sections.push(section(
            "windows-install-in",
            vec![Box::new(Section::new(rows))],
        ));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the appliance page reads top to bottom as the sections it shows"
    )]
    fn appliance_page(&self, page: &mut PageParts) {
        use crate::appliance_owner::AppliancePhase;
        let intents = &self.intents;
        let status = self.appliance_owner.status();
        let busy = self.appliance_busy();
        if !self.appliance_owner.has_status() {
            page.head.push(Box::new(secondary(&strings::t(
                "windows-appliance-checking",
            ))));
            return;
        }
        if !status.is_provisioned() {
            let title = strings::t("windows-appliance-not-installed");
            let banner = if status.phase == AppliancePhase::Unavailable {
                Banner::warning(title)
            } else {
                Banner::info(title)
            };
            page.head.push(Box::new(
                banner.with_message(strings::t("windows-appliance-not-installed-body")),
            ));
            let mut actions: Vec<Element> = vec![Box::new(
                action_button(
                    "windows-choose-windows-iso",
                    "windows-appliance-set-up",
                    ButtonRole::Primary,
                    intents,
                    Intent::ChooseIso,
                )
                .with_enabled(!busy),
            )];
            if status.detail.is_some() {
                actions.push(Box::new(action_button(
                    "windows-appliance-details",
                    if self.appliance_details {
                        "windows-hide-details"
                    } else {
                        "windows-show-details"
                    },
                    ButtonRole::Plain,
                    intents,
                    Intent::ToggleApplianceDetails,
                )));
            }
            page.head.push(Box::new(row(actions)));
            if self.appliance_details
                && let Some(detail) = &status.detail
            {
                page.head
                    .push(Box::new(caption(detail).with_kind(TextKind::Code)));
            }
            return;
        }

        page.head.push(Box::new(body(status.status_statement())));
        page.head.push(Box::new(PropertyList::new(vec![
            Property::new(
                strings::t("windows-activation-status"),
                status.license_label(),
            ),
            Property::new(
                strings::t("windows-published-apps"),
                status.published_applications.to_string(),
            )
            .with_kind(TextKind::Numeric),
        ])));
        if let Some(detail) = status.license_detail() {
            page.head.push(Box::new(caption(detail)));
        }

        let power: Vec<Element> = [
            (LifecycleAction::Start, "windows-start-appliance"),
            (LifecycleAction::Resume, "windows-resume-appliance"),
            (LifecycleAction::Restart, "windows-restart-appliance"),
            (LifecycleAction::Suspend, "windows-suspend-appliance"),
            (LifecycleAction::Shutdown, "windows-shutdown-appliance"),
        ]
        .into_iter()
        .filter(|(action, _)| status.power_state.allows(*action))
        .map(|(action, key)| -> Element {
            Box::new(
                action_button(
                    key,
                    key,
                    ButtonRole::Secondary,
                    intents,
                    Intent::Lifecycle(action),
                )
                .with_enabled(!busy),
            )
        })
        .collect();
        if !power.is_empty() {
            page.sections
                .push(section("windows-power", vec![Box::new(row(power))]));
        }

        page.sections.push(section(
            "windows-setup-recovery",
            vec![
                Box::new(secondary(&strings::t("windows-appliance-desktop-note"))),
                Box::new(row(vec![Box::new(
                    action_button(
                        "windows-open-recovery-desktop",
                        "windows-open-recovery-desktop",
                        ButtonRole::Secondary,
                        intents,
                        Intent::Recovery,
                    )
                    .with_enabled(status.recovery_available && !busy),
                )])),
            ],
        ));

        let running = status.power_state == AppliancePowerState::Running;
        let mut connection: Vec<Element> = Vec::new();
        if let Some(lease) = &status.temporary_egress {
            connection.push(Box::new(body(&format!(
                "{}: {}",
                lease.purpose.label(),
                lease.expiry_label()
            ))));
            connection.push(Box::new(row(vec![Box::new(
                action_button(
                    "windows-revoke-temporary-connection",
                    "windows-revoke-temporary-connection",
                    ButtonRole::Destructive,
                    intents,
                    Intent::RevokeConnection,
                )
                .with_enabled(!busy),
            )])));
        } else {
            connection.push(Box::new(secondary(&strings::t(if running {
                "windows-connection-explained"
            } else {
                "windows-connection-needs-running"
            }))));
            connection.push(Box::new(row(vec![
                Box::new(
                    action_button(
                        "windows-connect-temporarily",
                        "windows-connect-for-activation",
                        ButtonRole::Secondary,
                        intents,
                        Intent::ConnectActivation,
                    )
                    .with_enabled(running && !busy),
                ),
                Box::new(
                    action_button(
                        "windows-connect-for-update",
                        "windows-connect-for-update",
                        ButtonRole::Secondary,
                        intents,
                        Intent::ConnectUpdate,
                    )
                    .with_enabled(running && !busy),
                ),
            ])));
        }
        page.sections
            .push(section("windows-temporary-connection", connection));
    }

    fn dialog(&self, confirm: Confirm) -> ModalDialog<Text> {
        let name = self
            .selected_app()
            .map(|app| app.name.clone())
            .unwrap_or_default();
        let (title, message, action, confirm_intent, cancel, destructive) = match confirm {
            Confirm::Delete => (
                strings::t("windows-confirm-delete-title"),
                strings::format("windows-confirm-delete-body", "app", &name),
                strings::t("windows-confirm-delete-action"),
                Intent::ConfirmDelete,
                Intent::CancelDelete,
                true,
            ),
            Confirm::Escalate => (
                strings::t("windows-confirm-escalation-title"),
                strings::t("windows-confirm-escalation-body"),
                strings::t("windows-escalate-install-to-vm"),
                Intent::Escalate,
                Intent::CancelConfirmation,
                false,
            ),
            Confirm::Uninstall { vm } => (
                strings::t("windows-confirm-uninstall-title"),
                strings::format(
                    if vm {
                        "windows-confirm-vm-uninstall-body"
                    } else {
                        "windows-confirm-uninstall-body"
                    },
                    "app",
                    &name,
                ),
                strings::t("windows-vendor-uninstall"),
                Intent::Uninstall,
                Intent::CancelConfirmation,
                true,
            ),
        };
        ModalDialog::new(title, body(&message))
            .with_test_id("windows-confirmation")
            .primary_action(action, queue(&self.intents, confirm_intent))
            .secondary_action(strings::t("windows-cancel"), queue(&self.intents, cancel))
            .on_dismiss(queue(&self.intents, cancel))
            .with_destructive(destructive)
            .open(true)
    }

    /// Apply queued intents, then bring the columns up to date.
    fn settle(&mut self, doc: &mut AutoCommit) {
        self.apply_intents(doc);
        self.rebuild();
    }

    fn dialog_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(ModalDialog::is_open)
    }
}

const LOCAL_OPERATIONS: &[&str] = &[
    "windows.launch",
    "windows.launch-vm-fallback",
    "windows.publish",
    "windows.retry-install",
    "windows.escalate-install",
    "windows.remove-launcher",
    "windows.uninstall",
    "windows.delete-data",
    "windows.open-file",
    "windows.install",
    "windows.import-iso",
    "windows.confirm-capsule-install",
    "windows.confirm-vm-install",
    "windows.recovery",
    "windows.start",
    "windows.restart",
    "windows.suspend",
    "windows.resume",
    "windows.shutdown",
    "windows.temporary-network",
    "windows.update-network",
    "windows.revoke-network",
];

/// The window is its shell plus the model, the confirmation dialog and the
/// Windows operations the shell knows nothing about.
impl ui::widget::Decorator for WindowsApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource;
        inner = shell
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        LOCAL_OPERATIONS
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = self.shell.resource_operation_handlers();
        handlers.extend(LOCAL_OPERATIONS.iter().map(|handler| (*handler).to_owned()));
        handlers
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.project_shared_route_policy(doc);
            self.drain_portal();
            self.drain_document_portal();
            self.drain_iso_portal();
            self.refresh();
            self.settle(doc);
        }
        self.shell.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let Some(dialog) = self.dialog.as_mut().filter(|dialog| dialog.is_open()) {
            dialog.handle_event(event, ctx);
            *ctx.handled = true;
        } else {
            self.shell.handle_event(event, ctx);
            if !*ctx.handled
                && self.page == Page::App
                && let WindowEvent::KeyboardInput {
                    event: key_event, ..
                } = event
                && key_event.state == ElementState::Pressed
            {
                let last = self.catalogue.apps.len().saturating_sub(1);
                let next = match key_event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => Some(self.selected.saturating_sub(1)),
                    Key::Named(NamedKey::ArrowDown) => Some((self.selected + 1).min(last)),
                    _ => None,
                };
                if let Some(next) = next.filter(|next| *next != self.selected) {
                    self.intents.borrow_mut().push(Intent::SelectApp(next));
                    *ctx.handled = true;
                }
            }
        }
        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
        self.settle(doc);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.shell.children();
        if let Some(dialog) = self.dialog.as_ref().filter(|dialog| dialog.is_open()) {
            children.push(dialog);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.shell.for_each_child(visitor);
        if let Some(dialog) = self.dialog.as_ref().filter(|dialog| dialog.is_open()) {
            visitor(dialog);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self.shell.children_mut();
        if let Some(dialog) = self.dialog.as_mut().filter(|dialog| dialog.is_open()) {
            children.push(dialog);
        }
        children
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text, theme, presence);
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.dialog_open() {
            self.dialog.as_ref().map(ModalDialog::dialog_rect)
        } else {
            self.shell.modal_capture()
        }
    }

    fn traps_focus(&self) -> bool {
        self.dialog_open() || self.shell.traps_focus()
    }

    fn needs_redraw(&self) -> bool {
        self.dialog_open() || self.shell.needs_redraw()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let activates = matches!(
            action,
            AccessibilityAction::Activate
                | AccessibilityAction::Focus
                | AccessibilityAction::ScrollIntoView
        );
        let intent = target
            .strip_prefix("windows.select:")
            .filter(|_| activates)
            .and_then(|id| {
                self.catalogue
                    .apps
                    .iter()
                    .position(|candidate| candidate.id == id)
            })
            .map(Intent::SelectApp);
        let handled = if let Some(intent) = intent {
            self.intents.borrow_mut().push(intent);
            true
        } else {
            self.shell
                .perform_semantic_action(target, action, state, path)
        };
        let (doc, _) = ui::doc::document(state, path);
        self.settle(doc);
        handled
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            let started = ui::widget::start_in_children(self, op, doc, path);
            self.settle(doc);
            return started;
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            // A control in the view ran: apply what it asked for.
            self.settle(doc);
            return Ok(Some(started));
        }
        let started = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )?;
        self.refresh();
        self.rebuild();
        Ok(started)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.refresh();
        self.rebuild();
        if self
            .resource_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    fn name(&self) -> &str {
        "WindowsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("windows-app")
    }
}

impl ui::resources::PublishesResources for WindowsApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the manager door — keep only live catalogue extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == actions::MANAGER_RESOURCE_ID {
            return None;
        }
        find_resource(&self.publish_resources(doc), id).or_else(|| {
            self.is_hosted().then(|| {
                crate::resource::live_extras(self)
                    .into_iter()
                    .find(|resource| resource.id.as_str() == id)
            })?
        })
    }
}

impl ui::search::capability::SearchSource for WindowsApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }

    /// A picked hit shows its app, as choosing it in the list does.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AppModel, _path: &ModelPath) {
        let Some(id) = crate::search::app_from_anchor(action_id) else {
            return;
        };
        if let Some(index) = self.catalogue.apps.iter().position(|app| app.id == id) {
            self.select(Page::App, index);
            self.rebuild();
        }
    }
}

impl ui::widget::controls::ActionControls for WindowsApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.pending_control_invocation.take().or_else(|| {
            ui::widget::controls::ActionControls::take_control_invocation(&mut self.shell)
        })
    }
}

impl WindowsApp {
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Ok(None);
        }
        if self.resource_invocation.is_some() {
            return Err("another Windows operation is still running".into());
        }
        if !self.invoke_effect_operation(type_id, payload)? {
            return Ok(None);
        }
        let deferred = self.portal_rx.is_some()
            || self.document_portal_rx.is_some()
            || self.iso_portal_rx.is_some()
            || self.appliance_owner.busy()
            || !self.provider.operations_complete_on_return();
        if deferred {
            self.resource_invocation = Some(invocation);
            self.resource_waits_for_appliance = matches!(
                type_id,
                "windows.import-iso"
                    | "windows.recovery"
                    | "windows.start"
                    | "windows.restart"
                    | "windows.suspend"
                    | "windows.resume"
                    | "windows.shutdown"
                    | "windows.temporary-network"
                    | "windows.update-network"
                    | "windows.revoke-network"
            );
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }
}

fn find_resource(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    type RecordedInstall = Arc<Mutex<Option<(Vec<u8>, String)>>>;
    type RecordedDocument = Arc<Mutex<Option<(String, Vec<u8>)>>>;

    struct RecordingProvider {
        inner: SampleProvider,
        installed: RecordedInstall,
        opened_document: RecordedDocument,
    }

    impl CompatibilityProvider for RecordingProvider {
        fn catalogue(&mut self) -> Catalogue {
            self.inner.catalogue()
        }

        fn launch(&mut self, app_id: &str) -> Result<(), String> {
            self.inner.launch(app_id)
        }

        fn launch_vm_fallback(&mut self, app_id: &str) -> Result<(), String> {
            self.inner.launch_vm_fallback(app_id)
        }

        fn launch_vm_with_file(&mut self, app_id: &str, mut source: File) -> Result<(), String> {
            let mut bytes = Vec::new();
            source
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            *self.opened_document.lock().unwrap() = Some((app_id.to_string(), bytes));
            Ok(())
        }

        fn install(&mut self, mut source: File, source_name: &str) -> Result<(), String> {
            let mut bytes = Vec::new();
            source
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            *self.installed.lock().unwrap() = Some((bytes, source_name.to_string()));
            Ok(())
        }

        fn confirm_capsule_install(&mut self) -> Result<(), String> {
            self.inner.confirm_capsule_install()
        }

        fn confirm_vm_install(&mut self) -> Result<(), String> {
            self.inner.confirm_vm_install()
        }

        fn publish_vm_candidate(&mut self, entry_id: &str) -> Result<(), String> {
            self.inner.publish_vm_candidate(entry_id)
        }

        fn vendor_uninstall_vm(&mut self, entry_id: &str) -> Result<(), String> {
            self.inner.vendor_uninstall_vm(entry_id)
        }

        fn retry_install(&mut self, capsule_id: &str) -> Result<(), String> {
            self.inner.retry_install(capsule_id)
        }

        fn escalate_install_to_vm(&mut self, capsule_id: &str) -> Result<(), String> {
            self.inner.escalate_install_to_vm(capsule_id)
        }

        fn remove_launcher(&mut self, app_id: &str) -> Result<(), String> {
            self.inner.remove_launcher(app_id)
        }

        fn vendor_uninstall(&mut self, capsule_id: &str) -> Result<(), String> {
            self.inner.vendor_uninstall(capsule_id)
        }

        fn delete_data(&mut self, capsule_id: &str) -> Result<(), String> {
            self.inner.delete_data(capsule_id)
        }

        fn set_route_override(&mut self, app_id: &str, route: Route) -> Result<(), String> {
            self.inner.set_route_override(app_id, route)
        }

        fn reset_route_override(&mut self, app_id: &str) -> Result<(), String> {
            self.inner.reset_route_override(app_id)
        }
    }

    fn apply(app: &mut WindowsApp, doc: &mut AutoCommit, intent: Intent) {
        app.intents.borrow_mut().push(intent);
        app.settle(doc);
    }

    fn test_ids(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(id) = widget.test_id() {
            out.push(id.to_owned());
        }
        widget.for_each_child(&mut |child| test_ids(child, out));
    }

    fn ids(app: &WindowsApp) -> Vec<String> {
        let mut out = Vec::new();
        Widget::for_each_child(app, &mut |child| test_ids(child, &mut out));
        out
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn portal_pickers_hand_open_descriptors_to_the_right_target() {
        // Case `installer_portal_handoff_uses_its_open_descriptor_and_display_name`.
        {
            let installed = Arc::new(Mutex::new(None));
            let provider = RecordingProvider {
                inner: SampleProvider::new(),
                installed: Arc::clone(&installed),
                opened_document: Arc::new(Mutex::new(None)),
            };
            let mut app = WindowsApp::with_provider(Box::new(provider));
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("selected-installer.msi");
            std::fs::write(&path, b"accepted descriptor bytes").unwrap();
            let source = File::open(&path).unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_open_file(OpenFileOutcome::Opened(ui::portal::OpenedFile::new(
                source,
                "display-name.msi",
            )));
            let mut doc = AutoCommit::new();
            ui::host::scoped(&services, || apply(&mut app, &mut doc, Intent::Install));
            assert_eq!(
                recorder.pick_requests()[0].title(),
                "Add a Windows application"
            );
            // Replacing the old pathname before the UI drains it must not affect
            // the exact descriptor which the provider receives.
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"replacement path bytes").unwrap();

            app.drain_portal();

            assert_eq!(
                *installed.lock().unwrap(),
                Some((
                    b"accepted descriptor bytes".to_vec(),
                    "display-name.msi".into()
                ))
            );
        }
        // Case `vm_document_picker_hands_the_open_descriptor_to_the_selected_publication`.
        {
            let opened_document = Arc::new(Mutex::new(None));
            let provider = RecordingProvider {
                inner: SampleProvider::new(),
                installed: Arc::new(Mutex::new(None)),
                opened_document: Arc::clone(&opened_document),
            };
            let mut app = WindowsApp::with_provider(Box::new(provider));
            app.selected = app
                .catalogue
                .apps
                .iter()
                .position(|candidate| candidate.route == Route::Vm)
                .unwrap();
            let entry_id = app.selected_app().unwrap().id.clone();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("document.bin");
            std::fs::write(&path, b"portal document bytes").unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_open_file(OpenFileOutcome::Opened(ui::portal::OpenedFile::new(
                File::open(path).unwrap(),
                "document.bin",
            )));
            let payload = serde_json::to_vec(&[entry_id.as_str(), ""]).unwrap();
            assert!(
                ui::host::scoped(&services, || {
                    app.invoke_portal_operation("windows.open-file", &payload)
                })
                .unwrap()
            );
            assert_eq!(app.document_target.as_deref(), Some(entry_id.as_str()));
            assert_eq!(
                recorder.pick_requests()[0].title(),
                "Open a file in Windows"
            );

            app.drain_document_portal();

            assert_eq!(
                *opened_document.lock().unwrap(),
                Some((entry_id, b"portal document bytes".to_vec()))
            );
        }
        // Case `pickers_use_the_shared_portal_and_their_filters`.
        {
            let request = installer_pick_request();
            assert_eq!(request.title(), "Add a Windows application");
            assert_eq!(
                request.filter_names(),
                vec!["Windows applications", "All files"]
            );
            let request = windows_iso_pick_request();
            assert_eq!(request.title(), "Choose Windows installation media");
            assert_eq!(
                request.filter_names(),
                vec!["Windows installation media", "All files"]
            );
            let request = windows_document_pick_request();
            assert_eq!(request.title(), "Open a file in Windows");
            assert_eq!(request.filter_names(), vec!["All files"]);
        }
    }

    #[test]
    fn manager_state_is_honest_and_routes_are_stable() {
        // Case `override_updates_runner_without_changing_capsule_identity`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            let mut doc = AutoCommit::new();
            apply(&mut app, &mut doc, Intent::Route(Route::Vm));
            assert_eq!(app.catalogue.apps[0].route, Route::Vm);
            assert_eq!(app.catalogue.apps[0].capsule, capsule);
            assert!(
                actions::route_override(&doc, &app.catalogue.apps[0].id).is_some(),
                "route changes must write the shared document"
            );
            assert!(
                ids(&app).contains(&"windows-use-recommendation".to_owned()),
                "an overridden route offers the recommendation again"
            );
        }
        // Case `live_manager_does_not_claim_fixture_apps_are_installed`.
        {
            assert!(WindowsApp::new_live().catalogue.apps.is_empty());
        }
        // Case `controls_have_stable_automation_ids`.
        {
            let app = WindowsApp::new_offline();
            assert_eq!(Widget::test_id(&app), Some("windows-app"));
            let ids = ids(&app);
            for id in [
                "windows-install",
                "windows-app-adobe-reader",
                "windows-app-cyberpunk-2077",
                "windows-app-visual-studio",
                "windows-appliance",
                "windows-launch",
                "windows-route",
                "windows-remove-launcher",
                "windows-vendor-uninstall",
                "windows-delete-data",
            ] {
                assert!(ids.iter().any(|have| have == id), "missing {id} in {ids:?}");
            }
        }
        // Case `shared_route_document_is_projected_into_the_compatibility_provider`.
        {
            let mut app = WindowsApp::new_offline();
            let app_id = app.catalogue.apps[0].id.clone();
            let mut doc = AutoCommit::new();
            tilo_actions::Action::apply(
                &actions::WindowsAction::SetRoute {
                    app_id: app_id.clone(),
                    route: Route::Proton,
                },
                &mut doc,
            )
            .unwrap();
            app.project_shared_route_policy(&doc);
            app.refresh();
            assert_eq!(app.catalogue.app(&app_id).unwrap().route, Route::Proton);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn destructive_and_escalating_actions_require_explicit_confirmation() {
        // Case `deleting_private_data_requires_the_dialog_confirmation`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            let mut doc = AutoCommit::new();
            apply(&mut app, &mut doc, Intent::DeleteData);
            assert!(
                app.catalogue
                    .apps
                    .iter()
                    .any(|item| item.capsule == capsule)
            );
            assert_eq!(app.confirm_delete_data.as_deref(), Some(capsule.as_str()));
            assert!(app.dialog_open(), "deleting opens the confirmation dialog");

            // The original management action cannot confirm itself; only the
            // dialog's destructive action can.
            apply(&mut app, &mut doc, Intent::DeleteData);
            assert_eq!(app.confirm_delete_data.as_deref(), Some(capsule.as_str()));
            apply(&mut app, &mut doc, Intent::ConfirmDelete);
            assert!(!app.dialog_open());
            assert!(
                !app.catalogue
                    .apps
                    .iter()
                    .any(|item| item.capsule == capsule)
            );
        }
        // Case `cancelling_the_delete_dialog_keeps_the_app`.
        {
            let mut app = WindowsApp::new_offline_scenario("confirm-delete");
            assert!(app.dialog_open());
            assert_eq!(app.dialog_for, Some(Confirm::Delete));
            let mut doc = AutoCommit::new();
            apply(&mut app, &mut doc, Intent::CancelDelete);
            assert!(!app.dialog_open());
            assert_eq!(app.catalogue.apps.len(), 3);
        }
        // Case `escalation_to_the_shared_windows_appliance_is_confirmed_in_a_dialog`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            app.catalogue.apps[0].can_escalate_install_to_vm = true;
            let mut doc = AutoCommit::new();

            app.intents.borrow_mut().push(Intent::Escalate);
            app.apply_intents(&mut doc);
            assert_eq!(
                app.confirm_escalate_to_vm.as_deref(),
                Some(capsule.as_str())
            );
            assert_eq!(app.catalogue.apps[0].status, crate::model::AppStatus::Ready);

            app.intents.borrow_mut().push(Intent::Escalate);
            app.apply_intents(&mut doc);
            assert_eq!(
                app.catalogue.apps[0].status,
                crate::model::AppStatus::Installing
            );
            assert!(!app.catalogue.apps[0].can_escalate_install_to_vm);
        }
        // Case `changing_selection_drops_the_previous_apps_confirmation`.
        {
            let mut app = WindowsApp::new_offline();
            let mut doc = AutoCommit::new();
            apply(&mut app, &mut doc, Intent::Uninstall);
            assert!(app.dialog_open());
            app.catalogue.apps[1].can_vendor_uninstall = false;
            app.catalogue.apps[1].can_delete_data = false;
            apply(&mut app, &mut doc, Intent::SelectApp(1));
            assert!(!app.dialog_open());
            assert_eq!(app.confirm_vendor_uninstall, None);
            // Actions the newly selected app does not offer do nothing.
            app.intents.borrow_mut().push(Intent::DeleteData);
            app.apply_intents(&mut doc);
            assert_eq!(app.confirm_delete_data, None);
        }
    }

    #[test]
    fn navigation_follows_the_selection() {
        // Case `selecting_a_row_shows_its_page_and_keeps_the_selection_on_back`.
        {
            let mut app = WindowsApp::new_offline();
            let mut doc = AutoCommit::new();
            apply(&mut app, &mut doc, Intent::SelectApp(1));
            assert_eq!(app.page, Page::App);
            assert_eq!(app.selected, 1);
            assert_eq!(app.content_title(), "Cyberpunk 2077");
            apply(&mut app, &mut doc, Intent::SelectAppliance);
            assert_eq!(app.page, Page::Appliance);
            assert_eq!(
                app.selected, 1,
                "the app selection survives a visit to Windows"
            );
        }
        // Case `accessibility_selects_catalogue_rows_by_id`.
        {
            let mut app = WindowsApp::new_offline();
            assert!(Widget::perform_semantic_action(
                &mut app,
                "windows.select:cyberpunk-2077",
                &AccessibilityAction::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(app.selected, 1);
            assert!(!Widget::perform_semantic_action(
                &mut app,
                "windows.select:stale-id",
                &AccessibilityAction::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
        }
        // Case `a_freshly_inspected_installer_asks_where_it_goes`.
        {
            let mut provider = SampleProvider::new();
            provider.seed_install_confirmation();
            let mut app = WindowsApp::with_provider(Box::new(provider));
            app.catalogue.vm_install_confirmation = None;
            app.refresh();
            assert_eq!(app.page, Page::PendingInstall);
            app.rebuild();
            let ids = ids(&app);
            assert!(ids.contains(&"windows-confirm-capsule-install".to_owned()));
            assert!(ids.contains(&"windows-confirm-vm-install".to_owned()));
        }
        // Case `platform_search_finds_apps_by_name_and_opens_them`.
        {
            let mut app = WindowsApp::new_offline();
            let providers = ui::search::capability::search_providers(&app);
            let hits: Vec<_> = providers
                .iter()
                .flat_map(|provider| provider.search("cyberpunk"))
                .collect();
            let hit = hits
                .iter()
                .find(|hit| hit.title == "Cyberpunk 2077")
                .expect("search finds an app by its name");
            Widget::sync(&mut app, &mut AutoCommit::new(), &automerge::ROOT);
            ui::search::capability::activate_search_result(
                &mut app,
                &hit.action_id,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            );
            assert_eq!((app.page, app.selected), (Page::App, 1));
            assert_eq!(app.content_title(), "Cyberpunk 2077");
        }
    }

    #[test]
    fn failures_and_the_appliance_explain_themselves() {
        // Case `failed_setup_offers_recovery_and_disables_launch`.
        {
            let app = WindowsApp::new_offline_scenario("error");
            let failed = app.selected_app().expect("failed sample application");
            assert_eq!(failed.status, crate::model::AppStatus::Failed);
            assert!(!failed.launch_available);
            let ids = ids(&app);
            assert!(ids.contains(&"windows-retry-install".to_owned()));
            assert!(ids.contains(&"windows-escalate-install-to-vm".to_owned()));
        }
        // Case `a_missing_appliance_offers_set_up_and_hides_the_raw_error`.
        {
            let mut app = WindowsApp::new_offline_scenario("appliance-setup");
            let ids_before = ids(&app);
            assert!(ids_before.contains(&"windows-choose-windows-iso".to_owned()));
            assert!(ids_before.contains(&"windows-appliance-details".to_owned()));
            assert!(!app.appliance_details);
            let mut doc = AutoCommit::new();
            apply(&mut app, &mut doc, Intent::ToggleApplianceDetails);
            assert!(app.appliance_details);
        }
        // Case `a_running_appliance_offers_only_the_power_actions_it_allows`.
        {
            let app = WindowsApp::new_offline_scenario("appliance");
            let ids = ids(&app);
            assert!(ids.contains(&"windows-restart-appliance".to_owned()));
            assert!(ids.contains(&"windows-shutdown-appliance".to_owned()));
            assert!(!ids.contains(&"windows-start-appliance".to_owned()));
            assert!(!ids.contains(&"windows-resume-appliance".to_owned()));
        }
    }
}
