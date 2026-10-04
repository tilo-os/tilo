//! The Passwords window: the shared [`NavShell`] with the vault's places in a
//! [`NavList`], the items of the chosen place in the overview, and the open
//! item, the editor or a tool page in the content. The shell owns the header
//! row, the sidebar toggle, the widths, the drawer and the push stack; this
//! module keeps them in step with [`Core`].

use std::cell::RefCell;
use std::rc::Rc;

use accounts::SecretItemKind;
use automerge::{AutoCommit, ObjId};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    AppModel, Button, Element, ModelPath, NavItem, NavList, NavPane, NavSection, NavShell, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::list_pane::ListPane;
use crate::search::VaultSearchProvider;
use crate::vault::{Core, Dialog, Request, Requests, Screen, Shared, VaultFilter};

/// The `--shots --scenario` names this app stages.
pub const SCENARIOS: &[&str] = &[
    "detail",
    "editor",
    "new-item",
    "favorites",
    "review",
    "app-access",
    "transfer",
    "import",
    "export",
    "backup",
    "delete",
    "long-list",
    "empty",
    "sidebar",
];

/// Layouts at one window size before a capture stages shell state.
const SETTLED_LAYOUTS: u32 = 3;

fn filter_icon(filter: VaultFilter) -> Icon {
    match filter {
        VaultFilter::All => Icon::LayoutGrid,
        VaultFilter::Favorites => Icon::Star,
        VaultFilter::Kind(SecretItemKind::Login) => Icon::Globe,
        VaultFilter::Kind(SecretItemKind::ApiKey) => Icon::Code,
        VaultFilter::Kind(SecretItemKind::SecureNote) => Icon::FileText,
        VaultFilter::Kind(SecretItemKind::Wifi) => Icon::Wifi,
        VaultFilter::Kind(SecretItemKind::Other) => Icon::Layers,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Core {
    fn nav_sections(&self) -> Vec<NavSection> {
        let vault = VaultFilter::ALL
            .into_iter()
            .map(|filter| {
                let item = NavItem::new(format!("filter:{}", filter.as_str()), filter.label())
                    .icon(filter_icon(filter));
                match self.count(filter) {
                    0 => item,
                    n => item.count(count(n)),
                }
            })
            .collect();
        let mut access = NavItem::new("app-access", "App access").icon(Icon::Monitor);
        if !self.app_grants.is_empty() {
            access = access.count(count(self.app_grants.len()));
        }
        vec![
            NavSection::new("vault", "Vault", vault),
            NavSection::new(
                "tools",
                "Tools",
                vec![
                    NavItem::new("review", "Password review").icon(Icon::Shield),
                    access,
                    NavItem::new("transfer", "Import and export").icon(Icon::ArrowUpDown),
                ],
            ),
        ]
    }
}

/// The sidebar: the shared [`NavList`], kept in step with the vault.
struct Sidebar {
    list: NavList,
    core: Shared,
    sections: Vec<NavSection>,
    selected: String,
}

impl Sidebar {
    fn new(core: &Shared, requests: &Requests) -> Self {
        let (sections, selected) = {
            let core = core.borrow();
            (core.nav_sections(), core.place_id())
        };
        let requests = Rc::clone(requests);
        let list = NavList::new(sections.clone())
            .with_id("passwords.sidebar")
            .with_accessible_name("Vault")
            .selected(&selected)
            .on_select(move |id| requests.borrow_mut().push(Request::Navigate(id)));
        Self {
            list,
            core: Rc::clone(core),
            sections,
            selected,
        }
    }

    fn refresh(&mut self) {
        let (sections, selected) = {
            let core = self.core.borrow();
            (core.nav_sections(), core.place_id())
        };
        if sections != self.sections {
            self.list.set_sections(sections.clone());
            self.sections = sections;
        }
        if self.selected != selected
            || self.list.selected_id().as_deref() != Some(selected.as_str())
        {
            self.list.set_selected(Some(&selected));
            self.selected = selected;
        }
    }
}

impl ui::widget::Decorator for Sidebar {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.list
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.list, state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.refresh();
        Widget::update(&mut self.list, state, path)
    }

    fn name(&self) -> &'static str {
        "PasswordsSidebar"
    }
}

