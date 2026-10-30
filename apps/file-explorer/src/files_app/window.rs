//! The Files window: the shared [`FilesState`] laid out in its shell, with
//! the menus it opened shown over it. The view holds the state with the
//! window, so its effect handler and publications reach what the window
//! shows.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use ui::time::Instant;
use ui::widget::context_menu::ContextMenu;
use ui::widget::{
    AppModel, Collaborative, ModelPath, NavShell, SidebarPlacement, TaffyTreeExt, Widget,
};
use ui::{AppKind, AppWidget, IconDescriptor};
use vello::kurbo::Point;

use super::{FilesState, StagedScenario, native_observer};
use crate::command::FileCommand;
use crate::i18n::t;

/// The window's own widgets over the [`FilesState`]: the shell its panes
/// are composed into, and the menus shown over it.
pub(crate) struct Chrome {
    /// The window shell: `[ places | file view ]` under one header row.
    /// Held persistently (NOT rebuilt inside `build_layout`) so its docked
    /// state and eased reveal survive the per-structure-change rebuild of the
    /// panes — the two panes are swapped in via `NavShell::set_sidebar` /
    /// `set_content` each rebuild. A narrow window turns the places list into
    /// a drawer the shell summons.
    pub(super) nav: NavShell,
    /// Right-click menu, owned here (not in the rebuilt composition) so it
    /// stays open across frames. `menu_commands[i]` is the command to run
    /// for menu item `i` (None for separators).
    pub(super) context_menu: ContextMenu,
    pub(super) menu_commands: Vec<Option<FileCommand>>,
    /// The "New File ▸" flyout submenu, populated from the Templates dir when
    /// the main menu opens; empty (and closed) when there are no templates.
    pub(super) submenu: ContextMenu,
    pub(super) submenu_commands: Vec<Option<FileCommand>>,
}

/// A menu the state opened or closed, for the window to show.
pub(super) enum MenuChange {
    /// Show this menu, with the command of each row, as the main menu.
    Main(ContextMenu, Vec<Option<FileCommand>>),
    /// Show this menu, with the command of each row, beside the main one.
    Sub(ContextMenu, Vec<Option<FileCommand>>),
    /// Close the flyout beside the main menu.
    CloseSub,
}

impl Chrome {
    pub(super) fn new(nav: NavShell) -> Self {
        Self {
            nav,
            context_menu: ContextMenu::new(),
            menu_commands: Vec::new(),
            submenu: ContextMenu::new(),
            submenu_commands: Vec::new(),
        }
    }

    /// Whether `point` is over the places list (docked, or the places
    /// screen of a narrow window).
    pub(super) fn in_places_list(&self, point: Point) -> bool {
        self.nav.is_sidebar_open() && self.nav.geometry().sidebar_column.contains(point)
    }

    /// The content column below the shell's header row: where the file view
    /// and the empty, loading and error surfaces stand.
    pub(super) fn content_body_rect(&self) -> vello::kurbo::Rect {
        let geometry = self.nav.geometry();
        let column = geometry.content_column;
        vello::kurbo::Rect::new(
            column.x0,
            column.y0 + geometry.header_height,
            column.x1,
            column.y1,
        )
    }

    /// Bring the places list in front (a narrow window's places screen),
    /// settled deterministically for a headless screenshot. In the wide
    /// layout the places list is already docked.
    pub(super) fn show_places(&mut self) {
        self.nav.open_sidebar();
        self.nav.snap();
    }

    /// Forward an event to the three body children in visual order — chrome
    /// above, the responsive `nav` body, chrome below — stopping at the first
    /// that consumes it. `node_id`'s taffy children are `[above, nav, below]`
    /// (see `build_layout`).
    pub(super) fn forward_to_body(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        let Some(&nav_n) = children.first() else {
            return;
        };
        if !*ctx.handled {
            self.nav
                .handle_event(event, &mut ctx.reborrow(ctx.path, nav_n));
        }
    }

