//! The Settings window, as a view of the [`Settings`] resource.
//!
//! ## Layout
//!
//! The whole window is one [`NavShell`] (from the shared toolkit): a
//! cohesive sidebar panel — the nav list — beside the selected category's
//! content pane. The shell owns the responsive behaviour end to end,
//! resolving into exactly one of two states by whether the sidebar fits
//! beside the content:
//!
//! * **wide** — the sidebar fits, so it is always docked and the content
//!   fills the area to its right. There is no collapse control;
//! * **narrow (phone)** — the sidebar does not fit, so the list and the content
//!   are two separate full-screen screens: the list screen and the content
//!   screen (the pane, full-screen, with a back arrow that returns to the
//!   list). Picking a category navigates the list → its content screen.
//!
//! ## The view and the window
//!
//! [`window`] is the view: it answers [`SettingsEffect`]s and publishes the
//! live rows from the [`SettingsApp`] it shares with the window, and builds
//! the content column as a part of the view keyed by what it shows
//! ([`PaneKey`]). A pane bound to its settings sections is view code over
//! them ([`crate::panes::view`]); the others are the widgets their areas
//! build. The window ([`SettingsWindow`]) holds the shell, follows the pane
//! asked for, the window bar's search and a setting asked to be shown, and
//! moves the key; the part is built again when it moves.

use std::rc::Rc;

