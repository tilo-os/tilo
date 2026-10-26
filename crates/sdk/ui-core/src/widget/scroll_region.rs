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

/// See the module docs.
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
    /// Bars lie over the content's trailing edge instead of in lanes of
    /// their own ([`Self::with_overlaid_bars`]).
    overlaid_bars: bool,
    persisted: Option<Rc<Cell<(f32, f32)>>>,
    /// What the frame being drawn shows of the content, and what the last
    /// frame the host presented showed ([`Self::offer_copy`]).
    shown: crate::scroll_copy::ShownFrames<RegionShown>,
    /// The rounded corners of the window the region sits in, learned where
    /// it last drew: its bars keep out of them.
    corners: Cell<crate::widget::scrollbar::SurfaceCorners>,
    /// Wheel and touchpad motion along the region's main axis: a detented
    /// wheel's notches and a touchpad flick glide on as every scroller's do
    /// ([`crate::virtual_viewport::glide_step`]), in the frames the host
    /// asks for and steps through [`Self::advance`].
    glide: crate::virtual_viewport::ScrollMomentum,
    /// The motion toward what the host asked to show ([`Self::glide_to`]).
    scroll_to: crate::motion::ScrollTo,
    /// The theme's motion, at the last [`Self::set_viewport`], and device
    /// pixels per logical pixel where the region last handled input.
    motion: crate::tokens::EffectiveMotion,
    device_scale: f64,
    /// The content's opacity in the frame being drawn and in the frame on
    /// screen: below 1 while it fades in after a long jump, when no copy of
    /// it is offered.
    fade_drawn: Cell<f32>,
    fade_shown: f32,
}

/// Where a frame shows the content: its offset, the clip it paints into,
/// the transform it lands on screen with, what the region paints over it
/// without moving it (the edge fades' bands), its bars' tracks, which
/// lie beside the content, and the thumbs the bars painted in them.
#[derive(Clone, Debug, PartialEq)]
struct RegionShown {
    offset: Vec2,
    clip: Rect,
    transform: Affine,
    overlays: Vec<Rect>,
    tracks: Vec<Rect>,
    thumbs: Vec<(Rect, vello::peniko::Color)>,
}

impl crate::scroll_copy::Shown for RegionShown {
    fn scroll(&self) -> Vec2 {
        self.offset
    }

    fn clip(&self) -> Rect {
        self.clip
    }

    fn transform(&self) -> Affine {
        self.transform
    }

