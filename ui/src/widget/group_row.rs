//! `GroupRow` — a horizontal flex container of `WindowGroup`s that also
//! routes cross-group tab drag-drop.
//!
//! Replacing the generic `Row` for the "groups inside a strip" layer lets
//! us intercept release events *after* every child has had a chance to
//! drain its own `TabBar`. If any child published a `pending_detach` (set
//! by `TabBar` when a drag gesture ends outside the bar's own rect), we
//! hit-test the release point against sibling bars and — if one matches —
//! splice the detached tab + its `TileHost` into the target group.
//!
//! Within-group reorder continues to live inside `TabBar` / `WindowGroup`;
//! this widget only adds the between-groups edge of the drag-drop story.

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
use crate::widget::TileContent;
use crate::widget::{
    DEFAULT_STRIP_USABLE_PX, HANDLE_SLOT_PX, ResizeHandle, TabDescriptor, TileHost, Widget,
    WindowGroup, WindowWidth,
};

/// Idle threshold after the last wheel event before we kick off a
/// snap-to-nearest-group animation. Mirrors the concept's
/// `snap-x snap-proximity` heuristic in `DesktopView.tsx:396` —
/// while the user is actively wheeling we don't snap; once they stop,
/// the strip eases to the nearest group's left edge.
const SNAP_IDLE_MS: u64 = 200;
/// Duration of the snap animation itself (cubic-out ease). Matches the
/// 200 ms feel the spawn animation in `window_group.rs` uses for
/// secondary motions — long enough to read as eased, short enough to
/// stay out of the user's way if they want to wheel again.
const SNAP_ANIM_MS: u64 = 200;
/// Duration of the "make space" slide played on the groups a newly
/// inserted one pushes along. Deliberately the same 420 ms cubic-out as
/// `window_group.rs`'s spawn fade, which runs on the new group at the
/// same moment: the neighbours travelling and the new window arriving
/// are one motion, and two curves of different lengths would read as
/// two events.
const MAKE_SPACE_ANIM_MS: u64 = 420;

/// Touchpad travel that equals one wheel detent for the discrete jump
/// gestures (strip group jump, shell workspace switch) — one jump per
/// this many pixels of accumulated scroll. Also the scale for hi-res
/// wheels whose notches the host session reports as pixel deltas.
pub const JUMP_PX_PER_DETENT: f64 = 50.0;

/// Turns a stream of scroll deltas (in fractional detents) into whole
/// discrete steps: the Niri wheel-bind feel, where a clicky wheel fires
/// once per notch and a touchpad fires once per [`JUMP_PX_PER_DETENT`]
/// of accumulated travel. Reversing direction drops the partial
/// balance. Deliberately NO quiet-time decay: real hardware (hi-res
/// wheels, touchpads) delivers fractional detents with arbitrary
/// pauses, and a decay meant slow deliberate scrolling could never
/// reach a whole detent — observed live as "the jump works once, then
/// I can not scroll further". A stale sub-detent balance is harmless:
/// the flip reset clears it the moment the user scrolls the other way,
/// and same-direction leftovers only make the next jump come a hair
/// sooner. Shared by the shell's modifier-wheel gestures so every
/// consumer counts detents the same way.
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
        whole as i32
    }
}

/// Scroll delta → fractional detents for the jump gestures, on the same
/// axis promotion the strip has always used (vertical wheel promoted to
/// horizontal travel; winit's positive-y-is-up flipped). So wheel-DOWN
/// yields a positive detent = jump RIGHT / next, matching Niri's
/// `Mod+Shift+WheelScrollDown → focus-column-right`.
pub fn wheel_jump_detents(delta: &MouseScrollDelta) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (-x - y) as f64,
        MouseScrollDelta::PixelDelta(pp) => (-pp.x - pp.y) / JUMP_PX_PER_DETENT,
    }
}

/// In-flight "make space" motion. `offsets` is how far *left* of its new
/// home each affected group started, keyed by group id, so the eased
/// paint is `new_x + offset * (1 - t)` — the group renders where it used
/// to be and travels to where it now belongs.
#[derive(Clone, Debug)]
struct ShiftAnim {
    started: Instant,
    offsets: Vec<(u64, f64)>,
}

/// In-flight snap state: from `from` to `to` over `SNAP_ANIM_MS`,
/// started at `started`. Cleared when the curve settles or the user
/// starts wheeling again (a fresh wheel event resets `last_wheel_at`,
/// and the next `tick_snap` call notices the snap target no longer
/// matches the current scroll target and rebases).
#[derive(Clone, Copy, Debug)]
struct SnapAnim {
    from: f64,
    to: f64,
    started: Instant,
}

/// Pointer displacement (logical px) a press on a group's tab-bar
/// background must exceed before it's treated as a whole-group reorder
/// drag rather than a plain click. Below the threshold the gesture stays
/// a click (focus the group); past it the row starts moving the group and
/// paints the drop indicator. Matches the feel of the tab-detach
/// threshold so grabbing a group's header and jiggling doesn't yank it out
/// of place.
const GROUP_DRAG_THRESHOLD: f64 = 6.0;

/// In-flight whole-group reorder drag. Set when a press lands on a
/// group's tab-bar background (its "titlebar" — see
/// [`WindowGroup::bar_grab_hit`]) and cleared on release. `src_idx` is the
/// group's index in `groups` at grab time; `start` is the press point, used
/// to gate the click-vs-drag threshold. `moved` latches once the pointer
/// travels past [`GROUP_DRAG_THRESHOLD`], at which point the row claims the
/// motion events and paints the drop indicator.
#[derive(Clone, Copy, Debug)]
struct GroupDrag {
    src_idx: usize,
    group_id: u64,
    start: Point,
    moved: bool,
}

/// Payload published by [`GroupRow::take_pending_cross_strip_detach`] when
/// a tab drag ends outside the row's own vertical band — the shell-side
/// harvester consumes it, hit-tests the release point against other strips'
/// bounds, and either delivers the payload via
/// [`GroupRow::accept_cross_strip_drop`] on the destination row or returns
/// it to the source row on a miss.
///
/// The `(TabDescriptor, TileHost)` pair is the same shape
/// [`WindowGroup::detach_tab`] returns, so the source group has *already*
/// been mutated by the time this is published — the shell either splices
/// the payload into a destination row or returns it to the source via
/// [`GroupRow::accept_cross_strip_drop`].
pub struct CrossStripDetach {
    pub tab_id: u64,
    pub desc: TabDescriptor,
    pub host: TileHost,
    pub release_point: Point,
}

pub struct GroupRow {
    pub groups: Vec<WindowGroup>,
    /// Inter-group resize handles — one per gap, so for N groups we
    /// keep `max(0, N - 1)` handles. Sized by `build_layout` to match
    /// the current group count; each handle's neighbour rects are
    /// refreshed by `apply_layout` after `WindowGroup::apply_layout`
    /// has populated screen-space coordinates. Held inline (not as
    /// children-managed boxes) so the widget tree's lifetime
    /// invariants stay simple — handles only exist while their two
    /// neighbours both exist, which the row's structural mutations
    /// already enforce via `structure_dirty`.
    handles: Vec<ResizeHandle>,
    /// Per-handle taffy node ids, captured during `build_layout` so
    /// `apply_layout` and `handle_event` can dispatch in order. Always
    /// `handles.len()` entries when populated.
    handle_node_ids: Vec<NodeId>,
    rect: Rect,
    /// Per-child taffy node ids, captured during `build_layout` so
    /// `apply_layout` can walk them back out in order.
    node_ids: Vec<NodeId>,
    /// Last seen cursor position in logical coords, used so a release
    /// that went to a sibling's bar (and therefore set `handled=true` on
    /// the target) still has a known release point to drive our
    /// post-dispatch detach sweep.
    last_cursor: Point,
    /// Set when this row's `self.groups` vector is mutated structurally
    /// (e.g. a tab is split into a brand-new `WindowGroup`). The App's
    /// tick handler polls via `take_structure_dirty` and rebuilds the
    /// taffy tree so the new group participates in layout / draw. Without
    /// this signal, `apply_layout` would walk `node_ids` that don't cover
    /// the new group and skip it entirely.
    structure_dirty: bool,
    /// Index of the inter-group gap the cursor is currently hovering over
    /// while a child `TabBar` has a drag in flight. `None` when no drag
    /// is active or the cursor is inside a group's own rect. `draw` reads
    /// this to paint a single indigo capsule at the hovered gap so the
    /// user sees "drop here" feedback before releasing. Cleared on mouse
    /// release regardless of the drop resolution, and whenever no child
    /// reports `drag_in_flight()` so stale indicators don't persist past
    /// the gesture that set them.
    hover_gap_idx: Option<usize>,
    /// Horizontal scroll offset in logical pixels. The strip's natural
    /// content width (sum of group widths + gaps) can exceed the row's
    /// viewport-aligned `rect.width()`; `scroll_x` shifts every child's
    /// rendered x by `-scroll_x` so the user can pan groups that would
    /// otherwise sit off the right edge into view. Mirrors the concept's
    /// `flex-1 overflow-x-auto` strip in `DesktopView.tsx:396`. Updated
    /// by mouse-wheel events while the cursor is over the row's rect
    /// (vertical wheel pans horizontally — Niri-style global tabbing).
    /// Clamped to `[0, max(0, content_w - viewport_w)]` after every
    /// layout pass so a structural mutation that shrinks the content
    /// (e.g. closing a group) auto-snaps the view back into bounds.
    scroll_x: f64,
    /// Total content width including inter-group gaps, captured each
    /// `apply_layout` so `handle_event`'s wheel handler can clamp
    /// `scroll_x` without re-summing children.
    content_w: f64,
    /// Per-group local x-positions (relative to the row's content origin),
    /// captured during `apply_layout`. These are the snap targets — the
    /// strip eases `scroll_x` to whichever entry is nearest after a
    /// wheel-idle window. Re-built every layout pass so a structural
    /// mutation that inserts / removes / reorders groups produces the
    /// updated set of legal snap stops automatically.
    group_left_edges: Vec<f64>,
    /// Timestamp of the most recent `MouseWheel` event handled by this
    /// row. `tick_snap` waits for `SNAP_IDLE_MS` of quiet on this
    /// timestamp before kicking off the snap animation; a fresh wheel
    /// event during a snap cancels the in-flight curve (the timestamp
    /// jumps forward, the idle gate fails again, and the existing snap
    /// is dropped on the next tick).
    last_wheel_at: Option<Instant>,
    /// In-flight snap animation, if any. `None` when the strip is at
    /// rest or actively being wheeled. `Some` after the idle gate has
    /// fired and we've committed to a target group's left edge.
    snap_anim: Option<SnapAnim>,
    /// Tab payload waiting to be routed across strips by the shell-side
    /// harvester. Set by the post-dispatch detach sweep when a release
    /// landed outside the row's own vertical band — too far for the
    /// in-row gap-drop / sibling-bar resolution to be the right answer,
    /// because the user is plausibly aiming at a different strip's
    /// `GroupRow`. The shell drains this each frame via
    /// [`Self::take_pending_cross_strip_detach`], hit-tests the release
    /// point against every other strip's row rect, and either delivers
    /// the payload to a destination row via
    /// [`Self::accept_cross_strip_drop`] or returns it to the source row.
    ///
    /// `None` whenever the most recent drag either resolved within this
    /// row or hasn't fired yet. The take-pattern accessor leaves the slot
    /// empty so a subsequent harvester call doesn't double-deliver.
    pending_cross_strip_detach: Option<CrossStripDetach>,
    /// `true` whenever `scroll_x` has changed since the last `draw`.
    /// The parent Column / Stack caches its subtree scene and only
    /// rebuilds when a child reports `needs_redraw() == true`. Without
    /// this flag, a wheel-pan updates `scroll_x` but the cached scene
    /// keeps painting the old child positions — the user sees no
    /// movement (exactly the "I scroll and nothing happens" bug from
    /// 2026-06-02). Cleared in `draw` once the new scene is recorded.
    scroll_dirty: bool,
    /// Horizontal canvas-x of the first group. The strip's
    /// "canvas" goes [0, leading_inset, leading_inset+g0_w, ...].
    /// When zero (default) the first group sits flush against
    /// `rect.x0`; the shell sets this to the wheel-disc width so
    /// the first group leaves room for the overlay-drawn wheel to
    /// pan into and out of off-screen-left territory in lock-step
    /// with the strip itself. `apply_layout` translates every
    /// child's screen-space origin by `rect.x0 + leading_inset -
    /// scroll_x`, so the leading_inset moves with `scroll_x` just
    /// like every other point in canvas space.
    leading_inset: f64,
    /// **Transient fullscreen override** (id of the group promoted to occupy
    /// the whole output, e.g. a slideshow that requested
    /// `xdg_toplevel.set_fullscreen`). Set by the shell each frame from the
    /// compositor's live fullscreen state — NOT persisted, NOT part of the
    /// `WorkspaceTree`. When `Some(id)` and this row contains that group, the
    /// row collapses to that single group at full area exactly like a
    /// [`WindowGroup::maximized`] group, but the shell additionally suppresses
    /// the top bar / sidebar so the promotion covers the entire output. When
    /// the id belongs to a *different* strip, [`Self::hidden`] is set instead.
    /// Cleared (both back to their defaults) restores the exact prior layout.
    fullscreen_group: Option<u64>,
    /// **Transient full-output suppression.** Set by the shell on every strip
    /// row that does NOT own the current fullscreen group, so the fullscreen
    /// surface is the only thing painted over the output. `build_layout`
    /// collapses the whole row to `Display::None` while set. Not persisted.
    hidden: bool,
    /// In-flight whole-group reorder drag (grab a group's tab-bar
    /// background and slide it to a new slot). `None` when no group is
    /// being dragged. Distinct from a tab-detach drag (which lives inside
    /// a child `TabBar` and moves a single tab) and from an inter-group
    /// resize drag (which changes widths, not order).
    group_drag: Option<GroupDrag>,
    /// While a group reorder drag is past its threshold, the insertion
    /// gap index the cursor currently targets (`0..=groups.len()`). `draw`
    /// reads it to paint the drop indicator so the user sees where the
    /// group will land before releasing. Cleared on release.
    group_reorder_target: Option<usize>,
    /// One-shot published when a group reorder drag commits: `(group_id,
    /// new_index)` where `new_index` is the group's landing slot in the
    /// post-move `groups` vector. The shell drains it each frame via
    /// [`Self::take_pending_group_reorder`] and mirrors the move into the
    /// persisted [`WorkspaceTree`] so the new order survives a restart and
    /// replicates to peers. Take-pattern (leaves `None`) so a later
    /// harvest tick can't double-apply.
    pending_group_reorder: Option<(u64, usize)>,
    /// The displays this strip spans, in the compositor's combined
    /// coordinate space, each carrying the width its window groups
    /// resolve their [`WindowWidth`] fractions against. Pushed down by the
    /// shell before every `build_layout`; empty in a bare `ui` host, which
    /// falls back to the row's own geometry (see [`Self::usable_at`]).
    ///
    /// A strip can span more than one display — the compositor paints one
    /// scene across the bounding box of every output — so a group's width
    /// has to resolve against the display that group sits on. Resolving
    /// against the combined desktop would make a full-width window span
    /// both monitors.
    output_spans: Vec<OutputSpan>,
    /// Every group's taffy-local x as of the most recent layout pass,
    /// keyed by group id. The "where it used to be" half of the
    /// make-space slide. Taffy-local rather than screen-space on
    /// purpose: panning the strip moves every group on screen, and a
    /// pan is not something to animate away from.
    last_local_x: HashMap<u64, f64>,
    /// Set by [`Self::insert_group`] when the insertion pushes existing
    /// groups along. The next layout pass turns the positions they had
    /// into the offsets they travel from, and clears this.
    shift_armed: bool,
    /// The make-space slide currently playing, if any.
    shift_anim: Option<ShiftAnim>,
    /// When set, the point on the curve [`Self::shift_anim_t`] reports
    /// instead of reading the clock, and the slide is held against
    /// retirement. Only ever set by [`Self::hold_shift_at`], which the
    /// capture harness uses; always `None` in a running shell.
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
    /// whatever shell chrome the display reserves (on the leftmost
    /// display, the wheel-disc slot). This is the denominator every
    /// [`WindowWidth`] fraction is taken of.
    pub usable: f64,
}

impl GroupRow {
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

    /// Index of the group this row should promote to full-output, if the
    /// current fullscreen group id belongs to this row. Fullscreen takes
    /// precedence over an in-row `maximized` flag (both drive the same
    /// collapse-siblings layout branch, but a live fullscreen request wins).
    fn fullscreen_idx(&self) -> Option<usize> {
        let id = self.fullscreen_group?;
        self.groups.iter().position(|g| g.id == id)
    }

    /// Set (or clear) the transient fullscreen override + full-output
    /// suppression for this strip row. Driven by the shell each frame from the
    /// compositor's live fullscreen state:
    ///   * `fullscreen_group = Some(id)` where `id` is one of *this* row's
    ///     groups → promote it to the whole output (siblings + handles hidden);
    ///   * `hidden = true` → this strip owns no part of the fullscreen surface,
    ///     so collapse the entire row away.
    /// Passing `(None, false)` (the reset) restores the row's normal tiling
    /// layout exactly. Marks the row structurally dirty on a real change so the
    /// host rebuilds the taffy tree and the collapse/restore takes effect.
    pub fn set_fullscreen(&mut self, fullscreen_group: Option<u64>, hidden: bool) {
        if self.fullscreen_group != fullscreen_group || self.hidden != hidden {
            self.fullscreen_group = fullscreen_group;
            self.hidden = hidden;
            self.structure_dirty = true;
        }
    }

