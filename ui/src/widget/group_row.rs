//! `GroupRow` — a horizontal flex container of `WindowGroup`s that also
//! routes cross-group tab drag-drop: it intercepts release events after
//! every child `TabBar` has drained its own drag, and if one published a
//! `pending_detach`, splices the tab + `TileHost` into a matching sibling.
//! Within-group reorder still lives in `TabBar` / `WindowGroup`.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use taffy::{Dimension, Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::TileContent;
use crate::widget::{
    DEFAULT_STRIP_USABLE_PX, HANDLE_SLOT_PX, ResizeHandle, TabDescriptor, TileHost, Widget,
    WindowGroup, WindowWidth,
};

/// Idle time before the strip snaps to the nearest group's left edge.
const SNAP_IDLE_MS: u32 = 200;
/// Snap-animation duration (cubic-out).
const SNAP_ANIM_MS: u32 = 200;
/// "Make space" slide duration for groups a new insertion pushes along —
/// matches `window_group.rs`'s spawn fade (420ms cubic-out), which runs
/// at the same moment, so the two read as one event.
const MAKE_SPACE_ANIM_MS: u32 = 420;

/// Touchpad px per wheel detent for the discrete jump gestures; also the
/// scale for hi-res wheel notches reported as pixel deltas.
pub const JUMP_PX_PER_DETENT: f64 = 50.0;

/// Turns fractional-detent scroll deltas into whole steps. No quiet-time
/// decay: it would stop slow scrolling from ever accumulating a whole
/// detent, and a stale sub-detent balance is harmless since it clears the
/// next time the user reverses direction.
#[derive(Default)]
pub struct WheelJumpAccum {
    accum: f64,
}

impl WheelJumpAccum {
    pub fn push(&mut self, detents: f64, _now: Instant) -> i32 {
        if detents == 0.0 {
            return 0;
        }
        if self.accum != 0.0 && self.accum.signum() != detents.signum() {
            self.accum = 0.0;
        }
        self.accum += detents;
        let whole = self.accum.trunc();
        self.accum -= whole;
        // Real scroll deltas never approach i32 range; clamping first
        // makes the narrowing cast below a lossless, saturating no-op.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped into i32 range on the line above, so this cast cannot truncate"
        )]
        {
            whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        }
    }
}

/// Scroll delta → fractional jump detents. Winit's y is flipped (positive
/// = up), so wheel-DOWN yields a positive detent = jump right/next,
/// matching Niri's `Mod+Shift+WheelScrollDown → focus-column-right`.
pub fn wheel_jump_detents(delta: &MouseScrollDelta) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => f64::from(-x - y),
        MouseScrollDelta::PixelDelta(pp) => (-pp.x - pp.y) / JUMP_PX_PER_DETENT,
    }
}

/// In-flight "make space" motion: `offsets` is how far *left* of its new
/// home each group started (by group id), so eased paint is
/// `new_x + offset * (1 - t)`.
#[derive(Clone, Debug)]
struct ShiftAnim {
    started: Instant,
    offsets: Vec<(u64, f64)>,
}

/// In-flight snap animation; cleared when the curve settles or the user
/// starts wheeling again.
#[derive(Clone, Copy, Debug)]
struct SnapAnim {
    from: f64,
    to: f64,
    started: Instant,
}

/// Press displacement (px) before a tab-bar-background press becomes a
/// whole-group reorder drag instead of a focus click.
const GROUP_DRAG_THRESHOLD: f64 = 6.0;

/// In-flight whole-group reorder drag, set on a tab-bar-background press
/// (see [`WindowGroup::bar_grab_hit`]) and cleared on release. `moved`
/// latches once the pointer passes [`GROUP_DRAG_THRESHOLD`].
#[derive(Clone, Copy, Debug)]
struct GroupDrag {
    src_idx: usize,
    group_id: u64,
    start: Point,
    moved: bool,
}

/// Payload published when a tab drag ends outside the row's own band; the
/// shell hit-tests the release point against other strips and delivers it
/// via [`GroupRow::accept_cross_strip_drop`] or returns it to source.
/// The source group is already mutated (detached) by the time this is
/// published, matching what [`WindowGroup::detach_tab`] returns.
pub struct CrossStripDetach {
    pub tab_id: u64,
    pub desc: TabDescriptor,
    pub host: TileHost,
    pub release_point: Point,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag (structure_dirty, scroll_dirty, hidden, shift_armed) is an independent, \
              per-frame invalidation signal read and cleared on its own schedule, not a \
              combinatorial config API — folding them into an enum or bitflags would not make \
              any call site clearer"
)]
pub struct GroupRow {
    pub groups: Vec<WindowGroup>,
    /// One resize handle per gap: `max(0, groups.len() - 1)`.
    handles: Vec<ResizeHandle>,
    handle_node_ids: Vec<NodeId>,
    rect: Rect,
    node_ids: Vec<NodeId>,
    /// Last cursor pos; lets a release that landed on a sibling's bar
    /// still resolve to a release point for the detach sweep.
    last_cursor: Point,
    /// Set on structural mutation; App's tick handler polls & rebuilds taffy.
    structure_dirty: bool,
    /// Gap index the cursor hovers during a child drag; `draw` paints
    /// "drop here" here. Cleared on release or when no child is dragging.
    hover_gap_idx: Option<usize>,
    /// Horizontal scroll offset (px): each child renders at `-scroll_x`.
    /// Clamped to `[0, max(0, content_w - viewport_w)]` after each layout.
    scroll_x: f64,
    /// Total content width incl. gaps; cached so the wheel handler can
    /// clamp `scroll_x` without re-summing children.
    content_w: f64,
    /// Per-group local x from the last layout — the strip's snap targets.
    group_left_edges: Vec<f64>,
    /// Timestamp of the last wheel event; `tick_snap` waits `SNAP_IDLE_MS`
    /// of quiet here before starting the snap animation.
    last_wheel_at: Option<Instant>,
    snap_anim: Option<SnapAnim>,
    /// Set by the post-dispatch detach sweep when a release lands outside
    /// this row's band; drained via [`Self::take_pending_cross_strip_detach`].
    pending_cross_strip_detach: Option<CrossStripDetach>,
    /// Set whenever `scroll_x` changes since the last `draw`; without it
    /// the parent's cached scene would keep painting stale positions on a
    /// wheel-pan.
    scroll_dirty: bool,
    /// Canvas-x gutter before the first group (set to the wheel-disc
    /// width so it pans with the strip). `apply_layout` offsets every
    /// child's screen-space origin by `rect.x0 + leading_inset - scroll_x`.
    leading_inset: f64,
    /// Each group's `(id, left, right)` canvas-x extent as last laid out.
    /// Measured off taffy rather than re-derived from stored widths: a
    /// group taffy shrank to fit is that width on screen regardless of
    /// its stored fraction.
    laid_spans: Vec<(u64, f64, f64)>,
    /// Transient fullscreen override (not persisted), set by the shell
    /// each frame from compositor state. Collapses the row to this one
    /// group; if it belongs to another strip, [`Self::hidden`] is set
    /// instead.
    fullscreen_group: Option<u64>,
    /// Transient (not persisted): true on every strip that doesn't own
    /// the current fullscreen group; collapses the row via `Display::None`.
    hidden: bool,
    /// In-flight whole-group reorder drag (distinct from a tab-detach
    /// drag inside a child `TabBar`, and from an inter-group resize drag).
    group_drag: Option<GroupDrag>,
    /// Insertion gap index targeted by an in-progress reorder drag;
    /// `draw` paints the indicator here. Cleared on release.
    group_reorder_target: Option<usize>,
    /// One-shot `(group_id, new_index)` on reorder commit; drained via
    /// [`Self::take_pending_group_reorder`] into the persisted `WorkspaceTree`.
    pending_group_reorder: Option<(u64, usize)>,
    /// Displays this strip spans; each group resolves its [`WindowWidth`]
    /// fraction against the span it's laid out on, not the combined
    /// desktop — otherwise a full-width window could span two monitors.
    /// Empty in a bare `ui` host, see [`Self::usable_at`].
    output_spans: Vec<OutputSpan>,
    /// Each group's taffy-local x from the last layout, keyed by id — the
    /// make-space slide's "where it used to be".
    last_local_x: HashMap<u64, f64>,
    /// Set by [`Self::insert_group`] when insertion pushes groups along;
    /// the next layout turns prior positions into travel offsets and
    /// clears this.
    shift_armed: bool,
    shift_anim: Option<ShiftAnim>,
    /// Test-only clock override for [`Self::shift_anim_t`] (capture
    /// harness); always `None` in a running shell.
    shift_hold: Option<Duration>,
}

/// One display's horizontal extent in the compositor's combined
/// coordinate space, plus the width window groups on it size themselves
/// against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpan {
    /// Left edge of the display in combined coordinates.
    pub x: f64,
    /// Full width of the display.
    pub width: f64,
    /// Width available to window groups on this display: `width` minus
    /// whatever shell chrome it reserves (on the leftmost display, the
    /// wheel-disc slot). The denominator every [`WindowWidth`] fraction
    /// is taken of.
    pub usable: f64,
}

