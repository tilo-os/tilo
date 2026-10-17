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
use crate::panes;
use crate::sidebar;
use ui::tokens::Metrics;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{Container, NavPane, NavShell, Scrollable, Widget};
use ui::widget::{NavMode, SearchField};
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
    /// The text scale used to construct the retained shell. A custom-painted
    /// sidebar has scale-dependent geometry, so a system text-scale change
    /// rebuilds the shell while preserving only durable/local app state.
    /// The live, local-only search query typed into the sidebar's search
    /// field. When non-empty the sidebar shows a filtered results list instead
    /// of the sectioned category list.
    search_query: String,
    /// The pane asked for, taken in at the next sync: the open effect (and,
    /// outside a host, a sidebar row) writes it, so the shell is rebuilt
    /// between frames, never under an event being dispatched. Selection
    /// stays out of collaboration, persistence and undo.
    sidebar_selection: ui::LocalState<Option<Category>>,
    /// Each pane's scroll offset. The content scroller is rebuilt whenever the
    /// pane's structure changes (a control applied, a category opened), so the
    /// offset lives here, per category, rather than in the rebuilt widget.
    pane_scroll: std::cell::RefCell<std::collections::HashMap<Category, ScrollOffset>>,
    /// Process-local backing state for the sidebar search field. Search is a
    /// transient filter, never a durable setting or CRDT text object.
    search_state: ui::LocalState<String>,
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
    /// The assistive software this device has installed.
    software: panes::accessibility::Software,
    /// The notification policy and the apps it has rules for.
    notifications: panes::notifications::NotificationsArea,
    /// The person's default apps and the apps that could be.
    default_apps: panes::personal_apps::DefaultAppsArea,
    /// This device's clock.
    time_date: panes::time_date::TimeDateArea,
    /// The installed input sources and the attached input devices.
    input: panes::mouse_keyboard::InputArea,
    keyboard_map_input: ui::KeyboardMapInput,
    keyboard_map_cache: crate::panes::mouse_keyboard::KeyboardPreviewCacheHandle,
    /// This device's battery, power profile and charge cap.
    power: panes::power::PowerArea,
    /// This device's system image, firmware and codec pack.
    updates: panes::updates::UpdatesArea,
    storage: panes::storage::StorageArea,
    disk_unlock: panes::disk_unlock::DiskUnlockArea,
    /// This device's privacy switches, protection and secret store.
    permissions: panes::permissions::PermissionsArea,
    printers: panes::printers::PrintersArea,
    users: panes::users::UsersArea,
    bluetooth: panes::bluetooth::BluetoothArea,
    speakers: panes::speakers::SpeakersArea,
    agent_models: panes::agent_models::AgentModelsArea,
    device_mesh: panes::device_mesh::DeviceMeshArea,
    audio: panes::audio::AudioArea,
    accounts: panes::accounts::AccountsArea,
    displays: panes::display::DisplayArea,
    network: panes::network::NetworkArea,
    /// When the areas above read what their live rows show.
    area_reads: crate::effects::AreaReads,
    /// The pane shown is the stand-in for one whose area is still being
    /// read ([`Self::pane_reading`]); the sync its read lands in builds it.
    pane_waits: std::cell::Cell<bool>,
    /// The text scale the shell was built for. A large-text preference widens
    /// the docked sidebar and its breakpoint with the type (the same policy
    /// the Store applies to its card grid), so a change rebuilds the shell.
    shell_text_scale: f32,
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
        // A detached app reaches no device, so it probes for no installed
        // software either: what a host reaches is read when it attaches.
        let software = panes::accessibility::Software::for_host(&cx);
        // The areas that act through the host share one client for the
        // action door, which the window's connection carries.
        let device_mesh = panes::device_mesh::DeviceMeshArea::new(&cx);
        let users = panes::users::UsersArea::new(&cx, device_mesh.remote());
        Self {
            notifications: panes::notifications::NotificationsArea::new(&cx),
            default_apps: panes::personal_apps::DefaultAppsArea::new(&cx),
            time_date: panes::time_date::TimeDateArea::new(&cx),
            input: panes::mouse_keyboard::InputArea::new(&cx),
            keyboard_map_input: ui::KeyboardMapInput::default(),
            keyboard_map_cache: std::rc::Rc::default(),
            power: panes::power::PowerArea::new(&cx),
            updates: panes::updates::UpdatesArea::new(&cx),
            storage: panes::storage::StorageArea::new(&cx, device_mesh.remote()),
            disk_unlock: panes::disk_unlock::DiskUnlockArea::new(&cx),
            permissions: panes::permissions::PermissionsArea::new(&cx),
            printers: panes::printers::PrintersArea::new(&cx),
            users,
            bluetooth: panes::bluetooth::BluetoothArea::new(&cx),
            speakers: panes::speakers::SpeakersArea::new(&cx),
            agent_models: panes::agent_models::AgentModelsArea::new(&cx),
            device_mesh,
            audio: panes::audio::AudioArea::new(&cx),
            accounts: panes::accounts::AccountsArea::new(&cx)
                .with_account_root(account_root.clone()),
            displays: panes::display::DisplayArea::new(&cx),
            network: panes::network::NetworkArea::new(&cx),
            area_reads: crate::effects::AreaReads::of(&cx),
            pane_waits: std::cell::Cell::new(false),
            selected: Category::DEFAULT,
            structure_dirty: false,
            cx,
            narrow_screen: NavPane::Content,
            width_hint: None,
            search_query: String::new(),
            sidebar_selection: ui::LocalState::new(None),
            pane_scroll: std::cell::RefCell::default(),
            search_state: ui::LocalState::new(String::new()),
            account_root,
            rect: vello::kurbo::Rect::ZERO,
            shell: None,
            shell_text_scale: 1.0,
            wallpaper_status: ui::LocalState::new(None),
            software,
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
    pub(crate) fn request_pane(&self, category: Category) {
        self.sidebar_selection.set(Some(category));
    }

    /// Open the pane a search hit or an agent's `open` names by its row.
    pub(crate) fn opener(&self) -> impl FnMut(&str) + 'static {
        let selection = self.sidebar_selection.clone();
        move |anchor| {
            if let Some(category) = crate::effects::pane_of_row(anchor) {
                selection.set(Some(category));
            }
        }
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
            self.selected = category;
            self.structure_dirty = true;
            self.refresh_shell_children();
        }
        if let Some(shell) = &mut self.shell {
            shell.show(ui::widget::NavPane::Content);
        }
    }

    /// Whether the current frame resolves to the narrow (phone) layout.
    pub fn is_narrow(&self) -> bool {
        match &self.shell {
            Some(shell) if shell.geometry().content_column.width() > 0.0 => {
                !shell.geometry().stack.is_empty()
            }
            _ => self
                .width_hint
                .is_some_and(|w| w < Metrics::BASE.compact_breakpoint * self.shell_text_scale),
        }
    }

    /// Whether the sidebar / list is currently shown (always when wide, or the
    /// list screen when narrow). Used by `--shots` assertions.
    pub fn sidebar_open(&self) -> bool {
        self.shell.as_ref().is_some_and(NavShell::is_sidebar_open)
    }

    /// The current search query (empty when not searching).
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// The on-screen rect of the narrow content screen's back-arrow button
    /// (after a layout pass), so interaction tests can click the real control
    /// the shell draws. Zero-sized outside the narrow content screen.
    pub fn back_button_rect(&self) -> vello::kurbo::Rect {
        self.shell
            .as_ref()
            .map_or(vello::kurbo::Rect::ZERO, |shell| shell.geometry().back)
    }

    /// Filter the sidebar by `query`, as typing it into the search field
    /// does.
    pub fn search(&mut self, query: &str) {
        self.search_state.set(query.to_string());
        if let Some(field) = self.shell.as_mut().and_then(NavShell::filter_mut) {
            field.set_query(query);
        }
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
        // The toolkit's page frame: one readable column on the leading edge
        // with the page gutter around it, so a wide window leaves its extra
        // room beside the form rather than around a narrow floating column.
        let content: Box<dyn Widget> = Box::new(
            Container::new()
                .readable()
                .with_inset(ui::tokens::Rhythm::Group)
                .with_child(self.build_pane_body()),
        );
        let offset = std::rc::Rc::clone(
            self.pane_scroll
                .borrow_mut()
                .entry(self.selected)
                .or_default(),
        );
        let scroll = Scrollable::new_vertical()
            .with_persisted_offset(offset)
            .with_child(content);
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
    }

    pub(crate) fn set_browser_identity_enabled(
        &self,
        account: String,
        enabled: bool,
    ) -> Result<(), String> {
        self.account_root
            .set_browser_identity_enabled(account, enabled)
    }

    pub(crate) fn remove_browser_identity(&self, account: String) -> Result<(), String> {
        self.account_root.remove_browser_identity(account)
    }

    /// Record this process's already-bound public identity for browser choices.
    /// The caller supplies only an HTTPS origin; the identity is never guessed.
    pub(crate) fn publish_browser_identity(&self, origin: String) -> Result<(), String> {
        let identity =
            ui::local_identity::bound().ok_or("Sign in before publishing this identity")?;
        let root = hex::encode(identity.group_public_key);
        self.account_root
            .publish_browser_identity(root, origin, identity.display_name)
    }

    /// Hand the panes their host: panes built from now on bind their
    /// controls to its door.
    pub(crate) fn attach_host(
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
        self.notifications = panes::notifications::NotificationsArea::new(&cx);
        self.default_apps = panes::personal_apps::DefaultAppsArea::new(&cx);
        self.time_date = panes::time_date::TimeDateArea::new(&cx);
        self.input = panes::mouse_keyboard::InputArea::new(&cx);
        self.power = panes::power::PowerArea::new(&cx);
        self.updates = panes::updates::UpdatesArea::new(&cx);
        self.disk_unlock = panes::disk_unlock::DiskUnlockArea::new(&cx);
        self.permissions = panes::permissions::PermissionsArea::new(&cx);
        self.printers = panes::printers::PrintersArea::new(&cx);
        self.bluetooth = panes::bluetooth::BluetoothArea::new(&cx);
        self.speakers = panes::speakers::SpeakersArea::new(&cx);
        self.agent_models = panes::agent_models::AgentModelsArea::new(&cx);
        self.device_mesh = panes::device_mesh::DeviceMeshArea::new(&cx);
        self.users = panes::users::UsersArea::new(&cx, self.device_mesh.remote());
        self.storage = panes::storage::StorageArea::new(&cx, self.device_mesh.remote());
        self.audio = panes::audio::AudioArea::new(&cx);
        self.accounts =
            panes::accounts::AccountsArea::new(&cx).with_account_root(self.account_root.clone());
        self.displays = panes::display::DisplayArea::new(&cx);
        self.network = panes::network::NetworkArea::new(&cx);
        self.area_reads = crate::effects::AreaReads::of(&cx);
        self.software = panes::accessibility::Software::for_host(&cx);
        self.cx = cx;
        self.structure_dirty = true;
        self.refresh_shell_children();
    }

    /// Whether the published live rows include `area`'s: those of an area
    /// that has read and of the pane shown, which reads it anyway — every
    /// area's when reading is not put off ([`crate::effects::AreaReads`]).
    pub(crate) fn publishes_area(&self, area: &crate::effects::LiveArea) -> bool {
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
        let area_reading = crate::effects::LIVE_AREAS
            .iter()
            .filter(|area| area.pane == self.selected && !(area.is_open)(self))
            .filter_map(|area| area.background.as_ref())
            .any(|read| {
                let _ = (read.start)(self);
                !(read.finished)(self)
            });
        area_reading
            || (matches!(
                self.selected,
                Category::Keyboard | Category::Pointing | Category::Spelling
            ) && self.input.catalog_pending())
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
            crate::effects::Step::Done => self.input.prefetch_catalog(),
        }
    }

    /// Where the last wallpaper request stands.
    pub(crate) fn wallpaper_status(&self) -> &ui::LocalState<Option<String>> {
        &self.wallpaper_status
    }

    /// The assistive software this device has installed.
    pub(crate) fn software(&self) -> panes::accessibility::Software {
        self.software
    }

    /// The notification policy and the apps it has rules for.
    pub(crate) fn notifications(&self) -> &panes::notifications::NotificationsArea {
        &self.notifications
    }

    /// The person's default apps and the apps that could be.
    pub(crate) fn default_apps(&self) -> &panes::personal_apps::DefaultAppsArea {
        &self.default_apps
    }

    /// This device's clock.
    pub(crate) fn time_date(&self) -> &panes::time_date::TimeDateArea {
        &self.time_date
    }

    /// The installed input sources and the attached input devices.
    pub(crate) fn input(&self) -> &panes::mouse_keyboard::InputArea {
        &self.input
    }

    /// This device's battery, power profile and charge cap.
    pub(crate) fn power(&self) -> &panes::power::PowerArea {
        &self.power
    }

    /// Put `area` in place of this device's clock, for a test.
    #[cfg(test)]
    pub(crate) fn set_time_date(&mut self, area: panes::time_date::TimeDateArea) {
        self.time_date = area;
    }

    /// Put `area` in place of this device's power hardware, for a test.
    #[cfg(test)]
    pub(crate) fn set_power(&mut self, area: panes::power::PowerArea) {
        self.power = area;
    }

    /// This device's system image, firmware and codec pack.
    pub(crate) fn updates(&self) -> &panes::updates::UpdatesArea {
        &self.updates
    }

    pub(crate) fn disk_unlock(&self) -> &panes::disk_unlock::DiskUnlockArea {
        &self.disk_unlock
    }

    /// This device's privacy switches, protection and secret store.
    pub(crate) fn permissions(&self) -> &panes::permissions::PermissionsArea {
        &self.permissions
    }

    /// Put `area` in place of this device's privacy and security, for a test.
    #[cfg(test)]
    pub(crate) fn set_permissions(&mut self, area: panes::permissions::PermissionsArea) {
        self.permissions = area;
    }

    pub(crate) fn storage(&self) -> &panes::storage::StorageArea {
        &self.storage
    }

    pub(crate) fn printers(&self) -> &panes::printers::PrintersArea {
        &self.printers
    }

    pub(crate) fn users(&self) -> &panes::users::UsersArea {
        &self.users
    }

    pub(crate) fn bluetooth(&self) -> &panes::bluetooth::BluetoothArea {
        &self.bluetooth
    }

    pub(crate) fn speakers(&self) -> &panes::speakers::SpeakersArea {
        &self.speakers
    }

    pub(crate) fn agent_models(&self) -> &panes::agent_models::AgentModelsArea {
        &self.agent_models
    }

    pub(crate) fn device_mesh(&self) -> &panes::device_mesh::DeviceMeshArea {
        &self.device_mesh
    }

    pub(crate) fn audio(&self) -> &panes::audio::AudioArea {
        &self.audio
    }

    /// Hosted acceptance fixtures can replace account services with isolated providers.
    #[doc(hidden)]
    pub fn accounts(&self) -> &panes::accounts::AccountsArea {
        &self.accounts
    }

    pub(crate) fn displays(&self) -> &panes::display::DisplayArea {
        &self.displays
    }

    pub(crate) fn network(&self) -> &panes::network::NetworkArea {
        &self.network
    }

    /// What the panes reach of the host.
    pub(crate) fn host(&self) -> &crate::host::PaneCx {
        &self.cx
    }

    /// Lay out what a screenshot scenario shows.
    pub(crate) fn stage_scene(&mut self, staged: crate::scenarios::Staged) {
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
            Staged::Search(category) => {
                self.select(category);
                self.search(crate::scenarios::SEARCH_QUERY);
            }
        }
    }

    /// Dispatch to the per-category pane body: the pane, or what stands in
    /// for it while its area is read ([`Self::pane_reading`]).
    fn build_pane_body(&self) -> Box<dyn Widget> {
        let reading = self.pane_reading();
        self.pane_waits.set(reading);
        if reading {
            return crate::widgets::cards(vec![crate::widgets::untitled(crate::widgets::rows(
                vec![crate::widgets::note(&crate::i18n::t(
                    "settings-pane-reading",
                ))],
            ))]);
        }
        match self.selected {
            Category::Network => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.network.model(),
            ))),
            Category::Bluetooth => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.bluetooth.model(),
            ))),
            Category::DeviceMesh => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.device_mesh.model(),
            ))),
            Category::Sound => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.audio.model(),
            ))),
            Category::Speakers => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.speakers.model(),
            ))),
            Category::Displays => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.displays.model(),
            ))),
            Category::Keyboard | Category::Pointing | Category::Spelling => Box::new(
                panes::mouse_keyboard::MouseKeyboardPane::new(self.cx.clone(), &self.input)
                    .with_page(match self.selected {
                        Category::Pointing => panes::mouse_keyboard::InputPage::Pointing,
                        Category::Spelling => panes::mouse_keyboard::InputPage::Spelling,
                        _ => panes::mouse_keyboard::InputPage::Keyboard,
                    })
                    .with_map_input(self.keyboard_map_input.clone())
                    .with_map_cache(self.keyboard_map_cache.clone()),
            ),
            Category::KeyboardShortcuts => Box::new(panes::keyboard_shortcuts::ShortcutPane::new(
                self.cx.clone(),
            )),
            Category::TimeDate => Box::new(panes::time_date::TimeDatePane::new(
                self.cx.clone(),
                &self.time_date,
            )),
            Category::Users => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.users.model(),
            ))),
            Category::Printers => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.printers.model(),
            ))),
            Category::Power => Box::new(panes::power::PowerPane::new(self.cx.clone(), &self.power)),
            Category::Appearance => Box::new(panes::appearance::AppearancePane::new(
                self.cx.clone(),
                self.wallpaper_status.clone(),
            )),
            Category::Accessibility => Box::new(panes::accessibility::AccessibilityPane::new(
                self.cx.clone(),
                self.software,
            )),
            Category::Notifications => Box::new(panes::notifications::NotificationsPane::new(
                self.cx.clone(),
                &self.notifications,
            )),
            Category::DefaultApps => Box::new(panes::personal_apps::PersonalAppsPane::new(
                self.cx.clone(),
                &self.default_apps,
            )),
            Category::Storage => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.storage.model(),
            ))),
            Category::ExternalAccounts => Box::new(panes::model::ModelPane::new(
                std::rc::Rc::clone(self.accounts.model()),
            )),
            Category::AgentModels => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.agent_models.model(),
            ))),
            Category::RegionFormats => Box::new(panes::region::RegionPane::new(self.cx.clone())),
            Category::Permissions => panes::permissions::build_with_disk_unlock(
                &self.cx,
                &self.permissions,
                Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                    self.disk_unlock.model(),
                ))),
            ),
            Category::Updates => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.updates.model(),
            ))),
            Category::About => panes::about::build(&self.cx),
        }
    }

    /// Create the persistent [`NavShell`] with the current nav + content +
    /// search and the seeded shot state. Shared layout resolves scaled metrics.
    fn build_shell(&self) -> NavShell {
        let nav = sidebar::build(
            &self.cx,
            self.selected,
            &self.search_query,
            &self.sidebar_selection,
        );
        let mut filter = SearchField::filter()
            .with_id("settings.search")
            .with_placeholder(&crate::i18n::t("settings-search-placeholder"));
        filter.set_query(&self.search_query);
        let mut shell = NavShell::new()
            .with_id("settings.shell")
            .mode(NavMode::Push)
            .sidebar(nav)
            .sidebar_header("", Vec::new())
            .with_filter(filter)
            .content(self.build_content())
            .content_header(self.selected.label(), Vec::new());
        shell.show(self.narrow_screen);
        shell
    }

    /// Swap the shell's nav + content to reflect the current selection /
    /// query. No-op before the shell exists (the first `build_layout` will
    /// build them fresh).
    fn refresh_shell_children(&mut self) {
        let content = self.build_content();
        if let Some(shell) = &mut self.shell {
            // Keep the retained navigation's viewport and focus when only
            // the selected pane changes.
            if let Some(nav) = shell.sidebar_mut::<ui::widget::NavList>() {
                nav.set_filter(&self.search_query);
                if nav.selected_id().as_deref() != Some(self.selected.slug()) {
                    nav.set_selected(Some(self.selected.slug()));
                }
            } else {
                shell.set_sidebar(Some(sidebar::build(
                    &self.cx,
                    self.selected,
                    &self.search_query,
                    &self.sidebar_selection,
                )));
            }
            shell.set_content(content);
            shell.set_content_title(self.selected.label());
        }
    }
}

