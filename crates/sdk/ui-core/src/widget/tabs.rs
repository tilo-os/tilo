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
use std::collections::{HashMap, HashSet};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Unpainted now, but `needs_animation_frame` and a regression test still
/// gate on it.
const DROP_ZONE_ANIM_MS: u64 = 140;

/// Drop-target indicator line width; thin enough not to compete with pill chrome.
const HOVER_DROP_LINE_PX: f64 = 1.5;
/// Fade-in when `hover_drop_idx` changes gap; shorter than the layout spring.
const HOVER_DROP_FADE_MS: u64 = 90;

/// Layout spring on reorder/insert/close; long enough to read as one motion, not a
/// jump cut.
const LAYOUT_ANIM_MS: u64 = 180;

/// Per-pill insert fade-in, distinct from `LAYOUT_ANIM_MS` so a new pill reads as
/// "appearing" rather than sliding in. Opacity + y-offset only; scale is skipped to
/// avoid a per-pill affine `push_layer`.
const INSERT_FADE_MS: u64 = 220;
/// Start y-offset for the insert fade; positive pulls the pill up at t=0, settling
/// to target at t=1.
const INSERT_FADE_Y_OFFSET: f64 = 10.0;
/// Room beyond a pill's fade clip that its damage part also claims, for
/// strokes and antialiasing on the clip's edge.
const PILL_PART_MARGIN: f64 = 4.0;

/// Per-pill close fade-out; pairs with `SPAWN_FADE_MS` in `tile_host.rs`.
pub const CLOSE_FADE_MS: f32 = 180.0;
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
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Rect + Vec2 is float component addition and cannot panic"
)]
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

/// Converts a millisecond animation-timing constant to `f64`. These are small
/// compile-time durations (well under a second), nowhere near `f64`'s 52-bit
/// mantissa limit, so the conversion is exact.
#[allow(
    clippy::cast_precision_loss,
    reason = "millisecond timing constants stay far below 2^52; exact in f64"
)]
fn ms_f64(ms: u64) -> f64 {
    ms as f64
}

