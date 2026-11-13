//! The Settings window, as a view of the [`Settings`] resource: one [`NavShell`]
//! with the sidebar's list beside the selected category's pane, resolved to a
//! docked sidebar when it fits and to two full screens when it does not.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::categories::Category;
use crate::effects::SettingsEffect;
#[cfg(feature = "tilo-os")]
use crate::keyboard_watch::KeyboardWatch;
use crate::resource::Settings;
use crate::reveal::{Reveal, RevealAsks};
use crate::settings_app::{ScrollOffset, SettingsApp};
use crate::sidebar;
use settings_store::index::Setting;
use ui::LocalState;
use ui::framework::{Publishes, Upkeep, ViewCx};
use ui::widget::toast::{Notice, ToastKind};
use ui::widget::{AppModel, Element, ModelPath, Widget};
use ui::widget::{Container, NavMode, NavPane, NavShell, QueryView, Scrollable};

#[derive(Clone)]
pub(crate) struct PaneKey {
    category: Category,
    found: Vec<&'static Setting>,
    reads: Vec<crate::setting::Read>,
    generation: u64,
}

impl PartialEq for PaneKey {
    fn eq(&self, other: &Self) -> bool {
        self.category == other.category && self.generation == other.generation
    }
}

#[derive(Default)]
struct Searched {
    query: String,
    settings: Vec<&'static Setting>,
    reads: Vec<crate::setting::Read>,
    over: Option<NavPane>,
}

pub(crate) struct Chrome {
    app: Rc<SettingsApp>,
    key: LocalState<PaneKey>,
    selection: LocalState<Option<String>>,
    title: LocalState<String>,
    pane: LocalState<NavPane>,
    filter: LocalState<String>,
    searched: RefCell<Searched>,
    value_follows: crate::setting::Follows,
    pages_seen: Cell<u64>,
    notice: RefCell<Notice>,
    queried: Cell<Option<Category>>,
    reveal: Rc<RevealAsks>,
}

impl Chrome {
    fn new(app: Rc<SettingsApp>) -> Rc<Self> {
        let selected = app.selected();
        let chrome = Self {
            key: LocalState::new(PaneKey {
                category: selected,
                found: Vec::new(),
                reads: Vec::new(),
                generation: 0,
            }),
            selection: LocalState::new(Some(selected.slug().to_owned())),
            title: LocalState::new(String::new()),
            pane: LocalState::new(NavPane::Content),
            filter: LocalState::new(String::new()),
            searched: RefCell::default(),
            value_follows: crate::setting::Follows::default(),
            pages_seen: Cell::new(app.pages().revision()),
            notice: RefCell::default(),
            queried: Cell::new(None),
            reveal: Rc::default(),
            app,
        };
        chrome.title.set(chrome.content_title());
        Rc::new(chrome)
    }

    fn content_title(&self) -> String {
        if self.searched.borrow().settings.is_empty() {
            let selected = self.app.selected();
            self.app
                .pages()
                .top(selected)
                .map_or_else(|| selected.label(), |page| page.title)
        } else {
            crate::i18n::t("settings-found-title")
        }
    }

    fn refresh(&self) {
        let next = {
            let searched = self.searched.borrow();
            self.key.with(|key| PaneKey {
                category: self.app.selected(),
                found: searched.settings.clone(),
                reads: searched.reads.clone(),
                generation: key.generation + 1,
            })
        };
        self.key.set(next);
        let title = self.content_title();
        if self.title.with(|shown| *shown != title) {
            self.title.set(title);
        }
        let slug = self.app.selected().slug();
        if self
            .selection
            .with(|chosen| chosen.as_deref() != Some(slug))
        {
            self.selection.set(Some(slug.to_owned()));
        }
    }

    pub(crate) fn select(&self, category: Category) {
        if self.app.selected() != category {
            self.reveal.kept.set(None);
            self.app.show(category);
            self.pages_seen.set(self.app.pages().revision());
            self.refresh();
        }
        self.show(NavPane::Content);
    }

