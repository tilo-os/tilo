//! Shared scrollbar interaction model — the single source of truth for
//! thumb geometry, thumb dragging, and track paging used by every
//! scroll-capable widget in the toolkit ([`Scrollable`], [`DataTable`],
//! [`DocumentViewport`]).
//!
//! Each widget owns its own **track geometry** (bars live on different
//! edges/insets) and its own **authoritative offset**; this module turns a
//! `(track, metrics, cursor)` triple into a thumb rect, a drag delta, or a
//! page step so the *behaviour* stays byte-for-byte identical everywhere —
//! pressing the thumb anchors `(cursor, offset)` and a subsequent move maps
//! the cursor delta onto the content range (clamped to `[0, max_scroll]`);
//! pressing the track pages the view by [`PAGE_FRACTION`] of a viewport;
//! hover reports whether the pointer is over the thumb so the owner can
//! brighten it.
//!
//! The model holds only ephemeral interaction state (a drag anchor + hover
//! flag) and is deliberately axis-scoped: a widget that scrolls on both
//! axes owns two [`Scrollbar`]s.
//!
//! [`Scrollable`]: crate::widget::Scrollable
//! [`DataTable`]: crate::widget::data_table::DataTable
//! [`DocumentViewport`]: crate::widget::DocumentViewport

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use crate::theme::Theme;
use crate::widget::data_table::tier_on;

/// Minimum thumb length (logical px) so a near-infinite content still
/// leaves a grabbable target.
pub const MIN_THUMB_LEN: f64 = 24.0;

/// Fraction of a viewport scrolled when paging via a track click.
pub const PAGE_FRACTION: f64 = 0.9;

/// Content must exceed the viewport by more than this (logical px) on an
/// axis before it is considered scrollable — otherwise the content
/// "fits" and the bar should be hidden entirely.
pub const FIT_EPS: f64 = 0.5;

/// Alpha of the track behind a pinned or expanded bar — the faintest
/// derived tier, so the track reads as a groove and not a second thumb.
pub const TRACK_ALPHA: u8 = 13;

/// The thumb's fill: the tertiary ink at rest and the secondary ink while
/// hovered, dragged or focused, each stepped out until it clears the
/// non-text floor (3:1) against the ground it scrolls over. One answer for
/// every scroll owner.
#[must_use]
pub fn thumb_color(theme: &Theme, active: bool) -> Color {
    let colors = &theme.tokens.colors;
    let ground = colors.surfaces.content;
    let tone = if active {
        colors.text_dim
    } else {
        colors.text_tertiary
    };
    colors.ink_on(ground, tone, crate::tokens::colors::NON_TEXT_MIN_CONTRAST)
}

/// The track's fill behind an expanded bar, derived against the base surface.
#[must_use]
pub fn track_color(theme: &Theme) -> Color {
    tier_on(theme.tokens.colors.surfaces.base, TRACK_ALPHA)
}

/// The scroll geometry along one axis, all lengths in logical px.
///
/// `content` is the full scrollable extent, `viewport` the visible
/// window, and `offset` the current scroll position (`0` = start). The
/// offset is *not* assumed pre-clamped — every derived quantity clamps
/// defensively.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    pub content: f64,
    pub viewport: f64,
    pub offset: f64,
}

impl ScrollMetrics {
    pub fn new(content: f64, viewport: f64, offset: f64) -> Self {
        Self {
            content,
            viewport,
            offset,
        }
    }

    /// The maximum reachable offset — how far the content can travel past
    /// the viewport. Zero when the content fits.
    pub fn max_scroll(&self) -> f64 {
        (self.content - self.viewport).max(0.0)
    }

    /// Whether the content overflows the viewport by more than [`FIT_EPS`]
    /// (i.e. a bar is worth showing).
    pub fn overflows(&self) -> bool {
        self.content > self.viewport + FIT_EPS && self.viewport > 0.0
    }
}

