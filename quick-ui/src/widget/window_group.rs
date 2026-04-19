use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use taffy::style_helpers::length;
use crate::widget::{Widget, TabBar, TabDescriptor, TileContent, TileHost};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

pub const WIDTH_MIN_PX: u32 = 300;
pub const WIDTH_MAX_PX: u32 = 1200;
pub const RESIZE_HANDLE_PX: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowWidth { Narrow, Normal, Wide, Custom(u32) }

impl WindowWidth {
    pub fn pixels(&self) -> u32 {
        match self {
            WindowWidth::Narrow => 400,
            WindowWidth::Normal => 600,
            WindowWidth::Wide => 800,
            WindowWidth::Custom(p) => (*p).clamp(WIDTH_MIN_PX, WIDTH_MAX_PX),
        }
    }
}

pub struct WindowGroup {
    pub id: u64,
    bar: TabBar,
    hosts: Vec<(u64, TileHost)>,
    width: WindowWidth,
    rect: Rect,
    resize_origin: Option<(f32, u32)>,
    pub hover_resize: bool,
    /// User has requested the concept's "maximized" mode for this group.
    /// The shell paints the floating controls on top of this rect while
    /// set; click the maximize icon again to un-maximize (restore).
    pub maximized: bool,
    /// Hit region for the floating "restore" pill drawn while maximized.
    /// Populated each draw so clicks always map to what's on screen.
    restore_rect: Rect,
    restore_hover: bool,
    /// Hit region for the floating "close active tab" glyph drawn while
    /// maximized, sitting just right of the restore pill to mirror the
    /// concept's `[Search] [Minimize2] [X]` cluster.
    close_rect: Rect,
    close_hover: bool,
    last_cursor: Point,
}

impl WindowGroup {
    pub fn new(id: u64, tabs_with_content: Vec<(TabDescriptor, TileContent)>, active_id: u64, width: WindowWidth) -> Self {
        let descs: Vec<TabDescriptor> = tabs_with_content.iter().map(|(d, _)| d.clone()).collect();
        let hosts: Vec<(u64, TileHost)> = tabs_with_content.into_iter().map(|(d, c)| (d.id, TileHost::new(c))).collect();
        Self {
            id,
            bar: TabBar::new(&format!("g{id}"), descs, active_id),
            hosts,
            width,
            rect: Rect::ZERO,
            resize_origin: None,
            hover_resize: false,
            maximized: false,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            last_cursor: Point::ZERO,
        }
    }
    pub fn width(&self) -> WindowWidth { self.width }
    pub fn begin_resize(&mut self, pointer_x: f32) { self.resize_origin = Some((pointer_x, self.width.pixels())); }
    pub fn update_resize(&mut self, pointer_x: f32) {
        if let Some((origin_x, origin_w)) = self.resize_origin {
            let delta = pointer_x - origin_x;
            let new_w = ((origin_w as f32) + delta).round().max(0.0) as i64;
            let clamped = (new_w.max(WIDTH_MIN_PX as i64).min(WIDTH_MAX_PX as i64)) as u32;
            self.width = WindowWidth::Custom(clamped);
        }
    }
    pub fn end_resize(&mut self) { self.resize_origin = None; }
    pub fn active_host_mut(&mut self) -> Option<&mut TileHost> {
        let id = self.bar.active_id();
        self.hosts.iter_mut().find(|(i, _)| *i == id).map(|(_, h)| h)
    }
}

