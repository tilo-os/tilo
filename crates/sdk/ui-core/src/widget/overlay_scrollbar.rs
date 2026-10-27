//! The overlay scroll bar for a host that scrolls and paints its own
//! content (a terminal's scrollback, a canvas): the thumb a [`Scrollable`]
//! shows, laid over the content's trailing edge instead of standing in a
//! gutter beside it, shown while the content scrolls or the pointer is at
//! it and faded out at rest ([`BarFade`]); always shown under reduced
//! motion. Showing or hiding it never changes the space the content has.
//!
//! [`Scrollable`]: crate::widget::Scrollable

use std::cell::Cell;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use super::scrollbar::{BarFade, PressOutcome, ScrollMetrics, Scrollbar, thumb_color};
use crate::color_compat::ColorCompat;
use crate::theme::Theme;

/// A vertical overlay scroll bar. The host keeps its own offset and gives
/// the bar the area it scrolls ([`Self::track`]) and its [`ScrollMetrics`]
/// each time; the bar keeps the drag, the hover and the idle fade, and the
/// theme's sizes and motion policy as last given ([`Self::set_theme`]).
#[derive(Debug)]
pub struct OverlayScrollbar {
    bar: Scrollbar,
    fade: Cell<BarFade>,
    /// The offset the bar was last painted at: a change shows the bar.
    painted_offset: Cell<Option<f64>>,
    /// The pointer is in the bar's lane: the bar stays shown.
    pointer_near: bool,
    /// The lane's width (`metrics.scrollbar_gutter`).
    gutter: f64,
    /// The thumb's thickness (`metrics.scroll_thumb`).
    thumb: f64,
    /// The theme asks for reduced motion: the bar does not fade.
    reduce_motion: bool,
}

impl Default for OverlayScrollbar {
    fn default() -> Self {
        Self::vertical()
    }
}

impl OverlayScrollbar {
    /// A vertical bar along the trailing edge, at the default theme's sizes.
    #[must_use]
    pub fn vertical() -> Self {
        let metrics = crate::tokens::Metrics::BASE;
        Self {
            bar: Scrollbar::vertical(),
            fade: Cell::new(BarFade::default()),
            painted_offset: Cell::new(None),
            pointer_near: false,
            gutter: f64::from(metrics.scrollbar_gutter),
            thumb: f64::from(metrics.scroll_thumb),
            reduce_motion: false,
        }
    }

