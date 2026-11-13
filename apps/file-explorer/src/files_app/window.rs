//! The Files window: the shared state laid out in its shell, with the menus
//! the state opens drawn over it.

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
use ui::widget::DrawChild;

pub(crate) struct Chrome {
    pub(super) nav: NavShell,
    pub(super) context_menu: ContextMenu,
    pub(super) menu_commands: Vec<Option<FileCommand>>,
    pub(super) submenu: ContextMenu,
    pub(super) submenu_commands: Vec<Option<FileCommand>>,
}

pub(super) enum MenuChange {
    Main(ContextMenu, Vec<Option<FileCommand>>),
    Sub(ContextMenu, Vec<Option<FileCommand>>),
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

    pub(super) fn in_places_list(&self, point: Point) -> bool {
        self.nav.is_sidebar_open() && self.nav.geometry().sidebar_column.contains(point)
    }

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

    pub(super) fn show_places(&mut self) {
        self.nav.open_sidebar();
        self.nav.snap();
    }

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

pub struct FileExplorerWidget {
    pub(super) state: Rc<RefCell<FilesState>>,
    pub(super) chrome: Chrome,
    pub(super) live: Option<ui::framework::Live<crate::resource::FileExplorer>>,
    pub(super) presented: Option<(u64, u64)>,
}

impl Default for FileExplorerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl FileExplorerWidget {
    pub fn state(&self) -> std::cell::RefMut<'_, FilesState> {
        self.state.borrow_mut()
    }

    pub(super) fn with_state<T>(&mut self, f: impl FnOnce(&mut FilesState, &mut Chrome) -> T) -> T {
        let mut state = self.state.borrow_mut();
        let answer = f(&mut state, &mut self.chrome);
        let menus = std::mem::take(&mut state.menus);
        drop(state);
        self.chrome.adopt(menus);
        answer
    }

    #[cfg(test)]
    pub(crate) fn chrome(&mut self) -> &mut Chrome {
        self.with_state(|_, _| ());
        &mut self.chrome
    }

    pub(crate) fn follow(&mut self, live: ui::framework::Live<crate::resource::FileExplorer>) {
        self.live = Some(live);
        self.follow_document();
    }

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

    #[doc(hidden)]
    pub fn debug_show_places(&mut self) {
        self.chrome.show_places();
    }

    #[doc(hidden)]
    pub fn debug_places_open(&self) -> bool {
        self.chrome.nav.is_sidebar_open()
    }

    #[doc(hidden)]
    pub fn debug_content_rect(&self) -> vello::kurbo::Rect {
        self.chrome.nav.geometry().content_column
    }

    #[doc(hidden)]
    pub fn debug_sidebar_rect(&self) -> vello::kurbo::Rect {
        self.chrome.nav.geometry().sidebar_column
    }

    #[doc(hidden)]
    pub fn debug_body_mode(&self) -> SidebarPlacement {
        self.chrome.nav.geometry().sidebar
    }

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

impl ui::presence::capability::PublishesPresence for FileExplorerWidget {
    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        self.state.borrow().shared_selection()
    }
}

impl ui::undo::UndoControl for FileExplorerWidget {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> ui::UndoStatus) {
        self.state().undo.publish(&status());
    }

    fn take_undo_request(&mut self) -> Option<ui::UndoRequest> {
        self.state().undo.take_request()
    }
}

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
        self.with_state(|state, chrome| {
            state.update_toolbar();
            state.composed_once = true;
            state.refresh_places_pane(chrome);
            state.refresh_cloud_header(chrome);
            chrome.nav.set_content(state.build_content_pane(theme));
        });

        let nav_node = self.chrome.nav.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
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
        ui::outside_paint::note();
        self.with_state(|state, chrome| {
            state.poll_history_result();
            state.paint_background(theme);
            chrome
                .nav
                .draw_child(scene, transform, text_renderer, theme, presence);
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
        // The reveal slide keeps settling across frames, so the host keeps pumping them.
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
            &self.chrome.context_menu,
            &self.chrome.submenu,
        ];
        v
    }

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
            if let Some(invocation) = state.app_chooser.take() {
                return Some(invocation);
            }
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn has_action_control_bindings(&self) -> bool {
        ui::widget::controls::children_have_action_control_bindings(self)
    }
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