impl Widget for WindowGroup {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let Some(h) = self.active_host_mut() { h.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool { self.active_host_mut().map(|h| h.update(s, p)).unwrap_or(false) }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let bar_node = self.bar.build_layout(taffy, theme, node_map);
        let active_id = self.bar.active_id();
        let tile_node = if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.build_layout(taffy, theme, node_map)
        } else {
            taffy.new_leaf(Style::default()).unwrap()
        };
        let w_px = self.width.pixels() as f32;
        let node = taffy.new_with_children(Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size { width: Dimension::length(w_px), height: Dimension::percent(1.0) },
            gap: taffy::Size { width: length(0.0), height: length(0.0) },
            ..Default::default()
        }, &[bar_node, tile_node]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        let children = taffy.children(node).unwrap();
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        self.bar.apply_layout(taffy, children[0], new_origin);
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.apply_layout(taffy, children[1], new_origin);
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        self.bar.draw(scene, transform, tr, theme, presence);
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.draw(scene, transform, tr, theme, presence);
        }
        // resize handle visualization
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        let col = if self.hover_resize || self.resize_origin.is_some() { theme.tokens.colors.border_focus } else { theme.tokens.colors.border_standard };
        scene.fill(vello::peniko::Fill::NonZero, transform, col, None, &handle_rect);

        // Maximized visual: 2 px accent outline around the whole group so
        // the user gets immediate feedback from the tab's maximize click
        // even before the shell paints concept-style floating controls.
        if self.maximized {
            let outline = Rect::new(
                self.rect.x0 - 1.0,
                self.rect.y0 - 1.0,
                self.rect.x1 + 1.0,
                self.rect.y1 + 1.0,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &outline,
            );

            // Floating controls at the top of the maximized group, matching
            // the concept's `[app title]  …  [Restore][×]` layout.
            let pad = 6.0_f64;
            let pill_h = 22.0_f64;
            let y0 = self.rect.y0 + pad;

            // Left: app-title pill. Shows the active tab's title so the
            // maximized tile still tells you which app owns the surface.
            let active_title = self
                .bar
                .tabs()
                .iter()
                .find(|t| t.id == self.bar.active_id())
                .map(|t| t.title.clone())
                .unwrap_or_default();
            let title_size = theme.tokens.typography.scale.xs;
            let (tw, th) = tr.measure(&active_title, title_size, None);
            let title_pill_w = (tw as f64 + 24.0).max(80.0);
            let title_pill = Rect::new(
                self.rect.x0 + pad,
                y0,
                self.rect.x0 + pad + title_pill_w,
                y0 + pill_h,
            );
            let title_rr =
                vello::kurbo::RoundedRect::from_rect(title_pill, pill_h / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                vello::peniko::Color::rgba8(255, 255, 255, 38),
                None,
                &title_rr,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &title_rr,
            );
            let title_tx = title_pill.center().x - tw as f64 / 2.0;
            let title_ty = title_pill.center().y - th as f64 / 2.0;
            tr.draw_text(
                scene,
                &active_title,
                title_size,
                theme.tokens.colors.text_primary,
                transform * vello::kurbo::Affine::translate((title_tx, title_ty)),
                None,
            );

            // Right-action cluster: Restore pill + close glyph.
            let close_w = 22.0_f64;
            let close_x0 = self.rect.x1 - close_w - pad;
            self.close_rect = Rect::new(close_x0, y0, close_x0 + close_w, y0 + pill_h);
            let restore_w = 76.0_f64;
            let restore_x0 = close_x0 - restore_w - 4.0;
            self.restore_rect =
                Rect::new(restore_x0, y0, restore_x0 + restore_w, y0 + pill_h);

            // Restore pill.
            let pill = vello::kurbo::RoundedRect::from_rect(
                self.restore_rect,
                pill_h / 2.0,
            );
            let fill_alpha: u8 = if self.restore_hover { 64 } else { 38 };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                vello::peniko::Color::rgba8(255, 255, 255, fill_alpha),
                None,
                &pill,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &pill,
            );
            let label = "Restore";
            let (lw, lh) = tr.measure(label, title_size, None);
            let lx = self.restore_rect.center().x - lw as f64 / 2.0;
            let ly = self.restore_rect.center().y - lh as f64 / 2.0;
            tr.draw_text(
                scene,
                label,
                title_size,
                theme.tokens.colors.text_primary,
                transform * vello::kurbo::Affine::translate((lx, ly)),
                None,
            );

            // Close glyph: matches the concept's `X` button, tinted red on
            // hover so the destructive intent is obvious.
            let close_rr =
                vello::kurbo::RoundedRect::from_rect(self.close_rect, pill_h / 2.0);
            let close_alpha: u8 = if self.close_hover { 180 } else { 38 };
            let close_fill = if self.close_hover {
                theme.tokens.colors.semantic.danger
            } else {
                vello::peniko::Color::rgba8(255, 255, 255, close_alpha)
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                close_fill,
                None,
                &close_rr,
            );
            let glyph = "×";
            let glyph_size = theme.tokens.typography.scale.base;
            let (gw, gh) = tr.measure(glyph, glyph_size, None);
            let gx = self.close_rect.center().x - gw as f64 / 2.0;
            let gy = self.close_rect.center().y - gh as f64 / 2.0;
            tr.draw_text(
                scene,
                glyph,
                glyph_size,
                theme.tokens.colors.text_primary,
                transform * vello::kurbo::Affine::translate((gx, gy)),
                None,
            );
        } else {
            self.restore_rect = Rect::ZERO;
            self.close_rect = Rect::ZERO;
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        let children = taffy.children(node_id).unwrap();
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                self.hover_resize = handle_rect.contains(pt);
                self.restore_hover = self.maximized && self.restore_rect.contains(pt);
                self.close_hover = self.maximized && self.close_rect.contains(pt);
                if self.resize_origin.is_some() { self.update_resize(pt.x as f32); *handled = true; return; }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
                if self.maximized && self.close_rect.contains(self.last_cursor) =>
            {
                // Close-glyph click on a maximized group: drop the active
                // tab's host, remove the tab from the bar, and exit
                // maximize so the group stops drawing floating controls
                // that now point at a stale tab.
                let active_id = self.bar.active_id();
                if let Some(idx) = self.hosts.iter().position(|(i, _)| *i == active_id) {
                    self.hosts.remove(idx);
                }
                self.bar.remove_tab(active_id);
                self.maximized = false;
                *handled = true;
                return;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
                if self.maximized && self.restore_rect.contains(self.last_cursor) =>
            {
                // Restore-pill click: exit maximize. Handled here (not via
                // bar.pending_max) so the click never races with a tab
                // selection under the pill's geometry.
                self.maximized = false;
                *handled = true;
                return;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } if self.hover_resize => {
                // the origin x is captured at the current pointer via the current rect
                self.begin_resize(self.rect.x1 as f32);
                *handled = true; return;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } if self.resize_origin.is_some() => {
                self.end_resize(); *handled = true; return;
            }
            _ => {}
        }
        self.bar.handle_event(event, scale_factor, m, s, p, handled, focus_target, children[0], taffy);

        // Close a tab: drop its host + mirror the removal in the bar.
        if let Some(close_id) = self.bar.take_close() {
            if let Some(idx) = self.hosts.iter().position(|(i, _)| *i == close_id) {
                self.hosts.remove(idx);
            }
            self.bar.remove_tab(close_id);
            *handled = true;
        }

        // Maximize toggle from the tab bar's hover-action icon. We don't
        // yet reshape the layout tree (the shell paints floating controls
        // on top instead), so just flip the flag and let the shell pick it
        // up on its next draw.
        if let Some(_id) = self.bar.take_max() {
            self.maximized = !self.maximized;
            *handled = true;
        }

        // Drag-reorder within the bar: apply the same permutation to hosts
        // so the visible tab order matches the content order.
        if let Some((from, to)) = self.bar.take_reorder() {
            if from < self.hosts.len() && to < self.hosts.len() {
                let item = self.hosts.remove(from);
                self.hosts.insert(to, item);
            }
            *handled = true;
        }

        if *handled { return; }
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.handle_event(event, scale_factor, m, s, p, handled, focus_target, children[1], taffy);
        }
    }
    fn name(&self) -> &str { "WindowGroup" }
    fn get_rect(&self) -> Rect { self.rect }
    fn wants_pointer_capture(&self) -> bool { self.resize_origin.is_some() }
}
