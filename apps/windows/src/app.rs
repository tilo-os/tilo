//! Immediate-mode `QuickUI` surface for compatibility routing.
use crate::actions;
use crate::appliance_owner::{ApplianceOwnerClient, EgressPurpose, LifecycleAction};
use crate::model::{Catalogue, Route};
use crate::provider::{CompatibilityProvider, EmptyProvider, LiveProvider, SampleProvider};
use crate::strings;
use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::rc::Rc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Politeness, Role};
use ui::paint::PaintCtx;
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, ButtonStyle, ModalAction, ModalFrame, ModalHit, ModalLayout, ModalSpec, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

const PAD: f64 = 20.0;
const SIDEBAR_W: f64 = 296.0;
const ROW_H: f64 = 72.0;
// The desktop master/detail surface needs enough room for the status and
// guest-file controls as well as the route selector. Below this width a list
// → detail flow is clearer and, importantly, never paints an actionable
// control into the window's edge padding.
const COMPACT_W: f64 = 860.0;

fn compact_portal_reason(reason: &str) -> String {
    if reason.chars().count() > 52 {
        format!("{}…", reason.chars().take(51).collect::<String>())
    } else {
        reason.to_string()
    }
}

/// Immediate-mode text does not wrap individual property values. Keep compact
/// details within their value column instead of letting a long server-owned
/// runner or grant description disappear beyond the right edge; the complete
/// value remains available in the accessibility description.
fn compact_detail_value(value: &str, compact: bool) -> String {
    const COMPACT_VALUE_CHARS: usize = 34;
    if compact && value.chars().count() > COMPACT_VALUE_CHARS {
        format!(
            "{}…",
            value
                .chars()
                .take(COMPACT_VALUE_CHARS.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        value.to_string()
    }
}

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

#[derive(Clone, Copy)]
enum Hit {
    App(usize),
    Back,
}

pub struct WindowsApp {
    provider: Box<dyn CompatibilityProvider>,
    pub(crate) catalogue: Catalogue,
    actions: crate::resource::ActionSink,
    selected: usize,
    /// Compact windows use a list → detail flow rather than squeezing the
    /// desktop split until both panes become unreadable.
    compact_detail: bool,
    area: Rect,
    mouse: Point,
    /// The management card and catalogue share a deliberately bounded
    /// sidebar viewport. Small windows scroll this complete column rather
    /// than leaving the catalogue below an expanded appliance card.
    sidebar_scroll: f64,
    sidebar_content_bottom: f64,
    hits: Vec<(Rect, Hit)>,
    node: Option<NodeId>,
    notice: Option<String>,
    route_buttons: Vec<Button>,
    reset_button: Button,
    launch_button: Button,
    open_file_button: Button,
    vm_fallback_button: Button,
    install_button: Button,
    confirm_capsule_button: Button,
    confirm_vm_install_button: Button,
    publish_vm_candidate_button: Button,
    retry_button: Button,
    escalate_to_vm_button: Button,
    remove_launcher_button: Button,
    vendor_uninstall_button: Button,
    delete_data_button: Button,
    choose_windows_iso_button: Button,
    recovery_desktop_button: Button,
    start_windows_button: Button,
    restart_windows_button: Button,
    suspend_windows_button: Button,
    resume_windows_button: Button,
    shutdown_windows_button: Button,
    connect_windows_button: Button,
    update_windows_button: Button,
    revoke_windows_connection_button: Button,
    route_signal: Rc<Cell<Option<Route>>>,
    reset_signal: Rc<Cell<bool>>,
    launch_signal: Rc<Cell<bool>>,
    open_file_signal: Rc<Cell<bool>>,
    vm_fallback_signal: Rc<Cell<bool>>,
    install_signal: Rc<Cell<bool>>,
    confirm_capsule_signal: Rc<Cell<bool>>,
    confirm_vm_install_signal: Rc<Cell<bool>>,
    publish_vm_candidate_signal: Rc<Cell<bool>>,
    retry_signal: Rc<Cell<bool>>,
    escalate_to_vm_signal: Rc<Cell<bool>>,
    remove_launcher_signal: Rc<Cell<bool>>,
    vendor_uninstall_signal: Rc<Cell<bool>>,
    delete_data_signal: Rc<Cell<bool>>,
    choose_windows_iso_signal: Rc<Cell<bool>>,
    recovery_desktop_signal: Rc<Cell<bool>>,
    start_windows_signal: Rc<Cell<bool>>,
    restart_windows_signal: Rc<Cell<bool>>,
    suspend_windows_signal: Rc<Cell<bool>>,
    resume_windows_signal: Rc<Cell<bool>>,
    shutdown_windows_signal: Rc<Cell<bool>>,
    connect_windows_signal: Rc<Cell<bool>>,
    update_windows_signal: Rc<Cell<bool>>,
    revoke_windows_connection_signal: Rc<Cell<bool>>,
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
    /// Immediate-mode modal geometry from the latest frame. It is the single
    /// pointer-capture boundary while destructive deletion is awaiting an
    /// explicit confirm or cancel.
    delete_confirmation_layout: Option<ModalLayout>,
}

impl WindowsApp {
    fn clamp_sidebar_scroll(&mut self) {
        let max = (self.sidebar_content_bottom - self.area.y1).max(0.0);
        self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, max);
    }

    pub fn new_live() -> Self {
        match LiveProvider::connect() {
            Ok(provider) => Self::with_provider(Box::new(provider)),
            Err(detail) => Self::with_provider(Box::new(EmptyProvider::with_detail(detail))),
        }
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
    /// These never connect to the compatibility service.
    pub fn new_offline_scenario(scenario: &str) -> Self {
        let mut provider = SampleProvider::new();
        if matches!(scenario, "error" | "error-compact") {
            provider.seed_setup_failure();
        }
        let mut app = Self::with_provider(Box::new(provider));
        match scenario {
            "detail" | "error-compact" => app.compact_detail = true,
            "running" => app.selected = 1,
            "vm-detail" => {
                app.selected = 2;
                app.compact_detail = true;
            }
            "confirm-delete" => {
                app.compact_detail = true;
                if let Some(first) = app.catalogue.apps.first() {
                    app.confirm_delete_data = Some(first.capsule.clone());
                    app.notice = Some(strings::t("windows-confirm-delete"));
                }
            }
            _ => {}
        }
        app
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the constructor keeps retained controls and their signals adjacent for auditable UI wiring"
    )]
    pub fn with_provider(mut provider: Box<dyn CompatibilityProvider>) -> Self {
        // Retained controls may resolve and cache labels during their first
        // sync, before this canvas paints its first translated heading.
        // Register the app bundle before constructing any of them.
        let _ = strings::t("windows-app-title");
        let catalogue = provider.catalogue();
        let route_signal = Rc::new(Cell::new(None));
        let reset_signal = Rc::new(Cell::new(false));
        let launch_signal = Rc::new(Cell::new(false));
        let open_file_signal = Rc::new(Cell::new(false));
        let vm_fallback_signal = Rc::new(Cell::new(false));
        let install_signal = Rc::new(Cell::new(false));
        let retry_signal = Rc::new(Cell::new(false));
        let escalate_to_vm_signal = Rc::new(Cell::new(false));
        let remove_launcher_signal = Rc::new(Cell::new(false));
        let vendor_uninstall_signal = Rc::new(Cell::new(false));
        let delete_data_signal = Rc::new(Cell::new(false));
        let choose_windows_iso_signal = Rc::new(Cell::new(false));
        let recovery_desktop_signal = Rc::new(Cell::new(false));
        let start_windows_signal = Rc::new(Cell::new(false));
        let restart_windows_signal = Rc::new(Cell::new(false));
        let suspend_windows_signal = Rc::new(Cell::new(false));
        let resume_windows_signal = Rc::new(Cell::new(false));
        let shutdown_windows_signal = Rc::new(Cell::new(false));
        let connect_windows_signal = Rc::new(Cell::new(false));
        let update_windows_signal = Rc::new(Cell::new(false));
        let revoke_windows_connection_signal = Rc::new(Cell::new(false));
        let route_buttons = Route::ALL
            .iter()
            .map(|route| {
                let signal = Rc::clone(&route_signal);
                let route = *route;
                Button::new(
                    &format!("windows-route-{}", route.short().to_ascii_lowercase()),
                    route.button_key(),
                    None,
                )
                .with_size(108.0, 32.0)
                .with_enabled(false)
                .on_effect(move || signal.set(Some(route)))
            })
            .collect();
        let reset = Rc::clone(&reset_signal);
        let launch = Rc::clone(&launch_signal);
        let open_file = Rc::clone(&open_file_signal);
        let vm_fallback = Rc::clone(&vm_fallback_signal);
        let install = Rc::clone(&install_signal);
        let confirm_capsule_signal = Rc::new(Cell::new(false));
        let confirm_vm_install_signal = Rc::new(Cell::new(false));
        let publish_vm_candidate_signal = Rc::new(Cell::new(false));
        let retry = Rc::clone(&retry_signal);
        let escalate_to_vm = Rc::clone(&escalate_to_vm_signal);
        let remove_launcher = Rc::clone(&remove_launcher_signal);
        let vendor_uninstall = Rc::clone(&vendor_uninstall_signal);
        let delete_data = Rc::clone(&delete_data_signal);
        let choose_windows_iso = Rc::clone(&choose_windows_iso_signal);
        let recovery_desktop = Rc::clone(&recovery_desktop_signal);
        let start_windows = Rc::clone(&start_windows_signal);
        let restart_windows = Rc::clone(&restart_windows_signal);
        let suspend_windows = Rc::clone(&suspend_windows_signal);
        let resume_windows = Rc::clone(&resume_windows_signal);
        let shutdown_windows = Rc::clone(&shutdown_windows_signal);
        let connect_windows = Rc::clone(&connect_windows_signal);
        let update_windows = Rc::clone(&update_windows_signal);
        let revoke_windows_connection = Rc::clone(&revoke_windows_connection_signal);
        let projected_route_policy = catalogue
            .apps
            .iter()
            .map(|app| (app.id.clone(), None))
            .collect();
        Self {
            provider,
            catalogue,
            actions: crate::resource::ActionSink::default(),
            selected: 0,
            compact_detail: false,
            area: Rect::ZERO,
            mouse: Point::ZERO,
            sidebar_scroll: 0.0,
            sidebar_content_bottom: 0.0,
            hits: Vec::new(),
            node: None,
            notice: None,
            route_buttons,
            reset_button: Button::new(
                "windows-use-recommendation",
                "windows-use-recommendation",
                None,
            )
            .with_size(195.0, 32.0)
            .with_enabled(false)
            .on_effect(move || reset.set(true)),
            launch_button: Button::new("windows-launch", "windows-launch", None)
                .with_size(132.0, 32.0)
                .with_enabled(false)
                .on_effect(move || launch.set(true)),
            open_file_button: Button::new(
                "windows-open-file-in-vm",
                "windows-open-file-in-vm",
                None,
            )
            .with_size(164.0, 32.0)
            .with_enabled(false)
            .on_effect(move || open_file.set(true)),
            vm_fallback_button: Button::new("windows-open-in-vm", "windows-open-in-vm", None)
                .with_size(174.0, 32.0)
                .with_enabled(false)
                .on_effect(move || vm_fallback.set(true)),
            install_button: Button::new("windows-install", "windows-install", None)
                .with_size(256.0, 32.0)
                .on_effect(move || install.set(true)),
            confirm_capsule_button: Button::new(
                "windows-confirm-capsule-install",
                "windows-install-private-capsule",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_effect({
                let signal = Rc::clone(&confirm_capsule_signal);
                move || signal.set(true)
            }),
            confirm_vm_install_button: Button::new(
                "windows-confirm-vm-install",
                "windows-install-windows-appliance",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_effect({
                let signal = Rc::clone(&confirm_vm_install_signal);
                move || signal.set(true)
            }),
            publish_vm_candidate_button: Button::new(
                "windows-publish-vm-candidate",
                "windows-publish-vm-candidate",
                None,
            )
            .with_size(190.0, 32.0)
            .with_enabled(false)
            .on_effect({
                let signal = Rc::clone(&publish_vm_candidate_signal);
                move || signal.set(true)
            }),
            retry_button: Button::new("windows-retry-install", "windows-retry-install", None)
                .with_size(118.0, 32.0)
                .with_enabled(false)
                .on_effect(move || retry.set(true)),
            escalate_to_vm_button: Button::new(
                "windows-escalate-install-to-vm",
                "windows-escalate-install-to-vm",
                None,
            )
            .with_size(220.0, 32.0)
            .with_enabled(false)
            .on_effect(move || escalate_to_vm.set(true)),
            remove_launcher_button: Button::new(
                "windows-remove-launcher",
                "windows-remove-launcher",
                None,
            )
            .with_size(142.0, 32.0)
            .with_enabled(false)
            .on_effect(move || remove_launcher.set(true)),
            vendor_uninstall_button: Button::new(
                "windows-vendor-uninstall",
                "windows-vendor-uninstall",
                None,
            )
            .with_size(142.0, 32.0)
            .with_enabled(false)
            .on_effect(move || vendor_uninstall.set(true)),
            delete_data_button: Button::new("windows-delete-data", "windows-delete-data", None)
                .with_size(132.0, 32.0)
                .with_enabled(false)
                .on_effect(move || delete_data.set(true)),
            choose_windows_iso_button: Button::new(
                "windows-choose-windows-iso",
                "windows-choose-windows-iso",
                None,
            )
            .with_size(256.0, 32.0)
            .on_effect(move || choose_windows_iso.set(true)),
            recovery_desktop_button: Button::new(
                "windows-open-recovery-desktop",
                "windows-open-recovery-desktop",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_effect(move || recovery_desktop.set(true)),
            start_windows_button: Button::new(
                "windows-start-appliance",
                "windows-start-appliance",
                None,
            )
            .with_size(118.0, 32.0)
            .with_enabled(false)
            .on_effect(move || start_windows.set(true)),
            restart_windows_button: Button::new(
                "windows-restart-appliance",
                "windows-restart-appliance",
                None,
            )
            .with_size(118.0, 32.0)
            .with_enabled(false)
            .on_effect(move || restart_windows.set(true)),
            suspend_windows_button: Button::new(
                "windows-suspend-appliance",
                "windows-suspend-appliance",
                None,
            )
            .with_size(118.0, 32.0)
            .with_enabled(false)
            .on_effect(move || suspend_windows.set(true)),
            resume_windows_button: Button::new(
                "windows-resume-appliance",
                "windows-resume-appliance",
                None,
            )
            .with_size(118.0, 32.0)
            .with_enabled(false)
            .on_effect(move || resume_windows.set(true)),
            shutdown_windows_button: Button::new(
                "windows-shutdown-appliance",
                "windows-shutdown-appliance",
                None,
            )
            .with_size(118.0, 32.0)
            .with_enabled(false)
            .on_effect(move || shutdown_windows.set(true)),
            connect_windows_button: Button::new(
                "windows-connect-temporarily",
                "windows-connect-for-activation",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_effect(move || connect_windows.set(true)),
            update_windows_button: Button::new(
                "windows-connect-for-update",
                "windows-connect-for-update",
                None,
            )
            .with_size(118.0, 32.0)
            .with_enabled(false)
            .on_effect(move || update_windows.set(true)),
            revoke_windows_connection_button: Button::new(
                "windows-revoke-temporary-connection",
                "windows-revoke-temporary-connection",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_effect(move || revoke_windows_connection.set(true)),
            route_signal,
            reset_signal,
            launch_signal,
            open_file_signal,
            vm_fallback_signal,
            install_signal,
            confirm_capsule_signal,
            confirm_vm_install_signal,
            publish_vm_candidate_signal,
            retry_signal,
            escalate_to_vm_signal,
            remove_launcher_signal,
            vendor_uninstall_signal,
            delete_data_signal,
            choose_windows_iso_signal,
            recovery_desktop_signal,
            start_windows_signal,
            restart_windows_signal,
            suspend_windows_signal,
            resume_windows_signal,
            shutdown_windows_signal,
            connect_windows_signal,
            update_windows_signal,
            revoke_windows_connection_signal,
            portal_rx: None,
            document_portal_rx: None,
            document_target: None,
            iso_portal_rx: None,
            appliance_owner: ApplianceOwnerClient::new(),
            confirm_escalate_to_vm: None,
            confirm_vendor_uninstall: None,
            confirm_delete_data: None,
            resource_invocation: None,
            resource_waits_for_appliance: false,
            resource_result: None,
            pending_control_invocation: None,
            projected_route_policy,
            delete_confirmation_layout: None,
        }
    }

    fn queue_live_control(&mut self, resource: impl Into<String>, action: &str) -> bool {
        if !self.provider.action_native_controls() {
            return false;
        }
        self.pending_control_invocation = Some(tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new(resource, action),
            arguments: serde_json::Map::new(),
        });
        self.notice = Some("Request sent for authorization…".into());
        true
    }

    /// Convert retained button signals into the same inert addresses used by
    /// search and agents. The application host re-resolves these against the
    /// fresh tree, applies permission/confirmation policy, records Effects in
    /// history, and only then calls `start_operation`.
    fn queue_live_control_signal(&mut self) -> bool {
        if !self.provider.action_native_controls() {
            return false;
        }
        let appliance_action = [
            (&self.choose_windows_iso_signal, "import-iso"),
            (&self.recovery_desktop_signal, "recovery"),
            (&self.start_windows_signal, "start"),
            (&self.restart_windows_signal, "restart"),
            (&self.suspend_windows_signal, "suspend"),
            (&self.resume_windows_signal, "resume"),
            (&self.shutdown_windows_signal, "shutdown"),
            (&self.connect_windows_signal, "temporary-network"),
            (&self.update_windows_signal, "update-network"),
            (&self.revoke_windows_connection_signal, "revoke-network"),
        ]
        .into_iter()
        .find_map(|(signal, action)| signal.replace(false).then_some(action));
        if let Some(action) = appliance_action {
            return self.queue_live_control("windows.appliance", action);
        }
        if self.install_signal.replace(false) {
            return self.queue_live_control("windows.manager", "install");
        }
        if self.confirm_capsule_signal.replace(false) {
            return self.queue_live_control("windows.pending-install", "install-private-capsule");
        }
        if self.confirm_vm_install_signal.replace(false) {
            return self.queue_live_control("windows.pending-install", "install-windows-appliance");
        }
        let Some(app) = self.selected_app() else {
            return false;
        };
        let resource = format!("windows.app:{}", app.id);
        if let Some(route) = self.route_signal.take() {
            return self.queue_live_control(
                resource,
                &format!("route-{}", route.short().to_ascii_lowercase()),
            );
        }
        if self.reset_signal.replace(false) {
            return self.queue_live_control(resource, "reset-route");
        }
        let app_action = [
            (&self.open_file_signal, "open-file"),
            (&self.launch_signal, "launch"),
            (&self.vm_fallback_signal, "launch-vm-fallback"),
            (&self.publish_vm_candidate_signal, "publish"),
            (&self.retry_signal, "retry-install"),
            (&self.escalate_to_vm_signal, "escalate-install"),
            (&self.remove_launcher_signal, "remove-launcher"),
            (&self.vendor_uninstall_signal, "uninstall"),
            (&self.delete_data_signal, "delete-data"),
        ]
        .into_iter()
        .find_map(|(signal, action)| signal.replace(false).then_some(action));
        if let Some(action) = app_action {
            return self.queue_live_control(resource, action);
        }
        false
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
                Err(error) => self.notice = Some(error),
            }
        }
        self.refresh();
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
                Err(error) => self.notice = Some(error),
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

    fn cancel_delete_confirmation(&mut self) {
        self.confirm_delete_data = None;
        self.notice = None;
    }

    fn confirm_delete_data(&mut self) {
        let Some(capsule) = self.confirm_delete_data.take() else {
            return;
        };
        self.notice = self.provider.delete_data(&capsule).err();
        if self.notice.is_none() {
            self.notice = Some(strings::t("windows-data-deleted"));
        }
        self.refresh();
    }
    /// Conditional controls are retained widgets, while the detail pane is
    /// immediate-mode. Clear their interactive state whenever the selected
    /// model can change so a control which is no longer painted cannot retain
    /// an old hit rectangle or activation callback.
    fn deactivate_selection_controls(&mut self) {
        for button in &mut self.route_buttons {
            button.set_enabled(false);
        }
        self.reset_button.set_enabled(false);
        self.launch_button.set_enabled(false);
        self.open_file_button.set_enabled(false);
        self.vm_fallback_button.set_enabled(false);
        self.confirm_capsule_button.set_enabled(false);
        self.confirm_vm_install_button.set_enabled(false);
        self.publish_vm_candidate_button.set_enabled(false);
        self.retry_button.set_enabled(false);
        self.escalate_to_vm_button.set_enabled(false);
        self.remove_launcher_button.set_enabled(false);
        self.vendor_uninstall_button.set_enabled(false);
        self.delete_data_button.set_enabled(false);
        self.start_windows_button.set_enabled(false);
        self.restart_windows_button.set_enabled(false);
        self.suspend_windows_button.set_enabled(false);
        self.resume_windows_button.set_enabled(false);
        self.shutdown_windows_button.set_enabled(false);
        self.connect_windows_button.set_enabled(false);
        self.update_windows_button.set_enabled(false);
        self.revoke_windows_connection_button.set_enabled(false);

        self.route_signal.set(None);
        self.reset_signal.set(false);
        self.launch_signal.set(false);
        self.open_file_signal.set(false);
        self.vm_fallback_signal.set(false);
        self.confirm_capsule_signal.set(false);
        self.confirm_vm_install_signal.set(false);
        self.publish_vm_candidate_signal.set(false);
        self.retry_signal.set(false);
        self.escalate_to_vm_signal.set(false);
        self.remove_launcher_signal.set(false);
        self.vendor_uninstall_signal.set(false);
        self.delete_data_signal.set(false);
        self.start_windows_signal.set(false);
        self.restart_windows_signal.set(false);
        self.suspend_windows_signal.set(false);
        self.resume_windows_signal.set(false);
        self.shutdown_windows_signal.set(false);
        self.connect_windows_signal.set(false);
        self.update_windows_signal.set(false);
        self.revoke_windows_connection_signal.set(false);
    }
    fn queue_import_path(&mut self, path: &Path) {
        let source_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if source_name.is_empty() {
            self.notice = Some(strings::t("windows-invalid-installer-name"));
            return;
        }
        match File::open(path) {
            Ok(source) => match self.provider.install(source, source_name) {
                Ok(()) => self.notice = Some(strings::t("windows-installing")),
                Err(error) => self.notice = Some(error),
            },
            Err(error) => {
                self.notice = Some(format!("Could not open the selected installer: {error}"));
            }
        }
        self.refresh();
    }
    /// Queue an installer from the descriptor the portal worker opened. The
    /// portal path is intentionally not available here, so this UI cannot
    /// reopen a name after the user has approved it.
    fn queue_import_file(&mut self, source: File, source_name: &str) {
        if source_name.is_empty() {
            self.notice = Some(strings::t("windows-invalid-installer-name"));
            return;
        }
        match self.provider.install(source, source_name) {
            Ok(()) => self.notice = Some(strings::t("windows-installing")),
            Err(error) => self.notice = Some(error),
        }
        self.refresh();
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
                self.finish_resource_operation(Err(error.clone()));
                self.notice = Some(format!("Could not open the file picker: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                self.finish_resource_operation(Err(strings::t("windows-picker-disconnected")));
                self.notice = Some(strings::t("windows-picker-disconnected"));
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
                match self.appliance_owner.import_iso(source.into_file()) {
                    Ok(()) => {
                        self.notice = Some(strings::t("windows-iso-importing"));
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.iso_portal_rx = None;
                self.cancel_resource_operation("Windows ISO selection was cancelled");
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.iso_portal_rx = None;
                self.finish_resource_operation(Err(error.clone()));
                self.notice = Some(format!("Could not open the Windows ISO picker: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.iso_portal_rx = None;
                self.finish_resource_operation(Err(strings::t("windows-picker-disconnected")));
                self.notice = Some(strings::t("windows-picker-disconnected"));
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
                self.notice = target
                    .ok_or_else(|| "The Windows application selection expired.".to_string())
                    .and_then(|entry_id| {
                        self.provider
                            .launch_vm_with_file(&entry_id, source.into_file())
                    })
                    .err();
                if self.notice.is_none() {
                    self.notice = Some(strings::t("windows-file-launch-queued"));
                }
            }
            Ok(OpenFileOutcome::Cancelled) => {
                self.document_portal_rx = None;
                self.document_target = None;
                self.cancel_resource_operation("document selection was cancelled");
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.document_portal_rx = None;
                self.document_target = None;
                self.finish_resource_operation(Err(error.clone()));
                self.notice = Some(format!("Could not open the file picker: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.document_portal_rx = None;
                self.document_target = None;
                self.finish_resource_operation(Err(strings::t("windows-picker-disconnected")));
                self.notice = Some(strings::t("windows-picker-disconnected"));
            }
        }
    }
    pub(crate) fn refresh(&mut self) {
        if let Some(notice) = self.appliance_owner.drain() {
            self.notice = Some(notice);
        }
        if self.resource_invocation.is_some()
            && self.resource_waits_for_appliance
            && let Some(result) = self.appliance_owner.take_action_result()
        {
            self.finish_resource_operation(result.map(|_| ()));
        }
        self.deactivate_selection_controls();
        self.catalogue = self.provider.catalogue();
        if let Some(notice) = self.provider.take_notice() {
            self.notice = Some(notice);
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
    }
    fn hit(&mut self, hit: Hit) {
        match hit {
            Hit::App(index) => {
                self.deactivate_selection_controls();
                self.selected = index;
                self.notice = None;
                self.confirm_escalate_to_vm = None;
                self.confirm_vendor_uninstall = None;
                self.confirm_delete_data = None;
                if self.area.width() < COMPACT_W {
                    self.compact_detail = true;
                }
            }
            Hit::Back => self.compact_detail = false,
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered dispatcher drains the retained controls before mutating the selected model"
    )]
    fn apply_control_signals(&mut self, doc: &mut AutoCommit) {
        if self.queue_live_control_signal() {
            return;
        }
        if self.choose_windows_iso_signal.replace(false)
            && self.iso_portal_rx.is_none()
            && !self.appliance_owner.busy()
        {
            self.iso_portal_rx = Some(windows_iso_pick_request().send_open_file());
            self.notice = Some(strings::t("windows-choose-iso"));
        }
        if self.recovery_desktop_signal.replace(false) && !self.appliance_owner.busy() {
            self.notice = self.appliance_owner.open_recovery().err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-recovery-starting"));
            }
        }
        for (signal, action, started) in [
            (
                &self.start_windows_signal,
                LifecycleAction::Start,
                "windows-start-starting",
            ),
            (
                &self.restart_windows_signal,
                LifecycleAction::Restart,
                "windows-restart-starting",
            ),
            (
                &self.suspend_windows_signal,
                LifecycleAction::Suspend,
                "windows-suspend-starting",
            ),
            (
                &self.resume_windows_signal,
                LifecycleAction::Resume,
                "windows-resume-starting",
            ),
            (
                &self.shutdown_windows_signal,
                LifecycleAction::Shutdown,
                "windows-shutdown-starting",
            ),
        ] {
            if signal.replace(false) && !self.appliance_owner.busy() {
                self.notice = self.appliance_owner.lifecycle(action).err();
                if self.notice.is_none() {
                    self.notice = Some(strings::t(started));
                }
            }
        }
        if self.connect_windows_signal.replace(false) && !self.appliance_owner.busy() {
            let purpose = EgressPurpose::Activation;
            self.notice = self.appliance_owner.open_temporary_egress(purpose).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-temporary-connection-starting"));
            }
        }
        if self.update_windows_signal.replace(false) && !self.appliance_owner.busy() {
            self.notice = self
                .appliance_owner
                .open_temporary_egress(EgressPurpose::WindowsUpdate)
                .err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-temporary-connection-starting"));
            }
        }
        if self.revoke_windows_connection_signal.replace(false) && !self.appliance_owner.busy() {
            self.notice = self.appliance_owner.revoke_temporary_egress().err();
        }
        if self.confirm_capsule_signal.replace(false)
            && self.catalogue.vm_install_confirmation.is_some()
        {
            self.notice = self.provider.confirm_capsule_install().err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-capsule-setup-started"));
            }
            self.refresh();
        }
        if self.confirm_vm_install_signal.replace(false)
            && self.catalogue.vm_install_confirmation.is_some()
        {
            self.notice = self.provider.confirm_vm_install().err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-vm-setup-started"));
            }
            self.refresh();
        }
        if self.install_signal.replace(false) && self.portal_rx.is_none() {
            self.portal_rx = Some(installer_pick_request().send_open_file());
            self.notice = Some(strings::t("windows-choose-installer"));
        }
        if self.open_file_signal.replace(false)
            && self.document_portal_rx.is_none()
            && let Some(app) = self.selected_app()
            && app.route == Route::Vm
            && app.portal_file_open_available
        {
            self.document_target = Some(app.id.clone());
            self.document_portal_rx = Some(windows_document_pick_request().send_open_file());
            self.notice = Some(strings::t("windows-choose-document"));
        }
        if let Some(route) = self.route_signal.take()
            && let Some(app) = self.selected_app()
            && app.route_override_available
            && (route != Route::Vm || app.launch_available)
        {
            let id = app.id.clone();
            self.commit_route(doc, &actions::WindowsAction::SetRoute { app_id: id, route });
            self.project_shared_route_policy(doc);
            self.refresh();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-route-set"));
            }
        }
        if self.reset_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.route_override_available
            && app.uses_override()
        {
            let id = app.id.clone();
            self.commit_route(doc, &actions::WindowsAction::ResetRoute { app_id: id });
            self.project_shared_route_policy(doc);
            self.refresh();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-route-reset"));
            }
        }
        if self.launch_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.launch_available
        {
            let id = app.id.clone();
            let uses_vm = app.route == Route::Vm;
            self.notice = if uses_vm {
                self.provider.launch_vm_fallback(&id).err()
            } else {
                self.provider.launch(&id).err()
            };
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-launch-queued"));
            }
        }
        if self.vm_fallback_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.route != Route::Vm
            && app.vm_fallback_available
        {
            let id = app.id.clone();
            self.notice = self.provider.launch_vm_fallback(&id).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-vm-launch-queued"));
            }
        }
        if self.publish_vm_candidate_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_publish_vm_candidate
        {
            let id = app.id.clone();
            self.notice = self.provider.publish_vm_candidate(&id).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-vm-candidate-publishing"));
            }
            self.refresh();
        }
        if self.retry_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_retry_install
        {
            let capsule = app.capsule.clone();
            self.notice = self.provider.retry_install(&capsule).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-retrying"));
            }
        }
        if self.escalate_to_vm_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_escalate_install_to_vm
        {
            let capsule = app.capsule.clone();
            if self.confirm_escalate_to_vm.as_deref() == Some(capsule.as_str()) {
                self.confirm_escalate_to_vm = None;
                self.notice = self.provider.escalate_install_to_vm(&capsule).err();
                if self.notice.is_none() {
                    self.notice = Some(strings::t("windows-escalation-started"));
                }
                self.refresh();
            } else {
                self.confirm_escalate_to_vm = Some(capsule);
                self.confirm_vendor_uninstall = None;
                self.confirm_delete_data = None;
                self.notice = Some(strings::t("windows-confirm-escalation"));
            }
        }
        if self.remove_launcher_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_remove_launcher
        {
            let id = app.id.clone();
            self.notice = self.provider.remove_launcher(&id).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-launcher-removed"));
            }
        }
        if self.vendor_uninstall_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_vendor_uninstall
        {
            let vm_application = app.route == Route::Vm;
            let management_id = if vm_application {
                app.id.clone()
            } else {
                app.capsule.clone()
            };
            if self.confirm_vendor_uninstall.as_deref() == Some(management_id.as_str()) {
                self.confirm_vendor_uninstall = None;
                self.notice = if vm_application {
                    self.provider.vendor_uninstall_vm(&management_id).err()
                } else {
                    self.provider.vendor_uninstall(&management_id).err()
                };
                if self.notice.is_none() {
                    self.notice = Some(strings::t(if vm_application {
                        "windows-vm-uninstalling"
                    } else {
                        "windows-uninstalling"
                    }));
                }
            } else {
                self.confirm_vendor_uninstall = Some(management_id);
                self.confirm_delete_data = None;
                self.notice = Some(strings::t(if vm_application {
                    "windows-confirm-vm-uninstall"
                } else {
                    "windows-confirm-uninstall"
                }));
            }
        }
        if self.delete_data_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_delete_data
            && self.confirm_delete_data.is_none()
        {
            let capsule = app.capsule.clone();
            self.confirm_delete_data = Some(capsule);
            self.confirm_vendor_uninstall = None;
            self.notice = None;
        }
    }
    fn fill(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
        scene.fill(Fill::NonZero, t, color, None, &rect);
    }
    fn pill(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            color,
            None,
            &RoundedRect::from_rect(rect, 8.0),
        );
    }
    fn text(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        t: Affine,
        at: Point,
        text: &str,
        size: f32,
        color: Color,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(
            scene,
            text,
            size,
            color,
            t * Affine::translate((at.x, at.y)),
            None,
            weight,
        );
    }
    fn line(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        t: Affine,
        x: f64,
        y: f64,
        label: &str,
        value: &str,
        theme: &Theme,
    ) {
        Self::text(
            tr,
            scene,
            t,
            Point::new(x, y),
            label,
            12.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(x + 148.0, y),
            value,
            13.0,
            theme.text_primary,
            TextWeight::Regular,
        );
    }

    fn flow_button_rect(cursor: &mut Point, width: f64, bounds: Rect) -> Rect {
        if cursor.x > bounds.x0 && cursor.x + width > bounds.x1 {
            cursor.x = bounds.x0;
            cursor.y += 40.0;
        }
        let rect = Rect::new(
            cursor.x,
            cursor.y,
            (cursor.x + width).min(bounds.x1),
            cursor.y + 32.0,
        );
        cursor.x = rect.x1 + 10.0;
        rect
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
        self.notice = Some("Operation requested".into());
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

impl Widget for WindowsApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
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
        ]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.project_shared_route_policy(state);
        self.drain_portal();
        self.drain_document_portal();
        self.drain_iso_portal();
        self.refresh();
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap_or_else(|_| NodeId::from(0_u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.area = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the single canvas widget keeps paint order and hit geometry together"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        Self::fill(scene, t, self.area, theme.bg_app);
        self.delete_confirmation_layout = None;
        self.hits.clear();
        let compact = self.area.width() < COMPACT_W;
        if !compact || !self.compact_detail {
            let sidebar_view = Rect::new(
                self.area.x0,
                self.area.y0,
                if compact {
                    self.area.x1
                } else {
                    (self.area.x0 + SIDEBAR_W).min(self.area.x1)
                },
                self.area.y1,
            );
            Self::fill(scene, t, sidebar_view, theme.bg_primary);
            // All sidebar hit geometry follows the same translated content frame,
            // so scrolling never paints rows at one location and clicks them at
            // another. The window surface clips anything outside its viewport.
            let sidebar = Rect::new(
                sidebar_view.x0,
                sidebar_view.y0 - self.sidebar_scroll,
                sidebar_view.x1,
                sidebar_view.y1 - self.sidebar_scroll,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(sidebar.x0 + PAD, sidebar.y0 + 26.0),
                &strings::t("windows-app-title"),
                21.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(sidebar.x0 + PAD, sidebar.y0 + 53.0),
                &strings::t("windows-catalogue"),
                12.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            let install_rect = Rect::new(
                sidebar.x0 + PAD,
                sidebar.y0 + 70.0,
                sidebar.x1 - PAD,
                sidebar.y0 + 102.0,
            );
            self.install_button.apply_layout_external(install_rect);
            self.install_button.set_style(ButtonStyle::Primary);
            self.install_button.set_enabled(self.portal_rx.is_none());
            self.install_button.draw(scene, t, tr, theme, presence);
            if let Some(confirmation) = &self.catalogue.vm_install_confirmation {
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(sidebar.x0 + PAD, sidebar.y0 + 120.0),
                    &confirmation.display_name,
                    12.0,
                    theme.text_primary,
                    TextWeight::Bold,
                );
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(sidebar.x0 + PAD, sidebar.y0 + 136.0),
                    &strings::t("windows-choose-install-destination"),
                    10.0,
                    theme.text_dim,
                    TextWeight::Regular,
                );
                let capsule = Rect::new(
                    sidebar.x0 + PAD,
                    sidebar.y0 + 144.0,
                    sidebar.x1 - PAD,
                    sidebar.y0 + 176.0,
                );
                self.confirm_capsule_button.apply_layout_external(capsule);
                self.confirm_capsule_button.set_style(
                    if confirmation.recommended_route == Route::Vm {
                        ButtonStyle::Neutral
                    } else {
                        ButtonStyle::Primary
                    },
                );
                self.confirm_capsule_button.set_enabled(true);
                self.confirm_capsule_button
                    .draw(scene, t, tr, theme, presence);
                if confirmation.recommended_route != Route::Vm {
                    Self::text(
                        tr,
                        scene,
                        t,
                        Point::new(capsule.x1 - 82.0, capsule.y0 + 20.0),
                        &strings::t("windows-recommended"),
                        10.0,
                        theme.text_primary,
                        TextWeight::Bold,
                    );
                }
                let vm = Rect::new(
                    sidebar.x0 + PAD,
                    sidebar.y0 + 180.0,
                    sidebar.x1 - PAD,
                    sidebar.y0 + 212.0,
                );
                self.confirm_vm_install_button.apply_layout_external(vm);
                self.confirm_vm_install_button.set_style(
                    if confirmation.recommended_route == Route::Vm {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Neutral
                    },
                );
                self.confirm_vm_install_button.set_enabled(true);
                self.confirm_vm_install_button
                    .draw(scene, t, tr, theme, presence);
                if confirmation.recommended_route == Route::Vm {
                    Self::text(
                        tr,
                        scene,
                        t,
                        Point::new(vm.x1 - 82.0, vm.y0 + 20.0),
                        &strings::t("windows-recommended"),
                        10.0,
                        theme.text_primary,
                        TextWeight::Bold,
                    );
                }
            }
            let appliance_y = sidebar.y0
                + if self.catalogue.vm_install_confirmation.is_some() {
                    244.0
                } else {
                    116.0
                };
            let appliance = self.appliance_owner.status();
            Self::text(
                tr,
                scene,
                t,
                Point::new(sidebar.x0 + PAD, appliance_y),
                &strings::t("windows-appliance-setup"),
                13.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(sidebar.x0 + PAD, appliance_y + 20.0),
                appliance.next_step_label(),
                11.0,
                theme.text_primary,
                TextWeight::Regular,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(sidebar.x0 + PAD, appliance_y + 36.0),
                appliance.phase_label(),
                10.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            if let Some(detail) = &appliance.detail {
                PaintCtx::new(scene, t, tr, theme).text(
                    detail,
                    &TextStyle::new(10.0),
                    theme.text_dim,
                    Point::new(sidebar.x0 + PAD, appliance_y + 50.0),
                    Some(((sidebar.width() - PAD * 2.0) as f32).max(1.0)),
                );
            }
            if appliance.is_provisioned() {
                Self::line(
                    tr,
                    scene,
                    t,
                    sidebar.x0 + PAD,
                    appliance_y + 66.0,
                    "Power",
                    appliance.power_state.label(),
                    theme,
                );
            }
            // The activation state intentionally exists only in this appliance
            // management card. A published application's launch and window
            // chrome never receive this status or an activation reminder.
            if appliance.is_provisioned() {
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(sidebar.x0 + PAD, appliance_y + 84.0),
                    &strings::t("windows-activation-status"),
                    10.0,
                    theme.text_dim,
                    TextWeight::Regular,
                );
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(sidebar.x0 + PAD, appliance_y + 98.0),
                    appliance.license_label(),
                    10.0,
                    theme.text_primary,
                    TextWeight::Regular,
                );
                if let Some(detail) = appliance.license_detail() {
                    Self::text(
                        tr,
                        scene,
                        t,
                        Point::new(sidebar.x0 + PAD, appliance_y + 112.0),
                        detail,
                        9.0,
                        theme.text_dim,
                        TextWeight::Regular,
                    );
                }
            }
            PaintCtx::new(scene, t, tr, theme).text(
                &strings::t("windows-appliance-desktop-note"),
                &TextStyle::new(10.0),
                theme.text_dim,
                Point::new(sidebar.x0 + PAD, appliance_y + 132.0),
                Some(((sidebar.width() - PAD * 2.0) as f32).max(1.0)),
            );
            if appliance.is_provisioned() {
                let recovery = Rect::new(
                    sidebar.x0 + PAD,
                    appliance_y + 144.0,
                    sidebar.x1 - PAD,
                    appliance_y + 176.0,
                );
                self.recovery_desktop_button.apply_layout_external(recovery);
                self.recovery_desktop_button.set_style(ButtonStyle::Neutral);
                self.recovery_desktop_button
                    .set_enabled(appliance.recovery_available && !self.appliance_owner.busy());
                self.recovery_desktop_button
                    .draw(scene, t, tr, theme, presence);
                let lifecycle_y = appliance_y + 182.0;
                let lifecycle_buttons: [(&mut Button, LifecycleAction, f64, f64); 5] = [
                    (
                        &mut self.restart_windows_button,
                        LifecycleAction::Restart,
                        sidebar.x0 + PAD,
                        lifecycle_y,
                    ),
                    (
                        &mut self.suspend_windows_button,
                        LifecycleAction::Suspend,
                        sidebar.x0 + PAD + 124.0,
                        lifecycle_y,
                    ),
                    (
                        &mut self.resume_windows_button,
                        LifecycleAction::Resume,
                        sidebar.x0 + PAD,
                        lifecycle_y + 36.0,
                    ),
                    (
                        &mut self.shutdown_windows_button,
                        LifecycleAction::Shutdown,
                        sidebar.x0 + PAD + 124.0,
                        lifecycle_y + 36.0,
                    ),
                    (
                        &mut self.start_windows_button,
                        LifecycleAction::Start,
                        sidebar.x0 + PAD,
                        lifecycle_y + 72.0,
                    ),
                ];
                for (button, action, x, y) in lifecycle_buttons {
                    button.apply_layout_external(Rect::new(x, y, x + 118.0, y + 32.0));
                    button.set_style(ButtonStyle::Neutral);
                    button.set_enabled(
                        !self.appliance_owner.busy() && appliance.power_state.allows(action),
                    );
                    button.draw(scene, t, tr, theme, presence);
                }
                let connection = Rect::new(
                    sidebar.x0 + PAD,
                    lifecycle_y + 108.0,
                    sidebar.x1 - PAD,
                    lifecycle_y + 140.0,
                );
                self.connect_windows_button
                    .apply_layout_external(connection);
                self.connect_windows_button.set_style(ButtonStyle::Neutral);
                self.connect_windows_button.set_enabled(
                    !self.appliance_owner.busy()
                        && appliance.temporary_egress.is_none()
                        && appliance.power_state
                            == crate::appliance_owner::AppliancePowerState::Running,
                );
                self.connect_windows_button
                    .draw(scene, t, tr, theme, presence);
                let update = Rect::new(
                    sidebar.x0 + PAD,
                    lifecycle_y + 144.0,
                    sidebar.x1 - PAD,
                    lifecycle_y + 176.0,
                );
                self.update_windows_button.apply_layout_external(update);
                self.update_windows_button.set_style(ButtonStyle::Neutral);
                self.update_windows_button.set_enabled(
                    !self.appliance_owner.busy()
                        && appliance.temporary_egress.is_none()
                        && appliance.power_state
                            == crate::appliance_owner::AppliancePowerState::Running,
                );
                self.update_windows_button
                    .draw(scene, t, tr, theme, presence);
                if appliance.power_state != crate::appliance_owner::AppliancePowerState::Running {
                    Self::text(
                        tr,
                        scene,
                        t,
                        Point::new(sidebar.x0 + PAD, lifecycle_y + 194.0),
                        "Windows must be running to connect it temporarily.",
                        10.0,
                        theme.text_dim,
                        TextWeight::Regular,
                    );
                }
                if let Some(lease) = &appliance.temporary_egress {
                    Self::text(
                        tr,
                        scene,
                        t,
                        Point::new(sidebar.x0 + PAD, lifecycle_y + 194.0),
                        &format!("{} · {}", lease.purpose.label(), lease.expiry_label()),
                        10.0,
                        theme.text_dim,
                        TextWeight::Regular,
                    );
                    let revoke = Rect::new(
                        sidebar.x0 + PAD,
                        lifecycle_y + 202.0,
                        sidebar.x1 - PAD,
                        lifecycle_y + 234.0,
                    );
                    self.revoke_windows_connection_button
                        .apply_layout_external(revoke);
                    self.revoke_windows_connection_button
                        .set_style(ButtonStyle::Neutral);
                    self.revoke_windows_connection_button
                        .set_enabled(!self.appliance_owner.busy());
                    self.revoke_windows_connection_button
                        .draw(scene, t, tr, theme, presence);
                }
            } else {
                let choose_iso = Rect::new(
                    sidebar.x0 + PAD,
                    appliance_y + 166.0,
                    sidebar.x1 - PAD,
                    appliance_y + 198.0,
                );
                self.choose_windows_iso_button
                    .apply_layout_external(choose_iso);
                self.choose_windows_iso_button
                    .set_style(ButtonStyle::Primary);
                self.choose_windows_iso_button
                    .set_enabled(!self.appliance_owner.busy() && self.iso_portal_rx.is_none());
                self.choose_windows_iso_button
                    .draw(scene, t, tr, theme, presence);
            }
            let mut y = appliance_y
                + if appliance.is_provisioned() {
                    if appliance.temporary_egress.is_some() {
                        430.0
                    } else if appliance.power_state
                        != crate::appliance_owner::AppliancePowerState::Running
                    {
                        402.0
                    } else {
                        382.0
                    }
                } else {
                    218.0
                };
            for (index, app) in self.catalogue.apps.iter().enumerate() {
                let row = Rect::new(sidebar.x0 + 10.0, y, sidebar.x1 - 10.0, y + ROW_H - 6.0);
                if index == self.selected {
                    Self::pill(scene, t, row, theme.bg_active);
                }
                let badge = Rect::new(row.x0 + 10.0, row.y0 + 14.0, row.x0 + 68.0, row.y0 + 35.0);
                Self::pill(
                    scene,
                    t,
                    badge,
                    if app.route == Route::Vm {
                        theme.border_standard
                    } else {
                        theme.accent_primary
                    },
                );
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(badge.x0 + 7.0, badge.y0 + 4.0),
                    app.route.short(),
                    10.0,
                    theme.text_primary,
                    TextWeight::Bold,
                );
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(row.x0 + 78.0, row.y0 + 10.0),
                    &app.name,
                    13.0,
                    theme.text_primary,
                    TextWeight::Bold,
                );
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(row.x0 + 78.0, row.y0 + 31.0),
                    app.status.label(),
                    11.0,
                    theme.text_dim,
                    TextWeight::Regular,
                );
                self.hits.push((row, Hit::App(index)));
                y += ROW_H;
            }
            // `y` is in the translated paint frame; retain an unscrolled logical
            // extent so a wheel move cannot shrink its own maximum on the next
            // frame.
            self.sidebar_content_bottom = y.max(appliance_y + 440.0) + self.sidebar_scroll;
            let max_sidebar_scroll = (self.sidebar_content_bottom - sidebar_view.y1).max(0.0);
            self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, max_sidebar_scroll);
            if compact {
                return;
            }
        }
        if compact {
            let back = Rect::new(
                self.area.x0 + PAD,
                self.area.y0 + 12.0,
                self.area.x0 + 116.0,
                self.area.y0 + 48.0,
            );
            Self::pill(scene, t, back, theme.bg_primary);
            Self::text(
                tr,
                scene,
                t,
                Point::new(back.x0 + 14.0, back.y0 + 9.0),
                "← All apps",
                13.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            self.hits.push((back, Hit::Back));
        }
        let detail = Rect::new(
            if compact {
                self.area.x0 + PAD
            } else {
                self.area.x0 + SIDEBAR_W + PAD
            },
            self.area.y0 + if compact { 68.0 } else { PAD },
            self.area.x1 - PAD,
            self.area.y1 - PAD,
        );
        let Some(app) = self.selected_app().cloned() else {
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y0 + 8.0),
                &strings::t("windows-empty-title"),
                28.0,
                theme.text_primary,
                TextWeight::Bold,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y0 + 44.0),
                &self.catalogue.vm.detail,
                13.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y0 + 92.0),
                &strings::t("windows-empty-body"),
                13.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            return;
        };
        Self::text(
            tr,
            scene,
            t,
            Point::new(detail.x0, detail.y0 + 8.0),
            &app.name,
            28.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(detail.x0, detail.y0 + 44.0),
            &format!(
                "{} · {} · {}",
                app.publisher,
                app.catalogue_source,
                app.confidence.label()
            ),
            13.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        let status = Rect::new(
            detail.x0,
            detail.y0 + 70.0,
            detail.x0 + 128.0,
            detail.y0 + 98.0,
        );
        Self::pill(scene, t, status, theme.accent_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(status.x0 + 10.0, status.y0 + 6.0),
            &format!("{} · {}", app.route.short(), app.status.label()),
            11.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        let launch = Rect::new(
            status.x1 + 16.0,
            status.y0 - 2.0,
            status.x1 + 148.0,
            status.y1 + 2.0,
        );
        self.launch_button.apply_layout_external(launch);
        self.launch_button.set_enabled(app.launch_available);
        // An unavailable primary action must not retain the accent fill: on
        // high-contrast themes that made the disabled label look actionable
        // and reduced its legibility. The shared neutral disabled treatment
        // keeps the semantic disabled state and makes the distinction clear.
        self.launch_button.set_style(if app.launch_available {
            ButtonStyle::Primary
        } else {
            ButtonStyle::Neutral
        });
        self.launch_button.draw(scene, t, tr, theme, presence);
        let launch_error_offset = if app.launch_available {
            0.0
        } else if compact {
            56.0
        } else {
            28.0
        };
        if let Some(reason) = app
            .launch_unavailable_reason
            .as_deref()
            .filter(|_| !app.launch_available)
        {
            PaintCtx::new(scene, t, tr, theme).text(
                reason,
                &TextStyle::new(10.0),
                theme.text_dim,
                Point::new(launch.x0, status.y1 + 8.0),
                Some(((detail.x1 - launch.x0).max(120.0) as f32).max(1.0)),
            );
        }
        if app.route == Route::Vm {
            let open_file = Rect::new(
                launch.x1 + 12.0,
                status.y0 - 2.0,
                launch.x1 + 176.0,
                status.y1 + 2.0,
            );
            self.open_file_button.apply_layout_external(open_file);
            self.open_file_button.set_style(ButtonStyle::Neutral);
            self.open_file_button
                .set_enabled(app.portal_file_open_available);
            self.open_file_button.draw(scene, t, tr, theme, presence);
            if let Some(reason) = app
                .portal_file_open_unavailable_reason
                .as_deref()
                .filter(|_| !app.portal_file_open_available)
            {
                let compact_reason = compact_portal_reason(reason);
                Self::text(
                    tr,
                    scene,
                    t,
                    Point::new(open_file.x1 + 10.0, open_file.y0 + 20.0),
                    &compact_reason,
                    10.0,
                    theme.text_dim,
                    TextWeight::Regular,
                );
            }
        }
        if app.route != Route::Vm && app.vm_fallback_available {
            let vm = Rect::new(
                launch.x1 + 12.0,
                status.y0 - 2.0,
                launch.x1 + 186.0,
                status.y1 + 2.0,
            );
            self.vm_fallback_button.apply_layout_external(vm);
            self.vm_fallback_button.set_style(ButtonStyle::Neutral);
            self.vm_fallback_button.set_enabled(true);
            self.vm_fallback_button.draw(scene, t, tr, theme, presence);
        }
        let card = Rect::new(
            detail.x0,
            detail.y0 + 118.0 + launch_error_offset,
            detail.x1,
            detail.y0 + 238.0 + launch_error_offset,
        );
        Self::pill(scene, t, card, theme.bg_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(card.x0 + PAD, card.y0 + 16.0),
            &strings::t("windows-why-route"),
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        PaintCtx::new(scene, t, tr, theme).text(
            &app.reason,
            &TextStyle::new(13.0),
            theme.text_dim,
            Point::new(card.x0 + PAD, card.y0 + 44.0),
            Some(((card.width() - PAD * 2.0) as f32).max(1.0)),
        );
        PaintCtx::new(scene, t, tr, theme).text(
            app.route.security_summary(),
            &TextStyle::new(12.0),
            theme.text_dim,
            Point::new(card.x0 + PAD, card.y0 + 74.0),
            Some(((card.width() - PAD * 2.0) as f32).max(1.0)),
        );
        let routes_y = card.y1 + 22.0;
        Self::text(
            tr,
            scene,
            t,
            Point::new(detail.x0, routes_y),
            &strings::t("windows-open-with"),
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        for (index, route) in Route::ALL.iter().enumerate() {
            let x = detail.x0 + 94.0 + index as f64 * 118.0;
            let button = Rect::new(x, routes_y - 8.0, x + 108.0, routes_y + 24.0);
            if let Some(shared_button) = self.route_buttons.get_mut(index) {
                shared_button.apply_layout_external(button);
                shared_button.set_style(if *route == app.route {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Neutral
                });
                shared_button.set_enabled(
                    app.route_override_available && (*route != Route::Vm || app.launch_available),
                );
                shared_button.draw(scene, t, tr, theme, presence);
            }
        }
        let reset_needs_row = app.uses_override() && detail.width() < 660.0;
        if app.uses_override() {
            let reset = if reset_needs_row {
                Rect::new(
                    detail.x0,
                    routes_y + 32.0,
                    (detail.x0 + 195.0).min(detail.x1),
                    routes_y + 64.0,
                )
            } else {
                Rect::new(
                    detail.x0 + 460.0,
                    routes_y - 8.0,
                    detail.x0 + 655.0,
                    routes_y + 24.0,
                )
            };
            self.reset_button.apply_layout_external(reset);
            self.reset_button.set_style(ButtonStyle::Neutral);
            self.reset_button.set_enabled(app.route_override_available);
            self.reset_button.draw(scene, t, tr, theme, presence);
        }
        let route_notice_needs_row =
            app.route_override_unavailable_reason.is_some() && !app.route_override_available;
        if let Some(reason) = &app.route_override_unavailable_reason
            && !app.route_override_available
        {
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, routes_y + 42.0),
                reason,
                10.0,
                theme.text_dim,
                TextWeight::Regular,
            );
        }
        // Reserve real vertical space for conditional controls and their
        // explanation. Previously the access card began six pixels below an
        // unavailable-route message, which made both unreadable in compact
        // VM details.
        let route_controls_offset = if reset_needs_row { 40.0 } else { 0.0 }
            + if route_notice_needs_row { 24.0 } else { 0.0 };
        let vm_shared_session = app.route == Route::Vm;
        let info_line_offset = if vm_shared_session { 8.0 } else { 0.0 };
        let fallback_is_visible = !compact || app.installation_detail.is_none();
        let info_top = routes_y + 48.0 + route_controls_offset;
        let expected_info_bottom = info_top
            + 151.0
            + info_line_offset
            + 20.0 * app.grants.len() as f64
            + if app.installation_detail.is_some() {
                24.0
            } else {
                0.0
            }
            + if fallback_is_visible { 22.0 } else { 0.0 }
            + 18.0;
        let info = Rect::new(
            detail.x0,
            info_top,
            detail.x1,
            (routes_y + 292.0).max(expected_info_bottom).min(detail.y1),
        );
        Self::pill(scene, t, info, theme.bg_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(info.x0 + PAD, info.y0 + 16.0),
            &strings::t(if app.route == Route::Vm {
                "windows-shared-windows-session-access"
            } else {
                "windows-capsule-access"
            }),
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        if vm_shared_session {
            Self::text(
                tr,
                scene,
                t,
                Point::new(info.x0 + PAD, info.y0 + 34.0),
                &strings::t("windows-shared-session-trust"),
                9.0,
                theme.text_dim,
                TextWeight::Regular,
            );
        }
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 46.0 + info_line_offset,
            &strings::t("windows-runner"),
            &compact_detail_value(&app.runner, compact),
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 71.0 + info_line_offset,
            &strings::t(if app.route == Route::Vm {
                "windows-appliance"
            } else {
                "windows-capsule"
            }),
            &compact_detail_value(&app.capsule, compact),
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 96.0 + info_line_offset,
            &strings::t(if app.route == Route::Vm {
                "windows-guest-storage"
            } else {
                "windows-private-data"
            }),
            &compact_detail_value(&app.prefix, compact),
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 121.0 + info_line_offset,
            &strings::t(if app.route == Route::Vm {
                "windows-current-shared-session-access"
            } else {
                "windows-host-access"
            }),
            &compact_detail_value(
                &if app.route == Route::Vm {
                    if self.catalogue.vm.shared_session_active {
                        format!(
                            "{} · {}",
                            self.catalogue.vm.shared_session_grants.join(", "),
                            self.catalogue.vm.shared_session_contributors.join(", ")
                        )
                    } else {
                        "No active host redirects".to_string()
                    }
                } else {
                    app.access_summary()
                },
                compact,
            ),
            theme,
        );
        let mut grant_y = info.y0 + 151.0 + info_line_offset;
        for grant in &app.grants {
            Self::line(
                tr,
                scene,
                t,
                info.x0 + PAD,
                grant_y,
                &grant.label,
                &compact_detail_value(
                    &if vm_shared_session {
                        format!(
                            "{} · {}",
                            if grant.enabled {
                                "Host-authorized request"
                            } else {
                                "Not authorized"
                            },
                            grant.detail
                        )
                    } else {
                        format!(
                            "{} · {}",
                            if grant.enabled { "Enabled" } else { "Disabled" },
                            grant.detail
                        )
                    },
                    compact,
                ),
                theme,
            );
            grant_y += 20.0;
        }
        if let Some(detail) = &app.installation_detail {
            Self::text(
                tr,
                scene,
                t,
                Point::new(info.x0 + PAD, grant_y + 4.0),
                detail,
                11.0,
                theme.text_dim,
                TextWeight::Regular,
            );
            grant_y += 24.0;
        }
        // On a compact screen the installation state is the actionable
        // recovery information. Do not paint the longer background fallback
        // paragraph into the management controls below it; it remains in the
        // semantic description and is shown in full on the desktop surface.
        if fallback_is_visible {
            Self::text(
                tr,
                scene,
                t,
                Point::new(info.x0 + PAD, grant_y + 4.0),
                &app.fallback_detail,
                12.0,
                theme.text_dim,
                TextWeight::Regular,
            );
        }
        // The installation status is variable-length and appears after the
        // grant list. Anchor destructive maintenance controls below the last
        // rendered status line as well as below the card, so a compact VM
        // detail can never paint "Manage installation" over its publication
        // state.
        let manage_y = (info.y1 + 48.0).max(grant_y + 32.0).min(detail.y1 - 70.0);
        let mut manage_cursor = Point::new(detail.x0, manage_y);
        if app.can_retry_install
            || app.can_escalate_install_to_vm
            || app.can_remove_launcher
            || app.can_vendor_uninstall
            || app.can_delete_data
            || app.can_publish_vm_candidate
        {
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, manage_y - 24.0),
                &strings::t("windows-manage-installation"),
                13.0,
                theme.text_primary,
                TextWeight::Bold,
            );
        }
        if app.can_publish_vm_candidate {
            let rect = Self::flow_button_rect(&mut manage_cursor, 190.0, detail);
            self.publish_vm_candidate_button.apply_layout_external(rect);
            self.publish_vm_candidate_button
                .set_style(ButtonStyle::Primary);
            self.publish_vm_candidate_button.set_enabled(true);
            self.publish_vm_candidate_button
                .draw(scene, t, tr, theme, presence);
        }
        if app.can_retry_install {
            let rect = Self::flow_button_rect(&mut manage_cursor, 118.0, detail);
            self.retry_button.apply_layout_external(rect);
            self.retry_button.set_style(ButtonStyle::Neutral);
            self.retry_button.set_enabled(true);
            self.retry_button.draw(scene, t, tr, theme, presence);
        }
        if app.can_escalate_install_to_vm {
            let rect = Self::flow_button_rect(&mut manage_cursor, 220.0, detail);
            self.escalate_to_vm_button.apply_layout_external(rect);
            self.escalate_to_vm_button.set_style(ButtonStyle::Primary);
            self.escalate_to_vm_button.set_enabled(true);
            self.escalate_to_vm_button
                .draw(scene, t, tr, theme, presence);
        }
        if app.can_remove_launcher {
            let rect = Self::flow_button_rect(&mut manage_cursor, 142.0, detail);
            self.remove_launcher_button.apply_layout_external(rect);
            self.remove_launcher_button.set_style(ButtonStyle::Neutral);
            self.remove_launcher_button.set_enabled(true);
            self.remove_launcher_button
                .draw(scene, t, tr, theme, presence);
        }
        if app.can_vendor_uninstall {
            let rect = Self::flow_button_rect(&mut manage_cursor, 142.0, detail);
            self.vendor_uninstall_button.apply_layout_external(rect);
            self.vendor_uninstall_button.set_style(ButtonStyle::Neutral);
            self.vendor_uninstall_button.set_enabled(true);
            self.vendor_uninstall_button
                .draw(scene, t, tr, theme, presence);
        }
        if app.can_delete_data {
            let rect = Self::flow_button_rect(&mut manage_cursor, 132.0, detail);
            self.delete_data_button.apply_layout_external(rect);
            self.delete_data_button.set_style(ButtonStyle::Neutral);
            self.delete_data_button.set_enabled(true);
            self.delete_data_button.draw(scene, t, tr, theme, presence);
        }
        if let Some(notice) = &self.notice
            && self.confirm_delete_data.is_none()
        {
            let awaiting_confirmation = self.confirm_escalate_to_vm.is_some()
                || self.confirm_vendor_uninstall.is_some()
                || self.confirm_delete_data.is_some();
            // A second-click safety prompt is neither a success toast nor a
            // transient error. Give it its own visible treatment and wrap it
            // inside the compact detail width so the required confirmation is
            // never silently truncated off-screen.
            PaintCtx::new(scene, t, tr, theme).text(
                notice,
                &TextStyle::new(12.0),
                if awaiting_confirmation {
                    theme.accent_primary
                } else {
                    theme.status_success
                },
                Point::new(detail.x0, detail.y1 - 40.0),
                Some((detail.width() as f32).max(1.0)),
            );
        }
        if self.confirm_delete_data.as_deref() == Some(app.capsule.as_str()) {
            let cancel_label = strings::t("windows-cancel");
            let delete_label = strings::t("windows-confirm-delete-action");
            let title = strings::t("windows-confirm-delete-title");
            let actions = [
                ModalAction::secondary(&cancel_label),
                ModalAction::danger(&delete_label),
            ];
            let spec = ModalSpec::new(&title)
                .actions(&actions)
                .body_height(88.0)
                .max_width(440.0)
                .dismiss_on_scrim(false);
            let layout = ModalFrame::layout(self.area, &spec, tr, theme);
            ModalFrame::paint_backdrop(scene, t, tr, theme, &layout, &spec);
            let (ink, dim) = ModalFrame::ink(theme);
            PaintCtx::new(scene, t, tr, theme).text(
                &strings::t("windows-confirm-delete-body"),
                &TextStyle::new(13.0),
                dim,
                Point::new(layout.body.x0, layout.body.y0 + 4.0),
                Some((layout.body.width() as f32).max(1.0)),
            );
            Self::text(
                tr,
                scene,
                t,
                Point::new(layout.body.x0, layout.body.y0 - 20.0),
                &app.name,
                13.0,
                ink,
                TextWeight::Bold,
            );
            ModalFrame::paint_actions(scene, t, tr, theme, &layout, &spec);
            self.delete_confirmation_layout = Some(layout);
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one event boundary keeps modal capture ahead of the existing routed controls"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.confirm_delete_data.is_some() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.mouse = Point::new(position.x, position.y);
                *ctx.handled = true;
                return;
            }
            if let WindowEvent::KeyboardInput {
                event: key_event, ..
            } = event
                && key_event.state == ElementState::Pressed
            {
                match key_event.logical_key {
                    Key::Named(NamedKey::Escape) => self.cancel_delete_confirmation(),
                    Key::Named(NamedKey::Enter) => self.confirm_delete_data(),
                    _ => {}
                }
                *ctx.handled = true;
                return;
            }
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                if let Some(layout) = &self.delete_confirmation_layout {
                    match layout.hit(self.mouse) {
                        Some(ModalHit::Action(0) | ModalHit::Close) => {
                            self.cancel_delete_confirmation();
                        }
                        Some(ModalHit::Action(1)) => self.confirm_delete_data(),
                        // A destructive dialog deliberately does not dismiss
                        // on a stray backdrop click; every modal hit is still
                        // swallowed so nothing behind it can activate.
                        Some(ModalHit::Card | ModalHit::Scrim | ModalHit::Action(_)) | None => {}
                    }
                }
                *ctx.handled = true;
                return;
            }
            *ctx.handled = true;
            return;
        }
        macro_rules! forward_button_event {
            ($button:expr) => {
                $button.handle_event(event, ctx)
            };
        }
        for button in &mut self.route_buttons {
            forward_button_event!(button);
        }
        forward_button_event!(self.reset_button);
        forward_button_event!(self.launch_button);
        forward_button_event!(self.open_file_button);
        forward_button_event!(self.vm_fallback_button);
        forward_button_event!(self.install_button);
        forward_button_event!(self.confirm_capsule_button);
        forward_button_event!(self.confirm_vm_install_button);
        forward_button_event!(self.publish_vm_candidate_button);
        forward_button_event!(self.retry_button);
        forward_button_event!(self.escalate_to_vm_button);
        forward_button_event!(self.remove_launcher_button);
        forward_button_event!(self.vendor_uninstall_button);
        forward_button_event!(self.delete_data_button);
        forward_button_event!(self.choose_windows_iso_button);
        forward_button_event!(self.recovery_desktop_button);
        forward_button_event!(self.start_windows_button);
        forward_button_event!(self.restart_windows_button);
        forward_button_event!(self.suspend_windows_button);
        forward_button_event!(self.resume_windows_button);
        forward_button_event!(self.shutdown_windows_button);
        forward_button_event!(self.connect_windows_button);
        forward_button_event!(self.update_windows_button);
        forward_button_event!(self.revoke_windows_connection_button);
        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
        self.apply_control_signals(doc);
        match event {
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed && !*ctx.handled => {
                if key_event.logical_key == Key::Named(NamedKey::Escape) && self.compact_detail {
                    self.hit(Hit::Back);
                    *ctx.handled = true;
                    return;
                }
                let next = match key_event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => self.selected.saturating_sub(1),
                    Key::Named(NamedKey::ArrowDown) => {
                        (self.selected + 1).min(self.catalogue.apps.len().saturating_sub(1))
                    }
                    Key::Named(NamedKey::Home) => 0,
                    Key::Named(NamedKey::End) => self.catalogue.apps.len().saturating_sub(1),
                    _ => self.selected,
                };
                if next != self.selected && !self.catalogue.apps.is_empty() {
                    self.hit(Hit::App(next));
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.mouse.x >= self.area.x0
                    && self.mouse.x <= (self.area.x0 + SIDEBAR_W).min(self.area.x1) =>
            {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(position) => -position.y / ctx.scale_factor,
                };
                let before = self.sidebar_scroll;
                self.sidebar_scroll += delta;
                self.clamp_sidebar_scroll();
                *ctx.handled |= (self.sidebar_scroll - before).abs() > f64::EPSILON;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = Point::new(position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = self
                    .hits
                    .iter()
                    .find(|(rect, _)| rect.contains(self.mouse))
                    .map(|(_, hit)| *hit)
                {
                    self.hit(hit);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "WindowsApp"
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.hits
            .iter()
            .any(|(rect, _)| rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the custom canvas exports its complete semantic hierarchy in one ordered tree"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        let sidebar_bounds = Rect::new(
            self.area.x0,
            self.area.y0,
            if self.area.width() < COMPACT_W {
                self.area.x1
            } else {
                (self.area.x0 + SIDEBAR_W).min(self.area.x1)
            },
            self.area.y1,
        );
        let rows = self
            .hits
            .iter()
            .filter_map(|(bounds, hit)| {
                let Hit::App(index) = *hit else {
                    return None;
                };
                let app = self.catalogue.apps.get(index)?;
                // Rows outside the scrolled viewport are absent from both the
                // pixels and the semantic tree. Their stable target returns as
                // soon as scrolling makes them visible.
                if bounds.y1 <= sidebar_bounds.y0 || bounds.y0 >= sidebar_bounds.y1 {
                    return None;
                }
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_name(&app.name)
                        .with_description(format!(
                            "{}; {}; {}; {}",
                            app.publisher,
                            app.route.label(),
                            app.status.label(),
                            app.access_summary()
                        ))
                        .with_action_target(format!("windows.select:{}", app.id))
                        .with_bounds(*bounds)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(index == self.selected),
                        ),
                )
            })
            .collect();
        let mut children = vec![
            AccessNode::new(Role::Heading { level: 1 }).with_name(strings::t("windows-app-title")),
            AccessNode::new(Role::List)
                .with_name(strings::t("windows-catalogue"))
                .with_bounds(sidebar_bounds)
                .with_children(rows),
        ];
        if self.compact_detail && self.area.width() < COMPACT_W {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("All Windows apps")
                    .with_action_target("windows.back")
                    .with_bounds(
                        self.hits
                            .iter()
                            .find_map(|(bounds, hit)| matches!(hit, Hit::Back).then_some(*bounds))
                            .unwrap_or(Rect::ZERO),
                    )
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        if let Some(app) = self.selected_app() {
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(format!("{} details", app.name))
                    .with_description(format!(
                        "{}; status {}; {}; selected route {}; {}",
                        app.reason,
                        app.status.label(),
                        app.installation_detail
                            .as_deref()
                            .unwrap_or("No installation details"),
                        app.route.label(),
                        app.route.security_summary()
                    )),
            );
        }
        if let Some(notice) = &self.notice {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice)
                    .with_live(Politeness::Polite),
            );
        }
        if self.confirm_delete_data.is_some() {
            children.push(
                AccessNode::new(Role::Dialog)
                    .with_name(strings::t("windows-confirm-delete-title"))
                    .with_description(strings::t("windows-confirm-delete-body"))
                    .with_bounds(
                        self.delete_confirmation_layout
                            .as_ref()
                            .map_or(Rect::ZERO, |layout| layout.card),
                    )
                    .with_state(AccessState {
                        modal: true,
                        ..Default::default()
                    })
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name(strings::t("windows-cancel"))
                            .with_action_target("windows.delete-cancel")
                            .with_state(AccessState::new().focusable(true)),
                        AccessNode::new(Role::Button)
                            .with_name(strings::t("windows-confirm-delete-action"))
                            .with_action_target("windows.delete-confirm")
                            .with_state(AccessState::new().focusable(true)),
                    ]),
            );
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name(strings::t("windows-app-title"))
                .with_description(
                    "Manage isolated Windows applications and the optional Windows appliance",
                )
                .with_bounds(self.area)
                .with_children(children),
        )
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(
            action,
            AccessibilityAction::Activate
                | AccessibilityAction::Focus
                | AccessibilityAction::ScrollIntoView
        ) {
            return false;
        }
        if target == "windows.back" {
            self.hit(Hit::Back);
            return true;
        }
        if target == "windows.delete-cancel" && self.confirm_delete_data.is_some() {
            self.cancel_delete_confirmation();
            return true;
        }
        if target == "windows.delete-confirm" && self.confirm_delete_data.is_some() {
            self.confirm_delete_data();
            return true;
        }
        let Some(app_id) = target.strip_prefix("windows.select:") else {
            return false;
        };
        let Some(index) = self
            .catalogue
            .apps
            .iter()
            .position(|candidate| candidate.id == app_id)
        else {
            return false;
        };
        self.hit(Hit::App(index));
        if let Some((bounds, _)) = self
            .hits
            .iter()
            .find(|(_, hit)| matches!(hit, Hit::App(candidate) if *candidate == index))
        {
            if bounds.y0 < self.area.y0 {
                self.sidebar_scroll = (self.sidebar_scroll - (self.area.y0 - bounds.y0)).max(0.0);
            } else if bounds.y1 > self.area.y1 {
                self.sidebar_scroll += bounds.y1 - self.area.y1;
                self.clamp_sidebar_scroll();
            }
        }
        true
    }
    fn get_rect(&self) -> Rect {
        self.area
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.refresh();
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
    fn test_id(&self) -> Option<&str> {
        Some("windows-app")
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.delete_confirmation_layout
            .as_ref()
            .map(|layout| layout.card)
    }
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = self
            .route_buttons
            .iter()
            .map(|button| {
                (
                    button.test_id().unwrap_or_default().to_string(),
                    button.get_rect(),
                )
            })
            .collect::<Vec<_>>();
        rects.push((
            "windows-use-recommendation".into(),
            self.reset_button.get_rect(),
        ));
        rects.push(("windows-launch".into(), self.launch_button.get_rect()));
        rects.push((
            "windows-open-file-in-vm".into(),
            self.open_file_button.get_rect(),
        ));
        rects.push((
            "windows-open-in-vm".into(),
            self.vm_fallback_button.get_rect(),
        ));
        rects.push(("windows-install".into(), self.install_button.get_rect()));
        rects.push((
            "windows-install-private-capsule".into(),
            self.confirm_capsule_button.get_rect(),
        ));
        rects.push((
            "windows-install-windows-appliance".into(),
            self.confirm_vm_install_button.get_rect(),
        ));
        rects.push((
            "windows-publish-vm-candidate".into(),
            self.publish_vm_candidate_button.get_rect(),
        ));
        rects.push(("windows-retry-install".into(), self.retry_button.get_rect()));
        rects.push((
            "windows-escalate-install-to-vm".into(),
            self.escalate_to_vm_button.get_rect(),
        ));
        rects.push((
            "windows-remove-launcher".into(),
            self.remove_launcher_button.get_rect(),
        ));
        rects.push((
            "windows-vendor-uninstall".into(),
            self.vendor_uninstall_button.get_rect(),
        ));
        rects.push((
            "windows-delete-data".into(),
            self.delete_data_button.get_rect(),
        ));
        rects.push((
            "windows-choose-windows-iso".into(),
            self.choose_windows_iso_button.get_rect(),
        ));
        rects.push((
            "windows-open-recovery-desktop".into(),
            self.recovery_desktop_button.get_rect(),
        ));
        rects.push((
            "windows-start-appliance".into(),
            self.start_windows_button.get_rect(),
        ));
        rects.push((
            "windows-restart-appliance".into(),
            self.restart_windows_button.get_rect(),
        ));
        rects.push((
            "windows-suspend-appliance".into(),
            self.suspend_windows_button.get_rect(),
        ));
        rects.push((
            "windows-resume-appliance".into(),
            self.resume_windows_button.get_rect(),
        ));
        rects.push((
            "windows-shutdown-appliance".into(),
            self.shutdown_windows_button.get_rect(),
        ));
        rects.push((
            "windows-connect-temporarily".into(),
            self.connect_windows_button.get_rect(),
        ));
        rects.push((
            "windows-connect-for-update".into(),
            self.update_windows_button.get_rect(),
        ));
        rects.push((
            "windows-revoke-temporary-connection".into(),
            self.revoke_windows_connection_button.get_rect(),
        ));
        rects
    }
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self
            .route_buttons
            .iter()
            .map(|button| button as &dyn Widget)
            .collect::<Vec<_>>();
        children.push(&self.reset_button);
        children.push(&self.launch_button);
        children.push(&self.open_file_button);
        children.push(&self.vm_fallback_button);
        children.push(&self.install_button);
        children.push(&self.confirm_capsule_button);
        children.push(&self.confirm_vm_install_button);
        children.push(&self.publish_vm_candidate_button);
        children.push(&self.retry_button);
        children.push(&self.escalate_to_vm_button);
        children.push(&self.remove_launcher_button);
        children.push(&self.vendor_uninstall_button);
        children.push(&self.delete_data_button);
        children.push(&self.choose_windows_iso_button);
        children.push(&self.recovery_desktop_button);
        children.push(&self.start_windows_button);
        children.push(&self.restart_windows_button);
        children.push(&self.suspend_windows_button);
        children.push(&self.resume_windows_button);
        children.push(&self.shutdown_windows_button);
        children.push(&self.connect_windows_button);
        children.push(&self.update_windows_button);
        children.push(&self.revoke_windows_connection_button);
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self
            .route_buttons
            .iter_mut()
            .map(|button| button as &mut dyn Widget)
            .collect::<Vec<_>>();
        children.push(&mut self.reset_button);
        children.push(&mut self.launch_button);
        children.push(&mut self.open_file_button);
        children.push(&mut self.vm_fallback_button);
        children.push(&mut self.install_button);
        children.push(&mut self.confirm_capsule_button);
        children.push(&mut self.confirm_vm_install_button);
        children.push(&mut self.publish_vm_candidate_button);
        children.push(&mut self.retry_button);
        children.push(&mut self.escalate_to_vm_button);
        children.push(&mut self.remove_launcher_button);
        children.push(&mut self.vendor_uninstall_button);
        children.push(&mut self.delete_data_button);
        children.push(&mut self.choose_windows_iso_button);
        children.push(&mut self.recovery_desktop_button);
        children.push(&mut self.start_windows_button);
        children.push(&mut self.restart_windows_button);
        children.push(&mut self.suspend_windows_button);
        children.push(&mut self.resume_windows_button);
        children.push(&mut self.shutdown_windows_button);
        children.push(&mut self.connect_windows_button);
        children.push(&mut self.update_windows_button);
        children.push(&mut self.revoke_windows_connection_button);
        children
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