/// The application root.
pub struct PasswordsSurface {
    core: Shared,
    requests: Requests,
    shell: NavShell,
    search: VaultSearchProvider,
    /// The page revision the content column was built from.
    built_page: Option<u64>,
    /// The list revision the overview title and search index follow.
    built_list: Option<u64>,
    overview_title: String,
    shows_list: bool,
    /// Shell state a capture stages once the window has its size.
    pending: Option<String>,
    settled_layouts: u32,
    last_window: vello::kurbo::Rect,
    /// The toast last painted, so a new or expired one repaints the window.
    painted_toast: Option<String>,
}

impl PasswordsSurface {
    fn with_core(core: Core) -> Self {
        let core: Shared = Rc::new(RefCell::new(core));
        let requests: Requests = Rc::default();
        let new_item = {
            let requests = Rc::clone(&requests);
            Button::icon_only("passwords.add", Icon::Plus, "New item")
                .on_effect(move || requests.borrow_mut().push(Request::NewItem))
        };
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        let title = core.borrow().filter.label().to_owned();
        let shell = NavShell::new()
            .with_id("passwords.shell")
            .sidebar(Box::new(Sidebar::new(&core, &requests)))
            .overview(Box::new(ListPane::new(&core, &requests)))
            .overview_header(
                title.clone(),
                vec![Box::new(search) as Element, Box::new(new_item)],
            );
        let mut surface = Self {
            core,
            requests,
            shell,
            search: VaultSearchProvider::new(),
            built_page: None,
            built_list: None,
            overview_title: title,
            shows_list: true,
            pending: None,
            settled_layouts: 0,
            last_window: vello::kurbo::Rect::ZERO,
            painted_toast: None,
        };
        surface.settle(&mut AutoCommit::new());
        surface
    }

    /// The live app on the person's vault (opened on the first sync).
    pub fn live() -> Self {
        Self::with_core(Core::live())
    }

    /// The fixture vault, for tests and captures.
    pub fn demo() -> Self {
        Self::with_core(Core::demo())
    }

    #[cfg(test)]
    pub(crate) fn with_model(model: crate::PasswordsModel) -> Self {
        Self::with_core(Core::with_model(model))
    }