impl GroupRow {
    /// Paint selected active app surfaces without the row's viewport clip.
    ///
    /// The ordinary desktop pass clips the continuous strip to the physical
    /// output. Capture cannot reuse that pass for `FullApp`/`WholeStrip`: an
    /// app may be partly or wholly beyond the monitor while still having live
    /// layout and pixels. This method deliberately calls the owning group
    /// directly, preserving its real app draw path and geometry while omitting
    /// only the output clip.
    #[allow(
        clippy::too_many_arguments,
        reason = "capture painting mirrors the Widget draw context plus target selection"
    )]
    pub fn draw_capture_targets(
        &mut self,
        target_ids: &[String],
        extent: crate::widget::CaptureExtent,
        complete_semantic_content: bool,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Vec<Rect> {
        let mut rects = Vec::new();
        for group in &mut self.groups {
            let active = group.active_tab_id();
            let id = format!("group:{}:tab:{}", group.id, active);
            if !target_ids.iter().any(|wanted| wanted == &id) {
                continue;
            }
            let Some(_) = group.active_host() else {
                continue;
            };
            if extent == crate::widget::CaptureExtent::FullApp
                && complete_semantic_content
                && let Some(rect) =
                    group.draw_complete_capture(scene, transform, text_renderer, theme, presence)
            {
                rects.push(rect);
                continue;
            }
            // `group.draw` includes the tab/window decoration, so its capture
            // crop must use that same complete rect. Cropping to the active
            // TileHost made “App” silently omit its own chrome.
            let rect = group.get_rect();
            if rect.area() <= 0.0 {
                continue;
            }
            group.draw(scene, transform, text_renderer, theme, presence);
            rects.push(rect);
        }
        rects
    }

    pub fn new(groups: Vec<WindowGroup>) -> Self {
        Self {
            groups,
            handles: Vec::new(),
            handle_node_ids: Vec::new(),
            rect: Rect::ZERO,
            node_ids: Vec::new(),
            last_cursor: Point::ZERO,
            structure_dirty: false,
            hover_gap_idx: None,
            scroll_x: 0.0,
            content_w: 0.0,
            group_left_edges: Vec::new(),
            last_wheel_at: None,
            snap_anim: None,
            pending_cross_strip_detach: None,
            scroll_dirty: false,
            leading_inset: 0.0,
            laid_spans: Vec::new(),
            fullscreen_group: None,
            hidden: false,
            group_drag: None,
            group_reorder_target: None,
            pending_group_reorder: None,
            output_spans: Vec::new(),
            last_local_x: HashMap::new(),
            shift_armed: false,
            shift_anim: None,
            shift_hold: None,
        }
    }

    /// Group to promote fullscreen, if `fullscreen_group` belongs to this
    /// row. Takes precedence over an in-row `maximized` flag.
    fn fullscreen_idx(&self) -> Option<usize> {
        let id = self.fullscreen_group?;
        self.groups.iter().position(|g| g.id == id)
    }

    /// Set (or clear) the transient fullscreen override, driven by the
    /// shell each frame from compositor state. `(None, false)` restores
    /// normal tiling exactly.
    pub fn set_fullscreen(&mut self, fullscreen_group: Option<u64>, hidden: bool) {
        if self.fullscreen_group != fullscreen_group || self.hidden != hidden {
            self.fullscreen_group = fullscreen_group;
            self.hidden = hidden;
            self.structure_dirty = true;
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen_idx().is_some()
    }

    /// Reserve a canvas-x gutter before the first group so it pans with
    /// `scroll_x` too (set to the wheel-disc width).
    #[must_use]
    pub fn with_leading_inset(mut self, px: f64) -> Self {
        self.leading_inset = px;
        self
    }

    pub fn leading_inset(&self) -> f64 {
        self.leading_inset
    }

    pub fn set_leading_inset(&mut self, px: f64) {
        if (self.leading_inset - px).abs() > f64::EPSILON {
            self.leading_inset = px;
            self.scroll_dirty = true;
        }
    }

    /// Passing an empty slice restores the fallback in [`Self::usable_at`].
    pub fn set_output_spans(&mut self, spans: &[OutputSpan]) {
        if self.output_spans != spans {
            self.output_spans = spans.to_vec();
            self.structure_dirty = true;
        }
    }

    pub fn output_spans(&self) -> &[OutputSpan] {
        &self.output_spans
    }

    /// Usable width a group at canvas-x `x` should resolve its
    /// [`WindowWidth`] fraction against. `x` is canvas, not screen: panning
    /// must not resize windows, so a group keeps the basis of the display
    /// it's laid out on regardless of scroll. Falls back to the row's own
    /// width minus the leading gutter, then [`DEFAULT_STRIP_USABLE_PX`]
    /// before any layout has run.
    pub fn usable_at(&self, x: f64) -> f64 {
        if let Some(&first) = self.output_spans.first() {
            // Last span whose left edge is at or before `x`; the first
            // span for anything left of the whole arrangement.
            let mut chosen = first;
            for span in &self.output_spans {
                if x >= span.x {
                    chosen = *span;
                }
            }
            return chosen.usable.max(1.0);
        }
        let own = self.rect.width() - self.leading_inset;
        if own > 1.0 {
            own
        } else {
            DEFAULT_STRIP_USABLE_PX
        }
    }

    /// Per-group usable width, in strip order. Depends only on the groups'
    /// own stored widths and the pushed output spans, never on a previous
    /// frame's layout, so it's stable on the very first frame.
    pub fn usable_per_group(&self) -> Vec<f64> {
        let mut canvas_x = self.rect.x0 + self.leading_inset;
        self.groups
            .iter()
            .map(|g| {
                let usable = self.usable_at(canvas_x);
                canvas_x += g.width().resolve(usable) + HANDLE_SLOT_PX;
                usable
            })
            .collect()
    }

    /// Each group's `(id, left, right)` extent in unscrolled canvas space.
    /// Derived the same way as [`Self::usable_per_group`], so a group
    /// spliced in this frame already has a span without waiting for taffy.
    pub fn group_canvas_spans(&self) -> Vec<(u64, f64, f64)> {
        let mut canvas_x = self.rect.x0 + self.leading_inset;
        self.groups
            .iter()
            .map(|g| {
                let usable = self.usable_at(canvas_x);
                let width = g.width().resolve(usable);
                let span = (g.id, canvas_x, canvas_x + width);
                canvas_x += width + HANDLE_SLOT_PX;
                span
            })
            .collect()
    }

    /// Ids of groups currently visible. Reads laid-out rects, not canvas
    /// spans: "can I see it" is a question about pixels, so a group with
    /// no rect yet just reads as not-on-screen for that one frame.
    pub fn onscreen_group_ids(&self) -> Vec<u64> {
        self.groups
            .iter()
            .filter(|g| {
                let r = g.get_rect();
                r.width() > 0.0 && r.x1 > self.rect.x0 && r.x0 < self.rect.x1
            })
            .map(|g| g.id)
            .collect()
    }

    /// `rect` shifted by the pan, in the same space as
    /// [`Self::group_canvas_spans`].
    pub fn visible_canvas_span(&self) -> (f64, f64) {
        (self.rect.x0 + self.scroll_x, self.rect.x1 + self.scroll_x)
    }

    /// Used by the shell's `resync_strips_from_tree` to diff against the
    /// persisted [`shell::state::Strip::window_groups`], preserving
    /// existing groups' host state across an unrelated revision bump.
    pub fn group_ids(&self) -> Vec<u64> {
        self.groups.iter().map(|g| g.id).collect()
    }

    /// Inserting anywhere but the end pushes later groups along; the
    /// motion is armed here and measured by the next layout pass, see
    /// [`Self::shift_offset_for`].
    pub fn insert_group(&mut self, group: WindowGroup, idx: usize) {
        let clamped = idx.min(self.groups.len());
        if clamped < self.groups.len() {
            self.shift_armed = true;
        }
        self.groups.insert(clamped, group);
        self.structure_dirty = true;
    }

    pub fn shift_anim_t(&self) -> f64 {
        let Some(anim) = &self.shift_anim else {
            return 1.0;
        };
        let elapsed = self.shift_hold.unwrap_or_else(|| anim.started.elapsed());
        if elapsed >= Duration::from_millis(u64::from(MAKE_SPACE_ANIM_MS)) {
            return 1.0;
        }
        let raw =
            (elapsed.as_secs_f64() / (f64::from(MAKE_SPACE_ANIM_MS) / 1000.0)).clamp(0.0, 1.0);
        1.0 - (1.0 - raw).powi(3)
    }

    /// How far left of its laid-out position `group_id` should be
    /// *painted* this frame. The stored rect is always the final one, so
    /// hit-testing and the app's configure size are never mid-flight —
    /// same choice the per-group spawn fade makes.
    pub fn shift_offset_for(&self, group_id: u64) -> f64 {
        let Some(anim) = &self.shift_anim else {
            return 0.0;
        };
        let Some((_, from)) = anim.offsets.iter().find(|(id, _)| *id == group_id) else {
            return 0.0;
        };
        from * (1.0 - self.shift_anim_t())
    }

    /// True while any group is still travelling.
    fn shift_in_flight(&self) -> bool {
        self.shift_anim.is_some() && self.shift_anim_t() < 1.0
    }

    /// Freeze the in-flight slide at `elapsed`, reporting whether there
    /// was one to freeze. The curve runs on wall time, but an offscreen
    /// render costs far more wall time per frame than the curve lasts, so
    /// a capture harness stepping frames would only see start and end —
    /// this lets it request a specific point (still production `draw`,
    /// not a reconstruction). Not gated on `debug_assertions`: `shots`
    /// walks scenario timelines through this and ships in release.
    pub fn hold_shift_at(&mut self, elapsed: Duration) -> bool {
        if self.shift_anim.is_none() {
            return false;
        }
        self.shift_hold = Some(elapsed);
        true
    }

    pub fn remove_group_by_id(&mut self, id: u64) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        let removed = self.groups.len() != before;
        if removed {
            self.structure_dirty = true;
        }
        removed
    }

    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }

    pub fn handle_at(&self, idx: usize) -> Option<&ResizeHandle> {
        self.handles.get(idx)
    }

    /// `debug_assertions` gated so it's available to `ui/tests` but not
    /// shipped in release.
    #[cfg(any(test, debug_assertions))]
    pub fn group_at_for_test(&self, idx: usize) -> Option<&WindowGroup> {
        self.groups.get(idx)
    }

    /// `0.0` before the first layout pass.
    pub fn content_width(&self) -> f64 {
        self.content_w
    }

    pub fn max_scroll_x(&self) -> f64 {
        (self.content_w - self.rect.width()).max(0.0)
    }

    /// A group focused the same frame it was spliced in hasn't been
    /// through a layout pass yet, so its rect is degenerate and any pan
    /// on it is a no-op.
    pub fn group_is_laid_out(&self, group_id: u64) -> bool {
        self.groups
            .iter()
            .find(|g| g.id == group_id)
            .is_some_and(|g| g.get_rect().width() > 0.0)
    }

    /// Pan `scroll_x` so `group_id` is fully inside the viewport (Niri's
    /// "focused column follows into view"). Must be called after a layout
    /// pass (uses the group's live rect). Cancels any in-flight wheel-snap
    /// so this isn't immediately overridden by a snap-back.
    pub fn scroll_group_into_view(&mut self, group_id: u64) -> bool {
        let Some(sx) = self.scroll_target_for(group_id) else {
            return false;
        };
        self.scroll_x = sx;
        self.scroll_dirty = true;
        self.snap_anim = None;
        self.last_wheel_at = None;
        true
    }

    /// Same target as [`Self::scroll_group_into_view`], eased on the
    /// wheel-snap curve instead of teleporting, so a focus jump reads as
    /// motion rather than a cut.
    pub fn scroll_group_into_view_animated(&mut self, group_id: u64, now: Instant) -> bool {
        let Some(sx) = self.scroll_target_for(group_id) else {
            return false;
        };
        self.snap_anim = Some(SnapAnim {
            from: self.scroll_x,
            to: sx,
            started: now,
        });
        self.last_wheel_at = None;
        self.scroll_dirty = true;
        true
    }

    fn scroll_target_for(&self, group_id: u64) -> Option<f64> {
        let g = self.groups.iter().find(|g| g.id == group_id)?;
        let gr = g.get_rect();
        // Degenerate rect → layout hasn't run for this group yet.
        if gr.width() <= 0.0 {
            return None;
        }
        // Groups must clear the leading wheel-disc gutter on the left and
        // the row's right edge on the right.
        let left_bound = self.rect.x0 + self.leading_inset;
        let right_bound = self.rect.x1;
        let mut sx = self.scroll_x;
        // Reveal the right edge first, then the left — so for a group wider
        // than the clear viewport the left (start) edge wins and the user
        // sees the group's beginning.
        if gr.x1 > right_bound {
            sx += gr.x1 - right_bound;
        }
        if gr.x0 < left_bound {
            sx -= left_bound - gr.x0;
        }
        let viewport = self.rect.width().max(1.0);
        let upper = self.max_scroll_x().max(viewport * 3.0);
        sx = sx.clamp(0.0, upper);
        ((sx - self.scroll_x).abs() > f64::EPSILON).then_some(sx)
    }

    /// Whether one group is severed by the row's left edge while a
    /// *different* one is severed by its right edge — never allowed in
    /// the same frame, though cutting only one is fine. A group wider
    /// than the row is excluded: it's cut on both sides at every offset,
    /// so counting it would make the strip yank around a window it can
    /// never frame.
    pub fn cut_on_both_edges(&self) -> bool {
        self.cut_on_both_edges_at(&self.laid_spans, self.scroll_x)
    }

    /// Ids of groups the row's edges are cutting through right now.
    /// Empty when everything fits; otherwise the ones hanging off the
    /// ends, which is normal.
    pub fn groups_cut_by_row_edges(&self) -> Vec<u64> {
        let row_w = self.rect.width().max(1.0);
        let sx = self.scroll_x;
        self.laid_spans
            .iter()
            .filter(|&&(_, x0, x1)| {
                x1 - x0 > 0.0
                    // On screen at all, and crossing one of the ends.
                    && x1 > sx + 0.5
                    && x0 < sx + row_w - 0.5
                    && (x0 < sx - 0.5 || x1 > sx + row_w + 0.5)
            })
            .map(|&(id, _, _)| id)
            .collect()
    }

    /// [`Self::cut_on_both_edges`] evaluated against a hypothetical `sx`,
    /// so a candidate offset can be tested before the strip moves to it.
    fn cut_on_both_edges_at(&self, spans: &[(u64, f64, f64)], sx: f64) -> bool {
        let row_w = self.rect.width().max(1.0);
        let (mut left, mut right) = (false, false);
        for &(_, x0, x1) in spans {
            if x1 - x0 <= 0.0 || x1 - x0 > row_w {
                continue;
            }
            // Scrolled wholly past an end is not a cut — that is the
            // strip doing its job. Only a group with part of itself on
            // screen and part of itself past an edge has been severed.
            if x1 <= sx + 0.5 || x0 >= sx + row_w - 0.5 {
                continue;
            }
            if x0 < sx - 0.5 {
                left = true;
            }
            if x1 > sx + row_w + 0.5 {
                right = true;
            }
        }
        left && right
    }

    /// Where the strip should rest, given what was just laid out; `None`
    /// (the common case) means it already is. Two rules, in order: (1) if
    /// all groups could be on screen together, none may be cut — move to
    /// the nearest edge of the window that shows them all; (2) otherwise
    /// move to the nearest offset that flushes some group's edge with the
    /// row's edge. Nearest in both cases so the smallest move preserves
    /// what's already framed.
    fn strip_frame_target(&self, spans: &[(u64, f64, f64)]) -> Option<f64> {
        let row_w = self.rect.width().max(1.0);
        let max = self.max_scroll_x();
        let first = spans.first()?;
        let leftmost = spans.iter().fold(first.1, |a, s| a.min(s.1));
        let rightmost = spans.iter().fold(first.2, |a, s| a.max(s.2));

        if rightmost - leftmost <= row_w {
            // Everything fits at once, so nothing is allowed to be cut.
            // Any offset in `[rightmost - row_w, leftmost]` shows the
            // lot; clamped into the strip's legal range, the nearest
            // point of it is where the strip belongs.
            let lo = (rightmost - row_w).clamp(0.0, max);
            let hi = leftmost.clamp(0.0, max).max(lo);
            let target = self.scroll_x.clamp(lo, hi);
            return ((target - self.scroll_x).abs() > 0.5).then_some(target);
        }

        if !self.cut_on_both_edges_at(spans, self.scroll_x) {
            return None;
        }
        let mut best: Option<f64> = None;
        for &(_, x0, x1) in spans {
            if x1 - x0 <= 0.0 || x1 - x0 > row_w {
                continue;
            }
            for candidate in [x0, x1 - row_w] {
                let candidate = candidate.clamp(0.0, max);
                if self.cut_on_both_edges_at(spans, candidate) {
                    continue;
                }
                let nearer = best.is_none_or(|b: f64| {
                    (candidate - self.scroll_x).abs() < (b - self.scroll_x).abs()
                });
                if nearer {
                    best = Some(candidate);
                }
            }
        }
        best.filter(|t| (t - self.scroll_x).abs() > 0.5)
    }

    /// True while a snap is in flight, or the user just wheeled (within
    /// the idle threshold); keeps the shell's redraw chain pumping.
    fn snap_in_flight(&self) -> bool {
        if self.snap_anim.is_some() {
            return true;
        }
        // Pending snap: idle gate hasn't fired yet. Keep redraws pumping
        // so `tick_snap` runs the moment it clears.
        if let Some(t) = self.last_wheel_at
            && t.elapsed() < Duration::from_millis(u64::from(SNAP_IDLE_MS + SNAP_ANIM_MS))
            && !self.is_aligned_to_group()
        {
            return true;
        }
        false
    }

    fn is_aligned_to_group(&self) -> bool {
        if self.group_left_edges.is_empty() {
            return true;
        }
        self.group_left_edges
            .iter()
            .any(|&e| (e - self.scroll_x).abs() < 0.5)
    }

    /// Snap target nearest the current `scroll_x`. Candidates include
    /// canvas-x = 0 (the wheel-visible home) when `leading_inset > 0` —
    /// without it, a small flick away from 0 would snap forward to the
    /// first group instead of relaxing back home.
    fn nearest_group_edge(&self) -> Option<f64> {
        if self.group_left_edges.is_empty() {
            return None;
        }
        let max = self.max_scroll_x();
        if max <= 0.0 {
            return None;
        }
        let mut best: Option<f64> = None;
        let mut best_dist = f64::MAX;
        let home_targets: &[f64] = if self.leading_inset > 0.0 {
            &[0.0]
        } else {
            &[]
        };
        for &edge in self.group_left_edges.iter().chain(home_targets.iter()) {
            let candidate = edge.clamp(0.0, max);
            let d = (candidate - self.scroll_x).abs();
            if d < best_dist {
                best_dist = d;
                best = Some(candidate);
            }
        }
        best
    }

    /// Select `tab_id` inside `group_id` widget-side. The bar's
    /// `active_id` is authoritative and the shell's per-frame harvest
    /// mirrors it into the persisted tree, so a tree-only write would be
    /// overwritten one frame later.
    pub fn set_group_active_tab(&mut self, group_id: u64, tab_id: u64) -> bool {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group_id) else {
            return false;
        };
        if !g.set_active_tab(tab_id) {
            return false;
        }
        // Force the parent's subtree cache and taffy tree to rebuild
        // this frame, same as a click on the pill would.
        self.scroll_dirty = true;
        self.structure_dirty = true;
        true
    }

    /// Split out from `apply_layout` so headless tests can advance
    /// simulated time deterministically without staging a full taffy
    /// tree.
    pub fn tick_snap_at(&mut self, now: Instant) {
        let before = self.scroll_x;
        // Kicking a fresh snap off here, before the curve-advance branch
        // below, lets this same call both start it and apply its first
        // eased frame.
        if self.snap_anim.is_none()
            && let Some(last_wheel) = self.last_wheel_at
        {
            let idle = now.saturating_duration_since(last_wheel);
            if idle >= Duration::from_millis(u64::from(SNAP_IDLE_MS)) {
                if self.is_aligned_to_group() {
                    self.last_wheel_at = None;
                } else if let Some(target) = self.nearest_group_edge() {
                    if (target - self.scroll_x).abs() < 0.5 {
                        self.scroll_x = target;
                        self.last_wheel_at = None;
                    } else {
                        // Anchor `started` at the moment the idle
                        // gate was met, not `now` — so the very tick
                        // that detects idle already applies the
                        // curve's first eased frame. Falls back to
                        // `last_wheel` (skipping the idle delay) rather
                        // than panicking in the astronomically unlikely
                        // case this addition would overflow `Instant`.
                        let started = last_wheel
                            .checked_add(Duration::from_millis(u64::from(SNAP_IDLE_MS)))
                            .unwrap_or(last_wheel);
                        self.snap_anim = Some(SnapAnim {
                            from: self.scroll_x,
                            to: target,
                            started,
                        });
                    }
                } else {
                    self.last_wheel_at = None;
                }
            }
        }
        if let Some(anim) = self.snap_anim {
            let elapsed = now.saturating_duration_since(anim.started);
            let total = Duration::from_millis(u64::from(SNAP_ANIM_MS));
            if elapsed >= total {
                self.scroll_x = anim.to;
                self.snap_anim = None;
                self.last_wheel_at = None;
            } else {
                let raw =
                    (elapsed.as_secs_f64() / (f64::from(SNAP_ANIM_MS) / 1000.0)).clamp(0.0, 1.0);
                let eased = crate::ease_out(raw);
                self.scroll_x = anim.from + (anim.to - anim.from) * eased;
            }
        }
        // Must invalidate the parent's subtree cache or needs_redraw()
        // stays false and it re-emits the pre-snap cached scene.
        if (self.scroll_x - before).abs() > f64::EPSILON {
            self.scroll_dirty = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_scroll_x_for_test(&mut self, x: f64) {
        self.scroll_x = x;
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(crate) fn group_left_edges_for_test(&self) -> &[f64] {
        &self.group_left_edges
    }

    #[cfg(test)]
    pub(crate) fn set_last_wheel_at_for_test(&mut self, t: Instant) {
        self.last_wheel_at = Some(t);
    }

    #[cfg(test)]
    pub(crate) fn set_group_left_edges_for_test(&mut self, edges: Vec<f64>) {
        self.group_left_edges = edges;
    }

    #[cfg(test)]
    pub(crate) fn set_content_w_for_test(&mut self, w: f64) {
        self.content_w = w;
    }

    #[cfg(test)]
    pub(crate) fn set_rect_for_test(&mut self, r: Rect) {
        self.rect = r;
    }

    /// Exposed separately from `handle_event` so tests can drive a
    /// transfer without staging a full winit event.
    pub fn transfer_tab(&mut self, src: usize, dst: usize, tab_id: u64, target_idx: usize) -> bool {
        if src == dst || src >= self.groups.len() || dst >= self.groups.len() {
            return false;
        }
        // Detach from src. We have to keep the mutable borrow scoped so
        // we can reborrow `dst` afterwards.
        let detached = {
            let Some(src_group) = self.groups.get_mut(src) else {
                return false;
            };
            src_group.detach_tab(tab_id)
        };
        let Some((desc, host)) = detached else {
            return false;
        };
        let Some(dst_group) = self.groups.get_mut(dst) else {
            return false;
        };
        dst_group.insert_detached_tab(target_idx, desc, host);
        true
    }

    /// `Some(gap_idx)` if the release landed strictly between two groups
    /// (`0` before the first, `N` after the last); `None` if it landed
    /// inside a group's rect. Deliberately not y-band-restricted: the
    /// release comes from our own row's `TabBar`, so anywhere that isn't
    /// on a group — including above/below our rendered rect — is a
    /// new-window gap.
    pub fn gap_index_for(&self, release_point: Point) -> Option<usize> {
        if self.groups.is_empty() {
            return Some(0);
        }
        for g in &self.groups {
            if g.get_rect().contains(release_point) {
                return None;
            }
        }
        let first = self.groups.first()?;
        if release_point.x < first.get_rect().x0 {
            return Some(0);
        }
        for (j, pair) in self.groups.windows(2).enumerate() {
            let [a, b] = pair else { continue };
            let left = a.get_rect().x1;
            let right = b.get_rect().x0;
            if release_point.x >= left && release_point.x <= right {
                return Some(j.saturating_add(1));
            }
        }
        let last = self.groups.last()?;
        if release_point.x > last.get_rect().x1 {
            return Some(self.groups.len());
        }
        None
    }

    /// Detach `tab_id` from `src` into a brand-new `WindowGroup` at
    /// `gap_idx`, pruning `src` if that leaves it empty.
    pub fn split_tab_into_new_group(&mut self, src: usize, tab_id: u64, gap_idx: usize) -> bool {
        if src >= self.groups.len() {
            return false;
        }
        let detached = {
            let Some(src_group) = self.groups.get_mut(src) else {
                return false;
            };
            src_group.detach_tab(tab_id)
        };
        let Some((desc, host)) = detached else {
            return false;
        };
        let src_now_empty = self.groups.get(src).is_some_and(|g| g.tab_count() == 0);
        let new_id = self
            .groups
            .iter()
            .map(|g| g.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut new_group = WindowGroup::from_detached(new_id, desc, host, WindowWidth::Normal);
        new_group.start_spawn_animation();
        let clamped = gap_idx.min(self.groups.len());
        self.groups.insert(clamped, new_group);
        // Without this, `apply_layout` iterates stale `node_ids` and
        // skips the newly inserted group.
        self.structure_dirty = true;
        if src_now_empty {
            // Shifts by 1 iff the new group was inserted at or left of src.
            let src_after = if clamped <= src {
                src.saturating_add(1)
            } else {
                src
            };
            if src_after < self.groups.len() {
                self.groups.remove(src_after);
            }
        }
        true
    }

    pub fn take_pending_cross_strip_detach(&mut self) -> Option<CrossStripDetach> {
        self.pending_cross_strip_detach.take()
    }

    pub fn take_pending_group_reorder(&mut self) -> Option<(u64, usize)> {
        self.pending_group_reorder.take()
    }

    pub fn group_drag_active(&self) -> bool {
        self.group_drag.is_some_and(|d| d.moved)
    }

    pub fn group_reorder_target(&self) -> Option<usize> {
        self.group_reorder_target
    }

    /// Slot before the first group whose centre sits right of the
    /// cursor. Centre-based so nudging past a neighbour's midpoint
    /// commits the swap, matching tab-reorder feel.
    fn group_insertion_index_for(&self, p: Point) -> usize {
        for (i, g) in self.groups.iter().enumerate() {
            if p.x < g.get_rect().center().x {
                return i;
            }
        }
        self.groups.len()
    }

    /// `insert_at` is a pre-removal gap index; converted here to the
    /// post-removal landing slot so `WorkspaceTree::reorder_group_within_strip`
    /// reproduces the same order.
    fn commit_group_reorder(&mut self, src: usize, group_id: u64, insert_at: usize) {
        if src >= self.groups.len() {
            return;
        }
        // Removing `src` shifts every later slot left by one.
        let final_idx = if insert_at > src {
            insert_at.saturating_sub(1)
        } else {
            insert_at
        };
        let dest = final_idx.min(self.groups.len().saturating_sub(1));
        if dest == src {
            return;
        }
        let g = self.groups.remove(src);
        self.groups.insert(dest, g);
        self.structure_dirty = true;
        self.pending_group_reorder = Some((group_id, dest));
    }

    /// Returns `true` when the event is claimed by an in-flight group
    /// drag (so the caller skips resize-handle + per-child dispatch). A
    /// press records a *candidate* drag but does not claim it, so a
    /// plain click still falls through to the child for focus — only
    /// once past [`GROUP_DRAG_THRESHOLD`] does the row claim motion.
    fn update_group_drag(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Don't start a group drag while a tab-detach or resize
                // drag is already in flight, or one is already tracked.
                if self.group_drag.is_some()
                    || self
                        .groups
                        .iter()
                        .any(super::window_group::WindowGroup::drag_in_flight)
                    || self
                        .handles
                        .iter()
                        .any(super::resize_handle::ResizeHandle::is_dragging)
                {
                    return false;
                }
                if let Some(idx) = self
                    .groups
                    .iter()
                    .position(|g| g.bar_grab_hit(self.last_cursor))
                    && let Some(group_id) = self.groups.get(idx).map(|g| g.id)
                {
                    self.group_drag = Some(GroupDrag {
                        src_idx: idx,
                        group_id,
                        start: self.last_cursor,
                        moved: false,
                    });
                }
                // Never claim the press — let it set focus on the group.
                false
            }
            WindowEvent::CursorMoved { .. } => {
                let Some(mut drag) = self.group_drag else {
                    return false;
                };
                if !drag.moved {
                    let dx = self.last_cursor.x - drag.start.x;
                    let dy = self.last_cursor.y - drag.start.y;
                    if (dx * dx + dy * dy).sqrt() > GROUP_DRAG_THRESHOLD {
                        drag.moved = true;
                    }
                }
                self.group_drag = Some(drag);
                if drag.moved {
                    self.group_reorder_target =
                        Some(self.group_insertion_index_for(self.last_cursor));
                    return true;
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let Some(drag) = self.group_drag.take() else {
                    return false;
                };
                let claimed = drag.moved;
                if drag.moved
                    && let Some(insert_at) = self.group_reorder_target.take()
                {
                    self.commit_group_reorder(drag.src_idx, drag.group_id, insert_at);
                }
                self.group_reorder_target = None;
                claimed
            }
            _ => false,
        }
    }

    /// Splices the tab into a brand-new [`WindowGroup`] at the gap
    /// nearest `drop_pos`. Also used for the "no strip matched, return
    /// to sender" recovery path, called on the source row.
    pub fn accept_cross_strip_drop(
        &mut self,
        desc: TabDescriptor,
        host: TileHost,
        drop_pos: Point,
    ) {
        let new_id = self
            .groups
            .iter()
            .map(|g| g.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut new_group = WindowGroup::from_detached(new_id, desc, host, WindowWidth::Normal);
        new_group.start_spawn_animation();
        // `gap_index_for` returns `None` when `drop_pos` falls inside an
        // existing group's rect; default to "append at end" so the tab
        // can't be silently lost.
        let gap_idx = self.gap_index_for(drop_pos).unwrap_or(self.groups.len());
        let clamped = gap_idx.min(self.groups.len());
        self.groups.insert(clamped, new_group);
        self.structure_dirty = true;
    }
}

impl Widget for GroupRow {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for g in &mut self.groups {
            g.sync(state, root);
        }
    }

    fn update(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        let mut any = false;
        for g in &mut self.groups {
            if g.update(state, root) {
                any = true;
            }
        }
        any
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.node_ids.clear();
        // Resolve every group's width fraction before it builds its node
        // (walking left to right so each group's basis is the display its
        // left edge lands on), with no dependency on a previous frame's
        // layout — keeps a single-frame offscreen render identical to live.
        let usable_per_group = self.usable_per_group();
        for (g, &usable) in self.groups.iter_mut().zip(usable_per_group.iter()) {
            g.set_strip_usable_px(usable);
        }
        let group_nodes: Vec<NodeId> = self
            .groups
            .iter_mut()
            .map(|g| g.build_layout(taffy, theme, node_map))
            .collect();
        self.node_ids.clone_from(&group_nodes);
        // The moment any child group is maximized, the row collapses to
        // that single group via `display: None` on siblings; handles are
        // skipped since there are no visible gaps. Multiple simultaneous
        // `maximized` flags: first one wins. A transient fullscreen
        // promotion drives the same collapse and wins over `maximized`.
        let maximized_idx = self
            .fullscreen_idx()
            .or_else(|| self.groups.iter().position(|g| g.maximized));
        // Grow/truncate the handle pool to `max(0, N-1)`; truncating from
        // the tail is fine since an active drag's handle would have had
        // its neighbours change structurally anyway.
        let desired_handles = self.groups.len().saturating_sub(1);
        if self.handles.len() < desired_handles {
            self.handles
                .extend((self.handles.len()..desired_handles).map(|_| ResizeHandle::new()));
        } else if self.handles.len() > desired_handles {
            self.handles.truncate(desired_handles);
        }
        // Handle `i` sits in the gap after group `i`, so it takes group
        // `i`'s basis for its drag clamp.
        for (i, h) in self.handles.iter_mut().enumerate() {
            if let Some(&usable) = usable_per_group.get(i) {
                h.set_strip_usable_px(usable);
            }
        }
        // Interleave handle nodes between group nodes:
        // `[g0, h0, g1, h1, …, g_{N-1}]`. Omitted entirely while any group
        // is maximized (no visible gaps to mark).
        self.handle_node_ids.clear();
        let mut children: Vec<NodeId> =
            Vec::with_capacity(group_nodes.len().saturating_add(desired_handles));
        if maximized_idx.is_some() {
            for gn in &group_nodes {
                children.push(*gn);
            }
        } else {
            for (i, gn) in group_nodes.iter().enumerate() {
                children.push(*gn);
                if i.saturating_add(1) < group_nodes.len() {
                    let Some(h) = self.handles.get_mut(i) else {
                        continue;
                    };
                    let hn = h.build_layout(taffy, theme, node_map);
                    self.handle_node_ids.push(hn);
                    children.push(hn);
                }
            }
        }
        // `flex_shrink: 0.0` on every group: Niri-style, placing a new
        // tile never resizes siblings; `content_width` grows past
        // `rect.width()` and `draw` clips to `self.rect`. `flex_shrink:
        // 1.0` was tried and rejected — it silently shrinks siblings on
        // every spawn, violating the infinite-strip invariant.
        for (i, &node) in group_nodes.iter().enumerate() {
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.flex_shrink = 0.0;
            if let Some(max_i) = maximized_idx {
                if i == max_i {
                    // `maximized` already carries this grow style from
                    // `WindowGroup::build_layout`; a fullscreen-promoted
                    // group doesn't, so set it here too.
                    style.size.width = Dimension::auto();
                    style.flex_grow = 1.0e6;
                    style.flex_basis = Dimension::length(0.0);
                } else {
                    style.display = Display::None;
                }
            }
            let _ = taffy.set_style(node, style);
        }
        // Row must fill remaining vertical space so each child's
        // `height: percent(1.0)` resolves — otherwise the row measures
        // only as tall as the TabBar and `TileHost` collapses to zero.
        // Children beyond `rect.x1` are still walked and shifted into
        // screen-space by `scroll_x`; still hit-testable since
        // `gap_index_for` uses each group's stored post-shift `Rect`.
        let node = taffy.parent_node(
            Style {
                // A strip owning no part of the current fullscreen
                // surface collapses away entirely, so the promoted
                // group in another strip is the only thing painted.
                display: if self.hidden {
                    Display::None
                } else {
                    Display::Flex
                },
                flex_direction: FlexDirection::Row,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                flex_grow: 1.0,
                // Zero here: the inter-group `ResizeHandle` widgets
                // supply the visible 8-px footprint per gap, so a
                // non-zero gap on top of that would double the
                // inter-group distance.
                gap: Size {
                    width: taffy::LengthPercentage::length(0.0),
                    height: taffy::LengthPercentage::length(0.0),
                },
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // `group_left_edges` (the snap targets) is CANVAS-x, not
        // taffy-local-x: canvas-x = leading_inset + taffy.x, and
        // `scroll_x` is how far that's panned past the viewport's left
        // edge.
        let mut content_w = self.leading_inset;
        let mut edges: Vec<f64> = Vec::with_capacity(self.node_ids.len());
        let mut local_x: Vec<f64> = Vec::with_capacity(self.node_ids.len());
        // Only trustworthy when taffy has a node per group — a pass
        // landing between a splice and the rebuild would otherwise pair
        // widths with the wrong ids.
        let spans_aligned = self.node_ids.len() == self.groups.len();
        let mut spans: Vec<(u64, f64, f64)> = Vec::with_capacity(self.node_ids.len());
        for (i, &cn) in self.node_ids.iter().enumerate() {
            let cl = taffy.layout(cn).copied().unwrap_or_default();
            edges.push(self.leading_inset + f64::from(cl.location.x));
            local_x.push(f64::from(cl.location.x));
            let left = self.leading_inset + f64::from(cl.location.x);
            let right = left + f64::from(cl.size.width);
            if spans_aligned && let Some(g) = self.groups.get(i) {
                spans.push((g.id, left, right));
            }
            if right > content_w {
                content_w = right;
            }
        }
        self.content_w = content_w;
        self.group_left_edges = edges;
        // First pass that knows how far each group actually moved; only
        // trust it when taffy has a node per group.
        if spans_aligned {
            self.record_shift_anim(&local_x);
        }
        if self.shift_hold.is_none() && self.shift_anim.is_some() && self.shift_anim_t() >= 1.0 {
            self.shift_anim = None;
        }
        // Same widened upper bound the wheel handler uses (max_scroll_x
        // OR three viewports past the rightmost group) — clamping to
        // `max_scroll_x` alone would reset scroll every frame on a
        // single-group strip, killing the "scroll into empty space" feel.
        let viewport = self.rect.width().max(1.0);
        let upper = self.max_scroll_x().max(viewport * 3.0);
        if self.scroll_x > upper {
            self.scroll_x = upper;
        }
        if self.scroll_x < 0.0 {
            self.scroll_x = 0.0;
        }
        if spans_aligned {
            self.reframe_on_geometry_change(spans);
        }
        // Done AFTER the clamp so a structural mutation that snapped
        // scroll_x to max doesn't get re-eased.
        self.tick_snap_at(Instant::now());
        // Panning stays in lock-step with the wheel-disc's own
        // `-scroll_x` shift in `ShellOverlayHost::apply_layout`. A
        // fullscreen-promoted group drops the leading gutter and any
        // prior pan, pinning it flush at `rect.x0`.
        let (origin_inset, origin_scroll) = if self.is_fullscreen() {
            (0.0, 0.0)
        } else {
            (self.leading_inset, self.scroll_x)
        };
        let child_origin = Point::new(self.rect.x0 + origin_inset - origin_scroll, self.rect.y0);
        for (g, child_id) in self.groups.iter_mut().zip(self.node_ids.iter()) {
            g.apply_layout(taffy, *child_id, child_origin);
        }
        self.sync_handle_neighbours();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.scroll_dirty = false;
        // Clip to the row's rect: `content_width` routinely exceeds
        // `rect.width()`, so off-screen-right groups would bleed past
        // the window edge. 16px vertical pad keeps shadow/glow sprites
        // (ambient_pad=50, bloom_pad=120, see `TileHost::draw_steady_state`)
        // intact at top/bottom.
        let clip = Rect::new(
            self.rect.x0,
            self.rect.y0 - 16.0,
            self.rect.x1,
            self.rect.y1 + 16.0,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        // When any group is maximized, paint ONLY that group: siblings'
        // rects are degenerate (collapsed by `build_layout`), but
        // drawing them anyway would emit ghost chrome at the origin.
        let maximized_idx = self
            .fullscreen_idx()
            .or_else(|| self.groups.iter().position(|g| g.maximized));
        self.draw_groups(scene, transform, tr, theme, presence, maximized_idx);
        self.draw_handles(scene, transform, tr, theme, presence, maximized_idx);
        self.draw_reorder_capsule(scene, transform, theme);

        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track the cursor so our post-dispatch detach sweep can hit-test
        // sibling bars even after a child has swallowed the event.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            // Gate on an active drag so the "drop here" indicator never
            // shows for idle cursor motion.
            let any_drag = self
                .groups
                .iter()
                .any(super::window_group::WindowGroup::drag_in_flight);
            self.hover_gap_idx = if any_drag {
                self.gap_index_for(self.last_cursor)
            } else {
                None
            };
            // Push the cursor into every sibling bar so its drop-line
            // paints at the correct gap; the source bar drives its own
            // target internally. The setter no-ops on a bar's own
            // in-flight drag, so visiting it last in the fan-out can't
            // wipe it.
            let cursor_for_external = if any_drag {
                Some(self.last_cursor)
            } else {
                None
            };
            for g in &mut self.groups {
                g.set_external_drag_hover(cursor_for_external);
            }
        }

        // Drop is resolved on release; leaving the indicator set would
        // let a stale gap highlight persist into the next frame.
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.hover_gap_idx = None;
        }

        // Runs before resize-handle/per-child dispatch so, once past
        // threshold, it claims the motion/release they'd otherwise
        // contend for. A press records a candidate without claiming, so
        // a plain click still reaches the child for focus.
        if self.update_group_drag(event) {
            *handled = true;
            return;
        }

        // A bare wheel NEVER moves the strip — that only happens via
        // `scroll_group_into_view_animated`, not this event path.

        self.dispatch_to_handles(
            event,
            scale_factor,
            *modifiers,
            state,
            root,
            handled,
            focus_target,
            taffy,
        );
        self.apply_pending_handle_resizes();
        self.dispatch_to_groups(
            event,
            scale_factor,
            *modifiers,
            state,
            root,
            handled,
            focus_target,
            taffy,
        );
        self.prune_closed_groups();
        self.resolve_tab_detaches(handled);
    }

    fn name(&self) -> &'static str {
        "GroupRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        self.groups
            .iter_mut()
            .find_map(super::Widget::take_floating_surface_request)
    }

    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        self.groups
            .iter_mut()
            .find_map(super::Widget::take_capture_studio_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.groups
            .iter_mut()
            .flat_map(super::Widget::take_floating_surface_source_updates)
            .collect()
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for group in &mut self.groups {
            group.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        for group in &mut self.groups {
            group.set_floating_surface_bounds(bounds);
        }
    }
    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        self.groups
            .iter()
            .flat_map(super::Widget::floating_surface_presentations)
            .collect()
    }
    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(super::Widget::active_floating_surface_ids)
            .collect()
    }
    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for group in &mut self.groups {
            group.on_floating_surface_host_message(message);
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.groups.iter().any(super::Widget::wants_pointer_capture)
            || self
                .handles
                .iter()
                .any(super::Widget::wants_pointer_capture)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        // Resize seams win where their generous hit band overlaps a group's
        // edge. Empty tab-bar space then advertises whole-group dragging,
        // instead of making that useful mouse path discoverable by accident.
        if let Some(cursor) = self
            .handles
            .iter()
            .find_map(|handle| handle.cursor_shape(point))
        {
            return Some(cursor);
        }
        if self.group_drag.is_some() {
            return Some(winit::window::CursorIcon::Grabbing);
        }
        if self.groups.iter().any(|group| group.bar_grab_hit(point)) {
            return Some(winit::window::CursorIcon::Grab);
        }
        self.groups
            .iter()
            .find_map(|group| group.cursor_shape(point))
    }

    /// Neither this nor [`WindowGroup`] exposes `children()` (groups lay
    /// out outside taffy), so any rect a test needs has to be republished
    /// here. Each affordance is keyed by group id, plus the bare name for
    /// the leftmost, so a single-group scenario can stay unqualified.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut out = Vec::new();
        for g in &self.groups {
            let r = g.new_tab_rect();
            if r.area() <= 0.0 {
                continue;
            }
            if out.is_empty() {
                out.push(("tabbar_new_tab".to_string(), r));
            }
            out.push((format!("tabbar_new_tab[{}]", g.id), r));
        }
        // `tab:Files` per pill, so a scenario names a tab instead of a
        // coordinate; first group's pills also take the bare name.
        let mut first = true;
        for g in &self.groups {
            for (tab, rect) in g.tabs().iter().zip(g.tab_rects().iter()) {
                if rect.area() <= 0.0 {
                    continue;
                }
                if first {
                    out.push((format!("tab:{}", tab.title), *rect));
                }
                out.push((format!("tab:{}[{}]", tab.title, g.id), *rect));
            }
            first = false;
        }
        // By seam index, so a scenario can narrow a group with the real
        // gesture instead of a layout edit.
        for (i, h) in self.handles.iter().enumerate() {
            let r = h.get_rect();
            if r.area() > 0.0 {
                out.push((format!("group_resize_handle[{i}]"), r));
            }
        }
        // Hosted app widgets aren't children of the row (the tile host
        // owns them), so the walk into each active tile happens here.
        let mut first = true;
        for g in &self.groups {
            let Some(host) = g.active_host() else {
                continue;
            };
            let TileContent::App(app) = host.content() else {
                continue;
            };
            let mut named: Vec<(String, Rect)> = Vec::new();
            collect_named_rects(app.as_ref() as &dyn Widget, &mut named);
            for (name, rect) in named {
                if first {
                    out.push((name.clone(), rect));
                }
                out.push((format!("{name}[{}]", g.id), rect));
            }
            first = false;
        }
        out
    }

    fn needs_redraw(&self) -> bool {
        // A wheel-pan changes `scroll_x` without touching the row's rect
        // or any child's dirty bit, so without this the parent's cached
        // scene would keep painting pre-scroll positions.
        if self.scroll_dirty {
            return true;
        }
        self.groups.iter().any(super::Widget::needs_redraw)
            || self.handles.iter().any(super::Widget::needs_redraw)
    }

    fn needs_animation_frame(&self) -> bool {
        // Must bubble up child groups explicitly: the default
        // `children()` returns empty, so a TabBar drop-zone curve would
        // otherwise never reach the shell's overlay host.
        self.snap_in_flight()
            || self.shift_in_flight()
            || self.groups.iter().any(super::Widget::needs_animation_frame)
    }

    fn take_structure_dirty(&mut self) -> bool {
        // `fold` rather than `any`: must not short-circuit, every child
        // has to clear its own flag too.
        let own = std::mem::take(&mut self.structure_dirty);
        #[allow(clippy::unnecessary_fold)] // see comment above: must not short-circuit
        let children = self
            .groups
            .iter_mut()
            .map(super::Widget::take_structure_dirty)
            .fold(false, |acc, d| acc || d);
        // Forces a rebuild next frame while snapping, since `apply_layout`
        // (where `tick_snap_at` lives) otherwise only runs on events.
        let snap = self.snap_in_flight();
        own || children || snap
    }
}

