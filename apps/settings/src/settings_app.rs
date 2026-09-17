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
/// The toolkit's `compact_breakpoint` (HIG §9.6): a typical settings window is
/// docked while a narrow / split-screen placement gets the phone layout. At a
/// large-text preference the live breakpoint is this times the text scale
/// (see [`SettingsApp::build_shell`]).
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
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    local_operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    local_operation: Option<qos_actions::InvocationId>,
    local_operation_token: Option<String>,
    /// Deterministic, secret-free inventory used only by visual acceptance
    /// captures. Live construction always leaves this `None`.
    account_directory_override: Option<crate::accounts::AccountDirectory>,
    account_form_override: Option<crate::accounts::AccountKind>,
    /// Use stable, host-independent hardware fixtures in visual captures.
    /// Live construction always leaves this false.
    deterministic_hardware_for_shot: bool,
    account_probe: Option<(
        qos_actions::InvocationId,
        std::sync::mpsc::Receiver<Result<String, String>>,
    )>,
    rect: vello::kurbo::Rect,
    /// The reusable app-shell, created lazily on the first `build_layout` and
    /// persisted across rebuilds so its collapse/overlay state survives a
    /// category switch. Its nav + content are swapped on selection.
    shell: Option<AppShell>,
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
        if accounts::config_root().is_some()
            && let Err(error) = crate::accounts::recover_account_mutation()
        {
            tracing::warn!(target: "qos::settings", %error, "could not recover interrupted account mutation");
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
            shell_text_scale: 1.0,
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
                self.selected.label(),
                self.selected.subtitle(),
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

    /// Dispatch to the per-category pane body.
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            Category::Network => panes::network::build(),
            Category::Bluetooth => panes::bluetooth::build(),
            Category::DeviceMesh => panes::device_mesh::build(),
            Category::Sound => panes::sound::build(),
            Category::Displays if self.deterministic_hardware_for_shot => {
                Box::new(panes::display::DisplayPane::with_dispatcher(
                    crate::dispatcher::dispatcher_or_real(None),
                    0.62,
                ))
            }
            Category::Displays => panes::display::build(),
            Category::MouseKeyboard => Box::new(
                panes::mouse_keyboard::MouseKeyboardPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::KeyboardShortcuts => panes::keyboard_shortcuts::build(),
            Category::TimeDate => panes::time_date::build(),
            Category::Users => panes::users::build(),
            Category::Printers => panes::printers::build(),
            Category::Power => Box::new(
                panes::power::PowerPane::new().with_operation_status(&self.local_operation_status),
            ),
            Category::Appearance => panes::appearance::build(),
            Category::Accessibility => panes::accessibility::build(),
            Category::Notifications => panes::notifications::build(),
            Category::DefaultApps => panes::personal_apps::build(),
            Category::Storage => panes::storage::build(),
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
            Category::AgentModels => panes::agent_models::build(),
            Category::RegionFormats => panes::region::build_for_width(self.content_width_hint()),
            Category::Permissions => panes::permissions::build(),
            Category::Updates => Box::new(
                panes::updates::UpdatesPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::About => panes::about::build(),
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
        let narrow_screen = self.narrow_shot_screen.unwrap_or(NarrowScreen::Content);
        let mut shell = AppShell::new(nav, content)
            .with_search(self.build_search_field())
            .with_breakpoint(self.scaled_breakpoint())
            .with_sidebar_width(self.scaled_sidebar_width())
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
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use automerge::transaction::Transactable;

        if kind != qos_actions::OperationKind::Effect {
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
                }
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
    fn native_input_snapshot(
        &self,
        state: &automerge::AutoCommit,
        path: &automerge::ObjId,
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
                    .find(|category| widget.test_id() == Some(category.label()))
            {
                categories.push(category.slug());
            }
            widget.for_each_child(&mut |child| inspect(child, search, categories));
        }
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
                    && r[2].as_f64() > r[0].as_f64()
                    && r[3].as_f64() > r[1].as_f64()
            });
        Some(serde_json::json!({
            "app": "dev.qos.settings",
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
            crate::actions::UPDATE_OPERATION_TYPE,
            crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE,
            crate::actions::POWER_OPERATION_TYPE,
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
        if self.shell.is_none() || self.shell_text_scale.to_bits() != theme.text_scale.to_bits()
        {
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
    /// delegating to the [`ResourceProvider`](qos_actions::ResourceProvider) impl
    /// in [`crate::actions`]. Level-triggered: recomputed each frame from the
    /// shared document.
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Resolve canonical Settings resources at activation time as well as
    /// publishing the eager palette snapshot above.  Search results retain
    /// only stable action addresses, so this is the authoritative path that
    /// re-checks an address against the current resource surface before the
    /// action door opens it.
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resource(self, doc, id)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "local account and presentation operation routing stays auditable in one dispatch point"
    )]
    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Result<bool, String> {
        if type_id == crate::actions::UPDATE_OPERATION_TYPE
            && kind == qos_actions::OperationKind::View
        {
            let operation: crate::actions::UpdateOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            if matches!(operation, crate::actions::UpdateOperation::ReviewRollback) {
                self.select(Category::Updates);
                return Ok(true);
            }
            return Err("operation kind does not match update operation".into());
        }
        if type_id == crate::actions::ACCOUNT_OPERATION_TYPE {
            let operation: crate::actions::AccountOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            self.select(Category::ExternalAccounts);
            return match (kind, operation) {
                (
                    qos_actions::OperationKind::View,
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
                    qos_actions::OperationKind::View,
                    crate::actions::AccountOperation::Edit { id },
                ) => {
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Edit(id)));
                    Ok(true)
                }
                (
                    qos_actions::OperationKind::View,
                    crate::actions::AccountOperation::Reauthenticate { id },
                ) => {
                    self.account_commands.set(Some(
                        crate::panes::accounts::AccountViewCommand::Reauthenticate(id),
                    ));
                    Ok(true)
                }
                (
                    qos_actions::OperationKind::Effect,
                    crate::actions::AccountOperation::SubmitForm,
                ) => {
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Submit));
                    Ok(true)
                }
                (
                    qos_actions::OperationKind::View,
                    crate::actions::AccountOperation::CancelForm,
                ) => {
                    self.account_commands
                        .set(Some(crate::panes::accounts::AccountViewCommand::Cancel));
                    Ok(true)
                }
                (
                    qos_actions::OperationKind::Effect,
                    crate::actions::AccountOperation::SubmitReauthentication,
                ) => {
                    self.account_commands.set(Some(
                        crate::panes::accounts::AccountViewCommand::SubmitReauthentication,
                    ));
                    Ok(true)
                }
                (
                    qos_actions::OperationKind::View,
                    crate::actions::AccountOperation::CancelReauthentication,
                ) => {
                    self.account_commands.set(Some(
                        crate::panes::accounts::AccountViewCommand::CancelReauthentication,
                    ));
                    Ok(true)
                }
                (
                    qos_actions::OperationKind::View,
                    crate::actions::AccountOperation::Open { .. },
                ) => Ok(true),
                _ => Err("operation kind does not match account operation".into()),
            };
        }
        if kind != qos_actions::OperationKind::View
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

    fn start_resource_operation_with_document(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
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
        ) && kind == qos_actions::OperationKind::Effect
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
                qos_actions::OperationKind::View,
                operation @ (crate::actions::AccountOperation::Add { .. }
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
            (qos_actions::OperationKind::View, crate::actions::AccountOperation::Open { .. }) => {}
            (
                qos_actions::OperationKind::Effect,
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
                qos_actions::OperationKind::Effect,
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
            (qos_actions::OperationKind::Effect, crate::actions::AccountOperation::Test { id }) => {
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
            qos_actions::ActionOutput::default(),
        )))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        cancellation_requested: bool,
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
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
                    return Some(qos_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: qos_actions::ActionOutput {
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
                    return Some(qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
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
                return Some(qos_actions::InvocationUpdate::Cancelled {
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
                    Ok(message) => qos_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: qos_actions::ActionOutput {
                            value: Some(serde_json::Value::String(message)),
                            resources: Vec::new(),
                        },
                    },
                    Err(message) => qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
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
        let invocation = qos_actions::InvocationId::new();
        let start = app
            .start_resource_operation_with_document(
                invocation,
                qos_actions::OperationKind::Effect,
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
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &root)
                .is_none(),
            "a local invocation emits exactly one terminal update"
        );
    }

    #[test]
    fn non_cancellable_local_operation_ignores_cancel_until_the_pane_finishes() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = qos_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            qos_actions::OperationKind::Effect,
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
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn unrelated_pane_status_cannot_finish_a_pending_local_operation() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = qos_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            qos_actions::OperationKind::Effect,
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
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn overwritten_schedule_value_cannot_complete_the_original_invocation() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = qos_actions::InvocationId::new();
        let daily = crate::actions::UpdateOperation::SetSchedule {
            schedule: "Daily".into(),
        };
        let payload = serde_json::to_vec(&daily).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            qos_actions::OperationKind::Effect,
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
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn pane_reported_local_error_becomes_a_failed_terminal_update() {
        let mut app = SettingsApp::new();
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let invocation = qos_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
        app.start_resource_operation_with_document(
            invocation,
            qos_actions::OperationKind::Effect,
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
            Some(qos_actions::InvocationUpdate::Failed { .. })
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
                qos_actions::OperationKind::Effect,
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

#[cfg(test)]
#[path = "native_observer_tests.rs"]
mod native_observer_tests;
