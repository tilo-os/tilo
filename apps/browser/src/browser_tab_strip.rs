//! Browser-owned tab strip with a minimum-width scrolling window.
//! Generic `TabBar` users retain their existing fit-to-width behavior.
use automerge::{AutoCommit, ObjId};
use std::collections::{HashMap, HashSet};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::{
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{AppKind, TabBar, TabDescriptor, Widget},
};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;
const PINNED_WIDTH: f64 = 44.0;
const MIN_TAB_WIDTH: f64 = 120.0;
const NEW_TAB_WIDTH: f64 = 36.0;
pub struct BrowserTabStrip {
    inner: TabBar,
    tabs: Vec<TabDescriptor>,
    pinned: HashSet<u64>,
    visible_ids: Vec<u64>,
    viewport_width: f64,
    cursor: Point,
    viewport_dirty: bool,
    inner_node: Option<NodeId>,
}
impl BrowserTabStrip {
    pub fn new(tabs: Vec<TabDescriptor>, active: u64) -> Self {
        let mut inner = TabBar::new("browser", tabs.clone(), active);
        inner.set_show_maximize(false);
        Self {
            inner,
            tabs,
            pinned: HashSet::new(),
            visible_ids: Vec::new(),
            viewport_width: 0.0,
            cursor: Point::ORIGIN,
            viewport_dirty: false,
            inner_node: None,
        }
    }
    pub fn tabs(&self) -> &[TabDescriptor] {
        &self.tabs
    }
    pub fn tabs_mut(&mut self) -> &mut Vec<TabDescriptor> {
        &mut self.tabs
    }
    pub fn active_id(&self) -> u64 {
        self.inner.active_id()
    }
    pub fn set_active(&mut self, id: u64) {
        self.refresh_visible_for(id);
        self.inner.set_active(id);
        self.refresh_visible()
    }
    pub fn is_pinned(&self, id: u64) -> bool {
        self.pinned.contains(&id)
    }
    pub fn set_pinned(&mut self, id: u64, value: bool) {
        if value {
            self.pinned.insert(id);
        } else {
            self.pinned.remove(&id);
        }
        self.refresh_visible()
    }
    pub fn remove_tab(&mut self, id: u64) -> Option<usize> {
        let i = self.tabs.iter().position(|t| t.id == id)?;
        self.tabs.remove(i);
        self.pinned.remove(&id);
        self.inner.remove_tab(id);
        self.refresh_visible();
        Some(i)
    }
    pub fn start_closing(&mut self, id: u64) -> bool {
        if !self.inner.start_closing(id) {
            return false;
        }
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.tabs.remove(index);
        }
        true
    }
    pub fn finalize_close(&mut self, id: u64) -> bool {
        let changed = self.inner.finalize_close(id);
        self.refresh_visible();
        changed
    }
    pub fn has_closing(&self) -> bool {
        self.inner.has_closing()
    }
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from < self.tabs.len() && to < self.tabs.len() && from != to {
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            self.refresh_visible()
        }
    }
    pub fn take_close(&mut self) -> Option<u64> {
        self.inner.take_close()
    }
    pub fn take_new_tab(&mut self) -> Option<()> {
        self.inner.take_new_tab()
    }
    pub fn take_context_menu(&mut self) -> Option<(u64, Point)> {
        self.inner.take_context_menu()
    }
    pub fn tab_rects(&self) -> &[Rect] {
        self.inner.tab_rects()
    }
    pub fn visible_tab_id(&self, index: usize) -> Option<u64> {
        self.visible_ids.get(index).copied()
    }
    pub fn take_reorder(&mut self) -> Option<(usize, usize)> {
        let (from, to) = self.inner.take_reorder()?;
        let from_id = *self.visible_ids.get(from)?;
        let to_id = *self.visible_ids.get(to)?;
        Some((
            self.tabs.iter().position(|t| t.id == from_id)?,
            self.tabs.iter().position(|t| t.id == to_id)?,
        ))
    }
    pub fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.viewport_dirty) || self.inner.take_structure_dirty()
    }
    pub fn needs_animation_frame(&self) -> bool {
        self.inner.needs_animation_frame()
    }
    pub fn set_motion_reduced(&mut self, r: bool) {
        self.inner.set_motion_reduced(r)
    }
    fn refresh_visible(&mut self) {
        self.refresh_visible_for(self.inner.active_id());
    }
    fn refresh_visible_for(&mut self, active: u64) {
        let mut pinned: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| self.pinned.contains(&t.id))
            .cloned()
            .collect();
        let normal: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| !self.pinned.contains(&t.id))
            .cloned()
            .collect();
        let available = (self.viewport_width - NEW_TAB_WIDTH).max(PINNED_WIDTH);
        let active_pinned = pinned.iter().position(|tab| tab.id == active);
        let max_pinned = if active_pinned.is_some() {
            (available / PINNED_WIDTH).floor() as usize
        } else {
            ((available - MIN_TAB_WIDTH).max(0.0) / PINNED_WIDTH).floor() as usize
        };
        if pinned.len() > max_pinned {
            let focus = active_pinned.unwrap_or(0);
            let start = focus
                .saturating_add(1)
                .saturating_sub(max_pinned)
                .min(pinned.len() - max_pinned);
            pinned = pinned.into_iter().skip(start).take(max_pinned).collect();
        }
        let budget = (available - pinned.len() as f64 * PINNED_WIDTH).max(if normal.is_empty() {
            0.0
        } else {
            MIN_TAB_WIDTH
        });
        let slots = ((budget / MIN_TAB_WIDTH).floor() as usize).max(1);
        let ai = normal.iter().position(|t| t.id == active).unwrap_or(0);
        let start = if normal.len() <= slots {
            0
        } else {
            ai.saturating_add(1)
                .saturating_sub(slots)
                .min(normal.len() - slots)
        };
        let mut visible = pinned;
        visible.extend(normal.into_iter().skip(start).take(slots));
        for tab in &mut visible {
            tab.app_kind = Some(AppKind::Browser);
        }
        self.visible_ids = visible.iter().map(|t| t.id).collect();
        *self.inner.tabs_mut() = visible;
        for id in &self.visible_ids {
            if self.pinned.contains(id) {
                self.inner.pin(*id)
            } else {
                self.inner.unpin(*id)
            }
        }
        self.inner.set_active(active)
    }
    #[cfg(any(test, debug_assertions))]
    pub fn debug_layout(&mut self, w: f64) {
        self.set_viewport_width(w)
    }
    pub fn set_viewport_width(&mut self, w: f64) {
        self.viewport_width = w;
        self.refresh_visible()
    }
    #[cfg(any(test, debug_assertions))]
    pub fn active_is_visible(&self) -> bool {
        self.visible_ids.contains(&self.inner.active_id())
    }
    #[cfg(any(test, debug_assertions))]
    pub fn visible_ids(&self) -> &[u64] {
        &self.visible_ids
    }
    #[cfg(any(test, debug_assertions))]
    pub fn active_rect(&self) -> Option<Rect> {
        let index = self
            .visible_ids
            .iter()
            .position(|id| *id == self.inner.active_id())?;
        self.inner.tab_rects().get(index).copied()
    }
    #[cfg(any(test, debug_assertions))]
    pub fn new_tab_rect(&self) -> Rect {
        self.inner.new_tab_rect()
    }
    pub fn overflow_step(&mut self, forward: bool) {
        if self.tabs.is_empty() {
            return;
        }
        let current = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.inner.active_id())
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % self.tabs.len()
        } else {
            (current + self.tabs.len() - 1) % self.tabs.len()
        };
        self.set_active(self.tabs[next].id);
    }
}
impl Widget for BrowserTabStrip {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        self.inner.sync(s, p)
    }
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        theme: &Theme,
        n: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.refresh_visible();
        let child = self.inner.build_layout(t, theme, n);
        let _ = t.set_style(
            child,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(44.0),
                },
                ..Style::default()
            },
        );
        self.inner_node = Some(child);
        let node = ui::widget::TaffyTreeExt::parent_node(
            t,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(44.0),
                },
                ..Style::default()
            },
            &[child],
        );
        n.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, t: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(l) = t.layout(node) {
            let width = f64::from(l.size.width);
            if (width - self.viewport_width).abs() > 1.0 {
                self.viewport_width = width;
                self.viewport_dirty = true;
            }
        }
        if let Some(child) = self.inner_node {
            self.inner.apply_layout(t, child, origin)
        }
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        x: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) {
        self.inner.draw(s, x, tr, theme, p)
    }
    fn handle_event(
        &mut self,
        e: &WindowEvent,
        scale: f64,
        m: &winit::event::Modifiers,
        s: &mut AutoCommit,
        p: &ObjId,
        h: &mut bool,
        f: &mut Option<NodeId>,
        n: NodeId,
        t: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = e {
            self.cursor = Point::new(position.x / scale, position.y / scale);
        }
        if let WindowEvent::MouseWheel { delta, .. } = e
            && self.inner.bar_rect().contains(self.cursor)
        {
            let forward = match delta {
                winit::event::MouseScrollDelta::LineDelta(x, y) => {
                    *x < 0.0 || (*x == 0.0 && *y < 0.0)
                }
                winit::event::MouseScrollDelta::PixelDelta(pos) => {
                    pos.x < 0.0 || (pos.x == 0.0 && pos.y < 0.0)
                }
            };
            self.overflow_step(forward);
            *h = true;
            return;
        }
        self.inner.handle_event(e, scale, m, s, p, h, f, n, t)
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        a: &ui::a11y::Action,
        s: &mut AutoCommit,
        p: &ObjId,
    ) -> bool {
        self.inner.perform_semantic_action(target, a, s, p)
    }
    fn take_structure_dirty(&mut self) -> bool {
        BrowserTabStrip::take_structure_dirty(self)
    }
    fn needs_animation_frame(&self) -> bool {
        self.inner.needs_animation_frame()
    }
    fn set_motion_reduced(&mut self, r: bool) {
        self.inner.set_motion_reduced(r)
    }
    fn name(&self) -> &'static str {
        "BrowserTabStrip"
    }
    fn test_id(&self) -> Option<&str> {
        Some("browser.tab-strip")
    }
    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }
}
