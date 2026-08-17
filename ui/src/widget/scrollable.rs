use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::BlendMode;
use winit::event::{WindowEvent, MouseScrollDelta, MouseButton, ElementState};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::{Widget, Element};
use crate::widget::scrollbar::{Scrollbar, ScrollMetrics, PressOutcome};
use crate::color_compat::ColorCompat;
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
/// grabbable target. Mirrors [`scrollbar::MIN_THUMB_LEN`] and is used
/// only for the local track-length sanity guard.
const MIN_THUMB_LEN: f64 = crate::widget::scrollbar::MIN_THUMB_LEN;
/// Content must exceed the viewport by more than this (logical px) on an
/// axis before its scrollbar is shown — otherwise the content "fits" and
/// the bar is hidden entirely.
const FIT_EPS: f32 = 0.5;
/// How far (logical px) the bar's grab/reveal zone extends past its track
/// toward the content, so approaching the edge reveals a faded-out overlay
/// bar and the thin thumb stays comfortably grabbable.
const HOVER_EXPAND: f64 = 6.0;
/// Distance (logical px) scrolled per arrow-key press or per wheel line.
const LINE_STEP: f32 = 48.0;
/// Fraction of a viewport a track-page (click / PageUp-Down) moves. Mirrors
/// the shared [`scrollbar::PAGE_FRACTION`].
const PAGE_FRACTION: f32 = crate::widget::scrollbar::PAGE_FRACTION as f32;
/// Overlay auto-hide: how long the bar stays fully opaque after the last
/// scroll/hover before it starts fading.
const VISIBLE_HOLD_MS: u128 = 900;
/// Overlay auto-hide: fade-out ramp duration once the hold expires.
const FADE_MS: u128 = 320;
/// Click-and-hold track paging: delay before the first auto-repeat.
const HOLD_INITIAL_MS: u64 = 300;
/// Click-and-hold track paging: interval between repeats.
const HOLD_REPEAT_MS: u64 = 55;
/// Smooth-scroll easing time constant (seconds). The rendered offset chases
/// its target with `1 - exp(-dt / TAU)` per frame, so the motion is
/// frame-rate independent — identical feel at 60 Hz and 120 Hz. Smaller is
/// snappier; ~0.09 s reads as buttery but never floaty.
const SCROLL_EASE_TAU: f32 = 0.09;
/// Once the rendered offset is within this many logical px of its target the
/// ease snaps shut and stops pumping frames — no sub-pixel idle spin.
const SCROLL_SETTLE_EPS: f32 = 0.4;
/// Any single easing step advances by at most this slice of wall time, so a
/// wheel flick that resumes after a long idle can't teleport the content in
/// one giant frame (a stale `last_ease` is clamped to this).
const EASE_MAX_DT: f32 = 0.05;
/// Wheel notches arriving within this window count as one flick and get a
/// velocity boost, so a fast flick travels further per notch than a slow,
/// deliberate one.
const FLICK_WINDOW_MS: u128 = 140;
/// Upper bound on the flick boost (a back-to-back notch multiplies its step
/// by this; it tapers to 1× as the gap approaches [`FLICK_WINDOW_MS`]).
const FLICK_MAX_BOOST: f32 = 2.6;

/// A track click-and-hold in progress: the bar keeps paging toward the
/// press point on a timer until the pointer is released or the thumb
/// reaches the click.
#[derive(Clone, Copy, Debug)]
struct TrackHold {
    /// Which bar is held (`true` = vertical).
    vertical: bool,
    /// Page direction: `+1` toward the end, `-1` toward the start.
    dir: f64,
    /// The press point, in logical coordinates. Paging stops once the thumb
    /// reaches it so the view doesn't overshoot past the click.
    anchor: Point,
    /// When the next auto-repeat page is due.
    next_at: Instant,
}

pub struct Scrollable {
    axis: ScrollAxis,
    /// The **rendered** scroll position — what `apply_layout` positions the
    /// children by and the scrollbar thumb tracks. Under smooth scrolling it
    /// eases toward [`Self::target_x`]/[`Self::target_y`] each frame rather
    /// than jumping, so wheel/trackpad scrolling glides instead of stepping.
    offset_x: f32,
    offset_y: f32,
    /// The scroll position wheel/trackpad input drives toward. Equal to the
    /// rendered offset at rest and for every *instant* mutation (thumb drag,
    /// keyboard paging, programmatic `set_offset_y`); a wheel gesture pushes it
    /// ahead and the rendered offset chases it (the momentum/inertia feel).
    /// Held identical to the offset under reduced motion — no easing at all.
    target_x: f32,
    target_y: f32,
    content_w: f32,
    content_h: f32,
    viewport_w: f32,
    viewport_h: f32,
    vx: f32,
    vy: f32,
    decay: f32,
    sticky_header_height: f32,
    /// When set, pin the Scrollable's outer viewport height to an exact
    /// logical-pixel value instead of `percent(1.0)`. A window-hosted
    /// list/settings pane whose parent doesn't supply a definite height
    /// (e.g. an intrinsic-height column, or a standalone app root) would
    /// otherwise resolve `percent(1.0)` against an indefinite parent and
    /// collapse to a zero-height viewport — nothing scrolls. Setting this
    /// (or [`Scrollable::fill_parent_height`]) gives the viewport a real
    /// extent so `content_h > viewport_h` can be reached.
    fixed_height: Option<f32>,
    /// When set, absorb the parent's *remaining* flex height
    /// (`flex_grow: 1 + flex_basis: 0`) rather than claiming
    /// `percent(1.0)`. This is the correct sizing for the common
    /// "scrollable list below a fixed-height header" layout inside a flex
    /// Column: percent(1.0) would claim 100% of the parent on top of the
    /// header and overflow, whereas grow-from-zero takes exactly what's
    /// left. Mutually exclusive with [`Scrollable::fixed_height`]
    /// (an explicit pin wins).
    fill_parent_height: bool,
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
    /// Shared vertical-bar interaction model (thumb drag, track paging,
    /// hover). See [`crate::widget::scrollbar`].
    vbar: Scrollbar,
    /// Shared horizontal-bar interaction model.
    hbar: Scrollbar,
    /// Overlay auto-hide behaviour. When `true` (the default) the bar fades
    /// in on scroll/hover/focus and fades back out when idle, like a modern
    /// overlay scrollbar. When `false` the bar is always drawn at full
    /// opacity ([`Scrollable::always_visible`]).
    overlay: bool,
    /// Monotonic timestamp of the last bar "activity" (scroll, hover-enter,
    /// hover-leave, focus). Drives the overlay fade. `None` until the bar has
    /// ever been woken — a freshly-shown overlay list has no visible bar
    /// until the content overflows (a one-shot flash) or the user interacts.
    last_activity: Option<Instant>,
    /// Whether the pointer is currently within a bar's reveal zone. Pins the
    /// overlay bar visible while the pointer lingers near the edge, and
    /// expands the track chrome.
    pointer_near_bar: bool,
    /// Rising-edge latch so the bar flashes into view the first frame its
    /// content overflows (discoverability), then obeys the fade.
    was_scrollable: bool,
    /// Set when this scroll container holds keyboard focus, so PageUp/Down,
    /// Home/End and arrow keys scroll it. Also pins the overlay bar visible.
    focused: bool,
    /// An in-progress click-and-hold on a track (auto-repeat paging).
    hold: Option<TrackHold>,
    /// Purely-visual dirty flag (bar fade/hover) that must trigger a redraw
    /// but — unlike [`Self::scroll_dirty`] — needs no `apply_layout`, since
    /// nothing moved. Cleared by `mark_drawn`.
    bar_dirty: bool,
    /// Optional externally-owned scroll position that survives this widget
    /// being dropped and rebuilt. Apps that recreate their whole widget
    /// subtree every frame (the file explorer's rebuild-on-command model)
    /// would otherwise reset the scroll to the top on any structural change
    /// — clicking a row, typing in search, changing sort. Handing the
    /// Scrollable a shared `(offset_x, offset_y)` cell via
    /// [`Scrollable::with_persisted_offset`] lets the fresh instance seed
    /// itself from where the last one left off; every offset mutation writes
    /// back through `clamp`, so the cell always mirrors the live position.
    persisted_offset: Option<Rc<Cell<(f32, f32)>>>,
    /// Reserved right-edge lane (logical px) for the vertical overlay
    /// scrollbar, so the bar never draws over the content. When set, the child
    /// content is laid out `content_gutter` px narrower and the bar sits in the
    /// vacated lane — the content is flush to the lane's edge, the bar owns its
    /// own track to the right. `0` (the default) keeps the classic overlay bar
    /// floating above the content's right edge. See
    /// [`Scrollable::with_content_gutter`].
    content_gutter: f32,
    /// Master switch for eased (momentum) wheel/trackpad scrolling. On by
    /// default; opt out via [`Scrollable::with_smooth_scroll`] for a viewport
    /// that wants raw 1:1 wheel stepping. Independent of the drawn scrollbar.
    smooth: bool,
    /// Cached `Theme::reduce_motion`, refreshed every `build_layout`. The event
    /// and `sync` paths have no `Theme` in hand, so the smoothness decision
    /// reads this snapshot: when the OS asks to reduce motion, wheel scrolling
    /// applies instantly (no ease, no flick momentum).
    reduce_motion: bool,
    /// Wall-clock stamp of the previous easing step, so the per-frame
    /// interpolation uses real elapsed time (refresh-rate independent). `None`
    /// resets the ramp so a fresh gesture starts from a nominal frame slice.
    last_ease: Option<Instant>,
    /// Wall-clock stamp of the previous wheel notch, for the flick
    /// velocity boost (rapid successive notches travel further).
    last_wheel: Option<Instant>,
}

