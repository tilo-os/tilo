use crate::color_compat::ColorCompat;
use crate::dnd::{DRAG_START_THRESHOLD_PX, DragPayload};
use crate::icons;
use crate::paint::PaintCtx;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::tokens::colors::Colors;
use crate::widget::AppKind;
use crate::widget::ModelPath;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::hover_intent::{HoverIntent, Item as IntentItem, Sample as IntentSample};
use crate::widget::window_header::HistoryControl;
use std::collections::{HashMap, HashSet};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::motion::{Direction, Disclose, ListChange, MotionSpec, Snap};
use crate::tokens::{EffectiveMotion, MotionSource};

/// Drop-target indicator line width; thin enough not to compete with pill chrome.
const HOVER_DROP_LINE_PX: f64 = 1.5;
/// The tab bar's motions, from the standard ones (`motion::ListChange`
/// and friends): pills change places on the list change's move, arrive and
/// leave on its fade (opacity and a small rise; no per-pill scale layer), a
/// pill opens its chips on the disclosure, a held tab springs back as a
/// let-go surface settles (`motion::Snap`), and the drop line and a lifted
/// tab answer on the fade.
fn tab_progress(spec: MotionSpec, direction: Direction, elapsed: Duration) -> f64 {
    spec.progress_after(
        direction,
        EffectiveMotion::default(),
        MotionSource::Pointer,
        elapsed,
    )
}

/// How long one of the tab bar's motions runs.
fn tab_motion_time(spec: MotionSpec, direction: Direction) -> Duration {
    spec.settle_time(direction, EffectiveMotion::default(), MotionSource::Pointer)
}

/// Pills changing places on reorder, insert and close.
fn layout_anim() -> Duration {
    tab_motion_time(ListChange::MOVE, Direction::Enter)
}

/// A new pill fading in.
fn insert_fade() -> Duration {
    tab_motion_time(ListChange::FADE, Direction::Enter)
}

/// A closed pill fading out; a closed window group's tile leaves alike.
pub(crate) fn close_fade() -> Duration {
    tab_motion_time(ListChange::FADE, Direction::Exit)
}

/// The drop line fading in at a new gap.
fn hover_drop_fade() -> Duration {
    tab_motion_time(ListChange::FADE, Direction::Enter)
}

/// A pill opening its chips.
fn reveal_anim() -> Duration {
    tab_motion_time(Disclose::SPEC, Direction::Enter)
}

/// A held tab springing back once let go.
fn resist_return() -> Duration {
    tab_motion_time(Snap::SPEC, Direction::Enter)
}
/// Start y-offset for the insert fade; positive pulls the pill up at t=0, settling
/// to target at t=1.
const INSERT_FADE_Y_OFFSET: f64 = 10.0;

/// Terminal scale of the close curve (from 1.0).
const CLOSE_SCALE_END: f64 = 0.95;
/// Terminal y-offset of the close curve; negative drifts upward.
const CLOSE_Y_OFFSET_END: f64 = -20.0;

/// Alias of [`crate::chrome::shell::SelectionTier`] so the tab strip, top bar,
/// quick settings, workspace disc and strip ribbon all express selection the same
/// way.
pub(crate) type PillAppearance = crate::chrome::shell::SelectionTier;

/// Composes two affine transforms. `Affine`'s `Mul` impl is pure `f64` matrix
/// multiply-add with no overflow trap, so it cannot panic regardless of operand
/// magnitude — centralised here rather than repeating the exemption at every
/// pill/text transform composed below.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn compose(a: Affine, b: Affine) -> Affine {
    a * b
}

/// Offsets a rect by a delta. `Rect`'s `Add<Vec2>` impl is pure `f64` component
/// addition with no overflow trap, so — like [`compose`] — it cannot panic and
/// the exemption is centralised here instead of at every call site.
/// One pill's paint state, resolved once per frame by `draw_pill_inner` and
/// shared by the helpers that paint its body and chips.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact about the pill this frame"
)]
struct PillPaint<'a> {
    i: usize,
    tab: &'a TabDescriptor,
    /// Where the pill paints: its sprung visual rect, or the drag ghost's.
    rect: Rect,
    /// `rect` less the pill's laid-out target; the cached chip rects shift by it.
    offset: vello::kurbo::Vec2,
    look: PillAppearance,
    is_pinned: bool,
    is_active: bool,
    is_hover: bool,
    /// The drag ghost paints without hover emphasis.
    suppress_hover: bool,
    /// This frame's trailing reveal width.
    zone: f64,
}

fn shift_rect(r: Rect, d: vello::kurbo::Vec2) -> Rect {
    r + d
}

/// Chip fade-in strength at reveal `zone`, ramping 0→1 as the zone clears
/// `threshold` by [`CHIP_FADE_PX`].
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to 0.0..=1.0 before the narrowing f64 -> f32 cast"
)]
fn chip_alpha_at(zone: f64, threshold: f64) -> f32 {
    ((zone - threshold) / CHIP_FADE_PX).clamp(0.0, 1.0) as f32
}

/// Vector between two points. `Point`'s `Sub` impl is pure `f64` component
/// subtraction with no overflow trap, so — like [`compose`] — it cannot panic.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Point - Point is float component subtraction and cannot panic"
)]
fn point_delta(a: Point, b: Point) -> vello::kurbo::Vec2 {
    a - b
}

/// A tab on `ground` in one state: [`crate::chrome::selector_tier`], whose
/// tier table every selector shares.
pub(crate) fn pill_appearance(
    theme: &Theme,
    ground: vello::peniko::Color,
    is_active: bool,
    is_hover: bool,
    is_pressed: bool,
) -> PillAppearance {
    crate::chrome::selector_tier(
        theme,
        crate::chrome::shell::SelectionState::new(is_active, is_hover, is_pressed),
        ground,
    )
}

/// Gated on `look.fill` being opaque so the × never paints without a body under it —
/// deriving both from the same struct keeps that true by construction. Paint-only:
/// the hit rect stays live even when this is false, like the search/maximize chips.
pub(crate) fn close_affordance_visible(look: &PillAppearance, is_pinned: bool) -> bool {
    !is_pinned && look.fill.a() > 0
}

/// One pill's trailing-reserve animation. Interpolated every `apply_layout`, so hit
/// rects and paint always agree — a chip is never clickable before it's visible.
#[derive(Clone, Copy, Debug)]
struct RevealAnim {
    from: f64,
    to: f64,
    started: Instant,
}

/// A tab fading out after close, tracked apart from the live `tabs` vec so it keeps
/// rendering at declining alpha/scale for the close fade ([`close_fade`]) while survivors reflow.
#[derive(Clone, Debug)]
struct ClosingTab {
    desc: TabDescriptor,
    index: usize,
    was_active: bool,
    /// Rect at close-start; the fade animates in place, not tracking the layout
    /// spring.
    pill_rect: Rect,
    close_rect: Rect,
    started_at: Instant,
}

/// Whoever opened a tab on a shared desktop, when it is not this desktop's
/// person and they may not rearrange it: the tab offers no close, a drag on
/// it pulls a little and springs back, and hovering it names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabHolder {
    /// Their name, as presence shows it.
    pub name: String,
    /// Their presence colour.
    pub rgb: (u8, u8, u8),
}

/// A drag on a tab someone else holds: how far it was pulled, and since
/// when it springs back.
#[derive(Clone, Copy, Debug)]
struct Resist {
    tab: u64,
    origin: Point,
    pull: vello::kurbo::Vec2,
    released_at: Option<Instant>,
}

/// How far a held tab follows a drag before it stops: the reach of the
/// rubber band ([`crate::tokens::rubber_band`]), a section gap.
const RESIST_REACH: f32 = crate::tokens::Spacing::BASE.s4;

#[derive(Clone, Debug)]
pub struct TabDescriptor {
    pub id: u64,
    pub title: String,
    pub app_kind: Option<AppKind>,
    /// The application's own artwork, painted in place of the `app_kind`
    /// glyph — how an app Tilo did not package is recognisable in its tab.
    pub icon: Option<icons::AppImage>,
}

impl TabDescriptor {
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            app_kind: None,
            icon: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupFocus {
    Focused,
    Unfocused,
}

/// What a tab's content is doing, marked in the tab's leading slot in place
/// of its icon: loading, or making sound. A sound mark is a control: a
/// click on it is reported by [`TabBar::take_mark_click`] (the host mutes
/// or unmutes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabMark {
    /// The content is loading.
    Busy,
    /// The content is playing sound.
    Playing,
    /// The content's sound is muted.
    Muted,
}

impl TabMark {
    /// What the mark says to someone who cannot see it.
    fn description(self) -> &'static str {
        match self {
            Self::Busy => "Loading",
            Self::Playing => "Playing audio",
            Self::Muted => "Muted",
        }
    }

    /// Whether a click on the mark does something.
    fn is_control(self) -> bool {
        matches!(self, Self::Playing | Self::Muted)
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent interaction and animation flags are not one state machine"
)]
pub struct TabBar {
    group_id_tag: String,
    tabs: Vec<TabDescriptor>,
    active_id: u64,
    /// Whether this bar belongs to the shell's keyboard-focused window group.
    /// An active tab exists in every group; only the focused group's active tab
    /// receives the accent keyline.
    group_focus: GroupFocus,
    /// The tab list is one keyboard stop. Arrow/Home/End then select a tab
    /// inside the list, matching the native tab-list convention without
    /// flooding ordinary Tab traversal with every individual pill.
    keyboard_focus: bool,
    /// The bar stands on a docked strip of its own (the window group's,
    /// [`crate::chrome::SelectorRole::Strip`]) rather than on the window
    /// ground, so its tabs lift off the strip's ground.
    on_strip: bool,
    rect: Rect,
    tab_rects: Vec<Rect>,
    /// The 18px visual close chips. Kept separate from the larger pointer
    /// targets so the row stays quiet while closing remains easy to hit.
    close_rects: Vec<Rect>,
    /// At least a 32px square whenever a close chip is visible. The target is
    /// deliberately inside the pill, never spilling into a neighbouring tab.
    close_hit_rects: Vec<Rect>,
    /// Hover-only maximize-icon hit regions, aligned 1:1 with `tabs`.
    max_rects: Vec<Rect>,
    /// Hover-only "search in app" hit regions, aligned 1:1 with `tabs`.
    search_rects: Vec<Rect>,
    /// Hover-only History chips, aligned 1:1 with `tabs`; `Rect::ZERO` for a
    /// tab whose window has no History.
    history_rects: Vec<Rect>,
    /// The History control each tab's window offers, by tab id: the window's
    /// History door, shown as one more chip in the pill's cluster.
    history: HashMap<u64, HistoryControl>,
    new_tab_rect: Rect,
    /// Keyed by tab id, alongside the title/size it was measured at, so a rename or
    /// theme change invalidates rather than reusing a stale advance. Set from `draw`
    /// (the only pass with a [`TextRenderer`]) and read back by `apply_layout`.
    title_advance: HashMap<u64, (String, TextStyle, f64)>,
    /// `draw` measured an advance that differs from the one the last layout
    /// used for some pill, so the next layout must run to settle the row.
    advance_changed: bool,
    /// Aligned 1:1 with `tabs`; `None` for a pinned, icon-only pill.
    visible_titles: Vec<Option<String>>,
    /// Peer faces each pill shows at its trailing edge, aligned 1:1 with
    /// `tabs`. Set from `draw` (the only pass with presence) before titles
    /// are elided, so the title gives way to the faces instead of hiding
    /// under them.
    pill_faces: Vec<PillFaces>,
    face_metrics: FaceMetrics,
    /// Width each unpinned tab's full set of faces wants, keyed by tab id.
    /// Recorded in `draw` (presence is only known there) and added to the
    /// pill's natural width so a short title still has room for its faces.
    face_demand: HashMap<u64, f64>,
    /// `face_demand` changed since the last layout: the pills must resize.
    face_demand_stale: bool,
    pending_context_menu: Option<(u64, Point)>,
    hover_idx: Option<usize>,
    hover_close_idx: Option<usize>,
    hover_max_idx: Option<usize>,
    hover_search_idx: Option<usize>,
    hover_history_idx: Option<usize>,
    /// Which pill the pointer has opened. Resting on or heading for a
    /// control never opens or collapses anything that would move it; resting
    /// on a pill's body opens it after a short dwell; menu aim keeps it open
    /// while the pointer travels toward its chips. See `hover_intent`.
    hover_intent: HoverIntent,
    hover_new_tab: bool,
    press_origin: Option<(usize, Point)>,
    /// Offset from the pressed pill's top-left to the cursor at press time; the
    /// ghost paints at `last_cursor - drag_offset` since `DragPayload::Tab` doesn't
    /// carry this.
    drag_offset: Option<vello::kurbo::Vec2>,
    /// When the press crosses the drag threshold. The parent window uses the
    /// same lift progress for the handle and its attached content preview.
    drag_lifted_at: Option<Instant>,
    last_cursor: Point,
    pub pending_drag: Option<DragPayload>,
    pub pending_reorder: Option<(usize, usize)>,
    pub pending_close: Option<u64>,
    pub pending_max: Option<u64>,
    pub pending_search: Option<u64>,
    /// A press on a pill's History chip: the tab, and the control it showed.
    pending_history: Option<(u64, HistoryControl)>,
    pub pending_new_tab: Option<()>,
    /// One-shot selection edge produced by direct user interaction.  The
    /// retained shell must not infer a click merely because model rebuilding
    /// changed `active_id` between frames.
    pending_activation: Option<u64>,
    /// `(tab_id, release_point)` when a drag is released outside this bar's rect,
    /// for a parent (e.g. `GroupRow`) to resolve the drop target.
    pub pending_detach: Option<(u64, Point)>,
    drop_zone_target: Option<usize>,
    /// Gap index where the drop-target line paints. Written from own-bar drag
    /// events and, for cross-group drags, [`Self::set_external_drag_hover`] called
    /// by `GroupRow` on every sibling bar.
    hover_drop_idx: Option<usize>,
    hover_drop_animated_at: Option<Instant>,
    /// `tab_rects[i]` keyed by id, snapshotted just before a reorder/insert/remove;
    /// `draw` springs each pill from here to its new rect on the list change's move.
    prev_tab_rects_by_id: HashMap<u64, Rect>,
    layout_anim_started: Option<Instant>,
    inserted_at: HashMap<u64, Instant>,
    closing_tabs: Vec<ClosingTab>,
    /// Sidecar rather than a `TabDescriptor` field so pin state never ripples into
    /// every tab constructor.
    pub(crate) pinned_ids: HashSet<u64>,
    /// Per-tab leading-marker tint, keyed by tab id. Same sidecar reasoning as
    /// `pinned_ids`. A host that classifies its tabs by something the shell can't
    /// know — a buffer's language, a chat's account — hands the colour in; the
    /// widget never picks one itself, so the mark re-tints with the theme like
    /// everything else in the pill.
    pub(crate) accent_ids: HashMap<u64, vello::peniko::Color>,
    /// Tab ids whose content has unsaved changes. A modified tab holds its close
    /// slot open at rest (see `resting_reveal`) and paints a dot there in place
    /// of the ×; hovering swaps the × back, so the tab never stops being
    /// closable. Sidecar for the same reason as the two above.
    pub(crate) modified_ids: HashSet<u64>,
    /// Tabs someone else holds ([`TabHolder`]), by id. Sidecar for the same
    /// reason as the three above.
    held_ids: HashMap<u64, TabHolder>,
    /// A drag on a held tab, pulling it or springing it back.
    resist: Option<Resist>,
    /// Whether the hover-reveal maximize chip is offered at all. A host whose
    /// tabs are windows wants it; a host whose tabs are documents inside one
    /// window has nothing for it to do, and a chip that appears under the
    /// pointer and then does nothing is worse than no chip.
    show_maximize: bool,
    /// Whether tabs close from the strip. A strip of fixed views (a
    /// settings window's sections) has no close control.
    closable: bool,
    /// Whether the strip offers a new-tab control.
    new_tab: bool,
    /// Per-tab content marks, keyed by tab id (a sidecar like the others).
    marks: HashMap<u64, TabMark>,
    /// When each busy tab became busy: its arc turns for [`BUSY_TURNING`].
    busy_since: HashMap<u64, Instant>,
    /// The busy pose last drawn ([`Self::busy_pose_at`]); `None` when no
    /// busy mark was drawn.
    busy_drawn: Option<BusyPose>,
    /// The painted mark of each tab, aligned 1:1 with `tabs`.
    mark_rects: Vec<Rect>,
    /// A click on a sound mark, drained by [`TabBar::take_mark_click`].
    pending_mark: Option<(u64, TabMark)>,
    /// Whether this strip's host shows which window group has keyboard
    /// input ([`TabBar::set_group_focused`]). Only then does the active tab
    /// carry the focus keyline; in a strip of documents the active tab is
    /// the lift alone, like every other selection.
    shows_group_focus: bool,
    reveal_anim: HashMap<u64, RevealAnim>,
    /// Aligned 1:1 with `tabs`; read by the title zone, chip rects and body wash so
    /// width, hit region and paint can never disagree.
    reveal_px: Vec<f64>,
    /// Cached because `apply_layout`, where the reveal is interpolated, has no
    /// `Theme` of its own.
    reduce_motion: bool,
    /// The host's reduced-motion setting ([`Widget::set_motion_reduced`]),
    /// which holds whatever the theme says.
    host_reduced_motion: bool,
    /// A shortcut or action is being laid out: chips and pills take their
    /// new places at once.
    motion_source: crate::tokens::MotionSource,
    /// `tokens.metrics.min_pointer_target`, cached from `build_layout` for
    /// the same reason: the pointer target of a maximize/search chip is
    /// derived from its glyph rect at hit-test time (see
    /// [`Self::action_hit_rect`]).
    min_pointer_target: f64,
    /// Last theme fast-duration, for animation scheduling outside `draw`.
    drag_lift_duration: Duration,
    /// Window groups paint the lifted handle together with the attached app
    /// preview in the floating pass, above strip clips. Standalone tab bars
    /// keep the legacy inline overlay for their own tests/embedders.
    drag_overlay_owned_by_parent: bool,
    /// The bar is a window's ([`TabBar::set_window_controls`]): the window's
    /// own controls stand at its trailing end, and a window showing one tab
    /// shows it as its title rather than as a tab.
    window_controls: bool,
    /// Room after the tabs that the window keeps for its search control
    /// ([`TabBar::set_search_control`]); the tabs then carry no search chip.
    search_control: Option<f64>,
    controls: super::window_controls::WindowControls,
    hover_control: Option<super::window_controls::WindowControl>,
    pressed_control: Option<super::window_controls::WindowControl>,
    /// The window is maximized: its maximize control shows restore.
    maximized: bool,
    /// Whether the window showed several tabs at the last layout, and since
    /// when: the tabs' bodies grow in, or fade back into a plain title, over
    /// that change.
    tabbed: bool,
    tabbed_changed_at: Option<Instant>,
    /// The slots each tab's app declared for the window's bar
    /// ([`crate::window_toolbar`]), by tab id. Sidecar like the others.
    toolbars: HashMap<u64, crate::window_toolbar::WindowToolbar>,
    /// Each tab's location picture (a page's favicon), decoded from its
    /// slots: it stands where the app's icon would.
    location_images: HashMap<u64, icons::AppImage>,
    /// The active tab's slots: in the title's row while the window shows
    /// one tab, in a row of their own under the tabs while it shows several.
    toolbar_row: super::window_toolbar_row::ToolbarRow,
    /// The motion the person allows, from the last layout's theme.
    motion: EffectiveMotion,
}

/// Geometry shared by the tab handle and the window-content preview attached
/// to it while the pointer is carrying a tab. Kept independent of paint so a
/// parent can render the complete lifted object above strip clips.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TabDragVisual {
    pub tab_id: u64,
    pub lifted_handle: Rect,
    pub lift_progress: f64,
}

impl TabBar {
    /// Move the bar and every rectangle laid out in it by `dx`; see
    /// [`super::window_group::WindowGroup::offset_layout`].
    pub(crate) fn offset_layout(&mut self, dx: f64) {
        if dx == 0.0 {
            return;
        }
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.rect = shift_rect(self.rect, shift);
        for rect in &mut self.tab_rects {
            *rect = shift_rect(*rect, shift);
        }
        for rect in &mut self.close_rects {
            *rect = shift_rect(*rect, shift);
        }
        for rect in &mut self.close_hit_rects {
            *rect = shift_rect(*rect, shift);
        }
        for rect in &mut self.max_rects {
            *rect = shift_rect(*rect, shift);
        }
        for rect in &mut self.search_rects {
            *rect = shift_rect(*rect, shift);
        }
        for rect in &mut self.history_rects {
            *rect = shift_rect(*rect, shift);
        }
        self.new_tab_rect = shift_rect(self.new_tab_rect, shift);
        self.controls.offset(shift);
        let row = shift_rect(self.toolbar_row.row(), shift);
        self.toolbar_row.lay_out(row);
    }

    /// Source pill's opacity while the drag-ghost is visible; low enough to read as
    /// inactive but the slot still holds its place (no neighbour reflow).
    pub const SOURCE_PILL_DIM_ALPHA: f32 = 0.35;
    /// The handle is visibly lifted but remains translucent enough for a drop
    /// target to stay legible beneath it.
    pub const GHOST_PILL_ALPHA: f32 = 0.5;

    pub fn new(group_id_tag: &str, tabs: Vec<TabDescriptor>, active_id: u64) -> Self {
        let tabbed = tabs.len() > 1;
        Self {
            group_id_tag: group_id_tag.into(),
            tabs,
            active_id,
            group_focus: GroupFocus::Focused,
            keyboard_focus: false,
            on_strip: false,
            rect: Rect::ZERO,
            tab_rects: Vec::new(),
            close_rects: Vec::new(),
            close_hit_rects: Vec::new(),
            max_rects: Vec::new(),
            search_rects: Vec::new(),
            history_rects: Vec::new(),
            history: HashMap::new(),
            new_tab_rect: Rect::ZERO,
            title_advance: HashMap::new(),
            advance_changed: false,
            visible_titles: Vec::new(),
            pill_faces: Vec::new(),
            face_metrics: FaceMetrics::default(),
            face_demand: HashMap::new(),
            face_demand_stale: false,
            pending_context_menu: None,
            hover_idx: None,
            hover_close_idx: None,
            hover_max_idx: None,
            hover_search_idx: None,
            hover_history_idx: None,
            hover_intent: HoverIntent::default(),
            hover_new_tab: false,
            pinned_ids: HashSet::new(),
            accent_ids: HashMap::new(),
            modified_ids: HashSet::new(),
            held_ids: HashMap::new(),
            resist: None,
            show_maximize: true,
            closable: true,
            new_tab: true,
            marks: HashMap::new(),
            busy_since: HashMap::new(),
            busy_drawn: None,
            mark_rects: Vec::new(),
            pending_mark: None,
            shows_group_focus: false,
            press_origin: None,
            drag_offset: None,
            drag_lifted_at: None,
            last_cursor: Point::ZERO,
            pending_drag: None,
            pending_reorder: None,
            pending_close: None,
            pending_max: None,
            pending_search: None,
            pending_history: None,
            pending_new_tab: None,
            pending_activation: None,
            pending_detach: None,
            drop_zone_target: None,
            hover_drop_idx: None,
            hover_drop_animated_at: None,
            prev_tab_rects_by_id: HashMap::new(),
            layout_anim_started: None,
            inserted_at: HashMap::new(),
            closing_tabs: Vec::new(),
            reveal_anim: HashMap::new(),
            reveal_px: Vec::new(),
            reduce_motion: false,
            host_reduced_motion: false,
            motion_source: crate::tokens::MotionSource::default(),
            drag_lift_duration: crate::tokens::motion::Durations::default().fast(),
            drag_overlay_owned_by_parent: false,
            min_pointer_target: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            window_controls: false,
            search_control: None,
            controls: super::window_controls::WindowControls::default(),
            hover_control: None,
            pressed_control: None,
            maximized: false,
            tabbed,
            tabbed_changed_at: None,
            toolbars: HashMap::new(),
            location_images: HashMap::new(),
            toolbar_row: super::window_toolbar_row::ToolbarRow::new(),
            motion: EffectiveMotion::default(),
        }
    }

    /// Make this a window's bar: the window's controls (History,
    /// maximize/restore, close) stand at its trailing end and act on the
    /// active tab, and a window of one tab shows that tab as its title. The
    /// tabs themselves then carry no maximize or History chips.
    pub fn set_window_controls(&mut self, on: bool) {
        self.window_controls = on;
        if on {
            self.show_maximize = false;
        }
    }

    /// The window carries its own search control after the tabs, `width`
    /// wide with its gap: the tabs leave it that room and carry no search
    /// chip of their own. `None` gives the room back.
    pub fn set_search_control(&mut self, width: Option<f64>) {
        self.search_control = width;
    }

    /// Where the window's search control stands when the active tab put
    /// its slots in the bar: the row the slots stand in, which ends before
    /// the control's room, and how far left the open search may reach (the
    /// location's leading edge, so back and forward stay). `None` without
    /// slots: the control then follows the tabs ([`Self::tabs_end`]).
    #[must_use]
    pub fn toolbar_search_slot(&self) -> Option<(Rect, f64)> {
        self.active_toolbar()?;
        let row = self.toolbar_row_rect();
        let location = self.toolbar_row.slots().location;
        let reach = if location.area() > 0.0 {
            location.x0
        } else {
            row.x0
        };
        Some((row, reach))
    }

    /// The colour the bar stands on, for what covers part of it.
    #[must_use]
    pub fn ground_color(&self, theme: &Theme) -> vello::peniko::Color {
        self.ground(theme)
    }

    /// Where the tabs end: past the last pill, or past the "+" after them;
    /// the window's search control stands here.
    #[must_use]
    pub fn tabs_end(&self) -> f64 {
        let pills = self
            .tab_rects
            .iter()
            .filter(|r| r.area() > 0.0)
            .map(|r| r.x1)
            .fold(None, |end: Option<f64>, x| {
                Some(end.map_or(x, |e| e.max(x)))
            });
        let plus = (self.new_tab_rect.area() > 0.0).then_some(self.new_tab_rect.x1);
        match (pills, plus) {
            (Some(p), Some(n)) => p.max(n),
            (Some(p), None) => p,
            (None, Some(n)) => n,
            (None, None) => self.pill_row().x0,
        }
    }

    /// The window is maximized, so its maximize control offers restore.
    pub fn set_maximized(&mut self, maximized: bool) {
        self.maximized = maximized;
    }

    /// A window bar showing a single tab: the tab is the window's title, with
    /// no tab look.
    fn solo(&self) -> bool {
        self.window_controls && self.tabs.len() <= 1
    }

    /// Show the slots `id`'s app declared for the window's bar, or none.
    /// Returns whether that changed what the bar shows.
    pub fn set_toolbar(
        &mut self,
        id: u64,
        toolbar: Option<crate::window_toolbar::WindowToolbar>,
    ) -> bool {
        let toolbar = toolbar
            .map(crate::window_toolbar::WindowToolbar::bounded)
            .filter(|toolbar| !toolbar.is_empty());
        if self.toolbars.get(&id) == toolbar.as_ref() {
            return false;
        }
        let image = toolbar
            .as_ref()
            .and_then(|toolbar| toolbar.location.as_ref())
            .and_then(|location| location.image.as_ref());
        let decoded_same = self
            .toolbars
            .get(&id)
            .and_then(|toolbar| toolbar.location.as_ref())
            .and_then(|location| location.image.as_ref())
            == image;
        if !decoded_same {
            match image.and_then(crate::window_toolbar::ToolbarImage::app_image) {
                Some(image) => self.location_images.insert(id, image),
                None => self.location_images.remove(&id),
            };
        }
        match toolbar {
            Some(toolbar) => self.toolbars.insert(id, toolbar),
            None => self.toolbars.remove(&id),
        };
        true
    }

    /// The slots `id`'s app declared.
    #[must_use]
    pub fn toolbar(&self, id: u64) -> Option<&crate::window_toolbar::WindowToolbar> {
        self.toolbars.get(&id)
    }

    /// The picture tab `id` shows at its leading edge: its location's own
    /// (a page's favicon), else its app's artwork.
    fn tab_image<'a>(&'a self, t: &'a TabDescriptor) -> Option<&'a icons::AppImage> {
        self.location_images.get(&t.id).or(t.icon.as_ref())
    }

    /// What the menus of the active tab's slots keep inside: the window.
    pub fn set_popup_area(&mut self, area: Rect) {
        self.toolbar_row.set_popup_area(area);
    }

    /// Whether `p` is on the edited location or the suggestions under it.
    #[must_use]
    pub fn location_hit(&self, p: Point) -> bool {
        self.active_toolbar().is_some() && self.toolbar_row.location_hit(p)
    }

    /// What the person did with the apps' slots since the last call, by tab.
    pub fn take_toolbar_events(&mut self) -> Vec<(u64, crate::window_toolbar::ToolbarEvent)> {
        self.toolbar_row.take_events()
    }

    /// What the edited location held since the last call, by tab, for the
    /// app's suggestions ([`crate::window_toolbar::ToolbarEvent::Edit`]).
    pub fn take_location_drafts(&mut self) -> Vec<(u64, crate::window_toolbar::ToolbarEvent)> {
        self.toolbar_row.take_drafts()
    }

    /// Whether the active tab's location is being edited in the bar: the
    /// bar then takes the keyboard.
    #[must_use]
    pub fn editing_location(&self) -> bool {
        self.toolbar_row.is_editing()
    }

    /// Turn the active tab's editable location into a field (Ctrl+L).
    /// Returns whether it did.
    pub fn edit_location(&mut self) -> bool {
        self.toolbar_row.begin_edit()
    }

    /// Hand a keyboard or text event to the location field while it is
    /// edited. Returns whether the field took it.
    pub fn location_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        self.toolbar_row.is_editing() && self.toolbar_row.field_event(event, ctx)
    }

    /// Put the location back, ending its edit. Returns whether it was
    /// being edited.
    pub fn cancel_location_edit(&mut self) -> bool {
        self.toolbar_row.cancel_edit()
    }

    /// Where the active tab's slots stand, for the host's hit tests.
    #[must_use]
    pub fn toolbar_hit(&self, p: Point) -> bool {
        self.active_toolbar().is_some() && self.toolbar_row.hit(p)
    }

    /// Where the active tab's location stands (`Rect::ZERO` for none).
    #[must_use]
    pub fn location_rect(&self) -> Rect {
        self.toolbar_row.slots().location
    }

    /// Where each of the active tab's slots stands, for tests and the
    /// host: back, forward, location, then the actions.
    #[must_use]
    pub fn toolbar_rects(&self) -> (Rect, Rect, Rect, Vec<Rect>) {
        let slots = self.toolbar_row.slots();
        (
            slots.back,
            slots.forward,
            slots.location,
            slots.actions.clone(),
        )
    }

    /// The active tab's slots, on a window's bar.
    fn active_toolbar(&self) -> Option<&crate::window_toolbar::WindowToolbar> {
        self.window_controls
            .then(|| self.toolbars.get(&self.active_id))
            .flatten()
    }

    /// A pill shown as its icon alone: a pinned tab, or the one tab of a
    /// window whose app put its slots in the title's row.
    fn icon_only(&self, id: u64) -> bool {
        self.pinned_ids.contains(&id)
            || (self.solo() && id == self.active_id && self.active_toolbar().is_some())
    }

    /// Whether the bar stands two rows tall: the tabs, then the active
    /// tab's slots. While the window goes back to one tab the second row
    /// stays until the slots have moved up into the title's row, so the
    /// window's content is resized once, after the motion.
    fn two_rows(&self) -> bool {
        self.active_toolbar().is_some()
            && (self.tabs.len() > 1 || (!self.tabbed && self.tabbed_in_flight()))
    }

    /// The bar's height: one row, or two with the active tab's slots under
    /// the tabs.
    #[must_use]
    pub fn bar_height(&self) -> f64 {
        if self.two_rows() {
            TAB_BAR_HEIGHT + TOOLBAR_ROW_HEIGHT
        } else {
            TAB_BAR_HEIGHT
        }
    }

    /// The row the tabs (or the title) and the window's controls stand in.
    fn tab_row(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + TAB_BAR_HEIGHT.min(self.rect.height()),
        )
    }

    /// Where the active tab's slots stand: after the icon in the title's
    /// row while the window shows one tab, across a row of their own under
    /// the tabs while it shows several, and between the two as that
    /// changes, on the same motion as the tabs' bodies.
    fn toolbar_row_rect(&self) -> Rect {
        let tab_row = self.tab_row();
        let solo = {
            let icon = self.tab_rects.first().copied().unwrap_or(Rect::ZERO);
            let x0 = if icon.area() > 0.0 {
                icon.x1 + TOOLBAR_LEAD_GAP
            } else {
                self.pill_row().x0
            };
            let x1 =
                (self.controls.content_limit(tab_row) - self.search_control.unwrap_or(0.0)).max(x0);
            Rect::new(x0, tab_row.y0, x1, tab_row.y1)
        };
        // The back button's glyph sits under the tabs' leading glyph.
        let lead = self.pill_row().x0 + PILL_PAD_X + ICON_W / 2.0
            - super::window_controls::CONTROL_PX / 2.0;
        let below = Rect::new(
            lead,
            tab_row.y1,
            (self.rect.x1
                - super::window_controls::EDGE_INSET
                - self.search_control.unwrap_or(0.0))
            .max(lead),
            tab_row.y1 + TOOLBAR_ROW_HEIGHT,
        );
        let p = self.tabbed_progress();
        if p <= 0.0 {
            solo
        } else if p >= 1.0 {
            below
        } else {
            Rect::new(
                solo.x0 + (below.x0 - solo.x0) * p,
                solo.y0 + (below.y0 - solo.y0) * p,
                solo.x1 + (below.x1 - solo.x1) * p,
                solo.y1 + (below.y1 - solo.y1) * p,
            )
        }
    }

    /// Show the active tab's slots and lay them out where they stand now.
    fn lay_out_toolbar_row(&mut self) {
        let toolbar = self.active_toolbar().cloned();
        let active = self.active_id;
        self.toolbar_row
            .show(active, toolbar.as_ref(), self.motion, crate::time::now());
        let rect = self.toolbar_row_rect();
        self.toolbar_row.lay_out(rect);
    }

    /// How far the tabs' bodies have grown in: `0` while the window shows
    /// one tab as its title, `1` once it shows several as tabs, and between
    /// the two on the disclosure motion as it changes.
    fn tabbed_progress(&self) -> f64 {
        if !self.window_controls {
            return 1.0;
        }
        let target = if self.tabbed { 1.0 } else { 0.0 };
        let Some(at) = self.tabbed_changed_at else {
            return target;
        };
        if self.reduce_motion {
            return target;
        }
        let direction = if self.tabbed {
            Direction::Enter
        } else {
            Direction::Exit
        };
        let elapsed = crate::time::now().saturating_duration_since(at);
        if elapsed >= tab_motion_time(Disclose::SPEC, direction) {
            return target;
        }
        let p = tab_progress(Disclose::SPEC, direction, elapsed).clamp(0.0, 1.0);
        if self.tabbed { p } else { 1.0 - p }
    }

    fn tabbed_in_flight(&self) -> bool {
        self.window_controls
            && !self.reduce_motion
            && self.tabbed_changed_at.is_some_and(|at| {
                let direction = if self.tabbed {
                    Direction::Enter
                } else {
                    Direction::Exit
                };
                crate::time::now().saturating_duration_since(at)
                    < tab_motion_time(Disclose::SPEC, direction)
            })
    }

    /// The window controls' rects, for the host's hit tests and tests.
    #[must_use]
    pub fn window_controls(&self) -> &super::window_controls::WindowControls {
        &self.controls
    }

    /// Whether tabs close from the strip (default `true`). Without it no
    /// close control is drawn, reserved or announced.
    #[must_use]
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// No new-tab control at the end of the strip.
    #[must_use]
    pub fn without_new_tab(mut self) -> Self {
        self.new_tab = false;
        self
    }

    /// Mark what tab `id`'s content is doing, or clear the mark.
    pub fn set_mark(&mut self, id: u64, mark: Option<TabMark>) {
        if mark == Some(TabMark::Busy) {
            self.busy_since.entry(id).or_insert_with(crate::time::now);
        } else {
            self.busy_since.remove(&id);
        }
        match mark {
            Some(mark) => {
                self.marks.insert(id, mark);
            }
            None => {
                self.marks.remove(&id);
            }
        }
    }

    /// Whether tab `id`'s busy arc still turns at `now`: motion is on and it
    /// has been busy for less than [`BUSY_TURNING`].
    fn busy_turns(&self, id: u64, now: Instant) -> bool {
        !self.reduce_motion
            && self
                .busy_since
                .get(&id)
                .is_some_and(|since| now.saturating_duration_since(*since) < BUSY_TURNING)
    }

    /// The pose busy arcs show at `now`: the indicator step while one
    /// turns, still once all have come to rest.
    fn busy_pose_at(&self, now: Instant) -> BusyPose {
        let turning = self.busy_since.keys().any(|id| self.busy_turns(*id, now));
        if turning {
            BusyPose::Step(crate::animation::indicator_step(busy_epoch(), now))
        } else {
            BusyPose::Still
        }
    }

    /// The next instant a busy arc changes: its next step while one turns,
    /// or the moment the last turning one comes to rest.
    fn busy_deadline(&self, now: Instant) -> Option<Instant> {
        let turning = self.busy_since.keys().any(|id| self.busy_turns(*id, now));
        turning.then(|| crate::animation::next_indicator_step(busy_epoch(), now))
    }

    /// Tab `id`'s mark, if any.
    pub fn mark(&self, id: u64) -> Option<TabMark> {
        self.marks.get(&id).copied()
    }

    /// Where tab `id`'s mark is painted, for hit-testing and tests.
    pub fn mark_rect(&self, id: u64) -> Option<Rect> {
        self.marks.get(&id)?;
        let i = self.tabs.iter().position(|t| t.id == id)?;
        self.mark_rects.get(i).copied().filter(|r| r.area() > 0.0)
    }

    /// A click on a tab's sound mark, once: the host mutes or unmutes it.
    pub fn take_mark_click(&mut self) -> Option<(u64, TabMark)> {
        self.pending_mark.take()
    }

    /// The host shows which window group has keyboard input: the active tab
    /// of the focused group carries a keyline in primary ink, and the others
    /// carry the lift alone.
    pub fn set_group_focused(&mut self, focused: bool) {
        self.shows_group_focus = true;
        self.group_focus = if focused {
            GroupFocus::Focused
        } else {
            GroupFocus::Unfocused
        };
    }

    pub(crate) fn set_drag_overlay_owned_by_parent(&mut self, owned: bool) {
        self.drag_overlay_owned_by_parent = owned;
    }

    /// What starts the changes laid out next; see
    /// [`GroupRow::set_motion_source`](super::GroupRow::set_motion_source).
    pub(crate) fn set_motion_source(&mut self, source: crate::tokens::MotionSource) {
        self.motion_source = source;
        if source.lands_at_once() {
            self.layout_anim_started = None;
        }
    }

    /// No-op before the first layout pass — nothing to spring from yet.
    fn snapshot_layout_for_spring(&mut self) {
        if self.tab_rects.len() != self.tabs.len() || self.motion_source.lands_at_once() {
            return;
        }
        let mut snap: HashMap<u64, Rect> = HashMap::with_capacity(self.tabs.len());
        for (t, rect) in self.tabs.iter().zip(self.tab_rects.iter()) {
            snap.insert(t.id, *rect);
        }
        self.prev_tab_rects_by_id = snap;
        self.layout_anim_started = Some(crate::time::now());
    }

    /// Test fixture: the pointer over pill `idx`, having dwelt long enough to
    /// open it.
    #[cfg(test)]
    fn hover_open_for_tests(&mut self, idx: usize) {
        self.hover_idx = Some(idx);
        self.hover_intent.force_open(Some(idx));
    }

    /// Pin transient pill geometry at its resting state for deterministic
    /// integration screenshots. This does not alter production animation;
    /// it only removes wall-clock sampling from visual regression tests.
    #[cfg(any(test, debug_assertions))]
    pub fn force_settled_for_tests(&mut self) {
        self.hover_idx = None;
        self.hover_close_idx = None;
        self.hover_max_idx = None;
        self.hover_search_idx = None;
        self.hover_history_idx = None;
        self.hover_intent.clear();
        self.hover_new_tab = false;
        self.press_origin = None;
        self.pending_drag = None;
        self.drag_offset = None;
        self.drag_lifted_at = None;
        self.drop_zone_target = None;
        self.hover_drop_idx = None;
        self.layout_anim_started = None;
        self.prev_tab_rects_by_id.clear();
        self.inserted_at.clear();
        self.reveal_anim.clear();
        self.reveal_px.clear();
        self.hover_drop_animated_at = None;
    }

    /// Report whether a retained tab row has reached a static
    /// resting state. Visual tests use this after one layout pass instead of
    /// assuming that a fixed sleep covers every nested curve.
    #[cfg(any(test, debug_assertions))]
    pub fn is_settled_for_tests(&self) -> bool {
        !self.needs_animation_frame()
            && self.hover_idx.is_none()
            && self.press_origin.is_none()
            && self.pending_drag.is_none()
            && self.drop_zone_target.is_none()
            && self.hover_drop_idx.is_none()
    }

    /// Paints at an arbitrary rect `r` — the pill's sprung visual rect from `draw`,
    /// or the cursor-following ghost rect. `self.close_rects` etc. are cached at
    /// the pill's *target* position, so they're offset by `r - self.tab_rects[i]`
    /// before painting; hit-testing still consults the unoffset rects.
    fn draw_pill_inner(
        &self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        i: usize,
        r: Rect,
        suppress_hover: bool,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let Some(t) = self.tabs.get(i) else {
            return;
        };
        let is_pinned = self.icon_only(t.id);
        // Tabs are not pills: the control radius (design language, Shape).
        let radius = f64::from(theme.tokens.radii.control);
        let Some(&target) = self.tab_rects.get(i) else {
            return;
        };

        let is_active = t.id == self.active_id;
        let is_hover = !suppress_hover && Some(i) == self.hover_idx;
        let look = self.pill_look(i, theme, suppress_hover);
        // Press scale-down around the pill centre; `1.0` short-circuits to identity.
        let pill_transform = if (look.scale - 1.0).abs() > f64::EPSILON {
            let centre = r.center();
            compose(
                compose(
                    compose(transform, Affine::translate((centre.x, centre.y))),
                    Affine::scale(look.scale),
                ),
                Affine::translate((-centre.x, -centre.y)),
            )
        } else {
            transform
        };
        // The one selector painter, so a tab can't drift from the rest of
        // the chrome.
        let shape = RoundedRect::from_rect(r, radius);
        // A window of one tab shows it as a plain title; the tab's body grows
        // in as a second tab arrives and fades back as the last one leaves.
        let body = self.tabbed_progress();
        if body > 0.0 {
            let faded = body < 1.0;
            if faded {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a progress in 0..=1 is exact enough as f32 alpha"
                )]
                let alpha = body as f32;
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    pill_transform,
                    &r.inflate(2.0, 2.0),
                );
            }
            crate::chrome::paint_selector(
                scene,
                pill_transform,
                &shape,
                crate::chrome::SelectorRole::Item(&look),
                theme,
            );
            if faded {
                scene.pop_layer();
            }
        }
        if body >= 1.0
            && is_active
            && self.shows_group_focus
            && self.group_focus == GroupFocus::Focused
        {
            // A restrained focus keyline separates two different facts that
            // used to share one treatment: "active tab in this group" and
            // "group receiving keyboard input".  It sits inside the pill so
            // it cannot collide with neighbouring groups or drag targets.
            let inset = f64::from(theme.tokens.spacing.s2);
            let keyline = RoundedRect::from_rect(
                Rect::new(
                    r.x0 + inset,
                    r.y1 - FOCUS_KEYLINE_LIFT - FOCUS_KEYLINE_PX,
                    r.x1 - inset,
                    r.y1 - FOCUS_KEYLINE_LIFT,
                ),
                FOCUS_KEYLINE_PX / 2.0,
            );
            // Primary ink, not your colour: which group has input is state,
            // and a coloured underline on every focused window made the
            // accent the loudest thing on the desktop (HIG 9.4).
            scene.fill(
                vello::peniko::Fill::NonZero,
                pill_transform,
                theme.tokens.colors.text_primary,
                None,
                &keyline,
            );
        }
        // Rebind: the rest of the pill chrome paints under `pill_transform` so it
        // compresses with the body when pressed.
        let transform = pill_transform;

        // Faces name who is here; the mist only stands in when no face fits,
        // so a pill never carries both and is never washed in their colours.
        if self
            .pill_faces
            .get(i)
            .is_none_or(|faces| faces.slots() == 0)
        {
            Self::draw_pill_presence(scene, transform, &shape, r, t, theme, presence);
        }

        // Chip strength ramps 0→1 as the trailing zone opens, reaching full once the
        // zone clears the chip by CHIP_FADE_PX. Close-hover tints red; tab-hover
        // tints a subtle white. Suppressed on pinned/idle-inactive pills — see
        // [`close_affordance_visible`].
        let zone = self
            .reveal_px
            .get(i)
            .copied()
            .unwrap_or_else(|| Reveal::None.reserve());
        let pill = PillPaint {
            i,
            tab: t,
            rect: r,
            offset: r.origin() - target.origin(),
            look,
            is_pinned,
            is_active,
            is_hover,
            suppress_hover,
            zone,
        };
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        self.draw_pill_body(&mut ctx, &pill);
        self.draw_pill_faces(&mut ctx, presence, &pill);
        self.draw_pill_close_chip(&mut ctx, &pill);
        self.draw_pill_action_chips(&mut ctx, &pill);
    }

    /// The pill's icon/dot/title content — pinned pills get an app icon or
    /// accent dot only; unpinned pills add the (elided) title text. Split out
    /// of `draw_pill_inner` to keep it inside the `too_many_lines` budget.
    fn draw_pill_body(&self, ctx: &mut PaintCtx<'_>, pill: &PillPaint<'_>) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let &PillPaint {
            i,
            tab: t,
            rect: r,
            is_pinned,
            is_active,
            is_hover,
            ..
        } = pill;
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = self.idle_ink(theme);
        // The cursor-following copy suppresses hover, but carries the primary
        // label ink so an inactive source stays readable in motion.
        let text_color = if is_active || is_hover || pill.suppress_hover {
            text_primary
        } else {
            text_dim
        };
        let text_style = tab_title_style(theme);
        let title_weight = TextWeight::Semibold;
        let (_tw, th) = tr.measure_style(
            &t.title,
            None,
            &text_style.clone().with_weight(title_weight),
        );
        let accent = self.accent_ids.get(&t.id).copied();
        let image = self.tab_image(t);
        if is_pinned {
            Self::draw_pill_pinned_marker(scene, transform, r, (t, image), accent, text_color);
            return;
        }
        let leading_shift = if let Some(mark) = self.marks.get(&t.id).copied() {
            self.draw_pill_mark(
                scene,
                transform,
                theme,
                leading_glyph_rect(r),
                (t.id, mark),
                text_color,
            );
            ICON_W + ICON_GAP
        } else {
            Self::draw_pill_leading_glyph(scene, transform, r, (t, image), accent, text_color)
        };
        let title_x = title_x0(r, leading_shift);
        let title_y = r.center().y - f64::from(th) / 2.0;
        // Clip at this frame's reveal, not the close chip, so an idle pill gets
        // its full width and a mid-open pill hands room back as it opens.
        let clip_x1 = r.x1
            - self
                .reveal_px
                .get(i)
                .copied()
                .unwrap_or_else(|| Reveal::None.reserve());
        let clip_r = Rect::new(r.x0, r.y0, clip_x1.max(r.x0), r.y1);
        // Backstop only — `visible_titles` already carries the elided form.
        let label = self
            .visible_titles
            .get(i)
            .and_then(|s| s.as_deref())
            .unwrap_or(t.title.as_str());
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip_r,
        );
        tr.draw_style(
            scene,
            label,
            text_color,
            compose(transform, Affine::translate((title_x, title_y))),
            None,
            &text_style.clone().with_weight(title_weight),
        );
        scene.pop_layer();
    }

    /// A pinned pill's marker: an app icon if it has one, else its accent dot,
    /// else nothing (an empty bubble). Split out of `draw_pill_body` to keep
    /// that method inside the `too_many_lines` budget.
    fn draw_pill_pinned_marker(
        scene: &mut Scene,
        transform: Affine,
        r: Rect,
        (t, image): (&TabDescriptor, Option<&icons::AppImage>),
        accent: Option<vello::peniko::Color>,
        text_color: vello::peniko::Color,
    ) {
        let icon_rect = square_at(r.center(), ICON_W);
        if image.is_some_and(|image| icons::draw_app_image(scene, transform, icon_rect, image)) {
            return;
        }
        if let Some(kind) = t.app_kind {
            draw_app_icon(
                scene,
                transform,
                icon_rect,
                kind,
                accent.unwrap_or(text_color),
            );
        } else if let Some(dot) = accent {
            // No app glyph to tint, so the accent is the whole marker —
            // centred like the glyph would have been.
            let c = vello::kurbo::Circle::new(r.center(), ACCENT_DOT_R);
            scene.fill(vello::peniko::Fill::NonZero, transform, dot, None, &c);
        }
    }

    /// A tab's content mark in its leading slot: a turning arc while busy
    /// (a still three-quarter ring under reduced motion), a speaker while it
    /// plays or is muted, lifted under the pointer since a click on it
    /// mutes or unmutes.
    fn draw_pill_mark(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        (id, mark): (u64, TabMark),
        ink: vello::peniko::Color,
    ) {
        match mark {
            TabMark::Busy => {
                let now = crate::time::now();
                let turn = if self.busy_turns(id, now) {
                    // In indicator steps: the arc turns 30 times a second.
                    let t = crate::animation::indicator_elapsed(busy_epoch(), now).as_secs_f64();
                    t.fract() * std::f64::consts::TAU
                } else {
                    0.0
                };
                let radius = rect.width().min(rect.height()) / 2.0 - 1.5;
                let arc = vello::kurbo::Arc::new(
                    rect.center(),
                    (radius, radius),
                    turn,
                    std::f64::consts::TAU * 0.75,
                    0.0,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.5).with_caps(vello::kurbo::Cap::Round),
                    transform,
                    ink,
                    None,
                    &arc,
                );
            }
            TabMark::Playing | TabMark::Muted => {
                if rect.inflate(2.0, 2.0).contains(self.last_cursor) {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme
                            .tokens
                            .colors
                            .lift_on(theme.tokens.colors.surfaces.content, 1.0),
                        None,
                        &RoundedRect::from_rect(
                            rect.inflate(2.0, 2.0),
                            f64::from(theme.tokens.radii.item),
                        ),
                    );
                }
                let icon = if mark == TabMark::Muted {
                    icons::Icon::VolumeX
                } else {
                    icons::Icon::Volume2
                };
                icons::draw_icon(
                    scene,
                    transform,
                    rect,
                    icon,
                    ink,
                    icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    /// An unpinned pill's leading glyph — app icon, else accent dot, else
    /// nothing — returning the horizontal shift it reserves for the title.
    /// Split out of `draw_pill_body` to keep that method inside the
    /// `too_many_lines` budget.
    fn draw_pill_leading_glyph(
        scene: &mut Scene,
        transform: Affine,
        r: Rect,
        (t, image): (&TabDescriptor, Option<&icons::AppImage>),
        accent: Option<vello::peniko::Color>,
        text_color: vello::peniko::Color,
    ) -> f64 {
        // The glyph starts on the pill's own text inset (`PILL_PAD_X`), so
        // its left edge lines up with where a glyph-less title starts, and
        // the title follows it at `ICON_GAP` — one inset and one gap for
        // every pill, instead of a glyph tucked 2 px inside the text.
        let icon_rect = leading_glyph_rect(r);
        if image.is_some_and(|image| icons::draw_app_image(scene, transform, icon_rect, image)) {
            return ICON_W + ICON_GAP;
        }
        if let Some(kind) = t.app_kind {
            draw_app_icon(
                scene,
                transform,
                icon_rect,
                kind,
                accent.unwrap_or(text_color),
            );
            ICON_W + ICON_GAP
        } else if let Some(dot) = accent {
            // One slot, two possible occupants: the app glyph, or — for a
            // host whose tabs are classified by something the shell can't
            // name — this dot. Same width as the glyph so a leading zone
            // sized by `leading_zone` always has room for whichever paints.
            let c = vello::kurbo::Circle::new(icon_rect.center(), ACCENT_DOT_R);
            scene.fill(vello::peniko::Fill::NonZero, transform, dot, None, &c);
            ICON_W + ICON_GAP
        } else {
            0.0
        }
    }

    /// Presence-in-navigation mist. A peer on another tab should read as a
    /// destination, not as an unexplained notification count.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "Affine multiplication is the geometry library's point-transform operation"
    )]
    fn draw_pill_presence(
        scene: &mut Scene,
        transform: Affine,
        shape: &RoundedRect,
        r: Rect,
        t: &TabDescriptor,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let peers = presence.peers_on(&tab_presence_key(t.id));
        if peers.is_empty() {
            return;
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            shape,
        );
        let capped = peers.len().min(4);
        let count = u8::try_from(capped).unwrap_or(4);
        let segment_w = r.width() / f64::from(count);
        for (index, peer) in peers.iter().take(capped).enumerate() {
            let index = u8::try_from(index).unwrap_or(3);
            let x0 = r.x0 + f64::from(index) * segment_w;
            let x1 = if index.saturating_add(1) == count {
                r.x1
            } else {
                x0 + segment_w
            };
            let p0 = transform * Point::new(x0, r.y0);
            let p1 = transform * Point::new(x1, r.y1);
            let mist_rect = Rect::new(p0.x, p0.y, p1.x, p1.y);
            presence.draw_peer_mist(scene, theme, peer, mist_rect);
        }
        scene.pop_layer();
    }

    /// How many peer faces pill `i` has room for: up to [`PILL_MAX_FACES`]
    /// plus a "+N" disc, fewer when the title would drop under
    /// its full width or [`MIN_TITLE_PX`], none on a pinned pill or one too narrow for even a
    /// single face. The mist then says someone is there.
    fn fit_pill_faces(&self, i: usize, presence: &PresenceCtx) -> PillFaces {
        let (Some(tab), Some(&r)) = (self.tabs.get(i), self.tab_rects.get(i)) else {
            return PillFaces::default();
        };
        if self.icon_only(tab.id) {
            return PillFaces::default();
        }
        let peers = presence.peers_on(&tab_presence_key(tab.id)).len();
        let (x0, x1) = self.title_zone_without_faces(i, r);
        let keep = self.shaped_title_width(i).min(MIN_TITLE_PX);
        PillFaces::fit(peers, x1 - x0, keep, self.face_metrics)
    }

    /// Records how wide each unpinned tab's faces want to be, and marks the
    /// layout stale when that changed so the pills grow or shrink for them.
    fn record_face_demand(&mut self, presence: &PresenceCtx) {
        let metrics = self.face_metrics;
        let demand: HashMap<u64, f64> = self
            .tabs
            .iter()
            .filter(|tab| !self.icon_only(tab.id))
            .filter_map(|tab| {
                let peers = presence.peers_on(&tab_presence_key(tab.id)).len();
                let faces = peers.min(PILL_MAX_FACES);
                let wanted = PillFaces {
                    faces,
                    overflow: peers - faces,
                }
                .width(metrics);
                (wanted > 0.0).then_some((tab.id, wanted))
            })
            .collect();
        if demand != self.face_demand {
            self.face_demand = demand;
            self.face_demand_stale = true;
        }
    }

    /// Small faces of the peers on this tab, at the trailing end of the
    /// title zone (never under the close chip): initials in each person's
    /// presence colour, agents in the agent identity mark so software never
    /// reads as a person, ringed so overlapping faces stay separate.
    fn draw_pill_faces(
        &self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        pill: &PillPaint<'_>,
    ) {
        let faces = self.pill_faces.get(pill.i).copied().unwrap_or_default();
        if faces.slots() == 0 {
            return;
        }
        let r = pill.rect;
        let reserve = self
            .reveal_px
            .get(pill.i)
            .copied()
            .unwrap_or_else(|| Reveal::None.reserve());
        let metrics = self.face_metrics;
        let x0 = r.x1 - reserve - faces.width(metrics) + metrics.gap;
        let cy = r.center().y;
        // The ring is the pill's own ground (its body over the strip), so
        // overlapping faces read as cut out of the tab, not stickered on.
        let ring = crate::tokens::colors::over(
            self.ground(ctx.theme),
            pill.look.fill.with_alpha(1.0),
            pill.look.fill.components[3],
        );
        let face_rect = |slot: usize| {
            square_at(
                Point::new(
                    x0 + metrics.face / 2.0 + crate::num::f64_of_usize(slot) * metrics.step(),
                    cy,
                ),
                metrics.face,
            )
        };
        let peers = presence.peers_on(&tab_presence_key(pill.tab.id));
        for (slot, user) in peers.iter().take(faces.faces).enumerate() {
            let rect = face_rect(slot);
            let (red, green, blue) = presence.color_for(user);
            let colour = vello::peniko::Color::from_rgb8(red, green, blue);
            if presence.is_agent(user) {
                let theme = ctx.theme;
                let ring_px = f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0;
                ctx.scene.fill(
                    vello::peniko::Fill::NonZero,
                    ctx.transform,
                    ring,
                    None,
                    &RoundedRect::from_rect(
                        rect.inflate(ring_px, ring_px),
                        f64::from(theme.tokens.radii.detail),
                    ),
                );
                crate::agent_identity::draw_agent_identity_mark(
                    ctx.scene,
                    ctx.transform,
                    rect,
                    theme.tokens.colors.presence.of_rgb((red, green, blue)),
                    theme,
                    crate::agent_identity::AgentIdentityStyle::Solid,
                );
            } else {
                let name = presence
                    .names
                    .get(user)
                    .map_or(user.0.as_str(), String::as_str);
                let initial = name
                    .chars()
                    .find(|c| c.is_alphanumeric())
                    .map(|c| c.to_uppercase().collect::<String>())
                    .unwrap_or_default();
                crate::widget::AvatarFace::new(&initial, "")
                    .presence(colour)
                    .ringed_on(ring)
                    .paint(ctx, rect);
            }
        }
        crate::widget::AvatarFace::paint_overflow(
            ctx,
            face_rect(faces.faces),
            faces.overflow,
            Some(ring),
        );
    }

    /// The close × (or unsaved-changes dot) chip. Split out of
    /// `draw_pill_inner` to keep it inside the `too_many_lines` budget.
    fn draw_pill_close_chip(&self, ctx: &mut PaintCtx<'_>, pill: &PillPaint<'_>) {
        let PaintCtx {
            scene,
            transform,
            theme,
            ..
        } = ctx.reborrow();
        let &PillPaint {
            i,
            tab: t,
            offset,
            ref look,
            is_pinned,
            is_active,
            is_hover,
            suppress_hover,
            zone,
            ..
        } = pill;
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = self.idle_ink(theme);
        let close_rect_target = self.close_rects.get(i).copied().unwrap_or(Rect::ZERO);
        let is_held = self.held_ids.contains_key(&t.id);
        if !(close_affordance_visible(look, is_pinned || is_held) && close_rect_target.area() > 0.0)
        {
            return;
        }
        let alpha = chip_alpha_at(zone, CLOSE_HIT_PX);
        let close_r = shift_rect(close_rect_target, offset);
        let close_hover =
            !suppress_hover && Some(i) == self.hover_close_idx && self.pointer_settled();
        if self.modified_ids.contains(&t.id) && !is_hover && !close_hover {
            // Unsaved changes, pointer elsewhere: the slot reads as a dot
            // rather than an ×. Hovering swaps it back (below), so the tab
            // stays closable and the two meanings never share a frame.
            let dot = vello::kurbo::Circle::new(close_r.center(), MODIFIED_DOT_R);
            let color = if is_active { text_primary } else { text_dim };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color.with_alpha_factor(alpha),
                None,
                &dot,
            );
            return;
        }
        let (close_chip_color, close_chip_visible) = if close_hover {
            (
                theme.tokens.colors.semantic.danger.with_alpha_factor(0.8),
                true,
            )
        } else if is_hover {
            (
                chip_hover_tint(self.ground(theme), CHIP_HOVER_TINT_ALPHA),
                true,
            )
        } else {
            (vello::peniko::Color::TRANSPARENT, false)
        };
        if close_chip_visible {
            let chip = RoundedRect::from_rect(close_r, close_r.height() / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                close_chip_color.with_alpha_factor(alpha),
                None,
                &chip,
            );
        }
        let glyph_color = if close_hover {
            // Full white so the × reads crisp against the red fill.
            vello::peniko::Color::from_rgba8(255, 255, 255, 255)
        } else if is_active {
            text_primary
        } else {
            text_dim
        };
        icons::draw_icon(
            scene,
            transform,
            close_glyph_box(close_r),
            icons::Icon::X,
            glyph_color.with_alpha_factor(alpha),
            1.5,
        );
    }

    /// Maximize/search chips, painted exactly when `apply_layout` found room,
    /// so they slide out of the pill's right edge as it opens. Split out of
    /// `draw_pill_inner` to keep it inside the `too_many_lines` budget.
    fn draw_pill_action_chips(&self, ctx: &mut PaintCtx<'_>, pill: &PillPaint<'_>) {
        let PaintCtx {
            scene,
            transform,
            theme,
            ..
        } = ctx.reborrow();
        let &PillPaint {
            i,
            offset,
            is_pinned,
            suppress_hover,
            zone,
            ..
        } = pill;
        if suppress_hover || is_pinned {
            return;
        }
        let chip_tint = chip_hover_tint(self.ground(theme), ACTION_HOVER_TINT_ALPHA);
        let icon_idle = self.idle_ink(theme);
        let icon_active = theme.tokens.colors.text_primary;

        let max_rect_target = self.max_rects.get(i).copied().unwrap_or(Rect::ZERO);
        if self.show_maximize && max_rect_target.area() > 0.0 {
            let alpha = chip_alpha_at(zone, MAX_CHIP_REACH);
            let max_r = shift_rect(max_rect_target, offset);
            let max_hover = Some(i) == self.hover_max_idx && self.pointer_settled();
            if max_hover {
                // The same round the close chip gets, centred on the glyph,
                // rather than a capsule the height of the hit rect: three
                // chips on one pill wear one shape.
                let ring = chip_hover_ring(max_r);
                let chip = RoundedRect::from_rect(ring, ring.height() / 2.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    chip_tint.with_alpha_factor(alpha),
                    None,
                    &chip,
                );
            }
            let outline_color = if max_hover { icon_active } else { icon_idle };
            icons::draw_icon(
                scene,
                transform,
                max_r,
                icons::Icon::Maximize2,
                outline_color.with_alpha_factor(alpha),
                1.5,
            );
        }

        let search_rect_target = self.search_rects.get(i).copied().unwrap_or(Rect::ZERO);
        if search_rect_target.area() > 0.0 {
            let alpha = chip_alpha_at(zone, CHIP_CLUSTER_W);
            let search_r = shift_rect(search_rect_target, offset);
            let search_hover = Some(i) == self.hover_search_idx && self.pointer_settled();
            if search_hover {
                let ring = chip_hover_ring(search_r);
                let chip = RoundedRect::from_rect(ring, ring.height() / 2.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    chip_tint.with_alpha_factor(alpha),
                    None,
                    &chip,
                );
            }
            let icon_color = if search_hover { icon_active } else { icon_idle };
            icons::draw_icon(
                scene,
                transform,
                search_r,
                icons::Icon::Search,
                icon_color.with_alpha_factor(alpha),
                1.5,
            );
        }

        let history_rect_target = self.history_rects.get(i).copied().unwrap_or(Rect::ZERO);
        if history_rect_target.area() > 0.0 {
            let alpha = chip_alpha_at(zone, HISTORY_CHIP_REACH);
            let history_r = shift_rect(history_rect_target, offset);
            let history_hover = Some(i) == self.hover_history_idx && self.pointer_settled();
            // Open, the chip reads as on: the same round its hover wears.
            let open = self
                .tabs
                .get(i)
                .and_then(|t| self.history.get(&t.id))
                .is_some_and(|control| control.is_open());
            if history_hover || open {
                let ring = chip_hover_ring(history_r);
                let chip = RoundedRect::from_rect(ring, ring.height() / 2.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    chip_tint.with_alpha_factor(alpha),
                    None,
                    &chip,
                );
            }
            let icon_color = if history_hover || open {
                icon_active
            } else {
                icon_idle
            };
            icons::draw_icon(
                scene,
                transform,
                history_r,
                icons::Icon::History,
                icon_color.with_alpha_factor(alpha),
                1.5,
            );
        }
    }

    /// Narrower than `draw_pill_inner`: no hover/search/maximize/presence — the
    /// pill is non-interactive during the fade.
    fn draw_closing_pill(
        scene: &mut Scene,
        transform: Affine,
        c: &ClosingTab,
        tr: &mut TextRenderer,
        (theme, ground, text_dim): (&Theme, vello::peniko::Color, vello::peniko::Color),
    ) {
        let r = c.pill_rect;
        let radius = f64::from(theme.tokens.radii.control);
        // Hovered tier, not inactive: active-id already shifted at close-start, and
        // an inactive pill is fully transparent, which would leave the title and ×
        // floating on the bar for the fade.
        crate::chrome::paint_selector(
            scene,
            transform,
            &RoundedRect::from_rect(r, radius),
            crate::chrome::SelectorRole::Item(&pill_appearance(theme, ground, false, true, false)),
            theme,
        );
        let text_style = tab_title_style(theme);
        let title_weight = TextWeight::Semibold;
        let (_tw, th) = tr.measure_style(
            &c.desc.title,
            None,
            &text_style.clone().with_weight(title_weight),
        );
        let leading_shift = if let Some(kind) = c.desc.app_kind {
            draw_app_icon(scene, transform, leading_glyph_rect(r), kind, text_dim);
            ICON_W + ICON_GAP
        } else {
            0.0
        };
        let title_x = title_x0(r, leading_shift);
        let title_y = r.center().y - f64::from(th) / 2.0;
        // Clip just before the close glyph, same as `draw_pill_inner`, so the title
        // can't slide past the × while the pill shrinks.
        let clip_r = Rect::new(r.x0, r.y0, c.close_rect.x0 - 2.0, r.y1);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip_r,
        );
        tr.draw_style(
            scene,
            &c.desc.title,
            text_dim,
            compose(transform, Affine::translate((title_x, title_y))),
            None,
            &text_style.clone().with_weight(title_weight),
        );
        scene.pop_layer();
        icons::draw_icon(
            scene,
            transform,
            close_glyph_box(c.close_rect),
            icons::Icon::X,
            text_dim,
            1.5,
        );
    }

    fn compute_ghost_rect(&self) -> Option<Rect> {
        let source_idx = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => self.tabs.iter().position(|t| t.id == *tab)?,
            _ => return None,
        };
        let offset = self.drag_offset.unwrap_or(vello::kurbo::Vec2::ZERO);
        let pill = *self.tab_rects.get(source_idx)?;
        let w = pill.width();
        let h = pill.height();
        let x0 = self.last_cursor.x - offset.x;
        let y0 = self.last_cursor.y - offset.y;
        Some(Rect::new(x0, y0, x0 + w, y0 + h))
    }

    /// The complete visual description of the tab currently being carried.
    /// The lift uses the theme's fast motion token; reduced motion snaps to the
    /// tucked end state while preserving the spatial cue.
    pub(crate) fn drag_visual(&self, theme: &Theme) -> Option<TabDragVisual> {
        let tab_id = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => *tab,
            _ => return None,
        };
        let source_idx = self.tabs.iter().position(|tab| tab.id == tab_id)?;
        self.tab_rects.get(source_idx)?;
        let lifted_handle = self.compute_ghost_rect()?;
        let lift_progress = if theme.reduce_motion {
            1.0
        } else {
            self.drag_lifted_at.map_or(1.0, |started| {
                tab_progress(
                    ListChange::FADE,
                    Direction::Enter,
                    crate::time::now().saturating_duration_since(started),
                )
            })
        };
        Some(TabDragVisual {
            tab_id,
            lifted_handle,
            lift_progress,
        })
    }

    #[cfg(test)]
    pub fn ghost_rect(&self) -> Option<Rect> {
        self.compute_ghost_rect()
    }

    /// Test-only: lets ghost tests assert the dim without peeking into the vello
    /// scene.
    #[cfg(test)]
    pub fn pill_draw_alpha(&self, idx: usize) -> f32 {
        let source_idx = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => self.tabs.iter().position(|t| t.id == *tab),
            _ => None,
        };
        if source_idx == Some(idx) {
            Self::SOURCE_PILL_DIM_ALPHA
        } else {
            1.0
        }
    }
    /// The reveal zone `reveal` opens pill `i` to: fully open, a pill whose
    /// window has History holds one chip more.
    fn reserve_of(&self, i: usize, reveal: Reveal) -> f64 {
        let history = matches!(reveal, Reveal::Full)
            && self
                .tabs
                .get(i)
                .is_some_and(|t| self.history.contains_key(&t.id));
        if history {
            reveal.reserve() + CHIP_PITCH
        } else {
            reveal.reserve()
        }
    }

    /// Right edge to the leftmost chip of pill `i`'s open cluster.
    fn cluster_width(&self, i: usize) -> f64 {
        self.reserve_of(i, Reveal::Full) - TITLE_CHIP_GAP
    }

    /// Where pill `i`'s History chip stands once the reveal zone reaches it:
    /// one pitch left of the search chip.
    fn history_chip_rect(&self, i: usize, search: Rect, effective: f64) -> Rect {
        let has_history = self
            .tabs
            .get(i)
            .is_some_and(|t| self.history.contains_key(&t.id));
        if has_history && search.area() > 0.0 && effective >= HISTORY_CHIP_REACH {
            shift_rect(search, vello::kurbo::Vec2::new(-CHIP_PITCH, 0.0))
        } else {
            Rect::ZERO
        }
    }

    /// Pointer-independent by construction — lets `apply_layout` fit the whole row
    /// from resting widths and leave the fit alone as the pointer moves across it.
    fn resting_reveal(&self, i: usize) -> Reveal {
        let Some(t) = self.tabs.get(i) else {
            return Reveal::None;
        };
        if self.pinned_ids.contains(&t.id) || !self.closable {
            return Reveal::None;
        }
        if t.id == self.active_id || self.modified_ids.contains(&t.id) {
            // A modified tab has something to say at rest — the unsaved-changes
            // dot — and it lives in the close slot, so it holds that slot open
            // even when the tab is neither active nor hovered.
            Reveal::Close
        } else {
            Reveal::None
        }
    }

    /// Hover (or the press that follows it) is the only thing that lifts a pill
    /// above its resting tier.
    fn target_reveal(&self, i: usize) -> Reveal {
        let Some(t) = self.tabs.get(i) else {
            return Reveal::None;
        };
        if self.pinned_ids.contains(&t.id) || !self.closable {
            return Reveal::None;
        }
        let pressed = self.press_origin.is_some_and(|(j, _)| j == i);
        // The pointer opens a pill by resting on its body (never its ×, never
        // on its way to a control) for a short dwell from when it entered, so
        // sweeping across the row moves nothing; an open pill stays open while
        // the pointer heads for its chips, even across a neighbour or the gap.
        // A press and keyboard focus are explicit and open at once.
        if Some(i) == self.hover_intent.open()
            || pressed
            || self.keyboard_focus && t.id == self.active_id
        {
            // With the maximize chip withheld there is nothing above `Close` to
            // open into, and reserving `Full`'s width for a chip that will
            // never be placed would just push the title around for nothing.
            if self.show_maximize {
                Reveal::Full
            } else {
                Reveal::Close
            }
        } else {
            self.resting_reveal(i)
        }
    }

    /// Where tab `id`'s reveal curve has got to, without advancing it.
    /// Safe to call from `&self` passes (`draw`, `title_zone`).
    fn reveal_reserve(&self, id: u64) -> f64 {
        let Some(a) = self.reveal_anim.get(&id) else {
            return Reveal::None.reserve();
        };
        let t = tab_progress(
            Disclose::SPEC,
            Direction::Enter,
            crate::time::now().saturating_duration_since(a.started),
        );
        a.from + (a.to - a.from) * t
    }

    /// Where pill `i`'s chips sit once it has finished opening: the menu-aim
    /// target. Taken at the settled width rather than the current one, so a
    /// pointer that outruns the opening pill is still heading for them, and
    /// over the bar's full height, so a path that grazes the strip of bar
    /// above or below the pill on its way is still heading for them too.
    fn aim_target(&self, i: usize) -> Option<Rect> {
        let r = self.tab_rects.get(i).copied().filter(|r| r.area() > 0.0)?;
        let id = self.tabs.get(i)?.id;
        let still_to_grow = (self.reserve_of(i, Reveal::Full) - self.reveal_reserve(id)).max(0.0);
        let x1 = r.x1 + still_to_grow;
        let (y0, y1) = if self.rect.height() > 0.0 {
            (self.rect.y0.min(r.y0), self.rect.y1.max(r.y1))
        } else {
            (r.y0, r.y1)
        };
        Some(Rect::new(
            (x1 - self.cluster_width(i)).max(r.x0),
            y0,
            x1,
            y1,
        ))
    }

    /// Lets the hover intent's dwell and aim hold run out between pointer
    /// samples.
    fn settle_hover_intent(&mut self) {
        self.hover_intent.settle(crate::time::now());
    }

    /// Pill `i` as the hover intent reasons about it: where it starts (it
    /// grows rightward from there) and where its chips sit once open.
    fn intent_item(&self, i: usize) -> Option<IntentItem> {
        let r = self.tab_rects.get(i).copied().filter(|r| r.area() > 0.0)?;
        Some(IntentItem {
            idx: i,
            edge: r.x0,
            controls: self.aim_target(i)?,
        })
    }

    /// Every control clickable right now, as its pointer target: each
    /// painted ×'s full 32 px target (a press there closes the tab, so the ×
    /// must not move away from it), each revealed chip, and "new tab".
    fn clickable_controls(&self) -> Vec<Rect> {
        let pills = (0..self.tabs.len()).flat_map(|i| {
            let close = self
                .close_rects
                .get(i)
                .filter(|r| r.area() > 0.0)
                .and_then(|_| self.close_hit_rect(i));
            let max = self.max_rects.get(i).copied();
            let search = self.search_rects.get(i).copied();
            let history = self.history_rects.get(i).copied();
            [close, max, search, history]
        });
        pills
            .chain([Some(self.new_tab_rect)])
            .flatten()
            .filter(|r| r.area() > 0.0)
            .collect()
    }

    /// 0 at [`Reveal::None`], 1 at [`Reveal::Full`]. The body wash rides this so the
    /// fill fades out over the same frames the chips slide back under the pill's
    /// right edge, instead of vanishing on the first un-hovered frame and leaving
    /// them painting on nothing.
    fn reveal_fade(&self, idx: usize) -> f64 {
        let Some(t) = self.tabs.get(idx) else {
            return 0.0;
        };
        // Not yet laid out → report the tier the pill is heading for, so a hover
        // set between layout passes doesn't paint a chip with no body under it.
        let reserve = if self.reveal_anim.contains_key(&t.id) {
            self.reveal_reserve(t.id)
        } else {
            self.reserve_of(idx, self.target_reveal(idx))
        };
        let growth = self.reserve_of(idx, Reveal::Full) - Reveal::None.reserve();
        ((reserve - Reveal::None.reserve()) / growth).clamp(0.0, 1.0)
    }

    fn advance_reveal(&mut self, id: u64, to: f64) -> f64 {
        let Some(existing) = self.reveal_anim.get(&id).copied() else {
            // First pass seeing this tab — start settled rather than animating in,
            // so a static capture doesn't depend on how long the renderer took to boot.
            self.reveal_anim.insert(
                id,
                RevealAnim {
                    from: to,
                    to,
                    started: crate::time::now(),
                },
            );
            return to;
        };
        if self.reduce_motion || self.motion_source.lands_at_once() {
            self.reveal_anim.insert(
                id,
                RevealAnim {
                    from: to,
                    to,
                    started: crate::time::now(),
                },
            );
            return to;
        }
        if (existing.to - to).abs() <= f64::EPSILON {
            return self.reveal_reserve(id);
        }
        let from = self.reveal_reserve(id);
        self.reveal_anim.insert(
            id,
            RevealAnim {
                from,
                to,
                started: crate::time::now(),
            },
        );
        from
    }

    /// Test/shot clock control for the hover dwell: settle the pointer intent
    /// as if the dwell had run out since the pointer entered the pill it
    /// rests on. Production input still arms the dwell exclusively through
    /// `on_cursor_moved`. Returns whether a pill is open by pointer now.
    #[cfg(any(test, debug_assertions))]
    pub fn elapse_hover_dwell(&mut self) -> bool {
        // Every deadline lies ahead of the instant it is asked at, and each
        // settle retires the one it reached, so this ends.
        let mut now = crate::time::now();
        while let Some(due) = self.hover_intent.deadline(now) {
            now = due;
            self.hover_intent.settle(now);
        }
        self.hover_intent.open().is_some()
    }

    /// Whether the pointer is aiming at the controls it is on rather than
    /// passing over them: a control's hover wash shows only then.
    fn pointer_settled(&self) -> bool {
        self.hover_intent.settled(crate::time::now())
    }

    /// Hold every in-flight reveal curve `at` milliseconds along it. The curve runs
    /// on wall time and an offscreen frame costs more wall time than the whole
    /// curve lasts, so a capture that only steps frames would see just the
    /// endpoints — same escape as `GroupRow::hold_shift_at`. Returns whether any
    /// curve was running to sample.
    pub fn hold_reveal_at(&mut self, at: Duration) -> bool {
        let mut any = false;
        let now = crate::time::now();
        for anim in self.reveal_anim.values_mut() {
            if (anim.from - anim.to).abs() <= f64::EPSILON {
                continue;
            }
            anim.started = now.checked_sub(at).unwrap_or(now);
            any = true;
        }
        any
    }

    fn reveal_in_flight(&self) -> bool {
        let span = reveal_anim();
        self.reveal_anim
            .values()
            .any(|a| (a.from - a.to).abs() > f64::EPSILON && a.started.elapsed() < span)
    }

    fn natural_pill_width(&self, i: usize, reserve: f64) -> f64 {
        let Some(t) = self.tabs.get(i) else {
            return PILL_MIN_PX;
        };
        if self.icon_only(t.id) {
            return PINNED_W;
        }
        let title_w = self.shaped_title_width(i);
        let lead = self.leading_zone(i);
        let faces = self.face_demand.get(&t.id).copied().unwrap_or(0.0);
        (PILL_PAD_X + lead + title_w + faces + reserve).max(PILL_MIN_PX)
    }

    fn shaped_title_width(&self, i: usize) -> f64 {
        let Some(t) = self.tabs.get(i) else {
            return 0.0;
        };
        match self.title_advance.get(&t.id) {
            Some((title, _, w)) if title == &t.title => *w,
            // A retitled tab keeps the width it was drawn at until `draw`
            // measures the new title. Guessing from the character count
            // first made the pill jump wide, then narrow, on every title
            // change a client makes (a terminal's running status, say).
            Some((_, _, w)) => *w,
            None => {
                // Title lengths never approach `u32::MAX` chars, so this is
                // lossless in practice; `f64::from` preserves the value.
                let chars = u32::try_from(t.title.chars().count()).unwrap_or(u32::MAX);
                f64::from(chars) * APPROX_CHAR_W
            }
        }
    }

    fn leading_zone(&self, i: usize) -> f64 {
        let Some(t) = self.tabs.get(i) else {
            return 0.0;
        };
        if t.app_kind.is_some()
            || t.icon.is_some()
            || self.accent_ids.contains_key(&t.id)
            || self.marks.contains_key(&t.id)
        {
            ICON_W + ICON_GAP
        } else {
            0.0
        }
    }

    fn title_zone(&self, i: usize, r: Rect) -> (f64, f64) {
        let (x0, x1) = self.title_zone_without_faces(i, r);
        let faces = self
            .pill_faces
            .get(i)
            .copied()
            .unwrap_or_default()
            .width(self.face_metrics);
        (x0, (x1 - faces).max(x0))
    }

    fn title_zone_without_faces(&self, i: usize, r: Rect) -> (f64, f64) {
        let x0 = r.x0 + PILL_PAD_X + self.leading_zone(i);
        let reserve = self
            .reveal_px
            .get(i)
            .copied()
            .unwrap_or_else(|| Reveal::None.reserve());
        let x1 = r.x1 - reserve;
        (x0, x1.max(x0))
    }

    /// Elides with an ellipsis when the pill is too narrow — a plain clip-rect used
    /// to slice the last glyph in half instead. `None` for a pinned pill.
    pub fn visible_title(
        &mut self,
        i: usize,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<String> {
        let t = self.tabs.get(i)?;
        if self.icon_only(t.id) {
            return None;
        }
        let style = tab_title_style(theme);
        let title = t.title.clone();
        let id = t.id;
        // Record the shaped advance here — `apply_layout` has no text renderer of
        // its own, so it reads this back on the next pass.
        let (full_w, _) = tr.measure_style(
            &title,
            None,
            &style.clone().with_weight(TextWeight::Semibold),
        );
        let measured = f64::from(full_w);
        let previous = self
            .title_advance
            .insert(id, (title.clone(), style.clone(), measured));
        if previous.is_none_or(|(_, _, w)| (w - measured).abs() > 0.5) {
            self.advance_changed = true;
        }
        let r = *self.tab_rects.get(i)?;
        let (zx0, zx1) = self.title_zone(i, r);
        // A pill's title zone is a few hundred px at most, well inside `f32`'s
        // exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "title zone width is a small on-screen pixel span"
        )]
        let budget = (zx1 - zx0) as f32;
        if full_w <= budget {
            return Some(title);
        }
        if let Some((stem, extension)) = title.rsplit_once('.') {
            let chars: Vec<char> = stem.chars().collect();
            // File identities often differ at the end (number/version). Keep
            // that suffix; drop the extension only if it would hide identity.
            for suffix in [format!(".{extension}"), String::new()] {
                for keep in (3..=chars.len()).rev() {
                    let tail: String = chars
                        .iter()
                        .skip(chars.len().saturating_sub(keep))
                        .collect();
                    let candidate = format!("…{tail}{suffix}");
                    if tr
                        .measure_style(
                            &candidate,
                            None,
                            &style.clone().with_weight(TextWeight::Semibold),
                        )
                        .0
                        <= budget
                    {
                        return Some(candidate);
                    }
                }
            }
        }
        Some(tr.elide_to_width_style(
            &title,
            budget,
            &style.clone().with_weight(TextWeight::Semibold),
        ))
    }

    pub fn tabs(&self) -> &[TabDescriptor] {
        &self.tabs
    }
    pub fn tabs_mut(&mut self) -> &mut Vec<TabDescriptor> {
        &mut self.tabs
    }
    pub fn active_id(&self) -> u64 {
        self.active_id
    }
    pub fn set_active(&mut self, id: u64) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_id = id;
        }
    }

    /// Drain the tab selected by pointer, keyboard, or accessibility input.
    /// Programmatic reconciliation deliberately does not populate this edge.
    pub fn take_activation(&mut self) -> Option<u64> {
        self.pending_activation.take()
    }

    /// Move the selected tab within a focused tab list. The list itself is one
    /// Tab-stop; once it has focus, the standard tab-list keys choose among
    /// its pills. Returns `true` only for a key this widget consumed.
    fn handle_keyboard_navigation(&mut self, key: &Key) -> bool {
        if !self.keyboard_focus || self.tabs.is_empty() {
            return false;
        }
        let current = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        let last = self.tabs.len().saturating_sub(1);
        let target = match key {
            Key::Named(NamedKey::ArrowLeft) => current.checked_sub(1).unwrap_or(last),
            Key::Named(NamedKey::ArrowRight) => {
                if current == last {
                    0
                } else {
                    current.saturating_add(1)
                }
            }
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => last,
            _ => return false,
        };
        if let Some(tab) = self.tabs.get(target) {
            self.active_id = tab.id;
            self.pending_activation = Some(tab.id);
        }
        true
    }
    pub fn is_pinned(&self, id: u64) -> bool {
        self.pinned_ids.contains(&id)
    }
    /// Mark `id` as held by someone else ([`TabHolder`]), or clear the mark.
    /// `true` when that changed what the tab offers.
    pub fn set_holder(&mut self, id: u64, holder: Option<TabHolder>) -> bool {
        match holder {
            Some(holder) => self.held_ids.insert(id, holder.clone()).as_ref() != Some(&holder),
            None => self.held_ids.remove(&id).is_some(),
        }
    }
    /// Who holds `id`, when someone else does.
    pub fn holder(&self, id: u64) -> Option<&TabHolder> {
        self.held_ids.get(&id)
    }
    /// Whether someone else holds `id`.
    pub fn is_held(&self, id: u64) -> bool {
        self.held_ids.contains_key(&id)
    }
    /// Name whoever holds the tab under the pointer, in their colour, beside
    /// it in the tab strip: on someone else's window that is the whole
    /// explanation of why it offers no close and will not move. Within
    /// `viewport`, the strip's band, so it never covers the window's content.
    pub(crate) fn draw_holder_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        if self.pending_drag.is_some() {
            return;
        }
        let Some((t, rect)) = self
            .hover_idx
            .and_then(|i| self.tabs.get(i).zip(self.tab_rects.get(i)))
        else {
            return;
        };
        let Some(holder) = self.held_ids.get(&t.id) else {
            return;
        };
        let label = crate::i18n!("tab-held-by", "name" => holder.name.clone());
        let color = theme.tokens.colors.presence.of_rgb(holder.rgb);
        super::tooltip::draw_person_tooltip_on(
            scene,
            transform,
            tr,
            theme,
            (*rect, super::tooltip::TooltipSide::Beside),
            (&label, color),
            viewport,
        );
    }

    /// How far the held tab being dragged is pulled from its place, if one
    /// is: following the drag, or springing back after it.
    fn resist_offset(&self) -> Option<(u64, vello::kurbo::Vec2)> {
        let resist = self.resist?;
        let keep = match resist.released_at {
            None => 1.0,
            Some(at) => {
                let elapsed = crate::time::now().saturating_duration_since(at);
                if self.reduce_motion || elapsed >= resist_return() {
                    return None;
                }
                1.0 - tab_progress(Snap::SPEC, Direction::Enter, elapsed)
            }
        };
        Some((resist.tab, resist.pull * keep))
    }
    /// Caller ensures the tab exists; a stale entry is tolerated (filtered at
    /// render time) but should be cleaned up via `unpin` when the tab is removed.
    pub fn pin(&mut self, id: u64) {
        self.pinned_ids.insert(id);
    }
    pub fn unpin(&mut self, id: u64) {
        self.pinned_ids.remove(&id);
    }
    pub fn toggle_pin(&mut self, id: u64) {
        if !self.pinned_ids.insert(id) {
            self.pinned_ids.remove(&id);
        }
    }
    /// Replace tab `id`'s artwork. `true` when that changed what it paints.
    pub fn set_icon(&mut self, id: u64, icon: Option<icons::AppImage>) -> bool {
        match self.tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) if tab.icon != icon => {
                tab.icon = icon;
                true
            }
            _ => false,
        }
    }
    /// Tint `id`'s leading marker. A tab with no `app_kind` paints a filled dot
    /// in the leading slot; one with both tints its app glyph instead. Pass a
    /// colour read off the `Theme` — see `Self::accent_ids`.
    pub fn set_accent(&mut self, id: u64, color: vello::peniko::Color) {
        self.accent_ids.insert(id, color);
    }
    /// Drop `id`'s leading accent, giving the pill its leading zone back.
    pub fn clear_accent(&mut self, id: u64) {
        self.accent_ids.remove(&id);
    }
    /// The accent set for `id`, if any.
    pub fn accent(&self, id: u64) -> Option<vello::peniko::Color> {
        self.accent_ids.get(&id).copied()
    }
    /// Mark `id` as holding unsaved changes, or clear the mark.
    pub fn set_modified(&mut self, id: u64, modified: bool) {
        if modified {
            self.modified_ids.insert(id);
        } else {
            self.modified_ids.remove(&id);
        }
    }
    /// True if `id` is currently marked as holding unsaved changes.
    pub fn is_modified(&self, id: u64) -> bool {
        self.modified_ids.contains(&id)
    }
    /// Offer, or withhold, the hover-reveal maximize chip. Defaults to offered
    /// — see `Self::show_maximize` for when to turn it off.
    pub fn set_show_maximize(&mut self, show: bool) {
        self.show_maximize = show;
    }
    pub fn remove_tab(&mut self, id: u64) -> Option<usize> {
        let idx = self.tabs.iter().position(|t| t.id == id)?;
        // Snapshot before mutating so the spring has something to interpolate
        // from; the removed tab has no target and just disappears.
        self.snapshot_layout_for_spring();
        self.tabs.remove(idx);
        // Drop any stale sidecar entries so a recycled id starts unpinned,
        // untinted and unmodified.
        self.pinned_ids.remove(&id);
        self.accent_ids.remove(&id);
        self.modified_ids.remove(&id);
        if self.active_id == id {
            self.active_id = self
                .tabs
                .get(idx)
                .or_else(|| self.tabs.last())
                .map_or(0, |t| t.id);
        }
        Some(idx)
    }

    /// Active-id shifts to the neighbour immediately so the user never sees a
    /// "ghost active" frame during the fade. Idempotent on a tab already in
    /// `closing_tabs` — restarting the curve would pop the pill back to full
    /// opacity, worse than letting it finish.
    pub fn start_closing(&mut self, id: u64) -> bool {
        let Some((idx, desc)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, t)| t.id == id)
            .map(|(i, t)| (i, t.clone()))
        else {
            return false;
        };
        if self.closing_tabs.iter().any(|c| c.desc.id == id) {
            return false;
        }
        // Snapshot before the remove shifts `tab_rects`, so the fade renders in
        // place. Falls back to `Rect::ZERO` if the bar hasn't laid out yet (e.g. a
        // synthetic test setup) — nothing visible paints there anyway.
        let pill_rect = self.tab_rects.get(idx).copied().unwrap_or(Rect::ZERO);
        let close_rect = self.close_rects.get(idx).copied().unwrap_or(Rect::ZERO);
        self.pending_close = Some(id);
        self.snapshot_layout_for_spring();
        let was_active = self.active_id == id;
        self.tabs.remove(idx);
        if self.active_id == id {
            // Prefer the tab that slid into this index; fall back to the
            // last tab, then to 0 if the group is now empty.
            self.active_id = self
                .tabs
                .get(idx)
                .or_else(|| self.tabs.last())
                .map_or(0, |t| t.id);
        }
        self.closing_tabs.push(ClosingTab {
            desc,
            index: idx,
            was_active,
            pill_rect,
            close_rect,
            started_at: crate::time::now(),
        });
        true
    }

    /// Restore a requested close whose host still owns the live tab. No
    /// metadata or focus is lost while the client asks for a save decision.
    pub fn cancel_close(&mut self, id: u64) -> bool {
        let Some(index) = self.closing_tabs.iter().position(|tab| tab.desc.id == id) else {
            return false;
        };
        let tab = self.closing_tabs.remove(index);
        if !self.tabs.iter().any(|live| live.id == id) {
            self.tabs.insert(tab.index.min(self.tabs.len()), tab.desc);
        }
        if tab.was_active {
            self.active_id = id;
        }
        if self.pending_close == Some(id) {
            self.pending_close = None;
        }
        true
    }

    /// Called by `WindowGroup` once its own fade-out has elapsed.
    pub fn finalize_close(&mut self, id: u64) -> bool {
        let before = self.closing_tabs.len();
        self.closing_tabs.retain(|c| c.desc.id != id);
        // The pill itself left at `start_closing`; its sidecar state only
        // needs to go once nothing is still painting it, so a recycled id
        // doesn't inherit a dead tab's tint or unsaved-changes dot.
        self.accent_ids.remove(&id);
        self.modified_ids.remove(&id);
        self.closing_tabs.len() != before
    }

    pub fn has_closing(&self) -> bool {
        !self.closing_tabs.is_empty()
    }

    /// True if `id` is mid close-fade — already gone from the live tab list
    /// but still rendering its the close fade ([`close_fade`]) animation. A caller that
    /// reconciles pills against its own model checks this before
    /// re-inserting a still-open item, so cancelling a close (e.g. answering
    /// "no" to a discard-unsaved-changes prompt) doesn't pop a duplicate
    /// pill mid-fade.
    pub fn is_closing(&self, id: u64) -> bool {
        self.closing_tabs.iter().any(|c| c.desc.id == id)
    }

    /// `Vec<u64>`, not a borrowed slice of `ClosingTab`, to keep the private struct
    /// out of the public API.
    #[cfg(test)]
    pub fn closing_tab_ids(&self) -> Vec<u64> {
        self.closing_tabs.iter().map(|c| c.desc.id).collect()
    }

    /// `Instant` the close fade was latched for `id` — recorded at
    /// close-start, not on finalize.
    #[cfg(test)]
    pub fn closing_started_at(&self, id: u64) -> Option<Instant> {
        self.closing_tabs
            .iter()
            .find(|c| c.desc.id == id)
            .map(|c| c.started_at)
    }
    /// Released inside this bar → `pending_reorder` to the gap the drop indicator
    /// is pointing at ([`Self::drop_index_for`], same index `hover_drop_idx` uses).
    /// Released outside this bar's rect → `pending_detach`; the parent `GroupRow`
    /// resolves the target bar by hit-testing sibling `bar_rect()`s.
    fn finish_drag_release(&mut self, handled: &mut bool) {
        if self.toolbar_row.release(self.last_cursor) {
            *handled = true;
            return;
        }
        if let Some(control) = self.pressed_control.take() {
            // A control acts on release over itself, like every button.
            if self.controls.hit(self.last_cursor) == Some(control) {
                let active = self.active_id;
                match control {
                    super::window_controls::WindowControl::Close => {
                        let _ = self.start_closing(active);
                    }
                    super::window_controls::WindowControl::Maximize => {
                        self.pending_max = Some(active);
                    }
                    super::window_controls::WindowControl::History => {
                        let _ = self.press_history(active);
                    }
                }
            }
            *handled = true;
            return;
        }
        if let Some(resist) = self.resist.as_mut().filter(|r| r.released_at.is_none()) {
            if resist.pull == vello::kurbo::Vec2::ZERO {
                self.resist = None;
            } else {
                resist.released_at = Some(crate::time::now());
                *handled = true;
            }
        }
        if self.pending_drag.is_some() {
            if let Some((from, _)) = self.press_origin {
                if self.rect.contains(self.last_cursor) {
                    // Removing the dragged pill shifts every later slot down by
                    // one, so an insertion point past its own position maps to
                    // `ins - 1`.
                    let ins = self.drop_index_for(self.last_cursor);
                    let to = if ins > from {
                        ins.saturating_sub(1)
                    } else {
                        ins
                    };
                    if to != from && to < self.tabs.len() {
                        self.pending_reorder = Some((from, to));
                        *handled = true;
                    }
                } else if let Some(t) = self.tabs.get(from) {
                    self.pending_detach = Some((t.id, self.last_cursor));
                    *handled = true;
                }
            }
            self.pending_drag = None;
        }
        self.press_origin = None;
        // Reset drag/drop-zone state so a fresh drag starts with nothing stale
        // painting.
        self.drag_offset = None;
        self.drag_lifted_at = None;
        self.drop_zone_target = None;
        self.hover_drop_idx = None;
        self.hover_drop_animated_at = None;
    }

    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        self.snapshot_layout_for_spring();
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
    }
    /// Abandon a pending or lifted tab without turning its release into a drop.
    pub fn cancel_drag(&mut self) -> bool {
        if self.press_origin.is_none() && self.pending_drag.is_none() {
            return false;
        }
        self.pending_drag = None;
        let mut handled = false;
        self.finish_drag_release(&mut handled);
        true
    }

    pub fn drag_in_flight(&self) -> bool {
        self.pending_drag.is_some()
    }
    pub fn take_reorder(&mut self) -> Option<(usize, usize)> {
        self.pending_reorder.take()
    }
    pub fn take_close(&mut self) -> Option<u64> {
        self.pending_close.take()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn close_rect_for_tests(&self, tab_id: u64) -> Option<Rect> {
        self.tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .and_then(|index| self.close_hit_rect(index))
            .filter(|rect| rect.area() > 0.0)
    }

    /// The pointer target for a close glyph. During a live layout this is the
    /// 32px target at the pill's trailing edge; the visual glyph itself stays
    /// compact. The fallback keeps focused geometry tests that seed only the
    /// visual rects representative of the pre-layout state.
    fn close_hit_rect(&self, index: usize) -> Option<Rect> {
        self.close_hit_rects
            .get(index)
            .copied()
            .filter(|rect| rect.area() > 0.0)
            .or_else(|| {
                self.close_rects
                    .get(index)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
            })
    }

    pub fn take_context_menu(&mut self) -> Option<(u64, Point)> {
        self.pending_context_menu.take()
    }
    pub fn take_max(&mut self) -> Option<u64> {
        self.pending_max.take()
    }
    pub fn take_search(&mut self) -> Option<u64> {
        self.pending_search.take()
    }
    /// A press on a pill's History chip since the last call: the tab, and
    /// the control it showed (open History, or close it).
    pub fn take_history(&mut self) -> Option<(u64, HistoryControl)> {
        self.pending_history.take()
    }
    /// The History control tab `id`'s window offers, or `None` where it has
    /// no History: one more chip in the pill's cluster, left of search.
    pub fn set_history_control(&mut self, id: u64, control: Option<HistoryControl>) {
        let had = self.history.contains_key(&id);
        match control {
            Some(control) => {
                self.history.insert(id, control);
            }
            None => {
                self.history.remove(&id);
            }
        }
        if had != control.is_some() {
            // The pill's cluster gained or lost a chip: lay the row out again.
            self.advance_changed = true;
        }
    }
    /// The History control tab `id` shows, if any.
    #[must_use]
    pub fn history_control(&self, id: u64) -> Option<HistoryControl> {
        self.history.get(&id).copied()
    }
    /// Press tab `id`'s History chip: select the tab, and queue its control.
    fn press_history(&mut self, id: u64) -> bool {
        let Some(control) = self.history.get(&id).copied() else {
            return false;
        };
        if self.active_id != id {
            self.active_id = id;
            self.pending_activation = Some(id);
        }
        self.pending_history = Some((id, control));
        true
    }
    pub fn take_new_tab(&mut self) -> Option<()> {
        self.pending_new_tab.take()
    }
    /// `Rect::ZERO` when the pills have filled the bar and the chip is suppressed.
    pub fn new_tab_rect(&self) -> Rect {
        self.new_tab_rect
    }
    /// Pins the close chip to lucide's `X` so a revert to a hand-drawn primitive
    /// flips the suite red.
    #[cfg(test)]
    pub fn close_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::X
    }
    /// Always `Maximize2`: the cluster only renders non-maximized. The
    /// `Minimize2` swap lives on `WindowGroup`'s own maximized-controls accessor.
    #[cfg(test)]
    pub fn max_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::Maximize2
    }
    #[cfg(test)]
    pub fn search_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::Search
    }
    pub fn take_detach(&mut self) -> Option<(u64, Point)> {
        self.pending_detach.take()
    }
    pub fn bar_rect(&self) -> Rect {
        self.rect
    }
    pub(crate) fn max_rects_internal(&self) -> &[Rect] {
        &self.max_rects
    }
    pub fn tab_rects(&self) -> &[Rect] {
        &self.tab_rects
    }
    /// Gated `cfg(any(test, debug_assertions))` rather than `cfg(test)` so
    /// integration tests in other crates (which link the release-shaped lib) can
    /// resolve the chip from live geometry, matching `WindowGroup::bar_for_tests`.
    #[cfg(any(test, debug_assertions))]
    pub fn max_rects(&self) -> &[Rect] {
        &self.max_rects
    }

    #[cfg(any(test, debug_assertions))]
    pub fn search_rects(&self) -> &[Rect] {
        &self.search_rects
    }

    #[cfg(any(test, debug_assertions))]
    pub fn history_rects(&self) -> &[Rect] {
        &self.history_rects
    }

    /// The pointer target of a maximize/search chip: at least the theme's
    /// minimum pointer target on a side (HIG §9.6: 32 px), centred on the
    /// chip's glyph rect, and never outside the bar. The painted chip stays
    /// `ACTION_W` wide and the pill widths the layout pins do not move, so
    /// neighbouring targets inside one pill overlap; a hover or press in an
    /// overlap resolves by priority — close, then maximize, then search —
    /// which keeps the close target its full 32 px. `Rect::ZERO` for a
    /// chip the reveal has not placed.
    pub fn action_hit_rect(&self, chip: Rect) -> Rect {
        if chip.area() <= 0.0 {
            return Rect::ZERO;
        }
        let side = self.min_pointer_target.max(chip.width()).max(chip.height());
        let target = square_at(chip.center(), side);
        if self.rect.area() > 0.0 {
            target.intersect(self.rect)
        } else {
            target
        }
    }

    /// The action chip of any pill under `p`, as `(tab index, chip)`. A
    /// painted glyph wins outright — the maximize and search glyphs never
    /// overlap, so a press on what the eye sees goes where the eye expects
    /// — and only a point in neither glyph falls to the enlarged targets,
    /// by priority: maximize, then search. Points inside a close target are
    /// the close chip's and are resolved before this is consulted.
    fn action_chip_at(&self, p: Point) -> Option<(usize, ActionChip)> {
        let glyph = (0..self.tabs.len()).find_map(|i| {
            let max = self.max_rects.get(i).copied().unwrap_or(Rect::ZERO);
            let search = self.search_rects.get(i).copied().unwrap_or(Rect::ZERO);
            let history = self.history_rects.get(i).copied().unwrap_or(Rect::ZERO);
            if max.area() > 0.0 && max.contains(p) {
                Some((i, ActionChip::Maximize))
            } else if search.area() > 0.0 && search.contains(p) {
                Some((i, ActionChip::Search))
            } else if history.area() > 0.0 && history.contains(p) {
                Some((i, ActionChip::History))
            } else {
                None
            }
        });
        glyph.or_else(|| {
            (0..self.tabs.len()).find_map(|i| {
                let max = self.max_rects.get(i).copied().unwrap_or(Rect::ZERO);
                let search = self.search_rects.get(i).copied().unwrap_or(Rect::ZERO);
                let history = self.history_rects.get(i).copied().unwrap_or(Rect::ZERO);
                if self.action_hit_rect(max).contains(p) {
                    Some((i, ActionChip::Maximize))
                } else if self.action_hit_rect(search).contains(p) {
                    Some((i, ActionChip::Search))
                } else if self.action_hit_rect(history).contains(p) {
                    Some((i, ActionChip::History))
                } else {
                    None
                }
            })
        })
    }
    /// Insert before the first pill whose horizontal midpoint is past
    /// `release_point.x`; append if past every midpoint.
    pub fn drop_index_for(&self, release_point: Point) -> usize {
        for (i, r) in self.tab_rects.iter().enumerate() {
            let mid_x = f64::midpoint(r.x0, r.x1);
            if release_point.x < mid_x {
                return i;
            }
        }
        self.tab_rects.len()
    }
    /// Used by cross-group drag routing in `GroupRow::handle_event` to splice a
    /// detached tab into the target bar without forcing a full rebuild.
    pub fn insert_tab(&mut self, idx: usize, desc: TabDescriptor) {
        let clamped = idx.min(self.tabs.len());
        let id = desc.id;
        // Snapshot so neighbours slide aside. The new tab isn't in `prev_*`, so
        // `draw` plays its own fade-in via `inserted_at` instead.
        self.snapshot_layout_for_spring();
        self.tabs.insert(clamped, desc);
        self.inserted_at.insert(id, crate::time::now());
        self.active_id = id;
    }

    #[cfg(test)]
    pub(crate) fn drop_zone_target(&self) -> Option<usize> {
        self.drop_zone_target
    }

    #[cfg(test)]
    pub(crate) fn hover_drop_idx(&self) -> Option<usize> {
        self.hover_drop_idx
    }

    /// Called by `GroupRow::handle_event` for every sibling bar while a child
    /// group's tab drag is in flight. No-op while this bar has its own
    /// `pending_drag` in flight — the own-bar `CursorMoved` branch already drives
    /// `hover_drop_idx`, and the parent's fan-out shouldn't fight with it.
    pub fn set_external_drag_hover(&mut self, cursor: Option<Point>) {
        if self.pending_drag.is_some() {
            return;
        }
        let new_target = cursor
            .filter(|p| self.rect.contains(*p))
            .map(|p| self.drop_index_for(p));
        if new_target != self.hover_drop_idx {
            self.hover_drop_idx = new_target;
            if new_target.is_some() {
                self.hover_drop_animated_at = Some(crate::time::now());
            } else {
                self.hover_drop_animated_at = None;
            }
        }
    }

    /// Stand the bar on a docked strip of its own instead of the window
    /// ground: its tabs then lift off the strip's ground.
    pub fn set_on_strip(&mut self, on_strip: bool) {
        self.on_strip = on_strip;
    }

    /// The span the pills are laid out across: the whole bar, or on a
    /// docked strip the strip less the inset its pills keep from its ends,
    /// the same inset they keep from its top and bottom, so the strip's
    /// capsule is concentric with the tabs in it.
    fn pill_row(&self) -> Rect {
        let tab_row = self.tab_row();
        let row = if self.on_strip && tab_row.width() > 2.0 * TAB_PILL_INSET_Y {
            tab_row.inset((-TAB_PILL_INSET_Y, 0.0))
        } else {
            tab_row
        };
        if !self.window_controls {
            return row;
        }
        // The title's glyph keeps the same distance from the window's
        // leading edge as the close glyph keeps from its trailing edge.
        let lead = WINDOW_BAR_LEAD_PX;
        let x0 = row.x0 + lead;
        let x1 = (self.controls.content_limit(row) - self.search_control.unwrap_or(0.0)).max(x0);
        Rect::new(x0, row.y0, x1, row.y1)
    }

    /// The ink of an idle tab's title and glyphs: the dim tier, unless the
    /// bar stands on a frosted strip, whose ground is the desktop behind it
    /// under the tint. There it is the ink that holds on the tint over black
    /// and over white alike, so a bright wallpaper never washes it out.
    fn idle_ink(&self, theme: &Theme) -> vello::peniko::Color {
        let colors = &theme.tokens.colors;
        if !(self.on_strip && crate::chrome::selector_finish(theme).translucent()) {
            return colors.text_dim;
        }
        let grounds = crate::tokens::SelectorFinish::frosted_extremes(
            self.ground(theme),
            colors.surfaces.material_opacity,
        );
        colors.ink_on_any(
            &grounds,
            colors.text_dim,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        )
    }

    /// The ground this bar's tabs stand on: the docked strip's, or the
    /// window's.
    fn ground(&self, theme: &Theme) -> vello::peniko::Color {
        if self.on_strip {
            crate::chrome::strip_ground(theme)
        } else {
            theme.tokens.colors.surfaces.content
        }
    }

    /// The single place active/hover/pressed are read into an appearance, so
    /// `draw_pill_inner` and every other reader can't drift apart. Pressed tracks
    /// the press-source pill across the whole press → (optional drag) → release
    /// lifecycle, so press-scale composes with source-dim during a drag.
    fn pill_look(&self, idx: usize, theme: &Theme, suppress_hover: bool) -> PillAppearance {
        let is_active = self.tabs.get(idx).is_some_and(|t| t.id == self.active_id);
        let is_hover = !suppress_hover && Some(idx) == self.hover_idx;
        let is_pressed = !suppress_hover && self.press_origin.is_some_and(|(j, _)| j == idx);
        let is_carried = suppress_hover
            && matches!(
                &self.pending_drag,
                Some(DragPayload::Tab { tab, .. })
                    if self.tabs.get(idx).is_some_and(|item| item.id == *tab)
            );
        // The carried object needs its own readable surface even when its
        // source tab is inactive. This changes paint, not tab selection.
        let ground = self.ground(theme);
        let mut look =
            pill_appearance(theme, ground, is_active || is_carried, is_hover, is_pressed);
        // Inactive body rides the reveal curve instead of switching on/off with
        // the pointer, fading (fill + edge together, so the hairline can't flicker
        // off a frame late) over the same frames its chips slide back in.
        if !is_active && !is_pressed && !is_carried {
            let fade = if suppress_hover {
                0.0
            } else {
                self.reveal_fade(idx)
            };
            if fade > 0.0 {
                let hovered = pill_appearance(theme, ground, false, true, false);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "fade is clamped to 0.0..=1.0 by reveal_fade"
                )]
                {
                    look.fill = hovered.fill.with_alpha_factor(fade as f32);
                    look.border = hovered.border.map(|c| c.with_alpha_factor(fade as f32));
                }
            } else {
                look.fill = vello::peniko::Color::TRANSPARENT;
                look.border = None;
            }
        }
        look
    }

    /// Resolved through the exact predicate + appearance the draw pass uses.
    #[cfg(test)]
    pub(crate) fn close_affordance_visible_for_tests(&self, idx: usize, theme: &Theme) -> bool {
        let Some(t) = self.tabs.get(idx) else {
            return false;
        };
        close_affordance_visible(
            &self.pill_look(idx, theme, false),
            self.pinned_ids.contains(&t.id),
        )
    }

    /// Lets hover behaviour be exercised without a GPU draw.
    #[cfg(test)]
    pub(crate) fn close_chip_color_for_tests(
        &self,
        idx: usize,
        theme: &Theme,
    ) -> Option<vello::peniko::Color> {
        if !self.close_affordance_visible_for_tests(idx, theme) {
            return None;
        }
        let close_hover = Some(idx) == self.hover_close_idx && self.pointer_settled();
        let is_hover = Some(idx) == self.hover_idx;
        if close_hover {
            Some(theme.tokens.colors.semantic.danger.with_alpha_factor(0.8))
        } else if is_hover {
            Some(chip_hover_tint(self.ground(theme), CHIP_HOVER_TINT_ALPHA))
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn close_glyph_color_for_tests(
        &self,
        idx: usize,
        theme: &Theme,
    ) -> Option<vello::peniko::Color> {
        if !self.close_affordance_visible_for_tests(idx, theme) {
            return None;
        }
        let close_hover = Some(idx) == self.hover_close_idx && self.pointer_settled();
        let is_active = self.tabs[idx].id == self.active_id;
        Some(if close_hover {
            vello::peniko::Color::from_rgba8(255, 255, 255, 255)
        } else if is_active {
            theme.tokens.colors.text_primary
        } else {
            self.idle_ink(theme)
        })
    }

    /// The unsaved-changes dot's colour, mirroring the exact condition
    /// `draw_pill_inner` paints it under — `None` whenever the × would show
    /// instead (not modified, or the pointer is on the pill or its close chip).
    #[cfg(test)]
    pub(crate) fn modified_marker_color_for_tests(
        &self,
        idx: usize,
        theme: &Theme,
    ) -> Option<vello::peniko::Color> {
        let t = self.tabs.get(idx)?;
        if !self.modified_ids.contains(&t.id) {
            return None;
        }
        let is_hover = Some(idx) == self.hover_idx;
        let close_hover = Some(idx) == self.hover_close_idx;
        if is_hover || close_hover {
            return None;
        }
        let is_active = t.id == self.active_id;
        Some(if is_active {
            theme.tokens.colors.text_primary
        } else {
            self.idle_ink(theme)
        })
    }
}

// ---------- metrics ----------
//
// Compile-time because `Reveal::reserve` and `CHIP_CLUSTER_W` are `const`
// arithmetic over them, and because the shell pins several from outside
// (`TAB_BAR_HEIGHT`, `NEW_TAB_CHIP_PX`). Against the 4 px spacing scale:
//
//   on the grid   TAB_BAR_HEIGHT 44, PILL_PAD_X 12 (= s3), CLOSE_HIT_PX 32
//                 (= s8, the minimum pointer target), CHIP_PITCH 24 (= s6),
//                 ACTION_HIT_INSET_Y 4 (= s1), PINNED_W 28, MIN_TITLE_PX 40,
//                 PILL_MIN_PX 84
//   off the grid  TAB_PILL_INSET_Y 6 (pill 32 tall inside a 44 bar — the
//                 bar height is pinned), PILL_GAP 6, ICON_GAP 6, CLOSE_W 18,
//                 ACTION_W 14, ICON_W 14, CLOSE_INSET 7, TITLE_CHIP_GAP 10,
//                 CHIP_FADE_PX 10, PILL_SQUEEZE_FLOOR_PX 54
//
// The off-grid values are lucide glyph sizes and optical gaps around a
// 32 px pill; they are kept, named, and audited here rather than rounded,
// because rounding them moves every pill width the layout tests pin.

pub const TAB_BAR_HEIGHT: f64 = 44.0;
/// 44 – 2×6 = 32 px pill height and pointer-sized + affordance.
pub const TAB_PILL_INSET_Y: f64 = 6.0;
/// Action targets of the window's own controls (History, maximize/restore,
/// close), as assistive technology and automation address them.
pub const WINDOW_HISTORY_TARGET: &str = "window-history";
pub const WINDOW_MAXIMIZE_TARGET: &str = "window-maximize";
pub const WINDOW_CLOSE_TARGET: &str = "window-close";
/// Where a window bar's row starts: the leading glyph then sits as far from
/// the window's leading edge as the close glyph does from its trailing edge.
const WINDOW_BAR_LEAD_PX: f64 = super::window_controls::EDGE_INSET
    + (super::window_controls::CONTROL_PX - ICON_W) / 2.0
    - PILL_PAD_X;
/// The row the active tab's slots take under the tabs of a tabbed window.
pub const TOOLBAR_ROW_HEIGHT: f64 = 40.0;
/// Between the window's icon and its slots in the title's row.
const TOOLBAR_LEAD_GAP: f64 = 8.0;
/// Square at pill height so the "+" chip reads as part of the pill row.
pub const NEW_TAB_CHIP_PX: f64 = TAB_BAR_HEIGHT - 2.0 * TAB_PILL_INSET_Y;

const PILL_GAP: f64 = 6.0;
/// Text inset on either end of a pill; the leading glyph starts here too.
const PILL_PAD_X: f64 = 12.0;
const CLOSE_W: f64 = 18.0;
/// The glyph remains compact; its pointer target does not.
const CLOSE_HIT_PX: f64 = 32.0;
/// The × sits centred in its pointer target, so the three chips can share
/// one rhythm (see [`CHIP_PITCH`]).
const CLOSE_INSET: f64 = (CLOSE_HIT_PX - CLOSE_W) / 2.0;
/// How far a maximize/search chip's pointer rect stops short of the pill's
/// top and bottom edges.
const ACTION_HIT_INSET_Y: f64 = 4.0;
/// The group-focus keyline under an active pill: its thickness, and how
/// far it floats above the pill's bottom edge.
const FOCUS_KEYLINE_PX: f64 = 2.0;
const FOCUS_KEYLINE_LIFT: f64 = 0.5;
/// Hover wash behind the close chip when the *pill* (not the chip) is
/// hovered, and behind a hovered maximize/search chip — the latter stronger
/// because a 14 px glyph needs more ground to read as pressed.
const CHIP_HOVER_TINT_ALPHA: u8 = 28;
const ACTION_HOVER_TINT_ALPHA: u8 = 64;
/// Narrower than the close chip: close must stay a comfortable pointer
/// target; maximize/search are secondary and also reachable from the
/// context menu.
const ACTION_W: f64 = 14.0;
/// Centre-to-centre distance between neighbouring chips. Search, maximize
/// and close glyphs stand on one even rhythm; they used to sit 18 px apart
/// on the left and 31 px on the right, which read as two loose groups.
const CHIP_PITCH: f64 = 24.0;
const ICON_W: f64 = 14.0;
/// The box the × glyph is drawn in. Lucide's × spans the middle 12 of its 24
/// units while maximize and search span 18, so drawn in the 14 px box the
/// action glyphs use, the × would be visibly smaller and the ink gaps
/// uneven. At 1.5 × `ACTION_W` the three glyphs carry the same 10.5 px of
/// ink, and with their centres one [`CHIP_PITCH`] apart the gaps between
/// them are equal too.
const CLOSE_GLYPH_PX: f64 = ACTION_W * 1.5;
const ICON_GAP: f64 = 6.0;
const PINNED_W: f64 = 28.0;
/// Radius of the leading accent dot, when a tab is classified by colour
/// rather than by a typed app glyph.
const ACCENT_DOT_R: f64 = 4.0;
/// Radius of the unsaved-changes dot painted in the close slot. Slightly
/// smaller than the accent dot so the two never read as the same mark.
const MODIFIED_DOT_R: f64 = 3.5;
/// Fits an app glyph, a couple of characters, and the close chip.
const PILL_MIN_PX: f64 = 84.0;
/// Peer faces on a pill: at most this many faces before a "+N" disc.
const PILL_MAX_FACES: usize = 2;
/// Below this a pill can't host a legible title fragment alongside the close
/// chip, so squeezing further would be worse than eliding harder.
const PILL_SQUEEZE_FLOOR_PX: f64 = 54.0;

/// Narrowest a pill's title zone squeezes to before the reveal gives up chips
/// instead of characters — the cluster drops search, then maximize, then close.
const MIN_TITLE_PX: f64 = 40.0;

/// Right edge to the left edge of the maximize glyph: the reveal zone that
/// places it.
const MAX_CHIP_REACH: f64 = CLOSE_HIT_PX / 2.0 + CHIP_PITCH + ACTION_W / 2.0;

/// Right edge to the left edge of the leftmost revealed chip: close, then
/// maximize, then search.
const CHIP_CLUSTER_W: f64 = MAX_CHIP_REACH + CHIP_PITCH;

/// Right edge to the left edge of the History chip, one pitch past search,
/// on a pill whose window has History.
const HISTORY_CHIP_REACH: f64 = CHIP_CLUSTER_W + CHIP_PITCH;

// The painted maximize glyph stays clear of the close chip's 32 px target,
// so a press on what the eye sees as maximize never closes the tab.
const _: () = assert!(CLOSE_HIT_PX / 2.0 + CHIP_PITCH - ACTION_W / 2.0 > CLOSE_HIT_PX);

/// Gap between title and trailing chip; must be at least [`CHIP_FADE_PX`] for
/// a settled pill to paint its cluster at full opacity.
const TITLE_CHIP_GAP: f64 = 10.0;

/// How far past a chip the trailing zone must open before it's at full
/// strength — without it the chips arrive as hard pops, not a resolve.
const CHIP_FADE_PX: f64 = 10.0;

/// A pill's trailing-zone width is also what controls it shows — one value
/// drives both, so a pill is always exactly as wide as what it draws.
///
/// Hover growth is anchored: (1) rightward only, so a pill's own left edge
/// and everyone left of it never move; (2) into headroom `apply_layout`
/// already held back by fitting the row at *resting* widths inside
/// `width` less the widest opening, so neighbours translate but are never
/// re-fitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reveal {
    None,
    /// Close chip only — an active pill at rest.
    Close,
    /// Close, maximize and search — a hovered pill — and History where the
    /// window has it (`TabBar::reserve_of`).
    Full,
}

impl Reveal {
    pub(crate) const fn reserve(self) -> f64 {
        match self {
            Reveal::None => PILL_PAD_X,
            Reveal::Close => CLOSE_HIT_PX + TITLE_CHIP_GAP,
            Reveal::Full => CHIP_CLUSTER_W + TITLE_CHIP_GAP,
        }
    }
}

/// How far a pill without History opens: the headroom a row of them holds
/// back so growth never re-fits anyone.
#[cfg(test)]
const HOVER_GROWTH_PX: f64 = Reveal::Full.reserve() - Reveal::None.reserve();

/// First-frame estimate before `draw` records the real advance into
/// [`TabBar::title_advance`]. Deliberately an over-estimate: guessing wide is
/// invisible by the next frame; guessing narrow elides the title, which is
/// visible and sticky since a one-frame render never gets corrected.
const APPROX_CHAR_W: f64 = 9.5;

/// A row's clip is at the row rather than the group, so an unfit row's tail
/// pills would paint on top of the neighbour. Squeezes in three stages so the
/// pills that can give up space do it first:
///
/// 1. Everything fits — hand back the natural widths untouched.
/// 2. Shrink unpinned pills proportionally, no further than
///    [`PILL_SQUEEZE_FLOOR_PX`] (pinned pills are already at their minimum).
/// 3. Still over: scale every pill, pinned included. Ugly, but a legible
///    sliver beats a pill painted across its neighbour.
fn fit_pill_widths(natural: &[f64], pinned: &[bool], avail: f64) -> Vec<f64> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    // Not laid out yet — leave natural widths alone.
    if avail <= 0.0 {
        return natural.to_vec();
    }
    // `n >= 1` from the `n == 0` check above, so `n - 1` cannot underflow; tab
    // counts stay far below `u32::MAX`, so the `f64` conversion is exact.
    let gap_count = u32::try_from(n.saturating_sub(1)).unwrap_or(u32::MAX);
    let gaps = PILL_GAP * f64::from(gap_count);
    let budget = (avail - gaps).max(0.0);
    let total: f64 = natural.iter().sum();
    if total <= budget {
        return natural.to_vec();
    }

    let mut out = natural.to_vec();
    let pinned_total: f64 = natural
        .iter()
        .zip(pinned)
        .filter(|(_, p)| **p)
        .map(|(w, _)| *w)
        .sum();
    let flexible_total = total - pinned_total;
    if flexible_total > 0.0 {
        let flexible_budget = (budget - pinned_total).max(0.0);
        let scale = (flexible_budget / flexible_total).clamp(0.0, 1.0);
        for (w, is_pinned) in out.iter_mut().zip(pinned) {
            if !*is_pinned {
                *w = (*w * scale).max(PILL_SQUEEZE_FLOOR_PX);
            }
        }
    }

    // The floor above can still push the row over budget; uniform scale is
    // the only remaining lever, and is guaranteed to land inside `avail`.
    let squeezed: f64 = out.iter().sum();
    if squeezed > budget && squeezed > 0.0 {
        let scale = budget / squeezed;
        for w in &mut out {
            *w *= scale;
        }
    }
    out
}

/// Chip rects (close, maximize, search) for a pill of rect `r` whose reveal
/// zone has opened to `effective` px. A pinned pill never gets chips.
// Every parameter (widths/insets, plus the rect and reveal they're applied
// to) is independently load-bearing for this geometry.

fn chip_rects(
    r: Rect,
    effective: f64,
    is_pinned: bool,
    close_w: f64,
    close_inset: f64,
    max_w: f64,
    search_w: f64,
) -> (Rect, Rect, Rect) {
    if is_pinned {
        return (Rect::ZERO, Rect::ZERO, Rect::ZERO);
    }
    // The action chips' rects double as their pointer targets, so they run
    // taller than the 14 px glyph: `ACTION_HIT_INSET_Y` from either pill
    // edge. `draw_icon` centres the glyph within, so the three chips share
    // the pill's centre line whatever their rects' heights.
    let chip_y0 = r.y0 + ACTION_HIT_INSET_Y;
    let chip_y1 = r.y1 - ACTION_HIT_INSET_Y;
    let close_x0 = r.x1 - close_w - close_inset;
    // One pitch between neighbouring glyph centres. At that pitch the
    // maximize glyph already sits outside the 32 px close target (asserted
    // at compile time beside `CHIP_CLUSTER_W`), so the target cannot
    // swallow a visible maximize/search button.
    let close_cx = close_x0 + close_w / 2.0;
    let max_x0 = close_cx - CHIP_PITCH - max_w / 2.0;
    let search_x0 = close_cx - 2.0 * CHIP_PITCH - search_w / 2.0;
    let close = if effective >= CLOSE_HIT_PX {
        // The close glyph remains an 18px visual, centred in the larger pill.
        // Its 32px pointer target is derived separately in `apply_layout`.
        let close_glyph_y0 = r.center().y - close_w / 2.0;
        Rect::new(
            close_x0,
            close_glyph_y0,
            close_x0 + close_w,
            close_glyph_y0 + close_w,
        )
    } else {
        Rect::ZERO
    };
    let max_rect = if effective >= MAX_CHIP_REACH {
        Rect::new(max_x0, chip_y0, max_x0 + max_w, chip_y1)
    } else {
        Rect::ZERO
    };
    let search_rect = if effective >= CHIP_CLUSTER_W {
        Rect::new(search_x0, chip_y0, search_x0 + search_w, chip_y1)
    } else {
        Rect::ZERO
    };
    (close, max_rect, search_rect)
}

/// The style of a tab's title and of a window's title: Body, so a window's
/// name reads at the size of the text in it rather than a size smaller.
fn tab_title_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Body)
}

/// Where the × glyph is drawn inside its close chip rect.
fn close_glyph_box(close: Rect) -> Rect {
    square_at(close.center(), CLOSE_GLYPH_PX)
}

/// A `side`-px square centred on `c`.
fn square_at(c: Point, side: f64) -> Rect {
    Rect::new(
        c.x - side / 2.0,
        c.y - side / 2.0,
        c.x + side / 2.0,
        c.y + side / 2.0,
    )
}

/// Where an unpinned pill's leading glyph (app icon or accent dot) sits:
/// `ICON_W` square, starting on the pill's text inset and centred on the
/// pill's centre line. The title starts `ICON_GAP` after it — see
/// [`title_x0`].
/// What busy arcs show: a step of their turn, or the still ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BusyPose {
    Step(u64),
    Still,
}

/// How long a busy mark turns. Past this it is the still three-quarter
/// ring: a tab busy for long has said so, and an arc turning for minutes
/// keeps the whole screen repainting for nothing.
const BUSY_TURNING: Duration = Duration::from_secs(10);

/// The instant a busy mark's turn is measured from, shared by every strip so
/// two loading tabs turn together.
fn busy_epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(crate::time::now)
}

/// The presence key peers on a tab are registered under.
fn tab_presence_key(id: u64) -> PresenceKey {
    PresenceKey::Tab {
        id: format!("{id:x}"),
    }
}

/// Peer faces a pill shows: `faces` initials, then a "+`overflow`" disc
/// for everyone else (none when zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PillFaces {
    faces: usize,
    overflow: usize,
}

/// Sizes of a pill's peer faces, from the theme: each face is a medium icon
/// wide (large enough for a legible initial), keeps the smallest spacing
/// step from the title, and overlaps its neighbour by only half that step so
/// both initials stay whole.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FaceMetrics {
    face: f64,
    gap: f64,
    overlap: f64,
}

impl FaceMetrics {
    fn of(theme: &Theme) -> Self {
        let gap = f64::from(theme.tokens.spacing.s1);
        Self {
            face: f64::from(theme.tokens.metrics.icon_md),
            gap,
            // The ring alone overlaps the next face onto this one's edge,
            // never onto its initial.
            overlap: 0.0,
        }
    }

    fn step(self) -> f64 {
        self.face - self.overlap
    }
}

impl PillFaces {
    fn slots(self) -> usize {
        self.faces.saturating_add(usize::from(self.overflow > 0))
    }

    /// Width the faces take from the title zone, including their gap.
    fn width(self, metrics: FaceMetrics) -> f64 {
        match self.slots() {
            0 => 0.0,
            slots => {
                metrics.face
                    + crate::num::f64_of_usize(slots.saturating_sub(1)) * metrics.step()
                    + metrics.gap
            }
        }
    }

    /// The most faces for `peers` that still leave the title `keep` px of a
    /// `room`-wide zone; fewer faces fold into "+N".
    fn fit(peers: usize, room: f64, keep: f64, metrics: FaceMetrics) -> Self {
        (1..=peers.min(PILL_MAX_FACES))
            .rev()
            .map(|faces| Self {
                faces,
                overflow: peers.saturating_sub(faces),
            })
            .find(|candidate| room - candidate.width(metrics) >= keep)
            .unwrap_or_default()
    }
}

fn leading_glyph_rect(r: Rect) -> Rect {
    let x0 = r.x0 + PILL_PAD_X;
    Rect::new(
        x0,
        r.center().y - ICON_W / 2.0,
        x0 + ICON_W,
        r.center().y + ICON_W / 2.0,
    )
}

/// Left edge of a pill's title: the text inset, plus whatever the leading
/// glyph reserved (`ICON_W + ICON_GAP`, or nothing).
fn title_x0(r: Rect, leading_shift: f64) -> f64 {
    r.x0 + PILL_PAD_X + leading_shift
}

/// The two hover-reveal action chips a pill carries besides close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionChip {
    Maximize,
    Search,
    History,
}

/// The round a hovered action chip paints behind its glyph: `CLOSE_W`
/// square on the chip's centre, so it is the same disc the close chip
/// paints, not a capsule the height of the chip's pointer target.
fn chip_hover_ring(chip: Rect) -> Rect {
    square_at(chip.center(), CLOSE_W)
}

/// A hover wash for a chip, derived against the ground the bar stands on
/// the way a hairline is (HIG §9.5): white over a dark pill, black over a
/// light one. A literal white at low alpha was invisible on the light
/// palettes.
fn chip_hover_tint(ground: vello::peniko::Color, alpha: u8) -> vello::peniko::Color {
    Colors::hairline_on(ground, alpha)
}

impl Widget for TabBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion || self.host_reduced_motion;
        self.motion = EffectiveMotion::new(theme.tokens.motion, self.reduce_motion);
        self.min_pointer_target = f64::from(theme.tokens.metrics.min_pointer_target);
        // `TAB_BAR_HEIGHT` is a fixed 44px layout constant, well inside `f32`'s
        // exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "TAB_BAR_HEIGHT is a small fixed layout constant"
        )]
        let bar_height = self.bar_height() as f32;
        let style = Style {
            size: Size {
                width: Dimension::auto(),
                height: Dimension::length(bar_height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if self.window_controls {
            let tabbed = self.tabs.len() > 1;
            if tabbed != self.tabbed {
                self.tabbed = tabbed;
                self.tabbed_changed_at = Some(crate::time::now());
            }
            let history = self.history.contains_key(&self.active_id);
            self.controls =
                super::window_controls::WindowControls::lay_out(self.tab_row(), history);
        } else {
            self.controls = super::window_controls::WindowControls::default();
        }
        self.advance_changed = false;
        self.face_demand_stale = false;
        self.tab_rects.clear();
        self.close_rects.clear();
        self.close_hit_rects.clear();
        self.max_rects.clear();
        self.search_rects.clear();
        self.history_rects.clear();
        // Pre-size so the visual-order pass can write `tab_rects[data_idx]`
        // directly — data index stays the canonical key for every cache.
        self.tab_rects.resize(self.tabs.len(), Rect::ZERO);
        self.close_rects.resize(self.tabs.len(), Rect::ZERO);
        self.close_hit_rects.resize(self.tabs.len(), Rect::ZERO);
        self.max_rects.resize(self.tabs.len(), Rect::ZERO);
        self.search_rects.resize(self.tabs.len(), Rect::ZERO);
        self.history_rects.resize(self.tabs.len(), Rect::ZERO);
        self.reveal_px.clear();
        self.reveal_px
            .resize(self.tabs.len(), Reveal::None.reserve());
        self.mark_rects.clear();
        self.mark_rects.resize(self.tabs.len(), Rect::ZERO);

        self.settle_hover_intent();
        let (visual_order, widths, grow) = self.compute_pill_layout();

        let gap = PILL_GAP;
        let close_w = CLOSE_W;
        let max_w = ACTION_W;
        let search_w = ACTION_W;
        let close_inset = CLOSE_INSET;
        let pill_y0 = self.tab_row().y0 + TAB_PILL_INSET_Y;
        let pill_y1 = self.tab_row().y1 - TAB_PILL_INSET_Y;

        // Absorbs float drift only — `fit_pill_widths` already sized the row to
        // fit. Disabled with no width yet, where clamping would collapse to the origin.
        let row = self.pill_row();
        let right_limit = if row.width() > 0.0 {
            row.x1
        } else {
            f64::INFINITY
        };
        let mut x = row.x0;
        for (slot, &i) in visual_order.iter().enumerate() {
            let is_pinned = self.tabs.get(i).is_some_and(|t| self.icon_only(t.id));
            let w = widths.get(slot).copied().unwrap_or(0.0);
            let x1 = (x + w).min(right_limit);
            let r = Rect::new(x, pill_y0, x1.max(x), pill_y1);
            // This pill's reveal, capped by what's left after the title keeps
            // `MIN_TITLE_PX`. A chip is placed once the zone reaches it, so the
            // cluster slides out from the right edge: close, then max, then search.
            let effective = if is_pinned {
                0.0
            } else {
                let protect = self.shaped_title_width(i).min(MIN_TITLE_PX);
                let afford = (r.width() - PILL_PAD_X - self.leading_zone(i) - protect).max(0.0);
                let grow_px = grow.get(slot).copied().unwrap_or(0.0);
                (self.resting_reveal(i).reserve() + grow_px).min(afford)
            };
            let (close, max_rect, search_rect) = chip_rects(
                r,
                effective,
                is_pinned,
                close_w,
                close_inset,
                max_w,
                search_w,
            );
            // A window of one tab shows it as its title: the window's close
            // control closes it, so the title carries no × of its own.
            let close = if self.solo() { Rect::ZERO } else { close };
            let close_hit = if close.area() > 0.0 {
                let hit_w = CLOSE_HIT_PX.min(r.width());
                // The destructive target never sits on the painted maximize
                // glyph: `chip_rects` already places that glyph outside it,
                // and this keeps it so whatever the cluster's numbers.
                let x0 = if max_rect.area() > 0.0 {
                    (r.x1 - hit_w).max(max_rect.x1)
                } else {
                    r.x1 - hit_w
                };
                Rect::new(x0, r.y0, r.x1, r.y1)
            } else {
                Rect::ZERO
            };
            if let Some(slot) = self.reveal_px.get_mut(i) {
                *slot = effective;
            }
            if let Some(slot) = self.tab_rects.get_mut(i) {
                *slot = r;
            }
            // Every unpinned tab keeps the slot, so a mark set between
            // layouts lands (and hit-tests) where it will be painted.
            if let Some(slot) = self.mark_rects.get_mut(i)
                && !is_pinned
            {
                *slot = leading_glyph_rect(r);
            }
            if let Some(slot) = self.close_rects.get_mut(i) {
                *slot = close;
            }
            if let Some(slot) = self.close_hit_rects.get_mut(i) {
                *slot = close_hit;
            }
            if let Some(slot) = self.max_rects.get_mut(i) {
                *slot = max_rect;
            }
            // A window with a search control of its own shows no chip for it.
            let search_rect = if self.search_control.is_some() {
                Rect::ZERO
            } else {
                search_rect
            };
            if let Some(slot) = self.search_rects.get_mut(i) {
                *slot = search_rect;
            }
            let history_rect = self.history_chip_rect(i, search_rect, effective);
            if let Some(slot) = self.history_rects.get_mut(i) {
                *slot = history_rect;
            }
            x = r.x1 + gap;
        }
        // `x` is the strip's write head; the "+" chip is suppressed once pills
        // fill the bar.
        let plus_x1 = x + NEW_TAB_CHIP_PX;
        self.new_tab_rect = if self.new_tab && !self.solo() && plus_x1 <= row.x1 {
            Rect::new(x, pill_y0, plus_x1, pill_y1)
        } else {
            Rect::ZERO
        };
        self.lay_out_toolbar_row();
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.drag_lift_duration = theme.tokens.motion.durations.fast();
        self.busy_drawn =
            (!self.busy_since.is_empty()).then(|| self.busy_pose_at(crate::time::now()));
        // Guards one stale frame after a detach/insert mutates `tabs` outside the
        // layout pass; next `apply_layout` fixes it.
        if self.tab_rects.len() != self.tabs.len() {
            return;
        }
        self.face_metrics = FaceMetrics::of(theme);
        self.record_face_demand(presence);
        self.pill_faces = (0..self.tabs.len())
            .map(|i| self.fit_pill_faces(i, presence))
            .collect();
        // Only pass with a TextRenderer — shapes every title up front.
        self.visible_titles = (0..self.tabs.len())
            .map(|i| self.visible_title(i, tr, theme))
            .collect();

        // After a reorder/insert/remove, every surviving pill interpolates from
        // its previous rect to `tab_rects[i]` on the list change's move; a brand-new
        // tab (no snapshot) renders at target immediately.
        let layout_t = self.layout_anim_started.and_then(|t| {
            let elapsed = crate::time::now().saturating_duration_since(t);
            (elapsed < layout_anim())
                .then(|| tab_progress(ListChange::MOVE, Direction::Enter, elapsed))
        });

        // Stop `needs_animation_frame` pumping forever once every insert-fade elapses.
        let fade_window = insert_fade();
        self.inserted_at
            .retain(|_, started| started.elapsed() < fade_window);

        self.draw_pills(scene, transform, tr, theme, presence, layout_t, fade_window);
        // The tab list's focus ring comes and goes with keyboard focus: a
        // damage part around the active pill, not a repaint of the bar.
        let focus = crate::scene_parts::PartKey::new("ui.tab-focus", ());
        crate::scene_parts::part_drawn(scene, focus, |scene| {
            self.draw_keyboard_focus(scene, transform, theme)
        });
        self.draw_new_tab_chip(scene, transform, tr, theme);
        if self.window_controls {
            self.controls.paint(
                scene,
                transform,
                tr,
                theme,
                super::window_controls::ControlsState {
                    hovered: self.hover_control,
                    pressed: self.pressed_control,
                    maximized: self.maximized,
                    history_open: self
                        .history
                        .get(&self.active_id)
                        .is_some_and(|control| control.is_open()),
                },
            );
        }
        if self.window_controls {
            // A frame of a tab switch or of the 1↔2 tabs motion moves the
            // slots: follow them here, where every frame passes.
            if self.tabbed_in_flight() {
                let rect = self.toolbar_row_rect();
                self.toolbar_row.lay_out(rect);
            }
            self.toolbar_row
                .paint(scene, transform, tr, theme, presence);
        }
        self.draw_closing_tabs(scene, transform, tr, theme);
        self.draw_drop_indicator(scene, transform, theme);
        self.draw_drag_ghost(scene, transform, tr, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // An open menu of the slots (the overflow's, the suggestions) takes
        // the pointer over it first, and a press off it closes it.
        if self.toolbar_row.popup_open() && self.toolbar_row.popup_event(event, ctx) {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            return;
        }
        // The location field, while edited, has the keyboard and its own
        // pointer gestures (placing the caret, selecting).
        if self.toolbar_row.is_editing() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            let in_field = self.toolbar_row.slots().location.contains(self.last_cursor);
            let pointer = matches!(
                event,
                WindowEvent::MouseInput { .. } | WindowEvent::CursorMoved { .. }
            );
            if (!pointer || in_field) && self.toolbar_row.field_event(event, ctx) {
                if pointer {
                    *ctx.focus_target = Some(ctx.node_id);
                }
                return;
            }
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && event.logical_key == Key::Named(NamedKey::Escape)
            && self.cancel_drag()
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && self.handle_keyboard_navigation(&event.logical_key)
        {
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_moved(*position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::CursorLeft { .. } => {
                // Nothing can be aimed at from outside the surface.
                self.hover_idx = None;
                self.hover_control = None;
                self.pressed_control = None;
                self.toolbar_row.pointer_left();
                self.hover_close_idx = None;
                self.hover_max_idx = None;
                self.hover_search_idx = None;
                self.hover_history_idx = None;
                self.hover_intent.clear();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                // Works without first hovering to reveal the × — hosts actions
                // with no keyboard chord.
                let p = self.last_cursor;
                if let Some(idx) = self
                    .tab_rects
                    .iter()
                    .position(|r| r.area() > 0.0 && r.contains(p))
                    && let Some(t) = self.tabs.get(idx)
                {
                    self.pending_context_menu = Some((t.id, p));
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(ctx.handled);
                if *ctx.handled {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.finish_drag_release(ctx.handled);
            }
            _ => {}
        }
    }
    fn name(&self) -> &'static str {
        "TabBar"
    }

    /// Every button in the bar names its outcome: the slots' buttons and
    /// the window's controls.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if self.active_toolbar().is_some()
            && let Some(tip) = self.toolbar_row.tooltip_at(point)
        {
            return Some(tip);
        }
        if !self.window_controls {
            return None;
        }
        let control = self.controls.hit(point)?;
        let (rect, key) = match control {
            super::window_controls::WindowControl::History => {
                (self.controls.history, "standalone-history-title")
            }
            super::window_controls::WindowControl::Maximize => (
                self.controls.maximize,
                if self.maximized {
                    "host-window-restore"
                } else {
                    "host-window-maximize"
                },
            ),
            super::window_controls::WindowControl::Close => {
                (self.controls.close, "host-window-close")
            }
        };
        Some((rect, crate::i18n::I18nManager::lookup_plain(key)))
    }

    /// The slots' menus (overflow, suggestions) float in the window's top
    /// layer, over the content.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.toolbar_row
            .paint_popups(scene, transform, text_renderer, theme, presence);
    }
    fn test_id(&self) -> Option<&str> {
        Some("tabs")
    }

    /// So a scenario clicks the rect the renderer actually painted, not a
    /// guessed coordinate. The zero-area case (chip suppressed) is filtered
    /// out by the registry.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut out = vec![("tabbar_new_tab".to_string(), self.new_tab_rect)];
        // By title, so a scenario can select "Files" by name instead of a
        // coordinate that moves with every layout change.
        for (i, (tab, rect)) in self.tabs.iter().zip(self.tab_rects.iter()).enumerate() {
            out.push((format!("tab:{}", tab.title), *rect));
            out.push((format!("window_tab:{i}"), *rect));
        }
        // The active tab's slots in the window's bar.
        if self.active_toolbar().is_some() {
            let slots = self.toolbar_row.slots();
            out.push(("window_back".to_string(), slots.back));
            out.push(("window_forward".to_string(), slots.forward));
            out.push(("window_location".to_string(), slots.location));
            out.push(("window_more".to_string(), slots.overflow));
            out.push(("window_sidebar".to_string(), slots.sidebar));
            for (i, rect) in slots.actions.iter().enumerate() {
                out.push((format!("window_action:{i}"), *rect));
            }
        }
        // The window's own controls, by what they do.
        out.extend([
            (WINDOW_HISTORY_TARGET.to_string(), self.controls.history),
            (WINDOW_MAXIMIZE_TARGET.to_string(), self.controls.maximize),
            (WINDOW_CLOSE_TARGET.to_string(), self.controls.close),
        ]);
        out
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Maps to an AT tablist: the *active* tab's title as `value`, each tab as
        // a synthetic `Tab` child (`TabDescriptor` data, not Widget children).
        // Titles are runtime strings, not FTL keys, so no `i18n!` wrap.
        let active_idx = self.tabs.iter().position(|t| t.id == self.active_id);
        let value = active_idx
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.title.clone());
        let mut node = AccessNode::new(Role::TabBar)
            .with_semantic_key(format!("tabbar:{}", self.group_id_tag))
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                ..Default::default()
            });
        // Surface the active tab's ordinal so the reader can say "2 of 4". Tab
        // counts never approach `usize::MAX`, so `saturating_add` never actually
        // saturates — it just keeps the ordinal math panic-free.
        if let Some(i) = active_idx {
            node = node.with_position(i.saturating_add(1), self.tabs.len());
        }
        let count = self.tabs.len();
        let mut tab_children: Vec<AccessNode> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let node = AccessNode::new(Role::Tab)
                    .with_action_target(format!("tab:{}", t.id))
                    .with_name(t.title.clone())
                    .with_state(AccessState {
                        selected: t.id == self.active_id,
                        ..Default::default()
                    })
                    .with_position(i.saturating_add(1), count);
                // The unsaved-changes dot is colour-and-shape only, so a reader
                // that can't see it gets it as a description instead.
                // `AccessState` has no flag for this and inventing one would
                // ripple through every widget.
                let node = if self.pinned_ids.contains(&t.id) || !self.closable {
                    node
                } else {
                    node.with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name(format!("Close {}", t.title))
                            .with_action_target(format!("tab-close:{}", t.id))
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            }),
                    ])
                };
                let mut node = if self.modified_ids.contains(&t.id) {
                    node.with_description(crate::i18n!("tab-modified"))
                } else {
                    node
                };
                // The pill's search and History chips, where its open cluster
                // shows them.
                let chips = self.closable && self.show_maximize && !self.pinned_ids.contains(&t.id);
                if chips {
                    node.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(crate::i18n::widget_format(
                                "tab-search-in",
                                &[("title", t.title.clone().into())],
                            ))
                            .with_action_target(format!("tab-search:{}", t.id))
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            }),
                    );
                }
                if let Some(control) = self.history.get(&t.id).filter(|_| chips) {
                    node.children.push(
                        AccessNode::new(Role::ToggleButton)
                            .with_name(crate::i18n::I18nManager::lookup_plain(
                                "standalone-history-title",
                            ))
                            .with_action_target(format!("tab-history:{}", t.id))
                            .with_state(AccessState {
                                focusable: true,
                                checked: Some(control.is_open()),
                                ..Default::default()
                            }),
                    );
                }
                match self.marks.get(&t.id) {
                    Some(mark) if mark.is_control() => {
                        let mut node = node.with_description(mark.description());
                        node.children.push(
                            AccessNode::new(Role::ToggleButton)
                                .with_name(format!("Mute {}", t.title))
                                .with_action_target(format!("tab-mute:{}", t.id))
                                .with_state(AccessState {
                                    focusable: true,
                                    checked: Some(*mark == TabMark::Muted),
                                    ..Default::default()
                                }),
                        );
                        node
                    }
                    Some(mark) => node.with_description(mark.description()),
                    None => node,
                }
            })
            .collect();
        if self.new_tab {
            tab_children.push(
                AccessNode::new(Role::Button)
                    .with_name("New tab")
                    .with_action_target("tab-new")
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            );
        }
        // The window's own controls, acting on the active tab.
        if self.window_controls {
            let button = |role: Role, name: String, target: &str| {
                AccessNode::new(role)
                    .with_name(name)
                    .with_action_target(target)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            };
            if let Some(control) = self.history.get(&self.active_id) {
                let mut history = button(
                    Role::ToggleButton,
                    crate::i18n::I18nManager::lookup_plain("standalone-history-title"),
                    WINDOW_HISTORY_TARGET,
                );
                history.state.checked = Some(control.is_open());
                tab_children.push(history);
            }
            tab_children.push(button(
                Role::Button,
                crate::i18n::I18nManager::lookup_plain(if self.maximized {
                    "host-window-restore"
                } else {
                    "host-window-maximize"
                }),
                WINDOW_MAXIMIZE_TARGET,
            ));
            tab_children.push(button(
                Role::Button,
                crate::i18n::I18nManager::lookup_plain("host-window-close"),
                WINDOW_CLOSE_TARGET,
            ));
            // The active tab's slots, before the window's controls in
            // reading order.
            let slots = self.toolbar_row.accessibility();
            if !slots.is_empty() {
                let at = tab_children.len().saturating_sub(
                    if self.history.contains_key(&self.active_id) {
                        3
                    } else {
                        2
                    },
                );
                for (offset, slot) in slots.into_iter().enumerate() {
                    tab_children.insert(at + offset, slot);
                }
            }
        }
        node = node.with_children(tab_children);
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        if target == "tab-new" {
            if !self.new_tab {
                return false;
            }
            self.pending_new_tab = Some(());
            return true;
        }
        if self.window_controls && self.toolbar_row.perform(target) {
            return true;
        }
        if self.window_controls {
            let active = self.active_id;
            match target {
                WINDOW_CLOSE_TARGET => return self.start_closing(active),
                WINDOW_MAXIMIZE_TARGET => {
                    self.pending_max = Some(active);
                    return true;
                }
                WINDOW_HISTORY_TARGET => return self.press_history(active),
                _ => {}
            }
        }
        if let Some(id) = target
            .strip_prefix("tab-close:")
            .and_then(|value| value.parse::<u64>().ok())
        {
            return self.closable && !self.pinned_ids.contains(&id) && self.start_closing(id);
        }
        if let Some(id) = target
            .strip_prefix("tab-search:")
            .and_then(|value| value.parse::<u64>().ok())
        {
            if self.pinned_ids.contains(&id) || !self.tabs.iter().any(|tab| tab.id == id) {
                return false;
            }
            self.pending_search = Some(id);
            return true;
        }
        if let Some(id) = target
            .strip_prefix("tab-history:")
            .and_then(|value| value.parse::<u64>().ok())
        {
            return self.press_history(id);
        }
        if let Some(id) = target
            .strip_prefix("tab-mute:")
            .and_then(|value| value.parse::<u64>().ok())
        {
            let Some(mark) = self.marks.get(&id).copied().filter(|m| m.is_control()) else {
                return false;
            };
            self.pending_mark = Some((id, mark));
            return true;
        }
        let Some(id) = target
            .strip_prefix("tab:")
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return false;
        };
        if !matches!(action, crate::a11y::Action::Activate)
            || !self.tabs.iter().any(|tab| tab.id == id)
        {
            return false;
        }
        self.active_id = id;
        self.pending_activation = Some(id);
        true
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
    /// A fixed-height strip sized by its parent: only its first layout is a
    /// size change. Tab additions and removals are structural; peers arriving
    /// on or leaving a tab resize its pill for their faces.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
            || self.advance_changed
            || self.face_demand_stale
            || (self.rect.height() - self.bar_height()).abs() > 0.5
    }
    fn is_focusable(&self) -> bool {
        !self.tabs.is_empty()
    }
    fn set_focus(&mut self, focused: bool) {
        // The location field keeps editing when the host moves its focus
        // (a click on the window's bar activates the window): the shell
        // routes the keyboard to it while it edits, and Esc, Enter or a
        // press elsewhere ends it.
        self.keyboard_focus = focused;
    }
    fn is_focused(&self) -> bool {
        self.keyboard_focus
    }
    fn wants_pointer_capture(&self) -> bool {
        self.press_origin.is_some() || self.pending_drag.is_some()
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        // The busy mark stands still and a drag lifts at once.
        self.host_reduced_motion = reduced;
        self.reduce_motion = reduced;
        for child in self.children_mut() {
            child.set_motion_reduced(reduced);
        }
    }

    /// A dwell or an aim hold still to run out is a wait, not motion: the
    /// host sleeps until it is due and [`Widget::update`] settles it.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = crate::time::now();
        self.hover_intent
            .deadline(now)
            .into_iter()
            .chain(self.busy_deadline(now))
            .min()
    }

    /// A dwell or aim hold that came due opens (or hands over) its pill;
    /// the relayout this asks for starts the reveal.
    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let before = self.hover_intent.open();
        self.settle_hover_intent();
        // A busy arc's step (or its coming to rest) is due: paint it.
        let now = crate::time::now();
        let busy_step = self
            .busy_drawn
            .is_some_and(|drawn| drawn != self.busy_pose_at(now));
        self.hover_intent.open() != before || busy_step
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep pumping redraws while any curve is in flight: layout spring,
        // insert fade, close fade, drop-line fade-in, or reveal.
        let layout = self
            .layout_anim_started
            .is_some_and(|t| t.elapsed() < layout_anim());
        let insert_fade = self
            .inserted_at
            .values()
            .any(|t| t.elapsed() < insert_fade());
        let close_fade = self
            .closing_tabs
            .iter()
            .any(|c| c.started_at.elapsed() < close_fade());
        let hover_drop_fade = self
            .hover_drop_animated_at
            .is_some_and(|t| t.elapsed() < hover_drop_fade());
        // Keeps the curve running once the pointer stops moving, until an
        // opening pill finishes.
        let reveal = self.reveal_in_flight();
        let drag_lift = !self.reduce_motion
            && self.pending_drag.is_some()
            && self
                .drag_lifted_at
                .is_some_and(|started| started.elapsed() < self.drag_lift_duration);
        // Held still, a pulled tab only moves with the pointer: only its
        // spring back runs by itself.
        let resisting =
            self.resist.is_some_and(|r| r.released_at.is_some()) && self.resist_offset().is_some();
        layout
            || insert_fade
            || close_fade
            || hover_drop_fade
            || reveal
            || drag_lift
            || resisting
            || self.tabbed_in_flight()
            || self.toolbar_row.needs_animation_frame(crate::time::now())
    }
}

impl TabBar {
    /// Visual-slot ordering (pinned first) plus each slot's fitted width and
    /// in-flight reveal growth — the inputs `apply_layout`'s placement loop turns
    /// into rects. Split out so `apply_layout` itself stays inside the
    /// `too_many_lines` budget.
    fn compute_pill_layout(&mut self) -> (Vec<usize>, Vec<f64>, Vec<f64>) {
        // The only place the reveal curve advances, so rects and paint stay in step.
        let live: HashSet<u64> = self.tabs.iter().map(|t| t.id).collect();
        self.reveal_anim.retain(|id, _| live.contains(id));
        // Owned copy of the ids, not a borrow of `self.tabs`, so the loop below can
        // still call the `&mut self` `advance_reveal`.
        let tab_ids: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        let mut animated: Vec<f64> = Vec::with_capacity(tab_ids.len());
        for (i, &id) in tab_ids.iter().enumerate() {
            let target = self.reserve_of(i, self.target_reveal(i));
            animated.push(self.advance_reveal(id, target));
        }
        // Visual-slot → data-index: pinned first, then unpinned. `self.tabs`
        // stays untouched so caches keyed by data index still apply.
        let mut visual_order: Vec<usize> = Vec::with_capacity(self.tabs.len());
        for (i, t) in self.tabs.iter().enumerate() {
            if self.icon_only(t.id) {
                visual_order.push(i);
            }
        }
        for (i, t) in self.tabs.iter().enumerate() {
            if !self.icon_only(t.id) {
                visual_order.push(i);
            }
        }
        let pinned_flags: Vec<bool> = visual_order
            .iter()
            .map(|&i| self.tabs.get(i).is_some_and(|t| self.icon_only(t.id)))
            .collect();
        // Fit at resting widths (pointer elsewhere) — see [`Reveal`] for why.
        let resting: Vec<f64> = visual_order
            .iter()
            .map(|&i| self.natural_pill_width(i, self.resting_reveal(i).reserve()))
            .collect();
        // The widest pill's opening: a pill with History opens one chip wider.
        let headroom = visual_order
            .iter()
            .zip(&pinned_flags)
            .filter(|(_, pinned)| !**pinned)
            .map(|(&i, _)| self.reserve_of(i, Reveal::Full) - Reveal::None.reserve())
            .fold(0.0, f64::max);
        let row = self.pill_row();
        let laid_out = row.width() > 0.0;
        let fit_avail = if laid_out {
            (row.width() - headroom - NEW_TAB_CHIP_PX - PILL_GAP).max(1.0)
        } else {
            row.width()
        };
        let base = fit_pill_widths(&resting, &pinned_flags, fit_avail);
        // Growth beyond resting, scaled down together if two pills are briefly
        // part-open mid-handover so the total can't outrun the one-pill headroom.
        let mut grow: Vec<f64> = visual_order
            .iter()
            .map(|&i| {
                let a = animated.get(i).copied().unwrap_or(0.0);
                (a - self.resting_reveal(i).reserve()).max(0.0)
            })
            .collect();
        let total_grow: f64 = grow.iter().sum();
        if total_grow > headroom && total_grow > 0.0 {
            let k = headroom / total_grow;
            for g in &mut grow {
                *g *= k;
            }
        }
        let widths: Vec<f64> = base.iter().zip(grow.iter()).map(|(b, g)| b + g).collect();
        (visual_order, widths, grow)
    }

    /// One row of tab pills, each springing from its previous layout rect and
    /// fading in if freshly inserted or dimming if it's the drag source. Split
    /// out of `draw` to keep that dispatcher inside the `too_many_lines` budget.
    // Every parameter here is state `draw` already resolved once per frame — no
    // hidden refetching.
    fn draw_pills(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        layout_t: Option<f64>,
        fade_window: Duration,
    ) {
        // `None` if no drag, or the tab was removed mid-drag (avoids a panic).
        let drag_source_idx: Option<usize> = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => self.tabs.iter().position(|t| t.id == *tab),
            _ => None,
        };

        for (i, (t, &target)) in self.tabs.iter().zip(self.tab_rects.iter()).enumerate() {
            // prev → target lerp when a spring is in flight; otherwise render at target.
            let r = match (layout_t, self.prev_tab_rects_by_id.get(&t.id)) {
                (Some(tt), Some(prev)) => Rect::new(
                    prev.x0 + (target.x0 - prev.x0) * tt,
                    prev.y0 + (target.y0 - prev.y0) * tt,
                    prev.x1 + (target.x1 - prev.x1) * tt,
                    prev.y1 + (target.y1 - prev.y1) * tt,
                ),
                _ => target,
            };

            // Fade alpha 0→1 and y -10→0 on the list change's fade; `fade_alpha` < 1.0
            // wraps the paint in a `push_layer`.
            let (fade_alpha, fade_y): (f32, f64) = match self.inserted_at.get(&t.id) {
                Some(started) => {
                    let elapsed = started.elapsed();
                    if elapsed >= fade_window {
                        (1.0, 0.0)
                    } else {
                        let eased = tab_progress(ListChange::FADE, Direction::Enter, elapsed);
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "ease_out output stays in 0.0..=1.0"
                        )]
                        let eased_f32 = eased as f32;
                        (eased_f32, -INSERT_FADE_Y_OFFSET * (1.0 - eased))
                    }
                }
                None => (1.0, 0.0),
            };

            // Dim the in-place pill while dragged, so the slot reads as "pulled
            // out"; multiplies with `fade_alpha` so insert-fade mid-drag composes.
            let source_dim = if drag_source_idx == Some(i) {
                Self::SOURCE_PILL_DIM_ALPHA
            } else {
                1.0
            };
            let combined_alpha = fade_alpha * source_dim;

            let pull = self
                .resist_offset()
                .filter(|(tab, _)| *tab == t.id)
                .map_or(vello::kurbo::Vec2::ZERO, |(_, pull)| pull);
            let pill_transform = if fade_y == 0.0 && pull == vello::kurbo::Vec2::ZERO {
                transform
            } else {
                compose(transform, Affine::translate((pull.x, fade_y + pull.y)))
            };
            let pill_alpha_active = combined_alpha < 1.0;
            // Wider than the pill so hover-reveal action icons fade in cleanly.
            let close_w = self.close_rects.get(i).map_or(0.0, Rect::width);
            let clip = Rect::new(r.x0 - 2.0, r.y0 - 4.0, r.x1 + close_w + 60.0, r.y1 + 4.0);
            // Each pill is a damage part: hovering one repaints that pill.
            // It paints inside the clip a fading pill is held to, at its
            // fade offset; a press only shrinks it.
            let bounds = vec![
                transform.transform_rect_bbox(clip),
                pill_transform.transform_rect_bbox(clip),
            ];
            let key = crate::scene_parts::PartKey::new("ui.tab-pill", t.id);
            crate::scene_parts::part(scene, key, bounds, |scene| {
                if pill_alpha_active {
                    scene.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        combined_alpha,
                        transform,
                        &clip,
                    );
                }
                // Uses `pill_transform` so the fade's y-offset composes with any
                // layout-spring dx/dy.
                self.draw_pill_inner(
                    &mut PaintCtx::new(scene, pill_transform, tr, theme),
                    presence,
                    i,
                    r,
                    false,
                );

                // Must mirror the push above exactly — `pill_alpha_active` tracks that.
                if pill_alpha_active {
                    scene.pop_layer();
                }
            });
        }
    }

    /// A focus ring belongs on the active pill, not the whole row: selection
    /// remains the active pill's glass/keyline treatment, while this outline
    /// says that the *tab list* now owns keyboard input. The semantic focus
    /// and the painted target therefore stay in lockstep under high contrast
    /// and large text alike. Returns the bounds of what it painted, in scene
    /// coordinates.
    fn draw_keyboard_focus(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) -> Vec<Rect> {
        // While the location is edited the field shows where typing goes;
        // and a click that focused the tabs rings nothing (focus-visible).
        if !self.keyboard_focus || self.toolbar_row.is_editing() || !crate::focus_visible::shown() {
            return Vec::new();
        }
        let Some((idx, _)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.id == self.active_id)
        else {
            return Vec::new();
        };
        let Some(rect) = self
            .tab_rects
            .get(idx)
            .copied()
            .filter(|rect| rect.area() > 0.0)
        else {
            return Vec::new();
        };
        let ring_rect = rect.inset(1.0);
        let ring = RoundedRect::from_rect(
            ring_rect,
            f64::from(
                theme
                    .tokens
                    .radii
                    .concentric(theme.tokens.radii.control, 1.0),
            ),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            theme.tokens.colors.intent.accent_ink,
            None,
            &ring,
        );
        // Half the stroke outside the ring, and a pixel of antialiasing.
        vec![transform.transform_rect_bbox(ring_rect.inflate(2.0, 2.0))]
    }

    /// The idle/hover "+" new-tab chip, painted after the pills so a springing
    /// neighbour never draws over it; skipped during a drag so it can't be
    /// mistaken for a drop target.
    fn draw_new_tab_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !(self.new_tab_rect.area() > 0.0 && self.pending_drag.is_none()) {
            return;
        }
        // The shared icon button: bare at rest so it never competes with the
        // active tab, a lift of the bar under the pointer.
        let face =
            crate::widget::ButtonFace::icon_only(self.new_tab_rect, icons::Icon::Plus, "New tab")
                .control_size(crate::widget::ControlSize::Small)
                .hovered(self.hover_new_tab);
        crate::widget::Button::paint(scene, transform, tr, theme, &face);
    }

    /// Pills mid close-fade, retained past the fade window (at alpha 0) until
    /// `WindowGroup::sweep_closed_hosts` cleans them up. Drawn after the main
    /// loop so a closing pill sits above neighbours springing into its slot.
    fn draw_closing_tabs(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.closing_tabs.is_empty() {
            return;
        }
        let ground = self.ground(theme);
        let idle = self.idle_ink(theme);
        let close_window = close_fade();
        for c in &self.closing_tabs {
            let elapsed = c.started_at.elapsed();
            if elapsed >= close_window {
                continue;
            }
            let eased = tab_progress(ListChange::FADE, Direction::Exit, elapsed);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "1.0 - eased stays in 0.0..=1.0"
            )]
            let alpha = (1.0 - eased) as f32;
            let scale = 1.0 + (CLOSE_SCALE_END - 1.0) * eased;
            let y_offset = CLOSE_Y_OFFSET_END * eased;
            // Wide clip so the drop shadow + hover-reveal icons aren't chopped.
            let clip = Rect::new(
                c.pill_rect.x0 - 20.0,
                c.pill_rect.y0 - 20.0,
                c.pill_rect.x1 + 80.0,
                c.pill_rect.y1 + 40.0,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha.max(0.0),
                transform,
                &clip,
            );
            let centre = c.pill_rect.center();
            let close_transform = compose(
                compose(
                    compose(
                        compose(transform, Affine::translate((0.0, y_offset))),
                        Affine::translate((centre.x, centre.y)),
                    ),
                    Affine::scale(scale),
                ),
                Affine::translate((-centre.x, -centre.y)),
            );
            Self::draw_closing_pill(scene, close_transform, c, tr, (theme, ground, idle));
            scene.pop_layer();
        }
    }

    /// Drop-indicator line at `hover_drop_idx`, serving both the single-bar
    /// case and the cross-group case (where `pending_drag.is_some()` is false
    /// on sibling bars). Eased fade-in on the list change's fade.
    fn draw_drop_indicator(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(gap_idx) = self.hover_drop_idx else {
            return;
        };
        if self.tab_rects.is_empty() {
            return;
        }
        let now = crate::time::now();
        let eased_t = self.hover_drop_animated_at.map_or(1.0, |t| {
            tab_progress(
                ListChange::FADE,
                Direction::Enter,
                now.saturating_duration_since(t),
            )
        });
        // 4px above/below the pill's extent for bleed past the rounded caps.
        let (cx, y0, y1) = if gap_idx == 0 {
            self.tab_rects
                .first()
                .map_or((0.0, 0.0, 0.0), |r| (r.x0 - 3.0, r.y0 - 4.0, r.y1 + 4.0))
        } else if gap_idx >= self.tab_rects.len() {
            self.tab_rects
                .last()
                .map_or((0.0, 0.0, 0.0), |r| (r.x1 + 3.0, r.y0 - 4.0, r.y1 + 4.0))
        } else {
            let prev = self.tab_rects.get(gap_idx.saturating_sub(1));
            let next = self.tab_rects.get(gap_idx);
            match (prev, next) {
                (Some(prev), Some(next)) => {
                    let cx = f64::midpoint(prev.x1, next.x0);
                    (cx, prev.y0.min(next.y0) - 4.0, prev.y1.max(next.y1) + 4.0)
                }
                _ => (0.0, 0.0, 0.0),
            }
        };
        let half_w = HOVER_DROP_LINE_PX / 2.0;
        let line = Rect::new(cx - half_w, y0, cx + half_w, y1);
        // Derived against the bar's ground at 85% terminal alpha (HIG §9.5):
        // crisp but restrained, and it survives a light palette.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "eased_t stays in 0.0..=1.0"
        )]
        let alpha = (0.85 * eased_t as f32).clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "alpha is clamped to 0.0..=1.0 before scaling into a u8 channel"
        )]
        let alpha_u8 = (alpha * 255.0) as u8;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            Colors::hairline_on(self.ground(theme), alpha_u8),
            None,
            &line,
        );
    }

    /// The dragged pill's cursor-following ghost, painted LAST so it composites
    /// over everything. Not clipped to `self.rect`: cross-group drag handoff
    /// (`GroupRow::handle_event`) reads the raw cursor position, not the ghost.
    /// Hit-tests still use the original `tab_rects`.
    pub(crate) fn draw_drag_ghost(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(ghost_rect) = self.compute_ghost_rect() else {
            return;
        };
        let Some(source_idx) = (match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => self.tabs.iter().position(|t| t.id == *tab),
            _ => None,
        }) else {
            return;
        };
        // Generous so the ghost's drop shadow isn't clipped.
        let clip = Rect::new(
            ghost_rect.x0 - 20.0,
            ghost_rect.y0 - 20.0,
            ghost_rect.x1 + 20.0,
            ghost_rect.y1 + 40.0,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            if theme.colors_are_forced() {
                1.0
            } else {
                Self::GHOST_PILL_ALPHA
            },
            transform,
            &clip,
        );
        self.draw_pill_inner(
            &mut PaintCtx::new(scene, transform, tr, theme),
            presence,
            source_idx,
            ghost_rect,
            true,
        );
        scene.pop_layer();
    }

    /// `CursorMoved` handling: hover state, drag-start detection and the
    /// in-flight drop-zone target. Split out of `handle_event` to keep that
    /// dispatcher inside the `too_many_lines` budget.
    fn on_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor = p;
        self.toolbar_row.pointer_moved(p);
        self.hover_control = self.controls.hit(p);
        if self
            .pressed_control
            .is_some_and(|c| Some(c) != self.hover_control)
        {
            self.pressed_control = None;
        }
        // Pinned pills leave their close/max/search rects at `Rect::ZERO`;
        // skip them so a cursor crossing (0,0) doesn't latch a hover on a
        // glyph that isn't painted.
        let icon_only: Vec<bool> = self.tabs.iter().map(|t| self.icon_only(t.id)).collect();
        let is_pinned_at = |i: usize| icon_only.get(i).copied().unwrap_or(false);
        // The red wash follows the painted glyph. Presses, and the hover
        // intent (which never opens a pill from its ×), use the forgiving
        // 32 px target from `clickable_controls`.
        let is_held_at = |i: usize| self.tabs.get(i).is_some_and(|t| self.is_held(t.id));
        self.hover_close_idx = self
            .close_rects
            .iter()
            .position(|r| r.area() > 0.0 && r.contains(p))
            .filter(|&i| !is_pinned_at(i) && !is_held_at(i));
        // One point lights one chip: the close target first, then a painted
        // action glyph, then the enlarged action targets by priority — the
        // same resolution the press uses.
        let action = if self.hover_close_idx.is_some() {
            None
        } else {
            self.action_chip_at(p).filter(|&(i, _)| !is_pinned_at(i))
        };
        self.hover_max_idx = action
            .filter(|&(_, chip)| chip == ActionChip::Maximize)
            .map(|(i, _)| i);
        self.hover_search_idx = action
            .filter(|&(_, chip)| chip == ActionChip::Search)
            .map(|(i, _)| i);
        self.hover_history_idx = action
            .filter(|&(_, chip)| chip == ActionChip::History)
            .map(|(i, _)| i);
        self.hover_idx = self.tab_rects.iter().position(|r| r.contains(p));
        // Opening a pill grows it rightward, carrying its × and everything
        // after it along; the intent keeps that from happening under a
        // pointer that is on, or heading for, any of those controls.
        let controls = self.clickable_controls();
        let sample = IntentSample {
            p,
            under: self.hover_idx.and_then(|i| self.intent_item(i)),
            open: self.hover_intent.open().and_then(|i| self.intent_item(i)),
            controls: &controls,
        };
        self.hover_intent.on_motion(&sample, crate::time::now());
        self.hover_new_tab = self.new_tab_rect.area() > 0.0 && self.new_tab_rect.contains(p);
        if let Some(resist) = self.resist.as_mut().filter(|r| r.released_at.is_none()) {
            // Someone else's tab follows the drag a little and no further.
            let pull = |d: f64| {
                let reach = f64::from(crate::tokens::rubber_band(
                    crate::num::f32_of_f64(d.abs()),
                    RESIST_REACH,
                ));
                reach.copysign(d)
            };
            resist.pull = vello::kurbo::Vec2::new(
                pull(p.x - resist.origin.x),
                pull(p.y - resist.origin.y) / 2.0,
            );
            *handled = true;
        } else if let Some((idx, origin)) = self.press_origin
            && point_delta(p, origin).hypot() > DRAG_START_THRESHOLD_PX
            && let Some(t) = self.tabs.get(idx)
        {
            let group_tag_hash = self
                .group_id_tag
                .bytes()
                .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(b)));
            if self.pending_drag.is_none() {
                self.drag_lifted_at = Some(crate::time::now());
                self.pending_drag = Some(DragPayload::Tab {
                    group: group_tag_hash,
                    tab: t.id,
                });
            }
            *handled = true;
        }
        // Inside the bar → gap index from `drop_index_for`; outside → clear
        // (cross-group release is handled by `GroupRow`).
        if self.pending_drag.is_some() {
            let new_target = if self.rect.contains(p) {
                Some(self.drop_index_for(p))
            } else {
                None
            };
            self.drop_zone_target = new_target;
            // Same gap drives the white-line indicator; cross-group hovers
            // reach it via `set_external_drag_hover`.
            if new_target != self.hover_drop_idx {
                self.hover_drop_idx = new_target;
                if new_target.is_some() {
                    self.hover_drop_animated_at = Some(crate::time::now());
                } else {
                    self.hover_drop_animated_at = None;
                }
            }
        }
    }

    /// Left-button press: fresh hit-tests against authoritative rects, not the
    /// `CursorMoved` hover cache, which can lag a click that arrives before a
    /// hover-refresh — otherwise the icon hit is swallowed and the tab just
    /// activates. Split out of `handle_event` to keep that dispatcher inside
    /// the `too_many_lines` budget.
    fn on_left_press(&mut self, handled: &mut bool) {
        let cursor_pos = self.last_cursor;
        if let Some(control) = self.controls.hit(cursor_pos) {
            self.toolbar_row.cancel_edit();
            self.pressed_control = Some(control);
            *handled = true;
            return;
        }
        if self.active_toolbar().is_some() && self.toolbar_row.press(cursor_pos) {
            *handled = true;
            return;
        }
        let icon_only: Vec<bool> = self.tabs.iter().map(|t| self.icon_only(t.id)).collect();
        let is_pinned_at = |i: usize| icon_only.get(i).copied().unwrap_or(false);
        let close_idx = (0..self.tabs.len())
            .find(|&i| {
                self.close_hit_rect(i)
                    .is_some_and(|r| r.contains(cursor_pos))
            })
            .filter(|&i| !is_pinned_at(i))
            .filter(|&i| self.tabs.get(i).is_none_or(|t| !self.is_held(t.id)));
        let action = if close_idx.is_some() {
            None
        } else {
            self.action_chip_at(cursor_pos)
                .filter(|&(i, _)| !is_pinned_at(i))
        };
        let max_idx = action
            .filter(|&(_, chip)| chip == ActionChip::Maximize)
            .map(|(i, _)| i);
        let search_idx = action
            .filter(|&(_, chip)| chip == ActionChip::Search)
            .map(|(i, _)| i);
        let history_idx = action
            .filter(|&(_, chip)| chip == ActionChip::History)
            .map(|(i, _)| i);

        // Sits in the bar's trailing space, also the whole-group drag-grab
        // zone, so it must win the press first.
        if self.new_tab_rect.area() > 0.0 && self.new_tab_rect.contains(cursor_pos) {
            self.pending_new_tab = Some(());
            *handled = true;
            return;
        }
        // A sound mark is a control of its own: it mutes, it does not select.
        let mark_hit = self.tabs.iter().zip(&self.mark_rects).find_map(|(t, r)| {
            let mark = self.marks.get(&t.id).copied()?;
            (mark.is_control() && r.area() > 0.0 && r.inflate(2.0, 2.0).contains(cursor_pos))
                .then_some((t.id, mark))
        });
        if let Some(hit) = mark_hit {
            self.pending_mark = Some(hit);
            *handled = true;
            return;
        }

        // Close button takes priority over selection/drag.
        if let Some(ci) = close_idx
            && let Some(t) = self.tabs.get(ci)
        {
            let id = t.id;
            self.start_closing(id);
            *handled = true;
            return;
        }
        // Maximize icon — fires on first click regardless of whether
        // the tab is already active.
        if let Some(mi) = max_idx
            && let Some(t) = self.tabs.get(mi)
        {
            self.pending_max = Some(t.id);
            *handled = true;
            return;
        }
        // Search icon — same deal; shell drains `pending_search`.
        if let Some(si) = search_idx
            && let Some(t) = self.tabs.get(si)
        {
            // Deliberately does NOT activate the tab — chip
            // hits never bleed into selection, same as close
            // and maximize.
            self.pending_search = Some(t.id);
            *handled = true;
            return;
        }
        // History opens on the window it names: unlike the other chips it
        // selects its tab, so the History it opens is the one shown.
        if let Some(id) = history_idx.and_then(|hi| self.tabs.get(hi)).map(|t| t.id)
            && self.press_history(id)
        {
            *handled = true;
            return;
        }
        if let Some(idx) = self.hover_idx {
            // Can go stale if a neighbouring widget shrunk `tabs`/`tab_rects`
            // between move and press; clear so the next move repopulates it.
            let Some(pill) = self.tab_rects.get(idx).copied() else {
                self.hover_idx = None;
                return;
            };
            let origin = pill.center();
            self.press_origin = Some((idx, origin));
            if let Some(tab) = self.tabs.get(idx).filter(|t| self.is_held(t.id)) {
                self.resist = Some(Resist {
                    tab: tab.id,
                    origin: cursor_pos,
                    pull: vello::kurbo::Vec2::ZERO,
                    released_at: None,
                });
            }
            self.drag_offset = Some(vello::kurbo::Vec2::new(
                self.last_cursor.x - pill.x0,
                self.last_cursor.y - pill.y0,
            ));
            // Guard against `tabs`/`tab_rects` desyncing mid-frame.
            if let Some(tab) = self.tabs.get(idx) {
                self.active_id = tab.id;
                self.pending_activation = Some(tab.id);
            }
            *handled = true;
        }
    }
}

/// A window's application mark at any size: the app's own artwork when it
/// has some, else the app-kind glyph its tab pill wears, scaled up from the
/// pill's 14 px design size. Every surface that names a window by its app
/// (the tab pill, the window switcher, the overview) paints this one mark.
pub fn draw_app_mark(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Option<&icons::AppImage>,
    kind: AppKind,
    color: vello::peniko::Color,
) {
    if icon.is_some_and(|image| icons::draw_app_image(scene, transform, rect, image)) {
        return;
    }
    let side = rect.width().min(rect.height());
    if side <= 0.0 {
        return;
    }
    let scale = side / ICON_W;
    let glyph = transform
        .pre_translate(rect.center().to_vec2())
        .pre_scale(scale);
    draw_app_icon(scene, glyph, square_at(Point::ORIGIN, ICON_W), kind, color);
}

/// Approximates lucide icon shapes with kurbo primitives so we don't need an
/// icon-font codepoint; strokes are thin (1.1px) so glyphs read as chrome.
fn draw_app_icon(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    kind: AppKind,
    color: vello::peniko::Color,
) {
    let stroke = vello::kurbo::Stroke::new(1.1);
    let cx = rect.center().x;
    let cy = rect.center().y;
    match kind {
        AppKind::Terminal => {
            // ">_" prompt; skips lucide `Terminal`'s outer frame so it reads at 14px.
            let chev = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 4.0, cy - 3.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 0.5, cy)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 4.0, cy + 3.0)),
            ]);
            scene.stroke(&stroke, transform, color, None, &chev);
            let under = vello::kurbo::Line::new(
                Point::new(cx + 0.5, cy + 3.0),
                Point::new(cx + 4.5, cy + 3.0),
            );
            scene.stroke(&stroke, transform, color, None, &under);
        }
        AppKind::Files => {
            // Lucide `Folder`; the top-left tab gives it a folder-y read at this size.
            let tab = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 5.0, cy - 3.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 2.0, cy - 3.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 0.5, cy - 1.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 5.0, cy - 1.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 5.0, cy + 4.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 5.0, cy + 4.0)),
                vello::kurbo::PathEl::ClosePath,
            ]);
            scene.stroke(&stroke, transform, color, None, &tab);
        }
        AppKind::Editor => {
            // Lucide `FileCode2`: document outline (corner fold) + a `<>` glyph.
            let doc = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 4.0, cy - 5.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 2.0, cy - 5.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 4.5, cy - 2.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 4.5, cy + 5.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 4.0, cy + 5.0)),
                vello::kurbo::PathEl::ClosePath,
            ]);
            scene.stroke(&stroke, transform, color, None, &doc);
            let chevs = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 1.5, cy - 0.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 3.0, cy + 1.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 1.5, cy + 2.5)),
                vello::kurbo::PathEl::MoveTo(Point::new(cx + 1.5, cy - 0.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 3.0, cy + 1.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 1.5, cy + 2.5)),
            ]);
            scene.stroke(&stroke, transform, color, None, &chevs);
        }
        AppKind::Browser => {
            // Lucide `Globe`, omitting the curved latitude lines — too small to read.
            let circle = vello::kurbo::Circle::new(Point::new(cx, cy), 5.5);
            scene.stroke(&stroke, transform, color, None, &circle);
            let h = vello::kurbo::Line::new(Point::new(cx - 5.5, cy), Point::new(cx + 5.5, cy));
            scene.stroke(&stroke, transform, color, None, &h);
            let v = vello::kurbo::Line::new(Point::new(cx, cy - 5.5), Point::new(cx, cy + 5.5));
            scene.stroke(&stroke, transform, color, None, &v);
        }
        AppKind::App(icon) => {
            icons::draw_icon(
                scene,
                transform,
                rect,
                icon,
                color,
                icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        AppKind::Settings => {
            // Lucide `Settings`; full gear teeth are too small to read at 14px.
            let outer = vello::kurbo::Circle::new(Point::new(cx, cy), 5.5);
            scene.stroke(&stroke, transform, color, None, &outer);
            let inner = vello::kurbo::Circle::new(Point::new(cx, cy), 2.0);
            scene.stroke(&stroke, transform, color, None, &inner);
            for (dx, dy) in [(0.0, -5.5), (0.0, 5.5), (-5.5, 0.0), (5.5, 0.0)] {
                let t = vello::kurbo::Line::new(
                    Point::new(cx + dx * 0.9, cy + dy * 0.9),
                    Point::new(cx + dx * 1.25, cy + dy * 1.25),
                );
                scene.stroke(&stroke, transform, color, None, &t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::user_color;
    use crate::testing::{Dispatch, dispatch};
    use automerge::AutoCommit;

    fn bar() -> TabBar {
        TabBar::new(
            "g1",
            vec![
                TabDescriptor::new(1, "A"),
                TabDescriptor::new(2, "B"),
                TabDescriptor::new(3, "C"),
            ],
            1,
        )
    }

    /// `bar()` with pre-laid-out rects (3 pills, each 80px wide) — the shared
    /// fixture for the drag-ghost tests, which drive `handle_event` directly
    /// rather than going through a real `apply_layout` pass.
    fn ghost_test_bar() -> TabBar {
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(80.0, 6.0, 160.0, 34.0),
            Rect::new(160.0, 6.0, 240.0, 34.0),
        ];
        b.close_rects = vec![
            Rect::new(60.0, 10.0, 78.0, 30.0),
            Rect::new(140.0, 10.0, 158.0, 30.0),
            Rect::new(220.0, 10.0, 238.0, 30.0),
        ];
        b.max_rects = vec![
            Rect::new(40.0, 10.0, 56.0, 30.0),
            Rect::new(120.0, 10.0, 136.0, 30.0),
            Rect::new(200.0, 10.0, 216.0, 30.0),
        ];
        b.search_rects = vec![
            Rect::new(20.0, 10.0, 36.0, 30.0),
            Rect::new(100.0, 10.0, 116.0, 30.0),
            Rect::new(180.0, 10.0, 196.0, 30.0),
        ];
        b
    }

    #[test]
    fn tab_titles_stay_distinguishable_elided_and_named_at_every_width() {
        // Case `compact_large_text_document_tabs_remain_distinguishable`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut bar = TabBar::new(
                "docs",
                vec![
                    TabDescriptor::new(1, "Document-000.txt"),
                    TabDescriptor::new(2, "Document-060.txt"),
                ],
                1,
            );
            lay_out_at_width(&mut bar, 350.0);
            let first = bar.visible_title(0, &mut tr, &theme).unwrap();
            let second = bar.visible_title(1, &mut tr, &theme).unwrap();
            assert_ne!(first, second, "document identity must survive narrow tabs");
            assert!(second.contains("060"), "{second}");
            let mut scene = Scene::new();
            bar.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            assert!(
                scene
                    .encoding()
                    .resources
                    .glyph_runs
                    .iter()
                    .all(|run| run.font_size <= tab_title_style(&theme).size)
            );
            assert!(
                scene
                    .encoding()
                    .resources
                    .glyph_runs
                    .iter()
                    .any(|run| (run.font_size - tab_title_style(&theme).size).abs() < 0.01),
                "tab title must retain the requested 200% font size"
            );
        }

        // Case `a_title_that_does_not_fit_is_elided_not_sliced`.
        // `visible_title` is the function `draw` renders through, so this asserts
        // the text that actually reaches the scene.
        {
            let mut tr = TextRenderer::new();
            let theme = crate::theme::Theme::dark();
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "file-explorer-preferences"),
                    TabDescriptor::new(3, "Terminal"),
                ],
                1,
            );
            lay_out_at_width(&mut b, 320.0);

            let long = b
                .visible_title(1, &mut tr, &theme)
                .expect("the long pill has a title to render");
            assert!(
                long.ends_with('\u{2026}'),
                "a title that outruns its pill must carry an ellipsis, got {long:?}",
            );
            assert!(
                "file-explorer-preferences".starts_with(long.trim_end_matches('\u{2026}')),
                "the elided title must be a prefix of the real one, got {long:?}",
            );
            // A bare ellipsis would satisfy the two checks above while telling the
            // reader nothing — the pill must still carry a readable stem.
            assert!(
                long.chars().count() >= 4,
                "the elided title must keep a readable stem, got {long:?}",
            );
            assert!(
                long.chars().count() < "file-explorer-preferences".chars().count(),
                "the elided title must be shorter than the real one, got {long:?}",
            );

            // And a title that does fit is left exactly alone.
            let mut roomy = TabBar::new("g2", vec![TabDescriptor::new(1, "Notes")], 1);
            lay_out_at_width(&mut roomy, 600.0);
            assert_eq!(
                roomy.visible_title(0, &mut tr, &theme).as_deref(),
                Some("Notes"),
                "a title with room to spare must not be touched",
            );
        }

        // Case `a_row_with_room_to_spare_clips_no_name`.
        // The hover headroom is subtracted from the bar before pills are fitted,
        // which is right when the row is tight and wrong when it is not.
        {
            let mut tr = TextRenderer::new();
            let theme = crate::theme::Theme::dark();
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                    TabDescriptor::new(3, "Terminal"),
                    TabDescriptor::new(4, "Document"),
                ],
                1,
            );
            // Default single-group desktop width.
            lay_out_at_width(&mut b, 1300.0);
            for i in 0..b.tabs().len() {
                let title = b.tabs()[i].title.clone();
                let shown = b
                    .visible_title(i, &mut tr, &theme)
                    .unwrap_or_else(|| panic!("pill {i} ({title}) renders no title"));
                assert_eq!(
                    shown, title,
                    "pill {i} shows {shown:?} but the row has room for {title:?}",
                );
            }
        }

        // Case `squeezed_pills_still_name_their_tabs`.
        {
            let mut tr = TextRenderer::new();
            let theme = crate::theme::Theme::dark();
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                    TabDescriptor::new(3, "Terminal"),
                    TabDescriptor::new(4, "Browser"),
                ],
                1,
            );
            let width = 460.0;
            lay_out_at_width(&mut b, width);

            let bar_rect = b.bar_rect();
            for i in 0..b.tabs().len() {
                let title = b.tabs()[i].title.clone();
                let shown = b
                    .visible_title(i, &mut tr, &theme)
                    .unwrap_or_else(|| panic!("pill {i} ({title}) renders no title at all"));
                let stem = shown.trim_end_matches('\u{2026}');
                assert!(
                    stem.chars().count() >= 3,
                    "pill {i} ({title}) renders {shown:?} — a squeezed pill must \
                 still carry enough of its name to be recognised",
                );
                assert!(
                    title.starts_with(stem),
                    "pill {i} renders {shown:?}, which is not a prefix of {title:?}",
                );
            }
            // The squeeze is what buys the titles their room, not an overflow.
            for (i, r) in b.tab_rects().iter().enumerate() {
                assert!(
                    r.x1 <= bar_rect.x1 + 0.5,
                    "pill {i} ends at {} past the group edge at {}",
                    r.x1,
                    bar_rect.x1,
                );
            }
        }
    }

    #[test]
    fn tab_list_mutations_pick_neighbours_and_clean_sidecars() {
        // Case `remove_tab_drops_it_and_picks_neighbour_when_active`.
        {
            let mut b = bar();
            b.set_active(2);
            assert_eq!(b.remove_tab(2), Some(1));
            assert_eq!(b.tabs().len(), 2);
            // Active fell through to the tab that slid into position `1` (C).
            assert_eq!(b.active_id(), 3);
        }

        // Case `remove_tab_at_end_picks_last_remaining`.
        {
            let mut b = bar();
            b.set_active(3);
            b.remove_tab(3);
            assert_eq!(b.active_id(), 2);
        }

        // Case `reorder_moves_tab_to_requested_index`.
        {
            let mut b = bar();
            b.reorder(0, 2);
            let ids: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
            assert_eq!(ids, vec![2, 3, 1]);
        }

        // Case `pin_sidecar_tracks_ids_and_remove_tab_cleans_up`.
        {
            let mut b = bar();
            // bar() seeds tabs [1, 2, 3]
            assert!(!b.is_pinned(2), "fresh bar has no pinned tabs");
            b.pin(2);
            assert!(b.is_pinned(2));
            b.pin(2); // idempotent
            assert!(b.is_pinned(2));

            b.toggle_pin(2);
            assert!(!b.is_pinned(2), "toggle off");
            b.toggle_pin(2);
            assert!(b.is_pinned(2), "toggle on again");

            b.unpin(99); // unpinning a never-pinned id is a no-op
            b.unpin(2);
            assert!(!b.is_pinned(2));

            // A recycled id must not inherit stale pin state.
            b.pin(3);
            assert!(b.is_pinned(3));
            b.remove_tab(3);
            assert!(!b.is_pinned(3), "remove_tab must drop the sidecar entry");
        }

        // Case `accent_sidecar_tracks_ids_and_is_cleaned_up_on_remove`.
        {
            let mut b = bar();
            let red = vello::peniko::Color::from_rgba8(200, 60, 30, 255);
            assert_eq!(b.accent(2), None, "fresh bar has no accents");

            b.set_accent(2, red);
            assert_eq!(b.accent(2), Some(red));

            b.clear_accent(2);
            assert_eq!(b.accent(2), None);

            // A recycled id must not inherit a dead tab's tint.
            b.set_accent(3, red);
            b.remove_tab(3);
            assert_eq!(b.accent(3), None, "remove_tab must drop the sidecar entry");
        }
    }

    #[test]
    fn leading_slot_reserves_width_for_accents_and_app_icons() {
        // Case `tab_descriptor_new_sets_app_kind_none`.
        {
            // app_kind must default to None so `TabBar::draw` skips the
            // leading-icon reservation.
            let d = TabDescriptor::new(1, "X");
            assert_eq!(d.id, 1);
            assert_eq!(d.title, "X");
            assert!(d.app_kind.is_none());
        }

        // Case `pill_reserves_extra_width_when_app_kind_set`.
        {
            // With `app_kind = Some(kind)`, `apply_layout` reserves an extra
            // 20 px (icon_w 14 + icon_gap 6) on the leading edge.
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut a = TabBar::new("g1", vec![TabDescriptor::new(1, "A")], 1);
            let mut b = TabBar::new(
                "g1",
                vec![TabDescriptor {
                    id: 1,
                    title: "A".into(),
                    app_kind: Some(AppKind::Terminal),
                    icon: None,
                }],
                1,
            );
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

            let node_a = a.build_layout(&mut taffy, &theme, &mut node_map);
            let root_a = taffy.parent_node(Style::default(), &[node_a]);
            taffy
                .compute_layout(root_a, taffy::Size::MAX_CONTENT)
                .unwrap();
            a.apply_layout(&taffy, node_a, Point::ZERO);

            let mut taffy2 = TaffyTree::<()>::new();
            let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node_b = b.build_layout(&mut taffy2, &theme, &mut node_map2);
            let root_b = taffy2.parent_node(Style::default(), &[node_b]);
            taffy2
                .compute_layout(root_b, taffy::Size::MAX_CONTENT)
                .unwrap();
            b.apply_layout(&taffy2, node_b, Point::ZERO);

            let width_a = a.tab_rects[0].width();
            let width_b = b.tab_rects[0].width();
            // Both may hit `PILL_MIN_PX` since "A" is 1 char, so treat the floor as a
            // lower bound rather than asserting a strict >.
            assert!(
                width_b >= width_a || (width_a - width_b).abs() < 0.5,
                "typed-app pill must be at least as wide as the plain one (\
             a = {width_a}, b = {width_b})"
            );
            assert!(width_b > 0.0);
        }

        // Case `an_accent_opens_the_leading_slot_like_an_app_icon`.
        {
            // One slot, two occupants, one width: a strip mixing accented and
            // app-typed tabs must not wobble from one pill to the next — an
            // accent has to reserve exactly what the glyph does. The title is
            // long enough that natural width clears `PILL_MIN_PX` on its own, so
            // the leading-zone reservation is what's left to explain any gap.
            let mut plain = TabBar::new("g1", vec![TabDescriptor::new(1, "Documentation")], 1);
            let mut accented = TabBar::new("g1", vec![TabDescriptor::new(1, "Documentation")], 1);
            accented.set_accent(1, vello::peniko::Color::from_rgba8(10, 200, 90, 255));
            let mut typed = TabBar::new(
                "g1",
                vec![TabDescriptor {
                    id: 1,
                    title: "Documentation".into(),
                    app_kind: Some(AppKind::Editor),
                    icon: None,
                }],
                1,
            );

            lay_out_at_width(&mut plain, 900.0);
            lay_out_at_width(&mut accented, 900.0);
            lay_out_at_width(&mut typed, 900.0);

            let plain_w = plain.tab_rects[0].width();
            let accented_w = accented.tab_rects[0].width();
            let typed_w = typed.tab_rects[0].width();
            assert!(
                accented_w > plain_w,
                "an accented tab must be wider than a bare one (accented = \
             {accented_w}, plain = {plain_w})"
            );
            assert!(
                (accented_w - typed_w).abs() < 0.5,
                "an accent dot and an app glyph must reserve the same leading \
             width (accented = {accented_w}, typed = {typed_w})"
            );

            // Clearing the accent must give the width back.
            accented.clear_accent(1);
            lay_out_at_width(&mut accented, 900.0);
            let cleared_w = accented.tab_rects[0].width();
            assert!(
                (cleared_w - plain_w).abs() < 0.5,
                "clearing the accent must shrink the pill back down (cleared = \
             {cleared_w}, plain = {plain_w})"
            );
        }

        // Case `leading_glyph_sits_on_the_pill_inset_and_the_title_follows_by_icon_gap`.
        // The leading glyph starts on the same inset the title uses on a
        // glyph-less pill, and the title follows it by exactly `ICON_GAP` —
        // one inset and one gap, not a glyph tucked 2 px inside the text.
        {
            let r = Rect::new(100.0, 6.0, 300.0, 38.0);
            let glyph = leading_glyph_rect(r);
            assert_eq!(
                glyph.x0,
                r.x0 + PILL_PAD_X,
                "glyph starts on the text inset"
            );
            assert_eq!(glyph.width(), ICON_W);
            assert_eq!(glyph.height(), ICON_W, "the glyph slot is square");
            assert!(
                (glyph.center().y - r.center().y).abs() < 1e-9,
                "the glyph sits on the pill's centre line"
            );
            let with_glyph = title_x0(r, ICON_W + ICON_GAP);
            assert!((with_glyph - glyph.x1 - ICON_GAP).abs() < 1e-9);
            assert_eq!(
                title_x0(r, 0.0),
                r.x0 + PILL_PAD_X,
                "no glyph: title on the inset"
            );
            // The pinned marker occupies the same slot, centred on the pill.
            let pinned = square_at(r.center(), ICON_W);
            assert!((pinned.center().x - r.center().x).abs() < 1e-9);
            assert_eq!(pinned.width(), ICON_W);
        }
    }

    #[test]
    fn modified_tab_shows_a_dot_at_rest_and_the_close_glyph_on_hover() {
        // Case `modified_tab_holds_its_close_slot_open_at_rest`.
        {
            let mut b = bar(); // active = 1 (A); tab 2 (B) is idle and inactive.
            lay_out_at_width(&mut b, 900.0);
            let idx = b.tabs().iter().position(|t| t.id == 2).unwrap();
            assert_eq!(
                b.close_rects[idx],
                Rect::ZERO,
                "an idle, unmodified, inactive tab has no close chip at rest"
            );

            b.set_modified(2, true);
            lay_out_at_width(&mut b, 900.0);
            assert_ne!(
                b.close_rects[idx],
                Rect::ZERO,
                "a modified tab holds its close slot open even when idle and \
             inactive"
            );

            b.set_modified(2, false);
            lay_out_at_width(&mut b, 900.0);
            assert_eq!(
                b.close_rects[idx],
                Rect::ZERO,
                "clearing modified must let the slot close back up"
            );
        }

        // Case `modified_dot_paints_at_rest_and_the_x_returns_on_hover`.
        {
            let mut b = bar(); // active = 1 (A)
            b.set_modified(2, true); // inactive tab B, data index 1
            let theme = Theme::dark();

            assert_eq!(
                b.modified_marker_color_for_tests(1, &theme),
                Some(theme.tokens.colors.text_dim),
                "an inactive modified tab dots in text_dim at rest"
            );
            assert_eq!(
                b.modified_marker_color_for_tests(0, &theme),
                None,
                "the active tab (A) is not modified, so it has no dot"
            );

            b.hover_open_for_tests(1);
            assert_eq!(
                b.modified_marker_color_for_tests(1, &theme),
                None,
                "hovering the pill swaps the dot back to the × so the two \
             never share a frame"
            );

            b.hover_idx = None;
            b.hover_close_idx = Some(1);
            assert_eq!(
                b.modified_marker_color_for_tests(1, &theme),
                None,
                "hovering the close chip itself must also swap back to the ×"
            );

            b.hover_close_idx = None;
            assert!(
                b.modified_marker_color_for_tests(1, &theme).is_some(),
                "the dot returns once the pointer leaves again"
            );
        }

        // Case `modified_dot_swaps_for_the_close_glyph_on_hover_in_the_real_scene`.
        // Same swap as above, but through the real `draw()` path rather than the
        // test-only colour accessor — counts what actually lands in the `Scene`,
        // the way `active_pill_emits_more_color_fills_than_inactive_siblings`
        // does for the active-pill rim.
        {
            use vello_encoding::DrawTag;

            fn render(modified: bool, hover: bool) -> usize {
                let mut b = TabBar::new(
                    "g1",
                    vec![TabDescriptor::new(1, "A"), TabDescriptor::new(2, "B")],
                    1, // A active; B (data index 1) is the tab under test.
                );
                b.rect = Rect::new(0.0, 0.0, 160.0, 40.0);
                b.tab_rects = vec![
                    Rect::new(0.0, 6.0, 80.0, 34.0),
                    Rect::new(80.0, 6.0, 160.0, 34.0),
                ];
                b.close_rects = vec![
                    Rect::new(60.0, 10.0, 78.0, 30.0),
                    Rect::new(140.0, 10.0, 158.0, 30.0),
                ];
                b.max_rects = vec![Rect::ZERO; 2];
                b.search_rects = vec![Rect::ZERO; 2];
                if modified {
                    b.set_modified(2, true);
                }
                if hover {
                    b.hover_open_for_tests(1);
                }

                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let presence = PresenceCtx::default();
                let mut scene = Scene::new();
                b.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
                scene
                    .encoding()
                    .draw_tags
                    .iter()
                    .filter(|t| **t == DrawTag::COLOR)
                    .count()
            }

            let resting = render(true, false);
            let hovered = render(true, true);
            assert!(
                hovered > resting,
                "hovering a modified tab must swap its resting dot (one fill) \
             for the close-chip + × combination (a chip fill plus the ×'s \
             strokes), which paints strictly more colour primitives \
             (resting = {resting}, hovered = {hovered})"
            );

            // A hovered modified tab and a hovered clean tab must paint the same
            // close-glyph chrome — the modified state only changes what paints
            // at rest, never what paints under the pointer.
            let hovered_unmodified = render(false, true);
            assert_eq!(
                hovered, hovered_unmodified,
                "hovering must show the ordinary × cluster regardless of the \
             modified flag (modified = {hovered}, clean = {hovered_unmodified})"
            );
        }

        // Case `accessibility_reports_a_modified_tab_via_description`.
        {
            use crate::a11y::Role;
            use crate::i18n::I18nManager;
            // The i18n manager is a thread-local; each test re-inits its own bundle
            // (see `widget::text`'s tests for the same pattern).
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", "tab-modified = Unsaved changes".to_string());

            let mut b = bar();
            b.set_modified(2, true);
            let node = Widget::accessibility(&b).expect("TabBar always reports a node");
            let children: Vec<_> = node
                .children
                .into_iter()
                .filter(|child| child.role == Role::Tab)
                .collect();
            assert_eq!(children.len(), 3);
            assert_eq!(children[0].role, Role::Tab);
            assert_eq!(
                children[0].description, None,
                "tab A is clean and gets no description"
            );
            assert_eq!(
                children[1].description.as_deref(),
                Some("Unsaved changes"),
                "tab B is modified and must say so for a reader that can't see \
             the dot"
            );
        }
    }

    #[test]
    fn inactive_tab_action_chips_fire_on_first_click_unless_withheld() {
        // Case `press_on_inactive_tab_maximize_icon_fires_on_first_click`.
        {
            let mut b = bar();
            // A sits at [0..80], B at [86..166], C at [172..252]. Tab `1` (A)
            // is active — click B's (inactive) maximize.
            b.rect = Rect::new(0.0, 0.0, 252.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            b.close_rects = vec![
                Rect::new(60.0, 10.0, 78.0, 30.0),
                Rect::new(146.0, 10.0, 164.0, 30.0),
                Rect::new(232.0, 10.0, 250.0, 30.0),
            ];
            b.max_rects = vec![
                Rect::new(40.0, 10.0, 56.0, 30.0),
                Rect::new(126.0, 10.0, 142.0, 30.0),
                Rect::new(212.0, 10.0, 228.0, 30.0),
            ];
            b.search_rects = vec![
                Rect::new(20.0, 10.0, 36.0, 30.0),
                Rect::new(106.0, 10.0, 122.0, 30.0),
                Rect::new(192.0, 10.0, 208.0, 30.0),
            ];
            assert_eq!(b.active_id(), 1);

            // Deliberately leave `hover_max_idx` at None: mirrors a click position
            // not yet covered by a prior CursorMoved.
            b.last_cursor = Point::new(134.0, 20.0);
            b.hover_max_idx = None;
            b.hover_search_idx = None;
            b.hover_close_idx = None;
            b.hover_idx = Some(1);

            let press = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let handled = dispatch(&mut b, &press);

            assert_eq!(
                b.take_max(),
                Some(2),
                "maximize click on inactive tab must fire on first press",
            );
            assert!(handled, "press on the maximize icon is handled");
            assert_eq!(
                b.active_id(),
                1,
                "icon hit must not bleed into tab activation",
            );
        }

        // Case `press_on_inactive_tab_search_icon_fires_on_first_click`.
        {
            // Twin of the maximize test above, for the search icon.
            let mut b = bar();
            b.rect = Rect::new(0.0, 0.0, 252.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            b.close_rects = vec![
                Rect::new(60.0, 10.0, 78.0, 30.0),
                Rect::new(146.0, 10.0, 164.0, 30.0),
                Rect::new(232.0, 10.0, 250.0, 30.0),
            ];
            b.max_rects = vec![
                Rect::new(40.0, 10.0, 56.0, 30.0),
                Rect::new(126.0, 10.0, 142.0, 30.0),
                Rect::new(212.0, 10.0, 228.0, 30.0),
            ];
            b.search_rects = vec![
                Rect::new(20.0, 10.0, 36.0, 30.0),
                Rect::new(106.0, 10.0, 122.0, 30.0),
                Rect::new(192.0, 10.0, 208.0, 30.0),
            ];
            assert_eq!(b.active_id(), 1);

            // C's search rect (inactive tab, id 3); hover caches left empty.
            b.last_cursor = Point::new(200.0, 20.0);
            b.hover_max_idx = None;
            b.hover_search_idx = None;
            b.hover_close_idx = None;
            b.hover_idx = Some(2);

            let press = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let handled = dispatch(&mut b, &press);

            assert_eq!(
                b.take_search(),
                Some(3),
                "search click on inactive tab must fire on first press",
            );
            assert!(handled, "press on the search icon is handled");
            assert_eq!(
                b.active_id(),
                1,
                "search icon hit must not bleed into tab activation",
            );
        }

        // Case `set_show_maximize_false_withholds_the_hover_chip_cluster`.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let width = 900.0;
            let mut with_max = four_tab_bar();
            let mut without_max = four_tab_bar();
            without_max.set_show_maximize(false);

            lay_out_at_width_with(&mut with_max, width, &theme);
            lay_out_at_width_with(&mut without_max, width, &theme);
            hover_pill(&mut with_max, 1, width, &theme);
            hover_pill(&mut without_max, 1, width, &theme);

            assert!(
                with_max.max_rects()[1].area() > 0.0,
                "with maximize offered, hovering an unpinned tab reveals the \
             maximize chip"
            );
            assert_eq!(
                without_max.max_rects()[1],
                Rect::ZERO,
                "set_show_maximize(false) must withhold the maximize chip even \
             while hovered"
            );
            assert_eq!(
                without_max.search_rects()[1],
                Rect::ZERO,
                "withholding maximize must not leave a gap where the search \
             chip would have opened into the space maximize used to reserve"
            );
            // Close must still work — only the hover-reveal extras are withheld.
            assert!(without_max.close_rects[1].area() > 0.0);
        }
    }

    #[test]
    fn pinned_tabs_lead_the_bar_and_never_close() {
        // Case `pinned_tabs_render_to_the_left_of_unpinned`.
        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "A"),
                    TabDescriptor::new(2, "B"),
                    TabDescriptor::new(3, "C"),
                    TabDescriptor::new(4, "D"),
                ],
                1,
            );
            b.pin(2);
            b.pin(4);

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = b.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            b.apply_layout(&taffy, node, Point::ZERO);

            // Insertion order is unchanged — only x-positions reorder.
            let ids: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
            assert_eq!(ids, vec![1, 2, 3, 4]);

            let x_of = |id: u64| {
                let i = b.tabs().iter().position(|t| t.id == id).unwrap();
                b.tab_rects()[i].x0
            };
            assert!(
                x_of(2) < x_of(4),
                "pinned-2 must precede pinned-4 (insertion order within \
             pinned group)",
            );
            assert!(
                x_of(4) < x_of(1),
                "every pinned pill must precede every unpinned pill on x \
             (pinned-4 vs unpinned-1)",
            );
            assert!(x_of(4) < x_of(3), "pinned-4 must precede unpinned-3");
            assert!(
                x_of(1) < x_of(3),
                "unpinned-1 must precede unpinned-3 (insertion order within \
             unpinned group)",
            );

            let pinned_w = b.tab_rects()[b.tabs().iter().position(|t| t.id == 2).unwrap()].width();
            let unpinned_w =
                b.tab_rects()[b.tabs().iter().position(|t| t.id == 1).unwrap()].width();
            assert!(
                pinned_w < unpinned_w,
                "pinned pill ({pinned_w}) must be narrower than unpinned ({unpinned_w})",
            );
            assert!(
                pinned_w <= 32.0,
                "pinned pill must be compact, got {pinned_w}"
            );
        }

        // Case `pinned_tabs_have_no_close_rect`.
        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut b = TabBar::new(
                "g1",
                (1..=8u64)
                    .map(|i| TabDescriptor::new(i, format!("T{i}")))
                    .collect(),
                1,
            );
            b.pin(7);

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = b.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            b.apply_layout(&taffy, node, Point::ZERO);

            let pinned_idx = b.tabs().iter().position(|t| t.id == 7).unwrap();
            assert_eq!(
                b.close_rects[pinned_idx],
                Rect::ZERO,
                "pinned tab close_rect must be Rect::ZERO so hit-tests don't fire",
            );
            assert_eq!(
                b.max_rects[pinned_idx],
                Rect::ZERO,
                "pinned tab max_rect must be Rect::ZERO too — hover-reveal \
             cluster is suppressed alongside the close glyph",
            );
            assert_eq!(b.search_rects[pinned_idx], Rect::ZERO);

            let unpinned_idx = b.tabs().iter().position(|t| t.id == 1).unwrap();
            assert_ne!(
                b.close_rects[unpinned_idx],
                Rect::ZERO,
                "unpinned tabs keep their close rect",
            );
        }

        // Case `pinned_tab_press_at_close_position_does_not_close`.
        {
            let mut b = bar();
            // Hand-built rects (no taffy): three pills [0..80], [86..166],
            // [172..252] with canonical close chips. Pin tab 2 (data index 1).
            b.rect = Rect::new(0.0, 0.0, 252.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            b.close_rects = vec![
                Rect::new(60.0, 10.0, 78.0, 30.0),
                Rect::new(146.0, 10.0, 164.0, 30.0),
                Rect::new(232.0, 10.0, 250.0, 30.0),
            ];
            b.max_rects = vec![Rect::ZERO; 3];
            b.search_rects = vec![Rect::ZERO; 3];
            b.pin(2);

            let close_centre = b.close_rects[1].center();
            b.last_cursor = close_centre;

            let press = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            dispatch(&mut b, &press);

            assert!(
                b.pending_close.is_none(),
                "press on a pinned tab's close-rect must not seed pending_close",
            );
            assert!(
                b.closing_tab_ids().is_empty(),
                "pinned tab must not enter the closing-fade flow",
            );
        }

        // Case `pinned_active_tab_still_paints_no_close_glyph`.
        // Pinned tabs stay icon-only even while active — the compact 28-px
        // slot has no room for a `×`, and the press hit-test filters them out.
        {
            let theme = Theme::dark();
            let mut b = bar();
            b.pin(1); // tab A is both pinned and active.
            assert!(
                !b.close_affordance_visible_for_tests(0, &theme),
                "a pinned tab never shows a ×, active or not",
            );
            assert!(b.close_chip_color_for_tests(0, &theme).is_none());
        }
    }

    #[test]
    fn pill_fills_distinguish_state_tier_and_collaborator_presence() {
        // Case `pill_tiers_lift_off_the_ground_in_order_on_every_preset`.
        // Run over every preset, not just dark: asserting on fill *alpha* only
        // works while every tier is a white wash over a dark ground — on a light
        // theme a heavier alpha of white moves the pill *less*. What a person sees
        // is how far the composited body sits from its ground, so that's measured.
        {
            use crate::a11y::relative_luminance;
            for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let ground = crate::chrome::strip_ground(&theme);
                let g = relative_luminance(ground);
                let lift_of = |c: vello::peniko::Color| {
                    let a = f32::from(c.a()) / 255.0;
                    let ch = |o: u8, gr: u8| {
                        let blended = f32::from(o) * a + f32::from(gr) * (1.0 - a);
                        // A weighted average of two `u8` channels is mathematically
                        // bounded to 0.0..=255.0; the rounding cast is exact within
                        // that range (and Rust's `as` saturates regardless).
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "blended stays within 0.0..=255.0 by construction"
                        )]
                        let rounded = blended.round() as u8;
                        rounded
                    };
                    let seen = vello::peniko::Color::from_rgb8(
                        ch(c.r(), ground.r()),
                        ch(c.g(), ground.g()),
                        ch(c.b(), ground.b()),
                    );
                    (relative_luminance(seen) - g).abs()
                };

                let idle = pill_appearance(
                    &theme,
                    crate::chrome::strip_ground(&theme),
                    false,
                    false,
                    false,
                );
                let hover = pill_appearance(
                    &theme,
                    crate::chrome::strip_ground(&theme),
                    false,
                    true,
                    false,
                );
                let active = pill_appearance(
                    &theme,
                    crate::chrome::strip_ground(&theme),
                    true,
                    false,
                    false,
                );
                let active_hover = pill_appearance(
                    &theme,
                    crate::chrome::strip_ground(&theme),
                    true,
                    true,
                    false,
                );

                assert_eq!(
                    idle.fill.a(),
                    0,
                    "{}: idle inactive pill must paint zero fill so the bar reads \
                 as transparent",
                    theme.name,
                );
                assert!(
                    idle.border.is_none() && idle.inner_glow.is_none(),
                    "{}: idle inactive pill must paint no border or glow",
                    theme.name,
                );

                let hover_lift = lift_of(hover.fill);
                let active_lift = lift_of(active.fill);
                assert!(
                    hover_lift > 0.0,
                    "{}: a hovered pill is indistinguishable from the ground \
                 (lift {hover_lift:.5}) — hover has to be visible feedback, \
                 not a no-op",
                    theme.name,
                );
                assert!(
                    active_lift > hover_lift,
                    "{}: the active pill does not sit above the hovered one \
                 (active {active_lift:.5} <= hover {hover_lift:.5}), so the \
                 bar reads as uniform and the user cannot tell which tab is \
                 active at a glance",
                    theme.name,
                );
                assert!(
                    active.border.is_some()
                        && active.inner_glow.is_some()
                            == crate::chrome::selector_finish(&theme).rim().is_some(),
                    "{}: active pill must paint an edge, and the finish's lit rim, so \
                 it reads as raised",
                    theme.name,
                );
                assert_eq!(
                    active.fill, active_hover.fill,
                    "{}: active pill should not change on hover",
                    theme.name,
                );
            }
        }

        // Case `pressed_pill_uses_active_visuals_and_scales_down`.
        {
            let theme = Theme::dark();
            let pressed_inactive = pill_appearance(
                &theme,
                crate::chrome::strip_ground(&theme),
                false,
                false,
                true,
            );
            let active = pill_appearance(
                &theme,
                crate::chrome::strip_ground(&theme),
                true,
                false,
                false,
            );

            assert_eq!(
                pressed_inactive.fill.a(),
                active.fill.a(),
                "pressed pill must adopt the active fill alpha for instant feedback",
            );
            assert!(
                pressed_inactive.border.is_some(),
                "pressed pill paints a border"
            );
            assert!(
                pressed_inactive.inner_glow.is_some(),
                "pressed pill paints a glow"
            );
            assert!(
                pressed_inactive.scale < 1.0 && pressed_inactive.scale > 0.9,
                "pressed scale must compress (0.9 < scale < 1.0); got {}",
                pressed_inactive.scale,
            );
            assert!(
                (active.scale - 1.0).abs() < f64::EPSILON,
                "non-pressed active pill must not scale",
            );
        }

        // Case `active_pill_emits_more_color_fills_than_inactive_siblings`.
        // End-to-end scene check, pinned against the vello encoding: catches a
        // refactor that silently drops the active fill, which a helper-only check
        // on `pill_appearance` would miss.
        {
            use vello_encoding::DrawTag;

            fn render_three(active_id: u64) -> usize {
                let mut b = TabBar::new(
                    "g1",
                    vec![
                        TabDescriptor::new(1, "A"),
                        TabDescriptor::new(2, "B"),
                        TabDescriptor::new(3, "C"),
                    ],
                    active_id,
                );
                b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
                b.tab_rects = vec![
                    Rect::new(0.0, 6.0, 80.0, 34.0),
                    Rect::new(80.0, 6.0, 160.0, 34.0),
                    Rect::new(160.0, 6.0, 240.0, 34.0),
                ];
                b.close_rects = vec![
                    Rect::new(60.0, 10.0, 78.0, 30.0),
                    Rect::new(140.0, 10.0, 158.0, 30.0),
                    Rect::new(220.0, 10.0, 238.0, 30.0),
                ];
                b.max_rects = vec![
                    Rect::new(40.0, 10.0, 56.0, 30.0),
                    Rect::new(120.0, 10.0, 136.0, 30.0),
                    Rect::new(200.0, 10.0, 216.0, 30.0),
                ];
                b.search_rects = vec![
                    Rect::new(20.0, 10.0, 36.0, 30.0),
                    Rect::new(100.0, 10.0, 116.0, 30.0),
                    Rect::new(180.0, 10.0, 196.0, 30.0),
                ];
                // Bypasses `set_active`'s membership guard to stage "all inactive"
                // by pointing at id 0 (matches nothing).
                b.active_id = active_id;

                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let presence = PresenceCtx::default();
                let mut scene = Scene::new();
                b.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
                scene
                    .encoding()
                    .draw_tags
                    .iter()
                    .filter(|t| **t == DrawTag::COLOR)
                    .count()
            }

            let with_active = render_three(1);
            let all_inactive = render_three(0);

            assert!(
                with_active > all_inactive,
                "active pill must emit more colour fills than the same three pills \
             all rendered inactive (active+2 inactive: {with_active}, \
             all-inactive: {all_inactive}); the active body fill + border + \
             glow stroke account for the delta",
            );
        }

        // Case `peer_on_another_tab_paints_navigation_mist`.
        {
            use vello_encoding::DrawTag;

            fn render(with_peer: bool) -> usize {
                let mut b = TabBar::new(
                    "g1",
                    vec![TabDescriptor::new(1, "A"), TabDescriptor::new(2, "B")],
                    1,
                );
                b.rect = Rect::new(0.0, 0.0, 160.0, 40.0);
                b.tab_rects = vec![
                    Rect::new(0.0, 6.0, 80.0, 34.0),
                    Rect::new(80.0, 6.0, 160.0, 34.0),
                ];
                b.close_rects = vec![Rect::ZERO; 2];
                b.max_rects = vec![Rect::ZERO; 2];
                b.search_rects = vec![Rect::ZERO; 2];

                let mut presence = PresenceCtx::default();
                if with_peer {
                    presence.register_peer_at(
                        "ada".to_string(),
                        PresenceKey::Tab {
                            id: "2".to_string(),
                        },
                    );
                }
                let mut scene = Scene::new();
                b.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &presence,
                );
                scene
                    .encoding()
                    .draw_tags
                    .iter()
                    .filter(|tag| **tag == DrawTag::COLOR)
                    .count()
            }

            let empty = render(false);
            let occupied = render(true);
            assert!(
                occupied >= empty.saturating_add(2),
                "a peer in another view must add the mist's outer wash and bright core to its navigation tab (empty={empty}, occupied={occupied})",
            );
        }

        // Case `peers_on_a_tab_show_faces_that_give_way_to_the_title`.
        {
            fn faces_at(width: f64) -> (Vec<PillFaces>, f64) {
                let mut b = TabBar::new(
                    "g1",
                    vec![
                        TabDescriptor::new(1, "Alpha"),
                        TabDescriptor::new(2, "Quarterly planning"),
                    ],
                    1,
                );
                b.rect = Rect::new(0.0, 0.0, 80.0 + width, 40.0);
                b.tab_rects = vec![
                    Rect::new(0.0, 6.0, 80.0, 34.0),
                    Rect::new(80.0, 6.0, 80.0 + width, 34.0),
                ];
                b.close_rects = vec![Rect::ZERO; 2];
                b.max_rects = vec![Rect::ZERO; 2];
                b.search_rects = vec![Rect::ZERO; 2];
                let mut presence = PresenceCtx::default();
                for peer in ["ada", "bot", "cy", "dee", "eve"] {
                    presence.register_peer_at(
                        peer.to_string(),
                        PresenceKey::Tab {
                            id: "2".to_string(),
                        },
                    );
                }
                presence.set_participant_kind(crate::permissions::UserId("bot".into()), true);
                b.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &presence,
                );
                let r = b.tab_rects[1];
                let (_, title_x1) = b.title_zone(1, r);
                (b.pill_faces.clone(), r.x1 - title_x1)
            }

            let (wide, wide_trailing) = faces_at(200.0);
            assert_eq!(wide[0], PillFaces::default(), "nobody is on tab 1");
            assert_eq!(
                wide[1],
                PillFaces {
                    faces: 2,
                    overflow: 3
                },
                "two faces, the rest folded into +3"
            );
            assert!(
                wide_trailing
                    >= wide[1].width(FaceMetrics::of(&Theme::dark())) + Reveal::None.reserve(),
                "the title zone ends before the faces, so they never cover it"
            );
            let (medium, _) = faces_at(110.0);
            assert_eq!(
                medium[1],
                PillFaces {
                    faces: 1,
                    overflow: 4
                },
                "a tighter pill keeps a legible title and shows fewer faces"
            );
            let (narrow, _) = faces_at(70.0);
            assert_eq!(narrow[1], PillFaces::default(), "too narrow: mist only");

            // A short title: the pill grows by its faces' width, so after
            // the relayout the faces show next to the whole title.
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Main"),
                    TabDescriptor::new(2, "Notes"),
                ],
                1,
            );
            let reserve = b.resting_reveal(1).reserve();
            let draw_at = |b: &mut TabBar, width: f64, presence: &PresenceCtx| {
                b.rect = Rect::new(0.0, 0.0, 80.0 + width, 40.0);
                b.tab_rects = vec![
                    Rect::new(0.0, 6.0, 80.0, 34.0),
                    Rect::new(80.0, 6.0, 80.0 + width, 34.0),
                ];
                b.reveal_px = vec![0.0, reserve];
                b.close_rects = vec![Rect::ZERO; 2];
                b.max_rects = vec![Rect::ZERO; 2];
                b.search_rects = vec![Rect::ZERO; 2];
                b.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    presence,
                );
            };
            draw_at(&mut b, 200.0, &PresenceCtx::default());
            let content = PILL_PAD_X + b.leading_zone(1) + b.shaped_title_width(1) + reserve;
            let mut presence = PresenceCtx::default();
            presence.register_peer_at(
                "ada".to_string(),
                PresenceKey::Tab {
                    id: "2".to_string(),
                },
            );
            draw_at(&mut b, content.max(PILL_MIN_PX), &presence);
            assert!(b.face_demand_stale, "a new peer asks for a relayout");
            let one = PillFaces {
                faces: 1,
                overflow: 0,
            };
            let grown = b.natural_pill_width(1, reserve);
            let wanted = (content + one.width(FaceMetrics::of(&Theme::dark()))).max(PILL_MIN_PX);
            assert!(
                (grown - wanted).abs() < 0.01,
                "the pill grows by the face: {grown} vs {wanted}"
            );
            draw_at(&mut b, grown, &presence);
            assert_eq!(b.pill_faces[1], one, "the grown pill shows the face");
        }

        // Case `user_color_is_stable_per_id_and_differs_across_ids`.
        {
            let a1 = user_color("alice");
            let a2 = user_color("alice");
            let b1 = user_color("bob");
            assert_eq!(a1, a2, "same id must map to same color across calls");
            assert_ne!(a1, b1, "different ids should map to different colors");
        }
    }

    #[test]
    fn drag_drop_targets_resolve_gaps_from_pill_midpoints() {
        // Case `drop_index_for_uses_pill_midpoints`.
        {
            // Hand-built (no taffy): A [0,80], B [80,160], C [160,240].
            let mut b = bar();
            b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(80.0, 6.0, 160.0, 34.0),
                Rect::new(160.0, 6.0, 240.0, 34.0),
            ];
            // Left of A's midpoint (40) → insert before A (index 0).
            assert_eq!(b.drop_index_for(Point::new(10.0, 20.0)), 0);
            // Past A's mid but before B's mid (120) → insert at index 1.
            assert_eq!(b.drop_index_for(Point::new(90.0, 20.0)), 1);
            // Past B's mid but before C's mid (200) → insert at index 2.
            assert_eq!(b.drop_index_for(Point::new(170.0, 20.0)), 2);
            // Past every midpoint → append at len().
            assert_eq!(b.drop_index_for(Point::new(230.0, 20.0)), 3);
        }

        // Case `drop_index_for_empty_bar_is_zero`.
        {
            // A just-emptied bar should accept any drop at index 0.
            let mut b = bar();
            b.tab_rects.clear();
            assert_eq!(b.drop_index_for(Point::new(0.0, 0.0)), 0);
            assert_eq!(b.drop_index_for(Point::new(1000.0, 0.0)), 0);
        }

        // Case `release_reorders_to_indicated_gap_not_only_onto_a_pill`.
        {
            use winit::event::{ElementState, MouseButton};

            // Pills A [0,80], B [80,160], C [160,240] inside a 240 px bar.
            let make = || {
                let mut b = bar();
                b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
                b.tab_rects = vec![
                    Rect::new(0.0, 6.0, 80.0, 34.0),
                    Rect::new(80.0, 6.0, 160.0, 34.0),
                    Rect::new(160.0, 6.0, 240.0, 34.0),
                ];
                b
            };

            // Simulate "drag pill 0, release past C's midpoint" (append to end).
            let mut b = make();
            b.pending_drag = Some(DragPayload::Tab {
                group: 1,
                tab: b.tabs[0].id,
            });
            b.press_origin = Some((0, Point::new(40.0, 20.0)));
            b.last_cursor = Point::new(230.0, 20.0);
            let mut handled = false;
            b.finish_drag_release(&mut handled);
            assert_eq!(
                b.take_reorder(),
                Some((0, 2)),
                "released past the last midpoint must move the tab to the end",
            );

            // Drag pill 2, release before A's midpoint (insert at the very start).
            let mut b = make();
            b.pending_drag = Some(DragPayload::Tab {
                group: 1,
                tab: b.tabs[2].id,
            });
            b.press_origin = Some((2, Point::new(200.0, 20.0)));
            b.last_cursor = Point::new(5.0, 20.0);
            let mut handled = false;
            b.finish_drag_release(&mut handled);
            assert_eq!(
                b.take_reorder(),
                Some((2, 0)),
                "released before the first midpoint must move the tab to the front",
            );

            // Releasing where the tab already is must NOT publish a reorder.
            let mut b = make();
            b.pending_drag = Some(DragPayload::Tab {
                group: 1,
                tab: b.tabs[1].id,
            });
            b.press_origin = Some((1, Point::new(120.0, 20.0)));
            b.last_cursor = Point::new(120.0, 20.0);
            let mut handled = false;
            b.finish_drag_release(&mut handled);
            assert_eq!(b.take_reorder(), None, "a no-op drop publishes nothing");

            // Released outside the bar entirely → detach, not reorder.
            let mut b = make();
            b.pending_drag = Some(DragPayload::Tab {
                group: 1,
                tab: b.tabs[0].id,
            });
            b.press_origin = Some((0, Point::new(40.0, 20.0)));
            b.last_cursor = Point::new(120.0, 400.0);
            let mut handled = false;
            b.finish_drag_release(&mut handled);
            assert_eq!(b.take_reorder(), None, "outside the bar is not a reorder");
            assert!(b.pending_detach.is_some(), "outside the bar detaches");
            let _ = (ElementState::Pressed, MouseButton::Left);
        }

        // Case `drop_zone_target_tracks_cursor_during_drag`.
        {
            let mut b = bar();
            // Pills: A [0..80], B [80..160], C [160..240].
            b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(80.0, 6.0, 160.0, 34.0),
                Rect::new(160.0, 6.0, 240.0, 34.0),
            ];
            // Set directly rather than driving a real press+move-past-threshold,
            // which integration tests already cover.
            b.pending_drag = Some(DragPayload::Tab { group: 0, tab: 1 });
            // Pill B's left half → drop index 1 (insert before B).
            b.last_cursor = Point::new(90.0, 20.0);
            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(90.0, 20.0),
            };
            dispatch(&mut b, &event);
            assert_eq!(
                b.drop_zone_target(),
                Some(1),
                "cursor on B's left half → gap 1"
            );

            // Move cursor past pill C's midpoint → gap 3 (append).
            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(230.0, 20.0),
            };
            dispatch(&mut b, &event);
            assert_eq!(b.drop_zone_target(), Some(3), "past C's mid → gap 3");

            let release = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch(&mut b, &release);
            assert_eq!(b.drop_zone_target(), None, "release clears target");
            assert!(
                b.hover_intent.deadline(crate::time::now()).is_some()
                    && b.next_wake_deadline().is_some(),
                "release clears the drag target but keeps the resting pointer's \
                 hover dwell running, so the pill under it still opens",
            );
        }

        // Case `hover_drop_idx_follows_own_bar_drag_cursor`.
        {
            // Same three-pill fixture as the drop-zone test above.
            let mut b = bar();
            b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(80.0, 6.0, 160.0, 34.0),
                Rect::new(160.0, 6.0, 240.0, 34.0),
            ];
            assert_eq!(
                b.hover_drop_idx(),
                None,
                "fresh bar with no drag has no hover-drop indicator",
            );

            // Pill B's left half → gap 1.
            b.pending_drag = Some(DragPayload::Tab { group: 0, tab: 1 });
            b.last_cursor = Point::new(90.0, 20.0);
            let event = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(90.0, 20.0),
            };
            dispatch(&mut b, &event);
            assert_eq!(
                b.hover_drop_idx(),
                Some(1),
                "own-bar drag must set hover_drop_idx to the resolved gap (B's left half → 1)",
            );
            assert!(
                b.needs_animation_frame(),
                "hover-drop fade in flight must request redraws",
            );

            let release = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch(&mut b, &release);
            assert_eq!(
                b.hover_drop_idx(),
                None,
                "release must clear hover_drop_idx so a fresh drag starts with no stale line",
            );
        }

        // Case `set_external_drag_hover_resolves_gap_for_sibling_bars`.
        // `None` clears; `Some(point)` inside the bar resolves via `drop_index_for`,
        // outside is ignored; no-op while the bar has its own drag in flight.
        {
            let mut b = bar();
            b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(80.0, 6.0, 160.0, 34.0),
                Rect::new(160.0, 6.0, 240.0, 34.0),
            ];

            // Past every midpoint → gap 3 (append). No own drag, so the indicator lights up.
            b.set_external_drag_hover(Some(Point::new(230.0, 20.0)));
            assert_eq!(
                b.hover_drop_idx(),
                Some(3),
                "external hover past every midpoint → gap 3 (append)",
            );

            // -5 is outside the bar rect so the setter clamps to None.
            b.set_external_drag_hover(Some(Point::new(-5.0, 20.0)));
            assert_eq!(
                b.hover_drop_idx(),
                None,
                "point outside the bar rect clears the indicator",
            );

            // Within bar but left of A's midpoint (40) → gap 0.
            b.set_external_drag_hover(Some(Point::new(10.0, 20.0)));
            assert_eq!(b.hover_drop_idx(), Some(0));

            // None clears.
            b.set_external_drag_hover(None);
            assert_eq!(b.hover_drop_idx(), None);

            // No-op so the parent's fan-out can't wipe the source bar's own indicator.
            b.pending_drag = Some(DragPayload::Tab { group: 0, tab: 1 });
            b.hover_drop_idx = Some(2);
            b.set_external_drag_hover(None);
            assert_eq!(
                b.hover_drop_idx(),
                Some(2),
                "set_external_drag_hover is a no-op while own drag is in flight",
            );
            b.set_external_drag_hover(Some(Point::new(10.0, 20.0)));
            assert_eq!(
                b.hover_drop_idx(),
                Some(2),
                "set_external_drag_hover is a no-op while own drag is in flight, \
             even with an in-bar point",
            );
        }
    }

    #[test]
    fn pending_tab_requests_drain_exactly_once() {
        // Case `take_max_drains_pending_request_exactly_once`.
        {
            let mut b = bar();
            assert_eq!(b.take_max(), None);
            b.pending_max = Some(2);
            assert_eq!(b.take_max(), Some(2));
            // WindowGroup must not double-toggle `maximized` on a redundant poll.
            assert_eq!(b.take_max(), None);
        }

        // Case `take_search_drains_pending_request_exactly_once`.
        {
            let mut b = bar();
            assert_eq!(b.take_search(), None);
            b.pending_search = Some(3);
            assert_eq!(b.take_search(), Some(3));
            assert_eq!(b.take_search(), None);
        }

        // Case `take_close_moves_id_into_closing_vec`.
        {
            let mut b = bar_with_laid_out_rects();
            assert!(b.closing_tab_ids().is_empty(), "precondition: no closing");
            assert_eq!(b.take_close(), None, "precondition: no pending close");

            let started = b.start_closing(2);
            assert!(started, "start_closing on a live tab id must succeed");
            assert_eq!(
                b.closing_tab_ids(),
                vec![2],
                "closing_tabs must carry the closing id",
            );
            let stamp = b
                .closing_started_at(2)
                .expect("started_at recorded at close-start");
            assert!(
                stamp.elapsed() < close_fade(),
                "close-start timestamp is recent",
            );

            assert_eq!(
                b.take_close(),
                Some(2),
                "first drain returns the closing id once",
            );
            assert_eq!(
                b.take_close(),
                None,
                "second drain is None — matches the one-shot take_close contract",
            );
        }
    }

    #[test]
    fn tab_list_changes_animate_with_springs_and_fades_that_expire() {
        // Case `reorder_starts_layout_spring_and_snapshots_prev_rects`.
        {
            let mut b = bar();
            assert!(b.prev_tab_rects_by_id.is_empty());
            assert!(b.layout_anim_started.is_none());
            assert!(!b.needs_animation_frame());

            // What `apply_layout` would leave behind in production.
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];

            b.reorder(0, 2);
            assert_eq!(b.prev_tab_rects_by_id.len(), 3);
            assert_eq!(
                b.prev_tab_rects_by_id.get(&1).copied(),
                Some(Rect::new(0.0, 6.0, 80.0, 34.0)),
                "tab 1's pre-reorder rect must be captured"
            );
            assert!(b.layout_anim_started.is_some());
            assert!(
                b.needs_animation_frame(),
                "fresh reorder must request animation frames"
            );
        }

        // Case `insert_tab_triggers_layout_spring`.
        {
            let mut b = bar();
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            b.insert_tab(1, TabDescriptor::new(99, "NEW"));
            assert!(b.layout_anim_started.is_some());
            assert!(b.needs_animation_frame());
            // The new tab is *not* in the snapshot — it renders at target immediately,
            // guarded by `draw`'s `prev_tab_rects_by_id.get(...)`.
            assert!(!b.prev_tab_rects_by_id.contains_key(&99));
            assert!(b.prev_tab_rects_by_id.contains_key(&1));
        }

        // Case `remove_tab_triggers_layout_spring`.
        {
            let mut b = bar();
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            b.remove_tab(2);
            assert!(b.layout_anim_started.is_some());
            assert!(b.needs_animation_frame());
            // The removed id is in the snapshot too (captured before the remove),
            // which is harmless — no rendered tab looks it up.
            assert!(b.prev_tab_rects_by_id.contains_key(&1));
            assert!(b.prev_tab_rects_by_id.contains_key(&3));
        }

        // Case `insert_tab_records_inserted_at_for_fade_in`.
        {
            let mut b = bar();
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            assert!(!b.inserted_at.contains_key(&99), "precondition: no entry");
            b.insert_tab(1, TabDescriptor::new(99, "NEW"));
            assert!(
                b.inserted_at.contains_key(&99),
                "insert_tab must record a fade-in timestamp for the new tab id",
            );
            assert!(
                b.needs_animation_frame(),
                "insert fade must keep the redraw pump alive",
            );
        }

        // Case `insert_fade_clears_after_duration`.
        {
            // Backdate `inserted_at` so both `draw`'s retain and the timestamp check
            // here consider it expired, rather than waiting out the fade.
            let mut b = bar();
            let now = crate::time::now();
            b.inserted_at.insert(
                7,
                now.checked_sub(insert_fade() + Duration::from_millis(50))
                    .unwrap_or(now),
            );
            assert!(
                !b.needs_animation_frame(),
                "elapsed insert fade must not pump frames",
            );
        }

        // Case `layout_spring_clears_itself_after_duration`.
        {
            // Backdate `layout_anim_started` rather than waiting out the move.
            let mut b = bar();
            let now = crate::time::now();
            b.layout_anim_started = Some(
                now.checked_sub(layout_anim() + Duration::from_millis(50))
                    .unwrap_or(now),
            );
            assert!(
                !b.needs_animation_frame(),
                "elapsed animation must not request frames"
            );
        }
    }

    #[test]
    fn chip_glyphs_and_hover_tints_come_from_icons_and_tokens() {
        // Case `tab_close_glyph_draws_via_icons_module`.
        {
            assert_eq!(TabBar::close_glyph_icon_for_tests(), icons::Icon::X);
        }

        // Case `tab_max_glyph_draws_via_icons_module`.
        {
            assert_eq!(TabBar::max_glyph_icon_for_tests(), icons::Icon::Maximize2);
        }

        // Case `tab_search_glyph_draws_via_icons_module`.
        {
            assert_eq!(TabBar::search_glyph_icon_for_tests(), icons::Icon::Search);
        }

        // Case `close_hover_uses_danger_token_at_80_percent`.
        // A token path, not a hard-coded literal, so it tracks whatever danger
        // colour the active theme carries.
        {
            let mut b = bar();
            // The layout numbers don't matter — the chip-color decision only
            // consults hover state.
            b.rect = Rect::new(0.0, 0.0, 80.0, 40.0);
            b.tab_rects = vec![Rect::new(0.0, 6.0, 80.0, 34.0)];
            b.close_rects = vec![Rect::new(60.0, 10.0, 78.0, 30.0)];
            b.hover_idx = Some(0);
            b.hover_close_idx = Some(0);

            let theme = Theme::dark();
            let color = b
                .close_chip_color_for_tests(0, &theme)
                .expect("close-hover must produce a chip fill");
            let expected = theme.tokens.colors.semantic.danger.with_alpha_factor(0.8);
            assert_eq!(
                (color.r(), color.g(), color.b(), color.a()),
                (expected.r(), expected.g(), expected.b(), expected.a()),
                "close-hover must paint the theme danger token at 80% alpha",
            );

            b.hover_close_idx = None;
            let color = b
                .close_chip_color_for_tests(0, &theme)
                .expect("tab hover must still produce a subtle chip fill");
            assert_eq!(
                (color.r(), color.g(), color.b(), color.a()),
                (255, 255, 255, 28),
                "tab-only hover must stay as the subtle white tint",
            );

            b.hover_idx = None;
            assert!(
                b.close_chip_color_for_tests(0, &theme).is_none(),
                "idle close chip must not paint a fill",
            );
        }

        // Case `close_hover_glyph_is_full_white`.
        // `text_primary` can be slightly off-white under dark themes, which reads as
        // grayish against the red fill.
        {
            let mut b = bar();
            b.rect = Rect::new(0.0, 0.0, 80.0, 40.0);
            b.tab_rects = vec![Rect::new(0.0, 6.0, 80.0, 34.0)];
            b.close_rects = vec![Rect::new(60.0, 10.0, 78.0, 30.0)];
            b.hover_idx = Some(0);
            b.hover_close_idx = Some(0);

            let theme = Theme::dark();
            let color = b
                .close_glyph_color_for_tests(0, &theme)
                .expect("close glyph color must always be defined for a live tab");
            assert_eq!(
                (color.r(), color.g(), color.b(), color.a()),
                (255, 255, 255, 255),
                "close-hover glyph must be opaque white (concept: hover:text-white)",
            );
        }

        // Case `chip_hover_tint_is_derived_against_the_ground`.
        // A chip's hover wash is measured against the ground, so it is light
        // over a dark palette and dark over a light one — a literal white at
        // low alpha was invisible on the light palettes.
        {
            let ground = |theme: Theme| crate::chrome::strip_ground(&theme);
            let dark = chip_hover_tint(ground(Theme::dark()), ACTION_HOVER_TINT_ALPHA);
            let light = chip_hover_tint(ground(Theme::light()), ACTION_HOVER_TINT_ALPHA);
            assert_eq!(dark.a(), ACTION_HOVER_TINT_ALPHA);
            assert_eq!(light.a(), ACTION_HOVER_TINT_ALPHA);
            assert!(
                dark.r() > 128,
                "over a dark ground the wash is light: {dark:?}"
            );
            assert!(
                light.r() < 128,
                "over a light ground the wash is dark: {light:?}"
            );
        }
    }

    #[test]
    fn drag_ghost_follows_the_cursor_only_while_dragging_and_dims_the_source() {
        // Case `drag_ghost_paints_at_cursor_while_drag_in_flight`.
        {
            let mut b = ghost_test_bar();
            let mut handled = false;

            // Inside pill A, left of its search_rect (x=20), so the press isn't
            // intercepted as a search-icon hit.
            let press_cursor = Point::new(10.0, 15.0);
            dispatch_cursor(&mut b, press_cursor, &mut handled);

            // Step 2: Press on A.
            dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
            assert!(b.drag_offset.is_some(), "press must latch a drag_offset");
            assert!(
                b.ghost_rect().is_none(),
                "pre-threshold press must not paint a ghost",
            );

            let drag_cursor = Point::new(120.0, 60.0);
            dispatch_cursor(&mut b, drag_cursor, &mut handled);
            assert!(
                b.drag_in_flight(),
                "move past threshold must latch pending_drag",
            );

            // Ghost rect: top-left = last_cursor - drag_offset
            //   drag_offset was (10 - 0, 15 - 6) = (10, 9) at press time.
            //   last_cursor = (120, 60) after the threshold-crossing move.
            //   → ghost top-left = (120 - 10, 60 - 9) = (110, 51).
            //   → ghost size = source pill (A) = 80 x 28.
            let ghost = b
                .ghost_rect()
                .expect("drag-ghost must be present during drag");
            assert!(
                (ghost.x0 - 110.0).abs() < 0.01,
                "ghost x0 = cursor.x - offset.x (got {})",
                ghost.x0,
            );
            assert!(
                (ghost.y0 - 51.0).abs() < 0.01,
                "ghost y0 = cursor.y - offset.y (got {})",
                ghost.y0,
            );
            assert!(
                (ghost.width() - 80.0).abs() < 0.01,
                "ghost width tracks source pill width (got {})",
                ghost.width(),
            );
            assert!(
                (ghost.height() - 28.0).abs() < 0.01,
                "ghost height tracks source pill height (got {})",
                ghost.height(),
            );
            assert_eq!(
                TabBar::GHOST_PILL_ALPHA,
                0.5,
                "the lifted handle must remain half-transparent",
            );
            let reduced = b
                .drag_visual(&Theme::dark().with_reduce_motion(true))
                .expect("the same in-flight drag has a visual");
            assert_eq!(
                reduced.lift_progress, 1.0,
                "reduced motion keeps the tucked end state without animating to it",
            );
        }

        // Case `drag_ghost_clears_on_release`.
        // Otherwise the pill keeps painting at the last cursor position after the
        // drop commits — a visible ghost stuck at the edge of the screen.
        {
            let mut b = ghost_test_bar();
            let mut handled = false;

            // (10, 15): inside pill A, left of every icon rect so the press isn't
            // intercepted as a search/max/close hit.
            dispatch_cursor(&mut b, Point::new(10.0, 15.0), &mut handled);
            dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
            dispatch_cursor(&mut b, Point::new(120.0, 20.0), &mut handled);
            assert!(b.ghost_rect().is_some(), "precondition: ghost visible");

            // Position doesn't matter — every Released branch must clear the ghost.
            dispatch_left_button(&mut b, ElementState::Released, &mut handled);
            assert_eq!(
                b.ghost_rect(),
                None,
                "release must clear the ghost so the pill stops painting",
            );
            assert!(
                b.drag_offset.is_none(),
                "release must also clear the cached drag offset",
            );
        }

        // Case `drag_ghost_is_none_when_no_drag`.
        // Baseline the other drag tests implicitly rely on — if this regresses, the
        // ghost paint path fires on idle frames.
        {
            let b = bar();
            assert_eq!(b.ghost_rect(), None);
            assert!(b.drag_offset.is_none());
            assert!(!b.drag_in_flight());
        }

        // Case `source_pill_alpha_dims_while_dragged`.
        {
            let mut b = bar();
            assert!((b.pill_draw_alpha(0) - 1.0).abs() < f32::EPSILON);
            assert!((b.pill_draw_alpha(1) - 1.0).abs() < f32::EPSILON);
            assert!((b.pill_draw_alpha(2) - 1.0).abs() < f32::EPSILON);

            // Tab id 2 is index 1.
            b.pending_drag = Some(DragPayload::Tab { group: 0, tab: 2 });
            assert!(
                (b.pill_draw_alpha(0) - 1.0).abs() < f32::EPSILON,
                "non-source pill stays at full alpha",
            );
            assert!(
                (b.pill_draw_alpha(1) - TabBar::SOURCE_PILL_DIM_ALPHA).abs() < f32::EPSILON,
                "source pill dims to SOURCE_PILL_DIM_ALPHA (got {})",
                b.pill_draw_alpha(1),
            );
            assert!(
                (b.pill_draw_alpha(2) - 1.0).abs() < f32::EPSILON,
                "non-source pill stays at full alpha",
            );
            const {
                assert!(
                    TabBar::SOURCE_PILL_DIM_ALPHA < TabBar::GHOST_PILL_ALPHA,
                    "source dim must be more transparent than the ghost",
                );
            }
        }
    }

    /// Mirrors the hand-built rects other tests use to skip the taffy layout pass.
    fn holder() -> TabHolder {
        TabHolder {
            name: "Ben".into(),
            rgb: (200, 80, 40),
        }
    }

    fn move_to(b: &mut TabBar, x: f64, y: f64) {
        let mut handled = false;
        b.on_cursor_moved(winit::dpi::PhysicalPosition::new(x, y), 1.0, &mut handled);
    }

    #[test]
    fn someone_elses_tab_offers_no_close() {
        let mut b = bar_with_laid_out_rects();
        assert!(b.set_holder(2, Some(holder())));
        assert!(
            !b.set_holder(2, Some(holder())),
            "the same holder changes nothing"
        );
        // The pointer on its close glyph lights nothing and a press closes
        // nothing; it selects the tab like any press on it.
        move_to(&mut b, 155.0, 20.0);
        assert_eq!(b.hover_close_idx, None);
        let mut handled = false;
        b.on_left_press(&mut handled);
        assert_eq!(b.pending_close, None);
        assert!(b.closing_tabs.is_empty());
        assert_eq!(b.active_id(), 2);
        // Its own tab beside it closes as ever.
        let mut b = bar_with_laid_out_rects();
        b.set_holder(2, Some(holder()));
        move_to(&mut b, 69.0, 20.0);
        assert_eq!(b.hover_close_idx, Some(0));
    }

    #[test]
    fn a_drag_on_someone_elses_tab_pulls_it_a_little_and_springs_back() {
        let mut b = bar_with_laid_out_rects();
        b.set_holder(2, Some(holder()));
        move_to(&mut b, 88.0, 20.0);
        let mut handled = false;
        b.on_left_press(&mut handled);
        // Far past the drag threshold, and out of the bar.
        move_to(&mut b, 400.0, 220.0);
        assert!(b.pending_drag.is_none(), "it is not lifted");
        let (tab, pull) = b.resist_offset().expect("it is pulled");
        assert_eq!(tab, 2);
        assert!(pull.x > 0.0 && pull.x < f64::from(RESIST_REACH), "{pull:?}");
        assert!(pull.y > 0.0 && pull.y < f64::from(RESIST_REACH), "{pull:?}");
        let mut handled = false;
        b.finish_drag_release(&mut handled);
        assert!(handled, "the release is the tab's");
        assert_eq!(b.pending_detach, None);
        assert_eq!(b.pending_reorder, None);
        assert!(b.needs_animation_frame(), "it springs back");
        // Once back, nothing more moves.
        b.resist = b.resist.map(|resist| Resist {
            released_at: crate::time::now().checked_sub(resist_return() + Duration::from_millis(1)),
            ..resist
        });
        assert_eq!(b.resist_offset(), None);

        // Its own tab still lifts.
        let mut b = bar_with_laid_out_rects();
        b.set_holder(2, Some(holder()));
        move_to(&mut b, 6.0, 20.0);
        let mut handled = false;
        b.on_left_press(&mut handled);
        move_to(&mut b, 140.0, 20.0);
        assert!(b.pending_drag.is_some());
        assert_eq!(b.resist_offset(), None);
    }

    fn bar_with_laid_out_rects() -> TabBar {
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];
        b.close_rects = vec![
            Rect::new(60.0, 10.0, 78.0, 30.0),
            Rect::new(146.0, 10.0, 164.0, 30.0),
            Rect::new(232.0, 10.0, 250.0, 30.0),
        ];
        b.max_rects = vec![
            Rect::new(40.0, 10.0, 56.0, 30.0),
            Rect::new(126.0, 10.0, 142.0, 30.0),
            Rect::new(212.0, 10.0, 228.0, 30.0),
        ];
        b.search_rects = vec![
            Rect::new(20.0, 10.0, 36.0, 30.0),
            Rect::new(106.0, 10.0, 122.0, 30.0),
            Rect::new(192.0, 10.0, 208.0, 30.0),
        ];
        b
    }

    #[test]
    fn closing_tab_fades_out_without_hit_testing_or_keeping_focus() {
        // Case `is_closing_tracks_the_fade_until_finalize_close_and_cleans_up_sidecars`.
        {
            let mut b = bar_with_laid_out_rects();
            b.set_accent(2, vello::peniko::Color::from_rgba8(1, 2, 3, 255));
            b.set_modified(2, true);
            assert!(!b.is_closing(2), "precondition: nothing fading yet");

            b.start_closing(2);
            assert!(
                b.is_closing(2),
                "a reconciling caller must see the fade in flight so it doesn't \
             re-insert a still-open item on top of it",
            );
            assert!(!b.is_closing(3), "closing one id must not flag another");

            b.finalize_close(2);
            assert!(
                !b.is_closing(2),
                "finalize_close must clear the fade so a caller can insert a \
             genuinely-new pill for this id again",
            );
            assert_eq!(
                b.accent(2),
                None,
                "finalize_close must drop the dead tab's accent, same as remove_tab",
            );
            assert!(
                !b.is_modified(2),
                "finalize_close must drop the dead tab's modified mark too",
            );
        }

        // Case `closing_tab_still_renders_pill_during_fade`.
        {
            let mut b = bar_with_laid_out_rects();
            assert!(!b.has_closing(), "precondition: no closing pills");
            b.start_closing(2);
            assert!(
                b.has_closing(),
                "closing state must persist for draw to render the pill",
            );
            assert_eq!(
                b.closing_tab_ids(),
                vec![2],
                "draw walks this list to paint closing pills",
            );
            assert!(
                b.needs_animation_frame(),
                "close fade must pump redraws so the curve is visible",
            );
            let live: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
            assert_eq!(live, vec![1, 3], "closing tab is out of the live list");
        }

        // Case `closing_tab_active_shifts_to_neighbour_immediately`.
        {
            let mut b = bar_with_laid_out_rects();
            b.set_active(2);
            assert_eq!(b.active_id(), 2, "precondition: tab 2 is active");
            b.start_closing(2);
            assert_ne!(
                b.active_id(),
                2,
                "active must move off the closing tab at close-start",
            );
            assert!(
                b.active_id() == 1 || b.active_id() == 3,
                "active shifts to a live neighbour (got {})",
                b.active_id(),
            );
            // Logical promotion, not a cancellation of the fade.
            assert!(b.has_closing(), "close fade is still in flight");
        }

        // Case `closing_tab_is_not_hit_testable`.
        {
            let mut b = bar_with_laid_out_rects();
            b.set_active(1);
            b.start_closing(2);
            // Pill 2's old rect was [86,6,166,34]; survivors keep their original
            // rects in `tab_rects` until the next apply_layout, so index 1 now
            // matches live tab 3, not the closing tab.
            // Cursor over the closing pill's old close chip.
            let cursor = Point::new(155.0, 20.0);
            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(cursor.x, cursor.y),
            };
            dispatch(&mut b, &moved);
            // Pressing the closing pill's old close chip must NOT re-enter start_closing(2).
            let before_count = b.closing_tab_ids().len();
            let before_tabs: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
            let press = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            dispatch(&mut b, &press);
            // The closing set may stay the same or *add* tab 3 (its close chip
            // overlaps tab 2's old one here) — the invariant is that tab 2 isn't
            // re-added and the press didn't touch the in-flight fade.
            assert!(b.closing_tab_ids().contains(&2), "tab 2 is still fading");
            assert!(
                !b.closing_tab_ids()
                    .iter()
                    .filter(|id| **id == 2)
                    .count()
                    .gt(&1),
                "tab 2 must not be re-added to closing_tabs mid-fade",
            );
            assert!(
                !b.tabs().iter().any(|t| t.id == 2),
                "closing tab is never hit-testable back into the live list",
            );
            // Silence unused-binding warnings; the assertions above cover them.
            let _ = (before_count, before_tabs);
        }

        // Case `a_closing_pill_keeps_its_body_under_the_chips_it_is_still_painting`.
        {
            let theme = crate::theme::Theme::dark(); // motion on
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);

            hover_pill(&mut b, 1, width, &theme);
            std::thread::sleep(reveal_anim() + Duration::from_millis(40));
            lay_out_at_width_with(&mut b, width, &theme);
            assert!(b.close_rects[1].area() > 0.0);

            let mut handled = false;
            dispatch_cursor(&mut b, Point::new(-50.0, -50.0), &mut handled);
            lay_out_at_width_with(&mut b, width, &theme);

            assert_eq!(b.hover_idx, None, "the pointer has left the bar");
            assert!(
                b.close_rects[1].area() > 0.0,
                "the pill is still mid-shrink and still wide enough for its close chip",
            );
            assert!(
                b.close_affordance_visible_for_tests(1, &theme),
                "a chip that is still painted must still have a pill body under it",
            );
        }
    }

    #[test]
    fn close_glyph_paints_only_over_a_pill_body() {
        // Case `close_glyph_only_paints_on_pills_that_have_a_body`.
        // A `×` is never painted without a pill body behind it — see
        // [`close_affordance_visible`].
        {
            let theme = Theme::dark();
            let mut b = bar(); // tabs A(1) B(2) C(3), active = A.

            assert!(
                b.close_affordance_visible_for_tests(0, &theme),
                "the active pill has a body, so it keeps its ×",
            );
            for idle in [1usize, 2] {
                assert!(
                    !b.close_affordance_visible_for_tests(idle, &theme),
                    "idle inactive tab {idle} has a transparent body — a × there \
                 would float with no container around it",
                );
                assert!(
                    b.close_glyph_color_for_tests(idle, &theme).is_none(),
                    "no × painted ⇒ no glyph colour for tab {idle}",
                );
            }

            // The affordance is reachable via hover, just not permanent.
            b.hover_open_for_tests(1);
            assert!(
                b.close_affordance_visible_for_tests(1, &theme),
                "a hovered pill has a lifted body to host the ×",
            );
            assert!(
                !b.close_affordance_visible_for_tests(2, &theme),
                "hovering one tab must not reveal another tab's ×",
            );

            // Pressing does the same, so the glyph never blinks out mid-click.
            b.hover_idx = None;
            b.press_origin = Some((2, Point::ZERO));
            assert!(
                b.close_affordance_visible_for_tests(2, &theme),
                "a pressed pill paints the active-tier body, so it shows its ×",
            );
        }

        // Case `close_visibility_tracks_the_pill_body_in_every_state`.
        // The gate is derived from the body fill, so the two can never
        // disagree: a painted `×` implies a painted body, for every state
        // combination the appearance resolver can produce.
        {
            let theme = Theme::dark();
            for is_active in [false, true] {
                for is_hover in [false, true] {
                    for is_pressed in [false, true] {
                        let look = pill_appearance(
                            &theme,
                            crate::chrome::strip_ground(&theme),
                            is_active,
                            is_hover,
                            is_pressed,
                        );
                        assert_eq!(
                            close_affordance_visible(&look, false),
                            look.fill.a() > 0,
                            "close visibility must equal body visibility \
                         (active={is_active} hover={is_hover} pressed={is_pressed})",
                        );
                        assert!(
                            !close_affordance_visible(&look, true),
                            "a pinned pill is icon-only and never paints a ×",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chip_pointer_targets_are_sized_aligned_and_resolve_by_priority() {
        // Case `close_control_has_a_32px_target_without_enlarging_its_glyph`.
        {
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                ],
                1,
            );
            lay_out_at_width(&mut b, 640.0);

            let visual = b.close_rects[0];
            let hit = b.close_hit_rect(0).expect("active tab has a close target");
            assert_eq!(visual.width(), CLOSE_W, "the × visual stays compact");
            assert_eq!(visual.height(), CLOSE_W, "the × visual stays compact");
            assert!(
                hit.width() >= CLOSE_HIT_PX && hit.height() >= CLOSE_HIT_PX,
                "close requires a 32px pointer target, got {hit:?}",
            );
            assert!(
                hit.contains(visual.center()),
                "the pointer target must own the visible ×"
            );
            assert_eq!(TAB_BAR_HEIGHT, 44.0);
            assert_eq!(NEW_TAB_CHIP_PX, 32.0);
        }

        // Case `every_chip_shares_the_centre_line_and_one_hover_shape`.
        // The close, maximize and search chips all sit on the pill's centre
        // line, and a hovered action chip paints the same `CLOSE_W` disc the
        // close chip does — three chips, one shape.
        {
            let r = Rect::new(0.0, 6.0, 400.0, 38.0);
            let (close, max_rect, search) = chip_rects(
                r,
                CHIP_CLUSTER_W,
                false,
                CLOSE_W,
                CLOSE_INSET,
                ACTION_W,
                ACTION_W,
            );
            for (name, chip) in [("close", close), ("maximize", max_rect), ("search", search)] {
                assert!(chip.area() > 0.0, "{name} chip is laid out at full reveal");
                assert!(
                    (chip.center().y - r.center().y).abs() < 1e-9,
                    "{name} chip centre {} is off the pill centre {}",
                    chip.center().y,
                    r.center().y
                );
            }
            assert_eq!(close.width(), CLOSE_W);
            assert_eq!(close.height(), CLOSE_W);
            assert_eq!(max_rect.height(), r.height() - 2.0 * ACTION_HIT_INSET_Y);
            let ring = chip_hover_ring(max_rect);
            assert_eq!(ring.width(), CLOSE_W);
            assert_eq!(ring.height(), CLOSE_W);
            assert!((ring.center().x - max_rect.center().x).abs() < 1e-9);
            assert!((ring.center().y - max_rect.center().y).abs() < 1e-9);
            let search_ring = chip_hover_ring(search);
            assert_eq!(
                search_ring.size(),
                ring.size(),
                "one shape for both action chips"
            );
        }

        // Case `action_chips_have_a_minimum_pointer_target_centred_on_the_glyph`.
        // A maximize/search chip's pointer target is the theme's minimum
        // pointer target (32 px) centred on its 14 px glyph and inside the
        // bar, while the painted chip and the pill widths the layout pins stay
        // exactly where they were.
        {
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                ],
                1,
            );
            lay_out_at_width(&mut b, 640.0);
            let min = f64::from(
                crate::theme::Theme::dark()
                    .tokens
                    .metrics
                    .min_pointer_target,
            );
            let pill = Rect::new(
                100.0,
                b.rect.y0 + TAB_PILL_INSET_Y,
                500.0,
                b.rect.y1 - TAB_PILL_INSET_Y,
            );
            let (_, max_rect, search) = chip_rects(
                pill,
                CHIP_CLUSTER_W,
                false,
                CLOSE_W,
                CLOSE_INSET,
                ACTION_W,
                ACTION_W,
            );
            assert_eq!(
                max_rect.width(),
                ACTION_W,
                "the painted chip does not widen"
            );
            for (name, chip) in [("maximize", max_rect), ("search", search)] {
                let hit = b.action_hit_rect(chip);
                assert!(
                    hit.width() >= min && hit.height() >= min,
                    "{name} target {hit:?} is below the {min} px minimum"
                );
                assert!(
                    (hit.center().x - chip.center().x).abs() < 1e-9
                        && (hit.center().y - chip.center().y).abs() < 1e-9,
                    "{name} target is centred on its glyph"
                );
                assert!(
                    hit.y0 >= b.rect.y0 && hit.y1 <= b.rect.y1,
                    "{name} target stays inside the bar"
                );
                let beside = Point::new(chip.x0 - 4.0, chip.center().y);
                assert!(
                    !chip.contains(beside) && hit.contains(beside),
                    "a press beside the {name} glyph lands on its target"
                );
            }
            assert_eq!(
                b.action_hit_rect(Rect::ZERO),
                Rect::ZERO,
                "an unrevealed chip has no target"
            );
        }

        // Case `overlapping_action_targets_resolve_to_one_chip_by_priority`.
        // The enlarged targets overlap inside one pill; a hover or press in the
        // overlap resolves to one chip by priority (close, maximize, search),
        // and a point in the old 4 px dead gap between two glyphs now lands.
        {
            let mut b = bar();
            b.rect = Rect::new(0.0, 0.0, 252.0, 40.0);
            b.tab_rects = vec![
                Rect::new(0.0, 6.0, 80.0, 34.0),
                Rect::new(86.0, 6.0, 166.0, 34.0),
                Rect::new(172.0, 6.0, 252.0, 34.0),
            ];
            b.close_rects = vec![
                Rect::new(60.0, 10.0, 78.0, 30.0),
                Rect::new(146.0, 10.0, 164.0, 30.0),
                Rect::new(232.0, 10.0, 250.0, 30.0),
            ];
            b.max_rects = vec![
                Rect::new(40.0, 10.0, 56.0, 30.0),
                Rect::new(126.0, 10.0, 142.0, 30.0),
                Rect::new(212.0, 10.0, 228.0, 30.0),
            ];
            b.search_rects = vec![
                Rect::new(20.0, 10.0, 36.0, 30.0),
                Rect::new(106.0, 10.0, 122.0, 30.0),
                Rect::new(192.0, 10.0, 208.0, 30.0),
            ];
            let mut handled = false;
            // Between B's search glyph (ends at 122) and maximize glyph (starts
            // at 126): inside both enlarged targets — maximize wins.
            b.on_cursor_moved(
                winit::dpi::PhysicalPosition::new(124.0, 20.0),
                1.0,
                &mut handled,
            );
            assert_eq!(b.hover_max_idx, Some(1));
            assert_eq!(b.hover_search_idx, None, "one point lights one chip");
            // Left of the search glyph, inside its target only.
            b.on_cursor_moved(
                winit::dpi::PhysicalPosition::new(100.0, 20.0),
                1.0,
                &mut handled,
            );
            assert_eq!(b.hover_search_idx, Some(1));
            assert_eq!(b.hover_max_idx, None);
            // The press resolves the same way, from the authoritative rects.
            b.last_cursor = Point::new(124.0, 20.0);
            b.hover_idx = Some(1);
            b.on_left_press(&mut handled);
            assert_eq!(
                b.pending_max,
                Some(2),
                "the overlap press fires maximize on tab B"
            );
            assert_eq!(b.pending_search, None);
        }

        // Case `close_target_stays_off_the_maximize_glyph_and_glyphs_win_over_targets`.
        // On a live, hover-revealed pill the close target never covers the
        // painted maximize glyph, and a press on a painted glyph goes to that
        // chip whatever the enlarged targets around it would say — glyph
        // first, then targets by priority. Closing is destructive; it must
        // never be what a press on a visible secondary glyph does.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let width = 640.0;
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                ],
                1,
            );
            lay_out_at_width_with(&mut b, width, &theme);
            hover_pill(&mut b, 0, width, &theme);
            let max = b.max_rects()[0];
            let search = b.search_rects()[0];
            assert!(
                max.area() > 0.0 && search.area() > 0.0,
                "hovering the active pill reveals both action chips"
            );
            let close_hit = b
                .close_hit_rect(0)
                .expect("the active pill has a close target");
            assert!(
                close_hit.x0 >= max.x1,
                "close target {close_hit:?} starts after the maximize glyph {max:?}"
            );
            assert!(
                close_hit.width() >= CLOSE_HIT_PX - 1e-9,
                "and keeps its 32 px"
            );

            // At the even chip pitch the enlarged maximize target stops short
            // of the painted search glyph, so its trailing edge is search's
            // alone (the press resolution would let the glyph win anyway).
            let on_search_glyph = Point::new(search.x1 - 1.0, search.center().y);
            assert!(
                !b.action_hit_rect(max).contains(on_search_glyph),
                "the maximize target leaves the search glyph alone"
            );
            let mut handled = false;
            b.last_cursor = on_search_glyph;
            b.on_left_press(&mut handled);
            assert_eq!(
                b.pending_search.take(),
                Some(1),
                "a press on the search glyph searches"
            );
            assert_eq!(b.pending_max, None);

            // A press on the maximize glyph maximizes and never closes.
            b.last_cursor = max.center();
            b.on_left_press(&mut handled);
            assert_eq!(b.pending_max.take(), Some(1));
            assert_eq!(
                b.pending_close, None,
                "the destructive chip never takes a maximize press"
            );
        }

        // Case `press_with_stale_hover_idx_does_not_panic`.
        // `hover_idx` is populated by `CursorMoved` against the *previous* frame's
        // `tab_rects`; if a neighbouring widget's `handle_event` shrinks
        // `tabs`/`tab_rects` between move and press, indexing it can panic.
        {
            let mut b = bar();
            // Worst case: `tab_rects` is empty but `hover_idx` still points into an
            // arbitrary slot from the prior `CursorMoved`.
            b.tab_rects.clear();
            b.close_rects.clear();
            b.max_rects.clear();
            b.search_rects.clear();
            b.hover_idx = Some(7);
            // Must be set so the press handler's hit-tests run; the value itself
            // doesn't matter since the rect vecs are empty.
            b.last_cursor = Point::new(50.0, 20.0);

            let press = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            dispatch(&mut b, &press);
            assert_eq!(
                b.hover_idx, None,
                "stale hover_idx must be cleared when the rect cache no \
             longer covers it",
            );
            // Also guards the `pill = self.tab_rects[idx]` line below `origin`.
            assert!(b.press_origin.is_none());
            assert!(b.drag_offset.is_none());
        }
    }

    #[test]
    fn tab_bar_accessibility_names_role_value_and_tab_controls() {
        // Case `tab_bar_test_id_returns_tabs`.
        {
            let bar = TabBar::new("grp", vec![TabDescriptor::new(1, "a")], 1);
            assert_eq!(Widget::test_id(&bar), Some("tabs"));
        }

        // Case `accessibility_reports_tabbar_role_and_active_title`.
        {
            use crate::a11y::Role;
            // `bar()` builds tabs A/B/C with active_id = 1 ("A").
            let b = bar();
            let node = Widget::accessibility(&b).expect("tab bar is not presentational");
            assert_eq!(node.role, Role::TabBar);
            assert_eq!(node.name, None, "the bar has no intrinsic label");
            assert_eq!(
                node.value.as_deref(),
                Some("A"),
                "value is the active tab's title"
            );
            // Derived from the widget's own accessors, not a hardcoded assumption.
            assert_eq!(node.state.focusable, Widget::is_focusable(&b));
            assert_eq!(node.state.disabled, !Widget::is_enabled(&b));
        }

        // Case `accessibility_value_follows_active_tab`.
        {
            let mut b = bar();
            b.set_active(3); // "C"
            let node = Widget::accessibility(&b).unwrap();
            assert_eq!(
                node.value.as_deref(),
                Some("C"),
                "value tracks the active tab"
            );
        }

        // Case `accessible_new_and_close_controls_dispatch_the_same_tab_intents`.
        {
            let mut b = bar();
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_semantic_action(
                &mut b,
                "tab-new",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(b.take_new_tab().is_some());
            assert!(Widget::perform_semantic_action(
                &mut b,
                "tab-close:2",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert_eq!(b.take_close(), Some(2));
        }
    }

    #[test]
    fn keyboard_and_programmatic_selection_track_stable_tab_identity() {
        // Case `focused_tab_list_uses_standard_arrow_home_and_end_navigation`.
        {
            let mut b = bar();
            assert!(
                Widget::is_focusable(&b),
                "a non-empty tab strip is one Tab stop"
            );
            assert!(
                !b.handle_keyboard_navigation(&Key::Named(NamedKey::ArrowRight)),
                "unfocused tab strips must not steal the application's arrow keys"
            );

            Widget::set_focus(&mut b, true);
            assert!(b.handle_keyboard_navigation(&Key::Named(NamedKey::ArrowRight)));
            assert_eq!(b.active_id(), 2);
            assert_eq!(b.take_activation(), Some(2));
            assert_eq!(b.take_activation(), None, "activation is a one-shot edge");
            assert!(b.handle_keyboard_navigation(&Key::Named(NamedKey::End)));
            assert_eq!(b.active_id(), 3);
            assert!(b.handle_keyboard_navigation(&Key::Named(NamedKey::ArrowRight)));
            assert_eq!(b.active_id(), 1, "right wraps within the tab list");
            assert!(b.handle_keyboard_navigation(&Key::Named(NamedKey::Home)));
            assert_eq!(b.active_id(), 1);
            assert!(b.handle_keyboard_navigation(&Key::Named(NamedKey::ArrowLeft)));
            assert_eq!(b.active_id(), 3, "left wraps within the tab list");

            let node = Widget::accessibility(&b).expect("tab bar semantics");
            assert!(node.state.focusable);
            assert!(node.state.focused, "AT sees the same focus the ring paints");
        }

        // Case `synthetic_tab_activation_selects_by_stable_tab_id`.
        {
            let mut b = bar();
            assert!(Widget::perform_semantic_action(
                &mut b,
                "tab:3",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(b.active_id(), 3);
            assert_eq!(b.take_activation(), Some(3));
            assert!(!Widget::perform_semantic_action(
                &mut b,
                "tab:999",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
        }

        // Case `programmatic_selection_does_not_masquerade_as_user_activation`.
        {
            let mut b = bar();
            b.set_active(3);
            assert_eq!(b.active_id(), 3);
            assert_eq!(
                b.take_activation(),
                None,
                "model reconciliation must not steal desktop focus"
            );
            b.insert_tab(1, TabDescriptor::new(9, "New"));
            assert_eq!(b.active_id(), 9);
            assert_eq!(
                b.take_activation(),
                None,
                "a programmatic insert is not a click"
            );
        }

        // Case `keyboard_focus_reveals_active_actions_immediately`.
        {
            let theme = Theme::dark().with_reduce_motion(true);
            let mut b = four_tab_bar();
            Widget::set_focus(&mut b, true);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            assert!(b.max_rects()[0].area() > 0.0);
            assert!(b.search_rects()[0].area() > 0.0);
            let a11y = Widget::accessibility(&b).expect("tab bar accessibility");
            assert!(a11y.state.focused);
        }

        // Case `group_focus_is_distinct_from_active_tab_identity`.
        {
            let mut bar = four_tab_bar();
            assert_eq!(bar.group_focus, GroupFocus::Focused);
            let active = bar.active_id;
            bar.set_group_focused(false);
            assert_eq!(bar.active_id, active, "focus must not select another tab");
            assert_eq!(bar.group_focus, GroupFocus::Unfocused);
        }
    }

    /// The shape a window group imposes on its tab strip. `MAX_CONTENT` (used by
    /// the older layout tests) gives the bar unbounded width, which is precisely
    /// the case that cannot show an overflow.
    fn lay_out_at_width(b: &mut TabBar, width: f64) {
        lay_out_at_width_with(b, width, &crate::theme::Theme::dark());
    }

    /// Reduce-motion is the interesting theme: it's the production path that
    /// lands the reveal on target in a single pass, so a test wanting settled
    /// geometry can have it without sleeping through the curve.
    // Test-only fixture widths/heights are small, caller-chosen literals (a few
    // hundred px at most) — nowhere near `f32`'s range limits.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test fixture dimensions are small literal px values"
    )]
    fn lay_out_at_width_with(b: &mut TabBar, width: f64, theme: &crate::theme::Theme) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Style};

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = b.build_layout(&mut taffy, theme, &mut node_map);
        // Column, like `WindowGroup::build_layout`: the bar declares `width:
        // auto`, so it only takes the group's width as a column's cross axis. A
        // row parent would leave it content-sized — zero, for a leaf with no
        // measure func — a shape no live group has.
        let root = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: taffy::Size {
                    width: Dimension::length(width as f32),
                    height: Dimension::length(TAB_BAR_HEIGHT as f32),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(width as f32),
                    height: AvailableSpace::Definite(TAB_BAR_HEIGHT as f32),
                },
            )
            .unwrap();
        b.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn a_retitled_pill_keeps_its_measured_width_until_the_new_title_is_measured() {
        let theme = crate::theme::Theme::dark();
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
        b.title_advance
            .insert(1, ("Notes".into(), theme.text(TextRole::Label), 60.0));
        lay_out_at_width(&mut b, 900.0);
        let measured = b.tab_rects[0].width();
        // A client retitles the window: the character-count guess for the
        // new title would be far wider, and the pill must not jump to it.
        b.tabs[0].title = "a considerably longer title than before".into();
        lay_out_at_width(&mut b, 900.0);
        assert!(
            (b.tab_rects[0].width() - measured).abs() < 0.01,
            "pill jumped from {measured} to {} before the new title was measured",
            b.tab_rects[0].width()
        );
        // Once `draw` has measured a different advance, the bar asks for a
        // layout instead of keeping the stale width.
        assert!(!b.is_layout_dirty());
        b.advance_changed = true;
        assert!(b.is_layout_dirty());
        lay_out_at_width(&mut b, 900.0);
        assert!(!b.is_layout_dirty());
    }

    #[test]
    fn narrow_groups_keep_pills_and_chips_inside_the_bar() {
        // Case `pills_stay_inside_the_bar_when_the_group_is_narrow`.
        // The strip's clip is at the row, not the group, so nothing downstream
        // catches an overflow.
        {
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                    TabDescriptor::new(3, "Terminal"),
                    TabDescriptor::new(4, "Browser"),
                ],
                1,
            );
            let width = 380.0;
            lay_out_at_width(&mut b, width);

            let bar_rect = b.bar_rect();
            assert_eq!(
                bar_rect.width(),
                width,
                "harness must impose the group width"
            );
            for (i, r) in b.tab_rects().iter().enumerate() {
                assert!(
                    r.x1 <= bar_rect.x1 + 0.5,
                    "pill {i} ({}) ends at {} — past the group's right edge at {}; \
                 it would paint over the neighbouring group",
                    b.tabs()[i].title,
                    r.x1,
                    bar_rect.x1,
                );
            }
        }

        // Case `close_chips_stay_inside_the_bar_when_the_group_is_narrow`.
        // The close chip is inset from its pill's right edge, so a pill that only
        // just overflows can still throw a stray × into the gap between groups.
        {
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "file-explorer"),
                    TabDescriptor::new(3, "Terminal"),
                ],
                1,
            );
            let width = 360.0;
            lay_out_at_width(&mut b, width);

            let bar_rect = b.bar_rect();
            for (i, r) in b.close_rects.iter().enumerate() {
                if r.area() == 0.0 {
                    continue; // pinned pills carry no close rect
                }
                assert!(
                    r.x1 <= bar_rect.x1 + 0.5,
                    "close chip {i} ends at {} — past the group's right edge at {}",
                    r.x1,
                    bar_rect.x1,
                );
            }
        }

        // Case `a_pill_opening_in_a_narrow_group_stays_inside_it`.
        // Growth comes out of headroom the row held back, so this must hold even
        // mid-handover between two pills, when both are briefly part-open.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            for width in [300.0_f64, 380.0, 460.0, 620.0] {
                let mut b = four_tab_bar();
                lay_out_at_width_with(&mut b, width, &theme);
                let idle: Vec<Rect> = b.tab_rects().to_vec();
                for idx in 0..4 {
                    hover_pill(&mut b, idx, width, &theme);
                    // Must hold under a squeeze too, not only where there's slack —
                    // that's exactly where a re-fit under the pointer would be tempting.
                    for (i, ir) in idle.iter().enumerate().take(idx + 1) {
                        assert!(
                            (b.tab_rects()[i].x0 - ir.x0).abs() < 0.01,
                            "at {width} px with pill {idx} open, pill {i}'s leading edge \
                         moved from {} to {}",
                            ir.x0,
                            b.tab_rects()[i].x0,
                        );
                    }
                    for (i, ir) in idle.iter().enumerate().take(idx) {
                        assert!(
                            (b.tab_rects()[i].width() - ir.width()).abs() < 0.01,
                            "at {width} px, pill {i} was re-sized ({} -> {}) because the \
                         pointer moved onto pill {idx}",
                            ir.width(),
                            b.tab_rects()[i].width(),
                        );
                    }
                    let bar = b.bar_rect();
                    for (i, r) in b.tab_rects().iter().enumerate() {
                        assert!(
                            r.x1 <= bar.x1 + 0.5,
                            "at {width} px with pill {idx} open, pill {i} ends at {} \
                         past the group edge at {}",
                            r.x1,
                            bar.x1,
                        );
                    }
                    for (i, r) in b.close_rects.iter().enumerate() {
                        if r.area() == 0.0 {
                            continue;
                        }
                        assert!(
                            r.x1 <= bar.x1 + 0.5,
                            "at {width} px with pill {idx} open, close chip {i} \
                         escapes the group at {}",
                            r.x1,
                        );
                    }
                }
            }
        }

        // Case `a_squeezed_pill_gives_up_chips_before_it_gives_up_its_name`.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let mut tr = TextRenderer::new();
            let mut b = TabBar::new(
                "g1",
                vec![
                    TabDescriptor::new(1, "Notes"),
                    TabDescriptor::new(2, "Files"),
                    TabDescriptor::new(3, "Terminal"),
                    TabDescriptor::new(4, "Browser"),
                    TabDescriptor::new(5, "Settings"),
                    TabDescriptor::new(6, "Inspector"),
                ],
                1,
            );
            // Past what the row can show at resting width, so the fit has to squeeze.
            let width = 460.0;
            lay_out_at_width_with(&mut b, width, &theme);
            assert!(
                b.tab_rects()
                    .iter()
                    .enumerate()
                    .any(|(i, r)| r.width() + 0.5
                        < b.natural_pill_width(i, b.resting_reveal(i).reserve())),
                "the fixture stopped squeezing — pick more tabs or a narrower group, \
             otherwise this test no longer covers what it claims to",
            );

            for idx in 0..b.tabs().len() {
                hover_pill(&mut b, idx, width, &theme);
                for i in 0..b.tabs().len() {
                    let title = b.tabs()[i].title.clone();
                    let shown = b
                        .visible_title(i, &mut tr, &theme)
                        .unwrap_or_else(|| panic!("pill {i} ({title}) renders no title at all"));
                    let stem = shown.trim_end_matches('\u{2026}');
                    assert!(
                        stem.chars().count() >= 3,
                        "with pill {idx} open, pill {i} ({title}) renders {shown:?} — a \
                     squeezed pill must still carry enough of its name to be recognised",
                    );
                    assert!(
                        title.starts_with(stem),
                        "pill {i} renders {shown:?}, which is not a prefix of {title:?}",
                    );
                }
            }
        }
    }

    /// Aims at the pill's leading third so the cursor never lands on a chip,
    /// then lays out once the hover dwell has run.
    fn hover_pill(b: &mut TabBar, idx: usize, width: f64, theme: &crate::theme::Theme) {
        let r = b.tab_rects()[idx];
        let p = Point::new(r.x0 + r.width() * 0.25, r.center().y);
        let mut handled = false;
        dispatch_cursor(b, p, &mut handled);
        assert!(b.elapse_hover_dwell(), "resting on the pill opens it");
        lay_out_at_width_with(b, width, theme);
    }

    fn dispatch_cursor(b: &mut TabBar, p: Point, handled: &mut bool) {
        let (taffy, node) = built_layout(b);
        *handled = Dispatch::new()
            .handled(*handled)
            .layout(&taffy, node)
            .send(
                b,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(p.x, p.y),
                },
            )
            .handled;
    }

    /// Dispatches a left `MouseInput` event at the given press/release state —
    /// shared by every test that drives a click through `handle_event` without
    /// a real pointer.
    fn dispatch_left_button(b: &mut TabBar, state: ElementState, handled: &mut bool) {
        let (taffy, node) = built_layout(b);
        *handled = Dispatch::new()
            .handled(*handled)
            .layout(&taffy, node)
            .send(
                b,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state,
                    button: MouseButton::Left,
                },
            )
            .handled;
    }

    fn built_layout(b: &mut TabBar) -> (TaffyTree<()>, NodeId) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = b.build_layout(&mut taffy, &crate::theme::Theme::dark(), &mut node_map);
        (taffy, node)
    }

    fn browser_slots() -> crate::window_toolbar::WindowToolbar {
        use crate::window_toolbar::{
            ToolbarAction, ToolbarLocation, ToolbarNavigation, WindowToolbar,
        };
        WindowToolbar {
            sidebar: None,
            navigation: Some(ToolbarNavigation {
                back: true,
                forward: false,
            }),
            location: Some(ToolbarLocation {
                label: "duckduckgo.com".into(),
                value: "https://duckduckgo.com/".into(),
                editable: true,
                name: "Address and search".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![ToolbarAction {
                id: "reload".into(),
                icon: "refresh".into(),
                name: "Reload".into(),
                enabled: true,
                on: false,
            }],
        }
    }

    fn send_key(b: &mut TabBar, key: Key, text: Option<&str>) -> bool {
        let (taffy, node) = built_layout(b);
        let event = crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            key,
            text.map(Into::into),
            ElementState::Pressed,
        ));
        Dispatch::new().layout(&taffy, node).send(b, &event).handled
    }

    // USER REQUIREMENT (2026-10-19): "apps shouldnt be responsible for
    // rendering a top bar" -- the browser's address is its editable location
    // in the window's bar: a click edits the whole address with a go button,
    // Enter commits what was typed to the app, Esc puts it back.
    #[test]
    fn the_bar_edits_an_apps_location_and_commits_it_to_that_tab() {
        let _settled = crate::motion::testing::settled();
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
        b.set_window_controls(true);
        assert!(b.set_toolbar(1, Some(browser_slots())));
        assert!(!b.set_toolbar(1, Some(browser_slots())), "unchanged");
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(
            (b.bar_height() - TAB_BAR_HEIGHT).abs() < f64::EPSILON,
            "one row"
        );
        let location = b.location_rect();
        assert!(location.area() > 0.0);
        assert!(
            location.x1 <= b.window_controls().maximize.x0,
            "the location stops before the window's controls"
        );
        assert!(
            b.tab_rects()[0].width() <= PINNED_W + 0.5,
            "the title is the icon"
        );

        let mut handled = false;
        dispatch_cursor(&mut b, location.center(), &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        assert!(
            handled && b.editing_location(),
            "a click edits the location"
        );
        assert_eq!(
            b.toolbar_row.field_text(),
            Some("https://duckduckgo.com/"),
            "the whole address, not the domain"
        );
        assert!(send_key(&mut b, Key::Named(NamedKey::Escape), None));
        assert!(!b.editing_location(), "Esc puts the location back");
        assert!(b.take_toolbar_events().is_empty());

        assert!(b.edit_location(), "Ctrl+L edits it too");
        for ch in "example.org".chars() {
            let text = ch.to_string();
            assert!(send_key(
                &mut b,
                Key::Character(text.as_str().into()),
                Some(&text)
            ));
        }
        assert!(send_key(&mut b, Key::Named(NamedKey::Enter), None));
        assert!(!b.editing_location());
        assert_eq!(
            b.take_toolbar_events(),
            vec![(
                1,
                crate::window_toolbar::ToolbarEvent::Commit {
                    text: "example.org".into()
                }
            )],
            "typing replaced the selected address and Enter committed it"
        );
    }

    /// Browser slots with more actions than the row shows.
    fn crowded_slots() -> crate::window_toolbar::WindowToolbar {
        let mut slots = browser_slots();
        for (id, icon, name) in [
            ("bookmark", "star", "Bookmark this page"),
            ("library", "history", "History and bookmarks"),
            ("downloads", "download", "Downloads"),
            ("hidden", "eye", "Show hidden"),
            ("new-folder", "folder-plus", "New folder"),
        ] {
            slots.actions.push(crate::window_toolbar::ToolbarAction {
                id: id.into(),
                icon: icon.into(),
                name: name.into(),
                enabled: true,
                on: false,
            });
        }
        slots
    }

    /// At most three actions stand in the row, fewer when the location
    /// would be squeezed; the rest are in the overflow button's menu, which
    /// runs them, and every button names its outcome in a tooltip.
    #[test]
    fn actions_past_the_rows_room_are_in_an_overflow_menu_that_runs_them() {
        let _settled = crate::motion::testing::settled();
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
        b.set_window_controls(true);
        b.set_toolbar(1, Some(crowded_slots()));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let (_, _, location, actions) = b.toolbar_rects();
        assert_eq!(actions.len(), crate::window_toolbar::MAX_ACTIONS);
        let more = b.toolbar_row.slots().overflow;
        assert!(more.area() > 0.0, "six actions do not fit in the row");
        assert!(
            actions
                .iter()
                .all(|a| a.x1 <= more.x0 && a.x0 >= location.x1)
        );
        assert!(more.x1 <= b.window_controls().maximize.x0);

        // Tooltips: the slots' buttons and the window's controls.
        let lookup = crate::i18n::I18nManager::lookup_plain;
        let tip = |b: &TabBar, r: Rect| b.tooltip_target_at(r.center()).map(|(_, t)| t);
        assert_eq!(tip(&b, actions[0]), Some("Reload".to_owned()));
        assert_eq!(tip(&b, more), Some(lookup("window-toolbar-more")));
        assert_eq!(
            tip(&b, b.window_controls().close),
            Some(lookup("host-window-close"))
        );
        assert_eq!(tip(&b, location), None, "the location is no button");

        // The menu lists the actions the row has no room for and runs one.
        b.set_popup_area(Rect::new(0.0, 0.0, 900.0, 700.0));
        let mut handled = false;
        dispatch_cursor(&mut b, more.center(), &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        dispatch_left_button(&mut b, ElementState::Released, &mut handled);
        assert!(b.toolbar_row.popup_open(), "the overflow menu opened");
        assert!(send_key(&mut b, Key::Named(NamedKey::ArrowDown), None));
        assert!(send_key(&mut b, Key::Named(NamedKey::Enter), None));
        assert_eq!(
            b.take_toolbar_events(),
            vec![(
                1,
                crate::window_toolbar::ToolbarEvent::Action {
                    id: "downloads".into()
                }
            )],
            "the first action past the row's three"
        );
        assert!(!b.toolbar_row.popup_open());

        // Narrow, the location keeps its room and the actions go first.
        lay_out_at_width_with(&mut b, 420.0, &theme);
        let (_, _, narrow_location, narrow_actions) = b.toolbar_rects();
        assert!(narrow_actions.len() < crate::window_toolbar::MAX_ACTIONS);
        assert!(narrow_location.area() > 0.0);
        assert!(b.toolbar_row.slots().overflow.area() > 0.0);
    }

    /// A location with levels is a breadcrumb: a click on a level commits
    /// it; a click past them edits the whole path.
    #[test]
    fn a_locations_levels_are_a_breadcrumb_whose_levels_commit() {
        use crate::window_toolbar::LocationSegment;
        let _settled = crate::motion::testing::settled();
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut slots = browser_slots();
        if let Some(location) = slots.location.as_mut() {
            location.label = "Downloads".into();
            location.value = "~/Downloads".into();
            location.segments = vec![
                LocationSegment {
                    label: "Home".into(),
                    value: "~".into(),
                },
                LocationSegment {
                    label: "Downloads".into(),
                    value: "~/Downloads".into(),
                },
            ];
        }
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Downloads")], 1);
        b.set_window_controls(true);
        b.set_toolbar(1, Some(slots));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        // The breadcrumb lays its levels out as it is painted.
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        b.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        let location = b.location_rect();
        let home = (0..900_u32)
            .map(|x| Point::new(f64::from(x), location.center().y))
            .filter(|p| location.contains(*p))
            .find(|p| b.toolbar_row.crumb_at_for_tests(*p).is_some())
            .expect("the Home level is painted in the location");
        let mut handled = false;
        dispatch_cursor(&mut b, home, &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        assert!(
            !b.editing_location(),
            "a level goes there, it does not edit"
        );
        assert_eq!(
            b.take_toolbar_events(),
            vec![(
                1,
                crate::window_toolbar::ToolbarEvent::Commit { text: "~".into() }
            )]
        );
        let past = Point::new(location.x1 - 40.0, location.center().y);
        dispatch_cursor(&mut b, past, &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        assert!(b.editing_location());
        assert_eq!(b.toolbar_row.field_text(), Some("~/Downloads"));
    }

    /// While the location is edited the app hears its text (apart from what
    /// the person did) and its suggestions are listed under the field; the
    /// arrows choose one and Enter commits its value.
    #[test]
    fn the_app_hears_the_edited_text_and_its_suggestions_commit() {
        use crate::window_toolbar::{LocationSuggestion, ToolbarEvent};
        let _settled = crate::motion::testing::settled();
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
        b.set_window_controls(true);
        b.set_toolbar(1, Some(browser_slots()));
        b.set_popup_area(Rect::new(0.0, 0.0, 900.0, 700.0));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(b.edit_location());
        assert_eq!(
            b.take_location_drafts(),
            vec![(
                1,
                ToolbarEvent::Edit {
                    text: "https://duckduckgo.com/".into()
                }
            )],
            "the app hears what the edit starts from"
        );
        for ch in "ru".chars() {
            let text = ch.to_string();
            assert!(send_key(
                &mut b,
                Key::Character(text.as_str().into()),
                Some(&text)
            ));
        }
        assert_eq!(
            b.take_location_drafts(),
            vec![(1, ToolbarEvent::Edit { text: "ru".into() })],
            "only the latest text"
        );
        assert!(b.take_toolbar_events().is_empty(), "typing is no commit");

        let mut answered = browser_slots();
        if let Some(location) = answered.location.as_mut() {
            location.suggestions = vec![
                LocationSuggestion {
                    label: "rust".into(),
                    value: "rust".into(),
                    icon: Some("search".into()),
                },
                LocationSuggestion {
                    label: "Rust Programming Language".into(),
                    value: "https://www.rust-lang.org/".into(),
                    icon: Some("history".into()),
                },
            ];
        }
        b.set_toolbar(1, Some(answered));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(b.toolbar_row.popup_open(), "the suggestions are listed");
        assert!(b.editing_location(), "and the field still has the keyboard");
        assert!(send_key(&mut b, Key::Named(NamedKey::ArrowDown), None));
        assert!(send_key(&mut b, Key::Named(NamedKey::ArrowDown), None));
        assert!(send_key(&mut b, Key::Named(NamedKey::Enter), None));
        assert!(!b.editing_location());
        assert_eq!(
            b.take_toolbar_events(),
            vec![(
                1,
                ToolbarEvent::Commit {
                    text: "https://www.rust-lang.org/".into()
                }
            )]
        );
    }

    /// A page's favicon stands where the app's icon would.
    #[test]
    fn the_locations_picture_stands_in_for_the_apps_icon() {
        let mut slots = browser_slots();
        let png = crate::png::encode_rgba8(2, 2, &[255; 16]).unwrap();
        if let Some(location) = slots.location.as_mut() {
            location.image = Some(crate::window_toolbar::ToolbarImage { png });
        }
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
        b.set_window_controls(true);
        b.set_toolbar(1, Some(slots));
        let tab = b.tabs()[0].clone();
        let image = b.tab_image(&tab).expect("the favicon");
        assert!(image.pixels(16).is_some());
        b.set_toolbar(1, Some(browser_slots()));
        assert!(b.tab_image(&tab).is_none(), "no favicon, no picture");
    }

    /// Tabbed, the slots take a row of their own under the tabs, and the
    /// window's controls stand exactly where they stand untabbed.
    #[test]
    fn tabbed_the_slots_take_a_row_under_the_tabs_and_the_controls_stay() {
        let _settled = crate::motion::testing::settled();
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut solo = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
        solo.set_window_controls(true);
        solo.set_toolbar(1, Some(browser_slots()));
        lay_out_at_width_with(&mut solo, 900.0, &theme);
        let mut tabbed = TabBar::new(
            "g1",
            vec![
                TabDescriptor::new(1, "DuckDuckGo"),
                TabDescriptor::new(2, "Files"),
            ],
            1,
        );
        tabbed.set_window_controls(true);
        tabbed.set_toolbar(1, Some(browser_slots()));
        lay_out_at_width_with(&mut tabbed, 900.0, &theme);
        assert!((tabbed.bar_height() - (TAB_BAR_HEIGHT + TOOLBAR_ROW_HEIGHT)).abs() < f64::EPSILON);
        assert_eq!(tabbed.window_controls(), solo.window_controls());
        let (back, _, location, _) = tabbed.toolbar_rects();
        assert!(
            back.y0 >= tabbed.get_rect().y0 + TAB_BAR_HEIGHT - 0.5,
            "under the tabs"
        );
        assert!(
            location.width() > solo.location_rect().width(),
            "across the row"
        );
        assert!(
            tabbed.tab_rects()[0].width() > PINNED_W,
            "tabbed, the tab shows its label"
        );

        // A tab without slots: one row again, no slot rects.
        tabbed.set_active(2);
        lay_out_at_width_with(&mut tabbed, 900.0, &theme);
        assert!((tabbed.bar_height() - TAB_BAR_HEIGHT).abs() < f64::EPSILON);
        assert!(!tabbed.toolbar_hit(location.center()));
    }

    fn four_tab_bar() -> TabBar {
        TabBar::new(
            "g1",
            vec![
                TabDescriptor::new(1, "Notes"),
                TabDescriptor::new(2, "Files"),
                TabDescriptor::new(3, "Terminal"),
                TabDescriptor::new(4, "Browser"),
            ],
            1,
        )
    }

    /// Runs `f` with the toolkit clock reading `at`, so pointer samples and
    /// layout passes see one synthetic timeline.
    fn at_time<T>(at: Instant, f: impl FnOnce() -> T) -> T {
        crate::time::testing::with_monotonic_time(at, f)
    }

    /// A pointer sample at `p`, `ms` after `t0`, followed by the layout pass
    /// of the frame it lands in.
    fn sample_at(b: &mut TabBar, t0: Instant, ms: u64, p: Point, theme: &Theme) {
        let at = t0 + Duration::from_millis(ms);
        at_time(at, || {
            let mut handled = false;
            dispatch_cursor(b, p, &mut handled);
            lay_out_at_width_with(b, 900.0, theme);
        });
    }

    fn layout_at(b: &mut TabBar, t0: Instant, ms: u64, theme: &Theme) {
        at_time(t0 + Duration::from_millis(ms), || {
            lay_out_at_width_with(b, 900.0, theme);
        });
    }

    fn chips_open(b: &TabBar, i: usize) -> bool {
        b.max_rects()[i].area() > 0.0 && b.search_rects()[i].area() > 0.0
    }

    /// Points along a straight line from `a` to `b`, `n` steps, excluding `a`.
    fn path(a: Point, b: Point, n: u32) -> Vec<Point> {
        (1..=n)
            .map(|k| a.lerp(b, f64::from(k) / f64::from(n)))
            .collect()
    }

    /// A bar with pill 1 opened by a pointer resting on its leading third,
    /// and the instant (after the dwell) it opened at.
    fn bar_with_pill_one_open(theme: &Theme) -> (TabBar, Instant) {
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, theme);
        let r = b.tab_rects()[1];
        let t0 = crate::time::now();
        sample_at(&mut b, t0, 0, Point::new(r.x0 + 12.0, r.center().y), theme);
        let dwell = u64::try_from(crate::widget::hover_intent::DWELL.as_millis()).unwrap();
        layout_at(&mut b, t0, dwell, theme);
        assert!(chips_open(&b, 1), "resting on the pill opens it");
        (b, t0 + Duration::from_millis(dwell))
    }

    /// Resting on a pill opens it after a short dwell, counted from when the
    /// pointer entered: slow motion inside the pill does not restart it (the
    /// old gate re-armed on every sample, so chips came late), and the dwell
    /// is shorter than the old 200 ms one. Until it runs out the bar keeps
    /// frames coming, so a pointer that stops dead still opens the pill.
    #[test]
    fn resting_on_a_pill_opens_it_after_a_short_dwell_from_entry() {
        use crate::widget::hover_intent::DWELL;
        assert!(DWELL < Duration::from_millis(200), "shorter than before");
        let dwell = u64::try_from(DWELL.as_millis()).unwrap();
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let idle = b.tab_rects()[1].width();
        let r = b.tab_rects()[1];
        let t0 = crate::time::now();

        // Slow drift inside the pill's body, one pixel every 10 ms, leftward:
        // nothing this pill's opening would move lies that way, so the drift
        // is not heading for a control.
        let mut ms = 0;
        while ms < dwell {
            let x = r.x0 + 22.0 - f64::from(u32::try_from(ms / 10).unwrap());
            sample_at(&mut b, t0, ms, Point::new(x, r.center().y), &theme);
            assert!(
                !chips_open(&b, 1),
                "opened {ms} ms in, before the dwell ran"
            );
            assert!(
                b.next_wake_deadline().is_some(),
                "a pending dwell wakes the host when it is due"
            );
            ms += 10;
        }
        layout_at(&mut b, t0, dwell, &theme);
        assert!(
            chips_open(&b, 1),
            "the dwell counts from entry, not the last sample"
        );
        assert!(b.tab_rects()[1].width() > idle + 1.0);
        let quiet = t0 + Duration::from_millis(dwell) + crate::widget::hover_intent::SETTLE;
        assert_eq!(
            b.hover_intent.deadline(quiet),
            None,
            "nothing left to wake for"
        );
    }

    /// A pointer sweeping across the row (to reach a window below, say)
    /// spends less than the dwell over each pill, so none of them opens and
    /// nothing reflows under it.
    #[test]
    fn a_fast_sweep_across_the_row_opens_no_pill() {
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let idle: Vec<Rect> = b.tab_rects().to_vec();
        let y = idle[0].center().y;
        let from = Point::new(idle[0].x0 + 2.0, y);
        let to = Point::new(idle[3].x1 + 40.0, y);
        let t0 = crate::time::now();
        // A sample per 8 ms frame at 3 px/ms, a brisk but ordinary sweep.
        let steps = (1_u32..=1_000)
            .find(|n| f64::from(*n) * 24.0 >= to.x - from.x)
            .expect("a finite row");
        for (k, p) in path(from, to, steps).into_iter().enumerate() {
            let ms = u64::try_from(k).unwrap() * 8;
            sample_at(&mut b, t0, ms, p, &theme);
            for i in 0..4 {
                assert!(
                    !chips_open(&b, i),
                    "pill {i} opened under a sweep at {ms} ms"
                );
            }
            assert_eq!(b.tab_rects(), idle.as_slice(), "the row reflowed mid-sweep");
        }
        // Off the row: nothing is pending, nothing opens later.
        let end = u64::from(steps) * 8;
        sample_at(&mut b, t0, end + 8, Point::new(to.x, 400.0), &theme);
        layout_at(&mut b, t0, end + 2000, &theme);
        assert!((0..4).all(|i| !chips_open(&b, i)));
    }

    /// Menu aim: once a pill is open, a pointer heading for its chips keeps
    /// it open even where the path leaves the pill (here it grazes the strip
    /// of bar under the pill). Holds on a fast path (few, far-apart samples)
    /// and a slow one (many samples, each well inside the hold).
    #[test]
    fn an_open_pill_holds_while_the_pointer_heads_for_its_chips() {
        let theme = Theme::dark().with_reduce_motion(true);
        for (name, steps, frame_ms) in [("fast", 2_u32, 8_u64), ("slow", 24, 20)] {
            let (mut b, opened) = bar_with_pill_one_open(&theme);
            let open_w = b.tab_rects()[1].width();
            let r = b.tab_rects()[1];
            let chip = b.max_rects()[1].center();
            // From where it rested, down-right out under the pill's bottom
            // edge, then up onto the maximize chip.
            let start = Point::new(r.x0 + 12.0, r.center().y);
            let dip = Point::new(chip.x - 30.0, r.y1 + 3.0);
            assert!(dip.y < b.rect.y1, "the dip stays on the bar");
            let mut points = vec![start];
            points.extend(path(start, dip, steps));
            points.extend(path(dip, chip, steps));
            for (k, p) in points.into_iter().enumerate() {
                let ms = u64::try_from(k).unwrap() * frame_ms;
                sample_at(&mut b, opened, ms, p, &theme);
                assert!(
                    chips_open(&b, 1) && (b.tab_rects()[1].width() - open_w).abs() < 0.01,
                    "{name}: the pill closed or moved at {p:?} on the way to its chip",
                );
            }
            assert_eq!(b.hover_max_idx, Some(1), "{name}: arrived on the chip");
        }
    }

    /// The hold is only for a pointer still heading for the chips: moving
    /// away closes the pill at once, and stopping short releases it once the
    /// hold lapses, handing over to whatever is under the pointer.
    #[test]
    fn the_aim_hold_ends_when_the_pointer_turns_away_or_stops_short() {
        use crate::widget::hover_intent::{AIM_HOLD, DWELL};
        let theme = Theme::dark().with_reduce_motion(true);

        // Turning away: back to the left, onto pill 0.
        {
            let (mut b, opened) = bar_with_pill_one_open(&theme);
            let idle0 = b.tab_rects()[0];
            let p = Point::new(idle0.center().x, idle0.center().y);
            sample_at(&mut b, opened, 8, p, &theme);
            assert!(
                !chips_open(&b, 1),
                "moving away closes the open pill at once"
            );
            assert!(
                !chips_open(&b, 0),
                "and the next pill waits out its own dwell"
            );
            let dwell = u64::try_from(DWELL.as_millis()).unwrap();
            layout_at(&mut b, opened, 8 + dwell, &theme);
            assert!(chips_open(&b, 0));
        }

        // Stopping short, under the pill and off any other pill.
        {
            let (mut b, opened) = bar_with_pill_one_open(&theme);
            let r = b.tab_rects()[1];
            let chip = b.max_rects()[1].center();
            let start = Point::new(r.x0 + 12.0, r.y1 - 3.0);
            let short = Point::new(chip.x - 30.0, r.y1 + 3.0);
            sample_at(&mut b, opened, 0, start, &theme);
            sample_at(&mut b, opened, 8, short, &theme);
            assert!(chips_open(&b, 1), "still heading for the chips");
            assert!(
                b.next_wake_deadline().is_some(),
                "a running hold wakes the host when it runs out"
            );
            let hold = u64::try_from(AIM_HOLD.as_millis()).unwrap();
            layout_at(&mut b, opened, 8 + hold - 1, &theme);
            assert!(chips_open(&b, 1));
            layout_at(&mut b, opened, 8 + hold, &theme);
            assert!(
                !chips_open(&b, 1),
                "a pointer resting short of the chips lets go"
            );
        }
    }

    /// The ink a glyph paints when `draw_icon` draws `icon` into `rect`.
    fn glyph_ink(icon: icons::Icon, rect: Rect) -> Rect {
        use vello::kurbo::Shape;
        let side = rect.width().min(rect.height());
        let scale = side / icons::LUCIDE_VIEWBOX;
        let origin = Point::new(
            rect.x0 + (rect.width() - side) / 2.0,
            rect.y0 + (rect.height() - side) / 2.0,
        );
        let units = icons::icon_subpaths(icon)
            .iter()
            .map(Shape::bounding_box)
            .reduce(|a, b| a.union(b))
            .expect("a glyph with paths");
        Rect::new(
            origin.x + units.x0 * scale,
            origin.y + units.y0 * scale,
            origin.x + units.x1 * scale,
            origin.y + units.y1 * scale,
        )
    }

    /// The three chips stand on one rhythm: search, maximize and close glyph
    /// centres are one pitch apart, the × sits centred in its target, and the
    /// three glyphs carry the same ink width, so the visible gaps between
    /// them are equal too (a × drawn in the same box as the others is a
    /// third narrower, which left the right-hand gap wider).
    #[test]
    fn the_three_chips_are_evenly_spaced() {
        let r = Rect::new(0.0, 6.0, 400.0, 38.0);
        let (close, max_rect, search) = chip_rects(
            r,
            CHIP_CLUSTER_W,
            false,
            CLOSE_W,
            CLOSE_INSET,
            ACTION_W,
            ACTION_W,
        );
        let gap_left = max_rect.center().x - search.center().x;
        let gap_right = close.center().x - max_rect.center().x;
        assert!(
            (gap_left - gap_right).abs() < 1e-9,
            "{gap_left} vs {gap_right}"
        );
        assert!((gap_left - CHIP_PITCH).abs() < 1e-9);
        assert!((close.center().x - (r.x1 - CLOSE_HIT_PX / 2.0)).abs() < 1e-9);
        assert!(
            max_rect.x1 <= r.x1 - CLOSE_HIT_PX,
            "maximize clears the close target"
        );
        assert!(
            (search.x0 - (r.x1 - CHIP_CLUSTER_W)).abs() < 1e-9,
            "the cluster is exactly as wide as the zone that places it",
        );

        let search_ink = glyph_ink(icons::Icon::Search, search);
        let max_ink = glyph_ink(icons::Icon::Maximize2, max_rect);
        let close_ink = glyph_ink(icons::Icon::X, close_glyph_box(close));
        for (name, ink) in [("maximize", max_ink), ("×", close_ink)] {
            assert!(
                (ink.width() - search_ink.width()).abs() < 1e-9,
                "{name} ink is {} wide, search {}",
                ink.width(),
                search_ink.width(),
            );
        }
        let ink_left = max_ink.x0 - search_ink.x1;
        let ink_right = close_ink.x0 - max_ink.x1;
        assert!(
            (ink_left - ink_right).abs() < 1e-9,
            "visible gaps {ink_left} vs {ink_right}"
        );
    }

    /// The ×, the pill and the rest of the row, as one comparable value.
    fn row_geometry(b: &TabBar) -> (Vec<Rect>, Vec<Rect>) {
        (b.tab_rects().to_vec(), b.close_rects.clone())
    }

    /// Resting on the active pill's × is intent to click it. Opening the
    /// pill grows it rightward and would carry the × out from under the
    /// pointer, so the pill never opens from there: not when the pointer
    /// comes straight in, not when it crosses the title on its way, not
    /// however long it rests, and not anywhere in the ×'s 32 px target.
    #[test]
    fn resting_on_the_close_button_never_expands_the_pill_or_moves_the_close_button() {
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let rest = row_geometry(&b);
        let r = b.tab_rects()[0];
        let close = b.close_hit_rect(0).expect("the active pill shows its ×");
        let x = close.center();
        let t0 = crate::time::now();

        // Across the title at an ordinary pace, then onto the ×; the
        // last stretch slows down the way a hand homes in on a target.
        let start = Point::new(r.x0 + 12.0, r.center().y);
        let near = Point::new(close.x0 - 6.0, r.center().y + 1.0);
        let mut points = path(start, near, 12);
        points.extend(path(near, x, 6));
        let mut ms = 0;
        sample_at(&mut b, t0, ms, start, &theme);
        for p in points {
            ms += 12;
            sample_at(&mut b, t0, ms, p, &theme);
            assert_eq!(row_geometry(&b), rest, "the row moved at {p:?}");
        }
        for later in [50, 100, 150, 300, 1_000, 10_000] {
            layout_at(&mut b, t0, ms + later, &theme);
            assert!(!chips_open(&b, 0), "opened {later} ms after reaching the ×");
            assert_eq!(row_geometry(&b), rest, "the × moved {later} ms in");
        }

        // Straight onto the ×, and onto every corner of its target.
        let inset = 1.0;
        for p in [
            x,
            Point::new(close.x0 + inset, close.y0 + inset),
            Point::new(close.x1 - inset, close.y0 + inset),
            Point::new(close.x0 + inset, close.y1 - inset),
            Point::new(close.x1 - inset, close.y1 - inset),
        ] {
            let mut b = four_tab_bar();
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let above = Point::new(p.x, b.rect.y0 - 20.0);
            sample_at(&mut b, t0, 0, above, &theme);
            sample_at(&mut b, t0, 8, p, &theme);
            layout_at(&mut b, t0, 10_000, &theme);
            assert!(!chips_open(&b, 0), "opened resting at {p:?}");
            assert_eq!(row_geometry(&b), rest, "the × moved under {p:?}");
        }
    }

    /// Passing over the × is not hovering it. A pointer that crosses the
    /// active pill's × at speed, coming from the pill to its right on its way
    /// to where the chips will be, opens the pill on the usual dwell from
    /// entry, and the × lights no hover wash on the way through.
    #[test]
    fn crossing_the_close_button_at_speed_toward_the_chips_expands_the_pill() {
        use crate::widget::hover_intent::DWELL;
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[0];
        let close = b.close_hit_rect(0).expect("the active pill shows its ×");
        let y = r.center().y;
        let from = Point::new(b.tab_rects()[1].x0 + 10.0, y);
        // Where the search chip will sit once the pill has opened.
        let to = Point::new(close.x0 - 30.0, y);
        let t0 = crate::time::now();
        let mut entered = None;
        let mut crossed_x = false;
        // About 2 px/ms (the stretch is about 80 px), a sample per 8 ms frame.
        let steps = 5;
        sample_at(&mut b, t0, 0, from, &theme);
        for (k, p) in path(from, to, steps).into_iter().enumerate() {
            let ms = 8 * (u64::try_from(k).unwrap() + 1);
            sample_at(&mut b, t0, ms, p, &theme);
            if entered.is_none() && r.contains(p) {
                entered = Some(ms);
            }
            if close.contains(p) {
                crossed_x = true;
                let wash = at_time(t0 + Duration::from_millis(ms), || {
                    b.close_chip_color_for_tests(0, &theme)
                });
                let danger = theme.tokens.colors.semantic.danger.with_alpha_factor(0.8);
                assert_ne!(wash, Some(danger), "the × lit up under a passing pointer");
            }
        }
        assert!(crossed_x, "the path crosses the ×");
        let entered = entered.expect("the path enters the pill");
        let dwell = u64::try_from(DWELL.as_millis()).unwrap();
        layout_at(&mut b, t0, entered + dwell, &theme);
        assert!(chips_open(&b, 0), "crossing the × still opens the pill");
    }

    /// A slow approach that comes to rest on the × is aim: the pill never
    /// opens, the × never moves, and the × shows its hover wash once the
    /// pointer has settled.
    #[test]
    fn a_slow_approach_that_stops_on_the_close_button_is_aim() {
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let rest = row_geometry(&b);
        let r = b.tab_rects()[0];
        let close = b.close_hit_rect(0).expect("the active pill shows its ×");
        let x = close.center();
        let t0 = crate::time::now();
        // From the title onto the ×'s centre at 0.1 px/ms.
        let start = Point::new(close.x0 - 20.0, r.center().y);
        sample_at(&mut b, t0, 0, start, &theme);
        // The stretch is 36 px: one pixel per sample.
        let steps = 36;
        let mut ms = 0;
        for p in path(start, x, steps) {
            ms += 10;
            sample_at(&mut b, t0, ms, p, &theme);
            assert_eq!(row_geometry(&b), rest, "the row moved at {p:?}");
        }
        for later in [50, 100, 300, 1_000, 10_000] {
            layout_at(&mut b, t0, ms + later, &theme);
            assert!(
                !chips_open(&b, 0),
                "opened {later} ms after stopping on the ×"
            );
            assert_eq!(row_geometry(&b), rest, "the × moved {later} ms in");
        }
        let wash = at_time(t0 + Duration::from_millis(ms + 1_000), || {
            b.close_chip_color_for_tests(0, &theme)
        });
        let danger = theme.tokens.colors.semantic.danger.with_alpha_factor(0.8);
        assert_eq!(wash, Some(danger), "a settled pointer lights the ×");
    }

    /// Seeing more takes a pause on the pill's body. A pointer that was
    /// heading for the × and stops short on the title opens it only once it
    /// has clearly stopped heading there, not after the bare dwell.
    #[test]
    fn stopping_on_the_title_short_of_the_close_button_opens_after_the_aim_hold() {
        use crate::widget::hover_intent::{AIM_HOLD, DWELL};
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[0];
        let t0 = crate::time::now();
        let start = Point::new(r.x0 + 10.0, r.center().y);
        let stop = Point::new(r.x0 + 30.0, r.center().y);
        sample_at(&mut b, t0, 0, start, &theme);
        for (k, p) in path(start, stop, 4).into_iter().enumerate() {
            sample_at(&mut b, t0, 8 * (u64::try_from(k).unwrap() + 1), p, &theme);
        }
        let stopped = 32;
        let dwell = u64::try_from(DWELL.as_millis()).unwrap();
        let hold = u64::try_from(AIM_HOLD.as_millis()).unwrap();
        layout_at(&mut b, t0, stopped + dwell, &theme);
        assert!(!chips_open(&b, 0), "a bare dwell after heading for the ×");
        layout_at(&mut b, t0, stopped + hold, &theme);
        assert!(chips_open(&b, 0), "resting on the title asks to see more");
    }

    /// Menu aim from the ×: with the pill open, a pointer on its × that
    /// heads for the search chip keeps the pill open even where its path
    /// leaves the pill, here over the strip of bar above it.
    #[test]
    fn moving_from_the_close_button_toward_a_revealed_chip_keeps_the_pill_open() {
        let theme = Theme::dark().with_reduce_motion(true);
        for (name, steps, frame_ms) in [("fast", 2_u32, 8_u64), ("slow", 16, 20)] {
            let (mut b, opened) = bar_with_pill_one_open(&theme);
            let open = row_geometry(&b);
            let r = b.tab_rects()[1];
            let x = b.close_rects[1].center();
            let chip = b.search_rects()[1].center();
            let lift = Point::new((x.x + chip.x) / 2.0, b.rect.y0 + 2.0);
            assert!(lift.y < r.y0, "the path leaves the pill");
            // Along the pill to its ×, then up over the bar and onto search.
            let mut points = vec![Point::new(r.x0 + 12.0, r.center().y)];
            points.extend(path(points[0], x, steps));
            points.extend(path(x, lift, steps));
            points.extend(path(lift, chip, steps));
            for (k, p) in points.into_iter().enumerate() {
                let ms = u64::try_from(k).unwrap() * frame_ms;
                sample_at(&mut b, opened, ms, p, &theme);
                assert!(chips_open(&b, 1), "{name}: closed at {p:?}");
                assert_eq!(row_geometry(&b), open, "{name}: moved at {p:?}");
            }
            assert_eq!(b.hover_search_idx, Some(1), "{name}: arrived on search");
        }
    }

    /// Crossing over something unrelated closes the open pill at once: down
    /// off the bar toward the window, or onto another pill's body.
    #[test]
    fn crossing_over_unrelated_area_collapses_the_open_pill() {
        let theme = Theme::dark().with_reduce_motion(true);
        let (mut b, opened) = bar_with_pill_one_open(&theme);
        let r = b.tab_rects()[1];
        sample_at(
            &mut b,
            opened,
            8,
            Point::new(r.x0 + 14.0, r.y1 + 2.0),
            &theme,
        );
        sample_at(&mut b, opened, 16, Point::new(r.x0 + 16.0, 300.0), &theme);
        assert!(!chips_open(&b, 1), "down toward the window closes it");

        let (mut b, opened) = bar_with_pill_one_open(&theme);
        let other = b.tab_rects()[3];
        let p = Point::new(other.x0 + 12.0, other.center().y);
        sample_at(
            &mut b,
            opened,
            8,
            Point::new(other.x0 - 40.0, 300.0),
            &theme,
        );
        sample_at(&mut b, opened, 16, p, &theme);
        assert!(!chips_open(&b, 1), "another pill's body closes it");
    }

    /// Never move a control the pointer is heading for or resting on: with
    /// pill 1 open and pill 2 active (its × showing), a pointer leaving pill
    /// 1 for pill 2's × must not collapse pill 1 on the way, since that
    /// slides pill 2 and its × leftward under the arriving pointer.
    #[test]
    fn an_open_pill_holds_while_the_pointer_heads_for_a_neighbours_close_button() {
        let theme = Theme::dark().with_reduce_motion(true);
        let mut b = four_tab_bar();
        b.set_active(3);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let r = b.tab_rects()[1];
        let t0 = crate::time::now();
        let start = Point::new(r.x0 + 12.0, r.center().y);
        sample_at(&mut b, t0, 0, start, &theme);
        let dwell = u64::try_from(crate::widget::hover_intent::DWELL.as_millis()).unwrap();
        layout_at(&mut b, t0, dwell, &theme);
        assert!(chips_open(&b, 1), "resting on pill 1's body opens it");
        let target = b.close_hit_rect(2).expect("the active pill shows its ×");
        let close_2 = b.close_rects[2];
        let to = target.center();
        for (k, p) in path(start, to, 16).into_iter().enumerate() {
            let ms = dwell + 8 * (u64::try_from(k).unwrap() + 1);
            sample_at(&mut b, t0, ms, p, &theme);
            assert_eq!(b.close_rects[2], close_2, "pill 2's × moved at {p:?}");
        }
        layout_at(&mut b, t0, dwell + 60_000, &theme);
        assert_eq!(b.close_rects[2], close_2, "the × under the pointer moved");
        assert_eq!(b.hover_close_idx, Some(2));
    }

    /// Before/after frames for review: the pointer resting on the ×, the
    /// pointer travelling from the × to a revealed chip, and the pointer
    /// leaving for the window. Each frame is the bar at 2× with the pointer
    /// marked; every scenario also lands as one stacked sheet.
    #[test]
    #[ignore = "a review tool: needs a GPU adapter; writes PNGs to TILO_UI_SCREENSHOT_DIR"]
    fn render_window_controls_intent_frames() {
        let dir = std::path::PathBuf::from(
            std::env::var("TILO_UI_SCREENSHOT_DIR").expect("TILO_UI_SCREENSHOT_DIR"),
        );
        std::fs::create_dir_all(&dir).expect("shot dir");
        let theme = Theme::dark().with_reduce_motion(true);
        let (w, h, scale) = (480.0, TAB_BAR_HEIGHT, 2.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "small fixed canvas"
        )]
        let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
        let mut renderer =
            crate::offscreen::OffscreenRenderer::try_new(pw, ph).expect("a GPU adapter");
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut shoot = |b: &mut TabBar, p: Point| -> Vec<u8> {
            let mut scene = Scene::new();
            let t = Affine::scale(scale);
            scene.fill(
                vello::peniko::Fill::NonZero,
                t,
                theme.tokens.colors.surfaces.base,
                None,
                &Rect::new(0.0, 0.0, w, h),
            );
            b.draw(&mut scene, t, &mut tr, &theme, &presence);
            let dot = vello::kurbo::Circle::new(p, 3.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                t,
                vello::peniko::Color::from_rgb8(255, 210, 0),
                None,
                &dot,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                t,
                vello::peniko::Color::BLACK,
                None,
                &dot,
            );
            renderer.render(&scene, vello::peniko::Color::BLACK)
        };
        let sheet = |name: &str, frames: Vec<(String, Vec<u8>)>| {
            let mut all = Vec::new();
            for (label, rgba) in &frames {
                let path = dir.join(format!("{name}-{label}.png"));
                crate::png::write_rgba8(&path, pw, ph, rgba).expect("write frame");
                all.extend_from_slice(rgba);
            }
            let rows = u32::try_from(frames.len()).unwrap();
            crate::png::write_rgba8(dir.join(format!("{name}.png")), pw, ph * rows, &all)
                .expect("write sheet");
        };

        // 1. Across the title onto the active pill's ×, then rest there.
        {
            let mut b = four_tab_bar();
            lay_out_at_width_with(&mut b, 900.0, &theme);
            // The first draw records the real title advances; lay out again
            // so every position below is the one the reviewer sees.
            shoot(&mut b, Point::ZERO);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let r = b.tab_rects()[0];
            let x = b.close_hit_rect(0).expect("×").center();
            let start = Point::new(r.x0 + 12.0, r.center().y);
            let t0 = crate::time::now();
            let mut frames = Vec::new();
            sample_at(&mut b, t0, 0, start, &theme);
            frames.push(("00-enter".to_owned(), shoot(&mut b, start)));
            let mut ms = 0;
            for p in path(start, x, 10) {
                ms += 12;
                sample_at(&mut b, t0, ms, p, &theme);
            }
            frames.push(("01-on-x".to_owned(), shoot(&mut b, x)));
            for later in [100, 200, 400, 1_000] {
                layout_at(&mut b, t0, ms + later, &theme);
                let at = t0 + Duration::from_millis(ms + later);
                frames.push((
                    format!("02-rest-{later:04}ms"),
                    at_time(at, || shoot(&mut b, x)),
                ));
            }
            sheet("rest-on-x", frames);
        }

        // 2. Pill 1 opened from its body; from its × up over the bar and
        //    onto search.
        {
            let mut b = four_tab_bar();
            lay_out_at_width_with(&mut b, 900.0, &theme);
            // The first draw records the real title advances; lay out again
            // so every position below is the one the reviewer sees.
            shoot(&mut b, Point::ZERO);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let r = b.tab_rects()[1];
            let t0 = crate::time::now();
            let start = Point::new(r.x0 + 12.0, r.center().y);
            let mut frames = Vec::new();
            sample_at(&mut b, t0, 0, start, &theme);
            frames.push(("00-enter-body".to_owned(), shoot(&mut b, start)));
            layout_at(&mut b, t0, 400, &theme);
            frames.push(("01-rest-400ms".to_owned(), shoot(&mut b, start)));
            let x = if b.close_rects[1].area() > 0.0 {
                b.close_rects[1].center()
            } else {
                b.tab_rects()[1].center()
            };
            let mut ms = 400;
            for p in path(start, x, 6) {
                ms += 12;
                sample_at(&mut b, t0, ms, p, &theme);
            }
            frames.push(("02-on-x".to_owned(), shoot(&mut b, x)));
            let chip = if b.search_rects()[1].area() > 0.0 {
                b.search_rects()[1].center()
            } else {
                Point::new(x.x - 48.0, x.y)
            };
            let lift = Point::new((x.x + chip.x) / 2.0, b.rect.y0 + 2.0);
            for (k, p) in path(x, lift, 3)
                .into_iter()
                .chain(path(lift, chip, 3))
                .enumerate()
            {
                ms += 12;
                sample_at(&mut b, t0, ms, p, &theme);
                frames.push((format!("03-aim-{k}"), shoot(&mut b, p)));
            }
            // 3. Off toward the window.
            let away = Point::new(chip.x, 300.0);
            ms += 12;
            sample_at(&mut b, t0, ms, Point::new(chip.x, h - 1.0), &theme);
            ms += 12;
            sample_at(&mut b, t0, ms, away, &theme);
            frames.push((
                "04-leave".to_owned(),
                shoot(&mut b, Point::new(chip.x, h - 1.0)),
            ));
            sheet("aim-from-x", frames);
        }

        // 3. Onto the edge of the ×'s 32 px target (a press there closes the
        //    tab), outside the painted glyph, and rest.
        {
            let mut b = four_tab_bar();
            lay_out_at_width_with(&mut b, 900.0, &theme);
            shoot(&mut b, Point::ZERO);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let r = b.tab_rects()[0];
            let target = b.close_hit_rect(0).expect("×");
            let edge = Point::new(target.x0 + 3.0, target.y0 + 4.0);
            let t0 = crate::time::now();
            let mut frames = Vec::new();
            sample_at(&mut b, t0, 0, Point::new(edge.x, r.y0 - 20.0), &theme);
            sample_at(&mut b, t0, 12, edge, &theme);
            frames.push(("00-on-target".to_owned(), shoot(&mut b, edge)));
            for later in [100, 200, 400, 1_000] {
                layout_at(&mut b, t0, 12 + later, &theme);
                let at = t0 + Duration::from_millis(12 + later);
                frames.push((
                    format!("01-rest-{later:04}ms"),
                    at_time(at, || shoot(&mut b, edge)),
                ));
            }
            sheet("rest-on-x-target-edge", frames);
        }

        // 4. Pill 1 open, pill 2 active: the pointer leaves pill 1 for pill
        //    2's ×.
        {
            let mut b = four_tab_bar();
            b.set_active(3);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            shoot(&mut b, Point::ZERO);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let r = b.tab_rects()[1];
            let t0 = crate::time::now();
            let start = Point::new(r.x0 + 12.0, r.center().y);
            let mut frames = Vec::new();
            sample_at(&mut b, t0, 0, start, &theme);
            layout_at(&mut b, t0, 400, &theme);
            frames.push(("00-pill-1-open".to_owned(), shoot(&mut b, start)));
            let to = b.close_hit_rect(2).expect("×").center();
            let mut ms = 400;
            for (k, p) in path(start, to, 8).into_iter().enumerate() {
                ms += 12;
                sample_at(&mut b, t0, ms, p, &theme);
                if k % 2 == 1 {
                    frames.push((format!("01-toward-x-{k}"), shoot(&mut b, p)));
                }
            }
            layout_at(&mut b, t0, ms + 1_000, &theme);
            let at = t0 + Duration::from_millis(ms + 1_000);
            frames.push((
                "02-rest-1000ms".to_owned(),
                at_time(at, || shoot(&mut b, to)),
            ));
            sheet("toward-neighbour-x", frames);
        }

        // 5. From the pill on the right, across the active pill's × at speed,
        //    onto where its chips will be: transit, so it opens.
        {
            let mut b = four_tab_bar();
            lay_out_at_width_with(&mut b, 900.0, &theme);
            shoot(&mut b, Point::ZERO);
            lay_out_at_width_with(&mut b, 900.0, &theme);
            let y = b.tab_rects()[0].center().y;
            let close = b.close_hit_rect(0).expect("×");
            let from = Point::new(b.tab_rects()[1].x0 + 10.0, y);
            let to = Point::new(close.x0 - 30.0, y);
            let t0 = crate::time::now();
            let mut frames = Vec::new();
            sample_at(&mut b, t0, 0, from, &theme);
            frames.push(("00-start".to_owned(), shoot(&mut b, from)));
            let mut ms = 0;
            for (k, p) in path(from, to, 5).into_iter().enumerate() {
                ms += 8;
                sample_at(&mut b, t0, ms, p, &theme);
                frames.push((format!("01-cross-{k}"), shoot(&mut b, p)));
            }
            layout_at(&mut b, t0, ms + 120, &theme);
            frames.push(("02-rest-120ms".to_owned(), shoot(&mut b, to)));
            sheet("cross-x-to-chips", frames);
        }
    }

    #[test]
    fn revealed_chip_cluster_widens_only_its_pill_and_eases_open() {
        // Case `hovering_a_pill_widens_it_by_the_cluster_it_reveals`.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);

            // Pill 1 is inactive (tab 1 is active), so at rest it reveals nothing.
            let idle = b.tab_rects()[1].width();
            assert_eq!(
                b.close_rects[1],
                Rect::ZERO,
                "an idle inactive pill must not offer a close target it isn't painting",
            );

            hover_pill(&mut b, 1, width, &theme);
            let hovered = b.tab_rects()[1].width();

            assert!(
                (hovered - idle - HOVER_GROWTH_PX).abs() < 0.5,
                "hovering pill 1 took it from {idle} to {hovered}; it must grow by \
             the cluster it reveals ({HOVER_GROWTH_PX} px), not reveal chips \
             into width it was already holding",
            );
            assert!(
                b.close_rects[1].area() > 0.0,
                "close chip must be placed once revealed"
            );
            assert!(
                b.max_rects[1].area() > 0.0,
                "maximize chip must be placed once revealed"
            );
            assert!(
                b.search_rects[1].area() > 0.0,
                "search chip must be placed once revealed"
            );
        }

        // Case `an_opening_pill_moves_nothing_at_or_left_of_its_own_leading_edge`.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);
            let before: Vec<Rect> = b.tab_rects().to_vec();

            hover_pill(&mut b, 2, width, &theme);
            let after: Vec<Rect> = b.tab_rects().to_vec();

            for i in 0..=2 {
                assert!(
                    (after[i].x0 - before[i].x0).abs() < 0.01,
                    "pill {i}'s leading edge moved from {} to {} while pill 2 opened — \
                 everything at or left of the hovered pill must stay put",
                    before[i].x0,
                    after[i].x0,
                );
            }
            for i in 0..2 {
                assert!(
                    (after[i].width() - before[i].width()).abs() < 0.01,
                    "pill {i} was re-sized ({} -> {}) because the pointer moved; \
                 neighbours translate, they are never re-fitted",
                    before[i].width(),
                    after[i].width(),
                );
            }
            assert!(
                after[3].x0 > before[3].x0 + 1.0,
                "the pill right of the open one must slide right, away from the pointer",
            );
            assert!(
                (after[3].width() - before[3].width()).abs() < 0.01,
                "the pill right of the open one translates; it must not be squeezed",
            );
        }

        // Case `an_idle_pill_holds_no_room_for_chips_it_is_not_painting`.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);

            // Long enough to clear the width floor, so the assertion is about the
            // reserve and not the floor.
            let i = 2; // "Terminal"
            let expected =
                PILL_PAD_X + b.leading_zone(i) + b.shaped_title_width(i) + Reveal::None.reserve();
            assert!(
                expected > PILL_MIN_PX,
                "test picked a pill that is floored, not content-sized",
            );
            assert!(
                (b.tab_rects()[i].width() - expected).abs() < 0.5,
                "idle pill {i} is {} wide but its contents want {expected}; it is \
             still holding room for chips it does not paint",
                b.tab_rects()[i].width(),
            );
            // From the reader's side: the blank run between two titles should be two
            // paddings and a gap, not two paddings, a gap, and a cluster.
            let title_end =
                b.tab_rects()[i].x0 + PILL_PAD_X + b.leading_zone(i) + b.shaped_title_width(i);
            let next_title_start = b.tab_rects()[i + 1].x0 + PILL_PAD_X + b.leading_zone(i + 1);
            assert!(
                next_title_start - title_end < PILL_PAD_X + PILL_GAP + PILL_PAD_X + 1.0,
                "{} px of nothing between two titles",
                next_title_start - title_end,
            );
        }

        // Case `chip_hit_targets_exist_exactly_when_the_pill_is_showing_them`.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);

            // Active pill (index 0, id 1): close only.
            assert!(
                b.close_rects[0].area() > 0.0,
                "the active tab shows its close chip"
            );
            assert_eq!(
                b.max_rects[0],
                Rect::ZERO,
                "an unhovered active tab shows no maximize chip"
            );
            assert_eq!(
                b.search_rects[0],
                Rect::ZERO,
                "an unhovered active tab shows no search chip"
            );

            // Idle inactive pill: nothing.
            assert_eq!(b.close_rects[1], Rect::ZERO);
            assert_eq!(b.max_rects[1], Rect::ZERO);
            assert_eq!(b.search_rects[1], Rect::ZERO);

            // Hovered: all three, and each inside its own pill.
            hover_pill(&mut b, 1, width, &theme);
            for (name, r) in [
                ("close", b.close_rects[1]),
                ("maximize", b.max_rects[1]),
                ("search", b.search_rects[1]),
            ] {
                assert!(r.area() > 0.0, "{name} chip missing on the hovered pill");
                let pill = b.tab_rects()[1];
                assert!(
                    r.x0 >= pill.x0 - 0.5 && r.x1 <= pill.x1 + 0.5,
                    "{name} chip at {r:?} is outside its pill at {pill:?}",
                );
            }
        }

        // Case `the_reveal_eases_open_rather_than_snapping`.
        // A pill that snapped would be the same visual pop the always-on reserve
        // produced, just with the pill changing size too.
        {
            let theme = crate::theme::Theme::dark(); // motion on
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);
            let idle = b.tab_rects()[1].width();

            hover_pill(&mut b, 1, width, &theme);
            // The latching pass lays out at the starting value; sample partway along
            // since the host keeps pumping frames from there (`needs_animation_frame`).
            assert!(
                b.reveal_in_flight(),
                "the pointer arriving must start a curve"
            );
            std::thread::sleep(Duration::from_millis(15));
            lay_out_at_width_with(&mut b, width, &theme);
            let opening = b.tab_rects()[1].width();
            assert!(
                opening > idle && opening < idle + HOVER_GROWTH_PX,
                "part way along the curve the pill is {opening} wide; it must be \
             between its idle width {idle} and its open width {}",
                idle + HOVER_GROWTH_PX,
            );

            std::thread::sleep(reveal_anim() + Duration::from_millis(40));
            lay_out_at_width_with(&mut b, width, &theme);
            let settled = b.tab_rects()[1].width();
            assert!(
                (settled - idle - HOVER_GROWTH_PX).abs() < 0.5,
                "the curve left the pill at {settled}; it must land on {}",
                idle + HOVER_GROWTH_PX,
            );
        }

        // Case `reduce_motion_lands_the_reveal_in_a_single_pass`.
        // Under reduce-motion the host stops pumping animation frames, so a curve
        // started here would freeze part-open, too narrow for the chips it paints.
        {
            let theme = crate::theme::Theme::dark().with_reduce_motion(true);
            let mut b = four_tab_bar();
            let width = 900.0;
            lay_out_at_width_with(&mut b, width, &theme);
            let idle = b.tab_rects()[1].width();

            hover_pill(&mut b, 1, width, &theme);
            assert!(
                (b.tab_rects()[1].width() - idle - HOVER_GROWTH_PX).abs() < 0.5,
                "reduce-motion must skip the curve, not run it slowly",
            );
            assert!(
                !b.reveal_in_flight(),
                "no curve may be left running under reduce-motion"
            );
        }
    }

    /// A busy arc turns in indicator steps on wake deadlines — not on
    /// animation frames — and comes to rest after [`BUSY_TURNING`].
    #[test]
    fn a_busy_arc_steps_on_deadlines_and_rests_after_a_while() {
        let mut tabs = bar();
        tabs.set_mark(3, Some(TabMark::Busy));
        let since = tabs.busy_since[&3];
        let now = since + Duration::from_millis(5);
        assert!(!tabs.needs_animation_frame(), "a busy mark is not motion");
        let due = tabs.busy_deadline(now).expect("the arc's next step");
        assert_eq!(
            due,
            crate::animation::next_indicator_step(busy_epoch(), now)
        );
        tabs.busy_drawn = Some(tabs.busy_pose_at(now));
        let doc = automerge::AutoCommit::new();
        assert!(
            crate::time::testing::with_monotonic_time(due, || {
                Widget::update(&mut tabs, &doc, &automerge::ROOT)
            }),
            "the next step repaints"
        );
        let rested = since + BUSY_TURNING;
        assert_eq!(
            tabs.busy_deadline(rested),
            None,
            "a long-busy tab stands still"
        );
        assert_eq!(tabs.busy_pose_at(rested), BusyPose::Still);
    }

    #[test]
    fn a_strip_of_fixed_views_has_no_close_or_new_tab_and_marks_paint_and_hit() {
        let theme = crate::theme::Theme {
            reduce_motion: true,
            ..crate::theme::Theme::dark()
        };
        let mut fixed = bar().closable(false).without_new_tab();
        lay_out_at_width_with(&mut fixed, 600.0, &theme);
        assert_eq!(fixed.new_tab_rect(), Rect::ZERO, "no new-tab control");
        hover_pill(&mut fixed, 1, 600.0, &theme);
        assert!(
            fixed.close_rects.iter().all(|r| r.area() <= 0.0),
            "no close control, even under the pointer"
        );
        let node = Widget::accessibility(&fixed).unwrap();
        assert!(
            node.children
                .iter()
                .all(|tab| tab.children.is_empty() && tab.name.as_deref() != Some("New tab")),
            "nothing to close or add is announced"
        );

        // Marks: a playing tab's speaker is a control of its own.
        let mut tabs = bar();
        tabs.set_mark(2, Some(TabMark::Playing));
        tabs.set_mark(3, Some(TabMark::Busy));
        lay_out_at_width_with(&mut tabs, 600.0, &theme);
        assert_eq!(
            tabs.busy_deadline(crate::time::now()),
            None,
            "a busy mark under reduced motion stands still"
        );
        let speaker = tabs
            .mark_rect(2)
            .expect("the mark stands in the leading slot");
        assert!(tabs.tab_rects()[1].contains(speaker.center()));
        let mut handled = false;
        dispatch_cursor(&mut tabs, speaker.center(), &mut handled);
        dispatch_left_button(&mut tabs, ElementState::Pressed, &mut handled);
        assert_eq!(tabs.take_mark_click(), Some((2, TabMark::Playing)));
        assert_eq!(
            tabs.active_id(),
            1,
            "a click on the mark does not select the tab"
        );
        let node = Widget::accessibility(&tabs).unwrap();
        assert_eq!(
            node.children[1].description.as_deref(),
            Some("Playing audio")
        );
        assert_eq!(node.children[2].description.as_deref(), Some("Loading"));
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        tabs.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        assert!(!scene.encoding().draw_tags.is_empty());
    }

    #[test]
    fn the_active_tab_is_the_lift_alone_unless_the_host_shows_group_focus() {
        let theme = crate::theme::Theme::dark();
        let colour = |c: vello::peniko::Color| c.premultiply().to_rgba8().to_u32();
        let you = colour(theme.tokens.colors.intent.accent_ink);
        let ink = colour(theme.tokens.colors.text_primary);
        let paint = |b: &mut TabBar| {
            lay_out_at_width_with(b, 600.0, &theme);
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            b.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            scene.encoding().draw_data.clone()
        };
        let mut documents = bar();
        let unfocused = paint(&mut documents);
        assert!(
            !unfocused.contains(&you),
            "a strip of documents wears no accent"
        );
        let mut groups = bar();
        groups.set_group_focused(true);
        let focused = paint(&mut groups);
        assert!(
            focused.len() > unfocused.len() && focused.contains(&ink),
            "the focused window group's active tab carries a keyline"
        );
        assert!(
            !focused.contains(&you),
            "in primary ink: your colour is for keyboard focus rings, carets and selection"
        );
    }

    /// Idle tab titles on the frosted strip stay legible over any desktop:
    /// over the brightest and the darkest backdrop the tint can sit on.
    #[test]
    fn idle_titles_on_the_frosted_strip_read_over_any_desktop() {
        use crate::a11y::contrast_ratio;
        for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let mut bar = TabBar::new("g", vec![TabDescriptor::new(1, "A")], 1);
            bar.set_on_strip(true);
            let ink = bar.idle_ink(&theme);
            if !crate::chrome::selector_finish(&theme).translucent() {
                assert_eq!(
                    ink, theme.tokens.colors.text_dim,
                    "{}: a solid strip keeps the dim tier",
                    theme.name
                );
                continue;
            }
            for ground in crate::tokens::SelectorFinish::frosted_extremes(
                crate::chrome::strip_ground(&theme),
                theme.tokens.colors.surfaces.material_opacity,
            ) {
                let ratio = contrast_ratio(ink, ground);
                assert!(
                    ratio >= crate::tokens::colors::ON_FILL_MIN_CONTRAST,
                    "{}: an idle title reads {ratio:.2}:1 on the frosted strip",
                    theme.name
                );
            }
        }
    }
}
