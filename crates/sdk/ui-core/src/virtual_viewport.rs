//! Shared one-dimensional virtualization geometry for lists, trees, and tables.
//!
//! [`VirtualViewport`] keeps stable item keys, lazily measured heights, a
//! Fenwick prefix-sum index, and the scroll position in one place. Collection
//! widgets use it for four operations that must agree exactly: visible-range
//! calculation, row placement, hit testing, and scrollbar metrics.
//!
//! Reconciliation is anchor-preserving. Inserting, removing, or resizing rows
//! above the first visible item does not move the content under the user's
//! eyes. Measured heights follow stable keys across moves; unseen rows retain
//! the configured estimate until measured.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded usize index arithmetic and finite pixel sums are the core of this geometry index"
)]

use crate::num::f64_of_usize;
use crate::time::{Duration, Instant};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::ops::Range;

const PAGE_FRACTION: f64 = 0.9;
const DRAG_SCROLL_THRESHOLD: f64 = 4.0;
const MAX_DRAG_FLING_VELOCITY: f64 = 6_000.0;
const WHEEL_LINE_PX: f64 = 60.0;
/// How far back a lift looks when estimating the release velocity.
const KINETIC_WINDOW: Duration = Duration::from_millis(100);
/// The shortest interval one sample is assumed to span. Devices report at
/// 60-240 Hz; a burst delivered faster than that (event coalescing, replay,
/// a test driver) must not read as an unbounded velocity.
const KINETIC_MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(8);
/// Below this release speed a lift is a deliberate stop, not a throw.
const KINETIC_MIN_VELOCITY: f64 = 60.0;

/// Where an item should land when explicitly scrolled into view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollAlignment {
    Start,
    Center,
    End,
    /// Move only when the item is clipped; otherwise preserve the offset.
    #[default]
    Nearest,
}

impl ScrollAlignment {
    /// The scroll offset that lands an item spanning `top..top + height` this
    /// way in a `viewport`-tall window currently scrolled to `offset`.
    fn offset_for(self, top: f64, height: f64, offset: f64, viewport: f64) -> f64 {
        let bottom = top + height;
        match self {
            Self::Start => top,
            Self::Center => top - (viewport - height) / 2.0,
            Self::End => bottom - viewport,
            Self::Nearest if height >= viewport && top <= offset && bottom >= offset + viewport => {
                offset
            }
            Self::Nearest if top < offset => top,
            Self::Nearest if bottom > offset + viewport => bottom - viewport,
            Self::Nearest => offset,
        }
    }
}

/// How quickly a fling slows, per second: velocity decays as
/// `v(t) = v₀·e^(−k·t)`. One rate for every scroll surface, so lists
/// ([`ScrollMomentum`]) and documents (`Scrollable`) glide alike.
pub const FLING_DECAY_PER_SECOND: f64 = 9.0;

/// Below this speed (logical px/s) a glide has come to rest.
pub const GLIDE_REST_SPEED: f64 = 2.0;

/// Advance a fling by `seconds`: the distance it covers and the velocity it
/// keeps. The distance is the exact integral of the decay, so one 40 ms step
/// lands where four 10 ms steps do — a fling travels the same at 25, 60 and
/// 90 fps, and after a stalled frame. No clamp: a stall is covered, not
/// dropped.
#[must_use]
pub fn glide_step(velocity: f64, seconds: f64) -> (f64, f64) {
    let velocity = finite_or_zero(velocity);
    if seconds.is_nan() || seconds <= 0.0 || velocity == 0.0 {
        return (0.0, velocity);
    }
    let k = FLING_DECAY_PER_SECOND;
    let keep = (-k * seconds).exp();
    let displacement = velocity * (1.0 - keep) / k;
    let remaining = velocity * keep;
    let remaining = if remaining.abs() < GLIDE_REST_SPEED {
        0.0
    } else {
        remaining
    };
    (displacement, remaining)
}

/// Deterministic single-axis momentum used by virtualized collection widgets.
/// Wheel input moves immediately; line-wheel impulses additionally feed this
/// decay curve so detented wheels can traverse long collections without
/// requiring one event per row.
#[derive(Clone, Debug)]
pub struct ScrollMomentum {
    velocity: f64,
    /// When the glide was last advanced, or started: the next
    /// [`Self::advance`] covers the time since, never an idle gap before
    /// the glide began.
    last_step: Option<Instant>,
    max_velocity: f64,
    kinetic: KineticSampler,
    pending_pixel: bool,
}

impl Default for ScrollMomentum {
    fn default() -> Self {
        Self {
            velocity: 0.0,
            last_step: None,
            max_velocity: 6_000.0,
            kinetic: KineticSampler::new(),
            pending_pixel: false,
        }
    }
}

impl ScrollMomentum {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a logical-pixel wheel impulse. Return the immediate delta so the
    /// caller can apply it in the same input frame.
    pub fn push(&mut self, delta: f64) -> f64 {
        let delta = finite_or_zero(delta);
        self.start_clock();
        self.velocity = (self.velocity + delta * 18.0).clamp(-self.max_velocity, self.max_velocity);
        delta
    }

    /// A glide that was at rest starts its clock now.
    fn start_clock(&mut self) {
        if !self.is_active() {
            self.last_step = Some(crate::time::now());
        }
    }

    /// Continue from a content-space drag velocity after pointer release.
    /// The velocity is clamped to the same ceiling as accumulated wheel
    /// impulses, keeping a bad timestamp or synthetic pointer from launching
    /// the viewport an unbounded distance.
    pub fn fling(&mut self, velocity: f64) {
        self.start_clock();
        self.velocity = finite_or_zero(velocity).clamp(-self.max_velocity, self.max_velocity);
    }

    /// Advance the glide to `now` (the frame's time) and return the
    /// logical-pixel displacement since the last step or since it started.
    pub fn advance(&mut self, now: Instant) -> f64 {
        if !self.is_active() {
            self.last_step = None;
            return 0.0;
        }
        let elapsed = self
            .last_step
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        self.tick(elapsed)
    }

    /// Advance the curve by `elapsed` and return the logical-pixel
    /// displacement ([`glide_step`]).
    pub fn tick(&mut self, elapsed: Duration) -> f64 {
        if !self.is_active() {
            return 0.0;
        }
        let (displacement, velocity) = glide_step(self.velocity, elapsed.as_secs_f64());
        self.velocity = velocity;
        displacement
    }

    pub fn stop(&mut self) {
        self.velocity = 0.0;
        self.last_step = None;
    }