/// Converts an elapsed-time reading (in milliseconds) to `f64`. Only ever fed a
/// recent `Duration`, so — like [`ms_f64`] — the value stays far below the
/// precision limit.
#[allow(
    clippy::cast_precision_loss,
    reason = "elapsed animation time in milliseconds stays far below 2^52; exact in f64"
)]
fn duration_ms_f64(ms: u128) -> f64 {
    ms as f64
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

/// Delegates to [`crate::chrome::shell::selection_tier`]; the tier table lives there.
pub(crate) fn pill_appearance(
    theme: &Theme,
    is_active: bool,
    is_hover: bool,
    is_pressed: bool,
) -> PillAppearance {
    crate::chrome::shell::glass_selector(
        theme,
        crate::chrome::shell::SelectionState::new(is_active, is_hover, is_pressed),
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
/// rendering at declining alpha/scale for `CLOSE_FADE_MS` while survivors reflow.
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
    new_tab_rect: Rect,
    /// Keyed by tab id, alongside the title/size it was measured at, so a rename or
    /// theme change invalidates rather than reusing a stale advance. Set from `draw`
    /// (the only pass with a [`TextRenderer`]) and read back by `apply_layout`.
    title_advance: HashMap<u64, (String, TextStyle, f64)>,
    /// Aligned 1:1 with `tabs`; `None` for a pinned, icon-only pill.
    visible_titles: Vec<Option<String>>,
    pending_context_menu: Option<(u64, Point)>,
    hover_idx: Option<usize>,
    hover_close_idx: Option<usize>,
    hover_max_idx: Option<usize>,
    hover_search_idx: Option<usize>,
    /// Candidate pill and the instant its deliberate-hover dwell began.
    hover_intent: Option<(usize, Instant)>,
    /// Previous pointer sample used to reject fast transit across tabs.
    hover_motion_sample: Option<(Point, Instant)>,
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
    pub pending_new_tab: Option<()>,
    /// One-shot selection edge produced by direct user interaction.  The
    /// retained shell must not infer a click merely because model rebuilding
    /// changed `active_id` between frames.
    pending_activation: Option<u64>,
    /// `(tab_id, release_point)` when a drag is released outside this bar's rect,
    /// for a parent (e.g. `GroupRow`) to resolve the drop target.
    pub pending_detach: Option<(u64, Point)>,
    drop_zone_target: Option<usize>,
    drop_zone_animated_at: Option<Instant>,
    /// Gap index where the drop-target line paints. Written from own-bar drag
    /// events and, for cross-group drags, [`Self::set_external_drag_hover`] called
    /// by `GroupRow` on every sibling bar.
    hover_drop_idx: Option<usize>,
    hover_drop_animated_at: Option<Instant>,
    /// `tab_rects[i]` keyed by id, snapshotted just before a reorder/insert/remove;
    /// `draw` springs each pill from here to its new rect over `LAYOUT_ANIM_MS`.
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
        self.new_tab_rect = shift_rect(self.new_tab_rect, shift);
    }

    /// Source pill's opacity while the drag-ghost is visible; low enough to read as
    /// inactive but the slot still holds its place (no neighbour reflow).
    pub const SOURCE_PILL_DIM_ALPHA: f32 = 0.35;
    /// The handle is visibly lifted but remains translucent enough for a drop
    /// target to stay legible beneath it.
    pub const GHOST_PILL_ALPHA: f32 = 0.5;

    pub fn new(group_id_tag: &str, tabs: Vec<TabDescriptor>, active_id: u64) -> Self {
        Self {
            group_id_tag: group_id_tag.into(),
            tabs,
            active_id,
            group_focus: GroupFocus::Focused,
            keyboard_focus: false,
            rect: Rect::ZERO,
            tab_rects: Vec::new(),
            close_rects: Vec::new(),
            close_hit_rects: Vec::new(),
            max_rects: Vec::new(),
            search_rects: Vec::new(),
            new_tab_rect: Rect::ZERO,
            title_advance: HashMap::new(),
            visible_titles: Vec::new(),
            pending_context_menu: None,
            hover_idx: None,
            hover_close_idx: None,
            hover_max_idx: None,
            hover_search_idx: None,
            hover_intent: None,
            hover_motion_sample: None,
            hover_new_tab: false,
            pinned_ids: HashSet::new(),
            accent_ids: HashMap::new(),
            modified_ids: HashSet::new(),
            show_maximize: true,
            closable: true,
            new_tab: true,
            marks: HashMap::new(),
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
            pending_new_tab: None,
            pending_activation: None,
            pending_detach: None,
            drop_zone_target: None,
            drop_zone_animated_at: None,
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
            drag_lift_duration: crate::tokens::motion::Durations::default().fast(),
            drag_overlay_owned_by_parent: false,
            min_pointer_target: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
        }
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
        match mark {
            Some(mark) => {
                self.marks.insert(id, mark);
            }
            None => {
                self.marks.remove(&id);
            }
        }
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
    /// of the focused group carries a keyline in your colour, the focus
    /// colour, and the others carry the lift alone.
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

    /// No-op before the first layout pass — nothing to spring from yet.
    fn snapshot_layout_for_spring(&mut self) {
        if self.tab_rects.len() != self.tabs.len() {
            return;
        }
        let mut snap: HashMap<u64, Rect> = HashMap::with_capacity(self.tabs.len());
        for (t, rect) in self.tabs.iter().zip(self.tab_rects.iter()) {
            snap.insert(t.id, *rect);
        }
        self.prev_tab_rects_by_id = snap;
        self.layout_anim_started = Some(crate::time::now());
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
        self.hover_intent = None;
        self.hover_motion_sample = None;
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
        self.drop_zone_animated_at = None;
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
        let is_pinned = self.pinned_ids.contains(&t.id);
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
        // Shared glass material (`chrome::shell::paint_glass_body`) so the strip
        // can't drift from the rest of the chrome.
        let shape = RoundedRect::from_rect(r, radius);
        crate::chrome::shell::paint_glass_body(scene, pill_transform, r, radius, theme, &look, 1.0);
        if is_active && self.shows_group_focus && self.group_focus == GroupFocus::Focused {
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
            scene.fill(
                vello::peniko::Fill::NonZero,
                pill_transform,
                theme.tokens.colors.intent.you,
                None,
                &keyline,
            );
        }
        // Rebind: the rest of the pill chrome paints under `pill_transform` so it
        // compresses with the body when pressed.
        let transform = pill_transform;

        Self::draw_pill_presence(scene, transform, &shape, r, t, theme, presence);

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
        let text_dim = theme.tokens.colors.text_dim;
        // The cursor-following copy suppresses hover, but carries the primary
        // label ink so an inactive source stays readable in motion.
        let text_color = if is_active || is_hover || pill.suppress_hover {
            text_primary
        } else {
            text_dim
        };
        let text_style = theme.text(TextRole::Label);
        let title_weight = TextWeight::Semibold;
        let (_tw, th) = tr.measure_style(
            &t.title,
            None,
            &text_style.clone().with_weight(title_weight),
        );
        let accent = self.accent_ids.get(&t.id).copied();
        if is_pinned {
            Self::draw_pill_pinned_marker(scene, transform, r, t, accent, text_color);
            return;
        }
        let leading_shift = if let Some(mark) = self.marks.get(&t.id).copied() {
            self.draw_pill_mark(
                scene,
                transform,
                theme,
                leading_glyph_rect(r),
                mark,
                text_color,
            );
            ICON_W + ICON_GAP
        } else {
            Self::draw_pill_leading_glyph(scene, transform, r, t, accent, text_color)
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
        t: &TabDescriptor,
        accent: Option<vello::peniko::Color>,
        text_color: vello::peniko::Color,
    ) {
        let icon_rect = square_at(r.center(), ICON_W);
        if t.icon
            .as_ref()
            .is_some_and(|image| icons::draw_app_image(scene, transform, icon_rect, image))
        {
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
        mark: TabMark,
        ink: vello::peniko::Color,
    ) {
        match mark {
            TabMark::Busy => {
                let turn = if self.reduce_motion {
                    0.0
                } else {
                    let t = crate::time::now()
                        .saturating_duration_since(busy_epoch())
                        .as_secs_f64();
                    t.fract() * std::f64::consts::TAU
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
        t: &TabDescriptor,
        accent: Option<vello::peniko::Color>,
        text_color: vello::peniko::Color,
    ) -> f64 {
        // The glyph starts on the pill's own text inset (`PILL_PAD_X`), so
        // its left edge lines up with where a glyph-less title starts, and
        // the title follows it at `ICON_GAP` — one inset and one gap for
        // every pill, instead of a glyph tucked 2 px inside the text.
        let icon_rect = leading_glyph_rect(r);
        if t.icon
            .as_ref()
            .is_some_and(|image| icons::draw_app_image(scene, transform, icon_rect, image))
        {
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
        let tab_key = PresenceKey::Tab {
            id: format!("{:x}", t.id),
        };
        let peers = presence.peers_on(&tab_key);
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
        let text_dim = theme.tokens.colors.text_dim;
        let close_rect_target = self.close_rects.get(i).copied().unwrap_or(Rect::ZERO);
        if !(close_affordance_visible(look, is_pinned) && close_rect_target.area() > 0.0) {
            return;
        }
        let alpha = chip_alpha_at(zone, CLOSE_HIT_PX);
        let close_r = shift_rect(close_rect_target, offset);
        let close_hover = !suppress_hover && Some(i) == self.hover_close_idx;
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
            (chip_hover_tint(theme, CHIP_HOVER_TINT_ALPHA), true)
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
            close_r,
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
        let chip_tint = chip_hover_tint(theme, ACTION_HOVER_TINT_ALPHA);
        let icon_idle = theme.tokens.colors.text_dim;
        let icon_active = theme.tokens.colors.text_primary;

        let max_rect_target = self.max_rects.get(i).copied().unwrap_or(Rect::ZERO);
        if self.show_maximize && max_rect_target.area() > 0.0 {
            let alpha = chip_alpha_at(zone, CLOSE_HIT_PX + ACTION_GAP + ACTION_W);
            let max_r = shift_rect(max_rect_target, offset);
            let max_hover = Some(i) == self.hover_max_idx;
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
            let search_hover = Some(i) == self.hover_search_idx;
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
    }

    /// Narrower than `draw_pill_inner`: no hover/search/maximize/presence — the
    /// pill is non-interactive during the fade.
    fn draw_closing_pill(
        scene: &mut Scene,
        transform: Affine,
        c: &ClosingTab,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = c.pill_rect;
        let radius = f64::from(theme.tokens.radii.control);
        let text_dim = theme.tokens.colors.text_dim;
        // Hovered tier, not inactive: active-id already shifted at close-start, and
        // an inactive pill is fully transparent, which would leave the title and ×
        // floating on the bar for the fade.
        crate::chrome::shell::paint_glass_body(
            scene,
            transform,
            r,
            radius,
            theme,
            &pill_appearance(theme, false, true, false),
            1.0,
        );
        let text_style = theme.text(TextRole::Label);
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
            c.close_rect,
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
                let span = theme.tokens.motion.durations.fast().as_secs_f64();
                if span <= f64::EPSILON {
                    1.0
                } else {
                    crate::ease_out((started.elapsed().as_secs_f64() / span).clamp(0.0, 1.0))
                }
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
        let hover_intended = self.hover_intent.is_some_and(|(idx, started)| {
            idx == i && started.elapsed() >= Duration::from_millis(HOVER_INTENT_MS)
        });
        // Keyboard focus is explicit intent and must remain immediate. Pointer
        // hover, by contrast, waits so crossing the tab row does not move it.
        if hover_intended || pressed || self.keyboard_focus && t.id == self.active_id {
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
        let span = ms_f64(REVEAL_ANIM_MS) / 1000.0;
        if span <= 0.0 {
            return a.to;
        }
        let t = a.started.elapsed().as_secs_f64() / span;
        if t >= 1.0 {
            a.to
        } else {
            // Cubic ease-out: leaves quickly, lands softly against its neighbours.
            let u = 1.0 - t.clamp(0.0, 1.0);
            a.from + (a.to - a.from) * (1.0 - u * u * u)
        }
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
            self.target_reveal(idx).reserve()
        };
        ((reserve - Reveal::None.reserve()) / HOVER_GROWTH_PX).clamp(0.0, 1.0)
    }

    fn advance_reveal(&mut self, id: u64, target: Reveal) -> f64 {
        let to = target.reserve();
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
        if self.reduce_motion {
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

    /// Test/shot clock control for the pointer-intent dwell. Production input
    /// still arms this exclusively through `on_cursor_moved`.
    #[cfg(any(test, debug_assertions))]
    pub fn hold_hover_intent_at(&mut self, at: Duration) -> bool {
        let Some((idx, _)) = self.hover_intent else {
            return false;
        };
        let now = crate::time::now();
        self.hover_intent = Some((idx, now.checked_sub(at).unwrap_or(now)));
        true
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
        let span = Duration::from_millis(REVEAL_ANIM_MS);
        self.reveal_anim
            .values()
            .any(|a| (a.from - a.to).abs() > f64::EPSILON && a.started.elapsed() < span)
    }

    fn natural_pill_width(&self, i: usize, reserve: f64) -> f64 {
        let Some(t) = self.tabs.get(i) else {
            return PILL_MIN_PX;
        };
        if self.pinned_ids.contains(&t.id) {
            return PINNED_W;
        }
        let title_w = self.shaped_title_width(i);
        let lead = self.leading_zone(i);
        (PILL_PAD_X + lead + title_w + reserve).max(PILL_MIN_PX)
    }

    fn shaped_title_width(&self, i: usize) -> f64 {
        let Some(t) = self.tabs.get(i) else {
            return 0.0;
        };
        self.title_advance
            .get(&t.id)
            .filter(|(title, _, _)| title == &t.title)
            .map_or_else(
                || {
                    // Title lengths never approach `u32::MAX` chars, so this is
                    // lossless in practice; `f64::from` preserves the value.
                    let chars = u32::try_from(t.title.chars().count()).unwrap_or(u32::MAX);
                    f64::from(chars) * APPROX_CHAR_W
                },
                |(_, _, w)| *w,
            )
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
        if self.pinned_ids.contains(&t.id) {
            return None;
        }
        let style = theme.text(TextRole::Label);
        let title = t.title.clone();
        let id = t.id;
        // Record the shaped advance here — `apply_layout` has no text renderer of
        // its own, so it reads this back on the next pass.
        let (full_w, _) = tr.measure_style(
            &title,
            None,
            &style.clone().with_weight(TextWeight::Semibold),
        );
        self.title_advance
            .insert(id, (title.clone(), style.clone(), f64::from(full_w)));
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
    /// but still rendering its `CLOSE_FADE_MS` animation. A caller that
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
        self.drop_zone_animated_at = None;
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
    pub fn take_drag(&mut self) -> Option<DragPayload> {
        self.pending_drag.take()
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
    /// Shifts the bar's whole painted region — background, pills, hit rects —
    /// right by `px`, after `apply_layout` has already fit the row. A host
    /// drawing its own control over the bar's left corner (e.g. an overlay
    /// sidebar's hamburger toggle) reserves room for it here, rather than
    /// teaching the fit itself about a chip this widget doesn't know exists.
    /// No-op past a `px` that would collapse the bar.
    pub fn inset_left(&mut self, px: f64) {
        if px <= 0.0 || self.rect.width() <= px {
            return;
        }
        let shift = vello::kurbo::Vec2::new(px, 0.0);
        self.rect = Rect::new(self.rect.x0 + px, self.rect.y0, self.rect.x1, self.rect.y1);
        for r in self
            .tab_rects
            .iter_mut()
            .chain(self.close_rects.iter_mut())
            .chain(self.close_hit_rects.iter_mut())
            .chain(self.max_rects.iter_mut())
            .chain(self.search_rects.iter_mut())
        {
            *r = shift_rect(*r, shift);
        }
        self.new_tab_rect = shift_rect(self.new_tab_rect, shift);
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
            if max.area() > 0.0 && max.contains(p) {
                Some((i, ActionChip::Maximize))
            } else if search.area() > 0.0 && search.contains(p) {
                Some((i, ActionChip::Search))
            } else {
                None
            }
        });
        glyph.or_else(|| {
            (0..self.tabs.len()).find_map(|i| {
                let max = self.max_rects.get(i).copied().unwrap_or(Rect::ZERO);
                let search = self.search_rects.get(i).copied().unwrap_or(Rect::ZERO);
                if self.action_hit_rect(max).contains(p) {
                    Some((i, ActionChip::Maximize))
                } else if self.action_hit_rect(search).contains(p) {
                    Some((i, ActionChip::Search))
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
        let mut look = pill_appearance(theme, is_active || is_carried, is_hover, is_pressed);
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
                let hovered = pill_appearance(theme, false, true, false);
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
        let close_hover = Some(idx) == self.hover_close_idx;
        let is_hover = Some(idx) == self.hover_idx;
        if close_hover {
            Some(theme.tokens.colors.semantic.danger.with_alpha_factor(0.8))
        } else if is_hover {
            Some(chip_hover_tint(theme, CHIP_HOVER_TINT_ALPHA))
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
        let close_hover = Some(idx) == self.hover_close_idx;
        let is_active = self.tabs[idx].id == self.active_id;
        Some(if close_hover {
            vello::peniko::Color::from_rgba8(255, 255, 255, 255)
        } else if is_active {
            theme.tokens.colors.text_primary
        } else {
            theme.tokens.colors.text_dim
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
            theme.tokens.colors.text_dim
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
//                 (= s8, the minimum pointer target), ACTION_GAP 4 (= s1),
//                 ACTION_HIT_INSET_Y 4 (= s1), PINNED_W 28, MIN_TITLE_PX 40,
//                 PILL_MIN_PX 84
//   off the grid  TAB_PILL_INSET_Y 6 (pill 32 tall inside a 44 bar — the
//                 bar height is pinned), PILL_GAP 6, ICON_GAP 6, CLOSE_W 18,
//                 ACTION_W 14, ICON_W 14, CLOSE_INSET 3, TITLE_CHIP_GAP 10,
//                 CHIP_FADE_PX 10, PILL_SQUEEZE_FLOOR_PX 54
//
// The off-grid values are lucide glyph sizes and optical gaps around a
// 32 px pill; they are kept, named, and audited here rather than rounded,
// because rounding them moves every pill width the layout tests pin.

pub const TAB_BAR_HEIGHT: f64 = 44.0;
/// 44 – 2×6 = 32 px pill height and pointer-sized + affordance.
pub const TAB_PILL_INSET_Y: f64 = 6.0;
/// Square at pill height so the "+" chip reads as part of the pill row.
pub const NEW_TAB_CHIP_PX: f64 = TAB_BAR_HEIGHT - 2.0 * TAB_PILL_INSET_Y;

const PILL_GAP: f64 = 6.0;
/// Text inset on either end of a pill; the leading glyph starts here too.
const PILL_PAD_X: f64 = 12.0;
const CLOSE_W: f64 = 18.0;
/// The glyph remains compact; its pointer target does not.
const CLOSE_HIT_PX: f64 = 32.0;
const CLOSE_INSET: f64 = 3.0;
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
const ACTION_GAP: f64 = 4.0;
const ICON_W: f64 = 14.0;
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
/// Below this a pill can't host a legible title fragment alongside the close
/// chip, so squeezing further would be worse than eliding harder.
const PILL_SQUEEZE_FLOOR_PX: f64 = 54.0;

/// Narrowest a pill's title zone squeezes to before the reveal gives up chips
/// instead of characters — the cluster drops search, then maximize, then close.
const MIN_TITLE_PX: f64 = 40.0;

/// Right edge to the left edge of the leftmost revealed chip: close, then
/// maximize, then search.
const CHIP_CLUSTER_W: f64 = CLOSE_HIT_PX + ACTION_GAP + ACTION_W + ACTION_GAP + ACTION_W;

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
/// `width - HOVER_GROWTH_PX`, so neighbours translate but are never re-fitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reveal {
    None,
    /// Close chip only — an active pill at rest.
    Close,
    /// Close, maximize and search — a hovered pill.
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

/// The headroom the row holds back so growth never re-fits anyone.
const HOVER_GROWTH_PX: f64 = Reveal::Full.reserve() - Reveal::None.reserve();

/// Matched to [`LAYOUT_ANIM_MS`] so a pill opening and neighbours sliding
/// aside read as one motion.
const REVEAL_ANIM_MS: u64 = LAYOUT_ANIM_MS;
/// A pointer merely crossing the row must not reflow it. Secondary actions
/// appear only after a short, deliberate hover.
const HOVER_INTENT_MS: u64 = 200;
/// Cursor travel faster than this is navigation, not intent.
const HOVER_INTENT_MAX_SPEED_PX_PER_MS: f64 = 0.45;

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
    let close_target_x0 = r.x1 - CLOSE_HIT_PX;
    let close_x0 = r.x1 - close_w - close_inset;
    // Secondary actions sit outside the close target. This keeps the 32px
    // close target from swallowing a visible maximize/search button.
    let max_x0 = close_target_x0 - max_w - ACTION_GAP;
    let search_x0 = max_x0 - search_w - ACTION_GAP;
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
    let max_rect = if effective >= CLOSE_HIT_PX + ACTION_GAP + ACTION_W {
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
/// The instant a busy mark's turn is measured from, shared by every strip so
/// two loading tabs turn together.
fn busy_epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(crate::time::now)
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
}

/// The round a hovered action chip paints behind its glyph: `CLOSE_W`
/// square on the chip's centre, so it is the same disc the close chip
/// paints, not a capsule the height of the chip's pointer target.
fn chip_hover_ring(chip: Rect) -> Rect {
    square_at(chip.center(), CLOSE_W)
}

/// A hover wash for a chip, derived against the chrome's ground the way a
/// hairline is (HIG §9.5): white over a dark pill, black over a light one.
/// A literal white at low alpha was invisible on the light palettes.
fn chip_hover_tint(theme: &Theme, alpha: u8) -> vello::peniko::Color {
    Colors::hairline_on(theme.tokens.colors.surfaces.base, alpha)
}

impl Widget for TabBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion || self.host_reduced_motion;
        self.min_pointer_target = f64::from(theme.tokens.metrics.min_pointer_target);
        // `TAB_BAR_HEIGHT` is a fixed 44px layout constant, well inside `f32`'s
        // exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "TAB_BAR_HEIGHT is a small fixed layout constant"
        )]
        let bar_height = TAB_BAR_HEIGHT as f32;
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
        self.tab_rects.clear();
        self.close_rects.clear();
        self.close_hit_rects.clear();
        self.max_rects.clear();
        self.search_rects.clear();
        // Pre-size so the visual-order pass can write `tab_rects[data_idx]`
        // directly — data index stays the canonical key for every cache.
        self.tab_rects.resize(self.tabs.len(), Rect::ZERO);
        self.close_rects.resize(self.tabs.len(), Rect::ZERO);
        self.close_hit_rects.resize(self.tabs.len(), Rect::ZERO);
        self.max_rects.resize(self.tabs.len(), Rect::ZERO);
        self.search_rects.resize(self.tabs.len(), Rect::ZERO);
        self.reveal_px.clear();
        self.reveal_px
            .resize(self.tabs.len(), Reveal::None.reserve());
        self.mark_rects.clear();
        self.mark_rects.resize(self.tabs.len(), Rect::ZERO);

        let (visual_order, widths, grow) = self.compute_pill_layout();

        let gap = PILL_GAP;
        let close_w = CLOSE_W;
        let max_w = ACTION_W;
        let search_w = ACTION_W;
        let close_inset = CLOSE_INSET;
        let pill_y0 = self.rect.y0 + TAB_PILL_INSET_Y;
        let pill_y1 = self.rect.y1 - TAB_PILL_INSET_Y;

        // Absorbs float drift only — `fit_pill_widths` already sized the row to
        // fit. Disabled with no width yet, where clamping would collapse to the origin.
        let right_limit = if self.rect.width() > 0.0 {
            self.rect.x1
        } else {
            f64::INFINITY
        };
        let mut x = self.rect.x0;
        for (slot, &i) in visual_order.iter().enumerate() {
            let is_pinned = self
                .tabs
                .get(i)
                .is_some_and(|t| self.pinned_ids.contains(&t.id));
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
            if let Some(slot) = self.search_rects.get_mut(i) {
                *slot = search_rect;
            }
            x = r.x1 + gap;
        }
        // `x` is the strip's write head; the "+" chip is suppressed once pills
        // fill the bar.
        let plus_x1 = x + NEW_TAB_CHIP_PX;
        self.new_tab_rect = if self.new_tab && plus_x1 <= self.rect.x1 {
            Rect::new(x, pill_y0, plus_x1, pill_y1)
        } else {
            Rect::ZERO
        };
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
        // Guards one stale frame after a detach/insert mutates `tabs` outside the
        // layout pass; next `apply_layout` fixes it.
        if self.tab_rects.len() != self.tabs.len() {
            return;
        }
        // Only pass with a TextRenderer — shapes every title up front.
        self.visible_titles = (0..self.tabs.len())
            .map(|i| self.visible_title(i, tr, theme))
            .collect();

        // After a reorder/insert/remove, every surviving pill interpolates from
        // its previous rect to `tab_rects[i]` over `LAYOUT_ANIM_MS`; a brand-new
        // tab (no snapshot) renders at target immediately.
        let layout_t = self.layout_anim_started.and_then(|t| {
            let elapsed = t.elapsed();
            if elapsed >= Duration::from_millis(LAYOUT_ANIM_MS) {
                None
            } else {
                let raw = elapsed.as_secs_f64() / (ms_f64(LAYOUT_ANIM_MS) / 1000.0);
                Some(crate::ease_out(raw))
            }
        });

        // Stop `needs_animation_frame` pumping forever once every insert-fade elapses.
        let fade_window = Duration::from_millis(INSERT_FADE_MS);
        self.inserted_at
            .retain(|_, started| started.elapsed() < fade_window);

        self.draw_pills(scene, transform, tr, theme, presence, layout_t, fade_window);
        self.draw_keyboard_focus(scene, transform, theme);
        self.draw_new_tab_chip(scene, transform, tr, theme);
        self.draw_closing_tabs(scene, transform, tr, theme);
        self.draw_drop_indicator(scene, transform, theme);
        self.draw_drag_ghost(scene, transform, tr, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
        for (tab, rect) in self.tabs.iter().zip(self.tab_rects.iter()) {
            out.push((format!("tab:{}", tab.title), *rect));
        }
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
                let node = if self.modified_ids.contains(&t.id) {
                    node.with_description(crate::i18n!("tab-modified"))
                } else {
                    node
                };
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
        if let Some(id) = target
            .strip_prefix("tab-close:")
            .and_then(|value| value.parse::<u64>().ok())
        {
            return self.closable && !self.pinned_ids.contains(&id) && self.start_closing(id);
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
    /// size change. Tab additions and removals are structural.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn is_focusable(&self) -> bool {
        !self.tabs.is_empty()
    }
    fn set_focus(&mut self, focused: bool) {
        self.keyboard_focus = focused;
    }
    fn is_focused(&self) -> bool {
        self.keyboard_focus
    }
    fn wants_pointer_capture(&self) -> bool {
        self.press_origin.is_some() || self.pending_drag.is_some()
    }
    fn accepts_drop(&self, payload: &DragPayload) -> bool {
        matches!(payload, DragPayload::Tab { .. })
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        // The busy mark stands still and a drag lifts at once.
        self.host_reduced_motion = reduced;
        self.reduce_motion = reduced;
        for child in self.children_mut() {
            child.set_motion_reduced(reduced);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep pumping redraws while any curve is in flight: drop-zone expand,
        // layout spring, insert fade, close fade, drop-line fade-in, or reveal.
        let drop_zone = self
            .drop_zone_animated_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(DROP_ZONE_ANIM_MS));
        let layout = self
            .layout_anim_started
            .is_some_and(|t| t.elapsed() < Duration::from_millis(LAYOUT_ANIM_MS));
        let insert_fade = self
            .inserted_at
            .values()
            .any(|t| t.elapsed() < Duration::from_millis(INSERT_FADE_MS));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "CLOSE_FADE_MS is a small fixed positive animation constant"
        )]
        let close_fade_ms = CLOSE_FADE_MS as u128;
        let close_fade = self
            .closing_tabs
            .iter()
            .any(|c| c.started_at.elapsed().as_millis() < close_fade_ms);
        let hover_drop_fade = self
            .hover_drop_animated_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(HOVER_DROP_FADE_MS));
        // Keeps the curve running once the pointer stops moving, until an
        // opening pill finishes.
        let reveal = self.reveal_in_flight();
        let busy = !self.reduce_motion && self.marks.values().any(|m| *m == TabMark::Busy);
        let drag_lift = !self.reduce_motion
            && self.pending_drag.is_some()
            && self
                .drag_lifted_at
                .is_some_and(|started| started.elapsed() < self.drag_lift_duration);
        let hover_intent = self
            .hover_intent
            .is_some_and(|(_, started)| started.elapsed() < Duration::from_millis(HOVER_INTENT_MS));
        drop_zone
            || layout
            || insert_fade
            || close_fade
            || hover_drop_fade
            || reveal
            || drag_lift
            || hover_intent
            || busy
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
            let target = self.target_reveal(i);
            animated.push(self.advance_reveal(id, target));
        }
        // Visual-slot → data-index: pinned first, then unpinned. `self.tabs`
        // stays untouched so caches keyed by data index still apply.
        let mut visual_order: Vec<usize> = Vec::with_capacity(self.tabs.len());
        for (i, t) in self.tabs.iter().enumerate() {
            if self.pinned_ids.contains(&t.id) {
                visual_order.push(i);
            }
        }
        for (i, t) in self.tabs.iter().enumerate() {
            if !self.pinned_ids.contains(&t.id) {
                visual_order.push(i);
            }
        }
        let pinned_flags: Vec<bool> = visual_order
            .iter()
            .map(|&i| {
                self.tabs
                    .get(i)
                    .is_some_and(|t| self.pinned_ids.contains(&t.id))
            })
            .collect();
        // Fit at resting widths (pointer elsewhere) — see [`Reveal`] for why.
        let resting: Vec<f64> = visual_order
            .iter()
            .map(|&i| self.natural_pill_width(i, self.resting_reveal(i).reserve()))
            .collect();
        let headroom = if pinned_flags.iter().any(|p| !p) {
            HOVER_GROWTH_PX
        } else {
            0.0
        };
        let laid_out = self.rect.width() > 0.0;
        let fit_avail = if laid_out {
            (self.rect.width() - headroom - NEW_TAB_CHIP_PX - PILL_GAP).max(1.0)
        } else {
            self.rect.width()
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

            // Fade alpha 0→1 and y -10→0 over `INSERT_FADE_MS`; `fade_alpha` < 1.0
            // wraps the paint in a `push_layer`.
            let (fade_alpha, fade_y): (f32, f64) = match self.inserted_at.get(&t.id) {
                Some(started) => {
                    let elapsed = started.elapsed();
                    if elapsed >= fade_window {
                        (1.0, 0.0)
                    } else {
                        let raw = (elapsed.as_secs_f64() / (ms_f64(INSERT_FADE_MS) / 1000.0))
                            .clamp(0.0, 1.0);
                        let eased = crate::ease_out(raw);
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

            let pill_transform = if fade_y == 0.0 {
                transform
            } else {
                compose(transform, Affine::translate((0.0, fade_y)))
            };
            let pill_alpha_active = combined_alpha < 1.0;
            // Wider than the pill so hover-reveal action icons fade in cleanly.
            let close_w = self.close_rects.get(i).map_or(0.0, Rect::width);
            let clip = Rect::new(r.x0 - 2.0, r.y0 - 4.0, r.x1 + close_w + 60.0, r.y1 + 4.0);
            // Each pill is a damage part: hovering one repaints that pill.
            // It paints inside the clip a fading pill is held to, at its
            // fade offset; a press only shrinks it.
            let bounds = vec![
                transform.transform_rect_bbox(clip.inflate(PILL_PART_MARGIN, PILL_PART_MARGIN)),
                pill_transform
                    .transform_rect_bbox(clip.inflate(PILL_PART_MARGIN, PILL_PART_MARGIN)),
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
    /// and large text alike.
    fn draw_keyboard_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.keyboard_focus {
            return;
        }
        let Some((idx, _)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.id == self.active_id)
        else {
            return;
        };
        let Some(rect) = self
            .tab_rects
            .get(idx)
            .copied()
            .filter(|rect| rect.area() > 0.0)
        else {
            return;
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
            theme.tokens.colors.intent.you,
            None,
            &ring,
        );
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
        // `CLOSE_FADE_MS` is a small fixed positive animation-timing constant.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "CLOSE_FADE_MS is a small fixed positive animation constant"
        )]
        let close_window = Duration::from_millis(CLOSE_FADE_MS as u64);
        for c in &self.closing_tabs {
            let elapsed = c.started_at.elapsed();
            if elapsed >= close_window {
                continue;
            }
            let raw = (elapsed.as_secs_f64() / (f64::from(CLOSE_FADE_MS) / 1000.0)).clamp(0.0, 1.0);
            let eased = crate::ease_out(raw);
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
            Self::draw_closing_pill(scene, close_transform, c, tr, theme);
            scene.pop_layer();
        }
    }

    /// Drop-indicator line at `hover_drop_idx`, serving both the single-bar
    /// case and the cross-group case (where `pending_drag.is_some()` is false
    /// on sibling bars). Eased fade-in over `HOVER_DROP_FADE_MS`.
    fn draw_drop_indicator(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(gap_idx) = self.hover_drop_idx else {
            return;
        };
        if self.tab_rects.is_empty() {
            return;
        }
        let now = crate::time::now();
        let elapsed_ms = self.hover_drop_animated_at.map_or_else(
            || ms_f64(HOVER_DROP_FADE_MS),
            |t| duration_ms_f64(now.duration_since(t).as_millis()),
        );
        let raw_t = (elapsed_ms / ms_f64(HOVER_DROP_FADE_MS)).clamp(0.0, 1.0);
        let eased_t = crate::ease_out(raw_t);
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
            Colors::hairline_on(theme.tokens.colors.surfaces.base, alpha_u8),
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
        let now = crate::time::now();
        let speed = self.hover_motion_sample.map_or(0.0, |(previous, sampled)| {
            let elapsed_ms = now.duration_since(sampled).as_secs_f64() * 1000.0;
            if elapsed_ms <= f64::EPSILON {
                f64::INFINITY
            } else {
                point_delta(p, previous).hypot() / elapsed_ms
            }
        });
        self.hover_motion_sample = Some((p, now));
        self.last_cursor = p;
        // Pinned pills leave their close/max/search rects at `Rect::ZERO`;
        // skip them so a cursor crossing (0,0) doesn't latch a hover on a
        // glyph that isn't painted.
        let is_pinned_at = |i: usize| {
            self.tabs
                .get(i)
                .is_some_and(|t| self.pinned_ids.contains(&t.id))
        };
        // The forgiving 32px click target can consume an entire squeezed pill.
        // Only the painted glyph suppresses reveal intent; the larger target
        // remains in force for presses without making the other chips
        // unreachable on narrow active or modified tabs.
        self.hover_close_idx = self
            .close_rects
            .iter()
            .position(|r| r.area() > 0.0 && r.contains(p))
            .filter(|&i| !is_pinned_at(i));
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
        self.hover_idx = self.tab_rects.iter().position(|r| r.contains(p));
        let close_excluded = self.hover_close_idx.is_some();
        self.hover_intent = match (self.hover_intent, self.hover_idx) {
            (_, None) => None,
            (_, Some(_)) if close_excluded => None,
            (Some((candidate, started)), Some(idx))
                if candidate == idx && speed <= HOVER_INTENT_MAX_SPEED_PX_PER_MS =>
            {
                Some((candidate, started))
            }
            // Entering quickly starts a fresh dwell instead of dropping the
            // candidate. If the pointer stops here there is no later motion
            // event to arm it; if transit continues, each event resets the
            // timestamp and the reveal still cannot mature mid-flight.
            (_, Some(idx)) => Some((idx, now)),
        };
        self.hover_new_tab = self.new_tab_rect.area() > 0.0 && self.new_tab_rect.contains(p);
        if let Some((idx, origin)) = self.press_origin
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
            if new_target != self.drop_zone_target {
                self.drop_zone_target = new_target;
                self.drop_zone_animated_at = Some(crate::time::now());
            }
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
        let is_pinned_at = |i: usize| {
            self.tabs
                .get(i)
                .is_some_and(|t| self.pinned_ids.contains(&t.id))
        };
        let close_idx = (0..self.tabs.len())
            .find(|&i| {
                self.close_hit_rect(i)
                    .is_some_and(|r| r.contains(cursor_pos))
            })
            .filter(|&i| !is_pinned_at(i));
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
        if let Some(idx) = self.hover_idx {
            // Can go stale if a neighbouring widget shrunk `tabs`/`tab_rects`
            // between move and press; clear so the next move repopulates it.
            let Some(pill) = self.tab_rects.get(idx).copied() else {
                self.hover_idx = None;
                return;
            };
            let origin = pill.center();
            self.press_origin = Some((idx, origin));
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
                    .all(|run| run.font_size <= theme.tokens.typography.size(TextRole::Label))
            );
            assert!(
                scene
                    .encoding()
                    .resources
                    .glyph_runs
                    .iter()
                    .any(
                        |run| (run.font_size - theme.tokens.typography.size(TextRole::Label)).abs()
                            < 0.01
                    ),
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

            b.hover_idx = Some(1);
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
                    b.hover_idx = Some(1);
                    b.hover_intent = Some((
                        1,
                        Instant::now()
                            .checked_sub(Duration::from_millis(HOVER_INTENT_MS + 1))
                            .expect("hover intent instant"),
                    ));
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
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            b.handle_event(
                &press,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );

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
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            b.handle_event(
                &press,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );

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
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            b.handle_event(
                &press,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );

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
                let ground = theme.tokens.colors.surfaces.base;
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

                let idle = pill_appearance(&theme, false, false, false);
                let hover = pill_appearance(&theme, false, true, false);
                let active = pill_appearance(&theme, true, false, false);
                let active_hover = pill_appearance(&theme, true, true, false);

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
                    active.border.is_some() && active.inner_glow.is_some(),
                    "{}: active pill must paint a border + inner glow so it reads \
                 as raised",
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
            let pressed_inactive = pill_appearance(&theme, false, false, true);
            let active = pill_appearance(&theme, true, false, false);

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
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(90.0, 20.0),
            };
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            b.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
            assert_eq!(
                b.drop_zone_target(),
                Some(1),
                "cursor on B's left half → gap 1"
            );
            // Sampled immediately after the target change — well within DROP_ZONE_ANIM_MS.
            assert!(
                b.needs_animation_frame(),
                "drop-zone animation must request redraws"
            );

            // Move cursor past pill C's midpoint → gap 3 (append).
            let event = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(230.0, 20.0),
            };
            b.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
            assert_eq!(b.drop_zone_target(), Some(3), "past C's mid → gap 3");

            let release = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            b.handle_event(
                &release,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
            assert_eq!(b.drop_zone_target(), None, "release clears target");
            assert!(
                b.hover_intent.is_some() && b.needs_animation_frame(),
                "release clears the drag target but preserves the stationary pill-hover dwell",
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
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(90.0, 20.0),
            };
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            b.handle_event(
                &event,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
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
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            b.handle_event(
                &release,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
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
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "CLOSE_FADE_MS is a small fixed positive animation constant"
            )]
            let close_fade_ms = CLOSE_FADE_MS as u64;
            assert!(
                stamp.elapsed() < Duration::from_millis(close_fade_ms),
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
            // here consider it expired, rather than waiting out `INSERT_FADE_MS`.
            let mut b = bar();
            let now = crate::time::now();
            b.inserted_at.insert(
                7,
                now.checked_sub(Duration::from_millis(INSERT_FADE_MS + 50))
                    .unwrap_or(now),
            );
            assert!(
                !b.needs_animation_frame(),
                "elapsed insert fade must not pump frames",
            );
        }

        // Case `layout_spring_clears_itself_after_duration`.
        {
            // Backdate `layout_anim_started` rather than waiting out `LAYOUT_ANIM_MS`.
            let mut b = bar();
            let now = crate::time::now();
            b.layout_anim_started = Some(
                now.checked_sub(Duration::from_millis(LAYOUT_ANIM_MS + 50))
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
            let dark = chip_hover_tint(&Theme::dark(), ACTION_HOVER_TINT_ALPHA);
            let light = chip_hover_tint(&Theme::light(), ACTION_HOVER_TINT_ALPHA);
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
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;

            // Cursor over the closing pill's old close chip.
            let cursor = Point::new(155.0, 20.0);
            let moved = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(cursor.x, cursor.y),
            };
            b.handle_event(
                &moved,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
            // Pressing the closing pill's old close chip must NOT re-enter start_closing(2).
            let before_count = b.closing_tab_ids().len();
            let before_tabs: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
            let press = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            b.handle_event(
                &press,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
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
            std::thread::sleep(Duration::from_millis(REVEAL_ANIM_MS + 40));
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
    fn inset_left_shifts_every_rect_until_it_exceeds_the_bar() {
        // Case `inset_left_shifts_the_bar_and_every_cached_rect_together`.
        {
            let mut b = bar_with_laid_out_rects();
            let before_bar = b.bar_rect();
            let before_tab0 = b.tab_rects()[0];
            let before_close0 = b.close_rects[0];

            b.inset_left(40.0);

            assert_eq!(
                b.bar_rect(),
                Rect::new(
                    before_bar.x0 + 40.0,
                    before_bar.y0,
                    before_bar.x1,
                    before_bar.y1
                ),
                "the left edge moves in, the right edge does not",
            );
            assert_eq!(
                b.tab_rects()[0],
                before_tab0 + vello::kurbo::Vec2::new(40.0, 0.0),
                "every pill must move with the bar, not just its background",
            );
            assert_eq!(
                b.close_rects[0],
                before_close0 + vello::kurbo::Vec2::new(40.0, 0.0),
                "cached hit rects must move too, or a click would land on the \
             wrong pill after an inset",
            );
        }

        // Case `inset_left_is_a_no_op_past_the_bars_own_width`.
        {
            let mut b = bar_with_laid_out_rects();
            let before = b.bar_rect();
            b.inset_left(before.width() + 1.0);
            assert_eq!(
                b.bar_rect(),
                before,
                "an inset that would collapse the bar must be refused, not \
             produce a negative-width rect",
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
            b.hover_idx = Some(1);
            b.hover_intent = Some((
                1,
                Instant::now()
                    .checked_sub(Duration::from_millis(HOVER_INTENT_MS + 1))
                    .expect("hover intent instant"),
            ));
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
            b.hover_intent = None;
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
                        let look = pill_appearance(&theme, is_active, is_hover, is_pressed);
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

            // The search glyph's trailing edge lies inside the enlarged maximize
            // target; the painted glyph wins over the target.
            let on_search_glyph = Point::new(search.x1 - 1.0, search.center().y);
            assert!(
                b.action_hit_rect(max).contains(on_search_glyph),
                "the overlap this test relies on exists"
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
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let mut tree = TaffyTree::<()>::new();
            let node = tree.leaf_node(Style::default());
            let mut handled = false;
            let mut focus_target = None;
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            b.handle_event(
                &press,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut state),
                    path: &root_id,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );
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

    /// Aims at the pill's leading third so the cursor never lands on a chip.
    fn hover_pill(b: &mut TabBar, idx: usize, width: f64, theme: &crate::theme::Theme) {
        let r = b.tab_rects()[idx];
        let p = Point::new(r.x0 + r.width() * 0.25, r.center().y);
        let mut handled = false;
        dispatch_cursor(b, p, &mut handled);
        assert!(b.hold_hover_intent_at(Duration::from_millis(HOVER_INTENT_MS.saturating_add(1))));
        lay_out_at_width_with(b, width, theme);
    }

    fn dispatch_cursor(b: &mut TabBar, p: Point, handled: &mut bool) {
        use std::collections::HashMap;
        let mut doc = automerge::AutoCommit::new();
        let obj = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = {
            let theme = crate::theme::Theme::dark();
            b.build_layout(&mut taffy, &theme, &mut node_map)
        };
        let mut focus = None;
        b.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: crate::widget::Edits::lend(&mut doc),
                path: &obj,
                handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Dispatches a left `MouseInput` event at the given press/release state —
    /// shared by every test that drives a click through `handle_event` without
    /// a real pointer.
    fn dispatch_left_button(b: &mut TabBar, state: ElementState, handled: &mut bool) {
        use std::collections::HashMap;
        let mut doc = automerge::AutoCommit::new();
        let obj = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = {
            let theme = crate::theme::Theme::dark();
            b.build_layout(&mut taffy, &theme, &mut node_map)
        };
        let mut focus = None;
        b.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state,
                button: MouseButton::Left,
            },
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: crate::widget::Edits::lend(&mut doc),
                path: &obj,
                handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
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

    #[test]
    fn pointer_reveal_waits_for_hover_intent() {
        // Case `pointer_reveal_waits_for_hover_intent_timeout`.
        {
            let theme = Theme::dark().with_reduce_motion(true);
            let width = 900.0;
            let mut b = four_tab_bar();
            lay_out_at_width_with(&mut b, width, &theme);
            let idle = b.tab_rects()[1].width();
            let p = Point::new(b.tab_rects()[1].x0 + 12.0, b.tab_rects()[1].center().y);
            let mut handled = false;
            dispatch_cursor(&mut b, p, &mut handled);
            lay_out_at_width_with(&mut b, width, &theme);
            assert!((b.tab_rects()[1].width() - idle).abs() < 0.01);
            assert!(b.max_rects()[1].area() == 0.0 && b.search_rects()[1].area() == 0.0);

            assert!(
                b.hold_hover_intent_at(Duration::from_millis(HOVER_INTENT_MS.saturating_add(1)))
            );
            lay_out_at_width_with(&mut b, width, &theme);
            assert!(b.tab_rects()[1].width() > idle + 1.0);
            assert!(b.max_rects()[1].area() > 0.0 && b.search_rects()[1].area() > 0.0);
        }

        // Case `leaving_or_hovering_close_cancels_hover_intent`.
        {
            let mut b = ghost_test_bar();
            let mut handled = false;
            dispatch_cursor(&mut b, Point::new(90.0, 20.0), &mut handled);
            assert!(b.hover_intent.is_some());
            dispatch_cursor(&mut b, Point::new(90.0, 50.0), &mut handled);
            assert_eq!(b.hover_intent, None);

            b.hover_motion_sample = None;
            dispatch_cursor(&mut b, Point::new(90.0, 20.0), &mut handled);
            dispatch_cursor(&mut b, Point::new(150.0, 20.0), &mut handled);
            assert_eq!(b.hover_close_idx, Some(1));
            assert_eq!(b.hover_intent, None);
        }

        // Case `fast_transit_across_tabs_restarts_the_reveal_dwell`.
        {
            let mut b = ghost_test_bar();
            let now = Instant::now();
            b.hover_motion_sample = Some((Point::new(10.0, 20.0), now));
            let mut handled = false;
            dispatch_cursor(&mut b, Point::new(190.0, 20.0), &mut handled);
            assert_eq!(b.hover_idx, Some(2));
            let (candidate, started) = b.hover_intent.expect("destination dwell starts");
            assert_eq!(candidate, 2);
            assert!(
                started.elapsed() < Duration::from_millis(HOVER_INTENT_MS),
                "fast transit must not carry a mature reveal into its destination",
            );
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
            std::thread::sleep(Duration::from_millis(REVEAL_ANIM_MS / 3));
            lay_out_at_width_with(&mut b, width, &theme);
            let opening = b.tab_rects()[1].width();
            assert!(
                opening > idle && opening < idle + HOVER_GROWTH_PX,
                "part way along the curve the pill is {opening} wide; it must be \
             between its idle width {idle} and its open width {}",
                idle + HOVER_GROWTH_PX,
            );

            std::thread::sleep(Duration::from_millis(REVEAL_ANIM_MS + 40));
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
        assert!(
            tabs.needs_animation_frame() || theme.reduce_motion,
            "a busy mark turns"
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
        let you = theme
            .tokens
            .colors
            .intent
            .you
            .premultiply()
            .to_rgba8()
            .to_u32();
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
            scene.encoding().draw_data.contains(&you)
        };
        let mut documents = bar();
        assert!(
            !paint(&mut documents),
            "a strip of documents shows no keyline"
        );
        let mut groups = bar();
        groups.set_group_focused(true);
        assert!(
            paint(&mut groups),
            "the focused window group's active tab does"
        );
    }
}
