//! Retained browser workspace: each tab owns one live engine endpoint and its chrome.

use crate::BrowserWidget;
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, TabBar, TabDescriptor, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

struct Tab {
    id: u64,
    widget: Box<BrowserWidget>,
}

/// A bounded collection of retained documents in one shared browser runtime.
pub struct BrowserTabs {
    tabs: Vec<Tab>,
    // Host node bindings remain alive until the next structure rebuild.
    retired: Vec<Box<BrowserWidget>>,
    active_id: u64,
    next_id: u64,
    bar: TabBar,
    closed: Vec<String>,
    rect: Rect,
    dirty: bool,
    focus_new_address: bool,
    layout_active_id: Option<u64>,
    address_focus_node: Option<NodeId>,
    cursor: Point,
}

impl BrowserTabs {
    pub fn new(mut widget: BrowserWidget) -> Self {
        widget.set_tab_identity(1);
        let mut bar = TabBar::new("browser", vec![TabDescriptor::new(1, "New tab")], 1);
        bar.set_show_maximize(false);
        Self {
            tabs: vec![Tab {
                id: 1,
                widget: Box::new(widget),
            }],
            retired: Vec::new(),
            active_id: 1,
            next_id: 2,
            bar,
            closed: Vec::new(),
            rect: Rect::ZERO,
            dirty: true,
            focus_new_address: false,
            layout_active_id: None,
            address_focus_node: None,
            cursor: Point::ORIGIN,
        }
    }
    fn active(&self) -> &BrowserWidget {
        // A workspace always contains its active tab, including after last close.
        #[allow(clippy::unwrap_used, reason = "workspace retains an active tab")]
        &self
            .tabs
            .iter()
            .find(|tab| tab.id == self.active_id)
            .unwrap()
            .widget
    }
    fn active_mut(&mut self) -> &mut BrowserWidget {
        #[allow(clippy::unwrap_used, reason = "workspace retains an active tab")]
        &mut self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == self.active_id)
            .unwrap()
            .widget
    }
    pub fn engine_handle(&self) -> ui::webview::SharedEngine {
        self.active().engine_handle()
    }
    pub fn open(&mut self, url: Option<String>, background: bool) -> Result<(), String> {
        if self.tabs.len() >= 32 {
            return Err("At most 32 tabs can be open".into());
        }
        let focus_address = url.is_none();
        let engine = self.active().engine_handle().create_tab()?;
        engine.set_visible(false);
        let headless = self.active().is_headless();
        let mut widget = BrowserWidget::with_engine(engine, headless)
            .with_initial_uri(url.unwrap_or_else(|| "about:blank".into()));
        widget.share_history_pause(self.active());
        let id = self.next_id;
        self.next_id += 1;
        widget.set_tab_identity(id);
        self.tabs.push(Tab {
            id,
            widget: Box::new(widget),
        });
        self.bar.tabs_mut().push(TabDescriptor::new(id, "Loading…"));
        if !background {
            self.activate(id);
            self.focus_new_address = focus_address;
        }
        self.dirty = true;
        Ok(())
    }
    fn activate(&mut self, id: u64) {
        if id == self.active_id || !self.tabs.iter().any(|tab| tab.id == id) {
            return;
        }
        self.active().engine_handle().set_visible(false);
        self.active_id = id;
        self.address_focus_node = None;
        self.bar.set_active(id);
        self.active().engine_handle().set_visible(true);
        self.active_mut().refresh_shared_history();
        self.dirty = true;
    }
    fn close(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        // Allocate a replacement before releasing the final process owner.
        if self.tabs.len() == 1 && self.open(None, false).is_err() {
            return;
        }
        if self.active_id == id {
            if let Some(next) = self
                .tabs
                .get(index + 1)
                .or_else(|| self.tabs.iter().find(|tab| tab.id != id))
            {
                self.activate(next.id);
            }
        }
        let tab = self.tabs.remove(index);
        if self.closed.len() == 20 {
            self.closed.remove(0);
        }
        self.closed.push(tab.widget.tab_url().to_owned());
        self.retired.push(tab.widget);
        self.bar.remove_tab(id);
        self.bar.finalize_close(id);
        self.bar.set_active(self.active_id);
        self.dirty = true;
    }
    fn reopen(&mut self) -> Result<(), String> {
        if let Some(url) = self.closed.last().cloned() {
            self.open(Some(url), false)?;
            self.closed.pop();
        }
        Ok(())
    }
    fn reorder(&mut self, from: usize, to: usize) {
        if from < self.tabs.len() && to < self.tabs.len() && from != to {
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            self.bar.reorder(from, to);
            self.dirty = true;
        }
    }
    fn cycle(&mut self, backwards: bool) {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        let next = if backwards {
            (index + self.tabs.len() - 1) % self.tabs.len()
        } else {
            (index + 1) % self.tabs.len()
        };
        if let Some(tab) = self.tabs.get(next) {
            self.activate(tab.id);
        }
    }
    fn shortcut(&mut self, key: &Key, modifiers: winit::keyboard::ModifiersState) -> bool {
        let platform = modifiers.super_key() || modifiers.control_key();
        let shift = modifiers.shift_key();
        match key {
            Key::Character(c) if platform && c.eq_ignore_ascii_case("t") => {
                let result = if shift {
                    self.reopen()
                } else {
                    self.open(None, false)
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "Cannot open tab");
                }
            }
            Key::Character(c) if platform && c.eq_ignore_ascii_case("w") => {
                self.close(self.active_id)
            }
            Key::Named(NamedKey::Tab) if modifiers.control_key() => self.cycle(shift),
            Key::Character(c)
                if platform && shift && matches!(c.as_str(), "[" | "]" | "{" | "}") =>
            {
                self.cycle(matches!(c.as_str(), "[" | "{"))
            }
            Key::Character(c)
                if platform
                    && c.len() == 1
                    && c.parse::<usize>().is_ok_and(|n| (1..=9).contains(&n)) =>
            {
                let n = c.parse::<usize>().unwrap_or(1);
                let tab = if n == 9 {
                    self.tabs.last()
                } else {
                    self.tabs.get(n - 1)
                };
                if let Some(tab) = tab {
                    self.activate(tab.id);
                }
            }
            _ => return false,
        }
        true
    }
}