    fn show(&self, pane: NavPane) {
        if self.pane.with(|shown| *shown != pane) {
            self.pane.set(pane);
        }
    }

    fn stage(&self, staged: crate::scenarios::Staged) {
        use crate::scenarios::Staged;
        match staged {
            Staged::Pane(category) => self.select(category),
            Staged::List(category) => {
                self.select(category);
                self.show(NavPane::Sidebar);
            }
            Staged::Search(category, query) => {
                self.select(category);
                self.filter.set(query.to_owned());
            }
            Staged::Focused(category, control) => {
                self.select(category);
                self.reveal.control.set(Some(control));
            }
        }
    }

    fn follow_query(&self) {
        let query = self.filter.with(Clone::clone);
        let was_shown = {
            let mut searched = self.searched.borrow_mut();
            if query == searched.query {
                return;
            }
            searched.query.clone_from(&query);
            let found = crate::setting::matching(&query);
            if found.is_empty() && searched.settings.is_empty() {
                return;
            }
            let was_shown = !searched.settings.is_empty();
            searched.settings = found;
            was_shown
        };
        self.read_found();
        self.refresh();
        let shown = !self.searched.borrow().settings.is_empty();
        if shown && !was_shown {
            let over = self.pane.with(|pane| *pane);
            self.searched.borrow_mut().over = Some(over);
            self.show(NavPane::Content);
        } else if !shown && let Some(over) = self.searched.borrow_mut().over.take() {
            self.show(over);
        }
    }

    fn read_found(&self) -> bool {
        let mut searched = self.searched.borrow_mut();
        let values = crate::setting::reads(&self.app, &self.value_follows, &searched.settings);
        if values == searched.reads {
            return false;
        }
        searched.reads = values;
        true
    }

    fn content_shown(&self) -> bool {
        self.key.with(|key| key.generation) == self.app.content_built().get()
    }

    fn settled(&self) -> bool {
        !self.app.pane_settling() && self.content_shown()
    }

    fn sync(&self) {
        let app = &self.app;
        if let Some(staged) = app.take_staged() {
            self.stage(staged);
        }
        if settings_store::deferred_failure_pending() {
            let failures = settings_store::take_deferred_failures();
            for failure in &failures {
                tracing::warn!(
                    sections = ?failure.sections,
                    error = %failure.error,
                    "a settings change was not saved"
                );
            }
            if !failures.is_empty() {
                self.notice.borrow_mut().show(
                    crate::i18n::t("settings-change-not-saved"),
                    ToastKind::Error,
                );
            }
        }
        {
            let mut notice = self.notice.borrow_mut();
            notice.expire();
            notice.publish();
        }
        // Background work an effect started finishes whether or not its pane
        // is open.
        for area in app.areas().iter() {
            area.poll();
        }
        // After the first frame, the areas read one at a time until every
        // area's rows are published.
        if app.area_reads().take_due() {
            app.read_on();
        }
        if let Some(category) = app.take_requested_pane() {
            let found = {
                let mut searched = self.searched.borrow_mut();
                searched.over = None;
                !std::mem::take(&mut searched.settings).is_empty()
            };
            if found && app.selected() == category {
                self.refresh();
            }
            self.select(category);
            if let Some(setting) = app.take_requested_setting() {
                self.reveal.setting.set(Some(setting));
                // The pane is built anew even when it is already open, so
                // its control takes the keyboard in the rebuild.
                self.refresh();
            }
        }
        self.follow_query();
        let searching = !self.searched.borrow().settings.is_empty();
        if searching && self.read_found() {
            self.refresh();
        }
        if (app.pane_waits().get() && !app.pane_reading())
            || (app.keyboard_partial().get()
                && !app.areas().iter().any(crate::area::PaneArea::partial))
        {
            app.keyboard_partial().set_if_changed(false);
            self.refresh();
        }
        let pages = app.pages().revision();
        if pages != self.pages_seen.get() {
            self.pages_seen.set(pages);
            app.forget_scroll(app.selected());
            self.refresh();
        }
        if self.content_shown() && app.content_language().get() != ui::i18n::I18nManager::revision()
        {
            self.refresh();
        }
    }

