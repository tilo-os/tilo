//! `GroupRow` — a horizontal flex container of `WindowGroup`s that also routes cross-group
//! tab drag-drop: it intercepts releases after every child `TabBar` drained its own drag,
//! and splices a published `pending_detach` tab and `TileHost` into a matching sibling.
//! Within-group reorder lives in `TabBar` and `WindowGroup`.

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
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::DrawChild;
use crate::widget::TaffyTreeExt;
use crate::widget::TileContent;
use crate::widget::{
    DEFAULT_STRIP_USABLE_PX, EdgeGrab, FloatingSurfaceProvider, HANDLE_SLOT_PX, ResizeHandle,
    TabDescriptor, TileHost, Widget, WindowGroup, WindowWidth,
};

#[path = "group_row_edges.rs"]
mod edges;
use edges::RowEdgeDrag;
#[path = "group_row_canvas.rs"]
mod canvas;
#[path = "group_row_crossing.rs"]
mod crossing;
use crossing::LetGo;
#[path = "group_row_floats.rs"]
mod floats;
pub use floats::{FLOAT_CARD_MARGIN, FLOAT_CARD_SIZE, FLOAT_SIZE, FloatAsk, keep_float_inside};
#[path = "group_row_pictures.rs"]
mod pictures;
#[path = "group_row_scroll.rs"]
mod scroll;
pub use canvas::{CANVAS_ZOOM_MAX, CANVAS_ZOOM_MIN, CanvasView};
pub use scroll::{JUMP_PX_PER_DETENT, WheelJumpAccum, wheel_jump_detents};
use scroll::{SnapTiming, snap_motion, strip_motion};

/// Idle time before the strip snaps to the nearest group's left edge.
const SNAP_IDLE_MS: u32 = 200;
const MOVE_SETTLED_PX: f64 = 0.25;
const MOVE_SETTLED_PX_PER_S: f64 = crate::tokens::motion::MOVE_SETTLED_PX_PER_S; // rest velocity in px/s, not a Theme gap
/// Two pan targets closer than this are the same place on screen.
const SNAP_SETTLED_PX: f64 = 0.5;

/// How long a drag rests on a background tab before it comes to the front.
pub(crate) const DRAG_SPRING_LOAD: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default)]
struct DragReach {
    edge: crate::edge_autoscroll::EdgeAutoscroll,
    tab: Option<(u64, u64, Instant)>,
}
/// Release velocity (px/s) below which lifted fingers leave the strip where
/// it is rather than throwing it.
const FLING_MIN_VELOCITY: f64 = crate::tokens::motion::FLING_MIN_VELOCITY;

const TAB_GROUP_EDGE_FRAC: f64 = 0.25;
const TAB_GROUP_EDGE_MIN_PX: f64 = crate::tokens::Spacing::BASE.s12 as f64;
const TAB_GROUP_EDGE_MAX_PX: f64 = crate::tokens::Spacing::BASE.s12 as f64 * 3.0;
const TAB_GROUP_EDGE_MAX_FRAC: f64 = 0.32;
/// Keep a resolved target through a few pixels of jitter; release commits the retained preview
/// verbatim, so the window cannot land elsewhere than the indicator showed.
const TAB_DROP_HYSTERESIS_PX: f64 = crate::tokens::Spacing::BASE.s2 as f64;

fn tab_group_edge_width(group_width: f64) -> f64 {
    (group_width * TAB_GROUP_EDGE_FRAC)
        .clamp(TAB_GROUP_EDGE_MIN_PX, TAB_GROUP_EDGE_MAX_PX)
        // Degenerate test/layout sizes must still retain an interior.
        .min(group_width * TAB_GROUP_EDGE_MAX_FRAC)
}

const TRAILING_REVEAL_FRAC: f64 = 0.20;
pub const STRIP_EDGE_GUTTER_PX: f64 = crate::tokens::Spacing::BASE.s8 as f64;
pub const STRIP_BOTTOM_GUTTER_PX: f64 = crate::tokens::Spacing::BASE.s3 as f64;
pub const STRIP_TOP_GAP_PX: f64 = crate::tokens::Spacing::BASE.s1 as f64;
/// Maximum *additional* visual give while a three-finger pan pulls beyond a
/// legal end. The shared rubber-band curve approaches this asymptotically.
const STRIP_OVERSCROLL_REACH_FRAC: f64 = 0.28;
/// How far a group's shadow sprites paint past its rect: `bloom_pad` with the clip's margin. Not a
/// theme token, since it must match the compositor's shadow draw.
const GROUP_SPRITE_REACH_PX: f64 = crate::tokens::motion::GROUP_SPRITE_REACH;

/// Press displacement (px) before a tab-bar-background press becomes a
/// whole-group reorder drag instead of a focus click.
const GROUP_DRAG_THRESHOLD: f64 = crate::tokens::Metrics::BASE.icon_label_gap as f64;

const TOUCH_GRAB_HOLD: crate::time::Duration = crate::time::Duration::from_millis(300);

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

/// In-flight whole-group reorder drag, set on a tab-bar-background press or touch and cleared on
/// release; `moved` latches once the pointer passes the threshold.
#[derive(Clone, Copy, Debug)]
struct GroupDrag {
    src_idx: usize,
    group_id: u64,
    start: Point,
    lifted_at: Instant,
    moved: bool,
    touch: Option<u64>,
    /// Someone else holds a window of the group: it follows the drag a little and springs back, and
    /// never moves.
    held: bool,
    /// The press landed on a tab — a window of one tab shows it as the bar's title — so the window
    /// carried from there joins one it is let go over, where one carried by its empty bar
    /// rearranges the strip.
    from_tab: bool,
}

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

pub use tilo_desktop_model::DropTarget as TabDropTarget;

/// Payload published when a tab drag ends outside the row's band; the shell hit-tests the
/// release against other strips and delivers it or returns it to source.
pub struct CrossStripDetach {
    pub tab_id: u64,
    pub source_group_id: u64,
    pub source_group_index: usize,
    pub source_tab_index: usize,
    pub desc: TabDescriptor,
    pub host: TileHost,
    pub release_point: Point,
}

/// Persistable shape of one live group after a same-strip move: the shell's workspace tree owns
/// tile content, so the UI reports identities and presentation state only.
pub use tilo_desktop_model::GroupShape as GroupTopology;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag (structure_dirty, scroll_dirty, hidden, shift_armed) is an independent, \
              per-frame invalidation signal read and cleared on its own schedule, not a \
              combinatorial config API — folding them into an enum or bitflags would not make \
              any call site clearer"
)]
#[crate::component::not_a_component(shell, "the strip of window groups")]
pub struct GroupRow {
    external_navigation: bool,
    close_reports: Vec<crate::application::ComponentCloseReport>,
    pub groups: Vec<WindowGroup>,
    /// The window that had the keyboard when the frame last asked ([`Widget::take_app_activation`]).
    keyboard_window: Option<u64>,
    handles: Vec<ResizeHandle>,
    handle_nodes: Vec<Option<(NodeId, Style)>>,
    node: Option<NodeId>,
    run_nodes: Vec<NodeId>,
    laid_sections: Vec<usize>,
    laid_collapsed: Vec<bool>,
    rect: Rect,
    node_ids: Vec<NodeId>,
    laid_group_ids: Vec<u64>,
    last_cursor: Point,
    structure_dirty: bool,
    /// Windows back in their strip whose width must stand where the strip
    /// now puts it, rather than slide there from where it floated: read by
    /// the next [`Self::set_width_bases`].
    settle_widths: Vec<u64>,
    tab_drop_target: Option<TabDropTarget>,
    cross_strip_drop_target: Option<TabDropTarget>,
    external_drag_over: bool,
    drag_reach: DragReach,
    pan: Pan,
    scroll_quantum: Option<f64>,
    /// The row is drawn into a scene of its own: its parent's draw only records the transform it
    /// would have drawn with.
    /// One per display the strip is partitioned across, left to right.
    /// Empty when the strip is one continuous row (a single display).
    sections: Vec<Section>,
    group_sections: HashMap<u64, usize>,
    focus_section: usize,
    pending_split_section: Option<usize>,
    pending_cross_strip_detach: Option<CrossStripDetach>,
    scroll_dirty: bool,
    leading_inset: f64,
    fullscreen_group: Option<u64>,
    hidden: bool,
    group_drag: Option<GroupDrag>,
    edge_hover: Option<(EdgeGrab, bool)>,
    edge_drag: Option<RowEdgeDrag>,
    group_resist_back: Option<(u64, crate::motion::MotionValue<vello::kurbo::Vec2>)>,
    touch_pan: Option<TouchPan>,
    pointer_pan: Option<PointerPan>,
    group_reorder_target: Option<usize>,
    group_reorder_section: Option<usize>,
    pending_group_reorder: Option<(u64, usize)>,
    /// The window a bar drag carries, while it carries one, and a window carried here.
    carried: crossing::Carried,
    /// The next press carries the window it lands in from anywhere in it, its body included, as a
    /// press on its bar does: the press was made with Super held.
    carry_press: bool,
    /// Set after a tab crosses a group boundary in this row; the shell drains an identity-only
    /// snapshot and applies it to the persisted workspace tree.
    pending_topology_change: bool,
    /// Displays this strip spans: each group resolves its width fraction against the span it is
    /// laid out on, so a full-width window cannot span two monitors. Empty in a bare host.
    output_spans: Vec<OutputSpan>,
    last_laid_x: HashMap<u64, f64>,
    shift_armed: bool,
    moves: Vec<(u64, crate::motion::MotionValue<f64>)>,
    shift_hold: Option<Duration>,
    reduce_motion: bool,
    motion_source: MotionSource,
    motion: crate::tokens::Motion,
    sibling_fade: crate::motion::Presence,
    /// Capture-harness pose for [`Self::hold_sibling_fade_at`]: skips `sync_sibling_fade`, so a
    /// slow offscreen paint cannot retire the curve before the next snapshot.
    sibling_fade_hold: Option<Duration>,
    /// Group that stayed visible through the last maximize: restore uses it so siblings fade in on
    /// the tiled draw path, not only while `maximized_idx` is still set.
    fade_hero: Option<u64>,
    sibling_fade_drawn_moving: bool,
    culled_groups: Vec<u64>,
    synced_at: Option<Instant>,
    unpainted: Vec<u64>,
    unpainted_since: Option<Instant>,
    apart_window: Option<ApartWindow>,
    apart_spans: Vec<(usize, (f64, f64))>,
    apart_drawings: Vec<ApartDrawing>,
    /// While frames only move a screen-space strip: the last drawing of each window at rest,
    /// replayed where the window is now.
    pan_drawings: Vec<ApartDrawing>,
    pan_kept: Vec<u64>,
    replayed_groups: Vec<u64>,
    pan_theme: Option<Theme>,
    pan_drawn_frame: Option<u64>,
    /// Windows whose drawing cannot be kept (it carries a blurred material
    /// or pixels living outside it) while frames only move: drawn plainly.
    pan_unkept: Vec<u64>,
    /// Windows that put anything on the top layer in the last frame: an open menu may hang on where
    /// its widgets last drew, so such a window is drawn, never replayed.
    overlay_groups: Vec<u64>,
    strip_canvas_shift: Vec<(usize, f64, bool, f64)>,
    drawing_shift: Affine,
    canvas: Option<CanvasView>,
    canvas_drag_from: Option<Rect>,
    pending_canvas_move: Option<(u64, Rect)>,
    floats: HashMap<u64, Rect>,
    collapsed_cards: std::collections::HashSet<u64>,
    float_drag_from: Option<Rect>,
    pending_float: Option<FloatAsk>,
    float_fade: HashMap<u64, crate::motion::Presence>,
    drawn_covers: Vec<(u64, Rect)>,
}

fn seen_across(env: &crate::draw::DrawEnv<'_>, rect: Rect) -> Option<(f64, f64)> {
    env.viewport.map(|viewport| {
        (
            (viewport.x0 - rect.x0).max(0.0),
            (viewport.x1 - rect.x0).min(rect.width()),
        )
    })
}

struct GroupDrawing(ApartDrawing);

impl GroupDrawing {
    fn append_to(&self, scene: &mut Scene) -> usize {
        let drawing = &self.0;
        crate::scene_parts::append_with(
            scene,
            drawing.parts.as_ref(),
            Some(Affine::IDENTITY),
            |scene| crate::backdrop::append_nested(scene, &drawing.scene, Affine::IDENTITY),
        )
    }

    fn keep(self, carried: usize) -> Option<ApartDrawing> {
        (carried == 0 && !draws_external_pixels(&self.0.scene)).then_some(self.0)
    }

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

#[derive(Clone, Copy)]
struct ApartWindow {
    span: (f64, f64),
    panning: bool,
    rows: (f64, f64),
}

fn draws_external_pixels(scene: &Scene) -> bool {
    scene.encoding().resources.patches.iter().any(|patch| {
        matches!(patch, vello_encoding::Patch::Image { image, .. } if image.data.is_empty())
    })
}

impl ApartDrawing {
    /// Keep this drawing of its own as the window's live picture; one that showed a secret is
    /// never shown small, and one that culled part of the window leaves the last whole one.
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

struct ApartDrawing {
    id: u64,
    /// The group's canvas rect and the transform it was drawn with: the
    /// drawing is reused only while both hold.
    rect: Rect,
    transform: Affine,
    scene: Scene,
    parts: Option<crate::scene_parts::SectionParts>,
    shows_secret: bool,
    /// What of the group the drawing culled nothing in, across, from the group's left edge; `None`
    /// when nothing was culled. A replay must show no more of the group.
    seen: Option<(f64, f64)>,
    /// Nothing changed in the group since the drawing was made; a drawing replayed through a frame
    /// that may have changed it is not current, and is never shown.
    current: bool,
}

/// One display's horizontal extent in the compositor's combined space, plus the width
/// groups on it size themselves against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpan {
    pub x: f64,
    pub width: f64,
    /// Width available to groups on this display: `width` minus the shell chrome it reserves, the
    /// denominator every width fraction is taken of.
    pub usable: f64,
}

/// One display's share of a strip partitioned across displays: each holds its own groups, laid out
/// and clipped inside its section only, so none straddles a border and panning one never reveals
/// another's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripSection {
    pub output: u32,
    pub area: Rect,
    pub usable: f64,
    pub leading: bool,
}

