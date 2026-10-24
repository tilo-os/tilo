//! `SettingsApp` — the top-level settings hub.
//!
//! ## Layout
//!
//! The whole app is one [`NavShell`] (from the shared toolkit): a cohesive
//! sidebar panel — the search field pinned at its top, the nav list beneath —
//! beside the selected category's content pane. The shell owns the responsive
//! behaviour end to end, resolving into exactly one of two states by whether
//! the sidebar fits beside the content:
//!
//! * **wide** — the sidebar fits, so it is always docked (search on top, nav
//!   below) and the content fills the area to its right. There is no collapse
//!   control;
//! * **narrow (phone)** — the sidebar does not fit, so the list and the content
//!   are two separate full-screen screens: the list screen (search + nav) and
//!   the content screen (the pane, full-screen, with a back arrow that returns
//!   to the list). Picking a category navigates the list → its content screen.
//!
//! Settings supplies the shell with three things and reacts to selection: the
//! nav list ([`crate::sidebar::build`]), the selected category's content pane,
//! and the search field. It does not re-implement the breakpoint or the
//! sidebar surface — the shell owns all of that so every system app reads the
//! same.
//!
//! ## State and the rebuild that drives navigation
//!
//! The selected category lives on the widget (mirrored into the document via
//! the per-row select key). When [`SettingsApp::sync`] sees a category's
//! select key set, it updates `selected`, swaps the shell's nav + content, and
//! flags `structure_dirty`. The host's frame loop polls `take_structure_dirty`
//! and re-runs `build_layout`, which is what moves the visible pane to the new
//! category. The shell preserves the selected category across a back/forward
//! narrow navigation.

