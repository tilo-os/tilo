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
use ui::widget::{AppShell, Column, Container, NarrowScreen, Scrollable, TextInput, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// At or above this allocated width (logical px) the sidebar docks beside the
/// content; below it the shell switches to the narrow list-vs-content screens.
/// Sized so a typical settings window is docked while a narrow / split-screen
/// placement gets the phone layout.
pub const NAV_BREAKPOINT: f64 = 720.0;

pub struct SettingsApp {
    /// Currently-selected sidebar category.
    selected: Category,
    /// `true` once a selection (or other composition-affecting change) has
    /// happened and the host must re-run `build_layout`. Drained by
    /// `take_structure_dirty`.
    structure_dirty: bool,
    /// Which narrow screen a `--shots` / headless capture pins the shell to —
    /// the live `ui::App` event loop that would otherwise drive the navigation
    /// is absent. `None` (the runtime default) collapses to the CONTENT screen,
    /// matching the shared shell's behaviour; a capture can force the list or
    /// content screen explicitly. Applied when the shell is created.
    narrow_shot_screen: Option<NarrowScreen>,
    /// Seed width hint so the very first offscreen frame resolves the correct
    /// wide/narrow layout. `None` lets the shell decide from its own rect.
    width_hint: Option<f32>,
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
    /// Secret-free snapshot for resource enumeration. Refreshes at account
    /// mutation boundaries, never during the per-frame resource query.
    account_resources: Vec<accounts::Account>,
    account_commands: ui::LocalState<Option<crate::panes::accounts::AccountViewCommand>>,
    device_mesh_commands: ui::LocalState<Option<crate::actions::DeviceMeshOperation>>,
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    local_operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    local_operation: Option<tilo_actions::InvocationId>,
    local_operation_token: Option<String>,
    /// Deterministic, secret-free inventory used only by visual acceptance
    /// captures. Live construction always leaves this `None`.
    account_directory_override: Option<crate::accounts::AccountDirectory>,
    account_form_override: Option<crate::accounts::AccountKind>,
    /// Use stable, host-independent hardware fixtures in visual captures.
    /// Live construction always leaves this false.
    deterministic_hardware_for_shot: bool,
    account_probe: Option<(
        tilo_actions::InvocationId,
        std::sync::mpsc::Receiver<Result<String, String>>,
    )>,
    rect: vello::kurbo::Rect,
    /// The reusable app-shell, created lazily on the first `build_layout` and
    /// persisted across rebuilds so its collapse/overlay state survives a
    /// category switch. Its nav + content are swapped on selection.
    shell: Option<AppShell>,
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsApp {
    pub fn new() -> Self {
        if accounts::config_root().is_some()
            && let Err(error) = crate::accounts::recover_account_mutation()
        {
            tracing::warn!(target: "tilo::settings", %error, "could not recover interrupted account mutation");
        }
        Self {
            selected: Category::DEFAULT,
            structure_dirty: false,
            narrow_shot_screen: None,
            width_hint: None,
            search_query: String::new(),
            sidebar_selection: ui::LocalState::new(None),
            search_state: ui::LocalState::new(String::new()),
            search_provider: crate::search_provider::SettingsSearchProvider::new(),
            account_resources: accounts::AccountStore::load().all().to_vec(),
            account_commands: ui::LocalState::new(None),
            device_mesh_commands: ui::LocalState::new(None),
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            local_operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            local_operation: None,
            local_operation_token: None,
            account_directory_override: None,
            account_form_override: None,
            deterministic_hardware_for_shot: false,
            account_probe: None,
            rect: vello::kurbo::Rect::ZERO,
            shell: None,
        }
    }

    /// Construct with a specific category pre-selected. Used by tests and
    /// future deep-linking (`settings://network`).
    pub fn with_selected(category: Category) -> Self {
        let mut s = Self::new();
        s.selected = category;
        s
    }

    /// Seed the responsive layout with a known width before the first frame —
    /// the offscreen `--shots` renderer calls this so the cold screenshot
    /// resolves the correct wide/narrow layout immediately.
    #[must_use]
    pub fn with_width_hint(mut self, width: f32) -> Self {
        self.width_hint = Some(width);
        self
    }

    #[must_use]
    pub(crate) fn with_account_directory_for_shot(
        mut self,
        directory: crate::accounts::AccountDirectory,
    ) -> Self {
        self.account_directory_override = Some(directory);
        self
    }

    #[must_use]
    pub(crate) fn with_account_form_for_shot(mut self, kind: crate::accounts::AccountKind) -> Self {
        self.account_form_override = Some(kind);
        self
    }

    #[must_use]
    #[doc(hidden)]
    pub fn with_deterministic_hardware_for_shot(mut self) -> Self {
        self.deterministic_hardware_for_shot = true;
        self
    }

    /// Pin a narrow `--shots` capture to the content screen (with the back
    /// arrow). The narrow layout already collapses to the content by default,
    /// so this only matters for a capture that overrides an earlier seed. Has
    /// no visible effect when the layout resolves to wide.
    #[must_use]
    pub fn with_narrow_content_shot(mut self) -> Self {
        self.narrow_shot_screen = Some(NarrowScreen::Content);
        self
    }

    /// Pin a narrow `--shots` capture to the full-screen category list — the
    /// back-tap-away screen a collapse no longer defaults to. Without the live
    /// event loop nothing drives the navigation, so this seeds the list screen
    /// for its own snapshot. Has no visible effect when the layout is wide.
    #[must_use]
    pub fn with_narrow_list_shot(mut self) -> Self {
        self.narrow_shot_screen = Some(NarrowScreen::List);
        self
    }

    pub fn selected(&self) -> Category {
        self.selected
    }

    pub(crate) fn account_resource_inventory(&self) -> &[accounts::Account] {
        &self.account_resources
    }

    fn refresh_account_resources(&mut self) {
        self.account_resources = accounts::AccountStore::load().all().to_vec();
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
                .is_some_and(|w| f64::from(w) < NAV_BREAKPOINT),
        }
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

    /// Pre-seed a search query so the deterministic `--shots` render shows the
    /// filtered sidebar (there is no live event loop to type into the field).
    #[must_use]
    pub fn with_search_shot(mut self, query: &str) -> Self {
        self.search_query = query.to_string();
        self.search_state.set(query.to_string());
        self
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
                .with_placeholder("Search settings")
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
        let inner = Column::new()
            .with_intrinsic_height()
            .push(crate::widgets::vgap(14.0))
            .push(crate::widgets::page_heading(
                self.selected.label(),
                self.selected.subtitle(),
            ))
            .push(crate::widgets::vgap(28.0))
            .push(body);
        // Account forms and cards are more legible as a column on large
        // desktop displays. Operational panes deliberately keep their full
        // detail width for device tables, channel controls, and bindings.
        let content: Box<dyn Widget> = if self.uses_readable_content_measure() {
            Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(1040.0)
                    .with_child(Box::new(inner)),
            )
        } else {
            Box::new(inner)
        };
        let scroll = Scrollable::new_vertical().with_child(content);
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
    }

    /// Account forms are intentionally capped to a readable line length on
    /// large monitors while staying fluid on narrow screens. Other panes keep
    /// their full width until each has an equivalent content-measure review.
    fn uses_readable_content_measure(&self) -> bool {
        self.selected == Category::ExternalAccounts
    }

    /// Dispatch to the per-category pane body.
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            Category::Network => Box::new(
                panes::network::NetworkPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Bluetooth => Box::new(
                panes::bluetooth::BluetoothPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::DeviceMesh => Box::new(
                panes::device_mesh::DeviceMeshPane::new()
                    .with_command_state(&self.device_mesh_commands)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Sound => Box::new(
                panes::audio::AudioPane::new().with_operation_status(&self.local_operation_status),
            ),
            Category::Displays if self.deterministic_hardware_for_shot => {
                Box::new(panes::display::DisplayPane::with_dispatcher(
                    crate::dispatcher::dispatcher_or_real(None),
                    0.62,
                ))
            }
            Category::Displays => Box::new(
                panes::display::DisplayPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::MouseKeyboard => Box::new(
                panes::mouse_keyboard::MouseKeyboardPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::KeyboardShortcuts => panes::keyboard_shortcuts::build(),
            Category::TimeDate => panes::time_date::build(),
            Category::Users => Box::new(
                panes::users::UsersPane::new().with_operation_status(&self.local_operation_status),
            ),
            Category::Printers => Box::new(
                panes::printers::PrintersPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Power => Box::new(
                panes::power::PowerPane::new().with_operation_status(&self.local_operation_status),
            ),
            Category::Appearance => panes::appearance::build(),
            Category::Accessibility => panes::accessibility::build(),
            Category::Notifications => panes::notifications::build(),
            Category::DefaultApps => panes::personal_apps::build(),
            Category::Storage => Box::new(
                panes::storage::StoragePane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::ExternalAccounts => {
                let mut pane = self
                    .account_directory_override
                    .as_ref()
                    .map_or_else(panes::accounts::AccountsPane::new, |directory| {
                        panes::accounts::AccountsPane::from_dir(directory.clone(), false)
                    });
                if let Some(kind) = self.account_form_override {
                    pane = pane.with_open_form_for_shot(kind);
                }
                Box::new(
                    pane.with_command_state(&self.account_commands)
                        .with_account_tester(std::sync::Arc::clone(&self.account_tester)),
                )
            }
            Category::AgentModels => Box::new(
                panes::agent_models::AgentModelsPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::RegionFormats => panes::region::build(),
            Category::Permissions => panes::permissions::build(),
            Category::Updates => Box::new(
                panes::updates::UpdatesPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::About => panes::about::build(),
        }
    }

    /// Create the persistent [`AppShell`] with the current nav + content +
    /// search and the seeded shot state.
    fn build_shell(&self) -> AppShell {
        let nav = sidebar::build(self.selected, &self.search_query, &self.sidebar_selection);
        let content = self.build_content();
        // A narrow (phone) layout keeps the selected category's CONTENT on
        // screen — the category list is one back-tap away, never the default a
        // collapse lands on. A `--shots` capture can pin a specific narrow
        // screen; otherwise the shell collapses to the content.
        let narrow_screen = self.narrow_shot_screen.unwrap_or(NarrowScreen::Content);
        let mut shell = AppShell::new(nav, content)
            .with_search(self.build_search_field())
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

    /// Route a canonical device-local operation into the exact one-shot
    /// document latch consumed by its pane.  The pane remains the sole owner
    /// of the privileged command, its availability checks, progress UI, and
    /// final success/error notice; this adapter deliberately never performs a
    /// second system call.
    #[allow(
        clippy::too_many_lines,
        reason = "the three device-local protocols are kept together to audit their pane-latch routing"
    )]
    fn start_local_device_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use automerge::transaction::Transactable;

        if kind != tilo_actions::OperationKind::Effect {
            return Err("device-local settings commands must be effect operations".into());
        }
        if self.local_operation.is_some() {
            return Err("another device-local Settings operation is still running".into());
        }
        let accepted = match type_id {
            crate::actions::UPDATE_OPERATION_TYPE => {
                let operation: crate::actions::UpdateOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Updates);
                match operation {
                    crate::actions::UpdateOperation::Check => {
                        doc.put(path, panes::updates::CHECK_KEY, true)
                    }
                    crate::actions::UpdateOperation::Restart => {
                        doc.put(path, panes::updates::RESTART_KEY, true)
                    }
                    crate::actions::UpdateOperation::ReviewRollback => {
                        doc.put(path, panes::updates::ROLLBACK_KEY, true)
                    }
                    crate::actions::UpdateOperation::Rollback => {
                        doc.put(path, panes::updates::ROLLBACK_CONFIRM_KEY, true)
                    }
                    crate::actions::UpdateOperation::RefreshFirmware => {
                        doc.put(path, panes::updates::FIRMWARE_REFRESH_KEY, true)
                    }
                    crate::actions::UpdateOperation::InstallFirmware => {
                        doc.put(path, panes::updates::FIRMWARE_INSTALL_KEY, true)
                    }
                    crate::actions::UpdateOperation::SetAutomatic { enabled } => {
                        doc.put(path, panes::updates::AUTO_KEY, enabled)
                    }
                    crate::actions::UpdateOperation::SetSchedule { schedule } => {
                        if !["System schedule", "Daily", "Weekly"].contains(&schedule.as_str()) {
                            return Err("unsupported update schedule".into());
                        }
                        doc.put(path, panes::updates::SCHEDULE_KEY, schedule)
                    }
                    crate::actions::UpdateOperation::SetMetered { allowed } => {
                        doc.put(path, panes::updates::METERED_KEY, allowed)
                    }
                }
            }
            crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE => {
                let operation: crate::actions::KeyboardSourceOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::MouseKeyboard);
                let mut prefs = crate::prefs::SystemPrefs::load().input;
                prefs.normalise_keyboard_sources();
                match operation {
                    crate::actions::KeyboardSourceOperation::Add { label } => {
                        let available =
                            crate::input_sources::installed_catalog()
                                .iter()
                                .any(|entry| {
                                    entry.label == label
                                        && !prefs.keyboard_layouts.contains(&entry.id)
                                });
                        if !available
                            || prefs.keyboard_layouts.len()
                                >= settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
                        {
                            return Err("keyboard input source is no longer available".into());
                        }
                        doc.put(path, panes::mouse_keyboard::ADD_SOURCE_PICKER_KEY, label)
                            .and_then(|()| {
                                doc.put(path, panes::mouse_keyboard::ADD_SOURCE_KEY, true)
                            })
                    }
                    crate::actions::KeyboardSourceOperation::MoveUp { index } => {
                        if index == 0 || index >= prefs.keyboard_layouts.len() {
                            return Err("keyboard input source can no longer move up".into());
                        }
                        doc.put(path, format!("input.source.{index}.up"), true)
                    }
                    crate::actions::KeyboardSourceOperation::MoveDown { index } => {
                        if index + 1 >= prefs.keyboard_layouts.len() {
                            return Err("keyboard input source can no longer move down".into());
                        }
                        doc.put(path, format!("input.source.{index}.down"), true)
                    }
                    crate::actions::KeyboardSourceOperation::Remove { index } => {
                        if prefs.keyboard_layouts.len() <= 1
                            || index >= prefs.keyboard_layouts.len()
                        {
                            return Err("keyboard input source can no longer be removed".into());
                        }
                        doc.put(path, format!("input.source.{index}.remove"), true)
                    }
                }
            }
            crate::actions::POWER_OPERATION_TYPE => {
                let operation: crate::actions::PowerOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Power);
                match operation {
                    crate::actions::PowerOperation::SetChargeLimit { percent } => {
                        let available = os_actions::charge_capability()
                            .is_some_and(|capability| capability.choices.contains(&percent));
                        if !available {
                            return Err(
                                "battery charge-cap control is unavailable on this device".into()
                            );
                        }
                        doc.put(path, panes::power::CHARGE_LIMIT_KEY, format!("{percent}%"))
                    }
                    crate::actions::PowerOperation::SetProfile { profile } => {
                        if !["Power Saver", "Balanced", "Performance"].contains(&profile.as_str()) {
                            return Err("unsupported power profile".into());
                        }
                        doc.put(path, panes::power::PROFILE_KEY, profile)
                    }
                    crate::actions::PowerOperation::SetAutoSuspend { enabled } => {
                        doc.put(path, panes::power::AUTO_SUSPEND_KEY, enabled)
                    }
                    crate::actions::PowerOperation::SetBlankTimeout { label } => {
                        if ![
                            "1 minute",
                            "5 minutes",
                            "10 minutes",
                            "15 minutes",
                            "30 minutes",
                            "Never",
                        ]
                        .contains(&label.as_str())
                        {
                            return Err("unsupported screen blank timeout".into());
                        }
                        doc.put(path, panes::power::BLANK_KEY, label)
                    }
                    crate::actions::PowerOperation::SetSuspendTimeout { profile, label } => {
                        if ![
                            "5 minutes",
                            "10 minutes",
                            "15 minutes",
                            "30 minutes",
                            "45 minutes",
                            "1 hour",
                            "Never",
                        ]
                        .contains(&label.as_str())
                        {
                            return Err("unsupported suspend timeout".into());
                        }
                        let key = match profile.as_str() {
                            "default" => panes::power::SUSPEND_KEY,
                            "saver" => panes::power::SAVER_SUSPEND_KEY,
                            "balanced" => panes::power::BALANCED_SUSPEND_KEY,
                            "performance" => panes::power::PERFORMANCE_SUSPEND_KEY,
                            _ => return Err("unsupported power profile timeout".into()),
                        };
                        doc.put(path, key, label)
                    }
                    crate::actions::PowerOperation::SetLidAction { action } => {
                        if !["Suspend", "Lock screen", "Do nothing"].contains(&action.as_str()) {
                            return Err("unsupported power action".into());
                        }
                        doc.put(path, panes::power::LID_KEY, action)
                    }
                    crate::actions::PowerOperation::SetPowerKeyAction { action } => {
                        if !["Suspend", "Lock screen", "Do nothing"].contains(&action.as_str()) {
                            return Err("unsupported power action".into());
                        }
                        doc.put(path, panes::power::POWER_KEY_KEY, action)
                    }
                    crate::actions::PowerOperation::SetCriticalAction { action } => {
                        if !["Suspend", "Lock screen", "Do nothing"].contains(&action.as_str()) {
                            return Err("unsupported power action".into());
                        }
                        doc.put(path, panes::power::CRITICAL_KEY, action)
                    }
                }
            }
            crate::actions::NETWORK_OPERATION_TYPE => {
                let operation: crate::actions::NetworkOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Network);
                match operation {
                    crate::actions::NetworkOperation::Refresh => {
                        doc.put(path, panes::network::REFRESH_KEY, true)
                    }
                    crate::actions::NetworkOperation::SetWifiEnabled { enabled } => {
                        doc.put(path, panes::network::WIFI_ENABLED_KEY, enabled)
                    }
                    crate::actions::NetworkOperation::SetWiredEnabled { enabled } => {
                        let wired = os_actions::get_wired()?;
                        if wired.device.is_none() {
                            return Err("wired networking is unavailable on this device".into());
                        }
                        doc.put(path, panes::network::WIRED_ENABLED_KEY, enabled)
                    }
                    crate::actions::NetworkOperation::SetAirplaneMode { enabled } => {
                        doc.put(path, panes::network::AIRPLANE_KEY, enabled)
                    }
                    crate::actions::NetworkOperation::SetHotspot { enabled } => {
                        doc.put(path, panes::network::HOTSPOT_KEY, enabled)
                    }
                    crate::actions::NetworkOperation::SetMobileBroadband { enabled } => {
                        doc.put(path, panes::network::MOBILE_KEY, enabled)
                    }
                    crate::actions::NetworkOperation::RunDiagnostics => {
                        doc.put(path, panes::network::DIAGNOSTICS_KEY, true)
                    }
                }
            }
            crate::actions::BLUETOOTH_OPERATION_TYPE => {
                let operation: crate::actions::BluetoothOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Bluetooth);
                match operation {
                    crate::actions::BluetoothOperation::SetPowered { enabled } => {
                        doc.put(path, panes::bluetooth::BT_ENABLED_KEY, enabled)
                    }
                    crate::actions::BluetoothOperation::StartScan => {
                        doc.put(path, panes::bluetooth::SCAN_KEY, true)
                    }
                    crate::actions::BluetoothOperation::Refresh => {
                        doc.put(path, panes::bluetooth::REFRESH_KEY, true)
                    }
                    operation @ (crate::actions::BluetoothOperation::Pair { .. }
                    | crate::actions::BluetoothOperation::Connect { .. }
                    | crate::actions::BluetoothOperation::Disconnect { .. }
                    | crate::actions::BluetoothOperation::Forget { .. }) => {
                        let (device_path, action) = match operation {
                            crate::actions::BluetoothOperation::Pair { path } => (path, "pair"),
                            crate::actions::BluetoothOperation::Connect { path } => {
                                (path, "connect")
                            }
                            crate::actions::BluetoothOperation::Disconnect { path } => {
                                (path, "disconnect")
                            }
                            crate::actions::BluetoothOperation::Forget { path } => (path, "forget"),
                            _ => return Err("unsupported Bluetooth operation".into()),
                        };
                        let device = os_actions::bluez::devices()?
                            .into_iter()
                            .find(|device| device.path == device_path)
                            .ok_or_else(|| "Bluetooth device is no longer available".to_string())?;
                        let allowed = match action {
                            "pair" => !device.paired,
                            "connect" => device.paired && !device.connected,
                            "disconnect" => device.connected,
                            "forget" => device.paired,
                            _ => false,
                        };
                        if !allowed {
                            return Err(
                                "Bluetooth device state changed; refresh and try again".into()
                            );
                        }
                        doc.put(
                            path,
                            panes::bluetooth::device_action_key(&device_path, action),
                            true,
                        )
                    }
                }
            }
            crate::actions::STORAGE_OPERATION_TYPE => {
                let operation: crate::actions::StorageOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Storage);
                match operation {
                    crate::actions::StorageOperation::CleanupCaches => {
                        doc.put(path, panes::storage::CLEAN_KEY, true)
                    }
                    crate::actions::StorageOperation::FreeSyncedFiles => {
                        doc.put(path, panes::storage::SYNC_FREE_KEY, true)
                    }
                    crate::actions::StorageOperation::OpenDisks => {
                        doc.put(path, panes::storage::DISKS_KEY, true)
                    }
                    crate::actions::StorageOperation::OpenBackups => {
                        doc.put(path, panes::storage::BACKUPS_KEY, true)
                    }
                }
            }
            crate::actions::AUDIO_OPERATION_TYPE => {
                let operation: crate::actions::AudioOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                let graph = media::audio::enumerate();
                self.select(Category::Sound);
                match operation {
                    crate::actions::AudioOperation::SetOutputVolume { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("output volume must be between 0 and 1".into());
                        }
                        doc.put(path, panes::audio::VOLUME_KEY, value)
                    }
                    crate::actions::AudioOperation::SetOutputMuted { muted } => {
                        doc.put(path, panes::audio::MUTED_KEY, muted)
                    }
                    crate::actions::AudioOperation::SetInputVolume { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("input volume must be between 0 and 1".into());
                        }
                        doc.put(path, panes::audio::INPUT_VOLUME_KEY, value)
                    }
                    crate::actions::AudioOperation::SetBalance { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("output balance must be between 0 and 1".into());
                        }
                        doc.put(path, panes::audio::BALANCE_KEY, value)
                    }
                    crate::actions::AudioOperation::SetMono { enabled } => {
                        doc.put(path, panes::audio::MONO_KEY, enabled)
                    }
                    crate::actions::AudioOperation::SelectOutput { device_id } => {
                        let device = graph.sink_by_id(device_id).ok_or_else(|| {
                            "audio output device is no longer available".to_string()
                        })?;
                        doc.put(
                            path,
                            panes::audio::OUTPUT_DEVICE_KEY,
                            device.description.as_str(),
                        )
                    }
                    crate::actions::AudioOperation::SelectInput { device_id } => {
                        let device = graph
                            .sources
                            .iter()
                            .find(|device| device.id == device_id)
                            .ok_or_else(|| {
                                "audio input device is no longer available".to_string()
                            })?;
                        doc.put(
                            path,
                            panes::audio::INPUT_DEVICE_KEY,
                            device.description.as_str(),
                        )
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
                        let device = graph.sink_by_id(device_id).ok_or_else(|| {
                            "audio output device is no longer available".to_string()
                        })?;
                        doc.put(
                            path,
                            panes::audio::stream_sink_key(stream_id),
                            device.description.as_str(),
                        )
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
                        let device = graph
                            .sources
                            .iter()
                            .find(|device| device.id == device_id)
                            .ok_or_else(|| {
                                "audio input device is no longer available".to_string()
                            })?;
                        doc.put(
                            path,
                            panes::audio::stream_sink_key(stream_id),
                            device.description.as_str(),
                        )
                    }
                }
            }
            crate::actions::DISPLAY_OPERATION_TYPE => {
                let operation: crate::actions::DisplayOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Displays);
                let snapshot = settings_store::hardware::DisplayHardwareSnapshot::load()
                    .ok_or_else(|| "live display capabilities are unavailable".to_string())?;
                let output = |connector: &str| {
                    snapshot
                        .outputs
                        .iter()
                        .find(|output| output.descriptor.connector == connector)
                        .ok_or_else(|| "display output is no longer available".to_string())
                };
                match operation {
                    crate::actions::DisplayOperation::SetBrightness { value } => {
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("display brightness must be between 0 and 1".into());
                        }
                        doc.put(path, panes::display::BRIGHTNESS_KEY, value)
                    }
                    crate::actions::DisplayOperation::SetAutoBrightness { enabled } => {
                        doc.put(path, panes::display::AUTO_BRIGHTNESS_KEY, enabled)
                    }
                    crate::actions::DisplayOperation::SetScale { label } => {
                        if !["100%", "125%", "150%", "175%", "200%"].contains(&label.as_str()) {
                            return Err("unsupported display scale".into());
                        }
                        doc.put(path, panes::display::DISPLAY_SCALE_KEY, label)
                    }
                    crate::actions::DisplayOperation::SetEnabled { connector, enabled } => {
                        let _ = output(&connector)?;
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "enabled"),
                            enabled,
                        )
                    }
                    crate::actions::DisplayOperation::SetPrimary { connector, primary } => {
                        let _ = output(&connector)?;
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "primary"),
                            primary,
                        )
                    }
                    crate::actions::DisplayOperation::SetOutputScale { connector, label } => {
                        let _ = output(&connector)?;
                        if !["100%", "125%", "150%", "175%", "200%"].contains(&label.as_str()) {
                            return Err("unsupported output scale".into());
                        }
                        doc.put(path, panes::display::output_key(&connector, "scale"), label)
                    }
                    crate::actions::DisplayOperation::SetPosition { connector, label } => {
                        let _ = output(&connector)?;
                        if label.len() > 160 {
                            return Err("display position label is too long".into());
                        }
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "position"),
                            label,
                        )
                    }
                    crate::actions::DisplayOperation::SetMirror { connector, label } => {
                        let _ = output(&connector)?;
                        if label.len() > 160 {
                            return Err("display mirror label is too long".into());
                        }
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "mirror"),
                            label,
                        )
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
                        doc.put(path, panes::display::output_key(&connector, "mode"), label)
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
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "transform"),
                            label,
                        )
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
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "color_profile"),
                            label,
                        )
                    }
                    crate::actions::DisplayOperation::SetMode { connector, mode } => {
                        let output = output(&connector)?;
                        if !output.descriptor.modes.contains(&mode) {
                            return Err("display mode is no longer supported by that output".into());
                        }
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "mode"),
                            mode.label(),
                        )
                    }
                    crate::actions::DisplayOperation::SetTransform {
                        connector,
                        transform,
                    } => {
                        let _ = output(&connector)?;
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "transform"),
                            panes::display::transform_label(transform),
                        )
                    }
                    crate::actions::DisplayOperation::SetHdr { connector, enabled } => {
                        let output = output(&connector)?;
                        if !output.descriptor.hdr {
                            return Err("HDR is not available for that output".into());
                        }
                        doc.put(path, panes::display::output_key(&connector, "hdr"), enabled)
                    }
                    crate::actions::DisplayOperation::SetVrr { connector, enabled } => {
                        let output = output(&connector)?;
                        if !output.descriptor.vrr {
                            return Err(
                                "variable refresh rate is not available for that output".into()
                            );
                        }
                        doc.put(path, panes::display::output_key(&connector, "vrr"), enabled)
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
                        let label = profile.as_ref().map_or_else(
                            || "System default".to_string(),
                            |profile| {
                                output.color_profile_labels.get(profile).map_or_else(
                                    || profile.clone(),
                                    |name| format!("{name} · {profile}"),
                                )
                            },
                        );
                        doc.put(
                            path,
                            panes::display::output_key(&connector, "color_profile"),
                            label,
                        )
                    }
                    crate::actions::DisplayOperation::Apply => {
                        doc.put(path, panes::display::APPLY_DISPLAY_KEY, true)
                    }
                    crate::actions::DisplayOperation::Confirm => {
                        doc.put(path, panes::display::KEEP_DISPLAY_KEY, true)
                    }
                    crate::actions::DisplayOperation::Revert => {
                        doc.put(path, panes::display::REVERT_DISPLAY_KEY, true)
                    }
                }
            }
            crate::actions::PRINTER_OPERATION_TYPE => {
                let operation: crate::actions::PrinterOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Printers);
                match operation {
                    crate::actions::PrinterOperation::Refresh => {
                        doc.put(path, panes::printers::REFRESH_KEY, true)
                    }
                    crate::actions::PrinterOperation::Discover => {
                        doc.put(path, panes::printers::SCAN_KEY, true)
                    }
                    crate::actions::PrinterOperation::SetDefault { name } => {
                        if !os_actions::printing::queues()
                            .unwrap_or_default()
                            .iter()
                            .any(|q| q.name == name)
                        {
                            return Err("printer queue is no longer available".into());
                        }
                        doc.put(
                            path,
                            panes::printers::queue_action_key("default", &name),
                            true,
                        )
                    }
                    crate::actions::PrinterOperation::SetEnabled { name, enabled } => {
                        let queue = os_actions::printing::queues()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|q| q.name == name)
                            .ok_or_else(|| "printer queue is no longer available".to_string())?;
                        let allowed =
                            (queue.state == os_actions::printing::QueueState::Paused) == enabled;
                        if !allowed {
                            return Err("printer queue state changed; refresh and try again".into());
                        }
                        doc.put(
                            path,
                            panes::printers::queue_action_key("enabled", &name),
                            true,
                        )
                    }
                    crate::actions::PrinterOperation::Remove { name } => {
                        if !os_actions::printing::queues()
                            .unwrap_or_default()
                            .iter()
                            .any(|q| q.name == name)
                        {
                            return Err("printer queue is no longer available".into());
                        }
                        doc.put(
                            path,
                            panes::printers::queue_action_key("remove", &name),
                            true,
                        )
                    }
                    crate::actions::PrinterOperation::CancelJob { id } => {
                        if !os_actions::printing::jobs()
                            .unwrap_or_default()
                            .iter()
                            .any(|job| job.id == id)
                        {
                            return Err("print job is no longer available".into());
                        }
                        doc.put(path, panes::printers::job_action_key("cancel", &id), true)
                    }
                    crate::actions::PrinterOperation::RetryJob { id } => {
                        if !os_actions::printing::jobs()
                            .unwrap_or_default()
                            .iter()
                            .any(|job| job.id == id)
                        {
                            return Err("print job is no longer available".into());
                        }
                        doc.put(path, panes::printers::job_action_key("retry", &id), true)
                    }
                }
            }
            crate::actions::USER_OPERATION_TYPE => {
                let operation: crate::actions::UserOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Users);
                let user = |username: &str| {
                    crate::panes::users::users_for_display()
                        .into_iter()
                        .find(|user| user.username == username && !user.current)
                        .ok_or_else(|| "user account is no longer available".to_string())
                };
                match operation {
                    crate::actions::UserOperation::SetAdmin { username, admin } => {
                        let live = user(&username)?;
                        if live.admin == admin {
                            return Err("user role already has that value".into());
                        }
                        doc.put(path, format!("settings.users.role.{username}"), true)
                    }
                    crate::actions::UserOperation::SetAutologin { username, enabled } => {
                        let live = user(&username)?;
                        if live.automatic_login == enabled {
                            return Err("automatic-login state already has that value".into());
                        }
                        doc.put(path, format!("settings.users.autologin.{username}"), true)
                    }
                    crate::actions::UserOperation::Remove {
                        username,
                        remove_files,
                    } => {
                        let _ = user(&username)?;
                        let key = if remove_files {
                            format!("settings.users.confirm_remove.{username}")
                        } else {
                            format!("settings.users.confirm_remove_keep.{username}")
                        };
                        doc.put(path, key, true)
                    }
                }
            }
            crate::actions::AGENT_MODEL_OPERATION_TYPE => {
                let operation: crate::actions::AgentModelOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::AgentModels);
                let snapshot = panes::agent_models::live_snapshot()
                    .map_err(|error| format!("local model manager is unavailable: {error}"))?;
                let find_model = |id: &str| {
                    snapshot
                        .models
                        .iter()
                        .find(|model| model.package.id == id)
                        .ok_or_else(|| "local model is no longer available".to_string())
                };
                match operation {
                    crate::actions::AgentModelOperation::StageAccelerator => {
                        let allowed = snapshot.accelerator_setup.as_ref().is_some_and(|setup| {
                            matches!(
                                setup.status,
                                panes::agent_models::AcceleratorSetupState::Available
                                    | panes::agent_models::AcceleratorSetupState::Error { .. }
                            )
                        });
                        if !allowed {
                            return Err("accelerator setup is not currently available".into());
                        }
                        doc.put(path, panes::agent_models::ACCELERATOR_SETUP_KEY, true)
                    }
                    crate::actions::AgentModelOperation::Install { id, revision } => {
                        let package = snapshot.recommendation.as_ref().ok_or_else(|| {
                            "a signed recommended model is not currently available".to_string()
                        })?;
                        let accelerator_ready = snapshot
                            .accelerator_setup
                            .as_ref()
                            .is_none_or(|setup| !setup.status.blocks_model_setup());
                        if package.id != id
                            || package.revision != revision
                            || snapshot.hardware.fit
                                == panes::agent_models::HardwareFit::Unsupported
                            || !accelerator_ready
                        {
                            return Err("recommended model or hardware capability changed; refresh and review it again".into());
                        }
                        doc.put(path, panes::agent_models::CONFIRM_KEY, true)
                    }
                    crate::actions::AgentModelOperation::Start { id } => {
                        let model = find_model(&id)?;
                        if !matches!(
                            &model.phase,
                            panes::agent_models::LocalModelPhase::Ready { running: false, .. }
                        ) {
                            return Err("local model state changed; refresh and try again".into());
                        }
                        doc.put(
                            path,
                            panes::agent_models::model_key(
                                &id,
                                panes::agent_models::ModelAction::Start,
                            ),
                            true,
                        )
                    }
                    crate::actions::AgentModelOperation::Stop { id } => {
                        let model = find_model(&id)?;
                        if !matches!(
                            &model.phase,
                            panes::agent_models::LocalModelPhase::Ready { running: true, .. }
                        ) {
                            return Err("local model state changed; refresh and try again".into());
                        }
                        doc.put(
                            path,
                            panes::agent_models::model_key(
                                &id,
                                panes::agent_models::ModelAction::Stop,
                            ),
                            true,
                        )
                    }
                    crate::actions::AgentModelOperation::Retry { id, revision } => {
                        let model = find_model(&id)?;
                        if model.package.revision != revision
                            || !matches!(
                                &model.phase,
                                panes::agent_models::LocalModelPhase::Error { .. }
                            )
                        {
                            return Err("local model state changed; refresh and try again".into());
                        }
                        doc.put(
                            path,
                            panes::agent_models::model_key(
                                &id,
                                panes::agent_models::ModelAction::Retry,
                            ),
                            true,
                        )
                    }
                    crate::actions::AgentModelOperation::Remove { id } => {
                        let model = find_model(&id)?;
                        if !matches!(
                            &model.phase,
                            panes::agent_models::LocalModelPhase::Ready { .. }
                                | panes::agent_models::LocalModelPhase::Error { .. }
                        ) {
                            return Err(
                                "local model cannot be removed while its state is changing".into(),
                            );
                        }
                        doc.put(
                            path,
                            panes::agent_models::model_key(
                                &id,
                                panes::agent_models::ModelAction::Remove,
                            ),
                            true,
                        )
                    }
                }
            }
            crate::actions::DEVICE_MESH_OPERATION_TYPE => {
                let operation: crate::actions::DeviceMeshOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::DeviceMesh);
                self.device_mesh_commands.set(Some(operation));
                Ok(())
            }
            _ => return Ok(None),
        };
        accepted.map_err(|error| error.to_string())?;
        let token = crate::actions::local_operation_token(type_id, payload)?;
        self.local_operation_status
            .set(crate::actions::LocalOperationStatus::Running {
                token: token.clone(),
            });
        self.local_operation_token = Some(token);
        // The latch has only been admitted. The selected pane reports the
        // actual terminal result through `poll_resource_operation`.
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl Widget for SettingsApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            crate::actions::OPEN_PANE_OPERATION_TYPE,
            crate::actions::ACCOUNT_OPERATION_TYPE,
            crate::actions::UPDATE_OPERATION_TYPE,
            crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE,
            crate::actions::POWER_OPERATION_TYPE,
            crate::actions::NETWORK_OPERATION_TYPE,
            crate::actions::BLUETOOTH_OPERATION_TYPE,
            crate::actions::STORAGE_OPERATION_TYPE,
            crate::actions::AUDIO_OPERATION_TYPE,
            crate::actions::DISPLAY_OPERATION_TYPE,
            crate::actions::PRINTER_OPERATION_TYPE,
            crate::actions::USER_OPERATION_TYPE,
            crate::actions::AGENT_MODEL_OPERATION_TYPE,
            crate::actions::DEVICE_MESH_OPERATION_TYPE,
            crate::actions::AGENT_PROVIDER_OPERATION_TYPE,
        ]
    }

    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
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
        // survives category switches (which rebuild the tree).
        if self.shell.is_none() {
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
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        if let (Ok(children), Some(shell)) = (taffy.children(node_id), &mut self.shell)
            && let Some(&shell_node) = children.first()
        {
            shell.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                shell_node,
                taffy,
            );
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

    /// Publish the settings resource tree to the shell's one Ctrl+K palette,
    /// delegating to the [`ResourceProvider`](tilo_actions::ResourceProvider) impl
    /// in [`crate::actions`]. Level-triggered: recomputed each frame from the
    /// shared document.
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        <Self as tilo_actions::ResourceProvider>::resources(self, doc)
    }

    /// Resolve canonical Settings resources at activation time as well as
    /// publishing the eager palette snapshot above.  Search results retain
    /// only stable action addresses, so this is the authoritative path that
    /// re-checks an address against the current resource surface before the
    /// action door opens it.
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        <Self as tilo_actions::ResourceProvider>::resource(self, doc, id)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "local account and presentation operation routing stays auditable in one dispatch point"
    )]
    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Result<bool, String> {
        use agent_provider_config::{ProviderConfig, ProviderProfile, ProviderProtocol};

        if type_id == crate::actions::UPDATE_OPERATION_TYPE
            && kind == tilo_actions::OperationKind::View
        {
            let operation: crate::actions::UpdateOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            if matches!(operation, crate::actions::UpdateOperation::ReviewRollback) {
                self.select(Category::Updates);
                return Ok(true);
            }
            return Err("operation kind does not match update operation".into());
        }
        if type_id == crate::actions::AGENT_PROVIDER_OPERATION_TYPE {
            if kind != tilo_actions::OperationKind::Effect {
                return Err("provider configuration requires an effect operation".into());
            }
            let operation: crate::actions::AgentProviderOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            let protocol = |label: &str| match label {
                "anthropic" => Ok(ProviderProtocol::Anthropic),
                "openai-compatible" => Ok(ProviderProtocol::OpenAiCompatible),
                _ => Err("unsupported provider protocol".to_string()),
            };
            let mut config = ProviderConfig::load();
            match operation {
                crate::actions::AgentProviderOperation::Add { protocol: label } => {
                    config.upsert(ProviderProfile::hosted(protocol(&label)?));
                }
                crate::actions::AgentProviderOperation::SetProtocol {
                    id,
                    protocol: label,
                } => {
                    let profile = config
                        .profiles
                        .iter_mut()
                        .find(|profile| profile.id == id)
                        .ok_or_else(|| "provider is no longer available".to_string())?;
                    profile.protocol = protocol(&label)?;
                }
                crate::actions::AgentProviderOperation::SetEnabled { id, enabled } => {
                    let profile = config
                        .profiles
                        .iter_mut()
                        .find(|profile| profile.id == id)
                        .ok_or_else(|| "provider is no longer available".to_string())?;
                    profile.enabled = enabled;
                }
                crate::actions::AgentProviderOperation::Remove { id } => {
                    if !config.profiles.iter().any(|profile| profile.id == id) {
                        return Err("provider is no longer available".into());
                    }
                    tilo_model_gateway::remove_api_key(&id)
                        .map_err(|error| format!("provider vault key was not removed: {error}"))?;
                    let _ = config.remove(&id);
                }
                crate::actions::AgentProviderOperation::SetDefault { id } => {
                    let available = config.profiles.iter().any(|profile| {
                        profile.id == id && profile.enabled && profile.default_model.is_some()
                    });
                    if !available {
                        return Err("provider cannot currently be selected as default".into());
                    }
                    config.default_profile = Some(id);
                }
                crate::actions::AgentProviderOperation::SetDetails {
                    id,
                    label,
                    endpoint,
                    model,
                } => {
                    if label.trim().is_empty() || endpoint.trim().is_empty() {
                        return Err("provider name and endpoint are required".into());
                    }
                    let profile = config
                        .profiles
                        .iter_mut()
                        .find(|profile| profile.id == id)
                        .ok_or_else(|| "provider is no longer available".to_string())?;
                    profile.label = label;
                    profile.endpoint = endpoint;
                    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
                        if !profile.models.iter().any(|candidate| candidate.id == model) {
                            profile
                                .models
                                .push(agent_provider_config::ModelDescriptor::new(model.clone()));
                        }
                        profile.default_model = Some(model);
                    }
                }
            }
            config
                .save()
                .map_err(|error| format!("provider settings were not saved: {error}"))?;
            self.select(Category::AgentModels);
            return Ok(true);
        }
        if type_id == crate::actions::ACCOUNT_OPERATION_TYPE {
            let operation: crate::actions::AccountOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            self.select(Category::ExternalAccounts);
            return match (kind, operation) {
                (
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::Add { kind },
                ) => {
                    let kind = match kind.as_str() {
                        "mail" => crate::accounts::AccountKind::Mail,
                        "caldav" => crate::accounts::AccountKind::CalDav,
                        "webdav" => crate::accounts::AccountKind::WebDav,
                        "nextcloud" => crate::accounts::AccountKind::Nextcloud,
                        _ => return Err("unknown account kind".into()),
                    };
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
    fn start_resource_operation_with_document(
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
            crate::actions::UPDATE_OPERATION_TYPE
                | crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE
                | crate::actions::POWER_OPERATION_TYPE
                | crate::actions::NETWORK_OPERATION_TYPE
                | crate::actions::BLUETOOTH_OPERATION_TYPE
                | crate::actions::STORAGE_OPERATION_TYPE
                | crate::actions::AUDIO_OPERATION_TYPE
                | crate::actions::DISPLAY_OPERATION_TYPE
                | crate::actions::PRINTER_OPERATION_TYPE
                | crate::actions::USER_OPERATION_TYPE
                | crate::actions::AGENT_MODEL_OPERATION_TYPE
                | crate::actions::DEVICE_MESH_OPERATION_TYPE
        ) && kind == tilo_actions::OperationKind::Effect
        {
            let start = self.start_local_device_operation(kind, type_id, payload, doc, path)?;
            if matches!(start, Some(ui::widget::ResourceOperationStart::Accepted)) {
                self.local_operation = Some(invocation);
            }
            return Ok(start);
        }
        if type_id != crate::actions::ACCOUNT_OPERATION_TYPE {
            return self.start_resource_operation(invocation, kind, type_id, payload, doc, path);
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
                self.invoke_resource_operation(
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
                self.invoke_resource_operation(
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
                let stored = accounts::AccountStore::load()
                    .get(&accounts::AccountId::from(id.clone()))
                    .cloned()
                    .ok_or_else(|| "account no longer exists".to_string())?;
                let account_kind = match stored.type_id.as_str() {
                    "email" => crate::accounts::AccountKind::Mail,
                    "caldav" => crate::accounts::AccountKind::CalDav,
                    "webdav" => crate::accounts::AccountKind::WebDav,
                    "nextcloud" => crate::accounts::AccountKind::Nextcloud,
                    _ => return Err("unsupported account type".into()),
                };
                crate::panes::accounts::AccountsPane::remove_account(account_kind, &id)?;
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
                let account = accounts::AccountStore::load()
                    .get(&account_id)
                    .cloned()
                    .ok_or_else(|| "account no longer exists".to_string())?;
                let secret = accounts::SecretVault::open()
                    .get(&account_id)
                    .ok_or_else(|| {
                        "this account has no saved credential; sign in again first".to_string()
                    })?;
                let tester = std::sync::Arc::clone(&self.account_tester);
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let _ = sender.send(tester.test(&account, &secret));
                });
                self.account_probe = Some((invocation, receiver));
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            _ => return Err("operation kind does not match account operation".into()),
        }
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
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

    /// Fold the settings panes into the unified Ctrl+K palette. Every result
    /// addresses the pane resource's exported `open` View operation, alongside
    /// the collaborative setting-change actions those resources own.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }

    /// Bubble an on-screen control's dispatched-action undo bracket up from
    /// whichever pane produced it. A settings control lives deep under the shell
    /// (`AppShell → content → Scrollable → … → pane`), and the intermediate
    /// containers don't forward this rising-edge signal, so the root walks its
    /// subtree and returns the first pane's captured heads. The host records one
    /// undo boundary from it, so Ctrl+Z reverts the setting change.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        fn walk(w: &mut dyn Widget) -> Option<Vec<automerge::ChangeHash>> {
            if let Some(heads) = w.take_committed_action() {
                return Some(heads);
            }
            for child in w.children_mut() {
                if let Some(heads) = walk(child) {
                    return Some(heads);
                }
            }
            None
        }
        for child in self.children_mut() {
            if let Some(heads) = walk(child) {
                return Some(heads);
            }
        }
        None
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
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};

    fn layout_at(app: &mut SettingsApp, w: f32, h: f32) {
        layout_at_with_theme(app, w, h, &ui::Theme::dark());
    }

    fn layout_at_with_theme(app: &mut SettingsApp, w: f32, h: f32, theme: &ui::Theme) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy
            .new_with_children(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(w),
                        height: taffy::Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let measure_map = node_map.clone();
        let mut tr = ui::text_render::TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
    }

    #[test]
    fn new_starts_on_default_category() {
        assert_eq!(SettingsApp::new().selected(), Category::DEFAULT);
    }

    #[test]
    fn with_selected_overrides_default() {
        assert_eq!(
            SettingsApp::with_selected(Category::About).selected(),
            Category::About
        );
    }

    #[test]
    fn select_changes_category_and_flags_structure_dirty() {
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
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

    #[test]
    fn canonical_update_operation_is_accepted_not_completed_until_the_pane_reports() {
        use automerge::{ReadDoc, Value};

        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        let start = app
            .start_resource_operation_with_document(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::UPDATE_OPERATION_TYPE,
                &payload,
                &mut doc,
                &root,
            )
            .expect("the check operation is valid");
        assert!(matches!(
            start,
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert_eq!(app.selected(), Category::Updates);
        assert!(matches!(
            doc.get(&root, panes::updates::CHECK_KEY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
        ));
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &root)
                .is_none(),
            "latch admission must not create a successful history entry"
        );
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &crate::actions::UpdateOperation::Check,
                ),
                message: "checked".into(),
            });
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &root),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &root)
                .is_none(),
            "a local invocation emits exactly one terminal update"
        );
    }

    #[test]
    fn network_refresh_is_a_local_lifecycle_not_a_replicated_setting_action() {
        use automerge::{ReadDoc, Value};

        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let operation = crate::actions::NetworkOperation::Refresh;
        let payload = serde_json::to_vec(&operation).expect("network operation serializes");
        let invocation = tilo_actions::InvocationId::new();
        let start = app
            .start_resource_operation_with_document(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::NETWORK_OPERATION_TYPE,
                &payload,
                &mut doc,
                &root,
            )
            .expect("refresh admission is local and bounded");
        assert!(matches!(
            start,
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert_eq!(app.selected(), Category::Network);
        assert!(matches!(
            doc.get(&root, panes::network::REFRESH_KEY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
        ));
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

    #[test]
    fn bluetooth_refresh_waits_for_its_own_local_completion_token() {
        use automerge::{ReadDoc, Value};

        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let operation = crate::actions::BluetoothOperation::Refresh;
        let payload = serde_json::to_vec(&operation).expect("Bluetooth operation serializes");
        let invocation = tilo_actions::InvocationId::new();
        let start = app
            .start_resource_operation_with_document(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::BLUETOOTH_OPERATION_TYPE,
                &payload,
                &mut doc,
                &root,
            )
            .expect("Bluetooth refresh admission is valid");
        assert!(matches!(
            start,
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert_eq!(app.selected(), Category::Bluetooth);
        assert!(matches!(
            doc.get(&root, panes::bluetooth::REFRESH_KEY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
        ));
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::BLUETOOTH_OPERATION_TYPE,
                    &operation,
                ),
                message: "refreshed".into(),
            });
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &root),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn non_cancellable_local_operation_ignores_cancel_until_the_pane_finishes() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::UPDATE_OPERATION_TYPE,
            &payload,
            &mut doc,
            &root,
        )
        .unwrap();
        assert!(
            app.poll_resource_operation(invocation, true, &doc, &root)
                .is_none(),
            "cancellation cannot claim to stop an admitted system command"
        );
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &crate::actions::UpdateOperation::Check,
                ),
                message: "checked".into(),
            });
        assert!(matches!(
            app.poll_resource_operation(invocation, true, &doc, &root),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn unrelated_pane_status_cannot_finish_a_pending_local_operation() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::UPDATE_OPERATION_TYPE,
            &payload,
            &mut doc,
            &root,
        )
        .unwrap();
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::POWER_OPERATION_TYPE,
                    &crate::actions::PowerOperation::SetChargeLimit { percent: 80 },
                ),
                message: "unrelated UI change".into(),
            });
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &root)
                .is_none()
        );
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &crate::actions::UpdateOperation::Check,
                ),
                message: "checked".into(),
            });
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &root),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn overwritten_schedule_value_cannot_complete_the_original_invocation() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = tilo_actions::InvocationId::new();
        let daily = crate::actions::UpdateOperation::SetSchedule {
            schedule: "Daily".into(),
        };
        let payload = serde_json::to_vec(&daily).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::UPDATE_OPERATION_TYPE,
            &payload,
            &mut doc,
            &root,
        )
        .unwrap();
        let weekly = crate::actions::UpdateOperation::SetSchedule {
            schedule: "Weekly".into(),
        };
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &weekly,
                ),
                message: "weekly changed".into(),
            });
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &root)
                .is_none()
        );
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Succeeded {
                token: crate::actions::local_operation_token_for(
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &daily,
                ),
                message: "daily changed".into(),
            });
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &root),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn pane_reported_local_error_becomes_a_failed_terminal_update() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::UPDATE_OPERATION_TYPE,
            &payload,
            &mut doc,
            &root,
        )
        .unwrap();
        app.local_operation_status
            .set(crate::actions::LocalOperationStatus::Failed {
                token: crate::actions::local_operation_token_for(
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &crate::actions::UpdateOperation::Check,
                ),
                message: "offline".into(),
            });
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &root),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
    }

    #[test]
    fn local_operation_rejects_an_update_schedule_outside_the_panes_choices() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::SetSchedule {
            schedule: "Whenever".into(),
        })
        .unwrap();
        assert!(
            app.start_local_device_operation(
                tilo_actions::OperationKind::Effect,
                crate::actions::UPDATE_OPERATION_TYPE,
                &payload,
                &mut doc,
                &root,
            )
            .is_err()
        );
    }

    #[test]
    fn wide_layout_docks_the_sidebar() {
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
        assert!(!app.is_narrow(), "1200px should dock the sidebar");
        assert!(app.sidebar_open(), "docked sidebar is open by default");
    }

    #[test]
    fn narrow_layout_collapses_to_the_content_screen() {
        // A collapse to the narrow (phone) layout keeps the selected category's
        // CONTENT on screen — it never defaults to the category list. The list
        // is a back-tap away (see the back arrow), not the surface a collapse
        // lands on.
        let mut app = SettingsApp::new().with_width_hint(440.0);
        layout_at(&mut app, 440.0, 820.0);
        assert!(app.is_narrow(), "440px should resolve to the narrow layout");
        assert!(
            !app.sidebar_open(),
            "a collapse keeps the content — it must not default to the category list"
        );
        assert!(
            app.back_button_rect().width() > 0.0,
            "the content screen carries a back arrow to reach the category list"
        );
    }

    #[test]
    fn narrow_list_shot_shows_the_standalone_list_screen() {
        // The category list remains reachable (and capturable) as its own
        // full-screen screen — it is simply no longer the collapse default.
        let mut app = SettingsApp::new()
            .with_width_hint(440.0)
            .with_narrow_list_shot();
        layout_at(&mut app, 440.0, 820.0);
        assert!(app.is_narrow(), "440px should resolve to the narrow layout");
        assert!(
            app.sidebar_open(),
            "the list screen shows the standalone full-screen category list"
        );
        assert_eq!(
            app.back_button_rect(),
            vello::kurbo::Rect::ZERO,
            "the list screen has no back button"
        );
    }

    #[test]
    fn narrow_content_shot_drills_into_the_content_screen() {
        let mut app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(440.0)
            .with_narrow_content_shot();
        layout_at(&mut app, 440.0, 820.0);
        assert!(app.is_narrow());
        assert!(
            !app.sidebar_open(),
            "the content-screen shot shows the content full-screen (not the list)"
        );
        assert!(
            app.back_button_rect().width() > 0.0,
            "the content screen carries a back-arrow button"
        );
    }

    #[test]
    fn narrow_page_heading_explains_the_back_arrow_destination_without_overlap() {
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

        let selected = Category::RegionFormats;
        let mut app = SettingsApp::with_selected(selected)
            .with_width_hint(440.0)
            .with_narrow_content_shot();
        layout_at(&mut app, 440.0, 820.0);

        let mut heading = None;
        named_rect(&app, selected.label(), &mut heading);
        let heading = heading.expect("the content page names the selected destination");
        let back = app.back_button_rect();
        assert!(
            heading.y0 >= back.y1,
            "the destination heading begins below the back band, so the back arrow remains obvious"
        );
    }

    #[test]
    fn narrow_large_text_keeps_the_destination_in_the_initial_viewport() {
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

        let selected = Category::RegionFormats;
        let mut app = SettingsApp::with_selected(selected)
            .with_width_hint(390.0)
            .with_narrow_content_shot();
        layout_at_with_theme(
            &mut app,
            390.0,
            844.0,
            &ui::Theme::dark().with_text_scale(1.5),
        );

        let mut heading = None;
        named_rect(&app, selected.label(), &mut heading);
        let heading = heading.expect("the content page names the selected destination");
        assert!(
            heading.y0 < 844.0,
            "large text must retain the destination heading in the first viewport (y={})",
            heading.y0
        );
    }

    #[test]
    fn narrow_pick_navigates_list_to_content_and_back() {
        // Start on the list screen — the reachable back-tap state.
        let mut app = SettingsApp::new()
            .with_width_hint(440.0)
            .with_narrow_list_shot();
        layout_at(&mut app, 440.0, 820.0);
        assert!(app.sidebar_open(), "starts on the list screen");
        // Picking a category drills into its content screen.
        app.select(Category::Users);
        assert!(app.take_structure_dirty(), "the pick requests a rebuild");
        layout_at(&mut app, 440.0, 820.0);
        assert!(
            !app.sidebar_open(),
            "the pick navigated to the content screen"
        );
        assert!(
            app.back_button_rect().width() > 0.0,
            "a back arrow is present"
        );
    }

    #[test]
    fn search_query_is_local_and_flags_rebuild() {
        use automerge::ReadDoc;

        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
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

    #[test]
    fn seeded_search_shot_query_reaches_the_local_field() {
        let mut app = SettingsApp::with_selected(Category::Displays).with_search_shot("light");
        assert_eq!(app.search_query(), "light");
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert_eq!(
            app.search_query(),
            "light",
            "the local field must retain the requested capture filter"
        );
        assert_eq!(app.search_state.with(Clone::clone), "light");
        assert!(doc.get_heads().is_empty());
        layout_at(&mut app, 1200.0, 760.0);
    }

    #[test]
    fn wide_and_narrow_both_lay_out_without_panicking() {
        let mut wide = SettingsApp::with_selected(Category::ExternalAccounts);
        layout_at(&mut wide, 1200.0, 720.0);
        assert!(wide.get_rect().width() > 0.0);

        let mut narrow = SettingsApp::with_selected(Category::ExternalAccounts)
            .with_width_hint(440.0)
            .with_narrow_content_shot();
        layout_at(&mut narrow, 440.0, 820.0);
        assert!(narrow.get_rect().width() > 0.0);
    }

    /// The app must NOT draw its own title / app-name header — the OS window
    /// pill carries the name. The shell's chrome is exactly the search field,
    /// the nav list, and the content pane (no title element). We assert on the
    /// real composed tree: the shell's children are those three, in order.
    #[test]
    fn draws_no_app_name_header() {
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
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

    #[test]
    fn root_landmark_is_retained_across_responsive_shell_rebuilds() {
        let mut app = SettingsApp::with_selected(Category::Appearance);
        let wide = Widget::accessibility(&app).expect("Settings landmark");
        assert_eq!(wide.name.as_deref(), Some("Settings"));
        assert_eq!(wide.semantic_key.as_deref(), Some("settings.app"));

        layout_at(&mut app, 440.0, 820.0);
        let narrow = Widget::accessibility(&app).expect("Settings landmark survives narrow shell");
        assert_eq!(narrow.semantic_key, wide.semantic_key);
    }
}