impl Scrollable {
    pub fn new(axis: ScrollAxis) -> Self {
        Self {
            axis,
            offset_x: 0.0,
            offset_y: 0.0,
            target_x: 0.0,
            target_y: 0.0,
            content_w: 0.0,
            content_h: 0.0,
            viewport_w: 0.0,
            viewport_h: 0.0,
            vx: 0.0,
            vy: 0.0,
            decay: 0.92,
            sticky_header_height: 0.0,
            fixed_height: None,
            fill_parent_height: false,
            child: None,
            rect: Rect::ZERO,
            scroll_dirty: false,
            show_scrollbar: true,
            last_cursor: Point::ZERO,
            vbar: Scrollbar::vertical(),
            hbar: Scrollbar::horizontal(),
            overlay: true,
            last_activity: None,
            pointer_near_bar: false,
            was_scrollable: false,
            focused: false,
            hold: None,
            bar_dirty: false,
            persisted_offset: None,
            content_gutter: 0.0,
            smooth: true,
            reduce_motion: false,
            last_ease: None,
            last_wheel: None,
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

    /// Keep the scrollbar permanently visible instead of the default
    /// overlay auto-hide (fade-in-on-activity / fade-out-when-idle). Use for
    /// panes where a persistent bar is wanted as a constant affordance. The
    /// bar still only appears on an axis whose content actually overflows.
    pub fn always_visible(mut self) -> Self {
        self.overlay = false;
        self
    }

    /// Choose overlay auto-hide (`true`, the default) or a permanently
    /// visible bar (`false`). See [`Scrollable::always_visible`].
    pub fn with_overlay(mut self, overlay: bool) -> Self {
        self.overlay = overlay;
        self
    }

    /// Pin the scroll viewport to an exact logical-pixel height. Use this
    /// when the Scrollable is hosted somewhere that doesn't supply a
    /// definite height (an intrinsic-height column, a standalone window
    /// root) so the viewport doesn't collapse to zero and the pane can
    /// actually scroll. See [`Scrollable::fixed_height`].
    pub fn with_height(mut self, px: f32) -> Self {
        self.fixed_height = Some(px);
        self
    }

    /// Make the scroll viewport absorb the parent flex container's
    /// *remaining* height (grow-from-zero) instead of claiming 100%.
    /// Correct for "scrollable list below a fixed header" inside a flex
    /// Column. See [`Scrollable::fill_parent_height`].
    pub fn fill_parent_height(mut self) -> Self {
        self.fill_parent_height = true;
        self
    }

    /// Bind this Scrollable to an externally-owned scroll position so it
    /// survives the widget being dropped and rebuilt. The fresh instance
    /// seeds its offset from the cell; every later scroll writes back
    /// through `clamp`. See [`Scrollable::persisted_offset`]. The seeded
    /// offset is intentionally left *unclamped* here — content/viewport
    /// aren't known until the first `apply_layout`, which clamps it (and
    /// rewrites the cell) against the real extents.
    pub fn with_persisted_offset(mut self, cell: Rc<Cell<(f32, f32)>>) -> Self {
        let (x, y) = cell.get();
        self.offset_x = x;
        self.offset_y = y;
        // Resume at rest, not mid-ease: the rebuilt instance should sit exactly
        // where the last one left off with no glide from zero.
        self.target_x = x;
        self.target_y = y;
        self.persisted_offset = Some(cell);
        self
    }

    /// Enable (`true`, the default) or disable eased momentum wheel/trackpad
    /// scrolling. When off, each wheel notch moves the content 1:1 with no
    /// interpolation. Reduced motion (OS-driven) forces the instant path
    /// regardless of this flag.
    pub fn with_smooth_scroll(mut self, on: bool) -> Self {
        self.smooth = on;
        self
    }

    /// Reserve a `px`-wide lane on the right edge for the vertical scrollbar so
    /// the bar never overlaps the content. The child lays out `px` narrower and
    /// the overlay bar draws in the reserved lane; content is flush to the lane's
    /// left edge. Pass the bar's footprint (thickness + margins, ~12–14px) to get
    /// a clean gutter. `0` keeps the default floating-overlay behaviour.
    pub fn with_content_gutter(mut self, px: f32) -> Self {
        self.content_gutter = px.max(0.0);
        self
    }

    pub fn set_content_size(&mut self, w: f32, h: f32) { self.content_w = w; self.content_h = h; self.clamp(); }
    pub fn set_viewport_size(&mut self, w: f32, h: f32) { self.viewport_w = w; self.viewport_h = h; self.clamp(); }
    pub fn offset_x(&self) -> f32 { self.offset_x }
    pub fn offset_y(&self) -> f32 { self.offset_y }

    /// Directly set the vertical scroll offset (clamped to the content). For a
    /// manually-driven viewport seeding its initial position — e.g. a day
    /// timeline opening at the work-day rather than at midnight.
    pub fn set_offset_y(&mut self, y: f32) {
        self.offset_y = y;
        self.clamp();
        // A programmatic seek is an instant jump — collapse any pending ease so
        // the view doesn't glide away from the position just set.
        self.sync_target_to_offset();
    }

    /// Collapse the eased target onto the rendered offset. Called by every
    /// *instant* mutation (thumb drag, programmatic seek, `scroll_into_view`)
    /// so a later ease frame doesn't drag the view off the position just set.
    fn sync_target_to_offset(&mut self) {
        self.target_x = self.offset_x;
        self.target_y = self.offset_y;
    }

    /// Whether the vertical axis currently overflows its viewport (i.e. the
    /// content is taller than the visible area and can be scrolled). Lets a
    /// caller that drives this Scrollable manually decide whether to seed an
    /// initial offset or draw the bar.
    pub fn vertical_overflows(&self) -> bool {
        self.content_h > self.viewport_h + FIT_EPS
    }

    /// Position a manually-driven (child-less) Scrollable: set the viewport
    /// **rect** — used for scrollbar geometry and pointer hit-testing — and the
    /// viewport size together. Pair with [`Scrollable::set_content_size`] when the
    /// content is painted imperatively by the caller rather than laid out and
    /// drawn through this widget's own `draw`.
    pub fn set_viewport_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.set_viewport_size(rect.width().max(0.0) as f32, rect.height().max(0.0) as f32);
    }

    /// Composite the shared overlay scrollbar (track + thumb, with the same
    /// fade / hover behaviour as a laid-out Scrollable) on top of content the
    /// caller painted itself. Pairs with [`Scrollable::set_viewport_rect`] +
    /// [`Scrollable::set_content_size`] for a viewport this Scrollable neither
    /// lays out nor draws.
    pub fn draw_bar_overlay(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        self.draw_scrollbars(scene, transform, theme);
    }
    pub fn momentum_speed(&self) -> f32 { (self.vx * self.vx + self.vy * self.vy).sqrt() }

    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        let (ox, oy) = (self.offset_x, self.offset_y);
        match self.axis {
            ScrollAxis::Vertical => { self.offset_y += dy; }
            ScrollAxis::Horizontal => { self.offset_x += dx; }
            ScrollAxis::Both => { self.offset_x += dx; self.offset_y += dy; }
        }
        self.clamp();
        // `scroll_by` is the *instant* path (thumb drag, keyboard paging,
        // programmatic). Keep the eased target pinned to the offset so a
        // subsequent ease frame can't drag the view away from it.
        self.sync_target_to_offset();
        // Only signal dirty if the clamp didn't eat the entire
        // delta — at the edges of the content, `offset_y` doesn't
        // actually change, and we'd waste a redraw otherwise.
        if self.offset_x != ox || self.offset_y != oy {
            self.scroll_dirty = true;
            self.wake();
        }
    }