impl GroupRow {
    /// Records this frame's per-group local-x for the next `apply_layout`'s
    /// shift-fade comparison, and — when `shift_armed` was set by a splice
    /// this frame — starts a [`ShiftAnim`] for whichever groups actually
    /// moved. Split out of `apply_layout` to keep that under the line cap.
    fn record_shift_anim(&mut self, local_x: &[f64]) {
        if std::mem::take(&mut self.shift_armed) {
            let offsets: Vec<(u64, f64)> = self
                .groups
                .iter()
                .zip(local_x.iter())
                .filter_map(|(g, now)| {
                    let was = *self.last_local_x.get(&g.id)?;
                    let from = was - now;
                    // Sub-pixel motion is not motion.
                    (from.abs() > 0.5).then_some((g.id, from))
                })
                .collect();
            self.shift_anim = (!offsets.is_empty()).then(|| ShiftAnim {
                started: Instant::now(),
                offsets,
            });
        }
        self.last_local_x = self
            .groups
            .iter()
            .zip(local_x.iter())
            .map(|(g, x)| (g.id, *x))
            .collect();
    }

    /// Re-frame when the strip's geometry moved under the pan (resize,
    /// open/close, output change): the shell's into-view pan only
    /// triggers on focus change, so an offset chosen for a layout that
    /// no longer exists would otherwise persist. Held off while dragging
    /// so the strip doesn't snap out from under the hand; corrected on
    /// release. Split out of `apply_layout` to keep that under the line cap.
    fn reframe_on_geometry_change(&mut self, spans: Vec<(u64, f64, f64)>) {
        let geometry_moved = spans != self.laid_spans;
        self.laid_spans = spans;
        let steady = self.snap_anim.is_none()
            && !self.group_drag_active()
            && !self
                .handles
                .iter()
                .any(super::resize_handle::ResizeHandle::is_dragging);
        if geometry_moved
            && steady
            && let Some(target) = self.strip_frame_target(&self.laid_spans)
        {
            self.scroll_x = target;
            self.scroll_dirty = true;
            self.last_wheel_at = None;
        }
    }