    #[cfg(test)]
    pub(crate) fn core(&self) -> std::cell::Ref<'_, Core> {
        self.core.borrow()
    }

    #[cfg(test)]
    pub(crate) fn core_mut(&self) -> std::cell::RefMut<'_, Core> {
        self.core.borrow_mut()
    }

    #[cfg(test)]
    pub(crate) fn shell(&self) -> &NavShell {
        &self.shell
    }

    /// Apply what controls asked for, then bring the window in step: the
    /// content page, the overview, its title, the front pane and the
    /// search index.
    pub(crate) fn settle(&mut self, doc: &mut AutoCommit) {
        loop {
            let requests = std::mem::take(&mut *self.requests.borrow_mut());
            if requests.is_empty() {
                break;
            }
            for request in requests {
                self.core.borrow_mut().apply(request, doc);
            }
        }
        self.core.borrow_mut().expire();
        let (page, list, screen, title, front) = {
            let mut core = self.core.borrow_mut();
            (
                core.page_revision,
                core.list_revision,
                core.screen,
                core.filter.label().to_owned(),
                core.show_pane.take(),
            )
        };
        if self.built_page != Some(page) {
            self.built_page = Some(page);
            let content = {
                let core = self.core.borrow();
                crate::pages::content(&core, &self.core, &self.requests)
            };
            self.shell.set_content(content);
        }
        if screen.shows_list() != self.shows_list {
            self.shows_list = screen.shows_list();
            self.shell.set_overview(
                self.shows_list
                    .then(|| Box::new(ListPane::new(&self.core, &self.requests)) as Element),
            );
        }
        if self.built_list != Some(list) {
            self.built_list = Some(list);
            self.search.replace(&self.core.borrow().items);
        }
        if title != self.overview_title {
            self.shell.set_overview_title(title.clone());
            self.overview_title = title;
        }
        if let Some(front) = front {
            self.shell
                .show(if front == NavPane::Overview && !self.shows_list {
                    NavPane::Content
                } else {
                    front
                });
        }
    }

    pub(crate) fn attach_framework(
        &self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Passwords>,
    ) {
        self.core.borrow_mut().actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.core.borrow().actions.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&self) {
        self.core.borrow_mut().actions.mark_hosted_for_test();
    }

    /// Adopt collaborative view prefs; never clear the local vault cache.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::VaultState) {
        self.core.borrow_mut().adopt_resource_state(state);
        self.settle(&mut AutoCommit::new());
    }

    #[cfg(test)]
    pub(crate) fn commit_view(
        &self,
        doc: &mut AutoCommit,
        action: &crate::resource::PasswordsAction,
    ) {
        self.core.borrow_mut().commit_view(doc, action);
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<ui::tilo_actions::EncodedAction> {
        self.core.borrow().actions.last_dispatched().cloned()
    }

    #[cfg(test)]
    pub(crate) fn item_count(&self) -> usize {
        self.core.borrow().items.len()
    }

    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        self.core.borrow().publish_resources(doc)
    }

    /// Stage a capture on the fixture vault: `None` is the default frame.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let mut doc = AutoCommit::new();
        {
            let mut core = self.core.borrow_mut();
            core.load_demo();
            match scenario {
                Some("editor") => core.begin_edit(),
                Some("new-item") => core.begin_add(),
                Some("favorites") => core.select_filter(&mut doc, VaultFilter::Favorites),
                Some("review") => core.navigate(&mut doc, "review"),
                Some("app-access") => core.navigate(&mut doc, "app-access"),
                Some("transfer") => core.navigate(&mut doc, "transfer"),
                Some("import") => {
                    core.navigate(&mut doc, "transfer");
                    let rows = crate::demo::import_rows();
                    let skipped = core.import_skips(&rows);
                    core.dialog = Some(Dialog::ImportPreview { rows, skipped });
                }
                Some("export") => {
                    core.navigate(&mut doc, "transfer");
                    core.dialog = Some(Dialog::ExportPlaintext {
                        acknowledged: false,
                        reminded: false,
                    });
                }
                Some("backup") => {
                    core.navigate(&mut doc, "transfer");
                    core.dialog = Some(Dialog::Backup { error: None });
                }
                Some("delete") => {
                    core.begin_edit();
                    core.request_delete();
                }
                Some("long-list") => core.extend_demo_for_scrolling(),
                Some("empty") => {
                    core.items.clear();
                    core.selected = None;
                    core.app_grants.clear();
                }
                _ => {}
            }
            core.touch();
            core.show_pane = None;
        }
        self.pending = match scenario {
            Some(name @ ("detail" | "sidebar" | "editor" | "new-item")) => Some(name.to_owned()),
            _ => None,
        };
        self.settle(&mut doc);
    }

    fn stage_pending(&mut self) {
        let Some(scenario) = self.pending.take() else {
            return;
        };
        match scenario.as_str() {
            "sidebar" => self.shell.open_sidebar(),
            _ => self.shell.show(NavPane::Content),
        }
        self.shell.snap();
    }

    fn handle_shortcut(&self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed {
            return false;
        }
        let modifiers = ctx.modifiers.state();
        let command = modifiers.control_key() || modifiers.super_key();
        let screen = self.core.borrow().screen;
        let request = match &key.logical_key {
            Key::Named(NamedKey::ArrowDown) if screen == Screen::Vault && !command => {
                self.core.borrow_mut().step(ctx.state, true);
                return true;
            }
            Key::Named(NamedKey::ArrowUp) if screen == Screen::Vault && !command => {
                self.core.borrow_mut().step(ctx.state, false);
                return true;
            }
            Key::Named(NamedKey::Escape) if screen == Screen::Editor => Request::CancelEdit,
            Key::Character(c) if command && c.eq_ignore_ascii_case("n") => Request::NewItem,
            Key::Character(c)
                if command && c.eq_ignore_ascii_case("e") && screen == Screen::Vault =>
            {
                Request::Edit
            }
            Key::Character(c)
                if command && c.eq_ignore_ascii_case("s") && screen == Screen::Editor =>
            {
                Request::Save
            }
            _ => return false,
        };
        self.requests.borrow_mut().push(request);
        true
    }

    fn toast_changed(&self) -> bool {
        let core = self.core.borrow();
        core.toast.as_ref().map(|(message, _)| message) != self.painted_toast.as_ref()
    }

    fn paint_toast(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let core = self.core.borrow();
        if let Some((message, _)) = &core.toast {
            paint_transient(
                scene,
                transform,
                text,
                theme,
                core.window,
                ToastAnchor::BottomRight,
                &ToastCard::new(message, ToastKind::Info),
            );
        }
    }
}

