//! The Passwords window: the shared [`NavShell`] with the vault's places in a
//! [`NavList`], the items of the chosen place in the overview, and the open
//! item, the editor or a tool page in the content. The shell owns the header
//! row, the sidebar toggle, the widths, the drawer and the push stack; this
//! module keeps them in step with [`Core`].

use std::cell::RefCell;
use std::rc::Rc;
use ui::num::u32_of_usize;

use accounts::{SecretId, SecretItemKind};

use crate::pages::ItemActions;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::ToastKind;
use ui::widget::{
    AppModel, Button, Element, ModelPath, NavItem, NavList, NavPane, NavSection, NavShell, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::actions::PasswordsEffect;
use crate::list_pane::ListPane;
use crate::vault::{Core, Dialog, Request, Requests, Screen, Shared, VaultFilter};

/// The `--shots --scenario` names this app stages.
pub const SCENARIOS: &[&str] = &[
    "detail",
    "api-key",
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

impl Core {
    fn nav_sections(&self) -> Vec<NavSection> {
        let vault = VaultFilter::ALL
            .into_iter()
            .map(|filter| {
                let item = NavItem::new(format!("filter:{}", filter.as_str()), filter.label())
                    .icon(filter_icon(filter));
                match self.count(filter) {
                    0 => item,
                    n => item.count(u32_of_usize(n)),
                }
            })
            .collect();
        let mut access = NavItem::new("app-access", "App access").icon(Icon::Monitor);
        let access_count = self
            .app_grants
            .len()
            .saturating_add(self.app_proposals.len());
        if access_count > 0 {
            access = access.count(u32_of_usize(access_count));
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

/// The sidebar: the shared [`NavList`], following the vault.
fn sidebar(core: &Shared, requests: &Requests) -> NavList {
    let requests = Rc::clone(requests);
    let core = Rc::clone(core);
    NavList::new(Vec::new())
        .with_id("passwords.sidebar")
        .with_accessible_name("Vault")
        .on_select(move |id| requests.borrow_mut().push(Request::Navigate(id)))
        .follow(move || {
            let core = core.try_borrow().ok()?;
            Some((core.nav_sections(), Some(core.place_id())))
        })
}

pub struct PasswordsSurface {
    core: Shared,
    requests: Requests,
    shell: NavShell,
    /// The page revision the content column was built from, and whether
    /// the item's actions were the window bar's then.
    built_page: Option<(u64, bool)>,
    /// The list revision the overview title and search index follow.
    built_list: Option<u64>,
    overview_title: String,
    /// The overview header is in the window's bar ([`ui::window_toolbar`]).
    header_in_bar: bool,
    /// The list title, the shell's own slots and the item's actions the
    /// bar's slots were last declared with.
    declared_bar: Option<(
        String,
        ui::window_toolbar::WindowToolbar,
        Option<ItemActions>,
    )>,
    shows_list: bool,
    /// The empty vault stands in for the list and the item
    /// ([`NavShell::nothing_listed`]).
    nothing_listed: bool,
    /// Shell state a capture stages once the window has its size.
    pending: Option<String>,
    settled_layouts: u32,
    last_window: vello::kurbo::Rect,
    acknowledger: ui::widget::acknowledgement::Acknowledger,
}

/// The action ids of the window bar's slots: New item, and copying,
/// editing and starring the item shown.
const NEW_ITEM: &str = "new-item";
const COPY_SECRET: &str = "copy-secret";
const EDIT_ITEM: &str = "edit-item";
const FAVORITE: &str = "favorite";

impl PasswordsSurface {
    /// The overview header's actions: search and New item.
    fn overview_actions(requests: &Requests) -> Vec<Element> {
        let new_item = {
            let requests = Rc::clone(requests);
            Button::icon_only(
                "passwords.add",
                Icon::Plus,
                &crate::CATALOG.t("passwords-new-item"),
            )
            .on_own_device_effect(move || requests.borrow_mut().push(Request::NewItem))
        };
        // window-bar: fallback, the bar's search while it hosts the window
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        vec![Box::new(search) as Element, Box::new(new_item)]
    }

    /// The overview header moves into the window's bar while the bar hosts
    /// the slots ([`ui::window_toolbar`]) and back when it does not: the
    /// list shown is the window's location, New item its action, then
    /// copying, editing and starring the item shown, and search the bar's
    /// own. What the person did there is queued like a click.
    fn sync_window_toolbar(&mut self) {
        use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};
        let hosted = ui::window_toolbar::is_hosted();
        if hosted != self.header_in_bar {
            self.header_in_bar = hosted;
            self.shell.set_sidebar_toggle_in_window_bar(hosted);
            if hosted {
                self.shell.set_overview_header("", Vec::new());
            } else {
                // window-bar: fallback, the bar holds New item while it hosts the window
                self.shell.set_overview_header(
                    self.overview_title.clone(),
                    Self::overview_actions(&self.requests),
                );
                ui::window_toolbar::declare(None);
                self.declared_bar = None;
            }
        }
        if !hosted {
            return;
        }
        // The shell's own slots: the sidebar's toggle, and Back while an
        // item in a narrow window has its list to go back to.
        let shell = self.shell.window_toolbar();
        let item = crate::pages::item_actions(&self.core.borrow());
        let key = (self.overview_title.clone(), shell.clone(), item);
        if self.declared_bar.as_ref() != Some(&key) {
            let mut toolbar = WindowToolbar {
                location: Some(ToolbarLocation {
                    label: self.overview_title.clone(),
                    ..ToolbarLocation::default()
                }),
                ..shell
            };
            let mut actions = vec![ToolbarAction::new(
                NEW_ITEM,
                "plus",
                crate::CATALOG.t("passwords-new-item"),
            )];
            if let Some(item) = &key.2 {
                actions.extend([
                    ToolbarAction::new(COPY_SECRET, "copy", item.copy.clone()),
                    ToolbarAction::new(EDIT_ITEM, "pencil", "Edit"),
                    ToolbarAction::new(
                        FAVORITE,
                        "star",
                        crate::pages::favorite_label(item.favorite),
                    )
                    .on(item.favorite),
                ]);
            }
            NavShell::push_window_actions(&mut toolbar, actions);
            ui::window_toolbar::declare(Some(toolbar));
            self.declared_bar = Some(key);
        }
        for event in ui::window_toolbar::take_events() {
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            let request = match event {
                ToolbarEvent::Action { ref id } => match id.as_str() {
                    NEW_ITEM => Request::NewItem,
                    COPY_SECRET => Request::CopySecret,
                    EDIT_ITEM => Request::Edit,
                    FAVORITE => match self.declared_bar.as_ref().and_then(|key| key.2.as_ref()) {
                        Some(item) => Request::ToggleFavorite(item.id.clone()),
                        None => continue,
                    },
                    _ => continue,
                },
                _ => continue,
            };
            self.requests.borrow_mut().push(request);
        }
    }

    fn with_core(core: Core) -> Self {
        let core: Shared = Rc::new(RefCell::new(core));
        let requests: Requests = Rc::default();
        let title = core.borrow().filter.label().to_owned();
        // Maximized, the header lives in the window's bar.
        let shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("passwords.shell")
            .with_header_in_window_bar()
            .sidebar(Box::new(sidebar(&core, &requests)))
            .overview(Box::new(ListPane::new(&core, &requests)))
            .overview_header(title.clone(), Self::overview_actions(&requests));
        let mut surface = Self {
            core,
            requests,
            shell,
            built_page: None,
            built_list: None,
            overview_title: title,
            header_in_bar: false,
            declared_bar: None,
            shows_list: true,
            nothing_listed: false,
            pending: None,
            settled_layouts: 0,
            last_window: vello::kurbo::Rect::ZERO,
            acknowledger: ui::widget::acknowledgement::Acknowledger::default(),
        };
        surface.settle();
        surface
    }

    /// The window over the vault in `model`: the person's in a live window.
    pub fn new(model: crate::PasswordsModel) -> Self {
        Self::with_core(Core::with_model(model))
    }

    /// Wake the window through `wake` once its vault is open.
    pub(crate) fn wake_through(&self, wake: ui::WakeHandle) {
        self.core.borrow().wake_through(wake);
    }

    /// The fixture vault, for tests and captures.
    pub fn demo() -> Self {
        Self::with_core(Core::demo())
    }

    /// What a search hit or an agent's `open` runs: show the vault item
    /// its anchor names, as clicking its row does; refused for an item the
    /// vault does not hold.
    pub(crate) fn opener(&self) -> impl FnMut(&str) -> Result<(), &'static str> + 'static {
        let core = Rc::clone(&self.core);
        let requests = Rc::clone(&self.requests);
        move |anchor| {
            let id = anchor
                .strip_prefix(crate::actions::ITEM_PREFIX)
                .map(|id| SecretId(id.to_owned()))
                .ok_or("not an item of the vault")?;
            if core.borrow().item(&id).is_none() {
                return Err("the vault holds no such item");
            }
            requests.borrow_mut().push(Request::Open(id));
            Ok(())
        }
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

    #[cfg(test)]
    pub(crate) fn acknowledgement(&self) -> Option<String> {
        self.acknowledger
            .current(ui::time::now())
            .map(|ack| ack.text().to_owned())
    }

    /// Apply what controls asked for, then bring the window in step: the
    /// content page, the overview, its title, the front pane, the notice and
    /// the search index.
    pub(crate) fn settle(&mut self) {
        self.core.borrow_mut().refresh_model();
        self.sync_window_toolbar();
        loop {
            let requests = std::mem::take(&mut *self.requests.borrow_mut());
            if requests.is_empty() {
                break;
            }
            for request in requests {
                self.core.borrow_mut().apply(request);
            }
        }
        self.core.borrow_mut().expire();
        let (page, list, screen, title, front, toast, saved) = {
            let mut core = self.core.borrow_mut();
            (
                core.page_revision,
                core.list_revision,
                core.screen,
                core.filter.label().to_owned(),
                core.show_pane.take(),
                core.toast.take(),
                core.saved.take(),
            )
        };
        if let Some(message) = toast {
            self.shell.toast(message, ToastKind::Info);
        }
        if let Some(item) = saved {
            self.acknowledger.say(crate::CATALOG.t_args(
                "passwords-saved-item",
                &[("item", ui::i18n::ArgValue::Text(item))],
            ));
        }
        let built = (page, self.header_in_bar);
        if self.built_page != Some(built) {
            self.built_page = Some(built);
            let content = {
                let core = self.core.borrow();
                crate::pages::content(&core, &self.core, &self.requests, self.header_in_bar)
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
        // An empty vault with no editor or tool open.
        let nothing = screen == Screen::Vault && {
            let core = self.core.borrow();
            core.items.is_empty() && !core.opening && core.dialog.is_none()
        };
        if nothing != self.nothing_listed {
            self.nothing_listed = nothing;
            self.shell.set_nothing_listed(
                nothing.then(|| {
                    Box::new(crate::list_pane::vault_empty_state(&self.requests)) as Element
                }),
            );
        }
        if self.built_list != Some(list) {
            self.built_list = Some(list);
        }
        if title != self.overview_title {
            if !self.header_in_bar {
                self.shell.set_overview_title(title.clone());
            }
            self.overview_title = title;
            // The bar names the list now shown.
            self.sync_window_toolbar();
        }
        if let Some(front) = front {
            self.shell
                .show(if front == NavPane::Overview && !self.shows_list {
                    NavPane::Content
                } else {
                    front
                });
        }
        // The bar offers what can be done to the item now shown.
        self.sync_window_toolbar();
    }

    /// Answer an effect invoked by the palette, an agent, a script or a
    /// search hit, and bring the window in step.
    pub(crate) fn perform_effect(&mut self, effect: PasswordsEffect) -> Result<(), String> {
        let answer = self.core.borrow_mut().perform(effect);
        self.settle();
        answer
    }

    /// Whether a draft is open in the editor.
    pub(crate) fn is_editing(&self) -> bool {
        self.core.borrow().is_editing()
    }

    /// Live vault rows, one per item.
    pub(crate) fn item_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.core.borrow().item_rows()
    }

    /// Non-secret metadata handed to the framework's one search index.
    pub(crate) fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::search::entries(&self.core.borrow().items)
    }

    /// Whether the vault behind the secret-bearing search entries is
    /// unlocked, as the system's search index needs to know.
    pub(crate) fn secrets_unlocked(&self) -> Option<bool> {
        self.core.borrow().secrets_unlocked()
    }

    /// The live row `id`, whether or not the list shows it now.
    pub(crate) fn item_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        self.core.borrow().item_row(id)
    }

    /// Stage a capture on the fixture vault: `None` is the default frame.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        {
            let mut core = self.core.borrow_mut();
            core.load_demo();
            match scenario {
                Some("editor") => core.begin_edit(),
                Some("api-key") => {
                    core.open(&SecretId("demo-console".into()));
                }
                Some("new-item") => core.begin_add(),
                Some("favorites") => core.select_filter(VaultFilter::Favorites),
                Some("review") => core.navigate("review"),
                Some("app-access") => core.navigate("app-access"),
                Some("transfer") => core.navigate("transfer"),
                Some("import") => {
                    core.navigate("transfer");
                    let rows = crate::demo::import_rows();
                    let skipped = core.import_skips(&rows);
                    core.dialog = Some(Dialog::ImportPreview { rows, skipped });
                }
                Some("export") => {
                    core.navigate("transfer");
                    core.dialog = Some(Dialog::ExportPlaintext {
                        acknowledged: false,
                        reminded: false,
                    });
                }
                Some("backup") => {
                    core.navigate("transfer");
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
                    core.app_proposals.clear();
                }
                _ => {}
            }
            core.touch();
            core.show_pane = None;
        }
        self.pending = match scenario {
            Some(name @ ("detail" | "api-key" | "sidebar" | "editor" | "new-item")) => {
                Some(name.to_owned())
            }
            _ => None,
        };
        self.settle();
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

    fn handle_shortcut(&self, event: &WindowEvent, ctx: &ui::widget::EventCtx<'_>) -> bool {
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
                self.core.borrow_mut().step(true);
                return true;
            }
            Key::Named(NamedKey::ArrowUp) if screen == Screen::Vault && !command => {
                self.core.borrow_mut().step(false);
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
}

impl ui::widget::Decorator for PasswordsSurface {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        {
            let mut core = self.core.borrow_mut();
            core.transfer_poll_deadline.set(None);
            core.poll_transfers();
        }
        self.settle();
        Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.settle();
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
        let now = ui::time::now();
        if let Some(ack) = self.acknowledger.current(now) {
            let alpha = ack.alpha_at(now, theme.reduce_motion);
            ack.draw(scene, transform, text, theme, self.last_window, alpha);
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut node = Widget::accessibility(&self.shell)?;
        node.children.extend(
            self.acknowledger
                .current(ui::time::now())
                .map(|ack| ack.accessibility()),
        );
        Some(node)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::Focused(false)) {
            // A reveal is only for the actively attended window.
            self.core.borrow_mut().hide_revealed();
        }
        Widget::handle_event(&mut self.shell, event, ctx);
        if !*ctx.handled && self.handle_shortcut(event, ctx) {
            *ctx.handled = true;
        }
        self.settle();
    }

    fn needs_animation_frame(&self) -> bool {
        self.pending.is_some()
            || self.acknowledger.current(ui::time::now()).is_some()
            || Widget::needs_animation_frame(&self.shell)
    }

    fn needs_redraw(&self) -> bool {
        self.core.borrow().model_changed() || Widget::needs_redraw(&self.shell)
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

#[cfg(test)]
mod tests;