    /// Show the menus the state opened or closed since.
    pub(super) fn adopt(&mut self, changes: Vec<MenuChange>) {
        for change in changes {
            match change {
                MenuChange::Main(menu, commands) => {
                    self.context_menu = menu;
                    self.menu_commands = commands;
                }
                MenuChange::Sub(menu, commands) => {
                    self.submenu = menu;
                    self.submenu_commands = commands;
                }
                MenuChange::CloseSub => {
                    self.submenu.close();
                    self.submenu_commands.clear();
                }
            }
        }
    }
}

/// The Files window: the shared [`FilesState`] laid out in its shell, with
/// its menus over it.
pub struct FileExplorerWidget {
    pub(super) state: Rc<RefCell<FilesState>>,
    pub(super) chrome: Chrome,
    /// The places and the operations queue as the document holds them,
    /// followed as they change; `None` for a window outside the framework.
    pub(super) live: Option<ui::framework::Live<crate::resource::FileExplorer>>,
    pub(super) presented: Option<(u64, u64)>,
}

impl Default for FileExplorerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl FileExplorerWidget {
    /// What the window shows, shared with the view's effect handler and
    /// publications.
    pub fn state(&self) -> std::cell::RefMut<'_, FilesState> {
        self.state.borrow_mut()
    }

    /// Run `f` on the state and the window's widgets, then show the menus
    /// it opened or closed.
    pub(super) fn with_state<T>(&mut self, f: impl FnOnce(&mut FilesState, &mut Chrome) -> T) -> T {
        let mut state = self.state.borrow_mut();
        let answer = f(&mut state, &mut self.chrome);
        let menus = std::mem::take(&mut state.menus);
        drop(state);
        self.chrome.adopt(menus);
        answer
    }

    /// The window's widgets, with the menus the state opened since shown.
    #[cfg(test)]
    pub(crate) fn chrome(&mut self) -> &mut Chrome {
        self.with_state(|_, _| ());
        &mut self.chrome
    }

    /// Follow the document: the places and the operations queue as it holds
    /// them, from now on.
    pub(crate) fn follow(&mut self, live: ui::framework::Live<crate::resource::FileExplorer>) {
        self.live = Some(live);
        self.follow_document();
    }

    /// Show the places and the settled operations the document holds now,
    /// when it changed since; whether it did.
    fn follow_document(&mut self) -> bool {
        let Some(files) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        else {
            return false;
        };
        let mut state = self.state.borrow_mut();
        state.present_places(&files.bookmarks());
        state.follow_ops(&files.ops);
        true
    }

    /// Bring the places list in front (a narrow window's places screen),
    /// settled deterministically for a headless screenshot.
    #[doc(hidden)]
    pub fn debug_show_places(&mut self) {
        self.chrome.show_places();
    }

    /// Whether the places drawer / docked sidebar is currently revealed.
    #[doc(hidden)]
    pub fn debug_places_open(&self) -> bool {
        self.chrome.nav.is_sidebar_open()
    }

    /// The file-view pane's resolved rect — what a test asserts against to
    /// prove the file list is actually on screen.
    #[doc(hidden)]
    pub fn debug_content_rect(&self) -> vello::kurbo::Rect {
        self.chrome.nav.geometry().content_column
    }

    /// The docked sidebar's resolved rect — the drag-to-pin drop target.
    #[doc(hidden)]
    pub fn debug_sidebar_rect(&self) -> vello::kurbo::Rect {
        self.chrome.nav.geometry().sidebar_column
    }

    /// Where the places list is (docked beside the file view, or a drawer
    /// over it) — for tests asserting the sidebar reflows with the window
    /// width.
    #[doc(hidden)]
    pub fn debug_body_mode(&self) -> SidebarPlacement {
        self.chrome.nav.geometry().sidebar
    }

    /// The state the view's effect handler and publications share with
    /// the window.
    pub(crate) fn shared(&self) -> Rc<RefCell<FilesState>> {
        Rc::clone(&self.state)
    }
}

impl Collaborative for FileExplorerWidget {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.with_state(|state, _| state.take_collab_requests())
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.with_state(|state, _| state.handle_collab_event(event));
    }
}

