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
}

impl WindowGroup {
    pub fn new(id: u64, tabs_with_content: Vec<(TabDescriptor, TileContent)>, active_id: u64, width: WindowWidth) -> Self {
        let descs: Vec<TabDescriptor> = tabs_with_content.iter().map(|(d, _)| d.clone()).collect();
        let hosts: Vec<(u64, TileHost)> = tabs_with_content.into_iter().map(|(d, c)| (d.id, TileHost::new(c))).collect();
        Self { id, bar: TabBar::new(&format!("g{id}"), descs, active_id), hosts, width, rect: Rect::ZERO, resize_origin: None, hover_resize: false }
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
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        let children = taffy.children(node_id).unwrap();
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hover_resize = handle_rect.contains(pt);
                if self.resize_origin.is_some() { self.update_resize(pt.x as f32); *handled = true; return; }
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