use crate::categories::Category;
use crate::effects::SettingsEffect;
use crate::resource::Settings;
use crate::settings_app::{ScrollOffset, SettingsApp};
use crate::sidebar;
use ui::framework::{Publishes, ViewCx};
use ui::widget::NavMode;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, Element, ModelPath};
use ui::widget::{Container, NavPane, NavShell, Scrollable, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// What the content column shows: the pane of `category`, or the settings
/// the window bar's search found. `generation` moves whenever the column is
/// built again (a page drilled into, a read landed), so the part keyed by it
/// is.
#[derive(Clone)]
pub(crate) struct PaneKey {
    category: Category,
    found: Vec<&'static settings_store::index::Setting>,
    reads: Vec<crate::setting::Read>,
    generation: u64,
}

impl PartialEq for PaneKey {
    fn eq(&self, other: &Self) -> bool {
        self.category == other.category && self.generation == other.generation
    }
}

impl PaneKey {
    fn of(category: Category) -> Self {
        Self {
            category,
            found: Vec::new(),
            reads: Vec::new(),
            generation: 0,
        }
    }
}

/// The Settings window over `app`, as the view of [`Settings`]: it answers
/// every [`SettingsEffect`] and publishes the live rows of what `app`'s
/// areas read, and shows the panes, those bound to their sections built as
/// view code.
pub(crate) fn window(cx: &ViewCx<Settings>, mut app: SettingsApp) -> Element {
    let accounts = cx.shared_person::<accounts::person::PersonAccounts>();
    app.attach_host(crate::host::PaneCx::attached(cx), accounts);
    let app = app.into_shared();
    // A pane search hit, or an agent's `open`, shows the pane it names.
    cx.on_open(app.opener());
    {
        let (answer, status) = (Rc::clone(&app), Rc::clone(&app));
        cx.effects(move |effect: SettingsEffect, cx| crate::effects::answer(&answer, effect, cx))
            .status(move || crate::effects::statuses(&status));
    }
    {
        // A capture stages its scenario on the window's next sync.
        let staged = Rc::clone(&app);
        cx.on_stage(move |scenario| staged.stage(crate::scenarios::staged(scenario)));
    }
    let key = ui::LocalState::new(PaneKey::of(app.selected()));
    let content = {
        let app = Rc::clone(&app);
        cx.switch(&key, move |cx, key| content(&app, key, Some(&*cx)))
    };
    let mut window = SettingsWindow::over(Rc::clone(&app), Content::Bound(key, Some(content)));
    window.look = Some(Look {
        prefs: cx.settings::<crate::prefs::AppearancePrefs>(),
        device: cx.settings::<crate::prefs::AppearanceDevicePrefs>(),
    });
    let (rows, row, pending, intents) = (Rc::clone(&app), Rc::clone(&app), Rc::clone(&app), app);
    cx.publishing(
        Publishes::new()
            // One row per pane, then the devices, networks, accounts and
            // other things outside any document each area read.
            .live_rows(move || crate::effects::rows(&rows))
            .live_row(move |id| crate::effects::row(&row, id))
            .live_row_pending(move |id| crate::effects::row_pending(&pending, id))
            .host_intents(move || {
                intents
                    .areas()
                    .iter()
                    .find_map(crate::area::PaneArea::take_host_intent)
            }),
        Box::new(window),
    )
}

/// What the content column shows for `key`, scrolled: the settings found,
/// the request for a workspace identity, a note while the pane's area reads,
/// or the pane. In the view (`cx`), a pane that is view code over its
/// sections is built as such, and so are the rows found of such a pane;
/// outside it, only the panes the areas build show.
fn content(app: &Rc<SettingsApp>, key: &PaneKey, cx: Option<&ViewCx<Settings>>) -> Element {
    app.content_built().set(key.generation);
    let (body, offset): (Element, ScrollOffset) = if key.found.is_empty() {
        (
            pane_body(app, key.category, cx),
            app.scroll_offset(key.category),
        )
    } else {
        let bound = cx.map_or_else(Vec::new, |cx| {
            crate::setting::bound_rows(cx, &key.found, &key.reads)
        });
        (
            Box::new(crate::setting::Found::new(
                app.host().clone(),
                key.found.clone(),
                key.reads.clone(),
                bound,
            )),
            ScrollOffset::default(),
        )
    };
    let content: Element = Box::new(
        Container::new()
            .readable()
            .with_inset(ui::tokens::Rhythm::Group)
            .with_child(body),
    );
    let scroll = Scrollable::new_vertical()
        .with_persisted_offset(offset)
        .with_child(content);
    Box::new(Container::new().filling().with_child(Box::new(scroll)))
}

/// The body of `category`'s pane: see [`content`].
fn pane_body(app: &Rc<SettingsApp>, category: Category, cx: Option<&ViewCx<Settings>>) -> Element {
    if category.needs_workspace_identity() && ui::workspace_identity::missing() {
        app.pane_waits().set(false);
        app.keyboard_partial().set(false);
        return Box::new(
            ui::workspace_identity::required_state(
                <crate::resource::SettingsShell as ui::framework::ResourceApp>::TITLE,
            )
            .inline(),
        );
    }
    let reading = app.pane_reading();
    app.pane_waits().set(reading);
    app.keyboard_partial().set(
        category == Category::Keyboard && app.areas().iter().any(crate::area::PaneArea::partial),
    );
    if reading {
        return crate::widgets::page(vec![crate::widgets::untitled(crate::widgets::rows(vec![
            crate::widgets::note(&crate::i18n::t("settings-pane-reading")),
        ]))]);
    }
    if let Some(pane) = cx.and_then(|cx| crate::panes::view(cx, app, category)) {
        return pane;
    }
    crate::area::registry()
        .panes
        .iter()
        .find(|pane| pane.categories.contains(&category))
        .map_or_else(
            || crate::widgets::page(Vec::new()),
            |pane| (pane.build)(app, category),
        )
}

/// The person's appearance, followed while the view lives: the look the
/// window shows itself in while the page that changes it is open.
struct Look {
    prefs: ui::framework::Setting<crate::prefs::AppearancePrefs>,
    device: ui::framework::Setting<crate::prefs::AppearanceDevicePrefs>,
}

/// How the window's content column is built.
enum Content {
    /// By the view's part keyed by what it shows: the window moves the key,
    /// and hands the part to its shell once.
    Bound(ui::LocalState<PaneKey>, Option<Element>),
    /// By the window itself, outside any view: the panes bound to their
    /// sections show nothing. A window a test lays out on its own.
    Detached(PaneKey),
}

/// The Settings window: the shell, its sidebar and the content column, over
/// the [`SettingsApp`] it shares with its view. See the module docs.
pub struct SettingsWindow {
    app: Rc<SettingsApp>,
    content: Content,
    structure_dirty: bool,
    /// The screen a narrow window opens on.
    narrow_screen: NavPane,
    /// The search a window without its shell yet is staged with.
    staged_filter: String,
    /// The setting asked to be shown, until its pane is built.
    reveal: Option<&'static settings_store::index::Setting>,
    /// The setting whose control took the keyboard, to scroll to once laid out.
    reveal_scroll: Option<&'static settings_store::index::Setting>,
    /// The setting shown, kept in sight until the person acts.
    revealed: Option<&'static settings_store::index::Setting>,
    /// The control a capture puts the keyboard on, once its pane is built.
    capture_focus: Option<&'static str>,
    /// The row of the setting just shown, marked for a moment.
    highlight: Option<(&'static settings_store::index::Setting, ui::time::Instant)>,
    rect: vello::kurbo::Rect,
    shell: Option<NavShell>,
    /// The text scale the content column was composed for.
    shell_text_scale: f32,
    /// The pane the window bar's search opened on, while it is open.
    queried: Option<Category>,
    found_query: String,
    found: Vec<&'static settings_store::index::Setting>,
    found_reads: Vec<crate::setting::Read>,
    /// The screen a narrow window showed before it showed the settings found.
    found_over: Option<NavPane>,
    value_follows: crate::setting::Follows,
    pages_seen: u64,
    /// The language the content column was built in
    /// ([`ui::i18n::I18nManager::revision`]).
    language_seen: u64,
    secret: Option<crate::secrets::ShownPrompt>,
    /// The person's appearance, as the view follows it; `None` outside a
    /// view, where it is read from the settings when asked.
    look: Option<Look>,
}

impl std::ops::Deref for SettingsWindow {
    type Target = SettingsApp;

    fn deref(&self) -> &SettingsApp {
        &self.app
    }
}

impl Default for SettingsWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsWindow {
    /// A window over a fresh [`SettingsApp`], outside any view.
    pub fn new() -> Self {
        Self::detached(SettingsApp::new())
    }

    /// A window over `app`, outside any view: it builds its content column
    /// itself.
    #[must_use]
    pub fn detached(app: SettingsApp) -> Self {
        let app = app.into_shared();
        let key = PaneKey::of(app.selected());
        Self::over(app, Content::Detached(key))
    }

    fn over(app: Rc<SettingsApp>, content: Content) -> Self {
        Self {
            app,
            content,
            structure_dirty: false,
            narrow_screen: NavPane::Content,
            staged_filter: String::new(),
            reveal: None,
            reveal_scroll: None,
            revealed: None,
            capture_focus: None,
            highlight: None,
            rect: vello::kurbo::Rect::ZERO,
            shell: None,
            shell_text_scale: 1.0,
            queried: None,
            found_query: String::new(),
            found: Vec::new(),
            found_reads: Vec::new(),
            found_over: None,
            value_follows: crate::setting::Follows::default(),
            pages_seen: 0,
            language_seen: ui::i18n::I18nManager::revision(),
            secret: None,
            look: None,
        }
    }

    /// A detached window on `category`.
    pub fn with_selected(category: Category) -> Self {
        Self::detached(SettingsApp::new().with_category(category))
    }

    /// This window on `category`.
    #[must_use]
    pub fn with_category(mut self, category: Category) -> Self {
        self.select(category);
        self
    }

    /// This window opening on the list screen when narrow.
    #[must_use]
    pub fn with_category_list(mut self) -> Self {
        self.narrow_screen = NavPane::Sidebar;
        self
    }

    /// What the window shares with its view.
    pub fn app(&self) -> &SettingsApp {
        &self.app
    }

    #[cfg(test)]
    pub fn pending_reveal(&self) -> Option<&'static str> {
        self.reveal.or(self.reveal_scroll).map(|setting| setting.id)
    }

    /// Show `category`'s pane.
    pub fn select(&mut self, category: Category) {
        if self.app.selected() != category {
            self.revealed = None;
            self.app.show(category);
            self.pages_seen = self.app.host().pages().revision();
            self.structure_dirty = true;
            self.refresh_shell_children();
        }
        if let Some(shell) = &mut self.shell {
            shell.show(ui::widget::NavPane::Content);
        }
    }

    pub fn is_narrow(&self) -> bool {
        self.shell
            .as_ref()
            .is_some_and(|shell| !shell.geometry().stack.is_empty())
    }

    pub fn sidebar_open(&self) -> bool {
        self.shell.as_ref().is_some_and(NavShell::is_sidebar_open)
    }

    pub fn search_query(&self) -> &str {
        match &self.shell {
            Some(shell) => shell
                .sidebar_ref::<ui::widget::NavList>()
                .map_or("", ui::widget::NavList::filter),
            None => &self.staged_filter,
        }
    }

    pub fn back_button_rect(&self) -> vello::kurbo::Rect {
        self.shell
            .as_ref()
            .map_or(vello::kurbo::Rect::ZERO, |shell| shell.geometry().back)
    }

    pub(crate) fn narrow_sidebar(&mut self, query: &str) {
        match self
            .shell
            .as_mut()
            .and_then(NavShell::sidebar_mut::<ui::widget::NavList>)
        {
            Some(nav) => nav.set_filter(query),
            None => query.clone_into(&mut self.staged_filter),
        }
        self.follow_query(query);
    }

    /// Follow what the window bar's search asks for now: the settings it
    /// finds are shown in place of the pane while it finds any.
    fn follow_query(&mut self, query: &str) {
        if query == self.found_query {
            return;
        }
        query.clone_into(&mut self.found_query);
        let found = crate::setting::matching(query);
        if found.is_empty() && self.found.is_empty() {
            return;
        }
        let was_shown = !self.found.is_empty();
        self.found = found;
        self.read_found();
        self.structure_dirty = true;
        self.refresh_shell_children();
        let narrow = self.is_narrow();
        if let Some(shell) = self.shell.as_mut().filter(|_| narrow) {
            if !self.found.is_empty() && !was_shown {
                self.found_over = Some(shell.front_pane());
                shell.show(NavPane::Content);
            } else if self.found.is_empty()
                && let Some(over) = self.found_over.take()
            {
                shell.show(over);
            }
        }
    }

    /// Read the values of the settings found again; whether one changed.
    fn read_found(&mut self) -> bool {
        self.value_follows.refresh();
        let values = crate::setting::reads(&self.app, &self.value_follows, &self.found);
        if values == self.found_reads {
            return false;
        }
        self.found_reads = values;
        true
    }

    #[must_use]
    pub fn found_settings(&self) -> Vec<&'static str> {
        self.found.iter().map(|setting| setting.id).collect()
    }

    /// Stage `staged`: a pane, the list screen, a search or a focused control.
    pub fn stage_scene(&mut self, staged: crate::scenarios::Staged) {
        use crate::scenarios::Staged;
        match staged {
            Staged::Pane(category) => self.select(category),
            Staged::List(category) => {
                self.select(category);
                self.narrow_screen = NavPane::Sidebar;
                if let Some(shell) = &mut self.shell {
                    shell.back();
                }
            }
            Staged::Search(category, query) => {
                self.select(category);
                self.narrow_sidebar(query);
            }
            Staged::Focused(category, control) => {
                self.select(category);
                self.capture_focus = Some(control);
            }
        }
    }

    /// The content column for the shell: the view's part, handed over once,
    /// or the column built here.
    fn take_content(&mut self) -> Option<Element> {
        match &mut self.content {
            Content::Bound(_, content) => content.take(),
            Content::Detached(key) => Some(content(&self.app, key, None)),
        }
    }

    fn build_shell(&mut self) -> NavShell {
        let nav = sidebar::build(self.app.host(), self.app.selected(), &self.staged_filter);
        let can_go_back = self.app.host().pages().clone();
        let go_back = self.app.host().pages().clone();
        let mut shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("settings.shell")
            .mode(NavMode::Push)
            .sidebar(nav)
            .sidebar_header("", Vec::new())
            .content_header(self.content_title(), Vec::new())
            .on_back(
                move || can_go_back.can_go_back(),
                move || {
                    go_back.back();
                },
            );
        if let Some(content) = self.take_content() {
            shell.set_content(content);
        }
        shell.show(self.narrow_screen);
        shell
    }

    fn content_title(&self) -> String {
        if self.found.is_empty() {
            let selected = self.app.selected();
            self.app
                .host()
                .pages()
                .top(selected)
                .map_or_else(|| selected.label(), |page| page.title)
        } else {
            crate::i18n::t("settings-found-title")
        }
    }

    /// Build the content column again for what the window shows now, and
    /// title it.
    fn refresh_shell_children(&mut self) {
        let next = |key: &PaneKey, window: &Self| PaneKey {
            category: window.app.selected(),
            found: window.found.clone(),
            reads: window.found_reads.clone(),
            generation: key.generation + 1,
        };
        match &self.content {
            Content::Bound(key, _) => {
                let moved = key.with(|key| next(key, self));
                key.set(moved);
            }
            Content::Detached(key) => {
                let moved = next(key, self);
                self.content = Content::Detached(moved);
                if self.shell.is_some()
                    && let Some(content) = self.take_content()
                    && let Some(shell) = &mut self.shell
                {
                    shell.set_content(content);
                }
            }
        }
        let title = self.content_title();
        let selected = self.app.selected();
        if let Some(shell) = &mut self.shell {
            if let Some(nav) = shell.sidebar_mut::<ui::widget::NavList>() {
                if nav.selected_id().as_deref() != Some(selected.slug()) {
                    nav.set_selected(Some(selected.slug()));
                }
            } else {
                shell.set_sidebar(Some(sidebar::build(self.app.host(), selected, "")));
            }
            shell.set_content_title(title);
        }
    }
}