impl SettingsApp {
    pub(crate) fn keyboard_inspection(&self) -> Result<serde_json::Value, String> {
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
        let mut requests = self.accounts.remote.take_collab_requests();
        requests.extend(ui::widget::Collaborative::take_collab_requests(
            self.device_mesh.remote_mut(),
        ));
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.accounts.remote.handle_collab_event(event);
        ui::widget::Collaborative::handle_collab_event(self.device_mesh.remote_mut(), event);
    }
}

impl Widget for SettingsApp {
    ui::provides!(dyn ui::widget::Collaborative);

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
            search: &mut Option<serde_json::Value>,
            categories: &mut Vec<&'static str>,
            nav_id: &mut Option<&'static str>,
        ) {
            if widget.test_id() == Some(sidebar::NAV_ID) {
                *nav_id = Some(sidebar::NAV_ID);
            }
            if widget.test_id() == Some("settings.search") {
                let review = widget.text_review();
                *search = Some(serde_json::json!({
                    "rect": rect(widget.get_rect()),
                    "focused": widget.is_focused(),
                    "caret_byte": review.as_ref().map(|(caret, _)| *caret),
                    "text": review.map(|(_, text)| text),
                }));
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
            widget.for_each_child(&mut |child| inspect(child, search, categories, nav_id));
        }
        let (state, path) = ui::doc::document_ref(state, path);
        let mut search = None;
        let mut categories = Vec::new();
        let mut nav_id = None;
        inspect(self, &mut search, &mut categories, &mut nav_id);
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
        let search_rect = search.as_ref().and_then(|value| value.get("rect")).cloned();
        let search_ready = search_rect
            .as_ref()
            .and_then(serde_json::Value::as_array)
            .is_some_and(|r| {
                r.iter().all(|v| v.as_f64().is_some_and(f64::is_finite))
                    && matches!(
                        r.as_slice(),
                        [x0, y0, x1, y1, ..] if x1.as_f64() > x0.as_f64() && y1.as_f64() > y0.as_f64()
                    )
            });
        Some(serde_json::json!({
            "app": "so.tilo.settings",
            "search_id": "settings.search",
            "nav_id": nav_id,
            "ready": valid(self.rect) && search_ready,
            "local_search_text": self.search_state.with(Clone::clone),
            "synchronized_search_query": self.search_query,
            "search_widget_text": search.as_ref().and_then(|value| value.get("text")),
            "search_caret_byte": search.as_ref().and_then(|value| value.get("caret_byte")),
            "search_focused": search.as_ref().and_then(|value| value.get("focused")),
            "search_rect": search_rect,
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
        // Background work an effect started finishes whether or not its pane
        // is open.
        self.agent_models.poll_admin();
        self.updates.poll();
        self.storage.poll();
        self.disk_unlock.poll();
        self.printers.poll();
        self.speakers.poll();
        self.device_mesh.poll();
        self.users.poll();
        self.accounts.poll();
        self.displays.poll();
        self.network.poll();
        self.bluetooth.poll();
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
            self.select(category);
        }
        // The read the pane shown waits for has landed: build the pane.
        if self.pane_waits.get() && !self.pane_reading() {
            self.structure_dirty = true;
            self.refresh_shell_children();
        }
        if let Some(field) = self.shell.as_ref().and_then(NavShell::filter)
            && self.search_state.with(|query| query == &self.search_query)
            && field.value() != self.search_query
        {
            self.search_state.set(field.value().to_owned());
        }
        let query = self.search_state.with(Clone::clone);
        if query != self.search_query {
            self.search_query = query;
            if let Some(field) = self.shell.as_mut().and_then(NavShell::filter_mut) {
                field.set_query(&self.search_query);
            }
            // Filtering changes only navigation. Reconstructing the active
            // pane here would discard its transient state and reseed its
            // document bindings on every search character, and flagging the
            // composition dirty would rebuild the whole layout tree for it.
            // The navigation list is virtualised: it re-measures itself.
            if let Some(nav) = self
                .shell
                .as_mut()
                .and_then(NavShell::sidebar_mut::<ui::widget::NavList>)
            {
                nav.set_filter(&self.search_query);
            }
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
        let shell_node = shell.build_layout(taffy, theme, node_map);

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
        if let Some(field) = self.shell.as_ref().and_then(NavShell::filter)
            && self.search_state.with(|query| query != field.value())
        {
            self.search_state.set(field.value().to_owned());
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
        self.agent_models.admin_busy()
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
        "Personal Settings"
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{f0493}") // cog
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Settings)
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
            harness.settle_for_capture(ui::time::Duration::from_secs(5)),
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

    /// The category list stays capturable as its own full-screen screen, and
    /// a busy pane lays out both wide and narrow.
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
        // Case `narrow_page_heading_explains_the_back_arrow_destination_without_overlap`.
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

        // Case `narrow_large_text_keeps_the_destination_in_the_initial_viewport`.
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

        // Case `wide_large_text_reserves_a_readable_sidebar_before_layout`.
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

        // Case `narrow_pick_navigates_list_to_content_and_back`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            // Start on the list screen — the reachable back-tap state.
            let app = config.app().with_width_hint(440.0).with_category_list();
            let mut app = layout_at(app, 440.0, 820.0);
            assert!(app.sidebar_open(), "starts on the list screen");
            // Picking a category drills into its content screen.
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

    /// Search queries stay local and reach the field.
    #[test]
    fn search_query_is_local_and_reaches_the_field() {
        // A local query narrows the retained navigation without recomposing the pane.
        {
            use automerge::ReadDoc;
            let config = crate::prefs::test_env::ScratchConfig::new();

            let app = config.app();
            let mut app = layout_at(app, 1200.0, 760.0);
            assert_eq!(app.search_query(), "", "starts with no query");

            let doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            app.search_state.set("night light".to_string());
            app.sync(&doc, &root);
            assert_eq!(app.search_query(), "night light");
            assert!(
                !app.take_structure_dirty(),
                "a new query filters navigation in place without rebuilding the pane"
            );
            let nav = app
                .shell
                .as_mut()
                .and_then(NavShell::sidebar_mut::<ui::widget::NavList>)
                .expect("the actual sidebar is mounted");
            assert_eq!(nav.filter(), "night light");
            let filtered = nav.visible_item_ids();
            nav.set_filter("");
            assert!(nav.visible_item_ids().len() > filtered.len());
            nav.set_filter("night light");
            assert_eq!(nav.visible_item_ids(), filtered);
            // Re-syncing the same query is a no-op (no spurious rebuild).
            app.sync(&doc, &root);
            assert!(!app.take_structure_dirty());
            assert!(
                doc.get(&root, "settings.search.edit").unwrap().is_none(),
                "a local filter must not create a shared search draft"
            );
        }

        // Case `a_search_reaches_the_local_field`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app().with_category(Category::Displays);
            app.search("light");
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(
                app.search_query(),
                "light",
                "the local field must retain the requested filter"
            );
            assert_eq!(app.search_state.with(Clone::clone), "light");
            assert!(doc.get_heads().is_empty());
            let _app = layout_at(app, 1200.0, 760.0);
        }
    }

    /// No app-name header, and the root landmark survives rebuilds.
    #[test]
    fn chrome_draws_no_app_header_and_keeps_its_landmark() {
        // Case `draws_no_app_name_header`.
        // The app must NOT draw its own title / app-name header — the OS window
        // pill carries the name. The shell's chrome is exactly the search field,
        // the nav list, and the content pane (no title element). We assert on the
        // real composed tree: the shell's children are those three, in order.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app();
            let app = layout_at(app, 1200.0, 760.0);
            let shell = app.shell.as_ref().expect("shell built");
            let names: Vec<&str> = shell.children().iter().map(|c| c.name()).collect();
            assert!(names.contains(&"NavList"), "the category list is admitted");
            assert!(
                names.contains(&"SearchField"),
                "the search field is admitted"
            );
            for child in shell.children() {
                assert_ne!(
                    child.accessibility().and_then(|node| node.name).as_deref(),
                    Some("Settings"),
                    "window chrome owns the app name; the shell may name its destination"
                );
            }
            // And the resolved layout is the wide split.
            assert!(shell.geometry().side_by_side);
        }

        // Case `root_landmark_is_retained_across_responsive_shell_rebuilds`.
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
}

#[cfg(test)]
#[path = "native_observer_tests.rs"]
mod native_observer_tests;
