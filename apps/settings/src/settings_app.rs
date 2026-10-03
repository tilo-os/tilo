//! `SettingsApp` — the top-level settings hub.
//!
//! ## Layout
//!
//! The whole app is one [`NavShell`] (from the shared toolkit): a sidebar
//! [`NavList`](ui::widget::NavList) of categories beside the selected
//! category's content pane. The shell owns the responsive behaviour end to
//! end:
//!
//! * **wide**: the sidebar is docked and the content fills the area to its
//!   right; the framework draws the sidebar toggle;
//! * **narrow**: the list and the content are two full-screen screens
//!   ([`NavMode::Push`]): the category list, and the pane with a Back button
//!   that returns to the list. Picking a category pushes its pane.
//!
//! Settings supplies the shell with the nav list ([`crate::sidebar::build`]),
//! the selected category's content pane and the content header (the pane's
//! title). It does not re-implement the breakpoint, the sidebar surface or
//! the toggle. Search belongs to the platform: the sidebar header carries
//! the search launcher, and every category is a result of the app's
//! [`crate::search_provider::SettingsSearchProvider`].
//!
//! ## State and the rebuild that drives navigation
//!
//! The selected category lives on the widget. When [`SettingsApp::sync`] sees
//! a navigation request from the list, it updates `selected`, swaps the
//! shell's content and header, and flags `structure_dirty`. The host's frame
//! loop polls `take_structure_dirty` and re-runs `build_layout`, which is what
//! moves the visible pane to the new category.

use crate::categories::Category;
use crate::panes;
use crate::sidebar;
use ui::tokens::Metrics;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{Column, Container, NavMode, NavPane, NavShell, Scrollable, Widget};
use ui::{AppKind, AppWidget, IconDescriptor};

/// The measure (logical px) a form-like pane's content is centred within on
/// wide windows, so a trailing control sits near its label: the toolkit's
/// form-sheet width.
const FORM_MEASURE: f32 = Metrics::BASE.panel_width_sheet;

/// The measure of a pane whose rows carry several controls (accounts,
/// devices, displays): a form sheet beside a list panel.
const WIDE_MEASURE: f32 = Metrics::BASE.panel_width_sheet + Metrics::BASE.panel_width_list;

