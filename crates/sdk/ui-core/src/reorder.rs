//! Shared drag/reorder state for virtualized collection widgets.

use crate::time::{Duration, Instant};

use vello::kurbo::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DropPlacement {
    Before,
    Inside,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DropTarget<K> {
    pub key: K,
    pub placement: DropPlacement,
}

impl<K> DropTarget<K> {
    pub fn new(key: K, placement: DropPlacement) -> Self {
        Self { key, placement }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DropCandidate<K> {
    pub target: DropTarget<K>,
    /// Insertion line or inside-target highlight painted by the widget.
    pub preview: Rect,
    pub can_hover_expand: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReorderDrop<K> {
    pub source: K,
    pub target: DropTarget<K>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReorderMotion<K> {
    /// Logical pixels to add to the collection's scroll offset.
    pub scroll_delta: f64,
    /// Emitted once after dwelling on an expandable inside target.
    pub hover_expand: Option<K>,
}

impl<K> Default for ReorderMotion<K> {
    fn default() -> Self {
        Self {
            scroll_delta: 0.0,
            hover_expand: None,
        }
    }
}

struct DragState<K> {
    source: K,
    origin: Point,
    cursor: Point,
    active: bool,
    candidate: Option<DropCandidate<K>>,
    hover_since: Option<Instant>,
    hover_emitted: bool,
}

/// Gesture controller shared by list and tree widgets. Widgets retain model
/// mutation authority and supply their own hit-tested, validated candidates.
pub struct ReorderController<K> {
    drag: Option<DragState<K>>,
    drag_threshold: f64,
    edge_zone: f64,
    max_scroll_speed: f64,
    hover_expand_delay: Duration,
}

impl<K> Default for ReorderController<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> ReorderController<K> {
    pub fn new() -> Self {
        Self {
            drag: None,
            drag_threshold: 4.0,
            edge_zone: 40.0,
            max_scroll_speed: 900.0,
            hover_expand_delay: Duration::from_millis(650),
        }
    }

    #[must_use]
    pub fn with_drag_threshold(mut self, logical_pixels: f64) -> Self {
        self.drag_threshold = logical_pixels.max(0.0);
        self
    }

    #[must_use]
    pub fn with_edge_scroll(mut self, zone: f64, max_speed: f64) -> Self {
        self.edge_zone = zone.max(0.0);
        self.max_scroll_speed = max_speed.max(0.0);
        self
    }

    #[must_use]
    pub fn with_hover_expand_delay(mut self, delay: Duration) -> Self {
        self.hover_expand_delay = delay;
        self
    }

    pub fn begin(&mut self, source: K, cursor: Point) {
        self.drag = Some(DragState {
            source,
            origin: cursor,
            cursor,
            active: false,
            candidate: None,
            hover_since: None,
            hover_emitted: false,
        });
    }

    /// Store a candidate only when `validate` permits the source/target pair.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "finite screen-space pointer subtraction cannot overflow f64"
    )]
    pub fn update(
        &mut self,
        cursor: Point,
        candidate: Option<DropCandidate<K>>,
        now: Instant,
        mut validate: impl FnMut(&K, &DropTarget<K>) -> bool,
    ) where
        K: PartialEq,
    {
        let Some(drag) = &mut self.drag else {
            return;
        };
        drag.cursor = cursor;
        if !drag.active {
            drag.active = (cursor - drag.origin).hypot() >= self.drag_threshold;
        }
        if !drag.active {
            return;
        }
        let candidate = candidate.filter(|candidate| validate(&drag.source, &candidate.target));
        let unchanged = drag.candidate.as_ref().map(|candidate| &candidate.target)
            == candidate.as_ref().map(|candidate| &candidate.target);
        drag.candidate = candidate;
        if unchanged {
            return;
        }
        drag.hover_since = drag
            .candidate
            .as_ref()
            .filter(|candidate| candidate.can_hover_expand)
            .map(|_| now);
        drag.hover_emitted = false;
    }

    #[must_use]
    pub fn tick(&mut self, viewport: Rect, elapsed: Duration, now: Instant) -> ReorderMotion<K>
    where
        K: Clone,
    {
        let Some(drag) = &mut self.drag else {
            return ReorderMotion::default();
        };
        if !drag.active {
            return ReorderMotion::default();
        }
        let velocity = edge_velocity(
            drag.cursor.y,
            viewport.y0,
            viewport.y1,
            self.edge_zone,
            self.max_scroll_speed,
        );
        let hover_expand = if !drag.hover_emitted
            && drag
                .candidate
                .as_ref()
                .is_some_and(|candidate| candidate.can_hover_expand)
            && drag.hover_since.is_some_and(|since| {
                now.saturating_duration_since(since) >= self.hover_expand_delay
            }) {
            drag.hover_emitted = true;
            drag.candidate
                .as_ref()
                .map(|candidate| candidate.target.key.clone())
        } else {
            None
        };
        ReorderMotion {
            scroll_delta: velocity * elapsed.as_secs_f64(),
            hover_expand,
        }
    }

    #[must_use]
    pub fn is_tracking(&self) -> bool {
        self.drag.is_some()
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    #[must_use]
    pub fn source(&self) -> Option<&K> {
        self.drag.as_ref().map(|drag| &drag.source)
    }

    #[must_use]
    pub fn target(&self) -> Option<&DropTarget<K>> {
        self.drag
            .as_ref()
            .and_then(|drag| drag.candidate.as_ref())
            .map(|candidate| &candidate.target)
    }

    #[must_use]
    pub fn preview(&self) -> Option<Rect> {
        self.drag
            .as_ref()
            .filter(|drag| drag.active)
            .and_then(|drag| drag.candidate.as_ref())
            .map(|candidate| candidate.preview)
    }

    pub fn cancel(&mut self) -> bool {
        self.drag.take().is_some()
    }

    pub fn finish(&mut self) -> Option<ReorderDrop<K>> {
        let drag = self.drag.take()?;
        if !drag.active {
            return None;
        }
        Some(ReorderDrop {
            source: drag.source,
            target: drag.candidate?.target,
        })
    }
}

fn edge_velocity(cursor: f64, start: f64, end: f64, zone: f64, max_speed: f64) -> f64 {
    if zone <= 0.0 || end <= start {
        return 0.0;
    }
    if cursor < start + zone {
        let strength = ((start + zone - cursor) / zone).clamp(0.0, 1.0);
        return -max_speed * strength * strength;
    }
    if cursor > end - zone {
        let strength = ((cursor - (end - zone)) / zone).clamp(0.0, 1.0);
        return max_speed * strength * strength;
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(key: u64, placement: DropPlacement, y: f64) -> DropCandidate<u64> {
        DropCandidate {
            target: DropTarget::new(key, placement),
            preview: Rect::new(0.0, y, 100.0, y + 2.0),
            can_hover_expand: placement == DropPlacement::Inside,
        }
    }

    #[test]
    fn reorder_drag_separates_clicks_and_emits_one_validated_drop() {
        // Case `threshold_separates_click_from_reorder`.
        {
            let now = crate::time::now();
            let mut reorder = ReorderController::new();
            reorder.begin(1, Point::new(10.0, 10.0));
            reorder.update(
                Point::new(12.0, 12.0),
                Some(candidate(2, DropPlacement::Before, 20.0)),
                now,
                |_, _| true,
            );
            assert!(!reorder.is_active());
            assert_eq!(reorder.finish(), None);
        }

        // Case `invalid_target_has_no_preview_or_drop`.
        {
            let now = crate::time::now();
            let mut reorder = ReorderController::new().with_drag_threshold(0.0);
            reorder.begin(1, Point::ZERO);
            reorder.update(
                Point::new(0.0, 20.0),
                Some(candidate(2, DropPlacement::After, 30.0)),
                now,
                |_, _| false,
            );
            assert_eq!(reorder.preview(), None);
            assert_eq!(reorder.finish(), None);
        }

        // Case `finish_emits_one_validated_drop`.
        {
            let now = crate::time::now();
            let mut reorder = ReorderController::new().with_drag_threshold(0.0);
            reorder.begin(1, Point::ZERO);
            reorder.update(
                Point::new(0.0, 20.0),
                Some(candidate(2, DropPlacement::After, 30.0)),
                now,
                |source, target| *source != target.key,
            );
            assert_eq!(reorder.preview(), Some(Rect::new(0.0, 30.0, 100.0, 32.0)));
            assert_eq!(
                reorder.finish(),
                Some(ReorderDrop {
                    source: 1,
                    target: DropTarget::new(2, DropPlacement::After)
                })
            );
            assert_eq!(reorder.finish(), None);
        }
    }

    #[test]
    fn reorder_drag_scrolls_toward_edges_and_expands_hovered_containers_once() {
        // Case `edge_scroll_accelerates_toward_edges`.
        {
            let now = crate::time::now();
            let mut reorder = ReorderController::new()
                .with_drag_threshold(0.0)
                .with_edge_scroll(40.0, 800.0);
            reorder.begin(1, Point::new(10.0, 50.0));
            reorder.update(Point::new(10.0, 5.0), None, now, |_, _| true);
            let top = reorder.tick(
                Rect::new(0.0, 0.0, 100.0, 200.0),
                Duration::from_millis(100),
                now,
            );
            assert!(top.scroll_delta < -50.0);
            reorder.update(Point::new(10.0, 195.0), None, now, |_, _| true);
            let bottom = reorder.tick(
                Rect::new(0.0, 0.0, 100.0, 200.0),
                Duration::from_millis(100),
                now,
            );
            assert!(bottom.scroll_delta > 50.0);
        }

        // Case `inside_hover_expands_once_after_delay`.
        {
            let now = crate::time::now();
            let mut reorder = ReorderController::new()
                .with_drag_threshold(0.0)
                .with_hover_expand_delay(Duration::from_millis(500));
            reorder.begin(1, Point::ZERO);
            reorder.update(
                Point::new(10.0, 10.0),
                Some(candidate(2, DropPlacement::Inside, 10.0)),
                now,
                |_, _| true,
            );
            let viewport = Rect::new(0.0, 0.0, 100.0, 100.0);
            assert_eq!(
                reorder
                    .tick(viewport, Duration::ZERO, now + Duration::from_millis(499))
                    .hover_expand,
                None
            );
            assert_eq!(
                reorder
                    .tick(viewport, Duration::ZERO, now + Duration::from_millis(500))
                    .hover_expand,
                Some(2)
            );
            assert_eq!(
                reorder
                    .tick(viewport, Duration::ZERO, now + Duration::from_secs(1))
                    .hover_expand,
                None
            );
        }
    }
}