    fn next_wake(&self) -> Option<ui::time::Instant> {
        let areas = self
            .app
            .areas()
            .iter()
            .filter_map(crate::area::PaneArea::next_wake)
            .min();
        match (areas, self.notice.borrow().deadline()) {
            (Some(areas), Some(notice)) => Some(areas.min(notice)),
            (areas, notice) => areas.or(notice),
        }
    }

    fn pane_query(&self) -> Option<&dyn crate::area::PaneQuery> {
        let category = self.queried.get().unwrap_or_else(|| self.app.selected());
        self.app
            .areas()
            .iter()
            .find_map(|area| area.query(category))
    }

    fn query_opened(&self) {
        if self.queried.get().is_none() {
            self.queried.set(Some(self.app.selected()));
        }
    }
}

pub(crate) fn window(cx: &ViewCx<Settings>, mut app: SettingsApp) -> Element {
    let accounts = cx.shared_person::<accounts::person::PersonAccounts>();
    app.attach(cx, accounts);
    let app = app.into_shared();
    cx.on_open(app.opener());
    {
        let (answer, status) = (Rc::clone(&app), Rc::clone(&app));
        cx.effects(move |effect: SettingsEffect, cx| crate::effects::answer(&answer, effect, cx))
            .status(move || crate::effects::statuses(&status));
    }
    {
        let staged = Rc::clone(&app);
        cx.on_stage(move |scenario| staged.stage(crate::scenarios::staged(scenario)));
    }
    let chrome = Chrome::new(Rc::clone(&app));
    let shell = shell(cx, &chrome);
    let content = Reveal::new(
        Rc::clone(&chrome.reveal),
        {
            let chrome = Rc::clone(&chrome);
            move || chrome.settled()
        },
        Box::new(
            Container::new()
                .filling()
                .with_no_padding()
                .landmark("Settings", "settings.app")
                .with_child(Box::new(shell)),
        ),
    );
    #[cfg(feature = "tilo-os")]
    let content: ui::widget::Element =
        Box::new(KeyboardWatch::new(Rc::clone(&app), Box::new(content)));
    #[cfg(not(feature = "tilo-os"))]
    let content: ui::widget::Element = Box::new(content);
    let content = crate::secrets::SecretSheet::new(app.secrets().clone(), content);
    let content = cx.upkeep(upkeep(cx, &chrome), Box::new(content));
    let content = Box::new(Inspected {
        app: Rc::clone(&app),
        chrome,
        content,
    });
    let (rows, row, pending, intents) = (Rc::clone(&app), Rc::clone(&app), Rc::clone(&app), app);
    cx.publishing(
        Publishes::new()
            .live_rows(move || crate::effects::rows(&rows))
            .live_row(move |id| crate::effects::row(&row, id))
            .live_row_pending(move |id| crate::effects::row_pending(&pending, id))
            .host_intents(move || {
                intents
                    .areas()
                    .iter()
                    .find_map(crate::area::PaneArea::take_host_intent)
            }),
        content,
    )
}

/// The shell over `chrome`: the sidebar's list, the content's title and
/// Back, the narrow window's front screen, and the content column, a part
/// of the view keyed by what it shows. The window bar's search narrows the
/// list even while a narrow window shows the content, where what it finds
/// shows; on a pane that lists the answer to a query (models on Hugging
/// Face), it queries the pane instead.
fn shell(cx: &ViewCx<Settings>, chrome: &Rc<Chrome>) -> NavShell {
    let content = {
        let app = Rc::clone(&chrome.app);
        cx.switch(&chrome.key, move |cx, key| content(cx, &app, key))
    };
    let content = {
        let (offered, opened, typed, closed) = (
            Rc::clone(chrome),
            Rc::clone(chrome),
            Rc::clone(chrome),
            Rc::clone(chrome),
        );
        QueryView::new(content, move |text| {
            typed.query_opened();
            if let Some(query) = typed.pane_query() {
                query.set_query_text(text);
            }
        })
        .offered_while(move || offered.pane_query().is_some())
        .opening(move || opened.query_opened())
        .closing(move || {
            if let Some(query) = closed.pane_query() {
                query.query_closed();
            }
            closed.queried.set(None);
        })
    };
    let (can_go_back, go_back) = (chrome.app.pages().clone(), chrome.app.pages().clone());
    NavShell::new()
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("settings.shell")
        .mode(NavMode::Push)
        .with_pane_state(&chrome.pane)
        .search_sidebar()
        .sidebar(sidebar::build(&cx.actions(), &chrome.selection, &chrome.filter).into_widget())
        .sidebar_header("", Vec::new())
        .content_header_bound(&chrome.title, Vec::new())
        .on_back(
            move || can_go_back.can_go_back(),
            move || {
                go_back.back();
            },
        )
        .content(Box::new(content))
}