impl SettingsWindow {
    /// What the keyboard drawn on the Keyboard pane shows now.
    ///
    /// # Errors
    /// The drawn keyboard could not be read.
    pub fn keyboard_inspection(&self) -> Result<serde_json::Value, String> {
        self.keyboard_now()
            .unwrap_or_else(|| self.app.keyboard_inspection())
    }

    /// The keyboard drawn on the Keyboard pane, read now; `None` while the
    /// pane draws none.
    fn keyboard_now(&self) -> Option<Result<serde_json::Value, String>> {
        fn mounted_map(widget: &dyn Widget) -> Option<&ui::KeyboardMap> {
            if let Some(map) = widget
                .as_any()
                .and_then(|value| value.downcast_ref::<ui::KeyboardMap>())
            {
                return Some(map);
            }
            widget.children().into_iter().find_map(mounted_map)
        }
        if self.app.selected() != Category::Keyboard {
            return None;
        }
        mounted_map(self).map(ui::KeyboardMap::inspection_snapshot)
    }

    /// Hand the effects what the drawn keyboard and the add-source picker
    /// show now: whatever moved them (a key held, a layout, a key inspected,
    /// a filter typed) went through the window.
    /// Whether the content column shows what the window last asked of it:
    /// the view's part is built again for a moved key on its next sync.
    fn content_shown(&self) -> bool {
        match &self.content {
            Content::Bound(key, _) => {
                key.with(|key| key.generation) == self.app.content_built().get()
            }
            Content::Detached(_) => true,
        }
    }

    fn share_keyboard(&self) {
        self.app
            .keyboard_drawn(self.keyboard_now().and_then(Result::ok));
        let picker = (self.app.selected() == Category::Keyboard)
            .then(|| crate::panes::mouse_keyboard::input_picker_snapshot(self))
            .flatten();
        self.app.input_picker_drawn(picker);
    }
}

// Account setup, nearby pairing and identity administration share the app's
// collaboration connection through their host-action clients.
impl ui::widget::Collaborative for SettingsWindow {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.app
            .areas()
            .iter()
            .flat_map(crate::area::PaneArea::take_collab_requests)
            .collect()
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        for area in self.app.areas().iter() {
            area.handle_collab_event(event);
        }
    }
}

/// The window bar's search queries the shown pane where it offers a query
/// ([`crate::area::PaneArea::query`]), and otherwise narrows the sidebar's
/// list and lists the settings it finds: the window is its `Filterable`,
/// so the search reaches the list while what it found is in front of it in
/// a narrow window.
impl SettingsWindow {
    /// What the window bar's search queries now: the pane it opened on while
    /// it stays open, else the shown pane, when that pane offers a query.
    fn pane_query(&self) -> Option<&dyn crate::area::PaneQuery> {
        let category = self.queried.unwrap_or_else(|| self.app.selected());
        self.app
            .areas()
            .iter()
            .find_map(|area| area.query(category))
    }
}

impl ui::widget::Queryable for SettingsWindow {
    fn query_opened(&mut self) {
        self.queried.get_or_insert_with(|| self.app.selected());
    }

    fn set_query_text(&mut self, text: &str) {
        self.queried.get_or_insert_with(|| self.app.selected());
        if let Some(query) = self.pane_query() {
            query.set_query_text(text);
        }
    }

    fn query_closed(&mut self) {
        if let Some(query) = self.pane_query() {
            query.query_closed();
        }
        self.queried = None;
    }
}

impl ui::widget::Filterable for SettingsWindow {
    fn set_filter(&mut self, query: &str) {
        self.narrow_sidebar(query);
    }

    fn filter_count(&self) -> Option<ui::widget::FilterCount> {
        self.shell
            .as_ref()?
            .sidebar_ref::<ui::widget::NavList>()
            .and_then(ui::widget::Filterable::filter_count)
    }
}

