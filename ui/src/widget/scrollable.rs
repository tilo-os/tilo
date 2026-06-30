use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::BlendMode;
use winit::event::{WindowEvent, MouseScrollDelta, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::{Widget, Element};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

#[derive(Clone, Copy, Debug)]
pub enum ScrollAxis { Vertical, Horizontal, Both }

/// Visual thickness of a scrollbar track/thumb, in logical pixels.
const BAR_THICKNESS: f64 = 8.0;
/// Inset of the track from the viewport edge on every side.
const BAR_MARGIN: f64 = 2.0;
/// Minimum thumb length so a near-infinite content still leaves a
/// grabbable target.
const MIN_THUMB_LEN: f64 = 24.0;
/// Content must exceed the viewport by more than this (logical px) on an
/// axis before its scrollbar is shown — otherwise the content "fits" and
/// the bar is hidden entirely.
const FIT_EPS: f32 = 0.5;
/// Fraction of a viewport scrolled when paging via a track click.
const PAGE_FRACTION: f32 = 0.9;

/// Live drag of a scrollbar thumb. `vertical` selects which bar; the grab
/// anchors let us map a cursor delta to an offset delta without snapping
/// the thumb's centre to the cursor.
#[derive(Clone, Copy)]
struct ThumbDrag {
    vertical: bool,
    /// Cursor coordinate along the drag axis when the grab began.
    grab_cursor: f64,
    /// `offset_*` along the drag axis when the grab began.
    grab_offset: f32,
}

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
    /// Master switch for the drawn scrollbar (track + thumb). On by
    /// default; flip off via [`Scrollable::with_scrollbar`] for cases
    /// that want pure wheel/momentum scrolling with no chrome.
    show_scrollbar: bool,
    /// Last observed cursor in logical-pixel space. Updated on every
    /// `CursorMoved`; used by `MouseInput::Pressed` to hit-test the
    /// thumb/track without depending on a prior hover edge.
    last_cursor: Point,
    /// Which thumb the cursor is currently over (`Some(true)` =
    /// vertical, `Some(false)` = horizontal, `None` = neither). Drives
    /// the hover brightening.
    hover: Option<bool>,
    /// Active thumb drag, if any.
    drag: Option<ThumbDrag>,
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
            show_scrollbar: true,
            last_cursor: Point::ZERO,
            hover: None,
            drag: None,
        }
    }
    pub fn new_vertical() -> Self { Self::new(ScrollAxis::Vertical) }
    pub fn new_horizontal() -> Self { Self::new(ScrollAxis::Horizontal) }
    pub fn with_child(mut self, c: Element) -> Self { self.child = Some(c); self }
    pub fn with_sticky_header(mut self, h: f32) -> Self { self.sticky_header_height = h; self }
    /// Show or hide the drawn scrollbar (track + draggable thumb).
    /// Defaults to `true`. The bar auto-hides per-axis whenever the
    /// content fits the viewport regardless of this flag; setting it
    /// `false` suppresses the bar unconditionally.
    pub fn with_scrollbar(mut self, show: bool) -> Self { self.show_scrollbar = show; self }

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

    // === Scrollbar geometry ============================================

    fn axis_has_vertical(&self) -> bool {
        matches!(self.axis, ScrollAxis::Vertical | ScrollAxis::Both)
    }
    fn axis_has_horizontal(&self) -> bool {
        matches!(self.axis, ScrollAxis::Horizontal | ScrollAxis::Both)
    }

    /// Whether the vertical bar is currently shown — requires the axis to
    /// allow it, the scrollbar to be enabled, a real viewport, and the
    /// content to actually overflow.
    fn vbar_active(&self) -> bool {
        self.show_scrollbar
            && self.axis_has_vertical()
            && self.viewport_h > 0.0
            && self.content_h > self.viewport_h + FIT_EPS
    }
    fn hbar_active(&self) -> bool {
        self.show_scrollbar
            && self.axis_has_horizontal()
            && self.viewport_w > 0.0
            && self.content_w > self.viewport_w + FIT_EPS
    }

    /// Track rect for the vertical bar (right edge), or `None` if the bar
    /// is inactive. Leaves room in the bottom-right corner when the
    /// horizontal bar is also active.
    fn vertical_track(&self) -> Option<Rect> {
        if !self.vbar_active() {
            return None;
        }
        let x1 = self.rect.x1 - BAR_MARGIN;
        let x0 = x1 - BAR_THICKNESS;
        let y0 = self.rect.y0 + BAR_MARGIN;
        let mut y1 = self.rect.y1 - BAR_MARGIN;
        if self.hbar_active() {
            y1 -= BAR_THICKNESS + BAR_MARGIN;
        }
        if y1 - y0 <= MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(Rect::new(x0, y0, x1, y1))
    }

    /// Track rect for the horizontal bar (bottom edge), or `None`.
    fn horizontal_track(&self) -> Option<Rect> {
        if !self.hbar_active() {
            return None;
        }
        let y1 = self.rect.y1 - BAR_MARGIN;
        let y0 = y1 - BAR_THICKNESS;
        let x0 = self.rect.x0 + BAR_MARGIN;
        let mut x1 = self.rect.x1 - BAR_MARGIN;
        if self.vbar_active() {
            x1 -= BAR_THICKNESS + BAR_MARGIN;
        }
        if x1 - x0 <= MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(Rect::new(x0, y0, x1, y1))
    }

    fn vertical_thumb(&self) -> Option<Rect> {
        let track = self.vertical_track()?;
        let track_len = track.height();
        let content = self.content_h as f64;
        let viewport = self.viewport_h as f64;
        if content <= 0.0 {
            return None;
        }
        let thumb_len = (viewport / content * track_len)
            .clamp(MIN_THUMB_LEN.min(track_len), track_len);
        let max_scroll = (content - viewport).max(0.0);
        let frac = if max_scroll > 0.0 {
            (self.offset_y as f64 / max_scroll).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let y0 = track.y0 + frac * (track_len - thumb_len);
        Some(Rect::new(track.x0, y0, track.x1, y0 + thumb_len))
    }

    fn horizontal_thumb(&self) -> Option<Rect> {
        let track = self.horizontal_track()?;
        let track_len = track.width();
        let content = self.content_w as f64;
        let viewport = self.viewport_w as f64;
        if content <= 0.0 {
            return None;
        }
        let thumb_len = (viewport / content * track_len)
            .clamp(MIN_THUMB_LEN.min(track_len), track_len);
        let max_scroll = (content - viewport).max(0.0);
        let frac = if max_scroll > 0.0 {
            (self.offset_x as f64 / max_scroll).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let x0 = track.x0 + frac * (track_len - thumb_len);
        Some(Rect::new(x0, track.y0, x0 + thumb_len, track.y1))
    }

    /// The thumb rect for the requested bar, or `None` when that bar is
    /// hidden (content fits, axis disabled, or scrollbar turned off).
    /// Pass `ScrollAxis::Vertical` or `ScrollAxis::Horizontal`; `Both`
    /// has no single thumb and returns `None`.
    pub fn thumb_rect(&self, axis: ScrollAxis) -> Option<Rect> {
        match axis {
            ScrollAxis::Vertical => self.vertical_thumb(),
            ScrollAxis::Horizontal => self.horizontal_thumb(),
            ScrollAxis::Both => None,
        }
    }

    fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.show_scrollbar {
            return;
        }
        let radius = BAR_THICKNESS / 2.0;
        let track_color = theme.tokens.colors.surface_white_5;
        for (track, thumb, vertical) in [
            (self.vertical_track(), self.vertical_thumb(), true),
            (self.horizontal_track(), self.horizontal_thumb(), false),
        ] {
            if let Some(track) = track {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    track_color,
                    None,
                    &RoundedRect::from_rect(track, radius),
                );
            }
            if let Some(thumb) = thumb {
                let active = self.drag.map(|d| d.vertical) == Some(vertical)
                    || self.hover == Some(vertical);
                // Idle thumb uses the neutral border token; hover/drag
                // brightens it to the dim-text token for affordance.
                let color = if active { theme.text_dim } else { theme.border_standard };
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(thumb, radius),
                );
            }
        }
    }

    fn begin_drag(&mut self, vertical: bool, cur: Point) {
        let grab_cursor = if vertical { cur.y } else { cur.x };
        let grab_offset = if vertical { self.offset_y } else { self.offset_x };
        self.drag = Some(ThumbDrag { vertical, grab_cursor, grab_offset });
        self.hover = Some(vertical);
        self.scroll_dirty = true;
    }

    /// Map the live cursor position to an absolute offset for the active
    /// drag and apply it via `scroll_by` (so clamp + dirty bookkeeping is
    /// reused). The thumb travels `track_len - thumb_len` pixels across
    /// the full `max_scroll` content range, so that ratio converts a
    /// cursor delta into a content delta.
    fn update_drag(&mut self, cur: Point) {
        let Some(drag) = self.drag else { return };
        if drag.vertical {
            let (Some(track), Some(thumb)) = (self.vertical_track(), self.vertical_thumb())
            else {
                return;
            };
            let travel = (track.height() - thumb.height()).max(1e-3);
            let max_scroll = (self.content_h - self.viewport_h).max(0.0);
            let delta_px = cur.y - drag.grab_cursor;
            let target = drag.grab_offset + (delta_px / travel) as f32 * max_scroll;
            self.scroll_by(0.0, target - self.offset_y);
        } else {
            let (Some(track), Some(thumb)) = (self.horizontal_track(), self.horizontal_thumb())
            else {
                return;
            };
            let travel = (track.width() - thumb.width()).max(1e-3);
            let max_scroll = (self.content_w - self.viewport_w).max(0.0);
            let delta_px = cur.x - drag.grab_cursor;
            let target = drag.grab_offset + (delta_px / travel) as f32 * max_scroll;
            self.scroll_by(target - self.offset_x, 0.0);
        }
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
        // Scrollbars composite above the (clipped) child content.
        self.draw_scrollbars(scene, transform, theme);
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
        // Scrollbars composite above the (clipped) child content.
        self.draw_scrollbars(scene, transform, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let cur = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = cur;
                // An active drag owns every subsequent move — map it to an
                // offset and swallow the event (don't forward to the child).
                if self.drag.is_some() {
                    self.update_drag(cur);
                    *handled = true;
                    return;
                }
                // Otherwise track which thumb (if any) is hovered so the
                // next draw can brighten it. Hover changes need a redraw
                // but still fall through so the child updates its own hover.
                let new_hover = if self.vertical_thumb().is_some_and(|t| t.contains(cur)) {
                    Some(true)
                } else if self.horizontal_thumb().is_some_and(|t| t.contains(cur)) {
                    Some(false)
                } else {
                    None
                };
                if new_hover != self.hover {
                    self.hover = new_hover;
                    self.scroll_dirty = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let cur = self.last_cursor;
                // Press on a thumb begins a drag.
                if self.vertical_thumb().is_some_and(|t| t.contains(cur)) {
                    self.begin_drag(true, cur);
                    *handled = true;
                    return;
                }
                if self.horizontal_thumb().is_some_and(|t| t.contains(cur)) {
                    self.begin_drag(false, cur);
                    *handled = true;
                    return;
                }
                // Press on the track (outside the thumb) pages toward the
                // click.
                if let (Some(track), Some(thumb)) = (self.vertical_track(), self.vertical_thumb()) {
                    if track.contains(cur) {
                        let page = self.viewport_h * PAGE_FRACTION;
                        if cur.y < thumb.y0 {
                            self.scroll_by(0.0, -page);
                        } else if cur.y > thumb.y1 {
                            self.scroll_by(0.0, page);
                        }
                        *handled = true;
                        return;
                    }
                }
                if let (Some(track), Some(thumb)) = (self.horizontal_track(), self.horizontal_thumb()) {
                    if track.contains(cur) {
                        let page = self.viewport_w * PAGE_FRACTION;
                        if cur.x < thumb.x0 {
                            self.scroll_by(-page, 0.0);
                        } else if cur.x > thumb.x1 {
                            self.scroll_by(page, 0.0);
                        }
                        *handled = true;
                        return;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.drag.take().is_some() {
                    self.scroll_dirty = true;
                    *handled = true;
                    return;
                }
            }
            _ => {}
        }
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
        self.drag.is_some() || self.child.as_ref().is_some_and(|c| c.wants_pointer_capture())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Vertical scrollable with a known viewport rect, viewport size and
    /// content height — enough to exercise the scrollbar geometry without
    /// a taffy pass.
    fn vertical(content_h: f32) -> Scrollable {
        let mut s = Scrollable::new_vertical();
        s.rect = Rect::new(0.0, 0.0, 200.0, 300.0);
        s.set_viewport_size(200.0, 300.0);
        s.set_content_size(200.0, content_h);
        s
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    fn mouse(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state,
            button: MouseButton::Left,
        }
    }

    /// Drive an event through the scrollable with default scaffolding and
    /// a single leaf taffy node (no child).
    fn dispatch(s: &mut Scrollable, event: &WindowEvent) -> bool {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        handled
    }

    #[test]
    fn thumb_length_scales_with_viewport_over_content_ratio() {
        // viewport 300 over content 600 → ratio 0.5; over content 1200 →
        // ratio 0.25. Longer thumb for the shorter content.
        let short = vertical(600.0).vertical_thumb().expect("bar shown at 2x");
        let tall = vertical(1200.0).vertical_thumb().expect("bar shown at 4x");
        assert!(
            short.height() > tall.height(),
            "thumb must shrink as content grows: 2x={}, 4x={}",
            short.height(),
            tall.height(),
        );
        // 2x content → thumb ≈ half the track; the track is the viewport
        // minus the two margins.
        let track_len = 300.0 - 2.0 * BAR_MARGIN;
        let expected = 0.5 * track_len;
        assert!(
            (short.height() - expected).abs() < 1.0,
            "2x content thumb should be ≈half the track ({expected}), got {}",
            short.height(),
        );
    }

    #[test]
    fn thumb_never_shorter_than_minimum() {
        // Absurd content : viewport ratio would give a sub-pixel thumb;
        // it must be clamped up to MIN_THUMB_LEN.
        let thumb = vertical(100_000.0).vertical_thumb().expect("bar shown");
        assert!(
            thumb.height() >= MIN_THUMB_LEN - 1e-6,
            "thumb {} below min {}",
            thumb.height(),
            MIN_THUMB_LEN,
        );
    }

    #[test]
    fn thumb_hidden_when_content_fits() {
        // Content shorter than the viewport → no scrollbar at all.
        let s = vertical(150.0);
        assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
        assert!(s.vertical_track().is_none());
        // Exactly fitting is still hidden.
        let s = vertical(300.0);
        assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
    }

    #[test]
    fn thumb_hidden_when_scrollbar_disabled() {
        let mut s = vertical(1200.0);
        s = s.with_scrollbar(false);
        assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
    }

    #[test]
    fn thumb_tracks_offset() {
        // At offset 0 the thumb sits at the track top; scrolled to the
        // bottom it sits at the track bottom.
        let mut s = vertical(1200.0);
        let top = s.vertical_thumb().unwrap();
        let track = s.vertical_track().unwrap();
        assert!((top.y0 - track.y0).abs() < 1e-6, "thumb starts at track top");

        s.scroll_by(0.0, 100_000.0); // clamp to max
        let bottom = s.vertical_thumb().unwrap();
        assert!(
            (bottom.y1 - track.y1).abs() < 1e-3,
            "fully scrolled thumb should reach track bottom: {} vs {}",
            bottom.y1,
            track.y1,
        );
    }

    #[test]
    fn dragging_thumb_updates_offset() {
        let mut s = vertical(1200.0); // max_scroll = 900
        let thumb = s.vertical_thumb().unwrap();
        // Cursor inside the vertical bar (right edge) at the thumb centre.
        let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
        let mid_y = (thumb.y0 + thumb.y1) / 2.0;

        dispatch(&mut s, &cursor_moved(bar_x, mid_y));
        assert!(dispatch(&mut s, &mouse(ElementState::Pressed)), "press on thumb handled");
        assert!(s.drag.is_some(), "press on thumb begins a drag");
        assert!(s.wants_pointer_capture(), "drag requests pointer capture");
        assert_eq!(s.offset_y(), 0.0);

        // Drag down by 100 px — offset must increase, staying clamped.
        dispatch(&mut s, &cursor_moved(bar_x, mid_y + 100.0));
        assert!(s.offset_y() > 0.0, "drag down increases offset");
        assert!(s.offset_y() <= 900.0, "offset stays clamped to max");
        let after_down = s.offset_y();

        // Drag back up past the start — offset returns toward 0.
        dispatch(&mut s, &cursor_moved(bar_x, mid_y - 100.0));
        assert!(s.offset_y() < after_down, "drag up decreases offset");

        // Release ends the drag.
        dispatch(&mut s, &mouse(ElementState::Released));
        assert!(s.drag.is_none(), "release ends the drag");
    }

    #[test]
    fn clicking_track_pages_toward_click() {
        let mut s = vertical(1200.0);
        let thumb = s.vertical_thumb().unwrap();
        let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
        // Click well below the thumb → page down.
        let below_y = thumb.y1 + 5.0;
        dispatch(&mut s, &cursor_moved(bar_x, below_y));
        assert!(dispatch(&mut s, &mouse(ElementState::Pressed)), "track click handled");
        assert!(s.drag.is_none(), "track click does not start a drag");
        let paged = s.offset_y();
        assert!(paged > 0.0, "click below thumb pages down: {paged}");
        assert!(
            (paged - 300.0 * PAGE_FRACTION).abs() < 1.0,
            "page step should be ≈viewport*{PAGE_FRACTION}, got {paged}",
        );
    }

    #[test]
    fn wheel_still_scrolls_with_scrollbar_present() {
        // Regression guard: adding the scrollbar must not break the wheel.
        let mut s = vertical(1200.0);
        let before = s.offset_y();
        let ev = WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: winit::event::TouchPhase::Moved,
        };
        assert!(dispatch(&mut s, &ev), "wheel handled");
        assert!(s.offset_y() > before, "wheel down increases offset");
    }
}
