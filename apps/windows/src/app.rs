//! Immediate-mode `QuickUI` surface for compatibility routing.
use crate::actions;
use crate::appliance_owner::ApplianceOwnerClient;
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
use ui::portal::{FileFilter, OpenFileChannel, OpenFileOutcome, PickRequest};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonStyle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

const PAD: f64 = 20.0;
const SIDEBAR_W: f64 = 296.0;
const ROW_H: f64 = 72.0;

fn installer_pick_request() -> PickRequest {
    PickRequest::open("Add a Windows application", "Install")
        .thread_name("qos-windows-installer-picker")
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
        .thread_name("qos-windows-iso-picker")
        .filter(
            FileFilter::new("Windows installation media")
                .glob("*.iso")
                .mimetype("application/x-cd-image")
                .mimetype("application/x-iso9660-image"),
        )
        .all_files()
}

#[derive(Clone, Copy)]
enum Hit {
    App(usize),
}

pub struct WindowsApp {
    provider: Box<dyn CompatibilityProvider>,
    catalogue: Catalogue,
    selected: usize,
    area: Rect,
    mouse: Point,
    hits: Vec<(Rect, Hit)>,
    node: Option<NodeId>,
    notice: Option<String>,
    route_buttons: Vec<Button>,
    reset_button: Button,
    launch_button: Button,
    vm_fallback_button: Button,
    install_button: Button,
    confirm_capsule_button: Button,
    confirm_vm_install_button: Button,
    publish_vm_candidate_button: Button,
    retry_button: Button,
    remove_launcher_button: Button,
    vendor_uninstall_button: Button,
    delete_data_button: Button,
    choose_windows_iso_button: Button,
    recovery_desktop_button: Button,
    route_signal: Rc<Cell<Option<Route>>>,
    reset_signal: Rc<Cell<bool>>,
    launch_signal: Rc<Cell<bool>>,
    vm_fallback_signal: Rc<Cell<bool>>,
    install_signal: Rc<Cell<bool>>,
    confirm_capsule_signal: Rc<Cell<bool>>,
    confirm_vm_install_signal: Rc<Cell<bool>>,
    publish_vm_candidate_signal: Rc<Cell<bool>>,
    retry_signal: Rc<Cell<bool>>,
    remove_launcher_signal: Rc<Cell<bool>>,
    vendor_uninstall_signal: Rc<Cell<bool>>,
    delete_data_signal: Rc<Cell<bool>>,
    choose_windows_iso_signal: Rc<Cell<bool>>,
    recovery_desktop_signal: Rc<Cell<bool>>,
    portal_rx: Option<OpenFileChannel>,
    iso_portal_rx: Option<OpenFileChannel>,
    appliance_owner: ApplianceOwnerClient,
    confirm_vendor_uninstall: Option<String>,
    confirm_delete_data: Option<String>,
}

