//! A chronological index for large, virtualised collections.
//!
//! [`DateIndexedScrollbar`] deliberately sits one layer above [`Scrollbar`]:
//! the latter owns pixel-accurate thumb behaviour while this type understands
//! that a collection has meaningful places in it (months, years, chapters,
//! releases, …).  It is an immediate-mode face and interaction model, rather
//! than a Photos control, so a reader, activity feed, media browser, or
//! search result can use the same date-indexed affordance without retaining
//! thousands of row widgets.
//!
//! The host remains authoritative for its virtual-list offset.  Feed the
//! model its `ScrollMetrics`, apply the returned [`DateIndexIntent`], and
//! re-paint.  This keeps scrolling smooth and lets a host preserve an item
//! anchor while changing thumbnail density.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Action, Role};
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::{PressOutcome, ScrollMetrics, Scrollbar};

/// The highest number of date markers a single frame paints or exports to
/// accessibility.  A ten-year collection may have 120 monthly boundaries;
/// presenting all of them in a 700px track is both unreadable and wasteful.
pub const MAX_PROJECTED_MARKERS: usize = 28;
/// Wider than the visible track, allowing a calm label preview when hovering
/// beside the bar while keeping the actual thumb target precise.
pub const PREVIEW_LANE_WIDTH: f64 = 156.0;
/// The marker hit size is independent of its one-pixel visual tick.
pub const MARKER_HIT_RADIUS: f64 = 12.0;
/// A compact but touch-reachable date-index track.
pub const DEFAULT_TRACK_WIDTH: f64 = 16.0;

/// A durable chronological boundary supplied by the collection owner.
///
/// `position` is normalised across the *logical* collection (`0.0` first,
/// `1.0` last), not the currently materialised slice.  This is important for
/// virtual lists: jumping to March must work even when March has not been
/// decoded or mounted yet.
#[derive(Clone, Debug, PartialEq)]
pub struct DateIndexMarker {
    /// Stable collection-owned id.  It is used in AT action targets, so do not
    /// derive it from a visible/localised label.
    pub id: String,
    /// Localised, concise text such as "Mar 2026" or "2023".
    pub label: String,
    /// Normalised position, clamped defensively before use.
    pub position: f64,
    /// Year boundaries survive projection before ordinary month boundaries.
    pub kind: DateIndexMarkerKind,
}

impl DateIndexMarker {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        position: f64,
        kind: DateIndexMarkerKind,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            position,
            kind,
        }
    }

    fn clamped_position(&self) -> f64 {
        if self.position.is_finite() {
            self.position.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Relative importance of a date boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DateIndexMarkerKind {
    /// A coarse boundary (usually a year) that stays visible under pressure.
    Major,
    /// A normal boundary (usually a month).
    Minor,
}

/// A projected marker with its pixel position along the current track.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedDateMarker {
    pub id: String,
    pub label: String,
    pub kind: DateIndexMarkerKind,
    pub position: f64,
    pub axis: f64,
}

/// Value used in hover, focus, and live announcements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateIndexPreview {
    pub id: String,
    pub label: String,
}

/// What the host should do after one interaction.  It contains only stable
/// positions/ids, never an app-specific row index.
#[derive(Clone, Debug, PartialEq)]
pub enum DateIndexIntent {
    /// Move the virtual collection to an absolute, normalised position.
    ScrollTo { position: f64 },
    /// Change density while preserving the host's content anchor.  `1` means
    /// show more items at once, `-1` means fewer.
    AdjustDensity { delta: i8 },
    /// A hover/focus preview changed.  This is presentation-only and does
    /// not move the collection.
    PreviewChanged(Option<DateIndexPreview>),
}

/// A modern chronological scrollbar face plus its ephemeral interaction
/// state.  It intentionally has no document binding: scroll position and
/// density are per-user presentation preferences, not shared collection data.
#[derive(Clone, Debug)]
pub struct DateIndexedScrollbar {
    semantic_id: String,
    bar: Scrollbar,
    rect: Rect,
    focused: bool,
    hovered: bool,
    last_cursor: Point,
    preview: Option<DateIndexPreview>,
}

impl DateIndexedScrollbar {
    pub fn new(semantic_id: impl Into<String>) -> Self {
        Self {
            semantic_id: semantic_id.into(),
            bar: Scrollbar::vertical().with_min_thumb(32.0),
            rect: Rect::ZERO,
            focused: false,
            hovered: false,
            last_cursor: Point::ZERO,
            preview: None,
        }
    }