impl ui::widget::Decorator for PasswordsSurface {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
        } else if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
        } else {
            Widget::provide(&self.shell, request);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
        } else if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
        } else {
            Widget::provide_mut(&mut self.shell, request);
        }
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        let answered =
            self.core
                .borrow_mut()
                .answer_operation(request.type_id, request.payload, doc)?;
        self.settle(doc);
        Ok(answered.then(|| {
            ui::widget::ResourceOperationStart::Completed(ui::tilo_actions::ActionOutput::default())
        }))
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            {
                let mut core = self.core.borrow_mut();
                core.open_pending();
                core.transfer_poll_deadline.set(None);
                core.poll_transfers();
            }
            self.settle(doc);
        }
        Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.settle(doc);
        }
        if self.settled_layouts >= SETTLED_LAYOUTS {
            self.stage_pending();
        }
        Widget::update(&mut self.shell, state, path)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        Widget::apply_layout(&mut self.shell, taffy, node, origin);
        let window = Widget::get_rect(&self.shell);
        self.settled_layouts = if window == self.last_window && window.area() > 0.0 {
            self.settled_layouts.saturating_add(1)
        } else {
            0
        };
        self.last_window = window;
        self.core.borrow_mut().window = window;
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(&mut self.shell, scene, transform, text, theme, presence);
        self.paint_toast(scene, transform, text, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::Focused(false) => {
                // A reveal is only for the actively attended window.
                self.core.borrow_mut().hide_revealed();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.core.borrow_mut().pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            _ => {}
        }
        Widget::handle_event(&mut self.shell, event, ctx);
        if !*ctx.handled && self.handle_shortcut(event, ctx) {
            *ctx.handled = true;
        }
        self.settle(ctx.state);
    }

    fn needs_animation_frame(&self) -> bool {
        self.pending.is_some() || Widget::needs_animation_frame(&self.shell)
    }

    fn needs_redraw(&self) -> bool {
        self.toast_changed() || Widget::needs_redraw(&self.shell)
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        if self.toast_changed() {
            regions.push(self.last_window);
        }
        Widget::collect_damage(&self.shell, regions);
    }

    fn mark_drawn(&mut self) {
        self.painted_toast = self
            .core
            .borrow()
            .toast
            .as_ref()
            .map(|(message, _)| message.clone());
        Widget::mark_drawn(&mut self.shell);
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let own = self.core.borrow().next_wake();
        match (own, Widget::next_wake_deadline(&self.shell)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn name(&self) -> &'static str {
        "Passwords"
    }
}

impl ui::search::capability::SearchSource for PasswordsSurface {
    /// The vault's items by name, username and website; never a secret.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search.clone())]
    }
}

impl ui::resources::PublishesResources for PasswordsSurface {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the vault door: keep only the live item and session extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        if self.is_hosted() {
            return self.core.borrow().live_item_extras();
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<ui::tilo_actions::Resource> {
        if self.is_hosted() && id == crate::actions::VAULT_RESOURCE_ID {
            return None;
        }
        find_resource(&self.publish_resources(doc), id)
    }
}

fn find_resource(
    resources: &[ui::tilo_actions::Resource],
    id: &str,
) -> Option<ui::tilo_actions::Resource> {
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
mod tests;
