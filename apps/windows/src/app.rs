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
use crate::provider::{CompatibilityProvider, EmptyProvider, LiveProvider};
use crate::strings;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::rc::Rc;
use ui::a11y::Action as AccessibilityAction;
use ui::icons::Icon;
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    AppModel, Banner, Button, ButtonRole, Column, Density, Element, Form, FormSection, ListRow,
    ModalDialog, ModelPath, NavPane, NavShell, Property, PropertyList, Row, Scrollable, Section,
    SectionHeader, Text, ToggleGroup, Widget,
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

type Intents = ui::Intents<Intent>;

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

fn action_button(
    id: &str,
    label_key: &str,
    role: ButtonRole,
    intents: &Intents,
    intent: Intent,
) -> Button {
    Button::new(id, &strings::t(label_key))
        .role(role)
        .on_effect(intents.sender(intent))
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

/// The compatibility service as the surface and its route projection share
/// it.
pub(crate) type SharedProvider = Rc<RefCell<Box<dyn CompatibilityProvider>>>;

impl Default for WindowsApp {
    fn default() -> Self {
        Self::disconnected()
    }
}

pub struct WindowsApp {
    provider: SharedProvider,
    pub(crate) catalogue: Catalogue,
    /// Where route edits go: the resource's dispatcher. `None` only for a
    /// widget no resource hosts.
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::Windows>>,
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
    /// The answer owed to the effect still running, given when the picker,
    /// the service or the appliance settles.
    resource_reply: Option<ui::framework::EffectReply>,
    resource_waits_for_appliance: bool,
    /// The routes the service reports, which the shared overrides are
    /// compared against.
    pub(crate) reported_routes: crate::resource::ReportedRoutes,
    /// The shared overrides as last presented, carried to this device's
    /// service while they differ from what it reports; `None` where the
    /// window reaches no service of its own (a capture, a past version).
    routes: Option<Rc<crate::resource::Windows>>,
    /// How the window runs: only a live one carries the overrides to a
    /// service of its own.
    pub(crate) mode: ui::framework::RunMode,
    /// Why carrying the overrides could not reach the service, until the
    /// notice line shows it.
    pub(crate) route_notice: Rc<RefCell<Option<String>>>,
    /// The application a search hit or an agent's `open` named, shown
    /// with the next intents.
    opened: Rc<RefCell<Option<String>>>,
}

impl WindowsApp {
    /// The surface before it knows what it runs for: connected to nothing,
    /// so building it (for a past version too) reaches no service.
    pub fn new() -> Self {
        Self::default()
    }
    fn disconnected() -> Self {
        const DETAIL: &str = "The Tilo compatibility service is not connected.";
        Self::with_parts(
            Box::new(EmptyProvider::with_detail(DETAIL)),
            ApplianceOwnerClient::offline(ApplianceStatus::unavailable(DETAIL)),
        )
    }
    /// Reach the session's compatibility service and the appliance owner, as
    /// a window someone uses does; an unreachable service leaves an honest
    /// empty state saying why.
    pub(crate) fn connect_live(&mut self) {
        self.appliance_owner = ApplianceOwnerClient::new();
        let provider: Box<dyn CompatibilityProvider> = match LiveProvider::connect() {
            Ok(provider) => Box::new(provider),
            Err(detail) => Box::new(EmptyProvider::with_detail(detail)),
        };
        self.use_provider(provider);
    }
    /// The bare widget over the sample catalogue, for tests of its layout
    /// and input.
    #[cfg(test)]
    pub(crate) fn new_offline() -> Self {
        Self::with_provider(Box::new(crate::provider::SampleProvider::new()))
    }
    /// Talk to `provider` from now on.
    pub(crate) fn use_provider(&mut self, provider: Box<dyn CompatibilityProvider>) {
        *self.provider.borrow_mut() = provider;
        self.refresh();
    }
    /// Take part in the resource: route edits go through `dispatcher`.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Windows>,
    ) {
        self.dispatcher = Some(dispatcher);
    }

    /// Carry the shared overrides `state` holds to this device's service.
    pub(crate) fn follow_routes(&mut self, state: &Rc<crate::resource::Windows>) {
        self.routes = Some(Rc::clone(state));
        self.carry_routes();
    }

    /// Bring the service's routes to the shared overrides: an override for
    /// each application whose reported route differs, a reset for one still
    /// running an override nobody holds. Level state, the latest winning; a
    /// failure is shown and tried again when the overrides or the reported
    /// routes next change.
    fn carry_routes(&mut self) {
        let Some(state) = &self.routes else {
            return;
        };
        let changes = self.reported_routes.pending(state);
        if changes.is_empty() {
            return;
        }
        for change in changes {
            if let Err(error) = crate::resource::carry(&self.provider, &change) {
                *self.route_notice.borrow_mut() = Some(error);
                break;
            }
        }
        // Show, and compare against, what the service runs now.
        let catalogue = self.provider.borrow_mut().catalogue();
        self.reported_routes.report(&catalogue);
        self.catalogue = catalogue;
    }
    /// The bare widget over the sample catalogue the capture of `scenario`
    /// shows, staged as that capture is.
    #[cfg(test)]
    pub(crate) fn new_offline_scenario(scenario: &str) -> Self {
        let mut provider = crate::provider::SampleProvider::new();
        if matches!(scenario, "error" | "error-compact") {
            provider.seed_setup_failure();
        }
        if scenario == "pending-install" {
            provider.seed_install_confirmation();
        }
        let mut app = Self::with_provider(Box::new(provider));
        app.stage_scenario(Some(scenario));
        app
    }

    /// Show `scenario` (`None` for the plain capture) over the catalogue
    /// the capture's fixture provides, with an offline appliance that never
    /// runs the owner helper. Captures only: a live window keeps its
    /// appliance.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let scenario = scenario.unwrap_or("wide");
        self.appliance_owner = ApplianceOwnerClient::offline(if scenario == "appliance-setup" {
            ApplianceStatus::unavailable(
                "The Windows appliance command failed: command process failed: \
                 No such file or directory (os error 2)",
            )
        } else {
            ApplianceStatus::sample_ready()
        });
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
        let intents = Intents::default();
        let install = Button::icon_only(
            "windows-install",
            Icon::Plus,
            &strings::t("windows-install"),
        )
        .on_effect(intents.sender(Intent::Install));
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("windows-shell")
            .overview_header(strings::t("windows-apps-title"), vec![Box::new(install)])
            .content_header(strings::t("windows-app-title"), Vec::new());
        let mut app = Self {
            provider: Rc::new(RefCell::new(provider)),
            catalogue,
            dispatcher: None,
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
            resource_reply: None,
            resource_waits_for_appliance: false,
            reported_routes: crate::resource::ReportedRoutes::default(),
            routes: None,
            mode: ui::framework::RunMode::Capture,
            route_notice: Rc::default(),
            opened: Rc::default(),
        };
        app.rebuild();
        app
    }

    /// Ask the host for `effect` as the live row `row` offers it, or on the
    /// manager when there is no row.
    fn queue_live_control(&mut self, row: Option<&str>, effect: &actions::WindowsEffect) -> bool {
        if !self.provider.borrow_mut().action_native_controls() {
            return false;
        }
        if let Some(dispatcher) = &self.dispatcher {
            match row {
                Some(row) => dispatcher.effect_on(row, effect),
                None => dispatcher.effect(effect),
            }
        }
        self.notice = Some(Notice::info(strings::t("windows-request-sent")));
        true
    }

    /// What a search hit or an agent's `open` runs: show the application
    /// its anchor names, as choosing its row does.
    pub(crate) fn opener(&self) -> impl FnMut(&str) -> Result<(), &'static str> + 'static {
        let opened = Rc::clone(&self.opened);
        move |anchor| {
            let app = anchor
                .strip_prefix(crate::search::APP_PREFIX)
                .ok_or("not a Windows application")?;
            *opened.borrow_mut() = Some(app.to_owned());
            Ok(())
        }
    }

    /// Turn a control's intent into the effect search and agents invoke.
    /// The application host re-resolves it against the fresh tree, applies
    /// permission and confirmation policy, records Effects in history, and
    /// only then calls `start_operation`.
    fn queue_live_control_intent(&mut self, intent: Intent) -> bool {
        use actions::WindowsEffect as E;
        if !self.provider.borrow_mut().action_native_controls() {
            return false;
        }
        let appliance = match intent {
            Intent::ChooseIso => Some(E::ImportIso { path: None }),
            Intent::Recovery => Some(E::Recovery),
            Intent::Lifecycle(LifecycleAction::Start) => Some(E::Start),
            Intent::Lifecycle(LifecycleAction::Restart) => Some(E::Restart),
            Intent::Lifecycle(LifecycleAction::Suspend) => Some(E::Suspend),
            Intent::Lifecycle(LifecycleAction::Resume) => Some(E::Resume),
            Intent::Lifecycle(LifecycleAction::Shutdown) => Some(E::Shutdown),
            Intent::ConnectActivation => Some(E::TemporaryNetwork),
            Intent::ConnectUpdate => Some(E::UpdateNetwork),
            Intent::RevokeConnection => Some(E::RevokeNetwork),
            _ => None,
        };
        if let Some(effect) = appliance {
            return self.queue_live_control(Some(actions::APPLIANCE_ROW), &effect);
        }
        match intent {
            Intent::Install => return self.queue_live_control(None, &E::Install { path: None }),
            Intent::ConfirmCapsule => {
                return self.queue_live_control(
                    Some(actions::PENDING_INSTALL_ROW),
                    &E::InstallPrivateCapsule,
                );
            }
            Intent::ConfirmVm => {
                return self.queue_live_control(
                    Some(actions::PENDING_INSTALL_ROW),
                    &E::InstallWindowsAppliance,
                );
            }
            _ => {}
        }
        let Some(app) = self.selected_app() else {
            return false;
        };
        let row = crate::search::app_resource_id(&app.id);
        let (id, capsule) = (app.id.clone(), app.capsule.clone());
        // A route edit is a document edit, not a service control: it goes
        // through the resource's dispatcher.
        let effect = match intent {
            Intent::OpenFile => E::OpenFile {
                app: id,
                path: None,
            },
            Intent::Launch => E::Launch { app: id },
            Intent::VmFallback => E::LaunchVmFallback { app: id },
            Intent::Publish => E::Publish { app: id },
            Intent::Retry => E::RetryInstall { capsule },
            Intent::Escalate => E::EscalateInstall { capsule },
            Intent::RemoveLauncher => E::RemoveLauncher { app: id },
            Intent::Uninstall => E::Uninstall { app: id },
            Intent::DeleteData => E::DeleteData { capsule },
            _ => return false,
        };
        self.queue_live_control(Some(&row), &effect)
    }
    pub(crate) fn selected_app(&self) -> Option<&crate::model::WindowsApp> {
        self.catalogue.apps.get(self.selected)
    }
    /// Submit a route edit through the resource's dispatcher; the route
    /// projection then carries it to the service on every device.
    fn dispatch(&self, edit: ui::tilo_resource::Edit<crate::resource::Windows>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(edit);
        }
    }

    fn finish_resource_operation(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.resource_reply.take() {
            reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    fn cancel_resource_operation(&mut self, message: &str) {
        self.finish_resource_operation(Err(message.to_string()));
    }

    /// Whether an effect is still waiting for its answer.
    pub(crate) fn is_busy(&self) -> bool {
        self.resource_reply.is_some()
    }

    /// Answer `effect`: ask the service, the appliance or the file chooser,
    /// and answer at once or once that settles.
    pub(crate) fn answer_effect(
        &mut self,
        effect: &actions::WindowsEffect,
        cx: &mut ui::framework::EffectCx<crate::resource::Windows, Self>,
    ) -> ui::framework::EffectAnswer {
        if let actions::WindowsEffect::Show { app } = effect {
            let index = self
                .catalogue
                .apps
                .iter()
                .position(|item| item.id == *app)
                .ok_or_else(|| "the application is no longer in the catalogue".to_owned())?;
            self.apply_intent(Intent::SelectApp(index));
            return Ok(ui::framework::Effected::done());
        }
        if self.resource_reply.is_some() {
            return Err("another Windows operation is still running".into());
        }
        let (type_id, _) = effect.operation();
        if !self.invoke_effect_operation(type_id, &effect.payload())? {
            return Err(format!("`{type_id}` is not a Windows operation"));
        }
        let deferred = self.portal_rx.is_some()
            || self.document_portal_rx.is_some()
            || self.iso_portal_rx.is_some()
            || self.appliance_owner.busy()
            || !self.provider.borrow_mut().operations_complete_on_return();
        if !deferred {
            return Ok(ui::framework::Effected::done());
        }
        self.resource_waits_for_appliance = effect.waits_for_appliance();
        self.resource_reply = Some(cx.reply());
        Ok(ui::framework::Effected::Pending)
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
        let result = self.provider.borrow_mut().delete_data(&capsule);
        self.report(result, "windows-data-deleted");
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
        let result = self.provider.borrow_mut().install(source, source_name);
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
                            .borrow_mut()
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
        if self.resource_reply.is_some()
            && self.resource_waits_for_appliance
            && let Some(result) = self.appliance_owner.take_action_result()
        {
            self.finish_resource_operation(result.map(|_| ()));
        }
        let had_pending = self.catalogue.vm_install_confirmation.is_some();
        let catalogue = self.provider.borrow_mut().catalogue();
        self.catalogue = catalogue;
        if self.reported_routes.report(&self.catalogue) {
            self.carry_routes();
        }
        if let Some(notice) = self.route_notice.borrow_mut().take() {
            self.notice = Some(Notice::error(notice));
        }
        let provider_notice = self.provider.borrow_mut().take_notice();
        if let Some(notice) = provider_notice {
            self.notice = Some(Notice::info(notice));
        }
        let result = (self.resource_reply.is_some() && !self.resource_waits_for_appliance)
            .then(|| self.provider.borrow_mut().take_operation_result())
            .flatten();
        if let Some(result) = result {
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
    pub(crate) fn apply_intents(&mut self) {
        let opened = self.opened.borrow_mut().take();
        if let Some(index) =
            opened.and_then(|app| self.catalogue.apps.iter().position(|item| item.id == app))
        {
            self.apply_intent(Intent::SelectApp(index));
        }
        let pending = self.intents.take();
        for intent in pending {
            self.apply_intent(intent);
        }
    }

    fn apply_intent(&mut self, intent: Intent) {
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
                    self.apply_app_intent(other);
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
                let result = self.provider.borrow_mut().confirm_capsule_install();
                self.report(result, "windows-capsule-setup-started");
                self.refresh();
            }
            Intent::ConfirmVm if self.catalogue.vm_install_confirmation.is_some() => {
                let result = self.provider.borrow_mut().confirm_vm_install();
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
    fn apply_app_intent(&mut self, intent: Intent) {
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
                let routes = crate::resource::Windows::paths().routes;
                self.dispatch(routes.key(app.id).set(route));
                self.notice = Some(Notice::info(strings::t("windows-route-set")));
            }
            Intent::ResetRoute if app.route_override_available && app.uses_override() => {
                let routes = crate::resource::Windows::paths().routes;
                self.dispatch(routes.remove(app.id));
                self.notice = Some(Notice::info(strings::t("windows-route-reset")));
            }
            Intent::Launch if app.launch_available => {
                let result = if app.route == Route::Vm {
                    self.provider.borrow_mut().launch_vm_fallback(&app.id)
                } else {
                    self.provider.borrow_mut().launch(&app.id)
                };
                self.report(result, "windows-launch-queued");
            }
            Intent::VmFallback if app.route != Route::Vm && app.vm_fallback_available => {
                let result = self.provider.borrow_mut().launch_vm_fallback(&app.id);
                self.report(result, "windows-vm-launch-queued");
            }
            Intent::Publish if app.can_publish_vm_candidate => {
                let result = self.provider.borrow_mut().publish_vm_candidate(&app.id);
                self.report(result, "windows-vm-candidate-publishing");
                self.refresh();
            }
            Intent::Retry if app.can_retry_install => {
                let result = self.provider.borrow_mut().retry_install(&app.capsule);
                self.report(result, "windows-retrying");
            }
            Intent::Escalate if app.can_escalate_install_to_vm => {
                if self.confirm_escalate_to_vm.as_deref() == Some(app.capsule.as_str()) {
                    self.confirm_escalate_to_vm = None;
                    let result = self
                        .provider
                        .borrow_mut()
                        .escalate_install_to_vm(&app.capsule);
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
                let result = self.provider.borrow_mut().remove_launcher(&app.id);
                self.report(result, "windows-launcher-removed");
            }
            Intent::Uninstall if app.can_vendor_uninstall => {
                let vm_application = app.route == Route::Vm;
                let management_id = if vm_application { app.id } else { app.capsule };
                if self.confirm_vendor_uninstall.as_deref() == Some(management_id.as_str()) {
                    self.confirm_vendor_uninstall = None;
                    let result = if vm_application {
                        self.provider
                            .borrow_mut()
                            .vendor_uninstall_vm(&management_id)
                    } else {
                        self.provider.borrow_mut().vendor_uninstall(&management_id)
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
                    self.provider.borrow_mut().install(source, source_name)?;
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
            "windows.confirm-capsule-install" => {
                self.provider.borrow_mut().confirm_capsule_install()?;
            }
            "windows.confirm-vm-install" => self.provider.borrow_mut().confirm_vm_install()?,
            "windows.open-file" => {
                let (app_id, file_path): (String, String) = serde_json::from_slice(payload)
                    .map_err(|_| "open-file names the Windows app and the file".to_string())?;
                let app_id = &app_id;
                let file_path = file_path.as_str();
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
                    self.provider
                        .borrow_mut()
                        .launch_vm_with_file(app_id, source)?;
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
                    self.provider.borrow_mut().launch_vm_fallback(&app.id)?;
                } else {
                    self.provider.borrow_mut().launch(&app.id)?;
                }
            }
            "windows.launch-vm-fallback" if app.vm_fallback_available => {
                self.provider.borrow_mut().launch_vm_fallback(&app.id)?;
            }
            "windows.publish" if app.can_publish_vm_candidate => {
                self.provider.borrow_mut().publish_vm_candidate(&app.id)?;
            }
            "windows.retry-install" if app.can_retry_install => {
                self.provider.borrow_mut().retry_install(&app.capsule)?;
            }
            "windows.escalate-install" if app.can_escalate_install_to_vm => {
                self.provider
                    .borrow_mut()
                    .escalate_install_to_vm(&app.capsule)?;
            }
            "windows.remove-launcher" if app.can_remove_launcher => {
                self.provider.borrow_mut().remove_launcher(&app.id)?;
            }
            "windows.uninstall" if app.can_vendor_uninstall => {
                if app.route == Route::Vm {
                    self.provider.borrow_mut().vendor_uninstall_vm(&app.id)?;
                } else {
                    self.provider.borrow_mut().vendor_uninstall(&app.capsule)?;
                }
            }
            "windows.delete-data" if app.can_delete_data => {
                self.provider.borrow_mut().delete_data(&app.capsule)?;
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
                .on_view(self.intents.sender(Intent::SelectPending));
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
                        .badge(app.route.label())
                        .selected(self.page == Page::App && index == self.selected)
                        .on_view(self.intents.sender(Intent::SelectApp(index)));
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
            .on_view(self.intents.sender(Intent::SelectAppliance));
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
        // The head (a notice, the app's summary) above its settings: a page
        // of sections on the readable column.
        let mut content = page.head.into_iter().fold(
            ui::widget::Page::new()
                .with_semantic_key("windows-content")
                .with_persisted_offset(Rc::clone(&self.content_scroll))
                .with_cursor(self.cursor),
            ui::widget::Page::head,
        );
        if !page.sections.is_empty() {
            let form = page.sections.into_iter().fold(
                Form::new().with_density(Some(Density::Compact)),
                Form::section,
            );
            content = content.section(Box::new(form));
        }
        Box::new(content)
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
            // The ways out of a failed setup are the banner's own actions.
            if app.can_retry_install {
                banner = banner.with_action(action_button(
                    "windows-retry-install",
                    "windows-retry-install",
                    ButtonRole::Primary,
                    intents,
                    Intent::Retry,
                ));
            }
            if app.can_escalate_install_to_vm {
                let escalate = action_button(
                    "windows-escalate-install-to-vm",
                    "windows-escalate-install-to-vm",
                    ButtonRole::Secondary,
                    intents,
                    Intent::Escalate,
                );
                banner = if app.can_retry_install {
                    banner.with_secondary_action(escalate)
                } else {
                    banner.with_action(escalate)
                };
            }
            page.head.push(Box::new(banner));
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
                let intents = intents.clone();
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
                        intents.push(Intent::Route(*route));
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
        let facts = ui::widget::PropertyColumn::new();
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
            access.push(Box::new(PropertyList::new(reach).shared_column(&facts)));
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
            vec![Box::new(PropertyList::new(about).shared_column(&facts))],
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
                .on_effect(intents.sender(intent))
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
            // Setting Windows up resolves the notice, so the banner holds it.
            let mut banner = banner
                .with_message(strings::t("windows-appliance-not-installed-body"))
                .with_action(
                    action_button(
                        "windows-choose-windows-iso",
                        "windows-appliance-set-up",
                        ButtonRole::Primary,
                        intents,
                        Intent::ChooseIso,
                    )
                    .with_enabled(!busy),
                );
            if status.detail.is_some() {
                banner = banner.with_secondary_action(action_button(
                    "windows-appliance-details",
                    if self.appliance_details {
                        "windows-hide-details"
                    } else {
                        "windows-show-details"
                    },
                    ButtonRole::Plain,
                    intents,
                    Intent::ToggleApplianceDetails,
                ));
            }
            page.head.push(Box::new(banner));
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
            .primary_action(action, self.intents.sender(confirm_intent))
            .secondary_action(strings::t("windows-cancel"), self.intents.sender(cancel))
            .on_dismiss(self.intents.sender(cancel))
            .with_destructive(destructive)
            .open(true)
    }

    /// Apply queued intents, then bring the columns up to date.
    fn settle(&mut self) {
        self.apply_intents();
        self.rebuild();
    }

    fn dialog_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(ModalDialog::is_open)
    }
}

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
        dyn ui::widget::controls::ActionControls;
        inner = shell
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.drain_portal();
        self.drain_document_portal();
        self.drain_iso_portal();
        self.refresh();
        self.settle();
        self.shell.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
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
                    self.intents.push(Intent::SelectApp(next));
                    *ctx.handled = true;
                }
            }
        }
        self.settle();
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
        state: &mut ui::widget::Edits<'_>,
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
            self.intents.push(intent);
            true
        } else {
            self.shell
                .perform_semantic_action(target, action, state, path)
        };
        self.settle();
        handled
    }
    fn test_id(&self) -> Option<&str> {
        Some("windows-app")
    }
}