    /// Feeds each `ResizeHandle` the group rects flanking its gap, after
    /// every group has its own new layout applied. Split out of
    /// `apply_layout` to keep that under the line cap.
    fn sync_handle_neighbours(&mut self) {
        debug_assert_eq!(
            self.handles.len(),
            self.groups.len().saturating_sub(1),
            "ResizeHandle pool must mirror inter-group gap count after build_layout",
        );
        for (i, h) in self.handles.iter_mut().enumerate() {
            let Some(left) = self.groups.get(i).map(WindowGroup::get_rect) else {
                continue;
            };
            let Some(right) = self
                .groups
                .get(i.saturating_add(1))
                .map(WindowGroup::get_rect)
            else {
                continue;
            };
            h.set_neighbour_rects(left, right);
        }
    }

    /// Paints either the sole maximized group, or every group eased in
    /// from its make-space-slide offset (see [`Self::shift_offset_for`]).
    fn draw_groups(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        maximized_idx: Option<usize>,
    ) {
        if let Some(max_i) = maximized_idx {
            if let Some(g) = self.groups.get_mut(max_i) {
                g.draw(scene, transform, tr, theme, presence);
            }
            return;
        }
        // A group a fresh insertion pushed along paints where it used to
        // be and eases to its new spot, so the strip is seen opening a
        // gap rather than having always had one.
        let remaining = 1.0 - self.shift_anim_t();
        let offsets: Vec<(u64, f64)> = if remaining > 0.0 {
            self.shift_anim
                .as_ref()
                .map(|a| a.offsets.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for g in &mut self.groups {
            let dx = offsets
                .iter()
                .find(|(id, _)| *id == g.id)
                .map_or(0.0, |(_, from)| from * remaining);
            let t = if dx.abs() > f64::EPSILON {
                shift_transform(transform, dx)
            } else {
                transform
            };
            g.draw(scene, t, tr, theme, presence);
        }
    }

    /// Painted after the groups so the accent rule sits on top. Skipped
    /// during a tab-drag (drop-zone capsule takes priority) and while
    /// any group is maximized (handle rects are stale). During a
    /// make-space slide, each handle travels with the group on its
    /// right — otherwise two adjacent white cards read as one wide
    /// window mid-slide.
    fn draw_handles(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        maximized_idx: Option<usize>,
    ) {
        let any_drag = self
            .groups
            .iter()
            .any(super::window_group::WindowGroup::drag_in_flight);
        if any_drag || maximized_idx.is_some() {
            return;
        }
        let remaining = 1.0 - self.shift_anim_t();
        let offsets: Vec<(u64, f64)> = if remaining > 0.0 {
            self.shift_anim
                .as_ref()
                .map(|a| a.offsets.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for (i, h) in self.handles.iter_mut().enumerate() {
            // A group with nothing on screen has no border to be one
            // side of. Still-opening counts as shown: it's already a
            // card with real edges at the width it's waiting to fill.
            let shown = |g: Option<&WindowGroup>| {
                g.is_some_and(|g| Widget::presented(g) || Widget::opening(g))
            };
            let next_i = i.saturating_add(1);
            if !shown(self.groups.get(i)) || !shown(self.groups.get(next_i)) {
                continue;
            }
            let dx = self
                .groups
                .get(next_i)
                .and_then(|g| offsets.iter().find(|(id, _)| *id == g.id))
                .map_or(0.0, |(_, from)| from * remaining);
            let t = if dx.abs() > f64::EPSILON {
                shift_transform(transform, dx)
            } else {
                transform
            };
            h.draw(scene, t, tr, theme, presence);
        }
    }

    /// Painted only while a group drag is past its threshold.
    fn draw_reorder_capsule(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let (Some(_), Some(ins)) = (
            self.group_drag.filter(|d| d.moved),
            self.group_reorder_target,
        ) else {
            return;
        };
        if self.groups.is_empty() {
            return;
        }
        let gap_x = if ins == 0 {
            self.groups.first().map_or(0.0, |g| g.get_rect().x0 - 5.0)
        } else if ins >= self.groups.len() {
            self.groups.last().map_or(0.0, |g| g.get_rect().x1 + 5.0)
        } else {
            let left = self
                .groups
                .get(ins.saturating_sub(1))
                .map(WindowGroup::get_rect);
            let right = self.groups.get(ins).map(WindowGroup::get_rect);
            left.zip(right)
                .map_or(0.0, |(l, r)| f64::midpoint(l.x1, r.x0))
        };
        let y0 = self.rect.y0;
        let y1 = self.rect.y1;
        if y1 <= y0 {
            return;
        }
        let thickness = 4.0;
        let capsule = RoundedRect::from_rect(
            Rect::new(gap_x - thickness / 2.0, y0, gap_x + thickness / 2.0, y1),
            thickness / 2.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.accent[6],
            None,
            &capsule,
        );
    }

    /// Resize handles dispatch FIRST so a press in the gap claims the
    /// event before adjacent groups see it. Skipped during any child's
    /// tab-drag, which owns the gap itself.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event's own signature"
    )]
    fn dispatch_to_handles(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) {
        let any_drag_inflight = self
            .groups
            .iter()
            .any(super::window_group::WindowGroup::drag_in_flight);
        if any_drag_inflight {
            return;
        }
        for (i, h) in self.handles.iter_mut().enumerate() {
            let Some(&node_id) = self.handle_node_ids.get(i) else {
                break;
            };
            h.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                root,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            if *handled {
                break;
            }
        }
    }

    /// Applied before the group dispatch loop so a mid-drag `CursorMoved`
    /// sees the updated widths this same frame.
    fn apply_pending_handle_resizes(&mut self) {
        for (i, h) in self.handles.iter_mut().enumerate() {
            if let Some((new_left, new_right)) = h.take_pending() {
                if let Some(g) = self.groups.get_mut(i) {
                    g.set_width_px(new_left);
                }
                if let Some(g) = self.groups.get_mut(i.saturating_add(1)) {
                    g.set_width_px(new_right);
                }
            }
            if h.take_pending_commit() {
                // End-of-drag: mirrors `WindowGroup::end_resize`.
                if let Some(g) = self.groups.get_mut(i) {
                    g.stamp_pending_width_change();
                }
                if let Some(g) = self.groups.get_mut(i.saturating_add(1)) {
                    g.stamp_pending_width_change();
                }
            }
        }
    }

    /// Stop dispatch once a child claims the event, but the detach sweep
    /// after this still runs regardless — that claim is for
    /// pointer-capture/focus, a separate concern. `groups` can grow
    /// mid-frame (detach sweep pushes a new `WindowGroup`), briefly
    /// outrunning `node_ids`; `.get` skips the new one until its next
    /// layout pass assigns a `node_id`.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event's own signature"
    )]
    fn dispatch_to_groups(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) {
        for (i, g) in self.groups.iter_mut().enumerate() {
            let Some(&node_id) = self.node_ids.get(i) else {
                break;
            };
            g.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                root,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            if *handled {
                break;
            }
        }
    }