impl Widget for SettingsWindow {
    // Tests and hosts reach the window by its type.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::widget::Queryable>() {
            if self.pane_query().is_some() {
                request.provide::<dyn ui::widget::Queryable>(self);
            }
        } else if request.wants::<dyn ui::widget::Filterable>() {
            request.provide::<dyn ui::widget::Filterable>(self);
        } else if request.wants::<dyn ui::widget::Collaborative>() {
            request.provide::<dyn ui::widget::Collaborative>(self);
        } else if request.wants::<dyn ui::application_view::ApplicationViewState>() {
            request.provide::<dyn ui::application_view::ApplicationViewState>(self);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::widget::Queryable>() {
            if self.pane_query().is_some() {
                request.provide::<dyn ui::widget::Queryable>(self);
            }
        } else if request.wants::<dyn ui::widget::Filterable>() {
            request.provide::<dyn ui::widget::Filterable>(self);
        } else if request.wants::<dyn ui::widget::Collaborative>() {
            request.provide::<dyn ui::widget::Collaborative>(self);
        } else if request.wants::<dyn ui::application_view::ApplicationViewState>() {
            request.provide::<dyn ui::application_view::ApplicationViewState>(self);
        }
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        use automerge::transaction::Transactable;
        fn rect(value: vello::kurbo::Rect) -> [f64; 4] {
            [value.x0, value.y0, value.x1, value.y1]
        }
        fn inspect(
            widget: &dyn Widget,
            categories: &mut Vec<&'static str>,
            nav_id: &mut Option<&'static str>,
        ) {
            if widget.test_id() == Some(sidebar::NAV_ID) {
                *nav_id = Some(sidebar::NAV_ID);
            }
            if let Some(list) = widget
                .as_any()
                .and_then(|value| value.downcast_ref::<ui::widget::NavList>())
            {
                for id in list.visible_item_ids() {
                    if let Some(category) = Category::from_slug(&id) {
                        categories.push(category.slug());
                    }
                }
            }
            widget.for_each_child(&mut |child| inspect(child, categories, nav_id));
        }
        let (state, path) = ui::doc::document_ref(state, path);
        let mut categories = Vec::new();
        let mut nav_id = None;
        inspect(self, &mut categories, &mut nav_id);
        // get_heads closes pending transactions. Only a disposable clone may
        // perform that operation: the original document stays untouched.
        let mut witness = state.clone();
        let mut heads = witness
            .get_heads()
            .into_iter()
            .map(|head| head.to_string())
            .collect::<Vec<_>>();
        heads.sort();
        let valid = |value: vello::kurbo::Rect| {
            value.is_finite() && value.width() > 0.0 && value.height() > 0.0
        };
        // The search is the window bar's, outside the app: what it narrowed
        // the sidebar to is what the app shows of it.
        Some(serde_json::json!({
            "app": "so.tilo.settings",
            "nav_id": nav_id,
            "ready": valid(self.rect) && nav_id.is_some(),
            "search_query": self.search_query(),
            "root_rect": rect(self.rect),
            "selected_category": self.app.selected().slug(),
            "rendered_result_count": categories.len(),
            "rendered_category_ids": categories,
            "narrow": self.is_narrow(),
            "sidebar_open": self.sidebar_open(),
            "document_root": format!("{path:?}"),
            "document_actor": state.get_actor().to_hex_string(),
            "document_witness_kind": "effective_heads_on_disposable_clone",
            "effective_document_heads": heads,
            "original_pending_ops": state.pending_ops(),
        }))
    }
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // A capture's scenario, staged through the view.
        if let Some(staged) = self.app.take_staged() {
            self.stage_scene(staged);
        }
        // A change the session refused to store (or could not) went back
        // to what it was: the window says so, plainly, rather than leaving
        // the person to find the old value on their next visit.
        if settings_store::deferred_failure_pending() {
            let failures = settings_store::take_deferred_failures();
            for failure in &failures {
                tracing::warn!(
                    sections = ?failure.sections,
                    error = %failure.error,
                    "a settings change was not saved"
                );
            }
            if !failures.is_empty()
                && let Some(shell) = self.shell.as_mut()
            {
                shell.toast(
                    crate::i18n::t("settings-change-not-saved"),
                    ui::widget::toast::ToastKind::Error,
                );
            }
        }
        // Background work an effect started finishes whether or not its pane
        // is open.
        for area in self.app.areas().iter() {
            area.poll();
        }
        // After the first frame, the areas read one at a time until every
        // area's rows are published.
        if self.app.area_reads().take_due() {
            self.app.read_on();
        }
        // A sidebar row opens its pane through the pane row's open effect;
        // only a bare app outside a host selects here. Search is local
        // presentation state: no CRDT text, collaboration traffic or undo.
        if let Some(category) = self.app.take_requested_pane() {
            // Opening a page, from the sidebar or a setting found, shows the
            // page; what search found is left behind.
            let found = !self.found.is_empty();
            self.found.clear();
            self.found_over = None;
            if found && self.app.selected() == category {
                self.structure_dirty = true;
                self.refresh_shell_children();
            }
            self.select(category);
            if let Some(setting) = self.app.take_requested_setting() {
                self.reveal = Some(setting);
                // The pane is built anew even when it is already open, so
                // its control takes the keyboard in the rebuild.
                self.structure_dirty = true;
                self.refresh_shell_children();
            }
        }
        // The window bar's search is typed into the sidebar's list, outside
        // the app: what it asks for now is read from there.
        let query = self.search_query().to_owned();
        self.follow_query(&query);
        if !self.found.is_empty() && self.read_found() {
            self.structure_dirty = true;
            self.refresh_shell_children();
        }
        // The read the pane shown waits for has landed, or the layout list
        // the Keyboard page showed without: build the pane.
        if (self.app.pane_waits().get() && !self.app.pane_reading())
            || (self.app.keyboard_partial().get()
                && !self.app.areas().iter().any(crate::area::PaneArea::partial))
        {
            self.app.keyboard_partial().set(false);
            self.structure_dirty = true;
            self.refresh_shell_children();
        }

        // A pane drilled into a page or came back out of one: the page is
        // built from its top, titled in the header.
        let pages = self.app.host().pages().revision();
        if pages != self.pages_seen {
            self.pages_seen = pages;
            self.app.forget_scroll(self.app.selected());
            self.structure_dirty = true;
            self.refresh_shell_children();
        }

        // Another language: the content column is built again in it, as a
        // page that is view code over its sections wrote its words as it
        // was built.
        let language = ui::i18n::I18nManager::revision();
        if language != self.language_seen {
            self.language_seen = language;
            self.structure_dirty = true;
            self.refresh_shell_children();
        }

        // A pane asked for a secret, or is done with the one it asked.
        if crate::secrets::ShownPrompt::follow(&mut self.secret, self.app.host().secrets()) {
            self.structure_dirty = true;
        }

        if let Some(shell) = &mut self.shell {
            shell.sync(state, path);
        }
        if let Some(shown) = &mut self.secret {
            shown.dialog.sync(state, path);
        }
        self.share_keyboard();
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Create the shell once and reuse it so its collapse/overlay state
        // survives category switches. The panes compose for the text scale:
        // a change of it builds the content column anew.
        crate::widgets::set_compose_text_growth(theme.text_growth());
        if self.shell.is_none() {
            self.shell_text_scale = theme.text_scale;
            self.shell = Some(self.build_shell());
        } else if self.shell_text_scale.to_bits() != theme.text_scale.to_bits() {
            self.shell_text_scale = theme.text_scale;
            self.refresh_shell_children();
        }
        let settled = !self.app.pane_settling() && self.content_shown();
        #[allow(
            clippy::expect_used,
            reason = "shell was just set unconditionally above; nothing between there and here can clear it"
        )]
        let shell = self.shell.as_mut().expect("just set above");
        // Composed panes recreate their controls on layout, including the
        // follow-up layout that scrolling a revealed control can request.
        // Preserve the current control's identity, not the old focus ordinal
        // or the last reveal request: either could steal focus after a move.
        let focused = focused_control_id(shell).map(str::to_owned);
        let shell_node = shell.build_layout(taffy, theme, node_map);
        // The pane asked to show a setting is built: its control takes the
        // keyboard, which the rebuild keeps on it. A pane still being read
        // (or showing its first reading as on its way) shows its control
        // once it is built; a control the pane does not
        // show (no such device) leaves the page open at its top.
        if let Some(setting) = self.reveal.filter(|_| settled) {
            self.reveal = None;
            clear_control_focus(shell);
            if focus_control(shell, &|id| setting.is_control(id)) {
                self.reveal_scroll = Some(setting);
            }
        } else if let Some(id) = self.capture_focus.filter(|_| settled) {
            if focus_control(shell, &|control| control == id) {
                self.capture_focus = None;
            }
        } else if let Some(id) = focused {
            focus_control(shell, &|control| control == id);
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        // `new_with_children` only fails for a child `NodeId` from a
        // different tree, which never happens here — see
        // `ui::widget::TaffyTreeExt`. Fall back to the child directly rather
        // than panic, on the off chance that ever changes.
        taffy.widget_node(style, &[shell_node], node_map, self)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        // `taffy.layout(...)` only errors for a `NodeId` from a different
        // tree, which never happens here — see `ui::widget::TaffyTreeExt`.
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let origin = vello::kurbo::Point::new(self.rect.x0, self.rect.y0);
        if let (Ok(children), Some(shell)) = (taffy.children(node), &mut self.shell)
            && let Some(&shell_node) = children.first()
        {
            shell.apply_layout(taffy, shell_node, origin);
            if let Some(setting) = self.reveal_scroll.take() {
                self.scroll_to_control(setting);
                self.highlight = Some((setting, ui::time::now()));
                self.revealed = Some(setting);
            } else if let Some(setting) = self.revealed {
                // Scrolls only once the control has left the view.
                self.scroll_to_control(setting);
            }
        }
        self.share_keyboard();
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(shell) = &mut self.shell {
            shell.draw(scene, transform, text_renderer, theme, presence);
        }
        // The row of a setting just shown is marked as a found match is,
        // for a moment.
        match self.highlighted_row() {
            Some(row) => ui::widget::paint_find_match(scene, transform, row, true, theme),
            None => self.highlight = None,
        }
        self.app.area_reads().presented();
        self.app.area_reads().wake();
    }

    fn observe_physical_input(&mut self, input: ui::widget::PhysicalInput) {
        fn observe_map(widget: &mut dyn Widget, input: ui::widget::PhysicalInput) -> bool {
            if let Some(map) = widget
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<ui::KeyboardMap>())
            {
                map.observe_physical_input(input);
                return true;
            }
            let mut visible = false;
            for child in widget.children_mut() {
                visible |= observe_map(child, input.clone());
            }
            visible
        }
        let visible = self.app.selected() == Category::Keyboard
            && self
                .shell
                .as_mut()
                .is_some_and(|shell| observe_map(shell, input.clone()));
        if !visible {
            self.app.keyboard_map_input().observe_configuration(&input);
            self.app.keyboard_map_input().clear();
        }
        self.share_keyboard();
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // Observe only the mounted foreground page, before its search/picker
        // can consume editing events. This never changes handling or focus.
        fn observe_map(widget: &mut dyn Widget, event: &winit::event::WindowEvent) {
            if let Some(map) = widget
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<ui::KeyboardMap>())
            {
                map.observe_foreground_event(event);
                return;
            }
            for child in widget.children_mut() {
                observe_map(child, event);
            }
        }
        // The person acts: what they do decides what is in sight now.
        if matches!(
            event,
            winit::event::WindowEvent::KeyboardInput { .. }
                | winit::event::WindowEvent::MouseInput { .. }
                | winit::event::WindowEvent::MouseWheel { .. }
                | winit::event::WindowEvent::Touch(_)
        ) {
            self.revealed = None;
        }
        // The password prompt takes every event while it is open.
        if let Some(shown) = &mut self.secret {
            shown.dialog.handle_detached(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
            );
            *ctx.handled = true;
            if crate::secrets::ShownPrompt::answered(&mut self.secret, self.app.host().secrets()) {
                self.structure_dirty = true;
            }
            return;
        }
        if self.app.selected() == Category::Keyboard
            && let Some(shell) = &mut self.shell
        {
            observe_map(shell, event);
        }
        if let (Ok(children), Some(shell)) = (ctx.taffy.children(ctx.node_id), &mut self.shell)
            && let Some(&shell_node) = children.first()
        {
            shell.handle_event(event, &mut ctx.reborrow(ctx.path, shell_node));
        }
        self.share_keyboard();
    }

    /// The window's shell, or while it is open the password prompt alone:
    /// nothing under it is reachable.
    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(shown) = &self.secret {
            return vec![&shown.dialog as &dyn Widget];
        }
        self.shell
            .as_ref()
            .map(|s| vec![s as &dyn Widget])
            .unwrap_or_default()
    }

    /// The shell, laid out under the prompt too: the prompt is drawn over
    /// it, outside the layout.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if let Some(shell) = &self.shell {
            visitor(shell);
        }
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(shown) = &mut self.secret {
            return vec![&mut shown.dialog as &mut (dyn Widget + '_)];
        }
        self.shell
            .as_mut()
            .map(|s| vec![s as &mut (dyn Widget + '_)])
            .unwrap_or_default()
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(shell) = &mut self.shell {
            shell.draw_overlay(scene, transform, text, theme, presence);
        }
        if let Some(shown) = &mut self.secret {
            shown
                .dialog
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        self.secret
            .as_ref()
            .and_then(|shown| shown.dialog.modal_capture())
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        // This root only delegates painting. Keep the shell's visual bounds
        // instead of expanding every control hover to the whole app window.
        if let Some(shell) = &self.shell {
            shell.collect_damage(regions);
            // The fading wash over a shown setting's row.
            if let Some((setting, _)) = self.highlight
                && let Some(row) = row_of(shell, setting)
            {
                regions.push(row.inflate(2.0, 2.0));
            }
        }
    }

    fn name(&self) -> &'static str {
        "SettingsWindow"
    }

    /// While the page that changes the look is open, the window shows
    /// itself in it the instant a control changes it: the host applies
    /// what differs. The toolkit folds in the contrast, the interface
    /// scale and the text scale.
    fn appearance_intent(&self) -> Option<ui::theme::AppearanceIntent> {
        if self.app.selected() != Category::Appearance {
            return None;
        }
        Some(match &self.look {
            Some(look) => look.prefs.state().with(|prefs| {
                look.device
                    .state()
                    .with(|device| crate::panes::appearance::intent(prefs, device))
            }),
            None => crate::panes::appearance::intent(
                &self
                    .app
                    .host()
                    .settings::<crate::prefs::AppearancePrefs>()
                    .get(),
                &self
                    .app
                    .host()
                    .settings::<crate::prefs::AppearanceDevicePrefs>()
                    .get(),
            ),
        })
    }

    /// Stable landmark for the responsive shell: the sidebar and pane subtree
    /// may rebuild as a category changes, but the application surface itself
    /// keeps one retained semantic identity.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("Settings")
                .with_semantic_key("settings.app"),
        )
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    /// Drains the selection-rebuild flag, bubbling the shell's own rebuild
    /// signal (a narrow list↔content navigation or a breakpoint cross).
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .shell
            .as_mut()
            .is_some_and(ui::Widget::take_structure_dirty);
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.secret
            .as_ref()
            .is_some_and(|shown| shown.dialog.needs_animation_frame())
            || self.highlight.is_some()
            || self.app.areas().iter().any(crate::area::PaneArea::busy)
            || self
                .shell
                .as_ref()
                .is_some_and(ui::Widget::needs_animation_frame)
    }

    fn has_pending_visual_work(&self) -> bool {
        self.app
            .areas()
            .iter()
            .any(crate::area::PaneArea::first_read)
            || self
                .shell
                .as_ref()
                .is_some_and(Widget::has_pending_visual_work)
    }

    fn needs_idle_sync(&self) -> bool {
        self.shell.as_ref().is_some_and(ui::Widget::needs_idle_sync)
    }

    fn layout_needs_measure(&self) -> bool {
        self.shell
            .as_ref()
            .is_some_and(ui::Widget::layout_needs_measure)
    }
}

impl AppWidget for SettingsWindow {
    fn app_kind(&self) -> AppKind {
        AppKind::Settings
    }

    fn app_title(&self) -> &'static str {
        "Settings"
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{f0493}") // cog
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Settings)
    }
}