    /// Classify one vertical wheel event. Line (detented) wheels accelerate
    /// through [`Self::push`]; pixel (touchpad, touchscreen) deltas track
    /// the finger exactly and, when the gesture's `Ended` phase arrives
    /// while still moving, continue as a fling. `reduce_motion` keeps every
    /// movement 1:1 with the input.
    pub fn wheel(
        &mut self,
        delta: &winit::event::MouseScrollDelta,
        phase: winit::event::TouchPhase,
        reduce_motion: bool,
    ) -> WheelStep {
        use winit::event::{MouseScrollDelta, TouchPhase};
        self.pending_pixel = false;
        match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.kinetic.reset();
                let delta = f64::from(*y) * -WHEEL_LINE_PX;
                if reduce_motion {
                    self.stop();
                    WheelStep::Scroll(delta)
                } else {
                    WheelStep::Scroll(self.push(delta))
                }
            }
            MouseScrollDelta::PixelDelta(position) => match phase {
                TouchPhase::Ended => match self.kinetic.release(crate::time::now()) {
                    Some((_, velocity)) if !reduce_motion => {
                        self.fling(velocity);
                        WheelStep::Fling
                    }
                    _ => WheelStep::Ignored,
                },
                TouchPhase::Cancelled => {
                    self.kinetic.reset();
                    WheelStep::Ignored
                }
                TouchPhase::Started | TouchPhase::Moved => {
                    if phase == TouchPhase::Started {
                        self.kinetic.reset();
                    }
                    // Fingers back on the surface catch a glide in progress.
                    self.stop();
                    let delta = -finite_or_zero(position.y);
                    if delta == 0.0 {
                        return WheelStep::Ignored;
                    }
                    self.pending_pixel = true;
                    WheelStep::Scroll(delta)
                }
            },
        }
    }

    /// Report whether the [`WheelStep::Scroll`] delta moved the content. A
    /// refused delta (at an edge) stops momentum and forgets the gesture so
    /// an outer scroll region can take it over, fling included.
    pub fn wheel_applied(&mut self, delta: f64, moved: bool) {
        if !moved {
            self.stop();
            self.kinetic.reset();
        } else if self.pending_pixel {
            self.kinetic.record(0.0, delta, crate::time::now());
        }
        self.pending_pixel = false;
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.velocity.abs() >= GLIDE_REST_SPEED
    }
}

/// Release-velocity estimator for continuous (pixel) scrolling.
///
/// Touchpads and touchscreens report a gesture as `PixelDelta` wheel events
/// bracketed by [`winit::event::TouchPhase`]s. Feed every delta the view
/// actually consumed to [`Self::record`]; on `Ended`, [`Self::release`]
/// returns the average content-space velocity over the last 100 ms, so a
/// finger that paused before lifting does not fling.
#[derive(Clone, Debug, Default)]
pub struct KineticSampler {
    samples: VecDeque<(Instant, f64, f64)>,
}

impl KineticSampler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one consumed content-space delta.
    pub fn record(&mut self, dx: f64, dy: f64, now: Instant) {
        self.prune(now);
        self.samples
            .push_back((now, finite_or_zero(dx), finite_or_zero(dy)));
    }

    /// Forget the gesture, e.g. when the view stopped consuming it.
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// End the gesture and return its content-space velocity in logical
    /// pixels per second, or `None` when the fingers had come to rest.
    pub fn release(&mut self, now: Instant) -> Option<(f64, f64)> {
        self.prune(now);
        let samples = std::mem::take(&mut self.samples);
        let first = samples.front()?.0;
        let count = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        let span = now
            .saturating_duration_since(first)
            .max(KINETIC_MIN_SAMPLE_INTERVAL.saturating_mul(count))
            .as_secs_f64();
        let (dx, dy) = samples
            .iter()
            .fold((0.0, 0.0), |(x, y), &(_, dx, dy)| (x + dx, y + dy));
        let (vx, vy) = (dx / span, dy / span);
        (vx.hypot(vy) >= KINETIC_MIN_VELOCITY).then_some((
            vx.clamp(-MAX_DRAG_FLING_VELOCITY, MAX_DRAG_FLING_VELOCITY),
            vy.clamp(-MAX_DRAG_FLING_VELOCITY, MAX_DRAG_FLING_VELOCITY),
        ))
    }

    fn prune(&mut self, now: Instant) {
        while self
            .samples
            .front()
            .is_some_and(|&(at, _, _)| now.saturating_duration_since(at) > KINETIC_WINDOW)
        {
            self.samples.pop_front();
        }
    }
}

/// What a vertical collection should do with one wheel event, decided by
/// [`ScrollMomentum::wheel`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WheelStep {
    /// Move the content by this many logical pixels now, then report whether
    /// it moved through [`ScrollMomentum::wheel_applied`].
    Scroll(f64),
    /// A continuous gesture ended fast enough to keep gliding; the momentum
    /// is now active and the event is consumed.
    Fling,
    /// Nothing to do (a phase marker, or a gesture that ended at rest).
    Ignored,
}

/// Result of moving a pointer tracked by [`DragScroller`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragScrollUpdate {
    /// Absolute content offset implied by the pointer displacement.
    pub target_offset: f64,
    /// `true` only on the update that crosses the drag threshold.
    pub began: bool,
}

/// Result of releasing a pointer tracked by [`DragScroller`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragScrollFinish {
    /// Whether the gesture crossed the drag threshold.
    pub dragged: bool,
    /// Content-space velocity suitable for [`ScrollMomentum::fling`].
    pub velocity: f64,
}

/// Thresholded, single-axis pointer panning shared by virtual collections.
/// A press remains a click until it moves four logical pixels; once active,
/// offset and release velocity are derived from the same pointer samples.
#[derive(Clone, Debug, Default)]
pub struct DragScroller {
    gesture: Option<DragGesture>,
}

#[derive(Clone, Debug)]
struct DragGesture {
    start_axis: f64,
    start_offset: f64,
    last_axis: f64,
    last_at: crate::time::Instant,
    velocity: f64,
    active: bool,
}

impl DragScroller {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self, axis: f64, offset: f64, now: crate::time::Instant) {
        self.gesture = Some(DragGesture {
            start_axis: finite_or_zero(axis),
            start_offset: finite_or_zero(offset),
            last_axis: finite_or_zero(axis),
            last_at: now,
            velocity: 0.0,
            active: false,
        });
    }

    pub fn update(&mut self, axis: f64, now: crate::time::Instant) -> Option<DragScrollUpdate> {
        let gesture = self.gesture.as_mut()?;
        let axis = finite_or_zero(axis);
        let distance = axis - gesture.start_axis;
        let began = !gesture.active && distance.abs() >= DRAG_SCROLL_THRESHOLD;
        if !gesture.active && !began {
            gesture.last_axis = axis;
            gesture.last_at = now;
            return None;
        }
        gesture.active = true;

        let seconds = now.saturating_duration_since(gesture.last_at).as_secs_f64();
        if seconds > 0.0 {
            let sample = (-(axis - gesture.last_axis) / seconds)
                .clamp(-MAX_DRAG_FLING_VELOCITY, MAX_DRAG_FLING_VELOCITY);
            // Low-pass filtering avoids a noisy final pointer sample deciding
            // the whole release velocity while remaining responsive.
            gesture.velocity = gesture.velocity * 0.65 + sample * 0.35;
        }
        gesture.last_axis = axis;
        gesture.last_at = now;
        Some(DragScrollUpdate {
            target_offset: gesture.start_offset - distance,
            began,
        })
    }

    pub fn finish(&mut self, now: crate::time::Instant) -> Option<DragScrollFinish> {
        let gesture = self.gesture.take()?;
        let idle = now.saturating_duration_since(gesture.last_at).as_secs_f64();
        let release_velocity = if idle >= 0.12 {
            0.0
        } else {
            gesture.velocity * (-12.0 * idle).exp()
        };
        Some(DragScrollFinish {
            dragged: gesture.active,
            velocity: if gesture.active {
                release_velocity
            } else {
                0.0
            },
        })
    }

    pub fn cancel(&mut self) -> bool {
        self.gesture.take().is_some()
    }

    #[must_use]
    pub fn is_tracking(&self) -> bool {
        self.gesture.is_some()
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.gesture.as_ref().is_some_and(|gesture| gesture.active)
    }
}