    /// Collected highest-index-first so removals don't shift indices out
    /// from under the loop.
    fn prune_closed_groups(&mut self) {
        let mut prune: Vec<usize> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.close_complete())
            .map(|(i, _)| i)
            .collect();
        if prune.is_empty() {
            return;
        }
        // `node_ids` deliberately stays stale (pre-prune length) until
        // the next `build_layout`; the debug_assert below pins that so a
        // future path popping a slot early trips loudly.
        let prune_count = prune.len();
        prune.sort_unstable_by(|a, b| b.cmp(a));
        for i in prune {
            self.groups.remove(i);
        }
        debug_assert_eq!(
            self.node_ids.len(),
            self.groups.len().saturating_add(prune_count),
            "GroupRow parallel-vec invariant violated: node_ids.len() ({}) \
             must equal pre-prune groups.len() (post-prune {} + pruned {}). \
             node_ids stays at the pre-prune length until structure_dirty \
             triggers a build_layout rebuild.",
            self.node_ids.len(),
            self.groups.len(),
            prune_count,
        );
        self.structure_dirty = true;
    }

    /// A release outside our vertical band is plausibly aimed at a
    /// sibling strip, which we can't resolve from here — detach and
    /// stash for the shell-side harvester (`pending_cross_strip_detach`).
    /// Only ONE detach diverts per frame; the gesture pipeline doesn't
    /// model dragging two tabs at once. Remaining in-row detaches are
    /// resolved by [`Self::resolve_one_tab_detach`].
    fn resolve_tab_detaches(&mut self, handled: &mut bool) {
        // CursorMoved only reclassifies on motion, so a child that
        // dropped its `pending_drag` internally could otherwise leave
        // this stuck once every drag has cleared.
        if self.hover_gap_idx.is_some()
            && !self
                .groups
                .iter()
                .any(super::window_group::WindowGroup::drag_in_flight)
        {
            self.hover_gap_idx = None;
        }

        let mut detaches: Vec<(usize, u64, Point)> = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if let Some((tab_id, release_point)) = g.take_tab_detach() {
                detaches.push((i, tab_id, release_point));
            }
        }

        let mut diverted: Option<usize> = None;
        for (idx, &(src_idx, tab_id, release_point)) in detaches.iter().enumerate() {
            let out_of_band = release_point.y < self.rect.y0 || release_point.y > self.rect.y1;
            if !out_of_band {
                continue;
            }
            let Some(src_group) = self.groups.get_mut(src_idx) else {
                continue;
            };
            if let Some((desc, host)) = src_group.detach_tab(tab_id) {
                self.pending_cross_strip_detach = Some(CrossStripDetach {
                    tab_id,
                    desc,
                    host,
                    release_point,
                });
                // Source might now be empty; don't prune here — it stays
                // alive until the shell delivers or returns the drop.
                self.structure_dirty = true;
                diverted = Some(idx);
                *handled = true;
                break;
            }
        }

        // Strip out the diverted detach so the in-row resolver below
        // doesn't see it (the tab is already gone from the source group).
        if let Some(skip_idx) = diverted {
            detaches.remove(skip_idx);
        }

        for (src_idx, tab_id, release_point) in detaches {
            self.resolve_one_tab_detach(src_idx, tab_id, release_point, handled);
        }
    }

    /// One in-row detach's landing spot: an existing sibling's tab bar,
    /// its tile area (edge bands split off a new group, the centre band
    /// splices into its tab list), a closing sibling's former gap, or —
    /// failing all of those — the plain inter-group gap. Falls back to
    /// re-inserting into `src_idx` if nothing resolved at all.
    fn resolve_one_tab_detach(
        &mut self,
        src_idx: usize,
        tab_id: u64,
        release_point: Point,
        handled: &mut bool,
    ) {
        if self.try_drop_on_sibling_bar(src_idx, tab_id, release_point, handled) {
            return;
        }
        if self.try_drop_in_tile_area(src_idx, tab_id, release_point, handled) {
            return;
        }
        if self.try_drop_on_closing_sibling_gap(src_idx, tab_id, release_point, handled) {
            return;
        }
        // Deliberately does NOT refuse a single-tab source dropped into
        // an adjacent gap: with one group, gap_idx is always 0 or 1, so
        // refusing would block every drag-out from a single-tab row
        // even though the flow produces correct state.
        if let Some(gap_idx) = self.gap_index_for(release_point)
            && self.split_tab_into_new_group(src_idx, tab_id, gap_idx)
        {
            *handled = true;
            return;
        }
        // No target resolved (e.g. release over a closing sibling's
        // content): treat as cancel. Each branch above detaches lazily
        // inside its own path, so an unmatched release already leaves
        // the source intact — this is a defensive guard against a
        // future refactor moving the detach earlier.
        let tab_in_source = self
            .groups
            .get(src_idx)
            .is_some_and(|g| g.tabs().iter().any(|t| t.id == tab_id));
        if !tab_in_source {
            let recovered = self.groups.iter_mut().find_map(|g| g.detach_tab(tab_id));
            if let Some((desc, host)) = recovered
                && let Some(src_group) = self.groups.get_mut(src_idx)
            {
                src_group.insert_detached_tab(0, desc, host);
            }
        }
    }

    /// First [`Self::resolve_one_tab_detach`] landing spot: an existing
    /// sibling's tab bar. Closing groups excluded: `insert_detached_tab`
    /// doesn't clear `closing_at`, so a spliced tab would be pruned with it
    /// at fade-end. Falls through (returns `false`) to the next path
    /// instead. Returns `true` once a bar-rect target is found, whether or
    /// not the drop itself completed — the caller commits to this path
    /// either way.
    fn try_drop_on_sibling_bar(
        &mut self,
        src_idx: usize,
        tab_id: u64,
        release_point: Point,
        handled: &mut bool,
    ) -> bool {
        let target_idx = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, g)| *i != src_idx && !g.is_closing() && g.bar_rect().contains(release_point))
            .map(|(i, _)| i);
        let Some(tgt_idx) = target_idx else {
            return false;
        };
        let Some(drop_idx) = self
            .groups
            .get(tgt_idx)
            .map(|g| g.drop_index_for(release_point))
        else {
            return true;
        };
        let Some(src_group) = self.groups.get_mut(src_idx) else {
            return true;
        };
        let moved_tab = src_group.detach_tab(tab_id);
        if let Some((desc, host)) = moved_tab
            && let Some(tgt) = self.groups.get_mut(tgt_idx)
        {
            tgt.insert_detached_tab(drop_idx, desc, host);
            *handled = true;
        }
        true
    }

    /// Second [`Self::resolve_one_tab_detach`] landing spot: three
    /// horizontal bands across a sibling's tile area. Edge bands (25% of
    /// width, capped at 48px — the gap alone is too thin to hit) create a
    /// new group to that side; the centre band splices into the sibling's
    /// tab list. Same "found means committed" return convention as
    /// [`Self::try_drop_on_sibling_bar`].
    fn try_drop_in_tile_area(
        &mut self,
        src_idx: usize,
        tab_id: u64,
        release_point: Point,
        handled: &mut bool,
    ) -> bool {
        let tile_target_idx = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, g)| {
                *i != src_idx
                    && !g.is_closing()
                    && g.get_rect().contains(release_point)
                    && !g.bar_rect().contains(release_point)
            })
            .map(|(i, _)| i);
        let Some(tgt_idx) = tile_target_idx else {
            return false;
        };
        let Some(tgt_rect) = self.groups.get(tgt_idx).map(WindowGroup::get_rect) else {
            return true;
        };
        let edge_band = (tgt_rect.width() * 0.25).min(48.0);
        let in_left_band = release_point.x < tgt_rect.x0 + edge_band;
        let in_right_band = release_point.x > tgt_rect.x1 - edge_band;
        if in_left_band || in_right_band {
            let gap_idx = if in_left_band {
                tgt_idx
            } else {
                tgt_idx.saturating_add(1)
            };
            if self.split_tab_into_new_group(src_idx, tab_id, gap_idx) {
                *handled = true;
            }
            return true;
        }
        let drop_idx = if release_point.x < tgt_rect.center().x {
            0
        } else {
            self.groups.get(tgt_idx).map_or(0, |g| g.tabs().len())
        };
        let Some(src_group) = self.groups.get_mut(src_idx) else {
            return true;
        };
        let moved_tab = src_group.detach_tab(tab_id);
        if let Some((desc, host)) = moved_tab
            && let Some(tgt) = self.groups.get_mut(tgt_idx)
        {
            tgt.insert_detached_tab(drop_idx, desc, host);
            *handled = true;
        }
        true
    }

    /// Third [`Self::resolve_one_tab_detach`] landing spot: a *closing*
    /// sibling's bar rect still exists geometrically, so `gap_index_for`
    /// returns `None`; this is the fallback to the left-of-closing-group
    /// gap instead of dropping the tab. Same "found means committed"
    /// return convention as [`Self::try_drop_on_sibling_bar`].
    fn try_drop_on_closing_sibling_gap(
        &mut self,
        src_idx: usize,
        tab_id: u64,
        release_point: Point,
        handled: &mut bool,
    ) -> bool {
        let closing_target = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, g)| *i != src_idx && g.is_closing() && g.bar_rect().contains(release_point))
            .map(|(i, _)| i);
        let Some(tgt_idx) = closing_target else {
            return false;
        };
        if self.split_tab_into_new_group(src_idx, tab_id, tgt_idx) {
            *handled = true;
        }
        true
    }
}

/// `base` shifted right by `dx` px. Wrapped so the one
/// `arithmetic_side_effects` allow lives in a single place: `Affine`'s
/// `Mul` is pure floating-point matrix composition and cannot panic or
/// overflow.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication is floating-point matrix composition; it cannot panic or overflow"
)]
fn shift_transform(base: Affine, dx: f64) -> Affine {
    base * Affine::translate((dx, 0.0))
}