impl ui::widget::controls::ActionControls for WindowsApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.pending_control_invocation.take()
    }
}

impl WindowsApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Ok(false);
        }
        self.invoke_effect_operation(type_id, payload)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Ok(None);
        }
        if self.resource_invocation.is_some() {
            return Err("another Windows operation is still running".into());
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
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
            app.install_signal.set(true);
            let mut doc = AutoCommit::new();
            ui::host::scoped(&services, || app.apply_control_signals(&mut doc));
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
        // Case `installer_picker_uses_the_shared_portal_and_windows_filters`.
        {
            let request = installer_pick_request();
            assert_eq!(request.title(), "Add a Windows application");
            assert_eq!(
                request.filter_names(),
                vec!["Windows applications", "All files"]
            );
        }
        // Case `appliance_iso_picker_uses_the_shared_portal`.
        {
            let request = windows_iso_pick_request();
            assert_eq!(request.title(), "Choose Windows installation media");
            assert_eq!(
                request.filter_names(),
                vec!["Windows installation media", "All files"]
            );
        }
        // Case `vm_document_picker_uses_the_shared_portal_and_defaults_read_only`.
        {
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
            app.route_signal.set(Some(Route::Vm));
            app.apply_control_signals(&mut doc);
            assert_eq!(app.catalogue.apps[0].route, Route::Vm);
            assert_eq!(app.catalogue.apps[0].capsule, capsule);
            assert!(
                actions::route_override(&doc, &app.catalogue.apps[0].id).is_some(),
                "route clicks must write the shared document"
            );
        }
        // Case `live_manager_does_not_claim_fixture_apps_are_installed`.
        {
            assert!(WindowsApp::new_live().catalogue.apps.is_empty());
        }
        // Case `shared_route_controls_have_stable_automation_ids`.
        {
            let app = WindowsApp::new_offline();
            assert_eq!(Widget::test_id(&app), Some("windows-app"));
            let ids = app
                .children()
                .iter()
                .filter_map(|child| child.test_id())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                vec![
                    "windows-route-wine",
                    "windows-route-proton",
                    "windows-route-vm",
                    "windows-use-recommendation",
                    "windows-launch",
                    "windows-open-file-in-vm",
                    "windows-open-in-vm",
                    "windows-install",
                    "windows-confirm-capsule-install",
                    "windows-confirm-vm-install",
                    "windows-publish-vm-candidate",
                    "windows-retry-install",
                    "windows-escalate-install-to-vm",
                    "windows-remove-launcher",
                    "windows-vendor-uninstall",
                    "windows-delete-data",
                    "windows-choose-windows-iso",
                    "windows-open-recovery-desktop",
                    "windows-start-appliance",
                    "windows-restart-appliance",
                    "windows-suspend-appliance",
                    "windows-resume-appliance",
                    "windows-shutdown-appliance",
                    "windows-connect-temporarily",
                    "windows-connect-for-update",
                    "windows-revoke-temporary-connection",
                ]
            );
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
        // Case `deleting_private_data_requires_an_explicit_confirmation_action`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            let mut doc = AutoCommit::new();
            app.delete_data_signal.set(true);
            app.apply_control_signals(&mut doc);
            assert!(
                app.catalogue
                    .apps
                    .iter()
                    .any(|item| item.capsule == capsule)
            );
            assert_eq!(app.confirm_delete_data.as_deref(), Some(capsule.as_str()));

            // A click on the original management action cannot confirm itself;
            // only the dedicated destructive control inside the modal can.
            app.delete_data_signal.set(true);
            app.apply_control_signals(&mut doc);
            assert_eq!(app.confirm_delete_data.as_deref(), Some(capsule.as_str()));
            app.confirm_delete_data();
            app.refresh();
            assert!(
                !app.catalogue
                    .apps
                    .iter()
                    .any(|item| item.capsule == capsule)
            );
        }
        // Case `delete_confirmation_is_an_accessible_modal_with_cancel_and_delete`.
        {
            let app = WindowsApp::new_offline_scenario("confirm-delete");
            let semantics = app.accessibility().expect("semantic root");
            let dialog = semantics
                .children
                .iter()
                .find(|node| node.role == Role::Dialog)
                .expect("destructive confirmation dialog");
            assert!(dialog.state.modal);
            assert_eq!(
                dialog.children[0].action_target.as_deref(),
                Some("windows.delete-cancel")
            );
            assert_eq!(
                dialog.children[1].action_target.as_deref(),
                Some("windows.delete-confirm")
            );
        }
        // Case `escalation_to_the_shared_windows_appliance_requires_a_second_click`.
        {
            let mut app = WindowsApp::new_offline();
            let capsule = app.catalogue.apps[0].capsule.clone();
            app.catalogue.apps[0].can_escalate_install_to_vm = true;
            let mut doc = AutoCommit::new();

            app.escalate_to_vm_signal.set(true);
            app.apply_control_signals(&mut doc);
            assert_eq!(
                app.confirm_escalate_to_vm.as_deref(),
                Some(capsule.as_str())
            );
            assert_eq!(app.catalogue.apps[0].status, crate::model::AppStatus::Ready);

            app.escalate_to_vm_signal.set(true);
            app.apply_control_signals(&mut doc);
            assert_eq!(
                app.catalogue.apps[0].status,
                crate::model::AppStatus::Installing
            );
            assert!(!app.catalogue.apps[0].can_escalate_install_to_vm);
        }
        // Case `changing_selection_deactivates_controls_that_are_not_drawn_for_the_new_app`.
        {
            let mut app = WindowsApp::new_offline();
            // Simulate the first row having been painted with every conditional
            // control active. The second row deliberately exposes none of them.
            for button in &mut app.route_buttons {
                button.set_enabled(true);
            }
            app.reset_button.set_enabled(true);
            app.launch_button.set_enabled(true);
            app.vm_fallback_button.set_enabled(true);
            app.retry_button.set_enabled(true);
            app.escalate_to_vm_button.set_enabled(true);
            app.remove_launcher_button.set_enabled(true);
            app.vendor_uninstall_button.set_enabled(true);
            app.delete_data_button.set_enabled(true);
            app.restart_windows_button.set_enabled(true);
            app.start_windows_button.set_enabled(true);
            app.suspend_windows_button.set_enabled(true);
            app.resume_windows_button.set_enabled(true);
            app.shutdown_windows_button.set_enabled(true);
            app.connect_windows_button.set_enabled(true);
            app.update_windows_button.set_enabled(true);
            app.revoke_windows_connection_button.set_enabled(true);
            app.route_signal.set(Some(Route::Wine));
            app.reset_signal.set(true);
            app.launch_signal.set(true);
            app.vm_fallback_signal.set(true);
            app.retry_signal.set(true);
            app.escalate_to_vm_signal.set(true);
            app.remove_launcher_signal.set(true);
            app.vendor_uninstall_signal.set(true);
            app.delete_data_signal.set(true);
            app.restart_windows_signal.set(true);
            app.start_windows_signal.set(true);
            app.suspend_windows_signal.set(true);
            app.resume_windows_signal.set(true);
            app.shutdown_windows_signal.set(true);
            app.connect_windows_signal.set(true);
            app.update_windows_signal.set(true);
            app.revoke_windows_connection_signal.set(true);

            let next = &mut app.catalogue.apps[1];
            next.route_override_available = false;
            next.launch_available = false;
            next.vm_fallback_available = false;
            next.can_retry_install = false;
            next.can_escalate_install_to_vm = false;
            next.can_remove_launcher = false;
            next.can_vendor_uninstall = false;
            next.can_delete_data = false;
            app.hit(Hit::App(1));

            assert!(app.route_buttons.iter().all(|button| !button.is_enabled()));
            assert!(!app.reset_button.is_enabled());
            assert!(!app.launch_button.is_enabled());
            assert!(!app.vm_fallback_button.is_enabled());
            assert!(!app.retry_button.is_enabled());
            assert!(!app.escalate_to_vm_button.is_enabled());
            assert!(!app.remove_launcher_button.is_enabled());
            assert!(!app.vendor_uninstall_button.is_enabled());
            assert!(!app.delete_data_button.is_enabled());
            assert!(!app.restart_windows_button.is_enabled());
            assert!(!app.start_windows_button.is_enabled());
            assert!(!app.suspend_windows_button.is_enabled());
            assert!(!app.resume_windows_button.is_enabled());
            assert!(!app.shutdown_windows_button.is_enabled());
            assert!(!app.connect_windows_button.is_enabled());
            assert!(!app.update_windows_button.is_enabled());
            assert!(!app.revoke_windows_connection_button.is_enabled());
            assert_eq!(app.route_signal.get(), None);
            assert!(!app.reset_signal.get());
            assert!(!app.launch_signal.get());
            assert!(!app.vm_fallback_signal.get());
            assert!(!app.retry_signal.get());
            assert!(!app.escalate_to_vm_signal.get());
            assert!(!app.remove_launcher_signal.get());
            assert!(!app.vendor_uninstall_signal.get());
            assert!(!app.delete_data_signal.get());
            assert!(!app.restart_windows_signal.get());
            assert!(!app.start_windows_signal.get());
            assert!(!app.suspend_windows_signal.get());
            assert!(!app.resume_windows_signal.get());
            assert!(!app.shutdown_windows_signal.get());
            assert!(!app.connect_windows_signal.get());
            assert!(!app.update_windows_signal.get());
            assert!(!app.revoke_windows_connection_signal.get());
        }
    }

    #[test]
    fn compact_layout_keeps_controls_reachable_and_explained() {
        // Case `sidebar_scroll_keeps_expanded_management_and_catalogue_reachable_at_600px`.
        {
            let mut app = WindowsApp::new_offline();
            app.area = Rect::new(0.0, 0.0, 900.0, 600.0);
            // Expanded appliance controls plus two catalogue rows exceed the
            // small viewport; scrolling reaches the final row without overshoot.
            app.sidebar_content_bottom = 820.0;
            app.sidebar_scroll = 10_000.0;
            app.clamp_sidebar_scroll();
            assert!((app.sidebar_scroll - 220.0).abs() < f64::EPSILON);
            app.sidebar_scroll = -10.0;
            app.clamp_sidebar_scroll();
            assert!(app.sidebar_scroll.abs() < f64::EPSILON);
        }
        // Case `disabled_open_file_uses_the_typed_reason_not_a_rail_slogan`.
        {
            assert_eq!(
                compact_portal_reason("Windows appliance is unavailable."),
                "Windows appliance is unavailable."
            );
            assert!(compact_portal_reason(&"x".repeat(60)).ends_with('…'));
        }
        // Case `compact_property_values_are_explicitly_ellipsized`.
        {
            assert_eq!(compact_detail_value("short", true), "short");
            assert_eq!(
                compact_detail_value(&"x".repeat(40), true),
                format!("{}…", "x".repeat(33))
            );
            assert_eq!(compact_detail_value(&"x".repeat(40), false), "x".repeat(40));
        }
        // Case `compact_layout_uses_a_reversible_list_detail_flow`.
        {
            let mut app = WindowsApp::new_offline();
            app.area = Rect::new(0.0, 0.0, 480.0, 800.0);
            app.hit(Hit::App(1));
            assert!(app.compact_detail);
            assert_eq!(app.selected, 1);
            app.hit(Hit::Back);
            assert!(!app.compact_detail);
            assert_eq!(app.selected, 1, "back preserves the user's selection");
        }
        // Case `compact_recovery_actions_wrap_inside_a_480px_detail_pane`.
        {
            let detail = Rect::new(20.0, 0.0, 460.0, 800.0);
            let mut cursor = Point::new(detail.x0, 710.0);
            for width in [118.0, 220.0, 142.0, 142.0, 132.0] {
                let rect = WindowsApp::flow_button_rect(&mut cursor, width, detail);
                assert!(rect.x0 >= detail.x0 && rect.x1 <= detail.x1, "{rect:?}");
                assert!(rect.y1 <= detail.y1, "{rect:?}");
            }
        }
    }

    #[test]
    fn assistive_technology_sees_catalogue_rows_and_recovery() {
        // Case `accessibility_names_and_routes_custom_catalogue_rows`.
        {
            let mut app = WindowsApp::new_offline();
            app.area = Rect::new(0.0, 0.0, 900.0, 600.0);
            app.hits = vec![
                (Rect::new(10.0, 200.0, 286.0, 266.0), Hit::App(0)),
                (Rect::new(10.0, 272.0, 286.0, 338.0), Hit::App(1)),
            ];
            let semantics = app.accessibility().expect("semantic root");
            assert_eq!(semantics.role, Role::Window);
            let list = semantics
                .children
                .iter()
                .find(|node| node.role == Role::List)
                .expect("catalogue list");
            assert_eq!(list.children.len(), 2);
            assert_eq!(list.children[1].name.as_deref(), Some("Cyberpunk 2077"));
            assert!(app.perform_semantic_action(
                "windows.select:cyberpunk-2077",
                &AccessibilityAction::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(app.selected, 1);
            assert!(!app.perform_semantic_action(
                "windows.select:stale-id",
                &AccessibilityAction::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
        }
        // Case `failed_setup_exposes_disabled_launch_and_recovery_to_assistive_technology`.
        {
            let mut provider = SampleProvider::new();
            provider.seed_setup_failure();
            let mut app = WindowsApp::with_provider(Box::new(provider));
            let failed = app.selected_app().expect("failed sample application");
            assert_eq!(failed.status, crate::model::AppStatus::Failed);
            assert!(!failed.launch_available);
            assert!(failed.can_retry_install);
            assert!(failed.can_escalate_install_to_vm);

            app.launch_button.set_enabled(false);
            app.retry_button.set_enabled(true);
            app.escalate_to_vm_button.set_enabled(true);
            assert!(
                Widget::accessibility(&app.launch_button)
                    .expect("launch semantics")
                    .state
                    .disabled
            );
            for button in [&app.retry_button, &app.escalate_to_vm_button] {
                let semantics = Widget::accessibility(button).expect("recovery semantics");
                assert!(semantics.state.focusable);
                assert!(!semantics.state.disabled);
            }

            let semantics = app.accessibility().expect("app semantics");
            let detail = semantics
                .children
                .iter()
                .find(|node| matches!(node.role, Role::Group))
                .expect("detail semantics");
            assert!(
                detail
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("Setup failed"))
            );
        }
    }
}
