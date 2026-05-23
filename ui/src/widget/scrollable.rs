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
    /// Flipped to true whenever `scroll_by` (or the momentum
    /// `tick`) actually moves `offset_*`. The App reads this via
    /// `needs_redraw` + `is_layout_dirty` to gate the next frame.
    /// Without this, a wheel event that *only* mutates Scrollable
    /// state would never trigger a redraw — the descendants
    /// haven't changed, but their resolved screen positions have,
    /// and `apply_layout` is the only path that pushes the new
    /// offset down to the children. Cleared by `mark_drawn`.
    scroll_dirty: bool,
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
            scroll_dirty: false,
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
        let (ox, oy) = (self.offset_x, self.offset_y);
        match self.axis {
            ScrollAxis::Vertical => { self.offset_y += dy; }
            ScrollAxis::Horizontal => { self.offset_x += dx; }
            ScrollAxis::Both => { self.offset_x += dx; self.offset_y += dy; }
        }
        self.clamp();
        // Only signal dirty if the clamp didn't eat the entire
        // delta — at the edges of the content, `offset_y` doesn't
        // actually change, and we'd waste a redraw otherwise.
        if self.offset_x != ox || self.offset_y != oy {
            self.scroll_dirty = true;
        }
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

/// Recursively walk a widget subtree and return the maximum `rect.y1`
/// across every descendant. Used by `Scrollable::apply_layout` to find
/// the true visual bottom edge when taffy's `content_size` under-reports
/// (typically a flex container truncating its last child to fit the
/// available main-axis budget).
fn max_bottom_edge(w: &dyn Widget) -> f64 {
    let mut deepest = w.get_rect().y1;
    for c in w.children() {
        deepest = deepest.max(max_bottom_edge(c));
    }
    deepest
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
            // Start from taffy's reported sizes — `content_size` is
            // the intrinsic extent including overflow past the rendered
            // box (gated by the `content_size` taffy feature; quick-ui
            // enables it for exactly this reason).
            self.content_w = cl.content_size.width.max(cl.size.width);
            self.content_h = cl.content_size.height.max(cl.size.height);

            // taffy's `content_size` for a flex container reports the
            // children's *unclamped layout*, but in practice we've seen
            // it cap at the available block when the inner column is
            // shorter than the literal sum of children (e.g. a flex
            // column whose children include a tall TreeView/ColorPicker
            // after a tight viewport pass). Walk the subtree ourselves
            // and pick the deeper of (taffy report, observed max y) so
            // the user can always scroll to the actual last pixel.
            let scroll_origin_y = self.rect.y0 - self.offset_y as f64;
            let max_y = max_bottom_edge(self.child.as_deref().unwrap());
            let walked_h = (max_y - scroll_origin_y).max(0.0) as f32;
            if walked_h > self.content_h {
                self.content_h = walked_h;
            }
            self.clamp();
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, transform, &self.rect);
        if let Some(c) = &mut self.child { c.draw(scene, transform, tr, theme, presence); }
        scene.pop_layer();
    }

    /// Override so the child recursion uses `draw_with_presence`
    /// (not `draw`) and the inner viewport is set to *this*
    /// scrollable's clip rect. Without this, scrollable content
    /// past the viewport edge would still emit primitives even
    /// though the `push_layer` above would clip them away — wasted
    /// work that the cull check is meant to eliminate.
    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) { return; }
        scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, transform, &self.rect);
        let prev_vp = presence.push_viewport(self.rect);
        if let Some(c) = &mut self.child {
            c.draw_with_presence(scene, transform, tr, theme, presence, permissions);
        }
        presence.pop_viewport(prev_vp);
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            // Wheel-to-px conversion. winit's `LineDelta` reports
            // notches (1.0 per detent on a typical mouse, ~0.1 per step
            // on a hi-res touchpad); the previous `× 20` came out to
            // 20 px / notch which felt sluggish — most desktops use
            // ~3 lines × 40 px ≈ 120 px / notch. `PixelDelta` arrives
            // already in physical pixels (touchpad) and stays at 1×.
            //
            // Sign: winit y-positive = "scroll up"; our model is
            // `content drawn at (… − offset)`, so positive offset_y
            // scrolls content UP (reveals what's below). To match
            // user intent, wheel-up must reduce offset_y → negate.
            const LINE_PX: f32 = 60.0;
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (*x * -LINE_PX, *y * -LINE_PX),
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

    /// Propagate pointer-capture intent from the contained child so an
    /// outer container's event-routing short-circuit honours an active
    /// drag deeper in the tree.
    fn wants_pointer_capture(&self) -> bool {
        self.child.as_ref().is_some_and(|c| c.wants_pointer_capture())
    }

    /// Returns true after a `scroll_by` actually moved the
    /// offset, OR while any descendant signals dirty — without
    /// the first clause, a wheel-only event (no child mutation)
    /// would silently fail to schedule the next frame and the
    /// scroll would visually freeze.
    fn needs_redraw(&self) -> bool {
        self.scroll_dirty || self.child.as_ref().is_some_and(|c| c.needs_redraw())
    }

    /// Forwarded version of the above for the layout gate. The
    /// child's resolved screen rects depend on `self.offset_*`,
    /// so a scroll demands `apply_layout` even though no widget
    /// changed *size*.
    fn is_layout_dirty(&self) -> bool {
        self.scroll_dirty || self.child.as_ref().is_some_and(|c| c.is_layout_dirty())
    }

    fn mark_drawn(&mut self) {
        self.scroll_dirty = false;
        if let Some(c) = &mut self.child {
            c.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect { self.rect }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false)
    }
}