fn upkeep(cx: &ViewCx<Settings>, chrome: &Rc<Chrome>) -> Upkeep {
    let app = &chrome.app;
    let (synced, waking) = (Rc::clone(chrome), Rc::clone(chrome));
    let (busy, pending, presented) = (Rc::clone(app), Rc::clone(app), Rc::clone(app));
    let (requests, events) = (Rc::clone(app), Rc::clone(app));
    let (captured, restored) = (Rc::clone(app), Rc::clone(chrome));
    let look = Rc::clone(app);
    let prefs = cx.settings::<crate::prefs::AppearancePrefs>();
    let device = cx.settings::<crate::prefs::AppearanceDevicePrefs>();
    Upkeep::new()
        .syncing(move || synced.sync())
        .busy(move || busy.areas().iter().any(crate::area::PaneArea::busy))
        .next_wake(move || waking.next_wake())
        .pending_visual_work(move || {
            pending.areas().iter().any(|area| {
                crate::area::PaneArea::first_read(area)
                    || crate::area::PaneArea::pending_visual_work(area)
            })
        })
        .presenting(move || {
            presented.area_reads().presented();
            presented.area_reads().wake();
        })
        .appearance(move || {
            (look.selected() == Category::Appearance).then(|| {
                prefs.state().with(|prefs| {
                    device
                        .state()
                        .with(|device| crate::panes::appearance::intent(prefs, device))
                })
            })
        })
        .collaboration(
            move || {
                requests
                    .areas()
                    .iter()
                    .flat_map(crate::area::PaneArea::take_collab_requests)
                    .collect()
            },
            move |event| {
                for area in events.areas().iter() {
                    area.handle_collab_event(event);
                }
            },
        )
        .kept_view(
            <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID,
            1,
            move || captured.selected().slug().as_bytes().to_vec(),
            move |payload| {
                let page = std::str::from_utf8(payload)
                    .ok()
                    .and_then(Category::from_slug)
                    .ok_or_else(|| "not a Settings page".to_owned())?;
                // A link the window was opened with is already asked for
                // and wins.
                if !restored.app.pane_requested() {
                    restored.select(page);
                }
                Ok(())
            },
        )
}

fn content(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>, key: &PaneKey) -> Element {
    app.content_built().set(key.generation);
    let (body, offset): (Element, ScrollOffset) = if key.found.is_empty() {
        (
            pane_body(cx, app, key.category),
            app.scroll_offset(key.category),
        )
    } else {
        (
            crate::setting::found(cx, &key.found, &key.reads),
            ScrollOffset::default(),
        )
    };
    app.content_language()
        .set_if_changed(ui::i18n::I18nManager::revision());
    // The page reads as one column in the middle of the room it has: a wide
    // window leaves its extra room on both sides, so a row's actions on its
    // trailing edge stay beside what they act on.
    let content: Element = Box::new(
        Container::new()
            .readable()
            .centered()
            .with_inset(ui::tokens::Rhythm::Group)
            .with_child(body),
    );
    let scroll = Scrollable::new_vertical()
        .with_persisted_offset(offset)
        .with_child(content);
    Box::new(Container::new().filling().with_child(Box::new(scroll)))
}