/// Horizontal pan of one run of groups (the whole row, or one section): canvas-x is the
/// unscrolled coordinate, `scroll_x` how far it is panned past the viewport's edge.
#[derive(Clone, Debug, Default)]
pub struct Pan {
    viewport: Rect,
    inset: f64,
    clear: f64,
    /// Horizontal scroll offset (px): each child renders at `-scroll_x`; at rest it is bounded by
    /// `pan_max_scroll_x`, and a direct gesture may hold a resisted value just outside.
    scroll_x: f64,
    strip_pan_origin: Option<f64>,
    finger_path: super::finger_path::FingerPath,
    motion: Option<crate::motion::MotionValue<f64>>,
    /// Total content width incl. gaps; cached so the wheel handler can
    /// clamp `scroll_x` without re-summing children.
    content_w: f64,
    group_left_edges: Vec<f64>,
    last_wheel_at: Option<Instant>,
    /// Each group's `(id, left, right)` canvas-x extent as last laid out, taken off taffy: a group
    /// taffy shrank to fit is that width on screen whatever its stored fraction says.
    laid_spans: Vec<(u64, f64, f64)>,
    /// Absolute external viewport navigation ([`GroupRow::pan_to`]) stays
    /// bounded through geometry changes.
    absolute: bool,
    /// A person's own pan last came to rest in the empty canvas past the last window, the only case
    /// where the pan keeps that allowance when the strip ends sooner.
    past_windows: bool,
}

impl Pan {
    /// Area a retained canvas keeps past each end of the content, so an overscroll pans within it;
    /// the rubber band never reaches the viewport share.
    fn canvas_pad(&self) -> f64 {
        (self.viewport.width().max(0.0) * STRIP_OVERSCROLL_REACH_FRAC).ceil() + 1.0
    }

    /// How far the row is panned.
    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    /// The area the row is shown in, as last laid out.
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    fn max_scroll_x(&self) -> f64 {
        self.max_scroll_x_of(self.content_w)
    }

    fn max_scroll_x_of(&self, content_w: f64) -> f64 {
        let gutter = if content_w > self.inset {
            self.inset + self.clear
        } else {
            0.0
        };
        (content_w + gutter - self.viewport.width()).max(0.0)
    }

    fn trailing_gutter(&self) -> f64 {
        if self.content_w > self.inset {
            self.inset + self.clear
        } else {
            0.0
        }
    }