/// The selected files and folders, for asking an agent about them: each
/// row's published resource as a whole item, in the order shown, and their
/// names listed.
impl ui::presence::capability::PublishesPresence for FileExplorerWidget {
    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        self.state.borrow().shared_selection()
    }
}

/// The window's undo, which the notice's and the menu's Undo ask for.
impl ui::undo::UndoControl for FileExplorerWidget {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> ui::UndoStatus) {
        self.state().undo.publish(&status());
    }

    fn take_undo_request(&mut self) -> Option<ui::UndoRequest> {
        self.state().undo.take_request()
    }
}

/// The window's search narrows the folder open: what a person types in the
/// bar filters the listing they are looking at, and nothing else.
impl ui::widget::Filterable for FileExplorerWidget {
    fn set_filter(&mut self, query: &str) {
        self.state().set_filter(query);
    }

    fn filter_count(&self) -> Option<ui::widget::FilterCount> {
        Some(self.state.borrow().find_count())
    }
}

impl Widget for FileExplorerWidget {
    ui::provides!(
        dyn ui::undo::UndoControl,
        dyn ui::widget::Collaborative,
        dyn ui::widget::controls::ActionControls,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::widget::Filterable
    );

    fn native_input_snapshot(
        &self,
        _doc: &AppModel,
        _path: &ModelPath,
    ) -> Option<serde_json::Value> {
        Some(native_observer::snapshot(self))
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        let followed = self.follow_document();
        followed | self.with_state(|state, _| state.poll_frame())
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.with_state(|files, chrome| {
            if let Some(StagedScenario(scenario)) = files.staged.take() {
                files.stage_scenario(chrome, scenario.as_deref());
            }
            files.sync_frame(chrome, state, path);
        });
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.state.borrow().wake_deadline(&self.chrome)
    }

    // The listing's poll and change reads are served here: the directory is
    // read again and compared, and only a listing that changed (or trusted
    // identities, or the collaborative catalogue) asks for a sync. Any other
    // due deadline is the sync's.
    fn serve_deadline(&mut self, now: Instant) -> bool {
        self.with_state(|state, chrome| {
            if state.other_deadline(chrome).is_some_and(|due| due <= now) {
                return true;
            }
            state.refresh_metadata_if_due(now)
        })
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Refresh the toolbar's context, then swap the freshly built body
        // panes into the persistent `nav`. `nav` and the toolbar it owns keep
        // their state across the swap (docked state; the path bar's
        // in-progress edit), so a rebuild triggered by navigation / sort
        // doesn't snap the body or the path bar back mid-interaction.
        self.with_state(|state, chrome| {
            state.update_toolbar();
            state.composed_once = true;
            state.refresh_places_pane(chrome);
            state.refresh_cloud_header(chrome);
            chrome.nav.set_content(state.build_content_pane(theme));
        });

        // The shell fills the window.
        let nav_node = self.chrome.nav.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a
        // different tree; the shell was just built in this one above.
        taffy.widget_node(style, &[nav_node], node_map, self)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.with_state(|state, chrome| {
            state.rect = ui::widget::layout_rect(taffy, node, origin);
            let base = vello::kurbo::Point::new(state.rect.x0, state.rect.y0);
            if let Ok(children) = taffy.children(node)
                && let Some(&nav_n) = children.first()
            {
                chrome.nav.apply_layout(taffy, nav_n, base);
                state.sync_toolbar_compact(chrome);
            }
        });
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.with_state(|state, chrome| {
            state.poll_share_result();
            state.poll_history_result();
            state.paint_background(theme);
            chrome
                .nav
                .draw(scene, transform, text_renderer, theme, presence);
            state.draw_rename_editor(scene, transform, text_renderer, theme, presence);
        });
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.with_state(|state, chrome| {
            state.draw_overlay_frame(chrome, scene, transform, text_renderer, theme, presence);
        });
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.with_state(|state, chrome| state.handle_window_event(chrome, event, ctx));
    }