    /// Whether this row is currently promoting one of its groups to occupy the
    /// full output. Read by the shell so it knows to suppress the top bar /
    /// sidebar and by tests asserting the enter/exit override.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen_idx().is_some()
    }

    /// Reserve a canvas-x gutter to the left of the first group. The
    /// strip's content origin shifts right by `px` before any group
    /// paints, so the leading gutter pans with `scroll_x` the same
    /// way every group does. The shell sets this to the wheel-disc
    /// width so the wheel (drawn in the overlay layer above the
    /// strip) and the first group move as one when the user pans the
    /// strip horizontally — replacing the old "wheel is fixed,
    /// content pans underneath" hack with a single canvas that the
    /// wheel happens to render the leftmost-96 px of.
    pub fn with_leading_inset(mut self, px: f64) -> Self {
        self.leading_inset = px;
        self
    }

    /// Width of the leading gutter on the strip's canvas. Read by the
    /// shell so the overlay-layer wheel disc can be positioned at the
    /// same canvas-x the strip's `apply_layout` reserved.
    pub fn leading_inset(&self) -> f64 {
        self.leading_inset
    }

    /// Update the leading-inset width at runtime. The wheel disc's
    /// rendered width is viewport-dependent (`sidebar_width_for_
    /// viewport`); the shell's `ShellOverlayHost::apply_layout` calls
    /// this once per frame so the strip's canvas always reserves
    /// exactly the wheel's current width — no overlap when the wheel
    /// is wide, no wallpaper gap when it's narrow.
    pub fn set_leading_inset(&mut self, px: f64) {
        if (self.leading_inset - px).abs() > f64::EPSILON {
            self.leading_inset = px;
            // Treat a leading-inset change like a scroll: the parent
            // Column's subtree cache must rebuild so the new origin
            // takes effect on the next paint.
            self.scroll_dirty = true;
        }
    }

    /// Tell the strip which displays it spans, so each group's width
    /// fraction resolves against the display that group sits on. The
    /// shell pushes this before every `build_layout`; passing an empty
    /// slice restores the fallback in [`Self::usable_at`].
    ///
    /// Flags `structure_dirty` on a real change so a monitor being
    /// plugged in, unplugged or resized re-bakes every group's width.
    pub fn set_output_spans(&mut self, spans: &[OutputSpan]) {
        if self.output_spans != spans {
            self.output_spans = spans.to_vec();
            self.structure_dirty = true;
        }
    }

    /// The displays this strip spans. Empty until the shell pushes them.
    pub fn output_spans(&self) -> &[OutputSpan] {
        &self.output_spans
    }

    /// Usable width a group whose left edge sits at combined-x `x` should
    /// resolve its [`WindowWidth`] fraction against.
    ///
    /// `x` is a *canvas* coordinate — the strip's own unscrolled space —
    /// deliberately, not a screen coordinate: panning the strip must not
    /// resize the windows in it. A group therefore keeps the basis of the
    /// display it is laid out on, whatever is currently scrolled into
    /// view.
    ///
    /// Falls back, in order, to the row's own laid-out width minus the
    /// leading gutter (a bare `ui` host, where the row *is* the display),
    /// and to [`DEFAULT_STRIP_USABLE_PX`] before any layout has run.
    pub fn usable_at(&self, x: f64) -> f64 {
        if !self.output_spans.is_empty() {
            // Last span whose left edge is at or before `x`; the first
            // span for anything left of the whole arrangement.
            let mut chosen = self.output_spans[0];
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

    /// Per-group usable width, in strip order: the width each group's
    /// [`WindowWidth`] fraction resolves against. Walks the strip left to
    /// right accumulating resolved widths, so a group that lands on the
    /// second display gets that display's basis rather than the first's.
    ///
    /// Depends only on the groups' own stored widths and the pushed output
    /// spans — never on a previous frame's layout — so it is stable on the
    /// very first frame.
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

    /// Each group's horizontal extent — `(id, left, right)` — in the
    /// strip's unscrolled canvas space, in strip order.
    ///
    /// Derived the same way [`Self::usable_per_group`] is: from the
    /// groups' own stored widths and the pushed output spans, never from
    /// a previous frame's layout. So a group spliced in this frame
    /// already has a span, which is what lets the shell answer "which
    /// border between windows can the user see" without waiting a frame
    /// for taffy.
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

    /// The slice of the strip's canvas currently on screen, in the same
    /// space [`Self::group_canvas_spans`] reports. A group is on screen
    /// exactly when its span overlaps this one: screen-x is canvas-x
    /// minus [`Self::scroll_x`], so the visible canvas window is the
    /// row's own rect shifted by the pan.
    pub fn visible_canvas_span(&self) -> (f64, f64) {
        (self.rect.x0 + self.scroll_x, self.rect.x1 + self.scroll_x)
    }

    /// Ordered list of group ids currently held by the row. Used by
    /// the shell's `resync_strips_from_tree` (overlay_host) to diff
    /// against the persisted [`shell::state::Strip::window_groups`]
    /// after a `chrome.bump()` so newly-added groups can be spliced
    /// in (or removed groups dropped) without rebuilding the entire
    /// row — preserves existing groups' host state (scroll positions,
    /// per-tile selection, etc.) on every revision bump that doesn't
    /// actually touch them.
    pub fn group_ids(&self) -> Vec<u64> {
        self.groups.iter().map(|g| g.id).collect()
    }

    /// Splice a [`WindowGroup`] into the row at `idx`. Clamps `idx`
    /// to `groups.len()` so callers don't have to bounds-check.
    /// Flags `structure_dirty` so the App's next layout pass
    /// rebuilds the taffy tree with the new child's node id.
    /// Inserting anywhere but the end pushes the groups from `idx`
    /// onwards along to make room. That is the moment the layout has
    /// something to say, so the motion is armed here and measured by the
    /// next layout pass — see [`Self::shift_offset_for`].
    pub fn insert_group(&mut self, group: WindowGroup, idx: usize) {
        let clamped = idx.min(self.groups.len());
        if clamped < self.groups.len() {
            self.shift_armed = true;
        }
        self.groups.insert(clamped, group);
        self.structure_dirty = true;
    }

    /// Eased 0..=1 progress through the make-space slide, or 1.0 when
    /// none is playing.
    pub fn shift_anim_t(&self) -> f64 {
        let Some(anim) = &self.shift_anim else {
            return 1.0;
        };
        let elapsed = self.shift_hold.unwrap_or_else(|| anim.started.elapsed());
        if elapsed >= Duration::from_millis(MAKE_SPACE_ANIM_MS) {
            return 1.0;
        }
        let raw = (elapsed.as_secs_f64() / (MAKE_SPACE_ANIM_MS as f64 / 1000.0)).clamp(0.0, 1.0);
        1.0 - (1.0 - raw).powi(3)
    }

    /// How far left of its laid-out position the group with `group_id`
    /// should be *painted* this frame, as it travels to the place the
    /// insertion moved it to. `0.0` for a group that is not moving, and
    /// for every group once the slide settles.
    ///
    /// The group's stored rect is always its final one, so hit-testing
    /// and the app's own configure size are never mid-flight — the same
    /// choice the per-group spawn fade makes.
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

    /// Freeze the in-flight slide `elapsed` into its curve, and report
    /// whether there was one to freeze.
    ///
    /// The curve runs on wall time, and an offscreen render costs far
    /// more wall time per frame than the whole curve lasts — so a
    /// capture harness stepping frames sees the start and the end and
    /// nothing in between. This lets it ask for a specific point
    /// instead. What it then renders is the production `draw` at that
    /// point, not a reconstruction of it.
    ///
    /// The freeze also holds the slide against the per-layout retirement,
    /// so the harness can walk the whole curve however long each render
    /// takes.
    ///
    /// Deliberately *not* gated on `debug_assertions`: the `shots` binary
    /// walks scenario timelines through this and is built in release for
    /// the OS image, so gating it breaks the image build while every debug
    /// build and test still passes.
    pub fn hold_shift_at(&mut self, elapsed: Duration) -> bool {
        if self.shift_anim.is_none() {
            return false;
        }
        self.shift_hold = Some(elapsed);
        true
    }

    /// Remove the group with id `id`. No-op if absent. Returns
    /// `true` if a group was actually removed (so the caller can
    /// avoid a redundant `structure_dirty` flag set elsewhere).
    /// Flags `structure_dirty` on success so the taffy tree
    /// drops the now-orphaned node id.
    pub fn remove_group_by_id(&mut self, id: u64) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        let removed = self.groups.len() != before;
        if removed {
            self.structure_dirty = true;
        }
        removed
    }

    /// Current horizontal scroll offset in logical pixels. Exposed so
    /// tests can assert `scroll_x` updates after a wheel event without
    /// reaching through private fields.
    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    /// Count of inter-group resize handles currently held by the
    /// row. Exposed so tests can assert the pool stays in lockstep
    /// with `groups.len() - 1` across structural mutations.
    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }

    /// Read-only access to the inter-group resize handle at `idx`.
    /// Returns `None` if the index is out of range. Exposed so
    /// integration tests can assert on hover/drag state without
    /// reaching through private fields.
    pub fn handle_at(&self, idx: usize) -> Option<&ResizeHandle> {
        self.handles.get(idx)
    }

    /// Read-only access to the child [`WindowGroup`] at `idx` — exposed so
    /// integration tests (in `ui/tests/`) can read a group's live
    /// `bar_rect` / `tab_rects` / screen rect to drive hit-tested gestures.
    /// `debug_assertions`-gated so it's available to `ui/tests` (which build
    /// in debug) but not shipped in release.
    #[cfg(any(test, debug_assertions))]
    pub fn group_at_for_test(&self, idx: usize) -> Option<&WindowGroup> {
        self.groups.get(idx)
    }

    /// Total natural content width (sum of group widths + inter-group
    /// gaps), as captured by the most recent `apply_layout`. Returns
    /// `0.0` before the first layout pass.
    pub fn content_width(&self) -> f64 {
        self.content_w
    }

    /// Maximum legal `scroll_x`: zero if the strip's content fits inside
    /// the row's viewport rect, otherwise the overflow distance.
    pub fn max_scroll_x(&self) -> f64 {
        (self.content_w - self.rect.width()).max(0.0)
    }

    /// Pan `scroll_x` so the group with `group_id` is fully inside the
    /// row's viewport — the Niri "focused column follows into view"
    /// behaviour. If the group hangs off the right edge the strip scrolls
    /// right just enough to reveal its right edge; off the left (behind the
    /// leading wheel-disc gutter) it scrolls left to clear it. A no-op when
    /// the group is already visible, not found, or not yet laid out.
    /// Returns `true` if `scroll_x` changed.
    ///
    /// Uses the group's live screen rect (populated by `apply_layout`), so
    /// it must be called after a layout pass — which the shell always has
    /// by the time it reconciles focus. Cancels any in-flight wheel-snap so
    /// the into-view target isn't immediately overridden by a snap-back.
    /// Whether `group_id` has a real laid-out rect in this row — i.e. whether
    /// [`Self::scroll_group_into_view`] can act on it at all. A group focused
    /// in the same frame it was spliced in has not been through a layout pass
    /// yet, so its rect is degenerate and any pan would be a no-op.
    pub fn group_is_laid_out(&self, group_id: u64) -> bool {
        self.groups
            .iter()
            .find(|g| g.id == group_id)
            .is_some_and(|g| g.get_rect().width() > 0.0)
    }

    pub fn scroll_group_into_view(&mut self, group_id: u64) -> bool {
        let Some(sx) = self.into_view_target(group_id) else {
            return false;
        };
        self.scroll_x = sx;
        self.scroll_dirty = true;
        self.snap_anim = None;
        self.last_wheel_at = None;
        true
    }

    /// Animated variant of [`Self::scroll_group_into_view`]: the same pan
    /// target, but eased there on the wheel-snap curve (`SNAP_ANIM_MS`
    /// cubic-out) instead of teleporting. This is what makes a focus jump
    /// (click, Alt+Shift wheel, keyboard) read as motion rather than a
    /// cut — the way Niri's `focus-column-left/right` slides the view to
    /// the newly-focused column. Returns `true` when a pan was started.
    pub fn scroll_group_into_view_animated(&mut self, group_id: u64, now: Instant) -> bool {
        let Some(sx) = self.into_view_target(group_id) else {
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

    /// Shared target math for [`Self::scroll_group_into_view`] and its
    /// animated variant: the `scroll_x` that puts `group_id` fully inside
    /// the viewport, or `None` when the group is missing, not laid out
    /// yet, or already fully visible (nothing to do).
    fn into_view_target(&self, group_id: u64) -> Option<f64> {
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

    /// True while a snap-to-nearest-group animation is in flight, or
    /// while the user just-wheeled (within the idle threshold). The
    /// shell's `needs_animation_frame()` chain reads this so the App
    /// keeps pumping redraws — without that, a wheel-then-idle would
    /// freeze the strip mid-pan and never ease into the snap target.
    fn snap_in_flight(&self) -> bool {
        if self.snap_anim.is_some() {
            return true;
        }
        // Pending snap: user has wheeled but the idle gate hasn't
        // fired yet. Keep redraws pumping so `tick_snap` runs the
        // moment the gate clears, even if no other event arrives.
        if let Some(t) = self.last_wheel_at
            && t.elapsed() < Duration::from_millis(SNAP_IDLE_MS + SNAP_ANIM_MS)
            && !self.is_aligned_to_group()
        {
            return true;
        }
        false
    }

    /// True iff `scroll_x` is within 0.5 px of one of the group left
    /// edges — i.e. the strip is already at rest at a snap stop, so
    /// the idle gate's tick has nothing to do. Used to short-circuit
    /// `snap_in_flight` after the snap settles so we stop pumping
    /// redraws.
    fn is_aligned_to_group(&self) -> bool {
        if self.group_left_edges.is_empty() {
            return true;
        }
        self.group_left_edges
            .iter()
            .any(|&e| (e - self.scroll_x).abs() < 0.5)
    }

    /// Find the snap target nearest to the current `scroll_x`, clamped
    /// into `[0, max_scroll_x()]`. Returns `None` if there are no
    /// groups (nothing to snap to) or if the strip can't actually
    /// scroll (max_scroll_x == 0 — content fits).
    ///
    /// Candidates are every group's canvas-x left edge PLUS canvas-x
    /// = 0 ("wheel visible" home position) when `leading_inset > 0`.
    /// Without the home target a small flick away from 0 would snap
    /// FORWARD to the first group (canvas-x = leading_inset) instead
    /// of relaxing back to the wheel-visible rest state — observably
    /// the strip "bounces away from home" after a tiny wheel flick.
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
        // Walk the explicit per-group edges, then optionally consider
        // the home position. Using `chain` here keeps the loop body a
        // single closure-free expression so the compiler can inline it.
        let home_targets: &[f64] = if self.leading_inset > 0.0 { &[0.0] } else { &[] };
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

    /// Select `tab_id` inside `group_id`, widget-side — the direction
    /// that sticks: the bar's `active_id` is authoritative and the
    /// shell's per-frame harvest mirrors it into the persisted tree, so
    /// a tree-only write would be overwritten one frame later. Used by
    /// the shell's Alt+Shift wheel window jump. Returns `true` when the
    /// active tab changed.
    pub fn set_group_active_tab(&mut self, group_id: u64, tab_id: u64) -> bool {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group_id) else {
            return false;
        };
        if !g.set_active_tab(tab_id) {
            return false;
        }
        // The displayed tile changed under a programmatic jump — force
        // the parent's subtree cache and the taffy tree to rebuild this
        // frame, the same invalidation a click on the pill produces
        // through the event path.
        self.scroll_dirty = true;
        self.structure_dirty = true;
        true
    }

    /// Per-frame snap tick. Called from `apply_layout` so the snap
    /// animation runs whenever the App relayouts (which happens every
    /// frame while `needs_animation_frame()` keeps redraws pumping
    /// AND `take_structure_dirty()` keeps `layout_dirty` true). Split
    /// out from `apply_layout` so headless tests can advance simulated
    /// time deterministically without staging a full taffy tree.
    ///
    /// Behaviour:
    /// - If a snap is in flight, ease `scroll_x` along the cubic-out
    ///   curve from `from` to `to` over `SNAP_ANIM_MS`. Clear the snap
    ///   when the curve settles.
    /// - Otherwise, if `last_wheel_at` is set and at least `SNAP_IDLE_MS`
    ///   has elapsed since the last wheel event, kick off a fresh snap
    ///   targeting the nearest group's left edge.
    pub fn tick_snap_at(&mut self, now: Instant) {
        let before = self.scroll_x;
        // No snap in flight — should we start one? Only if the user
        // actually wheeled (`last_wheel_at` is set) AND has been quiet
        // for at least the idle threshold AND the current scroll_x is
        // not already aligned to a group edge. Kicking off here BEFORE
        // the curve-advance branch lets the same `tick_snap_at` call
        // both start the snap AND apply its first eased frame — the
        // snap's `started` is anchored at `last_wheel_at + SNAP_IDLE_MS`
        // so the eased frame reflects the elapsed time since the idle
        // gate cleared, not since this particular tick fired.
        if self.snap_anim.is_none() {
            if let Some(last_wheel) = self.last_wheel_at {
                let idle = now.saturating_duration_since(last_wheel);
                if idle >= Duration::from_millis(SNAP_IDLE_MS) {
                    if self.is_aligned_to_group() {
                        self.last_wheel_at = None;
                    } else if let Some(target) = self.nearest_group_edge() {
                        if (target - self.scroll_x).abs() < 0.5 {
                            self.scroll_x = target;
                            self.last_wheel_at = None;
                        } else {
                            // Anchor `started` at the moment the idle
                            // gate was met (`last_wheel + SNAP_IDLE_MS`),
                            // not `now`. That way the very tick that
                            // detected idle already applies the curve's
                            // first eased frame for whatever duration
                            // has passed since the gate cleared.
                            let started = last_wheel
                                + Duration::from_millis(SNAP_IDLE_MS);
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
        }
        // Active snap (either pre-existing OR freshly kicked off
        // above): advance the cubic-out curve.
        if let Some(anim) = self.snap_anim {
            let elapsed = now.saturating_duration_since(anim.started);
            let total = Duration::from_millis(SNAP_ANIM_MS);
            if elapsed >= total {
                self.scroll_x = anim.to;
                self.snap_anim = None;
                self.last_wheel_at = None;
            } else {
                let raw = (elapsed.as_secs_f64() / (SNAP_ANIM_MS as f64 / 1000.0))
                    .clamp(0.0, 1.0);
                // Cubic-out ease — same shape the spawn animation in
                // `window_group.rs` uses for its slide-in. Punchy at the
                // start, gentle settle at the end.
                let eased = crate::ease_out(raw);
                self.scroll_x = anim.from + (anim.to - anim.from) * eased;
            }
        }
        // Any tick that actually moved scroll_x must invalidate the
        // parent Column's subtree cache, otherwise the snap curve
        // updates the value but `needs_redraw()` returns false (no
        // `scroll_dirty`, no child-leaf dirty bit) and the parent
        // re-emits its pre-snap cached scene. Symptom: the strip
        // staggers / freezes mid-snap and only paints intermittently
        // when an unrelated dirty source forces a rebuild.
        if (self.scroll_x - before).abs() > f64::EPSILON {
            self.scroll_dirty = true;
        }
    }

    /// Test helper: directly seed `scroll_x` so tests can probe the
    /// snap behaviour without driving a full wheel-event sequence.
    
#[cfg(test)]
    pub(crate) fn set_scroll_x_for_test(&mut self, x: f64) {
        self.scroll_x = x;
    }

    /// Test helper: directly seed `last_wheel_at` so tests can simulate
    /// the user having stopped wheeling some duration ago without
    /// having to actually sleep or run a real event loop.
    #[cfg(test)]
    /// Test-only view of the live snap stops (each group's left edge in
    /// canvas-x), as built by the last `apply_layout` pass.
    #[cfg(test)]
    pub(crate) fn group_left_edges_for_test(&self) -> &[f64] {
        &self.group_left_edges
    }

    pub(crate) fn set_last_wheel_at_for_test(&mut self, t: Instant) {
        self.last_wheel_at = Some(t);
    }

    /// Test helper: directly seed `group_left_edges` so tests that
    /// don't run a real taffy layout pass can still exercise the snap
    /// math.
    #[cfg(test)]
    pub(crate) fn set_group_left_edges_for_test(&mut self, edges: Vec<f64>) {
        self.group_left_edges = edges;
    }

    /// Test helper: directly seed `content_w` so `max_scroll_x` returns
    /// a non-zero value in tests that don't run a real taffy layout.
    #[cfg(test)]
    pub(crate) fn set_content_w_for_test(&mut self, w: f64) {
        self.content_w = w;
    }

    /// Test helper: directly seed the row's outer rect (for `max_scroll_x`).
    #[cfg(test)]
    pub(crate) fn set_rect_for_test(&mut self, r: Rect) {
        self.rect = r;
    }

    /// Move a tab (by id) from `src` to `dst` so that the dropped-in tab
    /// lands at index `target_idx`. Returns `true` if the move happened,
    /// `false` if the `src` group had no such tab. Exposed separately
    /// from `handle_event` so tests can drive the transfer without
    /// having to stage a full winit event.
    pub fn transfer_tab(
        &mut self,
        src: usize,
        dst: usize,
        tab_id: u64,
        target_idx: usize,
    ) -> bool {
        if src == dst || src >= self.groups.len() || dst >= self.groups.len() {
            return false;
        }
        // Detach from src. We have to keep the mutable borrow scoped so
        // we can reborrow `dst` afterwards.
        let detached = {
            let src_group = &mut self.groups[src];
            src_group.detach_tab(tab_id)
        };
        let Some((desc, host)) = detached else {
            return false;
        };
        let dst_group = &mut self.groups[dst];
        dst_group.insert_detached_tab(target_idx, desc, host);
        true
    }

    /// Classify a drag release point against the row's inter-group drop
    /// zones. Returns `Some(gap_idx)` if the release landed strictly
    /// between two groups (or before the first / after the last) while
    /// still inside the row's vertical band; `None` otherwise.
    ///
    /// `gap_idx` counts gap slots left-to-right:
    /// - `0` → insert a new group before the current first group
    /// - `i` (for `1 ≤ i < N`) → insert between groups `i-1` and `i`
    /// - `N` → append a new group to the right of the current last group
    ///
    /// Only returns `Some(_)` when the release is in the *gap* between
    /// group rects (not inside any group), so a drop inside a group's
    /// tile area still falls through to the sibling-bar detection above.
    pub fn gap_index_for(&self, release_point: Point) -> Option<usize> {
        if self.groups.is_empty() {
            // Empty row → any drop becomes the first group. We don't
            // y-band-restrict: a fresh shell with no groups has a degenerate
            // row rect and we still want drops anywhere in the strip to
            // land as "first group".
            return Some(0);
        }
        // Don't y-band-restrict: the release point comes from the source
        // `TabBar` which is always in our row, so a drop anywhere in
        // screen space that doesn't land on one of our groups is a
        // new-window gap. The strip area below / above the row's rendered
        // rect was silently refused by the old y-band check — that was
        // the root cause of the "drop zones show but drop has no effect"
        // bug. See `drop_into_new_window_works_when_release_is_…` tests.
        // Inside any group rect → not a gap drop (within-group / sibling
        // reorder is handled elsewhere).
        for g in &self.groups {
            if g.get_rect().contains(release_point) {
                return None;
            }
        }
        // Release is left of the first group → insert at 0.
        if release_point.x < self.groups[0].get_rect().x0 {
            return Some(0);
        }
        // Between pair (i-1, i)?
        for i in 1..self.groups.len() {
            let left = self.groups[i - 1].get_rect().x1;
            let right = self.groups[i].get_rect().x0;
            if release_point.x >= left && release_point.x <= right {
                return Some(i);
            }
        }
        // Release is right of the last group → append.
        if release_point.x > self.groups.last().unwrap().get_rect().x1 {
            return Some(self.groups.len());
        }
        None
    }

    /// Detach `tab_id` from `src` and splice it into a brand-new
    /// `WindowGroup` at `gap_idx`, pruning the source group if the
    /// detach left it empty. Returns `true` if the split happened.
    /// Exposed separately so tests can drive the flow without staging a
    /// full winit event sequence.
    pub fn split_tab_into_new_group(
        &mut self,
        src: usize,
        tab_id: u64,
        gap_idx: usize,
    ) -> bool {
        if src >= self.groups.len() {
            return false;
        }
        let detached = {
            let src_group = &mut self.groups[src];
            src_group.detach_tab(tab_id)
        };
        let Some((desc, host)) = detached else {
            return false;
        };
        let src_now_empty = self.groups[src].tab_count() == 0;
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
        // Flag a structural mutation so the App's tick handler rebuilds
        // the taffy tree. Without this, `apply_layout` would iterate the
        // stale `node_ids` (one per pre-split group) and skip the newly
        // inserted group entirely — the bug this whole field exists to fix.
        self.structure_dirty = true;
        if src_now_empty {
            // Source index shifts by 1 iff the new group was inserted
            // at or left of the source's original slot.
            let src_after = if clamped <= src { src + 1 } else { src };
            if src_after < self.groups.len() {
                self.groups.remove(src_after);
            }
        }
        true
    }

    /// Drain a cross-strip detach payload published by the post-dispatch
    /// detach sweep. Returns `Some` when the most recent drag ended
    /// outside the row's own vertical band — i.e. too far away for any
    /// in-row resolution (sibling bar / inter-group gap) to plausibly be
    /// the user's intent. The shell-side harvester (see
    /// `quick-shell::shell::overlay_host::ShellOverlayHost::dispatch_cross_strip_detaches`)
    /// drains this each frame, hit-tests the release point against every
    /// other strip's `GroupRow` rect, and either delivers the payload to
    /// the destination row via [`Self::accept_cross_strip_drop`] or
    /// returns it to the source row when no other strip matches.
    ///
    /// Take-pattern symmetry: leaves `None` behind so a subsequent harvester
    /// call doesn't double-deliver. Mirrors
    /// [`WindowGroup::take_tab_detach`] / `TabBar::take_reorder` /
    /// [`WindowGroup::take_pending_maximize_change`].
    pub fn take_pending_cross_strip_detach(&mut self) -> Option<CrossStripDetach> {
        self.pending_cross_strip_detach.take()
    }

    /// Drain the pending whole-group reorder one-shot published when a
    /// group-drag gesture commits. `Some((group_id, new_index))` means the
    /// user dragged that group to `new_index` in this row; the shell
    /// mirrors the move into the persisted [`WorkspaceTree`] so it survives
    /// a restart and replicates to peers. Take-pattern: leaves `None`
    /// behind so a later harvest tick can't double-apply.
    pub fn take_pending_group_reorder(&mut self) -> Option<(u64, usize)> {
        self.pending_group_reorder.take()
    }

    /// Whether a whole-group reorder drag is currently past its threshold
    /// (i.e. actively moving a group). Read by tests and by `draw` to gate
    /// the drop indicator.
    pub fn group_drag_active(&self) -> bool {
        self.group_drag.map(|d| d.moved).unwrap_or(false)
    }

    /// The insertion gap the active group-drag currently targets, if any.
    /// Exposed so integration tests can assert the drop indicator tracks
    /// the cursor without scraping the painted scene.
    pub fn group_reorder_target(&self) -> Option<usize> {
        self.group_reorder_target
    }

    /// Insertion index (`0..=groups.len()`) for a whole-group reorder given
    /// the cursor position `p`: the slot *before* the first group whose
    /// horizontal centre sits right of the cursor, or `groups.len()`
    /// (append) when the cursor is past every group's centre. Center-based
    /// so nudging a group just past a neighbour's midpoint commits the
    /// swap, matching the feel of tab reorder.
    fn group_insertion_index_for(&self, p: Point) -> usize {
        for (i, g) in self.groups.iter().enumerate() {
            if p.x < g.get_rect().center().x {
                return i;
            }
        }
        self.groups.len()
    }

    /// Apply a committed group reorder to `self.groups` and stamp the
    /// `pending_group_reorder` one-shot for the shell to mirror. `insert_at`
    /// is the gap index from [`Self::group_insertion_index_for`] (in the
    /// pre-removal index space); it's converted to the group's landing slot
    /// in the post-removal vector so the shell's
    /// `WorkspaceTree::reorder_group_within_strip` reproduces the exact same
    /// order. Marks the row structurally dirty so the App rebuilds the
    /// taffy tree (resize-handle pool + node ids must track the new order).
    fn commit_group_reorder(&mut self, src: usize, group_id: u64, insert_at: usize) {
        if src >= self.groups.len() {
            return;
        }
        // Removing `src` shifts every later slot left by one, so an
        // insertion gap to the right of the source lands one index lower
        // once the source is lifted out.
        let final_idx = if insert_at > src { insert_at - 1 } else { insert_at };
        let dest = final_idx.min(self.groups.len().saturating_sub(1));
        if dest == src {
            return;
        }
        let g = self.groups.remove(src);
        self.groups.insert(dest, g);
        self.structure_dirty = true;
        self.pending_group_reorder = Some((group_id, dest));
    }

    /// Whole-group reorder drag state machine. Returns `true` when the
    /// event is claimed by an in-flight group drag (so the caller skips the
    /// resize-handle + per-child dispatch that would otherwise fight it).
    ///
    /// Click-vs-drag: a press on a group's tab-bar background records a
    /// *candidate* drag but does NOT claim the press, so a plain click still
    /// falls through to the child for focus. Only once the pointer travels
    /// past [`GROUP_DRAG_THRESHOLD`] does the row start claiming motion and
    /// the eventual release.
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
                    || self.groups.iter().any(|g| g.drag_in_flight())
                    || self.handles.iter().any(|h| h.is_dragging())
                {
                    return false;
                }
                if let Some(idx) = self
                    .groups
                    .iter()
                    .position(|g| g.bar_grab_hit(self.last_cursor))
                {
                    self.group_drag = Some(GroupDrag {
                        src_idx: idx,
                        group_id: self.groups[idx].id,
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
                if drag.moved {
                    if let Some(insert_at) = self.group_reorder_target.take() {
                        self.commit_group_reorder(
                            drag.src_idx,
                            drag.group_id,
                            insert_at,
                        );
                    }
                }
                self.group_reorder_target = None;
                claimed
            }
            _ => false,
        }
    }

    /// Accept a tab payload that the shell-side harvester routed to this
    /// row from another strip. Splices the tab into a brand-new
    /// [`WindowGroup`] in the gap nearest `drop_pos`, mirroring the
    /// in-row `split_tab_into_new_group` flow but with a pre-detached
    /// payload (the source row already removed its tab when it published
    /// the cross-strip detach). Flags `structure_dirty` so the App's
    /// next layout pass rebuilds the taffy tree to include the new group.
    ///
    /// `drop_pos` is the same release point the source row published in
    /// [`CrossStripDetach::release_point`]; it's resolved against this
    /// row's gaps via the existing [`Self::gap_index_for`] machinery so
    /// "drop near the right edge of strip B" lands as a new group
    /// appended to strip B's row, "drop between groups 1 and 2" splices
    /// between them, and so on.
    ///
    /// Used both for the cross-strip success path AND the source-row
    /// "no other strip matched, return to sender" recovery path: in the
    /// recovery case the shell calls this method on the source row with
    /// the original release point, producing the same in-row "create new
    /// group near the release point" behaviour the local detach sweep
    /// would have produced if we hadn't intercepted it.
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
        // Resolve drop_pos to a gap index. `gap_index_for` ignores the
        // y-band so a drop above/below this row's vertical extent still
        // produces a sensible gap (closest-by-x). When `drop_pos` falls
        // inside one of our existing groups' rects (rare for cross-strip
        // — the shell only routes here when the release point is in
        // OUR row's bounds, but a release on the bar of an existing
        // group would still land inside a group rect), `gap_index_for`
        // returns `None` and we default to "append at end" so the tab
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
        // Resolve every group's width fraction BEFORE it builds its node,
        // walking the strip left to right so each group's basis is the
        // usable width of the display its own left edge lands on. Widths
        // determine the next group's left edge, so the walk is sequential
        // — but it is fully determined here, with no dependency on a
        // previous frame's layout, which is what keeps a single-frame
        // offscreen render (a screenshot) identical to the live one.
        let usable_per_group = self.usable_per_group();
        for (g, &usable) in self.groups.iter_mut().zip(usable_per_group.iter()) {
            g.set_strip_usable_px(usable);
        }
        let group_nodes: Vec<NodeId> = self
            .groups
            .iter_mut()
            .map(|g| g.build_layout(taffy, theme, node_map))
            .collect();
        self.node_ids = group_nodes.clone();
        // Concept-parity maximize: the moment any child group is
        // maximized, the row visually collapses to that single group
        // at the strip's full width and every sibling vanishes.
        // Mirrors the concept's `WindowFrame.tsx:188-200` behaviour
        // where the maximized frame is `position: fixed`,
        // `width: 100vw`, `height: 100vh`, `z-index: 50` — siblings
        // stay in the DOM but are visually obscured by the fixed
        // overlay. We model the same outcome via `display: None` on
        // sibling taffy nodes so they collapse to zero size and don't
        // paint. Inter-group `ResizeHandle`s are skipped entirely in
        // the maximize branch — they belong to gaps between visible
        // siblings, and the strip has no visible siblings while one
        // group owns the row.
        //
        // Edge case: if multiple groups happen to be flagged maximized
        // simultaneously (e.g. via a persisted-state restore path), the
        // first one wins; the rest are hidden alongside the non-
        // maximized siblings. Same observable outcome as the click-
        // toggle flow where only one is ever true at a time.
        // A transient fullscreen promotion (a group that requested
        // `xdg_toplevel.set_fullscreen`) drives the SAME collapse-siblings
        // layout as a persisted `maximized` flag, and wins over it — a live
        // present-mode request outranks a stale maximize. `promoted_idx` is
        // whichever group owns the row; the rest collapse to `Display::None`.
        let maximized_idx = self
            .fullscreen_idx()
            .or_else(|| self.groups.iter().position(|g| g.maximized));
        // Resize the handle pool to match the inter-group gap count
        // (`max(0, N-1)`). Handles are structurally tied to gaps
        // between adjacent groups, so a group insert/remove drives
        // the pool's resize. We grow with fresh `ResizeHandle::new`
        // entries (idle, no drag state) and truncate from the tail
        // when the group count shrinks. Preserving in-flight drag
        // state across a re-layout isn't required: the App rebuilds
        // taffy whenever `structure_dirty` fires (every group
        // insert/remove), so any active drag would have been on a
        // handle whose neighbours just changed structurally anyway.
        let desired_handles = self.groups.len().saturating_sub(1);
        if self.handles.len() < desired_handles {
            self.handles.extend(
                (self.handles.len()..desired_handles).map(|_| ResizeHandle::new()),
            );
        } else if self.handles.len() > desired_handles {
            self.handles.truncate(desired_handles);
        }
        // Each handle clamps its drag against the same per-display range
        // its neighbours were sized with. Handle `i` sits in the gap after
        // group `i`, so it takes group `i`'s basis — the display that gap
        // is on. Without this the clamps would still be the old fixed
        // 300..1200 px, which on a 4K panel refused to let a drag past
        // 31 % of the screen.
        for (i, h) in self.handles.iter_mut().enumerate() {
            if let Some(&usable) = usable_per_group.get(i) {
                h.set_strip_usable_px(usable);
            }
        }
        // Build per-handle taffy nodes and interleave them between
        // the group nodes. Order: `[g0, h0, g1, h1, g2, …, g_{N-1}]`
        // — handles only sit between adjacent siblings, never as
        // leading or trailing children. Their declared 8 px width
        // replaces the row's old inter-group gap, so the gap on the
        // flex container drops to 0 below.
        //
        // While any group is maximized, handles are omitted from the
        // child list entirely: the row has no visible inter-group
        // gaps because every sibling is collapsed to `display: None`.
        // Skipping the build keeps the handles from claiming non-zero
        // width inside the maximized layout and matches the concept's
        // absent resize affordance in fullscreen mode.
        self.handle_node_ids.clear();
        let mut children: Vec<NodeId> = Vec::with_capacity(group_nodes.len() + desired_handles);
        if maximized_idx.is_some() {
            for gn in group_nodes.iter() {
                children.push(*gn);
            }
        } else {
            for (i, gn) in group_nodes.iter().enumerate() {
                children.push(*gn);
                if i + 1 < group_nodes.len() {
                    let hn = self.handles[i].build_layout(taffy, theme, node_map);
                    self.handle_node_ids.push(hn);
                    children.push(hn);
                }
            }
        }
        // Pin `flex_shrink: 0.0` on every group child so the strip is
        // Niri-style: placing a new tile NEVER resizes siblings. The
        // row's intrinsic `content_width` then grows past the
        // viewport-aligned `rect.width()`; the user pans horizontally
        // through the wheel handler below, and the visible region is
        // clipped to `self.rect` in `draw` so off-screen-right groups
        // don't bleed past the window edge.
        //
        // A short-lived 2026-06-01 attempt (`flex_shrink: 1.0`) tried
        // to make groups shrink-to-fit instead, motivated by "apps draw
        // way over the window border" feedback. That fix violated the
        // infinite-strip invariant — siblings silently shrunk whenever
        // a new tile spawned, and the user immediately rejected it.
        // The real fix is the clip layer in `draw`, not shrinking
        // groups; clipping was missing, so overflow bled into the
        // window's edge / OS chrome.
        //
        // Maximize branch (`maximized_idx == Some(i)`): siblings at
        // index `!= i` get `display: None` so they collapse to zero
        // and don't paint. The maximized group keeps its own
        // `flex_grow: 1e6` (set in `WindowGroup::build_layout`),
        // which absorbs the strip's entire width inside the Flex row
        // — the concept's `width: 100vw` outcome.
        for (i, &node) in group_nodes.iter().enumerate() {
            let mut style = taffy.style(node).unwrap().clone();
            style.flex_shrink = 0.0;
            if let Some(max_i) = maximized_idx {
                if i != max_i {
                    style.display = Display::None;
                } else {
                    // Ensure the promoted group absorbs the whole row. A
                    // `maximized` group already carries this grow style from
                    // `WindowGroup::build_layout`, but a *fullscreen*-promoted
                    // group (whose own `maximized` flag is false) does not —
                    // set it here so both promotion paths reach the same
                    // full-row outcome.
                    style.size.width = Dimension::auto();
                    style.flex_grow = 1.0e6;
                    style.flex_basis = Dimension::length(0.0);
                }
            }
            taffy.set_style(node, style).unwrap();
        }
        // The row must fill the remaining vertical space inside the
        // parent Column so each child `WindowGroup`'s `height: percent(1.0)`
        // resolves to the full available height — otherwise the row only
        // measures as tall as the tallest child's natural height (i.e. the
        // 40 px TabBar) and the `TileHost` collapses to zero height,
        // hiding the concept's white card entirely.
        //
        // `width: percent(1.0)` keeps the row's outer rect viewport-
        // aligned (so it doesn't push parent Column wider when content
        // overflows). Children laid out beyond `rect.x1` are still
        // walked by `apply_layout` and shifted into screen-space by the
        // `scroll_x` translation in our origin computation; they remain
        // hit-testable because `gap_index_for` uses each group's stored
        // post-shift `Rect`.
        let node = taffy
            .new_with_children(
                Style {
                    // A strip that owns no part of the current fullscreen
                    // surface collapses away entirely, so the promoted group
                    // in another strip is the only thing painted over the
                    // output. Reset to `Flex` restores the row exactly.
                    display: if self.hidden { Display::None } else { Display::Flex },
                    flex_direction: FlexDirection::Row,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    // Gap goes to zero: the inter-group `ResizeHandle`
                    // widgets supply the visible 8-px footprint per
                    // gap. Keeping a non-zero gap here in addition to
                    // the handle's declared 8 px width would double
                    // the inter-group distance and push every group
                    // further apart than the original strip layout.
                    gap: Size {
                        width: taffy::LengthPercentage::length(0.0),
                        height: taffy::LengthPercentage::length(0.0),
                    },
                    ..Default::default()
                },
                &children,
            )
            .unwrap();
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
        // Total natural content width = (rightmost child's x + width) -
        // (leftmost child's x). With Taffy laying children out in order
        // from local x=0, this is simply `last.x + last.width` (no gap
        // after the last child). We read each child's *taffy-reported*
        // position so the inter-group gap (8 px declared on the row's
        // style) is included automatically.
        //
        // Same loop captures every child's local x position into
        // `group_left_edges` — these are the snap targets the
        // wheel-idle snap animation eases `scroll_x` toward. Re-built
        // every layout pass so insertions / removals / reorderings
        // produce the updated set of legal snap stops automatically.
        // Snap targets and content_w live in CANVAS-x (with the
        // leading inset baked in), not in taffy-local-x. The wheel
        // disc's overlay rendering occupies canvas-x [0, leading_inset];
        // every group sits at canvas-x = leading_inset + taffy.x. The
        // strip's `scroll_x` is "how far the canvas has panned past the
        // viewport's left edge", so a snap target at canvas-x = K means
        // `scroll_x = K - rect.x0` brings that point flush against the
        // viewport left.
        let mut content_w = self.leading_inset;
        let mut edges: Vec<f64> = Vec::with_capacity(self.node_ids.len());
        let mut local_x: Vec<f64> = Vec::with_capacity(self.node_ids.len());
        for &cn in &self.node_ids {
            let cl = taffy.layout(cn).unwrap();
            edges.push(self.leading_inset + cl.location.x as f64);
            local_x.push(cl.location.x as f64);
            let right = self.leading_inset + cl.location.x as f64 + cl.size.width as f64;
            if right > content_w {
                content_w = right;
            }
        }
        self.content_w = content_w;
        self.group_left_edges = edges;
        // Make-space slide: an insertion armed the motion, and this is
        // the first pass that knows how far each group actually moved.
        // Only trust the reading when taffy has a node per group — an
        // `apply_layout` that lands between the splice and the rebuild
        // would otherwise measure a truncated strip and animate the
        // wrong ones.
        if self.node_ids.len() == self.groups.len() {
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
        // Retire a settled slide so `needs_animation_frame` stops
        // pumping and `draw` stops composing an extra transform.
        if self.shift_hold.is_none() && self.shift_anim.is_some() && self.shift_anim_t() >= 1.0 {
            self.shift_anim = None;
        }
        // Clamp scroll into bounds — but use the SAME widened upper
        // bound the wheel handler uses (max_scroll_x OR three
        // viewports of empty wallpaper past the rightmost group).
        // A tight clamp to `max_scroll_x` was the bug that silently
        // reset the strip's scroll position every frame, defeating
        // the Niri-style "scroll into empty space" affordance the
        // user expects on a single-group strip.
        //
        // The original clamp's structural-mutation purpose (a closed
        // group can leave scroll_x past the new max) is preserved
        // by `max_scroll_x` clamping when the strip has real
        // content overflow; for the no-overflow case the widened
        // upper bound matches the wheel handler so we don't
        // perpetually reset whatever scroll the user dialled in.
        let viewport = self.rect.width().max(1.0);
        let upper = self.max_scroll_x().max(viewport * 3.0);
        if self.scroll_x > upper {
            self.scroll_x = upper;
        }
        if self.scroll_x < 0.0 {
            self.scroll_x = 0.0;
        }
        // Per-frame snap tick: advance any in-flight snap animation,
        // or kick a fresh one off if the user has been idle past the
        // threshold. Done AFTER the clamp so a structural mutation
        // that snapped scroll_x to max doesn't get re-eased.
        self.tick_snap_at(Instant::now());
        // Children's origin: the strip's canvas starts at canvas-x=0
        // (wheel disc occupies canvas-x [0, leading_inset]) and the
        // viewport shows canvas-x [scroll_x, scroll_x + viewport_w].
        // Translating taffy-local x by `rect.x0 + leading_inset -
        // scroll_x` gives the correct screen-space position for every
        // group: at scroll_x=0 the first group sits at
        // `rect.x0 + leading_inset` (flush against the wheel's right
        // edge); as scroll_x grows the whole canvas pans left in
        // lock-step with the wheel-disc's own `-scroll_x` shift in
        // `ShellOverlayHost::apply_layout`.
        // A fullscreen-promoted group covers the whole output: the sidebar
        // wheel is suppressed, so its leading gutter is dropped and any prior
        // pan is ignored, pinning the group flush at `rect.x0`.
        let (origin_inset, origin_scroll) = if self.is_fullscreen() {
            (0.0, 0.0)
        } else {
            (self.leading_inset, self.scroll_x)
        };
        let child_origin = Point::new(
            self.rect.x0 + origin_inset - origin_scroll,
            self.rect.y0,
        );
        for (g, child_id) in self.groups.iter_mut().zip(self.node_ids.iter()) {
            g.apply_layout(taffy, *child_id, child_origin);
        }
        // Refresh each inter-group `ResizeHandle`'s neighbour rect
        // snapshot. We always re-derive these from the freshly
        // laid-out group rects (post-`scroll_x` shift) so the hit
        // area + paint geometry track the visible groups exactly.
        // The handle count must match `groups.len() - 1` because
        // `build_layout` resized the pool to that value just above.
        debug_assert_eq!(
            self.handles.len(),
            self.groups.len().saturating_sub(1),
            "ResizeHandle pool must mirror inter-group gap count after build_layout",
        );
        for i in 0..self.handles.len() {
            let left = self.groups[i].get_rect();
            let right = self.groups[i + 1].get_rect();
            self.handles[i].set_neighbour_rects(left, right);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Clear the wheel-pan dirty flag now that we're about to
        // re-record the strip with the new `scroll_x`. The parent
        // Column's subtree cache picks up the new scene this call
        // emits.
        self.scroll_dirty = false;
        // Clip every painted child to the row's own rect. The strip is
        // Niri-style infinite: groups stay at their declared width
        // regardless of how many siblings exist, so the row's
        // `content_width` routinely exceeds the viewport-aligned
        // `rect.width()` and post-`scroll_x` off-screen-right groups
        // would otherwise bleed past the window's right edge. The clip
        // layer truncates them at the row boundary; horizontal scroll
        // brings them into view. Padding the clip vertically by 16 px
        // each side keeps the shadow / glow sprites (ambient_pad=50,
        // bloom_pad=120 — see `TileHost::draw_steady_state`) intact at
        // the row's top and bottom edges while still cutting horizontal
        // overflow at the strip boundary.
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
        // Concept-parity maximize: when any group is maximized, paint
        // ONLY that group. The siblings are already collapsed to
        // `display: None` by `build_layout`, so their stored rects are
        // degenerate (zero-area), but calling `draw` on them would
        // still emit chrome (bar pill outlines, shadow sprites) at the
        // origin — visible as ghost artwork bleeding behind the
        // maximized tile. Skipping the per-sibling draw keeps the row
        // looking like the concept's `width: 100vw` fixed overlay.
        //
        // A transient fullscreen promotion collapses siblings the same way,
        // and wins over a stale `maximized` flag — same as `build_layout`.
        let maximized_idx = self
            .fullscreen_idx()
            .or_else(|| self.groups.iter().position(|g| g.maximized));
        if let Some(max_i) = maximized_idx {
            if let Some(g) = self.groups.get_mut(max_i) {
                g.draw(scene, transform, tr, theme, presence);
            }
        } else {
            // A group a fresh insertion pushed along paints where it
            // used to be and eases to where it now belongs, so the strip
            // is seen opening a gap rather than having always had one.
            // Widths are untouched — this is a translation, which is the
            // only honest way to make room on a strip with unlimited
            // space to the right.
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
                    .map(|(_, from)| from * remaining)
                    .unwrap_or(0.0);
                let t = if dx.abs() > f64::EPSILON {
                    transform * Affine::translate((dx, 0.0))
                } else {
                    transform
                };
                g.draw(scene, t, tr, theme, presence);
            }
        }
        // Paint the inter-group resize handles after the groups so
        // the accent rule sits on top of any group-edge artwork.
        // The handles are skipped entirely while a tab-drag is in
        // flight: the cross-group drop-zone capsule (drawn below)
        // takes priority during a drag so the user isn't confused
        // by two overlapping affordances on the same gap.
        //
        // Handles are also skipped entirely while any group is
        // maximized — `build_layout` doesn't include the handle nodes
        // in the maximize branch, so their stored neighbour rects are
        // stale; painting them anyway would draw ghost rules at the
        // pre-maximize gap positions.
        //
        // During a make-space slide each handle travels with the group on
        // its right — which is where the gap it marks actually is. Two
        // adjacent windows are both white cards, so without the rule
        // between them a slide reads as one wide window rather than two
        // moving apart.
        let any_drag = self.groups.iter().any(|g| g.drag_in_flight());
        if !any_drag && maximized_idx.is_none() {
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
                // A handle is the rule between two windows, and the grip
                // for resizing them. A group with nothing on screen is
                // not a window, so the border it would be one side of is
                // not there either — drawing it anyway leaves a rule
                // floating in empty space.
                //
                // A group holding a place for an app that is still
                // starting *is* on screen: it is a card the width of the
                // window it is waiting for. Its edges are real edges, and
                // a rule that appeared only once the app finished loading
                // would be one more thing moving at the moment the person
                // is looking at the window arriving.
                let shown = |g: Option<&WindowGroup>| {
                    g.map(|g| Widget::presented(g) || Widget::opening(g))
                        .unwrap_or(false)
                };
                if !shown(self.groups.get(i)) || !shown(self.groups.get(i + 1)) {
                    continue;
                }
                let dx = self
                    .groups
                    .get(i + 1)
                    .and_then(|g| offsets.iter().find(|(id, _)| *id == g.id))
                    .map(|(_, from)| from * remaining)
                    .unwrap_or(0.0);
                let t = if dx.abs() > f64::EPSILON {
                    transform * Affine::translate((dx, 0.0))
                } else {
                    transform
                };
                h.draw(scene, t, tr, theme, presence);
            }
        }

        // Whole-group reorder drop indicator: a bright accent rule at the
        // gap the dragged group will land in. Painted only while a group
        // drag is past its threshold so an idle cursor never shows it.
        if let (Some(_), Some(ins)) = (
            self.group_drag.filter(|d| d.moved),
            self.group_reorder_target,
        ) {
            if !self.groups.is_empty() {
                let gap_x = if ins == 0 {
                    self.groups[0].get_rect().x0 - 5.0
                } else if ins >= self.groups.len() {
                    self.groups.last().unwrap().get_rect().x1 + 5.0
                } else {
                    (self.groups[ins - 1].get_rect().x1
                        + self.groups[ins].get_rect().x0)
                        / 2.0
                };
                let y0 = self.rect.y0;
                let y1 = self.rect.y1;
                if y1 > y0 {
                    let thickness = 4.0;
                    let capsule = RoundedRect::from_rect(
                        Rect::new(
                            gap_x - thickness / 2.0,
                            y0,
                            gap_x + thickness / 2.0,
                            y1,
                        ),
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
            }
        }

        // No inter-group drop-zone visual: the white line drawn by the
        // active tab bar already shows the per-pill drop target, and any
        // release in a sibling group's tile-area edge band (resolved by
        // the new edge-drop logic in `handle_event`) creates a new group
        // without needing a strip-level cue. The previous extreme-gap
        // accent capsule (gap_idx == 0 / groups.len()) was removed
        // 2026-06-24 per user feedback ("weird purple-ish rectangles as
        // drop off zones — these will eventually need to be removed").
        // Close the strip clip layer we pushed at the top of `draw`.
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
            // While any child has a drag in flight, publish the inter-group
            // gap the cursor is currently over so `draw` can paint the
            // "drop here" indicator. We explicitly gate on an active drag
            // so the indicator never shows for idle cursor motion — the
            // concept only surfaces gap targets *during* a drag gesture.
            let any_drag = self.groups.iter().any(|g| g.drag_in_flight());
            self.hover_gap_idx = if any_drag {
                self.gap_index_for(self.last_cursor)
            } else {
                None
            };
            // Cross-group minimalist drop indicator: while any child's
            // bar has a drag in flight, push the current cursor into
            // every sibling bar so the new `hover_drop_idx`-driven
            // white line paints at the correct inter-pill gap in
            // whichever bar the cursor is currently over. The source
            // bar drives its own `hover_drop_idx` from inside the bar's
            // own `CursorMoved` arm (see `TabBar::handle_event`); for
            // siblings we have to plumb the cursor through here because
            // the bars don't see each other's drag state. The setter
            // is a no-op while a bar has its own drag in flight, so a
            // fan-out that visits the source bar last (after the bar
            // itself wrote its target) can't wipe it.
            let cursor_for_external = if any_drag {
                Some(self.last_cursor)
            } else {
                None
            };
            for g in &mut self.groups {
                g.set_external_drag_hover(cursor_for_external);
            }
        }

        // Drop is resolved on release — the indicator has served its
        // purpose, and leaving it set would let a stale gap highlight
        // persist into the next frame (and possibly the next gesture,
        // since a fresh press replays CursorMoved before any drag starts).
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.hover_gap_idx = None;
        }

        // Whole-group reorder drag (grab a group's tab-bar background and
        // slide it to a new slot). Runs before the resize-handle and
        // per-child dispatch so, once past its threshold, it claims the
        // motion / release those would otherwise contend for. A press
        // records a candidate without claiming, so a plain click still
        // reaches the child for focus.
        if self.update_group_drag(event) {
            *handled = true;
            return;
        }

        // A bare wheel NEVER moves the strip — hard boundary
        // (2026-08-22): scrolling is for content inside a window, and
        // the view must not slide out from under the user. The strip
        // travels only through focus changes (click, Alt+Shift wheel
        // jump, keyboard) and the shell's modifier-wheel gestures,
        // which arrive via `scroll_group_into_view_animated`, not
        // through this event path. The wheel is deliberately left
        // unclaimed here so nothing about it reaches the strip.

        // Dispatch to inter-group resize handles FIRST so a press in
        // the gap claims the event before it reaches the adjacent
        // groups. Skipped while any child has a tab-drag in flight —
        // the cross-group drop resolution owns the gap during a
        // drag and would compete with the resize handle's press if
        // both ran. Each handle's `handle_event` is independent: a
        // press inside one handle's hit area doesn't perturb any
        // other handle's state.
        let any_drag_inflight = self.groups.iter().any(|g| g.drag_in_flight());
        if !any_drag_inflight {
            for (i, h) in self.handles.iter_mut().enumerate() {
                let Some(&node_id) = self.handle_node_ids.get(i) else {
                    break;
                };
                h.handle_event(
                    event, scale_factor, modifiers, state, root, handled, focus_target,
                    node_id, taffy,
                );
                if *handled {
                    break;
                }
            }
        }

        // Drain any handle width pulses produced by the dispatch above
        // and apply them to the two adjacent groups via `set_width_px`,
        // which converts each pixel measurement back into a fraction of
        // that group's own display.
        // We do this before the group dispatch loop so a mid-drag
        // CursorMoved sees the updated widths on the same frame —
        // otherwise the user perceives one frame of lag between
        // cursor motion and edge response.
        for i in 0..self.handles.len() {
            if let Some((new_left, new_right)) = self.handles[i].take_pending() {
                if i < self.groups.len() {
                    self.groups[i].set_width_px(new_left);
                }
                if i + 1 < self.groups.len() {
                    self.groups[i + 1].set_width_px(new_right);
                }
            }
            if self.handles[i].take_pending_commit() {
                // End-of-drag persistence pulse: stamp pending width-
                // change on both neighbours so the shell harvests
                // them on the next frame and writes through to the
                // persisted `WorkspaceTree`. Mirrors what
                // `WindowGroup::end_resize` does for the per-group
                // right-edge handle.
                if i < self.groups.len() {
                    self.groups[i].stamp_pending_width_change();
                }
                if i + 1 < self.groups.len() {
                    self.groups[i + 1].stamp_pending_width_change();
                }
            }
        }

        // Dispatch to every child; stop once one claims the event. We
        // deliberately *do not* bail out of the detach sweep even when a
        // child claimed — the claim is for pointer-capture / focus, while
        // the detach sweep is a separate "did anyone publish a detach?"
        // pass that runs regardless.
        //
        // `node_ids` is rebuilt every frame in `build_layout`, but
        // `groups` can grow mid-frame when a tab is dropped into a fresh
        // gap (the detach sweep below pushes a new `WindowGroup` and
        // marks `structure_dirty`). The mid-frame transient is benign —
        // the fresh group has no node_id this frame and gets one on the
        // next layout pass — so we tolerate the imbalance with `.get`
        // instead of indexing, and silently skip dispatch for the
        // newly-spawned group until layout catches up.
        for (i, g) in self.groups.iter_mut().enumerate() {
            let Some(&node_id) = self.node_ids.get(i) else {
                break;
            };
            g.handle_event(
                event, scale_factor, modifiers, state, root, handled, focus_target,
                node_id, taffy,
            );
            if *handled {
                break;
            }
        }

        // Group-close prune sweep: any child whose group-level close
        // fade has fully elapsed is dropped from `self.groups` and
        // the structure is flagged dirty so `App` rebuilds the taffy
        // tree. Mirrors the tab-close sweep inside `WindowGroup`:
        // the fade plays out in place, then the parent reaps the
        // shell only once the curve has settled. Collected
        // highest-index-first so the removals don't shift indices
        // out from under the loop. Children still in-flight
        // (`is_closing() && !close_complete()`) are left alone; the
        // group keeps painting its shrink.
        let mut prune: Vec<usize> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.close_complete())
            .map(|(i, _)| i)
            .collect();
        if !prune.is_empty() {
            // Parallel-vec hardening (Wave 2 / 2-D): `node_ids` is
            // populated by `build_layout` to match `groups` 1:1 and is
            // re-cleared / re-populated on the next layout pass after
            // we set `structure_dirty = true` below. The intermediate
            // post-prune state therefore has a *deliberately stale*
            // `node_ids.len() == pre-prune groups.len()` until the App
            // re-runs `build_layout`. The debug_assert below pins that
            // expectation: if some future code path popped a `node_ids`
            // slot outside the rebuild gate, the assert trips loudly.
            // We snapshot the prune count *before* the loop because the
            // `prune` vec is consumed by the removal walk.
            let prune_count = prune.len();
            prune.sort_unstable_by(|a, b| b.cmp(a));
            for i in prune {
                self.groups.remove(i);
            }
            debug_assert_eq!(
                self.node_ids.len(),
                self.groups.len() + prune_count,
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

        // Belt-and-suspenders: if dispatch (or our own release branch
        // above) cleared every child's drag, drop any lingering hover
        // target so `draw` can't paint a ghost indicator next frame. The
        // CursorMoved branch only reclassifies on motion, so without this
        // sweep a child that dropped its `pending_drag` internally (e.g.
        // a canceled press) could leave `hover_gap_idx` stuck at its last
        // value until the cursor moves again.
        if self.hover_gap_idx.is_some()
            && !self.groups.iter().any(|g| g.drag_in_flight())
        {
            self.hover_gap_idx = None;
        }

        // Post-dispatch sweep: any child with a pending detach?
        let mut detaches: Vec<(usize, u64, Point)> = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if let Some((tab_id, release_point)) = g.take_tab_detach() {
                detaches.push((i, tab_id, release_point));
            }
        }

        // Cross-strip divert: if the release landed *outside* our own
        // vertical band, the user's drop target is plausibly a sibling
        // strip's `GroupRow`. We can't resolve that from here — the
        // shell holds the WorkspaceTree and knows every strip's rect —
        // so detach the tab now and stash the payload for the shell-
        // side harvester to route. The harvester either delivers the
        // payload to the destination row's `accept_cross_strip_drop`
        // (when the release point hits another strip's rect) or returns
        // it to the source row's `accept_cross_strip_drop` (when no
        // strip matched — equivalent to the in-row "no target → create
        // new group near the release point" path the local resolver
        // below would have produced).
        //
        // We only divert ONE detach per frame — the first one whose
        // release is out-of-band. Multiple simultaneous detaches in a
        // single frame would imply the user dragged two tabs at once,
        // which the gesture pipeline doesn't model. The remaining
        // detaches (none, in practice) fall through to the in-row
        // resolver below so a future multi-touch path doesn't silently
        // drop them.
        let mut diverted: Option<usize> = None;
        for (idx, &(src_idx, tab_id, release_point)) in detaches.iter().enumerate() {
            // y-band check: release outside our row's vertical extent
            // means the user crossed a strip boundary, so route through
            // the shell. Inside the band, fall through to the in-row
            // resolver — that's the existing single-strip behaviour
            // (drop on a sibling bar, drop in an inter-group gap, drop
            // into empty strip space below the row's own content).
            let out_of_band =
                release_point.y < self.rect.y0 || release_point.y > self.rect.y1;
            if !out_of_band {
                continue;
            }
            // Detach the payload now so the source group reflects the
            // mutation immediately. The shell harvester re-inserts the
            // tab on the *next* frame via `accept_cross_strip_drop`, so
            // for one frame the row has one fewer tab — this matches
            // the in-row split flow's behaviour (the new group exists
            // for one frame before layout catches up).
            if let Some((desc, host)) = self.groups[src_idx].detach_tab(tab_id) {
                self.pending_cross_strip_detach = Some(CrossStripDetach {
                    tab_id,
                    desc,
                    host,
                    release_point,
                });
                // Source might now be empty — flag a structural mutation
                // so the App rebuilds the taffy tree if a future frame
                // reaps the empty group. We don't prune here because the
                // source stays "alive" until the shell either delivers
                // the cross-strip drop or returns it to source. Pruning
                // an empty source would force the shell to recreate the
                // group on the recovery path; cheaper to let the empty
                // group ride out one frame and let the existing close-
                // fade prune machinery handle it.
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
            // Find a sibling whose bar rect contains the release point.
            // Closing groups are filtered out: a `WindowGroup` whose
            // group-level close fade has latched still paints its bar
            // rect for the 180 ms shrink, so a cross-group drop can
            // visually resolve onto a group that's in the process of
            // disappearing. `insert_detached_tab` doesn't clear
            // `closing_at`, so the re-inserted tab would get pruned
            // alongside the group at fade-end — the tab would be
            // silently lost. Treating closing groups as if they
            // weren't there at all lets the drop fall through to the
            // gap-based new-group path below, where we splice the
            // dropped tab into a brand-new group in the gap to the
            // LEFT of the closing group. That's the cleanest UX: the
            // dragged tab lands next to where the user aimed (the
            // closing group is about to vacate the slot anyway) and
            // the user gets immediate visual confirmation that their
            // drop produced a new window.
            let target_idx = self
                .groups
                .iter()
                .enumerate()
                .find(|(i, g)| {
                    *i != src_idx
                        && !g.is_closing()
                        && g.bar_rect().contains(release_point)
                })
                .map(|(i, _)| i);
            if let Some(tgt_idx) = target_idx {
                // Resolve the per-pill drop index against the target's tab
                // rects: left of the first pill's midpoint → 0, between
                // pills → that pill's index, past the last pill → append.
                let drop_idx = self.groups[tgt_idx].drop_index_for(release_point);
                let detached = self.groups[src_idx].detach_tab(tab_id);
                if let Some((desc, host)) = detached {
                    self.groups[tgt_idx].insert_detached_tab(drop_idx, desc, host);
                    *handled = true;
                }
                continue;
            }
            // Tile-area drops on a sibling group. The release point
            // falls inside the group's `rect` but below `bar_rect.y1`
            // (the tab-bar branch already matched and `continue`'d
            // above). We split the tile horizontally into three bands:
            //
            //   1. Left edge band (outermost ~25% of width, capped at
            //      48 px) → CREATE a new group immediately to the LEFT
            //      of this sibling. This is the practical solution to
            //      the 2026-06-24 user complaint that the 8 px inter-
            //      group gap is impossible to hit, so dragging a tab
            //      to the area between G(A) and G(B) had nowhere to
            //      land that produced "new group between A and B".
            //   2. Right edge band (symmetric) → CREATE a new group
            //      immediately to the RIGHT of this sibling.
            //   3. Centre band (everything else) → splice into the
            //      sibling's existing tab list, left half → prepend
            //      (idx 0), right half → append (idx = tabs.len()),
            //      matching the 2026-06-02 user spec.
            //
            // Closing groups are excluded throughout: a tab spliced
            // into a group that's about to be pruned would be lost
            // alongside the group at fade-end. Drops on closing
            // groups' tile areas fall through to the gap-based
            // new-window path below.
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
            if let Some(tgt_idx) = tile_target_idx {
                let tgt_rect = self.groups[tgt_idx].get_rect();
                let edge_band = (tgt_rect.width() * 0.25).min(48.0);
                let in_left_band = release_point.x < tgt_rect.x0 + edge_band;
                let in_right_band = release_point.x > tgt_rect.x1 - edge_band;
                if in_left_band || in_right_band {
                    // Edge band → create a new group at the gap to the
                    // left (or right) of the target sibling. The split
                    // helper handles detach + source-prune + structure-
                    // dirty exactly the same as the explicit inter-
                    // group gap branch below; passing the correct
                    // gap_idx is what selects "between A and B" vs
                    // "after B".
                    let gap_idx = if in_left_band { tgt_idx } else { tgt_idx + 1 };
                    if self.split_tab_into_new_group(src_idx, tab_id, gap_idx) {
                        *handled = true;
                    }
                    continue;
                }
                let drop_idx = if release_point.x < tgt_rect.center().x {
                    0
                } else {
                    self.groups[tgt_idx].tabs().len()
                };
                let detached = self.groups[src_idx].detach_tab(tab_id);
                if let Some((desc, host)) = detached {
                    self.groups[tgt_idx].insert_detached_tab(drop_idx, desc, host);
                    *handled = true;
                }
                continue;
            }
            // The release may have landed on a *closing* sibling's bar
            // rect — the filter above treated it as not-there, but the
            // rect still exists geometrically, so `gap_index_for` will
            // return `None` (release is "inside a group"). Fall back
            // to the left-of-closing-group gap so the dropped tab
            // still produces a new window in approximately the right
            // spot instead of being silently dropped.
            let closing_target = self
                .groups
                .iter()
                .enumerate()
                .find(|(i, g)| {
                    *i != src_idx
                        && g.is_closing()
                        && g.bar_rect().contains(release_point)
                })
                .map(|(i, _)| i);
            if let Some(tgt_idx) = closing_target {
                if self.split_tab_into_new_group(src_idx, tab_id, tgt_idx) {
                    *handled = true;
                }
                continue;
            }
            // No sibling bar matched — try the inter-group gap zones.
            // Matches the concept's `DropZone` between every pair of
            // groups in `DesktopView.tsx:414`, which calls
            // `onMoveTab(src, 'new', tab, idx)` to splice a new group.
            //
            // We deliberately do NOT refuse the "single-tab source dropped
            // into an adjacent gap" case. Previously this was blanket-
            // refused as a suspected degenerate "drop-in-place", but for
            // single-tab rows that refuse rule blocks *every* drag-out
            // (gap_idx can only be 0 or 1 with a single group, and both
            // are adjacent). The detach + insert + prune flow produces
            // the correct state even when the visible result is identical
            // to the pre-drag state — so the refuse was costing us all
            // drag-out-to-new-window flows to fix a cosmetic no-op.
            if let Some(gap_idx) = self.gap_index_for(release_point)
                && self.split_tab_into_new_group(src_idx, tab_id, gap_idx)
            {
                *handled = true;
                continue;
            }
            // B-4: no valid drop target resolved — neither a non-closing
            // sibling bar, nor a closing sibling's bar, nor an inter-
            // group gap. The release point most commonly lands here
            // when the user drops over a *closing* sibling's content
            // area (below the bar): the closing-filter excludes it
            // from the first branch, the bar-rect check excludes it
            // from the second, and `gap_index_for` reports `None`
            // because the release is geometrically inside a group.
            // Treat as "cancel": the tab must remain in the source
            // group exactly where it started.
            //
            // The matched branches above each call `detach_tab`
            // lazily inside their own resolution path, so today an
            // unmatched release leaves the source intact already
            // (`tabs().any(...)` is true and the recovery below is
            // skipped). The recovery is a defensive guard: if a
            // future refactor ever moves the detach earlier in
            // this loop, an unmatched release would otherwise
            // silently drop the dragged tab on the floor. Sweeping
            // every other group for the orphaned id and splicing
            // it back onto source's head pins the post-condition
            // — "no valid target → tab stays in source" — at the
            // structural level rather than relying on the lazy-
            // detach pattern remaining in place.
            let tab_in_source =
                self.groups[src_idx].tabs().iter().any(|t| t.id == tab_id);
            if !tab_in_source {
                // We just confirmed source doesn't have the tab, so
                // its own `detach_tab` returns `None` and the search
                // sweeps every other group looking for the orphan.
                let recovered = self
                    .groups
                    .iter_mut()
                    .find_map(|g| g.detach_tab(tab_id));
                if let Some((desc, host)) = recovered {
                    self.groups[src_idx].insert_detached_tab(0, desc, host);
                }
            }
        }
    }

    fn name(&self) -> &str {
        "GroupRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Per-group affordances the harness can click by name.
    ///
    /// The row is the deepest node the widget-registry walk reaches —
    /// neither it nor [`WindowGroup`] exposes `children()`, because the
    /// strip lays its groups out itself rather than through taffy. So a
    /// rect that lives on a group's tab bar has to be republished here
    /// or it is unaddressable, which is what "could not resolve named
    /// target" meant for the new-tab chip.
    ///
    /// Each group's chip is keyed by its id, and the leftmost visible
    /// one also takes the bare name, so a scenario with one group can
    /// say `tabbar_new_tab` and a scenario with several can be explicit.
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
        // Every tab pill by title — `tab:Files` — so a scenario selects a
        // tab by name rather than by a coordinate that moves with the
        // layout. The first group's pills also take the bare name.
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
        // The inter-group resize handles, by seam index — so a scenario can
        // narrow a group with the real gesture instead of a layout edit.
        for (i, h) in self.handles.iter().enumerate() {
            let r = h.get_rect();
            if r.area() > 0.0 {
                out.push((format!("group_resize_handle[{i}]"), r));
            }
        }
        // Named affordances *inside* the active tile of each group — a file
        // manager's rows, a path bar — so automation can aim at what an
        // app laid out. Hosted app widgets are not children of the row
        // (the tile host owns them), so the walk happens here.
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
        // The parent Column / Stack caches its subtree scene via
        // `draw_with_presence` and only rebuilds when at least one
        // child reports `needs_redraw() == true`. A wheel-pan changes
        // `scroll_x` without changing this row's outer rect or any
        // child's leaf-level dirty bit — so without this override
        // the cached scene keeps painting the strip at the
        // pre-scroll positions (user-visible bug: "I scroll and
        // nothing happens"). The flag is cleared at the top of
        // `draw` once the new scene records.
        if self.scroll_dirty {
            return true;
        }
        // Fallback to the default "any descendant dirty" behaviour
        // so a tab-pill hover or in-tile state change still triggers
        // a redraw even when scroll_x hasn't moved.
        self.groups.iter().any(|g| g.needs_redraw())
            || self.handles.iter().any(|h| h.needs_redraw())
    }

    fn needs_animation_frame(&self) -> bool {
        // Bubble up any child group's animation signal — the default
        // `children()` impl returns an empty vec, so without this our
        // WindowGroup's TabBar drop-zone expand curve would never reach
        // the shell's overlay host and the redraw pump would stall
        // mid-animation.
        //
        // Also include our own snap-on-idle animation: while a snap is
        // in flight (or a wheel is pending the idle gate), the App
        // must keep pumping redraws so each frame re-runs
        // `apply_layout` and the eased `scroll_x` propagates to every
        // child's stored rect.
        //
        // Same for the make-space slide the groups an insertion pushed
        // along are playing: without a pumped redraw the strip would
        // freeze with them still sitting on the new group.
        self.snap_in_flight()
            || self.shift_in_flight()
            || self.groups.iter().any(|g| g.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Clear + report our own flag (so a second poll in the same tick
        // returns false), then forward to every group so a nested mutation
        // — e.g. a future tab-close-prunes-group path inside WindowGroup —
        // still bubbles up. `any` plus `|` combined so we don't
        // short-circuit the child traversal (each child must also clear).
        let own = std::mem::take(&mut self.structure_dirty);
        let children = self
            .groups
            .iter_mut()
            .map(|g| g.take_structure_dirty())
            .fold(false, |acc, d| acc || d);
        // While a snap is in flight, force the App to rebuild taffy +
        // re-run `apply_layout` next frame. Without this, `apply_layout`
        // (which is where `tick_snap_at` lives) would only run on
        // events, leaving the snap curve to evaluate against stale
        // child rects between frames. Setting `structure_dirty` is
        // heavier than strictly needed (full taffy rebuild instead of
        // just a re-layout) but matches the existing pattern other
        // animations in this file use to drive frame-by-frame updates.
        let snap = self.snap_in_flight();
        own || children || snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{TabDescriptor, TileContent, WindowWidth, min_group_px};

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

        // Tab 2 is gone from g0, present at index 0 of g1 (and active).
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
        // id 999 isn't in either group.
        assert!(!row.transfer_tab(0, 1, 999, 0));
    }

    #[test]
    fn split_tab_into_new_group_appends_with_empty_source_pruned() {
        // Source is single-tab (id 1), destination gap is at the end
        // (gap_idx=2 past the only other group). Expect: source
        // disappears, new group takes its place-plus-one (effectively
        // remains in the same x slot since the source was at index 0).
        let g0 = make_group(100, &[1]);       // single-tab — will be pruned
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 1, 2));

        // Two groups remain: g1 (with its original tabs) and the new
        // one (with just tab 1). The new group's id must be unique
        // relative to existing groups' ids.
        assert_eq!(row.groups.len(), 2);
        let first_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        let second_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(first_ids, vec![10, 11]);
        assert_eq!(second_ids, vec![1]);
        assert_ne!(row.groups[1].id, 200, "new group id must be unique");
    }

    #[test]
    fn split_tab_into_new_group_inserts_between_existing_groups() {
        // Source is multi-tab; drop lands at gap_idx=1 (between g0 and g1).
        // Source keeps its remaining tabs; new group sits between them.
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 2, 1));

        assert_eq!(row.groups.len(), 3);
        let ids: Vec<Vec<u64>> = row.groups.iter()
            .map(|g| g.tabs().iter().map(|t| t.id).collect())
            .collect();
        assert_eq!(ids[0], vec![1, 3]);
        assert_eq!(ids[1], vec![2]); // the new group with just the dropped tab
        assert_eq!(ids[2], vec![10, 11]);
        // Dropped tab must be active in its new solo group.
        assert_eq!(row.groups[1].active_tab_id(), 2);
    }

    #[test]
    fn split_tab_into_new_group_flags_structure_dirty_once() {
        // Regression guard for the "new group never renders" bug: after a
        // successful split, the row must report `take_structure_dirty()`
        // true so `App` rebuilds the taffy tree and the new group gets a
        // NodeId. The flag is edge-triggered — a second poll in the same
        // tick (without another mutation) returns false so we don't churn
        // taffy every frame.
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        // Fresh row has nothing pending.
        assert!(!row.take_structure_dirty(), "fresh row must not be dirty");

        // Split sets the flag.
        assert!(row.split_tab_into_new_group(0, 2, 1));
        assert!(
            row.take_structure_dirty(),
            "successful split must flag structure_dirty for the App to rebuild taffy"
        );
        // Flag is edge-triggered — no second pulse without another mutation.
        assert!(
            !row.take_structure_dirty(),
            "flag must clear after first poll so we don't rebuild every frame"
        );

        // A no-op split (unknown tab) must not flag the row.
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
        // Groups untouched.
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
        let ids: Vec<Vec<u64>> = row.groups.iter()
            .map(|g| g.tabs().iter().map(|t| t.id).collect())
            .collect();
        assert_eq!(ids[0], vec![1]); // the new group
        assert_eq!(ids[1], vec![2]); // original source minus tab 1
        assert_eq!(ids[2], vec![10]);
    }

    #[test]
    fn gap_index_for_classifies_inter_group_drops() {
        // Build a real taffy layout so the groups' rects are populated,
        // then probe `gap_index_for` at positions derived from those
        // rects.
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let g0 = make_group(100, &[1]);
        let g1 = make_group(200, &[10]);
        let mut row = GroupRow::new(vec![g0, g1]);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);

        // Wrap in an outer node so the row gets laid out inside a known
        // viewport (taffy won't otherwise know the row's outer bounds).
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::length(2000.0), height: Dimension::length(600.0) },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
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
        let y = (row_rect.y0 + row_rect.y1) / 2.0;

        // Left of first group → gap 0.
        assert_eq!(row.gap_index_for(Point::new(r0.x0 - 4.0, y)), Some(0));
        // Right of last group → gap N (=2).
        assert_eq!(row.gap_index_for(Point::new(r1.x1 + 4.0, y)), Some(2));
        // Inside a group's rect → None.
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 + 4.0, y)),
            None,
            "drops inside a group tile should not be inter-group gaps"
        );
        // Above the row (y outside) → still resolves to a gap. The
        // y-band restriction was removed so drops into the strip area
        // above/below the row's rect no longer silently fail (see
        // `drop_into_new_window_works_when_release_is_above_row`).
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 - 4.0, row_rect.y0 - 10.0)),
            Some(0)
        );
        // Between groups, if the layout left any gap — only assert if
        // one exists, since flex with gap=8 should give us some space.
        if r1.x0 > r0.x1 {
            let mid = (r0.x1 + r1.x0) / 2.0;
            assert_eq!(row.gap_index_for(Point::new(mid, y)), Some(1));
        }
    }

    /// Build a row with N groups (one per sub-slice) and drive a taffy
    /// layout pass so each group's rect and the row's rect are populated.
    /// The viewport defaults to 2000×600 so groups always fit and the
    /// row fills the vertical axis (`height: percent(1.0)` on each
    /// `WindowGroup`). Returns the row alongside the taffy tree + root
    /// node id so callers can invoke `handle_event` with a live tree.
    /// Usable strip width the row fixtures pin. Group widths are
    /// *fractions*, so a test only has round pixel numbers to assert
    /// against once the strip's width is fixed; 1200 puts the tiers on
    /// whole hundreds (Narrow 400 / Normal 800 / Wide 1200).
    const FIXTURE_USABLE_PX: f64 = 1200.0;

    /// A single-display span of `usable` px, as the shell pushes down.
    fn one_display(usable: f64) -> Vec<OutputSpan> {
        vec![OutputSpan { x: 0.0, width: usable, usable }]
    }

    fn make_row_with_groups_with_taffy(
        tab_specs: &[&[u64]],
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        make_row_with_groups_on_display(tab_specs, FIXTURE_USABLE_PX, 2000.0)
    }

    /// Re-run `build_layout` + `apply_layout` on an existing row, so a test
    /// that mutated group widths sees the geometry those widths produce.
    /// Returns the fresh taffy tree because the callers dispatch events
    /// against it.
    fn relayout_row_owned(mut row: GroupRow, viewport_w: f64) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(viewport_w as f32),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(viewport_w as f32),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    /// Lay a row out on a display of `usable` px inside a `viewport_w`
    /// wide window. Separate knobs because the strip deliberately
    /// overflows its viewport (that is what makes it scrollable), so the
    /// width groups size against is not the window width.
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
            .map(|(i, ids)| make_group(100 + i as u64 * 100, ids))
            .collect();
        let mut row = GroupRow::new(groups);
        row.set_output_spans(&one_display(usable));
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(viewport_w as f32),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(viewport_w as f32),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    /// Layout-only helper: discards the taffy tree + root after setting
    /// the row's rects. Most drop-zone tests only probe `gap_index_for`
    /// and don't need a live tree.
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
        Point::new((last.x1 + bar.x1) / 2.0, (bar.y0 + bar.y1) / 2.0)
    }

    fn press_at(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
        dispatch_at(row, taffy, root, WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        });
        dispatch_at(row, taffy, root, WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }

    fn dispatch_at(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, ev: WindowEvent) {
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(&ev, 1.0, &modifiers, &mut state, &path, &mut handled, &mut focus, root, taffy);
    }

    #[test]
    fn bar_grab_point_hits_the_group_grab_zone() {
        let row = make_row_with_groups(&[&[1], &[2]]);
        let p = group_grab_point(&row, 0);
        assert!(
            row.groups[0].bar_grab_hit(p),
            "the trailing tab-bar background must be a valid group-grab zone",
        );
        // A tab pill is NOT a grab zone (it drives the tab gesture).
        let pill = row.groups[0].tab_rects()[0].center();
        assert!(!row.groups[0].bar_grab_hit(pill));
    }

    #[test]
    fn drag_group_to_end_reorders_group_vector_and_publishes_one_shot() {
        // Row [100, 200, 300]. Grab group 100's header and drag it past
        // group 300's centre → it lands last: [200, 300, 100].
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        assert_eq!(row.group_ids(), vec![100, 200, 300]);

        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        // Move past group 300's centre so the insertion gap is "append".
        let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
        dispatch_at(&mut row, &taffy, root, WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(past.x, past.y),
        });
        assert!(row.group_drag_active(), "past-threshold move must engage the drag");
        assert_eq!(
            row.group_reorder_target(),
            Some(3),
            "cursor past every group's centre targets the append gap",
        );
        dispatch_at(&mut row, &taffy, root, WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });

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
        // Row [100, 200, 300]. Grab group 300 and drop it between 100 and
        // 200 → [100, 300, 200].
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 2);
        press_at(&mut row, &taffy, root, grab);
        // Aim at the left half of group 200 so the insertion gap is 1
        // (drop before group 200).
        let target = Point::new(row.groups[1].get_rect().center().x - 20.0, grab.y);
        dispatch_at(&mut row, &taffy, root, WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(target.x, target.y),
        });
        dispatch_at(&mut row, &taffy, root, WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        assert_eq!(row.group_ids(), vec![100, 300, 200]);
    }

    #[test]
    fn plain_click_on_bar_background_does_not_reorder() {
        // Press + release with no motion past the threshold is a click,
        // not a drag: the order is unchanged and nothing is published.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        dispatch_at(&mut row, &taffy, root, WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        assert_eq!(row.group_ids(), vec![100, 200, 300], "a click must not reorder");
        assert_eq!(row.take_pending_group_reorder(), None);
        assert!(!row.group_drag_active());
    }

    #[test]
    fn scroll_group_into_view_pans_to_reveal_offscreen_group() {
        // A wide strip whose content overflows the (narrow) viewport. The
        // last group starts off the right edge; scroll_group_into_view must
        // pan right so it's fully visible.
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        // Shrink the row's viewport so content overflows.
        row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        row.set_content_w_for_test(row.content_width());
        // The far-right group is group id 500 (100 + 4*100).
        let target_id = 500;
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
        // Two identical rows: one panned by the teleporting variant (the
        // reference target), one by the animated variant. The animated row
        // must end up exactly where the plain one jumped — just later,
        // via the snap curve.
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
        // Eased, not teleported: the call itself must not move the strip.
        assert_eq!(anim.scroll_x(), 0.0, "the pan is applied by ticks, not the call");
        // Mid-curve the strip is in motion, strictly between rest and target.
        anim.tick_snap_at(t0 + Duration::from_millis(SNAP_ANIM_MS / 2));
        let mid = anim.scroll_x();
        assert!(
            mid > 0.0 && mid < expected,
            "halfway through the curve scroll_x must be between 0 and {expected}, got {mid}"
        );
        // After the full duration it rests exactly on the plain target.
        anim.tick_snap_at(t0 + Duration::from_millis(SNAP_ANIM_MS + 50));
        assert_eq!(
            anim.scroll_x(),
            expected,
            "the animated pan must settle on the same target the teleporting variant picks"
        );
    }

    #[test]
    fn scroll_group_into_view_animated_is_a_noop_for_a_visible_group() {
        // Keep the fixture's full 2000 px viewport: the first group is
        // fully inside it, so there is nothing to animate.
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
        // Regression for the "drop zones show but drop has no effect" bug:
        // when the user drags a tab and releases into the strip area below
        // the row's rect (common because the shell's strip is taller than
        // any single group's rendered height, and mouse release jitter
        // pushes past the row edge), `gap_index_for` must still resolve to
        // an inter-group gap so `split_tab_into_new_group` can fire. Today
        // the y-band check in `gap_index_for` returns None for any point
        // outside `[rect.y0, rect.y1]`, silently dropping the action.
        let row = make_row_with_groups(&[&[1, 2, 3, 4]]);
        // 20 px below the row's lower edge, 4 px right of the only group's
        // right edge: classic "drop into empty strip" position.
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
        // Companion to the "below" case above: if the user drags a tab up
        // and slightly overshoots the top of the row (into the strip's
        // header chrome), the release must still splice a new group at
        // the correct gap rather than falling through silently.
        let row = make_row_with_groups(&[&[1, 2], &[10, 11]]);
        let r0 = row.groups[0].get_rect();
        // 15 px above row.y0, x left of the first group → gap 0.
        let release = Point::new(r0.x0 - 4.0, row.rect.y0 - 15.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(0),
            "release above the row should resolve to the left-of-first gap"
        );
    }

    #[test]
    fn single_tab_group_can_split_into_new_group_on_any_adjacent_gap() {
        // Regression for the refuse rule in `handle_event` that blocked
        // single-tab source groups from splitting into a new window when
        // the dropped gap was adjacent (the only kind of gap available in
        // a one-group row). The direct API has no refuse — the sweep did.
        // See `single_tab_group_split_is_not_refused_by_handle_event_sweep`
        // for the end-to-end event-driven counterpart. Keeping this as a
        // unit guard so `split_tab_into_new_group` stays callable for any
        // gap_idx regardless of source tab count.
        let mut row = make_row_with_groups(&[&[1]]);
        // Adjacent gap to the right of the sole group.
        assert!(
            row.split_tab_into_new_group(0, 1, 1),
            "single-tab group must be splittable into an adjacent gap"
        );
        // Source was pruned (empty after detach), new group took its slot.
        assert_eq!(row.groups.len(), 1);
        assert_eq!(row.groups[0].tabs().len(), 1);
        assert_eq!(row.groups[0].tabs()[0].id, 1);
        assert!(row.take_structure_dirty(), "split must flag structure_dirty");
    }

    #[test]
    fn single_tab_group_split_is_not_refused_by_handle_event_sweep() {
        // The direct `split_tab_into_new_group` API never had the refuse
        // rule — it lived in the post-dispatch sweep inside `handle_event`.
        // This test drives the sweep end-to-end: we publish a pending
        // detach on the source group's TabBar and dispatch a no-op event,
        // then assert a new group was spliced (source pruned because it
        // was single-tab). Before the fix, the sweep's
        // `src_only_tab && (left_of_src || right_of_src)` guard silently
        // refused the split.
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[7]]);
        let g0_rect = row.groups[0].get_rect();
        // Release point: right of the group (gap 1), inside the row's
        // y-band so the y-check passes today; this isolates the refuse
        // rule as the sole failing gate.
        let release = Point::new(g0_rect.x1 + 10.0, (g0_rect.y0 + g0_rect.y1) / 2.0);
        // Manually publish a detach on the source's TabBar — mimics what
        // `TabBar::handle_event` does when a drag release lands outside
        // its own rect.
        row.groups[0].bar_mut().pending_detach = Some((7, release));

        // Drive a handle_event pass; a CursorMoved event is a no-op that
        // triggers the post-dispatch detach sweep.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        // Post-sweep: the split must have fired end-to-end. The
        // observable signal is `structure_dirty` — `split_tab_into_new_group`
        // sets it on success; the refuse path never does.
        assert!(
            row.take_structure_dirty(),
            "single-tab source must split into adjacent gap: handle_event sweep should \
             flag structure_dirty. Today the `src_only_tab && adjacent` refuse rule \
             silently skips this path"
        );
        // And the group bookkeeping: row ends up with exactly one group
        // (new group inserted, then source pruned) carrying tab id 7.
        assert_eq!(row.groups.len(), 1);
        assert_eq!(row.groups[0].tabs().len(), 1);
        assert_eq!(row.groups[0].tabs()[0].id, 7);
        // And the source's pending_detach must have been drained by the
        // sweep (consumed, not left pending for the next tick).
        assert!(row.groups[0].bar_mut().pending_detach.is_none());
    }

    #[test]
    fn transfer_active_tab_promotes_sibling_in_source_and_activates_in_dest() {
        // Regression guard for the detach-then-insert activation flow:
        // when the moved tab is the source's *active* tab, the source
        // must fall through to a neighbour, and the destination must
        // promote the newly inserted tab to active (since dragging into
        // a new group implies focus follows the tab).
        let g0 = make_group(100, &[1, 2, 3]); // active = 1
        let g1 = make_group(200, &[10, 11]); // active = 10
        let mut row = GroupRow::new(vec![g0, g1]);
        assert_eq!(row.groups[0].active_tab_id(), 1);

        // Move the active tab (id 1) from g0 → g1 at index 0.
        assert!(row.transfer_tab(0, 1, 1, 0));

        // Source fell through to a neighbour (the first remaining tab).
        let src_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(src_ids, vec![2, 3]);
        assert_eq!(
            row.groups[0].active_tab_id(),
            2,
            "source must pick a surviving neighbour as the new active"
        );

        // Destination inserted the tab and promoted it to active.
        let dst_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(dst_ids, vec![1, 10, 11]);
        assert_eq!(
            row.groups[1].active_tab_id(),
            1,
            "dragged tab should be active in its new group"
        );
    }

    /// Stage a drag-in-flight on the given group's `TabBar` by setting
    /// its `pending_drag` directly. Mirrors what `TabBar::handle_event`
    /// does once the press-to-drag threshold is crossed, without needing
    /// us to replay a full press → move-past-threshold gesture. Shared
    /// by the three `hover_gap_idx` tests below.
    fn start_drag(row: &mut GroupRow, group_idx: usize, tab_id: u64) {
        use crate::dnd::DragPayload;
        row.groups[group_idx].bar_mut().pending_drag = Some(DragPayload::Tab {
            group: 0,
            tab: tab_id,
        });
    }

    /// Dispatch a `CursorMoved` at the given logical-space point through
    /// `GroupRow::handle_event`. Uses scale_factor=1 so the logical
    /// coords we pass are what `last_cursor` sees. Returns nothing — the
    /// test reads state off the row afterwards.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, taffy,
        );
    }

    #[test]
    fn hover_gap_tracks_cursor_during_drag() {
        // Regression guard for outstanding-work item #2 sub-3: while a
        // child TabBar has a drag in flight, `GroupRow` must classify
        // the cursor's current position against its inter-group gaps and
        // publish the result as `hover_gap_idx` so `draw` can paint a
        // "drop here" indicator. Without this, the user gets no visual
        // confirmation that releasing will split into a new window.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        // Target the gap between group 0 and group 1 (gap_idx == 1).
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = (row.rect.y0 + row.rect.y1) / 2.0;
        let mid_gap = Point::new((r0.x1 + r1.x0) / 2.0, mid_y);

        fire_cursor_moved(&mut row, &taffy, root, mid_gap);

        assert_eq!(
            row.hover_gap_idx,
            Some(1),
            "cursor in the gap between groups 0 and 1 must set hover_gap_idx = Some(1)"
        );
    }

    #[test]
    fn hover_gap_clears_on_release() {
        // After a drop is resolved (mouse released), the indicator must
        // vanish regardless of whether the release happened over a valid
        // gap or fell through. Leaving it set would persist a ghost
        // highlight into the next frame — the exact "zones don't go
        // away" feel-bad outstanding-work item #2 was about.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = (row.rect.y0 + row.rect.y1) / 2.0;
        let mid_gap = Point::new((r0.x1 + r1.x0) / 2.0, mid_y);
        fire_cursor_moved(&mut row, &taffy, root, mid_gap);
        assert!(row.hover_gap_idx.is_some(), "precondition: drag tracked the gap");

        // Release — the drop is now resolved. `handle_event` clears the
        // indicator unconditionally on left-button release.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        assert_eq!(
            row.hover_gap_idx, None,
            "MouseInput::Released must clear hover_gap_idx so no ghost indicator persists"
        );
    }

    /// When a child `WindowGroup` has finished its close fade
    /// (`close_complete()` returns true), the next `handle_event`
    /// tick must drop it from `self.groups` and flag
    /// `structure_dirty` so the App rebuilds the taffy tree. This
    /// is the pruning path the pre-feature code was missing — the
    /// only existing prune path was `split_tab_into_new_group`,
    /// so a tab-close-emptied group used to hang around forever.
    #[test]
    fn group_pruned_after_close_fade_completes() {
        use std::time::{Duration, Instant};
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Seed group 0 with an already-elapsed close timestamp so
        // the sweep treats it as ready-to-prune on the very next
        // tick.
        row.groups[0].set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(200),
        );
        assert_eq!(row.groups.len(), 2, "precondition: two groups before prune");

        // Drive a no-op CursorMoved to trigger the sweep.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
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

    /// Mirror of the prune test: a group whose close fade is still
    /// in flight (elapsed < `GROUP_CLOSE_FADE_MS`) must be left
    /// alone. The prune only fires once `close_complete()` returns
    /// true. Guards against a too-aggressive sweep that reaps the
    /// group before the user sees the animation.
    #[test]
    fn group_retained_during_close_fade() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Freshly-latched close — fade window has barely started,
        // far from elapsed.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        assert_eq!(
            row.groups.len(),
            2,
            "group mid-fade must NOT be pruned — the user is still watching the animation",
        );
    }

    /// A cross-group tab drop whose release point lands on a
    /// `WindowGroup` that is currently mid-close-fade must NOT be
    /// inserted into that group. `insert_detached_tab` doesn't
    /// clear `closing_at`, so the re-inserted tab would be pruned
    /// alongside the group at fade-end — the tab is silently lost.
    ///
    /// The fix filters closing groups out of the sibling bar-rect
    /// resolution, then falls through to a gap-based split: a
    /// brand-new group gets spliced in at the closing group's
    /// current slot (the gap to its LEFT), so the dragged tab
    /// ends up in a fresh adjacent window. This test seeds B as
    /// closing, drives a detach from A targeting B's bar rect,
    /// and asserts that B's tab list is unchanged and that a new
    /// group carrying A's dragged tab appears at B's position.
    #[test]
    fn drop_onto_closing_group_creates_new_group_instead() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // Two multi-tab groups so neither gets pruned by the
        // detach-empties-source rule — we want to assert specifically
        // on the drop-target side.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);

        // B is at index 1. Mark it as closing (fresh latch so the
        // prune sweep won't touch it this tick — only
        // `close_complete()` triggers prune).
        row.groups[1].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[1].is_closing());
        assert!(!row.groups[1].close_complete());
        let b_tabs_before: Vec<u64> =
            row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(b_tabs_before, vec![10, 11]);

        // Release point: inside B's bar rect. That's the exact
        // geometry the pre-fix code would resolve to B as a
        // cross-group drop target.
        let b_bar = row.groups[1].bar_rect();
        let release = Point::new(
            (b_bar.x0 + b_bar.x1) / 2.0,
            (b_bar.y0 + b_bar.y1) / 2.0,
        );
        // Publish a pending detach on A's bar — mimics what
        // `TabBar::handle_event` does when a drag release lands
        // outside A's own bar rect.
        row.groups[0].bar_mut().pending_detach = Some((1, release));

        // Drive a no-op CursorMoved so the detach sweep runs.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        // B's tab list must be untouched — the drop was diverted
        // away from the closing group.
        let b_idx = row
            .groups
            .iter()
            .position(|g| g.is_closing())
            .expect("B should still be present (mid-fade)");
        let b_tabs_after: Vec<u64> =
            row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_tabs_after, b_tabs_before,
            "closing group's tabs must be unchanged — the drop must not insert into it",
        );

        // The dragged tab (id 1) must have landed in a brand-new
        // group adjacent to (specifically: at the slot of) B.
        // Row now contains 3 groups: A (minus tab 1), new group
        // (tab 1), B (closing). The new group sits at what used
        // to be B's index.
        assert_eq!(
            row.groups.len(),
            3,
            "drop-onto-closing falls through to split-into-new-group",
        );
        let a_tabs: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(a_tabs, vec![2], "A lost its dragged tab (id 1)");
        // The new group carries only tab 1 and is NOT the closing
        // group. Find it positionally: closing group is at index 2.
        assert!(
            row.groups[2].is_closing(),
            "B (closing) should still be present at the right of the row",
        );
        let new_group_tabs: Vec<u64> =
            row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            new_group_tabs, vec![1],
            "dragged tab must end up in a fresh group, not in the closing group",
        );
        assert!(
            !row.groups[1].is_closing(),
            "the new group must NOT inherit the closing state",
        );
    }

    /// 2026-06-02 user feedback: a cross-group drag whose release lands
    /// in the TILE AREA of an existing sibling group (the white card
    /// below the tab bar) must splice the dragged tab into that group's
    /// tab list — left half → prepend (idx 0), right half → append
    /// (idx = tabs().len()). No visual indicator during hover (the
    /// affordance is invisible/implicit). This test pins both halves
    /// end-to-end through the post-dispatch detach sweep so a refactor
    /// that drops the new branch shows up red.
    #[test]
    fn drop_in_sibling_tile_area_left_half_prepends_right_half_appends() {
        use winit::event::WindowEvent;

        // Build a row where group B (idx 1) has tabs [10, 11, 12] so
        // both prepend (→ [1, 10, 11, 12]) and append (→ [10, 11, 12, 1])
        // outcomes are distinguishable from a default insert-at-zero.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11, 12]]);
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();

        // Release point inside B's tile area, on the LEFT half:
        // y below the bar, x left of B's centre.
        let release_left = Point::new(
            b_rect.x0 + (b_rect.width() * 0.25),
            (b_bar.y1 + b_rect.y1) / 2.0,
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
            &release_event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        let b_ids: Vec<u64> =
            row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_ids,
            vec![1, 10, 11, 12],
            "left-half tile drop must prepend the dragged tab to the target's tab list",
        );
        let a_ids: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            a_ids, vec![2],
            "source group must lose the dragged tab to the target",
        );
        // Dragged tab becomes the target's active tab — `insert_detached_tab`
        // pins this, mirroring the existing `transfer_tab` test.
        assert_eq!(row.groups[1].active_tab_id(), 1);

        // Reset for the right-half case using a fresh row.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11, 12]]);
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();
        let release_right = Point::new(
            b_rect.x0 + (b_rect.width() * 0.75),
            (b_bar.y1 + b_rect.y1) / 2.0,
        );
        assert!(b_rect.contains(release_right));
        assert!(!b_bar.contains(release_right));
        assert!(release_right.x > b_rect.center().x);

        row.groups[0].bar_mut().pending_detach = Some((1, release_right));
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &release_event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        let b_ids: Vec<u64> =
            row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_ids,
            vec![10, 11, 12, 1],
            "right-half tile drop must append the dragged tab to the target's tab list",
        );
    }

    /// Regression for B-4: a cross-group drop whose release point lands
    /// inside a *closing* sibling's content area (below its bar, but
    /// still inside the group's outer rect) used to fall through every
    /// resolution branch silently — the non-closing-bar branch filters
    /// closing groups out, the closing-bar branch needs the release in
    /// the bar rect, and `gap_index_for` returns `None` because the
    /// release is inside a group's `get_rect`. Without an explicit
    /// "no valid target → cancel" fallback the dragged tab is at risk
    /// of being lost. This test pins the source-keeps-the-tab invariant
    /// so the post-detach loop can never quietly drop it on the floor.
    #[test]
    fn cross_group_drop_into_closing_content_area_keeps_tab_in_source() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // Two multi-tab groups. B (index 1) goes into the closing fade
        // freshly so the prune sweep won't reap it this tick — only its
        // `is_closing()` flag is set, which is the trigger that funnels
        // the drop past both `target_idx` branches.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
        row.groups[1].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[1].is_closing());
        assert!(!row.groups[1].close_complete());

        // Release point: inside B's outer rect but BELOW B's bar rect —
        // i.e. over its tile/content area. This is the gap the bug
        // report calls out: the closing-filter excludes B from the
        // first branch, the bar-rect check excludes B from the second
        // branch, and `gap_index_for` reports `None` because the
        // release is geometrically inside a group.
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();
        let release = Point::new(
            (b_rect.x0 + b_rect.x1) / 2.0,
            (b_bar.y1 + b_rect.y1) / 2.0,
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

        // Publish a pending detach on A (source) — mimics what
        // `TabBar::handle_event` does when a drag release lands
        // outside A's own bar rect. We pre-stage the detach marker
        // (instead of replaying the full press → drag-past-threshold
        // gesture) because `press_origin` is private; the existing
        // sibling test `single_tab_group_split_is_not_refused_by_handle_event_sweep`
        // uses the same shortcut.
        let a_tabs_before: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        row.groups[0].bar_mut().pending_detach = Some((1, release));

        // Drive the actual release: `MouseInput::Released` flows
        // through `GroupRow::handle_event`'s post-dispatch detach
        // loop, which is the bug site. Using the real release event
        // (not a no-op `CursorMoved`) ensures we test the same path
        // the user actually triggers.
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
            &release_event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        // Source group must still contain its original tab list — the
        // dragged tab id 1 is still present. No data loss, no spurious
        // new group spliced in, no tab inserted into the closing group.
        let a_tabs_after: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            a_tabs_after, a_tabs_before,
            "source group must keep the dragged tab when no valid target resolves",
        );
        // Row layout untouched: still exactly two groups, B still closing.
        assert_eq!(
            row.groups.len(),
            2,
            "no new group should be spliced in for a cancelled drop",
        );
        let b_idx = row
            .groups
            .iter()
            .position(|g| g.is_closing())
            .expect("B should still be present mid-fade");
        let b_tabs_after: Vec<u64> =
            row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_tabs_after, vec![10, 11],
            "closing group's tabs must be untouched — drop did not insert into it",
        );
        // The pending_detach marker must have been drained by the
        // sweep — a leftover would re-fire next tick.
        assert!(
            row.groups[0].bar_mut().pending_detach.is_none(),
            "post-dispatch sweep must drain pending_detach even when no target resolves",
        );
    }

    #[test]
    fn hover_gap_is_none_when_no_drag() {
        // Motion without a drag must *never* populate `hover_gap_idx` —
        // the gap indicators are strictly a drag-time affordance. If we
        // classified idle cursor motion, the indicator would flash
        // during plain hover, which is not the concept's behaviour.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Intentionally NO `start_drag` call.
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = (row.rect.y0 + row.rect.y1) / 2.0;
        let mid_gap = Point::new((r0.x1 + r1.x0) / 2.0, mid_y);

        fire_cursor_moved(&mut row, &taffy, root, mid_gap);

        assert_eq!(
            row.hover_gap_idx, None,
            "cursor motion without an active drag must not publish a hover_gap_idx"
        );
    }

    /// Parallel-vec invariant: the prune sweep in `GroupRow::handle_event`
    /// removes every fully-faded group from `self.groups`. The companion
    /// `node_ids` vector is rebuilt by the next `build_layout` pass —
    /// `apply_layout` would walk a stale `node_ids` and crash if it ever
    /// outlived the prune without a rebuild gate. Two safeguards keep
    /// this honest:
    ///
    /// 1. The prune branch sets `structure_dirty = true`, so the App's
    ///    tick handler re-runs `build_layout` before the next
    ///    `apply_layout`, which clears and repopulates `node_ids`.
    /// 2. The production code path debug-asserts that `node_ids.len()`
    ///    after the prune still equals the *pre-prune* `groups.len()`
    ///    — i.e. nobody silently popped a slot off `node_ids` outside
    ///    the rebuild path. Fail-loud in debug builds; the test below
    ///    drives that assert by pruning two groups in a single tick.
    ///
    /// Gated on `cfg(debug_assertions)` because the asserts being
    /// exercised are themselves debug-only.
    #[cfg(debug_assertions)]
    #[test]
    fn group_prune_keeps_node_ids_aligned_in_debug() {
        use std::time::{Duration, Instant};
        use winit::event::WindowEvent;

        // Three groups; mark the outer two as already-elapsed so the
        // sweep prunes them on the next tick. Using non-adjacent prune
        // indices (0 and 2) verifies the highest-index-first sort
        // doesn't shift slots out from under the loop and that the
        // pre-loop count is captured before any mutation.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[0].set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(500),
        );
        row.groups[2].set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(500),
        );

        // Snapshot the pre-prune lengths. `node_ids` was populated by
        // the build_layout call inside `make_row_with_groups_with_taffy`,
        // so it must equal `groups.len()` here — that's the very
        // invariant the production debug_assert protects.
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

        // Drive a no-op CursorMoved to trigger the prune sweep. If the
        // production debug_assert ever fires (e.g. because someone
        // popped a node_ids slot outside the build_layout path), this
        // call would panic and the test would fail loudly.
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
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        // Two of three groups should be pruned. The remaining group is
        // the centre one (id-1 tab → groups[1] pre-prune).
        assert_eq!(
            row.groups.len(),
            1,
            "two fully-faded groups must be pruned in a single sweep",
        );

        // Either node_ids is in lockstep with groups (rebuilt-in-place
        // path) OR structure_dirty was set so the App rebuilds the
        // taffy tree before the next apply_layout pass. The plan
        // explicitly accepts either; we just refuse the third option
        // (drift with no rebuild gate) which would crash apply_layout.
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

    /// Helper that lays out N groups inside a *narrow* viewport so the
    /// strip's natural content width exceeds the row's rect — the
    /// precondition for horizontal scroll. Each `WindowWidth::Normal`
    /// group is 600 px so `count` × 600 + `count-1` × 8 (gap) is the
    /// natural content width; we set the viewport width so it's
    /// strictly less.
    fn make_overflow_row(count: usize, viewport_w: f64) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let groups: Vec<WindowGroup> = (0..count)
            .map(|i| make_group(100 + i as u64 * 100, &[i as u64 + 1]))
            .collect();
        let mut row = GroupRow::new(groups);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(viewport_w as f32),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(viewport_w as f32),
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
        // Invariant: when the sum of `WindowGroup`s' MINIMUM widths
        // (WIDTH_MIN_PX = 300) exceeds the strip's viewport, the strip
        // can no longer shrink them all to fit and `content_width()`
        // stays above viewport. `max_scroll_x()` then engages so the
        // wheel-pan / snap machinery has positive headroom.
        //
        // Original design (pre-2026-05-25) pinned `flex_shrink: 0` on
        // every child so even 4 Normal groups (4*600=2400) overflowed
        // a 1200 viewport. New design (`flex_shrink: 1`, `min_size:
        // 300`) lets groups shrink to fit until the minimums also
        // overflow — so the test now uses 8 groups so the sum-of-mins
        // (8*300 + 7*8 = 2456) clears the 1200 viewport regardless.
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

    /// Two windows never occupy the same place on a strip.
    ///
    /// `group_canvas_spans` is the function the shell reads to decide
    /// where a launch lands (`ShellOverlayHost::publish_strip_viewport`
    /// feeds it to `placement::placement_for_new_window`), so a strip
    /// whose spans overlap would seat a new window on top of one that is
    /// already there. This guards a frame that showed two word-processor
    /// windows stacked at 100% overlap — an outcome nothing in the strip
    /// layer asserted against, despite this being the only geometry the
    /// placement rule ever sees.
    ///
    /// Every pair is compared, not just neighbours: a single negative
    /// step would show up between adjacent spans, but two groups sharing
    /// an origin shows up only under a full pairwise sweep.
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

    #[test]
    fn drop_in_visible_trailing_area_creates_new_group_at_end() {
        // After a horizontal scroll has shifted the strip, a release
        // past the last visible group's right edge must still resolve
        // to the trailing gap (gap_idx = N) so the dropped tab spawns
        // a new group at the end. This is the Niri-style "infinite
        // strip" trailing drop zone — and the user's reported flow
        // ("drag a tab past the rightmost group → new group there").
        //
        // We don't need to actually scroll: with a narrow viewport,
        // the last group already sits past `row.rect.x1` because
        // `flex_shrink: 0.0` lets children overflow.
        let (mut row, _t, _r) = make_overflow_row(3, 1000.0);
        let last_rect = row.groups.last().unwrap().get_rect();
        let release = Point::new(last_rect.x1 + 50.0, last_rect.y0 + 20.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(3),
            "release past the last group's right edge must resolve to gap_idx=N"
        );
        // And the split actually fires for that gap.
        let src_tab = row.groups[0].tabs()[0].id;
        assert!(
            row.split_tab_into_new_group(0, src_tab, 3),
            "split_tab_into_new_group must succeed at the trailing gap"
        );
        // Source had only one tab, so it's pruned and the new group
        // sits at the original tail position. Net group count is 3.
        assert_eq!(row.groups.len(), 3);
        // The dropped tab must be the last group's only tab.
        let last_ids: Vec<u64> = row.groups.last().unwrap().tabs().iter().map(|t| t.id).collect();
        assert_eq!(last_ids, vec![src_tab]);
    }

    #[test]
    fn wheel_over_strip_never_moves_it() {
        // Hard boundary (2026-08-22): a bare wheel must NOT move the
        // strip — not a pan, not a jump. The view travels only through
        // focus changes and the shell's modifier-wheel gestures. This
        // holds both when content overflows the viewport and when it
        // fits, and the event is left unclaimed.
        use winit::dpi::PhysicalPosition;

        for (mut row, taffy, root) in [
            make_overflow_row(8, 1200.0),
            make_row_with_groups_with_taffy(&[&[1], &[10]]),
        ] {
            let pre_rect_0 = row.groups[0].get_rect();
            let cy = (row.rect.y0 + row.rect.y1) / 2.0;
            let cx = (row.rect.x0 + row.rect.x1) / 2.0;
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
            assert!(
                !handled,
                "a bare wheel must be left unclaimed by the strip"
            );
            // Give any (wrongly started) eased travel every chance to
            // show itself before reading the position.
            row.tick_snap_at(Instant::now() + Duration::from_millis(400));
            assert_eq!(
                row.scroll_x(),
                0.0,
                "a bare wheel must not move the strip"
            );
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
        // After the user wheel-pans the strip past a group boundary
        // and stops, the strip must ease `scroll_x` to the nearest
        // group's left edge with a cubic-out curve over ~200 ms.
        // Mirrors the concept's `snap-x snap-proximity` behaviour in
        // `DesktopView.tsx:396`.
        //
        // Synthesise the geometry directly (three 600-px groups, 8 px
        // gap → left edges at 0, 608, 1216) so we don't depend on
        // taffy's flex math for the precise stop positions.
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
        row.set_content_w_for_test(1816.0);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
        // Pretend we panned to mid-second-group (scroll_x = 250) and
        // stopped wheeling 250 ms ago. Nearest left-edge to 250 is 0
        // (distance 250) vs 608 (distance 358) — first group wins,
        // BUT 250 is closer to 0 by simple |edge - scroll_x|. To
        // exercise the "snap to the SECOND group's left edge" branch
        // we start from 400 (closer to 608 than to 0).
        row.set_scroll_x_for_test(400.0);
        let base = Instant::now();
        row.set_last_wheel_at_for_test(base - Duration::from_millis(250));

        // First tick after idle: snap kicks off and advances ~25 % of
        // the way (50 ms into a 200 ms cubic-out curve, so eased
        // ≈ 1 - (1 - 0.25)^3 ≈ 0.578). scroll_x must have moved
        // toward 608 but not yet arrived.
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

        // After the full 200 ms, scroll_x must have settled at the
        // target group's left edge.
        row.tick_snap_at(base + Duration::from_millis(250));
        assert!(
            (row.scroll_x() - 608.0).abs() < 1.0,
            "snap must settle at 608.0 after 200 ms; got {}",
            row.scroll_x()
        );
    }

    #[test]
    fn snap_does_not_fire_during_wheel_idle_window() {
        // Before the 200 ms idle threshold elapses, the snap must
        // NOT fire — we don't want the strip pulling itself out from
        // under the user mid-wheel.
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
        row.set_content_w_for_test(1816.0);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
        row.set_scroll_x_for_test(400.0);
        let base = Instant::now();
        // Only 100 ms since the last wheel — well inside the idle
        // window.
        row.set_last_wheel_at_for_test(base - Duration::from_millis(100));

        row.tick_snap_at(base);
        assert_eq!(
            row.scroll_x(),
            400.0,
            "snap must not fire while still inside the idle window",
        );
    }

    #[test]
    fn wheel_mid_snap_leaves_the_snap_untouched() {
        // A wheel notch arriving while a snap is easing must not
        // perturb it in any way — the bare wheel is a no-op for the
        // strip (hard boundary 2026-08-22), so the in-flight curve
        // keeps its target and settles exactly where it was headed.
        use winit::dpi::PhysicalPosition;

        // 8 groups → sum-of-mins exceeds the 1200 px viewport so
        // overflow + scroll engage regardless of shrink. Matches the
        // updated `content_width_exceeds_viewport_when_groups_overflow`.
        let (mut row, taffy, root) = make_overflow_row(8, 1200.0);
        // Stage an in-flight snap directly so we don't have to wait
        // out a real idle window.
        row.set_scroll_x_for_test(300.0);
        let base = Instant::now();
        row.set_last_wheel_at_for_test(base - Duration::from_millis(250));
        row.tick_snap_at(base);
        assert!(
            row.snap_anim.is_some(),
            "precondition: snap must be in flight",
        );

        // Drive a fresh wheel event. The cursor must be inside the
        // row's rect for the wheel handler to claim it.
        let cy = (row.rect.y0 + row.rect.y1) / 2.0;
        let cx = (row.rect.x0 + row.rect.x1) / 2.0;
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
            1.0, &modifiers, &mut state, &root_obj,
            &mut handled, &mut focus, root, &taffy,
        );
        row.handle_event(
            &WindowEvent::MouseWheel {
                device_id: dev,
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0, &modifiers, &mut state, &root_obj,
            &mut handled, &mut focus, root, &taffy,
        );
        let anim = row
            .snap_anim
            .expect("the in-flight snap must survive a bare wheel notch");
        // The idle snap was easing toward the stop nearest 300; the
        // notch must not have re-targeted it.
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
        // And it settles exactly there.
        row.tick_snap_at(Instant::now() + Duration::from_millis(400));
        assert!(
            (row.scroll_x() - expected).abs() < 0.5,
            "the snap must settle on its original target ({expected}); \
             got {}",
            row.scroll_x()
        );
    }

    /// User-reported freeze: drag a tab out → group 2 created, fine.
    /// Drag a second tab into the gap between groups 1 and 2 → freeze.
    /// This test runs the EXACT detach+drop sequence twice in a row to
    /// catch the regression at the GroupRow level. If it hangs or
    /// panics, the bug is here. If it passes, the freeze is one layer
    /// up (App tick, taffy rebuild, etc.).
    #[test]
    fn two_sequential_splits_do_not_hang() {
        use taffy::AvailableSpace;
        // Start with one 4-tab group, matching the live shell's
        // bootstrap_default Home workspace.
        let (mut row, mut taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2, 3, 4]]);

        // DROP #1: tab id 4 dropped to the right of the only group's
        // bar → should create a 2nd group.
        let g0_rect = row.groups[0].get_rect();
        let release_1 = Point::new(g0_rect.x1 + 30.0, g0_rect.y0 + 6.0);
        row.groups[0].bar_mut_for_tests().pending_detach = Some((4, release_1));

        let theme = Theme::dark();
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
            1.0, &modifiers, &mut state, &root_obj,
            &mut handled, &mut focus, root, &taffy,
        );
        // Mimic the App's RedrawRequested path: if any widget reports
        // structure_dirty, rebuild taffy + node_map so the new group
        // gets a node and a rect on the next layout pass.
        assert_eq!(row.groups.len(), 2, "drop 1 must produce 2 groups");
        assert!(row.take_structure_dirty(), "split must flag structure_dirty");
        let mut new_taffy: TaffyTree<()> = TaffyTree::new();
        let mut new_node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let new_root_child = row.build_layout(&mut new_taffy, &theme, &mut new_node_map);
        let new_root = new_taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[new_root_child],
            )
            .unwrap();
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
        let taffy = new_taffy;
        let root = new_root_child;

        // DROP #2: tab id 3 dropped INTO THE GAP between group 0 and
        // group 1 → should create a 3rd group between them. This is
        // the case the user reported freezes.
        let g0_rect = row.groups[0].get_rect();
        let g1_rect = row.groups[1].get_rect();
        eprintln!("after drop 1: g0={g0_rect:?} g1={g1_rect:?}");
        eprintln!("  g0 bar_rect={:?}", row.groups[0].bar_rect());
        eprintln!("  g1 bar_rect={:?}", row.groups[1].bar_rect());
        eprintln!("  row.rect={:?}", row.rect);
        let gap_mid = (g0_rect.x1 + g1_rect.x0) / 2.0;
        let release_2 = Point::new(gap_mid, g0_rect.y0 + 6.0);
        eprintln!("  release_2={release_2:?}");
        eprintln!("  gap_index_for(release_2)={:?}", row.gap_index_for(release_2));
        row.groups[0].bar_mut_for_tests().pending_detach = Some((3, release_2));

        let mut handled2 = false;
        row.handle_event(
            &winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &root_obj,
            &mut handled2, &mut focus, root, &taffy,
        );
        assert_eq!(
            row.groups.len(), 3,
            "drop 2 must produce 3 groups (was {})", row.groups.len()
        );
    }

    // ==================================================================
    // Inter-group `ResizeHandle` integration
    // ==================================================================

    /// Two groups should produce exactly one handle, three groups should
    /// produce two, single-group rows should produce none. The pool's
    /// `N-1` invariant is structural — verifying it here pins the
    /// `build_layout` resize policy so a refactor can't silently leak
    /// or under-allocate handles. Pairs with the unit-level
    /// `set_neighbour_rects_recomputes_hit_area` test on the handle
    /// itself.
    #[test]
    fn handle_pool_matches_inter_group_gap_count() {
        // 1 group → 0 handles.
        let row = make_row_with_groups(&[&[1]]);
        assert_eq!(row.handle_count(), 0, "single-group row needs no handle");

        // 2 groups → 1 handle.
        let row = make_row_with_groups(&[&[1], &[2]]);
        assert_eq!(row.handle_count(), 1, "two-group row has exactly one handle");

        // 3 groups → 2 handles.
        let row = make_row_with_groups(&[&[1], &[2], &[3]]);
        assert_eq!(row.handle_count(), 2, "three-group row has exactly two handles");
    }

    /// After a successful split-into-new-group, the next `build_layout`
    /// must extend the handle pool to match the new group count.
    /// Driven via the structural mutation flag the App already polls.
    #[test]
    fn handle_pool_grows_on_split_into_new_group() {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize};

        let mut row = make_row_with_groups(&[&[1, 2]]);
        assert_eq!(row.handle_count(), 0, "precondition: one-group row has no handle");

        // Split tab 2 into a brand-new group at the trailing gap.
        assert!(row.split_tab_into_new_group(0, 2, 1));
        assert_eq!(row.groups.len(), 2);
        assert!(row.take_structure_dirty(), "split must flag structure_dirty");

        // Re-run build_layout (what the App does on structure_dirty)
        // — the handle pool must grow to 1.
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
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
        // And the handle's neighbour rects must be populated — set
        // by apply_layout. A degenerate (zero-area) rect would mean
        // the handle has nothing to paint.
        let handle = row.handle_at(0).expect("handle 0 must exist");
        let r = handle.get_rect();
        assert!(r.width() > 0.0, "handle rect width must be positive after apply_layout");
        assert!(r.height() > 0.0, "handle rect height must be positive after apply_layout");
    }

    /// End-to-end press-drag-release on the inter-group handle: the row
    /// must dispatch the events to the handle, apply the resulting
    /// width pulses to both neighbours, and stamp pending width-changes
    /// on release so the shell persists. This is the closing-the-loop
    /// test the task brief calls out — exercising the actual
    /// production code path the user's mouse drives.
    #[test]
    fn drag_handle_resizes_both_neighbours_via_set_width() {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        assert_eq!(row.handle_count(), 1);

        // Capture starting widths. Both groups default to
        // `WindowWidth::Normal` — two thirds of the fixture's 1200 px
        // display, i.e. 800 px.
        let pre_left_w = row.groups[0].resolved_width_px();
        let pre_right_w = row.groups[1].resolved_width_px();
        assert_eq!(pre_left_w, 800.0);
        assert_eq!(pre_right_w, 800.0);

        // Resolve handle centre from the live row geometry.
        let handle_rect = row.handle_at(0).unwrap().get_rect();
        let cx = (handle_rect.x0 + handle_rect.x1) / 2.0;
        let cy = (handle_rect.y0 + handle_rect.y1) / 2.0;

        let dev = unsafe { std::mem::zeroed() };
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;

        // 1. CursorMoved over the handle → hover engages.
        row.handle_event(
            &WindowEvent::CursorMoved { device_id: dev, position: PhysicalPosition::new(cx, cy) },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
        );
        assert!(
            row.handle_at(0).unwrap().is_hovered(),
            "cursor over handle hit area must engage hover",
        );

        // 2. MouseInput::Pressed → drag starts.
        handled = false;
        row.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
        );
        assert!(handled, "press on handle must claim the event");
        assert!(row.handle_at(0).unwrap().is_dragging(), "press starts drag");

        // 3. CursorMoved +50 px right → width pulse applied to both
        //    neighbours via `set_width`.
        handled = false;
        row.handle_event(
            &WindowEvent::CursorMoved { device_id: dev, position: PhysicalPosition::new(cx + 50.0, cy) },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
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

        // Mid-drag must NOT stamp pending changes — that's
        // reserved for release. Drain both groups; both must be
        // None.
        assert_eq!(
            row.groups[0].take_pending_width_change(),
            None,
            "mid-drag does NOT pulse pending_width_change",
        );
        assert_eq!(row.groups[1].take_pending_width_change(), None);

        // 4. MouseInput::Released → drag ends, BOTH neighbours
        //    receive a pending width-change pulse.
        handled = false;
        row.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
        );
        assert!(handled, "release on handle must claim the event");
        assert!(!row.handle_at(0).unwrap().is_dragging(), "release ends drag");
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

    /// A handle drag that pushes the left neighbour past the display's
    /// minimum group width must NOT shrink it further. The right
    /// neighbour absorbs only the actual movement up to the clamp point
    /// — equivalent to the `compute_widths_clamps_left_at_min` unit test
    /// but driven through the row's full event dispatch path.
    #[test]
    fn drag_clamps_left_neighbour_at_minimum_width() {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Start both groups at `Narrow` (a third of the 1200 px display =
        // 400 px each). The pair sum has to leave the right neighbour room
        // to absorb the movement: from two `Normal`s the left neighbour
        // cannot reach the minimum at all, because the right would have to
        // grow past the display to make room.
        row.groups[0].set_width(WindowWidth::Narrow);
        row.groups[1].set_width(WindowWidth::Narrow);
        drop((taffy, root));
        let (mut row, taffy, root) = relayout_row_owned(row, 2000.0);

        // Resolve handle centre.
        let handle_rect = row.handle_at(0).unwrap().get_rect();
        let cx = (handle_rect.x0 + handle_rect.x1) / 2.0;
        let cy = (handle_rect.y0 + handle_rect.y1) / 2.0;

        let dev = unsafe { std::mem::zeroed() };
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;

        // Press the handle.
        row.handle_event(
            &WindowEvent::CursorMoved { device_id: dev, position: PhysicalPosition::new(cx, cy) },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
        );
        row.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
        );
        // Drag 500 px LEFT. Both groups start at 400 px; pushing left by
        // 500 would land left at -100, below the display's minimum of
        // `min_group_px(1200) = 240`. The clamp pins left at 240; right
        // absorbs only the actual 160 px the left moved, settling at 560.
        row.handle_event(
            &WindowEvent::CursorMoved { device_id: dev, position: PhysicalPosition::new(cx - 500.0, cy) },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
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

    /// During a tab drag-in-flight, the inter-group handle must NOT
    /// receive events — the cross-group drop resolution owns the gap.
    /// Otherwise a fast user dragging a tab past the gap centre would
    /// accidentally start a resize on the same frame.
    #[test]
    fn handle_does_not_engage_during_tab_drag_in_flight() {
        use winit::dpi::PhysicalPosition;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Stage a tab-drag on group 0's bar.
        start_drag(&mut row, 0, 1);
        assert!(row.groups[0].drag_in_flight());

        let handle_rect = row.handle_at(0).unwrap().get_rect();
        let cx = (handle_rect.x0 + handle_rect.x1) / 2.0;
        let cy = (handle_rect.y0 + handle_rect.y1) / 2.0;

        let dev = unsafe { std::mem::zeroed() };
        let modifiers = Modifiers::default();
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;

        // Cursor over the handle — but tab drag is in flight, so
        // the handle's hover branch must NOT activate.
        row.handle_event(
            &WindowEvent::CursorMoved { device_id: dev, position: PhysicalPosition::new(cx, cy) },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, root, &taffy,
        );
        assert!(
            !row.handle_at(0).unwrap().is_hovered(),
            "tab drag in flight must inhibit handle hover (cross-group drop owns the gap)",
        );
    }

    /// Concept-parity guard: when ANY group is maximized, every sibling
    /// must collapse to zero rendered area (matches the concept's
    /// `position: fixed; width: 100vw` overlay in `WindowFrame.tsx:188`
    /// where siblings stay in the DOM but are obscured), the maximized
    /// group must absorb the row's full inner width, and no inter-group
    /// resize handles must be present.
    ///
    /// Pre-fix the row used `flex_grow: 1e6 + flex_basis: 0` to make
    /// siblings squeeze toward min-content, but siblings remained
    /// visible at non-zero width. That's not the concept's behaviour —
    /// the maximized frame fully covers the strip.
    #[test]
    fn maximized_group_hides_siblings_and_absorbs_row_width() {
        // Three groups so we can verify BOTH a left-of-max and a
        // right-of-max sibling collapse, not just one.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        // Sanity: pre-maximize, every group has positive width and a
        // resize handle exists between each adjacent pair.
        assert!(row.groups[0].get_rect().width() > 0.0);
        assert!(row.groups[1].get_rect().width() > 0.0);
        assert!(row.groups[2].get_rect().width() > 0.0);
        assert_eq!(row.handle_count(), 2, "two gaps → two handles pre-maximize");

        // Flip group 1 (the middle one) to maximized and re-run the
        // full layout pass, mirroring what the App does after
        // `take_structure_dirty()` reports true on a maximize toggle.
        row.groups[1].maximized = true;
        let theme = Theme::dark();
        let mut taffy_new: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let new_root = row.build_layout(&mut taffy_new, &theme, &mut node_map);
        use taffy::AvailableSpace;
        let viewport = taffy_new
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[new_root],
            )
            .unwrap();
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

        // Siblings (indices 0 and 2) must collapse to zero width — the
        // concept's "obscured behind the fixed overlay" outcome,
        // modelled here via `display: None` on the taffy nodes.
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

        // The maximized group must absorb the row's inner width (within
        // a small fp tolerance for taffy rounding). It paints the
        // concept's `width: 100vw` fixed overlay.
        let r1 = row.groups[1].get_rect();
        let row_w = row.rect.width();
        assert!(
            (r1.width() - row_w).abs() < 1.0,
            "maximized group must absorb the row's full width; got group={} row={}",
            r1.width(),
            row_w,
        );

        // No inter-group resize handle nodes were built in the maximize
        // branch — they would be visible affordances on collapsed gaps.
        // The pool itself stays sized to N-1 (the handle_pool always
        // mirrors the gap count for structural simplicity), but
        // `handle_node_ids` must be empty so no draw / dispatch runs.
        assert_eq!(
            row.handle_node_ids.len(),
            0,
            "no handle taffy nodes must be built while a group is maximized",
        );
    }

    /// Restoring (un-maximizing) must put the row back to its
    /// pre-maximize layout: every sibling visible at positive width,
    /// every inter-group handle re-created. Guards against a one-way
    /// latch where the maximize branch leaves stale state behind.
    #[test]
    fn unmaximize_restores_siblings_and_handles() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        // Maximize then immediately un-maximize, re-running layout
        // after each flip.
        row.groups[1].maximized = true;
        let theme = Theme::dark();
        let mut relayout = |row: &mut GroupRow| -> TaffyTree<()> {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let root = row.build_layout(&mut taffy, &theme, &mut node_map);
            use taffy::AvailableSpace;
            let viewport = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(2000.0),
                            height: Dimension::length(600.0),
                        },
                        ..Default::default()
                    },
                    &[root],
                )
                .unwrap();
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

        // Now restore.
        row.groups[1].maximized = false;
        let _ = relayout(&mut row);

        for (i, g) in row.groups.iter().enumerate() {
            assert!(
                g.get_rect().width() > 0.0,
                "group {} must regain positive width after un-maximize",
                i,
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
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        use taffy::AvailableSpace;
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
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

    /// A transient fullscreen promotion collapses siblings and absorbs the
    /// row's full width — the same layout outcome as `maximized`, but driven
    /// by the compositor's live fullscreen state (`set_fullscreen`) rather than
    /// the persisted flag, and without any group carrying `maximized`.
    #[test]
    fn fullscreen_group_collapses_siblings_and_absorbs_row() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        assert!(!row.is_fullscreen());
        // Promote the middle group (id 200) to fullscreen — no `maximized`
        // flag is set on any group.
        row.set_fullscreen(Some(200), false);
        assert!(row.groups.iter().all(|g| !g.maximized), "no persisted maximize");
        relayout_row(&mut row);

        assert!(row.is_fullscreen(), "row reports it owns the fullscreen group");
        // Siblings collapse to zero rendered width.
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
        // Promoted group absorbs the full row width and pins flush at x0 (the
        // wheel gutter is dropped because the sidebar is suppressed).
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
        assert_eq!(row.handle_node_ids.len(), 0, "no resize handles under fullscreen");
    }

    /// Exiting fullscreen restores the EXACT prior tiling layout — every
    /// sibling rect returns to its pre-fullscreen geometry and the resize
    /// handles are rebuilt. This is the compositor-visible "restores exactly
    /// on exit" invariant, proven at the widget layer.
    #[test]
    fn exiting_fullscreen_restores_prior_layout_exactly() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        // Snapshot the settled tiling geometry before any promotion.
        let before: Vec<Rect> = row.groups.iter().map(|g| g.get_rect()).collect();
        let handles_before = row.handle_node_ids.len();
        assert_eq!(handles_before, 2);

        // Enter fullscreen, then exit.
        row.set_fullscreen(Some(300), false);
        relayout_row(&mut row);
        assert!(row.is_fullscreen());
        row.set_fullscreen(None, false);
        relayout_row(&mut row);

        assert!(!row.is_fullscreen(), "exit clears the fullscreen promotion");
        let after: Vec<Rect> = row.groups.iter().map(|g| g.get_rect()).collect();
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

    /// A strip that owns no part of the fullscreen surface is hidden outright
    /// (its whole row collapses to zero area), and un-hiding restores it.
    #[test]
    fn hidden_row_collapses_and_restores() {
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let before: Vec<Rect> = row.groups.iter().map(|g| g.get_rect()).collect();
        assert!(before.iter().all(|r| r.width() > 0.0));

        row.set_fullscreen(None, true);
        relayout_row(&mut row);
        // Under a Display::None row, every child collapses to zero area.
        for (i, g) in row.groups.iter().enumerate() {
            let r = g.get_rect();
            assert!(
                r.width() <= 0.5 && r.height() <= 0.5,
                "group {i} in a hidden strip must collapse; got {r:?}",
            );
        }

        row.set_fullscreen(None, false);
        relayout_row(&mut row);
        let after: Vec<Rect> = row.groups.iter().map(|g| g.get_rect()).collect();
        for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
            assert!(
                (b.width() - a.width()).abs() < 0.5,
                "group {i} width restores after un-hide: before {} after {}",
                b.width(),
                a.width(),
            );
        }
    }

    // -----------------------------------------------------------------
    // Making room. A strip has unlimited space to the right, so the way
    // to fit a new window between two others is to move the ones on its
    // right along — never to squeeze anybody.
    // -----------------------------------------------------------------

    /// The whole of rule "windows that move do not shrink", asserted on
    /// the geometry a real `build_layout` + `apply_layout` produces
    /// either side of a real `insert_group`.
    #[test]
    fn making_room_translates_the_groups_it_pushes_and_never_narrows_them() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let before: Vec<(u64, Rect)> = row
            .groups
            .iter()
            .map(|g| (g.id, g.get_rect()))
            .collect();

        // A window opening at the border between the first two.
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
            row.shift_offset_for(unmoved), 0.0,
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

    /// A window opened at the end of the strip moves nobody, so nothing
    /// animates. The strip just gets longer, which it always can.
    #[test]
    fn appending_at_the_end_animates_nothing() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let existing: Vec<u64> = row.groups.iter().map(|g| g.id).collect();

        let at_end = row.groups.len();
        row.insert_group(make_group(999, &[9]), at_end);
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);

        for id in existing {
            assert_eq!(
                row.shift_offset_for(id), 0.0,
                "nothing had to move, so group {id} must not animate",
            );
        }
        assert_eq!(row.shift_anim_t(), 1.0, "and no slide is in flight");
    }

    /// The strip keeps pumping frames while the slide plays — otherwise
    /// it would freeze with the neighbours still sitting on the new
    /// window.
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

/// Every named rect in `w`'s subtree: its `test_id`, its `test_rects`, and
/// the same for each child. Zero-area rects are skipped — a name means "on
/// screen now".
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