impl SettingsWindow {
    /// Scroll the pane so `setting`'s control is in sight, a little below
    /// the top so its card's heading shows too.
    fn scroll_to_control(&mut self, setting: &'static settings_store::index::Setting) {
        fn control_rect(
            widget: &dyn Widget,
            setting: &settings_store::index::Setting,
        ) -> Option<vello::kurbo::Rect> {
            if widget.test_id().is_some_and(|id| setting.is_control(id)) {
                return Some(widget.get_rect());
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| control_rect(child, setting))
        }
        /// Scroll the innermost scroller holding the control; whether one
        /// did.
        fn reveal(widget: &mut dyn Widget, setting: &settings_store::index::Setting) -> bool {
            if widget
                .children_mut()
                .into_iter()
                .any(|child| reveal(child, setting))
            {
                return true;
            }
            let Some(control) = control_rect(&*widget, setting) else {
                return false;
            };
            let Some(scroll) = widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Scrollable>())
            else {
                return false;
            };
            let viewport = scroll.content_viewport_rect();
            if control.y0 >= viewport.y0 && control.y1 <= viewport.y1 {
                return true;
            }
            let context = (viewport.height() * 0.2).clamp(24.0, 120.0);
            let top = control.y0 - viewport.y0 + f64::from(scroll.offset_y()) - context;
            scroll.scroll_into_view(
                ui::num::f32_of_f64(control.x0 - viewport.x0 + f64::from(scroll.offset_x())),
                ui::num::f32_of_f64(top.max(0.0)),
                1.0,
                ui::num::f32_of_f64(viewport.height()),
            );
            true
        }
        if let Some(shell) = &mut self.shell {
            reveal(shell, setting);
        }
    }

    /// The row of the setting just shown, while it is marked.
    fn highlighted_row(&self) -> Option<vello::kurbo::Rect> {
        let (setting, since) = self.highlight?;
        let elapsed = ui::time::now()
            .saturating_duration_since(since)
            .as_secs_f32();
        if elapsed >= HIGHLIGHT_HOLD {
            return None;
        }
        row_of(self.shell.as_ref()?, setting)
    }
}

/// How long a shown setting's row stays marked, in seconds.
const HIGHLIGHT_HOLD: f32 = 1.5;