    /// Eased (momentum) scroll — the wheel/trackpad path. Pushes the *target*
    /// ahead by the delta and lets the rendered offset chase it over the next
    /// frames (see [`Self::ease_tick`]), giving smooth, refresh-rate-adaptive
    /// motion. Falls back to an instant [`Self::scroll_by`] when smoothing is
    /// disabled or the OS asks to reduce motion. Rapid successive notches
    /// accumulate in the target faster than the offset can catch them, which
    /// is what reads as inertia.
    pub fn smooth_scroll_by(&mut self, dx: f32, dy: f32) {
        if !self.smooth || self.reduce_motion {
            self.scroll_by(dx, dy);
            return;
        }
        let was_easing = self.easing_active();
        let (tx, ty) = (self.target_x, self.target_y);
        match self.axis {
            ScrollAxis::Vertical => { self.target_y += dy; }
            ScrollAxis::Horizontal => { self.target_x += dx; }
            ScrollAxis::Both => { self.target_x += dx; self.target_y += dy; }
        }
        self.clamp();
        // Nothing changed (already at the content edge) → let the wheel bubble
        // to an outer scroll region; don't wake the bar for a no-op.
        if self.target_x == tx && self.target_y == ty {
            return;
        }
        // Starting a fresh glide from rest: reset the ease clock so the first
        // step uses a nominal frame slice rather than a stale, long-idle dt.
        if !was_easing {
            self.last_ease = None;
        }
        // The overlay bar rides scroll activity, and the ease loop needs a
        // frame pumped even though the rendered offset hasn't moved yet.
        self.wake();
        self.scroll_dirty = true;
    }

    /// Whether the rendered offset is still easing toward its target (and so
    /// the host must keep pumping frames). False under reduced motion or when
    /// smoothing is off — those paths keep offset and target identical.
    fn easing_active(&self) -> bool {
        self.smooth
            && !self.reduce_motion
            && ((self.target_x - self.offset_x).abs() > SCROLL_SETTLE_EPS
                || (self.target_y - self.offset_y).abs() > SCROLL_SETTLE_EPS)
    }

    /// Advance the rendered offset one frame toward its target with
    /// frame-rate-independent exponential smoothing. `now` is injected so the
    /// curve is unit-testable; production wires the wall clock via `sync`.
    /// Returns whether the offset actually moved.
    fn ease_tick(&mut self, now: Instant) -> bool {
        if !self.easing_active() {
            self.last_ease = Some(now);
            return false;
        }
        let dt = match self.last_ease {
            Some(prev) => {
                let measured = now.saturating_duration_since(prev).as_secs_f32();
                // Floor an implausibly small gap (a headless harness pumping
                // `sync` in a tight loop, or a >500 Hz display) to one nominal
                // 60 Hz step so every pumped frame makes forward progress and
                // the ease always terminates. Real 60/120/240 Hz frames sit
                // above the floor and use their true elapsed time.
                if measured < 0.002 { 1.0 / 60.0 } else { measured.min(EASE_MAX_DT) }
            }
            None => 1.0 / 60.0,
        };
        self.last_ease = Some(now);
        if dt <= 0.0 {
            return false;
        }
        let alpha = 1.0 - (-dt / SCROLL_EASE_TAU).exp();
        let (ox, oy) = (self.offset_x, self.offset_y);
        self.offset_x += (self.target_x - self.offset_x) * alpha;
        self.offset_y += (self.target_y - self.offset_y) * alpha;
        // Snap the last sub-pixel gap shut so the ease terminates cleanly.
        if (self.target_x - self.offset_x).abs() <= SCROLL_SETTLE_EPS {
            self.offset_x = self.target_x;
        }
        if (self.target_y - self.offset_y).abs() <= SCROLL_SETTLE_EPS {
            self.offset_y = self.target_y;
        }
        self.clamp();
        let moved = self.offset_x != ox || self.offset_y != oy;
        if moved {
            self.scroll_dirty = true;
        }
        moved
    }

    /// Velocity boost for a wheel notch at `now`: back-to-back notches (within
    /// [`FLICK_WINDOW_MS`]) scale up toward [`FLICK_MAX_BOOST`], tapering to 1×
    /// as the gap widens, so a fast flick travels further per notch than a
    /// deliberate one. Records `now` as the latest notch.
    fn flick_boost(&mut self, now: Instant) -> f32 {
        let boost = match self.last_wheel {
            Some(prev) => {
                let gap = now.saturating_duration_since(prev).as_millis();
                if gap >= FLICK_WINDOW_MS {
                    1.0
                } else {
                    let t = gap as f32 / FLICK_WINDOW_MS as f32; // 0 (fast) → 1 (slow)
                    FLICK_MAX_BOOST - (FLICK_MAX_BOOST - 1.0) * t
                }
            }
            None => 1.0,
        };
        self.last_wheel = Some(now);
        boost
    }

    /// Advance the smooth-scroll ease exactly one frame against the wall clock,
    /// returning whether the rendered offset moved. For headless shots that
    /// want to capture the view mid-glide (offset still short of its target)
    /// without a live frame loop.
    pub fn ease_step(&mut self) -> bool {
        self.ease_tick(Instant::now())
    }

    /// Run the ease to completion against a synthetic clock. For headless
    /// shots and tests that need the settled offset without a live frame loop;
    /// bounded so a runaway target can't spin forever.
    pub fn settle_scroll(&mut self) {
        let mut t = Instant::now();
        for _ in 0..600 {
            if !self.easing_active() {
                break;
            }
            t += Duration::from_millis(16);
            self.ease_tick(t);
        }
    }

    /// The scroll position the wheel/trackpad is driving toward (equals the
    /// rendered offset at rest). Exposed for callers and tests that need the
    /// eventual destination rather than the in-flight rendered position.
    pub fn scroll_target(&self) -> (f32, f32) {
        (self.target_x, self.target_y)
    }

