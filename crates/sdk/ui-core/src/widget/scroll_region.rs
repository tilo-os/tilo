//! A scrolling viewport for a widget that paints its own content: a
//! calculator's history, a chat transcript, a map's panel, a painted modal
//! body ([`super::ModalLayout::body`]).
//!
//! The retained [`super::Scrollable`] scrolls a widget tree. A painted host
//! has no tree to hand it, so it keeps a [`ScrollRegion`] instead: it tells
//! the region its viewport and how big its content is, forwards its events,
//! draws its content through [`ScrollRegion::paint`] (clipped and
//! translated), and gets the same wheel, keyboard, drag, bar, idle fade and
//! accessible scrolling every `Scrollable` has.

use std::cell::Cell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::ScrollAxis;
use super::scrollbar::{BarFade, PAGE_FRACTION, PressOutcome, ScrollMetrics, Scrollbar};
use crate::color_compat::ColorCompat;
use crate::theme::Theme;

/// See the [module docs](self).
#[derive(Debug)]
pub struct ScrollRegion {
    axis: ScrollAxis,
    /// The whole region, bar lanes included.
    viewport: Rect,
    content: Vec2,
    offset: Vec2,
    vbar: Scrollbar,
    hbar: Scrollbar,
    fade: BarFade,
    /// The bar lane and line step from the theme, at the last
    /// [`ScrollRegion::set_viewport`].
    lane: f64,
    line: f64,
    cursor: Point,
    pointer_near: bool,
    focused: bool,
    edge_fade: bool,
    persisted: Option<Rc<Cell<(f32, f32)>>>,
}

impl ScrollRegion {
    #[must_use]
    pub fn new(axis: ScrollAxis) -> Self {
        Self {
            axis,
            viewport: Rect::ZERO,
            content: Vec2::ZERO,
            offset: Vec2::ZERO,
            vbar: Scrollbar::vertical(),
            hbar: Scrollbar::horizontal(),
            fade: BarFade::default(),
            lane: 0.0,
            line: 0.0,
            cursor: Point::ZERO,
            pointer_near: false,
            focused: false,
            edge_fade: false,
            persisted: None,
        }
    }

    /// A region that scrolls up and down.
    #[must_use]
    pub fn vertical() -> Self {
        Self::new(ScrollAxis::Vertical)
    }

    /// A region that scrolls sideways.
    #[must_use]
    pub fn horizontal() -> Self {
        Self::new(ScrollAxis::Horizontal)
    }

    /// Keep the offset in `cell` across rebuilds of the host; it is clamped
    /// to the content once both sizes are known.
    #[must_use]
    pub fn with_persisted_offset(mut self, cell: Rc<Cell<(f32, f32)>>) -> Self {
        let (x, y) = cell.get();
        self.offset = Vec2::new(f64::from(x), f64::from(y));
        self.persisted = Some(cell);
        self
    }

    /// Fade the content out at an edge more of it can scroll in from, like
    /// `Scrollable::with_edge_fade`.
    #[must_use]
    pub fn with_edge_fade(mut self) -> Self {
        self.edge_fade = true;
        self
    }

    fn has_vertical(&self) -> bool {
        matches!(self.axis, ScrollAxis::Vertical | ScrollAxis::Both)
    }

    fn has_horizontal(&self) -> bool {
        matches!(self.axis, ScrollAxis::Horizontal | ScrollAxis::Both)
    }

    /// The region's rect (bar lanes included) for this layout, and the
    /// theme its lanes and steps come from.
    pub fn set_viewport(&mut self, viewport: Rect, theme: &Theme) {
        self.viewport = viewport;
        self.lane = f64::from(theme.tokens.metrics.scrollbar_gutter);
        self.line = f64::from(theme.tokens.metrics.wheel_line);
        self.clamp();
    }

    /// How big the content is, in the host's content coordinates.
    pub fn set_content_size(&mut self, width: f64, height: f64) {
        self.content = Vec2::new(width.max(0.0), height.max(0.0));
        self.clamp();
    }

    /// Whether the content is taller than the region.
    #[must_use]
    pub fn overflows_vertically(&self) -> bool {
        self.has_vertical() && self.content.y > self.viewport.height() + 0.5
    }