/// Outcome of a press hit-tested against one bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PressOutcome {
    /// The press missed this bar (neither thumb nor track).
    Miss,
    /// The press landed on the thumb and a drag has begun. The owner
    /// should mark itself dirty and capture the pointer; no offset change
    /// happens until [`Scrollbar::drag_to`].
    Thumb,
    /// The press landed on the track outside the thumb. The owner should
    /// set its offset to this value (already clamped to `[0, max_scroll]`).
    Page(f64),
}

/// Anchors captured when a thumb grab begins so a cursor delta maps to an
/// offset delta without snapping the thumb centre to the cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DragAnchor {
    /// Cursor coordinate along the drag axis at grab time.
    grab_cursor: f64,
    /// Offset at grab time.
    grab_offset: f64,
}

/// A single-axis scrollbar interaction model.
///
/// Construct one per axis ([`Scrollbar::vertical`] / [`Scrollbar::horizontal`]),
/// keep it alongside the owning widget's offset, and drive it from the
/// widget's event handler:
///
/// ```ignore
/// // press
/// match bar.press(track, metrics, cursor) {
///     PressOutcome::Thumb   => { /* dirty + capture */ }
///     PressOutcome::Page(o) => { self.set_offset(o); }
///     PressOutcome::Miss    => { /* fall through */ }
/// }
/// // move
/// if let Some(o) = bar.drag_to(track, metrics, cursor) { self.set_offset(o); }
/// // release
/// bar.release();
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Scrollbar {
    vertical: bool,
    min_thumb: f64,
    page_fraction: f64,
    drag: Option<DragAnchor>,
    hover: bool,
}

impl Scrollbar {
    fn new(vertical: bool) -> Self {
        Self {
            vertical,
            min_thumb: MIN_THUMB_LEN,
            page_fraction: PAGE_FRACTION,
            drag: None,
            hover: false,
        }
    }

    /// A vertical bar (thumb travels along Y, lives on the right edge).
    pub fn vertical() -> Self {
        Self::new(true)
    }

    /// A horizontal bar (thumb travels along X, lives on the bottom edge).
    pub fn horizontal() -> Self {
        Self::new(false)
    }

    /// Override the minimum thumb length (default [`MIN_THUMB_LEN`]).
    #[must_use]
    pub fn with_min_thumb(mut self, px: f64) -> Self {
        self.min_thumb = px;
        self
    }

    /// Override the page fraction (default [`PAGE_FRACTION`]).
    #[must_use]
    pub fn with_page_fraction(mut self, frac: f64) -> Self {
        self.page_fraction = frac;
        self
    }

    pub fn is_vertical(&self) -> bool {
        self.vertical
    }

    /// Whether a thumb drag is currently in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Whether the pointer is currently over the thumb.
    pub fn is_hovered(&self) -> bool {
        self.hover
    }

    /// Whether the thumb should render in its brightened state (hovered or
    /// dragging).
    pub fn is_active(&self) -> bool {
        self.hover || self.drag.is_some()
    }