fn pane_body(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>, category: Category) -> Element {
    let reading = app.pane_reading();
    app.pane_waits().set_if_changed(reading);
    app.keyboard_partial().set_if_changed(
        category == Category::Keyboard && app.areas().iter().any(crate::area::PaneArea::partial),
    );
    if reading {
        return provider_settings_ui::widgets::page(vec![provider_settings_ui::widgets::untitled(
            provider_settings_ui::widgets::rows(vec![provider_settings_ui::widgets::note(
                &crate::i18n::t("settings-pane-reading"),
            )]),
        )]);
    }
    crate::panes::view(cx, app, category)
        .unwrap_or_else(|| provider_settings_ui::widgets::page(Vec::new()))
}

pub struct Inspected {
    app: Rc<SettingsApp>,
    chrome: Rc<Chrome>,
    content: Element,
}

impl std::ops::Deref for Inspected {
    type Target = SettingsApp;

    fn deref(&self) -> &SettingsApp {
        &self.app
    }
}

impl Inspected {
    #[must_use]
    pub fn app(&self) -> &SettingsApp {
        &self.app
    }

    fn shell(&self) -> Option<&NavShell> {
        fn find(widget: &dyn Widget) -> Option<&NavShell> {
            if let Some(shell) = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<NavShell>())
            {
                return Some(shell);
            }
            widget.children().into_iter().find_map(find)
        }
        find(&*self.content)
    }

    #[must_use]
    pub fn is_narrow(&self) -> bool {
        self.shell()
            .is_some_and(|shell| !shell.geometry().stack.is_empty())
    }

    #[must_use]
    pub fn sidebar_open(&self) -> bool {
        self.shell().is_some_and(NavShell::is_sidebar_open)
    }

    #[must_use]
    pub fn search_query(&self) -> String {
        self.chrome.filter.with(Clone::clone)
    }

    pub fn narrow_sidebar(&self, query: &str) {
        self.chrome.filter.set(query.to_owned());
    }

    #[must_use]
    pub fn found_settings(&self) -> Vec<&'static str> {
        self.chrome
            .searched
            .borrow()
            .settings
            .iter()
            .map(|setting| setting.id)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn pending_reveal(&self) -> Option<&'static str> {
        self.chrome.reveal.pending()
    }
}