    fn same_view(&self, _other: &Self) -> bool {
        true
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        out.extend(self.overlays.iter().chain(&self.tracks));
    }
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
            overlaid_bars: false,
            persisted: None,
            shown: crate::scroll_copy::ShownFrames::default(),
            corners: Cell::new(crate::widget::scrollbar::SurfaceCorners::default()),
            glide: crate::virtual_viewport::ScrollMomentum::new(),
            scroll_to: crate::motion::ScrollTo::new(),
            motion: crate::tokens::EffectiveMotion::default(),
            device_scale: 1.0,
            fade_drawn: Cell::new(1.0),
            fade_shown: 1.0,
        }
    }

    /// A region that scrolls up and down.
    #[must_use]
    pub fn vertical() -> Self {
        Self::new(ScrollAxis::Vertical)
    }

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

    /// Lay the bars over the content's trailing edge rather than in lanes
    /// beside it, so the content keeps the region's whole width whether or
    /// not it overflows: content that grows past the region (a list opening
    /// in a panel) never narrows and reflows what was already there. For
    /// content with its own padding at least a lane wide on that edge, which
    /// the bar then sits in.
    #[must_use]
    pub fn with_overlaid_bars(mut self) -> Self {
        self.overlaid_bars = true;
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
        let lane = f64::from(theme.tokens.metrics.scrollbar_gutter);
        if self.viewport != viewport || self.lane.to_bits() != lane.to_bits() {
            self.cancel_interaction();
        }
        self.viewport = viewport;
        self.lane = lane;
        self.line = f64::from(theme.tokens.metrics.wheel_line);
        self.motion = theme.motion();
        self.clamp();
    }

    /// How big the content is, in the host's content coordinates.
    pub fn set_content_size(&mut self, width: f64, height: f64) {
        let content = Vec2::new(width.max(0.0), height.max(0.0));
        if self.content != content {
            self.cancel_interaction();
        }
        self.content = content;
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
    /// bar lanes while the content overflows (the whole region when the bars
    /// are overlaid). Lay the content out to this width.
    #[must_use]
    pub fn content_rect(&self) -> Rect {
        if self.overlaid_bars {
            return self.viewport;
        }
        self.inside_lanes()
    }

    /// The region less the bar lanes while the content overflows: beyond
    /// it lie the bars, beside the content or over its edge.
    fn inside_lanes(&self) -> Rect {
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

    /// Scroll to `offset` (clamped), at once.
    pub fn set_offset(&mut self, offset: Vec2) {
        self.glide.stop();
        self.scroll_to.take_over(crate::motion::frame_now());
        self.offset = offset;
        self.clamp();
    }

    /// Move to `offset` (clamped) the way a view moves to what it was asked
    /// to show (a find's next match, a search hit opened): a short jump the
    /// person just asked for glides there and a long one fades in at the
    /// target; one nobody asked for, or under reduced motion, lands
    /// ([`crate::motion::ScrollTo`]). The host steps it from its `update`
    /// ([`Self::advance`]), as it does a glide.
    pub fn glide_to(&mut self, offset: Vec2) {
        let now = crate::motion::frame_now();
        let max = self.max_offset();
        let target = Vec2::new(offset.x.clamp(0.0, max.x), offset.y.clamp(0.0, max.y));
        let shown = self.content_rect();
        self.glide.stop();
        let jump = self.scroll_to.go(
            self.offset,
            target,
            Vec2::new(shown.width(), shown.height()),
            self.motion,
            now,
        );
        if jump == crate::motion::ScrollJump::Glide {
            self.follow_scroll_to(now);
        } else {
            self.scroll_by(target - self.offset);
        }
    }

    /// Follow a [`Self::glide_to`] in progress to where it is drawn at
    /// `now`; returns whether the offset moved.
    fn follow_scroll_to(&mut self, now: crate::time::Instant) -> bool {
        let Some(at) = self
            .scroll_to
            .offset_in_device_pixels(now, self.device_scale)
        else {
            return false;
        };
        let moved = self.scroll_by(at - self.offset);
        let _ = self.scroll_to.arrived(now);
        moved
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
        transform.pre_translate(Vec2::new(shown.x0, shown.y0) - self.drawn_offset())
    }

    /// The offset the content is drawn at: where it is scrolled, and past
    /// an end by the give fingers pull it there with
    /// ([`crate::motion::RubberBand`]).
    fn drawn_offset(&self) -> Vec2 {
        let give = self.glide.give(crate::motion::frame_now());
        if self.glides_vertically() {
            self.offset + Vec2::new(0.0, give)
        } else {
            self.offset + Vec2::new(give, 0.0)
        }
    }

    /// Where a point in the window falls in the content, for hit-testing.
    #[must_use]
    pub fn to_content(&self, point: Point) -> Point {
        let shown = self.content_rect();
        point - Vec2::new(shown.x0, shown.y0) + self.drawn_offset()
    }

    /// The part of the content in view, in content coordinates (cull what
    /// lies outside it).
    #[must_use]
    pub fn visible_content(&self) -> Rect {
        let shown = self.content_rect();
        Rect::from_origin_size(self.drawn_offset().to_point(), shown.size())
    }

    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content.y, self.content_rect().height(), self.offset.y)
    }

    fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content.x, self.content_rect().width(), self.offset.x)
    }

    fn v_track(&self, theme: &Theme) -> Option<Rect> {
        self.overflows_vertically().then(|| {
            let shown = self.inside_lanes();
            let lane = Rect::new(shown.x1, self.viewport.y0, self.viewport.x1, shown.y1);
            self.corners
                .get()
                .clear(self.vbar.track_in(lane, theme), true)
        })
    }

    fn h_track(&self, theme: &Theme) -> Option<Rect> {
        self.overflows_horizontally().then(|| {
            let shown = self.inside_lanes();
            let lane = Rect::new(self.viewport.x0, shown.y1, shown.x1, self.viewport.y1);
            self.corners
                .get()
                .clear(self.hbar.track_in(lane, theme), false)
        })
    }

    /// Whether a bar's thumb is being dragged: the host keeps the pointer
    /// captured until it is released (`Widget::wants_pointer_capture`).
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.vbar.is_dragging() || self.hbar.is_dragging()
    }

    fn held(&self, theme: &Theme) -> bool {
        theme.reduce_motion
            || self.pointer_near
            || self.focused
            || self.vbar.is_active()
            || self.hbar.is_active()
    }

    /// Note what the frame being drawn shows, for a host that clips and
    /// draws its content itself (with [`Self::paint_bars`]) and offers its
    /// scrolls as copies ([`Self::offer_copy`]); [`Self::paint`] notes it.
    pub fn note_shown(&self, transform: Affine, theme: &Theme) {
        self.fade_drawn
            .set(self.scroll_to.opacity(crate::motion::frame_now()));
        self.corners
            .set(crate::widget::scrollbar::SurfaceCorners::seen(
                theme, transform,
            ));
        let clip = self.content_rect();
        let tracks: Vec<Rect> = [self.v_track(theme), self.h_track(theme)]
            .into_iter()
            .flatten()
            .collect();
        let mut overlays = Vec::new();
        if self.edge_fade {
            let reach = crate::surface::CONTENT_FADE_PX;
            if self.has_vertical() {
                overlays.push(Rect::new(clip.x0, clip.y0, clip.x1, clip.y0 + reach));
                overlays.push(Rect::new(clip.x0, clip.y1 - reach, clip.x1, clip.y1));
            }
            if self.has_horizontal() {
                overlays.push(Rect::new(clip.x0, clip.y0, clip.x0 + reach, clip.y1));
                overlays.push(Rect::new(clip.x1 - reach, clip.y0, clip.x1, clip.y1));
            }
        }
        self.shown.note(RegionShown {
            offset: self.drawn_offset(),
            clip,
            transform: crate::scroll_copy::on_screen(transform),
            overlays,
            tracks,
            thumbs: Vec::new(),
        });
    }

    /// From the host's `collect_damage`, on a frame whose content and
    /// offset are as on screen: add the bars' tracks to `regions` when what
    /// the bars paint changed since the frame on screen (a thumb fading out
    /// or lighting up under the pointer). Without both frames noted, every
    /// track either knows is added.
    pub fn damage_changed_bars(&self, regions: &mut Vec<Rect>) {
        let drawing = self.shown.drawing();
        match (drawing.as_deref(), self.shown.presented()) {
            (Some(drawing), Some(presented))
                if drawing.thumbs == presented.thumbs && drawing.tracks == presented.tracks => {}
            (drawing, presented) => regions.extend(
                drawing
                    .into_iter()
                    .chain(presented)
                    .flat_map(|shown| shown.tracks.iter().copied()),
            ),
        }
    }

    /// From the host's `collect_damage`, when nothing it paints in the
    /// region changed since the frame on screen but the offset: offer the
    /// scroll to a host that copies what stays on screen
    /// ([`crate::scroll_copy::offer`]), and add the bars' tracks, where the
    /// thumbs moved, to `regions`. `false` when the host does not copy, or
    /// the frame shows the content in another clip or under another
    /// transform, or the offset did not move: the host then damages the
    /// region as it would without copies.
    pub fn offer_copy<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) -> bool {
        // Content fading in after a long jump is not where a copy finds it.
        if self.fade_drawn.get() < 1.0 || self.fade_shown < 1.0 {
            return false;
        }
        if self.shown.compare(owner, regions) != crate::scroll_copy::Since::Copied {
            return false;
        }
        if let Some((drawing, presented)) = self.shown.frames() {
            regions.extend(drawing.tracks.iter().chain(&presented.tracks).copied());
        }
        true
    }

    /// From the host's `mark_drawn`: the frame just drawn is on screen. Not
    /// drawn this frame, the region's content is not known to be there, and
    /// its next scroll repaints.
    pub fn mark_presented(&mut self) {
        self.shown.present();
        self.fade_shown = self.fade_drawn.get();
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
        self.note_shown(transform, theme);
        let shown = self.content_rect();
        // After a long jump the content fades in where it landed.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            self.fade_drawn.get(),
            transform,
            &shown,
        );
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
        self.corners
            .set(crate::widget::scrollbar::SurfaceCorners::seen(
                theme, transform,
            ));
        let opacity = self
            .fade
            .opacity_at(crate::motion::frame_now(), self.held(theme));
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
                if let Some(mut shown) = self.shown.drawing_mut() {
                    shown.thumbs.push((thumb, colour));
                }
            }
        }
    }

    /// Whether the bar's fade or a glide needs frames now. A host that
    /// forwards this steps the glide from its `update` ([`Self::advance`]).
    #[must_use]
    pub fn needs_animation_frame(&self, theme: &Theme) -> bool {
        let now = crate::motion::frame_now();
        self.glide.is_active()
            || self.glide.give_needs_frame(now)
            || self.scroll_to.target().is_some()
            || self.scroll_to.is_moving(now)
            || self.fade_drawn.get() < 1.0
            || self.fade_shown < 1.0
            || self.fade.fading_at(now, self.held(theme))
    }

    /// Step a glide in progress to the frame's time, from the host's
    /// `update`; returns whether the offset moved, which the host treats
    /// as the scroll an event would have made (a copy, not a repaint). A
    /// glide that meets the content's end stops there.
    pub fn advance(&mut self) -> bool {
        let now = crate::motion::frame_now();
        // A fade-in after a long jump repaints its frames, as a scroll does.
        let fading = self.scroll_to.opacity(now) < 1.0 || self.fade_shown < 1.0;
        if self.scroll_to.target().is_some() {
            return self.follow_scroll_to(now) | fading;
        }
        // The give springing back moves the content as a glide does.
        let giving = self.glide.give_needs_frame(now);
        if !self.glide.is_active() {
            return fading | giving;
        }
        let delta = self.glide.advance(crate::motion::frame_now());
        let step = if self.glides_vertically() {
            Vec2::new(0.0, delta)
        } else {
            Vec2::new(delta, 0.0)
        };
        let moved = self.scroll_by(step);
        if !moved && delta != 0.0 {
            self.glide.stop();
        }
        moved
    }

    /// Whether a glide is in progress.
    #[must_use]
    pub fn is_gliding(&self) -> bool {
        self.glide.is_active()
    }

    /// The axis wheel motion glides along: the vertical one when the region
    /// scrolls both ways.
    fn glides_vertically(&self) -> bool {
        self.has_vertical()
    }

    /// When the bar starts to fade, for the host's `next_wake_deadline`.
    #[must_use]
    pub fn next_wake_deadline(&self, theme: &Theme) -> Option<crate::time::Instant> {
        self.fade
            .fade_deadline_at(crate::motion::frame_now(), self.held(theme))
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
        self.handle_window_event(
            event,
            ctx.scale_factor,
            ctx.modifiers.state(),
            ctx.handled,
            theme,
        )
    }

    /// Retire a captured scrollbar gesture and its transient pointer feedback.
    pub fn cancel_interaction(&mut self) -> bool {
        let changed = self.vbar.release()
            | self.hbar.release()
            | self.vbar.set_hover(false)
            | self.hbar.set_hover(false);
        self.pointer_near = false;
        changed
    }

    /// The same input path for painted hosts without a retained widget tree.
    pub fn handle_window_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::keyboard::ModifiersState,
        handled: &mut bool,
        theme: &Theme,
    ) -> bool {
        self.device_scale = scale_factor;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                let mut changed = false;
                if self.vbar.is_dragging()
                    && let Some(track) = self.v_track(theme)
                    && let Some(target) = self.vbar.drag_to(track, self.v_metrics(), self.cursor)
                {
                    changed |= self.scroll_by(Vec2::new(0.0, target - self.offset.y));
                    *handled = true;
                }
                if self.hbar.is_dragging()
                    && let Some(track) = self.h_track(theme)
                    && let Some(target) = self.hbar.drag_to(track, self.h_metrics(), self.cursor)
                {
                    changed |= self.scroll_by(Vec2::new(target - self.offset.x, 0.0));
                    *handled = true;
                }
                if let Some(track) = self.v_track(theme) {
                    changed |= self.vbar.update_hover(track, self.v_metrics(), self.cursor);
                }
                if let Some(track) = self.h_track(theme) {
                    changed |= self.hbar.update_hover(track, self.h_metrics(), self.cursor);
                }
                let shown = self.inside_lanes();
                let near = self.viewport.contains(self.cursor)
                    && (self.cursor.x >= shown.x1 || self.cursor.y >= shown.y1);
                if near != self.pointer_near {
                    self.pointer_near = near;
                    self.fade.wake(crate::time::now());
                    changed = true;
                }
                changed
            }
            WindowEvent::Focused(false) => self.cancel_interaction(),
            WindowEvent::KeyboardInput { event, .. }
                if self.is_dragging()
                    && event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                *handled = true;
                self.cancel_interaction()
            }
            // Ctrl+scroll zooms (`crate::content_zoom`): the window takes it.
            WindowEvent::MouseWheel { delta, phase, .. }
                if self.viewport.contains(self.cursor)
                    && !crate::content_zoom::is_zoom_modifier(
                        crate::platform::PlatformConventions::default(),
                        modifiers,
                    ) =>
            {
                // The hand takes over a jump in progress from where it is drawn.
                self.scroll_to.take_over(crate::motion::frame_now());
                if let Some(moved) =
                    self.wheel_glide(delta, *phase, scale_factor, modifiers, theme, handled)
                {
                    return moved;
                }
                let (mut dx, mut dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (-f64::from(*x) * self.line, -f64::from(*y) * self.line)
                    }
                    MouseScrollDelta::PixelDelta(p) => (-p.x / scale_factor, -p.y / scale_factor),
                };
                if modifiers.shift_key() && self.has_horizontal() && dx == 0.0 {
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
                    *handled = true;
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
                        if self.glide.is_active() {
                            self.glide.stop();
                        }
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
                                self.scroll_to.take_over(crate::motion::frame_now());
                                self.fade.wake(crate::time::now());
                                *handled = true;
                                return true;
                            }
                            PressOutcome::Page(target) => {
                                self.scroll_to.take_over(crate::motion::frame_now());
                                let delta = if vertical {
                                    Vec2::new(0.0, target - self.offset.y)
                                } else {
                                    Vec2::new(target - self.offset.x, 0.0)
                                };
                                *handled = true;
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
                        *handled = true;
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
                self.glide.stop();
                self.scroll_to.take_over(crate::motion::frame_now());
                let moved = self.scroll_by(delta);
                if moved {
                    *handled = true;
                }
                moved
            }
            _ => false,
        }
    }

    /// Wheel motion along the axis the region glides on, through the
    /// shared momentum: a notch moves at once and glides on, fingers move
    /// the content 1:1 and a flick glides on after they lift. `None` for
    /// motion across that axis (Shift+wheel sideways, a sideways swipe),
    /// which moves the content directly. Marks the event handled when it
    /// moved the content or started a glide; returns whether it moved.
    fn wheel_glide(
        &mut self,
        delta: &MouseScrollDelta,
        phase: winit::event::TouchPhase,
        scale_factor: f64,
        modifiers: winit::keyboard::ModifiersState,
        theme: &Theme,
        handled: &mut bool,
    ) -> Option<bool> {
        let (x, y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(*x), f64::from(*y)),
            MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
        };
        let vertical = self.glides_vertically();
        let across = if vertical { x } else { y };
        let along = if vertical { y } else { x };
        let sideways = vertical && modifiers.shift_key() && self.has_horizontal();
        if sideways || across.abs() > along.abs() {
            self.glide.stop();
            return None;
        }
        // The momentum reads a wheel's vertical motion: a region that only
        // scrolls sideways hands it the sideways motion in its place.
        let delta = if vertical {
            *delta
        } else {
            match delta {
                MouseScrollDelta::LineDelta(_, _) => MouseScrollDelta::LineDelta(0.0, along as f32),
                MouseScrollDelta::PixelDelta(_) => {
                    MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, along))
                }
            }
        };
        self.glide.set_device_scale(scale_factor);
        let shown = self.content_rect();
        let max = self.max_offset();
        let (extent, room) = if vertical {
            (shown.height(), max.y)
        } else {
            (shown.width(), max.x)
        };
        self.glide.set_view(extent, room > 0.0, theme.motion());
        let step = match self.glide.wheel(&delta, phase, theme.reduce_motion) {
            crate::virtual_viewport::WheelStep::Scroll(step) => step,
            crate::virtual_viewport::WheelStep::Fling => {
                // The glide moves the content in the frames that follow.
                *handled = true;
                return Some(false);
            }
            crate::virtual_viewport::WheelStep::Ignored => return Some(false),
        };
        // A notch is the theme's line, not the momentum's own step.
        let step = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * self.line,
            MouseScrollDelta::PixelDelta(_) => step,
        };
        let before = self.offset;
        self.scroll_by(if vertical {
            Vec2::new(0.0, step)
        } else {
            Vec2::new(step, 0.0)
        });
        let along = if vertical {
            self.offset.y - before.y
        } else {
            self.offset.x - before.x
        };
        let moved = self.glide.wheel_applied(step, along);
        if moved {
            *handled = true;
        }
        Some(moved)
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
        action.apply_to(self, Self::primary_vertical_scroll, |region, _, next| {
            region.scroll_by(Vec2::new(0.0, next - region.offset.y));
        })
    }
}