    /// Take the sizes and the motion policy of `theme`.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        self.thumb = f64::from(theme.tokens.metrics.scroll_thumb);
        self.reduce_motion = theme.reduce_motion;
    }

    /// The lane along the trailing edge of `area` the bar lies in, over
    /// the content: the pointer there holds the bar shown.
    #[must_use]
    pub fn lane(&self, area: Rect) -> Rect {
        Rect::new(
            (area.x1 - self.gutter).max(area.x0),
            area.y0,
            area.x1,
            area.y1,
        )
    }

    /// The thumb's track over `area`, where a `Scrollable`'s would be.
    #[must_use]
    pub fn track(&self, area: Rect) -> Rect {
        self.bar.track_of_thickness(self.lane(area), self.thumb)
    }

    /// Show the bar now, as a scroll does.
    pub fn wake(&self) {
        let mut fade = self.fade.get();
        fade.wake(crate::time::now());
        self.fade.set(fade);
    }

    /// Note the offset the content is shown at; a change since the last
    /// one shows the bar, whatever moved it (the wheel, a key, the bar).
    pub fn note_offset(&self, offset: f64) {
        if self
            .painted_offset
            .get()
            .is_some_and(|painted| (painted - offset).abs() > 0.5)
        {
            self.wake();
        }
        self.painted_offset.set(Some(offset));
    }

    /// Follow the pointer at `cursor` over `area`: the thumb under it
    /// brightens and the bar stays shown while it is in the lane. Returns
    /// whether anything the bar paints changed.
    pub fn update_pointer(&mut self, area: Rect, metrics: ScrollMetrics, cursor: Point) -> bool {
        let mut changed = self.bar.update_hover(self.track(area), metrics, cursor);
        let near = metrics.overflows() && self.lane(area).contains(cursor);
        if near != self.pointer_near {
            self.pointer_near = near;
            self.wake();
            changed = true;
        }
        changed
    }

    /// The pointer left: the bar fades as at rest. Returns whether
    /// anything the bar paints changed.
    pub fn pointer_left(&mut self) -> bool {
        let hovered = self.bar.set_hover(false);
        let near = std::mem::take(&mut self.pointer_near);
        if near {
            self.wake();
        }
        hovered || near
    }

    /// A press at `cursor` on the bar over `area`; see
    /// [`Scrollbar::press`]. A bar that has faded out takes none.
    pub fn press(&mut self, area: Rect, metrics: ScrollMetrics, cursor: Point) -> PressOutcome {
        if !metrics.overflows() || self.opacity() <= 0.003 {
            return PressOutcome::Miss;
        }
        let outcome = self.bar.press(self.track(area), metrics, cursor);
        if outcome != PressOutcome::Miss {
            self.wake();
        }
        outcome
    }

    /// The offset a drag in progress asks for; see [`Scrollbar::drag_to`].
    #[must_use]
    pub fn drag_to(&self, area: Rect, metrics: ScrollMetrics, cursor: Point) -> Option<f64> {
        self.bar.drag_to(self.track(area), metrics, cursor)
    }

    /// End a drag; returns whether one was in progress.
    pub fn release(&mut self) -> bool {
        let released = self.bar.release();
        if released {
            self.wake();
        }
        released
    }

    /// Whether the thumb is being dragged.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.bar.is_dragging()
    }

    fn held(&self) -> bool {
        self.reduce_motion || self.pointer_near || self.bar.is_active()
    }

    /// The bar's opacity at this frame.
    #[must_use]
    pub fn opacity(&self) -> f32 {
        self.fade
            .get()
            .opacity_at(crate::motion::frame_now(), self.held())
    }

    /// Paint the thumb over `area` for `metrics`, faded as the bar is now.
    /// Paints nothing while the content fits or the bar has faded out.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        area: Rect,
        metrics: ScrollMetrics,
        theme: &Theme,
    ) {
        if !metrics.overflows() {
            return;
        }
        let opacity = self.opacity();
        if opacity <= 0.003 {
            return;
        }
        if let Some(thumb) = self.bar.thumb_rect(self.track(area), metrics) {
            let radius = thumb.width().min(thumb.height()) / 2.0;
            let colour = thumb_color(theme, self.bar.is_active()).with_alpha_factor(opacity);
            scene.fill(
                Fill::NonZero,
                transform,
                colour,
                None,
                &RoundedRect::from_rect(thumb, radius),
            );
        }
    }

    /// Whether the fade needs frames now, for the host's
    /// `needs_animation_frame`.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.fade
            .get()
            .fading_at(crate::motion::frame_now(), self.held())
    }

    /// When the fade starts, for the host's `next_wake_deadline`.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.fade
            .get()
            .fade_deadline_at(crate::motion::frame_now(), self.held())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0.0, 0.0, 400.0, 300.0);

    fn overflowing(offset: f64) -> ScrollMetrics {
        ScrollMetrics::new(1000.0, 100.0, offset)
    }

    #[test]
    fn the_bar_shows_when_the_content_scrolls_and_fades_at_rest() {
        let theme = Theme::dark();
        let mut bar = OverlayScrollbar::vertical();
        bar.set_theme(&theme);
        bar.note_offset(0.0);
        assert!(bar.opacity() <= 0.003, "nothing scrolled yet");
        let mut scene = Scene::new();
        bar.paint(&mut scene, Affine::IDENTITY, AREA, overflowing(0.0), &theme);
        assert!(scene.encoding().is_empty(), "a bar at rest paints nothing");

        bar.note_offset(40.0);
        assert!(bar.opacity() > 0.99, "a scroll shows the bar");
        bar.paint(
            &mut scene,
            Affine::IDENTITY,
            AREA,
            overflowing(40.0),
            &theme,
        );
        assert!(!scene.encoding().is_empty(), "a shown bar paints its thumb");
        assert!(
            bar.next_wake_deadline().is_some(),
            "the host wakes when the fade starts"
        );
        let later = crate::time::now() + crate::time::Duration::from_secs(10);
        assert!(bar.fade.get().opacity_at(later, false) <= 0.003);
    }

    #[test]
    fn reduced_motion_and_the_pointer_keep_the_bar_shown() {
        let mut theme = Theme::dark();
        theme.reduce_motion = true;
        let mut bar = OverlayScrollbar::vertical();
        bar.set_theme(&theme);
        assert!(bar.held(), "reduced motion never fades the bar");
        assert_eq!(bar.next_wake_deadline(), None);

        theme.reduce_motion = false;
        bar.set_theme(&theme);
        assert!(bar.update_pointer(AREA, overflowing(0.0), bar.lane(AREA).center()));
        assert!(bar.held(), "the pointer at the bar holds it");
        assert!(bar.opacity() > 0.99);
        assert!(bar.pointer_left());
        assert!(!bar.held());
    }

    #[test]
    fn the_bar_lies_over_the_trailing_edge_and_takes_no_space() {
        let theme = Theme::dark();
        let mut bar = OverlayScrollbar::vertical();
        bar.set_theme(&theme);
        let lane = bar.lane(AREA);
        assert_eq!(lane.x1, AREA.x1);
        assert_eq!(
            lane.width(),
            f64::from(theme.tokens.metrics.scrollbar_gutter)
        );
        assert_eq!(
            bar.track(AREA),
            Scrollbar::vertical().track_in(lane, &theme),
            "the thumb stands where a Scrollable's does"
        );
    }

    #[test]
    fn a_faded_bar_takes_no_press() {
        let theme = Theme::dark();
        let mut bar = OverlayScrollbar::vertical();
        bar.set_theme(&theme);
        let on_thumb = bar.track(AREA).center();
        assert_eq!(
            bar.press(AREA, overflowing(450.0), on_thumb),
            PressOutcome::Miss
        );
        bar.wake();
        assert_eq!(
            bar.press(AREA, overflowing(450.0), on_thumb),
            PressOutcome::Thumb
        );
        assert!(bar.release());
    }
}