    /// Paint the shared quiet thumb for hosts with a custom content viewport.
    /// Geometry is identical to pointer hit-testing and follows the host's metrics.
    pub fn draw(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        track: Rect,
        metrics: ScrollMetrics,
        theme: &crate::theme::Theme,
        focused: bool,
    ) {
        let Some(thumb) = self.thumb_rect(track, metrics) else {
            return;
        };
        let radius = thumb.width().min(thumb.height()) / 2.0;
        let color = thumb_color(theme, self.is_active() || focused);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &vello::kurbo::RoundedRect::from_rect(thumb, radius),
        );
        if focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.you,
                None,
                &vello::kurbo::RoundedRect::from_rect(thumb.inflate(2.0, 2.0), radius + 2.0),
            );
        }
    }

    // --- axis helpers --------------------------------------------------

    fn axis_len(&self, r: Rect) -> f64 {
        if self.vertical { r.height() } else { r.width() }
    }
    fn axis_lo(&self, r: Rect) -> f64 {
        if self.vertical { r.y0 } else { r.x0 }
    }
    fn axis_hi(&self, r: Rect) -> f64 {
        if self.vertical { r.y1 } else { r.x1 }
    }
    fn cursor_axis(&self, p: Point) -> f64 {
        if self.vertical { p.y } else { p.x }
    }

    // --- geometry ------------------------------------------------------

    /// The thumb rect within `track` for the given `metrics`, or `None`
    /// when there's no meaningful content to represent. The owner is
    /// responsible for only supplying a `track` when the bar is actually
    /// shown (content overflows); this method does not re-check overflow so
    /// the thumb stays visible while a drag/clamp settles at an edge.
    pub fn thumb_rect(&self, track: Rect, m: ScrollMetrics) -> Option<Rect> {
        let track_len = self.axis_len(track);
        if m.content <= 0.0 || track_len <= 0.0 {
            return None;
        }
        let thumb_len =
            (m.viewport / m.content * track_len).clamp(self.min_thumb.min(track_len), track_len);
        let max_scroll = m.max_scroll();
        let frac = if max_scroll > 0.0 {
            (m.offset / max_scroll).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let lo = self.axis_lo(track) + frac * (track_len - thumb_len);
        Some(if self.vertical {
            Rect::new(track.x0, lo, track.x1, lo + thumb_len)
        } else {
            Rect::new(lo, track.y0, lo + thumb_len, track.y1)
        })
    }

    // --- interaction ---------------------------------------------------

    /// Set the hover state directly; returns `true` when it changed (so the
    /// owner can schedule a redraw only on an edge).
    pub fn set_hover(&mut self, hovered: bool) -> bool {
        if self.hover == hovered {
            false
        } else {
            self.hover = hovered;
            true
        }
    }

    /// Recompute hover from the cursor against the thumb; returns `true`
    /// when the hover state changed. A no-op while dragging (the thumb
    /// stays "hot").
    pub fn update_hover(&mut self, track: Rect, m: ScrollMetrics, cursor: Point) -> bool {
        if self.drag.is_some() {
            return false;
        }
        let over = self
            .thumb_rect(track, m)
            .is_some_and(|t| t.contains(cursor));
        self.set_hover(over)
    }

    /// Hit-test a press against the bar and, if it lands on the thumb,
    /// begin a drag. See [`PressOutcome`].
    pub fn press(&mut self, track: Rect, m: ScrollMetrics, cursor: Point) -> PressOutcome {
        let Some(thumb) = self.thumb_rect(track, m) else {
            return PressOutcome::Miss;
        };
        if thumb.contains(cursor) {
            self.drag = Some(DragAnchor {
                grab_cursor: self.cursor_axis(cursor),
                grab_offset: m.offset,
            });
            self.hover = true;
            return PressOutcome::Thumb;
        }
        if track.contains(cursor) {
            let page = m.viewport * self.page_fraction;
            let c = self.cursor_axis(cursor);
            let dir = if c < self.axis_lo(thumb) {
                -1.0
            } else if c > self.axis_hi(thumb) {
                1.0
            } else {
                0.0
            };
            let target = (m.offset + dir * page).clamp(0.0, m.max_scroll());
            return PressOutcome::Page(target);
        }
        PressOutcome::Miss
    }

    /// Map the live cursor to an absolute offset for the active drag, or
    /// `None` when no drag is in progress. The thumb travels
    /// `track_len - thumb_len` px across the full content range, so that
    /// ratio converts a cursor delta into a content delta. The result is
    /// clamped to `[0, max_scroll]`.
    pub fn drag_to(&self, track: Rect, m: ScrollMetrics, cursor: Point) -> Option<f64> {
        let anchor = self.drag?;
        let thumb = self.thumb_rect(track, m)?;
        let travel = (self.axis_len(track) - self.axis_len(thumb)).max(1e-3);
        let max_scroll = m.max_scroll();
        let delta_px = self.cursor_axis(cursor) - anchor.grab_cursor;
        let target = anchor.grab_offset + (delta_px / travel) * max_scroll;
        Some(target.clamp(0.0, max_scroll))
    }

    /// End any active drag; returns `true` when a drag was actually in
    /// progress (so the owner can mark itself dirty / release capture).
    pub fn release(&mut self) -> bool {
        self.drag.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 300-px-tall vertical track spanning x∈[92,100], y∈[0,300].
    fn v_track() -> Rect {
        Rect::new(92.0, 0.0, 100.0, 300.0)
    }

    /// The thumb clears the non-text floor against the ground it scrolls
    /// over, at rest and active, on every material; active is the stronger.
    #[test]
    fn the_thumb_clears_the_non_text_floor_on_every_material() {
        use crate::a11y::contrast_ratio;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.content;
            let idle = contrast_ratio(thumb_color(&theme, false), ground);
            let active = contrast_ratio(thumb_color(&theme, true), ground);
            assert!(
                idle >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "{}: idle thumb {idle:.2}:1",
                theme.name
            );
            assert!(
                active >= idle,
                "{}: active {active:.2} < idle {idle:.2}",
                theme.name
            );
        }
    }

    #[test]
    fn thumb_geometry_follows_content_ratio_offset_and_axis() {
        // Case `thumb_shrinks_as_content_grows`.
        {
            let bar = Scrollbar::vertical();
            let short = bar
                .thumb_rect(v_track(), ScrollMetrics::new(600.0, 300.0, 0.0))
                .unwrap();
            let tall = bar
                .thumb_rect(v_track(), ScrollMetrics::new(1200.0, 300.0, 0.0))
                .unwrap();
            assert!(short.height() > tall.height());
            // 2× content → thumb ≈ half the 300-px track.
            assert!(
                (short.height() - 150.0).abs() < 1.0,
                "got {}",
                short.height()
            );
        }

        // Case `thumb_clamped_to_minimum`.
        {
            let bar = Scrollbar::vertical();
            let thumb = bar
                .thumb_rect(v_track(), ScrollMetrics::new(100_000.0, 300.0, 0.0))
                .unwrap();
            assert!(thumb.height() >= MIN_THUMB_LEN - 1e-6);
        }

        // Case `thumb_tracks_offset_top_to_bottom`.
        {
            let bar = Scrollbar::vertical();
            let m0 = ScrollMetrics::new(1200.0, 300.0, 0.0);
            let top = bar.thumb_rect(v_track(), m0).unwrap();
            assert!((top.y0 - 0.0).abs() < 1e-6, "thumb starts at track top");
            // max_scroll = 900; fully scrolled thumb bottom == track bottom.
            let m1 = ScrollMetrics::new(1200.0, 300.0, 900.0);
            let bottom = bar.thumb_rect(v_track(), m1).unwrap();
            assert!((bottom.y1 - 300.0).abs() < 1e-3, "got {}", bottom.y1);
        }

        // Case `horizontal_axis_uses_x`.
        {
            let mut bar = Scrollbar::horizontal();
            let track = Rect::new(0.0, 92.0, 300.0, 100.0);
            let m = ScrollMetrics::new(1200.0, 300.0, 0.0);
            let thumb = bar.thumb_rect(track, m).unwrap();
            assert!(
                (thumb.x0 - 0.0).abs() < 1e-6,
                "h-thumb starts at track left"
            );
            assert!(thumb.width() < track.width(), "h-thumb shorter than track");
            let mid = Point::new(thumb.x0.midpoint(thumb.x1), 96.0);
            assert_eq!(bar.press(track, m, mid), PressOutcome::Thumb);
            let off = bar
                .drag_to(track, m, Point::new(mid.x + 100.0, 96.0))
                .unwrap();
            assert!(off > 0.0, "h-drag right increases offset: {off}");
        }
    }

    #[test]
    fn thumb_and_track_presses_drag_and_page_within_bounds() {
        // Case `press_thumb_begins_drag_and_move_updates_offset`.
        {
            let mut bar = Scrollbar::vertical();
            let m = ScrollMetrics::new(1200.0, 300.0, 0.0); // max_scroll = 900
            let thumb = bar.thumb_rect(v_track(), m).unwrap();
            let mid = Point::new(96.0, thumb.y0.midpoint(thumb.y1));
            assert_eq!(bar.press(v_track(), m, mid), PressOutcome::Thumb);
            assert!(bar.is_dragging());
            // Drag down 100 px. travel = 300 - thumb_len(=75) = 225.
            // Δoffset = 100/225 * 900 = 400.
            let moved = Point::new(96.0, mid.y + 100.0);
            let off = bar.drag_to(v_track(), m, moved).unwrap();
            assert!((off - 400.0).abs() < 1.0, "got {off}");
            assert!(bar.release());
            assert!(!bar.is_dragging());
        }

        // Case `drag_offset_clamped_at_ends`.
        {
            let mut bar = Scrollbar::vertical();
            let m = ScrollMetrics::new(1200.0, 300.0, 0.0);
            let thumb = bar.thumb_rect(v_track(), m).unwrap();
            let mid = Point::new(96.0, thumb.y0.midpoint(thumb.y1));
            bar.press(v_track(), m, mid);
            // Huge downward drag → clamped to max_scroll.
            let off = bar
                .drag_to(v_track(), m, Point::new(96.0, mid.y + 10_000.0))
                .unwrap();
            assert!((off - 900.0).abs() < 1e-6, "clamped to max: {off}");
            // Huge upward drag → clamped to 0.
            let off = bar
                .drag_to(v_track(), m, Point::new(96.0, mid.y - 10_000.0))
                .unwrap();
            assert!((off - 0.0).abs() < 1e-6, "clamped to 0: {off}");
        }

        // Case `press_track_below_thumb_pages_down`.
        {
            let mut bar = Scrollbar::vertical();
            let m = ScrollMetrics::new(1200.0, 300.0, 0.0);
            let thumb = bar.thumb_rect(v_track(), m).unwrap();
            let below = Point::new(96.0, thumb.y1 + 5.0);
            match bar.press(v_track(), m, below) {
                PressOutcome::Page(o) => {
                    assert!(
                        (o - 300.0 * PAGE_FRACTION).abs() < 1.0,
                        "page ≈ viewport*frac: {o}"
                    );
                }
                other => panic!("expected Page, got {other:?}"),
            }
            assert!(!bar.is_dragging(), "track click is not a drag");
        }

        // Case `press_track_above_thumb_pages_up_clamped`.
        {
            let mut bar = Scrollbar::vertical();
            // Already at top → paging up clamps to 0.
            let m = ScrollMetrics::new(1200.0, 300.0, 0.0);
            let thumb = bar.thumb_rect(v_track(), m).unwrap();
            // Force a non-zero start so "above" exists; use offset in the middle.
            let m_mid = ScrollMetrics::new(1200.0, 300.0, 450.0);
            let thumb_mid = bar.thumb_rect(v_track(), m_mid).unwrap();
            let above = Point::new(96.0, thumb_mid.y0 - 5.0);
            match bar.press(v_track(), m_mid, above) {
                PressOutcome::Page(o) => assert!(o < 450.0, "paged up from 450: {o}"),
                other => panic!("expected Page, got {other:?}"),
            }
            let _ = thumb; // silence unused in the top branch
        }

        // Case `hover_edge_detection`.
        {
            let mut bar = Scrollbar::vertical();
            let m = ScrollMetrics::new(1200.0, 300.0, 0.0);
            let thumb = bar.thumb_rect(v_track(), m).unwrap();
            let on = Point::new(96.0, thumb.y0.midpoint(thumb.y1));
            assert!(bar.update_hover(v_track(), m, on), "hover turns on");
            assert!(
                !bar.update_hover(v_track(), m, on),
                "no change while still on"
            );
            assert!(bar.is_hovered());
            let off = Point::new(96.0, thumb.y1 + 50.0);
            assert!(bar.update_hover(v_track(), m, off), "hover turns off");
            assert!(!bar.is_hovered());
        }

        // Case `press_misses_when_content_fits`.
        {
            let mut bar = Scrollbar::vertical();
            // content fits viewport → max_scroll 0, thumb fills the track.
            let m = ScrollMetrics::new(300.0, 300.0, 0.0);
            // A press outside the (full-length) thumb still misses the track
            // edges; a press inside begins a drag that can't move (max 0).
            let outside = Point::new(96.0, 400.0);
            assert_eq!(bar.press(v_track(), m, outside), PressOutcome::Miss);
        }
    }
}