pub struct SettingsApp {
    /// Currently-selected sidebar category.
    selected: Category,
    /// `true` once a selection (or other composition-affecting change) has
    /// happened and the host must re-run `build_layout`. Drained by
    /// `take_structure_dirty`.
    structure_dirty: bool,
    /// Framework dispatcher, cloned onto a preference pane when that pane is built.
    framework: Option<ui::framework::Dispatcher<crate::resource::Settings>>,
    /// Set when hosted as a [`ui::framework::ResourceSurface`]; tests may flip
    /// it without constructing a real dispatcher.
    hosted: bool,
    /// The screen a narrow window opens on. `None` (the runtime default)
    /// lets the shell open on its first screen, the category list; a capture
    /// or a deep link can name the content screen.
    narrow_front: Option<NavPane>,
    /// Seed width hint so the very first offscreen frame resolves the correct
    /// wide/narrow layout. `None` lets the shell decide from its own rect.
    width_hint: Option<f32>,
    /// Process-local navigation request written by a sidebar row. This keeps
    /// current-pane selection out of collaboration, persistence and undo.
    sidebar_selection: ui::LocalState<Option<Category>>,
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
    device_mesh_commands: ui::LocalState<Option<crate::actions::DeviceMeshOperation>>,
    speaker_commands: ui::LocalState<Option<crate::actions::SpeakerOperation>>,
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    local_operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    local_operation: Option<tilo_actions::InvocationId>,
    local_operation_token: Option<String>,
    local_pane_command: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    /// Deterministic, secret-free inventory used only by visual acceptance
    /// captures. Live construction always leaves this `None`.
    account_directory_override: Option<crate::accounts::AccountDirectory>,
    account_form_override: Option<crate::accounts::AccountKind>,
    /// A fixture a capture scenario shows in place of the live pane.
    #[cfg(not(feature = "thin"))]
    shot_demo: Option<crate::shots::Demo>,
    /// Use stable, host-independent hardware fixtures in visual captures.
    /// Live construction always leaves this false.
    deterministic_hardware_for_shot: bool,
    account_probe: Option<(
        tilo_actions::InvocationId,
        std::sync::mpsc::Receiver<Result<String, String>>,
    )>,
    rect: vello::kurbo::Rect,
    /// The live theme's text growth (at least 1×), recorded at layout so the
    /// readable measure and the first-frame width hint grow with the type.
    text_growth: f32,
    /// The window shell, created lazily on the first `build_layout` and
    /// persisted across rebuilds so its responsive state survives a category
    /// switch. Its content and header are swapped on selection.
    shell: Option<NavShell>,
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
        Self {
            selected: Category::DEFAULT,
            structure_dirty: false,
            framework: None,
            hosted: false,
            narrow_front: None,
            width_hint: None,
            sidebar_selection: ui::LocalState::new(None),
            search_provider: crate::search_provider::SettingsSearchProvider::new(),
            account_resources: account_root.store().all().to_vec(),
            account_root,
            account_commands: ui::LocalState::new(None),
            device_mesh_commands: ui::LocalState::new(None),
            speaker_commands: ui::LocalState::new(None),
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            local_operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            local_operation: None,
            local_operation_token: None,
            local_pane_command: ui::LocalState::new(None),
            account_directory_override: None,
            account_form_override: None,
            #[cfg(not(feature = "thin"))]
            shot_demo: None,
            deterministic_hardware_for_shot: false,
            account_probe: None,
            rect: vello::kurbo::Rect::ZERO,
            text_growth: 1.0,
            shell: None,
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

    /// Seed the responsive layout with a known width before the first frame —
    /// the offscreen `--shots` renderer calls this so the cold screenshot
    /// resolves the correct wide/narrow layout immediately.
    #[must_use]
    pub fn with_width_hint(mut self, width: f32) -> Self {
        self.width_hint = Some(width);
        self
    }

    #[must_use]
    #[doc(hidden)]
    pub fn with_deterministic_hardware_for_shot(mut self) -> Self {
        self.deterministic_hardware_for_shot = true;
        self
    }

    /// Open a narrow window on the content screen (the pane, with the Back
    /// button) instead of the category list. Has no visible effect when the
    /// layout resolves to wide.
    #[must_use]
    pub fn with_narrow_content_shot(mut self) -> Self {
        self.narrow_front = Some(NavPane::Content);
        self
    }

    /// Open a narrow window on the category list: the screen the Back button
    /// returns to. Has no visible effect when the layout is wide.
    #[must_use]
    pub fn with_narrow_list_shot(mut self) -> Self {
        self.narrow_front = Some(NavPane::Sidebar);
        self
    }

    /// Stage a capture scenario (`None`: a plain `--shots`): pin the
    /// deterministic hardware fixtures, select the scenario's pane and show
    /// its screen. Never called in a live window.
    #[cfg(not(feature = "thin"))]
    pub(crate) fn stage_scenario(&mut self, name: Option<&str>) {
        use crate::shots::{Demo, Scenario};
        let scenario = Scenario::parse(name);
        self.deterministic_hardware_for_shot = true;
        self.shot_demo = scenario.demo;
        if matches!(
            scenario.demo,
            Some(Demo::AccountsPopulated | Demo::AccountsForm)
        ) {
            self.account_directory_override = Some(crate::accounts::sample_directory());
        }
        if scenario.demo == Some(Demo::AccountsForm) {
            self.account_form_override = Some(crate::accounts::AccountKind::Nextcloud);
        }
        self.selected = scenario.category;
        self.narrow_front = if scenario.narrow_list {
            Some(NavPane::Sidebar)
        } else if scenario.narrow_content {
            Some(NavPane::Content)
        } else {
            None
        };
        self.structure_dirty = true;
        self.refresh_shell_children();
        if let Some(shell) = &mut self.shell {
            shell.set_sidebar(Some(Box::new(sidebar::build(
                self.selected,
                &self.sidebar_selection,
            ))));
            if let Some(pane) = self.narrow_front {
                shell.show(pane);
            }
        }
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
    /// content and header, and (on a narrow layout) pushes the selected
    /// category's content screen.
    pub fn select(&mut self, category: Category) {
        if self.selected != category {
            self.selected = category;
            self.structure_dirty = true;
            self.refresh_shell_children();
            if let Some(shell) = &mut self.shell {
                shell.show(NavPane::Content);
            }
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
                .is_some_and(|w| w < Metrics::BASE.compact_breakpoint * self.text_growth),
        }
    }

    /// The width a pane can expect on the coming frame: the shell's content
    /// column once laid out, else the window hint less the docked sidebar,
    /// capped at the pane's readable measure. A pane that composes
    /// differently at phone width (the Region formats section) reads this for
    /// its first frame and its own rect thereafter.
    fn content_width_hint(&self) -> Option<f32> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a logical-pixel width comfortably representable by f32"
        )]
        let column = self
            .shell
            .as_ref()
            .map(|shell| shell.geometry().content_column.width() as f32)
            .filter(|width| *width > 0.0)
            .or_else(|| {
                self.width_hint.map(|w| {
                    if self.is_narrow() {
                        w
                    } else {
                        (w - Metrics::BASE.sidebar_width * self.text_growth).max(0.0)
                    }
                })
            });
        column.map(|width| {
            let gutter = ui::tokens::Spacing::BASE.s6;
            (width - 2.0 * gutter)
                .min(self.readable_measure() * self.text_growth)
                .max(0.0)
        })
    }

    /// Whether the sidebar / list is currently shown (always when wide, or the
    /// list screen when narrow). Used by `--shots` assertions.
    pub fn sidebar_open(&self) -> bool {
        self.shell.as_ref().is_some_and(NavShell::is_sidebar_open)
    }

    /// The on-screen rect of the narrow content screen's Back button (after a
    /// layout pass), so interaction tests can click the real control the shell
    /// draws. Zero-sized outside the narrow content screen.
    pub fn back_button_rect(&self) -> vello::kurbo::Rect {
        self.shell
            .as_ref()
            .map_or(vello::kurbo::Rect::ZERO, |shell| shell.geometry().back)
    }

    /// The content header: the selected category's name. The window is named
    /// by the OS, so there is no app title; this is the page's own title.
    fn page_title(&self) -> String {
        self.selected.label()
    }

    /// Build the content pane: the selected category's body, wrapped in a
    /// vertical [`Scrollable`] so tall panes scroll instead of being clipped.
    /// The page gutter sits inside the scrolled content, so the last row is
    /// never cut off above a dead strip.
    ///
    /// This intentionally draws no heading of its own: the shell's content
    /// header names the page.
    fn build_content(&self) -> Box<dyn Widget> {
        let body = self.build_pane_body();
        let gutter = ui::tokens::Spacing::BASE.s6;
        let inner = Column::new()
            .with_no_padding()
            .with_horizontal_padding(gutter, gutter)
            .with_vertical_padding(gutter)
            .with_intrinsic_height()
            .push(body);
        // Forms are more legible as a column on large displays: a trailing
        // control stays near its label.
        let measure = (self.readable_measure() + 2.0 * gutter) * self.text_growth;
        let content: Box<dyn Widget> = Box::new(
            Container::new()
                .with_no_padding()
                .with_centered_max_width(measure)
                .with_child(Box::new(inner)),
        );
        Box::new(Scrollable::new_vertical().with_child(content))
    }

    /// The readable measure (logical px) a pane's content is centred within
    /// on large monitors. Every cap is a maximum, so narrow screens stay
    /// fluid. Panes whose rows carry several controls get the wide measure.
    fn readable_measure(&self) -> f32 {
        match self.selected {
            Category::ExternalAccounts
            | Category::DeviceMesh
            | Category::Displays
            | Category::Printers
            | Category::Users => WIDE_MEASURE,
            _ => FORM_MEASURE,
        }
    }

    /// Remember the dispatcher so preference panes built later submit through it.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Settings>,
    ) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Preference panes read the document in their own sync. An empty
    /// projection must not wipe unsynced local pane state.
    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "ResourceSurface adopt hook kept for API parity; panes sync from the document"
    )]
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::SettingsState) {
        let _ = state;
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    /// Dispatch to the per-category pane body.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match constructs every category's pane"
    )]
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            // A capture never waits for NetworkManager; it renders the
            // loaded pane without host networks.
            #[cfg(not(feature = "thin"))]
            Category::Network if self.shot_demo == Some(crate::shots::Demo::NetworkWifi) => {
                crate::shots::network_demo_pane()
            }
            #[cfg(not(feature = "thin"))]
            Category::Sound if self.shot_demo == Some(crate::shots::Demo::SoundApps) => {
                crate::shots::audio_demo_pane()
            }
            Category::Network if self.deterministic_hardware_for_shot => Box::new(
                panes::network::NetworkPane::new_demo(Vec::new())
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Network => Box::new(
                panes::network::NetworkPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            #[cfg(not(feature = "thin"))]
            Category::Bluetooth if self.deterministic_hardware_for_shot => Box::new(
                panes::bluetooth::BluetoothPane::for_shot()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Bluetooth => Box::new(
                panes::bluetooth::BluetoothPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::DeviceMesh => Box::new(
                panes::device_mesh::DeviceMeshPane::new()
                    .with_command_state(&self.device_mesh_commands)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Sound => Box::new(
                panes::audio::AudioPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Speakers => Box::new(
                panes::speakers::SpeakersPane::new()
                    .with_command_state(&self.speaker_commands)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Displays if self.deterministic_hardware_for_shot => Box::new(
                panes::display::DisplayPane::with_dispatcher(
                    crate::dispatcher::dispatcher_or_real(None),
                    0.62,
                )
                .with_external_monitor_for_shot(),
            ),
            Category::Displays => Box::new(
                panes::display::DisplayPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::MouseKeyboard => Box::new(
                panes::mouse_keyboard::MouseKeyboardPane::new()
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::KeyboardShortcuts => {
                let mut pane = panes::keyboard_shortcuts::ShortcutPane::default();
                if let Some(dispatcher) = &self.framework {
                    pane.bind_dispatcher(dispatcher.clone());
                }
                Box::new(pane)
            }
            #[cfg(not(feature = "thin"))]
            Category::TimeDate if self.deterministic_hardware_for_shot => {
                Box::new(panes::time_date::TimeDatePane::for_shot())
            }
            Category::TimeDate => panes::time_date::build(),
            #[cfg(not(feature = "thin"))]
            Category::Users if self.deterministic_hardware_for_shot => Box::new(
                panes::users::UsersPane::for_shot()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Users => Box::new(
                panes::users::UsersPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            #[cfg(not(feature = "thin"))]
            Category::Printers if self.deterministic_hardware_for_shot => Box::new(
                panes::printers::PrintersPane::for_shot()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Printers => Box::new(
                panes::printers::PrintersPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            #[cfg(not(feature = "thin"))]
            Category::Power if self.deterministic_hardware_for_shot => Box::new(
                panes::power::PowerPane::with_parts(
                    crate::dispatcher::dispatcher_or_real(None),
                    os_actions::BatteryInfo {
                        present: true,
                        capacity: 72,
                        status: "Discharging".into(),
                        cycle_count: Some(148),
                        on_ac: false,
                        full_capacity: Some(40_000_000),
                        design_capacity: Some(50_000_000),
                        charge_limit: Some(80),
                    },
                    os_actions::PowerProfile::Balanced,
                    crate::prefs::PowerPrefs::default(),
                )
                .with_command_state(&self.local_pane_command)
                .with_operation_status(&self.local_operation_status),
            ),
            Category::Power => Box::new(
                panes::power::PowerPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::Appearance => {
                let mut pane = panes::appearance::AppearancePane::new();
                if let Some(dispatcher) = &self.framework {
                    pane.bind_dispatcher(dispatcher.clone());
                }
                Box::new(pane)
            }
            Category::Accessibility => panes::accessibility::build(),
            Category::Notifications if self.deterministic_hardware_for_shot => {
                Box::new(panes::notifications::NotificationsPane::for_shot())
            }
            Category::Notifications => {
                let mut pane = panes::notifications::NotificationsPane::new();
                if let Some(dispatcher) = &self.framework {
                    pane.bind_dispatcher(dispatcher.clone());
                }
                Box::new(pane)
            }
            Category::DefaultApps => {
                let mut pane = panes::personal_apps::PersonalAppsPane::new();
                if let Some(dispatcher) = &self.framework {
                    pane.bind_dispatcher(dispatcher.clone());
                }
                Box::new(pane)
            }
            Category::Storage => Box::new(
                panes::storage::StoragePane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::ExternalAccounts => {
                let mut pane = self.account_directory_override.as_ref().map_or_else(
                    || panes::accounts::AccountsPane::in_root(self.account_root.clone()),
                    |directory| {
                        panes::accounts::AccountsPane::from_dir_in(
                            self.account_root.clone(),
                            directory.clone(),
                            false,
                        )
                    },
                );
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
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            Category::RegionFormats => {
                let mut pane = panes::region::RegionPane::new();
                if let Some(dispatcher) = &self.framework {
                    pane.bind_dispatcher(dispatcher.clone());
                }
                Box::new(match self.content_width_hint() {
                    Some(width) => pane.with_width_hint(width),
                    None => pane,
                })
            }
            Category::Permissions => panes::permissions::build(),
            Category::Updates => Box::new(
                panes::updates::UpdatesPane::new()
                    .with_command_state(&self.local_pane_command)
                    .with_operation_status(&self.local_operation_status),
            ),
            #[cfg(not(feature = "thin"))]
            Category::About if self.deterministic_hardware_for_shot => {
                panes::about::build_with(&panes::about::AboutInfo::for_shot())
            }
            Category::About => panes::about::build(),
        }
    }

    /// Create the persistent [`NavShell`]: the category list in the sidebar
    /// (with the platform search launcher in its header), the selected pane
    /// as content under its page title, and the seeded capture state.
    fn build_shell(&self) -> NavShell {
        let mut shell = NavShell::new()
            .with_id("settings.shell")
            .mode(NavMode::Push)
            .sidebar(Box::new(sidebar::build(
                self.selected,
                &self.sidebar_selection,
            )))
            .sidebar_header(
                "",
                vec![Box::new(ui::search::search_button(
                    ui::search::SearchRequest::this_app(),
                ))],
            )
            .content(self.build_content())
            .content_header(
                self.page_title(),
                vec![Box::new(ui::widget::history_button())],
            );
        if let Some(pane) = self.narrow_front {
            shell.show(pane);
        }
        shell
    }

    /// Swap the shell's content and page title to reflect the current
    /// selection. No-op before the shell exists (the first `build_layout`
    /// will build them fresh).
    fn refresh_shell_children(&mut self) {
        let content = self.build_content();
        let title = self.page_title();
        if let Some(shell) = &mut self.shell {
            shell.set_content(content);
            shell.set_content_title(title);
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
        if type_id == crate::actions::INPUT_OPERATION_TYPE {
            let operation: crate::actions::InputOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            self.select(Category::MouseKeyboard);
            return self.complete_immediate_local_operation(
                type_id,
                payload,
                Self::apply_input_operation(operation),
            );
        }
        if type_id == crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE {
            let operation: crate::actions::KeyboardSourceOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            self.select(Category::MouseKeyboard);
            return self.complete_immediate_local_operation(
                type_id,
                payload,
                Self::apply_keyboard_source_operation(operation),
            );
        }
        if type_id == crate::actions::TIME_DATE_OPERATION_TYPE {
            let operation: crate::actions::TimeDateOperation =
                serde_json::from_slice(payload).map_err(|error| error.to_string())?;
            self.select(Category::TimeDate);
            return self.complete_immediate_local_operation(
                type_id,
                payload,
                Self::apply_time_date_operation(operation),
            );
        }
        let accepted: Result<(), String> = match type_id {
            crate::actions::UPDATE_OPERATION_TYPE => {
                let operation: crate::actions::UpdateOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Updates);
                if let crate::actions::UpdateOperation::SetSchedule { schedule } = &operation
                    && !["System schedule", "Daily", "Weekly"].contains(&schedule.as_str())
                {
                    return Err("unsupported update schedule".into());
                }
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Updates(operation)));
                Ok(())
            }
            crate::actions::POWER_OPERATION_TYPE => {
                let operation: crate::actions::PowerOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Power);
                match &operation {
                    crate::actions::PowerOperation::SetChargeLimit { percent } => {
                        let available = os_actions::charge_capability()
                            .is_some_and(|capability| capability.choices.contains(percent));
                        if !available {
                            return Err(
                                "battery charge-cap control is unavailable on this device".into()
                            );
                        }
                    }
                    crate::actions::PowerOperation::SetProfile { profile } => {
                        if crate::panes::power::profile_from_label(profile).is_none() {
                            return Err("unsupported power profile".into());
                        }
                    }
                    crate::actions::PowerOperation::SetAutoSuspend { .. } => {}
                    crate::actions::PowerOperation::SetBlankTimeout { label } => {
                        if crate::panes::power::blank_from_label(label).is_none() {
                            return Err("unsupported screen blank timeout".into());
                        }
                    }
                    crate::actions::PowerOperation::SetLockTimeout { label } => {
                        if crate::panes::power::lock_from_label(label).is_none() {
                            return Err("unsupported automatic lock timeout".into());
                        }
                    }
                    crate::actions::PowerOperation::SetSuspendTimeout { profile, label } => {
                        if crate::panes::power::suspend_from_label(label).is_none() {
                            return Err("unsupported suspend timeout".into());
                        }
                        match profile.as_str() {
                            "default" | "saver" | "balanced" | "performance" => {}
                            _ => return Err("unsupported power profile timeout".into()),
                        }
                    }
                    crate::actions::PowerOperation::SetLidAction { action }
                    | crate::actions::PowerOperation::SetPowerKeyAction { action }
                    | crate::actions::PowerOperation::SetCriticalAction { action } => {
                        if crate::panes::power::action_from_label(action).is_none() {
                            return Err("unsupported power action".into());
                        }
                    }
                }
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Power(operation)));
                Ok(())
            }
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
            crate::actions::BLUETOOTH_OPERATION_TYPE => {
                let operation: crate::actions::BluetoothOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Bluetooth);
                match &operation {
                    crate::actions::BluetoothOperation::SetPowered { .. }
                    | crate::actions::BluetoothOperation::StartScan
                    | crate::actions::BluetoothOperation::Refresh => {}
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
                            crate::actions::BluetoothOperation::SetPowered { .. }
                            | crate::actions::BluetoothOperation::StartScan
                            | crate::actions::BluetoothOperation::Refresh => {
                                return Err("Bluetooth operation does not address a device".into());
                            }
                        };
                        let device = os_actions::bluez::devices()?
                            .into_iter()
                            .find(|device| device.path == *device_path)
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
                    }
                }
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Bluetooth(operation)));
                Ok(())
            }
            crate::actions::STORAGE_OPERATION_TYPE => {
                let operation: crate::actions::StorageOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Storage);
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Storage(operation)));
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
                        if !["Subtle (4500 K)", "Warm (3700 K)", "Very warm (2800 K)"]
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
                                .any(|(id, name)| label == format!("{name} ({id})"));
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
            crate::actions::PRINTER_OPERATION_TYPE => {
                let operation: crate::actions::PrinterOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Printers);
                let validated: Result<(), String> = match operation.clone() {
                    crate::actions::PrinterOperation::Refresh
                    | crate::actions::PrinterOperation::Discover => Ok(()),
                    crate::actions::PrinterOperation::SetDefault { name } => {
                        if !os_actions::printing::queues()
                            .unwrap_or_default()
                            .iter()
                            .any(|q| q.name == name)
                        {
                            return Err("printer queue is no longer available".into());
                        }
                        Ok(())
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
                        Ok(())
                    }
                    crate::actions::PrinterOperation::Remove { name } => {
                        if !os_actions::printing::queues()
                            .unwrap_or_default()
                            .iter()
                            .any(|q| q.name == name)
                        {
                            return Err("printer queue is no longer available".into());
                        }
                        Ok(())
                    }
                    crate::actions::PrinterOperation::CancelJob { id } => {
                        if !os_actions::printing::jobs()
                            .unwrap_or_default()
                            .iter()
                            .any(|job| job.id == id)
                        {
                            return Err("print job is no longer available".into());
                        }
                        Ok(())
                    }
                    crate::actions::PrinterOperation::RetryJob { id } => {
                        if !os_actions::printing::jobs()
                            .unwrap_or_default()
                            .iter()
                            .any(|job| job.id == id)
                        {
                            return Err("print job is no longer available".into());
                        }
                        Ok(())
                    }
                };
                validated?;
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::Printer(operation)));
                Ok(())
            }
            crate::actions::USER_OPERATION_TYPE => {
                let operation: crate::actions::UserOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::Users);
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::User(operation)));
                Ok(())
            }
            crate::actions::AGENT_MODEL_OPERATION_TYPE => {
                let operation: crate::actions::AgentModelOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::AgentModels);
                self.local_pane_command
                    .set(Some(crate::actions::LocalPaneCommand::AgentModel(
                        operation,
                    )));
                Ok(())
            }
            crate::actions::DEVICE_MESH_OPERATION_TYPE => {
                let operation: crate::actions::DeviceMeshOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                self.select(Category::DeviceMesh);
                self.device_mesh_commands.set(Some(operation));
                Ok(())
            }
            crate::actions::SPEAKER_OPERATION_TYPE => {
                let operation: crate::actions::SpeakerOperation =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if operation.command().valid() {
                    self.select(Category::Speakers);
                    self.speaker_commands.set(Some(operation));
                    Ok(())
                } else {
                    Err(
                        "that speaker request is incomplete: check the names, members and ids"
                            .to_string(),
                    )
                }
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

    /// Report a synchronously applied local setting through the same
    /// accepted/polled lifecycle used by longer device commands. The action is
    /// never written to the CRDT: completion reports whether the local backend
    /// save or system-service call succeeded.
    fn complete_immediate_local_operation(
        &mut self,
        type_id: &str,
        payload: &[u8],
        result: Result<String, String>,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let token = crate::actions::local_operation_token(type_id, payload)?;
        self.local_operation_status
            .set(crate::actions::LocalOperationStatus::Running {
                token: token.clone(),
            });
        self.local_operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded {
                token: token.clone(),
                message,
            },
            Err(message) => crate::actions::LocalOperationStatus::Failed {
                token: token.clone(),
                message,
            },
        });
        self.local_operation_token = Some(token);
        self.structure_dirty = true;
        self.refresh_shell_children();
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive match over the closed InputOperation catalogue"
    )]
    fn apply_input_operation(operation: crate::actions::InputOperation) -> Result<String, String> {
        let mut prefs = crate::prefs::SystemPrefs::load();
        let message = match operation {
            crate::actions::InputOperation::SetTouchpadNaturalScroll { enabled } => {
                prefs.input.touchpad_natural_scroll = Some(enabled);
                "touchpad natural scrolling updated"
            }
            crate::actions::InputOperation::SetMouseNaturalScroll { enabled } => {
                prefs.input.mouse_natural_scroll = Some(enabled);
                "mouse natural scrolling updated"
            }
            crate::actions::InputOperation::SetTapToClick { enabled } => {
                prefs.input.tap_to_click = enabled;
                "tap to click updated"
            }
            crate::actions::InputOperation::SetTouchpadPointerSpeed { value } => {
                prefs.input.touchpad_pointer_speed = Some(valid_input_fraction(value)?);
                "touchpad speed updated"
            }
            crate::actions::InputOperation::SetMousePointerSpeed { value } => {
                prefs.input.mouse_pointer_speed = Some(valid_input_fraction(value)?);
                "mouse speed updated"
            }
            crate::actions::InputOperation::SetKeyRepeat { value } => {
                prefs.input.key_repeat = valid_input_fraction(value)?;
                prefs.input.key_repeat_delay_ms = None;
                prefs.input.key_repeat_rate_hz = None;
                "key repeat updated"
            }
            crate::actions::InputOperation::SetTapDrag { enabled } => {
                prefs.input.tap_drag = enabled;
                "tap and drag updated"
            }
            crate::actions::InputOperation::SetTapDragLock { enabled } => {
                prefs.input.tap_drag_lock = enabled;
                "tap-and-drag lock updated"
            }
            crate::actions::InputOperation::SetDisableWhileTyping { enabled } => {
                prefs.input.disable_while_typing = enabled;
                "disable while typing updated"
            }
            crate::actions::InputOperation::SetMiddleEmulation { enabled } => {
                prefs.input.middle_emulation = enabled;
                "middle-button emulation updated"
            }
            crate::actions::InputOperation::SetLeftHanded { enabled } => {
                prefs.input.left_handed = enabled;
                "pointer handedness updated"
            }
            crate::actions::InputOperation::SetTouchpadGestures { enabled } => {
                prefs.input.touchpad_gestures = enabled;
                "touchpad gestures updated"
            }
            crate::actions::InputOperation::SetTouchpadEnabled { enabled } => {
                prefs.input.touchpad_enabled = enabled;
                if enabled {
                    "touchpad turned on"
                } else {
                    "touchpad turned off"
                }
            }
            crate::actions::InputOperation::SetTouchpadDisabledOnExternalMouse { enabled } => {
                prefs.input.touchpad_disabled_on_external_mouse = enabled;
                "touchpad mouse handoff updated"
            }
            crate::actions::InputOperation::SetTouchpadClickMethod { method } => {
                prefs.input.touchpad_click_method =
                    crate::panes::mouse_keyboard::click_method_for_id(&method)
                        .ok_or_else(|| "unsupported touchpad click method".to_string())?;
                "touchpad click method updated"
            }
            crate::actions::InputOperation::SetTouchpadScrollMethod { method } => {
                prefs.input.touchpad_scroll_method =
                    crate::panes::mouse_keyboard::scroll_method_for_id(&method)
                        .ok_or_else(|| "unsupported touchpad scroll method".to_string())?;
                "touchpad scroll method updated"
            }
            crate::actions::InputOperation::SetScrollSpeed { value } => {
                prefs.input.scroll_speed = settings_store::hardware::scroll_speed_from_slider(
                    valid_input_fraction(value)?,
                );
                "scroll speed updated"
            }
            crate::actions::InputOperation::SetAccelerationProfile { profile } => {
                prefs.input.accel_profile =
                    crate::panes::mouse_keyboard::accel_from_label(&profile)
                        .ok_or_else(|| "unsupported acceleration profile".to_string())?
                        .to_string();
                "pointer acceleration updated"
            }
            crate::actions::InputOperation::SetComposeKey { key } => {
                prefs.input.compose_key = crate::panes::mouse_keyboard::compose_from_label(&key)
                    .ok_or_else(|| "unsupported compose key".to_string())?
                    .as_stored()
                    .map(str::to_string);
                "compose key updated"
            }
            crate::actions::InputOperation::SetCapsLock { behavior } => {
                prefs.input.caps_lock =
                    crate::panes::mouse_keyboard::caps_lock_from_label(&behavior)
                        .ok_or_else(|| "unsupported Caps Lock behavior".to_string())?
                        .as_stored()
                        .map(str::to_string);
                "Caps Lock behavior updated"
            }
            crate::actions::InputOperation::SetActiveSource { label } => {
                let id = crate::input_sources::installed_catalog()
                    .into_iter()
                    .find(|entry| entry.label == label)
                    .map(|entry| entry.id)
                    .ok_or_else(|| "keyboard input source is no longer installed".to_string())?;
                if !prefs.input.keyboard_layouts.contains(&id) {
                    return Err("keyboard input source is not active on this device".into());
                }
                prefs.input.keyboard_layout = id;
                "active keyboard source updated"
            }
            crate::actions::InputOperation::SetTabletOutput { device, output } => {
                let output = crate::panes::mouse_keyboard::tablet_output_for_id(&output)
                    .ok_or_else(|| "that display is not connected".to_string())?;
                tablet_entry(&mut prefs, &device)?.output = output;
                "tablet mapping updated"
            }
            crate::actions::InputOperation::SetTabletKeepAspect { device, enabled } => {
                tablet_entry(&mut prefs, &device)?.keep_aspect = enabled;
                "tablet aspect ratio updated"
            }
            crate::actions::InputOperation::SetTabletLeftHanded { device, enabled } => {
                tablet_entry(&mut prefs, &device)?.left_handed = enabled;
                "tablet orientation updated"
            }
            crate::actions::InputOperation::SetTabletTipFeel { device, value } => {
                let feel = valid_input_fraction(value)?;
                tablet_entry(&mut prefs, &device)?.pressure_curve =
                    settings_store::hardware::pressure_curve_for_feel(feel);
                "pen tip feel updated"
            }
            crate::actions::InputOperation::SetTabletButton {
                device,
                button,
                action,
            } => {
                let action = crate::panes::mouse_keyboard::stylus_button_for_id(&action)
                    .ok_or_else(|| "unsupported pen button action".to_string())?;
                let config = tablet_entry(&mut prefs, &device)?;
                match button.as_str() {
                    "lower" => config.lower_button = action,
                    "upper" => config.upper_button = action,
                    "third" => config.third_button = action,
                    _ => return Err("unknown pen button".into()),
                }
                "pen button updated"
            }
            crate::actions::InputOperation::SetTabletEraser { device, action } => {
                let action = crate::panes::mouse_keyboard::eraser_for_id(&action)
                    .ok_or_else(|| "unsupported eraser action".to_string())?;
                tablet_entry(&mut prefs, &device)?.eraser = action;
                "eraser updated"
            }
            crate::actions::InputOperation::SetPadDial {
                device,
                dial,
                index,
                mode,
                action,
            } => {
                use crate::panes::mouse_keyboard::{MAX_PAD_DIALS, MAX_PAD_MODES, dial_for_id};
                if index >= MAX_PAD_DIALS || mode >= MAX_PAD_MODES {
                    return Err("unknown pad control".into());
                }
                let dial = match dial.as_str() {
                    "ring" => settings_store::hardware::PadDial::Ring(index),
                    "strip" => settings_store::hardware::PadDial::Strip(index),
                    _ => return Err("unknown pad control".into()),
                };
                let action = dial_for_id(&action)
                    .ok_or_else(|| "unsupported pad control action".to_string())?;
                tablet_entry(&mut prefs, &device)?.set_dial_action(dial, mode, action);
                "pad control updated"
            }
            crate::actions::InputOperation::SetPadButton {
                device,
                button,
                action,
            } => {
                if button >= crate::panes::mouse_keyboard::MAX_PAD_BUTTONS {
                    return Err("unknown pad button".into());
                }
                let choice = crate::panes::mouse_keyboard::pad_button_for_id(&action)
                    .ok_or_else(|| "unsupported pad button action".to_string())?;
                crate::panes::mouse_keyboard::set_pad_button(
                    tablet_entry(&mut prefs, &device)?,
                    button,
                    choice,
                );
                "pad button updated"
            }
        };
        prefs.input.normalise_keyboard_sources();
        prefs
            .save()
            .map_err(|error| format!("could not persist input preferences: {error}"))?;
        Ok(message.into())
    }

    fn apply_keyboard_source_operation(
        operation: crate::actions::KeyboardSourceOperation,
    ) -> Result<String, String> {
        let mut prefs = crate::prefs::SystemPrefs::load();
        prefs.input.normalise_keyboard_sources();
        let message = match operation {
            crate::actions::KeyboardSourceOperation::Add { label } => {
                let source = crate::input_sources::installed_catalog()
                    .into_iter()
                    .find(|entry| entry.label == label)
                    .ok_or_else(|| "keyboard input source is no longer installed".to_string())?;
                if prefs.input.keyboard_layouts.contains(&source.id) {
                    return Err("keyboard input source is already active".into());
                }
                if !crate::input_sources::has_room_for(&prefs.input.keyboard_layouts, &source.id) {
                    return Err(
                        "the maximum number of keyboard input sources is already active".into(),
                    );
                }
                prefs.input.keyboard_layout.clone_from(&source.id);
                prefs.input.keyboard_layouts.push(source.id);
                "keyboard input source added"
            }
            crate::actions::KeyboardSourceOperation::MoveUp { index } => {
                if index == 0 || index >= prefs.input.keyboard_layouts.len() {
                    return Err("keyboard input source can no longer move up".into());
                }
                prefs.input.keyboard_layouts.swap(index - 1, index);
                "keyboard input source moved up"
            }
            crate::actions::KeyboardSourceOperation::MoveDown { index } => {
                if index + 1 >= prefs.input.keyboard_layouts.len() {
                    return Err("keyboard input source can no longer move down".into());
                }
                prefs.input.keyboard_layouts.swap(index, index + 1);
                "keyboard input source moved down"
            }
            crate::actions::KeyboardSourceOperation::Remove { index } => {
                if prefs.input.keyboard_layouts.len() <= 1
                    || index >= prefs.input.keyboard_layouts.len()
                {
                    return Err("keyboard input source can no longer be removed".into());
                }
                let removed = prefs.input.keyboard_layouts.remove(index);
                if removed == prefs.input.keyboard_layout {
                    prefs.input.keyboard_layout = prefs
                        .input
                        .keyboard_layouts
                        .get(index.min(prefs.input.keyboard_layouts.len() - 1))
                        .cloned()
                        .ok_or_else(|| "no keyboard source remains active".to_string())?;
                }
                "keyboard input source removed"
            }
        };
        prefs.input.normalise_keyboard_sources();
        prefs
            .save()
            .map_err(|error| format!("could not persist keyboard input sources: {error}"))?;
        Ok(message.into())
    }

    fn apply_time_date_operation(
        operation: crate::actions::TimeDateOperation,
    ) -> Result<String, String> {
        match operation {
            crate::actions::TimeDateOperation::SetAutomaticTime { enabled } => {
                os_actions::set_ntp(enabled)
                    .map_err(|error| format!("automatic time could not be changed: {error}"))?;
                Ok("automatic time updated".into())
            }
            crate::actions::TimeDateOperation::SetTimezone { timezone } => {
                if !os_actions::list_timezones()
                    .unwrap_or_default()
                    .iter()
                    .any(|zone| zone == &timezone)
                {
                    return Err("time zone is not available on this device".into());
                }
                os_actions::set_timezone(&timezone)
                    .map_err(|error| format!("time zone could not be changed: {error}"))?;
                Ok("time zone updated".into())
            }
            crate::actions::TimeDateOperation::SetHourCycle { h24 } => {
                let mut region = crate::region::RegionSettings::load();
                region.locale.time_format = if h24 {
                    crate::region::TimeStyle::H24
                } else {
                    crate::region::TimeStyle::H12
                };
                region
                    .save()
                    .map_err(|error| format!("could not persist clock format: {error}"))?;
                Ok("clock format updated".into())
            }
        }
    }
}