    /// Whether the content is wider than the region.
    #[must_use]
    pub fn overflows_horizontally(&self) -> bool {
        self.has_horizontal() && self.content.x > self.viewport.width() + 0.5
    }

    /// The part of the region the content shows in: the region less the
    /// bar lanes while the content overflows. Lay the content out to this
    /// width.
    #[must_use]
    pub fn content_rect(&self) -> Rect {
        let mut rect = self.viewport;
        if self.overflows_vertically() {
            rect.x1 = (rect.x1 - self.lane).max(rect.x0);
        }
        if self.overflows_horizontally() {
            rect.y1 = (rect.y1 - self.lane).max(rect.y0);
        }
        rect
    }

    /// The current offset (how far the content is scrolled).
    #[must_use]
    pub fn offset(&self) -> Vec2 {
        self.offset
    }

    fn max_offset(&self) -> Vec2 {
        let shown = self.content_rect();
        Vec2::new(
            (self.content.x - shown.width()).max(0.0),
            (self.content.y - shown.height()).max(0.0),
        )
    }

    fn clamp(&mut self) {
        // A restored offset waits until both sizes are known.
        if self.viewport.is_zero_area() || self.content == Vec2::ZERO {
            return;
        }
        let max = self.max_offset();
        self.offset = Vec2::new(
            self.offset.x.clamp(0.0, max.x),
            self.offset.y.clamp(0.0, max.y),
        );
        if let Some(cell) = &self.persisted {
            cell.set((
                crate::num::f32_of_f64(self.offset.x),
                crate::num::f32_of_f64(self.offset.y),
            ));
        }
    }

    /// Scroll by `delta`; returns whether the offset moved.
    pub fn scroll_by(&mut self, delta: Vec2) -> bool {
        let before = self.offset;
        self.offset += delta;
        self.clamp();
        let moved = self.offset != before;
        if moved {
            self.fade.wake(crate::time::now());
        }
        moved
    }

    /// Scroll to `offset` (clamped).
    pub fn set_offset(&mut self, offset: Vec2) {
        self.offset = offset;
        self.clamp();
    }

    /// Scroll the least that shows `rect` (in content coordinates).
    pub fn scroll_into_view(&mut self, rect: Rect) {
        let shown = self.content_rect();
        let mut target = self.offset;
        if rect.y0 < target.y {
            target.y = rect.y0;
        } else if rect.y1 > target.y + shown.height() {
            target.y = rect.y1 - shown.height();
        }
        if rect.x0 < target.x {
            target.x = rect.x0;
        } else if rect.x1 > target.x + shown.width() {
            target.x = rect.x1 - shown.width();
        }
        self.scroll_by(target - self.offset);
    }

    /// The transform the host draws its content with: the region's own
    /// transform, moved to the content's origin and by the offset.
    #[must_use]
    pub fn content_transform(&self, transform: Affine) -> Affine {
        let shown = self.content_rect();
        transform.pre_translate(Vec2::new(shown.x0, shown.y0) - self.offset)
    }

    /// Where a point in the window falls in the content, for hit-testing.
    #[must_use]
    pub fn to_content(&self, point: Point) -> Point {
        let shown = self.content_rect();
        point - Vec2::new(shown.x0, shown.y0) + self.offset
    }