impl WindowsApp {
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
    #[allow(
        clippy::too_many_lines,
        reason = "the constructor keeps retained controls and their signals adjacent for auditable UI wiring"
    )]
    pub fn with_provider(mut provider: Box<dyn CompatibilityProvider>) -> Self {
        let catalogue = provider.catalogue();
        let route_signal = Rc::new(Cell::new(None));
        let reset_signal = Rc::new(Cell::new(false));
        let launch_signal = Rc::new(Cell::new(false));
        let vm_fallback_signal = Rc::new(Cell::new(false));
        let install_signal = Rc::new(Cell::new(false));
        let retry_signal = Rc::new(Cell::new(false));
        let remove_launcher_signal = Rc::new(Cell::new(false));
        let vendor_uninstall_signal = Rc::new(Cell::new(false));
        let delete_data_signal = Rc::new(Cell::new(false));
        let choose_windows_iso_signal = Rc::new(Cell::new(false));
        let recovery_desktop_signal = Rc::new(Cell::new(false));
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
                .on_click(move || signal.set(Some(route)))
            })
            .collect();
        let reset = Rc::clone(&reset_signal);
        let launch = Rc::clone(&launch_signal);
        let vm_fallback = Rc::clone(&vm_fallback_signal);
        let install = Rc::clone(&install_signal);
        let confirm_capsule_signal = Rc::new(Cell::new(false));
        let confirm_vm_install_signal = Rc::new(Cell::new(false));
        let publish_vm_candidate_signal = Rc::new(Cell::new(false));
        let retry = Rc::clone(&retry_signal);
        let remove_launcher = Rc::clone(&remove_launcher_signal);
        let vendor_uninstall = Rc::clone(&vendor_uninstall_signal);
        let delete_data = Rc::clone(&delete_data_signal);
        let choose_windows_iso = Rc::clone(&choose_windows_iso_signal);
        let recovery_desktop = Rc::clone(&recovery_desktop_signal);
        Self {
            provider,
            catalogue,
            selected: 0,
            area: Rect::ZERO,
            mouse: Point::ZERO,
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
            .on_click(move || reset.set(true)),
            launch_button: Button::new("windows-launch", "windows-launch", None)
                .with_size(132.0, 32.0)
                .with_enabled(false)
                .on_click(move || launch.set(true)),
            vm_fallback_button: Button::new("windows-open-in-vm", "windows-open-in-vm", None)
                .with_size(174.0, 32.0)
                .with_enabled(false)
                .on_click(move || vm_fallback.set(true)),
            install_button: Button::new("windows-install", "windows-install", None)
                .with_size(256.0, 32.0)
                .on_click(move || install.set(true)),
            confirm_capsule_button: Button::new(
                "windows-confirm-capsule-install",
                "windows-install-private-capsule",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_click({
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
            .on_click({
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
            .on_click({
                let signal = Rc::clone(&publish_vm_candidate_signal);
                move || signal.set(true)
            }),
            retry_button: Button::new("windows-retry-install", "windows-retry-install", None)
                .with_size(118.0, 32.0)
                .with_enabled(false)
                .on_click(move || retry.set(true)),
            remove_launcher_button: Button::new(
                "windows-remove-launcher",
                "windows-remove-launcher",
                None,
            )
            .with_size(142.0, 32.0)
            .with_enabled(false)
            .on_click(move || remove_launcher.set(true)),
            vendor_uninstall_button: Button::new(
                "windows-vendor-uninstall",
                "windows-vendor-uninstall",
                None,
            )
            .with_size(142.0, 32.0)
            .with_enabled(false)
            .on_click(move || vendor_uninstall.set(true)),
            delete_data_button: Button::new("windows-delete-data", "windows-delete-data", None)
                .with_size(132.0, 32.0)
                .with_enabled(false)
                .on_click(move || delete_data.set(true)),
            choose_windows_iso_button: Button::new(
                "windows-choose-windows-iso",
                "windows-choose-windows-iso",
                None,
            )
            .with_size(256.0, 32.0)
            .on_click(move || choose_windows_iso.set(true)),
            recovery_desktop_button: Button::new(
                "windows-open-recovery-desktop",
                "windows-open-recovery-desktop",
                None,
            )
            .with_size(256.0, 32.0)
            .with_enabled(false)
            .on_click(move || recovery_desktop.set(true)),
            route_signal,
            reset_signal,
            launch_signal,
            vm_fallback_signal,
            install_signal,
            confirm_capsule_signal,
            confirm_vm_install_signal,
            publish_vm_candidate_signal,
            retry_signal,
            remove_launcher_signal,
            vendor_uninstall_signal,
            delete_data_signal,
            choose_windows_iso_signal,
            recovery_desktop_signal,
            portal_rx: None,
            iso_portal_rx: None,
            appliance_owner: ApplianceOwnerClient::new(),
            confirm_vendor_uninstall: None,
            confirm_delete_data: None,
        }
    }
    fn selected_app(&self) -> Option<&crate::model::WindowsApp> {
        self.catalogue.apps.get(self.selected)
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
        self.vm_fallback_button.set_enabled(false);
        self.confirm_capsule_button.set_enabled(false);
        self.confirm_vm_install_button.set_enabled(false);
        self.publish_vm_candidate_button.set_enabled(false);
        self.retry_button.set_enabled(false);
        self.remove_launcher_button.set_enabled(false);
        self.vendor_uninstall_button.set_enabled(false);
        self.delete_data_button.set_enabled(false);

        self.route_signal.set(None);
        self.reset_signal.set(false);
        self.launch_signal.set(false);
        self.vm_fallback_signal.set(false);
        self.confirm_capsule_signal.set(false);
        self.confirm_vm_install_signal.set(false);
        self.publish_vm_candidate_signal.set(false);
        self.retry_signal.set(false);
        self.remove_launcher_signal.set(false);
        self.vendor_uninstall_signal.set(false);
        self.delete_data_signal.set(false);
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
            }
            Ok(OpenFileOutcome::Failed(error)) => {
                self.portal_rx = None;
                self.notice = Some(format!("Could not open the file picker: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
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
            Ok(OpenFileOutcome::Cancelled) => self.iso_portal_rx = None,
            Ok(OpenFileOutcome::Failed(error)) => {
                self.iso_portal_rx = None;
                self.notice = Some(format!("Could not open the Windows ISO picker: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.iso_portal_rx = None;
                self.notice = Some(strings::t("windows-picker-disconnected"));
            }
        }
    }
    fn refresh(&mut self) {
        if let Some(notice) = self.appliance_owner.drain() {
            self.notice = Some(notice);
        }
        self.deactivate_selection_controls();
        self.catalogue = self.provider.catalogue();
        if let Some(notice) = self.provider.take_notice() {
            self.notice = Some(notice);
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
                self.confirm_vendor_uninstall = None;
                self.confirm_delete_data = None;
            }
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered dispatcher drains the retained controls before mutating the selected model"
    )]
    fn apply_control_signals(&mut self) {
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
        if let Some(route) = self.route_signal.take()
            && let Some(app) = self.selected_app()
            && app.route_override_available
            && (route != Route::Vm || app.launch_available)
        {
            let id = app.id.clone();
            self.notice = self.provider.set_route_override(&id, route).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-route-set"));
            }
            self.refresh();
        }
        if self.reset_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.route_override_available
            && app.uses_override()
        {
            let id = app.id.clone();
            self.notice = self.provider.reset_route_override(&id).err();
            if self.notice.is_none() {
                self.notice = Some(strings::t("windows-route-reset"));
            }
            self.refresh();
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
                    self.notice = Some(strings::t("windows-uninstalling"));
                }
            } else {
                self.confirm_vendor_uninstall = Some(management_id);
                self.confirm_delete_data = None;
                self.notice = Some(strings::t("windows-confirm-uninstall"));
            }
        }
        if self.delete_data_signal.replace(false)
            && let Some(app) = self.selected_app()
            && app.can_delete_data
        {
            let capsule = app.capsule.clone();
            if self.confirm_delete_data.as_deref() == Some(capsule.as_str()) {
                self.confirm_delete_data = None;
                self.notice = self.provider.delete_data(&capsule).err();
                if self.notice.is_none() {
                    self.notice = Some(strings::t("windows-data-deleted"));
                }
            } else {
                self.confirm_delete_data = Some(capsule);
                self.confirm_vendor_uninstall = None;
                self.notice = Some(strings::t("windows-confirm-delete"));
            }
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
}

impl Widget for WindowsApp {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        self.drain_portal();
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
        self.hits.clear();
        let sidebar = Rect::new(
            self.area.x0,
            self.area.y0,
            (self.area.x0 + SIDEBAR_W).min(self.area.x1),
            self.area.y1,
        );
        Self::fill(scene, t, sidebar, theme.bg_primary);
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
            self.confirm_capsule_button
                .set_style(if confirmation.recommended_route == Route::Vm {
                    ButtonStyle::Neutral
                } else {
                    ButtonStyle::Primary
                });
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
            appliance.phase_label(),
            11.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        if let Some(detail) = &appliance.detail {
            Self::text(
                tr,
                scene,
                t,
                Point::new(sidebar.x0 + PAD, appliance_y + 34.0),
                detail,
                10.0,
                theme.text_dim,
                TextWeight::Regular,
            );
        }
        Self::text(
            tr,
            scene,
            t,
            Point::new(sidebar.x0 + PAD, appliance_y + 48.0),
            &strings::t("windows-appliance-desktop-note"),
            10.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        if appliance.is_provisioned() {
            let recovery = Rect::new(
                sidebar.x0 + PAD,
                appliance_y + 60.0,
                sidebar.x1 - PAD,
                appliance_y + 92.0,
            );
            self.recovery_desktop_button.apply_layout_external(recovery);
            self.recovery_desktop_button.set_style(ButtonStyle::Neutral);
            self.recovery_desktop_button
                .set_enabled(appliance.recovery_available && !self.appliance_owner.busy());
            self.recovery_desktop_button
                .draw(scene, t, tr, theme, presence);
        } else {
            let choose_iso = Rect::new(
                sidebar.x0 + PAD,
                appliance_y + 60.0,
                sidebar.x1 - PAD,
                appliance_y + 92.0,
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
        let mut y = appliance_y + 108.0;
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
        let detail = Rect::new(
            sidebar.x1 + PAD,
            self.area.y0 + PAD,
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
        self.launch_button.set_style(ButtonStyle::Primary);
        self.launch_button.draw(scene, t, tr, theme, presence);
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
        let card = Rect::new(detail.x0, detail.y0 + 118.0, detail.x1, detail.y0 + 238.0);
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
        Self::text(
            tr,
            scene,
            t,
            Point::new(card.x0 + PAD, card.y0 + 44.0),
            &app.reason,
            13.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(card.x0 + PAD, card.y0 + 74.0),
            app.route.security_summary(),
            12.0,
            theme.text_dim,
            TextWeight::Regular,
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
        if app.uses_override() {
            let reset = Rect::new(
                detail.x0 + 460.0,
                routes_y - 8.0,
                detail.x0 + 655.0,
                routes_y + 24.0,
            );
            self.reset_button.apply_layout_external(reset);
            self.reset_button.set_style(ButtonStyle::Neutral);
            self.reset_button.set_enabled(app.route_override_available);
            self.reset_button.draw(scene, t, tr, theme, presence);
        }
        let info = Rect::new(
            detail.x0,
            routes_y + 48.0,
            detail.x1,
            (routes_y + 226.0).min(detail.y1),
        );
        Self::pill(scene, t, info, theme.bg_primary);
        Self::text(
            tr,
            scene,
            t,
            Point::new(info.x0 + PAD, info.y0 + 16.0),
            &strings::t(if app.route == Route::Vm {
                "windows-appliance-access"
            } else {
                "windows-capsule-access"
            }),
            14.0,
            theme.text_primary,
            TextWeight::Bold,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 46.0,
            &strings::t("windows-runner"),
            &app.runner,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 71.0,
            &strings::t(if app.route == Route::Vm {
                "windows-appliance"
            } else {
                "windows-capsule"
            }),
            &app.capsule,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 96.0,
            &strings::t(if app.route == Route::Vm {
                "windows-guest-storage"
            } else {
                "windows-private-data"
            }),
            &app.prefix,
            theme,
        );
        Self::line(
            tr,
            scene,
            t,
            info.x0 + PAD,
            info.y0 + 121.0,
            &strings::t("windows-host-access"),
            &app.access_summary(),
            theme,
        );
        Self::text(
            tr,
            scene,
            t,
            Point::new(info.x0 + PAD, info.y0 + 151.0),
            &app.fallback_detail,
            12.0,
            theme.text_dim,
            TextWeight::Regular,
        );
        let manage_y = (info.y1 + 48.0).min(detail.y1 - 70.0);
        let mut manage_x = detail.x0;
        if app.can_retry_install
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
            let rect = Rect::new(manage_x, manage_y, manage_x + 190.0, manage_y + 32.0);
            self.publish_vm_candidate_button.apply_layout_external(rect);
            self.publish_vm_candidate_button
                .set_style(ButtonStyle::Primary);
            self.publish_vm_candidate_button.set_enabled(true);
            self.publish_vm_candidate_button
                .draw(scene, t, tr, theme, presence);
            manage_x += 200.0;
        }
        if app.can_retry_install {
            let rect = Rect::new(manage_x, manage_y, manage_x + 118.0, manage_y + 32.0);
            self.retry_button.apply_layout_external(rect);
            self.retry_button.set_style(ButtonStyle::Neutral);
            self.retry_button.set_enabled(true);
            self.retry_button.draw(scene, t, tr, theme, presence);
            manage_x += 128.0;
        }
        if app.can_remove_launcher {
            let rect = Rect::new(manage_x, manage_y, manage_x + 142.0, manage_y + 32.0);
            self.remove_launcher_button.apply_layout_external(rect);
            self.remove_launcher_button.set_style(ButtonStyle::Neutral);
            self.remove_launcher_button.set_enabled(true);
            self.remove_launcher_button
                .draw(scene, t, tr, theme, presence);
            manage_x += 152.0;
        }
        if app.can_vendor_uninstall {
            let rect = Rect::new(manage_x, manage_y, manage_x + 142.0, manage_y + 32.0);
            self.vendor_uninstall_button.apply_layout_external(rect);
            self.vendor_uninstall_button.set_style(ButtonStyle::Neutral);
            self.vendor_uninstall_button.set_enabled(true);
            self.vendor_uninstall_button
                .draw(scene, t, tr, theme, presence);
            manage_x += 152.0;
        }
        if app.can_delete_data {
            let rect = Rect::new(manage_x, manage_y, manage_x + 132.0, manage_y + 32.0);
            self.delete_data_button.apply_layout_external(rect);
            self.delete_data_button.set_style(ButtonStyle::Neutral);
            self.delete_data_button.set_enabled(true);
            self.delete_data_button.draw(scene, t, tr, theme, presence);
        }
        if let Some(notice) = &self.notice {
            Self::text(
                tr,
                scene,
                t,
                Point::new(detail.x0, detail.y1 - 20.0),
                notice,
                12.0,
                theme.status_success,
                TextWeight::Regular,
            );
        }
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        mods: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        for button in &mut self.route_buttons {
            button.handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        }
        self.reset_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.launch_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.vm_fallback_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.install_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.confirm_capsule_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.confirm_vm_install_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.publish_vm_candidate_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.retry_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.remove_launcher_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.vendor_uninstall_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.delete_data_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.choose_windows_iso_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.recovery_desktop_button
            .handle_event(event, scale, mods, state, path, handled, focus, node, taffy);
        self.apply_control_signals();
        match event {
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
                    *handled = true;
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "WindowsApp"
    }
    fn get_rect(&self) -> Rect {
        self.area
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        actions::provide_resources(&self.catalogue)
    }
    fn test_id(&self) -> Option<&str> {
        Some("windows-app")
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
        children.push(&self.vm_fallback_button);
        children.push(&self.install_button);
        children.push(&self.confirm_capsule_button);
        children.push(&self.confirm_vm_install_button);
        children.push(&self.publish_vm_candidate_button);
        children.push(&self.retry_button);
        children.push(&self.remove_launcher_button);
        children.push(&self.vendor_uninstall_button);
        children.push(&self.delete_data_button);
        children.push(&self.choose_windows_iso_button);
        children.push(&self.recovery_desktop_button);
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
        children.push(&mut self.vm_fallback_button);
        children.push(&mut self.install_button);
        children.push(&mut self.confirm_capsule_button);
        children.push(&mut self.confirm_vm_install_button);
        children.push(&mut self.publish_vm_candidate_button);
        children.push(&mut self.retry_button);
        children.push(&mut self.remove_launcher_button);
        children.push(&mut self.vendor_uninstall_button);
        children.push(&mut self.delete_data_button);
        children.push(&mut self.choose_windows_iso_button);
        children.push(&mut self.recovery_desktop_button);
        children
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    struct RecordingProvider {
        inner: SampleProvider,
        installed: Arc<Mutex<Option<(Vec<u8>, String)>>>,
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
    fn installer_portal_handoff_uses_its_open_descriptor_and_display_name() {
        let installed = Arc::new(Mutex::new(None));
        let provider = RecordingProvider {
            inner: SampleProvider::new(),
            installed: Arc::clone(&installed),
        };
        let mut app = WindowsApp::with_provider(Box::new(provider));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected-installer.msi");
        std::fs::write(&path, b"accepted descriptor bytes").unwrap();
        let source = File::open(&path).unwrap();
        app.portal_rx = Some(ui::portal::inject_open_file_for_test(
            source,
            "display-name.msi",
        ));
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

    #[test]
    fn override_updates_runner_without_changing_capsule_identity() {
        let mut app = WindowsApp::new_offline();
        let capsule = app.catalogue.apps[0].capsule.clone();
        app.route_signal.set(Some(Route::Vm));
        app.apply_control_signals();
        assert_eq!(app.catalogue.apps[0].route, Route::Vm);
        assert_eq!(app.catalogue.apps[0].capsule, capsule);
    }

    #[test]
    fn live_manager_does_not_claim_fixture_apps_are_installed() {
        assert!(WindowsApp::new_live().catalogue.apps.is_empty());
    }

    #[test]
    fn shared_route_controls_have_stable_automation_ids() {
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
                "windows-open-in-vm",
                "windows-install",
                "windows-install-private-capsule",
                "windows-install-windows-appliance",
                "windows-publish-vm-candidate",
                "windows-retry-install",
                "windows-remove-launcher",
                "windows-vendor-uninstall",
                "windows-delete-data",
                "windows-choose-windows-iso",
                "windows-open-recovery-desktop",
            ]
        );
    }

    #[test]
    fn installer_picker_uses_the_shared_portal_and_windows_filters() {
        let request = installer_pick_request();
        assert_eq!(request.title(), "Add a Windows application");
        assert_eq!(
            request.filter_names(),
            vec!["Windows applications", "All files"]
        );
    }

    #[test]
    fn appliance_iso_picker_uses_the_shared_portal() {
        let request = windows_iso_pick_request();
        assert_eq!(request.title(), "Choose Windows installation media");
        assert_eq!(
            request.filter_names(),
            vec!["Windows installation media", "All files"]
        );
    }

    #[test]
    fn deleting_private_data_requires_a_second_explicit_click() {
        let mut app = WindowsApp::new_offline();
        let capsule = app.catalogue.apps[0].capsule.clone();
        app.delete_data_signal.set(true);
        app.apply_control_signals();
        assert!(
            app.catalogue
                .apps
                .iter()
                .any(|item| item.capsule == capsule)
        );
        assert_eq!(app.confirm_delete_data.as_deref(), Some(capsule.as_str()));

        app.delete_data_signal.set(true);
        app.apply_control_signals();
        app.refresh();
        assert!(
            !app.catalogue
                .apps
                .iter()
                .any(|item| item.capsule == capsule)
        );
    }

    #[test]
    fn changing_selection_deactivates_controls_that_are_not_drawn_for_the_new_app() {
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
        app.remove_launcher_button.set_enabled(true);
        app.vendor_uninstall_button.set_enabled(true);
        app.delete_data_button.set_enabled(true);
        app.route_signal.set(Some(Route::Wine));
        app.reset_signal.set(true);
        app.launch_signal.set(true);
        app.vm_fallback_signal.set(true);
        app.retry_signal.set(true);
        app.remove_launcher_signal.set(true);
        app.vendor_uninstall_signal.set(true);
        app.delete_data_signal.set(true);

        let next = &mut app.catalogue.apps[1];
        next.route_override_available = false;
        next.launch_available = false;
        next.vm_fallback_available = false;
        next.can_retry_install = false;
        next.can_remove_launcher = false;
        next.can_vendor_uninstall = false;
        next.can_delete_data = false;
        app.hit(Hit::App(1));

        assert!(app.route_buttons.iter().all(|button| !button.is_enabled()));
        assert!(!app.reset_button.is_enabled());
        assert!(!app.launch_button.is_enabled());
        assert!(!app.vm_fallback_button.is_enabled());
        assert!(!app.retry_button.is_enabled());
        assert!(!app.remove_launcher_button.is_enabled());
        assert!(!app.vendor_uninstall_button.is_enabled());
        assert!(!app.delete_data_button.is_enabled());
        assert_eq!(app.route_signal.get(), None);
        assert!(!app.reset_signal.get());
        assert!(!app.launch_signal.get());
        assert!(!app.vm_fallback_signal.get());
        assert!(!app.retry_signal.get());
        assert!(!app.remove_launcher_signal.get());
        assert!(!app.vendor_uninstall_signal.get());
        assert!(!app.delete_data_signal.get());
    }
}