    /// The narrow right-edge track used by the widget.  Hosts may provide a
    /// wider rect for touch-first surfaces; the marker lane expands leftward.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn is_dragging(&self) -> bool {
        self.bar.is_dragging()
    }

    pub fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn preview(&self) -> Option<&DateIndexPreview> {
        self.preview.as_ref()
    }

    /// Whether an immediate-mode host should offer this widget first refusal
    /// of a pointer press/release.  It includes the visible track and the
    /// adjacent marker/preview lane, never the collection content itself.
    pub fn contains_interaction_point(&self, point: Point) -> bool {
        self.preview_lane().contains(point)
    }

    /// Normalise an offset without trusting callers to have clamped it.
    pub fn position(metrics: ScrollMetrics) -> f64 {
        let max = metrics.max_scroll();
        if max > 0.0 {
            (metrics.offset / max).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Convert an absolute position back into the host's logical offset.
    pub fn offset_for(metrics: ScrollMetrics, position: f64) -> f64 {
        position.clamp(0.0, 1.0) * metrics.max_scroll()
    }

    /// Project date markers into at most `limit` legible ticks.  Major
    /// boundaries and the first/last available boundary are retained first;
    /// the remainder is evenly sampled in stable chronological order.  The
    /// operation is O(n) over the already-small boundary list and produces a
    /// bounded result for paint *and* AT export.
    pub fn project_markers(
        &self,
        markers: &[DateIndexMarker],
        limit: usize,
    ) -> Vec<ProjectedDateMarker> {
        let limit = limit.clamp(2, MAX_PROJECTED_MARKERS);
        if markers.is_empty() || self.rect.height() <= 0.0 {
            return Vec::new();
        }
        let mut ordered: Vec<_> = markers
            .iter()
            .filter(|marker| marker.position.is_finite())
            .collect();
        ordered.sort_by(|left, right| {
            left.clamped_position()
                .total_cmp(&right.clamped_position())
                .then_with(|| left.id.cmp(&right.id))
        });
        if ordered.is_empty() {
            return Vec::new();
        }

        let mut selected = vec![false; ordered.len()];
        selected[0] = true;
        selected[ordered.len().saturating_sub(1)] = true;
        let mut used = selected.iter().filter(|chosen| **chosen).count();
        for (index, marker) in ordered.iter().enumerate() {
            if used >= limit {
                break;
            }
            if marker.kind == DateIndexMarkerKind::Major && !selected[index] {
                selected[index] = true;
                used += 1;
            }
        }
        // Evenly sample unchosen markers, retaining chronology.  Round to a
        // bounded index only after clamping the ratio, so huge lists cannot
        // turn a float conversion into an out-of-range index.
        if used < limit {
            let slots = limit - used;
            for step in 1..=slots {
                let ratio = step as f64 / (slots + 1) as f64;
                let index = (ratio * (ordered.len().saturating_sub(1)) as f64)
                    .round()
                    .clamp(0.0, ordered.len().saturating_sub(1) as f64)
                    as usize;
                if !selected[index] {
                    selected[index] = true;
                    used += 1;
                }
            }
            // Duplicated rounded samples are possible in a tiny collection;
            // fill the remaining bounded budget in input order.
            for index in 0..ordered.len() {
                if used >= limit {
                    break;
                }
                if !selected[index] {
                    selected[index] = true;
                    used += 1;
                }
            }
        }
        ordered
            .iter()
            .enumerate()
            .filter(|(index, _)| selected[*index])
            .map(|(_, marker)| ProjectedDateMarker {
                id: marker.id.clone(),
                label: marker.label.clone(),
                kind: marker.kind,
                position: marker.clamped_position(),
                axis: self.rect.y0 + marker.clamped_position() * self.rect.height(),
            })
            .collect()
    }

    fn preview_lane(&self) -> Rect {
        Rect::new(
            self.rect.x0 - PREVIEW_LANE_WIDTH,
            self.rect.y0 - MARKER_HIT_RADIUS,
            self.rect.x1 + MARKER_HIT_RADIUS,
            self.rect.y1 + MARKER_HIT_RADIUS,
        )
    }

    fn nearest_preview(
        &self,
        markers: &[ProjectedDateMarker],
        cursor: Point,
    ) -> Option<DateIndexPreview> {
        if !self.preview_lane().contains(cursor) {
            return None;
        }
        markers
            .iter()
            .min_by(|left, right| {
                (left.axis - cursor.y)
                    .abs()
                    .total_cmp(&(right.axis - cursor.y).abs())
            })
            .filter(|marker| (marker.axis - cursor.y).abs() <= MARKER_HIT_RADIUS)
            .map(|marker| DateIndexPreview {
                id: marker.id.clone(),
                label: marker.label.clone(),
            })
    }

    fn set_preview(&mut self, preview: Option<DateIndexPreview>) -> Option<DateIndexIntent> {
        if self.preview == preview {
            None
        } else {
            self.preview = preview.clone();
            Some(DateIndexIntent::PreviewChanged(preview))
        }
    }

    /// Pointer movement updates the hover label and, while captured, returns
    /// an absolute scroll target.  Call it even after the cursor leaves the
    /// track; that is what makes a thumb drag reliable.
    pub fn pointer_moved(
        &mut self,
        cursor: Point,
        metrics: ScrollMetrics,
        markers: &[DateIndexMarker],
    ) -> Option<DateIndexIntent> {
        self.last_cursor = cursor;
        let projected = self.project_markers(markers, MAX_PROJECTED_MARKERS);
        self.hovered = self.preview_lane().contains(cursor);
        if self.bar.is_dragging() {
            return self.bar.drag_to(self.rect, metrics, cursor).map(|offset| {
                DateIndexIntent::ScrollTo {
                    position: Self::position(ScrollMetrics::new(
                        metrics.content,
                        metrics.viewport,
                        offset,
                    )),
                }
            });
        }
        self.bar.update_hover(self.rect, metrics, cursor);
        self.set_preview(self.nearest_preview(&projected, cursor))
    }

    /// Press on a marker jumps precisely to that date; press on the thumb
    /// starts a captured drag; press elsewhere on the track follows the
    /// standard shared scrollbar page behaviour.
    pub fn press(
        &mut self,
        metrics: ScrollMetrics,
        markers: &[DateIndexMarker],
    ) -> Option<DateIndexIntent> {
        if self.contains_interaction_point(self.last_cursor) {
            self.focused = true;
        }
        let projected = self.project_markers(markers, MAX_PROJECTED_MARKERS);
        if let Some(marker) = projected.iter().find(|marker| {
            self.preview_lane().contains(self.last_cursor)
                && (marker.axis - self.last_cursor.y).abs() <= MARKER_HIT_RADIUS
        }) {
            return Some(DateIndexIntent::ScrollTo {
                position: marker.position,
            });
        }
        match self.bar.press(self.rect, metrics, self.last_cursor) {
            PressOutcome::Miss | PressOutcome::Thumb => None,
            PressOutcome::Page(offset) => Some(DateIndexIntent::ScrollTo {
                position: Self::position(ScrollMetrics::new(
                    metrics.content,
                    metrics.viewport,
                    offset,
                )),
            }),
        }
    }

    pub fn release(&mut self) -> bool {
        self.bar.release()
    }

    /// Convert a regular scroll wheel / two-finger scroll delta into an
    /// absolute timeline position.  The host may decline this when another
    /// focused surface owns the gesture.
    pub fn wheel(
        &self,
        delta: MouseScrollDelta,
        metrics: ScrollMetrics,
    ) -> Option<DateIndexIntent> {
        if !metrics.overflows() {
            return None;
        }
        let px = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 48.0,
            MouseScrollDelta::PixelDelta(position) => position.y,
        };
        if !px.is_finite() || px == 0.0 {
            return None;
        }
        let offset = (metrics.offset - px).clamp(0.0, metrics.max_scroll());
        Some(DateIndexIntent::ScrollTo {
            position: Self::position(ScrollMetrics::new(
                metrics.content,
                metrics.viewport,
                offset,
            )),
        })
    }

    /// Keyboard parity for pointer behaviour.  The normal arrows nudge the
    /// position, Page keys move a viewport, Home/End jump to endpoints, and
    /// `+`/`-` expose density without requiring a trackpad gesture.
    pub fn key(&self, key: &Key, metrics: ScrollMetrics) -> Option<DateIndexIntent> {
        let current = Self::position(metrics);
        let page = if metrics.content > 0.0 {
            (metrics.viewport / metrics.content).clamp(0.02, 1.0)
        } else {
            1.0
        };
        let position = match key {
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => Some(current - 0.05),
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => Some(current + 0.05),
            Key::Named(NamedKey::PageUp) => Some(current - page),
            Key::Named(NamedKey::PageDown) => Some(current + page),
            Key::Named(NamedKey::Home) => Some(0.0),
            Key::Named(NamedKey::End) => Some(1.0),
            Key::Character(value) if value == "+" || value == "=" => {
                return Some(DateIndexIntent::AdjustDensity { delta: 1 });
            }
            Key::Character(value) if value == "-" || value == "_" => {
                return Some(DateIndexIntent::AdjustDensity { delta: -1 });
            }
            _ => None,
        };
        position.map(|position| DateIndexIntent::ScrollTo {
            position: position.clamp(0.0, 1.0),
        })
    }

    /// Translate a winit event for immediate-mode hosts.  This deliberately
    /// does not consume pinch gestures: the collection needs the raw pinch
    /// anchor to keep the same item under a user's fingers while density
    /// changes, so `PinchGesture` is exposed as an integration seam instead.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        metrics: ScrollMetrics,
        markers: &[DateIndexMarker],
    ) -> Option<DateIndexIntent> {
        match event {
            WindowEvent::CursorMoved { position, .. } => self.pointer_moved(
                Point::new(position.x / scale_factor, position.y / scale_factor),
                metrics,
                markers,
            ),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(metrics, markers),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.release() => None,
            WindowEvent::MouseWheel { delta, .. } if self.hovered || self.focused => {
                self.wheel(*delta, metrics)
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.focused =>
            {
                self.key(&event.logical_key, metrics)
            }
            _ => None,
        }
    }

    /// Stable synthetic semantics for a custom canvas host.  The slider is
    /// one focus target; projected major/minor boundaries are discoverable
    /// jump buttons, bounded with the same projection as paint.
    pub fn accessibility(&self, metrics: ScrollMetrics, markers: &[DateIndexMarker]) -> AccessNode {
        let value = self
            .preview
            .as_ref()
            .map(|preview| preview.label.clone())
            .unwrap_or_else(|| format!("{:.0}%", Self::position(metrics) * 100.0));
        let mut children = vec![AccessNode::new(Role::Slider)
            .with_name("Timeline position")
            .with_description(
                "Chronological collection. Arrow keys move, Page Up and Page Down move a screen, Home and End jump to the beginning or end; plus and minus change density.",
            )
            .with_value(value)
            .with_bounds(self.rect)
            .with_state(AccessState::new().focusable(true).focused(self.focused))
            .with_action_target(format!("{}:position", self.semantic_id))];
        for marker in self.project_markers(markers, 12) {
            let bounds = Rect::new(
                self.rect.x0 - PREVIEW_LANE_WIDTH,
                marker.axis - MARKER_HIT_RADIUS,
                self.rect.x1 + MARKER_HIT_RADIUS,
                marker.axis + MARKER_HIT_RADIUS,
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Jump to {}", marker.label))
                    .with_bounds(bounds)
                    .with_state(AccessState::new().focusable(true))
                    .with_action_target(format!("{}:jump:{}", self.semantic_id, marker.id)),
            );
        }
        AccessNode::new(Role::Group)
            .with_name("Chronological index")
            .with_description("Date-indexed scrollbar")
            .with_bounds(self.preview_lane())
            .with_semantic_key(format!("{}:index", self.semantic_id))
            .with_children(children)
    }

    /// Route AT value and jump actions through the same intent path as real
    /// pointer/keyboard input.  Hosts should use this instead of separately
    /// mutating their scroll state from accessibility actions.
    pub fn perform_access_action(
        &mut self,
        target: &str,
        action: &Action,
        metrics: ScrollMetrics,
        markers: &[DateIndexMarker],
    ) -> Option<DateIndexIntent> {
        let prefix = format!("{}:", self.semantic_id);
        let local = target.strip_prefix(&prefix)?;
        match (local, action) {
            ("position", Action::Increment) => self.key(&Key::Named(NamedKey::ArrowDown), metrics),
            ("position", Action::Decrement) => self.key(&Key::Named(NamedKey::ArrowUp), metrics),
            ("position", Action::SetValue(value)) => {
                let parsed = value.trim().trim_end_matches('%').parse::<f64>().ok()?;
                if !parsed.is_finite() {
                    return None;
                }
                let position = if parsed > 1.0 { parsed / 100.0 } else { parsed };
                Some(DateIndexIntent::ScrollTo {
                    position: position.clamp(0.0, 1.0),
                })
            }
            (jump, Action::Activate) if jump.starts_with("jump:") => {
                let id = jump.trim_start_matches("jump:");
                markers.iter().find(|marker| marker.id == id).map(|marker| {
                    DateIndexIntent::ScrollTo {
                        position: marker.clamped_position(),
                    }
                })
            }
            _ => None,
        }
    }

    /// Paint the shared face.  No animation is required for meaning: hover,
    /// focus, and dragging all have static high-contrast affordances, so
    /// reduced-motion themes retain every piece of feedback.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: ScrollMetrics,
        markers: &[DateIndexMarker],
    ) {
        if !metrics.overflows() || self.rect.is_zero_area() {
            return;
        }
        let colors = &theme.tokens.colors;
        let lane = self.preview_lane();
        let active = self.hovered || self.focused || self.bar.is_active();
        if active {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.surface_white_5,
                None,
                &RoundedRect::from_rect(lane, 8.0),
            );
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.neutral[3],
            None,
            &RoundedRect::from_rect(self.rect, self.rect.width() / 2.0),
        );
        scene.stroke(
            &Stroke::new(if self.focused { 1.5 } else { 1.0 }),
            transform,
            if self.focused {
                theme.border_focus
            } else {
                theme.border_standard
            },
            None,
            &RoundedRect::from_rect(self.rect, self.rect.width() / 2.0),
        );
        let projected = self.project_markers(markers, MAX_PROJECTED_MARKERS);
        for marker in &projected {
            let width = if marker.kind == DateIndexMarkerKind::Major {
                9.0
            } else {
                5.0
            };
            let tick = Rect::new(
                self.rect.x0 - width - 3.0,
                marker.axis - 0.75,
                self.rect.x0 - 3.0,
                marker.axis + 0.75,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                if marker.kind == DateIndexMarkerKind::Major {
                    theme.accent_primary
                } else {
                    colors.border_white_20
                },
                None,
                &tick,
            );
        }
        if let Some(thumb) = self.bar.thumb_rect(self.rect, metrics) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                if active {
                    theme.accent_primary
                } else {
                    colors.surface_white_20
                },
                None,
                &RoundedRect::from_rect(thumb, thumb.width() / 2.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_white_20,
                None,
                &RoundedRect::from_rect(thumb, thumb.width() / 2.0),
            );
        }
        if let Some(preview) = &self.preview {
            if let Some(marker) = projected.iter().find(|marker| marker.id == preview.id) {
                let label_w = (preview.label.chars().count() as f64 * 7.2 + 22.0)
                    .clamp(72.0, PREVIEW_LANE_WIDTH - 8.0);
                let label_h = 26.0;
                let x1 = self.rect.x0 - 10.0;
                let label = Rect::new(
                    x1 - label_w,
                    (marker.axis - label_h / 2.0).clamp(lane.y0, lane.y1 - label_h),
                    x1,
                    (marker.axis + label_h / 2.0).clamp(lane.y0 + label_h, lane.y1),
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(label, 6.0),
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.border_standard,
                    None,
                    &RoundedRect::from_rect(label, 6.0),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &preview.label,
                    11.0,
                    theme.text_primary,
                    transform * Affine::translate((label.x0 + 9.0, label.y0 + 7.0)),
                    Some((label.width() - 18.0) as f32),
                    TextWeight::Medium,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrollbar() -> DateIndexedScrollbar {
        let mut scrollbar = DateIndexedScrollbar::new("test:chronology");
        scrollbar.set_rect(Rect::new(300.0, 20.0, 316.0, 420.0));
        scrollbar
    }

    fn metrics(offset: f64) -> ScrollMetrics {
        ScrollMetrics::new(10_000.0, 1_000.0, offset)
    }

    fn markers() -> Vec<DateIndexMarker> {
        vec![
            DateIndexMarker::new("2024", "2024", 0.0, DateIndexMarkerKind::Major),
            DateIndexMarker::new("2024-06", "Jun 2024", 0.25, DateIndexMarkerKind::Minor),
            DateIndexMarker::new("2025", "2025", 0.5, DateIndexMarkerKind::Major),
            DateIndexMarker::new("2025-10", "Oct 2025", 0.75, DateIndexMarkerKind::Minor),
            DateIndexMarker::new("2026", "2026", 1.0, DateIndexMarkerKind::Major),
        ]
    }

    #[test]
    fn marker_projection_is_bounded_ordered_and_keeps_years() {
        let bar = scrollbar();
        let mut input = Vec::new();
        for index in 0..120 {
            input.push(DateIndexMarker::new(
                format!("m{index}"),
                format!("Month {index}"),
                index as f64 / 119.0,
                if index % 12 == 0 {
                    DateIndexMarkerKind::Major
                } else {
                    DateIndexMarkerKind::Minor
                },
            ));
        }
        let projected = bar.project_markers(&input, 12);
        assert!(projected.len() <= 12);
        assert_eq!(
            projected.first().map(|marker| marker.id.as_str()),
            Some("m0")
        );
        assert_eq!(
            projected.last().map(|marker| marker.id.as_str()),
            Some("m119")
        );
        assert!(projected.iter().any(|marker| marker.id == "m12"));
        assert!(
            projected
                .windows(2)
                .all(|window| window[0].axis <= window[1].axis)
        );
    }

    #[test]
    fn pointer_marker_jump_uses_normalised_collection_position() {
        let mut bar = scrollbar();
        let all = markers();
        let moved = bar.pointer_moved(Point::new(305.0, 220.0), metrics(0.0), &all);
        assert!(matches!(
            moved,
            Some(DateIndexIntent::PreviewChanged(Some(_)))
        ));
        assert_eq!(
            bar.press(metrics(0.0), &all),
            Some(DateIndexIntent::ScrollTo { position: 0.5 })
        );
    }

    #[test]
    fn thumb_drag_keeps_tracking_after_pointer_leaves_lane() {
        let mut bar = scrollbar();
        let all = markers();
        let m = metrics(0.0);
        let thumb = bar.bar.thumb_rect(bar.rect, m).unwrap();
        let _ = bar.pointer_moved(Point::new(308.0, thumb.center().y), m, &all);
        assert_eq!(bar.press(m, &all), None);
        assert!(bar.is_dragging());
        let intent = bar
            .pointer_moved(Point::new(800.0, 360.0), m, &all)
            .expect("drag intent");
        assert!(matches!(intent, DateIndexIntent::ScrollTo { position } if position > 0.6));
    }

    #[test]
    fn keyboard_and_accessibility_share_absolute_scroll_intents() {
        let mut bar = scrollbar();
        bar.set_focus(true);
        let all = markers();
        assert_eq!(
            bar.key(&Key::Named(NamedKey::End), metrics(0.0)),
            Some(DateIndexIntent::ScrollTo { position: 1.0 })
        );
        assert_eq!(
            bar.perform_access_action(
                "test:chronology:jump:2025",
                &Action::Activate,
                metrics(0.0),
                &all
            ),
            Some(DateIndexIntent::ScrollTo { position: 0.5 })
        );
        assert_eq!(
            bar.perform_access_action(
                "test:chronology:position",
                &Action::SetValue("25%".into()),
                metrics(0.0),
                &all
            ),
            Some(DateIndexIntent::ScrollTo { position: 0.25 })
        );
    }

    #[test]
    fn semantics_are_bounded_and_have_stable_action_targets() {
        let bar = scrollbar();
        let semantics = bar.accessibility(metrics(0.0), &markers());
        assert_eq!(semantics.role, Role::Group);
        assert_eq!(semantics.children[0].role, Role::Slider);
        assert_eq!(
            semantics.children[0].action_target.as_deref(),
            Some("test:chronology:position")
        );
        assert!(semantics.children.len() <= 13);
        assert!(
            semantics.children[1]
                .action_target
                .as_deref()
                .is_some_and(|target| target.contains(":jump:"))
        );
    }

    #[test]
    fn plus_and_minus_offer_non_gesture_density_parity() {
        let bar = scrollbar();
        assert_eq!(
            bar.key(&Key::Character("+".into()), metrics(0.0)),
            Some(DateIndexIntent::AdjustDensity { delta: 1 })
        );
        assert_eq!(
            bar.key(&Key::Character("-".into()), metrics(0.0)),
            Some(DateIndexIntent::AdjustDensity { delta: -1 })
        );
    }
}