    fn has_pending_visual_work(&self) -> bool {
        let mut pending = self
            .state
            .borrow()
            .thumbs
            .as_deref()
            .is_some_and(|thumbs| !thumbs.is_idle());
        self.for_each_child(&mut |child| pending |= child.has_pending_visual_work());
        pending
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = std::mem::take(&mut self.state().structure_dirty);
        d |= self.chrome.nav.take_structure_dirty();
        d
    }

    fn needs_animation_frame(&self) -> bool {
        // The body's docked→push / reveal slide must keep the host pumping
        // frames while it settles; the floating status bubble likewise while it
        // lingers / fades.
        let state = self.state.borrow();
        state.cloud.pending.is_some()
            || state.mounted_cloud.is_pending()
            || state.cloud.chooser.is_some()
            || state.cloud_import_pending()
            || self.chrome.nav.needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        self.state
            .borrow()
            .collaborative_record_pager
            .borrow()
            .has_more()
            || self.chrome.nav.needs_idle_sync()
    }

    /// What the shell changed, and the rename editor where it changed. The
    /// window's other paint (menus, the preview, the drag ghost) is in its
    /// overlay, which the host repaints whole.
    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        let state = self.state.borrow();
        if state.collab_panel.is_none() {
            self.chrome.nav.collect_damage(regions);
        }
        state.rename_paint.damage(regions);
    }

    fn mark_drawn(&mut self) {
        {
            let mut state = self.state.borrow_mut();
            state.rename_paint.present();
            state.rename_input.mark_drawn();
        }
        for child in self.children_mut() {
            child.mark_drawn();
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.state.borrow().collab_panel.is_some() {
            return Vec::new();
        }
        let v: Vec<&dyn Widget> = vec![
            &self.chrome.nav,
            // Standalone overlays are not taffy children, but they are semantic
            // children: expose their menu/item nodes while open (closed nodes carry
            // `hidden`, so assistive technology drops them).
            &self.chrome.context_menu,
            &self.chrome.submenu,
        ];
        v
    }

    /// The shell is laid out whether or not a panel covers it, so the
    /// retained layout check walks it even while the panel keeps it from
    /// routing.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.chrome.nav);
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.state.borrow().collab_panel.is_some() {
            return Vec::new();
        }
        let chrome = &mut self.chrome;
        let v: Vec<&mut (dyn Widget + '_)> = vec![
            &mut chrome.nav,
            &mut chrome.context_menu,
            &mut chrome.submenu,
        ];
        v
    }

    fn name(&self) -> &'static str {
        "FileExplorerWidget"
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.state.borrow().rect
    }

    fn traps_focus(&self) -> bool {
        let state = self.state.borrow();
        state.collab_panel.is_some()
    }

    fn is_focusable(&self) -> bool {
        self.state.borrow().collab_panel.is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        let mut state = self.state();
        state.collab_panel_focused = focused && state.collab_panel.is_some();
    }

    fn is_focused(&self) -> bool {
        let state = self.state.borrow();
        state.collab_panel.is_some() && state.collab_panel_focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.state.borrow().window_accessibility())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        self.with_state(|state, _| state.perform_window_action(target, action))
    }
}

impl ui::widget::controls::ActionControls for FileExplorerWidget {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        {
            let mut state = self.state();
            if let Some(invocation) = state.selected_open.control.take() {
                return Some(invocation);
            }
            if let Some(invocation) = state.app_chooser.take() {
                return Some(invocation);
            }
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn has_action_control_bindings(&self) -> bool {
        // Own accessibility actions are semantic; only children emit actions.
        ui::widget::controls::children_have_action_control_bindings(self)
    }
    // A control a caller invoked through the door (a toolbar button) queued
    // its command; apply it now, as the click it stands for would.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &ui::widget::controls::ControlResult,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::on_control_invocation_result(child, address, result);
        }
        self.with_state(|state, _| {
            if state.drain_commands() {
                state.structure_dirty = true;
            }
        });
    }
}

impl AppWidget for FileExplorerWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        static TITLE: OnceLock<String> = OnceLock::new();
        TITLE.get_or_init(|| t("files-app-title")).as_str()
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰉋")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Files)
    }
}
