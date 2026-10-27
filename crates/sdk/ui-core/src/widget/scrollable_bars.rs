//! The scroll view's bars: where a track and its thumb stand, when the bar is
//! shown and how it fades, the hover zone that keeps a thin thumb grabbable, the
//! page-and-hold pacing, and the edge fades that say the content continues.

use super::*;
use crate::color_compat::ColorCompat;
use crate::theme::Theme;
use crate::widget::scrollbar::ScrollMetrics;
use vello::Scene;
use vello::kurbo::{Point, RoundedRect};

impl Scrollable {
    pub(super) fn vbar_active(&self) -> bool {
        self.show_scrollbar
            && self.axis_has_vertical()
            && self.effective_viewport_h() > 0.0
            && self.content_h > self.effective_viewport_h() + FIT_EPS
    }
    pub(super) fn hbar_active(&self) -> bool {
        self.show_scrollbar
            && self.axis_has_horizontal()
            && self.viewport_w > 0.0
            && self.content_w > self.viewport_w + FIT_EPS
    }

    /// Leaves room in the bottom-right corner when the horizontal bar is
    /// also active.
    pub(super) fn vertical_track(&self) -> Option<Rect> {
        if !self.vbar_active() {
            return None;
        }
        let (thickness, margin) = (self.bar_thickness, self.bar_margin);
        let x1 = self.rect.x1 - margin;
        let x0 = x1 - thickness;
        let y0 = self.rect.y0 + margin;
        let mut y1 = self.content_viewport_rect().y1 - margin;
        if self.hbar_active() {
            y1 -= thickness + margin;
        }
        let track = self.corners.clear(Rect::new(x0, y0, x1, y1), true);
        if track.height() <= MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(track)
    }

    pub(super) fn horizontal_track(&self) -> Option<Rect> {
        if !self.hbar_active() {
            return None;
        }
        let (thickness, margin) = (self.bar_thickness, self.bar_margin);
        let y1 = self.rect.y1 - margin;
        let y0 = y1 - thickness;
        let x0 = self.rect.x0 + margin;
        let mut x1 = self.rect.x1 - margin;
        if self.vbar_active() {
            x1 -= thickness + margin;
        }
        let track = self.corners.clear(Rect::new(x0, y0, x1, y1), false);
        if track.width() <= MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(track)
    }

