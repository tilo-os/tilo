//! `SettingsApp` — the top-level settings hub.
//!
//! ## Layout
//!
//! The whole app is one [`AppShell`] (from the shared toolkit): a cohesive
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
use ui::widget::{AppModel, ModelPath};
use ui::widget::{AppShell, Column, Container, NarrowScreen, Scrollable, TextInput, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// At or above this allocated width (logical px) the sidebar docks beside the
/// content; below it the shell switches to the narrow list-vs-content screens.
/// The toolkit's `compact_breakpoint` (HIG §9.6): a typical settings window is
/// docked while a narrow / split-screen placement gets the phone layout. At a
/// large-text preference the live breakpoint is this times the text scale
/// (see `SettingsApp::build_shell`).
pub const NAV_BREAKPOINT: f64 = ui::widget::DEFAULT_SHELL_BREAKPOINT;

/// Horizontal gutter (logical px) of the page column inside the content
/// area — the `s6` step.
const PAGE_GUTTER: f32 = ui::tokens::Spacing::BASE.s6;

/// Horizontal inset (logical px) of a pane from the content area's edge: the
/// filling container's standard padding (`s3`, the theme's
/// `padding_standard` default) plus [`PAGE_GUTTER`]. A pane's first-frame
/// width hint is the content area less this on each side.
const PAGE_INSET: f32 = ui::tokens::Spacing::BASE.s3 + PAGE_GUTTER;

/// Readable measure (logical px) the External Accounts cards are centred
/// within on large monitors; see [`SettingsApp::readable_content_measure`].
const ACCOUNTS_READABLE_WIDTH: f32 = 1040.0;

/// Readable measure (logical px) for a short label/chooser form — the same
/// default the toolkit's `FormPage` bounds its content to, so a settings form
/// and an automatic form agree on how far a trailing control may sit from its
/// label. See [`SettingsApp::readable_content_measure`].
const FORM_READABLE_WIDTH: f32 = 720.0;

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
    narrow_screen: NarrowScreen,
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
    /// Process-local navigation request written by a sidebar row. This keeps
    /// current-pane selection out of collaboration, persistence and undo.
    sidebar_selection: ui::LocalState<Option<Category>>,
    /// Process-local backing state for the sidebar search field. Search is a
    /// transient filter, never a durable setting or CRDT text object.
    search_state: ui::LocalState<String>,
    /// Retained, revision-aware catalogue behind Ctrl+K. Cloning its provider
    /// for the host is O(1), so a palette continuation always talks to the
    /// same generation instead of rebuilding an index on every frame.
    search_provider: crate::search_provider::SettingsSearchProvider,
    /// Where the account stores, credential vault and mutation journal live.
    account_root: crate::accounts::AccountRoot,
    /// Secret-free snapshot for resource enumeration. Refreshes at account
    /// mutation boundaries, never during the per-frame resource query.
    account_resources: Vec<accounts::Account>,
    account_commands: ui::LocalState<Option<crate::panes::accounts::AccountViewCommand>>,
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    local_operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    local_operation: Option<tilo_actions::InvocationId>,
    local_operation_token: Option<String>,
    local_pane_command: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    account_probe: Option<(
        tilo_actions::InvocationId,
        std::sync::mpsc::Receiver<Result<String, String>>,
    )>,
    rect: vello::kurbo::Rect,
    /// The reusable app-shell, created lazily on the first `build_layout` and
    /// persisted across rebuilds so its collapse/overlay state survives a
    /// category switch. Its nav + content are swapped on selection.
    shell: Option<AppShell>,
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
    /// This device's battery, power profile and charge cap.
    power: panes::power::PowerArea,
    /// This device's system image, firmware and codec pack.
    updates: panes::updates::UpdatesArea,
    storage: panes::storage::StorageArea,
    printers: panes::printers::PrintersArea,
    users: panes::users::UsersArea,
    bluetooth: panes::bluetooth::BluetoothArea,
    speakers: panes::speakers::SpeakersArea,
    agent_models: panes::agent_models::AgentModelsArea,
    device_mesh: panes::device_mesh::DeviceMeshArea,
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
        Self {
            notifications: panes::notifications::NotificationsArea::new(&cx),
            default_apps: panes::personal_apps::DefaultAppsArea::new(&cx),
            time_date: panes::time_date::TimeDateArea::new(&cx),
            input: panes::mouse_keyboard::InputArea::new(&cx),
            power: panes::power::PowerArea::new(&cx),
            updates: panes::updates::UpdatesArea::new(&cx),
            storage: panes::storage::StorageArea::new(&cx),
            printers: panes::printers::PrintersArea::new(&cx),
            users: panes::users::UsersArea::new(&cx),
            bluetooth: panes::bluetooth::BluetoothArea::new(&cx),
            speakers: panes::speakers::SpeakersArea::new(&cx),
            agent_models: panes::agent_models::AgentModelsArea::new(&cx),
            device_mesh: panes::device_mesh::DeviceMeshArea::new(&cx),
            selected: Category::DEFAULT,
            structure_dirty: false,
            cx,
            narrow_screen: NarrowScreen::Content,
            width_hint: None,
            search_query: String::new(),
            sidebar_selection: ui::LocalState::new(None),
            search_state: ui::LocalState::new(String::new()),
            search_provider: crate::search_provider::SettingsSearchProvider::new(),
            account_resources: account_root.store().all().to_vec(),
            account_root,
            account_commands: ui::LocalState::new(None),
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            local_operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            local_operation: None,
            local_operation_token: None,
            local_pane_command: ui::LocalState::new(None),
            account_probe: None,
            rect: vello::kurbo::Rect::ZERO,
            shell: None,
            shell_text_scale: 1.0,
            wallpaper_status: ui::LocalState::new(None),
            software: panes::accessibility::Software::installed(),
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
        self.narrow_screen = NarrowScreen::List;
        self
    }

    pub fn selected(&self) -> Category {
        self.selected
    }

    pub(crate) fn account_resource_inventory(&self) -> &[accounts::Account] {
        &self.account_resources
    }

    fn refresh_account_resources(&mut self) {
        self.account_resources = self.account_root.store().all().to_vec();
    }

    #[doc(hidden)]
    pub fn set_account_tester_for_test(
        &mut self,
        tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    ) {
        self.account_tester = tester;
    }

    /// Switch the active category. Flags `structure_dirty`, swaps the shell's
    /// nav + content, and (on a narrow layout) navigates from the list screen
    /// to the selected category's content screen.
    pub fn select(&mut self, category: Category) {
        if self.selected != category {
            self.selected = category;
            self.structure_dirty = true;
            self.refresh_shell_children();
            if let Some(shell) = &mut self.shell {
                shell.notify_selection();
            }
        }
    }

    /// Whether the current frame resolves to the narrow (phone) layout.
    pub fn is_narrow(&self) -> bool {
        match &self.shell {
            Some(s) => s.is_narrow(),
            None => self
                .width_hint
                .is_some_and(|w| f64::from(w) < self.scaled_breakpoint()),
        }
    }

    /// The width a pane can expect on the coming frame: the shell's content
    /// area once laid out, else the window hint less the docked sidebar —
    /// either way less the page inset on both sides, so the value is the
    /// pane's own width, not the content area's. A pane that composes
    /// differently at phone width (the Region formats card) reads this for
    /// its first frame and its own rect thereafter.
    fn content_width_hint(&self) -> Option<f32> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a logical-pixel width comfortably representable by f32"
        )]
        let content_area = self
            .shell
            .as_ref()
            .map(ui::AppShell::content_area_rect)
            .filter(|rect| rect.width() > 0.0)
            .map(|rect| rect.width() as f32)
            .or_else(|| {
                self.width_hint.map(|w| {
                    if self.is_narrow() {
                        w
                    } else {
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "the docked sidebar width is a small logical-pixel value"
                        )]
                        let sidebar = self.scaled_sidebar_width() as f32;
                        (w - sidebar).max(0.0)
                    }
                })
            });
        content_area.map(|w| (w - 2.0 * PAGE_INSET).max(0.0))
    }

    /// Whether the sidebar / list is currently shown (always when wide, or the
    /// list screen when narrow). Used by `--shots` assertions.
    pub fn sidebar_open(&self) -> bool {
        self.shell.as_ref().is_some_and(ui::AppShell::sidebar_open)
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
            .map_or(vello::kurbo::Rect::ZERO, ui::AppShell::back_rect)
    }

    /// Filter the sidebar by `query`, as typing it into the search field
    /// does.
    pub fn search(&mut self, query: &str) {
        self.search_state.set(query.to_string());
    }

    /// Build the local search field placed at the top of the shell's sidebar.
    /// The field shares typed local state with the app, so filtering survives a
    /// structural rebuild without creating a document draft. Because the shell
    /// hosts it as a direct child in the App's tree, a click focuses it and
    /// `autofocus_on_structure_change` restores focus + caret after the
    /// filtered sidebar rebuilds.
    fn build_search_field(&self) -> Box<dyn Widget> {
        Box::new(
            TextInput::from_state(&self.search_state)
                .with_kind(ui::widget::TextInputKind::Search)
                .with_id("settings.search")
                .with_placeholder(&crate::i18n::t("settings-search-placeholder"))
                .with_font_size(13.0),
        )
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
        let body = self.build_pane_body();
        // The page gutter (`s6`) inside the filling container's standard
        // inset is what `PAGE_INSET` names; heading and body sit a card step
        // apart, one `s4` below the page's top.
        let inner = Column::new()
            .with_no_padding()
            .with_horizontal_padding(PAGE_GUTTER, PAGE_GUTTER)
            .with_vertical_padding(crate::widgets::CARD_GAP)
            .with_intrinsic_height()
            .with_gap(ui::tokens::Spacing::BASE.s4)
            .push(crate::widgets::page_heading(
                &self.selected.label(),
                &self.selected.subtitle(),
            ))
            .push(body);
        // Account forms and cards are more legible as a column on large
        // desktop displays. Operational panes deliberately keep their full
        // detail width for device tables, channel controls, and bindings.
        let content: Box<dyn Widget> = if let Some(width) = self.readable_content_measure() {
            Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(width)
                    .with_child(Box::new(inner)),
            )
        } else {
            Box::new(inner)
        };
        let scroll = Scrollable::new_vertical().with_child(content);
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
    }

    /// The readable measure (logical px) a pane's content is centred within on
    /// large monitors, or `None` for panes that keep the full detail width.
    /// Every cap is a maximum, so narrow screens stay fluid.
    ///
    /// - Account cards keep the wide review measure their row hierarchy was
    ///   accepted at.
    /// - Region & Formats is a short label/chooser form: at the full detail
    ///   width its trailing dropdowns drifted ~600 px from their labels, so it
    ///   takes the same readable form measure the toolkit's `FormPage` uses.
    ///   (UI-review A3, settings-region-wide-balance.)
    ///
    /// Other panes keep their full width until each has an equivalent
    /// content-measure review.
    fn readable_content_measure(&self) -> Option<f32> {
        match self.selected {
            Category::ExternalAccounts => Some(ACCOUNTS_READABLE_WIDTH),
            Category::RegionFormats => Some(FORM_READABLE_WIDTH),
            _ => None,
        }
    }

    /// Hand the panes their host: panes built from now on bind their
    /// controls to its door.
    pub(crate) fn attach_host(&mut self, cx: crate::host::PaneCx) {
        // A screenshot never opens the person's accounts.
        if let Some(root) = cx.scene().and_then(|scene| scene.account_root.clone()) {
            self.account_root = crate::accounts::AccountRoot::at(root);
            self.refresh_account_resources();
        }
        self.notifications = panes::notifications::NotificationsArea::new(&cx);
        self.default_apps = panes::personal_apps::DefaultAppsArea::new(&cx);
        self.time_date = panes::time_date::TimeDateArea::new(&cx);
        self.input = panes::mouse_keyboard::InputArea::new(&cx);
        self.power = panes::power::PowerArea::new(&cx);
        self.updates = panes::updates::UpdatesArea::new(&cx);
        self.storage = panes::storage::StorageArea::new(&cx);
        self.printers = panes::printers::PrintersArea::new(&cx);
        self.users = panes::users::UsersArea::new(&cx);
        self.bluetooth = panes::bluetooth::BluetoothArea::new(&cx);
        self.speakers = panes::speakers::SpeakersArea::new(&cx);
        self.agent_models = panes::agent_models::AgentModelsArea::new(&cx);
        self.device_mesh = panes::device_mesh::DeviceMeshArea::new(&cx);
        self.software = panes::accessibility::Software::for_host(&cx);
        self.cx = cx;
        self.structure_dirty = true;
        self.refresh_shell_children();
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

    /// This device's system image, firmware and codec pack.
    pub(crate) fn updates(&self) -> &panes::updates::UpdatesArea {
        &self.updates
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

    /// Dispatch to the per-category pane body.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match constructs every category's pane"
    )]
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            // A capture never waits for NetworkManager or PipeWire; it shows
            // sample networks and a sample audio graph.
            Category::Network if !self.cx.reaches_devices() => Box::new(
                crate::scenarios::network_pane()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Network => Box::new(
                panes::network::NetworkPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Bluetooth => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.bluetooth.model(),
            ))),
            Category::DeviceMesh => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.device_mesh.model(),
            ))),
            Category::Sound if !self.cx.reaches_devices() => Box::new(
                crate::scenarios::audio_pane()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Sound => Box::new(
                panes::audio::AudioPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Speakers => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.speakers.model(),
            ))),
            Category::Displays if !self.cx.reaches_devices() => Box::new(
                panes::display::DisplayPane::with_dispatcher(
                    crate::dispatcher::dispatcher_or_real(None),
                    0.62,
                )
                .with_command_state(&self.local_pane_command)
                .with_operation_status(&self.local_operation_status),
            ),
            Category::Displays => Box::new(
                panes::display::DisplayPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::MouseKeyboard => Box::new(panes::mouse_keyboard::MouseKeyboardPane::new(
                self.cx.clone(),
                &self.input,
            )),
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
            Category::ExternalAccounts => {
                let mut pane = match self.cx.scene() {
                    // A screenshot shows the bundled sample accounts.
                    Some(_) => panes::accounts::AccountsPane::from_dir_in(
                        self.account_root.clone(),
                        crate::accounts::sample_directory(),
                        false,
                    ),
                    None => panes::accounts::AccountsPane::in_root(self.account_root.clone()),
                };
                if let Some(kind) = self.cx.scene().and_then(|scene| scene.account_form) {
                    pane = pane.with_open_form_for_shot(kind);
                }
                Box::new(
                    pane.with_command_state(&self.account_commands)
                        .with_account_tester(std::sync::Arc::clone(&self.account_tester)),
                )
            }
            Category::AgentModels => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.agent_models.model(),
            ))),
            Category::RegionFormats => {
                let pane = panes::region::RegionPane::new(self.cx.clone());
                Box::new(match self.content_width_hint() {
                    Some(width) => pane.with_width_hint(width),
                    None => pane,
                })
            }
            Category::Permissions => panes::permissions::build(&self.cx),
            Category::Updates => Box::new(panes::model::ModelPane::new(std::rc::Rc::clone(
                self.updates.model(),
            ))),
            Category::About => panes::about::build(&self.cx),
        }
    }

    /// The factor the shell's own metrics grow by under a large-text
    /// preference: the text scale, never below 1×. Large text needs the room
    /// a narrow window does — the sidebar's labels grow with the type, so
    /// its docked width and the breakpoint at which it docks grow with them
    /// rather than clipping 28 px labels into a 264 px column. At 1× both are
    /// exactly the shared shell's defaults. App-side until `AppShell`
    /// resolves the scaled `Metrics` from the theme itself (HIG §9.6 names
    /// that as the shells' next step); a scale change rebuilds the shell, so
    /// its narrow-screen memory resets on that one event.
    fn shell_scale(&self) -> f64 {
        f64::from(self.shell_text_scale.max(1.0))
    }

    /// The width at or above which the sidebar docks, at the current scale.
    fn scaled_breakpoint(&self) -> f64 {
        NAV_BREAKPOINT * self.shell_scale()
    }

    /// The docked sidebar's width at the current scale.
    fn scaled_sidebar_width(&self) -> f64 {
        sidebar::SIDEBAR_WIDTH * self.shell_scale()
    }

    /// Create the persistent [`AppShell`] with the current nav + content +
    /// search and the seeded shot state, for `shell_text_scale` (see
    /// [`Self::shell_scale`]).
    fn build_shell(&self) -> AppShell {
        let nav = sidebar::build(self.selected, &self.search_query, &self.sidebar_selection);
        let content = self.build_content();
        // A narrow (phone) layout keeps the selected category's CONTENT on
        // screen — the category list is one back-tap away, never the default a
        // collapse lands on. A `--shots` capture can pin a specific narrow
        // screen; otherwise the shell collapses to the content.
        let narrow_screen = self.narrow_screen;
        let mut shell = AppShell::new(nav, content)
            .with_search(self.build_search_field())
            // AppShell resolves text-authored extents against the live theme.
            // Pass the 1x values here; pre-scaling them would apply the text
            // scale twice when the shell refreshes its metrics.
            .with_breakpoint(NAV_BREAKPOINT)
            .with_sidebar_width(sidebar::SIDEBAR_WIDTH)
            .with_narrow_screen(narrow_screen);
        if let Some(w) = self.width_hint {
            shell = shell.with_width_hint(f64::from(w));
        }
        shell
    }

    /// Swap the shell's nav + content to reflect the current selection /
    /// query. No-op before the shell exists (the first `build_layout` will
    /// build them fresh).
    fn refresh_shell_children(&mut self) {
        let nav = sidebar::build(self.selected, &self.search_query, &self.sidebar_selection);
        let content = self.build_content();
        if let Some(shell) = &mut self.shell {
            shell.set_nav(nav);
            shell.set_content(content);
        }
    }

    /// Route a canonical device-local operation through process-local typed
    /// state. The pane remains the sole owner of the privileged command, its
    /// availability checks, progress UI, and final notice. The collaborative
    /// document is intentionally absent from this transport.
    #[allow(
        clippy::match_same_arms,
        clippy::too_many_lines,
        reason = "the closed device-local protocols stay together for auditable validation; identical no-op validation arms document explicitly supported variants"
    )]
    fn start_local_device_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &mut automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Err("device-local settings commands must be effect operations".into());
        }
        if self.local_operation.is_some() {
            return Err("another device-local Settings operation is still running".into());
        }
        let accepted: Result<(), String> = match type_id {
            crate::actions::NETWORK_OPERATION_TYPE => {
                let operation: crate::actions::NetworkOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Network);
                if matches!(
                    operation,
                    crate::actions::NetworkOperation::SetWiredEnabled { .. }
                ) {
                    let wired = os_actions::get_wired()?;
                    if wired.device.is_none() {
                        return Err("wired networking is unavailable on this device".into());
                    }
                }
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Network(operation)));
                Ok(())
            }
            crate::actions::AUDIO_OPERATION_TYPE => {
                let operation: crate::actions::AudioOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                let graph = media::audio::enumerate();
                self.select(Category::Sound);
                let validated: Result<(), String> = match operation.clone() {
                    crate::actions::AudioOperation::SetOutputVolume { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("output volume must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::AudioOperation::SetOutputMuted { .. } => Ok(()),
                    crate::actions::AudioOperation::SetInputVolume { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("input volume must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::AudioOperation::SetBalance { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("output balance must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::AudioOperation::SetMono { .. }
                    | crate::actions::AudioOperation::SetEchoCancellation { .. }
                    | crate::actions::AudioOperation::SetSystemSounds { .. } => Ok(()),
                    crate::actions::AudioOperation::SetSystemSoundVolume { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("system sound volume must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::AudioOperation::SetSoundTheme { theme } => {
                        if media::event_sound::installed_themes()
                            .iter()
                            .any(|installed| installed.id == theme)
                        {
                            Ok(())
                        } else {
                            Err("that sound theme is not installed".into())
                        }
                    }
                    crate::actions::AudioOperation::SetEventSound { event, .. } => {
                        settings_store::system_sounds::SystemSoundEvent::from_key(&event)
                            .map(|_| ())
                            .ok_or_else(|| "unknown system sound event".to_string())
                    }
                    crate::actions::AudioOperation::SetDeviceProfile {
                        device_id,
                        profile_index,
                    } => {
                        let hardware = graph
                            .hardware
                            .iter()
                            .find(|device| device.id == device_id)
                            .ok_or_else(|| "audio hardware is no longer available".to_string())?;
                        let profile = hardware
                            .profiles
                            .iter()
                            .find(|profile| profile.index == profile_index && profile.available)
                            .ok_or_else(|| "audio profile is no longer available".to_string())?;
                        let _ = profile;
                        Ok(())
                    }
                    crate::actions::AudioOperation::RouteStream {
                        stream_id,
                        playback,
                        device_id,
                    } => {
                        let stream_exists = if playback {
                            graph
                                .playback_streams
                                .iter()
                                .any(|stream| stream.id == stream_id)
                        } else {
                            graph
                                .record_streams
                                .iter()
                                .any(|stream| stream.id == stream_id)
                        };
                        if !stream_exists {
                            return Err("audio stream is no longer available".into());
                        }
                        let devices = if playback {
                            &graph.sinks
                        } else {
                            &graph.sources
                        };
                        match device_id {
                            None => {}
                            Some(id) => devices
                                .iter()
                                .find(|device| device.id == id)
                                .ok_or_else(|| "audio route is no longer available".to_string())
                                .map(|_| ())?,
                        }
                        Ok(())
                    }
                    crate::actions::AudioOperation::SetStreamVolume { stream_id, value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("stream volume must be between 0 and 1".into());
                        }
                        let exists = graph
                            .playback_streams
                            .iter()
                            .chain(&graph.record_streams)
                            .any(|stream| stream.id == stream_id);
                        if !exists {
                            return Err("audio stream is no longer available".into());
                        }
                        Ok(())
                    }
                    crate::actions::AudioOperation::SetStreamMuted { stream_id, muted } => {
                        let exists = graph
                            .playback_streams
                            .iter()
                            .chain(&graph.record_streams)
                            .any(|stream| stream.id == stream_id);
                        if !exists {
                            return Err("audio stream is no longer available".into());
                        }
                        let _ = muted;
                        Ok(())
                    }
                    crate::actions::AudioOperation::ToggleDiagnostics
                    | crate::actions::AudioOperation::TestChannel { .. }
                    | crate::actions::AudioOperation::ToggleMicrophoneMeter => Ok(()),
                    crate::actions::AudioOperation::SelectOutput { device_id } => {
                        graph.sink_by_id(device_id).ok_or_else(|| {
                            "audio output device is no longer available".to_string()
                        })?;
                        Ok(())
                    }
                    crate::actions::AudioOperation::SelectInput { device_id } => {
                        graph
                            .sources
                            .iter()
                            .find(|device| device.id == device_id)
                            .ok_or_else(|| {
                                "audio input device is no longer available".to_string()
                            })?;
                        Ok(())
                    }
                    crate::actions::AudioOperation::RoutePlayback {
                        stream_id,
                        device_id,
                    } => {
                        if !graph
                            .playback_streams
                            .iter()
                            .any(|stream| stream.id == stream_id)
                        {
                            return Err("playback stream is no longer available".into());
                        }
                        graph.sink_by_id(device_id).ok_or_else(|| {
                            "audio output device is no longer available".to_string()
                        })?;
                        Ok(())
                    }
                    crate::actions::AudioOperation::RouteRecording {
                        stream_id,
                        device_id,
                    } => {
                        if !graph
                            .record_streams
                            .iter()
                            .any(|stream| stream.id == stream_id)
                        {
                            return Err("recording stream is no longer available".into());
                        }
                        graph
                            .sources
                            .iter()
                            .find(|device| device.id == device_id)
                            .ok_or_else(|| {
                                "audio input device is no longer available".to_string()
                            })?;
                        Ok(())
                    }
                };
                validated?;
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Audio(operation)));
                Ok(())
            }
            crate::actions::DISPLAY_OPERATION_TYPE => {
                let operation: crate::actions::DisplayOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Displays);
                let snapshot = settings_store::hardware::DisplayHardwareSnapshot::load();
                let output = |connector: &str| {
                    snapshot
                        .as_ref()
                        .ok_or_else(|| "live display capabilities are unavailable".to_string())?
                        .outputs
                        .iter()
                        .find(|output| output.descriptor.connector == connector)
                        .ok_or_else(|| "display output is no longer available".to_string())
                };
                let validated: Result<(), String> = match operation.clone() {
                    crate::actions::DisplayOperation::SetBrightness { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("display brightness must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetAutoBrightness { .. }
                    | crate::actions::DisplayOperation::SetNightLight { .. }
                    | crate::actions::DisplayOperation::SetNightLightSchedule { .. } => Ok(()),
                    crate::actions::DisplayOperation::SetNightLightTemperature { label } => {
                        if !["Subtle · 4500 K", "Warm · 3700 K", "Very warm · 2800 K"]
                            .contains(&label.as_str())
                        {
                            return Err("unsupported Night Light warmth".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetScale { label } => {
                        if !["100%", "125%", "150%", "175%", "200%"].contains(&label.as_str()) {
                            return Err("unsupported display scale".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetOutputBrightness { connector, value } => {
                        let _ = output(&connector)?;
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("monitor brightness must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetEnabled { connector, .. }
                    | crate::actions::DisplayOperation::SetPrimary { connector, .. } => {
                        let _ = output(&connector)?;
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetOutputScale { connector, label } => {
                        let _ = output(&connector)?;
                        if !["100%", "125%", "150%", "175%", "200%"].contains(&label.as_str()) {
                            return Err("unsupported output scale".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetPosition { connector, label } => {
                        let _ = output(&connector)?;
                        if label.len() > 160 {
                            return Err("display position label is too long".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetMirror { connector, label } => {
                        let _ = output(&connector)?;
                        if label.len() > 160 {
                            return Err("display mirror label is too long".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetModeLabel { connector, label } => {
                        let output = output(&connector)?;
                        if !output
                            .descriptor
                            .modes
                            .iter()
                            .any(|mode| mode.label() == label)
                        {
                            return Err("display mode is no longer supported by that output".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetTransformLabel { connector, label } => {
                        let _ = output(&connector)?;
                        if ![
                            "Landscape",
                            "Portrait right",
                            "Upside down",
                            "Portrait left",
                        ]
                        .contains(&label.as_str())
                        {
                            return Err("unsupported display orientation".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetColorProfileLabel { connector, label } => {
                        let output = output(&connector)?;
                        let valid = label == "System default"
                            || output
                                .color_profile_labels
                                .iter()
                                .any(|(id, name)| label == format!("{name} · {id}"));
                        if !valid {
                            return Err(
                                "colour profile is no longer available for that output".into()
                            );
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetMode { connector, mode } => {
                        let output = output(&connector)?;
                        if !output.descriptor.modes.contains(&mode) {
                            return Err("display mode is no longer supported by that output".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetTransform { connector, .. } => {
                        let _ = output(&connector)?;
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetHdr { connector, enabled } => {
                        let output = output(&connector)?;
                        if !output.descriptor.hdr {
                            return Err("HDR is not available for that output".into());
                        }
                        let _ = enabled;
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetSdrBrightness { connector, value } => {
                        let output = output(&connector)?;
                        if !output.descriptor.hdr {
                            return Err("HDR is not available for that output".into());
                        }
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("SDR brightness must be between 0 and 1".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetVrr { connector, enabled } => {
                        let output = output(&connector)?;
                        if !output.descriptor.vrr {
                            return Err(
                                "variable refresh rate is not available for that output".into()
                            );
                        }
                        let _ = enabled;
                        Ok(())
                    }
                    crate::actions::DisplayOperation::SetColorProfile { connector, profile } => {
                        let output = output(&connector)?;
                        if let Some(profile) = &profile
                            && !output.descriptor.color_profiles.contains(profile)
                        {
                            return Err(
                                "colour profile is no longer available for that output".into()
                            );
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::MoveOutput { connector, x, y } => {
                        let _ = output(&connector)?;
                        let bound = crate::actions::MAX_DISPLAY_COORDINATE;
                        if !(-bound..=bound).contains(&x) || !(-bound..=bound).contains(&y) {
                            return Err("display position is outside the desktop".into());
                        }
                        Ok(())
                    }
                    crate::actions::DisplayOperation::ForgetDisplayTool { ref executable } => {
                        if settings_store::display_tools::TrustedDisplayTools::load()
                            .tools
                            .iter()
                            .any(|tool| tool.executable == *executable)
                        {
                            Ok(())
                        } else {
                            Err("that display tool is not trusted".into())
                        }
                    }
                    crate::actions::DisplayOperation::Identify
                    | crate::actions::DisplayOperation::Apply
                    | crate::actions::DisplayOperation::Confirm
                    | crate::actions::DisplayOperation::Revert => Ok(()),
                };
                validated?;
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Display(operation)));
                Ok(())
            }
            _ => return Ok(None),
        };
        accepted?;
        let token = crate::actions::local_operation_token(type_id, payload)?;
        self.local_operation_status
            .set(crate::actions::LocalOperationStatus::Running {
                token: token.clone(),
            });
        self.local_operation_token = Some(token);
        // The process-local command has only been admitted. The selected pane
        // reports the actual terminal result through `poll_resource_operation`.
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl Widget for SettingsApp {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

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
        ) {
            if widget.test_id() == Some("settings.search") {
                let review = widget.text_review();
                *search = Some(serde_json::json!({
                    "rect": rect(widget.get_rect()),
                    "focused": widget.is_focused(),
                    "caret_byte": review.as_ref().map(|(caret, _)| *caret),
                    "text": review.map(|(_, text)| text),
                }));
            }
            if widget.name() == "SidebarRow"
                && let Some(category) = Category::ALL
                    .iter()
                    .find(|category| widget.test_id() == Some(category.label().as_str()))
            {
                categories.push(category.slug());
            }
            widget.for_each_child(&mut |child| inspect(child, search, categories));
        }
        let (state, path) = ui::doc::document_ref(state, path);
        let mut search = None;
        let mut categories = Vec::new();
        inspect(self, &mut search, &mut categories);
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
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            crate::actions::OPEN_PANE_OPERATION_TYPE,
            crate::actions::ACCOUNT_OPERATION_TYPE,
            crate::actions::NETWORK_OPERATION_TYPE,
            crate::actions::AUDIO_OPERATION_TYPE,
            crate::actions::DISPLAY_OPERATION_TYPE,
        ]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Background work an effect started finishes whether or not its pane
        // is open.
        self.updates.poll();
        self.storage.poll();
        self.printers.poll();
        self.speakers.poll();
        self.device_mesh.poll();
        // Sidebar selection and search are local presentation state. They do
        // not create CRDT click flags, draft text, collaboration traffic or
        // undo entries. The canonical resource `settings.section:<slug>/open`
        // still reaches the same `select` method through its View operation.
        if let Some(category) = self.sidebar_selection.with(|request| *request) {
            self.sidebar_selection.set(None);
            self.select(category);
        }
        let query = self.search_state.with(Clone::clone);
        if query != self.search_query {
            self.search_query = query;
            self.structure_dirty = true;
            // Filtering changes only navigation. Reconstructing the active
            // pane here would discard its transient state and reseed its
            // document bindings on every search character.
            let nav = sidebar::build(self.selected, &self.search_query, &self.sidebar_selection);
            if let Some(shell) = &mut self.shell {
                shell.set_nav(nav);
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
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        // `new_with_children` only fails for a child `NodeId` from a
        // different tree, which never happens here — see
        // `ui::widget::TaffyTreeExt`. Fall back to the child directly rather
        // than panic, on the off chance that ever changes.
        let node = taffy
            .new_with_children(style, &[shell_node])
            .unwrap_or(shell_node);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        // `taffy.layout(...)` only errors for a `NodeId` from a different
        // tree, which never happens here — see `ui::widget::TaffyTreeExt`.
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = vello::kurbo::Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        if let Some(shell) = &mut self.shell {
            shell.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
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
        if child && self.selected == Category::ExternalAccounts {
            self.refresh_account_resources();
        }
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.shell
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

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = self.admit_resource_operation_with_document(
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
        cancellation_requested: bool,
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.local_operation == Some(invocation) {
            // These contracts explicitly advertise `cancellable: false`.
            // Once the pane has admitted a system command it may be touching
            // firmware, boot configuration, or durable input preferences, so
            // a cancellation request cannot honestly stop or cancel history.
            let _ = cancellation_requested;
            let terminal = self.local_operation_status.get();
            match terminal {
                crate::actions::LocalOperationStatus::Succeeded { token, message }
                    if self.local_operation_token.as_deref() == Some(token.as_str()) =>
                {
                    self.local_operation = None;
                    self.local_operation_token = None;
                    self.local_operation_status
                        .set(crate::actions::LocalOperationStatus::Idle);
                    return Some(tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::Value::String(message)),
                            resources: Vec::new(),
                        },
                    });
                }
                crate::actions::LocalOperationStatus::Failed { token, message }
                    if self.local_operation_token.as_deref() == Some(token.as_str()) =>
                {
                    self.local_operation = None;
                    self.local_operation_token = None;
                    self.local_operation_status
                        .set(crate::actions::LocalOperationStatus::Idle);
                    return Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "device_local_operation_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
                _ => return None,
            }
        }
        if self
            .account_probe
            .as_ref()
            .is_some_and(|(id, _)| *id == invocation)
        {
            if cancellation_requested {
                self.account_probe = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("connection check cancelled".into()),
                });
            }
            let result = self
                .account_probe
                .as_ref()
                .and_then(|(_, receiver)| receiver.try_recv().ok());
            if let Some(result) = result {
                self.account_probe = None;
                return Some(match result {
                    Ok(message) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::Value::String(message)),
                            resources: Vec::new(),
                        },
                    },
                    Err(message) => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "account_connection_failed".into(),
                            message,
                            details: None,
                            retryable: true,
                        },
                    },
                });
            }
            return None;
        }
        self.shell.as_mut().and_then(|shell| {
            shell.poll_resource_operation(invocation, cancellation_requested, doc, path)
        })
    }
}

impl ui::search::capability::SearchSource for SettingsApp {
    /// Fold the settings panes into the unified Ctrl+K palette. Every result
    /// addresses the pane resource's exported `open` View operation, alongside
    /// the collaborative setting-change actions those resources own.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for SettingsApp {
    /// The sections of this device's hardware and services, with the local
    /// operations their controls use, and the account inventory.
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        crate::actions::live_device_resources(doc, self.account_resource_inventory())
    }
    /// Resolve one of them by id when an invocation names it.
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        self.publish_resource(doc, id)
    }
}

impl SettingsApp {
    #[allow(
        clippy::too_many_lines,
        reason = "local account and presentation operation routing stays auditable in one dispatch point"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Result<bool, String> {
        if type_id == crate::actions::ACCOUNT_OPERATION_TYPE {
            let operation: crate::actions::AccountOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            self.select(Category::ExternalAccounts);
            return match (kind, operation) {
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::Add { kind },
                ) => {
                    let kind = crate::accounts::AccountKind::from_slug(&kind)
                        .ok_or_else(|| "unknown account kind".to_string())?;
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Add(kind)));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::Edit { id },
                ) => {
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Edit(id)));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::Reauthenticate { id },
                ) => {
                    self.account_commands.set(Some(
                        crate::panes::accounts::AccountViewCommand::Reauthenticate(id),
                    ));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::Effect,
                    crate::actions::AccountOperation::SubmitForm,
                ) => {
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Submit));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::CancelForm,
                ) => {
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Cancel));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::Effect,
                    crate::actions::AccountOperation::SubmitReauthentication,
                ) => {
                    self.account_commands.set(Some(
                        crate::panes::accounts::AccountViewCommand::SubmitReauthentication,
                    ));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::CancelReauthentication,
                ) => {
                    self.account_commands.set(Some(
                        crate::panes::accounts::AccountViewCommand::CancelReauthentication,
                    ));
                    Ok(true)
                }
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::Open { id },
                ) => {
                    // Accounts have no separate read-only detail surface. Open
                    // the exact account's settings form instead of discarding
                    // the canonical id and landing on a generic list.
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Edit(id)));
                    Ok(true)
                }
                _ => Err("operation kind does not match account operation".into()),
            };
        }
        if kind != tilo_actions::OperationKind::View
            || type_id != crate::actions::OPEN_PANE_OPERATION_TYPE
        {
            return Ok(false);
        }
        let slug: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let category = Category::ALL
            .iter()
            .copied()
            .find(|category| category.slug() == slug)
            .ok_or_else(|| "Settings pane is no longer available".to_string())?;
        self.select(category);
        Ok(true)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "central router keeps validation beside dispatch"
    )]
    fn admit_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if matches!(
            type_id,
            crate::actions::NETWORK_OPERATION_TYPE
                | crate::actions::AUDIO_OPERATION_TYPE
                | crate::actions::DISPLAY_OPERATION_TYPE
        ) && kind == tilo_actions::OperationKind::Effect
        {
            let start = self.start_local_device_operation(kind, type_id, payload, doc, path)?;
            if matches!(start, Some(ui::widget::ResourceOperationStart::Accepted)) {
                self.local_operation = Some(invocation);
            }
            return Ok(start);
        }
        if type_id != crate::actions::ACCOUNT_OPERATION_TYPE {
            let op = ui::widget::ResourceOperation::local(invocation, kind, type_id, payload);
            if let Some(started) = ui::widget::start_in_children(self, &op, doc, path)? {
                return Ok(Some(started));
            }
            return self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                });
        }
        let operation: crate::actions::AccountOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        self.select(Category::ExternalAccounts);
        match (kind, operation) {
            (
                tilo_actions::OperationKind::View,
                operation @ (crate::actions::AccountOperation::Add { .. }
                | crate::actions::AccountOperation::Open { .. }
                | crate::actions::AccountOperation::Edit { .. }
                | crate::actions::AccountOperation::Reauthenticate { .. }
                | crate::actions::AccountOperation::CancelForm
                | crate::actions::AccountOperation::CancelReauthentication),
            ) => {
                self.answer_resource_operation(
                    kind,
                    type_id,
                    &serde_json::to_vec(&operation).map_err(|error| error.to_string())?,
                    doc,
                    path,
                )?;
            }
            (
                tilo_actions::OperationKind::Effect,
                operation @ (crate::actions::AccountOperation::SubmitForm
                | crate::actions::AccountOperation::SubmitReauthentication),
            ) => {
                self.answer_resource_operation(
                    kind,
                    type_id,
                    &serde_json::to_vec(&operation).map_err(|error| error.to_string())?,
                    doc,
                    path,
                )?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::AccountOperation::Remove { id },
            ) => {
                let stored = self
                    .account_root
                    .store()
                    .get(&accounts::AccountId::from(id.clone()))
                    .cloned()
                    .ok_or_else(|| "account no longer exists".to_string())?;
                let account_kind =
                    crate::accounts::AccountKind::from_type_id(stored.type_id.as_str())
                        .ok_or_else(|| "unsupported account type".to_string())?;
                crate::panes::accounts::AccountsPane::remove_account(
                    &self.account_root,
                    &self.account_root.custodian(),
                    account_kind,
                    &id,
                )?;
                self.refresh_account_resources();
                self.refresh_shell_children();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::AccountOperation::Test { id },
            ) => {
                if self.account_probe.is_some() {
                    return Err("an account connection check is already running".into());
                }
                let account_id = accounts::AccountId::from(id);
                let account = self
                    .account_root
                    .store()
                    .get(&account_id)
                    .cloned()
                    .ok_or_else(|| "account no longer exists".to_string())?;
                let secret = self.account_root.vault().get(&account_id).ok_or_else(|| {
                    "this account has no saved credential; sign in again first".to_string()
                })?;
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                if crate::accounts::AccountKind::from_type_id(account.type_id.as_str())
                    .is_some_and(crate::accounts::AccountKind::is_cloud_storage)
                {
                    // A cloud grant is verified by asking the provider for a
                    // fresh access token, which custody also hands off.
                    let custodian = self.account_root.custodian();
                    std::thread::spawn(move || {
                        let _ = sender.send(
                            custodian
                                .publish(&account_id, true)
                                .map(|()| "The provider accepted the saved sign-in".to_string())
                                .map_err(|error| error.to_string()),
                        );
                    });
                } else {
                    let tester = std::sync::Arc::clone(&self.account_tester);
                    std::thread::spawn(move || {
                        let _ = sender.send(tester.test(&account, &secret));
                    });
                }
                self.account_probe = Some((invocation, receiver));
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            _ => return Err("operation kind does not match account operation".into()),
        }
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
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

    /// Hardware, network, input and Bluetooth effects are local lifecycles.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn hardware_and_input_effects_stay_off_the_collaborative_document() {
        // Case `hardware_effect_admission_never_mutates_the_collaborative_document`.
        {
            use std::collections::BTreeSet;

            let config = crate::prefs::test_env::ConfigGuard::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let cases = vec![
                (
                    crate::actions::NETWORK_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::NetworkOperation::Refresh).unwrap(),
                ),
                (
                    crate::actions::AUDIO_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::AudioOperation::SetOutputVolume {
                        value: 0.5,
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::DISPLAY_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::DisplayOperation::SetBrightness {
                        value: 0.5,
                    })
                    .unwrap(),
                ),
            ];
            let mut covered = BTreeSet::new();
            for (type_id, payload) in cases {
                let heads = doc.get_heads();
                assert!(matches!(
                    app.start_local_device_operation(
                        tilo_actions::OperationKind::Effect,
                        type_id,
                        &payload,
                        &mut doc,
                        &root,
                    )
                    .expect("bounded local command is admitted"),
                    Some(ui::widget::ResourceOperationStart::Accepted)
                ));
                assert_eq!(doc.get_heads(), heads, "{type_id} touched Automerge");
                covered.insert(type_id);
                app.local_pane_command.set(None);
                app.local_operation_status
                    .set(crate::actions::LocalOperationStatus::Idle);
                app.local_operation_token = None;
            }

            let heads = doc.get_heads();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::ACCOUNT_OPERATION_TYPE,
                        &serde_json::to_vec(&crate::actions::AccountOperation::SubmitForm).unwrap()
                    ),
                    &mut doc,
                    &root
                )
                .expect("account form command is admitted"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(doc.get_heads(), heads, "account command touched Automerge");
            covered.insert(crate::actions::ACCOUNT_OPERATION_TYPE);

            assert_eq!(
                covered,
                crate::actions::DEVICE_LOCAL_OPERATION_TYPES
                    .iter()
                    .copied()
                    .collect(),
                "the unchanged-heads matrix must cover the complete closed set"
            );
        }

        // Case `network_refresh_is_a_local_lifecycle_not_a_replicated_setting_action`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let heads = doc.get_heads();
            let operation = crate::actions::NetworkOperation::Refresh;
            let payload = serde_json::to_vec(&operation).expect("network operation serializes");
            let invocation = tilo_actions::InvocationId::new();
            let start = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::NETWORK_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut doc,
                    &root,
                )
                .expect("refresh admission is local and bounded");
            assert!(matches!(
                start,
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert_eq!(app.selected(), Category::Network);
            assert_eq!(
                doc.get_heads(),
                heads,
                "local effects must not touch Automerge"
            );
            assert_eq!(
                app.local_pane_command.get(),
                Some(crate::actions::LocalPaneCommand::Network(operation.clone()))
            );
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &root)
                    .is_none()
            );

            app.local_operation_status
                .set(crate::actions::LocalOperationStatus::Succeeded {
                    token: crate::actions::local_operation_token_for(
                        crate::actions::NETWORK_OPERATION_TYPE,
                        &operation,
                    ),
                    message: "refreshed".into(),
                });
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &root),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }
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
                heading.y0 >= back.y1,
                "the destination heading begins below the back band, so the back arrow remains obvious"
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
            let app = config.app().with_width_hint(1440.0);
            let app =
                layout_at_with_theme(app, 1440.0, 900.0, &ui::Theme::dark().with_text_scale(2.0));
            assert!(
                !app.is_narrow(),
                "1440px still admits a readable split at 200% text"
            );
            let sidebar = app
                .shell
                .as_ref()
                .expect("layout builds the shell")
                .sidebar_panel_rect();
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
        // Case `search_query_is_local_and_flags_rebuild`.
        {
            use automerge::ReadDoc;
            let config = crate::prefs::test_env::ScratchConfig::new();

            let app = config.app();
            let mut app = layout_at(app, 1200.0, 760.0);
            assert_eq!(app.search_query(), "", "starts with no query");

            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            app.search_state.set("night light".to_string());
            app.sync(&mut doc, &root);
            assert_eq!(app.search_query(), "night light");
            assert!(
                app.take_structure_dirty(),
                "a new query must flag a rebuild so the filtered sidebar recomposes"
            );
            // Re-syncing the same query is a no-op (no spurious rebuild).
            app.sync(&mut doc, &root);
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
            app.sync(&mut doc, &root);
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
            assert_eq!(
                names,
                vec!["TextInput", "Container", "Container"],
                "shell chrome is search + nav + content — no title/app-name header widget"
            );
            // And the resolved layout is the wide split.
            assert_eq!(shell.layout(), ui::widget::ShellLayout::Wide);
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