/// A key appeared more than once in a reconciliation input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileError {
    DuplicateKey,
}

#[derive(Clone, Debug)]
struct Item<K> {
    key: K,
    height: f64,
    measured: bool,
}

#[derive(Clone, Debug)]
struct Anchor<K> {
    primary: K,
    within: f64,
    fallback: Option<AnchorFallback<K>>,
}

#[derive(Clone, Debug)]
enum AnchorFallback<K> {
    Successor(K),
    Predecessor(K),
}

/// Variable-height virtual collection geometry, keyed by model identity.
#[derive(Clone, Debug)]
pub struct VirtualViewport<K> {
    items: Vec<Item<K>>,
    index_by_key: HashMap<K, usize>,
    /// Fenwick tree; slot zero is unused.
    sums: Vec<f64>,
    estimated_height: f64,
    viewport: f64,
    offset: f64,
    overscan: f64,
}

impl<K> VirtualViewport<K>
where
    K: Clone + Eq + Hash,
{
    /// Create an empty viewport. Estimates and measurements are clamped to at
    /// least one logical pixel so prefix sums remain strictly increasing.
    #[must_use]
    pub fn new(estimated_height: f64) -> Self {
        Self {
            items: Vec::new(),
            index_by_key: HashMap::new(),
            sums: vec![0.0],
            estimated_height: valid_height(estimated_height),
            viewport: 0.0,
            offset: 0.0,
            overscan: 0.0,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn estimated_height(&self) -> f64 {
        self.estimated_height
    }

    /// Change the fallback height for rows that have not been measured. The
    /// first visible stable key stays at the same screen position.
    pub fn set_estimated_height(&mut self, height: f64) {
        let next = valid_height(height);
        if next.to_bits() == self.estimated_height.to_bits() {
            return;
        }
        let anchor = self.capture_anchor();
        self.estimated_height = next;
        for item in &mut self.items {
            if !item.measured {
                item.height = next;
            }
        }
        self.rebuild_sums();
        self.restore_anchor(anchor.as_ref());
    }

    pub fn set_viewport(&mut self, height: f64) {
        self.viewport = finite_non_negative(height);
        self.clamp_offset();
    }

    #[must_use]
    pub fn viewport(&self) -> f64 {
        self.viewport
    }

    pub fn set_overscan(&mut self, pixels: f64) {
        self.overscan = finite_non_negative(pixels);
    }

    #[must_use]
    pub fn overscan(&self) -> f64 {
        self.overscan
    }

    #[must_use]
    pub fn content_extent(&self) -> f64 {
        self.prefix_sum(self.items.len())
    }

    #[must_use]
    pub fn max_offset(&self) -> f64 {
        (self.content_extent() - self.viewport).max(0.0)
    }

    #[must_use]
    pub fn offset(&self) -> f64 {
        self.offset
    }

    pub fn set_offset(&mut self, offset: f64) {
        self.offset = finite_non_negative(offset).min(self.max_offset());
    }

    pub fn scroll_by(&mut self, delta: f64) {
        self.set_offset(self.offset + finite_or_zero(delta));
    }

    /// Replace model order while retaining measurements and the visible
    /// anchor by stable key.
    ///
    /// # Errors
    ///
    /// Returns [`ReconcileError::DuplicateKey`] without changing the viewport
    /// when `keys` contains duplicates.
    pub fn reconcile<I>(&mut self, keys: I) -> Result<(), ReconcileError>
    where
        I: IntoIterator<Item = K>,
    {
        let keys: Vec<K> = keys.into_iter().collect();
        let mut unique = HashSet::with_capacity(keys.len());
        if keys.iter().any(|key| !unique.insert(key.clone())) {
            return Err(ReconcileError::DuplicateKey);
        }

        let mut anchor = self.capture_anchor();
        // Choose a surviving neighbour before replacing the old order. This
        // is computed only for a deleted anchor and avoids cloning the entire
        // tail of a million-row collection on every reconciliation.
        if let Some(anchor) = &mut anchor
            && !unique.contains(&anchor.primary)
            && let Some(index) = self.index_of(&anchor.primary)
        {
            anchor.fallback = self
                .items
                .iter()
                .skip(index.saturating_add(1))
                .find(|item| unique.contains(&item.key))
                .map(|item| AnchorFallback::Successor(item.key.clone()))
                .or_else(|| {
                    self.items
                        .iter()
                        .take(index)
                        .rev()
                        .find(|item| unique.contains(&item.key))
                        .map(|item| AnchorFallback::Predecessor(item.key.clone()))
                });
        }
        let old: HashMap<K, (f64, bool)> = self
            .items
            .drain(..)
            .map(|item| (item.key, (item.height, item.measured)))
            .collect();
        self.items = keys
            .into_iter()
            .map(|key| {
                let (height, measured) = old
                    .get(&key)
                    .copied()
                    .unwrap_or((self.estimated_height, false));
                Item {
                    key,
                    height,
                    measured,
                }
            })
            .collect();
        self.rebuild_index();
        self.rebuild_sums();
        self.restore_anchor(anchor.as_ref());
        Ok(())
    }

    #[must_use]
    pub fn key(&self, index: usize) -> Option<&K> {
        self.items.get(index).map(|item| &item.key)
    }

    #[must_use]
    pub fn index_of(&self, key: &K) -> Option<usize> {
        self.index_by_key.get(key).copied()
    }

    #[must_use]
    pub fn item_height(&self, index: usize) -> Option<f64> {
        self.items.get(index).map(|item| item.height)
    }

    #[must_use]
    pub fn item_top(&self, index: usize) -> Option<f64> {
        (index < self.items.len()).then(|| self.prefix_sum(index))
    }

    /// Top of an item in viewport coordinates (`0` is the viewport's top).
    #[must_use]
    pub fn item_view_top(&self, index: usize) -> Option<f64> {
        self.item_top(index).map(|top| top - self.offset)
    }

    /// Cache a measured height. Returns whether geometry changed.
    pub fn measure_index(&mut self, index: usize, height: f64) -> bool {
        let next = valid_height(height);
        let Some(item) = self.items.get(index) else {
            return false;
        };
        if item.measured && item.height.to_bits() == next.to_bits() {
            return false;
        }
        let anchor = self.capture_anchor();
        let previous = item.height;
        if let Some(item) = self.items.get_mut(index) {
            item.height = next;
            item.measured = true;
        }
        self.add_sum(index, next - previous);
        self.restore_anchor(anchor.as_ref());
        true
    }

    /// Cache a measured height by stable key. Returns whether geometry changed.
    pub fn measure(&mut self, key: &K, height: f64) -> bool {
        self.index_of(key)
            .is_some_and(|index| self.measure_index(index, height))
    }

    /// Half-open item range intersecting the viewport plus pixel overscan.
    #[must_use]
    pub fn visible_range(&self) -> Range<usize> {
        if self.items.is_empty() {
            return 0..0;
        }
        if self.viewport <= 0.0 {
            return 0..self.items.len();
        }
        let start_y = (self.offset - self.overscan).max(0.0);
        let end_y = (self.offset + self.viewport + self.overscan).min(self.content_extent());
        let start = self.index_at_content_offset(start_y).unwrap_or(0);
        let end = if end_y >= self.content_extent() {
            self.items.len()
        } else {
            self.index_at_content_offset(end_y)
                .map_or(self.items.len(), |index| index.saturating_add(1))
        };
        start..end.max(start)
    }

    /// Resolve a y coordinate relative to the viewport top to an item index.
    #[must_use]
    pub fn hit_test(&self, viewport_y: f64) -> Option<usize> {
        if viewport_y < 0.0 || (self.viewport > 0.0 && viewport_y >= self.viewport) {
            return None;
        }
        self.index_at_content_offset(self.offset + viewport_y)
    }

    pub fn scroll_to_index(&mut self, index: usize, alignment: ScrollAlignment) -> bool {
        let (Some(top), Some(height)) = (self.item_top(index), self.item_height(index)) else {
            return false;
        };
        let previous = self.offset;
        self.set_offset(alignment.offset_for(top, height, self.offset, self.viewport));
        self.offset.to_bits() != previous.to_bits()
    }

    fn index_at_content_offset(&self, offset: f64) -> Option<usize> {
        if self.items.is_empty() || offset < 0.0 || offset >= self.content_extent() {
            return None;
        }
        let mut lo = 0_usize;
        let mut hi = self.items.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.prefix_sum(mid.saturating_add(1)) <= offset {
                lo = mid.saturating_add(1);
            } else {
                hi = mid;
            }
        }
        (lo < self.items.len()).then_some(lo)
    }

    fn capture_anchor(&self) -> Option<Anchor<K>> {
        let index = self
            .index_at_content_offset(self.offset)
            .or_else(|| self.items.len().checked_sub(1))?;
        let primary = self.items.get(index)?.key.clone();
        let within = (self.offset - self.prefix_sum(index)).max(0.0);
        Some(Anchor {
            primary,
            within,
            fallback: None,
        })
    }

    fn restore_anchor(&mut self, anchor: Option<&Anchor<K>>) {
        let Some(anchor) = anchor else {
            self.clamp_offset();
            return;
        };
        if let Some(index) = self.index_of(&anchor.primary) {
            self.offset = self.prefix_sum(index) + anchor.within;
            self.clamp_offset();
            return;
        }
        match &anchor.fallback {
            Some(AnchorFallback::Successor(key)) => {
                if let Some(index) = self.index_of(key) {
                    self.offset = self.prefix_sum(index);
                }
            }
            Some(AnchorFallback::Predecessor(key)) => {
                if let Some(index) = self.index_of(key) {
                    self.offset = self.prefix_sum(index.saturating_add(1)) - self.viewport;
                }
            }
            None => {}
        }
        self.clamp_offset();
    }

    fn clamp_offset(&mut self) {
        self.offset = finite_non_negative(self.offset).min(self.max_offset());
    }

    fn rebuild_index(&mut self) {
        self.index_by_key.clear();
        self.index_by_key.reserve(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            self.index_by_key.insert(item.key.clone(), index);
        }
    }

    fn rebuild_sums(&mut self) {
        self.sums.clear();
        self.sums.resize(self.items.len().saturating_add(1), 0.0);
        for index in 0..self.items.len() {
            if let Some(height) = self.items.get(index).map(|item| item.height) {
                self.add_sum(index, height);
            }
        }
    }

    fn add_sum(&mut self, index: usize, delta: f64) {
        let mut slot = index.saturating_add(1);
        while slot < self.sums.len() {
            if let Some(value) = self.sums.get_mut(slot) {
                *value += delta;
            }
            let low_bit = slot & slot.wrapping_neg();
            slot += low_bit;
        }
    }

    fn prefix_sum(&self, end: usize) -> f64 {
        let mut slot = end.min(self.items.len());
        let mut total = 0.0;
        while slot > 0 {
            total += self.sums.get(slot).copied().unwrap_or(0.0);
            slot &= slot - 1;
        }
        total
    }
}

/// Stable scroll anchor captured from a model-resolved sparse viewport.
#[derive(Clone, Debug)]
pub struct SparseAnchor<K> {
    key: K,
    index: usize,
    within: f64,
}

impl<K> SparseAnchor<K> {
    #[must_use]
    pub fn key(&self) -> &K {
        &self.key
    }

    #[must_use]
    pub fn with_key<N>(self, key: N) -> SparseAnchor<N> {
        SparseAnchor {
            key,
            index: self.index,
            within: self.within,
        }
    }
}

/// Virtual geometry for collections whose logical item count is much larger
/// than the set of resident keys.
///
/// Unlike [`VirtualViewport`], this type does not store one entry per logical
/// row. Unmeasured rows share an estimate; only measured stable keys and their
/// prefix-sum deltas consume memory. The owning model resolves keys at visible
/// indices and indices for measured/anchored keys during reconciliation.
#[derive(Clone, Debug)]
pub struct SparseVirtualViewport<K> {
    count: usize,
    estimated_height: f64,
    minimum_height: f64,
    viewport: f64,
    offset: f64,
    overscan: f64,
    measurements: HashMap<K, f64>,
    positions: HashMap<K, usize>,
    measured_indices: Vec<usize>,
    prefix_deltas: Vec<f64>,
    height_by_index: HashMap<usize, f64>,
}

impl<K> SparseVirtualViewport<K>
where
    K: Clone + Eq + Hash,
{
    #[must_use]
    pub fn new(estimated_height: f64) -> Self {
        Self {
            count: 0,
            estimated_height: valid_height(estimated_height),
            minimum_height: 1.0,
            viewport: 0.0,
            offset: 0.0,
            overscan: 0.0,
            measurements: HashMap::new(),
            positions: HashMap::new(),
            measured_indices: Vec::new(),
            prefix_deltas: vec![0.0],
            height_by_index: HashMap::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[must_use]
    pub fn resident_measurements(&self) -> usize {
        self.measurements.len()
    }

    #[must_use]
    pub fn estimated_height(&self) -> f64 {
        self.effective_estimated_height()
    }

    pub fn set_estimated_height(&mut self, height: f64) {
        let next = valid_height(height);
        if next.to_bits() == self.estimated_height.to_bits() {
            return;
        }
        let anchor = self.capture_numeric_anchor();
        self.estimated_height = next;
        self.rebuild_prefix();
        self.restore_numeric_anchor(anchor);
    }

    /// Set a geometry floor without discarding authored estimates or stable
    /// measured heights. Lowering the floor restores those original values.
    pub fn set_minimum_height(&mut self, height: f64) {
        let next = valid_height(height);
        if next.to_bits() == self.minimum_height.to_bits() {
            return;
        }
        let anchor = self.capture_numeric_anchor();
        self.minimum_height = next;
        self.rebuild_prefix();
        self.restore_numeric_anchor(anchor);
    }

    #[must_use]
    pub fn minimum_height(&self) -> f64 {
        self.minimum_height
    }

    fn effective_estimated_height(&self) -> f64 {
        self.estimated_height.max(self.minimum_height)
    }

    pub fn set_viewport(&mut self, height: f64) {
        self.viewport = finite_non_negative(height);
        self.clamp_offset();
    }

    #[must_use]
    pub fn viewport(&self) -> f64 {
        self.viewport
    }

    pub fn set_overscan(&mut self, pixels: f64) {
        self.overscan = finite_non_negative(pixels);
    }

    #[must_use]
    pub fn overscan(&self) -> f64 {
        self.overscan
    }

    #[must_use]
    pub fn offset(&self) -> f64 {
        self.offset
    }

    pub fn set_offset(&mut self, offset: f64) {
        self.offset = finite_non_negative(offset).min(self.max_offset());
    }

    pub fn scroll_by(&mut self, delta: f64) {
        self.set_offset(self.offset + finite_or_zero(delta));
    }

    #[must_use]
    pub fn content_extent(&self) -> f64 {
        f64_of_usize(self.count) * self.effective_estimated_height()
            + self.prefix_deltas.last().copied().unwrap_or(0.0)
    }

    #[must_use]
    pub fn max_offset(&self) -> f64 {
        (self.content_extent() - self.viewport).max(0.0)
    }

    /// Capture the first visible stable key before a model mutation.
    pub fn capture_anchor<F>(&self, mut key_at: F) -> Option<SparseAnchor<K>>
    where
        F: FnMut(usize) -> Option<K>,
    {
        let index = self
            .index_at_content_offset(self.offset)
            .or_else(|| self.count.checked_sub(1))?;
        let key = key_at(index)?;
        Some(SparseAnchor {
            key,
            index,
            within: (self.offset - self.item_top(index)?).max(0.0),
        })
    }

    /// Reconcile a new logical count. `index_of` is called only for resident
    /// measured keys and the captured anchor, never for every logical row.
    pub fn reconcile<F>(&mut self, count: usize, mut index_of: F, anchor: Option<&SparseAnchor<K>>)
    where
        F: FnMut(&K) -> Option<usize>,
    {
        self.count = count;
        self.positions.clear();
        self.measurements.retain(|key, _| {
            let Some(index) = index_of(key).filter(|index| *index < count) else {
                return false;
            };
            self.positions.insert(key.clone(), index);
            true
        });
        self.rebuild_prefix();

        if let Some(anchor) = anchor {
            let stable_index = index_of(&anchor.key).filter(|index| *index < count);
            let index =
                stable_index.or_else(|| count.checked_sub(1).map(|last| anchor.index.min(last)));
            if let Some(index) = index {
                self.offset = self.item_top(index).unwrap_or(0.0)
                    + if stable_index == Some(index) {
                        anchor.within
                    } else {
                        0.0
                    };
            }
        }
        self.clamp_offset();
    }

    /// Store a measured height for the stable key currently at `index`.
    pub fn measure(&mut self, index: usize, key: K, height: f64) -> bool {
        if index >= self.count {
            return false;
        }
        let next = valid_height(height);
        if self
            .measurements
            .get(&key)
            .is_some_and(|old| old.to_bits() == next.to_bits())
            && self.positions.get(&key) == Some(&index)
        {
            return false;
        }
        let anchor = self.capture_numeric_anchor();
        self.measurements.insert(key.clone(), next);
        self.positions.insert(key, index);
        self.rebuild_prefix();
        self.restore_numeric_anchor(anchor);
        true
    }

    /// Move a resident measurement to a replacement stable key without
    /// changing its logical position. Useful when a placeholder resolves to
    /// a real model id.
    pub fn remap_key(&mut self, old: &K, new: K) -> bool {
        if old == &new {
            return false;
        }
        let measurement = self.measurements.remove(old);
        let position = self.positions.remove(old);
        let changed = measurement.is_some() || position.is_some();
        if let Some(height) = measurement {
            self.measurements.insert(new.clone(), height);
        }
        if let Some(index) = position {
            self.positions.insert(new, index);
        }
        if changed {
            self.rebuild_prefix();
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn item_height(&self, index: usize) -> Option<f64> {
        (index < self.count).then(|| {
            self.height_by_index
                .get(&index)
                .copied()
                .unwrap_or_else(|| self.effective_estimated_height())
        })
    }

    #[must_use]
    pub fn item_top(&self, index: usize) -> Option<f64> {
        (index < self.count).then(|| {
            f64_of_usize(index) * self.effective_estimated_height() + self.delta_before(index)
        })
    }

    #[must_use]
    pub fn item_view_top(&self, index: usize) -> Option<f64> {
        self.item_top(index).map(|top| top - self.offset)
    }

    #[must_use]
    pub fn visible_range(&self) -> Range<usize> {
        if self.count == 0 {
            return 0..0;
        }
        if self.viewport <= 0.0 {
            return 0..self.count;
        }
        let start_y = (self.offset - self.overscan).max(0.0);
        let end_y = (self.offset + self.viewport + self.overscan).min(self.content_extent());
        let start = self.index_at_content_offset(start_y).unwrap_or(0);
        let end = if end_y >= self.content_extent() {
            self.count
        } else {
            self.index_at_content_offset(end_y)
                .map_or(self.count, |index| index.saturating_add(1))
        };
        start..end.max(start)
    }

    #[must_use]
    pub fn hit_test(&self, viewport_y: f64) -> Option<usize> {
        if viewport_y < 0.0 || (self.viewport > 0.0 && viewport_y >= self.viewport) {
            return None;
        }
        self.index_at_content_offset(self.offset + viewport_y)
    }

    pub fn scroll_to_index(&mut self, index: usize, alignment: ScrollAlignment) -> bool {
        let (Some(top), Some(height)) = (self.item_top(index), self.item_height(index)) else {
            return false;
        };
        let previous = self.offset;
        self.set_offset(alignment.offset_for(top, height, self.offset, self.viewport));
        self.offset.to_bits() != previous.to_bits()
    }

    /// Scroll one page while keeping the focused row's screen position as
    /// the navigation anchor. Runtime and memory stay independent of `count`,
    /// including hundred-million-row models with variable measured heights.
    pub(crate) fn page_from(&mut self, index: usize, forward: bool) -> Option<(usize, bool)> {
        if self.count == 0 || self.viewport <= 0.0 || index >= self.count {
            return None;
        }
        let anchor_y = self
            .item_view_top(index)?
            .clamp(0.0, (self.viewport - 1.0).max(0.0));
        let previous = self.offset;
        let distance = self.viewport * PAGE_FRACTION * if forward { 1.0 } else { -1.0 };
        self.scroll_by(distance);
        let target = self.hit_test(anchor_y).unwrap_or_else(|| {
            if forward {
                self.count.saturating_sub(1)
            } else {
                0
            }
        });
        Some((target, self.offset.to_bits() != previous.to_bits()))
    }

    fn rebuild_prefix(&mut self) {
        let estimated = self.effective_estimated_height();
        let mut rows: Vec<(usize, f64)> = self
            .measurements
            .iter()
            .filter_map(|(key, height)| {
                self.positions
                    .get(key)
                    .copied()
                    .filter(|index| *index < self.count)
                    .map(|index| (index, *height))
            })
            .collect();
        rows.sort_unstable_by_key(|(index, _)| *index);

        self.measured_indices.clear();
        self.prefix_deltas.clear();
        self.prefix_deltas.push(0.0);
        self.height_by_index.clear();
        for (index, authored_height) in rows {
            let height = authored_height.max(self.minimum_height);
            self.measured_indices.push(index);
            self.height_by_index.insert(index, height);
            let previous = self.prefix_deltas.last().copied().unwrap_or(0.0);
            self.prefix_deltas.push(previous + height - estimated);
        }
    }

    fn delta_before(&self, index: usize) -> f64 {
        let position = self
            .measured_indices
            .partition_point(|measured| *measured < index);
        self.prefix_deltas.get(position).copied().unwrap_or(0.0)
    }

    fn index_at_content_offset(&self, offset: f64) -> Option<usize> {
        if self.count == 0 || offset < 0.0 || offset >= self.content_extent() {
            return None;
        }
        let mut lo = 0_usize;
        let mut hi = self.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let bottom = self.item_top(mid)? + self.item_height(mid)?;
            if bottom <= offset {
                lo = mid.saturating_add(1);
            } else {
                hi = mid;
            }
        }
        (lo < self.count).then_some(lo)
    }

    fn capture_numeric_anchor(&self) -> Option<(usize, f64)> {
        let index = self
            .index_at_content_offset(self.offset)
            .or_else(|| self.count.checked_sub(1))?;
        Some((index, (self.offset - self.item_top(index)?).max(0.0)))
    }

    fn restore_numeric_anchor(&mut self, anchor: Option<(usize, f64)>) {
        if let Some((index, within)) = anchor
            && let Some(top) = self.item_top(index)
        {
            self.offset = top + within;
        }
        self.clamp_offset();
    }

    fn clamp_offset(&mut self) {
        self.offset = finite_non_negative(self.offset).min(self.max_offset());
    }
}

fn valid_height(value: f64) -> f64 {
    if value.is_finite() {
        value.max(1.0)
    } else {
        1.0
    }
}

fn finite_non_negative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn wheel_and_drag_scrolling_preserve_clicks_and_settle() {
        // Case `kinetic_release_averages_the_recent_window`.
        {
            let start = crate::time::now();
            let mut sampler = KineticSampler::new();
            for step in 0..6_u64 {
                sampler.record(0.0, 10.0, start + Duration::from_millis(step * 16));
            }
            let (vx, vy) = sampler
                .release(start + Duration::from_millis(96))
                .expect("a moving lift flings");
            assert_eq!(vx, 0.0);
            // 60 px over the 96 ms the samples span.
            assert!((vy - 625.0).abs() < 1.0, "{vy}");
            assert_eq!(sampler.release(start + Duration::from_millis(96)), None);
        }

        // Case `kinetic_release_after_a_pause_is_at_rest`.
        {
            let start = crate::time::now();
            let mut sampler = KineticSampler::new();
            sampler.record(0.0, 40.0, start);
            assert_eq!(sampler.release(start + Duration::from_millis(150)), None);
        }

        // Case `kinetic_release_bounds_a_burst_of_simultaneous_samples`.
        {
            let start = crate::time::now();
            let mut sampler = KineticSampler::new();
            for _ in 0..4 {
                sampler.record(0.0, 20.0, start);
            }
            let (_, vy) = sampler.release(start).expect("fling");
            // Four samples are assumed to span at least 32 ms.
            assert!((vy - 2_500.0).abs() < 1.0, "{vy}");
        }

        // Case `pixel_wheel_gesture_flings_on_end_unless_motion_is_reduced`.
        {
            use winit::dpi::PhysicalPosition;
            use winit::event::{MouseScrollDelta, TouchPhase};
            let pixel = |y| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, y));
            for reduce_motion in [false, true] {
                let mut momentum = ScrollMomentum::new();
                for phase in [TouchPhase::Started, TouchPhase::Moved, TouchPhase::Moved] {
                    let step = momentum.wheel(&pixel(-20.0), phase, reduce_motion);
                    assert_eq!(step, WheelStep::Scroll(20.0));
                    momentum.wheel_applied(20.0, true);
                    assert!(!momentum.is_active(), "pixel deltas track the fingers 1:1");
                }
                let end = momentum.wheel(&pixel(0.0), TouchPhase::Ended, reduce_motion);
                if reduce_motion {
                    assert_eq!(end, WheelStep::Ignored);
                    assert!(!momentum.is_active());
                } else {
                    assert_eq!(end, WheelStep::Fling);
                    assert!(momentum.tick(Duration::from_millis(16)) > 0.0);
                }
            }
        }

        // Case `pixel_wheel_refused_at_an_edge_does_not_fling`.
        {
            use winit::dpi::PhysicalPosition;
            use winit::event::{MouseScrollDelta, TouchPhase};
            let pixel = |y| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, y));
            let mut momentum = ScrollMomentum::new();
            let _ = momentum.wheel(&pixel(-20.0), TouchPhase::Started, false);
            momentum.wheel_applied(20.0, true);
            let _ = momentum.wheel(&pixel(-20.0), TouchPhase::Moved, false);
            momentum.wheel_applied(20.0, false);
            let end = momentum.wheel(&pixel(0.0), TouchPhase::Ended, false);
            assert_eq!(end, WheelStep::Ignored);
        }

        // Case `a_fling_travels_the_same_at_any_frame_rate`.
        {
            // Half a second at each rate.
            let travel = |hz: u32| {
                let mut momentum = ScrollMomentum::new();
                momentum.fling(3_000.0);
                let frames = hz / 2;
                let step = Duration::from_secs_f64(1.0 / f64::from(hz));
                (0..frames).map(|_| momentum.tick(step)).sum::<f64>()
            };
            let at_60 = travel(60);
            for hz in [24, 90, 120] {
                let other = travel(hz);
                assert!(
                    (other - at_60).abs() < 0.5,
                    "{hz} Hz covers {other:.2} px, 60 Hz {at_60:.2} px"
                );
            }
            let stalled = {
                let mut momentum = ScrollMomentum::new();
                momentum.fling(3_000.0);
                momentum.tick(Duration::from_millis(500))
            };
            assert!(
                (stalled - at_60).abs() < 0.5,
                "a stall is covered, not dropped"
            );
        }

        // Case `a_glide_starts_its_clock_when_it_starts_not_at_the_last_frame`.
        {
            let mut momentum = ScrollMomentum::new();
            momentum.fling(3_000.0);
            let started = momentum.last_step.expect("a fling starts its clock");
            let frame = started + Duration::from_millis(11);
            let first = momentum.advance(frame);
            let expected = glide_step(3_000.0, 0.011).0;
            assert!((first - expected).abs() < 1e-6, "{first} vs {expected}");
        }

        // Case `scroll_momentum_accelerates_repeated_wheel_input_and_settles`.
        {
            let mut momentum = ScrollMomentum::new();
            assert_eq!(momentum.push(60.0), 60.0);
            let first = momentum.tick(Duration::from_millis(16));
            momentum.push(60.0);
            let accelerated = momentum.tick(Duration::from_millis(16));
            assert!(accelerated > first);

            for _ in 0..200 {
                momentum.tick(Duration::from_millis(16));
            }
            assert!(!momentum.is_active());
            assert_eq!(momentum.tick(Duration::from_millis(16)), 0.0);
        }

        // Case `drag_scroller_preserves_clicks_then_reports_content_offset_and_velocity`.
        {
            let start = crate::time::now();
            let mut drag = DragScroller::new();
            drag.begin(100.0, 250.0, start);
            assert_eq!(
                drag.update(102.0, start + Duration::from_millis(10)),
                None,
                "sub-threshold movement remains a click"
            );
            let update = drag
                .update(110.0, start + Duration::from_millis(20))
                .expect("crossing the threshold starts panning");
            assert!(update.began);
            assert_eq!(update.target_offset, 240.0);
            let update = drag
                .update(120.0, start + Duration::from_millis(30))
                .expect("active panning keeps producing offsets");
            assert!(!update.began);
            assert_eq!(update.target_offset, 230.0);
            let finish = drag
                .finish(start + Duration::from_millis(30))
                .expect("tracked gesture finishes once");
            assert!(finish.dragged);
            assert!(
                finish.velocity < 0.0,
                "dragging down flings content toward smaller offsets"
            );
            assert!(drag.finish(start + Duration::from_millis(30)).is_none());
        }

        // Case `drag_scroller_release_before_threshold_is_an_ordinary_click`.
        {
            let now = crate::time::now();
            let mut drag = DragScroller::new();
            drag.begin(20.0, 80.0, now);
            drag.update(22.0, now + Duration::from_millis(16));
            assert_eq!(
                drag.finish(now + Duration::from_millis(16)),
                Some(DragScrollFinish {
                    dragged: false,
                    velocity: 0.0,
                })
            );
        }

        // Case `drag_scroller_does_not_fling_after_the_pointer_has_stopped`.
        {
            let start = crate::time::now();
            let mut drag = DragScroller::new();
            drag.begin(100.0, 100.0, start);
            drag.update(80.0, start + Duration::from_millis(16));
            let finish = drag
                .finish(start + Duration::from_millis(200))
                .expect("gesture finishes");
            assert!(finish.dragged);
            assert_eq!(finish.velocity, 0.0);
        }
    }

    fn viewport() -> VirtualViewport<&'static str> {
        let mut view = VirtualViewport::new(20.0);
        view.reconcile(["a", "b", "c", "d", "e", "f"])
            .expect("unique keys");
        view.set_viewport(40.0);
        view
    }

    #[test]
    fn dense_placement_hit_testing_and_reveal_use_variable_heights() {
        // Case `visible_range_uses_variable_heights_and_pixel_overscan`.
        {
            let mut view = viewport();
            assert!(view.measure(&"a", 10.0));
            assert!(view.measure(&"b", 30.0));
            assert!(view.measure(&"c", 50.0));
            view.set_offset(35.0);
            assert_eq!(view.visible_range(), 1..3);
            view.set_overscan(25.0);
            assert_eq!(view.visible_range(), 1..4);
        }

        // Case `hit_testing_is_the_inverse_of_variable_row_placement`.
        {
            let mut view = viewport();
            view.measure(&"a", 10.0);
            view.measure(&"b", 30.0);
            view.measure(&"c", 50.0);
            view.set_offset(15.0);
            assert_eq!(view.item_view_top(1), Some(-5.0));
            assert_eq!(view.hit_test(0.0), Some(1));
            assert_eq!(view.hit_test(24.9), Some(1));
            assert_eq!(view.hit_test(25.0), Some(2));
            assert_eq!(view.hit_test(40.0), None, "bottom edge is outside");
        }

        // Case `nearest_scrolling_only_moves_for_clipped_items`.
        {
            let mut view = viewport();
            assert!(!view.scroll_to_index(0, ScrollAlignment::Nearest));
            assert!(view.scroll_to_index(3, ScrollAlignment::Nearest));
            assert_eq!(view.offset(), 40.0);
            assert!(!view.scroll_to_index(2, ScrollAlignment::Nearest));
        }

        // Case `nearest_reveal_preserves_the_visible_section_of_a_tall_row`.
        {
            let mut view = viewport();
            view.measure(&"a", 120.0);
            assert!(!view.scroll_to_index(0, ScrollAlignment::Nearest));
            view.set_offset(40.0);
            assert!(!view.scroll_to_index(0, ScrollAlignment::Nearest));
            assert_eq!(view.offset(), 40.0);
        }

        // Case `large_model_keeps_a_small_visible_window`.
        {
            let mut view = VirtualViewport::new(24.0);
            view.reconcile(0_u64..1_000_000).expect("unique keys");
            view.set_viewport(720.0);
            view.set_overscan(48.0);
            view.set_offset(view.max_offset() / 2.0);
            let range = view.visible_range();
            assert!(
                range.len() <= 35,
                "only viewport + overscan rows: {range:?}"
            );
            assert_eq!(view.content_extent(), 24_000_000.0);
        }
    }

    #[test]
    fn dense_anchor_and_measurements_follow_keys_through_edits() {
        // Case `insertion_above_viewport_preserves_stable_anchor`.
        {
            let mut view = viewport();
            view.set_offset(45.0); // c, five pixels below its top
            view.reconcile(["new-1", "new-2", "a", "b", "c", "d", "e", "f"])
                .expect("unique keys");
            assert_eq!(view.key(view.hit_test(0.0).expect("top item")), Some(&"c"));
            let top = view
                .item_view_top(view.index_of(&"c").expect("c"))
                .expect("c geometry");
            assert!((top + 5.0).abs() < 0.001);
        }

        // Case `measuring_above_viewport_does_not_jump_content`.
        {
            let mut view = viewport();
            view.set_offset(45.0); // c at -5
            let before = view.item_view_top(view.index_of(&"c").expect("c"));
            view.measure(&"a", 80.0);
            let after = view.item_view_top(view.index_of(&"c").expect("c"));
            assert_eq!(before, after);
        }

        // Case `measurements_follow_keys_across_reorder`.
        {
            let mut view = viewport();
            view.measure(&"b", 77.0);
            view.reconcile(["f", "b", "a", "c", "d", "e"])
                .expect("unique keys");
            assert_eq!(
                view.item_height(view.index_of(&"b").expect("b")),
                Some(77.0)
            );
        }

        // Case `deleting_anchor_uses_next_surviving_item`.
        {
            let mut view = viewport();
            view.set_offset(45.0); // c
            view.reconcile(["a", "b", "d", "e", "f"])
                .expect("unique keys");
            assert_eq!(view.key(view.hit_test(0.0).expect("top item")), Some(&"d"));
            assert_eq!(
                view.item_view_top(view.index_of(&"d").expect("d")),
                Some(0.0)
            );
        }

        // Case `duplicate_keys_are_rejected_without_mutation`.
        {
            let mut view = viewport();
            let before = view.content_extent();
            assert_eq!(
                view.reconcile(["a", "a"]),
                Err(ReconcileError::DuplicateKey)
            );
            assert_eq!(view.len(), 6);
            assert_eq!(view.content_extent(), before);
        }
    }

    #[test]
    fn sparse_viewport_pages_places_and_hits_at_huge_row_counts() {
        // Case `sparse_viewport_represents_a_hundred_million_rows_without_resident_keys`.
        {
            let mut view = SparseVirtualViewport::<u64>::new(24.0);
            view.reconcile(100_000_000, |_| None, None);
            view.set_viewport(720.0);
            view.set_overscan(48.0);
            view.set_offset(view.max_offset() / 2.0);

            assert_eq!(view.len(), 100_000_000);
            assert_eq!(view.resident_measurements(), 0);
            assert!(view.visible_range().len() <= 35);
            assert_eq!(view.content_extent(), 2_400_000_000.0);
        }

        // Case `sparse_paging_is_constant_space_at_hundred_million_rows`.
        {
            let mut view = SparseVirtualViewport::<u64>::new(24.0);
            view.reconcile(100_000_000, |_| None, None);
            view.set_viewport(720.0);
            let current = 50_000_000;
            view.set_offset(view.item_top(current).expect("middle row"));

            let (next, moved) = view.page_from(current, true).expect("page down");
            assert!(moved);
            assert_eq!(next, current + 27);
            assert_eq!(view.resident_measurements(), 0);

            let (previous, moved) = view.page_from(next, false).expect("page up");
            assert!(moved);
            assert_eq!(previous, current);
            assert_eq!(view.resident_measurements(), 0);
        }

        // Case `sparse_measurements_drive_placement_and_hit_testing`.
        {
            let mut view = SparseVirtualViewport::new(20.0);
            view.reconcile(6, |_| None::<usize>, None);
            view.set_viewport(40.0);
            assert!(view.measure(0, "a", 10.0));
            assert!(view.measure(1, "b", 30.0));
            assert!(view.measure(2, "c", 50.0));
            view.set_offset(15.0);

            assert_eq!(view.item_view_top(1), Some(-5.0));
            assert_eq!(view.hit_test(0.0), Some(1));
            assert_eq!(view.hit_test(24.9), Some(1));
            assert_eq!(view.hit_test(25.0), Some(2));
        }

        // Case `sparse_minimum_height_is_reversible_for_estimates_and_measurements`.
        {
            let mut view = SparseVirtualViewport::new(20.0);
            view.reconcile(3, |_| None::<usize>, None);
            assert!(view.measure(1, "custom", 12.0));

            view.set_minimum_height(48.0);
            assert_eq!(view.estimated_height(), 48.0);
            assert_eq!(view.item_height(0), Some(48.0));
            assert_eq!(view.item_height(1), Some(48.0));
            assert_eq!(view.content_extent(), 144.0);

            view.set_minimum_height(1.0);
            assert_eq!(view.estimated_height(), 20.0);
            assert_eq!(view.item_height(0), Some(20.0));
            assert_eq!(view.item_height(1), Some(12.0));
            assert_eq!(view.content_extent(), 52.0);
        }
    }

    #[test]
    fn sparse_anchor_and_measurements_follow_keys_through_edits() {
        // Case `sparse_reconciliation_preserves_a_stable_anchor_across_insertions`.
        {
            let mut view = SparseVirtualViewport::new(20.0);
            view.reconcile(100, |_| None::<usize>, None);
            view.set_viewport(40.0);
            view.set_offset(50.0 * 20.0 + 5.0);
            let anchor = view.capture_anchor(|index| u64::try_from(index).ok());
            let before = view.item_view_top(50);

            view.reconcile(
                110,
                |key| usize::try_from(*key).ok().map(|index| index + 10),
                anchor.as_ref(),
            );

            assert_eq!(view.item_view_top(60), before);
        }

        // Case `sparse_measurements_follow_keys_across_moves`.
        {
            let mut view = SparseVirtualViewport::new(20.0);
            view.reconcile(10, |_| None::<usize>, None);
            assert!(view.measure(3, "measured", 73.0));
            view.reconcile(10, |key| (*key == "measured").then_some(7), None);

            assert_eq!(view.item_height(3), Some(20.0));
            assert_eq!(view.item_height(7), Some(73.0));
            assert_eq!(view.resident_measurements(), 1);
        }

        // Case `sparse_measurement_can_move_from_placeholder_to_loaded_key`.
        {
            let mut view = SparseVirtualViewport::new(20.0);
            view.reconcile(10, |_| None::<usize>, None);
            assert!(view.measure(3, "loading", 73.0));
            assert!(view.remap_key(&"loading", "loaded"));
            view.reconcile(10, |key| (*key == "loaded").then_some(3), None);

            assert_eq!(view.item_height(3), Some(73.0));
            assert_eq!(view.resident_measurements(), 1);
            assert!(!view.remap_key(&"missing", "unused"));
        }

        // Case `sparse_deleted_anchor_falls_back_to_the_same_logical_position`.
        {
            let mut view = SparseVirtualViewport::new(20.0);
            view.reconcile(100, |_| None::<usize>, None);
            view.set_viewport(40.0);
            view.set_offset(50.0 * 20.0 + 5.0);
            let anchor = view.capture_anchor(|index| u64::try_from(index).ok());

            view.reconcile(99, |_| None, anchor.as_ref());

            assert_eq!(view.hit_test(0.0), Some(50));
            assert_eq!(view.item_view_top(50), Some(0.0));
        }
    }
}