    /// The part of the content in view, in content coordinates (cull what
    /// lies outside it).
    #[must_use]
    pub fn visible_content(&self) -> Rect {
        let shown = self.content_rect();
        Rect::from_origin_size(self.offset.to_point(), shown.size())
    }

    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content.y, self.content_rect().height(), self.offset.y)
    }

    fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content.x, self.content_rect().width(), self.offset.x)
    }

    fn v_track(&self, theme: &Theme) -> Option<Rect> {
        self.overflows_vertically().then(|| {
            let shown = self.content_rect();
            let lane = Rect::new(shown.x1, self.viewport.y0, self.viewport.x1, shown.y1);
            self.vbar.track_in(lane, theme)
        })
    }

    fn h_track(&self, theme: &Theme) -> Option<Rect> {
        self.overflows_horizontally().then(|| {
            let shown = self.content_rect();
            let lane = Rect::new(self.viewport.x0, shown.y1, shown.x1, self.viewport.y1);
            self.hbar.track_in(lane, theme)
        })
    }

    fn held(&self, theme: &Theme) -> bool {
        theme.reduce_motion
            || self.pointer_near
            || self.focused
            || self.vbar.is_active()
            || self.hbar.is_active()
    }

    /// Paint the content with `draw` (handed the scene and the content
    /// transform), clipped to the region, then the edge fades and the bars.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        draw: impl FnOnce(&mut Scene, Affine),
    ) {
        let shown = self.content_rect();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shown);
        draw(scene, self.content_transform(transform));
        scene.pop_layer();
        if self.edge_fade {
            let ground = theme.tokens.colors.surfaces.content;
            for edge in self.faded_edges() {
                crate::surface::content_edge_fade(scene, transform, shown, edge, ground);
            }
        }
        self.paint_bars(scene, transform, theme);
    }

    fn faded_edges(&self) -> Vec<crate::surface::ContentEdge> {
        use crate::surface::ContentEdge;
        let max = self.max_offset();
        let mut edges = Vec::new();
        if self.has_vertical() {
            if self.offset.y > 0.5 {
                edges.push(ContentEdge::Top);
            }
            if max.y - self.offset.y > 0.5 {
                edges.push(ContentEdge::Bottom);
            }
        }
        if self.has_horizontal() {
            if self.offset.x > 0.5 {
                edges.push(ContentEdge::Leading);
            }
            if max.x - self.offset.x > 0.5 {
                edges.push(ContentEdge::Trailing);
            }
        }
        edges
    }

    /// Paint the bars alone, for a host that clips and draws its content
    /// itself.
    pub fn paint_bars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let opacity = self.fade.opacity_at(crate::time::now(), self.held(theme));
        if opacity <= 0.003 {
            return;
        }
        for (track, metrics, bar) in [
            (self.v_track(theme), self.v_metrics(), &self.vbar),
            (self.h_track(theme), self.h_metrics(), &self.hbar),
        ] {
            let Some(track) = track else {
                continue;
            };
            if let Some(thumb) = bar.thumb_rect(track, metrics) {
                let radius = thumb.width().min(thumb.height()) / 2.0;
                let colour = super::scrollbar::thumb_color(theme, bar.is_active() || self.focused)
                    .with_alpha_factor(opacity);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colour,
                    None,
                    &RoundedRect::from_rect(thumb, radius),
                );
            }
        }
    }

    /// Whether the bar's fade needs frames now.
    #[must_use]
    pub fn needs_animation_frame(&self, theme: &Theme) -> bool {
        self.fade.fading_at(crate::time::now(), self.held(theme))
    }

    /// When the bar starts to fade, for the host's `next_wake_deadline`.
    #[must_use]
    pub fn next_wake_deadline(&self, theme: &Theme) -> Option<crate::time::Instant> {
        self.fade
            .fade_deadline_at(crate::time::now(), self.held(theme))
    }

    /// Follow the host's keyboard focus: a focused region scrolls with the
    /// keyboard and keeps its bar shown.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Wheel and touchpad scrolling over the region, dragging and paging
    /// with its bars, and Page Up/Down, Home/End and the arrows while the
    /// host holds focus. Marks the event handled when it scrolled. Returns
    /// whether the offset or the bars changed.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        theme: &Theme,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let mut changed = false;
                if self.vbar.is_dragging()
                    && let Some(track) = self.v_track(theme)
                    && let Some(target) = self.vbar.drag_to(track, self.v_metrics(), self.cursor)
                {
                    changed |= self.scroll_by(Vec2::new(0.0, target - self.offset.y));
                    *ctx.handled = true;
                }
                if self.hbar.is_dragging()
                    && let Some(track) = self.h_track(theme)
                    && let Some(target) = self.hbar.drag_to(track, self.h_metrics(), self.cursor)
                {
                    changed |= self.scroll_by(Vec2::new(target - self.offset.x, 0.0));
                    *ctx.handled = true;
                }
                if let Some(track) = self.v_track(theme) {
                    changed |= self.vbar.update_hover(track, self.v_metrics(), self.cursor);
                }
                if let Some(track) = self.h_track(theme) {
                    changed |= self.hbar.update_hover(track, self.h_metrics(), self.cursor);
                }
                let shown = self.content_rect();
                let near = self.viewport.contains(self.cursor)
                    && (self.cursor.x >= shown.x1 || self.cursor.y >= shown.y1);
                if near != self.pointer_near {
                    self.pointer_near = near;
                    self.fade.wake(crate::time::now());
                    changed = true;
                }
                changed
            }
            WindowEvent::MouseWheel { delta, .. } if self.viewport.contains(self.cursor) => {
                let (mut dx, mut dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (-f64::from(*x) * self.line, -f64::from(*y) * self.line)
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        (-p.x / ctx.scale_factor, -p.y / ctx.scale_factor)
                    }
                };
                if ctx.modifiers.state().shift_key() && self.has_horizontal() && dx == 0.0 {
                    (dx, dy) = (dy, 0.0);
                }
                if !self.has_horizontal() {
                    dx = 0.0;
                }
                if !self.has_vertical() {
                    dy = 0.0;
                }
                let moved = self.scroll_by(Vec2::new(dx, dy));
                if moved {
                    *ctx.handled = true;
                }
                moved
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    for vertical in [true, false] {
                        let (track, metrics) = if vertical {
                            (self.v_track(theme), self.v_metrics())
                        } else {
                            (self.h_track(theme), self.h_metrics())
                        };
                        let Some(track) = track else {
                            continue;
                        };
                        let bar = if vertical {
                            &mut self.vbar
                        } else {
                            &mut self.hbar
                        };
                        match bar.press(track, metrics, self.cursor) {
                            PressOutcome::Thumb => {
                                self.fade.wake(crate::time::now());
                                *ctx.handled = true;
                                return true;
                            }
                            PressOutcome::Page(target) => {
                                let delta = if vertical {
                                    Vec2::new(0.0, target - self.offset.y)
                                } else {
                                    Vec2::new(target - self.offset.x, 0.0)
                                };
                                *ctx.handled = true;
                                return self.scroll_by(delta);
                            }
                            PressOutcome::Miss => {}
                        }
                    }
                    false
                }
                ElementState::Released => {
                    let released = self.vbar.release() | self.hbar.release();
                    if released {
                        *ctx.handled = true;
                    }
                    released
                }
            },
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let shown = self.content_rect();
                let page = Vec2::new(shown.width(), shown.height()) * PAGE_FRACTION;
                let far = self.content.x.max(self.content.y);
                let delta = match (&event.logical_key, self.has_vertical()) {
                    (Key::Named(NamedKey::PageDown), true) => Vec2::new(0.0, page.y),
                    (Key::Named(NamedKey::PageUp), true) => Vec2::new(0.0, -page.y),
                    (Key::Named(NamedKey::PageDown), false) => Vec2::new(page.x, 0.0),
                    (Key::Named(NamedKey::PageUp), false) => Vec2::new(-page.x, 0.0),
                    (Key::Named(NamedKey::Home), true) => Vec2::new(0.0, -far),
                    (Key::Named(NamedKey::End), true) => Vec2::new(0.0, far),
                    (Key::Named(NamedKey::Home), false) => Vec2::new(-far, 0.0),
                    (Key::Named(NamedKey::End), false) => Vec2::new(far, 0.0),
                    (Key::Named(NamedKey::ArrowDown), true) => Vec2::new(0.0, self.line),
                    (Key::Named(NamedKey::ArrowUp), true) => Vec2::new(0.0, -self.line),
                    (Key::Named(NamedKey::ArrowRight), _) if self.has_horizontal() => {
                        Vec2::new(self.line, 0.0)
                    }
                    (Key::Named(NamedKey::ArrowLeft), _) if self.has_horizontal() => {
                        Vec2::new(-self.line, 0.0)
                    }
                    _ => return false,
                };
                let moved = self.scroll_by(delta);
                if moved {
                    *ctx.handled = true;
                }
                moved
            }
            _ => false,
        }
    }

    /// The region as the host's primary vertical scroll, for assistive
    /// technology and agents: forward `Widget::primary_vertical_scroll`.
    #[must_use]
    pub fn primary_vertical_scroll(&self) -> Option<super::PrimaryScrollRegion> {
        self.has_vertical().then(|| super::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: self.v_metrics(),
        })
    }

    /// Apply an accessible scroll action: forward
    /// `Widget::apply_primary_vertical_scroll`.
    pub fn apply_primary_vertical_scroll(
        &mut self,
        action: super::PrimaryScrollAction,
    ) -> super::PrimaryScrollResult {
        use super::PrimaryScrollResult;
        let Some(region) = self.primary_vertical_scroll() else {
            return PrimaryScrollResult::Unsupported;
        };
        let Some(next) = action.offset(region.metrics) else {
            return PrimaryScrollResult::Unsupported;
        };
        if (next - region.metrics.offset).abs() < f64::EPSILON {
            return PrimaryScrollResult::Unchanged(region);
        }
        self.scroll_by(Vec2::new(0.0, next - self.offset.y));
        self.primary_vertical_scroll().map_or(
            PrimaryScrollResult::Unsupported,
            PrimaryScrollResult::Changed,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(content_h: f64) -> ScrollRegion {
        let mut region = ScrollRegion::vertical();
        region.set_viewport(Rect::new(0.0, 0.0, 300.0, 200.0), &Theme::dark());
        region.set_content_size(280.0, content_h);
        region
    }

    fn send(region: &mut ScrollRegion, event: &WindowEvent) -> bool {
        let mut tree = taffy::TaffyTree::<()>::new();
        let node = crate::widget::TaffyTreeExt::leaf_node(&mut tree, taffy::Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        region.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
            &Theme::dark(),
        );
        handled
    }

    #[test]
    fn the_region_reserves_its_lane_only_while_content_overflows() {
        let fits = region(150.0);
        assert_eq!(fits.content_rect(), Rect::new(0.0, 0.0, 300.0, 200.0));
        let tall = region(1000.0);
        let lane = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        assert!((tall.content_rect().x1 - (300.0 - lane)).abs() < 1e-9);
    }

    #[test]
    fn wheel_and_keys_scroll_within_the_content_and_clamp() {
        let mut region = region(1000.0);
        send(
            &mut region,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(100.0, 100.0),
            },
        );
        assert!(send(
            &mut region,
            &WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            },
        ));
        let line = f64::from(Theme::dark().tokens.metrics.wheel_line);
        assert!((region.offset().y - line).abs() < 1e-9);
        region.set_focused(true);
        let end =
            crate::synthetic_input::key_input(&Key::Named(NamedKey::End), ElementState::Pressed);
        assert!(send(&mut region, &end));
        assert!(
            (region.offset().y - 800.0).abs() < 1e-9,
            "{:?}",
            region.offset()
        );
        assert!(!send(&mut region, &end), "already at the end");
        assert!(
            (region.to_content(Point::new(10.0, 10.0)).y - 810.0).abs() < 1e-9,
            "hit-testing goes through the offset"
        );
    }

    #[test]
    fn a_persisted_offset_is_clamped_and_written_back() {
        let cell = Rc::new(Cell::new((0.0_f32, 5000.0_f32)));
        let mut region = ScrollRegion::vertical().with_persisted_offset(Rc::clone(&cell));
        region.set_viewport(Rect::new(0.0, 0.0, 300.0, 200.0), &Theme::dark());
        region.set_content_size(280.0, 1000.0);
        assert!((region.offset().y - 800.0).abs() < 1e-9);
        assert!((cell.get().1 - 800.0).abs() < 1e-3);
    }

    #[test]
    fn accessible_scrolling_moves_the_region() {
        let mut region = region(1000.0);
        let result = region.apply_primary_vertical_scroll(super::super::PrimaryScrollAction::End);
        assert!(matches!(
            result,
            super::super::PrimaryScrollResult::Changed(_)
        ));
        assert!((region.offset().y - 800.0).abs() < 1e-9);
    }
}