#[cfg(test)]
#[path = "scroll_region/capture_tests.rs"]
mod capture_tests;

#[cfg(test)]
#[path = "scroll_region/copy_tests.rs"]
mod copy_tests;

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
    fn overlaid_bars_keep_the_content_width_and_sit_in_its_trailing_edge() {
        let theme = Theme::dark();
        let lane = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let viewport = Rect::new(0.0, 0.0, 300.0, 200.0);
        let mut region = ScrollRegion::vertical().with_overlaid_bars();
        region.set_viewport(viewport, &theme);
        region.set_content_size(300.0, 150.0);
        assert_eq!(region.content_rect(), viewport);
        region.set_content_size(300.0, 1000.0);
        assert_eq!(
            region.content_rect(),
            viewport,
            "content that overflows keeps the whole width"
        );
        let track = region
            .v_track(&theme)
            .expect("an overflowing region has a bar");
        assert!(
            track.x0 >= viewport.x1 - lane && track.x1 <= viewport.x1,
            "{track:?}"
        );
    }

    #[test]
    fn wheel_and_keys_scroll_within_the_content_and_clamp() {
        let mut region = region(1000.0);
        send(
            &mut region,
            &crate::synthetic_input::cursor_moved(100.0, 100.0),
        );
        assert!(send(
            &mut region,
            &crate::synthetic_input::mouse_wheel(0.0, -1.0),
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

    fn swipe(dy: f64, phase: winit::event::TouchPhase) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
            phase,
        }
    }

    /// Swipe the fingers up `steps` times, a frame apart, and lift them.
    fn flick(region: &mut ScrollRegion, start: crate::time::Instant, steps: u64) -> u64 {
        use crate::time::testing::with_monotonic_time;
        use winit::event::TouchPhase;
        send(region, &crate::synthetic_input::cursor_moved(100.0, 100.0));
        for tick in 0..steps {
            with_monotonic_time(start + frame(tick), || {
                send(region, &swipe(-20.0, TouchPhase::Moved));
            });
        }
        with_monotonic_time(start + frame(steps), || {
            send(region, &swipe(0.0, TouchPhase::Ended));
        });
        steps
    }

    fn frame(tick: u64) -> std::time::Duration {
        std::time::Duration::from_micros(tick * 8_333)
    }

    #[test]
    fn a_flick_glides_on_in_whole_steps_and_rests_at_the_end() {
        use crate::time::testing::with_monotonic_time;
        let mut region = region(100_000.0);
        let start = crate::time::now();
        let mut tick = flick(&mut region, start, 6);
        let lifted = region.offset().y;
        assert!((lifted - 120.0).abs() < 1e-9, "the fingers moved it 1:1");
        assert!(region.needs_animation_frame(&Theme::dark()), "it glides");
        let mut frames = 0;
        while region.is_gliding() && frames < 2_000 {
            tick += 1;
            frames += 1;
            let moved = with_monotonic_time(start + frame(tick), || region.advance());
            let y = region.offset().y;
            assert!(
                !moved || (y - y.round()).abs() < 1e-9,
                "a glide moves whole pixels, so the host copies it: {y}"
            );
        }
        assert!(region.offset().y > lifted + 100.0, "{:?}", region.offset());
        assert!(!region.is_gliding(), "it comes to rest");

        let mut short = self::region(400.0);
        let start = crate::time::now();
        let mut tick = flick(&mut short, start, 6);
        while short.is_gliding() && tick < 2_000 {
            tick += 1;
            with_monotonic_time(start + frame(tick), || short.advance());
        }
        assert!(
            (short.offset().y - 200.0).abs() < 1e-9,
            "it stops at the end"
        );
        assert!(!short.is_gliding());
    }

    #[test]
    fn a_held_key_or_the_fingers_catch_a_glide_and_reduced_motion_never_starts_one() {
        use crate::time::testing::with_monotonic_time;
        use winit::event::TouchPhase;
        let mut region = region(100_000.0);
        let start = crate::time::now();
        let tick = flick(&mut region, start, 6);
        assert!(region.is_gliding());
        with_monotonic_time(start + frame(tick + 1), || {
            send(&mut region, &swipe(-5.0, TouchPhase::Started));
        });
        assert!(!region.is_gliding(), "fingers back down catch it");

        let mut region = self::region(100_000.0);
        let tick = flick(&mut region, start, 6);
        region.set_focused(true);
        with_monotonic_time(start + frame(tick + 1), || {
            send(
                &mut region,
                &crate::synthetic_input::key_input(
                    &Key::Named(NamedKey::ArrowDown),
                    ElementState::Pressed,
                ),
            );
        });
        assert!(!region.is_gliding(), "a key lands where it says");

        let mut theme = Theme::dark();
        theme.reduce_motion = true;
        let mut calm = self::region(100_000.0);
        calm.handle_window_event(
            &crate::synthetic_input::cursor_moved(100.0, 100.0),
            1.0,
            winit::keyboard::ModifiersState::empty(),
            &mut false,
            &theme,
        );
        for tick in 0..6 {
            with_monotonic_time(start + frame(tick), || {
                calm.handle_window_event(
                    &swipe(-20.0, TouchPhase::Moved),
                    1.0,
                    winit::keyboard::ModifiersState::empty(),
                    &mut false,
                    &theme,
                );
            });
        }
        with_monotonic_time(start + frame(6), || {
            calm.handle_window_event(
                &swipe(0.0, TouchPhase::Ended),
                1.0,
                winit::keyboard::ModifiersState::empty(),
                &mut false,
                &theme,
            );
        });
        assert!((calm.offset().y - 120.0).abs() < 1e-9);
        assert!(
            !calm.is_gliding(),
            "reduced motion moves only with the fingers"
        );
    }

    #[test]
    fn a_jump_asked_for_glides_or_fades_in_and_one_nobody_asked_for_lands() {
        use crate::time::testing::with_monotonic_time;
        use crate::tokens::MotionSource;
        let start = crate::time::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        let theme = Theme::dark();
        let mut region = region(100_000.0);
        // A find's next match a view down glides there in whole pixels.
        with_monotonic_time(start, || {
            crate::motion::note_source(MotionSource::Keyboard);
            region.glide_to(Vec2::new(0.0, 300.0));
        });
        assert_eq!(region.offset().y, 0.0, "it starts where it is");
        assert!(region.needs_animation_frame(&theme));
        let drawn = with_monotonic_time(at(40), || {
            assert!(region.advance(), "a step is a scroll");
            region.offset().y
        });
        assert!(drawn > 0.0 && drawn < 300.0, "on its way: {drawn}");
        assert_eq!(drawn.fract(), 0.0, "in whole pixels, which copy");
        // The wheel takes over from where it is drawn.
        with_monotonic_time(at(40), || {
            send(
                &mut region,
                &crate::synthetic_input::cursor_moved(100.0, 100.0),
            );
            send(&mut region, &swipe(0.0, winit::event::TouchPhase::Moved));
        });
        with_monotonic_time(at(400), || {
            region.advance();
            assert!(
                !region.needs_animation_frame(&theme),
                "nothing moves after the wheel"
            );
        });
        assert_eq!(region.offset().y, drawn);

        // Undisturbed, it arrives exactly.
        with_monotonic_time(at(1000), || {
            crate::motion::note_source(MotionSource::Keyboard);
            region.glide_to(Vec2::new(0.0, 500.0));
        });
        with_monotonic_time(at(1500), || {
            region.advance();
        });
        assert_eq!(region.offset().y, 500.0);

        // Many views away it lands, fades in there, and offers no copy of
        // the fading content.
        with_monotonic_time(at(2000), || {
            crate::motion::note_source(MotionSource::Keyboard);
            region.glide_to(Vec2::new(0.0, 50_000.0));
            assert_eq!(region.offset().y, 50_000.0, "no scrolling through");
            let mut scene = Scene::new();
            region.paint(&mut scene, Affine::IDENTITY, &theme, |_, _| {});
            let mut regions = Vec::new();
            assert!(!region.offer_copy(&region, &mut regions));
            assert!(region.needs_animation_frame(&theme));
        });

        // A jump nobody's hand asked for lands.
        let mut calm = self::region(100_000.0);
        with_monotonic_time(at(5000), || {
            calm.glide_to(Vec2::new(0.0, 300.0));
        });
        assert_eq!(calm.offset().y, 300.0);
        assert!(!calm.needs_animation_frame(&theme));
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