impl ui::widget::controls::ActionControls for WindowsApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        ui::widget::controls::ActionControls::take_control_invocation(&mut self.shell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SampleProvider;
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    /// A live window brings its service's routes to the shared overrides
    /// when they change, and when the service reports routes that differ;
    /// one reaching no service of its own carries nothing.
    #[test]
    fn the_window_keeps_its_service_in_step_with_the_shared_overrides() {
        use crate::model::Route;
        let route = |app: &WindowsApp| {
            app.provider
                .borrow_mut()
                .catalogue()
                .app("adobe-reader")
                .map(|reader| reader.route)
        };
        let overridden = Rc::new(crate::resource::Windows {
            routes: ui::tilo_resource::Map::from_iter([("adobe-reader", Route::Proton)]),
        });

        let mut capture = WindowsApp::new_offline();
        capture.refresh();
        let before = route(&capture);
        ui::framework::ResourceSurface::present(&mut capture, &overridden);
        assert_eq!(route(&capture), before, "a capture reaches no service");

        let mut app = WindowsApp::new_offline();
        app.mode = ui::framework::RunMode::Live;
        app.refresh();
        ui::framework::ResourceSurface::present(&mut app, &overridden);
        assert_eq!(route(&app), Some(Route::Proton), "the override is carried");

        // The service reverting it on its own is brought back.
        app.provider
            .borrow_mut()
            .reset_route_override("adobe-reader")
            .unwrap();
        app.refresh();
        assert_eq!(route(&app), Some(Route::Proton));

        // Clearing the override resets the service.
        ui::framework::ResourceSurface::present(
            &mut app,
            &Rc::new(crate::resource::Windows::default()),
        );
        assert_ne!(route(&app), Some(Route::Proton));
    }

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

    fn apply(app: &mut WindowsApp, intent: Intent) {
        app.intents.push(intent);
        app.settle();
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
            ui::host::scoped(&services, || apply(&mut app, Intent::Install));
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
        // Case `route_edits_reach_the_shared_document`.
        {
            let mut app =
                ui::prelude::TestApp::<crate::resource::WindowsShell>::capture(Some("wide"))
                    .unwrap();
            let first = crate::provider::SampleProvider::new().catalogue().apps[0]
                .id
                .clone();
            app.harness()
                .perform_named("windows-route:segment:2", &AccessibilityAction::Activate)
                .expect("the shown app's route choice");
            app.settle();
            assert_eq!(app.state().route(&first), Some(Route::Vm));
        }
        // Case `an_unconnected_manager_does_not_claim_fixture_apps_are_installed`.
        {
            assert!(WindowsApp::new().catalogue.apps.is_empty());
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
    }

    #[test]
    fn destructive_and_escalating_actions_require_explicit_confirmation() {
        // Case `deleting_private_data_requires_the_dialog_confirmation`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            apply(&mut app, Intent::DeleteData);
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
            apply(&mut app, Intent::DeleteData);
            assert_eq!(app.confirm_delete_data.as_deref(), Some(capsule.as_str()));
            apply(&mut app, Intent::ConfirmDelete);
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
            apply(&mut app, Intent::CancelDelete);
            assert!(!app.dialog_open());
            assert_eq!(app.catalogue.apps.len(), 3);
        }
        // Case `escalation_to_the_shared_windows_appliance_is_confirmed_in_a_dialog`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            app.catalogue.apps[0].can_escalate_install_to_vm = true;

            app.intents.push(Intent::Escalate);
            app.apply_intents();
            assert_eq!(
                app.confirm_escalate_to_vm.as_deref(),
                Some(capsule.as_str())
            );
            assert_eq!(app.catalogue.apps[0].status, crate::model::AppStatus::Ready);

            app.intents.push(Intent::Escalate);
            app.apply_intents();
            assert_eq!(
                app.catalogue.apps[0].status,
                crate::model::AppStatus::Installing
            );
            assert!(!app.catalogue.apps[0].can_escalate_install_to_vm);
        }
        // Case `changing_selection_drops_the_previous_apps_confirmation`.
        {
            let mut app = WindowsApp::new_offline();
            apply(&mut app, Intent::Uninstall);
            assert!(app.dialog_open());
            app.catalogue.apps[1].can_vendor_uninstall = false;
            app.catalogue.apps[1].can_delete_data = false;
            apply(&mut app, Intent::SelectApp(1));
            assert!(!app.dialog_open());
            assert_eq!(app.confirm_vendor_uninstall, None);
            // Actions the newly selected app does not offer do nothing.
            app.intents.push(Intent::DeleteData);
            app.apply_intents();
            assert_eq!(app.confirm_delete_data, None);
        }
    }

    #[test]
    fn navigation_follows_the_selection() {
        // Case `selecting_a_row_shows_its_page_and_keeps_the_selection_on_back`.
        {
            let mut app = WindowsApp::new_offline();
            apply(&mut app, Intent::SelectApp(1));
            assert_eq!(app.page, Page::App);
            assert_eq!(app.selected, 1);
            assert_eq!(app.content_title(), "Cyberpunk 2077");
            apply(&mut app, Intent::SelectAppliance);
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
                &mut ui::widget::Edits::lend(&mut automerge::AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(app.selected, 1);
            assert!(!Widget::perform_semantic_action(
                &mut app,
                "windows.select:stale-id",
                &AccessibilityAction::Activate,
                &mut ui::widget::Edits::lend(&mut automerge::AutoCommit::new()),
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
            let mut app = ui::prelude::TestApp::<crate::resource::WindowsShell>::new().unwrap();
            app.with_surface::<WindowsApp, _>(|surface| {
                surface.use_provider(Box::new(SampleProvider::new()));
                surface.stage_scenario(Some("wide"));
            })
            .expect("authored catalogue over the live action host");
            app.settle();
            let providers = ui::search::capability::search_providers(app.harness().root_widget());
            let hit = providers
                .iter()
                .flat_map(|provider| provider.search("cyberpunk"))
                .find(|hit| hit.title == "Cyberpunk 2077")
                .expect("search finds an app by its name");
            assert_eq!(
                hit.action_id,
                crate::search::app_resource_id("cyberpunk-2077"),
                "a hit is anchored at the app's row"
            );
            app.call("open", serde_json::json!({ "anchor": hit.action_id }))
                .expect("the hit opens");
            app.settle();
            app.harness()
                .expect(
                    &ui::headless::SemanticExpectation::named("windows-app-cyberpunk-2077")
                        .selected(true),
                )
                .expect("the hit's app is the one shown");
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
            apply(&mut app, Intent::ToggleApplianceDetails);
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
