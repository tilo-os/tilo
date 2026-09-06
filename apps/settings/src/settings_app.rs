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
    /// The live search query typed into the sidebar's search field (mirrored
    /// from [`crate::sidebar::SEARCH_KEY`]). When non-empty the sidebar shows
    /// a filtered results list instead of the sectioned category list.
    search_query: String,
    /// Secret-free snapshot for resource enumeration. Refreshes at account
    /// mutation boundaries, never during the per-frame resource query.
    account_resources: Vec<accounts::Account>,
    account_commands: ui::LocalState<Option<crate::panes::accounts::AccountViewCommand>>,
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    /// Deterministic, secret-free inventory used only by visual acceptance
    /// captures. Live construction always leaves this `None`.
    account_directory_override: Option<crate::accounts::AccountDirectory>,
    account_form_override: Option<crate::accounts::AccountKind>,
    account_probe: Option<(
        qos_actions::InvocationId,
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
            tracing::warn!(target: "qos::settings", %error, "could not recover interrupted account mutation");
        }
        Self {
            selected: Category::DEFAULT,
            structure_dirty: false,
            narrow_shot_screen: None,
            width_hint: None,
            search_query: String::new(),
            account_resources: accounts::AccountStore::load().all().to_vec(),
            account_commands: ui::LocalState::new(None),
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            account_directory_override: None,
            account_form_override: None,
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
        self
    }

    /// Build the search field placed at the top of the shell's sidebar. Bound to
    /// [`sidebar::SEARCH_KEY`]; its value is re-read from the document on every
    /// `sync`, so the text survives the sidebar's filter rebuild. Because the
    /// shell hosts it as a direct child in the App's tree, a click focuses it
    /// and `autofocus_on_structure_change` restores focus + caret after the
    /// filtered sidebar rebuilds.
    fn build_search_field() -> Box<dyn Widget> {
        Box::new(
            TextInput::new(Vec::new())
                .with_kind(ui::widget::TextInputKind::Search)
                .with_id("settings.search")
                .with_on_change_path(sidebar::SEARCH_KEY)
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
        let scroll = Scrollable::new_vertical().with_child(Box::new(inner));
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
    }

    /// Dispatch to the per-category pane body.
    fn build_pane_body(&self) -> Box<dyn Widget> {
        match self.selected {
            Category::Network => panes::network::build(),
            Category::Bluetooth => panes::bluetooth::build(),
            Category::DeviceMesh => panes::device_mesh::build(),
            Category::Sound => panes::sound::build(),
            Category::Displays => panes::display::build(),
            Category::MouseKeyboard => panes::mouse_keyboard::build(),
            Category::KeyboardShortcuts => panes::keyboard_shortcuts::build(),
            Category::TimeDate => panes::time_date::build(),
            Category::Users => panes::users::build(),
            Category::Printers => panes::printers::build(),
            Category::Power => panes::power::build(),
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
            Category::RegionFormats => panes::region::build(),
            Category::Permissions => panes::permissions::build(),
            Category::Updates => panes::updates::build(),
            Category::About => panes::about::build(),
        }
    }

    /// Create the persistent [`AppShell`] with the current nav + content +
    /// search and the seeded shot state.
    fn build_shell(&self) -> AppShell {
        let nav = sidebar::build(self.selected, &self.search_query);
        let content = self.build_content();
        // A narrow (phone) layout keeps the selected category's CONTENT on
        // screen — the category list is one back-tap away, never the default a
        // collapse lands on. A `--shots` capture can pin a specific narrow
        // screen; otherwise the shell collapses to the content.
        let narrow_screen = self.narrow_shot_screen.unwrap_or(NarrowScreen::Content);
        let mut shell = AppShell::new(nav, content)
            .with_search(Self::build_search_field())
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
        let nav = sidebar::build(self.selected, &self.search_query);
        let content = self.build_content();
        if let Some(shell) = &mut self.shell {
            shell.set_nav(nav);
            shell.set_content(content);
        }
    }
}

impl Widget for SettingsApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            crate::actions::OPEN_PANE_OPERATION_TYPE,
            crate::actions::ACCOUNT_OPERATION_TYPE,
        ]
    }

    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        use automerge::{ReadDoc, Value, transaction::Transactable};

        // Category selection: each sidebar row writes `settings.select.<slug>`
        // = true on click. Reading it true here switches the active category
        // (via `select`, which swaps the shell's nav + content and flags a
        // rebuild). The first key seen wins; we clear it so the selection is a
        // one-shot edge, not a sticky state.
        for category in Category::ALL {
            let key = format!("settings.select.{}", category.slug());
            let pressed = matches!(
                state.get(path, key.as_str()),
                Ok(Some((Value::Scalar(s), _)))
                    if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if pressed {
                let _ = state.put(path, key.as_str(), automerge::ScalarValue::Boolean(false));
                self.select(*category);
            }
        }

        // The search field edits an Automerge `Text` object; ensure it exists
        // so typed characters actually persist (a `TextInput` only writes
        // through an existing Text object).
        let search_obj = crate::ensure_text_object(state, path, sidebar::SEARCH_KEY);
        // In `--shots` there is no event loop to type into the field, so a
        // pre-seeded query (via `with_search_shot`) is spliced into the object
        // once so the field visibly shows the query it filtered by.
        if !self.search_query.is_empty()
            && let Ok(existing) = state.text(&search_obj)
            && existing.is_empty()
        {
            let _ = state.splice_text(&search_obj, 0, 0, &self.search_query);
        }

        // Search query: the header's search field writes `settings.search`.
        // A changed value updates the filter and swaps the shell's (filtered)
        // nav list, then flags a rebuild. The field re-reads the same key on
        // its own `sync`, so the query text survives the rebuild.
        if let Some(q) = read_search(state, path)
            && q != self.search_query
        {
            self.search_query = q;
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
    /// delegating to the [`ResourceProvider`](qos_actions::ResourceProvider) impl
    /// in [`crate::actions`]. Level-triggered: recomputed each frame from the
    /// shared document.
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
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
        vec![Box::new(crate::search_provider::SettingsSearchProvider)]
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

/// Read the sidebar search query from the document. The field writes either a
/// plain string scalar or an Automerge `Text` object, so handle both.
fn read_search(state: &automerge::AutoCommit, path: &automerge::ObjId) -> Option<String> {
    use automerge::{ReadDoc, Value};
    match state.get(path, sidebar::SEARCH_KEY) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
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
        let theme = ui::Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
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
    fn search_query_updates_from_document_and_flags_rebuild() {
        use automerge::transaction::Transactable;
        let mut app = SettingsApp::new();
        layout_at(&mut app, 1200.0, 760.0);
        assert_eq!(app.search_query(), "", "starts with no query");

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        // The sidebar search field writes SEARCH_KEY; sync adopts it.
        let _ = doc.put(&root, sidebar::SEARCH_KEY, "night light");
        app.sync(&mut doc, &root);
        assert_eq!(app.search_query(), "night light");
        assert!(
            app.take_structure_dirty(),
            "a new query must flag a rebuild so the filtered sidebar recomposes"
        );
        // Re-syncing the same query is a no-op (no spurious rebuild).
        app.sync(&mut doc, &root);
        assert!(!app.take_structure_dirty());
    }

    #[test]
    fn seeded_search_shot_query_reaches_the_field() {
        use automerge::ReadDoc;
        let mut app = SettingsApp::with_selected(Category::Displays).with_search_shot("light");
        assert_eq!(app.search_query(), "light");
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(
            doc.get(&root, sidebar::SEARCH_KEY).unwrap().is_some(),
            "the seeded query is written to the field key"
        );
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