impl ui::widget::Decorator for Inspected {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn name(&self) -> &'static str {
        "SettingsInspected"
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        self.content.take_structure_change()
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
        self.content
            .for_each_child(&mut |child| inspect(child, &mut categories, &mut nav_id));
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
        let root = self.content.get_rect();
        Some(serde_json::json!({
            "app": "so.tilo.settings",
            "nav_id": nav_id,
            "ready": valid(root) && nav_id.is_some(),
            "search_query": self.search_query(),
            "root_rect": rect(root),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::prelude::TestApp;

    type Window = TestApp<crate::SettingsShell>;

    fn mounted_in(category: Category, (w, h): (u32, u32), theme: ui::Theme) -> Window {
        let mut app = Window::new().expect("settings opens headlessly");
        app.harness()
            .relayout_themed((w, h), theme)
            .expect("settings lays out");
        if category != Category::DEFAULT {
            crate::effects::show_pane(&mut app, category);
        }
        app.settle();
        app
    }

    fn mounted(category: Category, size: (u32, u32)) -> Window {
        mounted_in(category, size, ui::Theme::dark())
    }

    fn captured(scenario: &str, size: (u32, u32)) -> Window {
        let mut app = Window::capture_at(Some(scenario), size).expect("settings opens a capture");
        app.settle();
        app
    }

    struct Shell {
        narrow: bool,
        sidebar_open: bool,
        back: vello::kurbo::Rect,
        sidebar_column: vello::kurbo::Rect,
        side_by_side: bool,
        children: Vec<(String, Option<String>)>,
    }

    fn shell(app: &mut Window) -> Shell {
        app.with_widget::<NavShell, _>(|shell| {
            let geometry = shell.geometry();
            Shell {
                narrow: !geometry.stack.is_empty(),
                sidebar_open: shell.is_sidebar_open(),
                back: geometry.back,
                sidebar_column: geometry.sidebar_column,
                side_by_side: geometry.side_by_side,
                children: shell
                    .children()
                    .iter()
                    .map(|child| {
                        (
                            child.name().to_owned(),
                            child.accessibility().and_then(|node| node.name),
                        )
                    })
                    .collect(),
            }
        })
        .expect("settings shows its shell")
    }

    fn selected(app: &mut Window) -> Category {
        app.with_widget::<ui::widget::NavList, _>(|list| list.selected_id())
            .flatten()
            .as_deref()
            .and_then(Category::from_slug)
            .expect("the sidebar selects the pane shown")
    }

    fn named_rect(app: &mut Window, name: &str) -> Option<vello::kurbo::Rect> {
        fn walk(w: &dyn Widget, name: &str) -> Option<vello::kurbo::Rect> {
            if w.accessibility().and_then(|node| node.name).as_deref() == Some(name) {
                return Some(w.get_rect());
            }
            w.children().into_iter().find_map(|child| walk(child, name))
        }
        walk(app.harness().root_widget(), name)
    }

    fn sidebar_rows(root: &dyn Widget) -> usize {
        let own = root
            .as_any()
            .and_then(|value| value.downcast_ref::<ui::widget::NavList>())
            .map_or(0, |list| list.visible_item_ids().len());
        root.children()
            .into_iter()
            .map(sidebar_rows)
            .fold(own, usize::saturating_add)
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
        let mut app = mounted_in(
            Category::DEFAULT,
            (1200, 700),
            theme.with_reduce_motion(true),
        );
        let harness = app.harness();
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
        harness.send(&pointer(700.0, 28.0));
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
        harness.dispatch_to_root(&pointer(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        ));
        let regions = app
            .with_widget::<Inspected, _>(|window| {
                let mut regions = Vec::new();
                window.collect_damage(&mut regions);
                regions
            })
            .expect("the window is mounted");
        let harness = app.harness();
        let after = harness.render_rgba_with(&mut renderer, background);
        assert_eq!(before.len(), 1200 * 700 * 4);
        assert_eq!(after.len(), before.len());
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

    #[test]
    fn selecting_a_scrolled_sidebar_row_keeps_the_navigation_in_place() {
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

    #[test]
    fn category_selection_starts_on_the_default() {
        let mut app = mounted(Category::DEFAULT, (1200, 760));
        assert_eq!(selected(&mut app), Category::DEFAULT);
        crate::effects::show_pane(&mut app, Category::Users);
        app.settle();
        assert_eq!(selected(&mut app), Category::Users);
    }

    #[derive(Debug)]
    enum Back {
        Present,
        Absent,
        Unchecked,
    }

    type ResponsiveCase<'a> = (
        &'a str,
        fn((u32, u32)) -> Window,
        (u32, u32),
        Option<bool>,
        Option<bool>,
        Back,
    );

    fn assert_responsive_layouts(cases: Vec<ResponsiveCase<'_>>) {
        for (case, open, size, narrow, sidebar_open, back) in cases {
            let mut app = open(size);
            assert!(
                app.harness().root_widget().get_rect().width() > 0.0,
                "case {case}: lays out"
            );
            let shell = shell(&mut app);
            if let Some(narrow) = narrow {
                assert_eq!(shell.narrow, narrow, "case {case}: narrow");
            }
            if let Some(open) = sidebar_open {
                assert_eq!(shell.sidebar_open, open, "case {case}: sidebar open");
            }
            match back {
                Back::Present => assert!(
                    shell.back.width() > 0.0,
                    "case {case}: the content screen carries a back arrow to reach the category list"
                ),
                Back::Absent => assert_eq!(
                    shell.back,
                    vello::kurbo::Rect::ZERO,
                    "case {case}: the list screen has no back button"
                ),
                Back::Unchecked => {}
            }
        }
    }

    #[test]
    fn a_narrowed_window_draws_the_frame_a_phone_opens_at() {
        let appearance = Category::Appearance.slug();
        let mut narrowed = captured(appearance, (1280, 800));
        assert!(!shell(&mut narrowed).narrow, "the wide frame came first");
        narrowed
            .harness()
            .resize((390, 844))
            .expect("the phone pass lays out");
        narrowed.settle();
        let narrowed_shell = shell(&mut narrowed);
        assert!(narrowed_shell.narrow, "the narrowed window is narrow");
        let narrowed_frame = narrowed.harness().render_rgba();

        let mut opened = captured(appearance, (390, 844));
        let opened_shell = shell(&mut opened);
        assert!(opened_shell.narrow, "the opened window is narrow");
        let opened_frame = opened.harness().render_rgba();

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
        assert_eq!(narrowed_shell.back, opened_shell.back);
        assert_eq!(narrowed_shell.sidebar_open, opened_shell.sidebar_open);
    }

    #[test]
    fn responsive_layout_docks_wide_and_drills_into_content_when_narrow() {
        assert_responsive_layouts(vec![
            (
                "wide docks an open sidebar",
                |size| mounted(Category::DEFAULT, size),
                (1200, 760),
                Some(false),
                Some(true),
                Back::Unchecked,
            ),
            (
                "narrow collapse keeps the content",
                |size| mounted(Category::DEFAULT, size),
                (440, 820),
                Some(true),
                Some(false),
                Back::Present,
            ),
            (
                "narrow content shot drills into content",
                |size| mounted(Category::RegionFormats, size),
                (440, 820),
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
                |size| captured("narrow-list", size),
                (440, 820),
                Some(true),
                Some(true),
                Back::Absent,
            ),
            (
                "wide accounts lays out",
                |size| mounted(Category::ExternalAccounts, size),
                (1200, 720),
                None,
                None,
                Back::Unchecked,
            ),
            (
                "narrow accounts lays out",
                |size| mounted(Category::ExternalAccounts, size),
                (440, 820),
                None,
                None,
                Back::Unchecked,
            ),
        ]);
    }

    #[test]
    fn narrow_and_large_text_navigation_keeps_destinations_visible() {
        {
            let selected = Category::RegionFormats;
            let mut app = mounted(selected, (440, 820));
            let heading = named_rect(&mut app, &selected.label())
                .expect("the content page names the selected destination");
            let back = shell(&mut app).back;
            assert!(
                heading.intersect(back).area() == 0.0,
                "the destination heading and back control must not overlap"
            );
        }

        {
            let selected = Category::RegionFormats;
            let mut app = mounted_in(selected, (390, 844), ui::Theme::dark().with_text_scale(1.5));
            let heading = named_rect(&mut app, &selected.label())
                .expect("the content page names the selected destination");
            assert!(
                heading.y0 < 844.0,
                "large text must retain the destination heading in the first viewport (y={})",
                heading.y0
            );
        }

        {
            let mut app = mounted_in(
                Category::DEFAULT,
                (1800, 900),
                ui::Theme::dark().with_text_scale(2.0),
            );
            let wide = shell(&mut app);
            assert!(
                !wide.narrow,
                "1800px admits a readable split above the scaled compact breakpoint"
            );
            let sidebar = wide.sidebar_column;
            assert!(
                (sidebar.width() - sidebar::width_for_text_scale(2.0)).abs() < 0.5,
                "sidebar tracks the same scale as its custom-painted rows: {sidebar:?}"
            );
            let mut compact = mounted_in(
                Category::DEFAULT,
                (900, 844),
                ui::Theme::dark().with_text_scale(2.0),
            );
            assert!(
                shell(&mut compact).narrow,
                "a 900px split would crowd 200%-text navigation and must collapse"
            );
        }

        {
            let mut app = mounted(Category::DEFAULT, (440, 820));
            let back = shell(&mut app).back;
            app.harness().click_at(back.center().x, back.center().y);
            app.settle();
            assert!(shell(&mut app).sidebar_open, "starts on the list screen");
            let row = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .find_map(|(_, node)| {
                    let category = Category::ALL
                        .iter()
                        .find(|category| node.label() == Some(category.label().as_str()))?;
                    let bounds = node.bounds()?;
                    (node.role() == ui::accesskit::Role::TreeItem
                        && *category != Category::DEFAULT
                        && bounds.y0 >= 0.0
                        && bounds.y1 <= 820.0)
                        .then_some(bounds)
                })
                .expect("a destination row in view");
            app.harness()
                .click_at(f64::midpoint(row.x0, row.x1), f64::midpoint(row.y0, row.y1));
            app.settle();
            let shell = shell(&mut app);
            assert!(
                !shell.sidebar_open,
                "the pick navigated to the content screen"
            );
            assert!(shell.back.width() > 0.0, "a back arrow is present");
        }
    }

    #[test]
    fn the_window_bar_search_narrows_the_sidebar_in_place() {
        fn shows(widget: &dyn Widget, setting: &settings_store::index::Setting) -> bool {
            widget.test_id().is_some_and(|id| setting.is_control(id))
                || widget
                    .children()
                    .into_iter()
                    .any(|child| shows(child, setting))
        }
        let mut app = mounted(Category::DEFAULT, (1200, 760));
        assert_eq!(
            app.harness().native_input_snapshot().unwrap()["search_query"],
            "",
            "starts with no query"
        );
        assert!(
            ui::widget::find_test_id(app.harness().root_widget(), "settings.search").is_none(),
            "the sidebar draws no search field of its own"
        );
        let all = sidebar_rows(app.harness().root_widget());
        let heads = app.read_document(ui::tilo_resource::heads_of);

        let mut search = ui::widget::SearchDriver::default();
        {
            let root = app.harness().root_widget_mut();
            search.open(ui::window_toolbar::SearchOffer::of(&mut *root).into());
            assert_eq!(
                search.in_place(),
                ui::widget::InPlace::List,
                "the bar's search narrows Settings' list"
            );
            assert!(search.drive(
                root,
                "night light",
                ui::widget::FindOptions::default(),
                None,
                false
            ));
        }
        app.settle();
        assert_eq!(
            app.harness().native_input_snapshot().unwrap()["search_query"],
            "night light"
        );
        let night_light =
            settings_store::index::setting("display.night_light").expect("an indexed setting");
        assert!(
            shows(app.harness().root_widget(), night_light),
            "the settings it finds are shown in place of the pane"
        );
        assert!(
            sidebar_rows(app.harness().root_widget()) < all,
            "the search narrows the actual sidebar"
        );
        assert_eq!(
            app.read_document(ui::tilo_resource::heads_of),
            heads,
            "a local filter must not create a shared search draft"
        );
    }

    #[test]
    fn chrome_draws_no_app_header_and_keeps_its_landmark() {
        {
            let mut app = mounted(Category::DEFAULT, (1200, 760));
            let shell = shell(&mut app);
            let names: Vec<&str> = shell
                .children
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            assert!(names.contains(&"NavList"), "the category list is admitted");
            assert!(
                !names.contains(&"SearchField"),
                "the window bar's search is Settings' search: the sidebar draws no field"
            );
            for (_, label) in &shell.children {
                assert_ne!(
                    label.as_deref(),
                    Some("Settings"),
                    "window chrome owns the app name; the shell may name its destination"
                );
            }
            assert!(shell.side_by_side);
        }

        {
            fn landmark(app: &mut Window) -> bool {
                app.harness().a11y_nodes().into_iter().any(|(_, node)| {
                    node.role() == ui::accesskit::Role::Group && node.label() == Some("Settings")
                })
            }
            let mut wide = mounted(Category::Appearance, (1200, 760));
            assert!(landmark(&mut wide), "Settings landmark");
            let mut narrow = mounted(Category::Appearance, (440, 820));
            assert!(
                landmark(&mut narrow),
                "Settings landmark survives narrow shell"
            );
        }
    }
}

#[cfg(test)]
#[cfg(feature = "tilo-os")]
#[path = "native_observer_tests.rs"]
mod native_observer_tests;
