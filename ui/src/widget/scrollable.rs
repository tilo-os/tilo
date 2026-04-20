use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::BlendMode;
use winit::event::{WindowEvent, MouseScrollDelta};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::{Widget, Element};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

#[derive(Clone, Copy, Debug)]
pub enum ScrollAxis { Vertical, Horizontal, Both }

pub struct Scrollable {
    axis: ScrollAxis,
    offset_x: f32,
    offset_y: f32,
    content_w: f32,
    content_h: f32,
    viewport_w: f32,
    viewport_h: f32,
    vx: f32,
    vy: f32,
    decay: f32,
    sticky_header_height: f32,
    child: Option<Element>,
    rect: Rect,
}

impl Scrollable {
    pub fn new(axis: ScrollAxis) -> Self {
        Self {
            axis,
            offset_x: 0.0,
            offset_y: 0.0,
            content_w: 0.0,
            content_h: 0.0,
            viewport_w: 0.0,
            viewport_h: 0.0,
            vx: 0.0,
            vy: 0.0,
            decay: 0.92,
            sticky_header_height: 0.0,
            child: None,
            rect: Rect::ZERO,
        }
    }
    pub fn new_vertical() -> Self { Self::new(ScrollAxis::Vertical) }
    pub fn new_horizontal() -> Self { Self::new(ScrollAxis::Horizontal) }
    pub fn with_child(mut self, c: Element) -> Self { self.child = Some(c); self }
    pub fn with_sticky_header(mut self, h: f32) -> Self { self.sticky_header_height = h; self }

    pub fn set_content_size(&mut self, w: f32, h: f32) { self.content_w = w; self.content_h = h; self.clamp(); }
    pub fn set_viewport_size(&mut self, w: f32, h: f32) { self.viewport_w = w; self.viewport_h = h; self.clamp(); }
    pub fn offset_x(&self) -> f32 { self.offset_x }
    pub fn offset_y(&self) -> f32 { self.offset_y }
    pub fn momentum_speed(&self) -> f32 { (self.vx * self.vx + self.vy * self.vy).sqrt() }

    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        match self.axis {
            ScrollAxis::Vertical => { self.offset_y += dy; }
            ScrollAxis::Horizontal => { self.offset_x += dx; }
            ScrollAxis::Both => { self.offset_x += dx; self.offset_y += dy; }
        }
        self.clamp();
    }
    pub fn fling(&mut self, vx: f32, vy: f32) { self.vx = vx; self.vy = vy; }
    pub fn tick(&mut self, dt: f32) {
        self.offset_x += self.vx * dt;
        self.offset_y += self.vy * dt;
        self.vx *= self.decay;
        self.vy *= self.decay;
        if self.vx.abs() < 1.0 { self.vx = 0.0; }
        if self.vy.abs() < 1.0 { self.vy = 0.0; }
        self.clamp();
    }
    pub fn scroll_into_view(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if y < self.offset_y {
            self.offset_y = y;
        } else if y + h > self.offset_y + self.viewport_h {
            self.offset_y = (y + h - self.viewport_h).max(0.0);
        }
        if x < self.offset_x {
            self.offset_x = x;
        } else if x + w > self.offset_x + self.viewport_w {
            self.offset_x = (x + w - self.viewport_w).max(0.0);
        }
        self.clamp();
    }
    fn clamp(&mut self) {
        let max_x = (self.content_w - self.viewport_w).max(0.0);
        let max_y = (self.content_h - self.viewport_h).max(0.0);
        self.offset_x = self.offset_x.clamp(0.0, max_x);
        self.offset_y = self.offset_y.clamp(0.0, max_y);
    }
}

impl Widget for Scrollable {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        if let Some(c) = &mut self.child { c.sync(s, p); }
    }

    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        let ticking = self.momentum_speed() > 0.0;
        if ticking { self.tick(1.0 / 60.0); }
        let child = self.child.as_mut().map(|c| c.update(s, p)).unwrap_or(false);
        ticking || child
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let child_node = self.child.as_mut().map(|c| c.build_layout(taffy, theme, node_map));
        let style = Style {
            size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
            overflow: taffy::Point { x: taffy::Overflow::Hidden, y: taffy::Overflow::Hidden },
            ..Default::default()
        };
        let node = match child_node {
            Some(c) => taffy.new_with_children(style, &[c]).unwrap(),
            None => taffy.new_leaf(style).unwrap(),
        };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        self.viewport_w = l.size.width;
        self.viewport_h = l.size.height;
        if let Some(c) = &mut self.child {
            let child_node = taffy.children(node).unwrap()[0];
            c.apply_layout(
                taffy,
                child_node,
                Point::new(self.rect.x0 - self.offset_x as f64, self.rect.y0 - self.offset_y as f64),
            );
            let cl = taffy.layout(child_node).unwrap();
            self.content_w = cl.size.width;
            self.content_h = cl.size.height;
            self.clamp();
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        scene.push_layer(BlendMode::default(), 1.0, transform, &self.rect);
        if let Some(c) = &mut self.child { c.draw(scene, transform, tr, theme, presence); }
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (*x * -20.0, *y * -20.0),
                MouseScrollDelta::PixelDelta(pp) => (-pp.x as f32, -pp.y as f32),
            };
            self.scroll_by(dx, dy);
            *handled = true;
            return;
        }
        if let Some(c) = &mut self.child {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&child_node) = children.first() {
                    c.handle_event(event, scale_factor, m, s, p, handled, focus_target, child_node, taffy);
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child.as_mut().map(|c| c.as_mut() as &mut (dyn Widget + '_)).into_iter().collect()
    }

    fn name(&self) -> &str { "Scrollable" }

    fn get_rect(&self) -> Rect { self.rect }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false)
    }
}