/// The row of `setting`'s control under `root`, within the scroller that
/// holds it: the narrowest ancestor of the control at least as wide as most
/// of that scroller, which is the settings row.
fn row_of(
    root: &dyn Widget,
    setting: &settings_store::index::Setting,
) -> Option<vello::kurbo::Rect> {
    fn walk(
        widget: &dyn Widget,
        setting: &settings_store::index::Setting,
        trail: &mut Vec<(vello::kurbo::Rect, bool)>,
    ) -> Option<vello::kurbo::Rect> {
        let rect = widget.get_rect();
        if widget.test_id().is_some_and(|id| setting.is_control(id)) {
            let (viewport, at) = trail
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (_, scroller))| *scroller)
                .map(|(at, (viewport, _))| (*viewport, at))?;
            let row = trail
                .get(at..)
                .unwrap_or_default()
                .iter()
                .rev()
                .map(|(rect, _)| *rect)
                .find(|candidate| candidate.width() >= viewport.width() * 0.6)
                .unwrap_or(rect);
            return Some(row.intersect(viewport));
        }
        trail.push((rect, widget.name() == "Scrollable"));
        let found = widget
            .children()
            .into_iter()
            .find_map(|child| walk(child, setting, trail));
        trail.pop();
        found
    }
    walk(root, setting, &mut Vec::new())
}

/// The identity of the current keyboard control, before a pane rebuilds it.
fn focused_control_id(widget: &dyn Widget) -> Option<&str> {
    if widget.is_focusable()
        && widget.is_focused()
        && let Some(id) = widget.test_id()
    {
        return Some(id);
    }
    widget.children().into_iter().find_map(focused_control_id)
}

/// A reveal transfers the keyboard rather than leaving two focused controls.
fn clear_control_focus(widget: &mut dyn Widget) {
    if widget.is_focusable() && widget.is_focused() {
        widget.set_focus(false);
    }
    for child in widget.children_mut() {
        clear_control_focus(child);
    }
}

/// Put the keyboard on the first matching control under `widget`;
/// whether there was one.
fn focus_control(widget: &mut dyn Widget, matches: &dyn Fn(&str) -> bool) -> bool {
    fn first_focusable(widget: &mut dyn Widget) -> Option<&mut dyn Widget> {
        if widget.is_focusable() {
            return Some(widget);
        }
        widget.children_mut().into_iter().find_map(first_focusable)
    }
    if widget.test_id().is_some_and(matches) {
        return match first_focusable(widget) {
            Some(control) => {
                control.set_focus(true);
                true
            }
            None => false,
        };
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| focus_control(child, matches))
}

