//! Browser-owned tab strip with a minimum-width scrolling window.
//! Generic `TabBar` users retain their existing fit-to-width behavior.
use automerge::{AutoCommit, ObjId};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::{
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{AppKind, TabBar, TabDescriptor, Widget},
};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, Stroke},
    peniko::{Fill, ImageBrush},
};
use winit::event::{ElementState, MouseButton, WindowEvent};
const PINNED_WIDTH: f64 = 44.0;
const MIN_TAB_WIDTH: f64 = 120.0;
const NEW_TAB_WIDTH: f64 = 36.0;
const OVERFLOW_WIDTH: f64 = 24.0;
#[derive(Clone, Default)]
pub(crate) struct TabVisual {
    pub favicon: Option<vello::peniko::ImageData>,
    pub loading: bool,
    pub muted: bool,
    pub media_playing: bool,
}
pub struct BrowserTabStrip {
    inner: Box<TabBar>,
    tabs: Vec<TabDescriptor>,
    pinned: HashSet<u64>,
    visible_ids: Vec<u64>,
    viewport_width: f64,
    cursor: Point,
    viewport_dirty: bool,
    inner_node: Option<NodeId>,
    visuals: HashMap<u64, TabVisual>,
    pending_toggle_mute: Option<u64>,
    reduced_motion: bool,
    spinner_started: std::time::Instant,
    rect: Rect,
}
impl BrowserTabStrip {
    pub fn new(tabs: Vec<TabDescriptor>, active: u64) -> Self {
        let mut inner = Box::new(TabBar::new("browser", tabs.clone(), active));
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
            visuals: HashMap::new(),
            pending_toggle_mute: None,
            reduced_motion: false,
            spinner_started: std::time::Instant::now(),
            rect: Rect::ZERO,
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
        self.visuals.remove(&id);
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
    pub(crate) fn set_visual(&mut self, id: u64, visual: TabVisual) {
        self.visuals.insert(id, visual);
    }
    pub fn set_fixture_visual(
        &mut self,
        id: u64,
        favicon: Option<vello::peniko::ImageData>,
        loading: bool,
        media_playing: bool,
        muted: bool,
    ) {
        self.set_visual(
            id,
            TabVisual {
                favicon,
                loading,
                muted,
                media_playing,
            },
        );
    }
    #[cfg(test)]
    pub fn visual_count(&self) -> usize {
        self.visuals.len()
    }
    pub fn take_toggle_mute(&mut self) -> Option<u64> {
        self.pending_toggle_mute.take()
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
        let available =
            (self.viewport_width - NEW_TAB_WIDTH - 2.0 * OVERFLOW_WIDTH).max(PINNED_WIDTH);
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
    pub fn controls_are_outside_tabs(&self) -> bool {
        let (left, _) = self.overflow_rects();
        left.x0 >= self.inner.bar_rect().x1 - 0.5 && self.inner.new_tab_rect().x1 <= left.x0 + 0.5
    }
    #[cfg(any(test, debug_assertions))]
    pub fn media_control_precedes_title(&self, id: u64) -> bool {
        let Some(index) = self.visible_ids.iter().position(|visible| *visible == id) else {
            return false;
        };
        self.pinned.contains(&id)
            || self.media_rect(index).x1
                <= self
                    .inner
                    .tab_rects()
                    .get(index)
                    .map_or(0.0, |tab| tab.x0 + 32.0)
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
    fn overflow_rects(&self) -> (Rect, Rect) {
        let anchor = self.rect.x1;
        let y0 = self.rect.y0 + 6.0;
        let y1 = self.rect.y1 - 6.0;
        (
            Rect::new(
                anchor - 2.0 * OVERFLOW_WIDTH,
                y0,
                anchor - OVERFLOW_WIDTH,
                y1,
            ),
            Rect::new(anchor - OVERFLOW_WIDTH, y0, anchor, y1),
        )
    }
    fn media_rect(&self, index: usize) -> Rect {
        let Some(id) = self.visible_ids.get(index) else {
            return Rect::ZERO;
        };
        self.leading_rect(index, *id)
    }
    fn leading_rect(&self, index: usize, id: u64) -> Rect {
        let Some(tab) = self.inner.tab_rects().get(index) else {
            return Rect::ZERO;
        };
        let x0 = if self.pinned.contains(&id) {
            tab.center().x - 8.0
        } else {
            tab.x0 + 9.0
        };
        Rect::new(x0, tab.center().y - 8.0, x0 + 16.0, tab.center().y + 8.0)
    }
    fn has_hidden_before(&self) -> bool {
        self.partition_has_hidden(false, true) || self.partition_has_hidden(true, true)
    }
    fn has_hidden_after(&self) -> bool {
        self.partition_has_hidden(false, false) || self.partition_has_hidden(true, false)
    }
    fn partition_has_hidden(&self, pinned: bool, before: bool) -> bool {
        let partition: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| self.pinned.contains(&tab.id) == pinned)
            .map(|tab| tab.id)
            .collect();
        let visible: Vec<_> = self
            .visible_ids
            .iter()
            .copied()
            .filter(|id| self.pinned.contains(id) == pinned)
            .collect();
        let Some(edge) = (if before {
            visible.first()
        } else {
            visible.last()
        }) else {
            return !partition.is_empty();
        };
        partition
            .iter()
            .position(|id| id == edge)
            .is_some_and(|index| {
                if before {
                    index > 0
                } else {
                    index + 1 < partition.len()
                }
            })
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
                    width: Dimension::auto(),
                    height: Dimension::length(44.0),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(44.0),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Style::default()
            },
        );
        self.inner_node = Some(child);
        let controls = ui::widget::TaffyTreeExt::leaf_node(
            t,
            Style {
                size: Size {
                    width: Dimension::length(48.0),
                    height: Dimension::length(44.0),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                ..Style::default()
            },
        );
        let node = ui::widget::TaffyTreeExt::parent_node(
            t,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(44.0),
                },
                ..Style::default()
            },
            &[child, controls],
        );
        n.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, t: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(l) = t.layout(node) {
            let width = f64::from(l.size.width);
            self.rect = Rect::from_origin_size(
                (
                    origin.x + f64::from(l.location.x),
                    origin.y + f64::from(l.location.y),
                ),
                (width, f64::from(l.size.height)),
            );
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
        self.inner.draw(s, x, tr, theme, p);
        let ink = theme.tokens.colors.text_primary;
        for (index, id) in self.visible_ids.iter().copied().enumerate() {
            let Some(visual) = self.visuals.get(&id) else {
                continue;
            };
            let icon = self.leading_rect(index, id);
            if visual.loading || visual.favicon.is_some() || visual.media_playing || visual.muted {
                s.fill(
                    Fill::NonZero,
                    x,
                    theme.tokens.colors.surfaces.base,
                    None,
                    &icon.inflate(3.0, 3.0).to_rounded_rect(10.0),
                );
            }
            if visual.media_playing || visual.muted {
                let c = icon.center();
                s.fill(
                    Fill::NonZero,
                    x,
                    ink,
                    None,
                    &Rect::new(c.x - 7.0, c.y - 4.0, c.x - 3.0, c.y + 4.0),
                );
                let mut wave = BezPath::new();
                wave.move_to((c.x - 3.0, c.y - 4.0));
                wave.line_to((c.x + 2.0, c.y - 7.0));
                wave.line_to((c.x + 2.0, c.y + 7.0));
                wave.line_to((c.x - 3.0, c.y + 4.0));
                wave.close_path();
                s.fill(Fill::NonZero, x, ink, None, &wave);
                if visual.muted {
                    let mut slash = BezPath::new();
                    slash.move_to((c.x - 7.0, c.y - 7.0));
                    slash.line_to((c.x + 7.0, c.y + 7.0));
                    // The muted slash reads in the theme's danger tone, so it
                    // follows the palette (and stays legible in high contrast)
                    // rather than a red chosen for one theme.
                    s.stroke(
                        &Stroke::new(2.0),
                        x,
                        theme.tokens.colors.semantic.danger,
                        None,
                        &slash,
                    );
                }
            } else if visual.loading {
                let phase = if self.reduced_motion {
                    0.0
                } else {
                    (self.spinner_started.elapsed().as_millis() % 1000) as f64 / 1000.0
                };
                let mut arc = BezPath::new();
                let c = icon.center();
                let start = phase * std::f64::consts::TAU;
                for step in 0..=12 {
                    let a = start + (step as f64 / 12.0) * std::f64::consts::TAU * 0.72;
                    let point = Point::new(c.x + a.cos() * 6.5, c.y + a.sin() * 6.5);
                    if step == 0 {
                        arc.move_to(point);
                    } else {
                        arc.line_to(point);
                    }
                }
                s.stroke(&Stroke::new(2.0), x, ink, None, &arc);
            } else if let Some(image) = &visual.favicon {
                let brush = ImageBrush::new(image.clone());
                let sx = icon.width() / f64::from(image.width.max(1));
                let sy = icon.height() / f64::from(image.height.max(1));
                let bx = Affine::translate((icon.x0, icon.y0)) * Affine::scale_non_uniform(sx, sy);
                s.fill(Fill::NonZero, x, &brush, Some(bx), &icon);
            }
        }
        let (left, right) = self.overflow_rects();
        for (rect, enabled, forward) in [
            (left, self.has_hidden_before(), false),
            (right, self.has_hidden_after(), true),
        ] {
            s.fill(
                Fill::NonZero,
                x,
                theme.tokens.colors.surfaces.raised,
                None,
                &rect.to_rounded_rect(6.0),
            );
            let c = rect.center();
            let mut arrow = BezPath::new();
            if forward {
                arrow.move_to((c.x - 2.0, c.y - 5.0));
                arrow.line_to((c.x + 3.0, c.y));
                arrow.line_to((c.x - 2.0, c.y + 5.0));
            } else {
                arrow.move_to((c.x + 2.0, c.y - 5.0));
                arrow.line_to((c.x - 3.0, c.y));
                arrow.line_to((c.x + 2.0, c.y + 5.0));
            }
            s.stroke(
                &Stroke::new(2.0),
                x,
                if enabled {
                    ink
                } else {
                    theme.tokens.colors.text_dim
                },
                None,
                &arrow,
            );
        }
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
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = e
        {
            let (left, right) = self.overflow_rects();
            if left.contains(self.cursor) && self.has_hidden_before() {
                self.overflow_step(false);
                *h = true;
                return;
            }
            if right.contains(self.cursor) && self.has_hidden_after() {
                self.overflow_step(true);
                *h = true;
                return;
            }
            for (index, id) in self.visible_ids.iter().copied().enumerate() {
                if self
                    .visuals
                    .get(&id)
                    .is_some_and(|v| v.media_playing || v.muted)
                    && self.media_rect(index).contains(self.cursor)
                {
                    self.pending_toggle_mute = Some(id);
                    *h = true;
                    return;
                }
            }
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
        if matches!(a, ui::a11y::Action::Activate) {
            if target == "tab-overflow-before" && self.has_hidden_before() {
                self.overflow_step(false);
                return true;
            }
            if target == "tab-overflow-after" && self.has_hidden_after() {
                self.overflow_step(true);
                return true;
            }
            if let Some(id) = target
                .strip_prefix("tab-mute:")
                .and_then(|v| v.parse().ok())
            {
                self.pending_toggle_mute = Some(id);
                return true;
            }
        }
        self.inner.perform_semantic_action(target, a, s, p)
    }
    fn take_structure_dirty(&mut self) -> bool {
        BrowserTabStrip::take_structure_dirty(self)
    }
    fn needs_animation_frame(&self) -> bool {
        self.inner.needs_animation_frame()
            || (!self.reduced_motion && self.visuals.values().any(|v| v.loading))
    }
    fn set_motion_reduced(&mut self, r: bool) {
        self.reduced_motion = r;
        self.inner.set_motion_reduced(r)
    }
    fn name(&self) -> &'static str {
        "BrowserTabStrip"
    }
    fn test_id(&self) -> Option<&str> {
        Some("browser.tab-strip")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut node = self.inner.accessibility()?;
        for (index, child) in node.children.iter_mut().enumerate() {
            let identity = child
                .action_target
                .clone()
                .unwrap_or_else(|| format!("tab-child:{index}"));
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            identity.hash(&mut hasher);
            child.key = Some(hasher.finish());
        }
        for (index, id) in self.visible_ids.iter().copied().enumerate() {
            if let Some(visual) = self.visuals.get(&id) {
                if let Some(tab) = node
                    .children
                    .iter_mut()
                    .find(|n| n.action_target.as_deref() == Some(&format!("tab:{id}")))
                {
                    tab.state.busy = visual.loading;
                    if visual.media_playing {
                        tab.description = Some("Media playing".into());
                    }
                    if visual.muted {
                        tab.description = Some("Audio muted".into());
                    }
                    if visual.media_playing || visual.muted {
                        tab.children.push(
                            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                                .with_name(if visual.muted {
                                    "Unmute tab"
                                } else {
                                    "Mute tab"
                                })
                                .with_action_target(format!("tab-mute:{id}"))
                                .with_hashed_key(format!("tab-mute:{id}"))
                                .with_bounds(self.media_rect(index)),
                        );
                    }
                }
            }
        }
        let (left, right) = self.overflow_rects();
        node.children.push(
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name("Previous hidden tab")
                .with_action_target("tab-overflow-before")
                .with_hashed_key("tab-overflow-before")
                .with_state(ui::a11y::AccessState {
                    disabled: !self.has_hidden_before(),
                    ..Default::default()
                })
                .with_bounds(left),
        );
        node.children.push(
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name("Next hidden tab")
                .with_action_target("tab-overflow-after")
                .with_hashed_key("tab-overflow-after")
                .with_state(ui::a11y::AccessState {
                    disabled: !self.has_hidden_after(),
                    ..Default::default()
                })
                .with_bounds(right),
        );
        Some(node)
    }
}