impl Widget for BrowserTabs {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let mut requests = Vec::new();
        for tab in &mut self.tabs {
            tab.widget.sync(state, path);
            requests.extend(tab.widget.engine_handle().take_new_tab_requests());
            let title = tab.widget.tab_title();
            if let Some(desc) = self
                .bar
                .tabs_mut()
                .iter_mut()
                .find(|desc| desc.id == tab.id)
            {
                if desc.title != title {
                    desc.title = title;
                    self.dirty = true;
                }
            }
        }
        for (url, background) in requests {
            if let Err(error) = self.open(Some(url), background) {
                tracing::warn!(%error, "Cannot open link tab");
            }
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.retired.clear();
        let bar = self.bar.build_layout(taffy, theme, nodes);
        let page = self.active_mut().build_layout(taffy, theme, nodes);
        self.layout_active_id = Some(self.active_id);
        let focus_address = self.focus_new_address || self.address_focus_node.is_some();
        self.address_focus_node = None;
        if focus_address && self.active_mut().focus_tab_address() {
            self.focus_new_address = false;
            self.address_focus_node = nodes.iter().find_map(|(id, widget)| {
                // Nodes were built immediately above and all widgets are retained.
                let widget = unsafe { &**widget };
                (widget.test_id() == Some("browser.url_input")).then_some(*id)
            });
        }
        let root = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Style::default()
            },
            &[bar, page],
        );
        nodes.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        if let Ok(children) = taffy.children(node) {
            if let Some(bar) = children.first() {
                self.bar.apply_layout(taffy, *bar, self.rect.origin());
            }
            if let Some(page) = children.get(1) {
                let origin = self.rect.origin();
                self.active_mut().apply_layout(taffy, *page, origin);
            }
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.bar.draw(scene, transform, tr, theme, presence);
        self.active_mut()
            .draw(scene, transform, tr, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if *handled {
            return;
        }
        // A switch can be followed by another input before the host rebuilds.
        // Retained compositions still contain NodeIds from their previous tree.
        if self.layout_active_id != Some(self.active_id) {
            *handled = true;
            return;
        }
        if let Some(address) = self.address_focus_node.take() {
            *focus = Some(address);
        }
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::ModifiersChanged(_)
        ) {
            let m = modifiers.state();
            self.active()
                .engine_handle()
                .set_modifiers(ui::webview::KeyModifiers {
                    ctrl: m.control_key(),
                    shift: m.shift_key(),
                    alt: m.alt_key(),
                    meta: m.super_key(),
                });
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale, position.y / scale);
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if key.state == ElementState::Pressed
                && self.shortcut(&key.logical_key, modifiers.state())
            {
                *handled = true;
                return;
            }
        }
        let Ok(children) = taffy.children(node) else {
            return;
        };
        if let Some(bar) = children.first() {
            self.bar.handle_event(
                event, scale, modifiers, state, path, handled, focus, *bar, taffy,
            );
        }
        if let Some(id) = self.bar.take_close() {
            self.close(id);
            *handled = true;
            return;
        }
        if self.bar.take_new_tab().is_some() {
            if let Err(error) = self.open(None, false) {
                tracing::warn!(%error, "Cannot open tab");
            }
            *handled = true;
            return;
        }
        if let Some((from, to)) = self.bar.take_reorder() {
            self.reorder(from, to);
        }
        let selected = self.bar.active_id();
        if selected != self.active_id {
            self.activate(selected);
            *handled = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Middle,
            ..
        } = event
        {
            if let Some(index) = self
                .bar
                .tab_rects()
                .iter()
                .position(|rect| rect.contains(self.cursor))
            {
                if let Some(tab) = self.tabs.get(index) {
                    self.close(tab.id);
                    *handled = true;
                    return;
                }
            }
        }
        if !*handled {
            if let Some(page) = children.get(1) {
                self.active_mut().handle_event(
                    event, scale, modifiers, state, path, handled, focus, *page, taffy,
                );
            }
        }
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if self
            .bar
            .perform_semantic_action(target, action, state, path)
        {
            self.activate(self.bar.active_id());
            return true;
        }
        self.active_mut()
            .perform_semantic_action(target, action, state, path)
    }
    fn poll_resource_operation(
        &mut self,
        invocation: ui::qos_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<ui::qos_actions::InvocationUpdate> {
        self.tabs.iter_mut().find_map(|tab| {
            tab.widget
                .poll_resource_operation(invocation, cancellation_requested, doc, path)
        })
    }
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        self.active().search_providers()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.bar, self.active()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let active_id = self.active_id;
        let mut children: Vec<&mut dyn Widget> = vec![&mut self.bar];
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            children.push(tab.widget.as_mut());
        }
        children
    }
    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.dirty);
        let page = self.active_mut().take_structure_dirty();
        dirty || page || self.bar.take_structure_dirty()
    }
    fn needs_animation_frame(&self) -> bool {
        self.bar.needs_animation_frame() || self.active().needs_animation_frame()
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.bar.set_motion_reduced(reduced);
        for tab in &mut self.tabs {
            tab.widget.set_motion_reduced(reduced);
        }
    }
    fn take_control_invocation(&mut self) -> Option<ui::qos_actions::ControlInvocation> {
        self.active_mut().take_control_invocation()
    }
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.tabs
            .iter_mut()
            .find_map(|tab| tab.widget.take_floating_surface_request())
    }
    fn take_capture_studio_request(&mut self) -> Option<ui::widget::CaptureMode> {
        self.active_mut().take_capture_studio_request()
    }
    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.tabs
            .iter_mut()
            .flat_map(|tab| tab.widget.take_floating_surface_source_updates())
            .collect()
    }
    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.widget.active_floating_surface_ids())
            .collect()
    }
    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for tab in &mut self.tabs {
            tab.widget.on_floating_surface_host_message(message);
        }
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.active().modal_capture()
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.active().cursor_shape(point)
    }
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        self.active().continuation_metadata(state, root)
    }
    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        self.active_mut()
            .restore_continuation_metadata(metadata, state, root)
    }
    fn name(&self) -> &'static str {
        "BrowserTabs"
    }
    fn test_id(&self) -> Option<&str> {
        Some("browser.workspace")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
impl Drop for BrowserTabs {
    fn drop(&mut self) {
        self.active_mut().clear_history_on_workspace_exit();
    }
}

impl AppWidget for BrowserTabs {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        self.active().app_title()
    }
    fn app_icon(&self) -> IconDescriptor {
        self.active().app_icon()
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrowserWidget;
    use std::sync::Arc;
    use ui::webview::test_engine::TestEngine;
    use ui::webview::{FrameSlot, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};
    struct Engine(Arc<TestEngine>);
    impl WebViewEngine for Engine {
        fn create_tab(&self) -> Result<SharedEngine, String> {
            Ok(Arc::new(Self(TestEngine::new())))
        }
        fn send(&self, c: WebViewCommand) {
            self.0.send(c);
        }
        fn frames(&self) -> FrameSlot {
            self.0.frames()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            self.0.drain_events()
        }
    }
    fn workspace() -> BrowserTabs {
        BrowserTabs::new(BrowserWidget::with_engine(
            Arc::new(Engine(TestEngine::new())),
            true,
        ))
    }
    #[test]
    fn switching_retains_independent_documents_and_reorder_keeps_identity() {
        let mut tabs = workspace();
        let first = tabs.active_id;
        let first_engine = tabs.active().engine_handle();
        tabs.open(Some("https://two.test".into()), false).unwrap();
        let second = tabs.active_id;
        assert_ne!(first, second);
        assert!(!Arc::ptr_eq(&first_engine, &tabs.active().engine_handle()));
        tabs.activate(first);
        assert!(Arc::ptr_eq(&first_engine, &tabs.active().engine_handle()));
        tabs.reorder(0, 1);
        assert_eq!(tabs.active_id, first);
        assert_eq!(tabs.tabs[1].id, first);
    }
    #[test]
    fn reorder_keeps_layout_widget_addresses_stable() {
        let mut tabs = workspace();
        tabs.open(None, true).unwrap();
        let before = std::ptr::from_ref(tabs.active());
        tabs.reorder(0, 1);
        assert_eq!(std::ptr::from_ref(tabs.active()), before);
    }

    #[test]
    fn close_reopen_and_last_tab_preserve_a_usable_workspace() {
        let mut tabs = workspace();
        tabs.open(Some("https://two.test".into()), false).unwrap();
        let closed = tabs.active_id;
        tabs.close(closed);
        assert_eq!(tabs.tabs.len(), 1);
        tabs.reopen().unwrap();
        assert_eq!(tabs.tabs.len(), 2);
        assert_ne!(tabs.active_id, closed);
        assert_eq!(tabs.active().tab_url(), "https://two.test");
        let ids: Vec<_> = tabs.tabs.iter().map(|tab| tab.id).collect();
        for id in ids {
            tabs.close(id);
        }
        assert_eq!(tabs.tabs.len(), 1);
    }
    #[test]
    fn background_tabs_do_not_steal_selection_and_limit_is_bounded() {
        let mut tabs = workspace();
        let first = tabs.active_id;
        for _ in 1..32 {
            tabs.open(None, true).unwrap();
        }
        assert_eq!(tabs.active_id, first);
        assert!(tabs.open(None, true).is_err());
        assert_eq!(tabs.tabs.len(), 32);
    }
    #[test]
    fn resources_are_owned_only_by_the_page() {
        let tabs = workspace();
        assert!(tabs.resources(&AutoCommit::new()).is_empty());
        assert!(!tabs.active().resources(&AutoCommit::new()).is_empty());
    }

    #[test]
    fn new_tab_address_is_focused_after_layout() {
        let mut tabs = workspace();
        tabs.open(None, false).unwrap();
        let mut doc = AutoCommit::new();
        tabs.sync(&mut doc, &automerge::ROOT);
        let mut tree = TaffyTree::new();
        tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
        fn focused(widget: &dyn Widget) -> bool {
            (widget.test_id() == Some("browser.url_input") && widget.is_focused())
                || widget.children().iter().any(|child| focused(*child))
        }
        assert!(focused(&tabs));
    }

    #[test]
    fn switching_before_rebuild_does_not_dispatch_stale_layout_nodes() {
        let mut tabs = workspace();
        let mut tree = TaffyTree::new();
        for _ in 0..128 {
            tree.new_leaf(Style::default()).unwrap();
        }
        tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
        tabs.open(None, false).unwrap();
        let mut tree = TaffyTree::new();
        let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
        tabs.activate(1);
        tabs.handle_event(
            &WindowEvent::Focused(true),
            1.0,
            &Default::default(),
            &mut AutoCommit::new(),
            &automerge::ROOT,
            &mut false,
            &mut None,
            root,
            &tree,
        );
    }

    #[test]
    fn history_pause_is_shared_by_existing_and_new_tabs() {
        let mut tabs = workspace();
        tabs.open(None, true).unwrap();
        tabs.active_mut().debug_show_history_paused();
        assert!(tabs.tabs.iter().all(|tab| tab.widget.history_paused()));
        tabs.open(None, false).unwrap();
        assert!(tabs.active().history_paused());
    }

    #[test]
    fn background_navigation_keeps_its_invocation_route() {
        let mut tabs = workspace();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        tabs.sync(&mut doc, &root);
        let resources = tabs.active().resources(&doc);
        let page = resources[0]
            .children
            .iter()
            .find(|r| r.id.0 == "browser.page")
            .unwrap();
        let navigate = page
            .actions
            .iter()
            .find(|action| action.id == "navigate")
            .unwrap();
        let args = serde_json::from_value(serde_json::json!({"url": "https://one.test"})).unwrap();
        let encoded = navigate.build_json(&args).unwrap();
        let invocation = ui::qos_actions::InvocationId::new();
        tabs.start_resource_operation(
            invocation,
            navigate.kind,
            &encoded.type_id,
            &encoded.payload,
            &doc,
            &root,
        )
        .unwrap();
        tabs.open(None, false).unwrap();
        tabs.sync(&mut doc, &root);
        assert!(matches!(
            tabs.poll_resource_operation(invocation, false, &doc, &root),
            Some(ui::qos_actions::InvocationUpdate::Succeeded { .. })
        ));
    }
    #[test]
    fn pip_host_adoption_and_close_route_to_original_background_tab() {
        let mut tabs = workspace();
        let first = tabs.active_id;
        tabs.active_mut().debug_show_active_pip();
        let id = tabs.active().active_floating_surface_ids()[0].clone();
        tabs.open(None, false).unwrap();
        assert_ne!(tabs.active_id, first);
        tabs.on_floating_surface_host_message(
            &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                id: id.clone(),
                rect: [0.0, 0.0, 400.0, 300.0],
            },
        );
        assert!(tabs.active().active_floating_surface_ids().is_empty());
        assert_eq!(tabs.active_floating_surface_ids(), vec![id.clone()]);
        tabs.active_mut().debug_show_active_pip();
        let second = tabs.active().active_floating_surface_ids()[0].clone();
        assert_ne!(id, second);
        tabs.on_floating_surface_host_message(
            &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id },
        );
        assert_eq!(tabs.active_floating_surface_ids(), vec![second]);
    }
    #[test]
    fn background_pip_delivers_request_and_completes_invocation() {
        let engine = TestEngine::new();
        engine.set_media_element(Some(ui::webview::MediaElementInfo {
            element_id: "media:one".into(),
            viewport_rect: [0.1, 0.1, 0.8, 0.8],
            intrinsic_size: [1920, 1080],
            label: "Video".into(),
        }));
        let mut tabs = BrowserTabs::new(BrowserWidget::with_engine(Arc::new(Engine(engine)), true));
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        tabs.sync(&mut doc, &root);
        let mut tree = TaffyTree::new();
        let node = tree
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(900.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            })
            .unwrap();
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .unwrap();
        tabs.active_mut().apply_layout(&tree, node, Point::ORIGIN);
        let resources = tabs.active().resources(&doc);
        let page = resources[0]
            .children
            .iter()
            .find(|r| r.id.0 == "browser.page")
            .unwrap();
        let pip = page
            .actions
            .iter()
            .find(|action| action.id == "picture-in-picture")
            .unwrap();
        let invocation = ui::qos_actions::InvocationId::new();
        tabs.start_resource_operation(
            invocation,
            pip.kind,
            pip.operation_type.as_deref().unwrap(),
            br#"{"operation":"toggle-picture-in-picture"}"#,
            &doc,
            &root,
        )
        .unwrap();
        tabs.open(None, false).unwrap();
        tabs.sync(&mut doc, &root);
        let request = tabs
            .take_floating_surface_request()
            .expect("background tab request is delivered");
        assert_eq!(request.id, "browser.media-pip.1");
        assert!(matches!(
            tabs.poll_resource_operation(invocation, false, &doc, &root),
            Some(ui::qos_actions::InvocationUpdate::Succeeded { .. })
        ));
    }
}