/// The page shown, kept with the window: a reopened Settings window shows
/// the page it showed.
impl ui::application_view::ApplicationViewState for SettingsWindow {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        Ok(Some(ui::application_view::ApplicationLocalView {
            app_id: <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID.into(),
            version: 1,
            payload: self.app.selected().slug().as_bytes().to_vec(),
        }))
    }

    fn restore_application_view(
        &mut self,
        local: &ui::application_view::ApplicationLocalView,
        _doc: &automerge::AutoCommit,
    ) -> Result<ui::application_view::ApplicationFocusRestore, ui::view::ThawError> {
        let page = std::str::from_utf8(&local.payload)
            .ok()
            .and_then(Category::from_slug)
            .ok_or_else(|| ui::view::ThawError::State("not a Settings page".into()))?;
        // A link the window was opened with is already asked for and wins.
        if !self.app.pane_requested() {
            self.select(page);
        }
        Ok(ui::application_view::ApplicationFocusRestore::Generic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::Framed;

    struct DamageWitness {
        app: SettingsWindow,
        frames: std::rc::Rc<std::cell::RefCell<Vec<Vec<vello::kurbo::Rect>>>>,
    }

    impl ui::widget::Decorator for DamageWitness {
        type Inner = SettingsWindow;

        fn inner(&self) -> &SettingsWindow {
            &self.app
        }

        fn inner_mut(&mut self) -> &mut SettingsWindow {
            &mut self.app
        }

        fn mark_drawn(&mut self) {
            let mut regions = Vec::new();
            self.app.collect_damage(&mut regions);
            self.frames.borrow_mut().push(regions);
            self.app.mark_drawn();
        }
    }

    #[test]
    fn settings_sidebar_hover_damage_covers_changed_pixels_without_the_whole_window() {
        for theme in [
            ui::Theme::dark(),
            ui::Theme::light(),
            ui::Theme::high_contrast(),
        ] {
            sidebar_hover_damage_for_theme(theme);
        }
    }

    fn sidebar_hover_damage_for_theme(theme: ui::Theme) {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut harness = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            DamageWitness {
                app: config.app(),
                frames: std::rc::Rc::clone(&frames),
            },
            (1200, 700),
            theme.with_reduce_motion(true),
        );
        let bounds = harness
            .a11y_nodes()
            .into_iter()
            .find_map(|(_, node)| {
                (node.role() == ui::accesskit::Role::Button && node.label() == Some("Hide sidebar"))
                    .then(|| node.bounds())
                    .flatten()
            })
            .expect("the real shared sidebar toggle is exposed");
        let pointer = |x, y| ui::synthetic_input::cursor_moved(x, y);
        harness.send(&pointer(1190.0, 690.0));
        // The page at rest before the hover: the Network page's first scan
        // lands on a thread of its own, and on a loaded machine it could
        // otherwise land in the hover's frame and repaint the page with it.
        assert!(
            harness.settle_for_capture(tilo_test_env::hang_bound()),
            "the page comes to rest"
        );
        let mut renderer =
            ui::offscreen::OffscreenRenderer::try_new_with_cpu_preprocessing(1200, 700)
                .expect("the pixel oracle requires the production GPU fine rasterizer");
        let background = harness.theme().tokens.colors.surfaces.content;
        let before = harness.render_rgba_with(&mut renderer, background);
        assert!(
            before == harness.render_rgba_with(&mut renderer, background),
            "an unchanged frame must rasterize identically"
        );
        frames.borrow_mut().clear();
        harness.send(&pointer(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        ));
        let after = harness.render_rgba_with(&mut renderer, background);
        assert_eq!(before.len(), 1200 * 700 * 4);
        assert_eq!(after.len(), before.len());
        let regions = frames
            .borrow()
            .last()
            .cloned()
            .expect("hover paints a frame");
        assert!(!regions.is_empty(), "hover publishes real visual damage");
        let area: f64 = regions.iter().map(vello::kurbo::Rect::area).sum();
        assert!(
            area < 1200.0 * 700.0 / 2.0,
            "a sidebar control must qualify for local repaint: {regions:?}"
        );
        let mut changed = 0_usize;
        for (index, (old, new)) in before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .enumerate()
        {
            if old == new {
                continue;
            }
            changed = changed.saturating_add(1);
            let x = u32::try_from(index % 1200).expect("pixel column");
            let y = u32::try_from(index / 1200).expect("pixel row");
            let point = vello::kurbo::Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            assert!(
                regions
                    .iter()
                    .any(|region| region.inflate(16.0, 16.0).contains(point)),
                "hover changed pixel {point:?} {old:?}->{new:?} outside submitted damage {regions:?}"
            );
        }
        assert!(
            changed > 0,
            "the production pointer route visibly changes the toggle"
        );
    }

    fn find_by_test_id<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if root.test_id() == Some(id) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| find_by_test_id(child, id))
    }

    fn layout_at(app: SettingsWindow, w: f32, h: f32) -> Framed<SettingsWindow> {
        layout_at_with_theme(app, w, h, &ui::Theme::dark())
    }

    fn layout_at_with_theme(
        app: SettingsWindow,
        w: f32,
        h: f32,
        theme: &ui::Theme,
    ) -> Framed<SettingsWindow> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((w, h)),
            theme.clone(),
        )
    }

    #[test]
    fn selecting_a_scrolled_sidebar_row_keeps_the_navigation_in_place() {
        use ui::prelude::TestApp;

        type Window = TestApp<crate::SettingsShell>;
        fn row_rect(app: &mut Window, category: Category) -> Option<vello::kurbo::Rect> {
            app.harness()
                .a11y_nodes()
                .into_iter()
                .find_map(|(_, node)| {
                    if node.role() != ui::accesskit::Role::TreeItem
                        || node.label() != Some(category.label().as_str())
                    {
                        return None;
                    }
                    node.bounds().map(|bounds| {
                        vello::kurbo::Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
                    })
                })
        }
        fn selected(app: &mut Window) -> Category {
            app.with_widget::<SettingsWindow, _>(|settings| settings.selected())
                .expect("settings hosts its surface")
        }
        let mut app = Window::new().expect("settings opens headlessly");
        app.harness()
            .resize((1200, 520))
            .expect("settings lays out");
        app.settle();
        let home = selected(&mut app);
        let home_row = row_rect(&mut app, home).expect("the selected row starts visible");
        let harness = app.harness();
        harness.send(&ui::synthetic_input::cursor_moved(100.0, 300.0));
        harness.scroll(0.0, -3.0);
        // A wheel step glides on after it lands; measure where it rests.
        assert!(
            harness.settle_for_capture(tilo_test_env::hang_bound()),
            "the wheel's glide comes to rest"
        );
        assert!(
            row_rect(&mut app, home).is_none_or(|row| row.y0 < home_row.y0),
            "the real wheel route scrolled the sidebar"
        );
        // Shared lists publish the actual visible rows, so choose a measured
        // destination near the viewport's bottom rather than an offscreen id.
        let (category, scrolled) = Category::ALL
            .iter()
            .filter_map(|&category| {
                let rect = row_rect(&mut app, category)?;
                (category != home && rect.y0 >= 0.0 && rect.y1 <= 520.0).then_some((category, rect))
            })
            .max_by(|a, b| a.1.y0.total_cmp(&b.1.y0))
            .expect("a real visible navigation row");
        let center = scrolled.center();
        app.harness().click_at(center.x, center.y);
        app.settle();
        assert_eq!(
            selected(&mut app),
            category,
            "the actual row selected its pane"
        );
        let selected = row_rect(&mut app, category).expect("selected navigation row");
        assert_eq!(
            selected, scrolled,
            "selection must retain the navigation's scroll offset"
        );
    }

    /// The app opens on the default category unless told otherwise; a real
    /// selection change flags a rebuild and re-selecting does not.
    #[test]
    fn category_selection_starts_on_the_default_and_flags_rebuilds() {
        let config = crate::prefs::test_env::ScratchConfig::new();
        assert_eq!(config.app().selected(), Category::DEFAULT);

        let config = crate::prefs::test_env::ScratchConfig::new();
        assert_eq!(
            config.app().with_category(Category::About).selected(),
            Category::About
        );

        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let mut app = layout_at(app, 1200.0, 760.0);
        app.select(Category::Users);
        assert_eq!(app.selected(), Category::Users);
        assert!(
            app.take_structure_dirty(),
            "a real category change must flag a rebuild so the pane updates"
        );
        // Re-selecting the same category is a no-op (no spurious rebuild).
        app.select(Category::Users);
        assert!(!app.take_structure_dirty());
    }

    #[derive(Debug)]
    enum Back {
        Present,
        Absent,
        Unchecked,
    }

    type ResponsiveCase<'a> = (
        &'a str,
        fn(&crate::prefs::test_env::ScratchConfig) -> SettingsWindow,
        (f32, f32),
        Option<bool>,
        Option<bool>,
        Back,
    );

    /// Lay out each case's app (each with its own config) and check the
    /// resolved layout, sidebar state and back button.
    fn assert_responsive_layouts(cases: Vec<ResponsiveCase<'_>>) {
        for (case, build, (w, h), narrow, sidebar_open, back) in cases {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = build(&config);
            let app = layout_at(app, w, h);
            assert!(app.get_rect().width() > 0.0, "case {case}: lays out");
            if let Some(narrow) = narrow {
                assert_eq!(app.is_narrow(), narrow, "case {case}: narrow");
            }
            if let Some(open) = sidebar_open {
                assert_eq!(app.sidebar_open(), open, "case {case}: sidebar open");
            }
            match back {
                Back::Present => assert!(
                    app.back_button_rect().width() > 0.0,
                    "case {case}: the content screen carries a back arrow to reach the category list"
                ),
                Back::Absent => assert_eq!(
                    app.back_button_rect(),
                    vello::kurbo::Rect::ZERO,
                    "case {case}: the list screen has no back button"
                ),
                Back::Unchecked => {}
            }
        }
    }

    /// The frame a window narrowed to a phone's width draws is the frame a
    /// window opened at that width draws — the same picture, pixel for pixel.
    ///
    /// This is the responsive matrix's phone frame against `--viewport
    /// 390x844`: the matrix builds the app once, at its default viewport, and
    /// then resizes that same app to each preset. A surface that remembered
    /// the width it was built at — a seeded breakpoint, a layout chosen once —
    /// would draw its desktop layout clipped at 390 px here, which is not
    /// "responsive design is broken"; it is "Settings is glitchy". The
    /// breakpoint comes from the rect the shell is laid out in, every pass, so
    /// narrowing is a layout and never a memory.
    #[test]
    fn a_narrowed_window_draws_the_frame_a_phone_opens_at() {
        let config = crate::prefs::test_env::ScratchConfig::new();
        // `--responsive-matrix`: built wide, then the same app resized.
        let mut narrowed = layout_at(
            config.app().with_category(Category::Appearance),
            1280.0,
            800.0,
        );
        assert!(!narrowed.is_narrow(), "the wide frame came first");
        Framed::harness_mut(&mut narrowed)
            .resize(ui::headless::viewport((390, 844)))
            .expect("the phone pass lays out");
        assert!(narrowed.is_narrow(), "the narrowed window is narrow");
        let narrowed_frame = Framed::harness(&narrowed).render_rgba();

        // `--viewport 390x844 --shots`: built at the phone's size.
        let opened = layout_at_with_theme(
            config.app().with_category(Category::Appearance),
            390.0,
            844.0,
            &ui::Theme::dark(),
        );
        assert!(opened.is_narrow(), "the opened window is narrow");
        let opened_frame = Framed::harness(&opened).render_rgba();

        assert_eq!(
            narrowed_frame.len(),
            opened_frame.len(),
            "both frames are 390x844"
        );
        let differing = narrowed_frame
            .chunks_exact(4)
            .zip(opened_frame.chunks_exact(4))
            .filter(|(narrowed, opened)| narrowed != opened)
            .count();
        assert_eq!(
            differing, 0,
            "a window narrowed to a phone's width draws what a window opened at it draws"
        );
        // And the layout underneath, not just the pixels: the same screen
        // with the same control on it.
        assert_eq!(narrowed.back_button_rect(), opened.back_button_rect());
        assert_eq!(narrowed.sidebar_open(), opened.sidebar_open());
    }

    /// The responsive shell docks the sidebar when wide. A collapse to the
    /// narrow (phone) layout keeps the selected category's CONTENT on screen —
    /// it never defaults to the category list, which is a back-tap away.
    #[test]
    fn responsive_layout_docks_wide_and_drills_into_content_when_narrow() {
        // (case, app, size, narrow, sidebar open, back button)
        assert_responsive_layouts(vec![
            (
                "wide docks an open sidebar",
                |c| c.app(),
                (1200.0, 760.0),
                Some(false),
                Some(true),
                Back::Unchecked,
            ),
            (
                "narrow collapse keeps the content",
                |c| c.app(),
                (440.0, 820.0),
                Some(true),
                Some(false),
                Back::Present,
            ),
            (
                "narrow content shot drills into content",
                |c| c.app().with_category(Category::RegionFormats),
                (440.0, 820.0),
                Some(true),
                Some(false),
                Back::Present,
            ),
        ]);
    }

    #[test]
    fn narrow_list_screen_and_busy_panes_lay_out() {
        assert_responsive_layouts(vec![
            (
                "narrow list shot shows the standalone list",
                |c| c.app().with_category_list(),
                (440.0, 820.0),
                Some(true),
                Some(true),
                Back::Absent,
            ),
            (
                "wide accounts lays out",
                |c| c.app().with_category(Category::ExternalAccounts),
                (1200.0, 720.0),
                None,
                None,
                Back::Unchecked,
            ),
            (
                "narrow accounts lays out",
                |c| c.app().with_category(Category::ExternalAccounts),
                (440.0, 820.0),
                None,
                None,
                Back::Unchecked,
            ),
        ]);
    }

    /// Narrow headings, large-text viewports, readable sidebars and list-to-content navigation.
    #[test]
    fn narrow_and_large_text_navigation_keeps_destinations_visible() {
        {
            fn named_rect(w: &dyn Widget, name: &str, out: &mut Option<vello::kurbo::Rect>) {
                if out.is_none()
                    && w.accessibility().and_then(|node| node.name).as_deref() == Some(name)
                {
                    *out = Some(w.get_rect());
                }
                for child in w.children() {
                    named_rect(child, name, out);
                }
            }
            let config = crate::prefs::test_env::ScratchConfig::new();

            let selected = Category::RegionFormats;
            let app = config.app().with_category(selected);
            let app = layout_at(app, 440.0, 820.0);

            let mut heading = None;
            named_rect(&*app, &selected.label(), &mut heading);
            let heading = heading.expect("the content page names the selected destination");
            let back = app.back_button_rect();
            assert!(
                heading.intersect(back).area() == 0.0,
                "the destination heading and back control must not overlap"
            );
        }

        {
            fn named_rect(w: &dyn Widget, name: &str, out: &mut Option<vello::kurbo::Rect>) {
                if out.is_none()
                    && w.accessibility().and_then(|node| node.name).as_deref() == Some(name)
                {
                    *out = Some(w.get_rect());
                }
                for child in w.children() {
                    named_rect(child, name, out);
                }
            }
            let config = crate::prefs::test_env::ScratchConfig::new();

            let selected = Category::RegionFormats;
            let app = config.app().with_category(selected);
            let app =
                layout_at_with_theme(app, 390.0, 844.0, &ui::Theme::dark().with_text_scale(1.5));

            let mut heading = None;
            named_rect(&*app, &selected.label(), &mut heading);
            let heading = heading.expect("the content page names the selected destination");
            assert!(
                heading.y0 < 844.0,
                "large text must retain the destination heading in the first viewport (y={})",
                heading.y0
            );
        }

        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app();
            let app =
                layout_at_with_theme(app, 1800.0, 900.0, &ui::Theme::dark().with_text_scale(2.0));
            assert!(
                !app.is_narrow(),
                "1800px admits a readable split above the scaled compact breakpoint"
            );
            let sidebar = app
                .shell
                .as_ref()
                .expect("layout builds the shell")
                .geometry()
                .sidebar_column;
            assert!(
                (sidebar.width() - sidebar::width_for_text_scale(2.0)).abs() < 0.5,
                "sidebar tracks the same scale as its custom-painted rows: {sidebar:?}"
            );
            let compact = config.app();
            let compact = layout_at_with_theme(
                compact,
                900.0,
                844.0,
                &ui::Theme::dark().with_text_scale(2.0),
            );
            assert!(
                compact.is_narrow(),
                "a 900px split would crowd 200%-text navigation and must collapse"
            );
        }

        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            // Start on the list screen — the reachable back-tap state.
            let app = config.app().with_category_list();
            let mut app = layout_at(app, 440.0, 820.0);
            assert!(app.sidebar_open(), "starts on the list screen");
            app.select(Category::Users);
            assert!(app.take_structure_dirty(), "the pick requests a rebuild");
            Framed::harness_mut(&mut app)
                .relayout_at((440, 820))
                .expect("layout settles");
            assert!(
                !app.sidebar_open(),
                "the pick navigated to the content screen"
            );
            assert!(
                app.back_button_rect().width() > 0.0,
                "a back arrow is present"
            );
        }
    }

    // Settings' search is the window bar's field: it narrows the sidebar in place,
    // and the sidebar draws no search field of its own.
    #[test]
    fn the_window_bar_search_narrows_the_sidebar_in_place() {
        use automerge::ReadDoc;
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let mut app = layout_at(app, 1200.0, 760.0);
        // The pane shown reads off the UI thread and is built again once its
        // read lands; that rebuild is not the query's.
        tilo_test_env::wait_for("the pane shown has read", || {
            let _ = Framed::harness_mut(&mut app).poll_frame();
            (!app.pane_waits().get() && !app.pane_reading()).then_some(())
        });
        let _ = app.take_structure_dirty();
        assert_eq!(app.search_query(), "", "starts with no query");
        assert!(
            find_by_test_id(&*app, "settings.search").is_none(),
            "the sidebar draws no search field of its own"
        );

        let doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let mut search = ui::widget::SearchDriver::default();
        {
            let settings: &mut SettingsWindow = &mut app;
            search.open(ui::window_toolbar::SearchOffer::of(settings).into());
            assert_eq!(
                search.in_place(),
                ui::widget::InPlace::List,
                "the bar's search narrows Settings' list"
            );
            assert!(search.drive(
                settings,
                "night light",
                ui::widget::FindOptions::default(),
                None,
                false
            ));
        }
        app.sync(&doc, &root);
        assert_eq!(app.search_query(), "night light");
        assert_eq!(
            app.found_settings().first(),
            Some(&"display.night_light"),
            "the settings it finds are shown in place of the pane"
        );
        assert!(
            app.take_structure_dirty(),
            "the content shows what was found"
        );
        let nav = app
            .shell
            .as_mut()
            .and_then(NavShell::sidebar_mut::<ui::widget::NavList>)
            .expect("the actual sidebar is mounted");
        let filtered = nav.visible_item_ids();
        nav.set_filter("");
        assert!(nav.visible_item_ids().len() > filtered.len());
        nav.set_filter("night light");
        app.sync(&doc, &root);
        assert_eq!(app.search_query(), "night light");
        assert!(!app.take_structure_dirty());
        assert!(
            doc.get(&root, "settings.search.edit").unwrap().is_none(),
            "a local filter must not create a shared search draft"
        );
    }

    /// No app-name header, and the root landmark survives rebuilds.
    #[test]
    fn chrome_draws_no_app_header_and_keeps_its_landmark() {
        // Case `draws_no_app_name_header`.
        // The app must NOT draw its own title / app-name header — the OS window
        // pill carries the name, and its search is Settings' search. The shell's
        // chrome is the nav list and the content pane (no title element, no
        // search field). We assert on the real composed tree.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app();
            let app = layout_at(app, 1200.0, 760.0);
            let shell = app.shell.as_ref().expect("shell built");
            let names: Vec<&str> = shell.children().iter().map(|c| c.name()).collect();
            assert!(names.contains(&"NavList"), "the category list is admitted");
            assert!(
                !names.contains(&"SearchField"),
                "the window bar's search is Settings' search: the sidebar draws no field"
            );
            for child in shell.children() {
                assert_ne!(
                    child.accessibility().and_then(|node| node.name).as_deref(),
                    Some("Settings"),
                    "window chrome owns the app name; the shell may name its destination"
                );
            }
            assert!(shell.geometry().side_by_side);
        }

        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app().with_category(Category::Appearance);
            let wide = Widget::accessibility(&app).expect("Settings landmark");
            assert_eq!(wide.name.as_deref(), Some("Settings"));
            assert_eq!(wide.semantic_key.as_deref(), Some("settings.app"));

            let app = layout_at(app, 440.0, 820.0);
            let narrow =
                Widget::accessibility(&*app).expect("Settings landmark survives narrow shell");
            assert_eq!(narrow.semantic_key, wide.semantic_key);
        }
    }

    #[test]
    fn without_a_workspace_identity_only_the_identity_pages_ask_for_one() {
        use ui::prelude::TestApp;

        ui::workspace_identity::set_missing(true);
        let mut app = TestApp::<crate::SettingsShell>::new()
            .expect("Settings opens while the workspace has no identity");
        for category in Category::ALL.iter().copied() {
            crate::effects::show_pane(&mut app, category);
            app.harness().wait_until(ui::time::Duration::ZERO, |_| true);
            let asks = app
                .harness()
                .a11y_node_named("Choose who you are here")
                .is_some();
            assert_eq!(
                asks,
                category.needs_workspace_identity(),
                "{category:?} asks for an identity only when it is about one"
            );
            if asks {
                assert!(
                    app.harness().a11y_node_named("Choose identity").is_some(),
                    "{category:?} offers the identity chooser"
                );
            }
        }
        assert!(Category::Users.needs_workspace_identity());
        assert!(!Category::Appearance.needs_workspace_identity());
        assert!(!Category::Network.needs_workspace_identity());
        ui::workspace_identity::set_missing(false);
    }
}

#[cfg(test)]
#[path = "native_observer_tests.rs"]
mod native_observer_tests;