#[cfg(test)]
mod primary_region_tests {
    use super::*;
    #[test]
    fn semantic_primary_offsets_are_finite_clamped_and_geometry_owned() {
        let metrics = ScrollMetrics::new(1000.0, 200.0, 96.0);
        assert_eq!(PrimaryScrollAction::Line(1).offset(metrics), Some(144.0));
        assert_eq!(PrimaryScrollAction::Page(-1).offset(metrics), Some(0.0));
        assert_eq!(
            PrimaryScrollAction::Absolute(9999.0).offset(metrics),
            Some(800.0)
        );
        assert_eq!(PrimaryScrollAction::Start.offset(metrics), Some(0.0));
        assert_eq!(PrimaryScrollAction::End.offset(metrics), Some(800.0));
        assert_eq!(
            PrimaryScrollAction::Absolute(f64::NAN).offset(metrics),
            None
        );
        assert_eq!(PrimaryScrollAction::Line(0).offset(metrics), None);
        assert_eq!(
            PrimaryScrollAction::End.offset(ScrollMetrics::new(f64::INFINITY, 20.0, 0.0)),
            None
        );
    }
}

/// The application's explicit primary region. Owners stamp component identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrimaryScrollRegion {
    pub key: &'static str,
    pub owner_generation: Option<uuid::Uuid>,
    pub metrics: ScrollMetrics,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimaryScrollAction {
    Line(i8),
    Page(i8),
    Absolute(f64),
    Start,
    End,
}
impl PrimaryScrollAction {
    pub fn offset(self, m: ScrollMetrics) -> Option<f64> {
        if !m.content.is_finite()
            || !m.viewport.is_finite()
            || !m.offset.is_finite()
            || m.content < 0.0
            || m.viewport < 0.0
        {
            return None;
        }
        let next = match self {
            Self::Line(sign @ (-1 | 1)) => m.offset + f64::from(sign) * 48.0,
            Self::Page(sign @ (-1 | 1)) => m.offset + f64::from(sign) * m.viewport * PAGE_FRACTION,
            Self::Absolute(offset) => offset,
            Self::Start => 0.0,
            Self::End => m.max_scroll(),
            _ => return None,
        };
        next.is_finite().then(|| next.clamp(0.0, m.max_scroll()))
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimaryScrollResult {
    Unsupported,
    Unchanged(PrimaryScrollRegion),
    Changed(PrimaryScrollRegion),
}
impl PrimaryScrollResult {
    #[must_use]
    pub fn stamp(self, generation: uuid::Uuid) -> Self {
        match self {
            Self::Unsupported => self,
            Self::Unchanged(mut r) => {
                r.owner_generation = Some(generation);
                Self::Unchanged(r)
            }
            Self::Changed(mut r) => {
                r.owner_generation = Some(generation);
                Self::Changed(r)
            }
        }
    }
}