use crate::categories::Category;
use crate::sidebar;
use ui::tokens::Metrics;
use ui::widget::NavMode;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{Container, NavPane, NavShell, Scrollable, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// One scroller's offset, shared with every scroller rebuilt in its place.
type ScrollOffset = std::rc::Rc<std::cell::Cell<(f32, f32)>>;

pub struct SettingsApp {
    /// Currently-selected sidebar category.
    selected: Category,
    /// `true` once a selection (or other composition-affecting change) has
    /// happened and the host must re-run `build_layout`. Drained by
    /// `take_structure_dirty`.
    structure_dirty: bool,
    /// What the panes reach of the host: the settings document, the door
    /// their controls go through, and what they are built for.
    cx: crate::host::PaneCx,
    /// Which narrow screen the shell starts on: the selected category's
    /// content unless a test asks for the list. Applied when the shell is
    /// created.
    narrow_screen: NavPane,
    /// Seed width hint so the very first offscreen frame resolves the correct
    /// wide/narrow layout. `None` lets the shell decide from its own rect.
    width_hint: Option<f32>,
    /// The pane asked for, taken in at the next sync: the open effect (and,
    /// outside a host, a sidebar row) writes it, so the shell is rebuilt
    /// between frames, never under an event being dispatched. Selection
    /// stays out of collaboration, persistence and undo.
    sidebar_selection: ui::LocalState<Option<Category>>,
    /// What a staged scene narrowed the sidebar to before the shell
    /// existed; the shell's first sidebar starts narrowed to it.
    staged_filter: String,
    /// The setting a link or a search hit asked to be shown, taken in with
    /// the pane it is on.
    setting_request: ui::LocalState<Option<&'static settings_store::index::Setting>>,
    /// The setting to show once its pane is built: the keyboard goes to its
    /// control as the pane is built, and once it is laid out the pane
    /// scrolls to it.
    reveal: Option<&'static settings_store::index::Setting>,
    /// The setting whose control the pane scrolls to after this layout.
    reveal_scroll: Option<&'static settings_store::index::Setting>,
    /// The setting last shown, kept in sight until the person acts: what a
    /// page shows once its reading lands (a battery's card and chart above
    /// the shown control) would otherwise push the control out of view.
    revealed: Option<&'static settings_store::index::Setting>,
    /// A capture's control to put the keyboard on once its pane is built.
    capture_focus: Option<&'static str>,
    /// The setting just shown and when: its row is marked for a moment, so
    /// the eye finds it.
    highlight: Option<(&'static settings_store::index::Setting, ui::time::Instant)>,
    /// Each pane's scroll offset. The content scroller is rebuilt whenever the
    /// pane's structure changes (a control applied, a category opened), so the
    /// offset lives here, per category, rather than in the rebuilt widget.
    pane_scroll: std::cell::RefCell<std::collections::HashMap<Category, ScrollOffset>>,
    /// Retained, revision-aware catalogue behind Ctrl+K. Cloning its provider
    /// for the host is O(1), so a palette continuation always talks to the
    /// same generation instead of rebuilding an index on every frame.
    /// Where the account stores, credential vault and mutation journal live.
    account_root: crate::accounts::AccountRoot,
    rect: vello::kurbo::Rect,
    /// The reusable app-shell, created lazily on the first `build_layout` and
    /// persisted across rebuilds so its collapse/overlay state survives a
    /// category switch. Its nav + content are swapped on selection.
    shell: Option<NavShell>,
    /// Where the last wallpaper request stands, for the Appearance pane.
    wallpaper_status: ui::LocalState<Option<String>>,
    /// What each pane reads of the device and keeps between visits
    /// ([`crate::area`]).
    areas: crate::area::Areas,
    keyboard_map_input: ui::KeyboardMapInput,
    /// When the areas above read what their live rows show.
    area_reads: crate::effects::AreaReads,
    /// The pane shown is the stand-in for one whose area is still being
    /// read ([`Self::pane_reading`]); the sync its read lands in builds it.
    pane_waits: std::cell::Cell<bool>,
    /// The Keyboard page shown was built while the device's layout list
    /// was still being read; it is built again once the list is in.
    keyboard_partial: std::cell::Cell<bool>,
    /// The text scale the shell was built for. A large-text preference widens
    /// the docked sidebar and its breakpoint with the type (the same policy
    /// the Store applies to its card grid), so a change rebuilds the shell.
    shell_text_scale: f32,
    /// The pane whose query the window bar's open search drives
    /// ([`crate::area::PaneQuery`]), from when it opened until it closes.
    queried: Option<Category>,
    /// What the window bar's search last asked for, and the settings it
    /// found, shown as their rows in place of the pane until a page is
    /// opened ([`crate::setting::Found`]). Empty when it found none.
    found_query: String,
    found: Vec<&'static settings_store::index::Setting>,
    /// What each setting found and shown by name or as a switch of the
    /// device is set to, read at every
    /// sync from the sections `value_follows` keeps and what the areas last
    /// read: cheap reads, compared to rebuild only on a change.
    found_reads: Vec<crate::setting::Read>,
    /// The screen a narrow window showed when search found something and
    /// brought what it found to the front: shown again when the search
    /// finds nothing more. `None` while nothing found is shown, or once a
    /// page was opened from it.
    found_over: Option<NavPane>,
    value_follows: crate::setting::Follows,
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsApp {
    pub fn new() -> Self {
        Self::with_account_root(crate::accounts::AccountRoot::live())
    }

    /// Keep every account store, the credential vault and the interrupted
    /// mutation journal under `config_root` instead of the user's config
    /// directory. An instance over its own root never replays or clears
    /// another instance's journal.
    #[must_use]
    pub fn with_config_root(config_root: impl Into<std::path::PathBuf>) -> Self {
        Self::with_account_root(crate::accounts::AccountRoot::at(config_root))
    }

    fn with_account_root(account_root: crate::accounts::AccountRoot) -> Self {
        account_root.recover_mutation_or_warn();
        let cx = crate::host::PaneCx::detached(ui::storage::AppStorage::for_app(
            <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID,
        ));
        let areas = crate::area::Areas::of(&crate::area::AreaContext {
            cx: &cx,
            account_root: &account_root,
        });
        Self {
            areas,
            keyboard_map_input: ui::KeyboardMapInput::default(),
            area_reads: crate::effects::AreaReads::of(&cx),
            pane_waits: std::cell::Cell::new(false),
            keyboard_partial: std::cell::Cell::new(false),
            selected: Category::DEFAULT,
            structure_dirty: false,
            cx,
            narrow_screen: NavPane::Content,
            width_hint: None,
            sidebar_selection: ui::LocalState::new(None),
            staged_filter: String::new(),
            setting_request: ui::LocalState::new(None),
            reveal: None,
            reveal_scroll: None,
            revealed: None,
            capture_focus: None,
            highlight: None,
            pane_scroll: std::cell::RefCell::default(),
            account_root,
            rect: vello::kurbo::Rect::ZERO,
            shell: None,
            shell_text_scale: 1.0,
            wallpaper_status: ui::LocalState::new(None),
            queried: None,
            found_query: String::new(),
            found: Vec::new(),
            found_reads: Vec::new(),
            found_over: None,
            value_follows: crate::setting::Follows::default(),
        }
    }

    /// Construct with a specific category pre-selected. Used by tests and
    /// future deep-linking (`settings://network`).
    pub fn with_selected(category: Category) -> Self {
        Self::new().with_category(category)
    }

    /// Pre-select `category` on an already constructed app.
    #[must_use]
    pub fn with_category(mut self, category: Category) -> Self {
        self.selected = category;
        self
    }

    /// Seed the responsive layout with a known width before the first frame,
    /// so a first layout resolves the wide or narrow layout at once.
    #[must_use]
    pub fn with_width_hint(mut self, width: f32) -> Self {
        self.width_hint = Some(width);
        self
    }

    /// Start a narrow layout on the full-screen category list — the
    /// back-tap-away screen a collapse does not default to. Has no visible
    /// effect when the layout is wide.
    #[must_use]
    pub fn with_category_list(mut self) -> Self {
        self.narrow_screen = NavPane::Sidebar;
        self
    }

    pub fn selected(&self) -> Category {
        self.selected
    }

    /// Ask for `category`'s pane; it opens at the next sync.
    pub fn request_pane(&self, category: Category) {
        self.sidebar_selection.set(Some(category));
    }

    /// Ask for `setting`'s pane, scrolled to the setting with the keyboard
    /// on its control; it opens at the next sync.
    pub fn request_setting(&self, setting: &str) -> Result<(), String> {
        let (category, setting) = setting_and_pane(setting)
            .ok_or_else(|| format!("Settings has no setting `{setting}`"))?;
        self.setting_request.set(Some(setting));
        self.sidebar_selection.set(Some(category));
        Ok(())
    }

    /// Open the pane a search hit or an agent's `open` names by its row,
    /// or the setting it names.
    pub fn opener(&self) -> impl FnMut(&str) + 'static {
        let selection = self.sidebar_selection.clone();
        let setting_request = self.setting_request.clone();
        move |anchor| {
            if let Some(category) = crate::effects::pane_of_row(anchor) {
                selection.set(Some(category));
            } else if let Some((category, setting)) = anchor
                .strip_prefix(crate::effects::SETTING_ANCHOR)
                .and_then(setting_and_pane)
            {
                setting_request.set(Some(setting));
                selection.set(Some(category));
            }
        }
    }

    /// The setting a reveal is waiting to show, for tests.
    #[cfg(test)]
    pub fn pending_reveal(&self) -> Option<&'static str> {
        self.reveal.or(self.reveal_scroll).map(|setting| setting.id)
    }

    /// Switch the active category. Flags `structure_dirty`, swaps the shell's
    /// nav + content, and (on a narrow layout) navigates from the list screen
    /// to the selected category's content screen.
    ///
    /// Picking the category that is already selected still opens it: on the
    /// narrow list screen that row is the checked one, and choosing it must
    /// show its content rather than do nothing.
    pub fn select(&mut self, category: Category) {
        if category != Category::Keyboard {
            self.keyboard_map_input.clear();
        }
        if self.selected != category {
            self.revealed = None;
            self.selected = category;
            self.structure_dirty = true;
            self.refresh_shell_children();
        }
        if let Some(shell) = &mut self.shell {
            shell.show(ui::widget::NavPane::Content);
        }
    }

    /// The narrowest window the shell docks the list in: the list at its
    /// minimum beside a settings page's (`NavShell`, `ContentKind::Page`).
    fn dock_width() -> f32 {
        let m = Metrics::BASE;
        m.sidebar_min_width + ui::widget::ContentKind::Page.min_width(&m)
    }

    /// Whether the current frame resolves to the narrow (phone) layout.
    pub fn is_narrow(&self) -> bool {
        match &self.shell {
            Some(shell) if shell.geometry().content_column.width() > 0.0 => {
                !shell.geometry().stack.is_empty()
            }
            _ => self
                .width_hint
                .is_some_and(|w| w < Self::dock_width() * self.shell_text_scale),
        }
    }

    /// Whether the sidebar / list is currently shown (always when wide, or the
    /// list screen when narrow). Used by `--shots` assertions.
    pub fn sidebar_open(&self) -> bool {
        self.shell.as_ref().is_some_and(NavShell::is_sidebar_open)
    }

    /// What the window's search narrowed the sidebar to (empty when not
    /// searching). The search is the window bar's: the sidebar's list is
    /// what it filters in place.
    pub fn search_query(&self) -> &str {
        match &self.shell {
            Some(shell) => shell
                .sidebar_ref::<ui::widget::NavList>()
                .map_or("", ui::widget::NavList::filter),
            None => &self.staged_filter,
        }
    }

    /// The on-screen rect of the narrow content screen's back-arrow button
    /// (after a layout pass), so interaction tests can click the real control
    /// the shell draws. Zero-sized outside the narrow content screen.
    pub fn back_button_rect(&self) -> vello::kurbo::Rect {
        self.shell
            .as_ref()
            .map_or(vello::kurbo::Rect::ZERO, |shell| shell.geometry().back)
    }

    /// Build the content pane: an unboxed category heading followed by the
    /// selected category's body, wrapped in a vertical [`Scrollable`] so tall
    /// panes scroll with a real overlay scrollbar instead of being clipped.
    ///
    /// This intentionally does *not* redraw the app name: the OS owns window
    /// chrome. It does establish the current Settings destination, though.
    /// That is important on wide screens (where the detail needs an entry
    /// point) and on narrow screens (where the shell's back arrow returns to
    /// the Settings list).
    fn build_content(&self) -> Box<dyn Widget> {
        // What search found reads from its top each time it changes; a pane
        // keeps where it was scrolled to.
        let (body, offset): (Box<dyn Widget>, ScrollOffset) = if self.found.is_empty() {
            (
                self.build_pane_body(),
                std::rc::Rc::clone(
                    self.pane_scroll
                        .borrow_mut()
                        .entry(self.selected)
                        .or_default(),
                ),
            )
        } else {
            (
                Box::new(crate::setting::Found::new(
                    self.cx.clone(),
                    self.found.clone(),
                    self.found_reads.clone(),
                )),
                ScrollOffset::default(),
            )
        };
        // The toolkit's page frame: one readable column on the leading edge
        // with the page gutter around it, so a wide window leaves its extra
        // room beside the form rather than around a narrow floating column.
        let content: Box<dyn Widget> = Box::new(
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

    pub fn set_browser_identity_enabled(
        &self,
        account: String,
        enabled: bool,
    ) -> Result<(), String> {
        self.account_root
            .set_browser_identity_enabled(account, enabled)
    }

    pub fn remove_browser_identity(&self, account: String) -> Result<(), String> {
        self.account_root.remove_browser_identity(account)
    }

    /// Record this process's already-bound public identity for browser choices.
    /// The caller supplies only an HTTPS origin; the identity is never guessed.
    pub fn publish_browser_identity(&self, origin: String) -> Result<(), String> {
        let identity =
            ui::local_identity::bound().ok_or("Sign in before publishing this identity")?;
        let root = hex::encode(identity.group_public_key);
        self.account_root
            .publish_browser_identity(root, origin, identity.display_name)
    }

    /// Hand the panes their host: panes built from now on bind their
    /// controls to its door.
    pub fn attach_host(
        &mut self,
        cx: crate::host::PaneCx,
        accounts: Option<ui::framework::PersonRecord<accounts::person::PersonAccounts>>,
    ) {
        // A screenshot never opens the person's accounts.
        if let Some(root) = cx.scene().and_then(|scene| scene.account_root.clone()) {
            self.account_root = crate::accounts::AccountRoot::at(root);
        } else if let Some(record) = accounts {
            self.account_root = self.account_root.clone().with_person(record);
            // What the device's file held before it followed the record is
            // taken in once; then the services outside any app read the file
            // as the record's projection. A window that does not reach this
            // device (a test's) leaves the device's file alone.
            if cx.reaches_devices()
                && let Err(error) = self
                    .account_root
                    .take_in_device_accounts()
                    .and_then(|()| self.account_root.project())
            {
                tracing::warn!(%error, "the device's account file could not follow the person's accounts");
            }
        }
        self.areas = crate::area::Areas::of(&crate::area::AreaContext {
            cx: &cx,
            account_root: &self.account_root,
        });
        self.area_reads = crate::effects::AreaReads::of(&cx);
        self.cx = cx;
        self.structure_dirty = true;
        self.refresh_shell_children();
    }

    /// Whether the published live rows include `area`'s: those of an area
    /// that has read and of the pane shown, which reads it anyway — every
    /// area's when reading is not put off ([`crate::effects::AreaReads`]).
    pub fn publishes_area(&self, area: &crate::effects::LiveArea) -> bool {
        // An area read off the UI thread is published once its read has
        // been taken in: asking for its rows before would wait for it.
        !self.area_reads.deferred()
            || (area.is_open)(self)
            || (area.pane == self.selected && area.background.is_none())
    }

    /// Whether the selected pane's area is still being read off the UI
    /// thread: its commands and D-Bus calls can take seconds, so the pane
    /// says it is being read until the read lands, rather than the window
    /// waiting for it. Starts the read if the warm-up has not reached the
    /// area yet. A window that reads stand-ins (a screenshot, a test that
    /// does not read as a live window does) reads them at once instead.
    fn pane_reading(&self) -> bool {
        if !self.area_reads.deferred() {
            return false;
        }
        crate::area::registry()
            .live_areas
            .iter()
            .filter(|area| area.pane == self.selected && !(area.is_open)(self))
            .filter_map(|area| area.background.as_ref())
            .any(|read| {
                let _ = (read.start)(self);
                !(read.finished)(self)
            })
    }

    /// Read on after the first frame, so the published rows fill in an area
    /// at a time: the device reads that block run on the background
    /// executor ([`crate::effects::AreaReads::step`]). Once every area has
    /// read, the input sources are read the same way, last, so the Mouse &
    /// Keyboard pane opens on them.
    fn read_on(&self) {
        match self.area_reads.step(self) {
            crate::effects::Step::Read => self.area_reads.wake(),
            crate::effects::Step::Waiting => {}
            crate::effects::Step::Done => {
                // The input sources, the battery and the clock are read once
                // too, so their pages open on what they last reported.
                for area in self.areas.iter() {
                    area.warm();
                }
            }
        }
    }

    /// Where the last wallpaper request stands.
    pub fn wallpaper_status(&self) -> &ui::LocalState<Option<String>> {
        &self.wallpaper_status
    }

    /// What each pane reads of the device and keeps between visits.
    pub fn areas(&self) -> &crate::area::Areas {
        &self.areas
    }

    /// Put `area` in place of the window's area of its type, for a test.
    #[cfg(test)]
    pub fn replace_area<A: crate::area::PaneArea>(&mut self, area: A) {
        self.areas.replace(area);
    }

    /// The physical keyboard input the Keyboard page's drawing follows.
    pub fn keyboard_map_input(&self) -> ui::KeyboardMapInput {
        self.keyboard_map_input.clone()
    }

    /// What the panes reach of the host.
    pub fn host(&self) -> &crate::host::PaneCx {
        &self.cx
    }

    /// Narrow the sidebar to `query`, as the window bar's search does (a
    /// staged scene has no bar to type into).
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

    /// Take in what the window bar's search asks for now: the settings it
    /// finds are shown as their rows, in place of the pane, until the
    /// person opens a page. A query finding none leaves the pane shown.
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
        // In a narrow window what search found is a screen of its own, in
        // front of the list it narrows; finding nothing more goes back.
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

    /// Read what the settings found are set to; whether any reads otherwise
    /// than when last read.
    fn read_found(&mut self) -> bool {
        self.value_follows.refresh();
        let values = crate::setting::reads(self, &self.value_follows, &self.found);
        if values == self.found_reads {
            return false;
        }
        self.found_reads = values;
        true
    }

    /// The settings the window bar's search found and shows, for tests.
    #[must_use]
    pub fn found_settings(&self) -> Vec<&'static str> {
        self.found.iter().map(|setting| setting.id).collect()
    }

    /// Lay out what a screenshot scenario shows.
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

    /// Settings opens while the workspace has no identity selected; the
    /// pages about that identity ask for one in place of their content.
    fn identity_request(&self) -> Option<Box<dyn Widget>> {
        if !(self.selected.needs_workspace_identity() && ui::workspace_identity::missing()) {
            return None;
        }
        self.pane_waits.set(false);
        self.keyboard_partial.set(false);
        // Inline: a page's content, hugging what it says, as the other
        // pages' sections do; the pane scrolls it.
        Some(Box::new(
            ui::workspace_identity::required_state(
                <crate::resource::SettingsShell as ui::framework::ResourceApp>::TITLE,
            )
            .inline(),
        ))
    }

    /// Dispatch to the per-category pane body: the pane, or what stands in
    /// for it while its area is read ([`Self::pane_reading`]).
    fn build_pane_body(&self) -> Box<dyn Widget> {
        if let Some(asks) = self.identity_request() {
            return asks;
        }
        let reading = self.pane_reading();
        self.pane_waits.set(reading);
        self.keyboard_partial.set(
            self.selected == Category::Keyboard
                && self.areas.iter().any(crate::area::PaneArea::partial),
        );
        if reading {
            return crate::widgets::page(vec![crate::widgets::untitled(crate::widgets::rows(
                vec![crate::widgets::note(&crate::i18n::t(
                    "settings-pane-reading",
                ))],
            ))]);
        }
        let selected = self.selected;
        crate::area::registry()
            .panes
            .iter()
            .find(|pane| pane.categories.contains(&selected))
            .map_or_else(
                || crate::widgets::page(Vec::new()),
                |pane| (pane.build)(self, selected),
            )
    }

    /// Create the persistent [`NavShell`] with the current nav + content +
    /// search and the seeded shot state. Shared layout resolves scaled metrics.
    fn build_shell(&self) -> NavShell {
        let nav = sidebar::build(
            &self.cx,
            self.selected,
            &self.staged_filter,
            &self.sidebar_selection,
        );
        let mut shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("settings.shell")
            .mode(NavMode::Push)
            .sidebar(nav)
            .sidebar_header("", Vec::new())
            .content(self.build_content())
            .content_header(self.content_title(), Vec::new());
        shell.show(self.narrow_screen);
        shell
    }

    /// What the content says it is: the pane's name, or that it lists what
    /// search found.
    fn content_title(&self) -> String {
        if self.found.is_empty() {
            self.selected.label()
        } else {
            crate::i18n::t("settings-found-title")
        }
    }

    /// Swap the shell's nav + content to reflect the current selection /
    /// query. No-op before the shell exists (the first `build_layout` will
    /// build them fresh).
    fn refresh_shell_children(&mut self) {
        let content = self.build_content();
        let title = self.content_title();
        if let Some(shell) = &mut self.shell {
            // Keep the retained navigation's viewport and focus when only
            // the selected pane changes.
            if let Some(nav) = shell.sidebar_mut::<ui::widget::NavList>() {
                if nav.selected_id().as_deref() != Some(self.selected.slug()) {
                    nav.set_selected(Some(self.selected.slug()));
                }
            } else {
                shell.set_sidebar(Some(sidebar::build(
                    &self.cx,
                    self.selected,
                    "",
                    &self.sidebar_selection,
                )));
            }
            shell.set_content(content);
            shell.set_content_title(title);
        }
    }
}

impl SettingsApp {
    pub fn keyboard_inspection(&self) -> Result<serde_json::Value, String> {
        fn mounted_map(widget: &dyn Widget) -> Option<&ui::KeyboardMap> {
            if let Some(map) = widget
                .as_any()
                .and_then(|value| value.downcast_ref::<ui::KeyboardMap>())
            {
                return Some(map);
            }
            widget.children().into_iter().find_map(mounted_map)
        }
        if self.selected == Category::Keyboard
            && let Some(map) = mounted_map(self)
        {
            return map.inspection_snapshot();
        }
        Ok(serde_json::json!({
            "available": false, "page_visible": false, "foreground": false,
            "mode": null, "source": null, "model": null, "inspected_key": null,
            "selected_symbols": [], "native_layout_available": false,
            "native_group": null, "native_modifiers": null, "pressed_physical_keys": [],
        }))
    }
}

// Account setup, nearby pairing and identity administration share the app's
// collaboration connection through their host-action clients.
impl ui::widget::Collaborative for SettingsApp {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.areas
            .iter_mut()
            .flat_map(|area| area.take_collab_requests())
            .collect()
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        for area in self.areas.iter_mut() {
            area.handle_collab_event(event);
        }
    }
}

/// The window bar's search queries the shown pane where it offers a query
/// ([`crate::area::PaneArea::query`]), and otherwise narrows the sidebar's
/// list and lists the settings it finds: the window is its `Filterable`,
/// so the search reaches the list while what it found is in front of it in
/// a narrow window.
impl SettingsApp {
    /// What the window bar's search queries now: the pane it opened on while
    /// it stays open, else the shown pane, when that pane offers a query.
    fn pane_query(&self) -> Option<&dyn crate::area::PaneQuery> {
        let category = self.queried.unwrap_or(self.selected);
        self.areas.iter().find_map(|area| area.query(category))
    }
}

impl ui::widget::Queryable for SettingsApp {
    fn query_opened(&mut self) {
        self.queried.get_or_insert(self.selected);
    }

    fn set_query_text(&mut self, text: &str) {
        self.queried.get_or_insert(self.selected);
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

impl ui::widget::Filterable for SettingsApp {
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

impl Widget for SettingsApp {
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
            "selected_category": self.selected.slug(),
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
        for area in self.areas.iter() {
            area.poll();
        }
        // After the first frame, the areas read one at a time until every
        // area's rows are published.
        if self.area_reads.take_due() {
            self.read_on();
        }
        // A sidebar row opens its pane through the pane row's open effect;
        // only a bare app outside a host selects here. Search is local
        // presentation state: no CRDT text, collaboration traffic or undo.
        if let Some(category) = self.sidebar_selection.with(|request| *request) {
            self.sidebar_selection.set(None);
            // Opening a page, from the sidebar or a setting found, shows the
            // page; what search found is left behind.
            let found = !self.found.is_empty();
            self.found.clear();
            self.found_over = None;
            if found && self.selected == category {
                self.structure_dirty = true;
                self.refresh_shell_children();
            }
            self.select(category);
            if let Some(setting) = self.setting_request.with(|request| *request) {
                self.setting_request.set(None);
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
        if (self.pane_waits.get() && !self.pane_reading())
            || (self.keyboard_partial.get()
                && !self.areas.iter().any(crate::area::PaneArea::partial))
        {
            self.keyboard_partial.set(false);
            self.structure_dirty = true;
            self.refresh_shell_children();
        }

        if let Some(shell) = &mut self.shell {
            shell.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Create the shell once and reuse it so its collapse/overlay state
        // survives category switches (which rebuild the tree). A text-scale
        // change is the one thing that re-sizes the shell itself, so it is the
        // one thing that builds it anew.
        crate::widgets::set_compose_text_growth(theme.text_growth());
        if self.shell.is_none() || self.shell_text_scale.to_bits() != theme.text_scale.to_bits() {
            self.shell_text_scale = theme.text_scale;
            self.shell = Some(self.build_shell());
        }
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
        // shows its control once it is built; a control the pane does not
        // show (no such device) leaves the page open at its top.
        if let Some(setting) = self.reveal
            && !self.pane_waits.get()
        {
            self.reveal = None;
            clear_control_focus(shell);
            if focus_control(shell, &|id| setting.is_control(id)) {
                self.reveal_scroll = Some(setting);
            }
        } else if let Some(id) = self.capture_focus.filter(|_| !self.pane_waits.get()) {
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
        self.area_reads.presented();
        self.area_reads.wake();
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
        let visible = self.selected == Category::Keyboard
            && self
                .shell
                .as_mut()
                .is_some_and(|shell| observe_map(shell, input.clone()));
        if !visible {
            self.keyboard_map_input.observe_configuration(&input);
            self.keyboard_map_input.clear();
        }
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
        if self.selected == Category::Keyboard
            && let Some(shell) = &mut self.shell
        {
            observe_map(shell, event);
        }
        if let (Ok(children), Some(shell)) = (ctx.taffy.children(ctx.node_id), &mut self.shell)
            && let Some(&shell_node) = children.first()
        {
            shell.handle_event(event, &mut ctx.reborrow(ctx.path, shell_node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.shell
            .as_ref()
            .map(|s| vec![s as &dyn Widget])
            .unwrap_or_default()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.shell
            .as_mut()
            .map(|s| vec![s as &mut (dyn Widget + '_)])
            .unwrap_or_default()
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
        "SettingsApp"
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
        self.highlight.is_some()
            || self.areas.iter().any(crate::area::PaneArea::busy)
            || self
                .shell
                .as_ref()
                .is_some_and(ui::Widget::needs_animation_frame)
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

impl AppWidget for SettingsApp {
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

impl SettingsApp {
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

fn setting_and_pane(id: &str) -> Option<(Category, &'static settings_store::index::Setting)> {
    let setting = settings_store::index::setting(id)?;
    Some((Category::from_slug(setting.page)?, setting))
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
impl ui::application_view::ApplicationViewState for SettingsApp {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        Ok(Some(ui::application_view::ApplicationLocalView {
            app_id: <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID.into(),
            version: 1,
            payload: self.selected.slug().as_bytes().to_vec(),
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
        if self.sidebar_selection.with(Option::is_none) {
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
        app: SettingsApp,
        frames: std::rc::Rc<std::cell::RefCell<Vec<Vec<vello::kurbo::Rect>>>>,
    }

    impl ui::widget::Decorator for DamageWitness {
        type Inner = SettingsApp;

        fn inner(&self) -> &SettingsApp {
            &self.app
        }

        fn inner_mut(&mut self) -> &mut SettingsApp {
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

    fn layout_at(app: SettingsApp, w: f32, h: f32) -> Framed<SettingsApp> {
        layout_at_with_theme(app, w, h, &ui::Theme::dark())
    }

    fn layout_at_with_theme(
        app: SettingsApp,
        w: f32,
        h: f32,
        theme: &ui::Theme,
    ) -> Framed<SettingsApp> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((w, h)),
            theme.clone(),
        )
    }

    #[test]
    fn selecting_a_scrolled_sidebar_row_keeps_the_navigation_in_place() {
        fn row_rect(app: &Framed<SettingsApp>, category: Category) -> Option<vello::kurbo::Rect> {
            Framed::harness(app)
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
        let config = crate::prefs::test_env::ScratchConfig::new();
        let mut app = layout_at(config.app(), 1200.0, 520.0);
        let home = app.selected();
        let home_row = row_rect(&app, home).expect("the selected row starts visible");
        let harness = Framed::harness_mut(&mut app);
        harness.send(&ui::synthetic_input::cursor_moved(100.0, 300.0));
        harness.scroll(0.0, -3.0);
        // A wheel step glides on after it lands; measure where it rests.
        assert!(
            harness.settle_for_capture(tilo_test_env::hang_bound()),
            "the wheel's glide comes to rest"
        );
        assert!(
            row_rect(&app, home).is_none_or(|row| row.y0 < home_row.y0),
            "the real wheel route scrolled the sidebar"
        );
        // Shared lists publish the actual visible rows, so choose a measured
        // destination near the viewport's bottom rather than an offscreen id.
        let (category, scrolled) = Category::ALL
            .iter()
            .filter_map(|&category| {
                let rect = row_rect(&app, category)?;
                (category != home && rect.y0 >= 0.0 && rect.y1 <= 520.0).then_some((category, rect))
            })
            .max_by(|a, b| a.1.y0.total_cmp(&b.1.y0))
            .expect("a real visible navigation row");
        let center = scrolled.center();
        Framed::harness_mut(&mut app).click_at(center.x, center.y);
        assert_eq!(app.selected(), category, "the actual row selected its pane");
        let selected = row_rect(&app, category).expect("selected navigation row");
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
        fn(&crate::prefs::test_env::ScratchConfig) -> SettingsApp,
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
                |c| c.app().with_width_hint(440.0),
                (440.0, 820.0),
                Some(true),
                Some(false),
                Back::Present,
            ),
            (
                "narrow content shot drills into content",
                |c| {
                    c.app()
                        .with_category(Category::RegionFormats)
                        .with_width_hint(440.0)
                },
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
                |c| c.app().with_width_hint(440.0).with_category_list(),
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
                |c| {
                    c.app()
                        .with_category(Category::ExternalAccounts)
                        .with_width_hint(440.0)
                },
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
            let app = config.app().with_category(selected).with_width_hint(440.0);
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
            let app = config.app().with_category(selected).with_width_hint(390.0);
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
            let app = config.app().with_width_hint(1800.0);
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
            let compact = config.app().with_width_hint(900.0);
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
            let app = config.app().with_width_hint(440.0).with_category_list();
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
            (!app.pane_waits.get() && !app.pane_reading()).then_some(())
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
            let settings: &mut SettingsApp = &mut app;
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