    fn pan_max_scroll_x(&self) -> f64 {
        let viewport = self.viewport.width().max(1.0);
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

    fn resampled_travel(&self, now: Instant) -> Option<f64> {
        self.finger_path.resampled(now)
    }

    /// Velocity (travel px/s) of the fingers as they lift at `now`.
    fn release_velocity(&self, now: Instant) -> f64 {
        self.finger_path.release_velocity(now)
    }

    fn direct_pan_moving(&self, now: Instant) -> bool {
        let Some(origin) = self.strip_pan_origin else {
            return false;
        };
        self.finger_path.predicting(now)
            || self.resampled_travel(now).is_some_and(|travel| {
                (self.rubber_banded(origin + travel) - self.scroll_x).abs() > f64::EPSILON
            })
    }

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

    #[cfg(test)]
    fn scroll_target_for(&self, gr: Rect) -> Option<f64> {
        self.reveal_target(gr, self.max_scroll_x())
    }

    /// The offset that brings a group laid out at `gr` fully inside the viewport, or `None` when it
    /// already is, on a strip whose windows end at `end` once widths settle, so a reveal never
    /// eases into empty canvas.
    fn reveal_target(&self, gr: Rect, end: f64) -> Option<f64> {
        // Degenerate rect → layout hasn't run for this group yet.
        if gr.width() <= 0.0 {
            return None;
        }
        // A revealed group stands at least the gap between two windows clear of the viewport's
        // edges: a window's edge is drawn just outside its rect, so a reveal flush with the edge
        // would cut its border.
        let slack = (self.viewport.width() - gr.width()).max(0.0);
        let margin = HANDLE_SLOT_PX.min(slack / 2.0);
        let left_bound = self.viewport.x0 + margin;
        let right_bound = self.viewport.x1 - margin;
        let right_bound = if gr.width() <= right_bound - self.clear - left_bound {
            right_bound - self.clear
        } else {
            right_bound
        };
        let mut sx = self.scroll_x;
        // An oversized group cannot fit, so reveal its start consistently; checking the original
        // rect after a right-edge correction would right-align it.
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

    fn stop_motion(&mut self) {
        if let Some(mut motion) = self.motion.take() {
            motion.snap(motion.target());
        }
    }

    fn animating(&self) -> bool {
        self.motion.is_some()
    }

    fn motion_moving(&self, now: Instant) -> bool {
        self.motion.is_some_and(|motion| motion.is_moving(now))
    }

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
                    && x1 > sx + 0.5
                    && x0 < sx + row_w - 0.5
                    && (x0 < sx - 0.5 || x1 > sx + row_w + 0.5)
            })
            .map(|&(id, _, _)| id)
            .collect()
    }

    fn snap_in_flight(&self, snap: SnapTiming) -> bool {
        let now = crate::motion::frame_now();
        if self.motion_moving(now) || self.direct_pan_moving(now) {
            return true;
        }
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

    fn tick_snap_at(&mut self, now: Instant, snap: SnapTiming, hold: Option<Duration>) -> bool {
        let before = self.scroll_x;
        if let Some(origin) = self.strip_pan_origin {
            if let Some(travel) = self.resampled_travel(now) {
                self.scroll_x = self.rubber_banded(origin + travel);
            }
            return (self.scroll_x - before).abs() > f64::EPSILON;
        }
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
                        // The snap starts when the idle check was met, not at `now`, so the tick
                        // that sees the idle shows how far it has come; it falls back to
                        // `last_wheel` on the astronomically unlikely overflow.
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

    fn slide_scroll_from(&mut self, from: f64, now: Instant, timing: SnapTiming) {
        if timing.timing.is_instant() {
            return;
        }
        let mut motion = snap_motion(from, self.scroll_x);
        motion.set(self.scroll_x, timing.timing, now);
        self.motion = Some(motion);
        self.scroll_x = from;
    }

    /// Slide to `sx` from the drawn position, from rest: a shortcut's reveal must be over within
    /// its budget, and a press mid-slide retargets from the drawing, so presses never queue.
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

    /// Note where the groups were laid out; the pan stays. A strip never scrolls by itself after a
    /// resize, insert, close or output change — only explicit navigation moves it, plus the clamp
    /// into range.
    fn note_laid_spans(&mut self, spans: Vec<(u64, f64, f64)>) {
        self.laid_spans = spans;
    }

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

#[derive(Clone, Debug)]
struct Section {
    spec: StripSection,
    pan: Pan,
    built_viewport: Rect,
    shift: f64,
}

impl Section {
    /// The section's area where its row rests: never inverted.
    fn area(&self) -> Rect {
        let area = self.spec.area;
        Rect::new(area.x0, area.y0, area.x1.max(area.x0), area.y1.max(area.y0))
    }

    fn viewport(&self) -> Rect {
        self.area() + vello::kurbo::Vec2::new(0.0, self.shift)
    }

    fn off_display(&self) -> bool {
        let height = self.area().height();
        height > 0.0 && self.shift.abs() >= height - 0.5
    }
}

const BINARY_GRID: f64 = 1_048_576.0;

/// `value` rounded to a multiple of 2⁻²⁰ layout pixels: a device-pixel pan step is no binary
/// fraction, so a strip moved into canvas coordinates and back would round differently and a kept
/// canvas would never match.
fn on_binary_grid(value: f64) -> f64 {
    (value * BINARY_GRID).round() / BINARY_GRID
}

/// Whether a strip panned to `scroll` can be drawn in canvas coordinates: the retained
/// canvas spans the content from no pan to the furthest, plus `pad` past either end.
pub fn pan_within_canvas(scroll: f64, max_scroll: f64, pad: f64) -> bool {
    (-pad..=max_scroll.max(0.0) + pad).contains(&scroll)
}

impl GroupRow {
    pub fn new(groups: Vec<WindowGroup>) -> Self {
        Self {
            groups,
            keyboard_window: None,
            close_reports: vec![],
            external_navigation: false,
            handles: Vec::new(),
            handle_nodes: Vec::new(),
            node: None,
            run_nodes: Vec::new(),
            laid_sections: Vec::new(),
            laid_collapsed: Vec::new(),
            rect: Rect::ZERO,
            node_ids: Vec::new(),
            laid_group_ids: Vec::new(),
            last_cursor: Point::ZERO,
            structure_dirty: false,
            settle_widths: Vec::new(),
            tab_drop_target: None,
            cross_strip_drop_target: None,
            external_drag_over: false,
            drag_reach: DragReach::default(),
            pan: Pan::default(),
            scroll_quantum: None,
            sections: Vec::new(),
            group_sections: HashMap::new(),
            focus_section: 0,
            pending_split_section: None,
            pending_cross_strip_detach: None,
            scroll_dirty: false,
            leading_inset: 0.0,
            fullscreen_group: None,
            hidden: false,
            group_drag: None,
            edge_hover: None,
            edge_drag: None,
            group_resist_back: None,
            touch_pan: None,
            pointer_pan: None,
            group_reorder_target: None,
            group_reorder_section: None,
            pending_group_reorder: None,
            carried: crossing::Carried::default(),
            carry_press: false,
            pending_topology_change: false,
            output_spans: Vec::new(),
            last_laid_x: HashMap::new(),
            shift_armed: false,
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
            strip_canvas_shift: Vec::new(),
            drawing_shift: Affine::IDENTITY,
            pan_unkept: Vec::new(),
            canvas: None,
            canvas_drag_from: None,
            pending_canvas_move: None,
            floats: HashMap::new(),
            collapsed_cards: std::collections::HashSet::new(),
            float_drag_from: None,
            pending_float: None,
            float_fade: HashMap::new(),
            drawn_covers: Vec::new(),
        }
    }

    /// Whether a group at `rect`, shifted by `dx`, paints anything inside the row, shadows
    /// included; a canvas-coordinate draw covers the whole canvas, so the window must include every
    /// group a pan can reveal.
    fn paints_within_row(&self, rect: Rect, dx: f64) -> bool {
        let reach = rect.inflate(GROUP_SPRITE_REACH_PX, 0.0);
        let x1 = if crate::retained_present::unbake_scroll() {
            self.rect.x1 + self.max_scroll_x() + 2.0 * self.canvas_pad()
        } else {
            self.rect.x1
        };
        reach.x1 + dx > self.rect.x0 && reach.x0 + dx < x1
    }

    fn offset_section(&mut self, section: usize, dx: f64) {
        let in_section: Vec<bool> = (0..self.groups.len())
            .map(|index| self.section_index_of(index) == section)
            .collect();
        for (index, group) in self.groups.iter_mut().enumerate() {
            if in_section.get(index).copied().unwrap_or(false) {
                group.offset_layout(dx);
            }
        }
        for (index, handle) in self.handles.iter_mut().enumerate() {
            let left = in_section.get(index).copied().unwrap_or(false);
            let right = in_section
                .get(index.saturating_add(1))
                .copied()
                .unwrap_or(false);
            if left && right {
                handle.offset_layout(dx);
            }
        }
    }

    fn fullscreen_idx(&self) -> Option<usize> {
        let id = self.fullscreen_group?;
        self.groups.iter().position(|g| g.id == id)
    }

    /// Set or clear the transient fullscreen override, driven by the shell from compositor
    /// state; `(None, false)` restores normal tiling exactly.
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
        if self.is_floating(group.id) || self.canvas.is_some() {
            return false;
        }
        if group.maximized || self.fullscreen_group == Some(group.id) {
            return false;
        }
        let section = self.section_index_of(i);
        self.groups.iter().enumerate().any(|(j, other)| {
            j != i
                && self.section_index_of(j) == section
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
        self.sibling_fade.show(
            want_open,
            crate::motion::WindowMotion::SIBLINGS,
            theme.motion(),
            self.motion_source,
            crate::motion::frame_now(),
        );
    }

    fn sibling_fade_in_flight(&self) -> bool {
        self.sibling_fade.is_moving(crate::motion::frame_now())
    }

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

    /// Rewind the live fade so a test can sample it mid-flight without sleeping, rebuilt
    /// from the current hide intent.
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

    /// Freeze the sibling fade at `elapsed` along `durations.fast`: offscreen paints cost more wall
    /// time than the curve, so a capture harness would only ever see the endpoints. Not depending on
    /// `debug_assertions`, since `shots` ships in release.
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

    /// How much of this strip's top edge something overlays now (the shell's top bar while it
    /// shows over the windows under an auto-hiding one): every window in the strip moves down by
    /// it, whole, so its own bar stands below the overlay instead of under it.
    ///
    /// The window covering its display is the exception: it keeps the whole display, the bar's
    /// band included, and the bar paints over it while it shows. Moved down, it would hang off the
    /// display's bottom edge and jump each time the bar came and went. Its own bar, which it
    /// shows nowhere else, stands over its content under the bar while the bar shows.
    pub fn set_top_overlay_inset(&mut self, inset: f64) {
        let covering = self.fullscreen_idx();
        for (index, group) in self.groups.iter_mut().enumerate() {
            let covers = covering == Some(index);
            group.set_top_overlay_inset(if covers { 0.0 } else { inset });
            group.set_fullscreen_bar((covers && inset > 0.0).then_some(inset));
        }
    }

    /// Reserve a canvas-x gutter before the first group so it pans with `scroll_x`; on a
    /// partitioned strip only the leading section carries it.
    #[must_use]
    pub fn with_leading_inset(mut self, px: f64) -> Self {
        self.leading_inset = px;
        self.pan.inset = px;
        self
    }

    pub fn leading_inset(&self) -> f64 {
        self.leading_inset
    }

    /// Keep `px` of the viewport's trailing edge clear for the cards floating over the shell there:
    /// a window brought into view from now on stands clear of them where it fits beside them.
    pub fn set_trailing_clear(&mut self, px: f64) {
        let px = px.max(0.0);
        self.pan.clear = px;
        for section in &mut self.sections {
            section.pan.clear = px;
        }
    }

    pub fn set_leading_inset(&mut self, px: f64) {
        if (self.leading_inset - px).abs() > f64::EPSILON {
            self.leading_inset = px;
            if self.sections.is_empty() {
                self.pan.inset = px;
            }
            for section in &mut self.sections {
                section.pan.inset = if section.spec.leading { px } else { 0.0 };
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

    /// Partition the strip across displays: one section per display, left to right, and each group's
    /// display. Fewer than two sections restores the one continuous row, and a section keeps its pan
    /// while its display stays. Groups must already be ordered section by section.
    pub fn set_sections(&mut self, sections: &[StripSection], assignment: &HashMap<u64, u32>) {
        if sections.len() < 2 {
            if !self.sections.is_empty() {
                let keep = self
                    .sections
                    .iter()
                    .position(|section| section.spec.leading)
                    .unwrap_or(0);
                let mut pan = std::mem::take(&mut self.sections)
                    .into_iter()
                    .nth(keep)
                    .map(|section| section.pan)
                    .unwrap_or_default();
                pan.inset = self.leading_inset;
                self.pan = pan;
                self.group_sections.clear();
                self.focus_section = 0;
                self.structure_dirty = true;
            }
            return;
        }
        let specs_changed = self.sections.len() != sections.len()
            || self
                .sections
                .iter()
                .zip(sections.iter())
                .any(|(have, want)| have.spec != *want);
        if specs_changed {
            let mut previous = std::mem::take(&mut self.sections);
            self.sections = sections
                .iter()
                .map(|spec| {
                    let pan = previous
                        .iter()
                        .position(|section| section.spec.output == spec.output)
                        .map(|i| previous.swap_remove(i).pan)
                        .unwrap_or_default();
                    Section {
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
                        shift: 0.0,
                    }
                })
                .collect();
            self.structure_dirty = true;
        }
        let group_sections: HashMap<u64, usize> = assignment
            .iter()
            .filter_map(|(&group, &output)| {
                sections
                    .iter()
                    .position(|section| section.output == output)
                    .map(|section| (group, section))
            })
            .collect();
        let mut merged = group_sections;
        for (&group, &section) in &self.group_sections {
            if !assignment.contains_key(&group) && self.groups.iter().any(|g| g.id == group) {
                merged.entry(group).or_insert(section);
            }
        }
        if merged != self.group_sections {
            self.group_sections = merged;
            self.structure_dirty = true;
        }
        self.focus_section = self
            .focus_section
            .min(self.sections.len().saturating_sub(1));
    }

    /// Show this strip on `outputs` only: its other sections move off their display, so it neither
    /// draws nor takes input there, and a display coming back draws it where it was left.
    pub fn set_shown_outputs(&mut self, outputs: &[u32]) {
        for section in &mut self.sections {
            section.shift = if outputs.contains(&section.spec.output) {
                0.0
            } else {
                section.area().height()
            };
        }
    }

    /// Put `group_id` in `display`'s section before the shell's next [`Self::set_sections`], so the
    /// display a window opens on owns it from the first frame.
    pub fn set_group_display(&mut self, group_id: u64, display: u32) {
        let Some(section) = self
            .sections
            .iter()
            .position(|section| section.spec.output == display)
        else {
            return;
        };
        if self.group_sections.insert(group_id, section) != Some(section) {
            self.structure_dirty = true;
        }
    }

    /// Whether `p` stands on the display the window at `idx` is laid out on. On a strip spread
    /// across displays a window whose rect reaches past its display's edge is clipped there, so
    /// the next display's canvas is not its own; a floating window stands over whichever display
    /// it is put on.
    pub(super) fn on_display_under(&self, idx: usize, p: Point) -> bool {
        self.sections.is_empty()
            || self.canvas.is_some()
            || self
                .groups
                .get(idx)
                .is_some_and(|group| self.is_floating(group.id))
            || self.section_at(p) == Some(self.section_index_of(idx))
    }

    /// Whether the window at `idx` is drawn under `p`: its laid-out rect holds the point and the
    /// point stands on its own display ([`Self::on_display_under`]).
    pub(super) fn shown_under(&self, idx: usize, p: Point) -> bool {
        self.groups
            .get(idx)
            .is_some_and(|group| group.get_rect().contains(p))
            && self.on_display_under(idx, p)
    }

    /// The index of the shown window drawn under `p`, other than `except`: the last laid out
    /// there, and never one closing or hidden behind a maximized sibling.
    pub(super) fn window_under(&self, p: Point, except: Option<u64>) -> Option<usize> {
        self.groups
            .iter()
            .enumerate()
            .rev()
            .find(|(i, group)| {
                Some(group.id) != except
                    && !group.is_closing()
                    && !self.layout_hides_group(*i)
                    && self.shown_under(*i, p)
            })
            .map(|(i, _)| i)
    }

    /// The shown group a left press at the remembered cursor lands in; mouse buttons carry
    /// no position of their own.
    /// The window whose title the pointer is on ([`WindowGroup::bar_title_hit`]), if any.
    pub fn group_title_under_cursor(&self) -> Option<u64> {
        let id = self.group_under_cursor()?;
        self.groups
            .iter()
            .find(|group| group.id == id)
            .filter(|group| group.bar_title_hit(self.last_cursor))
            .map(|group| group.id)
    }

    pub fn group_under_cursor(&self) -> Option<u64> {
        self.float_group_at(self.last_cursor).or_else(|| {
            self.window_under(self.last_cursor, None)
                .map(|i| self.groups[i].id)
        })
    }

    /// Whether logical point `p` is over this strip: inside one of its sections when it is
    /// partitioned, else within its row's band.
    pub fn covers(&self, p: Point) -> bool {
        if self.sections.is_empty() {
            p.y >= self.rect.y0 && p.y <= self.rect.y1
        } else {
            self.sections
                .iter()
                .any(|section| !section.off_display() && section.viewport().contains(p))
        }
    }

    fn section_index_of(&self, idx: usize) -> usize {
        if self.sections.is_empty() {
            return 0;
        }
        let assigned = |i: usize| {
            self.groups
                .get(i)
                .and_then(|g| self.group_sections.get(&g.id).copied())
        };
        (0..=idx)
            .rev()
            .find_map(assigned)
            .or_else(|| (idx..self.groups.len()).find_map(assigned))
            .unwrap_or(0)
            .min(self.sections.len().saturating_sub(1))
    }

    /// The section `group_id` is laid out in; `None` when the strip is one continuous row or
    /// the group isn't in it.
    pub fn section_of_group(&self, group_id: u64) -> Option<usize> {
        if self.sections.is_empty() {
            return None;
        }
        let idx = self.groups.iter().position(|g| g.id == group_id)?;
        Some(self.section_index_of(idx))
    }

    /// The display `group_id` is laid out on, when the strip is partitioned.
    pub fn output_of_group(&self, group_id: u64) -> Option<u32> {
        let section = self.section_of_group(group_id)?;
        self.sections
            .get(section)
            .map(|section| section.spec.output)
    }

    fn section_range(&self, section: usize) -> std::ops::Range<usize> {
        if self.sections.is_empty() {
            return 0..self.groups.len();
        }
        let sections: Vec<usize> = (0..self.groups.len())
            .map(|i| self.section_index_of(i))
            .collect();
        let start = sections
            .iter()
            .position(|&l| l >= section)
            .unwrap_or(sections.len());
        let end = sections
            .iter()
            .position(|&l| l > section)
            .unwrap_or(sections.len());
        start..end.max(start)
    }

    /// The section under logical point `p`, else the section nearest to it horizontally.
    pub fn section_at(&self, p: Point) -> Option<usize> {
        if self.sections.is_empty() {
            return None;
        }
        self.sections
            .iter()
            .position(|section| !section.off_display() && section.viewport().contains(p))
            .or_else(|| {
                self.sections
                    .iter()
                    .position(|section| p.x >= section.spec.area.x0 && p.x < section.spec.area.x1)
            })
            .or_else(|| {
                self.sections
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

    /// Point keyboard-driven panning (direct strip pans, the visible span launches consult)
    /// at the section of display `output`.
    pub fn set_focus_output(&mut self, output: u32) {
        if let Some(section) = self
            .sections
            .iter()
            .position(|section| section.spec.output == output)
        {
            self.focus_section = section;
        }
    }

    fn pan_of_section(&self, section: usize) -> &Pan {
        self.sections.get(section).map_or(&self.pan, |l| &l.pan)
    }

    fn pan_of_section_mut(&mut self, section: usize) -> &mut Pan {
        match self.sections.get_mut(section) {
            Some(l) => &mut l.pan,
            None => &mut self.pan,
        }
    }

    fn pans(&self) -> Vec<&Pan> {
        if self.sections.is_empty() {
            vec![&self.pan]
        } else {
            self.sections.iter().map(|section| &section.pan).collect()
        }
    }

    /// The pan of display `output`'s section, when the strip is partitioned.
    pub fn output_pan(&self, output: u32) -> Option<&Pan> {
        self.sections
            .iter()
            .find(|section| section.spec.output == output)
            .map(|section| &section.pan)
    }

    fn leading_pan(&self) -> &Pan {
        self.sections
            .iter()
            .find(|section| section.spec.leading)
            .map_or(&self.pan, |section| &section.pan)
    }

    fn pan_of_group_mut(&mut self, group_id: u64) -> &mut Pan {
        let section = self.section_of_group(group_id).unwrap_or(0);
        self.pan_of_section_mut(section)
    }

    /// Usable width a group at canvas-x `x` resolves its width fraction against: canvas, not
    /// screen, so panning never resizes windows. Falls back to the row's width less the leading
    /// gutter, then the default.
    pub fn usable_at(&self, x: f64) -> f64 {
        if let Some(&first) = self.output_spans.first() {
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

    /// Per group, in strip order, whether it has its run of the strip to itself: a lone
    /// window at the default width takes it, so a neighbour closing widens it and a new one
    /// narrows it again.
    pub fn alone_per_group(&self) -> Vec<bool> {
        let sections: Vec<usize> = (0..self.groups.len())
            .map(|i| self.section_index_of(i))
            .collect();
        let open: Vec<bool> = self
            .groups
            .iter()
            .map(|g| !g.is_closing() && !self.is_floating(g.id))
            .collect();
        (0..self.groups.len())
            .map(|i| {
                open.get(i).copied().unwrap_or(false)
                    && !sections
                        .iter()
                        .zip(&open)
                        .enumerate()
                        .any(|(j, (&section, &other))| {
                            j != i && other && Some(&section) == sections.get(i)
                        })
            })
            .collect()
    }

    /// Per-group usable width in strip order, from stored widths and pushed spans alone, so it is
    /// stable on the first frame.
    pub fn usable_per_group(&self) -> Vec<f64> {
        if !self.sections.is_empty() {
            return (0..self.groups.len())
                .map(|i| {
                    self.sections
                        .get(self.section_index_of(i))
                        .map_or(DEFAULT_STRIP_USABLE_PX, |section| {
                            section.spec.usable.max(1.0)
                        })
                })
                .collect();
        }
        let alone = self.alone_per_group();
        let floating = self.floating_flags();
        let mut canvas_x = self.rect.x0 + self.leading_inset;
        self.groups
            .iter()
            .zip(alone)
            .zip(&floating)
            .map(|((g, alone), &floating)| {
                let usable = self.usable_at(canvas_x);
                if !floating {
                    canvas_x += g.width().resolve_in_strip(usable, alone) + HANDLE_SLOT_PX;
                }
                usable
            })
            .collect()
    }

    /// Each group's `(id, left, right)` in unscrolled canvas space, derived like the usable width
    /// so a group spliced in this frame already has a span; on a partitioned strip each section's
    /// canvas starts at its display's edge.
    pub fn group_canvas_spans(&self) -> Vec<(u64, f64, f64)> {
        let alone = self.alone_per_group();
        let floating = self.floating_flags();
        if !self.sections.is_empty() {
            let usable = self.usable_per_group();
            let mut cursor: Vec<f64> = self
                .sections
                .iter()
                .map(|section| section.spec.area.x0 + section.pan.inset)
                .collect();
            return self
                .groups
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let section = self.section_index_of(i);
                    let x0 = cursor.get(section).copied().unwrap_or(0.0);
                    if floating.get(i).copied().unwrap_or(false) {
                        return (g.id, x0, x0);
                    }
                    let width = g.width().resolve_in_strip(
                        usable.get(i).copied().unwrap_or(DEFAULT_STRIP_USABLE_PX),
                        alone.get(i).copied().unwrap_or(false),
                    );
                    if let Some(c) = cursor.get_mut(section) {
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
            .zip(&floating)
            .map(|((g, alone), &floating)| {
                let usable = self.usable_at(canvas_x);
                if floating {
                    return (g.id, canvas_x, canvas_x);
                }
                let width = g.width().resolve_in_strip(usable, alone);
                let span = (g.id, canvas_x, canvas_x + width);
                canvas_x += width + HANDLE_SLOT_PX;
                span
            })
            .collect()
    }

    fn dest_content_w(&self, section: usize) -> f64 {
        let inset = self.pan_of_section(section).inset;
        let collapsed = self.groups.iter().enumerate().any(|(i, g)| {
            self.section_index_of(i) == section
                && (g.maximized || self.fullscreen_group == Some(g.id))
        });
        if collapsed {
            return inset;
        }
        let floating = self.floating_flags();
        let mut x = inset;
        let mut prev = false;
        for i in 0..self.groups.len() {
            if self.section_index_of(i) != section {
                continue;
            }
            let Some(g) = self.groups.get(i) else {
                continue;
            };
            if floating.get(i).copied().unwrap_or(false) {
                continue;
            }
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

    /// Ids of groups currently visible, read off laid-out rects: "can I see it" is about
    /// pixels, so a group with no rect yet reads as not on screen.
    pub fn onscreen_group_ids(&self) -> Vec<u64> {
        self.groups
            .iter()
            .enumerate()
            .filter(|(i, g)| {
                let r = g.get_rect();
                let view = if self.sections.is_empty() {
                    self.rect
                } else {
                    self.pan_of_section(self.section_index_of(*i)).viewport
                };
                r.width() > 0.0 && r.x1 > view.x0 && r.x0 < view.x1
            })
            .map(|(_, g)| g.id)
            .collect()
    }

    /// The visible viewport shifted by the pan, in the same space as the group canvas spans; on a
    /// partitioned strip, the focused display's section.
    pub fn visible_canvas_span(&self) -> (f64, f64) {
        if self.sections.is_empty() {
            return (
                self.rect.x0 + self.pan.scroll_x,
                self.rect.x1 + self.pan.scroll_x,
            );
        }
        let pan = self.pan_of_section(self.focus_section);
        (
            pan.viewport.x0 + pan.scroll_x,
            pan.viewport.x1 + pan.scroll_x,
        )
    }

    /// Ids of the groups laid out on display `output`, in order.
    pub fn group_ids_on(&self, output: u32) -> Vec<u64> {
        let Some(section) = self
            .sections
            .iter()
            .position(|section| section.spec.output == output)
        else {
            return self.group_ids();
        };
        self.groups
            .get(self.section_range(section))
            .unwrap_or(&[])
            .iter()
            .map(|g| g.id)
            .collect()
    }

    /// Used by the shell's `resync_strips_from_tree` to diff against the persisted window
    /// groups, keeping host state across an unrelated revision bump.
    pub fn group_ids(&self) -> Vec<u64> {
        self.groups.iter().map(|g| g.id).collect()
    }

    /// Inserting anywhere but the end pushes later groups along: the motion is armed here
    /// and measured by the next layout pass.
    pub fn insert_group(&mut self, group: WindowGroup, idx: usize) {
        let clamped = idx.min(self.groups.len());
        if clamped < self.groups.len() {
            self.shift_armed = true;
        }
        self.groups.insert(clamped, group);
        self.structure_dirty = true;
    }

    /// Make width for a window that just opened, beside the one it opened to the right of, else the
    /// one after it: two that don't fit share the display, and when no share leaves both usable the
    /// strip scrolls instead. New widths are stamped as pending changes.
    pub fn share_with_neighbour(&mut self, group_id: u64) -> bool {
        self.share_with_neighbour_placing(group_id, false)
    }

    /// [`Self::share_with_neighbour`] for a window back in its strip from floating over the shell:
    /// both take their share at once, so the strip shows them whole from the first frame instead of
    /// one running past its edge while its widths catch up.
    pub fn share_with_neighbour_at_once(&mut self, group_id: u64) -> bool {
        self.share_with_neighbour_placing(group_id, true)
    }

    /// Make width for the window `group_id` that opened over the shell beside its neighbour: the
    /// card a floating window draws at — the window and the band and ring it draws round it — is
    /// `card_px` wide, and the window it opened next to gives up exactly that much of the strip,
    /// so the card stands beside what the person was reading rather than over it. Both widths
    /// stand there from this frame, since the card lands over the place they make. False when the
    /// strip is too small beside the card.
    pub fn share_with_floating_neighbour(&mut self, group_id: u64, card_px: f64) -> bool {
        let Some(idx) = self.groups.iter().position(|g| g.id == group_id) else {
            return false;
        };
        let section = self.section_of_group(group_id);
        let usable_per_group = self.usable_per_group();
        let usable = usable_per_group
            .get(idx)
            .copied()
            .unwrap_or(DEFAULT_STRIP_USABLE_PX);
        let beside = |i: usize| {
            self.groups.get(i).is_some_and(|g| {
                !g.is_closing()
                    && !g.maximized
                    && !self.is_floating(g.id)
                    && self.section_of_group(g.id) == section
            })
        };
        let neighbour = match idx.checked_sub(1) {
            Some(left) if beside(left) => left,
            _ if beside(idx.saturating_add(1)) => idx.saturating_add(1),
            _ => return false,
        };
        if usable_per_group.get(neighbour) != Some(&usable) {
            return false;
        }
        let gap = if self.external_navigation {
            0.0
        } else {
            HANDLE_SLOT_PX
        };
        let Some((neighbour_width, card_width)) =
            tilo_desktop_model::width::share_for_float(card_px, usable, gap)
        else {
            return false;
        };
        let alone = !(0..self.groups.len()).any(|i| i != idx && i != neighbour && beside(i));
        if alone && let Some(g) = self.groups.get_mut(neighbour) {
            g.set_alone(true);
        }
        for (i, width) in [(neighbour, neighbour_width), (idx, card_width)] {
            let Some(g) = self.groups.get_mut(i) else {
                continue;
            };
            if g.width() != width {
                g.share_width(width);
                g.stamp_pending_width_change();
                g.snap_width_spring();
            }
        }
        self.structure_dirty = true;
        true
    }

    fn share_with_neighbour_placing(&mut self, group_id: u64, at_once: bool) -> bool {
        let Some(idx) = self.groups.iter().position(|g| g.id == group_id) else {
            return false;
        };
        let section = self.section_of_group(group_id);
        let usable_per_group = self.usable_per_group();
        let usable = usable_per_group
            .get(idx)
            .copied()
            .unwrap_or(DEFAULT_STRIP_USABLE_PX);
        let beside = |i: usize| {
            self.groups.get(i).is_some_and(|g| {
                !g.is_closing()
                    && !g.maximized
                    && !self.is_floating(g.id)
                    && self.section_of_group(g.id) == section
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
        let screen = usable + 2.0 * STRIP_EDGE_GUTTER_PX;
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
            let Some(g) = self.groups.get_mut(i) else {
                continue;
            };
            if g.width() != width {
                g.share_width(width);
                g.stamp_pending_width_change();
            }
            if at_once {
                g.snap_width_spring();
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

    fn shift_in_flight(&self) -> bool {
        let now = crate::motion::frame_now();
        self.shift_hold.is_none() && self.moves.iter().any(|(_, m)| m.is_moving(now))
    }

    fn land_moves(&mut self) {
        for (_, offset) in &mut self.moves {
            offset.snap(0.0);
        }
        self.moves.clear();
    }

    fn move_timing(&self, source: MotionSource) -> crate::motion::Timing {
        let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
        crate::motion::WindowMotion::MOVE.timing(crate::motion::Direction::Enter, motion, source)
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

    /// How far the row is panned.
    pub fn scroll_x(&self) -> f64 {
        self.leading_pan().scroll_x
    }

    /// The area the row is shown in, as last laid out.
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
    /// canvas keeps at least that much margin: with whole-device-pixel pans,
    /// a canvas shift (`pan + pad`) is then whole device pixels too.
    fn section_canvas_pad(&self, pan: &Pan) -> f64 {
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

    /// Whether the strip still has to move: a pan easing to its target, or
    /// fingers moving it. A strip at rest draws no more frames for its pan, so
    /// a host or a test that waits on the strip coming to rest reads this
    /// rather than counting frames.
    pub fn strip_moving(&self, now: Instant) -> bool {
        self.pans().into_iter().any(|pan| {
            pan.animating() || pan.strip_pan_origin.is_some() || pan.direct_pan_moving(now)
        })
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

    /// Area a retained canvas keeps left of the content and right of the furthest pan, in
    /// layout pixels.
    pub fn canvas_pad(&self) -> f64 {
        self.section_canvas_pad(self.leading_pan())
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

    /// Set the real strip viewport to an absolute finite offset. Browser and
    /// other external navigation controllers use this narrow border instead of
    /// synthesizing wheel input or moving rendered output independently.
    pub fn pan_to(&mut self, requested: f64) -> bool {
        if !requested.is_finite() {
            return false;
        }
        let pan = self.pan_of_section_mut(self.focus_section);
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

    /// `debug_assertions` guarded so it's available to `crates/ui/ui-core/tests` but not
    /// shipped in release.
    #[cfg(any(test, debug_assertions))]
    pub fn group_at_for_test(&self, idx: usize) -> Option<&WindowGroup> {
        self.groups.get(idx)
    }

    /// `0.0` before the first layout pass. On a partitioned strip, the
    /// focused section's.
    pub fn content_width(&self) -> f64 {
        self.pan_of_section(self.focus_section).content_w
    }

    pub fn max_scroll_x(&self) -> f64 {
        self.pan_of_section(self.focus_section).max_scroll_x()
    }

    /// Furthest legal resting offset for direct strip panning. Unlike the
    /// content-tight focus bound, this leaves a deliberate glimpse of canvas
    /// after the final app. Content which already fits stays pinned at home.
    pub fn pan_max_scroll_x(&self) -> f64 {
        self.pan_of_section(self.focus_section).pan_max_scroll_x()
    }

    /// Rebase total-travel updates onto the strip's current visual position.
    /// A fresh axis lock calls this even if a prior stream lost its End at a
    /// lock/session boundary, preventing stale gesture state from jumping.
    /// On a partitioned strip only the focused display's section pans.
    pub fn begin_strip_pan(&mut self) {
        self.pan_of_section_mut(self.focus_section)
            .begin_strip_pan();
    }

    /// Track a three-finger horizontal gesture in direct-manipulation space. `travel` is total
    /// signed scroll travel since Begin (positive moves toward later groups), reported `at` (the
    /// input event's own time, so a batch drained late keeps its spacing). Legal canvas tracks 1:1;
    /// beyond either end the shared iOS-style curve supplies increasing resistance. Each frame
    /// shows the travel resampled at the frame's time, so a steady swipe moves the strip evenly
    /// whatever the touchpad's report rate.
    pub fn update_strip_pan(&mut self, travel: f64, at: Instant) -> bool {
        let moved = self
            .pan_of_section_mut(self.focus_section)
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
        let pan = self.pan_of_section_mut(self.focus_section);
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
        if self
            .pan_of_section(self.focus_section)
            .strip_pan_origin
            .is_some()
        {
            crate::control_hints::performed::note(
                crate::control_hints::performed::Performed::PannedView,
            );
        }
        let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
        let settle = self.move_timing(MotionSource::Gesture);
        let settling = self
            .pan_of_section_mut(self.focus_section)
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
        self.pan_of_section_mut(self.focus_section).absolute = true;
    }

    /// Whether a layout pass has given `group_id` its place. A group
    /// focused the same frame it was spliced in has none yet, so a pan to
    /// it has nothing to aim at. A window opening has its place from its
    /// first layout, while its slot still grows from nothing: a reveal aims
    /// at where it will be once grown (`Self::scroll_target_for`), so the
    /// strip pans as the window opens rather than once it has some width.
    pub fn group_is_laid_out(&self, group_id: u64) -> bool {
        self.laid_group_ids.contains(&group_id) && self.groups.iter().any(|g| g.id == group_id)
    }

    /// Pan `group_id`'s run (the row, or its display's section) so the group is fully inside the
    /// viewport (Niri's "focused column follows into view"). Call after a layout pass (it uses the
    /// group's live rect). Cancels any in-flight wheel-snap so a snap-back does not override it.
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

    /// Say what starts the transitions laid out next: a shortcut or action lands its result on the
    /// next frame (groups take their new slots, widths and maximized rects at once; siblings and
    /// tab chips do not fade), while a pointer or gesture keeps the full motion. The host sets it
    /// before acting and returns it to [`MotionSource::Pointer`] once the layout showing the result
    /// has run.
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

    /// The pan that brings `group_id` wholly into view, aimed at where the group will be once the
    /// slot-width springs of it and the groups before it have settled, not at where they are this
    /// frame. A window that has just opened grows from zero width over several frames: aiming at
    /// its first frames' rect ends the pan short, and the host's edge-triggered framing (keying on
    /// destination widths) then never frames it, leaving the focused new window off screen.
    fn scroll_target_for(&self, group_id: u64) -> Option<f64> {
        let (section, rect) = self.settled_rect_of(group_id)?;
        self.pan_of_section(section)
            .reveal_target(rect, self.settled_max_scroll_x(section))
    }

    fn settled_max_scroll_x(&self, section: usize) -> f64 {
        self.pan_of_section(section)
            .max_scroll_x_of(self.dest_content_w(section))
    }

    /// Pan so that `keep` and `also` are both wholly in view when they fit
    /// on screen together, else just `keep`; glided like
    /// [`Self::scroll_group_into_view_animated`]. For a window that opened
    /// beside the one the person is in without taking their focus: both
    /// are shown, and if they cannot be, the person keeps what they were
    /// looking at. `false` when neither has been laid out in one section or
    /// nothing has to move.
    pub fn scroll_groups_into_view_animated(
        &mut self,
        keep: u64,
        also: u64,
        now: Instant,
        source: MotionSource,
    ) -> bool {
        let (Some((section, kept)), Some((also_section, added))) =
            (self.settled_rect_of(keep), self.settled_rect_of(also))
        else {
            return self.scroll_group_into_view_animated(keep, now, source);
        };
        let pan = self.pan_of_section(section);
        let both = kept.union(added);
        let rect = if also_section == section && both.width() <= pan.viewport.width() {
            both
        } else {
            kept
        };
        let Some(sx) = pan.reveal_target(rect, self.settled_max_scroll_x(section)) else {
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

    fn settled_rect_of(&self, group_id: u64) -> Option<(usize, Rect)> {
        let g = self.groups.iter().find(|g| g.id == group_id)?;
        if !self.laid_group_ids.contains(&group_id) {
            return None;
        }
        let section = self.section_of_group(group_id).unwrap_or(0);
        let mut rect = g.get_rect();
        let mut shift = 0.0;
        for earlier in &self.groups {
            if earlier.id == group_id {
                break;
            }
            if self.section_of_group(earlier.id).unwrap_or(0) == section {
                shift += earlier.dest_width_px() - earlier.layout_width_px();
            }
        }
        let own = g.dest_width_px() - g.layout_width_px();
        rect = Rect::new(rect.x0 + shift, rect.y0, rect.x1 + shift + own, rect.y1);
        Some((section, rect))
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

    /// Select `tab_id` inside `group_id` widget-side. The bar's `active_id` is authoritative and
    /// the shell's per-frame harvest mirrors it into the persisted tree, so a tree-only write would
    /// be overwritten a frame later.
    pub fn set_group_active_tab(&mut self, group_id: u64, tab_id: u64) -> bool {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group_id) else {
            return false;
        };
        if !g.tabs().iter().any(|tab| tab.id == tab_id) {
            return false;
        }
        // Selecting the tab already visible is still a valid address. The shell uses this boolean
        // as a stale-target guard before it writes durable focus, so `false` must mean "the
        // group/tab does not exist", not merely "nothing needed repainting".
        if g.active_tab_id() == tab_id {
            return true;
        }
        if !g.set_active_tab(tab_id) {
            return false;
        }
        self.scroll_dirty = true;
        self.structure_dirty = true;
        true
    }

    /// Split out from `apply_layout` so headless tests can advance simulated time
    /// deterministically without staging a full taffy tree.
    pub fn tick_snap_at(&mut self, now: Instant) {
        let mut moved = false;
        let snap = self.snap_timing(MotionSource::Pointer);
        let hold = self.shift_hold.filter(|_| !self.moves.is_empty());
        if self.sections.is_empty() {
            moved = self.pan.tick_snap_at(now, snap, hold);
        }
        for section in &mut self.sections {
            moved |= section.pan.tick_snap_at(now, snap, hold);
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

    /// Exposed separately from `handle_event` so tests can drive a transfer without staging
    /// a full winit event.
    pub fn transfer_tab(&mut self, src: usize, dst: usize, tab_id: u64, target_idx: usize) -> bool {
        if src == dst || src >= self.groups.len() || dst >= self.groups.len() {
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
        let Some(dst_group) = self.groups.get_mut(dst) else {
            return false;
        };
        dst_group.insert_detached_tab(target_idx, desc, host);
        self.prune_empty_group_after_transfer(src);
        self.pending_topology_change = true;
        true
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

    /// `Some(gap_idx)` if the release landed strictly between two groups (`0` before the first, `N`
    /// after the last); `None` inside a group's rect. Deliberately not y-band-restricted: the
    /// release comes from our own row's `TabBar`, so anywhere that isn't on a group is a new-window
    /// gap.
    pub fn gap_index_for(&self, release_point: Point) -> Option<usize> {
        if self.groups.is_empty() {
            return Some(0);
        }
        for i in 0..self.groups.len() {
            if self.shown_under(i, release_point) {
                return None;
            }
        }
        if let Some(section) = self.section_at(release_point) {
            let run = self.section_range(section);
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

    /// Detach `tab_id` from `src` into a brand-new `WindowGroup` at `gap_idx`, pruning
    /// `src` if that leaves it empty.
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
        if !self.sections.is_empty() {
            let section = self
                .pending_split_section
                .take()
                .unwrap_or_else(|| self.section_index_of(src));
            self.group_sections.insert(new_id, section);
        }
        self.pending_split_section = None;
        self.groups.insert(clamped, new_group);
        self.structure_dirty = true;
        if src_now_empty {
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

    /// Whether the next press carries the window it lands in from anywhere in it, as a press on
    /// its bar does (a press made with Super held), or only from its bar.
    pub fn set_carry_press(&mut self, carry: bool) {
        self.carry_press = carry;
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

    /// Preview a tab arriving from another strip.
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

    fn drag_over_row(&self) -> bool {
        self.external_drag_over
            || self.cross_strip_drop_target.is_some()
            || self.tab_drag_active()
            || self.group_drag_moving()
    }

    /// A whole window of this row is being moved (not one someone else holds,
    /// which only resists the pull and never moves).
    fn group_drag_moving(&self) -> bool {
        self.group_drag.is_some_and(|drag| drag.moved && !drag.held)
    }

    /// How fast, in px/s, the section under `p` pans for a drag at `p`:
    /// negative leftwards, `0` outside the edge bands or with nowhere to go.
    fn drag_edge_velocity(&self, p: Point) -> f64 {
        let pan = self.pan_of_section(self.section_at(p).unwrap_or(self.focus_section));
        let view = pan.viewport;
        if !view.contains(p) {
            return 0.0;
        }
        let velocity = crate::edge_autoscroll::velocity(p.x, view.x0, view.x1);
        let spare = if velocity < 0.0 {
            pan.scroll_x > 0.0
        } else {
            pan.scroll_x < pan.max_scroll_x()
        };
        if spare { velocity } else { 0.0 }
    }

    /// The background tab of another window under `p`: its group and tab. The tab a window already
    /// shows, the bar a drag started from (it is reordering its own tabs) and the window being
    /// moved are never spring-loaded.
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
                    && self.shown_under(i, p)
            })
            .find_map(|(_, group)| {
                let index = group.tab_rects().iter().position(|r| r.contains(p))?;
                let tab = group.tabs().get(index)?.id;
                (tab != group.active_tab_id()).then_some((group.id, tab))
            })
    }

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
            let section = self.section_at(p).unwrap_or(self.focus_section);
            let pan = self.pan_of_section_mut(section);
            let next = (pan.scroll_x + step).clamp(0.0, pan.max_scroll_x());
            let travel = next - pan.scroll_x;
            if travel.abs() > f64::EPSILON {
                pan.absolute = false;
                pan.jump_to(next);
                self.scroll_dirty = true;
                moved = true;
                let in_section = self.sections.is_empty()
                    || self
                        .group_drag
                        .and_then(|drag| self.section_of_group(drag.group_id))
                        == Some(section);
                if in_section
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

    fn drag_reach_pending(&self) -> bool {
        self.drag_over_row()
            && (self.drag_edge_velocity(self.last_cursor).abs() >= f64::EPSILON
                || self.drag_reach.tab.is_some())
    }

    #[cfg(test)]
    fn advance_drag_reach_for_test(&mut self, now: Instant) -> bool {
        self.advance_drag_reach(now)
    }

    /// Resolve one pointer position into the strip's complete tab-drop grammar. The tab bar wins
    /// and keeps its precise pill-gap insertion; in the body, adaptive outer bands make a new group
    /// while the interior prepends or appends to the existing group's tab list. Only space outside
    /// every group can otherwise produce a new group.
    pub fn tab_drop_target_at(&self, p: Point) -> Option<TabDropTarget> {
        for (group_idx, group) in self.groups.iter().enumerate() {
            let rect = group.get_rect();
            if !self.shown_under(group_idx, p) || self.layout_hides_group(group_idx) {
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

    fn group_insertion_for(&self, p: Point) -> (usize, Option<usize>) {
        let section = match self.group_drag {
            Some(drag) if !self.sections.is_empty() => Some(
                self.section_at(p)
                    .unwrap_or_else(|| self.section_index_of(drag.src_idx)),
            ),
            _ => None,
        };
        let run = section.map_or(0..self.groups.len(), |section| self.section_range(section));
        for i in run.clone() {
            if self
                .groups
                .get(i)
                .is_some_and(|g| p.x < g.get_rect().center().x)
            {
                return (i, section);
            }
        }
        (run.end, section)
    }

    fn commit_group_reorder(
        &mut self,
        src: usize,
        group_id: u64,
        insert_at: usize,
        section: Option<usize>,
    ) {
        if src >= self.groups.len() {
            return;
        }
        let changes_section = section.is_some_and(|section| {
            !self.sections.is_empty() && self.section_index_of(src) != section
        });
        let final_idx = if insert_at > src {
            insert_at.saturating_sub(1)
        } else {
            insert_at
        };
        let dest = final_idx.min(self.groups.len().saturating_sub(1));
        if dest == src && !changes_section {
            return;
        }
        let g = self.groups.remove(src);
        self.groups.insert(dest, g);
        if let Some(section) = section.filter(|_| changes_section) {
            self.group_sections.insert(group_id, section);
        }
        self.structure_dirty = true;
        self.pending_group_reorder = Some((group_id, dest));
    }

    /// A press or touch at the cursor on a group's bar lifts that group as
    /// a drag candidate, unless another drag is already in flight.
    fn begin_group_drag(&mut self, touch: Option<u64>) {
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
        let at = self.last_cursor;
        let anywhere = std::mem::take(&mut self.carry_press);
        if let Some(idx) = self.groups.iter().enumerate().position(|(i, g)| {
            if !self.shown_under(i, at) {
                return false;
            }
            if anywhere {
                return g.get_rect().contains(at);
            }
            if self.canvas.is_some() {
                return g.carry_grab_hit(at) || (touch.is_some() && g.touch_grab_hit(at));
            }
            g.bar_grab_hit(at) || (touch.is_some() && g.touch_grab_hit(at))
        }) && let Some((group_id, held)) = self.groups.get(idx).map(|g| (g.id, g.is_held()))
        {
            self.group_drag = Some(GroupDrag {
                src_idx: idx,
                group_id,
                start: self.last_cursor,
                lifted_at: crate::time::now(),
                moved: false,
                touch,
                held,
                from_tab: false,
            });
        }
    }

    fn move_group_drag(&mut self) -> bool {
        let Some(mut drag) = self.group_drag else {
            return false;
        };
        let mut lifted = false;
        if !drag.moved {
            let dx = self.last_cursor.x - drag.start.x;
            let dy = self.last_cursor.y - drag.start.y;
            if (dx * dx + dy * dy).sqrt() > GROUP_DRAG_THRESHOLD {
                drag.moved = true;
                lifted = true;
            }
        }
        if drag.moved && !drag.held {
            self.carry_window(&drag);
        }
        self.group_drag = Some(drag);
        if drag.moved && drag.held {
            // Pulled, never placed: there is no gap to drop it in.
            return true;
        }
        if drag.moved && self.canvas.is_some() {
            if lifted && let Some(group) = self.groups.iter_mut().find(|g| g.id == drag.group_id) {
                group.cancel_press_for_carry();
            }
            self.carry_on_canvas(drag.group_id, self.last_cursor - drag.start);
            return true;
        }
        if drag.moved && self.floats.contains_key(&drag.group_id) {
            if lifted && let Some(group) = self.groups.iter_mut().find(|g| g.id == drag.group_id) {
                group.cancel_press_for_carry();
            }
            self.carry_float_drag(drag.group_id, drag.start);
            return true;
        }
        if drag.moved {
            if lifted && let Some(group) = self.groups.iter_mut().find(|g| g.id == drag.group_id) {
                group.cancel_press_for_carry();
            }
            let (target, section) = self.group_insertion_for(self.last_cursor);
            self.group_reorder_target = Some(target);
            self.group_reorder_section = section;
            return true;
        }
        false
    }

    fn end_group_drag(&mut self, commit: bool) -> bool {
        let Some(drag) = self.group_drag.take() else {
            return false;
        };
        let claimed = drag.moved;
        match self.let_the_window_go(drag.group_id, commit && drag.from_tab) {
            LetGo::Here => {}
            LetGo::CarriedAway => return claimed,
            LetGo::Joined(into) => {
                // The place the window shows becomes a tab of the window it is let go over, in
                // front there: the row moves it as a tab drag does, and the window it left goes
                // with its last tab.
                if commit && drag.moved {
                    crate::control_hints::performed::note(
                        crate::control_hints::performed::Performed::MovedWindow,
                    );
                }
                if let Some(tab) = self
                    .groups
                    .get(drag.src_idx)
                    .map(WindowGroup::active_tab_id)
                {
                    let at = self
                        .groups
                        .get(into)
                        .map_or(0, |group| group.drop_index_for(self.last_cursor));
                    let _ = self.transfer_tab(drag.src_idx, into, tab, at);
                }
                self.group_reorder_target = None;
                self.group_reorder_section = None;
                return claimed;
            }
        }
        if drag.held {
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
            self.group_reorder_section = None;
            return claimed;
        }
        if self.canvas.is_some() {
            self.put_down_on_canvas(drag.group_id, commit && drag.moved);
            return claimed;
        }
        if self.float_drag_from.is_some() {
            self.put_down_float(drag.group_id, commit && drag.moved);
            self.group_reorder_target = None;
            self.group_reorder_section = None;
            return claimed;
        }
        if commit && drag.moved && !self.covers(self.last_cursor) {
            self.lift_from_drag(drag.group_id);
            return claimed;
        }
        let section = self.group_reorder_section.take();
        if commit && drag.moved {
            crate::control_hints::performed::note(
                crate::control_hints::performed::Performed::MovedWindow,
            );
        }
        if commit
            && drag.moved
            && let Some(insert_at) = self.group_reorder_target.take()
        {
            self.commit_group_reorder(drag.src_idx, drag.group_id, insert_at, section);
        }
        self.group_reorder_target = None;
        claimed
    }

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

    fn begin_touch_pan_candidate(&mut self, id: u64, at: Point) {
        if self.touch_pan.is_none()
            && self.canvas.is_none()
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
        let section = self.section_at(p)?;
        self.sections
            .get(section)
            .map(|section| section.spec.output)
    }

    /// The finger left the display it started on. That is a group rehome,
    /// not a strip pan — #1833 only claimed same-display horizontal swipes.
    fn finger_crossed_display(&self, from: Point, to: Point) -> bool {
        match (self.output_at(from), self.output_at(to)) {
            (Some(from), Some(to)) => from != to,
            _ => false,
        }
    }

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

    /// Whether the pointer rests on the strip's own chrome rather than in a window's
    /// content: canvas, or a window's bar, where no app of the strip can scroll what a
    /// wheel turns.
    #[must_use]
    pub fn pointer_over_chrome(&self) -> bool {
        let p = self.last_cursor;
        self.rect.contains(p)
            && self.edge_grab_at(p).is_none()
            && !self.groups.iter().enumerate().any(|(i, group)| {
                !self.layout_hides_group(i)
                    && group
                        .presentation_bounds()
                        .client
                        .is_some_and(|client| client.contains(p))
            })
    }

    fn over_background(&self, p: Point) -> bool {
        self.rect.contains(p)
            && self.edge_grab_at(p).is_none()
            && self.window_under(p, None).is_none()
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
                    && self.canvas.is_none()
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

    /// Returns `true` when the event is claimed by an in-flight group drag or a one-finger strip
    /// pan, so the caller skips resize-handle and per-child dispatch. A press only records a
    /// *candidate*, so a tap still reaches the child for focus; past [`GROUP_DRAG_THRESHOLD`] the
    /// row claims motion.
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
                if let Some(section) = self.section_at(self.last_cursor) {
                    self.group_sections.insert(new_id, section);
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

    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        self.groups
            .iter_mut()
            .any(|group| FloatingSurfaceProvider::floating_surface_event(group, id, event, ctx))
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

    fn take_pip_area_request(&mut self) -> bool {
        self.groups
            .iter_mut()
            .any(FloatingSurfaceProvider::take_pip_area_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.groups
            .iter_mut()
            .flat_map(FloatingSurfaceProvider::take_floating_surface_source_updates)
            .collect()
    }

    fn draw_floating_surfaces(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        for group in &mut self.groups {
            FloatingSurfaceProvider::draw_floating_surfaces(
                group,
                &mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()),
            );
        }
    }

    fn draw_floating_surface(&mut self, id: &str, ctx: &mut crate::draw::DrawCtx<'_>) -> bool {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.groups.iter_mut().any(|group| {
            FloatingSurfaceProvider::draw_floating_surface(
                group,
                id,
                &mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()),
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

struct RowPlan {
    section_of: Vec<usize>,
    maximized_in: Vec<Option<usize>>,
}

impl RowPlan {
    fn collapsed(&self, sections: usize) -> Vec<bool> {
        (0..sections)
            .map(|section| self.maximized_in.get(section).copied().flatten().is_some())
            .collect()
    }
}

/// Set `node`'s style to `style`, unless it has it: a style set marks the
/// node and its ancestors for layout again.
fn restyle(taffy: &mut TaffyTree<()>, node: NodeId, style: Style) {
    if taffy.style(node).ok() != Some(&style) {
        let _ = taffy.set_style(node, style);
    }
}

impl GroupRow {
    fn prepare_layout(&mut self, theme: &Theme) {
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.tokens.motion;
        self.sync_sibling_fade(theme);
        self.set_width_bases();
        // Every slot springs from where it is drawn to where it goes, the closing one to nothing
        // and its neighbours into the gap, whether or not the strip overflows: the pan's bounds
        // follow where the springs are going ([`Self::dest_content_w`]), so a slot in flight never
        // makes the strip scrollable.
        for group in &mut self.groups {
            group.set_reduce_motion(theme.reduce_motion);
            group.set_motion_source(self.motion_source);
            if group.width_target_is_stale() && !group.is_closing() {
                group.snap_width_spring();
            } else {
                group.step_width_spring();
            }
        }
    }

    fn set_width_bases(&mut self) {
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
            if let Some(at) = self.settle_widths.iter().position(|id| *id == group.id) {
                self.settle_widths.swap_remove(at);
                group.snap_width_spring();
            }
        }
    }

    /// Before the windows lay out: every width fraction resolved (left to right, so each group's
    /// basis is the display its left edge lands on) with no dependency on a previous frame's
    /// layout, which keeps a single-frame offscreen render identical to live; and which window
    /// presents fullscreen.
    fn present_groups(&mut self) {
        self.set_width_bases();
        for group in &mut self.groups {
            group.set_fullscreen_presentation(self.fullscreen_group == Some(group.id));
        }
    }

    fn size_handle_pool(&mut self) {
        let desired = self.desired_handles();
        if self.handles.len() < desired {
            self.handles
                .extend((self.handles.len()..desired).map(|_| ResizeHandle::new()));
        } else if self.handles.len() > desired {
            self.handles.truncate(desired);
        }
    }

    fn desired_handles(&self) -> usize {
        if self.external_navigation {
            0
        } else {
            self.groups.len().saturating_sub(1)
        }
    }

    fn plan_layout(&self) -> RowPlan {
        let section_of: Vec<usize> = (0..self.groups.len())
            .map(|i| self.section_index_of(i))
            .collect();
        let runs = self.sections.len().max(1);
        let mut maximized_in: Vec<Option<usize>> = vec![None; runs];
        if self.canvas.is_some() {
            return RowPlan {
                section_of,
                maximized_in,
            };
        }
        if let Some(fs) = self.fullscreen_idx()
            && let Some(slot) = maximized_in.get_mut(section_of.get(fs).copied().unwrap_or(0))
        {
            *slot = Some(fs);
        }
        for (i, g) in self.groups.iter().enumerate() {
            if !g.maximized {
                continue;
            }
            if let Some(slot) = maximized_in.get_mut(section_of.get(i).copied().unwrap_or(0))
                && slot.is_none()
            {
                *slot = Some(i);
            }
        }
        RowPlan {
            section_of,
            maximized_in,
        }
    }

    /// Whether the borders between windows have nodes: always in a
    /// partitioned strip (one between two sections is parked out of layout),
    /// and while no window is maximized otherwise.
    fn handles_laid(&self, plan: &RowPlan) -> bool {
        if self.canvas.is_some() {
            return false;
        }
        !self.sections.is_empty() || plan.maximized_in.first().copied().flatten().is_none()
    }

    /// The style the row gives window `i`'s node over the window's `own`. `flex_shrink: 0.0` on
    /// every group: Niri-style, placing a new tile never resizes siblings, and `content_width`
    /// grows past the viewport with `draw` clipping to it. `flex_shrink: 1.0` was tried and
    /// rejected: it silently shrinks siblings on every spawn, breaking the infinite-strip
    /// invariant.
    fn group_node_style(&self, plan: &RowPlan, i: usize, own: Style) -> Style {
        if let Some(style) = self.float_node_style(i, &own) {
            return style;
        }
        if let (Some(view), Some(group)) = (&self.canvas, self.groups.get(i)) {
            return Self::canvas_node_style(view, group.id, own);
        }
        let mut style = own;
        style.flex_shrink = 0.0;
        let section = plan.section_of.get(i).copied().unwrap_or(0);
        if let Some(max_i) = plan.maximized_in.get(section).copied().flatten() {
            if i == max_i {
                style.size.width = Dimension::auto();
                style = crate::widget::layout::filling(style, 1.0e6);
            } else {
                style.display = Display::None;
            }
        }
        style
    }

    fn assemble(
        &mut self,
        taffy: &mut TaffyTree<()>,
        plan: &RowPlan,
        group_nodes: &[NodeId],
    ) -> (Vec<NodeId>, Style) {
        let run_style = |display: Display| Style {
            display,
            flex_direction: FlexDirection::Row,
            // Groups past the viewport are the strip's to pan and clip, not the extent of what
            // holds it: otherwise taffy carries their width up as every ancestor's content size and
            // the shell column reports itself as wide as the whole strip. `Clip`, not `Hidden`:
            // the automatic minimum size, and so the layout, stays as it was.
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
        let row_display = if self.hidden {
            Display::None
        } else {
            Display::Flex
        };
        let children: Vec<NodeId> = if self.sections.is_empty() || self.canvas.is_some() {
            let mut children: Vec<NodeId> = Vec::with_capacity(group_nodes.len().saturating_mul(2));
            for (i, gn) in group_nodes.iter().enumerate() {
                children.push(*gn);
                if let Some(Some((hn, own))) = self.handle_nodes.get(i) {
                    restyle(taffy, *hn, own.clone());
                    children.push(*hn);
                }
            }
            children
        } else {
            let row = self.rect;
            let mut section_children: Vec<Vec<NodeId>> = vec![Vec::new(); self.sections.len()];
            let mut parked: Vec<NodeId> = Vec::new();
            let collapsed = plan.collapsed(self.sections.len());
            for (i, gn) in group_nodes.iter().enumerate() {
                let section = plan.section_of.get(i).copied().unwrap_or(0);
                if let Some(run) = section_children.get_mut(section) {
                    run.push(*gn);
                }
                let Some(Some((hn, own))) = self.handle_nodes.get(i) else {
                    continue;
                };
                let same_section =
                    plan.section_of.get(i.saturating_add(1)).copied() == Some(section);
                let mut style = own.clone();
                if same_section && !collapsed.get(section).copied().unwrap_or(false) {
                    if let Some(run) = section_children.get_mut(section) {
                        run.push(*hn);
                    }
                } else {
                    style.display = Display::None;
                    parked.push(*hn);
                }
                restyle(taffy, *hn, style);
            }
            let reuse = self.run_nodes.len() == self.sections.len();
            let mut children = Vec::with_capacity(self.sections.len().saturating_add(parked.len()));
            for (k, (section, run)) in self.sections.iter_mut().zip(section_children).enumerate() {
                let viewport = section.area();
                section.built_viewport = viewport;
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
                match self.run_nodes.get(k).copied().filter(|_| reuse) {
                    Some(node) => {
                        restyle(taffy, node, style);
                        if taffy.children(node).ok().as_deref() != Some(&run[..]) {
                            let _ = taffy.set_children(node, &run);
                        }
                        children.push(node);
                    }
                    None => children.push(taffy.parent_node(style, &run)),
                }
            }
            if !reuse {
                self.run_nodes.clone_from(&children);
            }
            children.extend(parked);
            children
        };
        // Row must fill remaining vertical space so each child's `height: percent(1.0)` resolves,
        // or the row measures only as tall as the TabBar and `TileHost` collapses to zero. Children
        // beyond the viewport are still walked and shifted into screen space by the pan, and stay
        // hit-testable since `gap_index_for` uses each group's stored post-shift `Rect`. Its height
        // is the band it is given, never its windows' content: with the automatic minimum, a long
        // document's content height reached the row through every window and raised them all past
        // the screen.
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
        (children, style)
    }

    fn laid_same_groups(&self) -> bool {
        self.node.is_some()
            && !(self.external_navigation && self.hidden)
            && self.laid_group_ids.len() == self.groups.len()
            && self.node_ids.len() == self.groups.len()
            && self
                .groups
                .iter()
                .zip(&self.laid_group_ids)
                .zip(&self.node_ids)
                .all(|((group, &id), &node)| group.id == id && group.layout_node() == Some(node))
    }

    /// The row's node in the retained layout tree, while it has one.
    #[must_use]
    pub fn layout_node(&self) -> Option<NodeId> {
        self.node
    }
}

impl Widget for GroupRow {
    crate::provides!(
        dyn crate::widget::FloatingSurfaceProvider,
        #[cfg(feature = "model")]
        dyn crate::widget::presence_topology::CollaborationTopology,
        dyn crate::component_lifecycle::ComponentLifecycle
    );

    // Modal capture and presentation polling follow visible content, never inactive tabs.
    fn modal_capture(&self) -> Option<Rect> {
        self.children().into_iter().find_map(Widget::modal_capture)
    }
    /// A group the latest frame left unpainted (outside the row) had that frame for every deadline
    /// due by its sync. One still pending waits for the group's paint (a status pill that clears
    /// itself when drawn), so it must not wake the host every frame; its later deadlines (an alarm,
    /// a poll) still wake it.
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

    fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        fn drawn(scene: &Scene) -> (usize, usize) {
            let encoding = scene.encoding();
            (encoding.path_tags.len(), encoding.draw_tags.len())
        }
        self.overlay_groups.clear();
        for index in self.shown_group_indices() {
            let before = drawn(scene);
            let group = &mut self.groups[index];
            group.draw_overlay(&mut crate::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ));
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

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        self.for_each_child(visitor);
        for handle in &self.handles {
            visitor(handle);
        }
        true
    }

    /// Every group the row holds, the culled ones included: each still owns
    /// its windows' records.
    fn retained_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.groups
            .iter_mut()
            .map(|group| group as &mut dyn Widget)
            .collect()
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
        // A pan in motion (a throw, a settle, a reveal, fingers resampled at the frame clock)
        // advances here, once per host frame. Moving marks the row `scroll_dirty` and asks for a
        // layout pass that re-applies the windows' positions; it never rebuilds or re-measures the
        // strip.
        if !self.hidden {
            if self.node_ids.len() == self.groups.len() {
                self.prune_closed_groups();
            }
            let now = crate::motion::frame_now();
            self.tick_snap_at(now);
            self.advance_drag_reach(now);
            self.advance_edge_autoscroll(now);
            if self.scroll_dirty {
                crate::widget::note_layout_moved();
            }
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
        self.prepare_layout(theme);
        self.node_ids.clear();
        self.node = None;
        self.run_nodes.clear();
        self.handle_nodes.clear();
        if self.external_navigation && self.hidden {
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
        self.present_groups();
        let group_nodes: Vec<NodeId> = self
            .groups
            .iter_mut()
            .map(|g| g.build_layout(taffy, theme, node_map))
            .collect();
        self.node_ids.clone_from(&group_nodes);
        self.laid_group_ids = self.groups.iter().map(|group| group.id).collect();
        self.size_handle_pool();
        let plan = self.plan_layout();
        for (i, &node) in group_nodes.iter().enumerate() {
            let own = taffy.style(node).cloned().unwrap_or_default();
            let _ = taffy.set_style(node, self.group_node_style(&plan, i, own));
        }
        let laid = self.handles_laid(&plan);
        for i in 0..group_nodes.len().saturating_sub(1) {
            let node = self.handles.get_mut(i).filter(|_| laid).map(|handle| {
                let node = handle.build_layout(taffy, theme, node_map);
                (node, taffy.style(node).cloned().unwrap_or_default())
            });
            self.handle_nodes.push(node);
        }
        let (children, style) = self.assemble(taffy, &plan, &group_nodes);
        self.laid_sections.clone_from(&plan.section_of);
        self.laid_collapsed = plan.collapsed(self.sections.len());
        let node = taffy.widget_node(style, &children, node_map, self);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if self.external_navigation && self.hidden {
            self.rect = Rect::ZERO;
            return;
        }
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        // A floating window is laid out at its own rect, never in the flow
        // of a section or canvas: what the strip measures stays its own.
        self.mark_floating();
        let floating = self.floating_flags();
        // Portable hosts learn their width here. Rebuild provisional widths
        // in the same bounded settlement, including a same-viewport root swap.
        self.structure_dirty |= !self.width_basis_is_current();
        if let Some(view) = self.canvas.clone() {
            self.apply_canvas_layout(taffy, &view);
            self.apply_float_layout(taffy);
            self.sync_handle_neighbours();
            self.recheck_edge_hover();
            return;
        }
        if self.sections.is_empty() {
            self.pan.viewport = self.rect;
            self.pan.inset = self.leading_inset;
        }
        for section in &mut self.sections {
            let area = section.area();
            if (area.y0 - section.built_viewport.y0).abs() > 0.5
                || (area.height() - section.built_viewport.height()).abs() > 0.5
                || (area.x0 - section.built_viewport.x0).abs() > 0.5
            {
                self.structure_dirty = true;
            }
            section.pan.viewport = section.viewport();
            section.pan.inset = if section.spec.leading {
                self.leading_inset
            } else {
                0.0
            };
        }
        let section_of: Vec<usize> = (0..self.groups.len())
            .map(|i| self.section_index_of(i))
            .collect();
        let runs = self.sections.len().max(1);
        let insets: Vec<f64> = (0..runs).map(|k| self.pan_of_section(k).inset).collect();
        let mut content_w: Vec<f64> = insets.clone();
        let mut edges: Vec<Vec<f64>> = vec![Vec::new(); runs];
        // Only trustworthy when taffy has a node per group — a pass
        // landing between a splice and the rebuild would otherwise pair
        // widths with the wrong ids.
        let spans_aligned = self.node_ids.len() == self.groups.len();
        let mut spans: Vec<Vec<(u64, f64, f64)>> = vec![Vec::new(); runs];
        for (i, &cn) in self.node_ids.iter().enumerate() {
            if floating.get(i).copied().unwrap_or(false) {
                continue;
            }
            let cl = taffy.layout(cn).copied().unwrap_or_default();
            let k = section_of
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
            let pan = self.pan_of_section_mut(k);
            // The strip the springs are going to, not where they are: a slot in flight can lay out
            // wider for a moment and must not make the strip scrollable meanwhile.
            pan.content_w = if dest > pan.inset { dest } else { w };
            pan.group_left_edges = e;
            if spans_aligned && !edge_resizing {
                pan.hold_view_across_close(&s, &going);
            }
            let before = pan.scroll_x;
            // Preserve transient rubber-band travel while fingers are down or a release settle is
            // in flight. At rest, geometry changes clamp to the deliberate trailing allowance
            // instead of the old three viewports of unbounded canvas: when the strip ends sooner
            // (its last window closed, or the screen widened) the pan stays within the windows,
            // since left in the allowance it would strand the view in empty canvas nobody panned
            // to, with windows cut off at the other edge. Only a person's own pan there keeps it.
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
        if self.snap_in_flight() {
            crate::widget::note_layout_moved();
        }
        let collapsed: Vec<bool> = (0..runs)
            .map(|k| {
                self.groups.iter().enumerate().any(|(i, g)| {
                    section_of.get(i).copied().unwrap_or(0) == k
                        && (g.maximized || self.fullscreen_group == Some(g.id))
                })
            })
            .collect();
        let origins: Vec<Point> = (0..runs)
            .map(|k| {
                let pan = self.pan_of_section(k);
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
            if hide.get(i).copied().unwrap_or(false) {
                continue;
            }
            if floating.get(i).copied().unwrap_or(false) {
                continue;
            }
            let k = section_of.get(i).copied().unwrap_or(0);
            let origin = origins.get(k).copied().unwrap_or(Point::ORIGIN);
            g.apply_layout(taffy, *child_id, origin);
        }
        self.apply_float_layout(taffy);
        // Measured on screen, after the pan: a close or insert re-frames
        // the pan in this same pass, and only the drawn position is what
        // must not jump. Only trusted when taffy has a node per group.
        if spans_aligned {
            self.record_shift_anim(now);
        }
        self.sync_handle_neighbours();
        self.recheck_edge_hover();
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
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
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.tokens.motion;
        self.sync_sibling_fade(theme);
        self.sibling_fade_drawn_moving = self.sibling_fade_in_flight();
        if self.external_navigation && self.hidden {
            self.pan_drawings.clear();
            return;
        }
        self.scroll_dirty = false;
        let section_of: Vec<usize> = (0..self.groups.len())
            .map(|i| self.section_index_of(i))
            .collect();
        let runs: Vec<(usize, Rect)> = if self.sections.is_empty() {
            vec![(0, self.rect)]
        } else {
            self.sections
                .iter()
                .enumerate()
                .filter(|(_, section)| !section.off_display())
                .map(|(k, section)| (k, section.pan.viewport))
                .collect()
        };
        for (k, viewport) in runs {
            if viewport.width() <= 0.0 || viewport.height() <= 0.0 {
                continue;
            }
            let pan = self.pan_of_section(k);
            let scroll = self.drawn_pan_x(pan);
            let max_scroll = pan.max_scroll_x();
            let pad = self.section_canvas_pad(pan);
            let unbake = crate::retained_present::unbake_scroll()
                && self.canvas.is_none()
                && pan_within_canvas(scroll, max_scroll, pad);
            let clip_right = if unbake {
                viewport.x1 + max_scroll + 2.0 * pad
            } else {
                viewport.x1
            };
            let shift = if unbake { scroll + pad } else { 0.0 };
            let shifted = shift.abs() > f64::EPSILON;
            let rested = scroll + pad;
            let kept = if let Some((_, kept, was_screen, drawn)) = self
                .strip_canvas_shift
                .iter_mut()
                .find(|(section, ..)| *section == k)
            {
                if !unbake && *was_screen {
                    *kept = rested;
                }
                *was_screen = !unbake;
                *drawn = rested;
                *kept
            } else {
                self.strip_canvas_shift.push((k, rested, !unbake, rested));
                rested
            };
            let drawn_shift = if unbake { kept } else { kept - rested };
            self.drawing_shift = Affine::translate((drawn_shift, 0.0));
            if shifted {
                self.offset_section(k, shift);
            }
            // The stretch of the canvas this draw's pan shows: the viewport, moved by the shift
            // that took the groups there. On a canvas, windows stand anywhere, panned past the
            // row's edges too: what is drawn stops at the row, never over the top bar above it.
            let bounds = if self.canvas.is_some() {
                viewport
            } else {
                Self::drawn_bounds(viewport)
            };
            let clip = Rect {
                x1: clip_right,
                ..bounds
            };
            self.apart_window = unbake
                .then(|| self.apart_window_of_section(k, viewport, shift, (clip.y0, clip.y1)));
            let in_run: Vec<bool> = section_of.iter().map(|&section| section == k).collect();
            let maximized_idx =
                self.fullscreen_idx()
                    .filter(|&i| in_run.get(i).copied().unwrap_or(false))
                    .or_else(|| {
                        self.groups.iter().enumerate().position(|(i, g)| {
                            g.maximized && in_run.get(i).copied().unwrap_or(false)
                        })
                    });
            // A maximized window at rest inside the viewport is the section's only drawing: the clip
            // would cut nothing, and left out, the window's client frame stays the last thing
            // drawn, which a display can scan out instead of compositing.
            let clipped = !maximized_idx
                .and_then(|i| self.groups.get(i))
                .is_some_and(|group| !shifted && clip.contains_rect(group.drawn_rect()));
            if clipped {
                // A clipped section is one damage part, there whichever of its groups draw, bounded by
                // the clip that bounds them: a group the row leaves out and draws again changes
                // only parts inside the section, which then damages the section alone, not every display.
                crate::scene_parts::begin(
                    scene,
                    crate::scene_parts::PartKey::new("ui.strip-section", k),
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
                &mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()),
                maximized_idx,
                &in_run,
            );
            crate::window_pictures::unseen(self.culled_groups.iter());
            crate::window_pictures::settled(self.groups.iter().map(|group| &group.id));
            self.draw_handles(
                &mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()),
                maximized_idx,
                &in_run,
            );
            if clipped {
                scene.pop_layer();
                crate::scene_parts::end(scene, vec![transform.transform_rect_bbox(clip)]);
            }
            if shifted {
                self.offset_section(k, -shift);
            }
        }
        if self.apart_window.take().is_none() {
            self.apart_drawings.clear();
            self.apart_spans.clear();
        }
        // A window this frame did not draw or replay at rest may change
        // before it is shown again, and a frame that is not only a pan may
        // have changed any of them.
        let kept = &self.pan_kept;
        self.pan_drawings
            .retain(|drawing| kept.contains(&drawing.id));
        self.draw_floats(scene, transform, (tr, theme, &env));
        self.draw_group_drag_preview(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            tr,
            theme,
            env.clone(),
        ));
        self.draw_reorder_capsule(scene, transform, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.external_navigation && self.hidden {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.refresh_tab_drop_preview();
        }

        // Runs before resize-handle and per-child dispatch so, once past threshold, it claims the
        // motion and release they would otherwise contend for. A press records a candidate without
        // claiming, so a plain click still reaches the child for focus.
        if self.update_pointer_pan(event) || self.update_group_drag(event, ctx.scale_factor) {
            *ctx.handled = true;
            return;
        }

        // A bare wheel NEVER moves the strip — that only happens via
        // `scroll_group_into_view_animated`, not this event path.

        // A clipped group's trailing `+` can overlap the invisible resize border at the viewport
        // edge. The exact painted control owns both the cursor update and the press; otherwise the
        // border keeps its usual first refusal. Forwarding the move matters because button events
        // carry no coordinates.
        let stands_here: Vec<bool> = (0..self.groups.len())
            .map(|i| !self.layout_hides_group(i) && self.on_display_under(i, self.last_cursor))
            .collect();
        let new_tab_owned = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) && self.groups.iter().enumerate().any(|(i, group)| {
            stands_here.get(i).copied().unwrap_or(false) && group.new_tab_hit(self.last_cursor)
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
                stands_here.get(*i).copied().unwrap_or(false) && group.new_tab_hit(cursor)
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
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && let Some(drag) = self.group_drag.as_mut()
        {
            drag.from_tab = self
                .groups
                .get(drag.src_idx)
                .is_some_and(WindowGroup::pressed_a_tab);
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

        Some(
            AccessNode::new(Role::Group).with_name(crate::i18n::widget_format(
                "widget-hosted-applications",
                &[],
            )),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The shell gave the keyboard to another window of the row since the frame last asked: a search
    /// chose it, it opened, the one in front closed, or the person clicked it. Its app is the shell's
    /// to name ([`crate::application::AppActivation::Ready`]); the row reports the move.
    fn take_app_activation(&mut self, _ready: bool) -> Option<crate::application::AppActivation> {
        let now = self
            .groups
            .iter()
            .find(|group| group.focused())
            .map(|group| group.id);
        let before = std::mem::replace(&mut self.keyboard_window, now);
        (before.is_some() && now.is_some() && now != before)
            .then_some(crate::application::AppActivation::Moved)
    }

    fn wants_pointer_capture(&self) -> bool {
        (self.canvas.is_some() && self.group_drag.is_some_and(|drag| drag.moved))
            || self.pointer_pan.is_some_and(|pan| pan.engaged)
            || self.edge_drag.is_some()
            || self.groups.iter().any(super::Widget::wants_pointer_capture)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.edge_drag.is_some() || self.edge_grab_at(point).is_some() {
            return Some(winit::window::CursorIcon::EwResize);
        }
        if self.group_drag.is_some() || self.pointer_pan.is_some_and(|pan| pan.engaged) {
            return Some(winit::window::CursorIcon::Grabbing);
        }
        let stands_here = |i: usize| !self.layout_hides_group(i) && self.on_display_under(i, point);
        if self
            .groups
            .iter()
            .enumerate()
            .any(|(i, group)| stands_here(i) && group.bar_grab_hit(point))
        {
            return Some(winit::window::CursorIcon::Grab);
        }
        self.groups
            .iter()
            .enumerate()
            .filter(|(i, _)| stands_here(*i))
            .find_map(|(_, group)| group.cursor_shape(point))
    }

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
        for (i, h) in self.handles.iter().enumerate() {
            let r = h.get_rect();
            if r.area() > 0.0 {
                out.push((format!("group_resize_handle[{i}]"), r));
            }
        }
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
        if self.scroll_dirty || self.sibling_fade_needs_frame() || self.float_fade_needs_frame() {
            return true;
        }
        if self
            .groups
            .iter()
            .any(|group| crate::window_pictures::owed(group.id))
        {
            return true;
        }
        if self
            .strip_canvas_shift
            .iter()
            .any(|(_, kept, was_screen, drawn)| *was_screen && (kept - drawn).abs() > f64::EPSILON)
        {
            return true;
        }
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
        // The row owns drag animations outside its presentation children; propagate them alongside
        // child animation requests. Off-screen groups use the same cull as `needs_redraw`: a hosted
        // app past the strip edge must not keep the shell at the refresh rate.
        self.snap_in_flight()
            || self.shift_in_flight()
            || self.drag_reach_pending()
            || self.edge_autoscroll_pending()
            || self.group_drag.is_some_and(|drag| drag.moved && !drag.held)
            || self
                .group_resist_back
                .is_some_and(|(id, _)| self.group_pull(id).is_some())
            || self.sibling_fade_needs_frame()
            || self.float_fade_needs_frame()
            || self
                .groups
                .iter()
                .filter(|g| self.paints_within_row(g.get_rect(), 0.0))
                .any(super::Widget::needs_animation_frame)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }
    fn take_structure_change(&mut self) -> super::StructureChange {
        let own = if std::mem::take(&mut self.structure_dirty) {
            if self.laid_same_groups() {
                super::StructureChange::Children
            } else {
                super::StructureChange::Rebuild
            }
        } else {
            super::StructureChange::None
        };
        self.groups.iter_mut().fold(own, |change, group| {
            change.join(group.take_structure_change())
        })
    }
    fn remount_children(&mut self, remount: &mut super::Remount<'_>) -> bool {
        let Some(row) = self.node else {
            return false;
        };
        if !self.laid_same_groups() || self.desired_handles() != self.handles.len() {
            return false;
        }
        self.prepare_layout(remount.theme());
        self.present_groups();
        if !self
            .groups
            .iter_mut()
            .all(|group| group.remount_in_row(remount))
        {
            return false;
        }
        let plan = self.plan_layout();
        let partitioned = !self.sections.is_empty() && self.canvas.is_none();
        if partitioned
            && (plan.section_of != self.laid_sections
                || plan.collapsed(self.sections.len()) != self.laid_collapsed
                || self.run_nodes.len() != self.sections.len())
        {
            return false;
        }
        let group_nodes = self.node_ids.clone();
        for (i, (group, &node)) in self.groups.iter().zip(&group_nodes).enumerate() {
            restyle(
                remount.taffy(),
                node,
                self.group_node_style(&plan, i, group.own_style()),
            );
        }
        let laid = self.handles_laid(&plan);
        let gaps = group_nodes.len().saturating_sub(1);
        if self.handle_nodes.len() != gaps {
            return false;
        }
        for (slot, handle) in self.handle_nodes.iter_mut().zip(self.handles.iter_mut()) {
            match (laid, slot.as_ref()) {
                (true, None) => {
                    let node = remount.mount(row, handle);
                    let own = remount.taffy().style(node).cloned().unwrap_or_default();
                    *slot = Some((node, own));
                }
                (false, Some(&(node, _))) => {
                    remount.unmount(node);
                    *slot = None;
                }
                _ => {}
            }
        }
        let (children, style) = self.assemble(remount.taffy(), &plan, &group_nodes);
        restyle(remount.taffy(), row, style);
        if remount.taffy().children(row).ok().as_deref() != Some(&children[..]) {
            let _ = remount.set_children(row, &children);
        }
        true
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

#[cfg(feature = "model")]
impl crate::widget::presence_topology::CollaborationTopology for GroupRow {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.groups
            .iter_mut()
            .map(|group| group as &mut dyn Widget)
            .collect()
    }
}

impl GroupRow {
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
        let sections_match: Vec<bool> = (0..self.handles.len())
            .map(|i| self.section_index_of(i) == self.section_index_of(i.saturating_add(1)))
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
            if !sections_match.get(i).copied().unwrap_or(true) {
                h.set_neighbour_rects(Rect::ZERO, Rect::ZERO);
                continue;
            }
            let floating = |index: usize| {
                self.groups
                    .get(index)
                    .is_some_and(|group| self.floats.contains_key(&group.id))
            };
            if floating(i) || floating(i.saturating_add(1)) {
                h.set_neighbour_rects(Rect::ZERO, Rect::ZERO);
                continue;
            }
            h.set_neighbour_rects(left, right);
        }
    }

    fn draw_groups(
        &mut self,
        ctx: &mut crate::draw::DrawCtx<'_>,
        maximized_idx: Option<usize>,
        in_run: &[bool],
    ) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        self.culled_groups.clear();
        if let Some(max_i) = maximized_idx {
            let fade_alpha =
                crate::motion::unit_opacity(self.sibling_fade.progress(crate::motion::frame_now()));
            if fade_alpha > f32::EPSILON {
                for i in 0..self.groups.len() {
                    let sibling = &mut self.groups[i];
                    if i == max_i
                        || !in_run.get(i).copied().unwrap_or(false)
                        || self.floats.contains_key(&sibling.id)
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
                    sibling.draw_child(scene, transform, tr, theme, &env);
                    scene.pop_layer();
                }
            }
            if max_i < self.groups.len() {
                self.draw_group_plain(max_i, scene, transform, (tr, theme, &env));
            }
            return;
        }
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
            if self.floats.contains_key(&self.groups[i].id) {
                continue;
            }
            let (id, rect) = (self.groups[i].id, self.groups[i].get_rect());
            let dx = self.move_state_at(id, now).0;
            if !self.paints_within_row(rect, dx) {
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
            // A damage part of its own: a strip that reads the shell again
            // repaints only itself.
            match self.apart_window {
                // Only a group at rest in its slot: one moving, lifted or
                // fading is drawn every frame.
                Some(window)
                    if !source_is_lifted && !fading_sibling && dx.abs() <= f64::EPSILON =>
                {
                    self.draw_group_apart(i, window, scene, t, (tr, theme, &env));
                }
                // Only a group at rest in its slot, as above.
                None if crate::retained_present::pan_replay()
                    && !source_is_lifted
                    && !fading_sibling
                    && dx.abs() <= f64::EPSILON =>
                {
                    self.draw_group_replaying(i, scene, t, (tr, theme, &env));
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
                    self.draw_group_plain(i, scene, t, (tr, theme, &env));
                }
            }
            if source_is_lifted {
                scene.pop_layer();
            }
            if fading_sibling {
                scene.pop_layer();
            }
        }
        // A window wanted small that this frame drew only in part, or not at all, is drawn whole
        // once for its picture.
        for i in 0..self.groups.len() {
            if in_run.get(i).copied().unwrap_or(false) {
                self.picture_unseen_group(i, transform, (tr, theme, &env));
            }
        }
        if self.apart_window.is_some() {
            let groups = &self.groups;
            self.apart_drawings
                .retain(|drawing| groups.iter().any(|group| group.id == drawing.id));
        }
    }

    fn apart_window_of_section(
        &mut self,
        k: usize,
        viewport: Rect,
        shift: f64,
        rows: (f64, f64),
    ) -> ApartWindow {
        let span = (viewport.x0 + shift, viewport.x1 + shift);
        let bits = |(x0, x1): (f64, f64)| (x0.to_bits(), x1.to_bits());
        let last = self
            .apart_spans
            .iter_mut()
            .find(|(section, _)| *section == k);
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

    /// Draw group `i` of a row drawn in canvas coordinates, of which the screen shows `window`.
    /// While the strip pans, a group wholly off screen replays its last drawing when that was made
    /// at the same canvas rect and transform; on a frame that only moved laid-out content
    /// ([`crate::retained_present::PanFrame::only_moved`]) so does a group on screen that is at
    /// rest ([`Self::group_at_rest`]) whose drawing is current and shows all the screen shows of
    /// it. Any other group is drawn and its drawing kept; a strip at rest after any other frame
    /// draws every group. No stale pixel is shown: off screen nothing of it is, and on screen it
    /// replays only a drawing made since anything last changed, at the canvas rect it still has.
    /// The strip's drawing stays byte for byte the same while the pan only moves it, which is what
    /// lets the presenter move its pixels instead of rasterising. Like a culled group, a replayed
    /// one keeps its dirty state for its next real draw.
    fn draw_group_apart(
        &mut self,
        i: usize,
        window: ApartWindow,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, env): (&mut TextRenderer, &Theme, &crate::draw::DrawEnv<'_>),
    ) {
        let (id, rect) = (self.groups[i].id, self.groups[i].get_rect());
        let reach = rect.inflate(GROUP_SPRITE_REACH_PX, 0.0);
        let on_screen = reach.x1 > window.span.0 && reach.x0 < window.span.1;
        // The group is a damage part of the strip: a presenter that keeps
        // the strip's pixels rasterises only where a group's drawing changed
        // (a window off screen that changed while the strip rested: none of
        // the screen). Everything it paints stays inside its sprites' reach
        // and the section's clip.
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
        let at_rest = only_moved && on_screen && self.group_at_rest(i, crate::time::now());
        if ((window.panning && !on_screen) || only_moved)
            && let Some(drawing) = kept.and_then(|index| self.apart_drawings.get_mut(index))
            && drawing.rect == rect
            && drawing.transform == transform
            && (!on_screen || (at_rest && drawing.current))
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
        let drawing = self.record_group(i, local, transform, (tr, theme, env));
        let carried =
            crate::scene_parts::part(scene, key, bounds, |scene| drawing.append_to(scene));
        drawing.offer_picture(carried);
        if let Some(drawing) = drawing.keep(carried) {
            self.apart_drawings.push(drawing);
        }
    }

    fn draw_group_plain(
        &mut self,
        i: usize,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, env): (&mut TextRenderer, &Theme, &crate::draw::DrawEnv<'_>),
    ) {
        if !crate::window_pictures::wanted(self.groups[i].id) {
            self.groups[i].draw_child(scene, transform, tr, theme, env);
            return;
        }
        let drawing = self.record_group(i, Scene::new(), transform, (tr, theme, env));
        let carried = drawing.append_to(scene);
        drawing.offer_picture(carried);
    }

    fn record_group(
        &mut self,
        i: usize,
        mut local: Scene,
        transform: Affine,
        (tr, theme, env): (&mut TextRenderer, &Theme, &crate::draw::DrawEnv<'_>),
    ) -> GroupDrawing {
        local.reset();
        crate::backdrop::nest_scene(&local);
        let secrets = crate::secret_paint::SecretPaint::begin();
        let group = &mut self.groups[i];
        let (id, rect) = (group.id, group.get_rect());
        let ((), parts) = crate::scene_parts::record(&mut local, |local| {
            group.draw_child(local, transform, tr, theme, env);
        });
        GroupDrawing(ApartDrawing {
            id,
            rect,
            transform,
            scene: local,
            parts,
            shows_secret: secrets.finish(),
            seen: seen_across(env, rect),
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

    /// Draw group `i` of a strip drawn in screen space on a frame that only moves laid-out content
    /// ([`crate::retained_present::pan_replay`]). A window at rest ([`Self::group_at_rest`]) whose
    /// last drawing was made at its size, height and transform in the frame just before, and culled
    /// none of what the screen shows of it now, replays that drawing moved by how far the strip
    /// panned since. Any other window at rest is drawn, culled as on any frame, and its drawing
    /// kept; a window not at rest, or whose drawing cannot be kept, is drawn as on any frame. What
    /// the window's widgets note while drawing still describes where it was drawn; the host draws
    /// again before input or a semantics read reaches them
    /// ([`crate::retained_present::PanReplay`]). Like a culled window, a replayed one keeps its
    /// dirty state for its next real draw.
    fn draw_group_replaying(
        &mut self,
        i: usize,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, env): (&mut TextRenderer, &Theme, &crate::draw::DrawEnv<'_>),
    ) {
        let (id, rect) = (self.groups[i].id, self.groups[i].get_rect());
        if !self.group_at_rest(i, crate::time::now()) || self.pan_unkept.contains(&id) {
            self.draw_group_plain(i, scene, transform, (tr, theme, env));
            return;
        }
        let kept = self
            .pan_drawings
            .iter()
            .position(|drawing| drawing.id == id);
        self.pan_kept.push(id);
        if let Some(drawing) = kept.and_then(|index| self.pan_drawings.get(index))
            && drawing.transform == transform
            && drawing.rect.size() == rect.size()
            && drawing.rect.y0.to_bits() == rect.y0.to_bits()
        {
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
        let drawing = self.record_group(i, local, transform, (tr, theme, env));
        let carried = drawing.append_to(scene);
        drawing.offer_picture(carried);
        match drawing.keep(carried) {
            Some(drawing) => self.pan_drawings.push(drawing),
            None => self.pan_unkept.push(id),
        }
    }

    fn draw_group_drag_preview(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
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
        group.draw_group_drag_visual(&mut crate::draw::DrawCtx::new(
            scene,
            preview_transform,
            tr,
            theme,
            env.clone(),
        ));
    }

    fn group_drag_lift(&self, theme: &Theme) -> Option<Affine> {
        let drag = self.group_drag.filter(|drag| drag.moved && !drag.held)?;
        if self.is_floating(drag.group_id) {
            return None;
        }
        let progress = if theme.reduce_motion {
            1.0
        } else {
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
        ctx: &mut crate::draw::DrawCtx<'_>,
        maximized_idx: Option<usize>,
        in_run: &[bool],
    ) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        let any_drag = self
            .groups
            .iter()
            .any(super::window_group::WindowGroup::drag_in_flight);
        if any_drag || maximized_idx.is_some() || self.canvas.is_some() {
            return;
        }
        let now = crate::motion::frame_now();
        let offsets: Vec<f64> = self
            .groups
            .iter()
            .map(|g| self.move_state_at(g.id, now).0)
            .collect();
        for (i, h) in self.handles.iter_mut().enumerate() {
            let shown = |g: Option<&WindowGroup>| g.is_some_and(WindowGroup::stands);
            let next_i = i.saturating_add(1);
            // Only a gap between two groups of this run; the border between
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
            h.draw_child(scene, t, tr, theme, &env);
        }
    }

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
        let end_offset = HANDLE_SLOT_PX / 2.0;
        let (run, band) = match self.section_at(self.last_cursor) {
            Some(section) => (
                self.section_range(section),
                self.pan_of_section(section).viewport,
            ),
            None => (0..self.groups.len(), self.rect),
        };
        let gap_x = if run.is_empty() {
            band.x0
                + self
                    .pan_of_section(self.section_at(self.last_cursor).unwrap_or(0))
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

    fn dispatch_to_groups(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Mouse buttons and wheels carry no position, so dispatch the group that owns the
        // remembered cursor (or an active capture) before siblings. What the pointer stands on
        // may take it, and so may a group holding the pointer from a press of its own still under
        // way (a tab dragged onto another display): what the strip draws under a point is what
        // may take it, so a window laid out past its display's edge, clipped there, takes nothing
        // on the next display. A release and a key press reach every group: they end or start
        // what a group holds, wherever the pointer stands.
        let pressing = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        let takes_pointer: Vec<bool> = (0..self.groups.len())
            .map(|i| {
                !self.layout_hides_group(i)
                    && (self.on_display_under(i, self.last_cursor)
                        || self.groups[i].wants_pointer_capture())
            })
            .collect();
        // Edge grips first, for every group: they overlay the windows' own
        // content, so no group's tile may swallow the pointer before the group
        // under it has seen it.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) {
            for i in 0..self.groups.len() {
                if !takes_pointer.get(i).copied().unwrap_or(false) {
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
                    !self.layout_hides_group(i)
                        && self.on_display_under(i, self.last_cursor)
                        && group.new_tab_hit(self.last_cursor)
                })
                .or_else(|| {
                    self.groups.iter().enumerate().position(|(i, group)| {
                        !self.layout_hides_group(i) && group.wants_pointer_capture()
                    })
                })
                .or_else(|| self.float_under_cursor())
                .or_else(|| self.window_under(self.last_cursor, None))
        });
        let preferred = preferred.flatten();
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
            let owner = first_shown(WindowGroup::takes_typing)
                .or_else(|| first_shown(|group| super::context_menu_area::focus_within(group)))
                .or_else(|| first_shown(WindowGroup::focused));
            if let Some(i) = owner
                && let Some(&node_id) = self.node_ids.get(i)
                && let Some(group) = self.groups.get_mut(i)
            {
                group.handle_event(event, &mut ctx.reborrow(ctx.path, node_id));
                // The window that holds the keyboard has the keys: what it does not take is no
                // other window's, and not input the host holds for content on its way.
                *ctx.handled = true;
                return;
            }
        }
        let mut order = Vec::with_capacity(self.groups.len());
        if let Some(i) = preferred {
            order.push(i);
        }
        if !(wheel && preferred.is_some()) {
            order.extend((0..self.groups.len()).filter(|i| {
                Some(*i) != preferred
                    && !self.layout_hides_group(*i)
                    && (!pressing || takes_pointer.get(*i).copied().unwrap_or(false))
            }));
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
        let prune_count = prune.len();
        prune.compact_sort_by(|a, b| b.cmp(a));
        for i in prune {
            let mut group = self.groups.remove(i);
            self.close_reports
                .extend(crate::component_lifecycle::close_components(&mut group));
        }
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
                self.structure_dirty = true;
                diverted = Some(idx);
                *handled = true;
                break;
            }
        }

        if let Some(skip_idx) = diverted {
            detaches.remove(skip_idx);
        }

        for (src_idx, tab_id, _release_point) in detaches {
            self.resolve_one_tab_detach(src_idx, tab_id, self.tab_drop_target, handled);
        }
    }

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
            Some(TabDropTarget::GroupGap { gap_index }) => {
                self.pending_split_section = self.section_at(self.last_cursor);
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
#[path = "group_row_drag_tests.rs"]
mod drag_tests;
#[cfg(test)]
#[path = "group_row_edge_tests.rs"]
mod edge_tests;
#[cfg(test)]
#[path = "group_row_float_tests.rs"]
mod float_tests;
#[cfg(test)]
#[path = "group_row_keyboard_tests.rs"]
mod keyboard_tests;
#[cfg(test)]
#[path = "group_row_pacing_tests.rs"]
mod pacing_tests;
#[cfg(test)]
#[path = "group_row_presence_tests.rs"]
mod presence_tests;
#[cfg(test)]
#[path = "group_row_section_tests.rs"]
mod section_tests;
#[cfg(test)]
#[path = "group_row_sharing_tests.rs"]
mod sharing_tests;
#[cfg(test)]
#[path = "group_row_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "group_row_making_way_tests.rs"]
mod making_way_tests;