/// Zero-area rects are skipped — a name means "on screen now".
fn collect_named_rects(w: &dyn Widget, out: &mut Vec<(String, Rect)>) {
    if let Some(id) = w.test_id() {
        let r = w.get_rect();
        if r.area() > 0.0 {
            out.push((id.to_string(), r));
        }
    }
    for (name, rect) in w.test_rects() {
        if rect.area() > 0.0 {
            out.push((name, rect));
        }
    }
    for child in w.children() {
        collect_named_rects(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{TabDescriptor, TileContent, WindowWidth, min_group_px};

    /// Narrows a fixture viewport width to the `f32` taffy layout expects.
    /// Test fixtures only ever pass small literal pixel constants (hundreds
    /// to low thousands), far inside `f32`'s exact-integer range, so this
    /// cannot lose precision.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test fixture viewport widths are small literal pixel constants, always exactly representable in f32"
    )]
    fn px32(v: f64) -> f32 {
        v as f32
    }

    /// `t` minus `ms` milliseconds, saturating rather than panicking in
    /// the astronomically unlikely case the subtraction would underflow
    /// `Instant`'s valid range.
    fn earlier(t: Instant, ms: u64) -> Instant {
        t.checked_sub(Duration::from_millis(ms)).unwrap_or(t)
    }

    fn make_group(id: u64, tab_ids: &[u64]) -> WindowGroup {
        let tabs: Vec<(TabDescriptor, TileContent)> = tab_ids
            .iter()
            .map(|t| {
                (
                    TabDescriptor::new(*t, format!("T{t}")),
                    TileContent::Stub {
                        kind: crate::widget::AppKind::Settings,
                        label: format!("S{t}"),
                    },
                )
            })
            .collect();
        let active = tab_ids[0];
        WindowGroup::new(id, tabs, active, WindowWidth::Normal)
    }

    #[test]
    fn transfer_tab_moves_descriptor_and_host_between_groups() {
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.transfer_tab(0, 1, 2, 0));

        let g0 = &row.groups[0];
        let g1 = &row.groups[1];
        let g0_ids: Vec<u64> = g0.tabs().iter().map(|t| t.id).collect();
        let g1_ids: Vec<u64> = g1.tabs().iter().map(|t| t.id).collect();
        assert_eq!(g0_ids, vec![1, 3]);
        assert_eq!(g1_ids, vec![2, 10, 11]);
    }

    #[test]
    fn transfer_tab_is_noop_for_same_group() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[3]);
        let mut row = GroupRow::new(vec![g0, g1]);
        assert!(!row.transfer_tab(0, 0, 1, 0));
    }

    #[test]
    fn transfer_tab_is_noop_for_unknown_tab() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[3]);
        let mut row = GroupRow::new(vec![g0, g1]);
        assert!(!row.transfer_tab(0, 1, 999, 0));
    }

    #[test]
    fn split_tab_into_new_group_appends_with_empty_source_pruned() {
        let g0 = make_group(100, &[1]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 1, 2));

        assert_eq!(row.groups.len(), 2);
        let first_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        let second_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(first_ids, vec![10, 11]);
        assert_eq!(second_ids, vec![1]);
        assert_ne!(row.groups[1].id, 200, "new group id must be unique");
    }

    #[test]
    fn split_tab_into_new_group_inserts_between_existing_groups() {
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 2, 1));

        assert_eq!(row.groups.len(), 3);
        let ids: Vec<Vec<u64>> = row
            .groups
            .iter()
            .map(|g| g.tabs().iter().map(|t| t.id).collect())
            .collect();
        assert_eq!(ids[0], vec![1, 3]);
        assert_eq!(ids[1], vec![2]);
        assert_eq!(ids[2], vec![10, 11]);
        assert_eq!(row.groups[1].active_tab_id(), 2);
    }

    #[test]
    fn split_tab_into_new_group_flags_structure_dirty_once() {
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(!row.take_structure_dirty(), "fresh row must not be dirty");

        assert!(row.split_tab_into_new_group(0, 2, 1));
        assert!(
            row.take_structure_dirty(),
            "successful split must flag structure_dirty for the App to rebuild taffy"
        );
        // Edge-triggered — no second pulse without another mutation.
        assert!(
            !row.take_structure_dirty(),
            "flag must clear after first poll so we don't rebuild every frame"
        );

        assert!(!row.split_tab_into_new_group(0, 9999, 0));
        assert!(
            !row.take_structure_dirty(),
            "failed split must not flag structure_dirty"
        );
    }

    #[test]
    fn split_tab_into_new_group_noop_for_unknown_tab() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[3]);
        let mut row = GroupRow::new(vec![g0, g1]);
        assert!(!row.split_tab_into_new_group(0, 999, 0));
        assert_eq!(row.groups.len(), 2);
        let g0_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(g0_ids, vec![1, 2]);
    }

    #[test]
    fn split_tab_into_new_group_prepends_when_gap_zero() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[10]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 1, 0));

        assert_eq!(row.groups.len(), 3);
        let ids: Vec<Vec<u64>> = row
            .groups
            .iter()
            .map(|g| g.tabs().iter().map(|t| t.id).collect())
            .collect();
        assert_eq!(ids[0], vec![1]);
        assert_eq!(ids[1], vec![2]);
        assert_eq!(ids[2], vec![10]);
    }

    #[test]
    fn gap_index_for_classifies_inter_group_drops() {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let g0 = make_group(100, &[1]);
        let g1 = make_group(200, &[10]);
        let mut row = GroupRow::new(vec![g0, g1]);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);

        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(2000.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(2000.0),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);

        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let row_rect = row.rect;
        let y = f64::midpoint(row_rect.y0, row_rect.y1);

        assert_eq!(row.gap_index_for(Point::new(r0.x0 - 4.0, y)), Some(0));
        assert_eq!(row.gap_index_for(Point::new(r1.x1 + 4.0, y)), Some(2));
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 + 4.0, y)),
            None,
            "drops inside a group tile should not be inter-group gaps"
        );
        // Above the row still resolves to a gap: not y-band-restricted.
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 - 4.0, row_rect.y0 - 10.0)),
            Some(0)
        );
        // Only assert if the layout actually left a gap.
        if r1.x0 > r0.x1 {
            let mid = f64::midpoint(r0.x1, r1.x0);
            assert_eq!(row.gap_index_for(Point::new(mid, y)), Some(1));
        }
    }

    /// 1200 puts the width tiers on whole hundreds (Narrow 400 / Normal
    /// 800 / Wide 1200), so fraction-based group widths get round pixel
    /// numbers to assert against.
    const FIXTURE_USABLE_PX: f64 = 1200.0;

    fn one_display(usable: f64) -> Vec<OutputSpan> {
        vec![OutputSpan {
            x: 0.0,
            width: usable,
            usable,
        }]
    }

    fn make_row_with_groups_with_taffy(tab_specs: &[&[u64]]) -> (GroupRow, TaffyTree<()>, NodeId) {
        make_row_with_groups_on_display(tab_specs, FIXTURE_USABLE_PX, 2000.0)
    }

    fn relayout_row_owned(mut row: GroupRow, viewport_w: f64) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(px32(viewport_w)),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(px32(viewport_w)),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    /// Separate `usable`/`viewport_w` knobs: the strip deliberately
    /// overflows its viewport, so the width groups size against isn't
    /// the window width.
    fn make_row_with_groups_on_display(
        tab_specs: &[&[u64]],
        usable: f64,
        viewport_w: f64,
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let groups: Vec<WindowGroup> = tab_specs
            .iter()
            .enumerate()
            .map(|(i, ids)| make_group((i as u64).saturating_mul(100).saturating_add(100), ids))
            .collect();
        let mut row = GroupRow::new(groups);
        row.set_output_spans(&one_display(usable));
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(px32(viewport_w)),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(px32(viewport_w)),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    fn make_row_with_groups(tab_specs: &[&[u64]]) -> GroupRow {
        make_row_with_groups_with_taffy(tab_specs).0
    }

    /// A point in group `idx`'s tab-bar background — right of the last tab
    /// pill, at the bar's vertical centre — where a whole-group reorder
    /// grab lands.
    fn group_grab_point(row: &GroupRow, idx: usize) -> Point {
        let g = &row.groups[idx];
        let bar = g.bar_rect();
        let last = g.tab_rects().last().copied().unwrap_or(bar);
        Point::new(
            f64::midpoint(last.x1, bar.x1),
            f64::midpoint(bar.y0, bar.y1),
        )
    }

    fn press_at(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
        dispatch_at(
            row,
            taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
        );
        dispatch_at(
            row,
            taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
    }

    fn dispatch_at(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, ev: &WindowEvent) {
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            ev,
            1.0,
            &modifiers,
            &mut state,
            &path,
            &mut handled,
            &mut focus,
            root,
            taffy,
        );
    }

    #[test]
    fn bar_grab_point_hits_the_group_grab_zone() {
        let row = make_row_with_groups(&[&[1], &[2]]);
        let p = group_grab_point(&row, 0);
        assert!(
            row.groups[0].bar_grab_hit(p),
            "the trailing tab-bar background must be a valid group-grab zone",
        );
        assert_eq!(row.cursor_shape(p), Some(winit::window::CursorIcon::Grab));
        // A tab pill is NOT a grab zone (it drives the tab gesture).
        let pill = row.groups[0].tab_rects()[0].center();
        assert!(!row.groups[0].bar_grab_hit(pill));
    }

    #[test]
    fn drag_group_to_end_reorders_group_vector_and_publishes_one_shot() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        assert_eq!(row.group_ids(), vec![100, 200, 300]);

        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(past.x, past.y),
            },
        );
        assert!(
            row.group_drag_active(),
            "past-threshold move must engage the drag"
        );
        assert_eq!(
            row.group_reorder_target(),
            Some(3),
            "cursor past every group's centre targets the append gap",
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );

        assert_eq!(
            row.group_ids(),
            vec![200, 300, 100],
            "dragged group must land at the dropped slot; neighbours shift left",
        );
        assert_eq!(
            row.take_pending_group_reorder(),
            Some((100, 2)),
            "commit must publish (group_id, landing_index) for the shell to persist",
        );
    }

    #[test]
    fn drag_group_backwards_between_two_groups() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 2);
        press_at(&mut row, &taffy, root, grab);
        // Aim at the left half of group 200 (insertion gap 1).
        let target = Point::new(row.groups[1].get_rect().center().x - 20.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(target.x, target.y),
            },
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(row.group_ids(), vec![100, 300, 200]);
    }

    #[test]
    fn plain_click_on_bar_background_does_not_reorder() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(
            row.group_ids(),
            vec![100, 200, 300],
            "a click must not reorder"
        );
        assert_eq!(row.take_pending_group_reorder(), None);
        assert!(!row.group_drag_active());
    }

    #[test]
    fn scroll_group_into_view_pans_to_reveal_offscreen_group() {
        let (mut row, _taffy, _root) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        row.set_content_w_for_test(row.content_width());
        let target_id = 500; // far-right group, off the viewport's edge
        let before = row.scroll_x();
        let changed = row.scroll_group_into_view(target_id);
        assert!(changed, "an off-screen group must trigger a scroll");
        assert!(
            row.scroll_x() > before,
            "revealing a right-edge group must increase scroll_x (was {before}, now {})",
            row.scroll_x(),
        );
    }

    #[test]
    fn scroll_group_into_view_animated_eases_to_the_same_target() {
        // One row panned by the teleporting variant (reference target),
        // one by the animated variant — must land on the same place.
        let build = || {
            let (mut row, _taffy, _root) =
                make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
            row.set_content_w_for_test(row.content_width());
            row
        };
        let mut plain = build();
        let mut anim = build();
        let target_id = 500; // far-right group, off the viewport's edge
        assert!(plain.scroll_group_into_view(target_id));
        let expected = plain.scroll_x();
        assert!(expected > 0.0);

        let t0 = Instant::now();
        assert!(
            anim.scroll_group_into_view_animated(target_id, t0),
            "an off-screen group must start an animated pan"
        );
        assert_eq!(
            anim.scroll_x(),
            0.0,
            "the pan is applied by ticks, not the call"
        );
        anim.tick_snap_at(t0 + Duration::from_millis(u64::from(SNAP_ANIM_MS / 2)));
        let mid = anim.scroll_x();
        assert!(
            mid > 0.0 && mid < expected,
            "halfway through the curve scroll_x must be between 0 and {expected}, got {mid}"
        );
        anim.tick_snap_at(t0 + Duration::from_millis(u64::from(SNAP_ANIM_MS + 50)));
        assert_eq!(
            anim.scroll_x(),
            expected,
            "the animated pan must settle on the same target the teleporting variant picks"
        );
    }

    #[test]
    fn scroll_group_into_view_animated_is_a_noop_for_a_visible_group() {
        // Fixture's full 2000px viewport: first group is fully inside.
        let (mut row, _taffy, _root) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        assert!(!row.scroll_group_into_view_animated(100, Instant::now()));
        assert_eq!(row.scroll_x(), 0.0);
        assert!(
            !row.needs_animation_frame(),
            "a no-op must not leave an animation pumping frames"
        );
    }

    #[test]
    fn drop_into_new_window_works_when_release_is_in_empty_strip_area_below_group() {
        // The shell's strip is taller than any single group, and release
        // jitter pushes past the row edge.
        let row = make_row_with_groups(&[&[1, 2, 3, 4]]);
        let release = Point::new(row.groups[0].get_rect().x1 + 4.0, row.rect.y1 + 20.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(1),
            "release below the row's vertical band should still resolve to the right-of-last gap so \
             the tab splits into a new window"
        );
    }

    #[test]
    fn drop_into_new_window_works_when_release_is_above_row() {
        // Companion to the "below" case.
        let row = make_row_with_groups(&[&[1, 2], &[10, 11]]);
        let r0 = row.groups[0].get_rect();
        let release = Point::new(r0.x0 - 4.0, row.rect.y0 - 15.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(0),
            "release above the row should resolve to the left-of-first gap"
        );
    }

    #[test]
    fn single_tab_group_can_split_into_new_group_on_any_adjacent_gap() {
        // Companion to `single_tab_group_split_is_not_refused_by_handle_event_sweep`.
        let mut row = make_row_with_groups(&[&[1]]);
        assert!(
            row.split_tab_into_new_group(0, 1, 1),
            "single-tab group must be splittable into an adjacent gap"
        );
        // Source pruned (empty after detach), new group took its slot.
        assert_eq!(row.groups.len(), 1);
        assert_eq!(row.groups[0].tabs().len(), 1);
        assert_eq!(row.groups[0].tabs()[0].id, 1);
        assert!(
            row.take_structure_dirty(),
            "split must flag structure_dirty"
        );
    }

    #[test]
    fn single_tab_group_split_is_not_refused_by_handle_event_sweep() {
        // Drives the post-dispatch sweep end-to-end, where the refuse
        // rule (unlike the direct API above) used to live.
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[7]]);
        let g0_rect = row.groups[0].get_rect();
        let release = Point::new(g0_rect.x1 + 10.0, f64::midpoint(g0_rect.y0, g0_rect.y1));
        // Mimics what `TabBar::handle_event` does on an out-of-rect release.
        row.groups[0].bar_mut().pending_detach = Some((7, release));

        // A no-op event that still triggers the sweep.
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(release.x, release.y),
        };
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        assert!(
            row.take_structure_dirty(),
            "single-tab source must split into adjacent gap: handle_event sweep should \
             flag structure_dirty. Today the `src_only_tab && adjacent` refuse rule \
             silently skips this path"
        );
        assert_eq!(row.groups.len(), 1);
        assert_eq!(row.groups[0].tabs().len(), 1);
        assert_eq!(row.groups[0].tabs()[0].id, 7);
        assert!(row.groups[0].bar_mut().pending_detach.is_none());
    }

    #[test]
    fn transfer_active_tab_promotes_sibling_in_source_and_activates_in_dest() {
        let g0 = make_group(100, &[1, 2, 3]); // active = 1
        let g1 = make_group(200, &[10, 11]); // active = 10
        let mut row = GroupRow::new(vec![g0, g1]);
        assert_eq!(row.groups[0].active_tab_id(), 1);

        assert!(row.transfer_tab(0, 1, 1, 0));

        let src_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(src_ids, vec![2, 3]);
        assert_eq!(
            row.groups[0].active_tab_id(),
            2,
            "source must pick a surviving neighbour as the new active"
        );

        let dst_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(dst_ids, vec![1, 10, 11]);
        assert_eq!(
            row.groups[1].active_tab_id(),
            1,
            "dragged tab should be active in its new group"
        );
    }

    /// Stage a drag-in-flight on the given group's `TabBar` directly,
    /// mirroring what `TabBar::handle_event` does once the
    /// press-to-drag threshold is crossed, without replaying the full
    /// gesture.
    fn start_drag(row: &mut GroupRow, group_idx: usize, tab_id: u64) {
        use crate::dnd::DragPayload;
        row.groups[group_idx].bar_mut().pending_drag = Some(DragPayload::Tab {
            group: 0,
            tab: tab_id,
        });
    }

    fn fire_cursor_moved(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            taffy,
        );
    }

    #[test]
    fn hover_gap_tracks_cursor_during_drag() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        // Gap between group 0 and group 1 (gap_idx == 1).
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
        let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);

        fire_cursor_moved(&mut row, &taffy, root, mid_gap);

        assert_eq!(
            row.hover_gap_idx,
            Some(1),
            "cursor in the gap between groups 0 and 1 must set hover_gap_idx = Some(1)"
        );
    }

    #[test]
    fn hover_gap_clears_on_release() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
        let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);
        fire_cursor_moved(&mut row, &taffy, root, mid_gap);
        assert!(
            row.hover_gap_idx.is_some(),
            "precondition: drag tracked the gap"
        );

        let event = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        assert_eq!(
            row.hover_gap_idx, None,
            "MouseInput::Released must clear hover_gap_idx so no ghost indicator persists"
        );
    }

    #[test]
    fn group_pruned_after_close_fade_completes() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Already-elapsed timestamp: ready-to-prune on the very next tick.
        row.groups[0].set_closing_at_for_tests(earlier(Instant::now(), 200));
        assert_eq!(row.groups.len(), 2, "precondition: two groups before prune");

        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        assert_eq!(
            row.groups.len(),
            1,
            "fully-faded group must be dropped from self.groups",
        );
        assert!(
            row.take_structure_dirty(),
            "prune must flag structure_dirty so App rebuilds taffy",
        );
    }

    /// Mirror of the prune test above.
    #[test]
    fn group_retained_during_close_fade() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        row.groups[0].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[0].is_closing());
        assert!(!row.groups[0].close_complete());

        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        assert_eq!(
            row.groups.len(),
            2,
            "group mid-fade must NOT be pruned — the user is still watching the animation",
        );
    }

    #[test]
    fn drop_onto_closing_group_creates_new_group_instead() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // Two multi-tab groups so neither gets pruned by the
        // detach-empties-source rule.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);

        // B (index 1) is fresh-latched closing, so the prune sweep won't
        // touch it this tick.
        row.groups[1].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[1].is_closing());
        assert!(!row.groups[1].close_complete());
        let b_tabs_before: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(b_tabs_before, vec![10, 11]);

        let b_bar = row.groups[1].bar_rect();
        let release = Point::new(
            f64::midpoint(b_bar.x0, b_bar.x1),
            f64::midpoint(b_bar.y0, b_bar.y1),
        );
        row.groups[0].bar_mut().pending_detach = Some((1, release));

        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(release.x, release.y),
        };
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        let b_idx = row
            .groups
            .iter()
            .position(WindowGroup::is_closing)
            .expect("B should still be present (mid-fade)");
        let b_tabs_after: Vec<u64> = row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_tabs_after, b_tabs_before,
            "closing group's tabs must be unchanged — the drop must not insert into it",
        );

        // A (minus tab 1), new group (tab 1), B (closing): the new group
        // sits at what used to be B's index.
        assert_eq!(
            row.groups.len(),
            3,
            "drop-onto-closing falls through to split-into-new-group",
        );
        let a_tabs: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(a_tabs, vec![2], "A lost its dragged tab (id 1)");
        assert!(
            row.groups[2].is_closing(),
            "B (closing) should still be present at the right of the row",
        );
        let new_group_tabs: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            new_group_tabs,
            vec![1],
            "dragged tab must end up in a fresh group, not in the closing group",
        );
        assert!(
            !row.groups[1].is_closing(),
            "the new group must NOT inherit the closing state",
        );
    }

    #[test]
    fn drop_in_sibling_tile_area_left_half_prepends_right_half_appends() {
        use winit::event::WindowEvent;

        // B has tabs [10, 11, 12] so prepend/append are distinguishable
        // from a default insert-at-zero.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11, 12]]);
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();

        let release_left = Point::new(
            b_rect.x0 + (b_rect.width() * 0.25),
            f64::midpoint(b_bar.y1, b_rect.y1),
        );
        assert!(b_rect.contains(release_left));
        assert!(!b_bar.contains(release_left));
        assert!(release_left.x < b_rect.center().x);

        row.groups[0].bar_mut().pending_detach = Some((1, release_left));
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let release_event = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        row.handle_event(
            &release_event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        let b_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_ids,
            vec![1, 10, 11, 12],
            "left-half tile drop must prepend the dragged tab to the target's tab list",
        );
        let a_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            a_ids,
            vec![2],
            "source group must lose the dragged tab to the target",
        );
        assert_eq!(row.groups[1].active_tab_id(), 1);

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11, 12]]);
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();
        let release_right = Point::new(
            b_rect.x0 + (b_rect.width() * 0.75),
            f64::midpoint(b_bar.y1, b_rect.y1),
        );
        assert!(b_rect.contains(release_right));
        assert!(!b_bar.contains(release_right));
        assert!(release_right.x > b_rect.center().x);

        row.groups[0].bar_mut().pending_detach = Some((1, release_right));
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &release_event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        let b_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_ids,
            vec![10, 11, 12, 1],
            "right-half tile drop must append the dragged tab to the target's tab list",
        );
    }

    /// A release in a *closing* sibling's content area falls through
    /// every resolution branch (`gap_index_for` also returns `None` since
    /// it's inside a group's rect). Pins the fallback: no valid target
    /// means the tab stays in source.
    #[test]
    fn cross_group_drop_into_closing_content_area_keeps_tab_in_source() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // B (index 1) is fresh-latched closing so the prune sweep won't
        // reap it this tick.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
        row.groups[1].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[1].is_closing());
        assert!(!row.groups[1].close_complete());

        // Inside B's outer rect but BELOW its bar rect — its content area.
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();
        let release = Point::new(
            f64::midpoint(b_rect.x0, b_rect.x1),
            f64::midpoint(b_bar.y1, b_rect.y1),
        );
        assert!(
            b_rect.contains(release),
            "precondition: release is inside B's outer rect",
        );
        assert!(
            !b_bar.contains(release),
            "precondition: release is OUTSIDE B's bar rect (content area)",
        );
        assert_eq!(
            row.gap_index_for(release),
            None,
            "precondition: gap_index_for returns None for inside-group releases",
        );

        // Pre-stage the detach marker instead of replaying press →
        // drag-past-threshold: `press_origin` is private.
        let a_tabs_before: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        row.groups[0].bar_mut().pending_detach = Some((1, release));

        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let release_event = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        row.handle_event(
            &release_event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        let a_tabs_after: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            a_tabs_after, a_tabs_before,
            "source group must keep the dragged tab when no valid target resolves",
        );
        assert_eq!(
            row.groups.len(),
            2,
            "no new group should be spliced in for a cancelled drop",
        );
        let b_idx = row
            .groups
            .iter()
            .position(WindowGroup::is_closing)
            .expect("B should still be present mid-fade");
        let b_tabs_after: Vec<u64> = row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_tabs_after,
            vec![10, 11],
            "closing group's tabs must be untouched — drop did not insert into it",
        );
        assert!(
            row.groups[0].bar_mut().pending_detach.is_none(),
            "post-dispatch sweep must drain pending_detach even when no target resolves",
        );
    }

    #[test]
    fn hover_gap_is_none_when_no_drag() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
        let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);

        fire_cursor_moved(&mut row, &taffy, root, mid_gap);

        assert_eq!(
            row.hover_gap_idx, None,
            "cursor motion without an active drag must not publish a hover_gap_idx"
        );
    }

    /// The prune sweep removes faded groups from `self.groups` while
    /// `node_ids` stays stale until the next `build_layout`; the
    /// production `debug_assert` pins that. Gated since the asserts
    /// exercised are themselves debug-only.
    #[cfg(debug_assertions)]
    #[test]
    fn group_prune_keeps_node_ids_aligned_in_debug() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // Non-adjacent prune indices (0, 2) verify the
        // highest-index-first sort doesn't shift slots under the loop.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[0].set_closing_at_for_tests(earlier(Instant::now(), 500));
        row.groups[2].set_closing_at_for_tests(earlier(Instant::now(), 500));

        let groups_pre = row.groups.len();
        let node_ids_pre = row.node_ids.len();
        assert_eq!(
            groups_pre, 3,
            "precondition: row was seeded with three groups",
        );
        assert_eq!(
            node_ids_pre, groups_pre,
            "precondition: node_ids must equal groups.len() after build_layout",
        );

        // Production debug_assert would panic here if node_ids drifted.
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );

        assert_eq!(
            row.groups.len(),
            1,
            "two fully-faded groups must be pruned in a single sweep",
        );

        // Only refused outcome: drift with no rebuild gate set, which
        // would crash apply_layout.
        let aligned_now = row.node_ids.len() == row.groups.len();
        let dirty_for_rebuild = row.take_structure_dirty();
        assert!(
            aligned_now || dirty_for_rebuild,
            "after prune: node_ids must either match groups.len() ({}) directly \
             or structure_dirty must be set so build_layout rebuilds them \
             (node_ids.len() = {})",
            row.groups.len(),
            row.node_ids.len(),
        );
    }

    /// Each `WindowWidth::Normal` group is 600px, so `count`×600 +
    /// (`count`-1)×8 (gap) is the natural content width; pass a
    /// `viewport_w` strictly less to force horizontal overflow.
    fn make_overflow_row(count: usize, viewport_w: f64) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let groups: Vec<WindowGroup> = (0..count)
            .map(|i| {
                make_group(
                    (i as u64).saturating_mul(100).saturating_add(100),
                    &[(i as u64).saturating_add(1)],
                )
            })
            .collect();
        let mut row = GroupRow::new(groups);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(px32(viewport_w)),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(px32(viewport_w)),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    #[test]
    fn content_width_exceeds_viewport_when_groups_overflow() {
        // WIDTH_MIN_PX = 300; 8 groups so sum-of-mins (8*300 + 7*8 =
        // 2456) clears the 1200 viewport regardless of shrinking.
        let (row, _t, _r) = make_overflow_row(8, 1200.0);
        let expected_min = 8.0 * 300.0 + 7.0 * 8.0 - 1.0; // 2455, 1 px fp slop.
        assert!(
            row.content_width() >= expected_min,
            "content_w must reflect the floored group widths; got {} expected ≥{}",
            row.content_width(),
            expected_min
        );
        assert!(
            row.max_scroll_x() > 0.0,
            "viewport 1200 < content {} → max_scroll must be positive",
            row.content_width()
        );
    }

    /// `group_canvas_spans` is what the shell reads to place a new
    /// launch, so overlapping spans would seat a window on top of one
    /// already there. Every pair is compared, not just neighbours: two
    /// groups sharing an origin only shows up under a full pairwise sweep.
    #[test]
    fn no_two_groups_on_a_strip_share_horizontal_space() {
        for (count, viewport) in [(2usize, 1280.0_f64), (3, 1280.0), (4, 900.0), (8, 1200.0)] {
            let (row, _t, _r) = make_overflow_row(count, viewport);
            let spans = row.group_canvas_spans();
            assert_eq!(
                spans.len(),
                count,
                "every group on a {count}-group strip gets a span",
            );

            let mut ids: Vec<u64> = spans.iter().map(|(id, _, _)| *id).collect();
            ids.sort_unstable();
            let distinct = {
                let mut d = ids.clone();
                d.dedup();
                d.len()
            };
            assert_eq!(
                distinct, count,
                "a strip holds {count} distinct groups, not the same one twice \
                 (ids: {ids:?})",
            );

            for (i, (id, l, r)) in spans.iter().enumerate() {
                assert!(
                    r > l,
                    "group {id} (index {i}) occupies a positive width, got \
                     {l:.1}..{r:.1}",
                );
            }
            for (i, (a_id, a_l, a_r)) in spans.iter().enumerate() {
                for (b_id, b_l, b_r) in spans.iter().skip(i + 1) {
                    assert!(
                        a_r <= b_l || b_r <= a_l,
                        "groups {a_id} ({a_l:.0}..{a_r:.0}) and {b_id} \
                         ({b_l:.0}..{b_r:.0}) overlap on a {count}-group strip \
                         at viewport {viewport} — that is two windows drawn in \
                         one place",
                    );
                }
            }
        }
    }

    /// Lay `row` out inside a `viewport_w`-wide viewport through the
    /// real `build_layout` / `apply_layout` pair, rebuilding the taffy
    /// tree so a changed width actually takes — the baked length on the
    /// old node is what a stale tree would keep serving.
    fn relayout_strip(row: &mut GroupRow, viewport_w: f64) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(px32(viewport_w)),
                    height: Dimension::length(1080.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(px32(viewport_w)),
                    height: AvailableSpace::Definite(1080.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
    }

    /// A strip of `count` groups, each `permille` of the strip's usable
    /// width, behind the shell's wheel-disc gutter — the shape the
    /// compositor actually lays out.
    fn strip_at(count: usize, viewport_w: f64, leading_inset: f64, permille: u16) -> GroupRow {
        let groups: Vec<WindowGroup> = (0..count)
            .map(|i| {
                make_group(
                    (i as u64).saturating_mul(100).saturating_add(100),
                    &[(i as u64).saturating_add(1)],
                )
            })
            .collect();
        let mut row = GroupRow::new(groups);
        row.set_leading_inset(leading_inset);
        set_strip_permille(&mut row, permille);
        // Twice: the first `build_layout` runs before `apply_layout` has
        // set the row's rect, so it resolves against
        // `DEFAULT_STRIP_USABLE_PX` rather than this viewport.
        relayout_strip(&mut row, viewport_w);
        relayout_strip(&mut row, viewport_w);
        row
    }

    fn set_strip_permille(row: &mut GroupRow, permille: u16) {
        for g in &mut row.groups {
            g.set_width(WindowWidth::Fraction(permille));
        }
    }

    /// Which groups are wholly on screen, by id.
    fn whole_groups(row: &GroupRow) -> Vec<u64> {
        let cut = row.groups_cut_by_row_edges();
        let (vis0, vis1) = row.visible_canvas_span();
        row.laid_spans
            .iter()
            .filter(|(id, x0, x1)| !cut.contains(id) && *x1 > vis0 + 0.5 && *x0 < vis1 - 0.5)
            .map(|(id, _, _)| *id)
            .collect()
    }

    /// The strip only re-frames when *focus* moves, so widening every
    /// group (moving every edge) would otherwise keep a stale offset.
    /// Sequence mirrors the compositor's own: open + bring into view,
    /// then give groups a new share of the strip.
    #[test]
    fn widening_a_strip_never_leaves_it_cut_on_both_edges() {
        let mut row = strip_at(2, 1920.0, 291.0, 667);
        // Sitting where bringing the newest window into view leaves it:
        // hard against its right end.
        let parked = row.max_scroll_x();
        row.set_scroll_x_for_test(parked);
        assert!(parked > 0.0, "two groups at 667 permille overflow 1920");

        set_strip_permille(&mut row, 720);
        relayout_strip(&mut row, 1920.0);

        assert!(
            !row.cut_on_both_edges(),
            "the strip rests cut on both edges after the widening — groups \
             {:?} at scroll_x {:.0}, which shows half of one window on the \
             left and half of another on the right",
            row.laid_spans,
            row.scroll_x(),
        );
        let whole = whole_groups(&row);
        assert!(
            !whole.is_empty(),
            "if the two groups cannot both fit, one of them must still be \
             framed whole — spans {:?} at scroll_x {:.0}",
            row.laid_spans,
            row.scroll_x(),
        );
        assert_eq!(
            row.groups_cut_by_row_edges().len(),
            1,
            "exactly one window runs off an end when two of them overflow \
             the output; spans {:?} at scroll_x {:.0}",
            row.laid_spans,
            row.scroll_x(),
        );
    }

    /// When both groups fit, neither is cut: an offset showing every
    /// window whole exists, and resting anywhere else means a stale pan.
    #[test]
    fn a_strip_that_fits_the_output_frames_every_window_whole() {
        let mut row = strip_at(2, 1920.0, 291.0, 720);
        row.set_scroll_x_for_test(row.max_scroll_x());

        set_strip_permille(&mut row, 580);
        relayout_strip(&mut row, 1920.0);

        let span = row
            .laid_spans
            .last()
            .zip(row.laid_spans.first())
            .map(|((_, _, r), (_, l, _))| r - l)
            .unwrap_or_default();
        assert!(
            span <= 1920.0,
            "test setup: two groups at 580 permille must fit 1920, got {span:.0}",
        );
        assert_eq!(
            row.groups_cut_by_row_edges(),
            Vec::<u64>::new(),
            "nothing may be cut when the whole strip fits on screen — spans \
             {:?} at scroll_x {:.0}",
            row.laid_spans,
            row.scroll_x(),
        );
        assert_eq!(
            whole_groups(&row).len(),
            2,
            "both windows are on screen and whole — spans {:?} at scroll_x {:.0}",
            row.laid_spans,
            row.scroll_x(),
        );
    }

    /// A window bigger than the output is cut on both sides by
    /// definition; without a guard the framing rule would chase an
    /// impossible target forever. Hand-built spans: `WindowWidth::resolve`
    /// clamps every group to the strip's own width, so a real layout
    /// can't currently produce an oversized span.
    #[test]
    fn a_window_wider_than_the_output_is_left_alone() {
        let mut row = strip_at(1, 900.0, 0.0, 1000);
        // 1400px window across a 900px row, parked showing its middle so
        // both row edges cross it.
        let oversized = [(100u64, 0.0, 1400.0)];
        row.set_scroll_x_for_test(250.0);
        assert!(
            !row.cut_on_both_edges_at(&oversized, row.scroll_x()),
            "a single oversized window is not a two-sided cut: there is no \
             offset that frames it, so there is nothing to repair",
        );
        assert_eq!(
            row.strip_frame_target(&oversized),
            None,
            "the framing rule leaves an unframeable window where it is",
        );
        // Two of them, so the rule cannot fall back on a neighbour either.
        let both = [(100u64, 0.0, 1400.0), (200u64, 1408.0, 2808.0)];
        row.set_scroll_x_for_test(700.0);
        assert_eq!(
            row.strip_frame_target(&both),
            None,
            "nor when every group on the strip is wider than the strip",
        );
    }

    #[test]
    fn drop_in_visible_trailing_area_creates_new_group_at_end() {
        // No need to actually scroll: with a narrow viewport the last
        // group already sits past `row.rect.x1` (`flex_shrink: 0.0`).
        let (mut row, _t, _r) = make_overflow_row(3, 1000.0);
        let last_rect = row.groups.last().unwrap().get_rect();
        let release = Point::new(last_rect.x1 + 50.0, last_rect.y0 + 20.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(3),
            "release past the last group's right edge must resolve to gap_idx=N"
        );
        let src_tab = row.groups[0].tabs()[0].id;
        assert!(
            row.split_tab_into_new_group(0, src_tab, 3),
            "split_tab_into_new_group must succeed at the trailing gap"
        );
        // Source had only one tab, so it's pruned; net group count is 3.
        assert_eq!(row.groups.len(), 3);
        let last_ids: Vec<u64> = row
            .groups
            .last()
            .unwrap()
            .tabs()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(last_ids, vec![src_tab]);
    }

    #[test]
    fn wheel_over_strip_never_moves_it() {
        use winit::dpi::PhysicalPosition;

        for (mut row, taffy, root) in [
            make_overflow_row(8, 1200.0),
            make_row_with_groups_with_taffy(&[&[1], &[10]]),
        ] {
            let pre_rect_0 = row.groups[0].get_rect();
            let cy = f64::midpoint(row.rect.y0, row.rect.y1);
            let cx = f64::midpoint(row.rect.x0, row.rect.x1);
            let mut state = AutoCommit::new();
            let root_obj = automerge::ROOT.clone();
            let modifiers = Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            let dev = unsafe { std::mem::zeroed() };
            row.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: PhysicalPosition::new(cx, cy),
                },
                1.0,
                &modifiers,
                &mut state,
                &root_obj,
                &mut handled,
                &mut focus,
                root,
                &taffy,
            );
            row.handle_event(
                &WindowEvent::MouseWheel {
                    device_id: dev,
                    delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                    phase: winit::event::TouchPhase::Moved,
                },
                1.0,
                &modifiers,
                &mut state,
                &root_obj,
                &mut handled,
                &mut focus,
                root,
                &taffy,
            );
            assert!(!handled, "a bare wheel must be left unclaimed by the strip");
            // Lets any wrongly-started eased travel show itself.
            row.tick_snap_at(Instant::now() + Duration::from_millis(400));
            assert_eq!(row.scroll_x(), 0.0, "a bare wheel must not move the strip");
            row.apply_layout(&taffy, root, Point::ORIGIN);
            assert_eq!(
                row.groups[0].get_rect().x0,
                pre_rect_0.x0,
                "group rects must not shift after a bare wheel"
            );
        }
    }

    #[test]
    fn scroll_snaps_to_nearest_group_after_wheel_idle() {
        // Synthesised directly (three 600px groups, 8px gap → edges at
        // 0, 608, 1216) rather than depending on taffy's flex math.
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
        row.set_content_w_for_test(1816.0);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
        // 400 is closer to 608 than to 0: exercises the second group's edge.
        row.set_scroll_x_for_test(400.0);
        let base = Instant::now();
        row.set_last_wheel_at_for_test(earlier(base, 250));

        // First tick after idle: snap kicks off and advances ~25% of the
        // way (50 ms into a 200 ms cubic-out curve). scroll_x must have
        // moved toward 608 but not yet arrived.
        row.tick_snap_at(base + Duration::from_millis(50));
        assert!(
            row.scroll_x() > 400.0,
            "snap must move scroll_x toward the target; got {}",
            row.scroll_x()
        );
        assert!(
            row.scroll_x() < 608.0,
            "snap must NOT have arrived after only 50 ms; got {}",
            row.scroll_x()
        );

        row.tick_snap_at(base + Duration::from_millis(250));
        assert!(
            (row.scroll_x() - 608.0).abs() < 1.0,
            "snap must settle at 608.0 after 200 ms; got {}",
            row.scroll_x()
        );
    }

    #[test]
    fn snap_does_not_fire_during_wheel_idle_window() {
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
        row.set_content_w_for_test(1816.0);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
        row.set_scroll_x_for_test(400.0);
        let base = Instant::now();
        // Only 100 ms since the last wheel — inside the idle window.
        row.set_last_wheel_at_for_test(earlier(base, 100));

        row.tick_snap_at(base);
        assert_eq!(
            row.scroll_x(),
            400.0,
            "snap must not fire while still inside the idle window",
        );
    }

    #[test]
    fn wheel_mid_snap_leaves_the_snap_untouched() {
        use winit::dpi::PhysicalPosition;

        // 8 groups → sum-of-mins exceeds the 1200px viewport.
        let (mut row, taffy, root) = make_overflow_row(8, 1200.0);
        // Stage an in-flight snap directly to skip a real idle window.
        row.set_scroll_x_for_test(300.0);
        let base = Instant::now();
        row.set_last_wheel_at_for_test(earlier(base, 250));
        row.tick_snap_at(base);
        assert!(
            row.snap_anim.is_some(),
            "precondition: snap must be in flight",
        );

        // Cursor must be inside the row's rect for the handler to claim it.
        let cy = f64::midpoint(row.rect.y0, row.rect.y1);
        let cx = f64::midpoint(row.rect.x0, row.rect.x1);
        let mut state = AutoCommit::new();
        let root_obj = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let dev = unsafe { std::mem::zeroed() };
        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        row.handle_event(
            &WindowEvent::MouseWheel {
                device_id: dev,
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        let anim = row
            .snap_anim
            .expect("the in-flight snap must survive a bare wheel notch");
        // Was easing toward the stop nearest 300; must not be re-targeted.
        let edges = row.group_left_edges_for_test().to_vec();
        let expected = edges
            .iter()
            .copied()
            .min_by(|a, b| (a - 300.0).abs().partial_cmp(&(b - 300.0).abs()).unwrap())
            .unwrap();
        assert!(
            (anim.to - expected).abs() < 0.5,
            "the snap must still target the stop nearest its origin \
             ({expected}); got {}",
            anim.to
        );
        row.tick_snap_at(Instant::now() + Duration::from_millis(400));
        assert!(
            (row.scroll_x() - expected).abs() < 0.5,
            "the snap must settle on its original target ({expected}); \
             got {}",
            row.scroll_x()
        );
    }

    /// Rebuilds a full taffy tree and lays `row` out on it — mimics
    /// `App`'s `RedrawRequested` path after a structural mutation, so a
    /// group spliced in by the previous drop actually gets a rect
    /// before the test drives its next interaction.
    fn relayout_via_new_taffy_tree(row: &mut GroupRow) -> (TaffyTree<()>, NodeId) {
        use taffy::AvailableSpace;
        let theme = Theme::dark();
        let mut new_taffy: TaffyTree<()> = TaffyTree::new();
        let mut new_node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let new_root_child = row.build_layout(&mut new_taffy, &theme, &mut new_node_map);
        let new_root = new_taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(2000.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[new_root_child],
        );
        new_taffy
            .compute_layout_with_measure(
                new_root,
                taffy::Size {
                    width: AvailableSpace::Definite(2000.0),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        row.apply_layout(&new_taffy, new_root_child, Point::ORIGIN);
        (new_taffy, new_root_child)
    }

    /// Two detach+drop sequences in a row, to catch a freeze at the
    /// `GroupRow` level. If it hangs or panics, the bug is here; if it
    /// passes, look one layer up (`App` tick, taffy rebuild).
    #[test]
    fn two_sequential_splits_do_not_hang() {
        // One 4-tab group, matching the live shell's bootstrap_default
        // Home workspace.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2, 3, 4]]);

        // DROP #1: tab 4 dropped right of the only group's bar → 2nd group.
        let g0_rect = row.groups[0].get_rect();
        let release_1 = Point::new(g0_rect.x1 + 30.0, g0_rect.y0 + 6.0);
        row.groups[0].bar_mut_for_tests().pending_detach = Some((4, release_1));

        let modifiers = winit::event::Modifiers::default();
        let mut state = automerge::AutoCommit::new();
        let root_obj = automerge::ROOT;
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        // Mimic App's RedrawRequested path so the new group gets a rect.
        assert_eq!(row.groups.len(), 2, "drop 1 must produce 2 groups");
        assert!(
            row.take_structure_dirty(),
            "split must flag structure_dirty"
        );
        let (taffy, root) = relayout_via_new_taffy_tree(&mut row);

        // DROP #2: tab 3 dropped into the gap between groups 0/1 → 3rd group.
        let g0_rect = row.groups[0].get_rect();
        let g1_rect = row.groups[1].get_rect();
        eprintln!("after drop 1: g0={g0_rect:?} g1={g1_rect:?}");
        eprintln!("  g0 bar_rect={:?}", row.groups[0].bar_rect());
        eprintln!("  g1 bar_rect={:?}", row.groups[1].bar_rect());
        eprintln!("  row.rect={:?}", row.rect);
        let gap_mid = f64::midpoint(g0_rect.x1, g1_rect.x0);
        let release_2 = Point::new(gap_mid, g0_rect.y0 + 6.0);
        eprintln!("  release_2={release_2:?}");
        eprintln!(
            "  gap_index_for(release_2)={:?}",
            row.gap_index_for(release_2)
        );
        row.groups[0].bar_mut_for_tests().pending_detach = Some((3, release_2));

        let mut handled2 = false;
        row.handle_event(
            &winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled2,
            &mut focus,
            root,
            &taffy,
        );
        assert_eq!(
            row.groups.len(),
            3,
            "drop 2 must produce 3 groups (was {})",
            row.groups.len()
        );
    }

    // ==================================================================
    // Inter-group `ResizeHandle` integration
    // ==================================================================

    /// Pins the `build_layout` resize policy so a refactor can't
    /// silently leak or under-allocate handles.
    #[test]
    fn handle_pool_matches_inter_group_gap_count() {
        let row = make_row_with_groups(&[&[1]]);
        assert_eq!(row.handle_count(), 0, "single-group row needs no handle");

        let row = make_row_with_groups(&[&[1], &[2]]);
        assert_eq!(
            row.handle_count(),
            1,
            "two-group row has exactly one handle"
        );

        let row = make_row_with_groups(&[&[1], &[2], &[3]]);
        assert_eq!(
            row.handle_count(),
            2,
            "three-group row has exactly two handles"
        );
    }

    #[test]
    fn handle_pool_grows_on_split_into_new_group() {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize};

        let mut row = make_row_with_groups(&[&[1, 2]]);
        assert_eq!(
            row.handle_count(),
            0,
            "precondition: one-group row has no handle"
        );

        assert!(row.split_tab_into_new_group(0, 2, 1));
        assert_eq!(row.groups.len(), 2);
        assert!(
            row.take_structure_dirty(),
            "split must flag structure_dirty"
        );

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(2000.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(2000.0),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        row.apply_layout(&taffy, root, Point::ORIGIN);
        assert_eq!(
            row.handle_count(),
            1,
            "after split: pool must grow to groups.len() - 1",
        );
        let handle = row.handle_at(0).expect("handle 0 must exist");
        let r = handle.get_rect();
        assert!(
            r.width() > 0.0,
            "handle rect width must be positive after apply_layout"
        );
        assert!(
            r.height() > 0.0,
            "handle rect height must be positive after apply_layout"
        );
    }

    /// Shared `row.handle_event` shape for the drag-handle tests below
    /// (fixed scale factor, a caller-owned scratch `handled` flag).
    /// Returns whether the event was claimed.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event's own shape 1:1 for a test dispatcher"
    )]
    fn dispatch_row(
        row: &mut GroupRow,
        event: &winit::event::WindowEvent,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus: &mut Option<NodeId>,
        root: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        row.handle_event(
            event,
            1.0,
            &modifiers,
            state,
            path,
            &mut handled,
            focus,
            root,
            taffy,
        );
        handled
    }

    /// Build a two-group row fixture, hover its resize handle open, and
    /// press to start a drag — the shared prologue for
    /// `drag_handle_resizes_both_neighbours_via_set_width`. Split out to
    /// keep that under the line cap.
    #[allow(
        clippy::type_complexity,
        reason = "test fixture bundling everything the drag-continuation needs; a named struct would only move the same fields, not reduce them"
    )]
    fn start_handle_drag() -> (
        GroupRow,
        TaffyTree<()>,
        NodeId,
        f64,
        f64,
        winit::event::DeviceId,
        Modifiers,
        AutoCommit,
        ObjId,
        Option<NodeId>,
    ) {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        assert_eq!(row.handle_count(), 1);

        // `WindowWidth::Normal` is two thirds of the fixture's 1200px
        // display, i.e. 800px.
        assert_eq!(row.groups[0].resolved_width_px(), 800.0);
        assert_eq!(row.groups[1].resolved_width_px(), 800.0);

        let handle_rect = row.handle_at(0).unwrap().get_rect();
        let cx = f64::midpoint(handle_rect.x0, handle_rect.x1);
        let cy = f64::midpoint(handle_rect.y0, handle_rect.y1);

        let dev = unsafe { std::mem::zeroed() };
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut focus: Option<NodeId> = None;

        // CursorMoved over the handle → hover engages.
        dispatch_row(
            &mut row,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            modifiers,
            &mut state,
            &path,
            &mut focus,
            root,
            &taffy,
        );
        assert!(
            row.handle_at(0).unwrap().is_hovered(),
            "cursor over handle hit area must engage hover",
        );

        // MouseInput::Pressed → drag starts.
        let handled = dispatch_row(
            &mut row,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            modifiers,
            &mut state,
            &path,
            &mut focus,
            root,
            &taffy,
        );
        assert!(handled, "press on handle must claim the event");
        assert!(row.handle_at(0).unwrap().is_dragging(), "press starts drag");

        (row, taffy, root, cx, cy, dev, modifiers, state, path, focus)
    }

    /// End-to-end press-drag-release exercising the actual production
    /// path the user's mouse drives, through to the shell-persisted
    /// width-change stamp.
    #[test]
    fn drag_handle_resizes_both_neighbours_via_set_width() {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root, cx, cy, dev, modifiers, mut state, path, mut focus) =
            start_handle_drag();

        // CursorMoved +50 px right → width pulse applied to both
        // neighbours via `set_width`.
        dispatch_row(
            &mut row,
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx + 50.0, cy),
            },
            modifiers,
            &mut state,
            &path,
            &mut focus,
            root,
            &taffy,
        );
        assert_eq!(
            row.groups[0].resolved_width_px(),
            850.0,
            "left neighbour grew by exactly dx",
        );
        assert_eq!(
            row.groups[1].resolved_width_px(),
            750.0,
            "right neighbour shrank by exactly dx",
        );

        assert_eq!(
            row.groups[0].take_pending_width_change(),
            None,
            "mid-drag does NOT pulse pending_width_change",
        );
        assert_eq!(row.groups[1].take_pending_width_change(), None);

        let handled = dispatch_row(
            &mut row,
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
            modifiers,
            &mut state,
            &path,
            &mut focus,
            root,
            &taffy,
        );
        assert!(handled, "release on handle must claim the event");
        assert!(
            !row.handle_at(0).unwrap().is_dragging(),
            "release ends drag"
        );
        assert_eq!(
            row.groups[0].take_pending_width_change(),
            Some(708),
            "release stamps left neighbour's persisted width as a fraction \
             of its display (850/1200)",
        );
        assert_eq!(
            row.groups[1].take_pending_width_change(),
            Some(625),
            "release stamps right neighbour's persisted width as a fraction \
             of its display (750/1200)",
        );
    }

    /// Equivalent to `compute_widths_clamps_left_at_min` but driven
    /// through the row's full event dispatch path.
    #[test]
    fn drag_clamps_left_neighbour_at_minimum_width() {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // `Narrow` = a third of the 1200px display = 400px each; from two
        // `Normal`s the left neighbour couldn't reach the minimum at all,
        // since the right would have to grow past the display.
        row.groups[0].set_width(WindowWidth::Narrow);
        row.groups[1].set_width(WindowWidth::Narrow);
        drop((taffy, root));
        let (mut row, taffy, root) = relayout_row_owned(row, 2000.0);

        let handle_rect = row.handle_at(0).unwrap().get_rect();
        let cx = f64::midpoint(handle_rect.x0, handle_rect.x1);
        let cy = f64::midpoint(handle_rect.y0, handle_rect.y1);

        let dev = unsafe { std::mem::zeroed() };
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;

        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            1.0,
            &modifiers,
            &mut state,
            &path,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        row.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &modifiers,
            &mut state,
            &path,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        // Drag 500px left: 400 - 500 = -100, below `min_group_px(1200) =
        // 240`. Clamp pins left at 240; right absorbs only the actual
        // 160px moved, settling at 560.
        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx - 500.0, cy),
            },
            1.0,
            &modifiers,
            &mut state,
            &path,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        let min = min_group_px(FIXTURE_USABLE_PX);
        assert_eq!(
            row.groups[0].resolved_width_px(),
            min,
            "left neighbour must clamp at the display's minimum group width \
             ({min}), not shrink past it",
        );
        assert_eq!(
            row.groups[1].resolved_width_px(),
            800.0 - min,
            "right neighbour absorbs only the actual movement, not the requested 500",
        );
        assert_eq!(
            row.groups[0].resolved_width_px() + row.groups[1].resolved_width_px(),
            800.0,
            "pair sum is preserved across the clamp",
        );
    }

    /// Otherwise a fast drag past the gap centre could accidentally
    /// start a resize on the same frame.
    #[test]
    fn handle_does_not_engage_during_tab_drag_in_flight() {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        assert!(row.groups[0].drag_in_flight());

        let handle_rect = row.handle_at(0).unwrap().get_rect();
        let cx = f64::midpoint(handle_rect.x0, handle_rect.x1);
        let cy = f64::midpoint(handle_rect.y0, handle_rect.y1);

        let dev = unsafe { std::mem::zeroed() };
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;

        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            1.0,
            &modifiers,
            &mut state,
            &path,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        assert!(
            !row.handle_at(0).unwrap().is_hovered(),
            "tab drag in flight must inhibit handle hover (cross-group drop owns the gap)",
        );
    }

    #[test]
    fn maximized_group_hides_siblings_and_absorbs_row_width() {
        use taffy::AvailableSpace;

        // Three groups verifies BOTH a left-of-max and right-of-max
        // sibling collapse, not just one.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        assert!(row.groups[0].get_rect().width() > 0.0);
        assert!(row.groups[1].get_rect().width() > 0.0);
        assert!(row.groups[2].get_rect().width() > 0.0);
        assert_eq!(row.handle_count(), 2, "two gaps → two handles pre-maximize");

        // Mirrors what the App does after `take_structure_dirty()`.
        row.groups[1].maximized = true;
        let theme = Theme::dark();
        let mut taffy_new: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let new_root = row.build_layout(&mut taffy_new, &theme, &mut node_map);
        let viewport = taffy_new.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(2000.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[new_root],
        );
        taffy_new
            .compute_layout_with_measure(
                viewport,
                taffy::Size {
                    width: AvailableSpace::Definite(2000.0),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy_new, new_root, Point::ORIGIN);
        let _ = taffy; // silence unused-binding lint from the helper.
        let _ = root;

        let r0 = row.groups[0].get_rect();
        let r2 = row.groups[2].get_rect();
        assert!(
            r0.width() <= 0.5,
            "left sibling must collapse to zero width when a peer is maximized; got {}",
            r0.width(),
        );
        assert!(
            r2.width() <= 0.5,
            "right sibling must collapse to zero width when a peer is maximized; got {}",
            r2.width(),
        );

        // Small fp tolerance for taffy rounding.
        let r1 = row.groups[1].get_rect();
        let row_w = row.rect.width();
        assert!(
            (r1.width() - row_w).abs() < 1.0,
            "maximized group must absorb the row's full width; got group={} row={}",
            r1.width(),
            row_w,
        );

        // Pool itself stays sized to N-1; `handle_node_ids` must be
        // empty so no draw/dispatch runs on collapsed gaps.
        assert_eq!(
            row.handle_node_ids.len(),
            0,
            "no handle taffy nodes must be built while a group is maximized",
        );
    }

    #[test]
    fn unmaximize_restores_siblings_and_handles() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[1].maximized = true;
        let theme = Theme::dark();
        let relayout = |row: &mut GroupRow| -> TaffyTree<()> {
            use taffy::AvailableSpace;

            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let root = row.build_layout(&mut taffy, &theme, &mut node_map);
            let viewport = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[root],
            );
            taffy
                .compute_layout_with_measure(
                    viewport,
                    taffy::Size {
                        width: AvailableSpace::Definite(2000.0),
                        height: AvailableSpace::Definite(600.0),
                    },
                    |_, _, _, _, _| taffy::Size::ZERO,
                )
                .expect("taffy compute_layout");
            row.apply_layout(&taffy, root, Point::ORIGIN);
            taffy
        };
        let _ = relayout(&mut row);

        row.groups[1].maximized = false;
        let _ = relayout(&mut row);

        for (i, g) in row.groups.iter().enumerate() {
            assert!(
                g.get_rect().width() > 0.0,
                "group {i} must regain positive width after un-maximize",
            );
        }
        assert_eq!(
            row.handle_node_ids.len(),
            2,
            "handle nodes must be rebuilt on un-maximize (2 gaps → 2 handles)",
        );
    }

    /// Shared relayout helper for the fullscreen tests: rebuilds the taffy
    /// tree + runs a full layout pass at a fixed 2000×600 viewport, exactly
    /// like `unmaximize_restores_siblings_and_handles`.
    fn relayout_row(row: &mut GroupRow) {
        use taffy::AvailableSpace;

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(2000.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            },
            &[root],
        );
        taffy
            .compute_layout_with_measure(
                viewport,
                taffy::Size {
                    width: AvailableSpace::Definite(2000.0),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
    }

    /// Same layout outcome as `maximized`, but via `set_fullscreen`
    /// (compositor's live state) with no group carrying `maximized`.
    #[test]
    fn fullscreen_group_collapses_siblings_and_absorbs_row() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        assert!(!row.is_fullscreen());
        // Promote the middle group (id 200).
        row.set_fullscreen(Some(200), false);
        assert!(
            row.groups.iter().all(|g| !g.maximized),
            "no persisted maximize"
        );
        relayout_row(&mut row);

        assert!(
            row.is_fullscreen(),
            "row reports it owns the fullscreen group"
        );
        assert!(
            row.groups[0].get_rect().width() <= 0.5,
            "left sibling collapses under fullscreen; got {}",
            row.groups[0].get_rect().width(),
        );
        assert!(
            row.groups[2].get_rect().width() <= 0.5,
            "right sibling collapses under fullscreen; got {}",
            row.groups[2].get_rect().width(),
        );
        // The wheel gutter is dropped because the sidebar is suppressed.
        let r1 = row.groups[1].get_rect();
        assert!(
            (r1.width() - row.rect.width()).abs() < 1.0,
            "fullscreen group absorbs the row width; got {} vs {}",
            r1.width(),
            row.rect.width(),
        );
        assert!(
            (r1.x0 - row.rect.x0).abs() < 1.0,
            "fullscreen group pins flush-left with no wheel gutter; got x0 {} vs {}",
            r1.x0,
            row.rect.x0,
        );
        assert_eq!(
            row.handle_node_ids.len(),
            0,
            "no resize handles under fullscreen"
        );
    }

    #[test]
    fn exiting_fullscreen_restores_prior_layout_exactly() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let before: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
        let handles_before = row.handle_node_ids.len();
        assert_eq!(handles_before, 2);

        row.set_fullscreen(Some(300), false);
        relayout_row(&mut row);
        assert!(row.is_fullscreen());
        row.set_fullscreen(None, false);
        relayout_row(&mut row);

        assert!(!row.is_fullscreen(), "exit clears the fullscreen promotion");
        let after: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
        for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
            assert!(
                (b.x0 - a.x0).abs() < 0.5
                    && (b.x1 - a.x1).abs() < 0.5
                    && (b.width() - a.width()).abs() < 0.5,
                "group {i} rect must restore exactly: before {b:?} after {a:?}",
            );
        }
        assert_eq!(
            row.handle_node_ids.len(),
            2,
            "resize handles rebuilt on fullscreen exit",
        );
    }

    #[test]
    fn hidden_row_collapses_and_restores() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let before: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
        assert!(before.iter().all(|r| r.width() > 0.0));

        row.set_fullscreen(None, true);
        relayout_row(&mut row);
        for (i, g) in row.groups.iter().enumerate() {
            let r = g.get_rect();
            assert!(
                r.width() <= 0.5 && r.height() <= 0.5,
                "group {i} in a hidden strip must collapse; got {r:?}",
            );
        }

        row.set_fullscreen(None, false);
        relayout_row(&mut row);
        let after: Vec<Rect> = row.groups.iter().map(WindowGroup::get_rect).collect();
        for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
            assert!(
                (b.width() - a.width()).abs() < 0.5,
                "group {i} width restores after un-hide: before {} after {}",
                b.width(),
                a.width(),
            );
        }
    }

    // A strip has unlimited space to the right, so fitting a new window
    // between two others means moving the ones on its right along —
    // never squeezing anybody.

    #[test]
    fn making_room_translates_the_groups_it_pushes_and_never_narrows_them() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let before: Vec<(u64, Rect)> = row.groups.iter().map(|g| (g.id, g.get_rect())).collect();

        row.insert_group(make_group(999, &[9]), 1);
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);

        let fresh = row
            .groups
            .iter()
            .find(|g| g.id == 999)
            .expect("the inserted group is in the row");
        let travel = fresh.get_rect().width() + HANDLE_SLOT_PX;
        assert!(travel > 1.0, "fixture: the new window has a real width");

        for (idx, (id, was)) in before.iter().enumerate() {
            let now = row
                .groups
                .iter()
                .find(|g| g.id == *id)
                .expect("no pre-existing group may disappear")
                .get_rect();
            assert!(
                (now.width() - was.width()).abs() < 0.5,
                "group {id} must keep its width when room is made for \
                 another: was {} now {}",
                was.width(),
                now.width(),
            );
            let expected_dx = if idx == 0 { 0.0 } else { travel };
            assert!(
                (now.x0 - was.x0 - expected_dx).abs() < 0.5,
                "group {id} must translate by exactly the new window's \
                 footprint ({expected_dx}), not by some compressed \
                 fraction of it: moved {}",
                now.x0 - was.x0,
            );
        }
    }

    /// And the ones that moved are *seen* moving: they paint where they
    /// used to be and travel to where they now belong.
    #[test]
    fn the_groups_that_moved_start_from_where_they_were() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let unmoved = row.groups[0].id;
        let pushed: Vec<u64> = row.groups[1..].iter().map(|g| g.id).collect();

        row.insert_group(make_group(999, &[9]), 1);
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
        let travel = row
            .groups
            .iter()
            .find(|g| g.id == 999)
            .expect("inserted group")
            .get_rect()
            .width()
            + HANDLE_SLOT_PX;

        assert_eq!(
            row.shift_offset_for(unmoved),
            0.0,
            "a group that did not move must not be animated",
        );
        for id in pushed {
            let off = row.shift_offset_for(id);
            assert!(
                off < 0.0,
                "group {id} must start left of its new home, i.e. where it \
                 was; got {off}",
            );
            assert!(
                off >= -travel - 0.5 && off.abs() > travel * 0.5,
                "group {id} must start at (about) the position it had — a \
                 travel of {travel} px; got {off}",
            );
        }
    }

    #[test]
    fn appending_at_the_end_animates_nothing() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let existing: Vec<u64> = row.groups.iter().map(|g| g.id).collect();

        let at_end = row.groups.len();
        row.insert_group(make_group(999, &[9]), at_end);
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);

        for id in existing {
            assert_eq!(
                row.shift_offset_for(id),
                0.0,
                "nothing had to move, so group {id} must not animate",
            );
        }
        assert_eq!(row.shift_anim_t(), 1.0, "and no slide is in flight");
    }

    #[test]
    fn a_slide_in_flight_asks_for_animation_frames() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        assert!(
            !row.needs_animation_frame(),
            "fixture: an idle strip is idle",
        );
        row.insert_group(make_group(999, &[9]), 0);
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
        assert!(
            row.needs_animation_frame(),
            "a strip making room must keep redrawing until it settles",
        );
    }
}