    /// Refresh the overlay auto-hide timer: fade the bar to full opacity now
    /// and restart the fade-out countdown. Called on any scroll, on hover
    /// enter/leave, and when the container gains focus.
    fn wake(&mut self) {
        self.last_activity = Some(Instant::now());
        self.bar_dirty = true;
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
        // An instant reveal — don't let a pending ease glide back off the
        // element we just brought into view.
        self.sync_target_to_offset();
    }
    fn clamp(&mut self) {
        let max_x = (self.content_w - self.viewport_w).max(0.0);
        let max_y = (self.content_h - self.viewport_h).max(0.0);
        self.offset_x = self.offset_x.clamp(0.0, max_x);
        self.offset_y = self.offset_y.clamp(0.0, max_y);
        // Keep the eased target inside the same range — otherwise a target
        // parked past a now-shorter content (a list that shrank) would leave
        // the offset easing toward an unreachable position and never settle.
        self.target_x = self.target_x.clamp(0.0, max_x);
        self.target_y = self.target_y.clamp(0.0, max_y);
        // Mirror the live position into the shared cell (if any) so a fresh
        // instance rebuilt next frame seeds from here. Every offset mutation
        // funnels through `clamp`, so this is the single write-back point.
        if let Some(cell) = &self.persisted_offset {
            cell.set((self.offset_x, self.offset_y));
        }
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

    /// Scroll geometry along the vertical axis, for the shared bar model.
    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content_h as f64, self.viewport_h as f64, self.offset_y as f64)
    }
    /// Scroll geometry along the horizontal axis.
    fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content_w as f64, self.viewport_w as f64, self.offset_x as f64)
    }

    fn vertical_thumb(&self) -> Option<Rect> {
        let track = self.vertical_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    fn horizontal_thumb(&self) -> Option<Rect> {
        let track = self.horizontal_track()?;
        self.hbar.thumb_rect(track, self.h_metrics())
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

    /// The pointer-reveal zone for the vertical bar: its track widened toward
    /// the content by [`HOVER_EXPAND`] so a faded-out overlay bar reveals as
    /// the pointer approaches the edge and the thin thumb stays grabbable.
    fn vertical_hover_zone(&self) -> Option<Rect> {
        let t = self.vertical_track()?;
        Some(Rect::new(t.x0 - HOVER_EXPAND, t.y0, t.x1, t.y1))
    }
    fn horizontal_hover_zone(&self) -> Option<Rect> {
        let t = self.horizontal_track()?;
        Some(Rect::new(t.x0, t.y0 - HOVER_EXPAND, t.x1, t.y1))
    }

    /// Whether the pointer is inside either bar's reveal zone.
    fn cursor_near_bar(&self, cur: Point) -> bool {
        self.vertical_hover_zone().is_some_and(|z| z.contains(cur))
            || self.horizontal_hover_zone().is_some_and(|z| z.contains(cur))
    }

    /// Whether the bar is pinned fully visible by an active interaction —
    /// dragging, held-paging, hovered, near the edge, or focused — as opposed
    /// to riding the idle fade.
    fn bar_pinned(&self) -> bool {
        self.focused
            || self.hold.is_some()
            || self.pointer_near_bar
            || self.vbar.is_active()
            || self.hbar.is_active()
    }

    /// Current overlay opacity in `[0, 1]`. `1.0` for a persistent bar, under
    /// reduced motion, or while pinned; otherwise it rides the
    /// hold-then-fade curve since the last activity. `now`/`reduce_motion`
    /// are injected so the curve is unit-testable; `bar_opacity` wires in the
    /// wall clock and the live theme.
    fn bar_opacity_at(&self, now: Instant, reduce_motion: bool) -> f32 {
        if !self.overlay || reduce_motion || self.bar_pinned() {
            return 1.0;
        }
        match self.last_activity {
            None => 0.0,
            Some(t) => {
                let e = now.saturating_duration_since(t).as_millis();
                if e < VISIBLE_HOLD_MS {
                    1.0
                } else if e < VISIBLE_HOLD_MS + FADE_MS {
                    1.0 - (e - VISIBLE_HOLD_MS) as f32 / FADE_MS as f32
                } else {
                    0.0
                }
            }
        }
    }

    fn bar_opacity(&self, theme: &Theme) -> f32 {
        self.bar_opacity_at(Instant::now(), theme.reduce_motion)
    }

    /// Whether the overlay fade is still animating (opacity changing) and the
    /// host should keep pumping frames. False for a persistent bar, while
    /// pinned (opacity constant), or once fully faded.
    fn fade_in_progress(&self) -> bool {
        if !self.overlay || self.bar_pinned() {
            return false;
        }
        match self.last_activity {
            None => false,
            Some(t) => t.elapsed().as_millis() < VISIBLE_HOLD_MS + FADE_MS,
        }
    }

    /// Advance a click-and-hold track page. Called every frame from `sync`
    /// with the wall clock (and directly, with an injected clock, by tests).
    /// Pages once toward the held direction when the repeat timer is due and
    /// the thumb hasn't yet reached the press point, then reschedules.
    fn advance_hold(&mut self, now: Instant) {
        let Some(hold) = self.hold else { return };
        if now < hold.next_at {
            return;
        }
        // Reschedule first so a hold that pauses at the click point still
        // ticks (and resumes if the geometry later allows another step).
        if let Some(h) = &mut self.hold {
            h.next_at = now + Duration::from_millis(HOLD_REPEAT_MS);
        }
        let page = if hold.vertical {
            self.viewport_h * PAGE_FRACTION
        } else {
            self.viewport_w * PAGE_FRACTION
        };
        // Stop paging once the thumb has reached / passed the press point so
        // the view settles exactly where the user clicked rather than racing
        // past it.
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
            self.scroll_by(0.0, (hold.dir as f32) * page);
        } else {
            self.scroll_by((hold.dir as f32) * page, 0.0);
        }
    }

    fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.show_scrollbar {
            return;
        }
        let opacity = self.bar_opacity(theme);
        if opacity <= 0.003 {
            return;
        }
        let radius = BAR_THICKNESS / 2.0;
        // The track chrome only shows in the "expanded" state (persistent,
        // focused, hovered near the edge, dragging, or held); an idle overlay
        // bar is just the thumb, like macOS / GNOME overlay scrollbars.
        let expanded = !self.overlay || self.bar_pinned();
        let track_color = theme.tokens.colors.surface_white_5.with_alpha_factor(opacity);
        for (track, thumb, active) in [
            (self.vertical_track(), self.vertical_thumb(), self.vbar.is_active()),
            (self.horizontal_track(), self.horizontal_thumb(), self.hbar.is_active()),
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
                // Idle thumb uses the neutral border token; hover/drag/focus
                // brightens it to the dim-text token for affordance.
                let base = if active || self.focused { theme.text_dim } else { theme.border_standard };
                let color = base.with_alpha_factor(opacity);
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
        // Drive click-and-hold track paging: while the host pumps animation
        // frames (see `needs_animation_frame`) it re-syncs each tick, giving
        // the held bar its auto-repeat cadence.
        if self.hold.is_some() {
            self.advance_hold(Instant::now());
        }
        // Advance smooth (momentum) wheel/trackpad scrolling: the rendered
        // offset eases toward its target one frame per re-sync. `sync` is the
        // hook the host pumps every animation frame (`needs_animation_frame`
        // re-arms it), giving the ease its per-frame cadence at the display's
        // refresh rate — the same mechanism as `advance_hold` above.
        if self.easing_active() {
            self.ease_tick(Instant::now());
        }
        if let Some(c) = &mut self.child { c.sync(s, p); }
    }

    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        let ticking = self.momentum_speed() > 0.0;
        if ticking { self.tick(1.0 / 60.0); }
        let child = self.child.as_mut().map(|c| c.update(s, p)).unwrap_or(false);
        ticking || child
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        // Snapshot reduced-motion from the live theme — the event/`sync` paths
        // that decide whether to ease have no `Theme` in hand. If motion was
        // just turned down mid-glide, finish the scroll instantly so the offset
        // never strands partway to a target it will no longer ease toward.
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion && (self.offset_x != self.target_x || self.offset_y != self.target_y) {
            self.offset_x = self.target_x;
            self.offset_y = self.target_y;
            self.scroll_dirty = true;
        }
        let child_node = self.child.as_mut().map(|c| c.build_layout(taffy, theme, node_map));
        // Resolve the outer height. Default (`percent(1.0)`) is preserved
        // for the overwhelmingly-common "fills a definite-height parent"
        // case. An explicit pin or `fill_parent_height` opt-in rescues the
        // zero-viewport collapse when the parent height is indefinite.
        let height = match self.fixed_height {
            Some(px) => Dimension::length(px),
            None if self.fill_parent_height => Dimension::length(0.0),
            None => Dimension::percent(1.0),
        };
        // Reserve the right-edge scrollbar lane as padding: the child's
        // content box shrinks by `content_gutter`, so nothing lays out under
        // the overlay bar (drawn at the viewport's right edge, inside the gap).
        let pad_right = taffy::LengthPercentage::length(self.content_gutter);
        let style = Style {
            size: Size { width: Dimension::percent(1.0), height },
            flex_grow: if self.fill_parent_height { 1.0 } else { 0.0 },
            overflow: taffy::Point { x: taffy::Overflow::Hidden, y: taffy::Overflow::Hidden },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(0.0),
                right: pad_right,
                top: taffy::LengthPercentage::length(0.0),
                bottom: taffy::LengthPercentage::length(0.0),
            },
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
        // Flash the overlay bar into view the first frame its content
        // overflows, so a scrollable region is discoverable without a
        // scroll gesture, then let the idle fade take over.
        let scrollable_now = self.vbar_active() || self.hbar_active();
        if scrollable_now && !self.was_scrollable && self.last_activity.is_none() {
            self.wake();
        }
        self.was_scrollable = scrollable_now;
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
                if self.vbar.is_dragging() {
                    if let Some(track) = self.vertical_track() {
                        if let Some(target) = self.vbar.drag_to(track, self.v_metrics(), cur) {
                            self.scroll_by(0.0, target as f32 - self.offset_y);
                        }
                    }
                    *handled = true;
                    return;
                }
                if self.hbar.is_dragging() {
                    if let Some(track) = self.horizontal_track() {
                        if let Some(target) = self.hbar.drag_to(track, self.h_metrics(), cur) {
                            self.scroll_by(target as f32 - self.offset_x, 0.0);
                        }
                    }
                    *handled = true;
                    return;
                }
                // Otherwise track which thumb (if any) is hovered so the
                // next draw can brighten it. Hover changes are visual-only
                // (no relayout) and still fall through so the child updates
                // its own hover.
                let mut changed = false;
                if let Some(track) = self.vertical_track() {
                    changed |= self.vbar.update_hover(track, self.v_metrics(), cur);
                } else {
                    changed |= self.vbar.set_hover(false);
                }
                if let Some(track) = self.horizontal_track() {
                    changed |= self.hbar.update_hover(track, self.h_metrics(), cur);
                } else {
                    changed |= self.hbar.set_hover(false);
                }
                // Overlay reveal: the pointer entering or leaving a bar's edge
                // zone pins it visible / restarts its fade. Only edge
                // transitions touch the timer, so ordinary content-area moves
                // never keep the bar awake.
                let near = self.cursor_near_bar(cur);
                if near != self.pointer_near_bar {
                    self.pointer_near_bar = near;
                    self.wake();
                    changed = true;
                }
                if changed {
                    self.bar_dirty = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let cur = self.last_cursor;
                // A press anywhere inside the scroll region claims keyboard
                // focus so PageUp/Down, Home/End and arrows scroll it. A child
                // that consumes the press (a button, a row) overwrites this via
                // its own `focus_target` after we forward below, so focus still
                // lands on the deepest interactive target.
                if self.rect.contains(cur) {
                    *focus_target = Some(node_id);
                }
                // Vertical bar: thumb press → drag; track press → page + hold.
                if let Some(track) = self.vertical_track() {
                    match self.vbar.press(track, self.v_metrics(), cur) {
                        PressOutcome::Thumb => {
                            self.scroll_dirty = true;
                            self.wake();
                            *handled = true;
                            return;
                        }
                        PressOutcome::Page(target) => {
                            let dir = (target - self.offset_y as f64).signum();
                            self.scroll_by(0.0, target as f32 - self.offset_y);
                            if dir != 0.0 {
                                self.hold = Some(TrackHold {
                                    vertical: true,
                                    dir,
                                    anchor: cur,
                                    next_at: Instant::now() + Duration::from_millis(HOLD_INITIAL_MS),
                                });
                            }
                            *handled = true;
                            return;
                        }
                        PressOutcome::Miss => {}
                    }
                }
                // Horizontal bar.
                if let Some(track) = self.horizontal_track() {
                    match self.hbar.press(track, self.h_metrics(), cur) {
                        PressOutcome::Thumb => {
                            self.scroll_dirty = true;
                            self.wake();
                            *handled = true;
                            return;
                        }
                        PressOutcome::Page(target) => {
                            let dir = (target - self.offset_x as f64).signum();
                            self.scroll_by(target as f32 - self.offset_x, 0.0);
                            if dir != 0.0 {
                                self.hold = Some(TrackHold {
                                    vertical: false,
                                    dir,
                                    anchor: cur,
                                    next_at: Instant::now() + Duration::from_millis(HOLD_INITIAL_MS),
                                });
                            }
                            *handled = true;
                            return;
                        }
                        PressOutcome::Miss => {}
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // End any track-hold auto-repeat and any thumb drag.
                let had_hold = self.hold.take().is_some();
                if self.vbar.release() || self.hbar.release() || had_hold {
                    self.scroll_dirty = true;
                    *handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.focused && ke.state == ElementState::Pressed =>
            {
                let page_v = self.viewport_h * PAGE_FRACTION;
                let page_h = self.viewport_w * PAGE_FRACTION;
                // A huge single step that `clamp` caps at the content edge —
                // used for Home/End on whichever axis is primary.
                let full = self.content_h.max(self.content_w);
                let has_v = self.axis_has_vertical();
                let moved = match &ke.logical_key {
                    Key::Named(NamedKey::PageDown) if has_v => { self.scroll_by(0.0, page_v); true }
                    Key::Named(NamedKey::PageUp) if has_v => { self.scroll_by(0.0, -page_v); true }
                    Key::Named(NamedKey::PageDown) => { self.scroll_by(page_h, 0.0); true }
                    Key::Named(NamedKey::PageUp) => { self.scroll_by(-page_h, 0.0); true }
                    Key::Named(NamedKey::Home) if has_v => { self.scroll_by(0.0, -full); true }
                    Key::Named(NamedKey::End) if has_v => { self.scroll_by(0.0, full); true }
                    Key::Named(NamedKey::Home) => { self.scroll_by(-full, 0.0); true }
                    Key::Named(NamedKey::End) => { self.scroll_by(full, 0.0); true }
                    Key::Named(NamedKey::ArrowDown) if has_v => { self.scroll_by(0.0, LINE_STEP); true }
                    Key::Named(NamedKey::ArrowUp) if has_v => { self.scroll_by(0.0, -LINE_STEP); true }
                    Key::Named(NamedKey::ArrowRight) if self.axis_has_horizontal() => { self.scroll_by(LINE_STEP, 0.0); true }
                    Key::Named(NamedKey::ArrowLeft) if self.axis_has_horizontal() => { self.scroll_by(-LINE_STEP, 0.0); true }
                    _ => false,
                };
                if moved {
                    *handled = true;
                    return;
                }
                // A non-nav key falls through to the child below.
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
            let (mut dx, mut dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (*x * -LINE_PX, *y * -LINE_PX),
                MouseScrollDelta::PixelDelta(pp) => (-pp.x as f32, -pp.y as f32),
            };
            // Velocity boost for discrete notches: a fast flick (notches close
            // together) travels further per notch than a slow, deliberate one.
            // Trackpad `PixelDelta` already encodes velocity in its magnitude,
            // so it's left un-boosted to keep precise two-finger scrolling 1:1.
            // Reduced motion opts out entirely — pure 1:1 notch stepping.
            if matches!(delta, MouseScrollDelta::LineDelta(..)) && self.smooth && !self.reduce_motion {
                let boost = self.flick_boost(Instant::now());
                dx *= boost;
                dy *= boost;
            }
            // Shift+wheel scrolls horizontally: remap the dominant vertical
            // delta onto X when the axis can scroll horizontally and the
            // wheel is (as on a plain mouse) vertical-only. Tilt wheels that
            // already report an X delta pass through unchanged.
            if m.state().shift_key() && self.axis_has_horizontal() && dx == 0.0 {
                dx = dy;
                dy = 0.0;
            }
            // Drive the *target*; the rendered offset eases toward it over the
            // next frames (instant under reduced motion / when smoothing is
            // off). "Did the wheel take?" is whether the target moved.
            let before = (self.target_x, self.target_y);
            self.smooth_scroll_by(dx, dy);
            // Only consume the wheel when it actually moved the view. Leaving an
            // at-the-edge / can't-scroll wheel unhandled lets it bubble to an
            // outer or sibling scroll region (e.g. the sidebar behind a content
            // pane that fits, or the parent once an inner list is maxed) —
            // standard nested-scroll chaining.
            if (self.target_x, self.target_y) != before {
                *handled = true;
            }
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

    /// A scroll container is keyboard-focusable exactly when it can actually
    /// scroll on an enabled axis (so non-overflowing wrappers stay out of the
    /// tab order). While focused it consumes PageUp/Down, Home/End and arrow
    /// keys to scroll; see `handle_event`.
    fn is_focusable(&self) -> bool {
        self.show_scrollbar && (self.vbar_active() || self.hbar_active())
    }
    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            // Surfacing/hiding focus flips the bar's pinned-visible state.
            self.wake();
        }
    }
    fn is_focused(&self) -> bool { self.focused }

    /// Propagate pointer-capture intent from the contained child so an
    /// outer container's event-routing short-circuit honours an active
    /// drag deeper in the tree. A live thumb drag or track-hold also holds
    /// capture so the whole gesture routes here even if the pointer strays
    /// off the bar.
    fn wants_pointer_capture(&self) -> bool {
        self.vbar.is_dragging()
            || self.hbar.is_dragging()
            || self.hold.is_some()
            || self.child.as_ref().is_some_and(|c| c.wants_pointer_capture())
    }

    /// Keep the host pumping frames while the overlay bar is fading or a
    /// track-hold is auto-repeating. Settles back to idle (returns false)
    /// once the fade completes and no hold is active, so an idle list never
    /// spins.
    fn needs_animation_frame(&self) -> bool {
        self.hold.is_some()
            || self.easing_active()
            || self.fade_in_progress()
            || self.child.as_ref().is_some_and(|c| c.needs_animation_frame())
    }

    /// Returns true after a `scroll_by` actually moved the
    /// offset, while the overlay bar's fade/hover is mid-change, OR while any
    /// descendant signals dirty — without the first clauses a wheel-only
    /// event (no child mutation) or a fade tick would silently fail to
    /// schedule the next frame and the scroll/bar would visually freeze.
    fn needs_redraw(&self) -> bool {
        self.scroll_dirty
            || self.bar_dirty
            || self.easing_active()
            || self.fade_in_progress()
            || self.child.as_ref().is_some_and(|c| c.needs_redraw())
    }

    /// Forwarded version of the above for the layout gate. The
    /// child's resolved screen rects depend on `self.offset_*`,
    /// so a scroll demands `apply_layout` even though no widget
    /// changed *size*.
    fn is_layout_dirty(&self) -> bool {
        // An in-flight ease repositions the child every frame, so layout must
        // re-run to push the moving offset down — even on a frame whose
        // `ease_tick` hasn't yet flipped `scroll_dirty`.
        self.scroll_dirty
            || self.easing_active()
            || self.child.as_ref().is_some_and(|c| c.is_layout_dirty())
    }

    /// A scroll only *repositions* the child (via the offset applied in
    /// `apply_layout`); the intrinsic taffy layout is identical, so
    /// `scroll_dirty` alone must NOT force a full `compute_layout`. Only a
    /// genuinely measure-dirty descendant does. This is what lets the App
    /// re-run `apply_layout` (cheap) without re-solving the whole content
    /// tree (expensive) on every scroll frame.
    fn layout_needs_measure(&self) -> bool {
        self.child.as_ref().is_some_and(|c| c.layout_needs_measure())
    }

    fn mark_drawn(&mut self) {
        self.scroll_dirty = false;
        self.bar_dirty = false;
        if let Some(c) = &mut self.child {
            c.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect { self.rect }

    /// A scroll container is a semantic region: it reports [`Role::Group`]
    /// (a generic scroll region) so it stays in the a11y tree as a container
    /// while its scrolled content flows through `children()`. It carries no
    /// name of its own — the meaningful label belongs to whatever it wraps.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Group))
    }

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
        assert!(s.vbar.is_dragging(), "press on thumb begins a drag");
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
        assert!(!s.vbar.is_dragging(), "release ends the drag");
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
        assert!(!s.vbar.is_dragging(), "track click does not start a drag");
        let paged = s.offset_y();
        assert!(paged > 0.0, "click below thumb pages down: {paged}");
        let page_frac = crate::widget::scrollbar::PAGE_FRACTION as f32;
        assert!(
            (paged - 300.0 * page_frac).abs() < 1.0,
            "page step should be ≈viewport*{page_frac}, got {paged}",
        );
    }

    #[test]
    fn accessibility_reports_group_role_and_no_name() {
        use crate::a11y::Role;
        let s = vertical(1200.0);
        let node = Widget::accessibility(&s).expect("scroll container is a region");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name, None, "a scroll container carries no name of its own");
        assert_eq!(node.value, None);
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
        // The wheel drives the eased target; let the ease run to completion.
        s.settle_scroll();
        assert!(s.offset_y() > before, "wheel down increases offset");
    }

    use crate::widget::{Column, Text};
    use std::collections::HashMap;
    use taffy::prelude::TaffyMaxContent;

    /// Lay out `root` inside a parent that supplies NO definite height
    /// (max-content) — the pathological host for a Scrollable. Returns the
    /// scrollable's resolved viewport height via `get_rect`.
    fn viewport_height_under_indefinite_parent(mut scroll: Scrollable) -> f64 {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let scroll_node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
        // An auto-height block parent => the child's percent(1.0) height has
        // nothing to resolve against (the classic zero-viewport trap).
        let parent = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Block,
                    size: Size {
                        width: Dimension::length(400.0),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[scroll_node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(parent, Size::max_content(), |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            })
            .unwrap();
        scroll.apply_layout(&taffy, scroll_node, Point::ZERO);
        scroll.get_rect().height()
    }

    fn tall_child() -> Element {
        let mut col = Column::new().with_intrinsic_height();
        for i in 0..40 {
            col = col.push(Box::new(Text::new(&format!("row {i}"))));
        }
        Box::new(col)
    }

    /// Drive a full layout under an indefinite parent, then probe whether
    /// the resulting Scrollable can actually scroll (i.e. its content
    /// exceeds its viewport so `scroll_by` moves the offset).
    fn can_scroll_under_indefinite_parent(mut scroll: Scrollable) -> (f64, bool) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let scroll_node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Block,
                    size: Size {
                        width: Dimension::length(400.0),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[scroll_node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(parent, Size::max_content(), |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            })
            .unwrap();
        scroll.apply_layout(&taffy, scroll_node, Point::ZERO);
        let vh = scroll.get_rect().height();
        scroll.scroll_by(0.0, 200.0);
        (vh, scroll.offset_y() > 0.0)
    }

    #[test]
    fn with_height_pins_viewport_under_indefinite_parent() {
        let h = viewport_height_under_indefinite_parent(
            Scrollable::new_vertical().with_child(tall_child()).with_height(300.0),
        );
        assert!(
            (h - 300.0).abs() < 0.5,
            "with_height must pin the viewport to 300 even under an \
             indefinite-height parent; got {h}"
        );
    }

    #[test]
    fn pinned_height_makes_pane_scrollable_under_indefinite_parent() {
        // The fix's payoff: a pinned viewport is SHORTER than the content,
        // so the pane can scroll.
        let (vh, scrolled) = can_scroll_under_indefinite_parent(
            Scrollable::new_vertical().with_child(tall_child()).with_height(300.0),
        );
        assert!((vh - 300.0).abs() < 0.5, "viewport pinned to 300, got {vh}");
        assert!(scrolled, "a pinned-viewport pane with tall content must scroll");
    }

    #[test]
    fn content_gutter_insets_the_child_so_the_bar_never_overlaps_it() {
        // With a reserved gutter, the child content lays out narrower than the
        // viewport by exactly the gutter, and the vertical scrollbar track sits
        // entirely to the right of the child — no overlap.
        use std::collections::HashMap;
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        const GUTTER: f32 = 14.0;
        let mut scroll = Scrollable::new_vertical()
            .with_child(tall_child())
            .with_height(300.0)
            .with_content_gutter(GUTTER);
        let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::length(400.0), height: Dimension::length(300.0) },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                parent,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        scroll.apply_layout(&taffy, node, Point::ZERO);

        let child = &scroll.children()[0];
        let child_right = child.get_rect().x1;
        let track = scroll.vertical_track().expect("tall content shows the bar");
        assert!(
            child_right <= track.x0 + 0.5,
            "content right edge {child_right} must not reach the bar track {} (gutter reserved)",
            track.x0,
        );
        assert!(
            (scroll.rect.x1 - child_right - GUTTER as f64).abs() < 1.0,
            "child is inset by exactly the gutter: viewport right {}, child right {child_right}",
            scroll.rect.x1,
        );
    }

    #[test]
    fn scroll_is_layout_dirty_but_needs_no_remeasure() {
        // The scroll fast path: after a wheel scroll the Scrollable IS
        // layout-dirty (children must be repositioned by `apply_layout`) but does
        // NOT need a taffy re-solve — `layout_needs_measure` stays false. The App
        // reads exactly this to skip `compute_layout` on scroll frames.
        use std::collections::HashMap;
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let mut scroll =
            Scrollable::new_vertical().with_child(tall_child()).with_height(300.0);
        let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(400.0),
                        height: Dimension::length(300.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                parent,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        scroll.apply_layout(&taffy, node, Point::ZERO);
        scroll.scroll_by(0.0, 40.0);
        assert!(scroll.is_layout_dirty(), "a scroll needs apply_layout to reposition");
        assert!(
            !scroll.layout_needs_measure(),
            "a laid-out subtree needs no taffy re-solve on a pure scroll",
        );
    }

    #[test]
    fn persisted_offset_survives_a_rebuild_into_a_fresh_scrollable() {
        // The file-explorer model: the whole widget subtree is thrown away and
        // rebuilt on any structural change. A shared offset cell must let the
        // fresh Scrollable resume exactly where the previous one was scrolled to
        // instead of snapping back to the top.
        let cell = Rc::new(Cell::new((0.0f32, 0.0f32)));

        // First instance: scroll it, and confirm it writes the live position
        // back into the shared cell (through `clamp`).
        let mut a = vertical(1200.0).with_persisted_offset(cell.clone());
        a.scroll_by(0.0, 250.0);
        assert_eq!(a.offset_y(), 250.0);
        assert_eq!(cell.get().1, 250.0, "scroll writes back into the shared cell");

        // A brand-new instance bound to the same cell seeds from it — this is
        // the rebuilt widget resuming the scroll.
        let b = Scrollable::new_vertical().with_persisted_offset(cell.clone());
        assert_eq!(b.offset_y(), 250.0, "rebuilt Scrollable resumes the saved offset");

        // Independent instances with no persisted cell still start at the top.
        let c = Scrollable::new_vertical();
        assert_eq!(c.offset_y(), 0.0, "an unbound Scrollable is unaffected");
    }

    #[test]
    fn persisted_offset_reclamps_and_rewrites_when_content_shrinks() {
        // If the content later shrinks below the saved offset (the directory got
        // shorter), the next clamp must both cap the offset AND rewrite the
        // smaller value back so the cell never strands a stale, un-reachable
        // position.
        let cell = Rc::new(Cell::new((0.0f32, 0.0f32)));
        let mut s = Scrollable::new_vertical().with_persisted_offset(cell.clone());
        // Establish a real scrolled state: tall content, then scroll to bottom.
        s.set_content_size(200.0, 1200.0);
        s.set_viewport_size(200.0, 300.0); // max scroll 900
        s.scroll_by(0.0, 900.0);
        assert_eq!(cell.get().1, 900.0, "scrolled to the bottom, cell tracks it");
        // The directory shrinks: content now only 400 tall → max scroll 100.
        s.set_content_size(200.0, 400.0);
        assert_eq!(s.offset_y(), 100.0, "offset clamped to the new max");
        assert_eq!(cell.get().1, 100.0, "cell rewritten with the clamped value");
    }

    // === New navigation: keyboard, hold-paging, both-axis, auto-hide ======

    /// A both-axis scrollable with a known viewport and overflowing content
    /// on both axes.
    fn both(content_w: f32, content_h: f32) -> Scrollable {
        let mut s = Scrollable::new(ScrollAxis::Both);
        s.rect = Rect::new(0.0, 0.0, 300.0, 300.0);
        s.set_viewport_size(300.0, 300.0);
        s.set_content_size(content_w, content_h);
        s
    }

    fn key(named: NamedKey) -> WindowEvent {
        // winit keeps `KeyEvent`'s tail fields non-public; the suite-wide
        // workaround (see data_table) is to zero-init and set what we read.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch_mods(s: &mut Scrollable, event: &WindowEvent, mods: &winit::event::Modifiers) -> bool {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(event, 1.0, mods, &mut doc, &root, &mut handled, &mut focus_target, node, &tree);
        handled
    }

    #[test]
    fn focusable_only_when_it_can_scroll() {
        // Overflowing content → focusable; fitting content → not, so a
        // non-scrolling wrapper never clutters the tab order.
        assert!(Widget::is_focusable(&vertical(1200.0)), "overflowing is focusable");
        assert!(!Widget::is_focusable(&vertical(150.0)), "fitting is not focusable");
        // Disabling the scrollbar removes it from the tab order too.
        let mut hidden = vertical(1200.0);
        hidden.show_scrollbar = false;
        assert!(!Widget::is_focusable(&hidden));
    }

    #[test]
    fn keyboard_pages_home_end_and_lines_when_focused() {
        let mut s = vertical(1200.0); // viewport 300, max_scroll 900
        s.set_focus(true);
        assert!(s.is_focused());

        // PageDown moves ~0.9 of a viewport.
        assert!(dispatch(&mut s, &key(NamedKey::PageDown)), "PageDown handled");
        let page = 300.0 * PAGE_FRACTION;
        assert!((s.offset_y() - page).abs() < 0.5, "PageDown ≈ 0.9 viewport, got {}", s.offset_y());

        // End jumps to the bottom (clamped to max_scroll).
        assert!(dispatch(&mut s, &key(NamedKey::End)), "End handled");
        assert!((s.offset_y() - 900.0).abs() < 1e-3, "End → bottom, got {}", s.offset_y());

        // PageUp from the bottom moves back up by a viewport-ful.
        assert!(dispatch(&mut s, &key(NamedKey::PageUp)), "PageUp handled");
        assert!((s.offset_y() - (900.0 - page)).abs() < 0.5, "PageUp from bottom, got {}", s.offset_y());

        // Home returns to the very top.
        assert!(dispatch(&mut s, &key(NamedKey::Home)), "Home handled");
        assert_eq!(s.offset_y(), 0.0, "Home → top");

        // ArrowDown nudges one line.
        assert!(dispatch(&mut s, &key(NamedKey::ArrowDown)), "ArrowDown handled");
        assert!((s.offset_y() - LINE_STEP).abs() < 1e-3, "ArrowDown = one line, got {}", s.offset_y());
    }

    #[test]
    fn keyboard_ignored_when_unfocused() {
        // The same keys must NOT scroll an unfocused container — otherwise
        // every scrollable in the tree would fight over the key.
        let mut s = vertical(1200.0);
        assert!(!s.is_focused());
        assert!(!dispatch(&mut s, &key(NamedKey::PageDown)), "unfocused key not handled");
        assert_eq!(s.offset_y(), 0.0, "unfocused container does not scroll");
    }

    #[test]
    fn shift_wheel_scrolls_horizontally() {
        use winit::keyboard::ModifiersState;
        let mut s = both(1200.0, 1200.0);
        let shift = winit::event::Modifiers::from(ModifiersState::SHIFT);
        let wheel = WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -1.0), // a plain vertical wheel
            phase: winit::event::TouchPhase::Moved,
        };
        assert!(dispatch_mods(&mut s, &wheel, &shift), "shift+wheel handled");
        s.settle_scroll();
        assert!(s.offset_x() > 0.0, "shift+wheel scrolls X, got x={}", s.offset_x());
        assert_eq!(s.offset_y(), 0.0, "shift+wheel leaves Y untouched");
    }

    #[test]
    fn tilt_wheel_scrolls_horizontally_without_shift() {
        let mut s = both(1200.0, 1200.0);
        let tilt = WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(-1.0, 0.0), // horizontal tilt
            phase: winit::event::TouchPhase::Moved,
        };
        assert!(dispatch(&mut s, &tilt), "tilt wheel handled");
        s.settle_scroll();
        assert!(s.offset_x() > 0.0, "tilt wheel scrolls X, got x={}", s.offset_x());
    }

    #[test]
    fn horizontal_thumb_drag_updates_offset_x() {
        let mut s = both(1200.0, 1200.0);
        let track = s.horizontal_track().expect("h-bar shown");
        let thumb = s.thumb_rect(ScrollAxis::Horizontal).expect("h-thumb");
        let mid_x = (thumb.x0 + thumb.x1) / 2.0;
        let bar_y = track.y0 + track.height() / 2.0;
        dispatch(&mut s, &cursor_moved(mid_x, bar_y));
        assert!(dispatch(&mut s, &mouse(ElementState::Pressed)), "press on h-thumb handled");
        assert!(s.hbar.is_dragging(), "h-thumb press begins a drag");
        dispatch(&mut s, &cursor_moved(mid_x + 80.0, bar_y));
        assert!(s.offset_x() > 0.0, "drag right increases offset_x: {}", s.offset_x());
        dispatch(&mut s, &mouse(ElementState::Released));
        assert!(!s.hbar.is_dragging(), "release ends the h-drag");
    }

    #[test]
    fn holding_track_pages_repeatedly_then_settles_at_click() {
        // Press the track below the thumb: it pages once immediately, arms a
        // hold, and each `advance_hold` tick pages again until the thumb
        // reaches the press point — then it stops rather than overshooting.
        let mut s = vertical(3000.0); // viewport 300, max_scroll 2700
        let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
        let thumb = s.vertical_thumb().unwrap();
        // Click deep down the track, well below the thumb.
        let click_y = thumb.y1 + (s.rect.y1 - thumb.y1) * 0.8;
        dispatch(&mut s, &cursor_moved(bar_x, click_y));
        assert!(dispatch(&mut s, &mouse(ElementState::Pressed)), "track press handled");
        assert!(s.hold.is_some(), "a track press below the thumb arms a hold");
        let after_first = s.offset_y();
        assert!(after_first > 0.0, "first page moved down: {after_first}");
        assert!(s.wants_pointer_capture(), "a live hold captures the pointer");

        // Auto-repeat: force several ticks by advancing the injected clock
        // past the repeat interval each time. Offset keeps climbing…
        let mut prev = after_first;
        let mut steps = 0;
        for _ in 0..40 {
            let due = s.hold.map(|h| h.next_at).unwrap();
            s.advance_hold(due + Duration::from_millis(1));
            if s.offset_y() > prev {
                steps += 1;
                prev = s.offset_y();
            } else {
                break; // thumb reached the click point → paging stopped
            }
        }
        assert!(steps >= 2, "the hold auto-repeats past the first page ({steps} extra steps)");

        // …and settles with the thumb straddling the click point, not past it.
        let thumb_now = s.vertical_thumb().unwrap();
        assert!(
            thumb_now.y1 >= click_y - BAR_THICKNESS && thumb_now.y0 <= click_y + 1.0,
            "paging stopped at the click, thumb=({},{}) click_y={click_y}",
            thumb_now.y0, thumb_now.y1,
        );

        // Release clears the hold.
        assert!(dispatch(&mut s, &mouse(ElementState::Released)), "release handled");
        assert!(s.hold.is_none(), "release ends the hold");
    }

    #[test]
    fn overlay_bar_fades_in_on_activity_and_out_when_idle() {
        // Default overlay bar: hidden at rest, full opacity right after a
        // scroll, then it holds, ramps down, and fully hides once idle.
        let mut s = vertical(1200.0);
        s.last_activity = None; // as freshly constructed
        s.pointer_near_bar = false;
        assert_eq!(s.bar_opacity_at(Instant::now(), false), 0.0, "idle overlay is hidden");

        s.wake();
        let t0 = s.last_activity.unwrap();
        assert_eq!(s.bar_opacity_at(t0, false), 1.0, "full opacity right after activity");
        assert_eq!(
            s.bar_opacity_at(t0 + Duration::from_millis(VISIBLE_HOLD_MS as u64 - 10), false),
            1.0,
            "still fully visible during the hold",
        );
        let mid = s.bar_opacity_at(
            t0 + Duration::from_millis((VISIBLE_HOLD_MS + FADE_MS / 2) as u64),
            false,
        );
        assert!(mid > 0.0 && mid < 1.0, "mid-fade opacity is partial: {mid}");
        assert_eq!(
            s.bar_opacity_at(
                t0 + Duration::from_millis((VISIBLE_HOLD_MS + FADE_MS + 100) as u64),
                false,
            ),
            0.0,
            "fully faded out once idle",
        );
    }

    #[test]
    fn overlay_fade_pins_visible_when_pinned_or_reduced_motion_or_persistent() {
        let mut s = vertical(1200.0);
        s.wake();
        let long_after = s.last_activity.unwrap() + Duration::from_secs(30);
        // Would be faded out by now…
        assert_eq!(s.bar_opacity_at(long_after, false), 0.0);
        // …but focus pins it visible.
        s.focused = true;
        assert_eq!(s.bar_opacity_at(long_after, false), 1.0, "focus pins the bar visible");
        s.focused = false;
        // Reduced motion keeps it fully visible (no fade animation to freeze).
        assert_eq!(s.bar_opacity_at(long_after, true), 1.0, "reduced motion pins visible");
        // A persistent (non-overlay) bar is always fully opaque.
        let mut persistent = vertical(1200.0).always_visible();
        persistent.last_activity = None;
        assert_eq!(persistent.bar_opacity_at(Instant::now(), false), 1.0, "persistent bar always visible");
    }

    #[test]
    fn needs_animation_frame_only_while_fading_or_holding() {
        // An idle (never-woken) overlay list must NOT pump frames.
        let s = vertical(1200.0);
        assert!(!Widget::needs_animation_frame(&s), "idle list does not animate");
        // A persistent bar is static too.
        let p = vertical(1200.0).always_visible();
        assert!(!Widget::needs_animation_frame(&p), "persistent bar is static");
        // Mid-fade → pumps.
        let mut fading = vertical(1200.0);
        fading.wake();
        assert!(Widget::needs_animation_frame(&fading), "a fading bar pumps frames");
    }

    #[test]
    fn default_scrollable_cannot_scroll_under_indefinite_parent() {
        // Documents the trap the new builders rescue: with the legacy
        // percent(1.0) default and no definite parent height, the
        // viewport expands to the full content extent (via apply_layout's
        // content-size walk), so viewport == content and `scroll_by` has
        // nowhere to go. The pane silently can't scroll. Pinning the
        // height (above) is the fix.
        let (_vh, scrolled) = can_scroll_under_indefinite_parent(
            Scrollable::new_vertical().with_child(tall_child()),
        );
        assert!(
            !scrolled,
            "legacy default is expected to be unscrollable under an \
             indefinite parent; the fix is opt-in via with_height / \
             fill_parent_height"
        );
    }

    // === Smooth (momentum) wheel scrolling ================================

    fn wheel_down() -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    #[test]
    fn wheel_drives_target_and_offset_eases_toward_it() {
        // A wheel notch moves the *target* immediately but the rendered offset
        // starts at rest and interpolates toward it over successive frames —
        // this is the smoothness the primitive gives every list for free.
        let mut s = vertical(1200.0); // viewport 300, max_scroll 900
        assert!(dispatch(&mut s, &wheel_down()), "wheel handled");

        let (_, target) = s.scroll_target();
        assert!(target > 0.0, "wheel pushes the target forward: {target}");
        assert_eq!(s.offset_y(), 0.0, "rendered offset has not jumped — it eases");
        assert!(s.easing_active(), "an ease is now in flight");

        // One frame: the offset moves partway, never past the target (proof of
        // interpolation rather than a snap).
        let t0 = Instant::now();
        s.ease_tick(t0);
        let after_one = s.offset_y();
        assert!(
            after_one > 0.0 && after_one < target,
            "one frame eases partway: 0 < {after_one} < {target}",
        );

        // Run the ease out: the offset converges onto the target and the ease
        // reports settled.
        s.settle_scroll();
        assert!(
            (s.offset_y() - target).abs() <= SCROLL_SETTLE_EPS,
            "eased offset converges to target: offset={} target={target}",
            s.offset_y(),
        );
        assert!(!s.easing_active(), "ease settles once the offset reaches target");
    }

    #[test]
    fn easing_pumps_animation_frames_until_settled() {
        // The host keeps pumping frames while the ease is live (so it renders
        // at the display refresh rate) and drops back to idle once settled.
        let mut s = vertical(1200.0);
        dispatch(&mut s, &wheel_down());
        assert!(s.easing_active(), "ease in flight after the wheel");
        assert!(
            Widget::needs_animation_frame(&s),
            "an in-flight ease requests the next frame",
        );
        assert!(Widget::is_layout_dirty(&s), "the moving offset re-runs layout");

        s.settle_scroll();
        assert!(!s.easing_active(), "ease no longer pumps once settled");
    }

    #[test]
    fn reduced_motion_makes_the_wheel_instant() {
        // With reduced motion the wheel applies 1:1 with no ease — offset and
        // target land together on the same frame, and nothing keeps animating.
        let mut s = vertical(1200.0);
        s.reduce_motion = true;
        dispatch(&mut s, &wheel_down());

        assert!(s.offset_y() > 0.0, "reduced-motion wheel still scrolls");
        assert_eq!(
            s.offset_y(),
            s.scroll_target().1,
            "reduced motion: offset == target immediately (no glide)",
        );
        assert!(!s.easing_active(), "reduced motion disables the ease/momentum");
    }

    #[test]
    fn smooth_scroll_opt_out_is_instant() {
        // `with_smooth_scroll(false)` opts a viewport back into raw 1:1 wheel
        // stepping — the offset moves on the event, no ease pending.
        let mut s = vertical(1200.0).with_smooth_scroll(false);
        dispatch(&mut s, &wheel_down());
        assert!(s.offset_y() > 0.0, "opt-out wheel scrolls instantly");
        assert_eq!(s.offset_y(), s.scroll_target().1, "offset == target with smoothing off");
        assert!(!s.easing_active());
    }

    #[test]
    fn flick_boost_scales_with_notch_spacing() {
        // The first notch is unboosted; a back-to-back notch is boosted well
        // above 1×; a notch after a long pause drops back to 1×.
        let mut s = vertical(1200.0);
        let t = Instant::now();
        assert!((s.flick_boost(t) - 1.0).abs() < 1e-6, "first notch has no prior → 1×");
        let fast = s.flick_boost(t + Duration::from_millis(10));
        assert!(fast > 1.5, "a rapid follow-up notch is boosted: {fast}");
        assert!(fast <= FLICK_MAX_BOOST + 1e-6, "boost is capped: {fast}");
        let slow = s.flick_boost(t + Duration::from_millis(10) + Duration::from_millis(500));
        assert!((slow - 1.0).abs() < 1e-6, "a notch after a pause is unboosted: {slow}");
    }

    #[test]
    fn fast_flick_travels_further_than_a_slow_one() {
        // Two notches back-to-back reach further than the same two notches
        // spaced out — "fast flick scrolls further", driven through the same
        // boost the wheel path applies.
        let t = Instant::now();

        let mut slow = vertical(3000.0); // max_scroll 2700, plenty of room
        let s1 = slow.flick_boost(t);
        slow.smooth_scroll_by(0.0, 60.0 * s1);
        let s2 = slow.flick_boost(t + Duration::from_millis(400)); // gap > window → 1×
        slow.smooth_scroll_by(0.0, 60.0 * s2);

        let mut fast = vertical(3000.0);
        let f1 = fast.flick_boost(t);
        fast.smooth_scroll_by(0.0, 60.0 * f1);
        let f2 = fast.flick_boost(t + Duration::from_millis(8)); // rapid → boosted
        fast.smooth_scroll_by(0.0, 60.0 * f2);

        assert!(
            fast.scroll_target().1 > slow.scroll_target().1,
            "a fast flick reaches further: fast={} slow={}",
            fast.scroll_target().1,
            slow.scroll_target().1,
        );
    }

    #[test]
    fn wheel_scroll_wakes_overlay_then_it_fades() {
        // The overlay progress bar shows *while* scrolling and hides shortly
        // after — asserted through the real wheel path (wheel → wake), not by
        // poking the timer directly.
        let mut s = vertical(1200.0);
        s.last_activity = None;
        assert_eq!(s.bar_opacity_at(Instant::now(), false), 0.0, "hidden at rest");

        dispatch(&mut s, &wheel_down());
        let woke = s.last_activity.expect("a wheel scroll wakes the overlay bar");
        assert_eq!(s.bar_opacity_at(woke, false), 1.0, "overlay visible while scrolling");
        assert_eq!(
            s.bar_opacity_at(
                woke + Duration::from_millis((VISIBLE_HOLD_MS + FADE_MS + 100) as u64),
                false,
            ),
            0.0,
            "overlay fades out after scrolling stops",
        );
    }

    #[test]
    fn instant_paths_keep_target_pinned_to_offset() {
        // Thumb drag / keyboard / programmatic seeks must not leave a dangling
        // ease target — otherwise the view would glide away after the gesture.
        let mut s = vertical(1200.0);
        s.scroll_by(0.0, 120.0);
        assert_eq!(s.offset_y(), 120.0);
        assert_eq!(s.scroll_target(), (0.0, 120.0), "scroll_by keeps target == offset");
        assert!(!s.easing_active(), "an instant scroll leaves no ease pending");

        s.set_offset_y(300.0);
        assert_eq!(s.scroll_target(), (0.0, 300.0), "set_offset_y keeps target == offset");
        assert!(!s.easing_active());
    }
}