/// The saved settings of the tablet or pad `device`, created on first use.
/// Keys are device identities from the compositor's snapshot, so anything
/// else is refused rather than stored.
fn tablet_entry<'a>(
    prefs: &'a mut crate::prefs::SystemPrefs,
    device: &str,
) -> Result<&'a mut settings_store::hardware::TabletConfig, String> {
    if device.is_empty() || device.len() > 512 || device.chars().any(char::is_control) {
        return Err("unknown tablet".into());
    }
    Ok(prefs.input.tablets.entry(device.to_string()).or_default())
}

fn valid_input_fraction(value: f64) -> Result<f64, String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("input value must be between 0.0 and 1.0".into())
    }
}

impl Widget for SettingsApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
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
        let (state, path) = ui::doc::document_ref(state, path);
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
        Some(serde_json::json!({
            "app": "so.tilo.settings",
            "nav_id": sidebar::NAV_ID,
            "ready": valid(self.rect),
            "root_rect": rect(self.rect),
            "selected_category": self.selected.slug(),
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
            crate::actions::INPUT_OPERATION_TYPE,
            crate::actions::TIME_DATE_OPERATION_TYPE,
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Sidebar selection is local presentation state. It does not create
        // CRDT click flags, collaboration traffic or undo entries. The
        // canonical resource `settings.section:<slug>/open` still reaches the
        // same `select` method through its View operation.
        if let Some(category) = self.sidebar_selection.with(|request| *request) {
            self.sidebar_selection.set(None);
            self.select(category);
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
        // Create the shell once and reuse it so its responsive state survives
        // category switches (which rebuild the content).
        crate::widgets::set_compose_text_growth(theme.text_growth());
        self.text_growth = theme.text_growth();
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
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns the
    /// `SettingsAction` preference door — keep only live device extras.
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::actions::live_device_resources(doc, self.account_resource_inventory());
        }
        self.publish_resources(doc)
    }
    /// Resolve canonical Settings resources at activation time as well as
    /// publishing the eager palette snapshot above.  Search results retain
    /// only stable action addresses, so this is the authoritative path that
    /// re-checks an address against the current resource surface before the
    /// action door opens it.
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "settings.root" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::CommittedActions for SettingsApp {
    /// Bubble an on-screen control's dispatched-action undo bracket up from
    /// whichever pane produced it. A settings control lives deep under the shell
    /// (`AppShell → content → Scrollable → … → pane`), and the intermediate
    /// containers don't forward this rising-edge signal, so the root walks its
    /// subtree and returns the first pane's captured heads. The host records one
    /// undo boundary from it, so Ctrl+Z reverts the setting change.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        fn walk(w: &mut dyn Widget) -> Option<Vec<automerge::ChangeHash>> {
            if let Some(heads) = ui::undo::take_committed_action(w) {
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
            crate::actions::UPDATE_OPERATION_TYPE
                | crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE
                | crate::actions::INPUT_OPERATION_TYPE
                | crate::actions::TIME_DATE_OPERATION_TYPE
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

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the saved curve is the exact preset constant"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "every tablet and pad operation, applied then refused, against one saved document"
    )]
    fn tablet_operations_persist_per_device_and_refuse_unknown_choices() {
        use crate::actions::InputOperation;
        use settings_store::hardware::{
            EraserAction, PadDial, PadDialAction, ShortcutAction, StylusButtonAction,
        };
        let _config = crate::prefs::test_env::ConfigGuard::new();
        let pen = "056a:0374:Wacom Intuos S Pen".to_string();
        let pad = "056a:0374:Wacom Intuos S Pad".to_string();
        for operation in [
            InputOperation::SetTabletOutput {
                device: pen.clone(),
                output: crate::panes::mouse_keyboard::WHOLE_DESKTOP.into(),
            },
            InputOperation::SetTabletKeepAspect {
                device: pen.clone(),
                enabled: true,
            },
            InputOperation::SetTabletLeftHanded {
                device: pen.clone(),
                enabled: true,
            },
            InputOperation::SetTabletTipFeel {
                device: pen.clone(),
                value: 0.0,
            },
            InputOperation::SetTabletButton {
                device: pen.clone(),
                button: "upper".into(),
                action: "shortcut:search".into(),
            },
            InputOperation::SetTabletButton {
                device: pen.clone(),
                button: "lower".into(),
                action: "right".into(),
            },
            InputOperation::SetTabletButton {
                device: pen.clone(),
                button: "third".into(),
                action: "back".into(),
            },
            InputOperation::SetTabletEraser {
                device: pen.clone(),
                action: "middle".into(),
            },
            InputOperation::SetPadButton {
                device: pad.clone(),
                button: 1,
                action: "shortcut:workspace_overview".into(),
            },
            InputOperation::SetPadDial {
                device: pad.clone(),
                dial: "ring".into(),
                index: 0,
                mode: 1,
                action: "brush-size".into(),
            },
            InputOperation::SetPadDial {
                device: pad.clone(),
                dial: "strip".into(),
                index: 1,
                mode: 0,
                action: "volume".into(),
            },
        ] {
            SettingsApp::apply_input_operation(operation).expect("a valid tablet choice applies");
        }
        let saved = crate::prefs::SystemPrefs::load().input.tablets;
        let pen_config = &saved[&pen];
        assert_eq!(pen_config.output, None);
        assert!(pen_config.keep_aspect && pen_config.left_handed);
        assert_eq!(
            pen_config.pressure_curve,
            settings_store::hardware::PRESSURE_CURVE_PRESETS[0]
        );
        assert_eq!(
            pen_config.upper_button,
            StylusButtonAction::Shortcut(ShortcutAction::Search)
        );
        assert_eq!(pen_config.lower_button, StylusButtonAction::Right);
        assert_eq!(pen_config.third_button, StylusButtonAction::Back);
        assert_eq!(pen_config.eraser, EraserAction::Middle);
        assert_eq!(
            saved[&pad].pad_buttons.get(&1),
            Some(&ShortcutAction::WorkspaceOverview)
        );
        assert_eq!(
            saved[&pad].dial_action(PadDial::Ring(0), 1),
            PadDialAction::BrushSize
        );
        assert_eq!(
            saved[&pad].dial_action(PadDial::Ring(0), 0),
            PadDialAction::Scroll,
            "the ring's other mode keeps scrolling"
        );
        assert_eq!(
            saved[&pad].dial_action(PadDial::Strip(1), 0),
            PadDialAction::Volume
        );
        // The compositor reads the same document.
        let compositor =
            settings_store::hardware::tablets_from_section(&settings_store::load_section("input"));
        assert_eq!(compositor.get(&pen), Some(pen_config));

        SettingsApp::apply_input_operation(InputOperation::SetPadButton {
            device: pad.clone(),
            button: 1,
            action: "none".into(),
        })
        .unwrap();
        assert!(
            crate::prefs::SystemPrefs::load().input.tablets[&pad]
                .pad_buttons
                .is_empty()
        );
        SettingsApp::apply_input_operation(InputOperation::SetPadButton {
            device: pad.clone(),
            button: 1,
            action: "show-map".into(),
        })
        .unwrap();
        assert_eq!(
            crate::prefs::SystemPrefs::load().input.tablets[&pad].pad_map_button,
            Some(1),
            "the compositor shows the pad map while button 2 is held"
        );

        for refused in [
            InputOperation::SetTabletOutput {
                device: pen.clone(),
                output: "HDMI-A-9".into(),
            },
            InputOperation::SetTabletButton {
                device: pen.clone(),
                button: "sideways".into(),
                action: "right".into(),
            },
            InputOperation::SetTabletButton {
                device: pen.clone(),
                button: "lower".into(),
                action: "shortcut:make_coffee".into(),
            },
            InputOperation::SetPadButton {
                device: pad.clone(),
                button: 99,
                action: "none".into(),
            },
            InputOperation::SetTabletEraser {
                device: pen.clone(),
                action: "explode".into(),
            },
            InputOperation::SetPadDial {
                device: pad.clone(),
                dial: "wheel".into(),
                index: 0,
                mode: 0,
                action: "scroll".into(),
            },
            InputOperation::SetPadDial {
                device: pad.clone(),
                dial: "ring".into(),
                index: 0,
                mode: 99,
                action: "scroll".into(),
            },
            InputOperation::SetPadDial {
                device: pad,
                dial: "ring".into(),
                index: 0,
                mode: 0,
                action: "shortcut:search".into(),
            },
            InputOperation::SetTabletKeepAspect {
                device: String::new(),
                enabled: true,
            },
        ] {
            assert!(SettingsApp::apply_input_operation(refused).is_err());
        }
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

    /// Update operations are accepted, then completed or failed only by the owning pane.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn local_operations_complete_only_when_the_pane_reports() {
        // Case `canonical_update_operation_is_accepted_not_completed_until_the_pane_reports`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let heads = doc.get_heads();
            let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
            let invocation = tilo_actions::InvocationId::new();
            let start = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::UPDATE_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut doc,
                    &root,
                )
                .expect("the check operation is valid");
            assert!(matches!(
                start,
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert_eq!(app.selected(), Category::Updates);
            assert_eq!(
                doc.get_heads(),
                heads,
                "local effects must not touch Automerge"
            );
            assert_eq!(
                app.local_pane_command.get(),
                Some(crate::actions::LocalPaneCommand::Updates(
                    crate::actions::UpdateOperation::Check
                ))
            );
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

        // Case `non_cancellable_local_operation_ignores_cancel_until_the_pane_finishes`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &payload,
                ),
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

        // Case `unrelated_pane_status_cannot_finish_a_pending_local_operation`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &payload,
                ),
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

        // Case `overwritten_schedule_value_cannot_complete_the_original_invocation`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let invocation = tilo_actions::InvocationId::new();
            let daily = crate::actions::UpdateOperation::SetSchedule {
                schedule: "Daily".into(),
            };
            let payload = serde_json::to_vec(&daily).unwrap();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &payload,
                ),
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

        // Case `pane_reported_local_error_becomes_a_failed_terminal_update`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::UPDATE_OPERATION_TYPE,
                    &payload,
                ),
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

        // Case `local_operation_rejects_an_update_schedule_outside_the_panes_choices`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
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
            let keyboard_label = crate::input_sources::installed_catalog()
                .into_iter()
                .find(|source| source.id != crate::prefs::SystemPrefs::load().input.keyboard_layout)
                .expect("the bounded installed catalogue has an alternate keyboard source")
                .label;
            let cases = vec![
                (
                    crate::actions::UPDATE_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::UpdateOperation::Check).unwrap(),
                ),
                (
                    crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::KeyboardSourceOperation::Add {
                        label: keyboard_label,
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::INPUT_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::InputOperation::SetTapToClick {
                        enabled: false,
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::TIME_DATE_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::TimeDateOperation::SetHourCycle {
                        h24: true,
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::POWER_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::PowerOperation::SetAutoSuspend {
                        enabled: false,
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::NETWORK_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::NetworkOperation::Refresh).unwrap(),
                ),
                (
                    crate::actions::BLUETOOTH_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::BluetoothOperation::Refresh).unwrap(),
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
                (
                    crate::actions::STORAGE_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::StorageOperation::OpenDisks).unwrap(),
                ),
                (
                    crate::actions::PRINTER_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::PrinterOperation::Refresh).unwrap(),
                ),
                (
                    crate::actions::USER_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::UserOperation::SetAdmin {
                        username: "bounded-missing-user".into(),
                        admin: true,
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::AGENT_MODEL_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::AgentModelOperation::StageAccelerator)
                        .unwrap(),
                ),
                (
                    crate::actions::DEVICE_MESH_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::DeviceMeshOperation::SetRole {
                        role: "Personal".into(),
                    })
                    .unwrap(),
                ),
                (
                    crate::actions::SPEAKER_OPERATION_TYPE,
                    serde_json::to_vec(&crate::actions::SpeakerOperation::Delete {
                        group: "ab".repeat(16),
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
                app.device_mesh_commands.set(None);
                app.speaker_commands.set(None);
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

            let heads = doc.get_heads();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::AGENT_PROVIDER_OPERATION_TYPE,
                        &serde_json::to_vec(&crate::actions::AgentProviderOperation::Add {
                            protocol: "anthropic".into(),
                        })
                        .unwrap()
                    ),
                    &mut doc,
                    &root
                )
                .expect("provider command is admitted"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(doc.get_heads(), heads, "provider command touched Automerge");
            covered.insert(crate::actions::AGENT_PROVIDER_OPERATION_TYPE);

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

        // Case `input_effect_persists_locally_and_never_latches_a_crdt_value`.
        {
            use automerge::ReadDoc;

            let config = crate::prefs::test_env::ConfigGuard::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let operation = crate::actions::InputOperation::SetTapToClick { enabled: false };
            let payload = serde_json::to_vec(&operation).expect("operation serializes");
            let invocation = tilo_actions::InvocationId::new();
            let start = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::INPUT_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut doc,
                    &root,
                )
                .expect("input effect is admitted");
            assert!(matches!(
                start,
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert_eq!(app.selected(), Category::MouseKeyboard);
            assert!(
                doc.get(&root, panes::mouse_keyboard::TAP_CLICK_KEY)
                    .unwrap()
                    .is_none()
            );
            assert!(!crate::prefs::SystemPrefs::load().input.tap_to_click);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &root),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // Case `bluetooth_refresh_waits_for_its_own_local_completion_token`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let mut app = config.app();
            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let heads = doc.get_heads();
            let operation = crate::actions::BluetoothOperation::Refresh;
            let payload = serde_json::to_vec(&operation).expect("Bluetooth operation serializes");
            let invocation = tilo_actions::InvocationId::new();
            let start = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::BLUETOOTH_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut doc,
                    &root,
                )
                .expect("Bluetooth refresh admission is valid");
            assert!(matches!(
                start,
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert_eq!(app.selected(), Category::Bluetooth);
            assert_eq!(doc.get_heads(), heads);
            assert_eq!(
                app.local_pane_command.get(),
                Some(crate::actions::LocalPaneCommand::Bluetooth(
                    operation.clone()
                ))
            );
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

    /// The responsive shell docks the sidebar when wide. A narrow (phone)
    /// window opens on the category list, the first screen; picking a
    /// category pushes its content.
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
                "narrow opens on the category list",
                |c| c.app().with_width_hint(440.0),
                (440.0, 820.0),
                Some(true),
                Some(true),
                Back::Absent,
            ),
            (
                "narrow content shot drills into content",
                |c| {
                    c.app()
                        .with_category(Category::RegionFormats)
                        .with_width_hint(440.0)
                        .with_narrow_content_shot()
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
                |c| c.app().with_width_hint(440.0).with_narrow_list_shot(),
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
                        .with_narrow_content_shot()
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
        // Case `narrow_content_screen_carries_the_back_arrow_beside_the_page_title`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config
                .app()
                .with_category(Category::RegionFormats)
                .with_width_hint(440.0)
                .with_narrow_content_shot();
            let app = layout_at(app, 440.0, 820.0);
            let back = app.back_button_rect();
            assert!(back.width() > 0.0, "the Back button is drawn");
            let content = app
                .shell
                .as_ref()
                .expect("layout builds the shell")
                .geometry()
                .content_column;
            assert!(
                back.y0 >= content.y0 && back.y1 <= content.y0 + back.height() * 2.0,
                "the Back button sits in the content column's header row"
            );
        }

        // Case `narrow_large_text_keeps_the_destination_in_the_initial_viewport`.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config
                .app()
                .with_category(Category::RegionFormats)
                .with_width_hint(390.0)
                .with_narrow_content_shot();
            let app =
                layout_at_with_theme(app, 390.0, 844.0, &ui::Theme::dark().with_text_scale(1.5));
            let content = app
                .shell
                .as_ref()
                .expect("layout builds the shell")
                .geometry()
                .content_column;
            assert!(
                content.width() > 0.0 && content.y0 < 844.0,
                "large text must retain the page in the first viewport ({content:?})"
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
                .geometry()
                .sidebar_column;
            assert!(
                sidebar.width() > f64::from(Metrics::BASE.sidebar_width),
                "the sidebar grows with the text it holds: {sidebar:?}"
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
            let app = config.app().with_width_hint(440.0).with_narrow_list_shot();
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

    /// No app-name header, and the root landmark survives rebuilds.
    #[test]
    fn chrome_draws_no_app_header_and_keeps_its_landmark() {
        // Case `draws_no_app_name_header`.
        // The app must NOT draw its own title or an app-name header: the OS
        // window names the app. The sidebar's header carries only the search
        // launcher; the content header carries the page's own title.
        {
            let config = crate::prefs::test_env::ScratchConfig::new();
            let app = config.app();
            let app = layout_at(app, 1200.0, 760.0);
            let shell = app.shell.as_ref().expect("shell built");
            // The resolved layout is the wide split: docked sidebar, content.
            assert!(shell.geometry().stack.is_empty());
            assert_eq!(
                shell.geometry().sidebar,
                ui::widget::SidebarPlacement::Docked
            );
            assert_eq!(app.page_title(), Category::DEFAULT.label());
            assert_ne!(
                app.page_title(),
                "Settings",
                "the page, not the app, is titled"
            );
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