    pub(super) fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            f64::from(self.content_h),
            f64::from(self.effective_viewport_h()),
            f64::from(self.offset_y),
        )
    }
    pub(super) fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            f64::from(self.content_w),
            f64::from(self.viewport_w),
            f64::from(self.offset_x),
        )
    }

    pub(super) fn vertical_thumb(&self) -> Option<Rect> {
        let track = self.vertical_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    pub(super) fn horizontal_thumb(&self) -> Option<Rect> {
        let track = self.horizontal_track()?;
        self.hbar.thumb_rect(track, self.h_metrics())
    }

    /// `ScrollAxis::Both` has no single thumb and always returns `None`.
    pub fn thumb_rect(&self, axis: ScrollAxis) -> Option<Rect> {
        match axis {
            ScrollAxis::Vertical => self.vertical_thumb(),
            ScrollAxis::Horizontal => self.horizontal_thumb(),
            ScrollAxis::Both => None,
        }
    }

    pub(super) fn vertical_hover_zone(&self) -> Option<Rect> {
        let t = self.vertical_track()?;
        Some(Rect::new(t.x0 - HOVER_EXPAND, t.y0, t.x1, t.y1))
    }
    pub(super) fn horizontal_hover_zone(&self) -> Option<Rect> {
        let t = self.horizontal_track()?;
        Some(Rect::new(t.x0, t.y0 - HOVER_EXPAND, t.x1, t.y1))
    }

    pub(super) fn cursor_near_bar(&self, cur: Point) -> bool {
        self.vertical_hover_zone().is_some_and(|z| z.contains(cur))
            || self
                .horizontal_hover_zone()
                .is_some_and(|z| z.contains(cur))
    }

    pub(super) fn bar_pinned(&self) -> bool {
        self.focused
            || self.hold.is_some()
            || self.pointer_near_bar
            || self.vbar.is_active()
            || self.hbar.is_active()
    }

    /// `now`/`reduce_motion` injected so the fade curve is unit-testable.
    pub(super) fn bar_opacity_at(&self, now: Instant, reduce_motion: bool) -> f32 {
        let held = !self.overlay || reduce_motion || self.bar_pinned();
        crate::widget::scrollbar::BarFade::opacity(self.last_activity, now, held)
    }

    pub(super) fn bar_opacity(&self, theme: &Theme) -> f32 {
        self.bar_opacity_at(crate::motion::frame_now(), theme.reduce_motion)
    }

    pub(super) fn fade_held(&self) -> bool {
        !self.overlay || self.reduce_motion_active || self.bar_pinned()
    }

    pub(super) fn fade_in_progress_at(&self, now: Instant) -> bool {
        crate::widget::scrollbar::BarFade::fading(self.last_activity, now, self.fade_held())
    }

    pub(super) fn fade_in_progress(&self) -> bool {
        self.fade_in_progress_at(crate::motion::frame_now())
    }

    pub(super) fn fade_start_deadline_at(&self, now: Instant) -> Option<Instant> {
        crate::widget::scrollbar::BarFade::fade_deadline(self.last_activity, now, self.fade_held())
    }

    /// Once the fade deadline passes, the compositor still needs one frame to
    /// replace the last visible thumb with the transparent scene. The opaque
    /// hold is neither an animation nor an expired fade.
    pub(super) fn needs_terminal_fade_frame_at(&self, now: Instant) -> bool {
        self.terminal_fade_damage.get()
            || (self.bar_visible_last_draw.get()
                && !self.fade_held()
                && crate::widget::scrollbar::BarFade::finished(self.last_activity, now))
    }

    pub(super) fn needs_terminal_fade_frame(&self) -> bool {
        self.needs_terminal_fade_frame_at(crate::motion::frame_now())
    }

    pub(super) fn advance_hold(&mut self, now: Instant) {
        let Some(hold) = self.hold else { return };
        if now < hold.next_at {
            return;
        }
        // Reschedule first so a hold paused at the click point still ticks.
        if let Some(h) = &mut self.hold {
            h.next_at = deadline_after(now, HOLD_REPEAT_MS);
        }
        let page = if hold.vertical {
            self.effective_viewport_h() * PAGE_FRACTION
        } else {
            self.viewport_w * PAGE_FRACTION
        };
        let reached = if hold.vertical {
            match self.vertical_thumb() {
                Some(thumb) => {
                    thumb.contains(hold.anchor)
                        || (hold.dir > 0.0 && thumb.y1 >= hold.anchor.y)
                        || (hold.dir < 0.0 && thumb.y0 <= hold.anchor.y)
                }
                None => true,
            }
        } else {
            match self.horizontal_thumb() {
                Some(thumb) => {
                    thumb.contains(hold.anchor)
                        || (hold.dir > 0.0 && thumb.x1 >= hold.anchor.x)
                        || (hold.dir < 0.0 && thumb.x0 <= hold.anchor.x)
                }
                None => true,
            }
        };
        if reached {
            return;
        }
        if hold.vertical {
            self.scroll_by(0.0, f32_of_f64(hold.dir) * page);
        } else {
            self.scroll_by(f32_of_f64(hold.dir) * page, 0.0);
        }
    }

    /// The edges more content can scroll in from, while edge fades are on.
    pub(super) fn faded_edges(&self) -> Vec<crate::surface::ContentEdge> {
        use crate::surface::ContentEdge;
        let mut edges = Vec::new();
        if !self.edge_fade {
            return edges;
        }
        if self.axis_has_vertical() {
            if self.offset_y > FIT_EPS {
                edges.push(ContentEdge::Top);
            }
            if self.max_offset_y() - self.offset_y > FIT_EPS {
                edges.push(ContentEdge::Bottom);
            }
        }
        if self.axis_has_horizontal() {
            let max_x = (self.content_w - self.viewport_w).max(0.0);
            if self.offset_x > FIT_EPS {
                edges.push(ContentEdge::Leading);
            }
            if max_x - self.offset_x > FIT_EPS {
                edges.push(ContentEdge::Trailing);
            }
        }
        edges
    }

    /// What this view paints over its content inside `clip` that does not
    /// move with it: its bars' tracks and the bands its edge fades cover.
    pub(super) fn copy_overlays(&self, clip: Rect) -> Vec<Rect> {
        let mut overlays: Vec<Rect> = [self.vertical_track(), self.horizontal_track()]
            .into_iter()
            .flatten()
            .collect();
        if self.edge_fade {
            let reach = crate::surface::CONTENT_FADE_PX;
            if self.axis_has_vertical() {
                overlays.push(Rect::new(clip.x0, clip.y0, clip.x1, clip.y0 + reach));
                overlays.push(Rect::new(clip.x0, clip.y1 - reach, clip.x1, clip.y1));
            }
            if self.axis_has_horizontal() {
                overlays.push(Rect::new(clip.x0, clip.y0, clip.x0 + reach, clip.y1));
                overlays.push(Rect::new(clip.x1 - reach, clip.y0, clip.x1, clip.y1));
            }
        }
        overlays
    }

    pub(super) fn draw_edge_fades(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let edges = self.faded_edges();
        if edges.is_empty() {
            return;
        }
        // The content's own lanes: the bar's gutter is not content.
        let mut area = self.content_viewport_rect();
        if self.reserve_vertical {
            area.x1 -= f64::from(self.content_gutter);
        }
        if self.reserve_horizontal {
            area.y1 -= f64::from(self.content_gutter);
        }
        let ground = theme.tokens.colors.surfaces.content;
        for edge in edges {
            crate::surface::content_edge_fade(scene, transform, area, edge, ground);
        }
    }

    pub(super) fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let opacity = self.bar_opacity(theme);
        let visible = self.show_scrollbar && opacity > 0.003;
        if self.bar_visible_last_draw.replace(visible) && !visible {
            self.terminal_fade_damage.set(true);
        }
        if !visible {
            return;
        }
        let radius = self.bar_thickness / 2.0;
        // An idle overlay bar is just the thumb, like macOS/GNOME.
        let expanded = !self.overlay || self.bar_pinned();
        let track_color = crate::widget::scrollbar::track_color(theme).with_alpha_factor(opacity);
        for (track, thumb, active) in [
            (
                self.vertical_track(),
                self.vertical_thumb(),
                self.vbar.is_active(),
            ),
            (
                self.horizontal_track(),
                self.horizontal_thumb(),
                self.hbar.is_active(),
            ),
        ] {
            if let (Some(track), true) = (track, expanded) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    track_color,
                    None,
                    &RoundedRect::from_rect(track, radius),
                );
            }
            if let Some(thumb) = thumb {
                // Hover/drag/focus brightens the derived idle thumb.
                let color = crate::widget::scrollbar::thumb_color(theme, active || self.focused)
                    .with_alpha_factor(opacity);
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
}
