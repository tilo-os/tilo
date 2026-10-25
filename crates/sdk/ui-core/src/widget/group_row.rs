//! `GroupRow` — a horizontal flex container of `WindowGroup`s that also
//! routes cross-group tab drag-drop: it intercepts release events after
//! every child `TabBar` has drained its own drag, and if one published a
//! `pending_detach`, splices the tab + `TileHost` into a matching sibling.
//! Within-group reorder still lives in `TabBar` / `WindowGroup`.

use crate::num::f32_of_f64;
use crate::time::{Duration, Instant};
use crate::tokens::MotionSource;
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use taffy::{
    Dimension, Display, FlexDirection, LengthPercentageAuto, NodeId, Overflow, Position, Size,
    Style, TaffyTree,
};
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::TileContent;
use crate::widget::{
    DEFAULT_STRIP_USABLE_PX, EdgeGrab, FloatingSurfaceProvider, HANDLE_SLOT_PX, ResizeHandle,
    TabDescriptor, TileHost, Widget, WindowGroup, WindowWidth,
};

#[path = "group_row_edges.rs"]
mod edges;
use edges::RowEdgeDrag;

/// Idle time before the strip snaps to the nearest group's left edge.
const SNAP_IDLE_MS: u32 = 200;
/// A group this close to its laid-out x, and this slow, has arrived.
const MOVE_SETTLED_PX: f64 = 0.25;
const MOVE_SETTLED_PX_PER_S: f64 = crate::tokens::motion::MOVE_SETTLED_PX_PER_S; // rest velocity in px/s, not a Theme gap
/// Two pan targets closer than this are the same place on screen.
const SNAP_SETTLED_PX: f64 = 0.5;

/// How long a drag rests on a background tab before it comes to the front.
pub(crate) const DRAG_SPRING_LOAD: Duration = Duration::from_millis(500);

/// What a drag (a tab, or content the compositor carries) is reaching for
/// beyond what is on screen. Strips are unbounded and windows hold several
/// tabs, so the drop target is usually off screen or behind another tab:
/// resting the drag at a lane's edge pans the strip under it, faster the
/// deeper it is, and resting it on a background tab brings that tab to the
/// front, so the drop can land there without dropping, panning and dragging
/// again.
#[derive(Clone, Copy, Debug, Default)]
struct DragReach {
    /// The edge pan's frame clock.
    edge: crate::edge_autoscroll::EdgeAutoscroll,
    /// The background tab under the drag, its group, and since when.
    tab: Option<(u64, u64, Instant)>,
}
/// Release velocity (px/s) below which lifted fingers leave the strip where
/// it is rather than throwing it.
const FLING_MIN_VELOCITY: f64 = crate::tokens::motion::FLING_MIN_VELOCITY;

/// A tab dragged over a window body gets a generous before/after target at
/// each outer edge. The target follows the window size, but stays useful on a
/// compact window and does not consume an absurd amount of an ultrawide one.
const TAB_GROUP_EDGE_FRAC: f64 = 0.25;
const TAB_GROUP_EDGE_MIN_PX: f64 = crate::tokens::Spacing::BASE.s12 as f64;
const TAB_GROUP_EDGE_MAX_PX: f64 = crate::tokens::Spacing::BASE.s12 as f64 * 3.0;
const TAB_GROUP_EDGE_MAX_FRAC: f64 = 0.32;
/// Keep a resolved target through a few pixels of boundary jitter. Release
/// commits the retained preview verbatim, so the window cannot land somewhere
/// other than the place the indicator showed.
const TAB_DROP_HYSTERESIS_PX: f64 = crate::tokens::Spacing::BASE.s2 as f64;

fn tab_group_edge_width(group_width: f64) -> f64 {
    (group_width * TAB_GROUP_EDGE_FRAC)
        .clamp(TAB_GROUP_EDGE_MIN_PX, TAB_GROUP_EDGE_MAX_PX)
        // Degenerate test/layout sizes must still retain an interior.
        .min(group_width * TAB_GROUP_EDGE_MAX_FRAC)
}

/// Empty canvas kept after the final group at the legal end of a strip.
/// A fifth of the viewport is enough breathing room to make the end legible
/// without letting the focused app feel abandoned in the middle of the screen.
const TRAILING_REVEAL_FRAC: f64 = 0.20;
/// Wallpaper a desktop keeps at both ends of a strip, as its leading inset
/// ([`GroupRow::with_leading_inset`]) and, through the usable width it
/// sizes windows against, at the trailing end: without it a window at the
/// strip's home or end has its border, rounded corners and shadow cut off
/// by the screen edge. A window casts `e2` (blur 8, a visible falloff of
/// about 10 px plus a 2 px offset), so `s8` holds the whole shadow.
pub const STRIP_EDGE_GUTTER_PX: f64 = crate::tokens::Spacing::BASE.s8 as f64;
/// Wallpaper a desktop keeps under a strip's tiled windows, so their bottom
/// border and rounded corners stand clear of the screen's bottom edge. A
/// maximized or fullscreen window takes the band.
pub const STRIP_BOTTOM_GUTTER_PX: f64 = crate::tokens::Spacing::BASE.s3 as f64;
/// Maximum *additional* visual give while a three-finger pan pulls beyond a
/// legal end. The shared rubber-band curve approaches this asymptotically.
const STRIP_OVERSCROLL_REACH_FRAC: f64 = 0.28;
/// How far a group's shadow sprites paint past its rect: `bloom_pad`
/// (120 px) in `TileHost::draw_steady_state`, with the clip's margin.
/// Not a Theme spacing token: it must match the compositor shadow draw,
/// not layout padding a theme can resize.
const GROUP_SPRITE_REACH_PX: f64 = crate::tokens::motion::GROUP_SPRITE_REACH;

/// Touchpad px per wheel detent for the discrete jump gestures; also the
/// scale for hi-res wheel notches reported as pixel deltas.
pub const JUMP_PX_PER_DETENT: f64 = crate::tokens::Layout::BASE.jump_px_per_detent as f64;

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

/// A pan or a group's drawn offset at rest at `at`, arriving within a
/// quarter pixel once sent somewhere.
fn strip_motion(at: f64) -> crate::motion::MotionValue<f64> {
    crate::motion::MotionValue::new(at).with_tolerance(MOVE_SETTLED_PX)
}

/// The pan at rest at `from`, to snap to `to` on a [`crate::motion::Snap`]
/// timing: it arrives within a thousandth of its travel (a quarter pixel at
/// least), as a token spring run once does, so a snap is over within its
/// token however far it goes.
fn snap_motion(from: f64, to: f64) -> crate::motion::MotionValue<f64> {
    crate::motion::MotionValue::new(from)
        .with_tolerance(MOVE_SETTLED_PX.max((to - from).abs() * 1e-3))
}

/// How the pan settles on a group's edge, on [`crate::motion::Snap`]: its
/// spring, and how long a run from rest takes (zero when it lands).
#[derive(Clone, Copy, Debug)]
struct SnapTiming {
    timing: crate::motion::Timing,
    settles: Duration,
}

impl SnapTiming {
    fn of(motion: crate::tokens::EffectiveMotion, source: MotionSource) -> Self {
        let (spec, enter) = (crate::motion::Snap::SPEC, crate::motion::Direction::Enter);
        let timing = spec.timing(enter, motion, source);
        let settles = spec.settle_time(enter, motion, source);
        Self { timing, settles }
    }
}

/// Press displacement (px) before a tab-bar-background press becomes a
/// whole-group reorder drag instead of a focus click.
const GROUP_DRAG_THRESHOLD: f64 = crate::tokens::Metrics::BASE.icon_label_gap as f64;

/// How long a finger rests on a tab bar before moving for the drag to carry
/// the window instead of panning the strip. A quick swipe pans; touch, hold
/// briefly, then drag moves the window, as on a phone's home screen. Shorter
/// than the long press that opens a context menu.
const TOUCH_GRAB_HOLD: crate::time::Duration = crate::time::Duration::from_millis(300);

/// One-finger swipe that is following the strip. Distinct from
/// [`GroupDrag`]: a horizontal swipe pans; a grab that goes mostly
/// vertical, or a pointer drag, still reorders.
#[derive(Clone, Copy, Debug)]
struct TouchPan {
    id: u64,
    start: Point,
    engaged: bool,
}

/// A left press on empty canvas, until its release. `engaged` latches
/// once the pointer has moved past [`GROUP_DRAG_THRESHOLD`] sideways.
#[derive(Clone, Copy)]
struct PointerPan {
    start: Point,
    engaged: bool,
}

/// In-flight whole-group reorder drag, set on a tab-bar-background press
/// or touch (see [`WindowGroup::bar_grab_hit`]) and cleared on release.
/// `moved` latches once the pointer passes [`GROUP_DRAG_THRESHOLD`].
#[derive(Clone, Copy, Debug)]
struct GroupDrag {
    src_idx: usize,
    group_id: u64,
    start: Point,
    lifted_at: Instant,
    moved: bool,
    /// The finger carrying the drag; `None` for the pointer.
    touch: Option<u64>,
    /// Someone else holds a window of the group
    /// ([`WindowGroup::is_held`]): it follows the drag a little and springs
    /// back, and never moves.
    held: bool,
}

// A held group springs back once let go as a let-go surface settles
// (`motion::Snap`); how far it follows a drag before it stops is a section
// gap ([`crate::tokens::Spacing::BASE`]).

/// `delta` as a held window follows it: a rubber band that gives a little
/// and no more, less so up and down.
fn resisted(delta: vello::kurbo::Vec2) -> vello::kurbo::Vec2 {
    let pull = |d: f64| {
        f64::from(crate::tokens::rubber_band(
            crate::num::f32_of_f64(d.abs()),
            crate::tokens::Spacing::BASE.s4,
        ))
        .copysign(d)
    };
    vello::kurbo::Vec2::new(pull(delta.x), pull(delta.y) / 2.0)
}

/// The one spatial answer to a tab drag within a strip — the desktop
/// model's drop target, so what the row previews is exactly what the
/// model applies.
///
/// Both hover feedback and release consume this value. A target in an
/// existing group names the exact tab-list insertion point; a group gap names
/// the slot where a new window group will be made.
pub use tilo_desktop_model::DropTarget as TabDropTarget;

/// Payload published when a tab drag ends outside the row's own band; the
/// shell hit-tests the release point against other strips and delivers it
/// via [`GroupRow::accept_cross_strip_drop`] or returns it to source.
/// The source group is already mutated (detached) by the time this is
/// published, matching what [`WindowGroup::detach_tab`] returns.
pub struct CrossStripDetach {
    pub tab_id: u64,
    pub source_group_id: u64,
    pub source_group_index: usize,
    pub source_tab_index: usize,
    pub desc: TabDescriptor,
    pub host: TileHost,
    pub release_point: Point,
}

/// Persistable shape of one live window group after a same-strip tab move.
/// Tile content remains owned by the shell's `WorkspaceTree`; the UI only
/// reports identities and presentation state, avoiding a second copy of app
/// data at the widget boundary.
pub use tilo_desktop_model::GroupShape as GroupTopology;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag (structure_dirty, scroll_dirty, hidden, shift_armed) is an independent, \
              per-frame invalidation signal read and cleared on its own schedule, not a \
              combinatorial config API — folding them into an enum or bitflags would not make \
              any call site clearer"
)]
pub struct GroupRow {
    external_navigation: bool,
    close_reports: Vec<crate::application::ComponentCloseReport>,
    pub groups: Vec<WindowGroup>,
    /// One resize handle per gap: `max(0, groups.len() - 1)`.
    handles: Vec<ResizeHandle>,
    handle_node_ids: Vec<NodeId>,
    rect: Rect,
    node_ids: Vec<NodeId>,
    /// The group ids `node_ids` was built for, in order: a splice or
    /// reorder since the last layout makes them disagree with `groups`.
    laid_group_ids: Vec<u64>,
    /// Last cursor pos; lets a release that landed on a sibling's bar
    /// still resolve to a release point for the detach sweep.
    last_cursor: Point,
    /// Set on structural mutation; App's tick handler polls & rebuilds taffy.
    structure_dirty: bool,
    /// The exact target currently previewed for a child tab drag. Release
    /// consumes this value rather than hit-testing a second time.
    tab_drop_target: Option<TabDropTarget>,
    /// Target previewed in this row for a tab whose gesture belongs to a
    /// different strip. Kept separate so the shell can clear cross-strip
    /// routing without disturbing compositor-owned content drag feedback.
    cross_strip_drop_target: Option<TabDropTarget>,
    /// Content the compositor carries is being dragged over this row
    /// ([`Self::set_external_content_drag_hover`]).
    external_drag_over: bool,
    /// Edge pan and spring-loaded tabs for a drag in flight.
    drag_reach: DragReach,
    /// Pan state of the whole row, or of the lane being worked on while
    /// [`Self::lanes`] is populated (see [`Pan`]).
    pan: Pan,
    /// The step a presenter that moves existing pixels needs the drawn pan
    /// on, in layout pixels: one device pixel (`1 / scale`), or one layout
    /// pixel for a CPU presenter. `None` draws the exact pan.
    scroll_quantum: Option<f64>,
    /// The row is drawn into a scene of its own ([`Self::draw_apart`]):
    /// its parent's draw only records the transform it would have drawn
    /// with.
    drawn_apart: bool,
    /// The transform the parent's draw passed while [`Self::drawn_apart`].
    apart_transform: Option<Affine>,
    /// One per display the strip is partitioned across, left to right.
    /// Empty when the strip is one continuous row (a single display).
    lanes: Vec<Lane>,
    /// Which lane each group sits in, by group id. Pushed by the shell from
    /// its per-device partition; a group the row itself creates is entered
    /// here with the lane it was dropped into.
    group_lanes: HashMap<u64, usize>,
    /// The lane keyboard focus and direct strip pans act on.
    focus_lane: usize,
    /// Lane a tab release resolved to, consumed by the split it triggers.
    pending_split_lane: Option<usize>,
    /// Set by the post-dispatch detach sweep when a release lands outside
    /// this row's band; drained via [`Self::take_pending_cross_strip_detach`].
    pending_cross_strip_detach: Option<CrossStripDetach>,
    /// Set whenever `scroll_x` changes since the last `draw`; without it
    /// the parent's cached scene would keep painting stale positions on a
    /// wheel-pan.
    scroll_dirty: bool,
    /// Canvas-x gutter before the first group (the strip-edge gutter, so
    /// it pans with the strip). `apply_layout` offsets every
    /// child's screen-space origin by `rect.x0 + leading_inset - scroll_x`.
    leading_inset: f64,
    /// Transient fullscreen override (not persisted), set by the shell
    /// each frame from compositor state. Collapses the row to this one
    /// group; if it belongs to another strip, [`Self::hidden`] is set
    /// instead.
    fullscreen_group: Option<u64>,
    /// Transient: the promoted group leaves its lane's leading gutter clear,
    /// as a user maximize does; a client fullscreen covers it.
    promotion_keeps_gutter: bool,
    /// A maximize can cover the gutter without hiding its Restore controls.
    promotion_keeps_controls: bool,
    /// Transient (not persisted): true on every strip that doesn't own
    /// the current fullscreen group; collapses the row via `Display::None`.
    hidden: bool,
    /// How far the [`super::StripDeck`] holding this row has it moved down
    /// (up, when negative) from where it rests: the row is laid out where it
    /// rests and moved by this, so a strip sliding in or out costs no layout.
    deck_shift: f64,
    /// Transient: the display another strip's fullscreen covers. This
    /// strip's lane there collapses; its lanes on other displays stay. A
    /// row with no lanes spans every display and collapses whole.
    covered_output: Option<u32>,
    /// In-flight whole-group reorder drag (distinct from a tab-detach
    /// drag inside a child `TabBar`, and from an edge resize).
    group_drag: Option<GroupDrag>,
    /// The window edge under the pointer, which a press would resize, and
    /// whether Shift was held when it was found (see [`super::edge_resize`]).
    edge_hover: Option<(EdgeGrab, bool)>,
    /// An edge resize in flight.
    edge_drag: Option<RowEdgeDrag>,
    /// A held group a drag let go of, springing back as a let-go surface
    /// settles ([`crate::motion::Snap`]): its id and how far it is pulled.
    group_resist_back: Option<(u64, crate::motion::MotionValue<vello::kurbo::Vec2>)>,
    /// One-finger swipe on the tab bar that is panning the strip.
    touch_pan: Option<TouchPan>,
    /// A mouse press on the canvas between and around windows: dragging it
    /// sideways pans the strip, as a finger on a tab bar does.
    pointer_pan: Option<PointerPan>,
    /// Insertion gap index targeted by an in-progress reorder drag;
    /// `draw` paints the indicator here. Cleared on release.
    group_reorder_target: Option<usize>,
    /// Lane the targeted gap is in, on a partitioned strip: a group dragged
    /// onto another display lands in that display's lane.
    group_reorder_lane: Option<usize>,
    /// One-shot `(group_id, new_index)` on reorder commit; drained via
    /// [`Self::take_pending_group_reorder`] into the persisted `WorkspaceTree`.
    pending_group_reorder: Option<(u64, usize)>,
    /// Set after a tab crosses a group boundary within this row. The shell
    /// drains a compact identity-only snapshot and applies it to the
    /// persisted workspace tree.
    pending_topology_change: bool,
    /// Displays this strip spans; each group resolves its [`WindowWidth`]
    /// fraction against the span it's laid out on, not the combined
    /// desktop — otherwise a full-width window could span two monitors.
    /// Empty in a bare `ui` host, see [`Self::usable_at`].
    output_spans: Vec<OutputSpan>,
    /// Each group's on-screen x from the last layout, after the pan,
    /// keyed by id — a move's "where it used to be".
    last_laid_x: HashMap<u64, f64>,
    /// Set when an insert, close or reorder moves groups along; the next
    /// layout turns prior positions into moves and clears this.
    shift_armed: bool,
    /// Each moved group's drawn offset from its laid-out x (px, negative =
    /// left of home), springing to 0 on [`crate::motion::WindowMotion::MOVE`].
    /// All of them start in the layout that moved them.
    moves: Vec<(u64, crate::motion::MotionValue<f64>)>,
    /// Groups another surface draws for now, as one shared element: the
    /// overview flying a window's picture between its strip and its place
    /// in the overview ([`Self::set_shown_elsewhere`]). Not painted here
    /// meanwhile, so the window is never on screen twice.
    shown_elsewhere: Vec<u64>,
    /// Capture-harness pose for every move: how far into its run it is
    /// drawn ([`Self::hold_shift_at`]); always `None` in a running shell.
    shift_hold: Option<Duration>,
    /// `theme.reduce_motion` as last laid out or drawn: the strip's snaps
    /// and make-space slides jump to where they were going.
    reduce_motion: bool,
    /// What started the transition being laid out; see
    /// [`Self::set_motion_source`].
    motion_source: MotionSource,
    /// `theme.tokens.motion` as last laid out or drawn.
    motion: crate::tokens::Motion,
    /// Openness of sibling groups while one is maximized or fullscreen.
    /// `1` is the tiled strip; `0` is hidden. A mid-flight Super+M
    /// retargets from the current alpha so the fade is interruptible.
    sibling_fade: crate::motion::Presence,
    /// Capture-harness pose for [`Self::hold_sibling_fade_at`]. Skips
    /// `sync_sibling_fade` so a slow offscreen paint cannot retire the
    /// 130 ms curve before the next snapshot.
    sibling_fade_hold: Option<Duration>,
    /// Group that stayed visible through the last maximize/fullscreen.
    /// Restore uses this so siblings fade in on the tiled draw path,
    /// not only while `maximized_idx` is still set.
    fade_hero: Option<u64>,
    /// The last draw caught [`Self::sibling_fade`] in flight. The row then
    /// stays dirty one frame past the fade's end, so a cached scene or a
    /// slow frame that last drew it partway does not keep that partway
    /// pose on screen after the fade has stopped asking for frames.
    sibling_fade_drawn_moving: bool,
    /// Groups the last draw skipped as outside the row. They keep their
    /// dirty state, so cached scenes rebuild once they are painted again.
    culled_groups: Vec<u64>,
    /// When the latest frame synced the groups, and which of them its draw
    /// then left unpainted: a deadline of theirs that was due by then has
    /// had its frame, and what is left of it is for their paint.
    synced_at: Option<Instant>,
    unpainted: Vec<u64>,
    unpainted_since: Option<Instant>,
    /// While the lane being drawn is in canvas coordinates (a presenter
    /// keeps the strip): what of the canvas the screen shows this draw.
    /// `None` in screen space.
    apart_window: Option<ApartWindow>,
    /// The canvas stretch each lane's last draw in canvas coordinates
    /// showed, by lane index: a lane showing another one now is panning.
    apart_spans: Vec<(usize, (f64, f64))>,
    /// While the row is drawn in canvas coordinates: each group's last
    /// drawing, which a group wholly off screen replays instead of drawing
    /// again while the strip pans ([`Self::draw_group_apart`]). Emptied once
    /// the row is drawn in screen space again.
    apart_drawings: Vec<ApartDrawing>,
    /// While frames only move the strip drawn in screen space
    /// ([`crate::retained_present::pan_replay`]): the last drawing of each
    /// window at rest, which the next such frame replays where the window
    /// is now ([`Self::draw_group_replaying`]). Any other frame empties it.
    pan_drawings: Vec<ApartDrawing>,
    /// Windows at rest the current draw drew or replayed: their drawings
    /// are kept, every other one is dropped.
    pan_kept: Vec<u64>,
    /// Windows the last draw replayed. Like a culled window, each keeps its
    /// state for its next real draw; unlike one, it was shown.
    replayed_groups: Vec<u64>,
    /// The theme [`Self::pan_drawings`] were drawn in.
    pan_theme: Option<Theme>,
    /// The frame ([`crate::retained_present::pan_frame`]) this row was last
    /// drawn in.
    pan_drawn_frame: Option<u64>,
    /// Windows whose drawing cannot be kept (it carries a blurred material
    /// or pixels living outside it) while frames only move: drawn plainly.
    pan_unkept: Vec<u64>,
    /// Windows that put anything on the top layer in the last frame
    /// ([`Widget::draw_overlay`]): an open menu or popover may hang on
    /// where the window's widgets last drew, so such a window is drawn,
    /// never replayed.
    overlay_groups: Vec<u64>,
    /// Per lane: the canvas shift of the pan its strips last read the
    /// desktop at, whether the lane's last draw was in screen space, and
    /// the shift it was drawn at.
    strip_desktop_shift: Vec<(usize, f64, bool, f64)>,
    /// The desktop's coordinates in the scene the current lane is drawn
    /// into ([`WindowGroup::draw_docked_strip`]).
    drawing_desktop: Affine,
}

/// What of a group at `rect` the draw viewport shows across, measured from
/// the group's left edge and limited to the group, which is what its
/// widgets cull against the viewport; `None` with no viewport (nothing is
/// culled).
/// Whether a drawing that culled nothing in `seen` of its group (see
/// [`seen_across`]) shows all the screen `shows` of it now.
fn covers(seen: Option<(f64, f64)>, shows: Option<(f64, f64)>) -> bool {
    match (seen, shows) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(seen), Some(shows)) => shows.0 >= shows.1 || (seen.0 <= shows.0 && shows.1 <= seen.1),
    }
}

fn seen_across(presence: &PresenceCtx, rect: Rect) -> Option<(f64, f64)> {
    presence.viewport.get().map(|viewport| {
        (
            (viewport.x0 - rect.x0).max(0.0),
            (viewport.x1 - rect.x0).min(rect.width()),
        )
    })
}

/// A group's drawing just made ([`GroupRow::record_group`]), not yet
/// appended to the frame.
struct GroupDrawing(ApartDrawing);

impl GroupDrawing {
    /// Append the drawing to `scene` as drawn, carrying the floating
    /// surfaces drawn into it; returns how many it carried.
    fn append_to(&self, scene: &mut Scene) -> usize {
        let drawing = &self.0;
        crate::scene_parts::append_with(
            scene,
            drawing.parts.as_ref(),
            Some(Affine::IDENTITY),
            |scene| crate::backdrop::append_nested(scene, &drawing.scene, Affine::IDENTITY),
        )
    }

    /// The drawing, when it may be kept for a replay: one that carried a
    /// blurred material (`carried` surfaces) is drawn every frame, since
    /// replayed, the material would lose its backdrop. So is one that draws
    /// pixels living outside it (a client's GPU buffer): those are valid
    /// only for the frame that drew them, and a replay could read a buffer
    /// its client already got back.
    fn keep(self, carried: usize) -> Option<ApartDrawing> {
        (carried == 0 && !draws_external_pixels(&self.0.scene)).then_some(self.0)
    }

    /// Keep the drawing as the window's live picture where one is wanted
    /// ([`crate::window_pictures`]). A blurred material it carried stays
    /// out of the picture (its backdrop is the screen's, not the
    /// picture's), which at a miniature's size leaves the window readable;
    /// pixels living outside the scene are valid for one frame only, so a
    /// drawing with them is never shown small.
    fn offer_picture(&self, _carried: usize) {
        if !crate::window_pictures::wanted(self.0.id) {
            return;
        }
        if draws_external_pixels(&self.0.scene) {
            crate::window_pictures::forget(self.0.id);
        } else {
            self.0.offer_picture();
        }
    }
}

/// What of a strip drawn in canvas coordinates the screen shows this draw.
#[derive(Clone, Copy)]
struct ApartWindow {
    /// `(x0, x1)` in the canvas's layout coordinates.
    span: (f64, f64),
    /// The screen shows another stretch than the lane's last draw did: the
    /// strip is panning, and groups off screen replay their drawings.
    panning: bool,
    /// `(y0, y1)` of the lane's clip in the row's coordinates: nothing the
    /// lane draws reaches past them.
    rows: (f64, f64),
}

/// Whether `scene` draws an image whose pixels are not in it: a client's
/// GPU buffer or another external frame carries no CPU pixels, every other
/// image does.
fn draws_external_pixels(scene: &Scene) -> bool {
    scene.encoding().resources.patches.iter().any(|patch| {
        matches!(patch, vello_encoding::Patch::Image { image, .. } if image.data.is_empty())
    })
}

impl ApartDrawing {
    /// Keep this drawing, which stands on its own, as the window's live
    /// picture where one is wanted ([`crate::window_pictures`]). One that
    /// showed a secret is never shown small, and one that culled part of
    /// the window leaves the last whole picture in place.
    fn offer_picture(&self) {
        if !crate::window_pictures::wanted(self.id) {
            return;
        }
        if self.shows_secret {
            crate::window_pictures::forget(self.id);
            return;
        }
        let whole = self
            .seen
            .is_none_or(|(x0, x1)| x0 <= 0.5 && x1 >= self.rect.width() - 0.5);
        if whole {
            crate::window_pictures::keep(
                self.id,
                &self.scene,
                self.transform.transform_rect_bbox(self.rect),
            );
        }
    }
}

/// A group's drawing in canvas coordinates, kept while a presenter pans the
/// strip it is in.
struct ApartDrawing {
    id: u64,
    /// The group's canvas rect and the transform it was drawn with: the
    /// drawing is reused only while both hold.
    rect: Rect,
    transform: Affine,
    scene: Scene,
    /// The damage parts the drawing marked ([`crate::scene_parts`]), which
    /// a replay marks again.
    parts: Option<crate::scene_parts::SectionParts>,
    /// The drawing put a secret on screen; a replay notes it again.
    shows_secret: bool,
    /// What of the group the drawing culled nothing in, across: the draw
    /// viewport's `(x0, x1)` measured from the group's left edge. `None`
    /// when nothing was culled. A replay must show no more of the group.
    seen: Option<(f64, f64)>,
    /// Nothing changed in the group since the drawing was made: it was drawn
    /// in the frame that last changed anything, or every frame since only
    /// moved laid-out content. A drawing replayed off screen through a frame
    /// that may have changed the group is not, and is never shown.
    current: bool,
}

/// One display's horizontal extent in the compositor's combined
/// coordinate space, plus the width window groups on it size themselves
/// against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpan {
    /// Left edge of the display in combined coordinates.
    pub x: f64,
    pub width: f64,
    /// Width available to window groups on this display: `width` minus
    /// whatever shell chrome it reserves (on the leftmost display, the
    /// strip-edge gutter). The denominator every [`WindowWidth`] fraction
    /// is taken of.
    pub usable: f64,
}

/// One display's share of a strip that is partitioned across displays.
///
/// Each display holds its own window groups: they are laid out, panned and
/// clipped inside that display's lane only, so a group never straddles a
/// seam and panning one display never reveals another display's windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripLane {
    /// The display this lane belongs to (the compositor's output id).
    pub output: u32,
    /// Where the lane's windows sit, in the row's coordinate space: this
    /// strip's band of the display. Every display stacks all of a
    /// workspace's strips in its own height, so the band lies on the
    /// display whatever part of the desktop the row itself is laid out
    /// over.
    pub area: Rect,
    /// Width window groups in this lane resolve their fractions against.
    pub usable: f64,
    /// Whether this lane begins with the strip-edge gutter.
    pub leading: bool,
}

/// Horizontal pan of one run of groups: the whole row, or one lane.
///
/// Canvas-x is the unscrolled coordinate groups are laid out in, starting at
/// the viewport's left edge; `scroll_x` is how far that canvas is panned
/// past it.
#[derive(Clone, Debug, Default)]
pub struct Pan {
    /// What this run is clipped to and framed against.
    viewport: Rect,
    /// Canvas-x gutter before the first group (the strip-edge gutter).
    inset: f64,
    /// Horizontal scroll offset (px): each child renders at `-scroll_x`.
    /// At rest it is bounded by [`Self::pan_max_scroll_x`]; an active
    /// direct gesture may hold a resisted transient value just outside
    /// either end.
    scroll_x: f64,
    /// Scroll offset at the start of the current direct-manipulation
    /// gesture. Gesture updates carry total travel, so this remains fixed
    /// until End.
    strip_pan_origin: Option<f64>,
    /// The current gesture's finger path. The frame tick resamples it
    /// ([`FingerPath::resampled`]); release reads its velocity
    /// ([`FingerPath::release_velocity`]).
    finger_path: super::finger_path::FingerPath,
    /// The pan travelling on its own: a throw coasting, an overpull
    /// settling, a reveal, a wheel's snap to a group's edge. It carries its
    /// velocity, so a retarget (the next wheel notch, a further reveal)
    /// continues the motion instead of restarting a curve from rest.
    motion: Option<crate::motion::MotionValue<f64>>,
    /// Total content width incl. gaps; cached so the wheel handler can
    /// clamp `scroll_x` without re-summing children.
    content_w: f64,
    /// Per-group canvas-x from the last layout — the snap targets.
    group_left_edges: Vec<f64>,
    /// Timestamp of the last wheel event; `tick_snap` waits
    /// `SNAP_IDLE_MS` of quiet here before starting the snap animation.
    last_wheel_at: Option<Instant>,
    /// Each group's `(id, left, right)` canvas-x extent as last laid out.
    /// Measured off taffy rather than re-derived from stored widths: a
    /// group taffy shrank to fit is that width on screen regardless of its
    /// stored fraction.
    laid_spans: Vec<(u64, f64, f64)>,
    /// Absolute external viewport navigation ([`GroupRow::pan_to`]) stays
    /// bounded through geometry changes.
    absolute: bool,
    /// A person's own pan last came to rest in the empty canvas past the
    /// last window ([`Self::pan_max_scroll_x`]'s allowance). Only then does
    /// the pan keep that allowance when the strip ends sooner; otherwise it
    /// stays within the windows.
    past_windows: bool,
}

impl Pan {
    /// Room a retained canvas keeps past each end of the content, so an
    /// overscroll pans within it too: the rubber band never reaches
    /// [`STRIP_OVERSCROLL_REACH_FRAC`] of the viewport.
    fn canvas_pad(&self) -> f64 {
        (self.viewport.width().max(0.0) * STRIP_OVERSCROLL_REACH_FRAC).ceil() + 1.0
    }

    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The furthest a focus or host navigation pans: the last group in
    /// view with the strip's gutter after it, as the leading inset keeps
    /// one before the first.
    fn max_scroll_x(&self) -> f64 {
        self.max_scroll_x_of(self.content_w)
    }

    /// [`Self::max_scroll_x`] once the windows are `content_w` wide.
    fn max_scroll_x_of(&self, content_w: f64) -> f64 {
        let gutter = if content_w > self.inset {
            self.inset
        } else {
            0.0
        };
        (content_w + gutter - self.viewport.width()).max(0.0)
    }

    /// The leading inset again after the last group, once there is one.
    fn trailing_gutter(&self) -> f64 {
        if self.content_w > self.inset {
            self.inset
        } else {
            0.0
        }
    }

    fn pan_max_scroll_x(&self) -> f64 {
        let viewport = self.viewport.width().max(1.0);
        // A lone window that fits, its gutters included, stays at home: it
        // takes the strip, so there is no further window to glimpse past.
        if self.laid_spans.len() <= 1
            && self.content_w + self.trailing_gutter() <= viewport + SNAP_SETTLED_PX
        {
            return 0.0;
        }
        let trailing = (viewport * TRAILING_REVEAL_FRAC).max(self.trailing_gutter());
        (self.content_w + trailing - viewport).max(0.0)
    }

    fn begin_strip_pan(&mut self) {
        self.strip_pan_origin = Some(self.scroll_x);
        self.finger_path.clear();
        self.stop_motion();
        self.last_wheel_at = None;
    }

    /// Where a direct pan asking for `requested` is shown: 1:1 within the
    /// legal canvas, against the shared rubber band past either end.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the shared rubber-band token accepts f32 screen-pixel distances; viewport-sized \
                  f64 values are safely representable at sub-pixel precision and the result is \
                  immediately widened back into the row's f64 geometry"
    )]
    fn rubber_banded(&self, requested: f64) -> f64 {
        let max = self.pan_max_scroll_x();
        let reach = self.viewport.width().max(1.0) * STRIP_OVERSCROLL_REACH_FRAC;
        if requested < 0.0 {
            -f64::from(crate::tokens::rubber_band(
                (-requested) as f32,
                reach as f32,
            ))
        } else if requested > max {
            max + f64::from(crate::tokens::rubber_band(
                (requested - max) as f32,
                reach as f32,
            ))
        } else {
            requested
        }
    }

    /// The gesture's total travel where the fingers were at `now`
    /// ([`FingerPath::resampled`]).
    fn resampled_travel(&self, now: Instant) -> Option<f64> {
        self.finger_path.resampled(now)
    }

    /// Velocity (travel px/s) of the fingers as they lift at `now`.
    fn release_velocity(&self, now: Instant) -> f64 {
        self.finger_path.release_velocity(now)
    }

    /// Whether a frame at `now` would show a direct pan somewhere else
    /// than the strip is: fingers down and the resampled path not yet where
    /// the last frame put it. Fingers resting on the pad ask for nothing.
    fn direct_pan_moving(&self, now: Instant) -> bool {
        let Some(origin) = self.strip_pan_origin else {
            return false;
        };
        self.resampled_travel(now).is_some_and(|travel| {
            (self.rubber_banded(origin + travel) - self.scroll_x).abs() > f64::EPSILON
        })
    }

    /// Record the fingers' total `travel` reported `at`. The strip shows it
    /// at once; the next frame tick then places it where the fingers were
    /// at that frame ([`Self::resampled_travel`]).
    fn update_strip_pan(&mut self, travel: f64, at: Instant) -> bool {
        let origin = *self.strip_pan_origin.get_or_insert(self.scroll_x);
        self.finger_path.push(at, travel);
        let next = self.rubber_banded(origin + travel);
        self.stop_motion();
        self.last_wheel_at = None;
        if (next - self.scroll_x).abs() <= f64::EPSILON {
            return false;
        }
        self.scroll_x = next;
        true
    }

    /// The fingers lifted at `now`. With `throw`, a strip still moving
    /// keeps the fingers' velocity and coasts to a stop ([`Snap::COAST`]);
    /// one released in elastic overpull, or thrown past an end, settles on
    /// that end on `settle`. A strip released at rest within the canvas
    /// stays exactly where it is.
    ///
    /// [`Snap::COAST`]: crate::motion::Snap::COAST
    fn end_strip_pan(
        &mut self,
        now: Instant,
        throw: bool,
        motion: crate::tokens::EffectiveMotion,
        settle: crate::motion::Timing,
    ) -> bool {
        let from = match (self.strip_pan_origin, self.resampled_travel(now)) {
            (Some(origin), Some(travel)) => self.rubber_banded(origin + travel),
            _ => self.scroll_x,
        };
        let velocity = if throw {
            self.release_velocity(now)
        } else {
            0.0
        };
        self.strip_pan_origin = None;
        self.finger_path.clear();
        self.last_wheel_at = None;
        self.scroll_x = from;
        let max = self.pan_max_scroll_x();
        let coast = crate::motion::Snap::rest_for(from, velocity, motion);
        let to = coast.clamp(0.0, max);
        self.past_windows = to > self.max_scroll_x() + 0.5;
        if velocity.abs() < FLING_MIN_VELOCITY && (to - from).abs() <= SNAP_SETTLED_PX {
            self.scroll_x = to;
            return false;
        }
        // Within the canvas the throw coasts on, its velocity decaying
        // exponentially over the gesture token's coast. Past an end it lands
        // on that end at the settle rate, a short give rather than a long
        // crawl.
        let timing = if (coast - to).abs() <= f64::EPSILON {
            crate::motion::Snap::COAST.timing(
                crate::motion::Direction::Enter,
                motion,
                MotionSource::Gesture,
            )
        } else {
            settle
        };
        if timing.is_instant() {
            self.scroll_x = to;
            return true;
        }
        let mut pan = strip_motion(from);
        pan.release(to, velocity, timing, now);
        self.motion = Some(pan);
        true
    }

    /// [`Self::reveal_target`] on a strip whose windows are not moving.
    #[cfg(test)]
    fn scroll_target_for(&self, gr: Rect) -> Option<f64> {
        self.reveal_target(gr, self.max_scroll_x())
    }

    /// The offset that brings a group laid out at `gr` fully inside the
    /// viewport, or `None` when it already is, on a strip whose windows end
    /// at offset `end` once their width springs settle. A reveal comes to
    /// rest within the windows, as the pan does once nothing moves it, so it
    /// never eases into the canvas past the last window only to be pulled
    /// back.
    fn reveal_target(&self, gr: Rect, end: f64) -> Option<f64> {
        // Degenerate rect → layout hasn't run for this group yet.
        if gr.width() <= 0.0 {
            return None;
        }
        // A revealed group stands at least the gap between two windows clear
        // of the viewport's edges, as far as the room allows: a window's
        // edge is drawn just outside its rect, so a reveal flush with the
        // screen edge cut its border and rounded corners off. The strip's
        // first and last groups keep their whole gutter, below.
        let slack = (self.viewport.width() - gr.width()).max(0.0);
        let margin = HANDLE_SLOT_PX.min(slack / 2.0);
        let left_bound = self.viewport.x0 + margin;
        let right_bound = self.viewport.x1 - margin;
        let mut sx = self.scroll_x;
        // An oversized group cannot fit; reveal its start consistently.
        // Otherwise use the nearest edge. Checking the original left rect
        // after a right-edge correction would right-align an oversized card.
        if gr.width() > right_bound - left_bound {
            sx += gr.x0 - left_bound;
        } else if gr.x1 > right_bound {
            sx += gr.x1 - right_bound;
        } else if gr.x0 < left_bound {
            sx -= left_bound - gr.x0;
        }
        let upper = if self.absolute {
            self.max_scroll_x()
        } else {
            end
        };
        // A reveal that would stop within the gutter of either end of the
        // strip goes to that end, when what it reveals still fits there:
        // flush with the screen edge, the first or last window's border and
        // corners are cut off by it.
        let at = |to: f64| gr + vello::kurbo::Vec2::new(self.scroll_x - to, 0.0);
        if self.inset > 0.0 {
            if sx > 0.0 && sx <= self.inset + 0.5 && at(0.0).x1 <= right_bound + 0.5 {
                sx = 0.0;
            } else if sx < end && sx >= end - self.inset - 0.5 && at(end).x0 >= left_bound - 0.5 {
                sx = end;
            }
        }
        sx = sx.clamp(0.0, upper);
        ((sx - self.scroll_x).abs() > f64::EPSILON).then_some(sx)
    }

    fn jump_to(&mut self, sx: f64) {
        self.scroll_x = sx;
        self.stop_motion();
        self.last_wheel_at = None;
    }

    /// Stop the pan's own motion where the pan is: it asks for no more
    /// frames.
    fn stop_motion(&mut self) {
        if let Some(mut motion) = self.motion.take() {
            motion.snap(motion.target());
        }
    }

    /// Whether the pan is travelling on its own (a snap or a glide) and
    /// its frames have not yet shown it at rest.
    fn animating(&self) -> bool {
        self.motion.is_some()
    }

    /// Whether the pan's own motion still moves at `now`.
    fn motion_moving(&self, now: Instant) -> bool {
        self.motion.is_some_and(|motion| motion.is_moving(now))
    }

    /// Glide to `sx` on `timing`, from where the pan is at `now` and at the
    /// speed it is moving, so a stream of retargets (one per wheel notch)
    /// reads as one continuous motion rather than a curve restarting from
    /// rest at every step. Re-asking for the target already in flight
    /// changes nothing, and a sub-pixel move is not worth a frame.
    fn glide_to(&mut self, sx: f64, now: Instant, timing: crate::motion::Timing) {
        if timing.is_instant() {
            self.jump_to(sx);
            return;
        }
        match self.motion.as_mut() {
            Some(motion) if (motion.target() - sx).abs() <= SNAP_SETTLED_PX => return,
            Some(motion) => motion.set(sx, timing, now),
            None if (sx - self.scroll_x).abs() <= SNAP_SETTLED_PX => {
                self.jump_to(sx);
                return;
            }
            None => {
                let mut motion = strip_motion(self.scroll_x);
                motion.set(sx, timing, now);
                self.motion = Some(motion);
            }
        }
        self.last_wheel_at = None;
    }

    fn groups_cut_by_edges(&self) -> Vec<u64> {
        let row_w = self.viewport.width().max(1.0);
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

    /// True while a snap is in flight, or the user just wheeled (within
    /// the idle threshold and the `snap` that follows it); keeps the
    /// shell's redraw chain pumping.
    fn snap_in_flight(&self, snap: SnapTiming) -> bool {
        let now = crate::motion::frame_now();
        if self.motion_moving(now) || self.direct_pan_moving(now) {
            return true;
        }
        // Pending snap: idle gate hasn't fired yet. Keep redraws pumping
        // so `tick_snap` runs the moment it clears.
        if let Some(t) = self.last_wheel_at
            && now.saturating_duration_since(t)
                < Duration::from_millis(u64::from(SNAP_IDLE_MS)) + snap.settles
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
    /// canvas-x = 0 (the gutter-visible home) when the run has a gutter —
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
        let home_targets: &[f64] = if self.inset > 0.0 { &[0.0] } else { &[] };
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

    /// Advance the snap state to `now`; a wheel's snap settles on `snap`,
    /// and under reduced motion a snap lands on the tick it starts. A
    /// capture's `hold` poses the pan's motion that far into its run.
    /// Returns whether `scroll_x` moved.
    fn tick_snap_at(&mut self, now: Instant, snap: SnapTiming, hold: Option<Duration>) -> bool {
        let before = self.scroll_x;
        // Fingers down: show where they were at this frame.
        if let Some(origin) = self.strip_pan_origin {
            if let Some(travel) = self.resampled_travel(now) {
                self.scroll_x = self.rubber_banded(origin + travel);
            }
            return (self.scroll_x - before).abs() > f64::EPSILON;
        }
        // Starting a fresh snap here, before the motion advances below, lets
        // this same call both start it and show its first frame.
        if self.motion.is_none()
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
                        // The snap starts the moment the idle gate was met,
                        // not `now`, so the tick that sees the idle already
                        // shows how far it has come since. Falls back to
                        // `last_wheel` (skipping the idle delay) in the
                        // astronomically unlikely case this addition would
                        // overflow `Instant`.
                        let started = last_wheel
                            .checked_add(Duration::from_millis(u64::from(SNAP_IDLE_MS)))
                            .unwrap_or(last_wheel);
                        let mut motion = snap_motion(self.scroll_x, target);
                        motion.set(target, snap.timing, started);
                        self.motion = Some(motion);
                    }
                } else {
                    self.last_wheel_at = None;
                }
            }
        }
        // Under reduced motion whatever was set moving (before the row
        // learned the person's setting, say) lands on this tick.
        let reduce_motion = snap.settles.is_zero();
        if let Some(motion) = self.motion {
            if let Some(hold) = hold {
                self.scroll_x = motion.sampled_after(hold).0;
            } else if !reduce_motion && motion.is_moving(now) {
                self.scroll_x = motion.get(now);
            } else {
                self.scroll_x = motion.target();
                self.stop_motion();
                self.last_wheel_at = None;
            }
        }
        (self.scroll_x - before).abs() > f64::EPSILON
    }

    /// Ease on the snap curve from `from` to where the pan now is, on
    /// `timing`, rather than cutting there.
    fn slide_scroll_from(&mut self, from: f64, now: Instant, timing: SnapTiming) {
        if timing.timing.is_instant() {
            return;
        }
        let mut motion = snap_motion(from, self.scroll_x);
        motion.set(self.scroll_x, timing.timing, now);
        self.motion = Some(motion);
        self.scroll_x = from;
    }

    /// Slide to `sx` on `timing` from where the pan is drawn at `now`: a
    /// shortcut's reveal, which may only say where the target went and must
    /// be over within its budget, so it starts from rest. A press mid-slide
    /// retargets from the drawn position, so presses never queue.
    fn slide_to(&mut self, sx: f64, now: Instant, timing: SnapTiming) {
        self.last_wheel_at = None;
        let drawn = self.motion.map_or(self.scroll_x, |motion| motion.get(now));
        if timing.settles.is_zero() || (sx - drawn).abs() <= SNAP_SETTLED_PX {
            self.jump_to(sx);
            return;
        }
        self.stop_motion();
        let mut motion = snap_motion(drawn, sx);
        motion.set(sx, timing.timing, now);
        self.motion = Some(motion);
        self.scroll_x = drawn;
    }

    /// Note where the run's groups were laid out. The pan stays where it
    /// is: the strip never scrolls by itself to frame windows after a
    /// resize, an insert, a close or an output change. Only explicit
    /// navigation (switching to a window, Super+arrows) and bringing a
    /// newly opened or focused window into view move it, besides the clamp
    /// into its legal range.
    fn note_laid_spans(&mut self, spans: Vec<(u64, f64, f64)>) {
        self.laid_spans = spans;
    }

    /// Keep what the run shows where it is while a window before it closes:
    /// as the closing window narrows, or its slot goes, the canvas before
    /// the view shrinks, and the pan follows it by as much. So a window
    /// closed left of the view takes nothing on screen with it, where the
    /// windows shown used to slide over by its whole width, out of view.
    /// The anchor is the first window reaching into the view, as last laid
    /// out; `spans` is the new layout, `going` the windows closing.
    /// Reordering across the anchor, resizes and openings leave the pan
    /// alone.
    fn hold_view_across_close(&mut self, spans: &[(u64, f64, f64)], going: &[u64]) {
        let old = &self.laid_spans;
        let kept = |id: u64| spans.iter().any(|&(other, _, _)| other == id);
        let Some(at) = old
            .iter()
            .position(|&(id, _, right)| right > self.scroll_x && kept(id))
        else {
            return;
        };
        let Some(&(anchor, old_left, _)) = old.get(at) else {
            return;
        };
        let Some(&(_, new_left, _)) = spans.iter().find(|&&(id, _, _)| id == anchor) else {
            return;
        };
        let before_old: Vec<u64> = old.iter().take(at).map(|&(id, _, _)| id).collect();
        let before_new: Vec<u64> = spans
            .iter()
            .map(|&(id, _, _)| id)
            .take_while(|&id| id != anchor)
            .collect();
        let closing = before_old
            .iter()
            .any(|&id| going.contains(&id) || !kept(id));
        let order_kept = before_old
            .iter()
            .copied()
            .filter(|&id| kept(id))
            .eq(before_new.iter().copied());
        // Never past the strip's start: there the windows close the gap.
        let delta = (new_left - old_left).max(-self.scroll_x.max(0.0));
        if !closing || !order_kept || delta.abs() <= f64::EPSILON {
            return;
        }
        self.scroll_x += delta;
        if let Some(origin) = self.strip_pan_origin.as_mut() {
            *origin += delta;
        }
        if let Some(motion) = self.motion.as_mut() {
            motion.shift_by(delta);
        }
    }
}

/// A lane plus the pan it was last laid out with.
#[derive(Clone, Debug)]
struct Lane {
    spec: StripLane,
    pan: Pan,
    /// The area the lane's taffy node was built against, so layout can
    /// tell when the row moved under it and a rebuild is due.
    built_viewport: Rect,
    /// How far the strip deck has the row moved up or down from where it
    /// rests ([`GroupRow::set_deck_shift`]).
    shift: f64,
}

impl Lane {
    /// The lane's area where its row rests: never inverted.
    fn area(&self) -> Rect {
        let area = self.spec.area;
        Rect::new(area.x0, area.y0, area.x1.max(area.x0), area.y1.max(area.y0))
    }

    /// The lane's viewport on screen: its area, moved with the row by the
    /// strip deck. A pure vertical move needs no new layout.
    fn viewport(&self) -> Rect {
        self.area() + vello::kurbo::Vec2::new(0.0, self.shift)
    }
}

/// Steps per layout pixel of [`on_binary_grid`].
const BINARY_GRID: f64 = 1_048_576.0;

/// `value` rounded to a multiple of 2⁻²⁰ layout pixels. A device-pixel pan
/// step (two thirds of a layout pixel at 150 %) is no binary fraction, so
/// moving a strip to canvas coordinates and back would round differently
/// at every pan, and the canvas a presenter keeps would never be the same
/// twice. On this grid the sums are exact (a desktop is far narrower than
/// 2³² layout pixels), and the pan is off a device pixel by under 10⁻⁶ of
/// one.
fn on_binary_grid(value: f64) -> f64 {
    (value * BINARY_GRID).round() / BINARY_GRID
}

/// Whether a strip panned to `scroll` (whole pixels) can be drawn in canvas
/// coordinates: the retained canvas spans the content from no pan to the
/// furthest, plus `pad` past either end for the rubber band.
pub fn pan_within_canvas(scroll: f64, max_scroll: f64, pad: f64) -> bool {
    (-pad..=max_scroll.max(0.0) + pad).contains(&scroll)
}

impl GroupRow {
    /// Paint selected active app surfaces without the row's viewport clip.
    ///
    /// The ordinary desktop pass clips the continuous strip to the physical
    /// output. A capture of whole windows cannot reuse that pass: a window
    /// may be partly or wholly beyond the monitor while still having live
    /// layout and pixels. This calls each owning group directly, keeping its
    /// real app draw path and geometry and omitting only the output clip.
    /// Returns each painted window's id and the rect it was painted in.
    pub fn draw_capture_targets(
        &mut self,
        capture: &crate::widget::LiveCapture<'_>,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
    ) -> Vec<(String, Rect)> {
        let crate::widget::LiveCapture {
            target_ids,
            complete_semantic_content,
            content_only,
        } = *capture;
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
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
            if complete_semantic_content
                && let Some(rect) =
                    group.draw_complete_capture(scene, transform, text_renderer, theme, presence)
            {
                rects.push((id, rect));
                continue;
            }
            // `group.draw` includes the tab/window decoration, so its capture
            // crop must use that same complete rect.
            let rect = match group.active_host() {
                Some(host) if content_only => host.get_rect(),
                _ => group.get_rect(),
            };
            if rect.area() <= 0.0 {
                continue;
            }
            group.draw(scene, transform, text_renderer, theme, presence);
            rects.push((id, rect));
        }
        rects
    }

    pub fn new(groups: Vec<WindowGroup>) -> Self {
        Self {
            groups,
            close_reports: vec![],
            external_navigation: false,
            handles: Vec::new(),
            handle_node_ids: Vec::new(),
            rect: Rect::ZERO,
            node_ids: Vec::new(),
            laid_group_ids: Vec::new(),
            last_cursor: Point::ZERO,
            structure_dirty: false,
            tab_drop_target: None,
            cross_strip_drop_target: None,
            external_drag_over: false,
            drag_reach: DragReach::default(),
            pan: Pan::default(),
            scroll_quantum: None,
            drawn_apart: false,
            deck_shift: 0.0,
            apart_transform: None,
            lanes: Vec::new(),
            group_lanes: HashMap::new(),
            focus_lane: 0,
            pending_split_lane: None,
            pending_cross_strip_detach: None,
            scroll_dirty: false,
            leading_inset: 0.0,
            fullscreen_group: None,
            promotion_keeps_gutter: false,
            promotion_keeps_controls: false,
            hidden: false,
            covered_output: None,
            group_drag: None,
            edge_hover: None,
            edge_drag: None,
            group_resist_back: None,
            touch_pan: None,
            pointer_pan: None,
            group_reorder_target: None,
            group_reorder_lane: None,
            pending_group_reorder: None,
            pending_topology_change: false,
            output_spans: Vec::new(),
            last_laid_x: HashMap::new(),
            shift_armed: false,
            shown_elsewhere: Vec::new(),
            moves: Vec::new(),
            shift_hold: None,
            reduce_motion: false,
            motion_source: MotionSource::default(),
            motion: crate::tokens::Motion::default(),
            sibling_fade: crate::motion::Presence::opened(),
            sibling_fade_hold: None,
            fade_hero: None,
            sibling_fade_drawn_moving: false,
            culled_groups: Vec::new(),
            synced_at: None,
            unpainted: Vec::new(),
            unpainted_since: None,
            apart_window: None,
            apart_spans: Vec::new(),
            apart_drawings: Vec::new(),
            pan_drawings: Vec::new(),
            pan_kept: Vec::new(),
            replayed_groups: Vec::new(),
            pan_theme: None,
            pan_drawn_frame: None,
            overlay_groups: Vec::new(),
            strip_desktop_shift: Vec::new(),
            drawing_desktop: Affine::IDENTITY,
            pan_unkept: Vec::new(),
        }
    }

    /// Whether a group at `rect`, shifted by `dx`, paints anything inside
    /// the row, including its shadow sprites.
    ///
    /// A strip drawn in canvas coordinates draws the whole canvas: after
    /// [`Self::offset_lane`] the rects are canvas coordinates, so the window
    /// has to include every group a pan can reveal.
    fn paints_within_row(&self, rect: Rect, dx: f64) -> bool {
        let reach = rect.inflate(GROUP_SPRITE_REACH_PX, 0.0);
        let x1 = if crate::retained_present::unbake_scroll() {
            self.rect.x1 + self.max_scroll_x() + 2.0 * self.canvas_pad()
        } else {
            self.rect.x1
        };
        reach.x1 + dx > self.rect.x0 && reach.x0 + dx < x1
    }

    /// Shift one lane from screen coordinates to canvas coordinates, or back.
    fn offset_lane(&mut self, lane: usize, dx: f64) {
        let in_lane: Vec<bool> = (0..self.groups.len())
            .map(|index| self.lane_index_of(index) == lane)
            .collect();
        for (index, group) in self.groups.iter_mut().enumerate() {
            if in_lane.get(index).copied().unwrap_or(false) {
                group.offset_layout(dx);
            }
        }
        for (index, handle) in self.handles.iter_mut().enumerate() {
            let left = in_lane.get(index).copied().unwrap_or(false);
            let right = in_lane
                .get(index.saturating_add(1))
                .copied()
                .unwrap_or(false);
            if left && right {
                handle.offset_layout(dx);
            }
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
    pub fn set_external_navigation(&mut self, external: bool) {
        if self.external_navigation != external {
            self.external_navigation = external;
            self.structure_dirty = true;
        }
        for group in &mut self.groups {
            group.set_external_navigation(external);
        }
    }
    fn siblings_should_hide(&self) -> bool {
        self.fullscreen_group.is_some() || self.groups.iter().any(|g| g.maximized)
    }

    fn layout_hides_group(&self, i: usize) -> bool {
        let Some(group) = self.groups.get(i) else {
            return false;
        };
        if group.maximized || self.fullscreen_group == Some(group.id) {
            return false;
        }
        let lane = self.lane_index_of(i);
        self.groups.iter().enumerate().any(|(j, other)| {
            j != i
                && self.lane_index_of(j) == lane
                && (other.maximized || self.fullscreen_group == Some(other.id))
        })
    }

    fn sync_sibling_fade(&mut self, theme: &Theme) {
        if self.sibling_fade_hold.is_some() {
            return;
        }
        let hide = self.siblings_should_hide();
        if hide {
            self.fade_hero = self
                .fullscreen_idx()
                .or_else(|| self.groups.iter().position(|g| g.maximized))
                .and_then(|i| self.groups.get(i).map(|g| g.id));
        } else if self.sibling_fade.is_open()
            && !self.sibling_fade.is_moving(crate::motion::frame_now())
        {
            self.fade_hero = None;
        }
        let want_open = !hide;
        // The siblings cross-fade (`motion::CROSS_FADE`); a shortcut's
        // result shows at once. A repeated intent changes nothing.
        self.sibling_fade.show(
            want_open,
            crate::motion::WindowMotion::SIBLINGS,
            theme.motion(),
            self.motion_source,
            crate::motion::frame_now(),
        );
    }

    /// Whether the sibling fade is moving now.
    fn sibling_fade_in_flight(&self) -> bool {
        self.sibling_fade.is_moving(crate::motion::frame_now())
    }

    /// Whether the sibling fade wants a frame: it is moving, or the last
    /// draw showed it partway and the frame with it at rest is still owed.
    fn sibling_fade_needs_frame(&self) -> bool {
        self.sibling_fade_drawn_moving || self.sibling_fade_in_flight()
    }

    #[cfg(test)]
    pub(crate) fn sibling_fade_value(&self) -> f32 {
        crate::motion::unit_opacity(self.sibling_fade.progress(crate::motion::frame_now()))
    }

    #[cfg(test)]
    pub(crate) fn sibling_fade_is_animating(&self) -> bool {
        self.sibling_fade.is_moving(crate::motion::frame_now())
    }

    /// Rewind the live fade so a test can sample it mid-flight without
    /// sleeping. Rebuilds from the current hide intent against a start
    /// `elapsed` ago.
    #[cfg(test)]
    pub(crate) fn advance_sibling_fade(&mut self, elapsed: Duration) {
        let now = crate::time::now();
        let started = now.checked_sub(elapsed).unwrap_or(now);
        let hide = self.siblings_should_hide();
        self.sibling_fade = if hide {
            crate::motion::Presence::opened()
        } else {
            crate::motion::Presence::closed()
        };
        self.sibling_fade.show(
            !hide,
            crate::motion::WindowMotion::SIBLINGS,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
            started,
        );
    }

    /// Freeze the sibling fade at `elapsed` along `durations.fast`.
    /// Offscreen paints cost more wall time than the 130 ms curve, so a
    /// capture harness stepping frames only ever sees the endpoints —
    /// this poses production `draw` at a chosen point. Not gated on
    /// `debug_assertions`: `shots` ships in release.
    pub fn hold_sibling_fade_at(&mut self, elapsed: Duration) -> bool {
        if self.groups.len() < 2 {
            return false;
        }
        let hide = self.siblings_should_hide();
        let eased = crate::motion::WindowMotion::SIBLINGS.progress_after(
            crate::motion::Direction::of(!hide),
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
            elapsed,
        );
        let openness = if hide { 1.0 - eased } else { eased };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "openness is 0..=1 so permille is 0..=1000, which fits u16"
        )]
        let permille = (openness * 1000.0).round().clamp(0.0, 1000.0) as u16;
        self.sibling_fade = crate::motion::Presence::held_at(permille);
        self.sibling_fade_hold = Some(elapsed);
        true
    }

    pub fn clear_sibling_fade_hold(&mut self) {
        self.sibling_fade_hold = None;
    }

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

    /// Collapse this strip's lane on display `output`, which another
    /// strip's fullscreen covers, and keep its lanes on the other displays;
    /// `None` shows every lane again.
    pub fn set_covered_output(&mut self, output: Option<u32>) {
        if self.covered_output != output {
            self.covered_output = output;
            self.structure_dirty = true;
        }
    }

    /// The lane collapsed under another strip's fullscreen, if this row
    /// has one on the covered display.
    fn covered_lane(&self) -> Option<usize> {
        let output = self.covered_output?;
        self.lanes
            .iter()
            .position(|lane| lane.spec.output == output)
    }

    /// Visible screen-space edge grip, using ordinary widget dispatch geometry.
    /// The viewport clips grips, and promoted or hidden groups have none.
    pub fn group_resize_rects(&self, group_id: u64) -> Vec<Rect> {
        if self.hidden {
            return Vec::new();
        }
        let Some((index, group)) = self
            .groups
            .iter()
            .enumerate()
            .find(|(_, group)| group.id == group_id)
        else {
            return Vec::new();
        };
        if self.layout_hides_group(index) || self.fullscreen_group == Some(group_id) {
            return Vec::new();
        }
        group
            .resize_grab_rects()
            .into_iter()
            .map(|rect| rect.intersect(self.rect))
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
            .collect()
    }

    /// Whether `p` lies on a window edge a press would resize.
    pub fn resize_grab_hit_at(&self, p: Point) -> bool {
        self.edge_grab_at(p).is_some()
    }

    /// Whether the group promoted by [`Self::set_fullscreen`] stops at its
    /// lane's leading gutter (a maximize) or covers it (a client
    /// fullscreen).
    pub fn set_promotion_keeps_gutter(&mut self, keeps: bool) {
        if self.promotion_keeps_gutter != keeps {
            self.promotion_keeps_gutter = keeps;
            self.structure_dirty = true;
        }
    }

    /// Keep the maximized system controls even when the promoted group
    /// covers its gutter. Client fullscreen leaves this off.
    pub fn set_promotion_keeps_controls(&mut self, keeps: bool) {
        if self.promotion_keeps_controls != keeps {
            self.promotion_keeps_controls = keeps;
            self.structure_dirty = true;
        }
    }

    /// Reserve a canvas-x gutter before the first group so it pans with
    /// `scroll_x` too (the strip-edge gutter). On a strip partitioned
    /// across displays only the leading lane carries it.
    #[must_use]
    pub fn with_leading_inset(mut self, px: f64) -> Self {
        self.leading_inset = px;
        self.pan.inset = px;
        self
    }

    pub fn leading_inset(&self) -> f64 {
        self.leading_inset
    }

    pub fn set_leading_inset(&mut self, px: f64) {
        if (self.leading_inset - px).abs() > f64::EPSILON {
            self.leading_inset = px;
            if self.lanes.is_empty() {
                self.pan.inset = px;
            }
            for lane in &mut self.lanes {
                lane.pan.inset = if lane.spec.leading { px } else { 0.0 };
            }
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

    /// Partition this strip across displays: one lane per display, left to
    /// right, and the display each group belongs to (`assignment`, by group
    /// id → [`StripLane::output`]). Fewer than two lanes restores the one
    /// continuous row. A lane keeps its pan across calls as long as its
    /// display stays, so re-pushing the same partition every frame is free.
    ///
    /// The groups must already be ordered lane by lane (every group of the
    /// first lane, then every group of the second, …); the shell's
    /// reconciliation keeps them so.
    pub fn set_lanes(&mut self, lanes: &[StripLane], assignment: &HashMap<u64, u32>) {
        if lanes.len() < 2 {
            if !self.lanes.is_empty() {
                let keep = self
                    .lanes
                    .iter()
                    .position(|lane| lane.spec.leading)
                    .unwrap_or(0);
                let mut pan = std::mem::take(&mut self.lanes)
                    .into_iter()
                    .nth(keep)
                    .map(|lane| lane.pan)
                    .unwrap_or_default();
                pan.inset = self.leading_inset;
                self.pan = pan;
                self.group_lanes.clear();
                self.focus_lane = 0;
                self.structure_dirty = true;
            }
            return;
        }
        let specs_changed = self.lanes.len() != lanes.len()
            || self
                .lanes
                .iter()
                .zip(lanes.iter())
                .any(|(have, want)| have.spec != *want);
        if specs_changed {
            let mut previous = std::mem::take(&mut self.lanes);
            self.lanes = lanes
                .iter()
                .map(|spec| {
                    let pan = previous
                        .iter()
                        .position(|lane| lane.spec.output == spec.output)
                        .map(|i| previous.swap_remove(i).pan)
                        .unwrap_or_default();
                    Lane {
                        spec: *spec,
                        pan: Pan {
                            inset: if spec.leading {
                                self.leading_inset
                            } else {
                                0.0
                            },
                            ..pan
                        },
                        built_viewport: Rect::ZERO,
                        shift: self.deck_shift,
                    }
                })
                .collect();
            self.structure_dirty = true;
        }
        let group_lanes: HashMap<u64, usize> = assignment
            .iter()
            .filter_map(|(&group, &output)| {
                lanes
                    .iter()
                    .position(|lane| lane.output == output)
                    .map(|lane| (group, lane))
            })
            .collect();
        // Keep lanes the row assigned itself to groups the shell hasn't
        // placed yet (a tab dropped into a new group this frame).
        let mut merged = group_lanes;
        for (&group, &lane) in &self.group_lanes {
            if !assignment.contains_key(&group) && self.groups.iter().any(|g| g.id == group) {
                merged.entry(group).or_insert(lane);
            }
        }
        if merged != self.group_lanes {
            self.group_lanes = merged;
            self.structure_dirty = true;
        }
        self.focus_lane = self.focus_lane.min(self.lanes.len().saturating_sub(1));
    }

    /// The lanes this strip is partitioned into; empty for one continuous
    /// row.
    pub fn lanes(&self) -> Vec<StripLane> {
        self.lanes.iter().map(|lane| lane.spec).collect()
    }

    /// Whether the strip is partitioned across displays.
    pub fn is_partitioned(&self) -> bool {
        !self.lanes.is_empty()
    }

    /// The shown group a left press at the remembered cursor lands in;
    /// mouse buttons carry no position of their own.
    pub fn group_under_cursor(&self) -> Option<u64> {
        self.groups
            .iter()
            .enumerate()
            .find(|(i, group)| {
                !self.layout_hides_group(*i)
                    && !group.is_closing()
                    && group.get_rect().contains(self.last_cursor)
            })
            .map(|(_, group)| group.id)
    }

    /// Whether logical point `p` is over this strip: inside one of its
    /// lanes when it is partitioned, else within its row's band.
    pub fn covers(&self, p: Point) -> bool {
        if self.lanes.is_empty() {
            p.y >= self.rect.y0 && p.y <= self.rect.y1
        } else {
            self.lanes.iter().any(|lane| lane.viewport().contains(p))
        }
    }

    /// Lane of the group at vector index `idx`: its assigned lane, else the
    /// lane of the nearest assigned group before it, else the first lane.
    fn lane_index_of(&self, idx: usize) -> usize {
        if self.lanes.is_empty() {
            return 0;
        }
        let assigned = |i: usize| {
            self.groups
                .get(i)
                .and_then(|g| self.group_lanes.get(&g.id).copied())
        };
        (0..=idx)
            .rev()
            .find_map(assigned)
            .or_else(|| (idx..self.groups.len()).find_map(assigned))
            .unwrap_or(0)
            .min(self.lanes.len().saturating_sub(1))
    }

    /// The lane `group_id` is laid out in; `None` when the strip is one
    /// continuous row or the group isn't in it.
    pub fn lane_of_group(&self, group_id: u64) -> Option<usize> {
        if self.lanes.is_empty() {
            return None;
        }
        let idx = self.groups.iter().position(|g| g.id == group_id)?;
        Some(self.lane_index_of(idx))
    }

    /// The display `group_id` is laid out on, when the strip is partitioned.
    pub fn output_of_group(&self, group_id: u64) -> Option<u32> {
        let lane = self.lane_of_group(group_id)?;
        self.lanes.get(lane).map(|lane| lane.spec.output)
    }

    /// The display a group was explicitly placed on in this row: by the
    /// shell's partition, or by a tab dropped onto that display.
    pub fn assigned_output_of_group(&self, group_id: u64) -> Option<u32> {
        let lane = *self.group_lanes.get(&group_id)?;
        self.lanes.get(lane).map(|l| l.spec.output)
    }

    /// Vector indices of the groups in lane `lane`. Groups are kept lane by
    /// lane, so this is one run; for an empty lane it is the empty run where
    /// its groups would go.
    fn lane_range(&self, lane: usize) -> std::ops::Range<usize> {
        if self.lanes.is_empty() {
            return 0..self.groups.len();
        }
        let lanes: Vec<usize> = (0..self.groups.len())
            .map(|i| self.lane_index_of(i))
            .collect();
        let start = lanes.iter().position(|&l| l >= lane).unwrap_or(lanes.len());
        let end = lanes.iter().position(|&l| l > lane).unwrap_or(lanes.len());
        start..end.max(start)
    }

    /// The lane under logical point `p`, else the lane nearest to it
    /// horizontally. `None` for one continuous row.
    pub fn lane_at(&self, p: Point) -> Option<usize> {
        if self.lanes.is_empty() {
            return None;
        }
        self.lanes
            .iter()
            .position(|lane| lane.viewport().contains(p))
            .or_else(|| {
                self.lanes
                    .iter()
                    .position(|lane| p.x >= lane.spec.area.x0 && p.x < lane.spec.area.x1)
            })
            .or_else(|| {
                self.lanes
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        let da = (a.spec.area.center().x - p.x).abs();
                        let db = (b.spec.area.center().x - p.x).abs();
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, _)| i)
            })
    }

    /// Point keyboard-driven panning (direct strip pans, the visible span
    /// launches consult) at the lane of display `output`.
    pub fn set_focus_output(&mut self, output: u32) {
        if let Some(lane) = self
            .lanes
            .iter()
            .position(|lane| lane.spec.output == output)
        {
            self.focus_lane = lane;
        }
    }

    fn pan_of_lane(&self, lane: usize) -> &Pan {
        self.lanes.get(lane).map_or(&self.pan, |l| &l.pan)
    }

    fn pan_of_lane_mut(&mut self, lane: usize) -> &mut Pan {
        match self.lanes.get_mut(lane) {
            Some(l) => &mut l.pan,
            None => &mut self.pan,
        }
    }

    /// Every pan of the row: the row's own, or one per lane.
    fn pans(&self) -> Vec<&Pan> {
        if self.lanes.is_empty() {
            vec![&self.pan]
        } else {
            self.lanes.iter().map(|lane| &lane.pan).collect()
        }
    }

    /// The pan of display `output`'s lane, when the strip is partitioned.
    pub fn output_pan(&self, output: u32) -> Option<&Pan> {
        self.lanes
            .iter()
            .find(|lane| lane.spec.output == output)
            .map(|lane| &lane.pan)
    }

    /// The pan of the run that carries the leading gutter: the row's, or the
    /// leading lane's.
    fn leading_pan(&self) -> &Pan {
        self.lanes
            .iter()
            .find(|lane| lane.spec.leading)
            .map_or(&self.pan, |lane| &lane.pan)
    }

    fn pan_of_group_mut(&mut self, group_id: u64) -> &mut Pan {
        let lane = self.lane_of_group(group_id).unwrap_or(0);
        self.pan_of_lane_mut(lane)
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

    /// Per group, in strip order, whether it has its run of the strip (its
    /// lane) to itself: no other window there that is not closing. A lone
    /// window at the default width takes the strip
    /// ([`WindowWidth::resolve_in_strip`]), so closing its last neighbour
    /// widens it and opening one beside it narrows it again.
    pub fn alone_per_group(&self) -> Vec<bool> {
        let lanes: Vec<usize> = (0..self.groups.len())
            .map(|i| self.lane_index_of(i))
            .collect();
        let open: Vec<bool> = self.groups.iter().map(|g| !g.is_closing()).collect();
        (0..self.groups.len())
            .map(|i| {
                open.get(i).copied().unwrap_or(false)
                    && !lanes
                        .iter()
                        .zip(&open)
                        .enumerate()
                        .any(|(j, (&lane, &other))| j != i && other && Some(&lane) == lanes.get(i))
            })
            .collect()
    }

    /// Per-group usable width, in strip order. Depends only on the groups'
    /// own stored widths and the pushed output spans or lanes, never on a
    /// previous frame's layout, so it's stable on the very first frame.
    pub fn usable_per_group(&self) -> Vec<f64> {
        if !self.lanes.is_empty() {
            return (0..self.groups.len())
                .map(|i| {
                    self.lanes
                        .get(self.lane_index_of(i))
                        .map_or(DEFAULT_STRIP_USABLE_PX, |lane| lane.spec.usable.max(1.0))
                })
                .collect();
        }
        let alone = self.alone_per_group();
        let mut canvas_x = self.rect.x0 + self.leading_inset;
        self.groups
            .iter()
            .zip(alone)
            .map(|(g, alone)| {
                let usable = self.usable_at(canvas_x);
                canvas_x += g.width().resolve_in_strip(usable, alone) + HANDLE_SLOT_PX;
                usable
            })
            .collect()
    }

    /// Each group's `(id, left, right)` extent in unscrolled canvas space.
    /// Derived the same way as [`Self::usable_per_group`], so a group
    /// spliced in this frame already has a span without waiting for taffy.
    /// On a partitioned strip each lane's canvas starts at its own
    /// display's left edge.
    pub fn group_canvas_spans(&self) -> Vec<(u64, f64, f64)> {
        let alone = self.alone_per_group();
        if !self.lanes.is_empty() {
            let usable = self.usable_per_group();
            let mut cursor: Vec<f64> = self
                .lanes
                .iter()
                .map(|lane| lane.spec.area.x0 + lane.pan.inset)
                .collect();
            return self
                .groups
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let lane = self.lane_index_of(i);
                    let width = g.width().resolve_in_strip(
                        usable.get(i).copied().unwrap_or(DEFAULT_STRIP_USABLE_PX),
                        alone.get(i).copied().unwrap_or(false),
                    );
                    let x0 = cursor.get(lane).copied().unwrap_or(0.0);
                    if let Some(c) = cursor.get_mut(lane) {
                        *c = x0 + width + HANDLE_SLOT_PX;
                    }
                    (g.id, x0, x0 + width)
                })
                .collect();
        }
        let mut canvas_x = self.rect.x0 + self.leading_inset;
        self.groups
            .iter()
            .zip(alone)
            .map(|(g, alone)| {
                let usable = self.usable_at(canvas_x);
                let width = g.width().resolve_in_strip(usable, alone);
                let span = (g.id, canvas_x, canvas_x + width);
                canvas_x += width + HANDLE_SLOT_PX;
                span
            })
            .collect()
    }

    /// Dest-width canvas extent of one lane, in the same units as
    /// [`Pan::content_w`]. Taffy sees in-flight spring widths; the legal
    /// pan bound is the strip those springs are going to, so a half-open
    /// slot cannot shrink overflow.
    fn dest_content_w(&self, lane: usize) -> f64 {
        let inset = self.pan_of_lane(lane).inset;
        let collapsed = self.groups.iter().enumerate().any(|(i, g)| {
            self.lane_index_of(i) == lane && (g.maximized || self.fullscreen_group == Some(g.id))
        });
        if collapsed {
            return inset;
        }
        let mut x = inset;
        let mut prev = false;
        for i in 0..self.groups.len() {
            if self.lane_index_of(i) != lane {
                continue;
            }
            let Some(g) = self.groups.get(i) else {
                continue;
            };
            if prev && !self.external_navigation {
                x += HANDLE_SLOT_PX;
            }
            x += if g.is_closing() {
                g.layout_width_px()
            } else {
                g.dest_width_px()
            };
            prev = true;
        }
        x
    }

    /// Ids of groups currently visible. Reads laid-out rects, not canvas
    /// spans: "can I see it" is a question about pixels, so a group with
    /// no rect yet just reads as not-on-screen for that one frame.
    pub fn onscreen_group_ids(&self) -> Vec<u64> {
        self.groups
            .iter()
            .enumerate()
            .filter(|(i, g)| {
                let r = g.get_rect();
                let view = if self.lanes.is_empty() {
                    self.rect
                } else {
                    self.pan_of_lane(self.lane_index_of(*i)).viewport
                };
                r.width() > 0.0 && r.x1 > view.x0 && r.x0 < view.x1
            })
            .map(|(_, g)| g.id)
            .collect()
    }

    /// The visible viewport shifted by the pan, in the same space as
    /// [`Self::group_canvas_spans`]. On a partitioned strip, the focused
    /// display's lane.
    pub fn visible_canvas_span(&self) -> (f64, f64) {
        if self.lanes.is_empty() {
            return (
                self.rect.x0 + self.pan.scroll_x,
                self.rect.x1 + self.pan.scroll_x,
            );
        }
        let pan = self.pan_of_lane(self.focus_lane);
        (
            pan.viewport.x0 + pan.scroll_x,
            pan.viewport.x1 + pan.scroll_x,
        )
    }

    /// Ids of the groups laid out on display `output`, in order. Every
    /// group when the strip is one continuous row.
    pub fn group_ids_on(&self, output: u32) -> Vec<u64> {
        let Some(lane) = self
            .lanes
            .iter()
            .position(|lane| lane.spec.output == output)
        else {
            return self.group_ids();
        };
        self.groups
            .get(self.lane_range(lane))
            .unwrap_or(&[])
            .iter()
            .map(|g| g.id)
            .collect()
    }

    /// Used by the shell's `resync_strips_from_tree` to diff against the
    /// persisted `shell::state::Strip::window_groups`, preserving
    /// existing groups' host state across an unrelated revision bump.
    pub fn group_ids(&self) -> Vec<u64> {
        self.groups.iter().map(|g| g.id).collect()
    }

    /// Inserting anywhere but the end pushes later groups along; the
    /// motion is armed here and measured by the next layout pass, see
    /// [`Self::shift_offset_for`]. Closing and reordering arm it too.
    pub fn insert_group(&mut self, group: WindowGroup, idx: usize) {
        let clamped = idx.min(self.groups.len());
        if clamped < self.groups.len() {
            self.shift_armed = true;
        }
        self.groups.insert(clamped, group);
        self.structure_dirty = true;
    }

    /// Make room on screen for `group_id`, a window that has just opened,
    /// beside the window next to it: the one it opened to the right of, else
    /// the one after it. When the two don't fit together they share the
    /// display ([`tilo_desktop_model::width::share_beside`]); when no share
    /// leaves both usable, nothing changes and the strip scrolls to the new
    /// window instead. New widths are stamped as pending width changes, so
    /// the host persists them the way it persists a resize. Returns whether
    /// any width changed.
    pub fn share_with_neighbour(&mut self, group_id: u64) -> bool {
        let Some(idx) = self.groups.iter().position(|g| g.id == group_id) else {
            return false;
        };
        let lane = self.lane_of_group(group_id);
        let usable_per_group = self.usable_per_group();
        let usable = usable_per_group
            .get(idx)
            .copied()
            .unwrap_or(DEFAULT_STRIP_USABLE_PX);
        let beside = |i: usize| {
            self.groups.get(i).is_some_and(|g| {
                !g.is_closing() && !g.maximized && self.lane_of_group(g.id) == lane
            })
        };
        let neighbour = match idx.checked_sub(1) {
            Some(left) if beside(left) => left,
            _ if beside(idx.saturating_add(1)) => idx.saturating_add(1),
            _ => return false,
        };
        let (Some(opened), Some(next_to)) = (self.groups.get(idx), self.groups.get(neighbour))
        else {
            return false;
        };
        if opened.maximized || usable_per_group.get(neighbour) != Some(&usable) {
            return false;
        }
        let gap = if self.external_navigation {
            0.0
        } else {
            HANDLE_SLOT_PX
        };
        // The run pans past its leading gutter, so a pair can be seen whole
        // across that too.
        let screen = usable + self.pan_of_lane(lane.unwrap_or(0)).inset;
        let Some((neighbour_width, opened_width)) = tilo_desktop_model::width::share_beside(
            next_to.width(),
            opened.width(),
            usable,
            screen,
            gap,
        ) else {
            return false;
        };
        for (i, width) in [(neighbour, neighbour_width), (idx, opened_width)] {
            if let Some(g) = self.groups.get_mut(i)
                && g.width() != width
            {
                g.share_width(width);
                g.stamp_pending_width_change();
            }
        }
        self.structure_dirty = true;
        true
    }

    /// Move one existing group to its persisted strip position without
    /// publishing a user drag.  Shell reconciliation uses this after a
    /// remote/document revision changes the order: keeping the original
    /// `WindowGroup` instance preserves its tab hosts, focus and in-flight
    /// presentation state.
    pub fn reorder_group_by_id(&mut self, id: u64, idx: usize) -> bool {
        let Some(from) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        let to = idx.min(self.groups.len().saturating_sub(1));
        if from == to {
            return false;
        }
        let group = self.groups.remove(from);
        self.groups.insert(to, group);
        self.shift_armed = true;
        self.structure_dirty = true;
        true
    }

    /// `group_id`'s drawn offset from its laid-out x and that offset's
    /// velocity (px/s) at `now`; `(0, 0)` for a group at rest. A capture's
    /// hold poses it that far into its run, still.
    fn move_state_at(&self, group_id: u64, now: Instant) -> (f64, f64) {
        self.moves
            .iter()
            .find(|(id, _)| *id == group_id)
            .map_or((0.0, 0.0), |(_, m)| match self.shift_hold {
                Some(hold) => m.sampled_after(hold),
                None => (m.get(now), m.velocity(now)),
            })
    }

    /// How far right of its laid-out position `group_id` should be
    /// *painted* this frame (negative = left). The stored rect is always
    /// the final one, so hit-testing and the app's configure size are
    /// never mid-flight — same choice the per-group spawn fade makes.
    pub fn shift_offset_for(&self, group_id: u64) -> f64 {
        self.move_state_at(group_id, crate::motion::frame_now()).0
    }

    /// True while any group is still travelling. A capture's held pose
    /// stands still.
    fn shift_in_flight(&self) -> bool {
        let now = crate::motion::frame_now();
        self.shift_hold.is_none() && self.moves.iter().any(|(_, m)| m.is_moving(now))
    }

    /// Land every group's move: the groups are drawn at home and ask for
    /// no more frames.
    fn land_moves(&mut self) {
        for (_, offset) in &mut self.moves {
            offset.snap(0.0);
        }
        self.moves.clear();
    }

    /// How a group's rect, or the pan carrying the strip's windows, moves
    /// when `source` moved it.
    fn move_timing(&self, source: MotionSource) -> crate::motion::Timing {
        let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
        crate::motion::WindowMotion::MOVE.timing(crate::motion::Direction::Enter, motion, source)
    }

    /// Freeze the in-flight slide at `elapsed`, reporting whether there
    /// was one to freeze. The curve runs on wall time, but an offscreen
    /// render costs far more wall time per frame than the curve lasts, so
    /// a capture harness stepping frames would only see start and end —
    /// this lets it request a specific point (still production `draw`,
    /// not a reconstruction). Not gated on `debug_assertions`: `shots`
    /// walks scenario timelines through this and ships in release.
    pub fn hold_shift_at(&mut self, elapsed: Duration) -> bool {
        if self.moves.is_empty() {
            return false;
        }
        self.shift_hold = Some(elapsed);
        true
    }

    /// Say which groups another surface is drawing for now (the overview's
    /// flight carries their pictures); the row leaves them out until they
    /// are handed back with a set that no longer names them.
    pub fn set_shown_elsewhere(&mut self, ids: Vec<u64>) {
        if self.shown_elsewhere != ids {
            self.shown_elsewhere = ids;
            self.scroll_dirty = true;
        }
    }

    pub fn remove_group_by_id(&mut self, id: u64) -> bool {
        let before = self.groups.len();
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            self.close_reports
                .extend(crate::component_lifecycle::close_components(group));
        }
        self.groups.retain(|g| g.id != id);
        let removed = self.groups.len() != before;
        if removed {
            self.shift_armed = true;
            self.structure_dirty = true;
        }
        removed
    }

    /// How far the row is panned. On a partitioned strip, the leading
    /// lane's pan (the one carrying the leading gutter); see
    /// [`Self::output_pan`] for any other display.
    pub fn scroll_x(&self) -> f64 {
        self.leading_pan().scroll_x
    }

    /// The area the row is shown in, as last laid out. On a partitioned
    /// strip, the leading lane's, as [`Self::scroll_x`].
    pub fn viewport(&self) -> Rect {
        self.leading_pan().viewport
    }

    /// Select the presenter's strip-pan precision: the drawn pan lands on
    /// multiples of `quantum` layout pixels (`1 / scale` puts it on whole
    /// device pixels), or exactly where the gesture put it with `None`, the
    /// default. A quantum that is not finite and positive is ignored.
    pub fn set_scroll_quantum(&mut self, quantum: Option<f64>) {
        let quantum = quantum.filter(|step| step.is_finite() && *step > 0.0);
        if self.scroll_quantum != quantum {
            self.scroll_quantum = quantum;
            self.scroll_dirty = true;
        }
    }

    /// `value` on the presenter's pan step: the nearest multiple of the
    /// quantum, or `value` itself without one.
    fn quantized(&self, value: f64) -> f64 {
        match self.scroll_quantum {
            Some(step) => on_binary_grid((value / step).round() * step),
            None => value,
        }
    }

    fn drawn_pan_x(&self, pan: &Pan) -> f64 {
        self.quantized(pan.scroll_x)
    }

    /// [`Pan::canvas_pad`] on the presenter's pan step, rounded up so the
    /// canvas keeps at least that much room: with whole-device-pixel pans,
    /// a canvas shift (`pan + pad`) is then whole device pixels too.
    fn lane_canvas_pad(&self, pan: &Pan) -> f64 {
        let pad = pan.canvas_pad();
        match self.scroll_quantum {
            Some(step) => on_binary_grid((pad / step).ceil() * step),
            None => pad,
        }
    }

    /// The pan used by layout, input geometry and the presenter's canvas.
    pub fn drawn_scroll_x(&self) -> f64 {
        self.drawn_pan_x(self.leading_pan())
    }

    /// Where a strip whose viewport is `viewport` draws: the viewport and a
    /// 16 px pad above and below it that keeps shadow and glow sprites
    /// (ambient_pad=50, bloom_pad=120, see `TileHost::draw_steady_state`)
    /// intact at the top and bottom.
    pub fn drawn_bounds(viewport: Rect) -> Rect {
        Rect::new(
            viewport.x0,
            viewport.y0 - 16.0,
            viewport.x1,
            viewport.y1 + 16.0,
        )
    }

    /// How far a retained draw moves the row into canvas coordinates
    /// ([`crate::retained_present::unbake_scroll`]): the drawn pan plus the
    /// canvas's padding while it lies within the canvas, zero outside a
    /// retained draw.
    pub fn canvas_shift(&self) -> f64 {
        let scroll = self.drawn_scroll_x();
        let pad = self.canvas_pad();
        if crate::retained_present::unbake_scroll()
            && pan_within_canvas(scroll, self.max_scroll_x(), pad)
        {
            scroll + pad
        } else {
            0.0
        }
    }

    /// Room a retained canvas keeps left of the content and right of the
    /// furthest pan, in layout pixels.
    pub fn canvas_pad(&self) -> f64 {
        self.lane_canvas_pad(self.leading_pan())
    }

    /// A frame was presented: each group may step its width spring once
    /// more. The token a step spends returns when the group is marked
    /// drawn, but a host that presents frames without marking its content
    /// drawn (the compositor) must say so here, or every width spring it
    /// does not snap stops after its first step while its group keeps
    /// asking for animation frames.
    pub fn frame_presented(&mut self) {
        for group in &mut self.groups {
            group.frame_presented();
        }
    }

    /// Where the strip deck has this row, relative to where it rests: its
    /// lanes move with it (their areas are given where it rests). The
    /// deck sets this before it lays the row out.
    pub(crate) fn set_deck_shift(&mut self, shift: f64) {
        self.deck_shift = shift;
        for lane in &mut self.lanes {
            lane.shift = shift;
        }
    }

    /// Have the parent's draw skip this row, recording only its transform,
    /// so its owner draws it into a scene of its own with
    /// [`Self::draw_apart`]: a presenter that pans each strip on its own
    /// needs each strip's drawing apart from the others'.
    pub fn set_drawn_apart(&mut self, apart: bool) {
        self.drawn_apart = apart;
        self.apart_transform = None;
    }

    /// Draw the row into `scene` with the transform its parent's draw
    /// passed, once that draw skipped it ([`Self::set_drawn_apart`]).
    /// `false`, drawing nothing, when the parent did not draw it.
    pub fn draw_apart(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        let Some(transform) = self.apart_transform.take() else {
            return false;
        };
        self.drawn_apart = false;
        self.draw(scene, transform, tr, theme, presence);
        self.drawn_apart = true;
        true
    }

    /// Set the real strip viewport to an absolute finite offset. Browser and
    /// other external navigation controllers use this narrow seam instead of
    /// synthesizing wheel input or moving rendered output independently.
    pub fn pan_to(&mut self, requested: f64) -> bool {
        if !requested.is_finite() {
            return false;
        }
        let pan = self.pan_of_lane_mut(self.focus_lane);
        pan.absolute = true;
        let next = requested.clamp(0.0, pan.max_scroll_x());
        let changed = (next - pan.scroll_x).abs() > f64::EPSILON
            || pan.animating()
            || pan.last_wheel_at.is_some();
        if changed {
            pan.jump_to(next);
            self.scroll_dirty = true;
        }
        changed
    }

    /// Carry peer-local viewport presentation through a retained hierarchy
    /// rebuild. The next layout still validates/clamps against its new geometry.
    pub fn inherit_viewport(&mut self, previous: &Self) {
        self.rect = previous.rect;
        self.pan = previous.pan.clone();
        if self.lanes.is_empty() {
            self.lanes.clone_from(&previous.lanes);
            self.group_lanes.clone_from(&previous.group_lanes);
            self.focus_lane = previous.focus_lane;
        } else {
            for lane in &mut self.lanes {
                if let Some(old) = previous
                    .lanes
                    .iter()
                    .find(|old| old.spec.output == lane.spec.output)
                {
                    lane.pan = old.pan.clone();
                }
            }
        }
        self.output_spans.clone_from(&previous.output_spans);
    }

    /// Resolve an admitted keyboard owner through its actual layout ancestry.
    /// Stored layout identities prevent stale child indices after a splice.
    pub fn group_for_focus_target(&self, mut target: NodeId, tree: &TaffyTree<()>) -> Option<u64> {
        loop {
            if let Some(index) = self.node_ids.iter().position(|node| *node == target) {
                let id = *self.laid_group_ids.get(index)?;
                return self
                    .groups
                    .iter()
                    .any(|group| group.id == id && !group.is_closing())
                    .then_some(id);
            }
            target = tree.parent(target)?;
        }
    }

    /// Whether the stored geometry still belongs to the current group order.
    /// A splice/reorder must complete layout before a host frames its selection.
    pub fn group_layout_is_current(&self) -> bool {
        self.laid_group_ids
            .iter()
            .copied()
            .eq(self.groups.iter().map(|group| group.id))
            && self.width_basis_is_current()
    }

    fn width_basis_is_current(&self) -> bool {
        self.groups
            .iter()
            .zip(self.usable_per_group())
            .all(|(group, usable)| (group.strip_usable_px() - usable).abs() <= 0.5)
    }

    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }

    pub fn handle_at(&self, idx: usize) -> Option<&ResizeHandle> {
        self.handles.get(idx)
    }

    /// `debug_assertions` gated so it's available to `crates/sdk/ui-core/tests` but not
    /// shipped in release.
    #[cfg(any(test, debug_assertions))]
    pub fn group_at_for_test(&self, idx: usize) -> Option<&WindowGroup> {
        self.groups.get(idx)
    }

    /// `0.0` before the first layout pass. On a partitioned strip, the
    /// focused lane's.
    pub fn content_width(&self) -> f64 {
        self.pan_of_lane(self.focus_lane).content_w
    }

    pub fn max_scroll_x(&self) -> f64 {
        self.pan_of_lane(self.focus_lane).max_scroll_x()
    }

    /// Furthest legal resting offset for direct strip panning. Unlike the
    /// content-tight focus bound, this leaves a deliberate glimpse of canvas
    /// after the final app. Content which already fits stays pinned at home.
    pub fn pan_max_scroll_x(&self) -> f64 {
        self.pan_of_lane(self.focus_lane).pan_max_scroll_x()
    }

    /// Rebase total-travel updates onto the strip's current visual position.
    /// A fresh axis lock calls this even if a prior stream lost its End at a
    /// lock/session boundary, preventing stale gesture state from jumping.
    /// On a partitioned strip only the focused display's lane pans.
    pub fn begin_strip_pan(&mut self) {
        self.pan_of_lane_mut(self.focus_lane).begin_strip_pan();
    }

    /// Track a three-finger horizontal gesture in direct-manipulation space.
    /// `travel` is total signed scroll travel since Begin (positive moves
    /// toward later groups), reported `at` (the input event's own time, so
    /// a batch drained late keeps its spacing). Legal canvas tracks 1:1;
    /// beyond either end the shared iOS-style curve supplies continuously
    /// increasing resistance. Each frame shows the travel resampled at the
    /// frame's time, so a steady swipe moves the strip evenly whatever the
    /// touchpad's report rate.
    pub fn update_strip_pan(&mut self, travel: f64, at: Instant) -> bool {
        let moved = self
            .pan_of_lane_mut(self.focus_lane)
            .update_strip_pan(travel, at);
        if moved {
            self.scroll_dirty = true;
        }
        moved
    }

    /// Hold the strip under a pan that reports where it is, not fingers:
    /// at `requested`, an offset that may lie past either end, shown 1:1
    /// within the strip and against the rubber band beyond it, at once and
    /// with nothing to resample. A wheel gesture calls this with each event's
    /// total and lets go with [`Self::cancel_strip_pan`], so a pull past an
    /// end springs back as a released trackpad pan does.
    pub fn hold_strip_pull(&mut self, requested: f64, at: Instant) -> bool {
        let pan = self.pan_of_lane_mut(self.focus_lane);
        pan.begin_strip_pan();
        pan.strip_pan_origin = Some(0.0);
        let moved = pan.update_strip_pan(requested, at);
        if moved {
            self.scroll_dirty = true;
        }
        moved
    }

    /// Release direct manipulation at `now`. A strip released at rest
    /// stays exactly where the fingers left it; a thrown one coasts on with
    /// their velocity (not under reduced motion); elastic overpull settles
    /// to its end.
    pub fn end_strip_pan(&mut self, now: Instant) -> bool {
        self.finish_strip_pan(now, !self.reduce_motion)
    }

    /// Cancel direct manipulation at `now`: the strip stays where it is,
    /// thrown nowhere, and only elastic overpull settles to its end.
    pub fn cancel_strip_pan(&mut self, now: Instant) -> bool {
        self.finish_strip_pan(now, false)
    }

    fn finish_strip_pan(&mut self, now: Instant, throw: bool) -> bool {
        let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
        let settle = self.move_timing(MotionSource::Gesture);
        let settling = self
            .pan_of_lane_mut(self.focus_lane)
            .end_strip_pan(now, throw, motion, settle);
        if settling {
            self.scroll_dirty = true;
        }
        settling
    }

    /// Keep the strip where a person's pan left it, bounded as
    /// [`Self::pan_to`] bounds it, until focus next pans a window into
    /// view. A wheel spin stops wherever its notches took the strip, often
    /// with windows cut at both edges.
    pub fn hold_strip_pan(&mut self) {
        self.pan_of_lane_mut(self.focus_lane).absolute = true;
    }

    /// Whether a layout pass has given `group_id` its place. A group
    /// focused the same frame it was spliced in has none yet, so a pan to
    /// it has nothing to aim at. A window opening has its place from its
    /// first layout, while its slot still grows from nothing: a reveal aims
    /// at where it will be once grown ([`Self::scroll_target_for`]), so the
    /// strip pans as the window opens rather than once it has some width.
    pub fn group_is_laid_out(&self, group_id: u64) -> bool {
        self.laid_group_ids.contains(&group_id) && self.groups.iter().any(|g| g.id == group_id)
    }

    /// Pan `group_id`'s run (the row, or its display's lane) so the group
    /// is fully inside the viewport (Niri's "focused column follows into
    /// view"). Must be called after a layout pass (uses the group's live
    /// rect). Cancels any in-flight wheel-snap so this isn't immediately
    /// overridden by a snap-back.
    pub fn scroll_group_into_view(&mut self, group_id: u64) -> bool {
        self.pan_of_group_mut(group_id).absolute = false;
        let Some(sx) = self.scroll_target_for(group_id) else {
            return false;
        };
        self.pan_of_group_mut(group_id).jump_to(sx);
        self.scroll_dirty = true;
        true
    }

    /// Same target as [`Self::scroll_group_into_view`], moved as `source`
    /// asks. After a pointer or gesture it glides on the group-move spring
    /// instead of teleporting, so a focus jump reads as motion rather than a
    /// cut, and a run of jumps (a spun wheel) as one motion that keeps its
    /// speed. After a shortcut or action the content is already where it
    /// belongs and only the viewport slides, within `fast`, to say where it
    /// went.
    pub fn scroll_group_into_view_animated(
        &mut self,
        group_id: u64,
        now: Instant,
        source: MotionSource,
    ) -> bool {
        self.pan_of_group_mut(group_id).absolute = false;
        let Some(sx) = self.scroll_target_for(group_id) else {
            return false;
        };
        if source.lands_at_once() {
            let slide = self.snap_timing(source);
            self.pan_of_group_mut(group_id).slide_to(sx, now, slide);
        } else {
            let timing = self.move_timing(source);
            self.pan_of_group_mut(group_id).glide_to(sx, now, timing);
        }
        self.scroll_dirty = true;
        true
    }

    /// Say what starts the transitions laid out next: a shortcut or action
    /// lands its result on the next frame (groups take their new slots,
    /// widths and maximized rects at once, siblings and tab chips do not
    /// fade), while a pointer or gesture keeps the full motion. The host
    /// sets it before acting and returns it to [`MotionSource::Pointer`]
    /// once the layout that shows the result has run.
    pub fn set_motion_source(&mut self, source: MotionSource) {
        if self.motion_source == source {
            return;
        }
        self.motion_source = source;
        for group in &mut self.groups {
            group.set_motion_source(source);
        }
    }

    pub fn motion_source(&self) -> MotionSource {
        self.motion_source
    }

    /// The pan that brings `group_id` wholly into view, aimed at where the
    /// group will be once the slot-width springs of it and the groups before
    /// it have settled, not at where they are this frame.
    ///
    /// A window that has just opened grows from zero width over several
    /// frames. Aiming at its first frames' rect ends the pan short of it, and
    /// the host's edge-triggered framing (which keys on destination widths)
    /// sees nothing change afterwards and never frames it again: the focused
    /// new window is left off screen.
    fn scroll_target_for(&self, group_id: u64) -> Option<f64> {
        let (lane, rect) = self.settled_rect_of(group_id)?;
        self.pan_of_lane(lane)
            .reveal_target(rect, self.settled_max_scroll_x(lane))
    }

    /// The furthest a reveal pans `lane` once the slot-width springs of its
    /// groups have settled: [`Pan::max_scroll_x`] of the strip they are
    /// going to ([`Self::dest_content_w`]), which is already where they
    /// end, so nothing of their flight is added to it again.
    fn settled_max_scroll_x(&self, lane: usize) -> f64 {
        self.pan_of_lane(lane)
            .max_scroll_x_of(self.dest_content_w(lane))
    }

    /// Pan so that `keep` and `also` are both wholly in view when they fit
    /// on screen together, else just `keep`; glided like
    /// [`Self::scroll_group_into_view_animated`]. For a window that opened
    /// beside the one the person is in without taking their focus: both
    /// are shown, and if they cannot be, the person keeps what they were
    /// looking at. `false` when neither has been laid out in one lane or
    /// nothing has to move.
    pub fn scroll_groups_into_view_animated(
        &mut self,
        keep: u64,
        also: u64,
        now: Instant,
        source: MotionSource,
    ) -> bool {
        let (Some((lane, kept)), Some((also_lane, added))) =
            (self.settled_rect_of(keep), self.settled_rect_of(also))
        else {
            return self.scroll_group_into_view_animated(keep, now, source);
        };
        let pan = self.pan_of_lane(lane);
        let both = kept.union(added);
        let rect = if also_lane == lane && both.width() <= pan.viewport.width() {
            both
        } else {
            kept
        };
        let Some(sx) = pan.reveal_target(rect, self.settled_max_scroll_x(lane)) else {
            return false;
        };
        self.pan_of_group_mut(keep).absolute = false;
        if source.lands_at_once() {
            let slide = self.snap_timing(source);
            self.pan_of_group_mut(keep).slide_to(sx, now, slide);
        } else {
            let timing = self.move_timing(source);
            self.pan_of_group_mut(keep).glide_to(sx, now, timing);
        }
        self.scroll_dirty = true;
        true
    }

    /// Where `group_id` will be once the slot-width springs of it and the
    /// groups before it have settled, and the lane it is in.
    fn settled_rect_of(&self, group_id: u64) -> Option<(usize, Rect)> {
        let g = self.groups.iter().find(|g| g.id == group_id)?;
        // No layout pass has placed this group yet, so it has no position.
        if !self.laid_group_ids.contains(&group_id) {
            return None;
        }
        let lane = self.lane_of_group(group_id).unwrap_or(0);
        let mut rect = g.get_rect();
        let mut shift = 0.0;
        for earlier in &self.groups {
            if earlier.id == group_id {
                break;
            }
            if self.lane_of_group(earlier.id).unwrap_or(0) == lane {
                shift += earlier.dest_width_px() - earlier.layout_width_px();
            }
        }
        let own = g.dest_width_px() - g.layout_width_px();
        rect = Rect::new(rect.x0 + shift, rect.y0, rect.x1 + shift + own, rect.y1);
        Some((lane, rect))
    }

    /// Ids of groups the viewport edges are cutting through right now.
    /// Empty when everything fits; otherwise the ones hanging off the
    /// ends, which is normal.
    pub fn groups_cut_by_row_edges(&self) -> Vec<u64> {
        self.pans()
            .iter()
            .flat_map(|pan| pan.groups_cut_by_edges())
            .collect()
    }

    /// Select `tab_id` inside `group_id` widget-side. The bar's
    /// `active_id` is authoritative and the shell's per-frame harvest
    /// mirrors it into the persisted tree, so a tree-only write would be
    /// overwritten one frame later.
    pub fn set_group_active_tab(&mut self, group_id: u64, tab_id: u64) -> bool {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group_id) else {
            return false;
        };
        if !g.tabs().iter().any(|tab| tab.id == tab_id) {
            return false;
        }
        // Selecting the tab already visible is still a valid address.  The
        // shell uses this boolean as a stale-target guard before it writes
        // durable focus, so `false` must mean "the group/tab does not
        // exist", not merely "nothing needed repainting".
        if g.active_tab_id() == tab_id {
            return true;
        }
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
    /// tree. Advances every run's snap.
    pub fn tick_snap_at(&mut self, now: Instant) {
        let mut moved = false;
        let snap = self.snap_timing(MotionSource::Pointer);
        // A capture's hold poses the pan with the group moves it started
        // alongside.
        let hold = self.shift_hold.filter(|_| !self.moves.is_empty());
        if self.lanes.is_empty() {
            moved = self.pan.tick_snap_at(now, snap, hold);
        }
        for lane in &mut self.lanes {
            moved |= lane.pan.tick_snap_at(now, snap, hold);
        }
        // Must invalidate the parent's subtree cache or needs_redraw()
        // stays false and it re-emits the pre-snap cached scene.
        if moved {
            self.scroll_dirty = true;
        }
    }

    fn snap_in_flight(&self) -> bool {
        let snap = self.snap_timing(MotionSource::Pointer);
        self.pans().iter().any(|pan| pan.snap_in_flight(snap))
    }

    /// How the pan settles when `source` moved it, on the standard snap.
    fn snap_timing(&self, source: MotionSource) -> SnapTiming {
        let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
        SnapTiming::of(motion, source)
    }

    /// A direct pan is under the fingers, or settling after them.
    pub fn direct_pan_in_progress(&self) -> bool {
        let snap = self.snap_timing(MotionSource::Pointer);
        self.pans()
            .iter()
            .any(|pan| pan.strip_pan_origin.is_some() || pan.snap_in_flight(snap))
    }

    /// Leave the pan at `x` as a person's own pan would.
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn set_scroll_x_for_test(&mut self, x: f64) {
        self.pan.scroll_x = x;
        self.pan.past_windows = x > self.pan.max_scroll_x() + 0.5;
    }

    /// The groups presented as children ([`Widget::children_mut`]): none
    /// while hidden, the promoted one while one is fullscreen or
    /// maximized, otherwise all of them.
    fn shown_group_indices(&self) -> Vec<usize> {
        if self.hidden {
            return Vec::new();
        }
        match self
            .fullscreen_idx()
            .or_else(|| self.groups.iter().position(|g| g.maximized))
        {
            Some(index) => vec![index],
            None => (0..self.groups.len()).collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn replayed_groups_for_test(&self) -> &[u64] {
        &self.replayed_groups
    }

    #[cfg(test)]
    pub(crate) fn group_left_edges_for_test(&self) -> &[f64] {
        &self.pan.group_left_edges
    }

    #[cfg(test)]
    pub(crate) fn set_last_wheel_at_for_test(&mut self, t: Instant) {
        self.pan.last_wheel_at = Some(t);
    }

    #[cfg(test)]
    pub(crate) fn set_group_left_edges_for_test(&mut self, edges: Vec<f64>) {
        self.pan.group_left_edges = edges;
    }

    #[cfg(test)]
    pub(crate) fn set_content_w_for_test(&mut self, w: f64) {
        self.pan.content_w = w;
    }

    #[cfg(test)]
    pub(crate) fn set_rect_for_test(&mut self, r: Rect) {
        self.rect = r;
        self.pan.viewport = r;
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
        self.prune_empty_group_after_transfer(src);
        self.pending_topology_change = true;
        true
    }

    /// Move the active tab out to an adjacent standalone group.
    pub fn detach_active_tab(&mut self, group_id: u64) -> bool {
        let Some(src) = self.groups.iter().position(|group| group.id == group_id) else {
            return false;
        };
        let tab = self.groups[src].active_tab_id();
        self.split_tab_into_new_group(src, tab, src.saturating_add(1))
    }

    /// Move the active tab into the adjacent group in `direction`.
    pub fn attach_active_tab(&mut self, group_id: u64, direction: i32) -> bool {
        let Some(src) = self.groups.iter().position(|group| group.id == group_id) else {
            return false;
        };
        let dst = if direction < 0 {
            src.checked_sub(1)
        } else {
            src.checked_add(1).filter(|idx| *idx < self.groups.len())
        };
        let Some(dst) = dst.filter(|&dst| self.lane_index_of(dst) == self.lane_index_of(src))
        else {
            return false;
        };
        let tab = self.groups[src].active_tab_id();
        let target = self.groups[dst].tabs().len();
        self.transfer_tab(src, dst, tab, target)
    }

    /// A tab merged into a sibling may have emptied its source window.
    /// Keep the row's structural invariant here so every transfer path uses
    /// the same pruning rule before publishing its topology.
    fn prune_empty_group_after_transfer(&mut self, source: usize) {
        if self
            .groups
            .get(source)
            .is_some_and(|group| group.tab_count() == 0)
        {
            self.groups.remove(source);
            self.structure_dirty = true;
        }
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
        if let Some(lane) = self.lane_at(release_point) {
            let run = self.lane_range(lane);
            let Some(first) = self.groups.get(run.start).filter(|_| !run.is_empty()) else {
                return Some(run.start);
            };
            if release_point.x < first.get_rect().x0 {
                return Some(run.start);
            }
            for j in run.start..run.end.saturating_sub(1) {
                let (Some(a), Some(b)) = (self.groups.get(j), self.groups.get(j.saturating_add(1)))
                else {
                    continue;
                };
                if release_point.x >= a.get_rect().x1 && release_point.x <= b.get_rect().x0 {
                    return Some(j.saturating_add(1));
                }
            }
            return Some(run.end);
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
        if !self.lanes.is_empty() {
            // The display the tab was dropped on, else its source's.
            let lane = self
                .pending_split_lane
                .take()
                .unwrap_or_else(|| self.lane_index_of(src));
            self.group_lanes.insert(new_id, lane);
        }
        self.pending_split_lane = None;
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
        self.pending_topology_change = true;
        true
    }

    /// Drain the current row topology after a same-strip tab transfer.
    /// Returning the final shape rather than a sequence of move events makes
    /// multiple transfers in one frame idempotent at the persistence boundary.
    pub fn take_pending_topology_change(&mut self) -> Option<Vec<GroupTopology>> {
        if !std::mem::take(&mut self.pending_topology_change) {
            return None;
        }
        Some(
            self.groups
                .iter()
                .map(|group| GroupTopology {
                    id: group.id,
                    tab_ids: group.tabs().iter().map(|tab| tab.id).collect(),
                    active_tab: group.active_tab_id(),
                    width: group.width(),
                })
                .collect(),
        )
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

    /// The target whose indicator is currently on screen for a tab drag.
    pub fn tab_drop_target(&self) -> Option<TabDropTarget> {
        self.tab_drop_target
    }

    /// Cancel owned tab gestures and their in-strip drop feedback.
    pub fn cancel_tab_drag(&mut self) -> bool {
        let mut cancelled = false;
        for group in &mut self.groups {
            cancelled |= group.cancel_tab_drag();
        }
        if cancelled {
            self.clear_tab_drop_preview();
        }
        cancelled
    }

    /// Whether one of this row's own tab bars currently owns a drag.
    pub fn tab_drag_active(&self) -> bool {
        self.groups.iter().any(WindowGroup::drag_in_flight)
    }

    /// The tab one of this row's tab bars is dragging, while one is.
    pub fn dragged_tab(&self) -> Option<u64> {
        self.groups.iter().find_map(WindowGroup::dragged_tab)
    }

    /// The exact destination currently painted for a tab from another strip.
    pub fn cross_strip_drop_target(&self) -> Option<TabDropTarget> {
        self.cross_strip_drop_target
    }

    /// Preview a tab arriving from another strip. The stored enum is later
    /// consumed verbatim by the shell's cross-strip commit path.
    pub fn set_cross_strip_tab_drag_hover(&mut self, cursor: Option<Point>) {
        let was_active = self.cross_strip_drop_target.is_some();
        self.cross_strip_drop_target = cursor.and_then(|p| {
            self.last_cursor = p;
            self.tab_drop_target_with_previous(p, self.cross_strip_drop_target)
        });
        // A no-op clear must not erase a compositor-owned content-drop line,
        // which shares the tab bar's external-hover paint primitive.
        if cursor.is_none() && !was_active {
            return;
        }
        for group in &mut self.groups {
            let hover = match self.cross_strip_drop_target {
                Some(TabDropTarget::ExistingGroup { group_id, .. }) if group.id == group_id => {
                    cursor
                }
                _ => None,
            };
            group.set_external_drag_hover(hover);
        }
    }

    /// Drive the same preview for content carried by the compositor rather
    /// than by one of this row's tabs. `None` retires the indicator when the
    /// protocol drag leaves or finishes.
    pub fn set_external_content_drag_hover(&mut self, cursor: Option<Point>) {
        self.external_drag_over = cursor.is_some();
        self.tab_drop_target = cursor.and_then(|p| {
            self.last_cursor = p;
            self.tab_drop_target_with_hysteresis(p)
        });
        for group in &mut self.groups {
            let hover = match self.tab_drop_target {
                Some(TabDropTarget::ExistingGroup { group_id, .. }) if group.id == group_id => {
                    cursor
                }
                _ => None,
            };
            group.set_external_drag_hover(hover);
        }
    }

    /// Whether a drag that can drop into this row is in flight over it: one
    /// of its own tabs, a whole window of it being moved, a tab from another
    /// strip, or compositor content.
    fn drag_over_row(&self) -> bool {
        self.external_drag_over
            || self.cross_strip_drop_target.is_some()
            || self.tab_drag_active()
            || self.group_drag_moving()
    }

    /// Where a drag that can land on another strip is, while one is in
    /// flight from or over this row: one of its own tabs, a tab from another
    /// strip, or compositor content. The strip deck reads it to reach the
    /// strip above or below ([`super::StripDeck`]).
    pub(crate) fn strip_reaching_drag(&self) -> Option<Point> {
        (self.external_drag_over
            || self.cross_strip_drop_target.is_some()
            || self.tab_drag_active())
        .then_some(self.last_cursor)
    }

    /// A whole window of this row is being moved (not one someone else holds,
    /// which only resists the pull and never moves).
    fn group_drag_moving(&self) -> bool {
        self.group_drag.is_some_and(|drag| drag.moved && !drag.held)
    }

    /// How fast, in px/s, the lane under `p` pans for a drag at `p`:
    /// negative leftwards, `0` outside the edge bands or with nowhere to go.
    fn drag_edge_velocity(&self, p: Point) -> f64 {
        let pan = self.pan_of_lane(self.lane_at(p).unwrap_or(self.focus_lane));
        let view = pan.viewport;
        if !view.contains(p) {
            return 0.0;
        }
        let velocity = crate::edge_autoscroll::velocity(p.x, view.x0, view.x1);
        let room = if velocity < 0.0 {
            pan.scroll_x > 0.0
        } else {
            pan.scroll_x < pan.max_scroll_x()
        };
        if room { velocity } else { 0.0 }
    }

    /// The background tab of another window under `p`: its group and tab.
    /// The tab a window already shows, the bar a drag started from (it is
    /// reordering its own tabs) and the window being moved are never
    /// spring-loaded.
    fn background_tab_at(&self, p: Point) -> Option<(u64, u64)> {
        let moving = self.group_drag.map(|drag| drag.group_id);
        self.groups
            .iter()
            .enumerate()
            .filter(|&(i, group)| {
                !self.layout_hides_group(i)
                    && !group.is_closing()
                    && !group.drag_in_flight()
                    && Some(group.id) != moving
            })
            .find_map(|(_, group)| {
                let index = group.tab_rects().iter().position(|r| r.contains(p))?;
                let tab = group.tabs().get(index)?.id;
                (tab != group.active_tab_id()).then_some((group.id, tab))
            })
    }

    /// Advance [`DragReach`] to `now`: pan the lane under a drag resting in
    /// an edge band, and bring a background tab rested on long enough to the
    /// front. Returns whether anything moved.
    fn advance_drag_reach(&mut self, now: Instant) -> bool {
        if !self.drag_over_row() {
            self.drag_reach = DragReach::default();
            return false;
        }
        let p = self.last_cursor;
        let mut moved = false;
        let velocity = self.drag_edge_velocity(p);
        let step = self.drag_reach.edge.advance(velocity, now);
        if step != 0.0 {
            let lane = self.lane_at(p).unwrap_or(self.focus_lane);
            let pan = self.pan_of_lane_mut(lane);
            let next = (pan.scroll_x + step).clamp(0.0, pan.max_scroll_x());
            let travel = next - pan.scroll_x;
            if travel.abs() > f64::EPSILON {
                pan.absolute = false;
                pan.jump_to(next);
                self.scroll_dirty = true;
                moved = true;
                // A window being moved is drawn at its slot plus the pointer's
                // travel since the grab. Its slot scrolls with the strip, so
                // the travel grows by what the strip panned and the window
                // stays under the pointer.
                let in_lane = self.lanes.is_empty()
                    || self
                        .group_drag
                        .and_then(|drag| self.lane_of_group(drag.group_id))
                        == Some(lane);
                if in_lane
                    && self.group_drag_moving()
                    && let Some(drag) = self.group_drag.as_mut()
                {
                    drag.start.x -= travel;
                }
            }
        }
        match (self.background_tab_at(p), self.drag_reach.tab) {
            (Some((group, tab)), Some((g, t, since))) if g == group && t == tab => {
                if now.saturating_duration_since(since) >= DRAG_SPRING_LOAD {
                    self.drag_reach.tab = None;
                    moved |= self.set_group_active_tab(group, tab);
                }
            }
            (Some((group, tab)), _) => self.drag_reach.tab = Some((group, tab, now)),
            (None, _) => self.drag_reach.tab = None,
        }
        if moved {
            // What is under the resting pointer changed: re-aim the preview.
            if self.external_drag_over {
                self.set_external_content_drag_hover(Some(p));
            } else if self.group_drag_moving() {
                let _ = self.move_group_drag();
            } else if self.cross_strip_drop_target.is_some() {
                self.set_cross_strip_tab_drag_hover(Some(p));
            } else {
                self.refresh_tab_drop_preview();
            }
        }
        moved
    }

    /// Whether [`Self::advance_drag_reach`] has work on the next frame.
    fn drag_reach_pending(&self) -> bool {
        self.drag_over_row()
            && (self.drag_edge_velocity(self.last_cursor).abs() >= f64::EPSILON
                || self.drag_reach.tab.is_some())
    }

    /// [`Self::advance_drag_reach`] at an explicit time, for tests.
    #[cfg(test)]
    fn advance_drag_reach_for_test(&mut self, now: Instant) -> bool {
        self.advance_drag_reach(now)
    }

    /// Resolve one pointer position into the strip's complete tab-drop
    /// grammar. The tab bar wins and keeps its precise pill-gap insertion.
    /// In the body, adaptive outer bands make a new group while the remaining
    /// interior prepends/appends to the existing group's tab list. Only space
    /// outside every group can otherwise produce a new group.
    pub fn tab_drop_target_at(&self, p: Point) -> Option<TabDropTarget> {
        for (group_idx, group) in self.groups.iter().enumerate() {
            let rect = group.get_rect();
            // A window a maximized one covers keeps its last tiled rect, but
            // nothing of it shows: it is no drop target.
            if !rect.contains(p) || self.layout_hides_group(group_idx) {
                continue;
            }
            if group.is_closing() {
                // Preserve the existing closing-bar escape hatch: the fading
                // group cannot safely adopt a tab, so its former bar resolves
                // to the gap immediately before it. Its content is no target.
                return group
                    .bar_rect()
                    .contains(p)
                    .then_some(TabDropTarget::GroupGap {
                        gap_index: group_idx,
                    });
            }
            if group.bar_rect().contains(p) {
                return Some(TabDropTarget::ExistingGroup {
                    group_id: group.id,
                    tab_index: group.drop_index_for(p),
                });
            }

            let edge = tab_group_edge_width(rect.width());
            if p.x < rect.x0 + edge {
                return Some(TabDropTarget::GroupGap {
                    gap_index: group_idx,
                });
            }
            if p.x > rect.x1 - edge {
                return Some(TabDropTarget::GroupGap {
                    gap_index: group_idx.saturating_add(1),
                });
            }
            return Some(TabDropTarget::ExistingGroup {
                group_id: group.id,
                tab_index: if p.x < rect.center().x {
                    0
                } else {
                    group.tabs().len()
                },
            });
        }

        self.gap_index_for(p)
            .map(|gap_index| TabDropTarget::GroupGap { gap_index })
    }

    /// Retain the previous target for a small band around any boundary. The
    /// resolver itself stays pure and directly testable; hysteresis is only a
    /// presentation policy for successive pointer samples.
    fn tab_drop_target_with_hysteresis(&self, p: Point) -> Option<TabDropTarget> {
        self.tab_drop_target_with_previous(p, self.tab_drop_target)
    }

    fn tab_drop_target_with_previous(
        &self,
        p: Point,
        previous: Option<TabDropTarget>,
    ) -> Option<TabDropTarget> {
        let next = self.tab_drop_target_at(p);
        let Some(previous) = previous else {
            return next;
        };
        if next == Some(previous) {
            return next;
        }
        let near_samples = [
            Point::new(p.x - TAB_DROP_HYSTERESIS_PX, p.y),
            Point::new(p.x + TAB_DROP_HYSTERESIS_PX, p.y),
            Point::new(p.x, p.y - TAB_DROP_HYSTERESIS_PX),
            Point::new(p.x, p.y + TAB_DROP_HYSTERESIS_PX),
        ];
        if near_samples
            .into_iter()
            .any(|sample| self.tab_drop_target_at(sample) == Some(previous))
        {
            Some(previous)
        } else {
            next
        }
    }

    fn refresh_tab_drop_preview(&mut self) {
        let any_drag = self
            .groups
            .iter()
            .any(super::window_group::WindowGroup::drag_in_flight);
        self.tab_drop_target = if any_drag && self.covers(self.last_cursor) {
            self.tab_drop_target_with_hysteresis(self.last_cursor)
        } else {
            None
        };
        for group in &mut self.groups {
            let cursor = match self.tab_drop_target {
                Some(TabDropTarget::ExistingGroup { group_id, .. }) if group.id == group_id => {
                    Some(self.last_cursor)
                }
                _ => None,
            };
            group.set_external_drag_hover(cursor);
        }
    }

    fn clear_tab_drop_preview(&mut self) {
        self.tab_drop_target = None;
        for group in &mut self.groups {
            group.set_external_drag_hover(None);
        }
    }

    /// Slot before the first group whose centre sits right of the
    /// cursor. Centre-based so nudging past a neighbour's midpoint
    /// commits the swap, matching tab-reorder feel. On a partitioned strip
    /// the slot is among the groups of the display under the cursor, with
    /// that display's lane: dragging a group onto another display moves it
    /// there.
    fn group_insertion_for(&self, p: Point) -> (usize, Option<usize>) {
        let lane = match self.group_drag {
            Some(drag) if !self.lanes.is_empty() => Some(
                self.lane_at(p)
                    .unwrap_or_else(|| self.lane_index_of(drag.src_idx)),
            ),
            _ => None,
        };
        let run = lane.map_or(0..self.groups.len(), |lane| self.lane_range(lane));
        for i in run.clone() {
            if self
                .groups
                .get(i)
                .is_some_and(|g| p.x < g.get_rect().center().x)
            {
                return (i, lane);
            }
        }
        (run.end, lane)
    }

    /// `insert_at` is a pre-removal gap index; converted here to the
    /// post-removal landing slot so the desktop's `ReorderGroup` command
    /// reproduces the same order. On a partitioned strip `lane` is the
    /// lane the gap is in; a group landing in another lane moves to it even
    /// when its index stays the same (the last group of one display dropped
    /// at the start of the next).
    fn commit_group_reorder(
        &mut self,
        src: usize,
        group_id: u64,
        insert_at: usize,
        lane: Option<usize>,
    ) {
        if src >= self.groups.len() {
            return;
        }
        let changes_lane =
            lane.is_some_and(|lane| !self.lanes.is_empty() && self.lane_index_of(src) != lane);
        // Removing `src` shifts every later slot left by one.
        let final_idx = if insert_at > src {
            insert_at.saturating_sub(1)
        } else {
            insert_at
        };
        let dest = final_idx.min(self.groups.len().saturating_sub(1));
        if dest == src && !changes_lane {
            return;
        }
        let g = self.groups.remove(src);
        self.groups.insert(dest, g);
        if let Some(lane) = lane.filter(|_| changes_lane) {
            self.group_lanes.insert(group_id, lane);
        }
        self.structure_dirty = true;
        self.pending_group_reorder = Some((group_id, dest));
    }

    /// A press or touch at the cursor on a group's bar lifts that group as
    /// a drag candidate, unless another drag is already in flight.
    fn begin_group_drag(&mut self, touch: Option<u64>) {
        // Don't start a group drag while a tab-detach or resize drag is
        // already in flight, or one is already tracked.
        if self.group_drag.is_some()
            || self.edge_drag.is_some()
            || self.edge_grab_at(self.last_cursor).is_some()
            || self
                .groups
                .iter()
                .any(super::window_group::WindowGroup::drag_in_flight)
        {
            return;
        }
        // A finger may carry the window from anywhere on its tab bar,
        // tabs included: a tab is too small a target to have to avoid.
        let at = self.last_cursor;
        if let Some(idx) = self
            .groups
            .iter()
            .position(|g| g.bar_grab_hit(at) || (touch.is_some() && g.touch_grab_hit(at)))
            && let Some((group_id, held)) = self.groups.get(idx).map(|g| (g.id, g.is_held()))
        {
            self.group_drag = Some(GroupDrag {
                src_idx: idx,
                group_id,
                start: self.last_cursor,
                lifted_at: crate::time::now(),
                moved: false,
                touch,
                held,
            });
        }
    }

    /// The drag followed the cursor; whether it is past the threshold and
    /// so claims the motion.
    fn move_group_drag(&mut self) -> bool {
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
        if drag.moved && drag.held {
            // Pulled, never placed: there is no gap to drop it in.
            return true;
        }
        if drag.moved {
            let (target, lane) = self.group_insertion_for(self.last_cursor);
            self.group_reorder_target = Some(target);
            self.group_reorder_lane = lane;
            return true;
        }
        false
    }

    /// The drag was let go (`commit`) or cancelled; whether it had moved
    /// and so claims the release.
    fn end_group_drag(&mut self, commit: bool) -> bool {
        let Some(drag) = self.group_drag.take() else {
            return false;
        };
        let claimed = drag.moved;
        if drag.held {
            // Under reduced motion the held group is back in place at once.
            if drag.moved && !self.reduce_motion {
                let pull = resisted(self.last_cursor - drag.start);
                let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
                let back = crate::motion::Snap::SPEC.timing(
                    crate::motion::Direction::Enter,
                    motion,
                    MotionSource::Gesture,
                );
                let mut pulled = crate::motion::MotionValue::new(pull);
                pulled.set(vello::kurbo::Vec2::ZERO, back, crate::time::now());
                self.group_resist_back = Some((drag.group_id, pulled));
            }
            self.group_reorder_target = None;
            self.group_reorder_lane = None;
            return claimed;
        }
        let lane = self.group_reorder_lane.take();
        if commit
            && drag.moved
            && let Some(insert_at) = self.group_reorder_target.take()
        {
            self.commit_group_reorder(drag.src_idx, drag.group_id, insert_at, lane);
        }
        self.group_reorder_target = None;
        claimed
    }

    /// How far the group `id`, which someone else holds a window of, is
    /// pulled by a drag on it, or springs back after one.
    fn group_pull(&self, id: u64) -> Option<vello::kurbo::Vec2> {
        if let Some(drag) = self
            .group_drag
            .filter(|drag| drag.held && drag.moved && drag.group_id == id)
        {
            return Some(resisted(self.last_cursor - drag.start));
        }
        let (group, pulled) = self.group_resist_back?;
        let now = crate::motion::frame_now();
        (group == id && pulled.is_moving(now)).then(|| pulled.get(now))
    }

    /// A finger on the tab bar (pills or ground) may become a strip pan.
    fn begin_touch_pan_candidate(&mut self, id: u64, at: Point) {
        if self.touch_pan.is_none()
            && self
                .groups
                .iter()
                .any(|group| group.bar_rect().contains(at))
        {
            self.touch_pan = Some(TouchPan {
                id,
                start: at,
                engaged: false,
            });
        }
    }

    /// Display under `p` when this strip is partitioned; `None` on a
    /// single continuous row, so a same-row swipe is never a rehome.
    fn output_at(&self, p: Point) -> Option<u32> {
        let lane = self.lane_at(p)?;
        self.lanes.get(lane).map(|lane| lane.spec.output)
    }

    /// The finger left the display it started on. That is a group rehome,
    /// not a strip pan — #1833 only claimed same-display horizontal swipes.
    fn finger_crossed_display(&self, from: Point, to: Point) -> bool {
        match (self.output_at(from), self.output_at(to)) {
            (Some(from), Some(to)) => from != to,
            _ => false,
        }
    }

    /// Horizontal travel past the grab threshold pans 1:1 while the finger
    /// stays on the start display. Crossing onto another display, or a
    /// reorder that already claimed the finger, is left to the group drag.
    fn update_touch_pan(&mut self, id: u64, at: Point) -> bool {
        let Some(mut pan) = self.touch_pan.filter(|pan| pan.id == id) else {
            return false;
        };
        let travel = pan.start.x - at.x;
        let dy = at.y - pan.start.y;
        if self.finger_crossed_display(pan.start, at) {
            if pan.engaged {
                let _ = self.cancel_strip_pan(crate::time::now());
                pan.engaged = false;
                self.touch_pan = Some(pan);
            }
            return false;
        }
        if !pan.engaged {
            let held = self.group_drag.is_some_and(|drag| {
                drag.touch == Some(id)
                    && crate::time::now().saturating_duration_since(drag.lifted_at)
                        >= TOUCH_GRAB_HOLD
            });
            if held
                || self.group_drag.is_some_and(|drag| drag.moved)
                || travel.abs() <= GROUP_DRAG_THRESHOLD
                || travel.abs() <= dy.abs()
            {
                return false;
            }
            // Keep the group-drag candidate: a later step may still cross
            // onto another display and must be able to rehome. End of a
            // same-display pan drops that candidate without committing it.
            self.begin_strip_pan();
            pan.engaged = true;
        }
        let _ = self.update_strip_pan(travel, crate::time::now());
        self.touch_pan = Some(pan);
        true
    }

    /// Whether the pointer is over canvas no window covers.
    pub fn pointer_over_background(&self) -> bool {
        self.over_background(self.last_cursor)
    }

    /// Whether `p` is canvas no window, tab bar or resize seam covers.
    fn over_background(&self, p: Point) -> bool {
        self.rect.contains(p)
            && self.edge_grab_at(p).is_none()
            && !self
                .groups
                .iter()
                .enumerate()
                .any(|(i, g)| !self.layout_hides_group(i) && g.get_rect().contains(p))
    }

    /// Pan the strip with a mouse dragged across empty canvas. The press
    /// is never claimed, so a click on the background still does what it
    /// did; motion is claimed once it passes the drag threshold.
    fn update_pointer_pan(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.pointer_pan = (self.group_drag.is_none()
                    && !self.groups.iter().any(|g| g.maximized)
                    && self.over_background(self.last_cursor))
                .then_some(PointerPan {
                    start: self.last_cursor,
                    engaged: false,
                });
                false
            }
            WindowEvent::CursorMoved { .. } => {
                let Some(mut pan) = self.pointer_pan else {
                    return false;
                };
                let travel = pan.start.x - self.last_cursor.x;
                if !pan.engaged {
                    if travel.abs() <= GROUP_DRAG_THRESHOLD {
                        return false;
                    }
                    self.begin_strip_pan();
                    pan.engaged = true;
                }
                let _ = self.update_strip_pan(travel, crate::time::now());
                self.pointer_pan = Some(pan);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let Some(pan) = self.pointer_pan.take() else {
                    return false;
                };
                if pan.engaged {
                    let _ = self.end_strip_pan(crate::time::now());
                    self.hold_strip_pan();
                }
                pan.engaged
            }
            _ => false,
        }
    }

    fn end_touch_pan(&mut self, id: u64) -> bool {
        let Some(pan) = self.touch_pan.take() else {
            return false;
        };
        if pan.id != id {
            self.touch_pan = Some(pan);
            return false;
        }
        if !pan.engaged {
            return false;
        }
        let _ = self.end_strip_pan(crate::time::now());
        true
    }

    /// Returns `true` when the event is claimed by an in-flight group
    /// drag or a one-finger strip pan, so the caller skips resize-handle
    /// and per-child dispatch. A press records a *candidate* but does not
    /// claim it, so a tap still reaches the child for focus — only once
    /// past [`GROUP_DRAG_THRESHOLD`] does the row claim motion.
    fn update_group_drag(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        let pointer_drag = self.group_drag.is_none_or(|drag| drag.touch.is_none());
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.begin_group_drag(None);
                // Never claim the press — let it set focus on the group.
                false
            }
            WindowEvent::CursorMoved { .. } if pointer_drag => self.move_group_drag(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if pointer_drag => self.end_group_drag(true),
            // A finger on the tab bar pans the strip when the swipe stays
            // on this display; crossing onto another display, or a mostly
            // vertical grab, still reorders the group.
            WindowEvent::Touch(touch) => {
                let carries = self
                    .group_drag
                    .is_some_and(|drag| drag.touch == Some(touch.id));
                let ours = self.touch_pan.is_some_and(|pan| pan.id == touch.id);
                let at = Point::new(
                    touch.location.x / scale_factor,
                    touch.location.y / scale_factor,
                );
                match touch.phase {
                    winit::event::TouchPhase::Started => {
                        self.last_cursor = at;
                        self.begin_group_drag(Some(touch.id));
                        self.begin_touch_pan_candidate(touch.id, at);
                        false
                    }
                    winit::event::TouchPhase::Moved if ours || carries => {
                        self.last_cursor = at;
                        if self.update_touch_pan(touch.id, at) {
                            true
                        } else if carries {
                            self.move_group_drag()
                        } else {
                            false
                        }
                    }
                    winit::event::TouchPhase::Ended if ours || carries => {
                        self.last_cursor = at;
                        if self.end_touch_pan(touch.id) {
                            let _ = self.end_group_drag(false);
                            true
                        } else {
                            self.end_group_drag(true)
                        }
                    }
                    winit::event::TouchPhase::Cancelled if ours || carries => {
                        if self.end_touch_pan(touch.id) {
                            let _ = self.end_group_drag(false);
                            true
                        } else {
                            self.end_group_drag(false)
                        }
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Apply the exact target already previewed by this destination row.
    /// Returns `false` for a stale group id so the shell can restore the
    /// detached payload to its source without losing its live host.
    pub fn accept_cross_strip_drop(
        &mut self,
        new_id: u64,
        desc: TabDescriptor,
        host: TileHost,
        target: TabDropTarget,
    ) -> bool {
        match target {
            TabDropTarget::ExistingGroup {
                group_id,
                tab_index,
            } => {
                let Some(group) = self
                    .groups
                    .iter_mut()
                    .find(|group| group.id == group_id && !group.is_closing())
                else {
                    return false;
                };
                group.insert_detached_tab(tab_index, desc, host);
            }
            TabDropTarget::GroupGap { gap_index } => {
                let mut new_group =
                    WindowGroup::from_detached(new_id, desc, host, WindowWidth::Normal);
                new_group.start_spawn_animation();
                if let Some(lane) = self.lane_at(self.last_cursor) {
                    self.group_lanes.insert(new_id, lane);
                }
                self.groups
                    .insert(gap_index.min(self.groups.len()), new_group);
            }
        }
        self.cross_strip_drop_target = None;
        for group in &mut self.groups {
            group.set_external_drag_hover(None);
        }
        self.structure_dirty = true;
        true
    }

    /// Restore a detach whose durable cross-strip transaction was refused.
    /// The original group and position win; this path must be visually a no-op.
    pub fn restore_cross_strip_detach(&mut self, payload: CrossStripDetach) {
        if let Some(group) = self
            .groups
            .iter_mut()
            .find(|group| group.id == payload.source_group_id)
        {
            group.insert_detached_tab(payload.source_tab_index, payload.desc, payload.host);
        } else {
            let group = WindowGroup::from_detached(
                payload.source_group_id,
                payload.desc,
                payload.host,
                WindowWidth::Normal,
            );
            let at = payload.source_group_index.min(self.groups.len());
            self.groups.insert(at, group);
        }
        self.structure_dirty = true;
    }
}

impl FloatingSurfaceProvider for GroupRow {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        for group in &mut self.groups {
            FloatingSurfaceProvider::on_floating_surface_focus_changed(group, id, focused);
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        for group in &mut self.groups {
            FloatingSurfaceProvider::on_floating_surface_ime_source_changed(group, id);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        for group in &mut self.groups {
            FloatingSurfaceProvider::on_floating_surface_event_source(group, id);
        }
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        self.groups
            .iter()
            .find_map(|group| FloatingSurfaceProvider::floating_surface_accessibility(group, id))
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        self.groups.iter_mut().any(|group| {
            FloatingSurfaceProvider::perform_floating_surface_accessibility_action(
                group, id, target, action, state, path,
            )
        })
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, Rect)> {
        self.groups
            .iter()
            .find_map(|group| FloatingSurfaceProvider::floating_surface_ime(group, id))
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        self.groups
            .iter_mut()
            .find_map(FloatingSurfaceProvider::take_floating_surface_request)
    }

    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        self.groups
            .iter_mut()
            .find_map(FloatingSurfaceProvider::take_capture_studio_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.groups
            .iter_mut()
            .flat_map(FloatingSurfaceProvider::take_floating_surface_source_updates)
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
            FloatingSurfaceProvider::draw_floating_surfaces(
                group,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        self.groups.iter_mut().any(|group| {
            FloatingSurfaceProvider::draw_floating_surface(
                group,
                id,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            )
        })
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        for group in &mut self.groups {
            FloatingSurfaceProvider::set_floating_surface_bounds(group, bounds);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        self.groups
            .iter()
            .flat_map(FloatingSurfaceProvider::floating_surface_presentations)
            .collect()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(FloatingSurfaceProvider::active_floating_surface_ids)
            .collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for group in &mut self.groups {
            FloatingSurfaceProvider::on_floating_surface_host_message(group, message);
        }
    }
}

impl Widget for GroupRow {
    crate::provides!(
        dyn crate::widget::FloatingSurfaceProvider,
        dyn crate::collaboration::participants::CollaborationTopology,
        dyn crate::component_lifecycle::ComponentLifecycle
    );

    // Modal capture and presentation polling follow visible content, never inactive tabs.
    fn modal_capture(&self) -> Option<Rect> {
        self.children().into_iter().find_map(Widget::modal_capture)
    }
    /// A group the latest frame left unpainted (outside the row) had that
    /// frame for every deadline due by its sync. One still pending waits for
    /// the group's paint (a status pill that clears itself when drawn), which
    /// only showing the group gives, so it must not wake the host on every
    /// frame. Its later deadlines (an alarm, a poll) still wake it.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let served = |child: &dyn Widget, deadline: Instant| {
            self.unpainted_since.is_some_and(|since| deadline <= since)
                && self.groups.iter().any(|group| {
                    self.unpainted.contains(&group.id)
                        && std::ptr::addr_eq(std::ptr::from_ref::<dyn Widget>(group), child)
                })
        };
        self.children()
            .into_iter()
            .filter_map(|child| {
                child
                    .next_wake_deadline()
                    .filter(|&deadline| !served(child, deadline))
            })
            .min()
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        for child in self.groups.iter_mut().map(|group| group as &mut dyn Widget) {
            child.set_platform_conventions(conventions);
        }
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        for child in self.groups.iter_mut().map(|group| group as &mut dyn Widget) {
            child.set_interaction_environment(environment);
        }
    }

    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        for child in self.groups.iter_mut().map(|group| group as &mut dyn Widget) {
            child.mount_native_views(host);
        }
    }

    fn unmount_native_views(&mut self) {
        for child in self.groups.iter_mut().map(|group| group as &mut dyn Widget) {
            child.unmount_native_views();
        }
    }

    fn native_view_frame_start(&mut self) {
        for child in self.groups.iter_mut().map(|group| group as &mut dyn Widget) {
            child.native_view_frame_start();
        }
    }

    // Presentation traversal exposes only visible groups; resources/lifecycle retain all owners.
    fn children(&self) -> Vec<&dyn Widget> {
        if self.hidden {
            return vec![];
        }
        if let Some(index) = self
            .fullscreen_idx()
            .or_else(|| self.groups.iter().position(|g| g.maximized))
        {
            return vec![&self.groups[index]];
        }
        self.groups
            .iter()
            .map(|group| group as &dyn Widget)
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let shown = self.shown_group_indices();
        self.groups
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| shown.contains(index))
            .map(|(_, group)| group as &mut dyn Widget)
            .collect()
    }

    /// Each window's top layer, in [`Self::children_mut`]'s order as the
    /// default draws it, noting which windows put anything there
    /// (`Self::overlay_groups`).
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        fn drawn(scene: &Scene) -> (usize, usize) {
            let encoding = scene.encoding();
            (encoding.path_tags.len(), encoding.draw_tags.len())
        }
        self.overlay_groups.clear();
        for index in self.shown_group_indices() {
            let before = drawn(scene);
            let group = &mut self.groups[index];
            group.draw_overlay(scene, transform, text_renderer, theme, presence);
            if drawn(scene) != before {
                self.overlay_groups.push(group.id);
            }
        }
    }

    // Read-only resource discovery follows retained owners, including inactive tabs.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for group in &self.groups {
            visitor(group);
        }
    }

    /// The windows and the resize handles between them.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        self.for_each_child(visitor);
        for handle in &self.handles {
            visitor(handle);
        }
        true
    }

    #[allow(private_interfaces, reason = "toolkit-only checked custody traversal")]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        for child in &mut self.groups {
            child.visit_components(request)?;
        }
        Ok(())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        self.synced_at = Some(crate::time::now());
        for g in &mut self.groups {
            g.sync(state, root);
        }
    }

    fn update(&mut self, state: &AppModel, root: &ModelPath) -> bool {
        // A pan in motion (a throw, a settle, a reveal, fingers resampled at
        // the frame clock) advances here, once per host frame. Moving marks
        // the row `scroll_dirty`, a layout pass that re-applies the windows'
        // positions; it never rebuilds or re-measures the strip, which a
        // structural invalidation per animation frame used to do.
        if !self.hidden {
            // A close that has played out ends on the frame, not on the
            // next input event. One prune per layout keeps the slots in
            // step with taffy's nodes.
            if self.node_ids.len() == self.groups.len() {
                self.prune_closed_groups();
            }
            let now = crate::motion::frame_now();
            self.tick_snap_at(now);
            self.advance_drag_reach(now);
            self.advance_edge_autoscroll(now);
        }
        let mut any = false;
        for g in &mut self.groups {
            if g.update(state, root) {
                any = true;
            }
        }
        any
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.tokens.motion;
        self.sync_sibling_fade(theme);
        let usable_per_group = self.usable_per_group();
        let alone = self.alone_per_group();
        for ((g, &usable), alone) in self
            .groups
            .iter_mut()
            .zip(usable_per_group.iter())
            .zip(alone)
        {
            g.set_strip_usable_px(usable);
            g.set_alone(alone);
        }
        // Every slot springs from where it is drawn to where it goes, the
        // closing one to nothing and its neighbours into the room, whether or
        // not the strip overflows: the pan's bounds follow where the springs
        // are going ([`Self::dest_content_w`]), so a slot in flight never
        // makes the strip scrollable. Only a spring aimed at a width the
        // strip no longer has (the display changed under it) lands.
        for group in &mut self.groups {
            group.set_reduce_motion(theme.reduce_motion);
            group.set_motion_source(self.motion_source);
            if group.width_target_is_stale() && !group.is_closing() {
                group.snap_width_spring();
            } else {
                group.step_width_spring();
            }
        }
        self.node_ids.clear();
        if self.external_navigation && self.hidden {
            // Retained groups remain lifecycle owners, but none of their
            // History handles belong to this hidden row's one-node layout.
            for group in &mut self.groups {
                group.discard_layout_nodes();
            }
            let node = taffy.leaf_node(Style {
                display: Display::None,
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        // Resolve every group's width fraction before it builds its node
        // (walking left to right so each group's basis is the display its
        // left edge lands on), with no dependency on a previous frame's
        // layout — keeps a single-frame offscreen render identical to live.
        let usable_per_group = self.usable_per_group();
        let alone = self.alone_per_group();
        for ((g, &usable), alone) in self
            .groups
            .iter_mut()
            .zip(usable_per_group.iter())
            .zip(alone)
        {
            g.set_strip_usable_px(usable);
            g.set_alone(alone);
        }
        for group in &mut self.groups {
            group.set_fullscreen_presentation(
                self.fullscreen_group == Some(group.id)
                    && !self.promotion_keeps_gutter
                    && !self.promotion_keeps_controls,
            );
        }
        let group_nodes: Vec<NodeId> = self
            .groups
            .iter_mut()
            .map(|g| g.build_layout(taffy, theme, node_map))
            .collect();
        self.node_ids.clone_from(&group_nodes);
        self.laid_group_ids = self.groups.iter().map(|group| group.id).collect();
        let lane_of: Vec<usize> = (0..self.groups.len())
            .map(|i| self.lane_index_of(i))
            .collect();
        // The moment any child group is maximized, its run collapses to
        // that single group via `display: None` on siblings; handles are
        // skipped since there are no visible gaps. Multiple simultaneous
        // `maximized` flags in one run: first one wins. A transient
        // fullscreen promotion drives the same collapse and wins over
        // `maximized`. A partitioned strip collapses per lane, so a window
        // maximized on one display leaves the others as they were.
        let runs = self.lanes.len().max(1);
        let mut maximized_in: Vec<Option<usize>> = vec![None; runs];
        if let Some(fs) = self.fullscreen_idx()
            && let Some(slot) = maximized_in.get_mut(lane_of.get(fs).copied().unwrap_or(0))
        {
            *slot = Some(fs);
        }
        for (i, g) in self.groups.iter().enumerate() {
            if !g.maximized {
                continue;
            }
            if let Some(slot) = maximized_in.get_mut(lane_of.get(i).copied().unwrap_or(0))
                && slot.is_none()
            {
                *slot = Some(i);
            }
        }
        // Grow/truncate the seam pool to `max(0, N-1)`. A seam holds no
        // drag state (the row's `edge_drag` does), so its index may change.
        let desired_handles = if self.external_navigation {
            0
        } else {
            self.groups.len().saturating_sub(1)
        };
        if self.handles.len() < desired_handles {
            self.handles
                .extend((self.handles.len()..desired_handles).map(|_| ResizeHandle::new()));
        } else if self.handles.len() > desired_handles {
            self.handles.truncate(desired_handles);
        }
        // `flex_shrink: 0.0` on every group: Niri-style, placing a new
        // tile never resizes siblings; `content_width` grows past the
        // viewport and `draw` clips to it. `flex_shrink: 1.0` was tried
        // and rejected — it silently shrinks siblings on every spawn,
        // violating the infinite-strip invariant.
        let covered_lane = self.covered_lane();
        for (i, &node) in group_nodes.iter().enumerate() {
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.flex_shrink = 0.0;
            let lane = lane_of.get(i).copied().unwrap_or(0);
            if covered_lane == Some(lane) {
                style.display = Display::None;
            } else if let Some(max_i) = maximized_in.get(lane).copied().flatten() {
                if i == max_i {
                    // `maximized` already carries this grow style from
                    // `WindowGroup::build_layout`; a fullscreen-promoted
                    // group doesn't, so set it here too.
                    style.size.width = Dimension::auto();
                    style.flex_grow = 1.0e6;
                    style.flex_basis = Dimension::length(0.0);
                    if self.promotion_keeps_gutter
                        && self.fullscreen_group.is_some()
                        && self.fullscreen_idx() == Some(i)
                    {
                        style.margin.left =
                            LengthPercentageAuto::length(f32_of_f64(self.pan_of_lane(lane).inset));
                    }
                } else {
                    style.display = Display::None;
                }
            }
            let _ = taffy.set_style(node, style);
        }
        self.handle_node_ids.clear();
        let run_style = |display: Display| Style {
            display,
            flex_direction: FlexDirection::Row,
            // Groups past the viewport are the strip's to pan and clip, not
            // extent of what holds it: without this, taffy carries their
            // width up as every ancestor's content size and the desktop
            // column reports itself as wide as the whole strip, reaching
            // across and past every display. `Clip`, not `Hidden`: the
            // automatic minimum size, and so the layout, stays as it was.
            overflow: taffy::Point {
                x: Overflow::Clip,
                y: Overflow::Visible,
            },
            // Zero here: the inter-group `ResizeHandle` widgets supply the
            // visible 8-px footprint per gap, so a non-zero gap on top of
            // that would double the inter-group distance.
            gap: Size {
                width: taffy::LengthPercentage::length(0.0),
                height: taffy::LengthPercentage::length(0.0),
            },
            ..Default::default()
        };
        // A strip owning no part of the current fullscreen surface
        // collapses away entirely, so the promoted group in another strip
        // is the only thing painted.
        let row_display = if self.hidden || (self.covered_output.is_some() && self.lanes.is_empty())
        {
            Display::None
        } else {
            Display::Flex
        };
        let children: Vec<NodeId> = if self.lanes.is_empty() {
            // Interleave handle nodes between group nodes:
            // `[g0, h0, g1, h1, …, g_{N-1}]`. Omitted entirely while any
            // group is maximized (no visible gaps to mark).
            let maximized_idx = maximized_in.first().copied().flatten();
            let mut children: Vec<NodeId> =
                Vec::with_capacity(group_nodes.len().saturating_add(desired_handles));
            if maximized_idx.is_some() {
                children.extend(group_nodes.iter().copied());
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
            children
        } else {
            // One absolutely placed run per lane, over its display. Every
            // handle still gets a node so `handle_node_ids` stays aligned
            // with the pool; one between two lanes (or in a collapsed lane)
            // is parked out of layout.
            // Where the row rests: the deck's shift is a move, not layout.
            let row = self.rect - vello::kurbo::Vec2::new(0.0, self.deck_shift);
            let mut lane_children: Vec<Vec<NodeId>> = vec![Vec::new(); self.lanes.len()];
            let mut parked: Vec<NodeId> = Vec::new();
            for (i, gn) in group_nodes.iter().enumerate() {
                let lane = lane_of.get(i).copied().unwrap_or(0);
                if let Some(run) = lane_children.get_mut(lane) {
                    run.push(*gn);
                }
                if i.saturating_add(1) >= group_nodes.len() {
                    continue;
                }
                let Some(h) = self.handles.get_mut(i) else {
                    continue;
                };
                let hn = h.build_layout(taffy, theme, node_map);
                self.handle_node_ids.push(hn);
                let same_lane = lane_of.get(i.saturating_add(1)).copied() == Some(lane);
                let collapsed = maximized_in.get(lane).copied().flatten().is_some()
                    || covered_lane == Some(lane);
                if same_lane && !collapsed {
                    if let Some(run) = lane_children.get_mut(lane) {
                        run.push(hn);
                    }
                } else {
                    let mut style = taffy.style(hn).cloned().unwrap_or_default();
                    style.display = Display::None;
                    let _ = taffy.set_style(hn, style);
                    parked.push(hn);
                }
            }
            let mut children = Vec::with_capacity(self.lanes.len().saturating_add(parked.len()));
            for (lane, run) in self.lanes.iter_mut().zip(lane_children) {
                let viewport = lane.area();
                lane.built_viewport = viewport;
                let style = Style {
                    position: Position::Absolute,
                    inset: taffy::Rect {
                        left: LengthPercentageAuto::length(f32_of_f64(viewport.x0 - row.x0)),
                        right: LengthPercentageAuto::auto(),
                        top: LengthPercentageAuto::length(f32_of_f64(viewport.y0 - row.y0)),
                        bottom: LengthPercentageAuto::auto(),
                    },
                    size: Size {
                        width: Dimension::length(f32_of_f64(viewport.width())),
                        height: Dimension::length(f32_of_f64(viewport.height())),
                    },
                    ..run_style(Display::Flex)
                };
                children.push(taffy.parent_node(style, &run));
            }
            children.extend(parked);
            children
        };
        // Row must fill remaining vertical space so each child's
        // `height: percent(1.0)` resolves — otherwise the row measures
        // only as tall as the TabBar and `TileHost` collapses to zero.
        // Children beyond the viewport are still walked and shifted into
        // screen-space by the pan; still hit-testable since
        // `gap_index_for` uses each group's stored post-shift `Rect`.
        // Its height is the band it is given, never its windows' content: a
        // window's page scrolls inside it. With the automatic minimum, a
        // page's content height reached the row through each window (an
        // app's height there is a cross size, which its content sets), so a
        // long document raised every window in the strip past the screen.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: Size {
                width: Dimension::auto(),
                height: Dimension::length(0.0),
            },
            flex_grow: 1.0,
            ..run_style(row_display)
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if self.external_navigation && self.hidden {
            self.rect = Rect::ZERO;
            return;
        }
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        // Portable hosts learn their width here. Rebuild provisional widths
        // in the same bounded settlement, including a same-viewport root swap.
        self.structure_dirty |= !self.width_basis_is_current();
        if self.lanes.is_empty() {
            self.pan.viewport = self.rect;
            self.pan.inset = self.leading_inset;
        }
        for lane in &mut self.lanes {
            let area = lane.area();
            // Built against another area: lay out again so every lane's
            // height matches its band. The deck moving the row is no
            // such change.
            if (area.y0 - lane.built_viewport.y0).abs() > 0.5
                || (area.height() - lane.built_viewport.height()).abs() > 0.5
                || (area.x0 - lane.built_viewport.x0).abs() > 0.5
            {
                self.structure_dirty = true;
            }
            lane.pan.viewport = lane.viewport();
            lane.pan.inset = if lane.spec.leading {
                self.leading_inset
            } else {
                0.0
            };
        }
        let lane_of: Vec<usize> = (0..self.groups.len())
            .map(|i| self.lane_index_of(i))
            .collect();
        let runs = self.lanes.len().max(1);
        // Per run: canvas-x = inset + taffy-local x, where the run's taffy
        // parent sits at its viewport's left edge; `scroll_x` is how far
        // that's panned past it.
        let insets: Vec<f64> = (0..runs).map(|k| self.pan_of_lane(k).inset).collect();
        let mut content_w: Vec<f64> = insets.clone();
        let mut edges: Vec<Vec<f64>> = vec![Vec::new(); runs];
        // Only trustworthy when taffy has a node per group — a pass
        // landing between a splice and the rebuild would otherwise pair
        // widths with the wrong ids.
        let spans_aligned = self.node_ids.len() == self.groups.len();
        let mut spans: Vec<Vec<(u64, f64, f64)>> = vec![Vec::new(); runs];
        for (i, &cn) in self.node_ids.iter().enumerate() {
            let cl = taffy.layout(cn).copied().unwrap_or_default();
            let k = lane_of
                .get(i)
                .copied()
                .unwrap_or(0)
                .min(runs.saturating_sub(1));
            let inset = insets.get(k).copied().unwrap_or(0.0);
            let left = inset + f64::from(cl.location.x);
            let right = left + f64::from(cl.size.width);
            if let Some(e) = edges.get_mut(k) {
                e.push(left);
            }
            if spans_aligned
                && let Some(g) = self.groups.get(i)
                && let Some(s) = spans.get_mut(k)
            {
                s.push((g.id, left, right));
            }
            if let Some(w) = content_w.get_mut(k)
                && right > *w
            {
                *w = right;
            }
        }
        // An edge resize holds the pan itself (`group_row_edges`): no clamp
        // may move the strip under the pointer.
        let edge_resizing = self.edge_drag.is_some();
        let ease_reframe = self.shift_armed && !self.reduce_motion;
        let reframe = self.snap_timing(self.motion_source);
        let now = crate::motion::frame_now();
        let dest_w: Vec<f64> = (0..runs).map(|k| self.dest_content_w(k)).collect();
        let going: Vec<u64> = self
            .groups
            .iter()
            .filter(|g| g.is_closing())
            .map(|g| g.id)
            .collect();
        for (k, ((w, e), s)) in content_w.into_iter().zip(edges).zip(spans).enumerate() {
            let dest = dest_w.get(k).copied().unwrap_or(0.0);
            let pan = self.pan_of_lane_mut(k);
            // The strip the springs are going to, not where they are: a slot
            // in flight (a window opening beside another, a neighbour taking
            // the room a closed one left) can lay out wider than that for a
            // moment, and must not make the strip scrollable meanwhile.
            pan.content_w = if dest > pan.inset { dest } else { w };
            pan.group_left_edges = e;
            if spans_aligned && !edge_resizing {
                pan.hold_view_across_close(&s, &going);
            }
            let before = pan.scroll_x;
            // Preserve transient rubber-band travel while fingers are down
            // or a release settle is in flight. At rest, geometry changes
            // clamp to the deliberate trailing allowance instead of the old
            // three-viewports of unbounded empty canvas.
            // When the strip ends sooner (its last window closed, or the
            // screen widened), the pan stays within the windows: left in the
            // trailing allowance it would strand the view in empty canvas
            // nobody panned to, with windows cut off at the other edge. Only
            // a person's own pan there keeps it.
            if !edge_resizing && pan.strip_pan_origin.is_none() && !pan.animating() {
                let end = if pan.past_windows {
                    pan.pan_max_scroll_x()
                } else {
                    pan.max_scroll_x()
                };
                pan.scroll_x = pan.scroll_x.clamp(0.0, end);
                pan.past_windows &= pan.scroll_x > pan.max_scroll_x() + 0.5;
            }
            if spans_aligned {
                pan.note_laid_spans(s);
            }
            // An insert, close or reorder that moves the pan eases it there,
            // carrying every group together; the groups'
            // own moves are measured on screen against this eased pan.
            if ease_reframe
                && pan.strip_pan_origin.is_none()
                && !pan.animating()
                && (pan.scroll_x - before).abs() > 0.5
            {
                pan.slide_scroll_from(before, now, reframe);
            }
        }
        // Done AFTER the clamp so a structural mutation that snapped
        // scroll_x to max doesn't get re-eased.
        self.tick_snap_at(now);
        // A maximized or fullscreen-promoted group drops the leading gutter
        // and any prior pan, pinning it flush at its viewport's left edge.
        let collapsed: Vec<bool> = (0..runs)
            .map(|k| {
                self.groups.iter().enumerate().any(|(i, g)| {
                    lane_of.get(i).copied().unwrap_or(0) == k
                        && (g.maximized || self.fullscreen_group == Some(g.id))
                })
            })
            .collect();
        let origins: Vec<Point> = (0..runs)
            .map(|k| {
                let pan = self.pan_of_lane(k);
                let (inset, scroll) = if collapsed.get(k).copied().unwrap_or(false) {
                    (0.0, 0.0)
                } else {
                    (pan.inset, self.drawn_pan_x(pan))
                };
                Point::new(pan.viewport.x0 + inset - scroll, pan.viewport.y0)
            })
            .collect();
        let hide: Vec<bool> = (0..self.groups.len())
            .map(|i| self.layout_hides_group(i))
            .collect();
        for (i, (g, child_id)) in self.groups.iter_mut().zip(self.node_ids.iter()).enumerate() {
            // Display::None would zero the last tiled rect. Keep it so
            // the sibling fade can draw from the last presented frame
            // instead of ghosting at the origin.
            if hide.get(i).copied().unwrap_or(false) {
                continue;
            }
            let k = lane_of.get(i).copied().unwrap_or(0);
            let origin = origins.get(k).copied().unwrap_or(Point::ORIGIN);
            g.apply_layout(taffy, *child_id, origin);
        }
        // Measured on screen, after the pan: a close or insert re-frames
        // the pan in this same pass, and only the drawn position is what
        // must not jump. Only trusted when taffy has a node per group.
        if spans_aligned {
            self.record_shift_anim(now);
        }
        self.sync_handle_neighbours();
        self.recheck_edge_hover();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Drawings are replayed only into the frame right after the one
        // that drew or replayed them, and only while frames only pan: any
        // frame between may have changed a window.
        let frame = crate::retained_present::pan_frame();
        let consecutive = self
            .pan_drawn_frame
            .is_some_and(|last| last.wrapping_add(1) == frame);
        self.pan_drawn_frame = Some(frame);
        self.pan_kept.clear();
        self.replayed_groups.clear();
        let only_moved =
            crate::retained_present::pan_frame_kind() != crate::retained_present::PanFrame::Drawn;
        if !only_moved || !consecutive || self.pan_theme.as_ref() != Some(theme) {
            self.pan_drawings.clear();
            self.pan_unkept.clear();
            self.pan_theme = only_moved.then(|| theme.clone());
        }
        if self.drawn_apart {
            self.apart_transform = Some(transform);
            self.pan_drawings.clear();
            return;
        }
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.tokens.motion;
        self.sync_sibling_fade(theme);
        self.sibling_fade_drawn_moving = self.sibling_fade_in_flight();
        if self.external_navigation && self.hidden {
            self.pan_drawings.clear();
            return;
        }
        self.scroll_dirty = false;
        let lane_of: Vec<usize> = (0..self.groups.len())
            .map(|i| self.lane_index_of(i))
            .collect();
        let runs: Vec<(usize, Rect)> = if self.lanes.is_empty() {
            vec![(0, self.rect)]
        } else {
            self.lanes
                .iter()
                .enumerate()
                .map(|(k, lane)| (k, lane.pan.viewport))
                .collect()
        };
        for (k, viewport) in runs {
            if viewport.width() <= 0.0 || viewport.height() <= 0.0 {
                continue;
            }
            // Clip to the run's viewport: content routinely exceeds it, so
            // off-screen-right groups would bleed past the display edge.
            // 16px vertical pad keeps shadow/glow sprites (ambient_pad=50,
            // bloom_pad=120, see `TileHost::draw_steady_state`) intact at
            // top/bottom.
            // A retained strip draws the canvas, not the current pan. Layout
            // placed the lane at its exact pan; shifting
            // the rects back by the same amount gives canvas positions that
            // do not change while the pan does. They are put back afterwards,
            // so hit testing still sees screen positions. The canvas keeps
            // room for the rubber band left of the content, so the content
            // starts `pad` in and an overscroll pans like any other pan.
            let pan = self.pan_of_lane(k);
            let scroll = self.drawn_pan_x(pan);
            let max_scroll = pan.max_scroll_x();
            let pad = self.lane_canvas_pad(pan);
            let unbake = crate::retained_present::unbake_scroll()
                && pan_within_canvas(scroll, max_scroll, pad);
            let clip_right = if unbake {
                viewport.x1 + max_scroll + 2.0 * pad
            } else {
                viewport.x1
            };
            let shift = if unbake { scroll + pad } else { 0.0 };
            let shifted = shift.abs() > f64::EPSILON;
            // A strip reads the desktop where it last rested: the same
            // drawing through a pan (a presenter moves its frosting with
            // it), read again from the second frame it rests in screen
            // space, so the first such frame is still the one the pan's
            // layers already show.
            let rested = scroll + pad;
            let kept = if let Some((_, kept, was_screen, drawn)) = self
                .strip_desktop_shift
                .iter_mut()
                .find(|(lane, ..)| *lane == k)
            {
                if !unbake && *was_screen {
                    *kept = rested;
                }
                *was_screen = !unbake;
                *drawn = rested;
                *kept
            } else {
                self.strip_desktop_shift.push((k, rested, !unbake, rested));
                rested
            };
            let desktop_shift = if unbake { kept } else { kept - rested };
            self.drawing_desktop = Affine::translate((desktop_shift, 0.0));
            if shifted {
                self.offset_lane(k, shift);
            }
            // The stretch of the canvas this draw's pan shows: the
            // viewport, moved by the shift that took the groups there.
            let clip = Rect {
                x1: clip_right,
                ..Self::drawn_bounds(viewport)
            };
            self.apart_window =
                unbake.then(|| self.apart_window_of_lane(k, viewport, shift, (clip.y0, clip.y1)));
            let in_run: Vec<bool> = lane_of.iter().map(|&lane| lane == k).collect();
            // When a group is maximized, siblings stay out of layout so
            // they are not squashed; they still paint under a fade.
            let maximized_idx =
                self.fullscreen_idx()
                    .filter(|&i| in_run.get(i).copied().unwrap_or(false))
                    .or_else(|| {
                        self.groups.iter().enumerate().position(|(i, g)| {
                            g.maximized && in_run.get(i).copied().unwrap_or(false)
                        })
                    });
            // A maximized window at rest inside the viewport is the lane's
            // only drawing: the clip would cut nothing, and left out, the
            // window's client frame stays the last thing drawn, which a
            // display can scan out instead of compositing the frame.
            let clipped = !maximized_idx
                .and_then(|i| self.groups.get(i))
                .is_some_and(|group| !shifted && clip.contains_rect(group.drawn_rect()));
            if clipped {
                // A clipped lane is one damage part, there whichever of its
                // groups draw, bounded by the clip that bounds them: a group
                // the row leaves out (another surface draws it, or it is off
                // screen) and draws again changes only parts inside the
                // lane, which then damages the lane alone, not every
                // display.
                crate::scene_parts::begin(
                    scene,
                    crate::scene_parts::PartKey::new("ui.strip-lane", k),
                );
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &clip,
                );
            }
            self.draw_groups(
                scene,
                transform,
                tr,
                theme,
                presence,
                maximized_idx,
                &in_run,
            );
            self.draw_handles(
                scene,
                transform,
                tr,
                theme,
                presence,
                maximized_idx,
                &in_run,
            );
            if clipped {
                scene.pop_layer();
                crate::scene_parts::end(scene, vec![transform.transform_rect_bbox(clip)]);
            }
            if shifted {
                self.offset_lane(k, -shift);
            }
        }
        if self.apart_window.take().is_none() {
            // In screen space again: the canvas drawings are stale.
            self.apart_drawings.clear();
            self.apart_spans.clear();
        }
        // A window this frame did not draw or replay at rest may change
        // before it is shown again, and a frame that is not only a pan may
        // have changed any of them.
        let kept = &self.pan_kept;
        self.pan_drawings
            .retain(|drawing| kept.contains(&drawing.id));
        self.draw_group_drag_preview(scene, transform, tr, theme, presence);
        self.draw_reorder_capsule(scene, transform, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.external_navigation && self.hidden {
            return;
        }
        // Track the cursor so our post-dispatch detach sweep can hit-test
        // sibling bars even after a child has swallowed the event.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.refresh_tab_drop_preview();
        }

        // Runs before resize-handle/per-child dispatch so, once past
        // threshold, it claims the motion/release they'd otherwise
        // contend for. A press records a candidate without claiming, so
        // a plain click still reaches the child for focus.
        if self.update_pointer_pan(event) || self.update_group_drag(event, ctx.scale_factor) {
            *ctx.handled = true;
            return;
        }

        // A bare wheel NEVER moves the strip — that only happens via
        // `scroll_group_into_view_animated`, not this event path.

        // A clipped group's trailing `+` can overlap the invisible resize
        // seam at the viewport edge. The exact painted control owns both the
        // cursor update and the press; otherwise the seam keeps its usual
        // first refusal. Forwarding the move matters because button events
        // carry no coordinates and the child bar hit-tests its cursor cache.
        let hidden: Vec<bool> = (0..self.groups.len())
            .map(|i| self.layout_hides_group(i))
            .collect();
        let new_tab_owned = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) && self.groups.iter().enumerate().any(|(i, group)| {
            !hidden.get(i).copied().unwrap_or(false) && group.new_tab_hit(self.last_cursor)
        });
        let cursor = self.last_cursor;
        let direct_new_tab = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && self
            .groups
            .iter_mut()
            .enumerate()
            .find(|(i, group)| {
                !hidden.get(*i).copied().unwrap_or(false) && group.new_tab_hit(cursor)
            })
            .is_some_and(|(_, group)| group.request_new_tab_if_hit(cursor));
        if direct_new_tab {
            *ctx.handled = true;
        }
        if new_tab_owned && !direct_new_tab {
            self.dispatch_to_groups(event, ctx);
        }
        if !*ctx.handled {
            self.handle_edge_resize(event, ctx);
        }
        if !new_tab_owned && !*ctx.handled {
            self.dispatch_to_groups(event, ctx);
        }
        self.prune_closed_groups();
        self.resolve_tab_detaches(ctx.handled);
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) {
            self.clear_tab_drop_preview();
        }
    }

    fn name(&self) -> &'static str {
        "GroupRow"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};

        Some(AccessNode::new(Role::Group).with_name("Hosted applications"))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn wants_pointer_capture(&self) -> bool {
        self.pointer_pan.is_some_and(|pan| pan.engaged)
            || self.edge_drag.is_some()
            || self.groups.iter().any(super::Widget::wants_pointer_capture)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        // A window edge wins over what is under it (`edge_grab_at` is the
        // same test a press makes). Empty tab-bar space then advertises
        // whole-group dragging, instead of making that useful mouse path
        // discoverable by accident.
        if self.edge_drag.is_some() || self.edge_grab_at(point).is_some() {
            return Some(winit::window::CursorIcon::EwResize);
        }
        if self.group_drag.is_some() || self.pointer_pan.is_some_and(|pan| pan.engaged) {
            return Some(winit::window::CursorIcon::Grabbing);
        }
        if self
            .groups
            .iter()
            .enumerate()
            .any(|(i, group)| !self.layout_hides_group(i) && group.bar_grab_hit(point))
        {
            return Some(winit::window::CursorIcon::Grab);
        }
        self.groups
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.layout_hides_group(*i))
            .find_map(|(_, group)| group.cursor_shape(point))
    }

    /// Publish qualified affordances by group id, plus bare aliases for
    /// the leftmost group, so single-group scenarios stay unqualified.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut out = Vec::new();
        for (i, g) in self.groups.iter().enumerate() {
            if self.layout_hides_group(i) {
                continue;
            }
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
        for (i, g) in self.groups.iter().enumerate() {
            if self.layout_hides_group(i) {
                continue;
            }
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
        for (i, g) in self.groups.iter().enumerate() {
            if self.layout_hides_group(i) {
                continue;
            }
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
        if self.hidden {
            return false;
        }
        // A wheel-pan changes `scroll_x` without touching the row's rect
        // or any child's dirty bit, so without this the parent's cached
        // scene would keep painting pre-scroll positions.
        // The sibling fade changes what the row paints with no child dirty:
        // a parent's subtree cache would otherwise keep re-emitting the
        // frame the fade started in.
        if self.scroll_dirty || self.sibling_fade_needs_frame() {
            return true;
        }
        // A strip come to rest still shows the desktop where its pan began:
        // it reads it again on the next frame.
        if self
            .strip_desktop_shift
            .iter()
            .any(|(_, kept, was_screen, drawn)| *was_screen && (kept - drawn).abs() > f64::EPSILON)
        {
            return true;
        }
        // A change inside an app past the row's edges shows nothing until
        // the strip moves, and moving it redraws the row.
        self.groups
            .iter()
            .filter(|g| self.paints_within_row(g.get_rect(), 0.0))
            .any(super::Widget::needs_redraw)
            || self.handles.iter().any(super::Widget::needs_redraw)
    }

    fn mark_drawn(&mut self) {
        let culled = std::mem::take(&mut self.culled_groups);
        let replayed = self.replayed_groups.clone();
        for group in &mut self.groups {
            if culled.contains(&group.id) || replayed.contains(&group.id) {
                group.next_frame();
            } else {
                group.mark_drawn();
            }
        }
        for handle in &mut self.handles {
            handle.mark_drawn();
        }
        self.unpainted = culled;
        self.unpainted_since = self.synced_at;
    }

    fn needs_animation_frame(&self) -> bool {
        if self.hidden {
            return false;
        }
        // The row owns drag animations outside its presentation children;
        // propagate them alongside child animation requests. Off-screen
        // groups use the same cull as `needs_redraw`: a hosted app past
        // the strip edge must not keep the desktop at the refresh rate.
        self.snap_in_flight()
            || self.shift_in_flight()
            || self.drag_reach_pending()
            || self.edge_autoscroll_pending()
            || self.group_drag.is_some_and(|drag| drag.moved && !drag.held)
            || self
                .group_resist_back
                .is_some_and(|(id, _)| self.group_pull(id).is_some())
            || self.sibling_fade_needs_frame()
            || self
                .groups
                .iter()
                .filter(|g| self.paints_within_row(g.get_rect(), 0.0))
                .any(super::Widget::needs_animation_frame)
    }

    fn take_structure_dirty(&mut self) -> bool {
        // `fold` rather than `any`: must not short-circuit, every child
        // has to clear its own flag too.
        let own = std::mem::take(&mut self.structure_dirty);
        #[allow(
            clippy::unnecessary_fold,
            reason = "all child dirty flags must be drained without short-circuiting"
        )]
        let children = self
            .groups
            .iter_mut()
            .map(super::Widget::take_structure_dirty)
            .fold(false, |acc, d| acc || d);
        own || children
    }
}

impl crate::component_lifecycle::ComponentLifecycle for GroupRow {
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        for child in &mut self.groups {
            self.close_reports.extend(child.close_components());
        }
        self.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.close_reports);
        for child in &mut self.groups {
            reports.extend(child.take_component_close_reports());
        }
        reports
    }
}

impl crate::collaboration::participants::CollaborationTopology for GroupRow {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.groups
            .iter_mut()
            .map(|group| group as &mut dyn Widget)
            .collect()
    }
}

impl GroupRow {
    /// Records this frame's per-group local-x for the next `apply_layout`'s
    /// comparison, and — when `shift_armed` was set by an insert, close or
    /// reorder — springs whichever groups moved from where they are drawn
    /// at `now`. A group already travelling keeps its drawn offset and
    /// velocity, so a second press mid-flight bends the one motion rather
    /// than snapping. Split out of `apply_layout` to keep that under the
    /// line cap.
    fn record_shift_anim(&mut self, now: Instant) {
        let laid_x: Vec<(u64, f64)> = self
            .groups
            .iter()
            .map(|g| (g.id, g.get_rect().x0))
            .collect();
        let armed = std::mem::take(&mut self.shift_armed);
        let timing = self.move_timing(self.motion_source);
        if timing.is_instant() {
            self.land_moves();
        } else if armed {
            let moves: Vec<(u64, crate::motion::MotionValue<f64>)> = laid_x
                .iter()
                .filter_map(|&(id, laid)| {
                    let jump = self.last_laid_x.get(&id).map_or(0.0, |was| was - laid);
                    let (drawn, velocity) = self.move_state_at(id, now);
                    let from = drawn + jump;
                    // Sub-pixel motion is not motion.
                    if from.abs() <= 0.5 && velocity.abs() <= MOVE_SETTLED_PX_PER_S {
                        return None;
                    }
                    let mut offset = strip_motion(from);
                    offset.release(0.0, velocity, timing, now);
                    Some((id, offset))
                })
                .collect();
            self.land_moves();
            self.moves = moves;
            self.shift_hold = None;
        } else if self.shift_hold.is_none() {
            let groups: Vec<u64> = self.groups.iter().map(|g| g.id).collect();
            self.moves
                .retain(|(id, m)| groups.contains(id) && m.is_moving(now));
        }
        self.last_laid_x = laid_x.into_iter().collect();
    }

    /// Feeds each `ResizeHandle` the group rects flanking its gap, after
    /// every group has its own new layout applied. Split out of
    /// `apply_layout` to keep that under the line cap.
    fn sync_handle_neighbours(&mut self) {
        debug_assert_eq!(
            self.handles.len(),
            if self.external_navigation {
                0
            } else {
                self.groups.len().saturating_sub(1)
            },
            "ResizeHandle pool must mirror inter-group gap count after build_layout",
        );
        let lanes_match: Vec<bool> = (0..self.handles.len())
            .map(|i| self.lane_index_of(i) == self.lane_index_of(i.saturating_add(1)))
            .collect();
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
            if !lanes_match.get(i).copied().unwrap_or(true) {
                // Between two displays there is no gap to drag.
                h.set_neighbour_rects(Rect::ZERO, Rect::ZERO);
                continue;
            }
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
        in_run: &[bool],
    ) {
        self.culled_groups.clear();
        if let Some(max_i) = maximized_idx {
            // A maximized window covers its whole lane. While its frame
            // grows over the siblings they fade out under it, drawn first so
            // none of their bars, frames or controls shows over its toolbar
            // or content; at rest nothing else of the strip paints. They
            // fade back in on restore, on the tiled path below (`fade_hero`).
            let fade_alpha =
                crate::motion::unit_opacity(self.sibling_fade.progress(crate::motion::frame_now()));
            if fade_alpha > f32::EPSILON {
                for i in 0..self.groups.len() {
                    let sibling = &mut self.groups[i];
                    if i == max_i
                        || !in_run.get(i).copied().unwrap_or(false)
                        || self.shown_elsewhere.contains(&sibling.id)
                    {
                        continue;
                    }
                    let rect = sibling.get_rect();
                    scene.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        fade_alpha,
                        transform,
                        &rect.inflate(16.0, 16.0),
                    );
                    sibling.draw(scene, transform, tr, theme, presence);
                    scene.pop_layer();
                }
            }
            if let Some(id) = self.groups.get(max_i).map(|g| g.id) {
                if self.shown_elsewhere.contains(&id) {
                    self.culled_groups.push(id);
                } else {
                    self.draw_group_plain(max_i, scene, transform, (tr, theme, presence));
                }
            }
            return;
        }
        // A group an insert, close or reorder moved paints where it was
        // and springs to its new spot, so the strip is seen opening or
        // closing a gap rather than having always had one.
        let now = crate::motion::frame_now();
        let fade_alpha = crate::motion::unit_opacity(self.sibling_fade.progress(now));
        // Only the hero's own run fades: on a strip spread across displays
        // the others keep their windows.
        let fade_hero = self.fade_hero.filter(|&hero| {
            self.groups
                .iter()
                .position(|group| group.id == hero)
                .is_some_and(|i| in_run.get(i).copied().unwrap_or(false))
        });
        for i in 0..self.groups.len() {
            if !in_run.get(i).copied().unwrap_or(false) {
                continue;
            }
            let (id, rect) = (self.groups[i].id, self.groups[i].get_rect());
            let dx = self.move_state_at(id, now).0;
            // The row clip hides a group past its edges, but vello still
            // flattens, bins and tiles everything encoded under the clip:
            // a strip of dozens of apps would size the GPU's bump buffers
            // for all of them.
            if !self.paints_within_row(rect, dx) || self.shown_elsewhere.contains(&id) {
                self.culled_groups.push(id);
                continue;
            }
            let t = if dx.abs() > f64::EPSILON {
                shift_transform(transform, dx)
            } else {
                transform
            };
            let t = match self.group_pull(id) {
                Some(pull) => pull_transform(t, pull),
                None => t,
            };
            let source_is_lifted = self
                .group_drag
                .is_some_and(|drag| drag.moved && !drag.held && drag.group_id == id);
            let fading_sibling =
                fade_hero.is_some_and(|hero| hero != id) && fade_alpha < 1.0 - f32::EPSILON;
            if fading_sibling && fade_alpha <= f32::EPSILON {
                continue;
            }
            if fading_sibling {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    fade_alpha,
                    t,
                    &rect.inflate(16.0, 16.0),
                );
            }
            if source_is_lifted {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    0.18,
                    t,
                    &rect.inflate(160.0, 160.0),
                );
            }
            // Into the row's scene, not the group's drawing, so a replayed
            // drawing still stands on a strip whose blur is recorded. Only
            // where the screen shows the group: a strip drawn off screen
            // would spend a blurred region on nothing.
            // A damage part of its own: a strip that reads the desktop again
            // repaints only itself.
            match self.apart_window {
                // Only a group at rest in its slot: one moving, lifted or
                // fading is drawn every frame.
                Some(window)
                    if !source_is_lifted && !fading_sibling && dx.abs() <= f64::EPSILON =>
                {
                    self.draw_group_apart(i, window, scene, t, (tr, theme, presence));
                }
                // Only a group at rest in its slot, as above.
                None if crate::retained_present::pan_replay()
                    && !source_is_lifted
                    && !fading_sibling
                    && dx.abs() <= f64::EPSILON =>
                {
                    self.draw_group_replaying(i, scene, t, (tr, theme, presence));
                }
                _ => {
                    // A frame that only moved but draws every window keeps
                    // the drawings of the windows at rest it draws: nothing in
                    // them changed.
                    if crate::retained_present::pan_frame_kind()
                        == crate::retained_present::PanFrame::Kept
                        && dx.abs() <= f64::EPSILON
                        && self.group_at_rest(i, now)
                    {
                        self.pan_kept.push(id);
                    }
                    self.draw_group_plain(i, scene, t, (tr, theme, presence));
                }
            }
            if source_is_lifted {
                scene.pop_layer();
            }
            if fading_sibling {
                scene.pop_layer();
            }
        }
        if self.apart_window.is_some() {
            let groups = &self.groups;
            self.apart_drawings
                .retain(|drawing| groups.iter().any(|group| group.id == drawing.id));
        }
    }

    /// What of lane `k`'s canvas the screen shows, its `viewport` moved by
    /// `shift`, and whether that is another stretch than its last canvas
    /// draw showed (remembered for the next draw); `rows` is the lane's
    /// clip, top to bottom.
    fn apart_window_of_lane(
        &mut self,
        k: usize,
        viewport: Rect,
        shift: f64,
        rows: (f64, f64),
    ) -> ApartWindow {
        let span = (viewport.x0 + shift, viewport.x1 + shift);
        let bits = |(x0, x1): (f64, f64)| (x0.to_bits(), x1.to_bits());
        let last = self.apart_spans.iter_mut().find(|(lane, _)| *lane == k);
        let panning = last
            .as_ref()
            .is_some_and(|(_, last)| bits(*last) != bits(span));
        match last {
            Some((_, last)) => *last = span,
            None => self.apart_spans.push((k, span)),
        }
        ApartWindow {
            span,
            panning,
            rows,
        }
    }

    /// Draw group `i` of a row drawn in canvas coordinates, of which the
    /// screen shows `window`. While the strip pans, a group wholly off
    /// screen replays its last drawing when that was made at the same canvas
    /// rect and transform. On a frame that only moved laid-out content
    /// ([`crate::retained_present::PanFrame::only_moved`]), so does a group
    /// on screen that is at rest ([`Self::group_at_rest`]) and whose drawing
    /// is current and shows all the screen shows of it. Any other group is
    /// drawn, and its drawing kept. A strip at rest after any other frame
    /// draws every group, so each app presents as it would.
    ///
    /// No stale pixel of a group is shown: off screen nothing of it is
    /// (a presenter panning the strip shows only the canvas under the
    /// screen), and on screen it replays only a drawing made since anything
    /// last changed, at the canvas rect it still has, so drawing it again
    /// would encode the same. The strip's drawing stays byte for byte the
    /// same while the pan only moves it, which is what lets the presenter
    /// move its pixels instead of rasterising the strip. Like a culled
    /// group, a replayed one keeps its dirty state for its next real draw.
    fn draw_group_apart(
        &mut self,
        i: usize,
        window: ApartWindow,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) {
        let (id, rect) = (self.groups[i].id, self.groups[i].get_rect());
        let reach = rect.inflate(GROUP_SPRITE_REACH_PX, 0.0);
        let on_screen = reach.x1 > window.span.0 && reach.x0 < window.span.1;
        // The group is a damage part of the strip: a presenter that keeps
        // the strip's pixels rasterises only where a group's drawing changed
        // (a window off screen that changed while the strip rested: none of
        // the screen). Everything it paints stays inside its sprites' reach
        // and the lane's clip.
        let key = crate::scene_parts::PartKey::new("ui.strip-group", id);
        let bounds = vec![transform.transform_rect_bbox(Rect::new(
            reach.x0,
            window.rows.0,
            reach.x1,
            window.rows.1,
        ))];
        let kept = self
            .apart_drawings
            .iter()
            .position(|drawing| drawing.id == id);
        // On a frame that only moved laid-out content, a group at rest that
        // stays where it was drawn shows its drawing as drawing it again
        // would: nothing in it changed, and in canvas coordinates nothing of
        // it moved.
        let only_moved = crate::retained_present::pan_frame_kind().only_moved();
        let shows = seen_across(presence, rect);
        let at_rest = only_moved && on_screen && self.group_at_rest(i, crate::time::now());
        if ((window.panning && !on_screen) || only_moved)
            && let Some(drawing) = kept.and_then(|index| self.apart_drawings.get_mut(index))
            && drawing.rect == rect
            && drawing.transform == transform
            && (!on_screen || (at_rest && drawing.current && covers(drawing.seen, shows)))
        {
            drawing.current &= only_moved;
            crate::scene_parts::part(scene, key, bounds, |scene| {
                crate::scene_parts::append(
                    scene,
                    &drawing.scene,
                    drawing.parts.as_ref(),
                    Some(Affine::IDENTITY),
                );
            });
            if drawing.shows_secret {
                crate::secret_paint::note_secret_painted();
            }
            if on_screen {
                drawing.offer_picture();
                self.replayed_groups.push(id);
                crate::retained_present::note_replayed();
            } else {
                self.culled_groups.push(id);
            }
            return;
        }
        let local = kept
            .map(|index| self.apart_drawings.swap_remove(index).scene)
            .unwrap_or_default();
        let drawing = self.record_group(i, local, transform, (tr, theme, presence));
        let carried =
            crate::scene_parts::part(scene, key, bounds, |scene| drawing.append_to(scene));
        drawing.offer_picture(carried);
        if let Some(drawing) = drawing.keep(carried) {
            self.apart_drawings.push(drawing);
        }
    }

    /// Draw group `i` as on any frame; while a live picture of it is wanted
    /// ([`crate::window_pictures`]), through a drawing of its own, so the
    /// picture is the drawing the window just made.
    fn draw_group_plain(
        &mut self,
        i: usize,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) {
        if !crate::window_pictures::wanted(self.groups[i].id) {
            self.groups[i].draw(scene, transform, tr, theme, presence);
            return;
        }
        let drawing = self.record_group(i, Scene::new(), transform, (tr, theme, presence));
        let carried = drawing.append_to(scene);
        drawing.offer_picture(carried);
    }

    /// Draw group `i` on its own into `local`, a scene of its own that is
    /// nested so floating chrome drawn into it waits for it, and keep what
    /// a replay of the drawing needs.
    fn record_group(
        &mut self,
        i: usize,
        mut local: Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) -> GroupDrawing {
        local.reset();
        crate::backdrop::nest_scene(&local);
        let secrets = crate::secret_paint::SecretPaint::begin();
        let group = &mut self.groups[i];
        let (id, rect) = (group.id, group.get_rect());
        let ((), parts) = crate::scene_parts::record(&mut local, |local| {
            group.draw(local, transform, tr, theme, presence);
        });
        GroupDrawing(ApartDrawing {
            id,
            rect,
            transform,
            scene: local,
            parts,
            shows_secret: secrets.finish(),
            seen: seen_across(presence, rect),
            current: true,
        })
    }

    /// Whether group `i` is at rest for a frame that only moves laid-out
    /// content: not keyboard focused, nothing on the top layer last frame,
    /// and no animation frame, unfinished visual work or due wake asked for.
    /// Such a frame follows one in which nothing reached a widget, the
    /// document, focus or presence, so only time could change the window,
    /// and it asks for none. Its dirty flag is no sign either way: a widget
    /// that does not track one reports itself dirty on every frame.
    fn group_at_rest(&self, i: usize, now: Instant) -> bool {
        let group = &self.groups[i];
        !group.focused()
            && !self.overlay_groups.contains(&group.id)
            && !group.needs_animation_frame()
            && !group.has_pending_visual_work()
            && group
                .next_wake_deadline()
                .is_none_or(|deadline| deadline > now)
    }

    /// Draw group `i` of a strip drawn in screen space on a frame that only
    /// moves laid-out content ([`crate::retained_present::pan_replay`]).
    ///
    /// A window at rest ([`Self::group_at_rest`]) whose last drawing was made
    /// at its size, height and transform, in the frame just before, and
    /// culled none of what the screen shows of it now, replays that drawing
    /// moved by how far the strip panned since. Any other window at rest is
    /// drawn, culled as on any frame, and its drawing kept. A window not at
    /// rest, or whose drawing cannot be kept, is drawn as on any frame.
    ///
    /// What the window's widgets note while drawing still describes where it
    /// was drawn; the host draws again before input or a semantics read
    /// reaches them ([`crate::retained_present::PanReplay`]). Like a culled
    /// window, a replayed one keeps its dirty state for its next real draw.
    fn draw_group_replaying(
        &mut self,
        i: usize,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) {
        let (id, rect) = (self.groups[i].id, self.groups[i].get_rect());
        if !self.group_at_rest(i, crate::time::now()) || self.pan_unkept.contains(&id) {
            self.draw_group_plain(i, scene, transform, (tr, theme, presence));
            return;
        }
        let kept = self
            .pan_drawings
            .iter()
            .position(|drawing| drawing.id == id);
        self.pan_kept.push(id);
        // The drawing culled what its draw viewport did not show; the
        // screen may show no more of the window now.
        let shows = seen_across(presence, rect);
        if let Some(drawing) = kept.and_then(|index| self.pan_drawings.get(index))
            && drawing.transform == transform
            && drawing.rect.size() == rect.size()
            && drawing.rect.y0.to_bits() == rect.y0.to_bits()
            && covers(drawing.seen, shows)
        {
            // The drawing's layout moved by `dx`; in the scene that is the
            // same move through the draw transform.
            let dx = rect.x0 - drawing.rect.x0;
            let moved =
                Affine::translate(transform * Point::new(dx, 0.0) - transform * Point::ORIGIN);
            crate::scene_parts::append(scene, &drawing.scene, drawing.parts.as_ref(), Some(moved));
            if drawing.shows_secret {
                crate::secret_paint::note_secret_painted();
            }
            drawing.offer_picture();
            self.replayed_groups.push(id);
            crate::retained_present::note_replayed();
            return;
        }
        let local = kept
            .map(|index| self.pan_drawings.swap_remove(index).scene)
            .unwrap_or_default();
        // Culled as on any frame: a window the pan brings further on screen
        // is drawn again until it shows no more than a drawing of it saw.
        let drawing = self.record_group(i, local, transform, (tr, theme, presence));
        let carried = drawing.append_to(scene);
        drawing.offer_picture(carried);
        match drawing.keep(carried) {
            Some(drawing) => self.pan_drawings.push(drawing),
            None => self.pan_unkept.push(id),
        }
    }

    /// Paint the window group being reordered as the same kind of lifted,
    /// translucent object as a carried tab: the pointer keeps its original
    /// grab offset, content settles slightly smaller beneath the handle, and
    /// the source remains as a dim vacancy. Hit-testing and layout stay at the
    /// source geometry until release.
    fn draw_group_drag_preview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(lift) = self.group_drag_lift(theme) else {
            return;
        };
        let Some(drag) = self.group_drag else {
            return;
        };
        let Some(group) = self
            .groups
            .iter_mut()
            .find(|group| group.id == drag.group_id)
        else {
            return;
        };
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine multiplication composes finite floating-point transforms"
        )]
        let preview_transform = transform * lift;
        group.draw_group_drag_visual(scene, preview_transform, tr, theme, presence);
    }

    /// Where the window being reordered is drawn, in the row's space: the
    /// point grabbed stays under the pointer, 1:1 and on the frame the
    /// pointer moved, while the window settles slightly smaller around that
    /// point. Scaling about anywhere else would slide the grabbed point
    /// away from the pointer by a few percent of its distance to there,
    /// which on a wide window is tens of pixels.
    fn group_drag_lift(&self, theme: &Theme) -> Option<Affine> {
        let drag = self.group_drag.filter(|drag| drag.moved && !drag.held)?;
        let progress = if theme.reduce_motion {
            1.0
        } else {
            // Lifted as a picked-up row is (`motion::ListChange::FADE`).
            crate::motion::ListChange::FADE.progress_after(
                crate::motion::Direction::Enter,
                theme.motion(),
                crate::tokens::MotionSource::Pointer,
                crate::time::now().saturating_duration_since(drag.lifted_at),
            )
        };
        let scale = 1.0 - 0.04 * progress;
        let (grab, at) = (drag.start, self.last_cursor);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine multiplication composes finite floating-point transforms"
        )]
        let lift = Affine::translate((at.x, at.y))
            * Affine::scale(scale)
            * Affine::translate((-grab.x, -grab.y));
        Some(lift)
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
        in_run: &[bool],
    ) {
        let any_drag = self
            .groups
            .iter()
            .any(super::window_group::WindowGroup::drag_in_flight);
        if any_drag || maximized_idx.is_some() {
            return;
        }
        let now = crate::motion::frame_now();
        let offsets: Vec<f64> = self
            .groups
            .iter()
            .map(|g| self.move_state_at(g.id, now).0)
            .collect();
        for (i, h) in self.handles.iter_mut().enumerate() {
            // A group with nothing on screen has no border to be one
            // side of. A window whose content is on its way counts as
            // shown: its place is reserved, with real edges at its width.
            let shown = |g: Option<&WindowGroup>| g.is_some_and(WindowGroup::stands);
            let next_i = i.saturating_add(1);
            // Only a gap between two groups of this run; the seam between
            // two displays is not a gap.
            if !in_run.get(i).copied().unwrap_or(false)
                || !in_run.get(next_i).copied().unwrap_or(false)
            {
                continue;
            }
            if !shown(self.groups.get(i)) || !shown(self.groups.get(next_i)) {
                continue;
            }
            let dx = offsets.get(next_i).copied().unwrap_or(0.0);
            let t = if dx.abs() > f64::EPSILON {
                shift_transform(transform, dx)
            } else {
                transform
            };
            h.draw(scene, t, tr, theme, presence);
        }
    }

    /// One full-height group-gap indicator shared by whole-group reorder and
    /// tab splitting. Sharing this geometry is what makes an edge-band target
    /// and a literal seam communicate the same resulting structure.
    fn draw_reorder_capsule(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let ins = self
            .group_drag
            .filter(|drag| drag.moved)
            .and(self.group_reorder_target)
            .or(match self.tab_drop_target {
                Some(TabDropTarget::GroupGap { gap_index }) => Some(gap_index),
                _ => None,
            })
            .or(match self.cross_strip_drop_target {
                Some(TabDropTarget::GroupGap { gap_index }) => Some(gap_index),
                _ => None,
            });
        let Some(ins) = ins else {
            return;
        };
        if self.groups.is_empty() {
            return;
        }
        // At either end the capsule sits where the next inter-group gap
        // would open — half a handle slot outside the outermost card — so
        // an end insertion is marked at the same distance from its
        // neighbour as a middle one. On a partitioned strip the ends are
        // those of the lane the insertion lands in.
        let end_offset = HANDLE_SLOT_PX / 2.0;
        let (run, band) = match self.lane_at(self.last_cursor) {
            Some(lane) => (self.lane_range(lane), self.pan_of_lane(lane).viewport),
            None => (0..self.groups.len(), self.rect),
        };
        let gap_x = if run.is_empty() {
            band.x0
                + self
                    .pan_of_lane(self.lane_at(self.last_cursor).unwrap_or(0))
                    .inset
        } else if ins <= run.start {
            self.groups
                .get(run.start)
                .map_or(0.0, |g| g.get_rect().x0 - end_offset)
        } else if ins >= run.end {
            self.groups
                .get(run.end.saturating_sub(1))
                .map_or(0.0, |g| g.get_rect().x1 + end_offset)
        } else {
            let left = self
                .groups
                .get(ins.saturating_sub(1))
                .map(WindowGroup::get_rect);
            let right = self.groups.get(ins).map(WindowGroup::get_rect);
            left.zip(right)
                .map_or(0.0, |(l, r)| f64::midpoint(l.x1, r.x0))
        };
        let y0 = band.y0;
        let y1 = band.y1;
        if y1 <= y0 {
            return;
        }
        // Twice the idle handle rule, so the insertion mark reads as the
        // gap "opening" rather than as another handle.
        let thickness = 2.0 * super::resize_handle::HANDLE_VISIBLE_PX;
        let capsule = RoundedRect::from_rect(
            Rect::new(gap_x - thickness / 2.0, y0, gap_x + thickness / 2.0, y1),
            thickness / 2.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.intent.accent_ink,
            None,
            &capsule,
        );
    }

    /// Stop dispatch once a child claims the event, but the detach sweep
    /// after this still runs regardless — that claim is for
    /// pointer-capture/focus, a separate concern. `groups` can grow
    /// mid-frame (detach sweep pushes a new `WindowGroup`), briefly
    /// outrunning `node_ids`; `.get` skips the new one until its next
    /// layout pass assigns a `node_id`.
    fn dispatch_to_groups(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Mouse buttons and wheels carry no position, so dispatch the group that owns the
        // remembered cursor (or an active capture) before siblings. Keep every
        // sibling in the fallback order: synthetic and keyboard-adjacent test
        // paths can legitimately lack a preceding cursor move.
        // Edge grips first, for every group: they overlay the windows' own
        // content, so no group's tile may swallow the pointer before the group
        // under it has seen it.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) {
            for i in 0..self.groups.len() {
                if self.layout_hides_group(i) {
                    continue;
                }
                if self.groups[i].handle_edge_input(event, ctx.scale_factor, *ctx.modifiers) {
                    *ctx.handled = true;
                    return;
                }
            }
        }
        let wheel = matches!(event, WindowEvent::MouseWheel { .. });
        let preferred = (wheel || matches!(event, WindowEvent::MouseInput { .. })).then(|| {
            self.groups
                .iter()
                .enumerate()
                .position(|(i, group)| {
                    !self.layout_hides_group(i) && group.new_tab_hit(self.last_cursor)
                })
                .or_else(|| {
                    self.groups.iter().enumerate().position(|(i, group)| {
                        !self.layout_hides_group(i) && group.wants_pointer_capture()
                    })
                })
                .or_else(|| {
                    self.groups.iter().enumerate().position(|(i, group)| {
                        !self.layout_hides_group(i) && group.get_rect().contains(self.last_cursor)
                    })
                })
        });
        let preferred = preferred.flatten();
        // Keys are the window's with the keyboard: the one holding the
        // focused control, else the focused window. A window beside it is
        // not handed them, since its app reads every key it gets as its own
        // (a letter would end in its shortcut or field instead).
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            let first_shown = |test: fn(&WindowGroup) -> bool| {
                self.groups
                    .iter()
                    .enumerate()
                    .position(|(i, group)| !self.layout_hides_group(i) && test(group))
            };
            let owner = first_shown(|group| super::context_menu_area::focus_within(group))
                .or_else(|| first_shown(WindowGroup::focused));
            if let Some(i) = owner
                && let Some(&node_id) = self.node_ids.get(i)
                && let Some(group) = self.groups.get_mut(i)
            {
                group.handle_event(event, &mut ctx.reborrow(ctx.path, node_id));
                return;
            }
        }
        let mut order = Vec::with_capacity(self.groups.len());
        if let Some(i) = preferred {
            order.push(i);
        }
        // A wheel is the window's under the pointer alone: one beside it
        // would scroll what the person is not pointing at (a chat's
        // messages take any wheel they are handed), and before this window
        // had its turn.
        if !(wheel && preferred.is_some()) {
            order.extend(
                (0..self.groups.len())
                    .filter(|i| Some(*i) != preferred && !self.layout_hides_group(*i)),
            );
        }
        for i in order {
            let Some(g) = self.groups.get_mut(i) else {
                continue;
            };
            let Some(&node_id) = self.node_ids.get(i) else {
                continue;
            };
            g.handle_event(event, &mut ctx.reborrow(ctx.path, node_id));
            if *ctx.handled {
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
        prune.compact_sort_by(|a, b| b.cmp(a));
        for i in prune {
            let mut group = self.groups.remove(i);
            self.close_reports
                .extend(crate::component_lifecycle::close_components(&mut group));
        }
        // What is left of the closed window, its last sliver and its seam,
        // goes with a slide like any other removal.
        self.shift_armed = true;
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
        let mut detaches: Vec<(usize, u64, Point)> = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if let Some((tab_id, release_point)) = g.take_tab_detach() {
                detaches.push((i, tab_id, release_point));
            }
        }

        let mut diverted: Option<usize> = None;
        for (idx, &(src_idx, tab_id, release_point)) in detaches.iter().enumerate() {
            let out_of_band = !self.covers(release_point);
            if !out_of_band {
                continue;
            }
            let Some(src_group) = self.groups.get_mut(src_idx) else {
                continue;
            };
            let source_tab_index = src_group
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)
                .unwrap_or_else(|| src_group.tab_count());
            if let Some((desc, host)) = src_group.detach_tab(tab_id) {
                self.pending_cross_strip_detach = Some(CrossStripDetach {
                    tab_id,
                    source_group_id: src_group.id,
                    source_group_index: src_idx,
                    source_tab_index,
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

        for (src_idx, tab_id, _release_point) in detaches {
            self.resolve_one_tab_detach(src_idx, tab_id, self.tab_drop_target, handled);
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
        target: Option<TabDropTarget>,
        handled: &mut bool,
    ) {
        let committed = match target {
            Some(TabDropTarget::ExistingGroup {
                group_id,
                tab_index,
            }) => self.move_tab_into_group(src_idx, tab_id, group_id, tab_index),
            // Deliberately does NOT refuse a single-tab source dropped into
            // an adjacent gap: replacing its now-empty source with the new
            // group is still the faithful result of the gesture.
            Some(TabDropTarget::GroupGap { gap_index }) => {
                self.pending_split_lane = self.lane_at(self.last_cursor);
                self.split_tab_into_new_group(src_idx, tab_id, gap_index)
            }
            None => false,
        };
        if committed {
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

    /// Apply an [`TabDropTarget::ExistingGroup`] without resolving geometry
    /// again. Same-group body drops become ordinary first/last reorders;
    /// sibling drops transfer the descriptor and live host together.
    fn move_tab_into_group(
        &mut self,
        src_idx: usize,
        tab_id: u64,
        target_group_id: u64,
        tab_index: usize,
    ) -> bool {
        let Some(dst_idx) = self
            .groups
            .iter()
            .position(|group| group.id == target_group_id && !group.is_closing())
        else {
            return false;
        };
        if src_idx == dst_idx {
            let Some(group) = self.groups.get_mut(src_idx) else {
                return false;
            };
            let Some(from) = group.tabs().iter().position(|tab| tab.id == tab_id) else {
                return false;
            };
            let to = if tab_index > from {
                tab_index.saturating_sub(1)
            } else {
                tab_index
            }
            .min(group.tab_count().saturating_sub(1));
            if to == from || !group.reorder_tab(from, to) {
                return false;
            }
            self.pending_topology_change = true;
            return true;
        }
        self.transfer_tab(src_idx, dst_idx, tab_id, tab_index)
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

#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication is floating-point matrix composition; it cannot panic or overflow"
)]
fn pull_transform(base: Affine, pull: vello::kurbo::Vec2) -> Affine {
    base * Affine::translate(pull)
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
    use crate::testing::Dispatch;
    use crate::widget::{
        EdgeGrab, TabDescriptor, TileContent, WindowEdge, WindowWidth, min_group_px,
    };

    /// `t` minus `ms` milliseconds, saturating rather than panicking in
    /// the astronomically unlikely case the subtraction would underflow
    /// `Instant`'s valid range.
    fn earlier(t: Instant, ms: u64) -> Instant {
        t.checked_sub(Duration::from_millis(ms)).unwrap_or(t)
    }

    pub(super) fn make_group(id: u64, tab_ids: &[u64]) -> WindowGroup {
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
    fn transfer_tab_moves_between_groups_and_ignores_same_or_unknown() {
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

        fn transfer_tab_is_noop_for_same_group() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[3]);
            let mut row = GroupRow::new(vec![g0, g1]);
            assert!(!row.transfer_tab(0, 0, 1, 0));
        }

        fn transfer_tab_is_noop_for_unknown_tab() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[3]);
            let mut row = GroupRow::new(vec![g0, g1]);
            assert!(!row.transfer_tab(0, 1, 999, 0));
        }

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

        fn active_tab_address_must_exist_even_when_the_bar_is_stale() {
            let stale = WindowGroup::new(
                100,
                vec![(
                    TabDescriptor::new(2, "surviving"),
                    TileContent::Stub {
                        kind: crate::widget::AppKind::Settings,
                        label: "surviving".into(),
                    },
                )],
                1,
                WindowWidth::Normal,
            );
            let mut row = GroupRow::new(vec![stale]);

            assert!(
                !row.set_group_active_tab(100, 1),
                "an active id left behind by a close is not a valid focus target"
            );
        }

        transfer_tab_moves_descriptor_and_host_between_groups();
        transfer_tab_is_noop_for_same_group();
        transfer_tab_is_noop_for_unknown_tab();
        transfer_active_tab_promotes_sibling_in_source_and_activates_in_dest();
        active_tab_address_must_exist_even_when_the_bar_is_stale();
    }

    #[test]
    fn split_tab_into_new_group_inserts_at_gap_and_prunes_source() {
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

        fn split_tab_into_new_group_noop_for_unknown_tab() {
            let g0 = make_group(100, &[1, 2]);
            let g1 = make_group(200, &[3]);
            let mut row = GroupRow::new(vec![g0, g1]);
            assert!(!row.split_tab_into_new_group(0, 999, 0));
            assert_eq!(row.groups.len(), 2);
            let g0_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
            assert_eq!(g0_ids, vec![1, 2]);
        }

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

        split_tab_into_new_group_appends_with_empty_source_pruned();
        split_tab_into_new_group_inserts_between_existing_groups();
        split_tab_into_new_group_flags_structure_dirty_once();
        split_tab_into_new_group_noop_for_unknown_tab();
        split_tab_into_new_group_prepends_when_gap_zero();
    }

    #[test]
    fn tab_drop_targets_classify_gaps_and_hold_hysteresis() {
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

        fn unified_tab_targets_cover_normal_and_compact_windows() {
            assert_unified_tab_targets_at_width(FIXTURE_USABLE_PX);
            assert_unified_tab_targets_at_width(480.0);
        }

        fn tab_target_hysteresis_retains_preview_then_crosses_cleanly() {
            let mut row = make_row_with_groups_on_display(&[&[1, 2], &[10, 11]], 1200.0, 2000.0).0;
            let rect = row.groups[1].get_rect();
            let body_y = f64::midpoint(row.groups[1].bar_rect().y1, rect.y1);
            let boundary = rect.x0 + tab_group_edge_width(rect.width());
            row.tab_drop_target = Some(TabDropTarget::GroupGap { gap_index: 1 });

            assert_eq!(
                row.tab_drop_target_with_hysteresis(Point::new(boundary + 2.0, body_y)),
                row.tab_drop_target,
                "two pixels of boundary jitter must retain the indicator the user saw",
            );
            assert!(matches!(
                row.tab_drop_target_with_hysteresis(Point::new(
                    boundary + TAB_DROP_HYSTERESIS_PX + 2.0,
                    body_y,
                )),
                Some(TabDropTarget::ExistingGroup { tab_index: 0, .. })
            ));
        }

        fn release_commits_the_previewed_target_without_resolving_again() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
            let target_id = row.groups[1].id;
            let misleading_release = Point::new(
                row.groups[1].get_rect().x1 - 1.0,
                row.groups[1].get_rect().center().y,
            );
            assert!(matches!(
                row.tab_drop_target_at(misleading_release),
                Some(TabDropTarget::GroupGap { .. })
            ));
            row.tab_drop_target = Some(TabDropTarget::ExistingGroup {
                group_id: target_id,
                tab_index: 0,
            });
            row.groups[0].bar_mut().pending_detach = Some((1, misleading_release));

            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            assert_eq!(
                row.groups.len(),
                2,
                "release must not create an unpreviewed group"
            );
            assert_eq!(
                row.groups[1]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 10, 11],
            );
            assert_eq!(
                row.tab_drop_target(),
                None,
                "release clears the consumed preview"
            );
        }

        gap_index_for_classifies_inter_group_drops();
        unified_tab_targets_cover_normal_and_compact_windows();
        tab_target_hysteresis_retains_preview_then_crosses_cleanly();
        release_commits_the_previewed_target_without_resolving_again();
    }

    fn assert_unified_tab_targets_at_width(usable: f64) {
        let row = make_row_with_groups_on_display(&[&[1, 2], &[10, 11, 12]], usable, 2000.0).0;
        let group = &row.groups[1];
        let rect = group.get_rect();
        let bar = group.bar_rect();
        let body_y = f64::midpoint(bar.y1, rect.y1);
        let edge = tab_group_edge_width(rect.width());
        assert!(
            edge * 2.0 <= rect.width() * (TAB_GROUP_EDGE_MAX_FRAC * 2.0 + f64::EPSILON),
            "edge targets must leave at least 36% of a compact body for tab insertion"
        );

        assert_eq!(
            row.tab_drop_target_at(Point::new(rect.x0 + edge - 1.0, body_y)),
            Some(TabDropTarget::GroupGap { gap_index: 1 }),
            "the full left border band creates a group before the window (width {})",
            rect.width(),
        );
        assert_eq!(
            row.tab_drop_target_at(Point::new(rect.x0 + edge, body_y)),
            Some(TabDropTarget::ExistingGroup {
                group_id: group.id,
                tab_index: 0,
            }),
            "the border's inner boundary belongs continuously to the left interior",
        );
        assert_eq!(
            row.tab_drop_target_at(Point::new(rect.center().x - 1.0, body_y)),
            Some(TabDropTarget::ExistingGroup {
                group_id: group.id,
                tab_index: 0,
            }),
        );
        assert_eq!(
            row.tab_drop_target_at(Point::new(rect.center().x, body_y)),
            Some(TabDropTarget::ExistingGroup {
                group_id: group.id,
                tab_index: group.tabs().len(),
            }),
            "the exact centre starts the append half",
        );
        assert_eq!(
            row.tab_drop_target_at(Point::new(rect.x1 - edge, body_y)),
            Some(TabDropTarget::ExistingGroup {
                group_id: group.id,
                tab_index: group.tabs().len(),
            }),
            "the border's inner boundary still belongs to the right interior",
        );
        assert_eq!(
            row.tab_drop_target_at(Point::new(rect.x1 - edge + 1.0, body_y)),
            Some(TabDropTarget::GroupGap { gap_index: 2 }),
            "the full right border band creates a group after the window",
        );

        let pills = group.tab_rects();
        let between_first_two = f64::midpoint(pills[0].x1, pills[1].x0);
        assert_eq!(
            row.tab_drop_target_at(Point::new(between_first_two, bar.center().y)),
            Some(TabDropTarget::ExistingGroup {
                group_id: group.id,
                tab_index: 1,
            }),
            "the top bar keeps exact pill-gap insertion instead of body zones",
        );
    }

    #[test]
    fn cross_strip_moves_accept_exact_targets_and_refuse_stale_ones() {
        fn cross_strip_accept_consumes_exact_existing_group_target() {
            let mut row = make_row_with_groups_on_display(&[&[1, 2]], 1200.0, 2000.0).0;
            let mut source = make_group(900, &[9]);
            let (desc, host) = source.detach_tab(9).expect("detached source tab");
            let target = TabDropTarget::ExistingGroup {
                group_id: row.groups[0].id,
                tab_index: 1,
            };
            row.cross_strip_drop_target = Some(target);

            assert!(row.accept_cross_strip_drop(901, desc, host, target));
            assert_eq!(
                row.groups[0]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 9, 2]
            );
            assert_eq!(row.cross_strip_drop_target(), None);
        }

        fn stale_cross_strip_existing_group_target_is_refused_without_mutation() {
            let mut row = make_row_with_groups_on_display(&[&[1, 2]], 1200.0, 2000.0).0;
            let mut source = make_group(900, &[9]);
            let (desc, host) = source.detach_tab(9).expect("detached source tab");

            assert!(!row.accept_cross_strip_drop(
                901,
                desc,
                host,
                TabDropTarget::ExistingGroup {
                    group_id: u64::MAX,
                    tab_index: 0,
                },
            ));
            assert_eq!(row.groups.len(), 1);
            assert_eq!(
                row.groups[0]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
        }

        fn refused_cross_strip_move_restores_exact_group_and_tab_order() {
            let mut row = make_row_with_groups(&[&[1, 2, 3], &[10]]);
            let source_group_index = 0;
            let source_group_id = row.groups[source_group_index].id;
            let source_tab_index = 1;
            let (desc, host) = row.groups[source_group_index]
                .detach_tab(2)
                .expect("fixture tab detaches");
            row.restore_cross_strip_detach(CrossStripDetach {
                tab_id: 2,
                source_group_id,
                source_group_index,
                source_tab_index,
                desc,
                host,
                release_point: Point::new(900.0, 900.0),
            });

            assert_eq!(row.group_ids(), vec![100, 200]);
            assert_eq!(
                row.groups[0]
                    .tabs()
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                vec![1, 2, 3],
            );
        }

        cross_strip_accept_consumes_exact_existing_group_target();
        stale_cross_strip_existing_group_target_is_refused_without_mutation();
        refused_cross_strip_move_restores_exact_group_and_tab_order();
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

    pub(super) fn make_row_with_groups_with_taffy(
        tab_specs: &[&[u64]],
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        make_row_with_groups_on_display(tab_specs, FIXTURE_USABLE_PX, 2000.0)
    }

    pub(super) fn relayout_row_owned(
        row: GroupRow,
        viewport_w: f64,
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        relayout_row_owned_with(row, viewport_w, &Theme::dark())
    }

    /// Closing the last window with the strip at its end leaves the windows
    /// that remain in view: the pan comes to rest at the new end of the
    /// windows, not in the empty canvas past them with the first window cut
    /// off at the other edge.
    #[test]
    fn closing_the_last_window_at_the_end_rests_on_the_windows_that_remain() {
        let theme = Theme::dark().with_reduce_motion(true);
        let (row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let (mut row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        let end = row.max_scroll_x();
        assert!(end > 1.0, "fixture: three windows overflow the strip");
        row.set_scroll_x_for_test(end);
        let (mut row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        assert!(
            (row.scroll_x() - end).abs() < 0.5,
            "fixture: panned to the last window"
        );
        let last = row.groups[2].id;
        assert!(row.remove_group_by_id(last));
        let (row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        let (row, _taffy, _root) = relayout_row_owned_with(row, 1200.0, &theme);
        assert!(
            row.pan_max_scroll_x() > row.max_scroll_x() + 1.0,
            "fixture: the strip keeps an allowance past its last window"
        );
        assert!(
            row.scroll_x() <= row.max_scroll_x() + 0.5,
            "the pan rests within the windows: {} past an end of {}",
            row.scroll_x(),
            row.max_scroll_x()
        );
    }

    pub(super) fn relayout_row_owned_with(
        mut row: GroupRow,
        viewport_w: f64,
        theme: &Theme,
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                    width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
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
                    width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                    width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    /// A presenter pans a strip drawn in canvas coordinates. The encoded
    /// scene must be the same at two pans, or every pan draws the strip
    /// again.
    #[test]
    fn panning_a_strip_drawn_in_canvas_coordinates_does_not_change_its_scene() {
        let specs: Vec<Vec<u64>> = (1..=12).map(|id| vec![id]).collect();
        let specs: Vec<&[u64]> = specs.iter().map(Vec::as_slice).collect();
        let (mut row, taffy, root) = make_row_with_groups_on_display(&specs, 2400.0, 800.0);
        assert!(
            row.max_scroll_x() > 200.0,
            "the fixture must be able to pan"
        );
        let theme = Theme::dark().with_reduce_motion(true);
        let mut text = TextRenderer::new();
        let mut draw_at = |row: &mut GroupRow, scroll: f64| {
            row.reduce_motion = true;
            row.set_scroll_x_for_test(scroll);
            row.apply_layout(&taffy, root, Point::ORIGIN);
            let mut scene = Scene::new();
            row.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            scene
        };
        row.reduce_motion = true;
        crate::retained_present::begin_retained_scene(true);
        let _warm = draw_at(&mut row, 0.0);
        let rested = draw_at(&mut row, 0.0);
        let again = draw_at(&mut row, 0.0);
        let panned = draw_at(&mut row, 200.0);
        let shift = row.canvas_shift();
        let on_screen = row.groups[0].get_rect().x0;
        crate::retained_present::end_retained_scene();
        row.set_scroll_x_for_test(0.0);
        row.apply_layout(&taffy, root, Point::ORIGIN);
        let at_rest = row.groups[0].get_rect().x0;
        assert!(
            rested.encoding_eq(&again),
            "two settled frames at rest must match before a pan is meaningful"
        );
        assert!(
            rested.encoding_eq(&panned),
            "a pan must leave the strip's scene as it was"
        );
        assert_eq!(
            shift,
            200.0 + row.canvas_pad(),
            "floating surfaces follow the same shift"
        );
        assert!(
            (on_screen - at_rest).abs() > 1.0,
            "drawing must put the windows back into screen coordinates"
        );
    }

    #[test]
    fn an_overscrolled_strip_pans_within_the_padded_canvas() {
        let specs: Vec<Vec<u64>> = (1..=12).map(|id| vec![id]).collect();
        let specs: Vec<&[u64]> = specs.iter().map(Vec::as_slice).collect();
        let (mut row, taffy, root) = make_row_with_groups_on_display(&specs, 2400.0, 800.0);
        let max = row.max_scroll_x();
        let pad = row.canvas_pad();
        assert!(
            pad > row.leading_pan().viewport().width() * STRIP_OVERSCROLL_REACH_FRAC,
            "the pad holds the furthest rubber band"
        );
        assert!(pan_within_canvas(-pad, max, pad) && pan_within_canvas(max + pad, max, pad));
        assert!(!pan_within_canvas(-pad - 1.0, max, pad));

        row.reduce_motion = true;
        row.apply_layout(&taffy, root, Point::ORIGIN);
        // A rubber band in progress: the setters clamp to the content.
        row.pan.scroll_x = -30.0;
        crate::retained_present::begin_retained_scene(true);
        let overscrolled = row.canvas_shift();
        row.set_scroll_x_for_test(120.0);
        let panned = row.canvas_shift();
        crate::retained_present::end_retained_scene();
        let outside = row.canvas_shift();
        assert_eq!(
            overscrolled,
            pad - 30.0,
            "a rubber band past the start pans within the canvas"
        );
        assert_eq!(panned, 120.0 + pad);
        assert_eq!(outside, 0.0, "outside a retained draw nothing is shifted");
    }

    fn make_row_with_groups(tab_specs: &[&[u64]]) -> GroupRow {
        make_row_with_groups_with_taffy(tab_specs).0
    }

    /// A row a host pans between frames, as the desktop surface pans its
    /// shown strip for a wheel: begin, travel and end in one step.
    struct PannedRow {
        row: GroupRow,
        travel: std::rc::Rc<std::cell::Cell<Option<f64>>>,
    }

    impl crate::widget::Decorator for PannedRow {
        type Inner = GroupRow;
        fn inner(&self) -> &GroupRow {
            &self.row
        }
        fn inner_mut(&mut self) -> &mut GroupRow {
            &mut self.row
        }
        fn needs_redraw(&self) -> bool {
            self.travel.get().is_some() || self.row.needs_redraw()
        }
        fn update(&mut self, state: &crate::widget::AppModel, path: &ModelPath) -> bool {
            if let Some(travel) = self.travel.take() {
                self.row.begin_strip_pan();
                assert!(
                    self.row.update_strip_pan(travel, crate::time::now()),
                    "the strip moves"
                );
                self.row.end_strip_pan(crate::time::now());
            }
            self.row.update(state, path)
        }
    }

    /// A pan translates windows that are already laid out: the frame
    /// re-applies positions, so hit-testing and culling follow, but solves
    /// no size, and a strip of windows pans at the cost of moving them.
    #[test]
    fn a_strip_pan_re_applies_positions_without_measuring() {
        let travel = std::rc::Rc::new(std::cell::Cell::new(None));
        let groups = (1..=6).map(|id| make_group(id * 100, &[id])).collect();
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            PannedRow {
                row: GroupRow::new(groups),
                travel: std::rc::Rc::clone(&travel),
            },
            (1200, 800),
            Theme::dark(),
        );
        for _ in 0..4 {
            harness.relayout();
        }
        let row = |harness: &crate::headless::FrameHarness| {
            let row = &harness.root::<PannedRow>().row;
            (
                row.scroll_x(),
                row.groups[0].get_rect(),
                row.pan_max_scroll_x(),
            )
        };
        let (start, first, max) = row(&harness);
        assert!(max > 300.0, "six windows overflow 1200 px: {max}");
        let nodes = harness.layout_node_ids();

        travel.set(Some(150.0));
        assert!(harness.poll_frame().unwrap(), "a pan presents a frame");
        let stats = harness.last_relayout();
        assert!(!stats.rebuilt, "a pan keeps the composition: {stats:?}");
        assert!(
            !stats.measured,
            "a pan solves no size: {stats:?}, asked by {:?}",
            harness.layout_measure_requesters()
        );
        assert_eq!(harness.layout_node_ids(), nodes);
        let (panned, moved, _) = row(&harness);
        assert!((panned - start - 150.0).abs() < 1e-9, "{start} -> {panned}");
        assert!(
            (first.x0 - moved.x0 - 150.0).abs() < 1e-9,
            "the windows are laid out where the strip panned to: {first:?} -> {moved:?}"
        );
        harness.verify_retained_geometry().unwrap();
    }

    /// An app whose wake deadline only its paint moves on, as a status pill
    /// that clears itself once drawn after its linger.
    struct PaintedDeadline {
        deadline: std::rc::Rc<std::cell::Cell<Option<Instant>>>,
        rect: Rect,
    }

    impl Widget for PaintedDeadline {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.fill_leaf(node_map, self)
        }
        crate::rect_geometry!();
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
            self.deadline.set(None);
        }
        fn next_wake_deadline(&self) -> Option<Instant> {
            self.deadline.get()
        }
        fn name(&self) -> &'static str {
            "PaintedDeadline"
        }
    }

    impl crate::widget::AppWidget for PaintedDeadline {
        fn uses_host_record(&self) -> bool {
            false
        }
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Painted deadline"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("P")
        }
    }

    /// A window past the strip's edge is not painted, so a deadline only its
    /// paint consumes stays due: once a frame has run for it, it must not
    /// wake the host on every frame until the window is shown. Its later
    /// deadlines still wake the host, and shown again it is painted.
    #[test]
    fn an_unpainted_window_does_not_keep_waking_the_host_for_its_paint() {
        let deadline = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut groups: Vec<WindowGroup> = (1..=5).map(|id| make_group(id * 100, &[id])).collect();
        groups.push(WindowGroup::new(
            600,
            vec![(
                TabDescriptor::new(6, "T6"),
                TileContent::App(Box::new(PaintedDeadline {
                    deadline: std::rc::Rc::clone(&deadline),
                    rect: Rect::ZERO,
                })),
            )],
            6,
            WindowWidth::Normal,
        ));
        let travel = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            PannedRow {
                row: GroupRow::new(groups),
                travel: std::rc::Rc::clone(&travel),
            },
            (1200, 800),
            Theme::dark(),
        );
        for _ in 0..4 {
            harness.relayout();
        }
        let due = |harness: &crate::headless::FrameHarness| {
            harness
                .frame_schedule()
                .next_wake_deadline
                .is_some_and(|at| at <= crate::time::now())
        };

        // Due while off screen: the host wakes once and the frame runs.
        deadline.set(Some(crate::time::now()));
        assert!(due(&harness), "a due deadline wakes the host");
        assert!(harness.poll_frame().unwrap());
        assert!(
            deadline.get().is_some(),
            "an off-screen window is not painted"
        );
        assert!(
            !due(&harness),
            "the deadline had its frame: no frame after frame"
        );

        let later = crate::time::now() + Duration::from_secs(60);
        deadline.set(Some(later));
        assert_eq!(harness.frame_schedule().next_wake_deadline, Some(later));

        deadline.set(Some(crate::time::now()));
        let max = harness.root::<PannedRow>().row.pan_max_scroll_x();
        travel.set(Some(max));
        assert!(harness.poll_frame().unwrap());
        assert_eq!(deadline.get(), None, "the window shown is painted");
    }

    /// A closing window's fade rebuilds the row once per drawn frame. Asking
    /// on every composition pass kept one frame from ever settling, so
    /// closing a window stopped a host's drawing (`CompositionDidNotSettle`).
    #[test]
    fn a_closing_window_lets_its_frame_settle() {
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.take_structure_dirty();
        row.groups[0].start_closing();
        let mut passes = 0;
        let settled = crate::widget::layout::settle_widget_composition(false, |_| {
            passes += 1;
            Ok((Some(Vec::new()), row.take_structure_dirty()))
        });
        assert!(settled.is_ok(), "{settled:?} after {passes} passes");
        assert_eq!(passes, 2, "one rebuild for the fade's frame, then settled");
        row.mark_drawn();
        assert!(
            row.take_structure_dirty(),
            "the next frame steps the fade again"
        );
        assert!(!row.take_structure_dirty());
    }

    fn drawn_strip(windows: u64) -> (GroupRow, Scene) {
        let specs: Vec<Vec<u64>> = (1..=windows).map(|id| vec![id]).collect();
        let specs: Vec<&[u64]> = specs.iter().map(Vec::as_slice).collect();
        let (mut row, _, _) = make_row_with_groups_on_display(&specs, FIXTURE_USABLE_PX, 2000.0);
        let mut scene = Scene::new();
        row.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        (row, scene)
    }

    /// A window whose live picture is wanted is drawn as always and its
    /// drawing kept as the picture, at its rect; a window not wanted keeps
    /// none, a drawing that showed a secret is never kept, and wanting
    /// nothing drops what was kept.
    #[test]
    fn a_wanted_window_keeps_its_drawing_as_a_live_picture() {
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            let (plain, _) = drawn_strip(3);
            let wanted = plain.groups[0].id;
            let other = plain.groups[1].id;
            crate::window_pictures::want(&[wanted]);
            let (row, scene) = drawn_strip(3);
            assert!(!scene.encoding().is_empty(), "the window is still drawn");
            let rect = crate::window_pictures::with_picture(wanted, |picture| {
                assert!(!picture.scene.encoding().is_empty());
                picture.rect
            });
            assert_eq!(rect, Some(row.groups[0].get_rect()));
            assert!(crate::window_pictures::with_picture(other, |_| ()).is_none());

            let secret = ApartDrawing {
                id: wanted,
                rect: Rect::new(0.0, 0.0, 100.0, 100.0),
                transform: Affine::IDENTITY,
                scene: Scene::new(),
                parts: None,
                shows_secret: true,
                seen: None,
                current: true,
            };
            secret.offer_picture();
            assert!(
                crate::window_pictures::with_picture(wanted, |_| ()).is_none(),
                "a secret is never shown small"
            );
            let _ = drawn_strip(3);
            assert!(crate::window_pictures::with_picture(wanted, |_| ()).is_some());
            crate::window_pictures::want(&[]);
            assert!(crate::window_pictures::with_picture(wanted, |_| ()).is_none());
        });
    }

    /// A desktop that accumulated dozens of windows must encode only those
    /// the row shows: vello sizes its GPU buffers from everything encoded,
    /// clipped or not, and WebGPU's 128 MiB binding limit is finite.
    #[test]
    fn a_long_strip_encodes_only_the_groups_it_shows() {
        // Fresh curves are read in the instant they start: a descheduled
        // runner cannot age them.
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            let (few, few_scene) = drawn_strip(6);
            let (mut many, many_scene) = drawn_strip(40);
            let shown = many.onscreen_group_ids();
            assert!(
                !shown.is_empty() && shown.len() < 6,
                "fixture shows {shown:?}"
            );
            assert_eq!(few.onscreen_group_ids(), shown);
            assert!(many.culled_groups.len() >= 30, "{:?}", many.culled_groups);
            assert!(
                shown.iter().all(|id| !many.culled_groups.contains(id)),
                "an on-screen group was culled"
            );
            let (few_paths, many_paths) =
                (few_scene.encoding().n_paths, many_scene.encoding().n_paths);
            assert!(
                many_paths < few_paths * 2,
                "40 windows encoded {many_paths} paths against {few_paths} for 6"
            );

            // Redraw requests consult exactly the painted set, so a dirty app
            // past the edge does not keep the desktop presenting frames.
            let (consulted, skipped): (Vec<u64>, Vec<u64>) =
                many.groups.iter().map(|g| g.id).partition(|id| {
                    let g = many.groups.iter().find(|g| g.id == *id).expect("group");
                    many.paints_within_row(g.get_rect(), 0.0)
                });
            assert_eq!(skipped, many.culled_groups);
            assert!(consulted.len() >= shown.len());

            // Animation cadence used to OR every group, so a hosted app that
            // kept asking for frames after it scrolled off pinned the shell
            // at 144 Hz. The spawn animation is a real per-group demand.
            let offscreen = many.culled_groups[0];
            let group = many
                .groups
                .iter_mut()
                .find(|g| g.id == offscreen)
                .expect("culled group");
            group.start_spawn_animation();
            assert!(
                group.needs_animation_frame(),
                "fixture: the off-screen group is asking for frames"
            );
            assert!(
                !many.needs_animation_frame(),
                "an idle hosted app past the edge must not keep requesting frames"
            );
        });
    }

    #[test]
    fn refused_cross_strip_move_restores_exact_group_and_tab_order() {
        let mut row = make_row_with_groups(&[&[1, 2, 3], &[10]]);
        let source_group_index = 0;
        let source_group_id = row.groups[source_group_index].id;
        let source_tab_index = 1;
        let (desc, host) = row.groups[source_group_index]
            .detach_tab(2)
            .expect("fixture tab detaches");
        row.restore_cross_strip_detach(CrossStripDetach {
            tab_id: 2,
            source_group_id,
            source_group_index,
            source_tab_index,
            desc,
            host,
            release_point: Point::new(900.0, 900.0),
        });

        assert_eq!(row.group_ids(), vec![100, 200]);
        assert_eq!(
            row.groups[0]
                .tabs()
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
        );
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

    // A press on a window's search control opens the platform search, which
    // takes the pointer from there, so the release never reaches the row.
    // Had the press lifted the window, the next motion would carry it as a
    // ghost under the pointer with no button held.
    #[test]
    fn a_press_on_the_window_search_never_lifts_the_window() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let anchor = row.groups[0].window_search().anchor();
        assert!(anchor.area() > 0.0, "the bar shows its search control");
        let at = anchor.center();
        assert!(
            !row.groups[0].bar_grab_hit(at),
            "the search control is no drag handle"
        );
        press_at(&mut row, &taffy, root, at);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &move_to(Point::new(at.x + 150.0, at.y + 300.0)),
        );
        assert!(
            !row.group_drag_active(),
            "moving on after the press carries no window"
        );
    }

    fn press_at(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
        dispatch_at(
            row,
            taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
        );
        dispatch_at(
            row,
            taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
    }

    fn touch_event(id: u64, phase: winit::event::TouchPhase, p: Point) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(p.x, p.y),
            force: None,
            id,
        })
    }

    /// Send `ev` to `row` laid out at `root`; whether it was claimed.
    fn dispatch_at(
        row: &mut GroupRow,
        taffy: &TaffyTree<()>,
        root: NodeId,
        ev: &WindowEvent,
    ) -> bool {
        Dispatch::new().layout(taffy, root).send(row, ev).handled
    }

    #[test]
    fn bar_drag_reorders_groups_only_from_the_grab_zone() {
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
                    device_id: winit::event::DeviceId::dummy(),
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
                    device_id: winit::event::DeviceId::dummy(),
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
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(target.x, target.y),
                },
            );
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );
            assert_eq!(row.group_ids(), vec![100, 300, 200]);
        }

        fn plain_click_on_bar_background_does_not_reorder() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let grab = group_grab_point(&row, 0);
            press_at(&mut row, &taffy, root, grab);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
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

        bar_grab_point_hits_the_group_grab_zone();
        drag_group_to_end_reorders_group_vector_and_publishes_one_shot();
        drag_group_backwards_between_two_groups();
        plain_click_on_bar_background_does_not_reorder();
    }

    /// A window someone else holds follows a drag a little, springs back
    /// and never moves.
    #[test]
    fn a_held_window_resists_a_drag_and_stays_in_place() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[0].set_tab_holder(
            1,
            Some(crate::widget::TabHolder {
                name: "Ben".into(),
                rgb: (200, 80, 40),
            }),
        );
        assert!(row.groups[0].is_held());
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(past.x, past.y),
            },
        );
        assert_eq!(
            row.group_reorder_target(),
            None,
            "there is no gap to drop it in"
        );
        let pull = row.group_pull(100).expect("it follows the drag");
        assert!(
            pull.x > 0.0 && pull.x < f64::from(crate::tokens::Spacing::BASE.s4),
            "{pull:?}"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(row.group_ids(), vec![100, 200, 300]);
        assert_eq!(row.take_pending_group_reorder(), None);
        assert!(row.group_pull(100).is_some(), "it springs back");
        assert!(row.needs_animation_frame());
    }

    /// A window carried by its bar is drawn with the grabbed point exactly
    /// under the pointer on every frame of the drag, while it is lifting
    /// and once lifted, wherever along the bar it was grabbed.
    #[test]
    fn a_dragged_window_tracks_the_pointer_one_to_one() {
        let theme = Theme::dark();
        let mut now = crate::time::now();
        crate::time::testing::with_monotonic_time(now, || {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            let grab = group_grab_point(&row, 0);
            press_at(&mut row, &taffy, root, grab);
            let mut at = grab;
            for step in 1..=24 {
                now += Duration::from_millis(16);
                at = Point::new(
                    grab.x + 15.0 * f64::from(step),
                    grab.y + 6.0 * f64::from(step),
                );
                crate::time::testing::with_monotonic_time(now, || {
                    dispatch_at(&mut row, &taffy, root, &move_to(at));
                    let lift = row.group_drag_lift(&theme).expect("the window is carried");
                    let drawn = lift * grab;
                    assert!(
                        (drawn - at).hypot() < 1e-9,
                        "frame {step}: grabbed point drawn at {drawn:?}, pointer at {at:?}"
                    );
                });
            }
            assert!(at.x - grab.x >= 350.0 && at.y - grab.y >= 140.0);
        });
    }

    /// Under reduced motion a held window is back in place at once: no
    /// spring-back, and nothing asks for frames.
    #[test]
    fn a_held_window_lands_back_at_once_under_reduced_motion() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.reduce_motion = true;
        row.groups[0].set_tab_holder(
            1,
            Some(crate::widget::TabHolder {
                name: "Ben".into(),
                rgb: (200, 80, 40),
            }),
        );
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let past = Point::new(row.groups[2].get_rect().center().x + 60.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(past.x, past.y),
            },
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert_eq!(row.group_pull(100), None, "no spring-back");
        assert!(!row.needs_animation_frame());
    }

    #[test]
    fn one_finger_horizontal_swipe_on_the_tab_bar_pans_the_strip() {
        let (mut row, taffy, root) = make_overflow_row(8, 1200.0);
        assert!(
            row.pan_max_scroll_x() > 80.0,
            "the fixture must have room to pan"
        );
        let start = group_grab_point(&row, 0);
        let before = row.scroll_x();
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Started, start),
        );
        let mid = Point::new(start.x - 80.0, start.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Moved, mid),
        );
        assert!(
            row.scroll_x() - before > 20.0,
            "a leftward swipe must pan toward later groups; before {before} after {}",
            row.scroll_x()
        );
        assert!(
            !row.group_drag_active(),
            "a horizontal swipe is a pan, not a reorder"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Ended, mid),
        );
    }

    #[test]
    fn touch_hold_then_drag_on_the_tab_bar_moves_the_window() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        let start = group_grab_point(&row, 0);
        let before = row.scroll_x();
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Started, start),
        );
        // The finger rested before moving.
        let drag = row
            .group_drag
            .as_mut()
            .expect("a finger on the bar lifts the group");
        drag.lifted_at = drag
            .lifted_at
            .checked_sub(TOUCH_GRAB_HOLD)
            .expect("monotonic clock has room");
        let to = Point::new(row.groups[2].get_rect().center().x + 60.0, start.y);
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let p = Point::new(start.x + (to.x - start.x) * t, start.y);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &touch_event(1, winit::event::TouchPhase::Moved, p),
            );
        }
        assert!(
            row.group_drag_active(),
            "after a hold, a horizontal drag carries the window"
        );
        assert!(
            (row.scroll_x() - before).abs() < f64::EPSILON,
            "a carried window does not pan the strip"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Ended, to),
        );
        assert_eq!(
            row.group_ids(),
            vec![200, 300, 100],
            "the carried window lands where the finger lifted"
        );
        assert_eq!(row.take_pending_group_reorder(), Some((100, 2)));
    }

    #[test]
    fn one_finger_swipe_onto_another_display_rehomes_the_group() {
        let (mut row, taffy, root) = relayout_row_owned(partitioned_row(), 1800.0);
        // Leftmost group: its grab sits on display 0. Group 200 can hang
        // across the seam, so a grab there never "crosses".
        let start = group_grab_point(&row, 0);
        assert_eq!(row.output_of_group(100), Some(0));
        assert_eq!(
            row.output_at(start),
            Some(0),
            "grab {start:?} must start on display 0"
        );
        assert!(
            row.groups[0].bar_grab_hit(start),
            "{start:?} must lift group 100"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Started, start),
        );
        let to = Point::new(1200.0, start.y);
        assert_eq!(
            row.output_at(to),
            Some(1),
            "{to:?} is on the second display"
        );
        for step in 1..=8 {
            let t = f64::from(step) / 8.0;
            let p = Point::new(start.x + (to.x - start.x) * t, start.y);
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &touch_event(1, winit::event::TouchPhase::Moved, p),
            );
        }
        assert!(
            row.group_drag_active(),
            "crossing a display must keep the group drag, not convert it to a pan"
        );
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &touch_event(1, winit::event::TouchPhase::Ended, to),
        );
        assert_eq!(
            row.output_of_group(100),
            Some(1),
            "the group belongs to the display it was dropped on"
        );
    }

    #[test]
    fn the_reveal_target_of_a_group_still_growing_is_where_it_will_end() {
        // A new window's slot grows from zero width. The pan toward it
        // must aim at the finished layout, not at the first frames' rect.
        let layout = |animate: bool| {
            let (mut row, _taffy, _root) =
                make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
            if animate {
                row.groups
                    .last_mut()
                    .expect("groups")
                    .start_spawn_animation();
                let (relaid, _t, _r) = relayout_row_owned(row, 700.0);
                row = relaid;
                row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
            }
            row
        };
        let settled = layout(false);
        let growing = layout(true);
        let last = growing.groups.last().expect("groups");
        assert!(
            last.dest_width_px() > last.layout_width_px() + 1.0,
            "the fixture must still be growing: {} of {}",
            last.layout_width_px(),
            last.dest_width_px()
        );
        let id = last.id;
        assert_eq!(
            growing.scroll_target_for(id),
            settled.scroll_target_for(id),
            "the pan aims at the finished layout"
        );
    }

    #[test]
    fn scroll_group_into_view_pans_or_eases_only_when_offscreen() {
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

            let t0 = crate::time::now();
            assert!(
                anim.scroll_group_into_view_animated(target_id, t0, MotionSource::Pointer),
                "an off-screen group must start an animated pan"
            );
            assert_eq!(
                anim.scroll_x(),
                0.0,
                "the pan is applied by ticks, not the call"
            );
            let snap = crate::tokens::Motion::default().durations.base();
            anim.tick_snap_at(t0 + snap / 2);
            let mid = anim.scroll_x();
            assert!(
                mid > 0.0 && mid < expected,
                "halfway through the curve scroll_x must be between 0 and {expected}, got {mid}"
            );
            anim.tick_snap_at(t0 + Duration::from_secs(1));
            assert_eq!(
                anim.scroll_x(),
                expected,
                "the animated pan must settle on the same target the teleporting variant picks"
            );
        }

        fn scroll_group_into_view_animated_is_a_noop_for_a_visible_group() {
            // Fixture's full 2000px viewport: first group is fully inside.
            let (mut row, _taffy, _root) =
                make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
            assert!(!row.scroll_group_into_view_animated(
                100,
                crate::time::now(),
                MotionSource::Pointer
            ));
            assert_eq!(row.scroll_x(), 0.0);
            assert!(
                !row.needs_animation_frame(),
                "a no-op must not leave an animation pumping frames"
            );
        }

        scroll_group_into_view_pans_to_reveal_offscreen_group();
        scroll_group_into_view_animated_eases_to_the_same_target();
        scroll_group_into_view_animated_is_a_noop_for_a_visible_group();
    }

    #[test]
    fn drop_into_new_window_from_strip_areas_and_closing_groups() {
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

        fn drop_into_new_window_works_when_release_is_above_row() {
            let row = make_row_with_groups(&[&[1, 2], &[10, 11]]);
            let r0 = row.groups[0].get_rect();
            let release = Point::new(r0.x0 - 4.0, row.rect.y0 - 15.0);
            assert_eq!(
                row.gap_index_for(release),
                Some(0),
                "release above the row should resolve to the left-of-first gap"
            );
        }

        fn drop_onto_closing_group_creates_new_group_instead() {
            use winit::event::WindowEvent;

            // Two multi-tab groups so neither gets pruned by the
            // detach-empties-source rule.
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);

            // B (index 1) is fresh-latched closing, so the prune sweep won't
            // touch it this tick.
            row.groups[1].set_closing_at_for_tests(crate::time::now());
            assert!(row.groups[1].is_closing());
            assert!(!row.groups[1].close_complete());
            let b_tabs_before: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
            assert_eq!(b_tabs_before, vec![10, 11]);

            let b_bar = row.groups[1].bar_rect();
            let release = Point::new(
                f64::midpoint(b_bar.x0, b_bar.x1),
                f64::midpoint(b_bar.y0, b_bar.y1),
            );
            row.tab_drop_target = row.tab_drop_target_at(release);
            row.groups[0].bar_mut().pending_detach = Some((1, release));

            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &event);

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

            row.tab_drop_target = row.tab_drop_target_at(release_left);
            row.groups[0].bar_mut().pending_detach = Some((1, release_left));
            let release_event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &release_event);

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

            row.tab_drop_target = row.tab_drop_target_at(release_right);
            row.groups[0].bar_mut().pending_detach = Some((1, release_right));
            dispatch_at(&mut row, &taffy, root, &release_event);

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
        fn cross_group_drop_into_closing_content_area_keeps_tab_in_source() {
            use winit::event::WindowEvent;

            // B (index 1) is fresh-latched closing so the prune sweep won't
            // reap it this tick.
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
            row.groups[1].set_closing_at_for_tests(crate::time::now());
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
            row.tab_drop_target = row.tab_drop_target_at(release);
            row.groups[0].bar_mut().pending_detach = Some((1, release));

            let release_event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &release_event);

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

        drop_into_new_window_works_when_release_is_in_empty_strip_area_below_group();
        drop_into_new_window_works_when_release_is_above_row();
        drop_onto_closing_group_creates_new_group_instead();
        drop_in_sibling_tile_area_left_half_prepends_right_half_appends();
        cross_group_drop_into_closing_content_area_keeps_tab_in_source();
        drop_in_visible_trailing_area_creates_new_group_at_end();
    }

    #[test]
    fn single_tab_group_splits_on_any_gap_repeatedly() {
        fn single_tab_group_can_split_into_new_group_on_any_adjacent_gap() {
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

        fn single_tab_group_split_is_not_refused_by_handle_event_sweep() {
            // Drives the post-dispatch sweep end-to-end, where the refuse
            // rule (unlike the direct API above) used to live.
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[7]]);
            let g0_rect = row.groups[0].get_rect();
            let release = Point::new(g0_rect.x1 + 10.0, f64::midpoint(g0_rect.y0, g0_rect.y1));
            // Mimics what `TabBar::handle_event` does on an out-of-rect release.
            row.tab_drop_target = row.tab_drop_target_at(release);
            row.groups[0].bar_mut().pending_detach = Some((7, release));

            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &event);

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

        /// Two detach+drop sequences in a row, to catch a freeze at the
        /// `GroupRow` level. If it hangs or panics, the bug is here; if it
        /// passes, look one layer up (`App` tick, taffy rebuild).
        fn two_sequential_splits_do_not_hang() {
            // One 4-tab group, matching the live shell's bootstrap_default
            // Home workspace.
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1, 2, 3, 4]]);

            // DROP #1: tab 4 dropped right of the only group's bar → 2nd group.
            let g0_rect = row.groups[0].get_rect();
            let release_1 = Point::new(g0_rect.x1 + 30.0, g0_rect.y0 + 6.0);
            row.tab_drop_target = row.tab_drop_target_at(release_1);
            row.groups[0].bar_mut_for_tests().pending_detach = Some((4, release_1));

            dispatch_at(
                &mut row,
                &taffy,
                root,
                &winit::event::WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                },
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
            row.tab_drop_target = row.tab_drop_target_at(release_2);
            row.groups[0].bar_mut_for_tests().pending_detach = Some((3, release_2));

            dispatch_at(
                &mut row,
                &taffy,
                root,
                &winit::event::WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                },
            );
            assert_eq!(
                row.groups.len(),
                3,
                "drop 2 must produce 3 groups (was {})",
                row.groups.len()
            );
        }

        single_tab_group_can_split_into_new_group_on_any_adjacent_gap();
        single_tab_group_split_is_not_refused_by_handle_event_sweep();
        two_sequential_splits_do_not_hang();
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
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        dispatch_at(row, taffy, root, &event);
    }

    #[test]
    fn hover_gap_tracks_drag_and_clears_without_one() {
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
                row.tab_drop_target,
                Some(TabDropTarget::GroupGap { gap_index: 1 }),
                "cursor in the gap must preview that exact group gap"
            );
        }

        fn hover_gap_clears_on_release() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            start_drag(&mut row, 0, 1);
            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
            let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);
            fire_cursor_moved(&mut row, &taffy, root, mid_gap);
            assert!(
                row.tab_drop_target.is_some(),
                "precondition: drag tracked the gap"
            );

            let event = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert_eq!(
                row.tab_drop_target, None,
                "MouseInput::Released must clear the preview so no ghost indicator persists"
            );
        }

        fn hover_gap_is_none_when_no_drag() {
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            let r0 = row.groups[0].get_rect();
            let r1 = row.groups[1].get_rect();
            let mid_y = f64::midpoint(row.rect.y0, row.rect.y1);
            let mid_gap = Point::new(f64::midpoint(r0.x1, r1.x0), mid_y);

            fire_cursor_moved(&mut row, &taffy, root, mid_gap);

            assert_eq!(
                row.tab_drop_target, None,
                "cursor motion without an active drag must not publish a drop preview"
            );
        }

        hover_gap_tracks_cursor_during_drag();
        hover_gap_clears_on_release();
        hover_gap_is_none_when_no_drag();
    }

    #[test]
    fn closing_groups_fade_before_pruning_with_aligned_nodes() {
        fn group_pruned_after_close_fade_completes() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            // Already-elapsed timestamp: ready-to-prune on the very next tick.
            row.groups[0].set_closing_at_for_tests(
                crate::time::now()
                    .checked_sub(
                        Theme::dark()
                            .tokens
                            .motion
                            .durations
                            .base()
                            .saturating_add(std::time::Duration::from_millis(40)),
                    )
                    .expect("representable close deadline"),
            );
            assert_eq!(row.groups.len(), 2, "precondition: two groups before prune");

            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
            };
            dispatch_at(&mut row, &taffy, root, &event);

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
        fn group_retained_during_close_fade() {
            use winit::event::WindowEvent;

            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
            row.groups[0].set_closing_at_for_tests(crate::time::now());
            assert!(row.groups[0].is_closing());
            assert!(!row.groups[0].close_complete());

            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
            };
            dispatch_at(&mut row, &taffy, root, &event);

            assert_eq!(
                row.groups.len(),
                2,
                "group mid-fade must NOT be pruned — the user is still watching the animation",
            );
        }

        /// The prune sweep removes faded groups from `self.groups` while
        /// `node_ids` stays stale until the next `build_layout`; the
        /// production `debug_assert` pins that. Gated since the asserts
        /// exercised are themselves debug-only.
        #[cfg(debug_assertions)]
        fn group_prune_keeps_node_ids_aligned_in_debug() {
            use winit::event::WindowEvent;

            // Non-adjacent prune indices (0, 2) verify the
            // highest-index-first sort doesn't shift slots under the loop.
            let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
            row.groups[0].set_closing_at_for_tests(earlier(crate::time::now(), 500));
            row.groups[2].set_closing_at_for_tests(earlier(crate::time::now(), 500));

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
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
            };
            dispatch_at(&mut row, &taffy, root, &event);

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

        group_pruned_after_close_fade_completes();
        group_retained_during_close_fade();
        #[cfg(debug_assertions)]
        group_prune_keeps_node_ids_aligned_in_debug();
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
                    width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                    width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    /// Lay a column holding one strip of `count` groups out `viewport_w`
    /// wide, twice so lanes set before the first pass are in its build, and
    /// return the column's rect with the strip's pan bound.
    fn column_around_strip(count: usize, viewport_w: f64, lanes: &[StripLane]) -> (Rect, f64) {
        use crate::widget::Column;
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize};

        let groups: Vec<WindowGroup> = (0..count)
            .map(|i| {
                let id = (i as u64).saturating_mul(100).saturating_add(100);
                make_group(id, &[(i as u64).saturating_add(1)])
            })
            .collect();
        let mut row = GroupRow::new(groups);
        let assignment: HashMap<u64, u32> = row
            .groups
            .iter()
            .map(|g| (g.id, lanes.first().map_or(0, |lane| lane.output)))
            .collect();
        row.set_lanes(lanes, &assignment);
        let mut column = Column::new().with_no_padding().push(Box::new(row));
        let theme = Theme::dark();
        let mut rect = Rect::ZERO;
        for _ in 0..2 {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
            let node = column.build_layout(&mut taffy, &theme, &mut node_map);
            let viewport = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout_with_measure(
                    viewport,
                    TSize {
                        width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
                        height: AvailableSpace::Definite(600.0),
                    },
                    |_, _, _, _, _| taffy::Size::ZERO,
                )
                .expect("taffy compute_layout");
            column.apply_layout(&taffy, node, Point::ORIGIN);
            rect = column.get_rect();
        }
        let max_scroll = column
            .children_mut()
            .into_iter()
            .next()
            .and_then(|row| row.as_any_mut())
            .and_then(|row| row.downcast_mut::<GroupRow>())
            .map_or(0.0, |row| row.max_scroll_x());
        (rect, max_scroll)
    }

    /// Windows past a strip's viewport are the strip's to pan and clip: the
    /// container holding it stays the width it was given, on one display
    /// and on a strip split across two.
    #[test]
    fn a_strip_past_its_viewport_never_widens_its_container() {
        let viewport_w = 2000.0;
        let lanes = [
            StripLane {
                output: 0,
                area: Rect::new(0.0, 0.0, 1200.0, 600.0),
                usable: 1200.0,
                leading: false,
            },
            StripLane {
                output: 1,
                area: Rect::new(1200.0, 0.0, viewport_w, 600.0),
                usable: 800.0,
                leading: false,
            },
        ];
        for (what, lanes) in [("one run", &[][..]), ("a lane per display", &lanes[..])] {
            let (rect, max_scroll) = column_around_strip(8, viewport_w, lanes);
            assert!(
                max_scroll > 0.0,
                "{what}: eight groups overflow, so the strip pans ({max_scroll})"
            );
            assert!(
                (rect.width() - viewport_w).abs() < 0.5,
                "{what}: the column is {rect:?}, not the {viewport_w} it was given"
            );
        }
    }

    #[test]
    fn strip_layout_never_overlaps_or_cuts_windows() {
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
                ids.compact_sort();
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

        /// Windows changing width on their own (a new share of the strip,
        /// a peer's resize) never pan the strip: it scrolls only for
        /// explicit navigation and to bring a newly opened or focused
        /// window into view. Sequence mirrors the compositor's own: open +
        /// bring into view, then give groups a new share of the strip.
        fn widening_a_strip_leaves_its_pan_where_it_was() {
            let mut row = strip_at(2, 1920.0, 291.0, 667);
            // Sitting where bringing the newest window into view leaves it:
            // hard against its right end.
            let parked = row.max_scroll_x();
            row.set_scroll_x_for_test(parked);
            assert!(parked > 0.0, "two groups at 667 permille overflow 1920");

            set_strip_permille(&mut row, 720);
            relayout_strip(&mut row, 1920.0);

            assert!(
                (row.scroll_x() - parked).abs() < 1e-6,
                "the strip moved from {parked:.0} to {:.0} by itself — spans {:?}",
                row.scroll_x(),
                row.pan.laid_spans,
            );
        }

        /// Even when every window would fit on screen and one is cut off at
        /// an edge, the strip does not scroll by itself to frame them all.
        fn narrowing_until_everything_fits_does_not_frame_the_strip() {
            let mut row = strip_at(2, 1920.0, 291.0, 720);
            row.set_scroll_x_for_test(300.0);
            relayout_strip(&mut row, 1920.0);

            set_strip_permille(&mut row, 580);
            // The windows narrow on their slot springs; measure them at rest.
            relayout_strip(&mut row, 1920.0);
            row.frame_presented();
            let settled = crate::time::now() + Duration::from_secs(2);
            crate::time::testing::with_monotonic_time(settled, || {
                relayout_strip(&mut row, 1920.0);
            });

            let span = row
                .pan
                .laid_spans
                .last()
                .zip(row.pan.laid_spans.first())
                .map(|((_, _, r), (_, l, _))| r - l)
                .unwrap_or_default();
            assert!(
                span <= 1920.0,
                "test setup: two groups at 580 permille must fit 1920, got {span:.0}",
            );
            assert!(
                (row.scroll_x() - 300.0).abs() < 1e-6,
                "the strip moved to {:.0} by itself to frame the windows — spans {:?}",
                row.scroll_x(),
                row.pan.laid_spans,
            );
            assert_eq!(
                row.groups_cut_by_row_edges(),
                vec![row.groups[0].id],
                "the first window stays cut where the person left the strip",
            );
        }

        content_width_exceeds_viewport_when_groups_overflow();
        no_two_groups_on_a_strip_share_horizontal_space();
        widening_a_strip_leaves_its_pan_where_it_was();
        narrowing_until_everything_fits_does_not_frame_the_strip();
    }

    /// Lay `row` out inside a `viewport_w`-wide viewport through the
    /// real `build_layout` / `apply_layout` pair, rebuilding the taffy
    /// tree so a changed width actually takes — the baked length on the
    /// old node is what a stale tree would keep serving.
    pub(super) fn relayout_strip(row: &mut GroupRow, viewport_w: f64) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(crate::num::f32_of_f64(viewport_w)),
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
                    width: AvailableSpace::Definite(crate::num::f32_of_f64(viewport_w)),
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
    pub(super) fn strip_at(
        count: usize,
        viewport_w: f64,
        leading_inset: f64,
        permille: u32,
    ) -> GroupRow {
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

    #[test]
    fn presenter_pan_precision_moves_real_input_rects_without_rounding_state() {
        let mut row = strip_at(3, 800.0, 0.0, 1000);
        let initial = row.groups[0].get_rect().x0;
        row.set_scroll_x_for_test(120.5);
        relayout_strip(&mut row, 800.0);
        assert_eq!(row.groups[0].get_rect().x0, initial - 120.5);
        row.set_scroll_quantum(Some(1.0));
        relayout_strip(&mut row, 800.0);
        assert_eq!(row.groups[0].get_rect().x0, initial - 121.0);
        assert_eq!(row.scroll_x(), 120.5, "the gesture retains its precision");
        row.set_scroll_quantum(None);
        relayout_strip(&mut row, 800.0);
        assert_eq!(row.groups[0].get_rect().x0, initial - 120.5);
    }

    /// At 150 % a device pixel is two thirds of a layout pixel: the drawn
    /// pan and the canvas pad land on its multiples, so a canvas shift is a
    /// whole number of device pixels and a presenter can move the strip's
    /// pixels by copying them.
    #[test]
    fn a_device_pixel_quantum_puts_pan_and_canvas_shift_on_device_pixels() {
        let scale = 1.5;
        let mut row = strip_at(3, 800.0, 0.0, 1000);
        let initial = row.groups[0].get_rect().x0;
        let unaligned_pad = row.canvas_pad();
        row.set_scroll_quantum(Some(1.0 / scale));
        row.set_scroll_x_for_test(120.5);
        relayout_strip(&mut row, 800.0);
        let drawn = row.drawn_scroll_x();
        let device = drawn * scale;
        assert!(
            (device - device.round()).abs() < 1e-5,
            "the drawn pan {drawn} is {device} device pixels"
        );
        assert!(
            (drawn - 120.5).abs() <= 1.0 / 3.0,
            "the nearest step: {drawn}"
        );
        assert!((row.groups[0].get_rect().x0 - (initial - drawn)).abs() < 1e-9);
        let pad = row.canvas_pad();
        let pad_device = pad * scale;
        assert!(
            (pad_device - pad_device.round()).abs() < 1e-5 && pad >= unaligned_pad,
            "the canvas pad {pad} (was {unaligned_pad}) is {pad_device} device pixels"
        );
        let _scope = crate::retained_present::RetainedScene::begin(true);
        let shift = row.canvas_shift() * scale;
        assert!(
            (shift - shift.round()).abs() < 1e-5,
            "the canvas shift is {shift} device pixels"
        );
        assert_eq!(row.scroll_x(), 120.5, "the gesture retains its precision");
    }

    /// While a presenter pans a strip drawn in canvas coordinates, a group
    /// wholly off screen replays its last drawing instead of being drawn
    /// again, every group the screen shows is drawn, and the strip's drawing
    /// stays byte for byte the same while only the pan moves. Held still, the
    /// strip draws every group; drawn in screen space again, the row drops
    /// the drawings.
    #[test]
    fn a_panned_strip_replays_groups_off_screen_and_draws_those_on_it() {
        fn draw(row: &mut GroupRow, text: &mut TextRenderer) -> Scene {
            let mut scene = Scene::new();
            row.draw(
                &mut scene,
                Affine::IDENTITY,
                text,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            scene
        }
        let text = &mut TextRenderer::new();
        let mut row = strip_at(8, 800.0, 0.0, 1000);
        relayout_strip(&mut row, 800.0);
        let scope = crate::retained_present::RetainedScene::begin(true);
        let first = draw(&mut row, text);
        assert!(
            row.culled_groups.is_empty(),
            "the first canvas draw draws every group: {:?}",
            row.culled_groups
        );
        let mut replayed_any = false;
        for scroll in [37.0, 1300.0, 2900.0, 0.0] {
            row.set_scroll_x_for_test(scroll);
            relayout_strip(&mut row, 800.0);
            let panned = draw(&mut row, text);
            let shown = row.onscreen_group_ids();
            assert!(!shown.is_empty(), "the screen shows a group at {scroll}");
            assert!(
                shown.iter().all(|id| !row.culled_groups.contains(id)),
                "at {scroll} a group on screen ({shown:?}) was replayed: {:?}",
                row.culled_groups
            );
            replayed_any |= !row.culled_groups.is_empty();
            assert!(
                first.encoding_eq(&panned),
                "a pan to {scroll} leaves the strip's canvas drawing as it was"
            );
        }
        assert!(replayed_any, "groups off screen replay their drawings");
        assert!(!row.apart_drawings.is_empty());
        // Held still, the strip draws every group again, so each app
        // presents as it would at rest.
        let held = draw(&mut row, text);
        assert!(
            row.culled_groups.is_empty(),
            "a strip at rest replays nothing: {:?}",
            row.culled_groups
        );
        assert!(first.encoding_eq(&held));
        drop(scope);
        let _ = draw(&mut row, text);
        assert!(
            row.apart_drawings.is_empty(),
            "a screen-space draw drops the canvas drawings"
        );
    }

    /// On a frame that only moved laid-out content, a strip drawn in canvas
    /// coordinates shows the groups on screen at rest by replaying their
    /// drawings, and the strip's drawing stays byte for byte the same. A
    /// focused group is drawn. A drawing replayed off screen through a frame
    /// that may have changed its group is drawn again before it is shown.
    #[test]
    fn a_frame_that_only_moved_replays_groups_on_screen_at_rest() {
        use crate::retained_present::{PanFrame, PanReplay, take_replayed};
        fn draw(row: &mut GroupRow, text: &mut TextRenderer, frame: PanFrame) -> Scene {
            thread_local! {
                static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
            }
            let number = FRAMES.with(|frames| {
                frames.set(frames.get().wrapping_add(1));
                frames.get()
            });
            let _frame = PanReplay::begin(frame, number);
            let mut scene = Scene::new();
            row.draw(
                &mut scene,
                Affine::IDENTITY,
                text,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            row.mark_drawn();
            scene
        }
        let text = &mut TextRenderer::new();
        let mut row = strip_at(8, 800.0, 0.0, 1000);
        relayout_strip(&mut row, 800.0);
        let _scope = crate::retained_present::RetainedScene::begin(true);
        let first = draw(&mut row, text, PanFrame::Drawn);
        take_replayed();
        row.set_scroll_x_for_test(37.0);
        relayout_strip(&mut row, 800.0);
        let moved = draw(&mut row, text, PanFrame::Moved);
        let shown = row.onscreen_group_ids();
        assert!(!shown.is_empty());
        assert!(
            shown.iter().all(|id| row.replayed_groups.contains(id)),
            "groups on screen at rest replay: shown {shown:?}, replayed {:?}",
            row.replayed_groups
        );
        assert_eq!(take_replayed(), shown.len());
        assert!(
            first.encoding_eq(&moved),
            "the replay draws what drawing would"
        );
        let focused = shown[0];
        let index = row.groups.iter().position(|g| g.id == focused).unwrap();
        let _ = row.groups[index].set_focused(true);
        let _ = draw(&mut row, text, PanFrame::Drawn);
        let _ = draw(&mut row, text, PanFrame::Moved);
        assert!(
            !row.replayed_groups.contains(&focused),
            "a focused group draws"
        );
        let _ = row.groups[index].set_focused(false);
        let _ = draw(&mut row, text, PanFrame::Drawn);
        // Panned away by a frame that may have changed anything: the groups
        // left behind replay off screen, and are drawn once shown again.
        row.set_scroll_x_for_test(2900.0);
        relayout_strip(&mut row, 800.0);
        let _ = draw(&mut row, text, PanFrame::Drawn);
        row.set_scroll_x_for_test(37.0);
        relayout_strip(&mut row, 800.0);
        take_replayed();
        let _ = draw(&mut row, text, PanFrame::Moved);
        assert!(
            shown.iter().all(|id| !row.replayed_groups.contains(id)),
            "drawings replayed through a changing frame are drawn again: {:?}",
            row.replayed_groups
        );
    }

    /// On frames that only pan a strip drawn in screen space, a window at
    /// rest shows its last drawing moved by the pan instead of drawing again,
    /// and the frame renders as one that drew every window does, in every
    /// theme and at fractional pans. A keyboard-focused window draws; a
    /// frame that is not only a pan draws every window and drops the
    /// drawings, so the next pan frame replays nothing; a pan frame that
    /// draws every window anyway keeps them.
    #[test]
    fn a_pan_frame_replays_windows_at_rest_and_renders_as_a_drawn_frame() {
        use crate::offscreen::OffscreenRenderer;
        use crate::retained_present::{PanFrame, PanReplay, take_replayed};
        fn draw(row: &mut GroupRow, text: &mut TextRenderer, theme: &Theme, replay: bool) -> Scene {
            draw_as(
                row,
                text,
                theme,
                if replay {
                    PanFrame::Replayed
                } else {
                    PanFrame::Drawn
                },
            )
        }
        fn draw_as(
            row: &mut GroupRow,
            text: &mut TextRenderer,
            theme: &Theme,
            frame: PanFrame,
        ) -> Scene {
            thread_local! {
                static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
            }
            let number = FRAMES.with(|frames| {
                frames.set(frames.get().wrapping_add(1));
                frames.get()
            });
            let _frame = PanReplay::begin(frame, number);
            let mut scene = Scene::new();
            let presence = PresenceCtx::empty();
            let previous = presence.push_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            row.draw(&mut scene, Affine::IDENTITY, text, theme, &presence);
            presence.pop_viewport(previous);
            row.mark_drawn();
            scene
        }
        let text = &mut TextRenderer::new();
        let mut renderer = OffscreenRenderer::new(800, 600);
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let mut row = strip_at(5, 800.0, 0.0, 400);
            relayout_strip(&mut row, 800.0);
            take_replayed();
            let _ = draw(&mut row, text, &theme, true);
            assert_eq!(take_replayed(), 0, "the first pan frame draws every window");
            for scroll in [37.0, 210.5, 333.25, 0.0] {
                row.set_scroll_x_for_test(scroll);
                relayout_strip(&mut row, 800.0);
                let replayed = draw(&mut row, text, &theme, true);
                let windows = take_replayed();
                assert!(windows > 0, "a pan to {scroll} replays the windows at rest");
                assert_eq!(windows, row.replayed_groups_for_test().len());
                // The same pan drawn by a frame that is not only a pan.
                let drawn = draw(&mut row, text, &theme, false);
                assert_eq!(
                    take_replayed(),
                    0,
                    "a frame that is not only a pan draws every window"
                );
                let replayed = renderer.render(&replayed, vello::peniko::Color::BLACK);
                let drawn = renderer.render(&drawn, vello::peniko::Color::BLACK);
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(&replayed, &drawn),
                    0,
                    "a pan to {scroll} replayed renders as drawn"
                );
                // That frame dropped the drawings: the next pan frame draws.
                let _ = draw(&mut row, text, &theme, true);
                assert_eq!(
                    take_replayed(),
                    0,
                    "drawings do not outlive a frame that is not only a pan"
                );
            }
            let shown = row.onscreen_group_ids();
            let focused = shown[0];
            let index = row
                .groups
                .iter()
                .position(|group| group.id == focused)
                .unwrap();
            let _ = row.groups[index].set_focused(true);
            let _ = draw(&mut row, text, &theme, false);
            let _ = draw(&mut row, text, &theme, true);
            row.set_scroll_x_for_test(120.0);
            relayout_strip(&mut row, 800.0);
            let _ = draw(&mut row, text, &theme, true);
            assert!(take_replayed() > 0);
            assert!(
                !row.replayed_groups_for_test().contains(&focused),
                "the focused window draws: {:?}",
                row.replayed_groups_for_test()
            );
            // A pan frame that draws every window keeps the drawings: the
            // one after replays again.
            let _ = draw_as(&mut row, text, &theme, PanFrame::Kept);
            assert_eq!(take_replayed(), 0, "a kept frame draws every window");
            row.set_scroll_x_for_test(140.0);
            relayout_strip(&mut row, 800.0);
            let _ = draw(&mut row, text, &theme, true);
            assert!(
                take_replayed() > 0,
                "drawings outlive a pan frame that drew every window"
            );
        }
    }

    /// Each window group of a strip drawn in canvas coordinates is a damage
    /// part of the strip's drawing: drawn again, panned (groups off screen
    /// replaying their drawings) or held still, the strip shows no damage,
    /// and a change to one group damages only where that group can paint.
    #[test]
    fn each_group_of_a_strip_drawn_in_canvas_coordinates_is_a_damage_part() {
        fn draw(
            row: &mut GroupRow,
            text: &mut TextRenderer,
        ) -> (Scene, crate::scene_parts::SectionParts) {
            let mut scene = Scene::new();
            let ((), parts) = crate::scene_parts::record(&mut scene, |scene| {
                row.draw(
                    scene,
                    Affine::IDENTITY,
                    text,
                    &Theme::dark(),
                    &PresenceCtx::empty(),
                );
            });
            (scene, parts.expect("the strip's parts are well nested"))
        }
        let text = &mut TextRenderer::new();
        let mut row = strip_at(8, 800.0, 0.0, 1000);
        relayout_strip(&mut row, 800.0);
        let _scope = crate::retained_present::RetainedScene::begin(true);
        let (first, first_parts) = draw(&mut row, text);
        let (again, again_parts) = draw(&mut row, text);
        assert_eq!(
            crate::scene_parts::damage(&first, &first_parts, &again, &again_parts),
            Ok(Vec::new())
        );
        row.set_scroll_x_for_test(1300.0);
        relayout_strip(&mut row, 800.0);
        let (panned, panned_parts) = draw(&mut row, text);
        assert!(
            !row.culled_groups.is_empty(),
            "groups off screen replay their drawings"
        );
        assert_eq!(
            crate::scene_parts::damage(&first, &first_parts, &panned, &panned_parts),
            Ok(Vec::new()),
            "a replayed group marks its part again"
        );
        let index = 5;
        let focused = row.groups[index].focused();
        let _ = row.groups[index].set_focused(!focused);
        let (changed, changed_parts) = draw(&mut row, text);
        let damage = crate::scene_parts::damage(&panned, &panned_parts, &changed, &changed_parts)
            .expect("a group bounds its change");
        assert!(!damage.is_empty(), "the group's focus edge changed");
        let shift = row.canvas_shift();
        let group = row.groups[index]
            .get_rect()
            .inflate(GROUP_SPRITE_REACH_PX, 0.0);
        for rect in damage {
            assert!(
                rect.x0 >= group.x0 + shift - 1e-6 && rect.x1 <= group.x1 + shift + 1e-6,
                "{rect:?} lies where group {index} paints ({group:?} shifted by {shift})"
            );
        }
    }

    /// A drawing of a client's GPU buffer (an image with no CPU pixels) is
    /// told apart from one of an image that carries its pixels.
    #[test]
    fn external_pixels_are_told_apart_from_carried_ones() {
        use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
        let image = |data: Vec<u8>| ImageData {
            data: Blob::new(std::sync::Arc::new(data)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        };
        let drawn = |image: &ImageData| {
            let mut scene = Scene::new();
            scene.draw_image(image, Affine::IDENTITY);
            scene
        };
        assert!(draws_external_pixels(&drawn(&image(Vec::new()))));
        assert!(!draws_external_pixels(&drawn(&image(vec![0; 16]))));
        assert!(!draws_external_pixels(&Scene::new()));
    }

    fn set_strip_permille(row: &mut GroupRow, permille: u32) {
        for g in &mut row.groups {
            g.set_width(WindowWidth::Fraction(permille));
        }
    }

    #[test]
    fn direct_pan_keeps_trailing_room_and_elastic_overpull() {
        fn wheel_over_strip_never_moves_it() {
            use winit::dpi::PhysicalPosition;

            for (mut row, taffy, root) in [
                make_overflow_row(8, 1200.0),
                make_row_with_groups_with_taffy(&[&[1], &[10]]),
            ] {
                let pre_rect_0 = row.groups[0].get_rect();
                let cy = f64::midpoint(row.rect.y0, row.rect.y1);
                let cx = f64::midpoint(row.rect.x0, row.rect.x1);
                let dev = winit::event::DeviceId::dummy();
                let mut handled = dispatch_at(
                    &mut row,
                    &taffy,
                    root,
                    &WindowEvent::CursorMoved {
                        device_id: dev,
                        position: PhysicalPosition::new(cx, cy),
                    },
                );
                handled |= dispatch_at(
                    &mut row,
                    &taffy,
                    root,
                    &WindowEvent::MouseWheel {
                        device_id: dev,
                        delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                        phase: winit::event::TouchPhase::Moved,
                    },
                );
                assert!(!handled, "a bare wheel must be left unclaimed by the strip");
                row.tick_snap_at(crate::motion::frame_now() + Duration::from_millis(400));
                assert_eq!(row.scroll_x(), 0.0, "a bare wheel must not move the strip");
                row.apply_layout(&taffy, root, Point::ORIGIN);
                assert_eq!(
                    row.groups[0].get_rect().x0,
                    pre_rect_0.x0,
                    "group rects must not shift after a bare wheel"
                );
            }
        }

        fn direct_pan_keeps_a_fifth_viewport_after_the_last_app() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_content_w_for_test(2200.0);

            assert_eq!(
                row.max_scroll_x(),
                1200.0,
                "focus-follow stays content-tight"
            );
            assert_eq!(
                row.pan_max_scroll_x(),
                1400.0,
                "direct pan reveals 20% of the viewport beyond the last app"
            );

            row.set_content_w_for_test(700.0);
            assert_eq!(
                row.pan_max_scroll_x(),
                0.0,
                "a strip which already fits must not drift into empty canvas"
            );
        }

        fn direct_pan_tracks_reversals_and_only_settles_elastic_overpull() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_content_w_for_test(2200.0);

            let t0 = crate::time::now();
            let at = |ms: u64| t0 + Duration::from_millis(ms);
            assert!(row.update_strip_pan(450.0, at(0)));
            assert!((row.scroll_x() - 450.0).abs() < 0.01);
            assert!(row.update_strip_pan(320.0, at(500)));
            assert!(
                (row.scroll_x() - 320.0).abs() < 0.01,
                "total gesture travel must reverse without rebasing its origin"
            );
            assert!(
                !row.end_strip_pan(at(600)),
                "an in-range release at rest must not move away from the fingers"
            );
            assert!((row.scroll_x() - 320.0).abs() < 0.01);

            // A new physical stream explicitly rebases, so a missing End at a
            // lock/session boundary cannot carry old total travel into it.
            row.begin_strip_pan();
            assert!(row.update_strip_pan(10.0, at(700)));
            assert!((row.scroll_x() - 330.0).abs() < 0.01);
            let _ = row.end_strip_pan(at(800));
        }

        fn direct_pan_overpull_has_diminishing_gain_and_returns_to_resting_end() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_content_w_for_test(2200.0);
            let end = row.pan_max_scroll_x();

            let t0 = crate::time::now();
            assert!(row.update_strip_pan(end, t0));
            let mut previous = row.scroll_x();
            let mut previous_gain = f64::INFINITY;
            let mut at = t0;
            for extra in [100.0, 200.0, 300.0, 400.0] {
                at += Duration::from_millis(16);
                assert!(row.update_strip_pan(end + extra, at));
                let gain = row.scroll_x() - previous;
                assert!(
                    gain > 0.0 && gain < previous_gain,
                    "each equal pull must still move, but less than the one before: {gain} >= {previous_gain}"
                );
                assert!(
                    row.scroll_x() < end + 280.0,
                    "the 28%-viewport asymptote is never crossed"
                );
                previous = row.scroll_x();
                previous_gain = gain;
            }

            let now = at + Duration::from_millis(8);
            assert!(row.end_strip_pan(now));
            row.tick_snap_at(now + Duration::from_millis(100));
            assert!(
                row.scroll_x() > end && row.scroll_x() < end + 280.0,
                "the settle stays inside the rubber band on its way back: {}",
                row.scroll_x()
            );
            row.tick_snap_at(now + Duration::from_secs(1));
            assert!(
                (row.scroll_x() - end).abs() < 0.01,
                "release must return exactly to the legal trailing rest"
            );
        }

        wheel_over_strip_never_moves_it();
        direct_pan_keeps_a_fifth_viewport_after_the_last_app();
        direct_pan_tracks_reversals_and_only_settles_elastic_overpull();
        direct_pan_overpull_has_diminishing_gain_and_returns_to_resting_end();
    }

    /// A window's edge is drawn just outside its rect, so a reveal that
    /// stopped flush with the screen edge cut the window's border and
    /// rounded corners off. Every reveal leaves at least the gap between
    /// two windows, and the strip's first and last windows their gutter.
    #[test]
    fn reveals_keep_windows_clear_of_the_screen_edges() {
        let gap = HANDLE_SLOT_PX;
        // Viewport 0..1000 with a 96 px gutter, so the first group lies at
        // canvas 96..696, the second at 704..1304, the last ends at 2400
        // and the strip's end, the gutter after it, is scroll 1496.
        let pan = Pan {
            viewport: Rect::new(0.0, 0.0, 1000.0, 600.0),
            inset: 96.0,
            scroll_x: 400.0,
            content_w: 2400.0,
            ..Pan::default()
        };
        // Scrolled 400, the first group sits at -304..296: half visible.
        let leftmost = Rect::new(-304.0, 0.0, 296.0, 600.0);
        let target = pan
            .scroll_target_for(leftmost)
            .expect("a cut group is revealed");
        assert!(
            target.abs() < 0.01,
            "home (0) with the gutter before it, not flush with the screen \
             edge (96) with its border cut off: {target}"
        );
        // Fully visible already: nothing to do.
        assert_eq!(
            pan.scroll_target_for(Rect::new(10.0, 0.0, 610.0, 600.0)),
            None
        );
        // Fully visible but closer to an edge than the gap: it moves clear.
        assert_eq!(
            pan.scroll_target_for(Rect::new(3.0, 0.0, 603.0, 600.0)),
            Some(400.0 - (gap - 3.0))
        );
        // A group in the middle cut on the right stops the gap before it.
        let target = pan
            .scroll_target_for(Rect::new(
                704.0 - 400.0 + 500.0,
                0.0,
                1404.0 - 400.0 + 500.0,
                600.0,
            ))
            .expect("a group cut on the right is revealed");
        assert!((target - (904.0 + gap)).abs() < 0.01, "{target}");
        // And one cut on the left the gap after the left edge.
        let target = pan
            .scroll_target_for(Rect::new(-200.0, 0.0, 400.0, 600.0))
            .expect("a group cut on the left is revealed");
        assert!((target - (200.0 - gap)).abs() < 0.01, "{target}");
        // A group as wide as the screen has no room for a gap: it fills it.
        let target = pan
            .scroll_target_for(Rect::new(-100.0, 0.0, 900.0, 600.0))
            .expect("a full-width group is revealed");
        assert!((target - 300.0).abs() < 0.01, "{target}");
        // The last group, 1800..2400, keeps the gutter after it.
        let target = pan
            .scroll_target_for(Rect::new(1800.0 - 400.0, 0.0, 2400.0 - 400.0, 600.0))
            .expect("the last group is revealed");
        assert!(
            (target - 1496.0).abs() < 0.01,
            "the strip's end with its gutter: {target}"
        );
    }

    /// Without a gutter the last window's reveal stops at the strip's end,
    /// where the pan rests once nothing moves it: easing on past it, into
    /// the canvas a person's own pan may reach, it was pulled back the
    /// moment the ease ended.
    #[test]
    fn revealing_the_last_window_without_a_gutter_stops_at_the_strip_end() {
        let pan = Pan {
            viewport: Rect::new(0.0, 0.0, 1000.0, 600.0),
            scroll_x: 400.0,
            content_w: 2400.0,
            ..Pan::default()
        };
        let target = pan
            .scroll_target_for(Rect::new(1800.0 - 400.0, 0.0, 2400.0 - 400.0, 600.0))
            .expect("the last group is revealed");
        assert!(
            (target - pan.max_scroll_x()).abs() < 0.01,
            "the strip's end ({}), not past it: {target}",
            pan.max_scroll_x()
        );
    }

    #[test]
    fn wheel_idle_snaps_to_nearest_group_once() {
        fn scroll_snaps_to_nearest_group_after_wheel_idle() {
            // Synthesised directly (three 600px groups, 8px gap → edges at
            // 0, 608, 1216) rather than depending on taffy's flex math.
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
            row.set_content_w_for_test(1816.0);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            // 400 is closer to 608 than to 0: exercises the second group's edge.
            row.set_scroll_x_for_test(400.0);
            let base = crate::time::now();
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

        fn snap_does_not_fire_during_wheel_idle_window() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
            row.set_content_w_for_test(1816.0);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_scroll_x_for_test(400.0);
            let base = crate::time::now();
            // Only 100 ms since the last wheel — inside the idle window.
            row.set_last_wheel_at_for_test(earlier(base, 100));

            row.tick_snap_at(base);
            assert_eq!(
                row.scroll_x(),
                400.0,
                "snap must not fire while still inside the idle window",
            );
        }

        fn wheel_mid_snap_leaves_the_snap_untouched() {
            use winit::dpi::PhysicalPosition;

            // 8 groups → sum-of-mins exceeds the 1200px viewport.
            let (mut row, taffy, root) = make_overflow_row(8, 1200.0);
            row.set_scroll_x_for_test(300.0);
            let base = crate::time::now();
            row.set_last_wheel_at_for_test(earlier(base, 250));
            row.tick_snap_at(base);
            assert!(
                row.pan.motion.is_some(),
                "precondition: snap must be in flight",
            );

            // Cursor must be inside the row's rect for the handler to claim it.
            let cy = f64::midpoint(row.rect.y0, row.rect.y1);
            let cx = f64::midpoint(row.rect.x0, row.rect.x1);
            let dev = winit::event::DeviceId::dummy();
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: PhysicalPosition::new(cx, cy),
                },
            );
            dispatch_at(
                &mut row,
                &taffy,
                root,
                &WindowEvent::MouseWheel {
                    device_id: dev,
                    delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            let anim = row
                .pan
                .motion
                .expect("the in-flight snap must survive a bare wheel notch");
            // Was easing toward the stop nearest 300; must not be re-targeted.
            let edges = row.group_left_edges_for_test().to_vec();
            let expected = edges
                .iter()
                .copied()
                .min_by(|a, b| (a - 300.0).abs().partial_cmp(&(b - 300.0).abs()).unwrap())
                .unwrap();
            assert!(
                (anim.target() - expected).abs() < 0.5,
                "the snap must still target the stop nearest its origin \
             ({expected}); got {}",
                anim.target()
            );
            row.tick_snap_at(crate::motion::frame_now() + Duration::from_millis(400));
            assert!(
                (row.scroll_x() - expected).abs() < 0.5,
                "the snap must settle on its original target ({expected}); \
             got {}",
                row.scroll_x()
            );
        }

        /// Reduced motion: the snap lands on the tick it starts, whether it
        /// follows a wheel's idle or a released direct pan.
        fn reduced_motion_snaps_land_on_their_first_tick() {
            let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
            row.reduce_motion = true;
            row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
            row.set_content_w_for_test(1816.0);
            row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
            row.set_scroll_x_for_test(400.0);
            let base = crate::time::now();
            row.set_last_wheel_at_for_test(earlier(base, 250));
            row.tick_snap_at(base);
            assert!(
                (row.scroll_x() - 608.0).abs() < 0.01,
                "the idle snap must land at once; got {}",
                row.scroll_x()
            );
            assert!(!row.snap_in_flight(), "and nothing is left in flight");

            row.begin_strip_pan();
            assert!(row.update_strip_pan(-800.0, base));
            assert!(row.scroll_x() < 0.0, "fixture: the pan overpulls");
            assert!(row.end_strip_pan(base));
            row.tick_snap_at(base);
            assert!(
                row.scroll_x().abs() < 0.01,
                "a released overpull must settle at once; got {}",
                row.scroll_x()
            );
        }

        scroll_snaps_to_nearest_group_after_wheel_idle();
        snap_does_not_fire_during_wheel_idle_window();
        wheel_mid_snap_leaves_the_snap_untouched();
        reduced_motion_snaps_land_on_their_first_tick();
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

    // ==================================================================
    // Seams, and resizing windows from their edges (`edge_resize`)
    // ==================================================================

    #[test]
    fn the_seam_pool_tracks_the_gaps() {
        /// Pins the `build_layout` seam policy so a refactor can't
        /// silently leak or under-allocate them.
        fn handle_pool_matches_inter_group_gap_count() {
            let row = make_row_with_groups(&[&[1]]);
            assert_eq!(row.handle_count(), 0, "single-group row needs no seam");
            let row = make_row_with_groups(&[&[1], &[2]]);
            assert_eq!(row.handle_count(), 1, "two-group row has exactly one");
            let row = make_row_with_groups(&[&[1], &[2], &[3]]);
            assert_eq!(row.handle_count(), 2, "three-group row has exactly two");
        }

        fn handle_pool_grows_on_split_into_new_group() {
            let mut row = make_row_with_groups(&[&[1, 2]]);
            assert_eq!(row.handle_count(), 0, "precondition: one group, no seam");
            assert!(row.split_tab_into_new_group(0, 2, 1));
            assert_eq!(row.groups.len(), 2);
            assert!(
                row.take_structure_dirty(),
                "split must flag structure_dirty"
            );
            let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
            assert_eq!(row.handle_count(), 1, "after split: groups.len() - 1");
            let r = row.handle_at(0).expect("seam 0").get_rect();
            assert!(r.width() > 0.0 && r.height() > 0.0, "laid out: {r:?}");
        }

        handle_pool_matches_inter_group_gap_count();
        handle_pool_grows_on_split_into_new_group();
    }

    #[test]
    fn a_lone_windows_edge_resizes_it_rather_than_moving_it() {
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1]]);
        let group_rect = row.groups[0].get_rect();
        let grab = Point::new(group_rect.x1 - 2.0, group_rect.center().y);
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(grab));
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Pressed)
        ));
        assert!(
            row.wants_pointer_capture(),
            "the lone window's resize must own pointer capture",
        );
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &move_to(Point::new(grab.x - 80.0, grab.y))
        ));
        assert_eq!(
            row.groups[0].layout_width_px(),
            group_rect.width() - 80.0,
            "dragging the lone trailing edge resizes rather than reorders the window",
        );
        assert!(!row.group_drag_active());
    }

    /// An app with a real minimum width, recording the edge it is told
    /// holds still while its window is resized.
    struct MinWidthApp {
        min: f64,
        anchor: std::rc::Rc<std::cell::Cell<(Option<WindowEdge>, bool)>>,
        rect: Rect,
    }

    impl Widget for MinWidthApp {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.fill_leaf(node_map, self)
        }
        crate::rect_geometry!();
        fn name(&self) -> &'static str {
            "MinWidthApp"
        }
    }

    impl crate::widget::AppWidget for MinWidthApp {
        fn uses_host_record(&self) -> bool {
            false
        }
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Files"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("F")
        }
        fn min_window_width(&self) -> Option<f64> {
            Some(self.min)
        }
        fn set_live_resize_anchor(&mut self, fixed: Option<WindowEdge>) {
            self.anchor.set((fixed, false));
        }
        fn release_live_resize_anchor(&mut self) {
            let (fixed, _) = self.anchor.get();
            self.anchor.set((fixed, true));
        }
    }

    /// A strip of three 400 px windows (a third of the 1200 px display
    /// each) on a 2000 px viewport, with window `focused` the one in use.
    /// `middle`, when given, stands in for the middle window.
    fn edge_strip(
        focused: Option<usize>,
        middle: Option<WindowGroup>,
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        let mut groups = vec![
            make_group(100, &[1]),
            make_group(200, &[2]),
            make_group(300, &[3]),
        ];
        if let Some(middle) = middle {
            groups[1] = middle;
        }
        for g in &mut groups {
            g.set_width(WindowWidth::Narrow);
        }
        let mut row = GroupRow::new(groups);
        row.set_output_spans(&one_display(FIXTURE_USABLE_PX));
        if let Some(i) = focused {
            let _ = row.groups[i].set_focused(true);
        }
        let (row, _, _) = relayout_row_owned(row, 2000.0);
        relayout_row_owned(row, 2000.0)
    }

    fn files_window(
        min: f64,
    ) -> (
        WindowGroup,
        std::rc::Rc<std::cell::Cell<(Option<WindowEdge>, bool)>>,
    ) {
        let anchor = std::rc::Rc::new(std::cell::Cell::new((None, false)));
        let group = WindowGroup::new(
            200,
            vec![(
                TabDescriptor::new(2, "Files"),
                TileContent::App(Box::new(MinWidthApp {
                    min,
                    anchor: std::rc::Rc::clone(&anchor),
                    rect: Rect::ZERO,
                })),
            )],
            2,
            WindowWidth::Narrow,
        );
        (group, anchor)
    }

    fn edge_event(
        row: &mut GroupRow,
        taffy: &TaffyTree<()>,
        root: NodeId,
        shift: bool,
        event: &WindowEvent,
    ) -> bool {
        let modifiers = if shift {
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
        } else {
            winit::event::Modifiers::default()
        };
        Dispatch::new()
            .modifiers(modifiers)
            .layout(taffy, root)
            .send(row, event)
            .handled
    }

    fn move_to(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }

    fn rects(row: &GroupRow) -> Vec<Rect> {
        row.groups.iter().map(WindowGroup::get_rect).collect()
    }

    /// Press at `from`, carry the pointer through `steps` (each relative to
    /// `from`), laying the strip out after every move as the frame loop
    /// does, and return the row (still pressed) and the rects after each
    /// step, the press's own frame first.
    fn drag_edge(
        row: GroupRow,
        taffy: &TaffyTree<()>,
        root: NodeId,
        from: Point,
        steps: &[f64],
        shift: bool,
    ) -> (GroupRow, TaffyTree<()>, NodeId, Vec<Vec<Rect>>) {
        let mut row = row;
        let _ = edge_event(&mut row, taffy, root, shift, &move_to(from));
        assert!(
            edge_event(&mut row, taffy, root, shift, &button(ElementState::Pressed)),
            "a press on an edge is the resize's, not the app's"
        );
        assert!(row.edge_resize_in_flight());
        let (mut row, mut taffy, mut root) = relayout_row_owned(row, 2000.0);
        let mut frames = vec![rects(&row)];
        for dx in steps {
            let at = Point::new(from.x + dx, from.y);
            assert!(edge_event(&mut row, &taffy, root, shift, &move_to(at)));
            (row, taffy, root) = relayout_row_owned(row, 2000.0);
            frames.push(rects(&row));
        }
        (row, taffy, root, frames)
    }

    fn widths(frame: &[Rect]) -> Vec<f64> {
        frame.iter().map(|r| r.width()).collect()
    }

    /// The pointer shape every host shows ([`crate::widget::cursor`]): a
    /// window edge, outer ones a little past the window too, says it
    /// resizes, and a drag keeps saying so wherever the pointer goes.
    #[test]
    fn a_window_edge_shows_the_resize_cursor_through_the_shared_rule() {
        use crate::widget::cursor::in_tree;
        use winit::window::CursorIcon;
        let (mut row, taffy, root) = edge_strip(None, None);
        let r = rects(&row);
        let y = r[0].center().y;
        let seam = Point::new(r[0].x1 - 2.0, y);
        assert_eq!(in_tree(&row, seam), Some(CursorIcon::EwResize));
        let outer = Point::new(r[2].x1 + crate::widget::RESIZE_HANDLE_PX - 1.0, y);
        assert_eq!(
            in_tree(&row, outer),
            Some(CursorIcon::EwResize),
            "an outer edge reaches out over the empty canvas"
        );
        let body = Point::new(r[1].center().x, y);
        assert_ne!(in_tree(&row, body), Some(CursorIcon::EwResize));
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(seam));
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Pressed)
        ));
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(body));
        assert_eq!(
            in_tree(&row, body),
            Some(CursorIcon::EwResize),
            "the drag holds the resize cursor off the edge it began on"
        );
    }

    #[test]
    fn the_seam_belongs_to_the_focused_window_and_hovering_shows_whose_edge_it_is() {
        {
            let (mut row, taffy, root) = edge_strip(None, None);
            let r = rects(&row);
            let y = r[0].center().y;
            let _ = edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &move_to(Point::new(r[0].x1 - 2.0, y)),
            );
            assert_eq!(
                row.edge_hover(),
                Some((
                    EdgeGrab {
                        group: 0,
                        edge: WindowEdge::Trailing
                    },
                    false
                ))
            );
            assert_eq!(row.groups[0].edge_highlight(), Some(WindowEdge::Trailing));
            assert_eq!(row.groups[1].edge_highlight(), None);
            assert_eq!(
                row.cursor_shape(Point::new(r[0].x1 - 2.0, y)),
                Some(winit::window::CursorIcon::EwResize)
            );
            let _ = edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &move_to(Point::new(r[1].x0 + 2.0, y)),
            );
            assert_eq!(
                row.edge_hover(),
                Some((
                    EdgeGrab {
                        group: 1,
                        edge: WindowEdge::Leading
                    },
                    false
                ))
            );
            assert_eq!(row.groups[0].edge_highlight(), None, "the highlight moves");
            assert_eq!(row.groups[1].edge_highlight(), Some(WindowEdge::Leading));
        }
        // Phase `the_window_in_use_owns_the_whole_seam`: the pointer in the
        // terminal's half of the seam still names Files' left edge, because
        // Files is the window the person is in.
        {
            let (mut row, taffy, root) = edge_strip(Some(1), None);
            let r = rects(&row);
            let y = r[0].center().y;
            for x in [
                r[0].x1 - 5.0,
                r[0].x1 - 1.0,
                f64::midpoint(r[0].x1, r[1].x0),
                r[1].x0 + 2.0,
            ] {
                let _ = edge_event(&mut row, &taffy, root, false, &move_to(Point::new(x, y)));
                assert_eq!(
                    row.edge_hover(),
                    Some((
                        EdgeGrab {
                            group: 1,
                            edge: WindowEdge::Leading
                        },
                        false
                    )),
                    "x = {x}"
                );
                assert_eq!(row.groups[0].edge_highlight(), None, "x = {x}");
                assert_eq!(row.groups[1].edge_highlight(), Some(WindowEdge::Leading));
            }
            // With Shift held both windows at the seam are shown changing.
            let _ = edge_event(
                &mut row,
                &taffy,
                root,
                true,
                &move_to(Point::new(r[0].x1 - 2.0, y)),
            );
            assert_eq!(row.groups[0].edge_highlight(), Some(WindowEdge::Trailing));
            assert_eq!(row.groups[1].edge_highlight(), Some(WindowEdge::Leading));
            assert_eq!(row.groups[2].edge_highlight(), None);
            let _ = edge_event(&mut row, &taffy, root, false, &move_to(r[1].center()));
            assert_eq!(row.edge_hover(), None);
            assert!(row.groups.iter().all(|g| g.edge_highlight().is_none()));
        }
    }

    #[test]
    fn a_plain_drag_resizes_only_the_grabbed_window_and_holds_its_opposite_edge() {
        // Phase `left_edge_grows_left_and_pushes_the_window_before_it`:
        // Files (the middle window, in use) grabbed on its left edge, in
        // the terminal's half of the seam, and dragged left.
        {
            let (row, taffy, root) = edge_strip(Some(1), None);
            let before = rects(&row);
            let from = Point::new(before[0].x1 - 2.0, before[0].center().y);
            let (mut row, taffy, root, frames) =
                drag_edge(row, &taffy, root, from, &[-20.0, -40.0, 40.0], false);
            assert_eq!(frames[0], before, "no jump at the press");
            for (frame, dx) in frames[1..].iter().zip([-20.0, -40.0, 40.0]) {
                assert_eq!(
                    widths(frame),
                    vec![400.0, 400.0 - dx, 400.0],
                    "only the grabbed window changes width (dx {dx})"
                );
                assert!(
                    (frame[1].x1 - before[1].x1).abs() < 1e-6,
                    "its right edge holds still on screen (dx {dx}): {frame:?}"
                );
                assert!(
                    (frame[1].x0 - (before[1].x0 + dx)).abs() < 1e-6,
                    "its left edge stays under the pointer (dx {dx})"
                );
                assert!(
                    (frame[0].x1 - (before[0].x1 + dx)).abs() < 1e-6,
                    "the window before it slides with the edge at its own width"
                );
                assert_eq!(frame[2], before[2], "the window after it does not move");
            }
            assert!(edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &button(ElementState::Released)
            ));
            assert!(!row.edge_resize_in_flight());
            assert_eq!(
                row.groups[1].take_pending_width_change(),
                Some(WindowWidth::Fraction(crate::widget::permille_for_px(
                    360.0,
                    FIXTURE_USABLE_PX
                )))
            );
            assert_eq!(row.groups[0].take_pending_width_change(), None);
            assert_eq!(row.groups[2].take_pending_width_change(), None);
        }
        {
            let (row, taffy, root) = edge_strip(Some(1), None);
            let before = rects(&row);
            let from = Point::new(before[2].x0 + 2.0, before[1].center().y);
            let (_row, _taffy, _root, frames) =
                drag_edge(row, &taffy, root, from, &[30.0, -50.0], false);
            for (frame, dx) in frames[1..].iter().zip([30.0, -50.0]) {
                assert_eq!(widths(frame), vec![400.0, 400.0 + dx, 400.0], "dx {dx}");
                assert!((frame[1].x0 - before[1].x0).abs() < 1e-6, "left edge holds");
                assert!((frame[2].x0 - (before[2].x0 + dx)).abs() < 1e-6, "pushed");
                assert_eq!(frame[0], before[0]);
            }
        }
        {
            let (row, taffy, root) = edge_strip(None, None);
            let before = rects(&row);
            let from = Point::new(before[0].x1 - 2.0, before[0].center().y);
            let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[25.0], false);
            assert_eq!(widths(&frames[1]), vec![425.0, 400.0, 400.0]);
            assert_eq!(frames[1][0].x0, before[0].x0);
        }
    }

    #[test]
    fn shift_drag_trades_width_between_the_two_windows_at_the_seam_only() {
        let (row, taffy, root) = edge_strip(Some(1), None);
        let before = rects(&row);
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (mut row, taffy, root, frames) = drag_edge(row, &taffy, root, from, &[-60.0], true);
        assert_eq!(widths(&frames[1]), vec![340.0, 460.0, 400.0]);
        assert_eq!(frames[1][0].x0, before[0].x0, "the outer edges stay put");
        assert!((frames[1][1].x1 - before[1].x1).abs() < 1e-6);
        assert_eq!(frames[1][2], before[2]);
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Released)
        ));
        assert!(row.groups[0].take_pending_width_change().is_some());
        assert!(row.groups[1].take_pending_width_change().is_some());
        assert_eq!(row.groups[2].take_pending_width_change(), None);
    }

    #[test]
    fn after_releasing_a_resize_no_window_moves() {
        // Narrow enough that the strip can pan while all three windows fit.
        const VIEWPORT: f64 = 1400.0;
        let (row, _, _) = edge_strip(Some(1), None);
        let (mut row, _, _) = relayout_row_owned(row, VIEWPORT);
        // Panned so the first window is cut off at the left edge.
        row.set_scroll_x_for_test(60.0);
        let (mut row, taffy, root) = relayout_row_owned(row, VIEWPORT);
        let before = rects(&row);
        assert!(
            before[2].x1 - before[0].x0 <= VIEWPORT,
            "fixture: every window fits on screen: {before:?}"
        );
        assert!(
            before[0].x0 < row.get_rect().x0,
            "fixture: the first window is cut off: {before:?}"
        );
        // The window in use grabbed at its right edge and widened, still
        // with room for all three on screen.
        let from = Point::new(before[2].x0 + 2.0, before[1].center().y);
        let _ = edge_event(&mut row, &taffy, root, false, &move_to(from));
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Pressed)
        ));
        let (mut row, taffy, root) = relayout_row_owned(row, VIEWPORT);
        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &move_to(Point::new(from.x + 20.0, from.y))
        ));
        let (mut row, taffy, root) = relayout_row_owned(row, VIEWPORT);
        let held = rects(&row);
        assert!(
            (held[1].width() - before[1].width() - 20.0).abs() < 1e-6,
            "fixture: the drag widened the window in use: {held:?}"
        );
        let scroll = row.scroll_x();

        assert!(edge_event(
            &mut row,
            &taffy,
            root,
            false,
            &button(ElementState::Released)
        ));
        assert!(!row.edge_resize_in_flight());
        for _ in 0..3 {
            let (laid, _, _) = relayout_row_owned(row, VIEWPORT);
            row = laid;
        }
        assert!(
            (row.scroll_x() - scroll).abs() < 1e-6,
            "the strip stays where the drag left it: {scroll} became {}",
            row.scroll_x()
        );
        assert!(!row.snap_in_flight(), "and nothing is on its way anywhere");
        for (i, (after, held)) in rects(&row).iter().zip(&held).enumerate() {
            assert!(
                (after.x0 - held.x0).abs() < 0.5 && (after.x1 - held.x1).abs() < 0.5,
                "window {i} moved after the release: {held:?} became {after:?}"
            );
        }
    }

    /// An app that states its minimum only through its widget tree, as a
    /// `NavShell` does ([`Widget::minimum_window_width`]), and nothing on
    /// `AppWidget`: an app running in the shell's own process.
    struct TreeMinimumApp {
        min: f64,
        rect: Rect,
    }

    impl Widget for TreeMinimumApp {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.fill_leaf(node_map, self)
        }
        crate::rect_geometry!();
        fn name(&self) -> &'static str {
            "TreeMinimumApp"
        }
        fn minimum_window_width(&self) -> Option<f64> {
            Some(self.min)
        }
    }

    impl crate::widget::AppWidget for TreeMinimumApp {
        fn uses_host_record(&self) -> bool {
            false
        }
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Files"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("F")
        }
    }

    /// The strip's resize stops at the minimum an in-process app's layout
    /// states (its compact layout), with no `AppWidget` override.
    #[test]
    fn an_in_process_apps_layout_minimum_stops_the_edge() {
        let group = WindowGroup::new(
            200,
            vec![(
                TabDescriptor::new(2, "Files"),
                TileContent::App(Box::new(TreeMinimumApp {
                    min: 340.0,
                    rect: Rect::ZERO,
                })),
            )],
            2,
            WindowWidth::Narrow,
        );
        let (row, taffy, root) = edge_strip(Some(1), Some(group));
        assert_eq!(row.groups[1].width_limits().min, 340.0);
        let before = rects(&row);
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[150.0], false);
        assert_eq!(widths(&frames[1])[1], 340.0, "stops at the app's minimum");
    }

    #[test]
    fn a_limit_stops_the_edge_and_resizes_nothing_else() {
        {
            let (files, anchor) = files_window(320.0);
            let (row, taffy, root) = edge_strip(Some(1), Some(files));
            let before = rects(&row);
            let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
            let (mut row, taffy, root, frames) =
                drag_edge(row, &taffy, root, from, &[150.0, 600.0], false);
            assert_eq!(
                anchor.get(),
                (Some(WindowEdge::Trailing), false),
                "the app hears that its right edge holds while its left one moves"
            );
            assert_eq!(
                widths(&frames[1]),
                vec![400.0, 320.0, 400.0],
                "stops at 320"
            );
            assert_eq!(
                widths(&frames[2]),
                vec![400.0, 320.0, 400.0],
                "and stays there"
            );
            assert!(
                (frames[2][1].x1 - before[1].x1).abs() < 1e-6,
                "right edge holds"
            );
            assert!(edge_event(
                &mut row,
                &taffy,
                root,
                false,
                &button(ElementState::Released)
            ));
            assert!(anchor.get().1, "and that the drag ended");
        }
        {
            let (row, taffy, root) = edge_strip(Some(1), None);
            let before = rects(&row);
            let from = Point::new(before[1].x1 - 2.0, before[1].center().y);
            let (_row, _taffy, _root, frames) =
                drag_edge(row, &taffy, root, from, &[-900.0], false);
            assert_eq!(
                widths(&frames[1]),
                vec![400.0, min_group_px(FIXTURE_USABLE_PX), 400.0]
            );
        }
        {
            let (files, _anchor) = files_window(320.0);
            let (row, taffy, root) = edge_strip(Some(1), Some(files));
            let before = rects(&row);
            let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
            let (_row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[200.0], true);
            assert_eq!(widths(&frames[1]), vec![480.0, 320.0, 400.0]);
        }
    }

    #[test]
    fn escape_puts_the_windows_and_the_strip_back() {
        let (row, taffy, root) = edge_strip(Some(1), None);
        let before = rects(&row);
        let scroll = row.scroll_x();
        let from = Point::new(before[1].x0 + 2.0, before[1].center().y);
        let (mut row, taffy, root, _) = drag_edge(row, &taffy, root, from, &[-80.0], false);
        let escape = crate::synthetic_input::named_key(winit::keyboard::NamedKey::Escape);
        assert!(edge_event(&mut row, &taffy, root, false, &escape));
        assert!(!row.edge_resize_in_flight());
        assert_eq!(row.scroll_x(), scroll);
        let (mut row, _, _) = relayout_row_owned(row, 2000.0);
        assert_eq!(rects(&row), before);
        assert!(
            row.groups
                .iter_mut()
                .all(|g| g.take_pending_width_change().is_none())
        );
    }

    #[test]
    fn resting_at_the_display_edge_auto_scrolls_and_the_edge_stays_under_the_pointer() {
        let (row, taffy, root) = edge_strip(Some(2), None);
        let before = rects(&row);
        let viewport = row.get_rect();
        let from = Point::new(before[2].x1 - 2.0, before[2].center().y);
        // Into the edge band, 4 px from the display's right edge.
        let to = viewport.x1 - 4.0 - from.x;
        let (mut row, _taffy, _root, frames) = drag_edge(row, &taffy, root, from, &[to], false);
        let width = frames[1][2].width();
        let t0 = crate::time::now();
        assert!(
            !row.advance_edge_autoscroll(t0),
            "the first tick only starts the clock"
        );
        assert!(row.advance_edge_autoscroll(t0 + Duration::from_millis(50)));
        let (row, _, _) = relayout_row_owned(row, 2000.0);
        let after = rects(&row);
        assert!(
            after[2].width() > width + 20.0,
            "it grew past the screen: {after:?}"
        );
        assert!(
            (after[2].x1 - (viewport.x1 - 4.0 + 2.0)).abs() < 1.0,
            "its right edge is still under the pointer: {after:?}"
        );
        assert_eq!(
            widths(&after)[..2],
            widths(&before)[..2],
            "no one else changed"
        );
    }

    #[test]
    fn maximize_and_fullscreen_absorb_the_row_and_restore_exactly() {
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
                r0.width() > 1.0,
                "left sibling keeps its last tiled rect so the fade can draw it; got {}",
                r0.width(),
            );
            assert!(
                r2.width() > 1.0,
                "right sibling keeps its last tiled rect so the fade can draw it; got {}",
                r2.width(),
            );

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

        fn maximized_group_drops_the_strip_pan_for_output_aligned_controls() {
            let mut row = strip_at(4, 1280.0, 248.0, 667);
            row.set_scroll_x_for_test(248.0);
            row.groups[3].maximized = true;

            relayout_strip(&mut row, 1280.0);

            let rect = row.groups[3].get_rect();
            assert!(
                (rect.x0 - row.rect.x0).abs() < 0.5,
                "maximized content and its Restore/Close controls must start at the output edge; group={rect:?} row={:?}",
                row.rect,
            );
            assert!(
                (rect.x1 - row.rect.x1).abs() < 0.5,
                "maximized content and its controls must end at the output edge; group={rect:?} row={:?}",
                row.rect,
            );
        }

        /// Same layout outcome as `maximized`, but via `set_fullscreen`
        /// (compositor's live state) with no group carrying `maximized`.
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
                row.groups[0].get_rect().width() > 1.0,
                "left sibling keeps its last tiled rect under fullscreen; got {}",
                row.groups[0].get_rect().width(),
            );
            assert!(
                row.groups[2].get_rect().width() > 1.0,
                "right sibling keeps its last tiled rect under fullscreen; got {}",
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

        maximized_group_hides_siblings_and_absorbs_row_width();
        unmaximize_restores_siblings_and_handles();
        maximized_group_drops_the_strip_pan_for_output_aligned_controls();
        fullscreen_group_collapses_siblings_and_absorbs_row();
        exiting_fullscreen_restores_prior_layout_exactly();
        hidden_row_collapses_and_restores();
    }

    #[test]
    fn maximize_fades_siblings_on_the_fast_curve_and_keeps_velocity() {
        // Fresh curves are read in the instant they start: a descheduled
        // runner cannot age them.
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            {
                let (row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
                let before = row.groups[0].get_rect().width();
                assert!(
                    (row.sibling_fade_value() - 1.0).abs() < 0.02,
                    "a tiled strip starts with siblings fully visible",
                );
                let mut row = row;
                row.groups[1].maximized = true;
                let (row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());
                let mut row = row;
                assert!(
                    row.sibling_fade_is_animating(),
                    "maximize must run the sibling fade, not snap Display::None",
                );
                assert!(
                    (row.groups[0].get_rect().width() - before).abs() < 1.0,
                    "the fading sibling keeps its last tiled width, was {before} now {}",
                    row.groups[0].get_rect().width(),
                );
                // EaseOut is front-loaded: 32ms of 130ms is still mid-opacity,
                // while 65ms is already ~0.12 and reads as "almost gone".
                row.advance_sibling_fade(Duration::from_millis(32));
                let mid = row.sibling_fade_value();
                assert!(
                    mid > 0.2 && mid < 0.8,
                    "a quarter of the way through fast the sibling is still on screen, got {mid}",
                );

                row.groups[1].maximized = false;
                let (row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());
                let after = row.sibling_fade_value();
                assert!(
                    (after - mid).abs() < 0.15,
                    "a new Super+M must keep the current alpha, was {mid} now {after}",
                );
                assert!(
                    row.sibling_fade_is_animating(),
                    "restore from mid-fade must keep pumping toward visible",
                );
            }

            {
                let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
                row.groups[1].maximized = true;
                let theme = Theme::dark().with_reduce_motion(true);
                let (row, ..) = relayout_row_owned_with(row, 2000.0, &theme);
                assert!(
                    row.sibling_fade_value() < 0.02,
                    "reduced motion hides siblings on the first frame, got {}",
                    row.sibling_fade_value(),
                );
                assert!(
                    !row.sibling_fade_is_animating(),
                    "reduced motion must not pump fade frames",
                );
            }
        });
    }

    /// A maximized window covers its strip: nothing of a neighbour paints
    /// over it. While its frame grows the neighbours fade out under it (they
    /// never cut out in one frame), and at rest it paints alone. The fade back
    /// in on restore keeps the row dirty until a frame drew it at rest, so
    /// a parent's subtree cache cannot keep a partway pose on screen.
    #[test]
    fn maximized_window_paints_alone_and_the_sibling_fade_redraws_to_rest() {
        fn encoded(row: &mut GroupRow) -> Vec<u32> {
            let mut scene = Scene::new();
            row.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            row.mark_drawn();
            scene.encoding().draw_tags.iter().map(|tag| tag.0).collect()
        }
        let (mut row, _t, _r) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[1].maximized = true;
        let (mut row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());

        row.advance_sibling_fade(Duration::from_secs(1));
        let settled = encoded(&mut row);
        // The first instant of the fade: the neighbours are still fully open.
        assert!(row.hold_sibling_fade_at(Duration::ZERO));
        let fading = encoded(&mut row);
        row.clear_sibling_fade_hold();
        row.advance_sibling_fade(Duration::from_secs(1));
        let settled_again = encoded(&mut row);
        assert_eq!(
            settled, settled_again,
            "the maximized window draws the same at rest"
        );
        assert!(
            fading.len() > settled.len(),
            "the neighbours fade out under the maximizing window, not cut"
        );
        assert!(
            fading.ends_with(&settled),
            "a neighbour fading out must not paint over the maximized window"
        );

        row.groups[1].maximized = false;
        let (mut row, ..) = relayout_row_owned_with(row, 2000.0, &Theme::dark());
        let _ = encoded(&mut row);
        assert!(
            row.sibling_fade_in_flight() && row.needs_redraw() && row.needs_animation_frame(),
            "the neighbours fading back in redraw the row every frame"
        );
        // The fade ends between frames: the last frame drawn showed it partway.
        row.advance_sibling_fade(Duration::from_secs(1));
        assert!(!row.sibling_fade_in_flight());
        assert!(
            row.sibling_fade_needs_frame() && row.needs_redraw() && row.needs_animation_frame(),
            "the frame with the neighbours at rest is still owed"
        );
        let _ = encoded(&mut row);
        assert!(
            !row.sibling_fade_needs_frame(),
            "once drawn at rest the fade asks for nothing more"
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

    #[test]
    fn hidden_row_retains_session_collaboration_delivery() {
        let first = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "hidden-session-a",
            "fixture.hidden-session-a",
        );
        let second = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "hidden-session-b",
            "fixture.hidden-session-b",
        );
        let first_events = std::rc::Rc::clone(&first.collab_events);
        let second_events = std::rc::Rc::clone(&second.collab_events);
        let first_group = WindowGroup::new(
            100,
            vec![(TabDescriptor::new(1, "First"), TileContent::App(first.root))],
            1,
            WindowWidth::Normal,
        );
        let second_group = WindowGroup::new(
            200,
            vec![(
                TabDescriptor::new(2, "Second"),
                TileContent::App(second.root),
            )],
            2,
            WindowWidth::Normal,
        );
        let mut row = GroupRow::new(vec![first_group, second_group]);
        row.set_fullscreen(None, true);

        crate::widget::handle_collab_event(
            &mut row,
            &crate::collaboration::CollabEvent::FollowChanged { peer_id: None },
        );
        assert_eq!(first_events.get(), 1);
        assert_eq!(second_events.get(), 1);
    }

    /// A 1000-wide display with the wheel gutter beside an 800-wide one
    /// whose bottom edge is 200 rows higher.
    fn two_lanes() -> Vec<StripLane> {
        vec![
            StripLane {
                output: 0,
                area: Rect::new(0.0, 0.0, 1000.0, 600.0),
                usable: 1000.0,
                leading: true,
            },
            StripLane {
                output: 1,
                area: Rect::new(1000.0, 0.0, 1800.0, 400.0),
                usable: 800.0,
                leading: false,
            },
        ]
    }

    /// Groups 100..=300 on the first display, 400..=600 on the second.
    fn partitioned_row() -> GroupRow {
        let groups: Vec<WindowGroup> = (1..=6_u64)
            .map(|i| make_group(i.saturating_mul(100), &[i]))
            .collect();
        let mut row = GroupRow::new(groups);
        let assignment: HashMap<u64, u32> = (1..=6_u64)
            .map(|i| (i.saturating_mul(100), u32::from(i > 3)))
            .collect();
        row.set_lanes(&two_lanes(), &assignment);
        // The first pass learns where the row sits; lanes are built
        // against that on the second.
        let (row, _, _) = relayout_row_owned(row, 1800.0);
        let (row, _, _) = relayout_row_owned(row, 1800.0);
        row
    }

    fn rect_of(row: &GroupRow, id: u64) -> Rect {
        row.groups
            .iter()
            .find(|g| g.id == id)
            .map(WindowGroup::get_rect)
            .expect("group laid out")
    }

    #[test]
    fn a_strip_across_displays_slides_on_every_display_without_a_new_layout() {
        let row = partitioned_row();
        let (mut row, taffy, root) = relayout_row_owned(row, 1800.0);
        let _ = row.take_structure_dirty();
        let rest: Vec<(u64, Rect)> = row.groups.iter().map(|g| (g.id, g.get_rect())).collect();
        for shift in [300.0, -250.0, 0.0] {
            row.set_deck_shift(shift);
            row.apply_layout(&taffy, root, Point::new(0.0, shift));
            assert!(
                !row.take_structure_dirty(),
                "moving the strip by {shift} needs no new layout"
            );
            for (id, at_rest) in &rest {
                let r = rect_of(&row, *id);
                assert!(
                    (r.y0 - (at_rest.y0 + shift)).abs() < 0.5
                        && (r.x0 - at_rest.x0).abs() < 0.5
                        && (r.height() - at_rest.height()).abs() < 0.5,
                    "window {id} on display {:?} moves with the strip by {shift}: {r:?} from {at_rest:?}",
                    row.output_of_group(*id)
                );
            }
        }
    }

    #[test]
    fn a_partitioned_strip_lays_each_display_out_on_its_own() {
        let row = partitioned_row();
        let lanes = two_lanes();
        for id in [100, 200, 300] {
            let r = rect_of(&row, id);
            assert!(
                r.x0 >= lanes[0].area.x0,
                "{id} starts left of its display: {r:?}"
            );
            assert!(
                r.y1 <= lanes[0].area.y1 + 0.5,
                "{id} is taller than its display"
            );
            assert_eq!(row.output_of_group(id), Some(0));
        }
        let first_on_second = rect_of(&row, 400);
        assert!(
            (first_on_second.x0 - lanes[1].area.x0).abs() < 0.5,
            "the second display's first window starts at its own left edge: {first_on_second:?}"
        );
        for id in [400, 500, 600] {
            let r = rect_of(&row, id);
            assert!(
                r.x0 >= lanes[1].area.x0,
                "{id} leaks onto the first display: {r:?}"
            );
            assert!(
                r.y1 <= lanes[1].area.y1 + 0.5,
                "{id} runs past the shorter display's bottom edge: {r:?}"
            );
            assert_eq!(row.output_of_group(id), Some(1));
        }
        // The seam between the displays is not a resize gap.
        let seam = row.handle_at(2).expect("handle after the third group");
        assert!(seam.get_rect().area() <= 0.0, "{:?}", seam.get_rect());
        assert_eq!(row.group_ids_on(1), vec![400, 500, 600]);
    }

    #[test]
    fn panning_one_display_leaves_the_other_where_it_was() {
        let mut row = partitioned_row();
        let before: Vec<Rect> = [100, 200, 300]
            .iter()
            .map(|&id| rect_of(&row, id))
            .collect();
        assert!(
            row.scroll_group_into_view(600),
            "the last window is off the second display"
        );
        let (row, _, _) = relayout_row_owned(row, 1800.0);
        let after: Vec<Rect> = [100, 200, 300]
            .iter()
            .map(|&id| rect_of(&row, id))
            .collect();
        assert_eq!(before, after, "the first display must not move");
        assert!(row.output_pan(1).expect("lane").scroll_x() > 0.0);
        assert_eq!(row.output_pan(0).expect("lane").scroll_x(), 0.0);
        let last = rect_of(&row, 600);
        assert!(last.x1 <= 1800.5 && last.x0 >= 1000.0, "{last:?}");
    }

    #[test]
    fn a_drag_resting_at_an_edge_pans_the_strip_under_it() {
        let (mut row, _taffy, _root) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        row.set_content_w_for_test(row.content_width());
        let max = row.max_scroll_x();
        assert!(max > 700.0, "fixture: most of the strip is off screen");
        let t0 = crate::time::now();
        let frame = Duration::from_millis(16);

        row.set_external_content_drag_hover(Some(Point::new(350.0, 300.0)));
        assert!(!row.drag_reach_pending());
        assert!(!row.advance_drag_reach_for_test(t0));

        // Pressed into the right edge: the strip pans right, frame by frame.
        row.set_external_content_drag_hover(Some(Point::new(698.0, 300.0)));
        assert!(row.drag_reach_pending(), "the edge pan asks for frames");
        let mut now = t0;
        row.advance_drag_reach_for_test(now);
        for _ in 0..10 {
            now += frame;
            assert!(row.advance_drag_reach_for_test(now));
        }
        let fast = row.scroll_x();
        assert!(fast > 100.0, "about 1.5 px/ms near the edge: {fast}");

        let (mut slow_row, _t, _r) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        slow_row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        slow_row.set_content_w_for_test(slow_row.content_width());
        slow_row.set_external_content_drag_hover(Some(Point::new(700.0 - 24.0, 300.0)));
        let mut slow_now = t0;
        slow_row.advance_drag_reach_for_test(slow_now);
        for _ in 0..10 {
            slow_now += frame;
            slow_row.advance_drag_reach_for_test(slow_now);
        }
        assert!(slow_row.scroll_x() > 0.0 && slow_row.scroll_x() < fast / 2.0);

        // It stops at the end of the strip, and pans back at the left edge.
        for _ in 0..200 {
            now += frame;
            row.advance_drag_reach_for_test(now);
        }
        assert!((row.scroll_x() - max).abs() < 1e-6);
        assert!(
            !row.drag_reach_pending(),
            "nowhere further to go: no frames"
        );
        row.set_external_content_drag_hover(Some(Point::new(2.0, 300.0)));
        now += frame;
        row.advance_drag_reach_for_test(now);
        now += frame;
        assert!(row.advance_drag_reach_for_test(now));
        assert!(row.scroll_x() < max);

        let at = row.scroll_x();
        row.set_external_content_drag_hover(None);
        now += frame;
        assert!(!row.advance_drag_reach_for_test(now));
        assert!((row.scroll_x() - at).abs() < f64::EPSILON);
        assert!(!row.drag_reach_pending());
    }

    #[test]
    fn moving_a_whole_window_to_an_edge_pans_the_strip_and_keeps_it_under_the_pointer() {
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1], &[2], &[3], &[4], &[5]]);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 700.0, 600.0));
        row.set_content_w_for_test(row.content_width());
        let grab = group_grab_point(&row, 0);
        press_at(&mut row, &taffy, root, grab);
        let edge = Point::new(698.0, grab.y);
        dispatch_at(
            &mut row,
            &taffy,
            root,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(edge.x, edge.y),
            },
        );
        assert!(
            row.group_drag_active(),
            "fixture: the window is being moved"
        );
        assert!(
            row.drag_reach_pending(),
            "resting at the edge asks for frames"
        );
        let start_before = row.group_drag.map(|d| d.start.x).unwrap();
        let t0 = crate::time::now();
        row.advance_drag_reach_for_test(t0);
        assert!(row.advance_drag_reach_for_test(t0 + Duration::from_millis(100)));
        let panned = row.scroll_x();
        assert!(panned > 0.0, "the strip panned under the moved window");
        let start_after = row.group_drag.map(|d| d.start.x).unwrap();
        assert!(
            (start_before - start_after - panned).abs() < 1e-6,
            "the moved window's travel grew by the pan, so it stays under the pointer"
        );
        assert!(row.group_reorder_target().is_some(), "the gap is re-aimed");
    }

    #[test]
    fn a_drag_resting_on_a_background_tab_brings_it_to_the_front() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1, 2], &[3]]);
        assert_eq!(row.groups[0].active_tab_id(), 1);
        let background = row.groups[0].tab_rects()[1].center();
        let t0 = crate::time::now();

        row.set_external_content_drag_hover(Some(background));
        assert!(!row.advance_drag_reach_for_test(t0));
        assert!(
            row.drag_reach_pending(),
            "a pending spring keeps frames coming"
        );
        assert!(!row.advance_drag_reach_for_test(t0 + Duration::from_millis(300)));
        assert_eq!(
            row.groups[0].active_tab_id(),
            1,
            "passing over is not resting"
        );
        assert!(row.advance_drag_reach_for_test(t0 + Duration::from_millis(520)));
        assert_eq!(
            row.groups[0].active_tab_id(),
            2,
            "the rested-on tab is in front"
        );

        // Moving off before the delay springs nothing.
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1, 2], &[3]]);
        let background = row.groups[0].tab_rects()[1].center();
        row.set_external_content_drag_hover(Some(background));
        row.advance_drag_reach_for_test(t0);
        row.set_external_content_drag_hover(Some(Point::new(background.x, 400.0)));
        row.advance_drag_reach_for_test(t0 + Duration::from_millis(600));
        assert_eq!(row.groups[0].active_tab_id(), 1);

        row.set_external_content_drag_hover(None);
        row.last_cursor = background;
        assert!(!row.advance_drag_reach_for_test(t0));
        assert!(!row.advance_drag_reach_for_test(t0 + Duration::from_secs(2)));
        assert_eq!(row.groups[0].active_tab_id(), 1);
    }
}
#[cfg(test)]
#[path = "group_row_keyboard_tests.rs"]
mod keyboard_tests;
#[cfg(test)]
#[path = "group_row_pacing_tests.rs"]
mod pacing_tests;
#[cfg(test)]
#[path = "group_row_sharing_tests.rs"]
mod sharing_tests;

#[cfg(test)]
#[path = "group_row_making_room_tests.rs"]
mod making_room_tests;
