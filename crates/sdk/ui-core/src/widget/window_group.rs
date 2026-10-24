use crate::icons;
use crate::num::f32_of_f64;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::TaffyTreeExt;
use crate::widget::edge_resize::{WidthLimits, WindowEdge};
use crate::widget::tabs::close_fade;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{
    FloatingSurfaceProvider, TabBar, TabDescriptor, TileContent, TileHost, Widget,
};
use automerge::AutoCommit;
use std::collections::HashMap;
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::Key;

use crate::tokens::TextRole;
/// Immutable presentation inspection in root logical coordinates.
/// Reading this does not lay out, paint, reveal controls or drain input edges.
/// An edge drag a [`WindowGroup`]'s row is carrying out on it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GroupEdgeDrag {
    /// The edge that follows the pointer; the other one holds still.
    moving: WindowEdge,
    /// The width before the drag, which a cancel goes back to.
    before: WindowWidth,
    /// The width this frame, in whole logical pixels.
    live_px: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowGroupBounds {
    pub group: Rect,
    pub client: Option<Rect>,
    pub tab_bar: Rect,
    pub maximize: Option<Rect>,
    pub restore: Option<Rect>,
    pub close: Option<Rect>,
}

/// Group widths are the desktop model's: a fraction of the strip, never
/// pixels, with one set of tiers and clamps for every host.
pub use tilo_desktop_model::width::{
    WIDTH_FLOOR_PX, WIDTH_MAX_PERMILLE, WIDTH_MIN_PERMILLE, WIDTH_NARROW_PERMILLE,
    WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE, WindowWidth, max_group_px, min_group_px,
    permille_for_px,
};

/// Only reached from a bare `ui` host with no output spans; the shell
/// always pushes real geometry first.
pub const DEFAULT_STRIP_USABLE_PX: f64 = 1280.0;

pub const RESIZE_HANDLE_PX: f64 = 6.0;

/// How long a window's place is held, with nothing drawn in it, for its
/// first frame: a window whose content arrives inside it appears whole,
/// with no fade (its slot opening is the motion); one that arrives later
/// cross-fades in over the ground it held.
const ARRIVAL_GRACE: Duration = Duration::from_millis(150);
/// The lifted handle remains readable while exposing the drop target; the app
/// pixels beneath it recede further so the target never disappears.
const DRAG_CONTENT_ALPHA: f32 = 0.42;
const DRAG_SOURCE_CONTENT_ALPHA: f32 = 0.18;
/// End scale of the fast lift curve. The content tucks under the full-size tab
/// handle instead of turning into a second opaque window under the pointer.
const DRAG_CONTENT_SCALE_END: f64 = 0.96;

/// How far a window's arrival or departure has come `elapsed` after it
/// began (`motion::WindowMotion::OPEN`).
fn window_progress(direction: crate::motion::Direction, elapsed: Duration) -> f64 {
    crate::motion::WindowMotion::OPEN.progress_after(
        direction,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
        elapsed,
    )
}

/// How far a window's content has cross-faded in over the ground its place
/// held, `elapsed` after it arrived past [`ARRIVAL_GRACE`]
/// (`motion::CROSS_FADE`).
fn arrival_progress(elapsed: Duration) -> f64 {
    crate::motion::CROSS_FADE.progress_after(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
        elapsed,
    )
}

/// How long a closing window takes to leave.
fn window_close_time() -> Duration {
    crate::motion::WindowMotion::OPEN.settle_time(
        crate::motion::Direction::Exit,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
    )
}

/// How long a closed tab's tile takes to leave: as a closed pill does.
fn group_close_fade() -> Duration {
    close_fade()
}

/// One channel of a window's motion on the strip (its rect while it
/// maximizes or restores, its slot's width while it opens, closes or is
/// resized by command), on the window move (`motion::WindowMotion::MOVE`:
/// the heavy, critically damped spring). Sampled once per drawn frame, so
/// every rebuild of one frame reads the same value. Closed form: a stalled
/// frame lands where the motion is, and a new target mid-flight keeps the
/// value and its velocity, so Super+M twice is one motion, not a queue.
#[derive(Debug)]
struct FrameSpring<T: crate::motion::Animatable> {
    motion: crate::motion::MotionValue<T>,
    drawn: T,
    at: Instant,
}

impl<T: crate::motion::Animatable> Drop for FrameSpring<T> {
    /// A spring let go mid-way (a shortcut landing what it changed, a
    /// close completing) is drawn no more: it asks for no more frames.
    fn drop(&mut self) {
        let target = self.motion.target();
        self.motion.snap(target);
    }
}

impl FrameSpring<f64> {
    /// A width in pixels arrives within a quarter of one.
    fn with_tolerance_px(mut self) -> Self {
        self.motion = self.motion.with_tolerance(0.25);
        self
    }
}

impl<T: crate::motion::Animatable> FrameSpring<T> {
    fn new(value: T) -> Self {
        Self {
            motion: crate::motion::MotionValue::new(value),
            drawn: value,
            at: crate::time::now(),
        }
    }

    /// Go to `target` from where the motion is now, at the speed it has.
    fn set_target(&mut self, target: T) {
        let timing = crate::motion::WindowMotion::MOVE.timing(
            crate::motion::Direction::Enter,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
        );
        self.motion.set(target, timing, crate::motion::frame_now());
    }

    fn snap(&mut self, value: T) {
        self.motion.snap(value);
        self.drawn = value;
    }

    fn step(&mut self, now: Instant) {
        self.drawn = self.motion.get(now);
        self.at = now;
    }

    /// The value as of the last frame sampled.
    fn value(&self) -> T {
        self.drawn
    }

    fn target(&self) -> T {
        self.motion.target()
    }

    #[cfg(test)]
    fn velocity(&self) -> T {
        self.motion.velocity(self.at)
    }

    /// Whether the last frame sampled is the motion's rest.
    fn is_settled(&self) -> bool {
        !self.motion.is_moving(self.at)
    }
}

impl WindowGroup {
    /// Move this group and the chrome laid out inside it. Used while a
    /// retained strip is drawn, so the encoded scene stays in canvas
    /// coordinates and a pan does not rewrite it. The caller puts the
    /// rects back before hit testing.
    pub(crate) fn offset_layout(&mut self, dx: f64) {
        if dx == 0.0 {
            return;
        }
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.rect = self.rect + shift;
        self.restore_rect = self.restore_rect + shift;
        self.fullscreen_rect = self.fullscreen_rect + shift;
        self.close_rect = self.close_rect + shift;
        self.bar.offset_layout(dx);
        self.search.offset_layout(dx);
        for (_, host) in &mut self.hosts {
            host.offset_layout(dx);
        }
    }

    /// Where the group draws this frame: its layout rect, or the spring's
    /// while it moves there.
    pub(crate) fn drawn_rect(&self) -> Rect {
        self.rect_spring
            .as_ref()
            .map_or(self.rect, FrameSpring::value)
    }

    #[cfg(test)]
    pub(crate) fn step_rect_spring_for_tests(&mut self) {
        self.step_rect_spring();
    }

    #[cfg(test)]
    pub(crate) fn rect_spring_width_velocity(&self) -> Option<f32> {
        self.rect_spring
            .as_ref()
            .map(|spring| f32_of_f64(spring.velocity().width()))
    }
}

/// The room the window's search control takes after its tabs: the control
/// and the gap before it.
const SEARCH_CONTROL_ROOM: f64 =
    super::window_controls::CONTROL_PX + super::window_controls::CONTROL_GAP;

const RESTORE_TARGET: &str = "window.restore";
const CLOSE_TARGET: &str = "window.close";
const FULLSCREEN_TARGET: &str = "window.fullscreen";

// Each bool below is an independent UI flag (resize-hover, maximized,
// search-open, focus, ...), not a bitset encoding related states, and the
// struct is a `pub` field surface read throughout `shell` — folding them
// into a bitflags/enum would be an API-breaking redesign, not a mechanical
// lint fix.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent UI flags on a widely-shared pub struct; not a state bitset"
)]
pub struct WindowGroup {
    external_navigation: bool,
    /// Transient presentation from the parent row; never a persisted maximize.
    fullscreen_presentation: bool,
    /// While fullscreen, the bar the person reached for (`fullscreen_bar`).
    fullscreen_bar: fullscreen_bar::FullscreenBar,
    close_reports: Vec<crate::application::ComponentCloseReport>,
    pub id: u64,
    bar: TabBar,
    hosts: Vec<(u64, TileHost)>,
    width: WindowWidth,
    /// Pushed down by the parent [`crate::widget::GroupRow`] before every
    /// `build_layout`; seeded to [`DEFAULT_STRIP_USABLE_PX`].
    strip_usable_px: f64,
    /// No other window shares this one's run of the strip, so a default
    /// width takes the strip ([`WindowWidth::resolve_in_strip`]). Pushed
    /// down by the parent row with [`Self::strip_usable_px`].
    alone: bool,
    /// The width this window had before it was narrowed to share the
    /// screen with a window that opened beside it
    /// ([`Self::share_width`]): alone again, it goes back to it. Any other
    /// change of width (the person's resize, a tier) forgets it.
    width_before_share: Option<WindowWidth>,
    rect: Rect,
    /// An edge drag the parent row is carrying out on this window
    /// ([`super::edge_resize`]).
    edge_drag: Option<GroupEdgeDrag>,
    /// The edge of this window a press would resize, shown before anything
    /// is pressed. The parent row decides it ([`super::edge_resize::seam_owner`]).
    edge_highlight: Option<WindowEdge>,
    /// The edge highlight changed since the last frame, which shows, moves
    /// or hides the grip.
    grip_dirty: bool,
    pub maximized: bool,
    pub search_active: bool,
    /// The active group of the active strip, set by the shell each frame.
    /// Consumed by the active [`TileHost`], which paints the signal on
    /// the window's own edge/shadow rather than the group drawing a ring
    /// around both the tab row and the card.
    pub focused: bool,
    maximized_controls: crate::widget::PaintedTargets,
    registered_controls: Option<(Rect, Rect, Rect)>,
    restore_rect: Rect,
    restore_hover: bool,
    /// The `[Full screen]` chip beside Restore, and its pending request.
    fullscreen_rect: Rect,
    fullscreen_hover: bool,
    pending_fullscreen_request: bool,
    close_rect: Rect,
    close_hover: bool,
    /// The painted `[app title]` chip of the floating bar; zero when hidden
    /// or when the app's header row holds the bar.
    title_rect: Rect,
    /// Whether the person reached for a maximized window's bar
    /// (`widget::top_edge_reveal`): the bar hides at rest and appears over
    /// the window's top edge.
    top_edge: crate::widget::TopEdgeReveal,
    /// The active host's header row took the window's bar: the host the
    /// offer went to, and the slot its row reported after layout.
    bar_offered_to: Option<u64>,
    bar_slot: Option<crate::widget::WindowBarSlot>,
    /// The motion the bar's fade plays, latched from the theme at draw.
    chrome_motion: crate::tokens::EffectiveMotion,
    /// Height of what overlays the window's top edge now (the desktop's top
    /// bar shown over a window under an auto-hiding top bar): the window's
    /// bar stands below it.
    top_overlay_inset: f64,
    /// A maximized window moves down while the desktop's bar shows over its
    /// top edge, so its own bar is never under the desktop's: where the
    /// slide started, and when. It moves, it is not resized, so the app is
    /// not laid out again for a bar that comes and goes.
    top_slide_from: f64,
    top_slide_since: Option<Instant>,
    /// Nothing of the desktop's stands beside a maximized window on its
    /// display (the top bar is not docked above it): maximized, it fills the
    /// display and draws edge to edge ([`Widget::set_full_bleed`]).
    fills_display: bool,
    last_cursor: Point,
    /// When the window was asked for: its slot opens from here, and its
    /// content, held back until it first draws, shows whole if that is
    /// within [`ARRIVAL_GRACE`].
    spawned_at: Option<Instant>,
    /// Content that first drew past the grace cross-fades in from here.
    arrival_fade_at: Option<Instant>,
    /// A tab just opened in this window that has not drawn yet, and the tab
    /// shown before it: the window keeps showing that one meanwhile, so its
    /// body is never blank.
    tab_waiting: Option<(u64, u64)>,
    /// The hand-over from the tab shown before to a new one, once the new
    /// one has drawn (`motion::CrossFade`).
    tab_fade: Option<crate::motion::CrossFade<u64>>,
    /// Drawn rect in flight between the tiled slot and the work area.
    /// Layout jumps to the destination; paint interpolates (no Taffy).
    rect_spring: Option<FrameSpring<Rect>>,
    /// Slot width in flight. Layout reads this so an open grows from 0,
    /// a close collapses the slot, and a resize retargets without a jump.
    width_spring: Option<FrameSpring<f64>>,
    /// The width the window's bar and content are laid out at while its slot
    /// width is in flight: the wide end of the motion. A widening window
    /// reflows to the width it is going to as the motion starts, its app's
    /// new frame arriving under the growing slot's clip; a narrowing one
    /// keeps the width it had, clipped by the slot, and reflows once it
    /// rests. Never on every frame. Read only while `width_spring` is set.
    content_px: f64,
    /// The width the window had when it began to restore from maximized:
    /// what it stays laid out at, clipped by its shrinking frame, until the
    /// frame rests. Read only while `rect_spring` is set or
    /// `rest_layout_owed`, and the window is not maximized.
    rect_from_width: f64,
    /// A restoring frame came to rest and the window is owed the layout at
    /// its own width: asks for the frame that lays it out.
    rest_layout_owed: bool,
    /// Set on any `draw` this group sat out because nothing in it had
    /// presented yet; drained on the first `draw` that does paint, which
    /// latches the arrival fade to the moment the window turns up rather
    /// than the moment its tab was created.
    held_back: bool,
    /// Tab-pill close fades, keyed by close-start `Instant` — the same
    /// stamp the bar snapshots into `closing_tabs` so both stay in
    /// lockstep. A side table (not a `TileHost` field) to keep
    /// `TileHost`'s shape stable. Swept once `elapsed > CLOSE_FADE_MS`.
    closing_hosts: HashMap<u64, Instant>,
    style_dirty: bool,
    /// The tab id the current taffy tile-node was built for.
    /// `build_layout` only builds the *active* tab's host, so on an
    /// active-tab switch the cached node still belongs to the old one —
    /// without a rebuild the new tab inherits a childless leaf and
    /// renders at a stale top-left rect. `take_structure_dirty` diffs
    /// this against the live active id to force that rebuild.
    built_active: Option<u64>,
    pending_maximize_change: Option<bool>,
    /// Per-mille, not px — a peer's output may be a different size.
    pending_width_change: Option<WindowWidth>,
    /// A search the window hands to the platform search
    /// ([`Self::take_pending_search_open`]).
    pending_search_open: Option<super::SearchHandoff>,
    /// The search control after the window's place, expanding into a field
    /// in place ([`super::WindowSearch`]), and the tab whose app it is on.
    search: super::WindowSearch,
    search_tab: Option<u64>,
    /// What the apps of tabs that run in processes of their own offer the
    /// bar's search and History control ([`Self::set_tab_window_offer`]).
    window_offers: Vec<(u64, crate::window_toolbar::WindowOffer)>,
    /// What the person did with the bar's search and History control on
    /// those tabs, for their apps ([`Self::take_toolbar_events`]).
    window_events: Vec<(u64, crate::window_toolbar::ToolbarEvent)>,
    pending_new_tab: Option<u64>,
    pending_tab_reorder: Option<(usize, usize)>,
    /// Close-button requests still awaiting the shell's durable workspace
    /// and client-lifecycle transaction. The widget owns the fade; the
    /// shell owns whether a window remains open.
    pending_close_commit: Vec<u64>,
    /// `None` while alive; `Some` when the last tab starts closing.
    /// Its existing hosts fade within the group transition. `GroupRow` prunes the group
    /// once `close_complete()` is true.
    closing_at: Option<Instant>,
    /// A close was asked of the owning client and not yet honoured: if the
    /// group vanishes from the canonical tree, that exit is its one retirement.
    close_requested: bool,
    close_duration: Duration,
    /// The close was a shortcut's or an action's: it lands, so the group is
    /// gone on the next frame instead of fading out.
    close_lands: bool,
    /// Per drawn frame the width spring steps once and a closing group asks
    /// for one rebuild (its spring steps in `build_layout`). A frame's
    /// composition takes several passes and real time moves between them:
    /// stepping or asking on every pass kept the frame from ever settling.
    width_stepped: bool,
    /// A rebuild found the frame token spent and left the spring where it
    /// was: the next drawn frame must step it.
    width_step_skipped: bool,
    close_rebuild_taken: bool,
    /// Last motion policy observed from the active theme. Widget scheduling
    /// has no `Theme` parameter, so draw latches it here and the next
    /// `needs_animation_frame` query can avoid waking for transitions that
    /// must snap to their endpoint.
    reduce_motion: bool,
    /// What started the transition the strip is laying out; a shortcut or
    /// action lands at once. Set by the row, see
    /// [`GroupRow::set_motion_source`](super::GroupRow::set_motion_source).
    motion_source: crate::tokens::MotionSource,
}

/// Height of the floating maximized-chrome chips. Off the 4 px grid, and
/// short of HIG §9.6's 28 px Selector: the row overlays app content, so it
/// is kept as slim as its `scale.xs` label allows. Recorded rather than
/// rounded — the row's geometry is what the maximize tests pin.
const MAXIMIZED_CHIP_H: f64 = 22.0;

fn maximized_chip_height(theme: &Theme) -> f64 {
    let caption = theme.text(TextRole::Caption);
    MAXIMIZED_CHIP_H
        .max(f64::from(caption.size * caption.line_height) + f64::from(theme.tokens.spacing.s1))
}

/// Height of the floating bar a maximized window shows over its top edge
/// when the person reaches for it: its chips and their inset. The bar takes
/// no room from the client, which fills the window.
pub fn maximized_chrome_height(theme: &Theme) -> f64 {
    (maximized_chip_height(theme) + 2.0 * crate::widget::tabs::TAB_PILL_INSET_Y).ceil()
}

/// Room the window's controls (Full screen, Restore, close) take at the
/// trailing end of an app's header row that holds the window's bar: three
/// of the row's icon buttons, a row gap apart.
pub fn merged_controls_width(theme: &Theme) -> f64 {
    let m = crate::chrome::HeaderRowMetrics::resolve(theme);
    3.0 * m.chip + 2.0 * m.gap
}
/// The per-group right-edge resize grip: two bars, each this wide, filling
/// the `RESIZE_HANDLE_PX` band between them.
const GRIP_BAR_PX: f64 = 2.0;
/// The grip runs a quarter of the group's height, clamped to
/// `spacing.s6 ..= spacing.s16 + spacing.s4`.
const GRIP_HEIGHT_SHARE: f64 = 0.25;

impl WindowGroup {
    /// Paint a whole group while its window handle is being reordered. The
    /// handle remains half-transparent and the app pixels recede further,
    /// matching the tab-drag visual without duplicating layout state.
    pub(crate) fn draw_group_drag_visual(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        crate::surface::shadow(
            scene,
            transform,
            self.rect,
            f64::from(theme.tokens.radii.tile),
            &theme.tokens.elevation.floating,
        );
        if !self.maximized {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                TabBar::GHOST_PILL_ALPHA,
                transform,
                &self.bar_rect().inflate(32.0, 32.0),
            );
            self.bar.draw(scene, transform, tr, theme, presence);
            scene.pop_layer();
        }
        if let Some((_, host)) = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == self.bar.active_id())
        {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                DRAG_CONTENT_ALPHA,
                transform,
                &host.get_rect().inflate(160.0, 160.0),
            );
            let was_focused = host.focused;
            host.set_focused(false);
            host.draw(scene, transform, tr, theme, presence);
            host.set_focused(was_focused);
            scene.pop_layer();
        }
    }

    pub fn new(
        id: u64,
        tabs_with_content: Vec<(TabDescriptor, TileContent)>,
        active_id: u64,
        width: WindowWidth,
    ) -> Self {
        Self::from_hosts(
            id,
            tabs_with_content
                .into_iter()
                .map(|(d, c)| (d, TileHost::new(c)))
                .collect(),
            active_id,
            width,
        )
    }

    /// Mount prepared hosts without unwrapping checked component custody.
    pub fn from_hosts(
        id: u64,
        tabs: Vec<(TabDescriptor, TileHost)>,
        active_id: u64,
        width: WindowWidth,
    ) -> Self {
        let descs = tabs.iter().map(|(d, _)| d.clone()).collect();
        let hosts = tabs
            .into_iter()
            .map(|(d, h)| {
                h.set_panic_scope(d.id);
                (d.id, h)
            })
            .collect();
        let mut bar = TabBar::new(&format!("g{id}"), descs, active_id);
        bar.set_drag_overlay_owned_by_parent(true);
        bar.set_window_controls(true);
        bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
        Self {
            id,
            close_reports: vec![],
            bar,
            hosts,
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            alone: false,
            width_before_share: None,
            rect: Rect::ZERO,
            external_navigation: false,
            fullscreen_presentation: false,
            fullscreen_bar: fullscreen_bar::FullscreenBar::default(),
            edge_drag: None,
            edge_highlight: None,
            grip_dirty: false,
            maximized: false,
            search_active: false,
            focused: false,
            maximized_controls: crate::widget::PaintedTargets::new(),
            registered_controls: None,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            fullscreen_rect: Rect::ZERO,
            fullscreen_hover: false,
            pending_fullscreen_request: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            title_rect: Rect::ZERO,
            top_edge: crate::widget::TopEdgeReveal::default(),
            bar_offered_to: None,
            bar_slot: None,
            chrome_motion: crate::tokens::EffectiveMotion::default(),
            top_overlay_inset: 0.0,
            top_slide_from: 0.0,
            top_slide_since: None,
            fills_display: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            arrival_fade_at: None,
            tab_waiting: None,
            tab_fade: None,
            rect_spring: None,
            width_spring: None,
            content_px: 0.0,
            rect_from_width: 0.0,
            rest_layout_owed: false,
            held_back: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            close_requested: false,
            width_stepped: false,
            width_step_skipped: false,
            close_rebuild_taken: false,
            close_duration: window_close_time(),
            close_lands: false,
            reduce_motion: false,
            motion_source: crate::tokens::MotionSource::default(),
            style_dirty: false,
            built_active: Some(active_id),
            pending_maximize_change: None,
            pending_width_change: None,
            pending_tab_reorder: None,
            pending_search_open: None,
            search: super::WindowSearch::new(),
            search_tab: None,
            window_offers: Vec::new(),
            window_events: Vec::new(),
            pending_new_tab: None,
            pending_close_commit: Vec::new(),
        }
    }
    pub fn width(&self) -> WindowWidth {
        self.width
    }

    fn navigation_bar_visible(&self) -> bool {
        !self.external_navigation && (!self.fullscreen_presentation || self.fullscreen_bar_drawn())
    }

    pub(crate) fn set_fullscreen_presentation(&mut self, fullscreen: bool) {
        if self.fullscreen_presentation != fullscreen {
            self.fullscreen_presentation = fullscreen;
            self.style_dirty = true;
            if !fullscreen {
                self.fullscreen_bar.revealed = false;
                self.fullscreen_bar.fade.snap(false);
            }
        }
    }

    pub fn set_external_navigation(&mut self, external: bool) {
        if self.external_navigation != external {
            self.external_navigation = external;
            self.style_dirty = true;
        }
    }
    #[must_use]
    pub fn with_maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    pub fn set_focused(&mut self, focused: bool) -> bool {
        if !focused {
            self.maximized_controls.cancel_pointer_press();
        }
        if self.focused == focused {
            return false;
        }
        self.focused = focused;
        self.sync_host_keyboard();
        true
    }

    /// Keys and IME go to the tile of the focused window ([`TileHost`]
    /// routes them by its own flag), so the flag follows the window's focus
    /// when it changes, not when the window is next drawn: a window whose
    /// drawing a pan replays, or one partly off screen, still takes what is
    /// typed the moment a click focuses it.
    fn sync_host_keyboard(&mut self) {
        let active = self.bar.active_id();
        let focused = self.focused || self.search_active;
        for (id, host) in &mut self.hosts {
            host.set_focused(focused && *id == active);
        }
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(crate::time::now());
        self.arm_width_from_zero();
    }

    /// Called by every paint: on the first one after the window's place was
    /// held back (nothing of it had drawn), decide how its content arrives.
    /// Inside [`ARRIVAL_GRACE`] of being asked for it shows whole; later, it
    /// cross-fades in. The slot's width spring is left as it is: it has been
    /// opening since the window was asked for.
    fn take_first_paint(&mut self) {
        let now = crate::time::now();
        if std::mem::take(&mut self.held_back) && !self.is_closing() {
            let late = self
                .spawned_at
                .is_none_or(|asked| now.saturating_duration_since(asked) >= ARRIVAL_GRACE);
            self.arrival_fade_at = late.then_some(now);
        }
        if self
            .arrival_fade_at
            .is_some_and(|t| arrival_progress(now.saturating_duration_since(t)) >= 1.0)
        {
            self.arrival_fade_at = None;
        }
    }

    /// A shortcut or action lands what it changed at once: the springs it
    /// started (width, maximize) jump to their targets, a spawn shows the
    /// group already in place, and the tab bar's chips settle.
    pub(crate) fn set_motion_source(&mut self, source: crate::tokens::MotionSource) {
        if self.motion_source == source {
            return;
        }
        self.motion_source = source;
        self.bar.set_motion_source(source);
        if source.lands_at_once() {
            if let Some(spring) = self.width_spring.as_mut() {
                spring.snap(spring.target());
                self.style_dirty = true;
            }
            if self.rect_spring.take().is_some() {
                self.style_dirty = true;
            }
            if !self.is_closing() {
                self.spawned_at = None;
                self.arrival_fade_at = None;
            }
        }
    }

    /// Whether a transition starting now jumps to its end.
    fn lands_at_once(&self) -> bool {
        self.reduce_motion || self.motion_source.lands_at_once()
    }

    pub(crate) fn set_reduce_motion(&mut self, on: bool) {
        self.reduce_motion = on;
        if on && let Some(spring) = self.width_spring.as_mut() {
            spring.snap(spring.target());
        }
    }

    /// Width the strip must reserve this frame — the spring value while
    /// a transition is in flight, otherwise the persisted fraction.
    pub fn layout_width_px(&self) -> f64 {
        if let Some(drag) = self.edge_drag {
            return drag.live_px;
        }
        if let Some(spring) = &self.width_spring {
            return spring.value().max(0.0);
        }
        if self.is_closing() {
            return 0.0;
        }
        self.resolved_width_px()
    }

    /// Width the slot is going to, in the same pixels as
    /// [`Self::layout_width_px`]. A later usable-basis change does not
    /// rewrite an in-flight target, so overflow can use this instead of
    /// the current fraction.
    pub fn dest_width_px(&self) -> f64 {
        if let Some(spring) = &self.width_spring {
            return spring.target().max(0.0);
        }
        self.layout_width_px()
    }

    /// True when the spring is still aimed at a previous usable basis.
    pub(crate) fn width_target_is_stale(&self) -> bool {
        self.width_spring.is_some() && (self.dest_width_px() - self.resolved_width_px()).abs() > 1.0
    }

    fn arm_width_from_zero(&mut self) {
        let target = self.resolved_width_px();
        if self.lands_at_once() {
            self.width_spring = None;
            return;
        }
        let mut spring = FrameSpring::new(0.0).with_tolerance_px();
        spring.set_target(target);
        self.width_spring = Some(spring);
        self.content_px = target;
        self.style_dirty = true;
    }

    fn retarget_width(&mut self, from: f64, to: f64) {
        let to = to.max(0.0);
        if self.lands_at_once() {
            self.width_spring = None;
            return;
        }
        if let Some(spring) = self.width_spring.as_mut() {
            let now = spring.value();
            self.content_px = now.max(to);
            spring.set_target(to);
            if to > now {
                self.hold_app_frames_at_leading_edge();
            }
            return;
        }
        let from = from.max(0.0);
        if (from - to).abs() < 0.5 {
            return;
        }
        let mut spring = FrameSpring::new(from).with_tolerance_px();
        // The motion starts when the resize was asked for, so its first
        // frame covers the time since.
        spring.set_target(to);
        self.width_spring = Some(spring);
        self.content_px = from.max(to);
        if to > from {
            self.hold_app_frames_at_leading_edge();
        }
        self.style_dirty = true;
    }

    /// The width the bar and content are laid out at while the window's
    /// frame is in flight, when it is not the frame's own: a restoring
    /// window keeps the width it had until its frame rests, and a slot in
    /// flight lays out at the wide end of its motion.
    fn content_width_in_flight(&self) -> Option<f64> {
        if self.edge_drag.is_some() {
            return None;
        }
        // Until the layout at its own width has been made, a restoring
        // window stays as it was laid out while its frame shrank.
        if self.rect_spring.is_some() || self.rest_layout_owed {
            return (!self.maximized && self.rect_from_width > 1.0).then_some(self.rect_from_width);
        }
        if self.maximized {
            return None;
        }
        let slot = self.width_spring.as_ref()?.value().max(0.0);
        ((self.content_px - slot).abs() > 0.5).then_some(self.content_px)
    }

    /// Advance the slot-width spring. Called from the row's layout pass
    /// so neighbours see the new width in the same frame.
    pub(crate) fn step_width_spring(&mut self) {
        if self.close_complete() {
            self.width_spring = None;
            return;
        }
        let Some(spring) = self.width_spring.as_mut() else {
            return;
        };
        if self.reduce_motion {
            spring.snap(spring.target());
            if !self.is_closing() {
                self.width_spring = None;
            }
            self.style_dirty = true;
            return;
        }
        // One step per drawn frame: composition settle rebuilds the same
        // frame up to eight times and must not walk the spring.
        if std::mem::replace(&mut self.width_stepped, true) {
            self.width_step_skipped = true;
            return;
        }
        // The frame's time: after a stall the width lands where the motion
        // is, rather than crawling in slow motion; it is exactly at rest
        // once settled, so no sub-pixel remainder keeps asking for frames.
        spring.step(crate::motion::frame_now());
        self.style_dirty = true;
        if spring.is_settled() && !self.is_closing() {
            self.width_spring = None;
        }
    }

    pub fn start_closing(&mut self) {
        if self.closing_at.is_some() {
            return;
        }
        let from = self.layout_width_px();
        self.closing_at = Some(crate::time::now());
        self.close_lands = self.motion_source.lands_at_once();
        self.close_rebuild_taken = false;
        self.retarget_width(from, 0.0);
    }

    /// Whether a close was requested of the client and abandoned visually
    /// because the client has not exited.
    pub fn close_was_requested(&self) -> bool {
        self.close_requested
    }

    /// A frame was presented by a host that never marks this group drawn
    /// ([`super::GroupRow::frame_presented`]): the next frame may step the
    /// width spring. If a rebuild skipped its step because the token was
    /// still spent (it came after a tick that rebuilt without painting),
    /// nothing has asked for the rebuild that would step it, and the spring
    /// would stay where it stopped while the group kept asking for
    /// animation frames, so ask for one.
    pub fn frame_presented(&mut self) {
        self.next_frame();
    }

    /// A new frame may step the width spring and rebuild a closing group
    /// once more. Called when the group is drawn, and by a row for groups
    /// it culled from a frame, so an off-screen transition still ends. A
    /// step skipped since the last frame (a settle rebuild of the frame
    /// that started the spring) asks for the rebuild that takes it, or the
    /// spring would stay where it stopped.
    pub fn next_frame(&mut self) {
        if self.width_step_skipped
            && !self.reduce_motion
            && self.width_spring.as_ref().is_some_and(|s| !s.is_settled())
        {
            self.style_dirty = true;
        }
        self.width_step_skipped = false;
        self.width_stepped = false;
        self.close_rebuild_taken = false;
    }

    pub fn is_closing(&self) -> bool {
        self.closing_at.is_some()
    }

    /// The close has run its course: the window has faded out and its slot
    /// has closed. Pruned before the slot is shut, the windows beside it
    /// would jump across what was left of it.
    pub fn close_complete(&self) -> bool {
        self.closing_at.is_some_and(|t| {
            self.reduce_motion
                || self.close_lands
                || (crate::time::now().saturating_duration_since(t) >= self.close_duration
                    && self.slot_at_rest())
        })
    }

    /// Whether the slot's width is where it is going, as of this frame.
    fn slot_at_rest(&self) -> bool {
        self.width_spring
            .as_ref()
            .is_none_or(|spring| !spring.motion.is_moving(crate::motion::frame_now()))
    }

    /// When a tab's close starts: now, or, for a shortcut that lands, as long
    /// ago as its fade lasts, so the closed tab is not drawn again.
    fn close_started(&self) -> Instant {
        let now = crate::time::now();
        if self.lands_at_once() {
            now.checked_sub(group_close_fade()).unwrap_or(now)
        } else {
            now
        }
    }

    /// Whether a maximized window's bar shows: the person reached for it
    /// (`widget::top_edge_reveal`). It hides at rest.
    pub fn maximized_chrome_shown(&self) -> bool {
        self.maximized_chrome_live() && self.top_edge.shown()
    }

    fn maximized_chrome_live(&self) -> bool {
        // A maximized window keeps its bar: the same bar, in the same place,
        // as when it isn't.
        false
    }

    /// Whether the shown bar's own surface is at `p`, so a press there is the
    /// bar's and not the app's under it. Only the bar's chips count: between
    /// them the app stays reachable.
    pub fn maximized_chrome_captures(&self, p: Point) -> bool {
        self.maximized_chrome_shown()
            && [
                self.title_rect,
                self.fullscreen_rect,
                self.restore_rect,
                self.close_rect,
            ]
            .iter()
            .any(|r| r.area() > 0.0 && r.contains(p))
    }

    /// What the bar covers and which of its controls a pointer may head for.
    fn edge_rects(&self) -> (Vec<Rect>, Vec<Rect>) {
        let controls: Vec<Rect> = [self.fullscreen_rect, self.restore_rect, self.close_rect]
            .into_iter()
            .filter(|r| r.area() > 0.0)
            .collect();
        let mut bar = controls.clone();
        match self.bar_slot {
            Some(slot) => bar.push(slot.row),
            None if self.title_rect.area() > 0.0 => bar.push(self.title_rect),
            None => {}
        }
        (bar, controls)
    }

    /// Feed the top-edge intent a pointer sample in the group's coordinates.
    /// The group sees its own pointer motion; a host whose client surface
    /// takes the pointer (a foreign window) reports it here too. Returns
    /// whether the bar showed or hid.
    pub fn note_maximized_pointer(&mut self, p: Point) -> bool {
        self.note_maximized_pointer_at(p, crate::time::now())
    }

    /// [`Self::note_maximized_pointer`] on a given timeline (tests drive
    /// slow and fast paths with it).
    pub(crate) fn note_maximized_pointer_at(&mut self, p: Point, now: Instant) -> bool {
        if !self.maximized_chrome_live() {
            return false;
        }
        let (bar, controls) = self.edge_rects();
        let geo = crate::widget::EdgeGeometry {
            window: self.rect,
            bar: &bar,
            controls: &controls,
        };
        let changed = self.top_edge.on_motion(p, &geo, now);
        if changed {
            self.top_edge_changed();
        }
        changed
    }

    /// A press at `p`: in the reveal band while the bar hides it is the
    /// app's, and no reveal follows until the pointer leaves the band.
    pub fn note_maximized_press(&mut self, p: Point) {
        if !self.maximized_chrome_live() {
            return;
        }
        let (bar, controls) = self.edge_rects();
        let geo = crate::widget::EdgeGeometry {
            window: self.rect,
            bar: &bar,
            controls: &controls,
        };
        self.top_edge.on_press(p, &geo);
    }

    /// Tell the group how much of its top edge something overlays now — the
    /// desktop's top bar while it shows over a window that reaches under an
    /// auto-hiding top bar — so the window's bar stands below it instead of
    /// under it. Zero while the desktop's bar stands above the window.
    /// Whether a maximized window here fills its display: nothing of the
    /// desktop's (a docked top bar) stands beside it.
    pub fn set_fills_display(&mut self, fills: bool) {
        self.fills_display = fills;
    }

    pub fn set_top_overlay_inset(&mut self, inset: f64) {
        let inset = inset.max(0.0);
        if (self.top_overlay_inset - inset).abs() > f64::EPSILON {
            self.top_slide_from = self.top_slide_now();
            self.top_slide_since = Some(crate::time::now());
            self.top_overlay_inset = inset;
            self.style_dirty = true;
            self.grip_dirty = true;
            if self.bar_offered_to.is_some() {
                self.style_dirty = true;
            }
            self.clear_maximized_controls();
        }
    }

    /// How far a maximized window stands moved down now: on its way to
    /// [`Self::top_overlay_inset`] on the panel slide.
    fn top_slide_now(&self) -> f64 {
        let to = self.top_overlay_inset;
        let Some(since) = self.top_slide_since else {
            return to;
        };
        if self.reduce_motion {
            return to;
        }
        let direction = if to >= self.top_slide_from {
            crate::motion::Direction::Enter
        } else {
            crate::motion::Direction::Exit
        };
        let spec = crate::motion::PanelSlide::SPEC;
        let motion = crate::tokens::EffectiveMotion::default();
        let source = crate::tokens::MotionSource::Pointer;
        let elapsed = crate::time::now().saturating_duration_since(since);
        if elapsed >= spec.settle_time(direction, motion, source) {
            return to;
        }
        let p = spec
            .progress_after(direction, motion, source, elapsed)
            .clamp(0.0, 1.0);
        self.top_slide_from + (to - self.top_slide_from) * p
    }

    fn top_slide_in_flight(&self) -> bool {
        (self.top_slide_now() - self.top_overlay_inset).abs() > 0.25
    }

    /// Test and shot fixture: as if the person had reached for the bar
    /// (`true`) or left it (`false`).
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn set_maximized_chrome_shown_for_tests(&mut self, shown: bool) {
        self.top_edge.force_shown(shown);
        self.top_edge_changed();
    }

    /// Test and shot clock control: settle the bar's pending dwell or hold
    /// as if it had run out. Returns whether the bar shows.
    #[cfg(any(test, debug_assertions))]
    pub fn elapse_maximized_chrome_intent(&mut self) -> bool {
        let before = self.top_edge.shown();
        let shown = self.top_edge.elapse();
        if shown != before {
            self.top_edge_changed();
        }
        shown
    }

    /// Offer the active app's header row the window's bar while maximized,
    /// and take it back from a host that no longer should hold it.
    fn offer_window_bar(&mut self, active_id: u64, theme: &Theme) {
        let now = crate::time::now();
        if !self.maximized_chrome_live() {
            self.top_edge.reset();
        } else if self.top_edge.settle(now) {
            self.grip_dirty = true;
        }
        let want = self
            .maximized_chrome_live()
            .then(|| crate::widget::WindowBar {
                controls_width: merged_controls_width(theme),
                revealed: self.top_edge.shown(),
                top_inset: self.top_overlay_inset,
            });
        if let Some(previous) = self.bar_offered_to
            && (want.is_none() || previous != active_id)
        {
            if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == previous) {
                h.offer_window_bar(None);
            }
            self.bar_offered_to = None;
            self.bar_slot = None;
        }
        if want.is_some()
            && let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id)
            && h.offer_window_bar(want)
        {
            self.bar_offered_to = Some(active_id);
        } else if want.is_some() {
            self.bar_offered_to = None;
            self.bar_slot = None;
        }
    }

    /// The window presents its tabs' History: each tab's pill carries the
    /// History control while the tab row shows, so no tile draws a band of
    /// its own for it. While the row is hidden (maximized, or navigation
    /// drawn elsewhere) the app's own header row may hold the control.
    fn offer_history_controls(&mut self) {
        let tab_row = self.navigation_bar_visible();
        for (id, host) in &mut self.hosts {
            host.set_window_presents_history(tab_row);
            // An app in a process of its own says what its control shows.
            let control = match self.window_offers.iter().find(|(tab, _)| tab == id) {
                Some((_, offer)) => offer.history.map(super::HistoryControl::from),
                None => host.history_control(),
            };
            self.bar.set_history_control(*id, control);
        }
    }

    /// Run a press on a pill's search or History chip: search is the
    /// host's to open ([`Self::take_pending_search_open`]); History runs on
    /// that tab's History door, as the History shortcut and the palette do.
    /// Returns whether one ran.
    fn drain_chip_presses(&mut self) -> bool {
        let mut pressed = false;
        if self.bar.take_search().is_some() {
            if self.search.is_open() {
                self.close_search(crate::tokens::MotionSource::Pointer);
            } else {
                self.open_search(crate::tokens::MotionSource::Pointer);
            }
            pressed = true;
        }
        let history = self.bar.take_history();
        if let Some((id, _)) = history
            && self.window_offers.iter().any(|(tab, _)| *tab == id)
        {
            // The app runs its History door in its own process.
            self.window_events
                .push((id, crate::window_toolbar::ToolbarEvent::History));
            pressed = true;
        } else if let Some((id, control)) = history
            && let Some((_, host)) = self.hosts.iter_mut().find(|(i, _)| *i == id)
        {
            if let Err(error) = host.invoke_history_control(control) {
                tracing::debug!(%error, "window: History control refused");
            }
            pressed = true;
        }
        pressed
    }

    /// The bar showed or hid: repaint, and rebuild where an app's header
    /// lives in the bar (it stands in the tree only while shown).
    fn top_edge_changed(&mut self) {
        self.grip_dirty = true;
        if !self.top_edge.shown() {
            self.maximized_controls.cancel_pointer_press();
        }
        if self.bar_slot.is_some_and(|slot| slot.hides_with_bar) {
            self.style_dirty = true;
        }
    }

    pub fn strip_usable_px(&self) -> f64 {
        self.strip_usable_px
    }

    /// Called by [`crate::widget::GroupRow::build_layout`] before each
    /// child builds its own node.
    pub fn set_strip_usable_px(&mut self, px: f64) {
        let px = px.max(1.0);
        if (self.strip_usable_px - px).abs() > 0.5 {
            self.strip_usable_px = px;
            self.style_dirty = true;
        }
    }

    /// Whether this window has its run of the strip to itself. Called by
    /// [`crate::widget::GroupRow::build_layout`] with
    /// [`Self::set_strip_usable_px`]; a placed window springs to the width
    /// that follows, as when its width is set.
    pub fn set_alone(&mut self, alone: bool) {
        if self.alone == alone {
            return;
        }
        let visual = self.layout_width_px();
        self.alone = alone;
        self.style_dirty = true;
        if alone && let Some(before) = self.width_before_share.take() {
            self.width = before;
            self.stamp_pending_width_change();
        }
        if self.edge_drag.is_none()
            && !self.is_closing()
            && (self.rect.width() > 1.0 || self.width_spring.is_some())
        {
            self.retarget_width(visual, self.resolved_width_px());
        }
    }

    pub fn resolved_width_px(&self) -> f64 {
        self.width
            .resolve_in_strip(self.strip_usable_px, self.alone)
    }

    /// Show (or, with `None`, hide) the grip on the edge of this window a
    /// press would resize. Set by the parent row from
    /// [`super::edge_resize::seam_owner`]; the window itself never decides.
    pub fn set_edge_highlight(&mut self, edge: Option<WindowEdge>) {
        if self.edge_highlight != edge {
            self.edge_highlight = edge;
            self.grip_dirty = true;
        }
    }

    /// The edge whose grip shows: the one being dragged, else the one a
    /// press would resize.
    pub fn edge_highlight(&self) -> Option<WindowEdge> {
        self.edge_drag
            .map(|drag| drag.moving)
            .or(self.edge_highlight)
    }

    /// The widths this window may be resized to: the strictest minimum and
    /// maximum any of its tabs' apps states ([`crate::widget::AppWidget::min_window_width`]),
    /// never below the strip's floor. Every tab shares the window's width, so
    /// a background tab's minimum binds as much as the shown one's.
    pub fn width_limits(&self) -> WidthLimits {
        let usable = self.strip_usable_px;
        let apps = || {
            self.hosts
                .iter()
                .filter_map(|(_, host)| match host.content() {
                    TileContent::App(app) => Some(app),
                    _ => None,
                })
        };
        let min = apps()
            .filter_map(|app| app.min_window_width())
            .fold(min_group_px(usable), f64::max);
        let max = apps()
            .filter_map(|app| app.max_window_width())
            .fold(max_group_px(usable), f64::min)
            .max(min);
        WidthLimits { min, max }
    }

    /// Start an edge drag on this window that moves its `moving` edge; the
    /// other edge holds still on screen. The width is then set live, in
    /// whole pixels, by [`Self::set_live_width`] until
    /// [`Self::end_edge_drag`]. Apps learn which edge holds, so a client
    /// that has not caught up with a new size yet is drawn against it.
    pub fn begin_edge_drag(&mut self, moving: WindowEdge) {
        let live_px = if self.rect.width() > 1.0 {
            self.rect.width()
        } else {
            self.layout_width_px()
        }
        .round();
        self.width_spring = None;
        self.edge_drag = Some(GroupEdgeDrag {
            moving,
            before: self.width,
            live_px,
        });
        self.edge_highlight = None;
        self.grip_dirty = true;
        self.style_dirty = true;
        self.set_live_resize_anchor(Some(moving.opposite()));
    }

    /// The width an edge drag puts this window at, this frame: whole
    /// pixels, as a client's geometry is, and exactly where the pointer is
    /// rather than on the stored fraction's grid.
    pub fn set_live_width(&mut self, px: f64) {
        let Some(drag) = self.edge_drag.as_mut() else {
            return;
        };
        let px = px.round();
        if (drag.live_px - px).abs() > f64::EPSILON {
            drag.live_px = px;
            self.style_dirty = true;
        }
    }

    /// The width this window had when its edge drag began.
    pub fn edge_drag_origin_px(&self) -> Option<f64> {
        self.edge_drag.map(|drag| drag.live_px)
    }

    /// End an edge drag. Committed, the width is stored as the share of the
    /// strip it landed on and handed to the shell to persist; cancelled (an
    /// Escape, focus loss), the window goes back to the width it had. Returns
    /// the change in laid-out width the commit itself makes: the stored share
    /// resolves to whole pixels on its own grid, so the row can keep the
    /// fixed edge exactly where it was.
    pub fn end_edge_drag(&mut self, commit: bool) -> f64 {
        let Some(drag) = self.edge_drag.take() else {
            return 0.0;
        };
        if commit {
            self.width = WindowWidth::from_px(drag.live_px, self.strip_usable_px);
            self.width_before_share = None;
            self.pending_width_change = Some(self.width);
        } else {
            self.width = drag.before;
        }
        self.style_dirty = true;
        self.grip_dirty = true;
        self.release_live_resize_anchor();
        self.resolved_width_px() - drag.live_px
    }

    /// Tell every tab's app which edge of the window holds still during a
    /// live resize ([`AppWidget::set_live_resize_anchor`]).
    fn set_live_resize_anchor(&mut self, fixed: Option<WindowEdge>) {
        for (_, host) in &mut self.hosts {
            if let Some(app) = host.app_mut() {
                app.set_live_resize_anchor(fixed);
            }
        }
    }

    /// The window widens on its own motion: an app's frame drawn at its last
    /// size stays at the leading edge, at that size, until its frame at the
    /// new size arrives. Never stretched to the frame.
    fn hold_app_frames_at_leading_edge(&mut self) {
        self.set_live_resize_anchor(Some(WindowEdge::Leading));
        self.release_live_resize_anchor();
    }

    /// The drag is over: an app may stop drawing against the fixed edge once
    /// its content has caught up with the window's size.
    fn release_live_resize_anchor(&mut self) {
        for (_, host) in &mut self.hosts {
            if let Some(app) = host.app_mut() {
                app.release_live_resize_anchor();
            }
        }
    }

    /// Narrow (or widen) this window to `w` to share the screen with a
    /// window opened beside it, remembering the width it had so that it
    /// gets it back once it is alone on the strip again.
    pub fn share_width(&mut self, w: WindowWidth) {
        let before = self.width_before_share.unwrap_or(self.width);
        self.set_width(w);
        if self.width != before {
            self.width_before_share = Some(before);
        }
    }

    /// A committed width: springs from where the window is drawn. Edge
    /// drags set widths live instead ([`Self::begin_edge_drag`]).
    pub fn set_width(&mut self, w: WindowWidth) {
        let visual = self.layout_width_px();
        let before = self.width;
        match w {
            WindowWidth::Fraction(p) => {
                self.width = WindowWidth::Fraction(p.clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE));
            }
            other => self.width = other,
        }
        if self.width != before {
            self.width_before_share = None;
            self.style_dirty = true;
            // A group that has never been placed is still being built;
            // snapping keeps constructors and first-layout fixtures from
            // spending a spring settle before they can be measured.
            if self.rect.width() > 1.0 || self.width_spring.is_some() {
                self.retarget_width(visual, self.resolved_width_px());
            }
        }
        // A width set since the last one stamped supersedes it: the host
        // must not later save the older one over it.
        if self
            .pending_width_change
            .is_some_and(|pending| pending != self.width)
        {
            self.pending_width_change = None;
        }
    }

    /// Pin the slot at its persisted width. Used after [`Self::set_width`]
    /// when the dest strip still fits the viewport, so in-flight values
    /// cannot invent overflow.
    pub(crate) fn snap_width_spring(&mut self) {
        if self.width_spring.is_none() {
            return;
        }
        self.width_spring = None;
        self.style_dirty = true;
    }

    pub fn stamp_pending_width_change(&mut self) {
        self.pending_width_change = Some(self.width);
    }
    /// A width change the host has not yet persisted: until it does, its
    /// saved width is the old one and must not be reapplied over it.
    pub fn has_pending_width_change(&self) -> bool {
        self.pending_width_change.is_some()
    }
    /// The width to persist, as the window holds it: a default width stays
    /// the default (so it keeps taking the strip while alone), not the
    /// fraction it resolves to.
    pub fn take_pending_width_change(&mut self) -> Option<WindowWidth> {
        self.pending_width_change.take()
    }

    pub fn take_pending_tab_reorder(&mut self) -> Option<(usize, usize)> {
        self.pending_tab_reorder.take()
    }

    pub fn take_pending_search_open(&mut self) -> Option<super::SearchHandoff> {
        self.pending_search_open.take()
    }

    #[must_use]
    pub fn window_search(&self) -> &super::WindowSearch {
        &self.search
    }

    /// Open the window's search on the active tab: in place where its app
    /// offers a document or a list, else the platform search narrowed to
    /// the app (what the control always did).
    pub fn open_search(&mut self, source: crate::tokens::MotionSource) {
        self.open_search_as(source, crate::window_toolbar::SearchOpening::Find);
    }

    /// [`Self::open_search`] as `opening` asks: for the replace chord, with
    /// the replacement holding the keyboard.
    pub fn open_search_as(
        &mut self,
        source: crate::tokens::MotionSource,
        opening: crate::window_toolbar::SearchOpening,
    ) {
        let active = self.bar.active_id();
        let offer = self.window_offer(active).cloned();
        let offered = offer.as_ref().map(|offer| offer.search.into());
        let Some((_, host)) = self.hosts.iter_mut().find(|(id, _)| *id == active) else {
            return;
        };
        // An app in a process of its own says what it offers; the window
        // looks into an app in its process itself.
        let in_place = offered.unwrap_or_else(|| host.in_place_search());
        if let TileContent::App(app) = host.content() {
            self.search.set_app_name(app.app_title());
        }
        if in_place == super::InPlace::Nothing {
            self.pending_search_open = Some(super::SearchHandoff {
                tab: active,
                reach: super::SearchReach::App,
                query: String::new(),
            });
            return;
        }
        if let Some(offer) = offer {
            self.search.open_remote(in_place, source);
            self.search.set_remote_abilities(offer.find);
        } else {
            self.search.open(in_place, source);
        }
        if opening == crate::window_toolbar::SearchOpening::Replace {
            self.search.focus_replacement();
        }
        self.search_tab = Some(active);
        self.search_active = true;
        self.drive_search();
    }

    /// What the app of tab `id` offers the bar, when it runs in a process of
    /// its own.
    fn window_offer(&self, id: u64) -> Option<&crate::window_toolbar::WindowOffer> {
        self.window_offers
            .iter()
            .find(|(tab, _)| *tab == id)
            .map(|(_, offer)| offer)
    }

    /// What the app of tab `id`, running in a process of its own, offers the
    /// bar's search and History control, or `None` (no such app, or it went
    /// away). Returns whether that changed.
    pub fn set_tab_window_offer(
        &mut self,
        id: u64,
        offer: Option<crate::window_toolbar::WindowOffer>,
    ) -> bool {
        let at = self.window_offers.iter().position(|(tab, _)| *tab == id);
        let history =
            |offer: Option<&crate::window_toolbar::WindowOffer>| offer.map(|offer| offer.history);
        // Only the History control is laid out in the bar; a count is drawn.
        let control_changed = history(offer.as_ref())
            != history(
                at.and_then(|at| self.window_offers.get(at))
                    .map(|(_, offer)| offer),
            );
        match (at, offer.as_ref()) {
            (Some(at), Some(offer)) if self.window_offers[at].1 == *offer => return false,
            (Some(at), Some(offer)) => self.window_offers[at].1 = offer.clone(),
            (None, Some(offer)) => self.window_offers.push((id, offer.clone())),
            (Some(at), None) => {
                self.window_offers.remove(at);
                if self.search_tab == Some(id) && self.search.is_remote() {
                    // The app went away under its search.
                    self.search.detach();
                    self.close_search(crate::tokens::MotionSource::Pointer);
                }
            }
            (None, None) => return false,
        }
        if let Some(offer) = offer
            && self.search_tab == Some(id)
        {
            self.search
                .set_remote_count(offer.count.map(super::FilterCount::from));
            self.search.set_remote_abilities(offer.find);
            if let Some(adopted) = &offer.adopted {
                self.search.set_remote_adopted(adopted);
            }
        }
        if control_changed {
            self.style_dirty = true;
        }
        self.grip_dirty = true;
        true
    }

    /// The app of tab `id`, in a process of its own, asked for the window's
    /// search (its Ctrl+F, or its Ctrl+H for `SearchOpening::Replace`): open
    /// it, or give the open one the keyboard. Only the tab the window shows
    /// searches. Returns whether it did.
    pub fn open_search_for_app(
        &mut self,
        id: u64,
        opening: crate::window_toolbar::SearchOpening,
    ) -> bool {
        if id != self.bar.active_id() || !self.navigation_bar_visible() {
            return false;
        }
        if self.search.is_open() {
            match opening {
                crate::window_toolbar::SearchOpening::Find => self.search.take_keyboard(),
                crate::window_toolbar::SearchOpening::Replace => self.search.focus_replacement(),
            }
            return true;
        }
        self.open_search_as(crate::tokens::MotionSource::Keyboard, opening);
        self.search.is_open() || self.pending_search_open.is_some()
    }

    /// Close the window's search: its app stops highlighting or narrowing.
    pub fn close_search(&mut self, source: crate::tokens::MotionSource) {
        self.search.close(source);
        self.search_active = false;
        self.drive_search();
        self.search_tab = None;
    }

    /// Carry the window's search to its tab's app ([`super::WindowSearch::drive`]).
    fn drive_search(&mut self) {
        if self.search.is_remote() {
            // An app in a process of its own is told, and drives itself.
            if let Some(tab) = self.search_tab {
                let events = self.search.take_remote_events();
                self.window_events
                    .extend(events.into_iter().map(|event| (tab, event)));
            }
            return;
        }
        if !self.search.wants_drive() {
            return;
        }
        let Some(tab) = self.search_tab else {
            return;
        };
        let search = &mut self.search;
        let driven = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == tab)
            .and_then(|(_, host)| host.visit_search_root(&mut |root| search.drive(root)));
        if driven.is_none() {
            // The tab's app is gone: nothing is left to tell.
            self.search.detach();
        }
    }

    /// Carry out what the person asked of the window's search.
    fn service_search(&mut self) -> bool {
        let requests = self.search.take_requests();
        let acted = !requests.is_empty();
        for request in requests {
            match request {
                super::WindowSearchRequest::Open => {
                    self.open_search(crate::tokens::MotionSource::Pointer);
                }
                super::WindowSearchRequest::Close => {
                    self.close_search(crate::motion::input_source());
                }
                super::WindowSearchRequest::Widen(reach) => {
                    // The platform search takes the query on from here.
                    self.pending_search_open = Some(super::SearchHandoff {
                        tab: self.bar.active_id(),
                        reach,
                        query: self.search.query().to_owned(),
                    });
                    self.close_search(crate::motion::input_source());
                }
            }
        }
        self.drive_search();
        acted
    }

    /// The search follows the window's active tab: switching tabs ends it on
    /// the tab it was on.
    fn follow_search_tab(&mut self) {
        if let Some(tab) = self.search_tab
            && tab != self.bar.active_id()
        {
            self.close_search(crate::tokens::MotionSource::Pointer);
        }
    }

    /// Whether `event` is Find (Ctrl+F, ⌘F), or Find and Replace (Ctrl+H,
    /// ⌘⌥F), and this window can search in place (for replacing, a
    /// document): the window opens its search, or gives the open one the
    /// keyboard, instead of the app opening a find of its own.
    fn takes_find(&mut self, event: &WindowEvent, modifiers: winit::event::Modifiers) -> bool {
        let Some(opening) = super::window_search::search_chord(
            event,
            modifiers.state(),
            crate::platform::PlatformConventions::default(),
        ) else {
            return false;
        };
        if !self.focused || !self.navigation_bar_visible() {
            return false;
        }
        let replace = opening == crate::window_toolbar::SearchOpening::Replace;
        if self.search.is_open() {
            if self.search.has_keyboard() {
                // Find again widens, and the replace chord goes to the
                // replacement: the search's own key handling.
                return false;
            }
            if replace {
                self.search.focus_replacement();
            } else {
                self.search.take_keyboard();
            }
            return true;
        }
        let active = self.bar.active_id();
        let in_place = match self.window_offer(active) {
            Some(offer) => offer.search.into(),
            None => self
                .active_host_mut()
                .map_or(super::InPlace::Nothing, TileHost::in_place_search),
        };
        if in_place == super::InPlace::Nothing || (replace && in_place != super::InPlace::Document)
        {
            return false;
        }
        self.open_search_as(crate::tokens::MotionSource::Keyboard, opening);
        true
    }

    /// Route a keyboard or input-method event to the window's search while
    /// its field holds the keyboard, for a host whose own chords and focus
    /// would otherwise see the key first. Whether the search took it; an
    /// event it did not take is the host's to dispatch as usual.
    pub fn route_search_input(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.search.has_keyboard()
            || !matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
        {
            return false;
        }
        let routed = self.search.route(event, ctx);
        if routed {
            self.service_search();
            self.drive_search();
        }
        routed
    }

    /// Place the search control after the tabs, and let it reach up to the
    /// window's own controls.
    fn place_search(&mut self) {
        if !self.navigation_bar_visible() {
            self.search.place(Rect::ZERO, 0.0);
            return;
        }
        let px = super::window_controls::CONTROL_PX;
        if let Some((row, reach)) = self.bar.toolbar_search_slot() {
            // The app put its slots in the bar: the control ends their row,
            // and the open search grows back over the location.
            let x0 = row.x1 + super::window_controls::CONTROL_GAP;
            let y0 = row.center().y - px / 2.0;
            self.search
                .place_trailing(Rect::new(x0, y0, x0 + px, y0 + px), reach.min(x0));
            return;
        }
        let row = self.bar.get_rect();
        let x0 = self.bar.tabs_end() + super::window_controls::CONTROL_GAP;
        let y0 = row.center().y - px / 2.0;
        let limit = self.bar.window_controls().content_limit(row);
        self.search
            .place(Rect::new(x0, y0, x0 + px, y0 + px), limit.max(x0 + px));
    }

    pub fn take_pending_new_tab(&mut self) -> Option<u64> {
        self.pending_new_tab.take()
    }

    /// Drain user-initiated close intents for the shell lifecycle owner.
    /// Programmatic closes do not enter this queue because their caller has
    /// already committed the workspace mutation before starting the fade.
    pub fn take_pending_close_commits(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_close_commit)
    }

    pub fn new_tab_rect(&self) -> Rect {
        self.bar.new_tab_rect()
    }

    /// Whether `point` is on the group's trailing new-tab control.
    ///
    /// The chip can remain visible at a clipped strip edge, where coarse
    /// group bounds are an ambiguous owner for a coordinate-only button
    /// event. GroupRow uses this exact painted target to route the press.
    pub(crate) fn new_tab_hit(&self, point: Point) -> bool {
        let rect = self.new_tab_rect();
        rect.area() > 0.0 && rect.contains(point)
    }

    /// Publish the same host-level request as the tab bar's `+` control when
    /// an ancestor has already resolved an otherwise ambiguous clipped-edge
    /// hit. Mouse button events carry no coordinates, so this avoids asking a
    /// stale child cursor cache to repeat `GroupRow`'s exact hit-test.
    pub(crate) fn request_new_tab_if_hit(&mut self, point: Point) -> bool {
        if !self.new_tab_hit(point) {
            return false;
        }
        self.pending_new_tab = Some(self.id);
        true
    }
    /// Not done inside [`Self::new`]: a group is also built by a bare `ui`
    /// host and by tests with no records at all, where an unbound tile
    /// keeps editing whatever document the parent pass carries.
    pub fn bind_tab_records(
        &mut self,
        store: &crate::record::RecordStore,
        record_for: impl Fn(u64) -> crate::record::RecordCheckout,
    ) {
        for (tab_id, host) in &mut self.hosts {
            host.bind_record(store.clone(), record_for(*tab_id));
        }
    }
    /// Deliberately not [`Widget::opening`], which is false as soon as
    /// one tab has drawn even if a tab behind it is still starting (and
    /// still needs animation frames).
    pub fn any_tile_opening(&self) -> bool {
        // `presented()` is also the latch that retires a newly arrived
        // client's opening state. Evaluate every host before asking which are
        // still opening, just as the group's ordinary presentation path does.
        let _ = Widget::presented(self);
        self.hosts.iter().any(|(_, h)| Widget::opening(h))
    }

    pub fn active_host_mut(&mut self) -> Option<&mut TileHost> {
        let id = self.bar.active_id();
        self.hosts
            .iter_mut()
            .find(|(i, _)| *i == id)
            .map(|(_, h)| h)
    }

    /// Read-only twin of [`Self::active_host_mut`] — avoids a `&mut`
    /// borrow that would conflict with the frame builder's own borrows.
    pub fn active_host(&self) -> Option<&TileHost> {
        let id = self.bar.active_id();
        self.hosts.iter().find(|(i, _)| *i == id).map(|(_, h)| h)
    }

    pub(crate) fn discard_layout_nodes(&mut self) {
        for (_, host) in &mut self.hosts {
            host.discard_layout_nodes();
        }
    }

    /// Complete capture bounds for the active app, including this group's tab
    /// decoration. `None` preserves the normal whole-window capture path.
    pub fn complete_capture_rect(&self) -> Option<Rect> {
        if self.maximized {
            return None;
        }
        self.active_host()
            .and_then(TileHost::complete_content_rect)
            .map(|content| self.rect.union(content))
    }

    /// Paint a complete semantic app canvas with the same shared window card
    /// and tab decoration as the live group, without changing live layout.
    pub fn draw_complete_capture(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        if self.maximized {
            return None;
        }
        let active_id = self.bar.active_id();
        let opening_name = self
            .bar
            .tabs()
            .iter()
            .find(|tab| tab.id == active_id)
            .map(|tab| tab.title.clone());
        let host = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == active_id)
            .map(|(_, host)| host)?;
        let capture_rect = host.complete_content_rect()?;
        self.bar
            .set_group_focused(self.focused || self.search_active);
        self.bar.draw(scene, transform, tr, theme, presence);
        if let Some(name) = opening_name.as_deref() {
            host.set_opening_name(name);
        }
        host.set_focused(self.focused || self.search_active);
        host.draw_complete_content_capture(scene, transform, tr, theme, presence)?;
        Some(self.rect.union(capture_rect))
    }

    /// Cancel the tab bar's gesture while preserving every live tab host.
    pub fn cancel_tab_drag(&mut self) -> bool {
        self.bar.cancel_drag()
    }

    /// Returns `(tab_id, release_point)` if the user released a drag
    /// outside the bar's own rect. The parent widget hit-tests the
    /// release point against sibling bars to find a target.
    pub fn take_tab_detach(&mut self) -> Option<(u64, Point)> {
        self.bar.take_detach()
    }

    /// The person asked, from the maximized chrome, for real fullscreen.
    pub fn take_pending_fullscreen_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_fullscreen_request)
    }

    /// Painted `[Full screen]` chip of a maximized group; zero when hidden.
    pub fn fullscreen_rect(&self) -> Rect {
        self.fullscreen_rect
    }

    pub fn take_pending_maximize_change(&mut self) -> Option<bool> {
        self.pending_maximize_change.take()
    }

    /// Keyboard/accessibility twin of the frame maximize button.
    pub fn toggle_maximized(&mut self) {
        self.begin_rect_spring_from_current();
        self.maximized = !self.maximized;
        self.pending_maximize_change = Some(self.maximized);
        self.style_dirty = true;
    }

    fn begin_rect_spring_from_current(&mut self) {
        if self.lands_at_once() || self.rect.width() <= 1.0 || self.rect.height() <= 1.0 {
            self.rect_spring = None;
            return;
        }
        if let Some(spring) = &self.rect_spring {
            // Already in flight: keep value and velocity. The next
            // apply_layout retargets to the new destination.
            self.rect_from_width = spring.value().width();
            return;
        }
        self.rect_from_width = self.rect.width();
        self.rect_spring = Some(FrameSpring::new(self.rect));
    }

    fn retarget_rect_spring(&mut self) {
        let dest = self.rect;
        let Some(spring) = self.rect_spring.as_ref() else {
            return;
        };
        if dest != spring.target() && dest.width() > spring.value().width() {
            // Widening: the app is given its final size now, and until its
            // frame at that size arrives the last one stays at the frame's
            // origin, at its own size, clipped by the growing frame.
            self.hold_app_frames_at_leading_edge();
        }
        let Some(spring) = self.rect_spring.as_mut() else {
            return;
        };
        if self.reduce_motion {
            spring.snap(dest);
            self.rect_spring = None;
            return;
        }
        spring.set_target(dest);
    }

    /// Sample the maximize/restore draw motion for this frame. It starts
    /// at rest where the group was tiled, so the first animated frame
    /// equals the last tiled one; a stalled frame lands where the motion
    /// is, never stepped slower than the clock.
    fn step_rect_spring(&mut self) {
        let Some(spring) = self.rect_spring.as_mut() else {
            return;
        };
        if self.reduce_motion {
            spring.snap(self.rect);
            self.rect_spring = None;
            return;
        }
        spring.step(crate::motion::frame_now());
        if spring.is_settled() {
            self.rect_spring = None;
            // Laid out at the width it had while it shrank: now at its own.
            if !self.maximized {
                self.style_dirty = true;
                self.rest_layout_owed = true;
            }
        }
    }

    fn rect_draw_affine(&self, base: Affine) -> Affine {
        let Some(spring) = &self.rect_spring else {
            return base;
        };
        let src = self.rect;
        let dst = spring.value();
        // Moved, never scaled: the bar and the app's frame are drawn at the
        // size they were laid out at, with their top-left where the
        // springing rect has it, and clipped to it ([`Self::frame_clip`]). A
        // client's buffer is never drawn stretched or squashed.
        base.pre_translate(vello::kurbo::Vec2::new(dst.x0 - src.x0, dst.y0 - src.y0))
    }

    /// Where the window's frame is this frame, in its moved coordinates:
    /// the springing rect while it maximizes or restores, the slot while
    /// its width is in flight and narrower than its content. `None` at
    /// rest, where nothing needs clipping.
    fn frame_clip(&self) -> Option<Rect> {
        if let Some(spring) = &self.rect_spring {
            let dst = spring.value();
            return Some(Rect::from_origin_size(self.rect.origin(), dst.size()));
        }
        self.content_width_in_flight().map(|_| self.rect)
    }

    /// Not gated by `cfg(any(test, debug_assertions))`: `ui`'s `cfg(test)`
    /// only applies when `ui` itself is under test, so the downstream
    /// shell's `--release` test build (`debug_assertions` off) would lose
    /// this method.
    pub fn publish_maximize_intent_for_tests(&mut self) {
        let active = self.bar.active_id();
        self.bar.pending_max = Some(active);
    }

    pub fn detach_tab(&mut self, tab_id: u64) -> Option<(TabDescriptor, TileHost)> {
        let host_idx = self.hosts.iter().position(|(i, _)| *i == tab_id)?;
        let (_, host) = self.hosts.remove(host_idx);
        let desc = self.bar.tabs().iter().find(|t| t.id == tab_id)?.clone();
        self.bar.remove_tab(tab_id);
        // Drop lingering close-fade state for the moved id, or a stale
        // entry keeps `needs_animation_frame` pumping until it ages out.
        self.closing_hosts.remove(&tab_id);
        self.pending_close_commit.retain(|id| *id != tab_id);
        self.bar.finalize_close(tab_id);
        Some((desc, host))
    }

    /// The sibling path — splitting into a *brand-new* group — plays the
    /// group-level fade instead, in `GroupRow::split_tab_into_new_group`,
    /// so the two never double up.
    ///
    /// The new tab is shown at once; until it has drawn, the window keeps
    /// showing the tab it showed before, and then cross-fades to it.
    pub fn insert_detached_tab(&mut self, idx: usize, desc: TabDescriptor, host: TileHost) {
        let id = desc.id;
        let before = self.bar.active_id();
        let before_shown = self
            .hosts
            .iter()
            .any(|(i, h)| *i == before && Widget::presented(h))
            && !self.closing_hosts.contains_key(&before);
        self.bar.insert_tab(idx, desc);
        let clamped = idx.min(self.hosts.len());
        host.set_panic_scope(id);
        self.hosts.insert(clamped, (id, host));
        if before_shown && before != id && self.bar.active_id() == id {
            self.tab_waiting = Some((before, id));
        }
    }

    /// Advance the hand-over to a newly opened tab: drop it when the tab is
    /// no longer the one shown, and start the cross-fade once it has drawn.
    fn advance_tab_handover(&mut self, theme: &Theme) {
        let active = self.bar.active_id();
        if let Some((from, to)) = self.tab_waiting {
            let from_alive = self.hosts.iter().any(|(i, _)| *i == from);
            if to != active || !from_alive {
                self.tab_waiting = None;
            } else if self
                .hosts
                .iter()
                .any(|(i, h)| *i == to && (Widget::presented(h) || Widget::opening(h)))
            {
                // Drawn, or waited for long enough that its opening card
                // says what is coming.
                let mut fade = crate::motion::CrossFade::new(from);
                // The new tab's content arriving, not a navigation: it
                // cross-fades whatever opened it.
                fade.show(
                    to,
                    theme.motion(),
                    crate::tokens::MotionSource::Pointer,
                    crate::motion::frame_now(),
                );
                self.tab_fade = Some(fade);
                self.tab_waiting = None;
            }
        }
        if self.tab_fade.as_ref().is_some_and(|fade| {
            *fade.shown() != active || !fade.is_moving(crate::motion::frame_now())
        }) {
            self.tab_fade = None;
        }
    }

    /// The tab drawn in place of the shown one while the shown one has not
    /// drawn yet.
    fn tab_standing_in(&self) -> Option<u64> {
        self.tab_waiting
            .filter(|&(_, to)| to == self.bar.active_id())
            .map(|(from, _)| from)
    }

    /// Move an existing tab to a persisted order without producing a
    /// user-gesture one-shot.  The tab bar and content hosts are parallel
    /// vectors, so a document-driven reorder has to move both or the pill
    /// and visible application diverge.
    pub fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.hosts.len() || to >= self.hosts.len() || from == to {
            return false;
        }
        debug_assert_eq!(
            self.bar.tabs().len(),
            self.hosts.len(),
            "WindowGroup parallel-vec invariant violated before a programmatic reorder"
        );
        self.bar.reorder(from, to);
        let host = self.hosts.remove(from);
        self.hosts.insert(to, host);
        true
    }

    pub fn from_detached(id: u64, desc: TabDescriptor, host: TileHost, width: WindowWidth) -> Self {
        let active_id = desc.id;
        let tab_id = desc.id;
        host.set_panic_scope(tab_id);
        let mut bar = TabBar::new(&format!("g{id}"), vec![desc], active_id);
        bar.set_drag_overlay_owned_by_parent(true);
        bar.set_window_controls(true);
        bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
        Self {
            id,
            close_reports: vec![],
            bar,
            hosts: vec![(tab_id, host)],
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            alone: false,
            width_before_share: None,
            rect: Rect::ZERO,
            external_navigation: false,
            fullscreen_presentation: false,
            fullscreen_bar: fullscreen_bar::FullscreenBar::default(),
            edge_drag: None,
            edge_highlight: None,
            grip_dirty: false,
            maximized: false,
            search_active: false,
            focused: false,
            maximized_controls: crate::widget::PaintedTargets::new(),
            registered_controls: None,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            fullscreen_rect: Rect::ZERO,
            fullscreen_hover: false,
            pending_fullscreen_request: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            title_rect: Rect::ZERO,
            top_edge: crate::widget::TopEdgeReveal::default(),
            bar_offered_to: None,
            bar_slot: None,
            chrome_motion: crate::tokens::EffectiveMotion::default(),
            top_overlay_inset: 0.0,
            top_slide_from: 0.0,
            top_slide_since: None,
            fills_display: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            arrival_fade_at: None,
            tab_waiting: None,
            tab_fade: None,
            rect_spring: None,
            width_spring: None,
            content_px: 0.0,
            rect_from_width: 0.0,
            rest_layout_owed: false,
            held_back: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            close_requested: false,
            width_stepped: false,
            width_step_skipped: false,
            close_rebuild_taken: false,
            close_duration: window_close_time(),
            close_lands: false,
            reduce_motion: false,
            motion_source: crate::tokens::MotionSource::default(),
            style_dirty: false,
            built_active: Some(active_id),
            pending_maximize_change: None,
            pending_width_change: None,
            pending_tab_reorder: None,
            pending_search_open: None,
            search: super::WindowSearch::new(),
            search_tab: None,
            window_offers: Vec::new(),
            window_events: Vec::new(),
            pending_new_tab: None,
            pending_close_commit: Vec::new(),
        }
    }

    pub fn tab_count(&self) -> usize {
        self.hosts.len()
    }

    pub fn bar_rect(&self) -> Rect {
        self.bar.bar_rect()
    }

    pub fn tab_rects(&self) -> &[Rect] {
        self.bar.tab_rects()
    }

    /// True inside the bar rect but not on a tab pill and not in the
    /// right-edge resize band. Used by [`GroupRow`](crate::widget::group_row::GroupRow) to arbitrate a press
    /// into a group-reorder drag vs. a tab/resize gesture.
    pub fn bar_grab_hit(&self, p: Point) -> bool {
        if !self.navigation_bar_visible() {
            return false;
        }
        if !self.bar_rect().contains(p) {
            return false;
        }
        if self.resize_grab_hit(p) {
            return false;
        }
        let plus = self.bar.new_tab_rect();
        if plus.area() > 0.0 && plus.contains(p) {
            return false;
        }
        if self.bar.toolbar_hit(p) || self.search.hit(p) {
            return false;
        }
        !self.tab_rects().iter().any(|r| r.contains(p))
    }

    /// Where a finger can grab the whole group: anywhere on its tab bar,
    /// tabs included, except the new-tab button and the resize edge.
    pub fn touch_grab_hit(&self, p: Point) -> bool {
        if !self.navigation_bar_visible() || !self.bar_rect().contains(p) {
            return false;
        }
        if self.resize_grab_hit(p) {
            return false;
        }
        let plus = self.bar.new_tab_rect();
        (plus.area() <= 0.0 || !plus.contains(p)) && !self.bar.toolbar_hit(p) && !self.search.hit(p)
    }

    /// True when `p` is in the group's trailing-edge resize target.
    /// `GroupRow` uses this before arming a whole-group drag so the two
    /// gestures cannot race; this matters especially for a one-group strip,
    /// where this grip is the only resize affordance.
    pub fn resize_grab_hit(&self, p: Point) -> bool {
        self.resize_grab_rects().iter().any(|rect| rect.contains(p))
    }

    /// The group's own pointer chrome (maximized controls, pin shortcut),
    /// ahead of its tab bar and tile. The row offers every group this before
    /// any group's tile sees the event: a tile that swallows pointer events
    /// (one still loading, or whose record is gone) must not leave another
    /// group's chrome stale.
    pub fn handle_edge_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        self.handle_direct_input(event, scale_factor, modifiers)
    }

    /// Both edge zones, leading first: where a press resizes a window rather
    /// than reaching its app. Which window it resizes is the row's to say
    /// ([`super::edge_resize::seam_owner`]). Empty while maximized or
    /// fullscreen.
    pub fn resize_grab_rects(&self) -> Vec<Rect> {
        [self.leading_resize_grab_rect(), self.resize_grab_rect()]
            .into_iter()
            .flatten()
            .collect()
    }

    /// Leading-edge zone, the mirror of [`Self::resize_grab_rect`]: the same
    /// width, inside the window.
    pub fn leading_resize_grab_rect(&self) -> Option<Rect> {
        (!self.maximized && !self.fullscreen_presentation).then(|| {
            Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + RESIZE_HANDLE_PX,
                self.rect.y1,
            )
        })
    }

    /// Painted trailing-edge grip, shared with compositor input admission.
    pub fn resize_grab_rect(&self) -> Option<Rect> {
        (!self.maximized && !self.fullscreen_presentation).then(|| {
            Rect::new(
                self.rect.x1 - RESIZE_HANDLE_PX,
                self.rect.y0,
                self.rect.x1,
                self.rect.y1,
            )
        })
    }

    pub fn drop_index_for(&self, release_point: Point) -> usize {
        self.bar.drop_index_for(release_point)
    }

    /// Update chrome metadata without replacing the document/component owner.
    pub fn update_tab_descriptor(&mut self, descriptor: TabDescriptor) -> bool {
        let Some(tab) = self
            .bar
            .tabs_mut()
            .iter_mut()
            .find(|tab| tab.id == descriptor.id)
        else {
            return false;
        };
        if tab.title == descriptor.title && tab.app_kind == descriptor.app_kind {
            return false;
        }
        *tab = descriptor;
        self.style_dirty = true;
        true
    }
    /// Show the slots tab `id`'s app declared for the window's bar
    /// ([`crate::window_toolbar`]), or none. Returns whether that changed.
    pub fn set_tab_toolbar(
        &mut self,
        id: u64,
        toolbar: Option<crate::window_toolbar::WindowToolbar>,
    ) -> bool {
        let changed = self.bar.set_toolbar(id, toolbar);
        if changed {
            // The bar may change its height, and its row its content.
            self.style_dirty = true;
            self.grip_dirty = true;
        }
        changed
    }

    /// The slots tab `id` shows in the bar.
    #[must_use]
    pub fn tab_toolbar(&self, id: u64) -> Option<&crate::window_toolbar::WindowToolbar> {
        self.bar.toolbar(id)
    }

    /// What the person did with the apps' slots in the bar, by tab.
    pub fn take_toolbar_events(&mut self) -> Vec<(u64, crate::window_toolbar::ToolbarEvent)> {
        let mut events = self.bar.take_toolbar_events();
        events.append(&mut self.window_events);
        events
    }

    /// What the edited location held since the last call, by tab, for the
    /// app's suggestions.
    pub fn take_location_drafts(&mut self) -> Vec<(u64, crate::window_toolbar::ToolbarEvent)> {
        self.bar.take_location_drafts()
    }

    /// Whether the active tab's location is being edited in the bar.
    #[must_use]
    pub fn editing_location(&self) -> bool {
        self.navigation_bar_visible() && self.bar.editing_location()
    }

    /// Hand a keyboard or text event to the location field while it is
    /// edited; the shell routes the keyboard here while
    /// [`Self::editing_location`]. Returns whether the field took it.
    pub fn location_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let took = self.bar.location_event(event, ctx);
        if took {
            self.grip_dirty = true;
        }
        took
    }

    /// What the button of the window's bar at `p` does, for its tooltip.
    #[must_use]
    pub fn bar_tooltip_at(&self, p: Point) -> Option<(Rect, String)> {
        if !self.navigation_bar_visible() {
            return None;
        }
        self.bar.tooltip_target_at(p)
    }

    /// Paint the bar's top layer (its slots' menus, the overflow's and the
    /// suggestions, and the search's options), for a host that draws the
    /// windows' top layer itself.
    pub fn draw_bar_popups(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.navigation_bar_visible() {
            self.bar
                .draw_overlay(scene, transform, text_renderer, theme, presence);
            self.search
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Whether `p` is on the edited location or the suggestions under it:
    /// a press there keeps the edit.
    #[must_use]
    pub fn location_hit(&self, p: Point) -> bool {
        self.navigation_bar_visible() && self.bar.location_hit(p)
    }

    /// End the location's edit, putting it back. Returns whether it was
    /// being edited.
    pub fn cancel_location_edit(&mut self) -> bool {
        let cancelled = self.bar.cancel_location_edit();
        if cancelled {
            self.grip_dirty = true;
        }
        cancelled
    }

    /// Where the active tab's location stands in the bar.
    #[must_use]
    pub fn location_rect(&self) -> Rect {
        self.bar.location_rect()
    }

    /// Turn the active tab's editable location into a field (Ctrl+L).
    pub fn edit_location(&mut self) -> bool {
        let started = self.navigation_bar_visible() && self.bar.edit_location();
        if started {
            self.grip_dirty = true;
        }
        started
    }

    /// Name tab `id` in place: its title becomes a field in its pill
    /// ([`crate::widget::TabBar::begin_rename`]); `name` is what a reader
    /// announces the field as. Returns whether the edit is open.
    pub fn begin_tab_rename(&mut self, id: u64, name: &str) -> bool {
        let started = self.navigation_bar_visible() && self.bar.begin_rename(id, name);
        if started {
            self.grip_dirty = true;
        }
        started
    }

    /// The tab being named in place, if any.
    #[must_use]
    pub fn renaming_tab(&self) -> Option<u64> {
        self.bar.renaming()
    }

    /// Whether `p` is on the name field of a tab being named.
    #[must_use]
    pub fn tab_rename_hit(&self, p: Point) -> bool {
        self.bar.rename_hit(p)
    }

    /// Hand a keyboard or text event to the name field while a tab is being
    /// named; the shell routes the keyboard here while
    /// [`Self::renaming_tab`]. Returns whether the field took it.
    pub fn tab_rename_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let took = self.bar.rename_event(event, ctx);
        self.grip_dirty = true;
        took
    }

    /// Commit the name being edited, as Enter does. Returns whether a tab
    /// was being named.
    pub fn commit_tab_rename(&mut self) -> bool {
        let committed = self.bar.commit_rename();
        if committed {
            self.grip_dirty = true;
        }
        committed
    }

    /// Put the title back, as Escape does. Returns whether a tab was being
    /// named.
    pub fn cancel_tab_rename(&mut self) -> bool {
        let cancelled = self.bar.cancel_rename();
        if cancelled {
            self.grip_dirty = true;
        }
        cancelled
    }

    /// A name committed in place since the last call: the tab, and the name
    /// (empty when the person cleared it).
    pub fn take_tab_rename(&mut self) -> Option<(u64, String)> {
        self.bar.take_rename()
    }

    pub fn tabs(&self) -> &[TabDescriptor] {
        self.bar.tabs()
    }

    /// Mark tab `id` as held by someone else ([`crate::widget::TabHolder`]),
    /// or clear the mark: such a tab offers no close and resists a drag.
    /// `true` when that changed what the window offers.
    pub fn set_tab_holder(&mut self, id: u64, holder: Option<crate::widget::TabHolder>) -> bool {
        self.bar.set_holder(id, holder)
    }

    /// Offer the new-tab control, or not, for someone who opens nothing
    /// here. `true` when that changed what the window offers.
    pub fn set_new_tab(&mut self, on: bool) -> bool {
        self.bar.set_new_tab(on)
    }

    /// Mark tab `id` as a window whose app may control the desktop, or
    /// clear the mark: while it is in front the bar says "Controls your
    /// desktop". `true` when that changed what the window shows.
    pub fn set_tab_desktop_control(&mut self, id: u64, on: bool) -> bool {
        self.bar.set_desktop_control(id, on)
    }

    /// Press the bar's "Controls your desktop" chip without a pointer, as an
    /// assistive technology does. Whether the bar shows it.
    pub fn activate_desktop_control(&mut self) -> bool {
        self.bar.activate_desktop_control()
    }

    /// A press on the bar's "Controls your desktop" chip, once: the tab and
    /// where the chip stands, for the host's menu.
    pub fn take_desktop_control_press(&mut self) -> Option<(u64, vello::kurbo::Rect)> {
        self.bar.take_desktop_control_press()
    }

    /// Whether someone else holds any of the window's tabs, so it does not
    /// move as a whole.
    pub fn is_held(&self) -> bool {
        self.bar.tabs().iter().any(|tab| self.bar.is_held(tab.id))
    }

    /// Replace tab `id`'s artwork. `true` when that changed what it paints.
    pub fn set_tab_icon(&mut self, id: u64, icon: Option<icons::AppImage>) -> bool {
        self.bar.set_icon(id, icon)
    }

    pub fn active_tab_id(&self) -> u64 {
        self.bar.active_id()
    }

    /// Layout bounds and the control targets cached by production paint.
    /// A missing target means it is not currently painted.
    pub fn presentation_bounds(&self) -> WindowGroupBounds {
        let painted = |rect: Rect| (rect.area() > 0.0).then_some(rect);
        // The window's own controls stand at the end of its bar, maximized or
        // not: one control maximizes or, on a maximized window, restores.
        let controls = if self.navigation_bar_visible() {
            *self.bar.window_controls()
        } else {
            crate::widget::window_controls::WindowControls::default()
        };
        let toggle = painted(controls.maximize);
        WindowGroupBounds {
            group: self.rect,
            client: self.active_host().map(Widget::get_rect),
            tab_bar: self.bar.get_rect(),
            maximize: if self.maximized {
                None
            } else {
                toggle.or_else(|| {
                    self.navigation_bar_visible()
                        .then(|| self.active_maximize_rect())
                        .flatten()
                        .and_then(painted)
                })
            },
            restore: if self.maximized {
                toggle.or_else(|| painted(self.restore_rect))
            } else {
                None
            },
            close: painted(controls.close).or_else(|| painted(self.close_rect)),
        }
    }

    /// Painted maximize target for the active tab, if that tab currently has
    /// one. This is immutable presentation geometry used by accessibility and
    /// optional developer diagnostics.
    pub fn active_maximize_rect(&self) -> Option<Rect> {
        let active = self.bar.active_id();
        self.bar
            .tabs()
            .iter()
            .position(|tab| tab.id == active)
            .and_then(|index| self.bar.max_rects_internal().get(index).copied())
    }

    /// Painted restore target for a maximized group. A zero rectangle means
    /// the control is not currently visible.
    pub fn restore_rect(&self) -> Rect {
        self.restore_rect
    }

    /// Drain an explicit user selection from the tab bar. Model-driven
    /// `set_active_tab` calls never create this edge.
    pub fn take_tab_activation(&mut self) -> Option<u64> {
        self.bar.take_activation()
    }

    /// The bar's `active_id` is the authoritative copy (the shell
    /// flushes it into the persisted tree after input), so a
    /// programmatic focus jump must land here, not only in the tree.
    pub fn set_active_tab(&mut self, id: u64) -> bool {
        if !self.bar.tabs().iter().any(|t| t.id == id) {
            return false;
        }
        // An already-selected tab is still a valid address. Callers use the
        // return value to distinguish a live target from a stale window id;
        // `false` must therefore mean "missing", not merely "no repaint".
        if self.bar.active_id() == id {
            return true;
        }
        self.bar.set_active(id);
        true
    }

    /// Exists because close was pointer-only: the `✕` only paints on an
    /// active/hovered/pressed pill, so there was no route for someone
    /// driving by keyboard.
    pub fn request_close_active_tab(&mut self) -> bool {
        let id = self.bar.active_id();
        self.request_close_tab(id)
    }

    pub fn take_tab_context_menu(&mut self) -> Option<(u64, Point)> {
        self.bar.take_context_menu()
    }

    /// Drains the bar's `pending_close` immediately rather than waiting
    /// for the next `handle_event`, since a menu pick arrives outside
    /// pointer dispatch and would otherwise leave the host un-faded
    /// until an unrelated event landed.
    pub fn request_close_tab(&mut self, tab_id: u64) -> bool {
        if !self.bar.start_closing(tab_id) {
            return false;
        }
        if let Some(close_id) = self.bar.take_close() {
            let started = self.close_started();
            self.closing_hosts.entry(close_id).or_insert(started);
        }
        if self.bar.tabs().is_empty() {
            self.start_closing();
        }
        true
    }

    /// Retain the native client while its host awaits actual close admission.
    /// The live host and tab metadata survive the provisional close animation.
    pub fn cancel_tab_close(&mut self, tab_id: u64) -> bool {
        let restored = self.bar.cancel_close(tab_id);
        self.closing_hosts.remove(&tab_id);
        self.pending_close_commit.retain(|id| *id != tab_id);
        if restored && self.closing_at.is_some() && !self.bar.tabs().is_empty() {
            // The last tab started the group's own retirement. The client has
            // not exited (it may be saving or asking), so the window stays and
            // the retirement is abandoned. Leaving it running would fade and
            // prune the group while its live tab remains canonical; resync then
            // rebuilds it and the real destroy animates a second time.
            self.closing_at = None;
            self.close_lands = false;
            self.close_requested = true;
            self.width_spring = None;
            self.style_dirty = true;
        }
        restored
    }

    /// Test-only: the state a click on a tab's `✕` leaves before the shell
    /// harvests it.
    pub fn click_close_for_tests(&mut self, tab_id: u64) -> bool {
        if !self.bar.start_closing(tab_id) {
            return false;
        }
        let _ = self.bar.take_close();
        self.closing_hosts
            .entry(tab_id)
            .or_insert_with(crate::time::now);
        if !self.pending_close_commit.contains(&tab_id) {
            self.pending_close_commit.push(tab_id);
        }
        if self.bar.tabs().is_empty() {
            self.start_closing();
        }
        true
    }

    pub fn drag_in_flight(&self) -> bool {
        self.bar.drag_in_flight()
    }

    /// The tab this group's bar is dragging, while it drags one.
    pub fn dragged_tab(&self) -> Option<u64> {
        self.bar.dragged_tab()
    }

    /// Whether one of this group's edges is being dragged to resize it.
    pub fn edge_resize_in_flight(&self) -> bool {
        self.edge_drag.is_some()
    }

    /// A no-op while the bar has its own `pending_drag` in flight, so the
    /// source bar's own indicator stays put.
    pub fn set_external_drag_hover(&mut self, cursor: Option<Point>) {
        self.bar.set_external_drag_hover(cursor);
    }

    #[cfg(test)]
    pub(crate) fn bar_mut(&mut self) -> &mut crate::widget::TabBar {
        &mut self.bar
    }

    /// `crates/session/shell/tests/` integration tests need this from outside
    /// the crate, as do its unit tests in an optimised profile.
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn bar_mut_for_tests(&mut self) -> &mut crate::widget::TabBar {
        &mut self.bar
    }

    #[cfg(any(test, debug_assertions))]
    pub fn close_rect_for_tests(&self, tab_id: u64) -> Option<Rect> {
        self.bar.close_rect_for_tests(tab_id)
    }

    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn tile_kind_labels_for_tests(&self) -> Vec<String> {
        self.hosts.iter().map(|(_, h)| h.kind_string()).collect()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn active_host_rect_for_tests(&self) -> Option<Rect> {
        self.presentation_bounds().client
    }

    #[cfg(any(test, debug_assertions))]
    pub fn bar_for_tests(&self) -> &crate::widget::TabBar {
        &self.bar
    }

    #[cfg(test)]
    pub fn close_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::X
    }

    #[cfg(test)]
    pub fn host_count(&self) -> usize {
        self.hosts.len()
    }

    /// Detaching and re-inserting would do the same job plus three things
    /// nobody asked for: activate the tab, play the arrival fade, and
    /// move it to the caller's index.
    pub fn host_for_tab_mut(&mut self, tab_id: u64) -> Option<&mut TileHost> {
        self.hosts
            .iter_mut()
            .find(|(id, _)| *id == tab_id)
            .map(|(_, h)| h)
    }

    /// Laid-out content bounds for a tab. Shell-owned system windows use
    /// this as their ordinary tile viewport instead of inventing separate
    /// floating geometry.
    pub fn content_rect_for_tab(&self, tab_id: u64) -> Option<Rect> {
        self.hosts
            .iter()
            .find(|(id, _)| *id == tab_id)
            .map(|(_, host)| host.get_rect())
    }

    #[cfg(test)]
    pub fn has_host(&self, id: u64) -> bool {
        self.hosts.iter().any(|(i, _)| *i == id)
    }

    #[cfg(test)]
    pub fn force_expire_closing(&mut self, id: u64) {
        if self.closing_hosts.contains_key(&id) {
            self.closing_hosts.insert(
                id,
                crate::time::now()
                    .checked_sub(group_close_fade() + Duration::from_millis(50))
                    .unwrap_or_else(crate::time::now),
            );
        }
    }

    #[cfg(test)]
    pub fn has_closing_host(&self, id: u64) -> bool {
        self.closing_hosts.contains_key(&id)
    }

    #[cfg(test)]
    pub(crate) fn set_closing_at_for_tests(&mut self, t: Instant) {
        self.closing_at = Some(t);
    }

    fn clear_maximized_controls(&mut self) {
        self.maximized_controls.cancel_pointer_press();
        self.maximized_controls.begin_frame();
        self.registered_controls = None;
        self.restore_rect = Rect::ZERO;
        self.close_rect = Rect::ZERO;
        self.fullscreen_rect = Rect::ZERO;
        self.title_rect = Rect::ZERO;
        self.restore_hover = false;
        self.close_hover = false;
        self.fullscreen_hover = false;
    }

    /// The group's own pointer and touch input, for the top-edge intent.
    fn feed_top_edge(&mut self, event: &WindowEvent, scale_factor: f64) {
        if !self.maximized_chrome_live() {
            return;
        }
        let scale = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let changed = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.note_maximized_pointer(Point::new(position.x / scale, position.y / scale))
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                self.note_maximized_press(self.last_cursor);
                false
            }
            WindowEvent::CursorLeft { .. } => self.top_edge.on_leave(crate::time::now()),
            WindowEvent::Touch(touch) => {
                let p = Point::new(touch.location.x / scale, touch.location.y / scale);
                match touch.phase {
                    winit::event::TouchPhase::Started => {
                        let (bar, controls) = self.edge_rects();
                        let geo = crate::widget::EdgeGeometry {
                            window: self.rect,
                            bar: &bar,
                            controls: &controls,
                        };
                        self.top_edge.on_touch_start(p, &geo);
                        false
                    }
                    winit::event::TouchPhase::Moved => {
                        self.top_edge.on_touch_move(p, crate::time::now())
                    }
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled => {
                        self.top_edge.on_touch_end();
                        false
                    }
                }
            }
            _ => false,
        };
        if changed && !matches!(event, WindowEvent::CursorMoved { .. }) {
            self.top_edge_changed();
        }
    }

    fn handle_maximized_controls(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if !self.maximized || !self.navigation_bar_visible() {
            self.clear_maximized_controls();
        }
        self.feed_top_edge(event, ctx.scale_factor);
        // System chrome keeps the active client's keyboard focus. It only
        // owns the pointer gesture, including its cancelled eventual release.
        let prior_focus = *ctx.focus_target;
        self.grip_dirty |= self.maximized_controls.handle_event(event, ctx);
        *ctx.focus_target = prior_focus;
        if matches!(
            event,
            WindowEvent::CursorLeft { .. } | WindowEvent::Focused(false)
        ) {
            self.restore_hover = false;
            self.close_hover = false;
        }
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.last_cursor = Point::new(self.rect.x0, self.rect.y1 + 1.0);
            self.clear_maximized_controls();
        }
        for action in self.maximized_controls.take_activated() {
            // A used control is done with the bar.
            if self.top_edge.hide(crate::time::now()) {
                self.top_edge_changed();
            }
            if action == CLOSE_TARGET && self.bar.is_held(self.bar.active_id()) {
                // Someone else's window is not this person's to close.
            } else if action == CLOSE_TARGET {
                let active = self.bar.active_id();
                self.bar.start_closing(active);
                let _ = self.bar.take_close();
                let started = self.close_started();
                self.closing_hosts.entry(active).or_insert(started);
                if !self.pending_close_commit.contains(&active) {
                    self.pending_close_commit.push(active);
                }
            } else if action == RESTORE_TARGET {
                self.begin_rect_spring_from_current();
            } else if action == FULLSCREEN_TARGET {
                // Real fullscreen keeps the group maximized underneath; the
                // host leaves it when fullscreen ends.
                self.pending_fullscreen_request = true;
                continue;
            } else {
                continue;
            }
            self.maximized = false;
            self.style_dirty = true;
            self.pending_maximize_change = Some(false);
            self.clear_maximized_controls();
        }
    }

    /// The event kinds a group's maximized-chrome close/restore chips and
    /// pin shortcut own outright — decided before the tab bar or active tile
    /// ever see the event. Returns whether the event was fully consumed (the
    /// caller should mark `handled` and stop dispatching further). Edge
    /// resizing is not here: the parent row owns it, because which window an
    /// edge belongs to depends on its neighbour ([`super::edge_resize`]).
    fn handle_direct_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        m: winit::event::Modifiers,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                self.restore_hover = self.maximized && self.restore_rect.contains(pt);
                self.fullscreen_hover = self.maximized && self.fullscreen_rect.contains(pt);
                self.close_hover = self.maximized && self.close_rect.contains(pt);
                false
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } if !self.external_navigation
                && m.state().control_key()
                && m.state().shift_key()
                && matches!(logical_key, Key::Character(s) if s.eq_ignore_ascii_case("p")) =>
            {
                // Routed here (not the bar's own handler) because keyboard
                // events arrive at the focused widget and the bar isn't
                // focusable.
                let active = self.bar.active_id();
                self.bar.toggle_pin(active);
                true
            }
            _ => false,
        }
    }

    /// Drops hosts whose close fade has fully elapsed (removal is deferred
    /// so the fade plays out with the tile still in place), then starts
    /// closing the whole group once nothing is left in it.
    fn sweep_expired_closing_hosts(&mut self) {
        // A closing window keeps its last tab's host to draw until the group
        // is pruned with everything in it.
        if self.is_closing() {
            return;
        }
        let close_window = group_close_fade();
        let expired: Vec<u64> = self
            .closing_hosts
            .iter()
            .filter(|(_, started)| {
                crate::time::now().saturating_duration_since(**started) >= close_window
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.closing_hosts.remove(&id);
            if let Some(idx) = self.hosts.iter().position(|(i, _)| *i == id) {
                let (_, mut host) = self.hosts.remove(idx);
                self.close_reports
                    .extend(crate::component_lifecycle::close_components(&mut host));
            }
            self.bar.finalize_close(id);
        }

        // `bar.tabs().is_empty()` rather than just `hosts.is_empty()` —
        // the bar is the user-visible "empty" signal.
        if !self.is_closing()
            && self.bar.tabs().is_empty()
            && self.hosts.is_empty()
            && self.closing_hosts.is_empty()
        {
            self.start_closing();
        }
    }

    /// Drains the tab bar's one-shot signals (close/maximize/search/
    /// new-tab/reorder) queued by `TabBar::handle_event` and applies each
    /// to group state. Returns whether any signal fired, so the caller can
    /// mark the event handled and skip forwarding it to the active tile.
    fn drain_bar_signals(&mut self) -> bool {
        let mut handled = false;

        // Host stays in `self.hosts` for `CLOSE_FADE_MS`; the sweep above drops it.
        if let Some(close_id) = self.bar.take_close() {
            let started = self.close_started();
            self.closing_hosts.entry(close_id).or_insert(started);
            if !self.pending_close_commit.contains(&close_id) {
                self.pending_close_commit.push(close_id);
            }
            if self.bar.tabs().is_empty() {
                self.start_closing();
            }
            handled = true;
        }

        // Without `style_dirty` the cached taffy node keeps the
        // pre-toggle width and the click looks like a no-op.
        // The window button is a pointer's: the window moves to its new
        // rect on the window motion, as Super+M from the same place would
        // not (a shortcut lands).
        if let Some(_id) = self.bar.take_max() {
            self.begin_rect_spring_from_current();
            self.maximized = !self.maximized;
            self.style_dirty = true;
            self.pending_maximize_change = Some(self.maximized);
            handled = true;
        }

        handled |= self.drain_chip_presses();

        // The group knows its own id; the shell knows what a new tab
        // should contain, so republish with the id and let it decide.
        if self.bar.take_new_tab().is_some() {
            self.pending_new_tab = Some(self.id);
            handled = true;
        }

        // `bar.tabs` and `hosts` are a strictly-aligned 1:1 mapping; a
        // refactor that permutes only one would desync pill `i` from
        // content `i` — the debug_assert documents that invariant.
        if let Some((from, to)) = self.bar.take_reorder()
            && self.reorder_tab(from, to)
        {
            self.pending_tab_reorder = Some((from, to));
            handled = true;
        }

        handled
    }

    /// The alpha/x/y/scale the group's spawn-in or close-out fade applies
    /// this frame, matching whichever animation (if any) is in flight.
    /// `fade_y` is currently always 0, kept in the tuple in case a future
    /// close curve wants a settle.
    fn fade_transform_params(&self) -> (f32, f64, f64, f64) {
        if self.close_complete() {
            return (0.0, 0.0, 0.0, 0.95);
        }
        if self.reduce_motion {
            return (if self.is_closing() { 0.0 } else { 1.0 }, 0.0, 0.0, 1.0);
        }
        if let Some(started) = self.closing_at {
            // No offset: the per-host curve already carries the y-drift.
            // Ratio of two `Duration`s taken via `as_secs_f64` rather than
            // an integer->float `as` cast on the millisecond counts.
            let eased = window_progress(
                crate::motion::Direction::Exit,
                crate::time::now().saturating_duration_since(started),
            );
            let alpha = f32_of_f64((1.0 - eased).max(0.0));
            let scale = 1.0 - 0.05 * eased;
            (alpha, 0.0, 0.0, scale)
        } else if let Some(started) = self.arrival_fade_at {
            let eased = arrival_progress(crate::time::now().saturating_duration_since(started));
            (crate::motion::unit_opacity(eased), 0.0, 0.0, 1.0)
        } else {
            (1.0, 0.0, 0.0, 1.0)
        }
    }

    /// `transform` moved by the spawn or close curve's offset and scale
    /// (`fade_transform_params`) and by the rect spring: where the group
    /// and everything of it paints this frame.
    fn fade_affine(
        &self,
        transform: Affine,
        (fade_x, fade_y, fade_scale): (f64, f64, f64),
    ) -> Affine {
        // Tolerance rather than exact float equality: this only gates a
        // no-op-transform fast path, so a value merely close to identity
        // is close enough to skip composing it.
        const FADE_EPS: f64 = 1e-9;
        // `fade_y` is currently always 0, kept in the tuple in case a
        // future close curve wants a settle.
        // Affine composition is floating-point matrix multiply — it
        // cannot overflow or panic the way integer arithmetic can.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let transform = if fade_x.abs() > FADE_EPS
            || fade_y.abs() > FADE_EPS
            || (fade_scale - 1.0).abs() > FADE_EPS
        {
            let cx = self.rect.center().x;
            let cy = self.rect.center().y;
            transform
                * Affine::translate((cx + fade_x, cy + fade_y))
                * Affine::scale(fade_scale)
                * Affine::translate((-cx, -cy))
        } else {
            transform
        };
        self.rect_draw_affine(transform)
    }

    /// Paints each still-fading closed tab (opacity 1→0, scale 1→0.95,
    /// y 0→-20 over `CLOSE_FADE_MS`). The id list is snapshotted first to
    /// borrow `self.hosts` mutably without fighting the borrow checker
    /// against `self.closing_hosts`.
    fn draw_closing_hosts(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The window closing with its last tab fades out as one, its
        // content with its bar and frame: the group's layer carries the fade,
        // so the tab is drawn as it last was for as long as the group closes.
        if self.is_closing() {
            let ids: Vec<u64> = self.closing_hosts.keys().copied().collect();
            for id in ids {
                if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == id) {
                    h.draw(scene, transform, tr, theme, presence);
                }
            }
            return;
        }
        let close_window = group_close_fade();
        let closing_ids: Vec<(u64, Instant)> = self
            .closing_hosts
            .iter()
            .filter(|(_, t)| crate::time::now().saturating_duration_since(**t) < close_window)
            .map(|(id, t)| (*id, *t))
            .collect();
        for (id, started) in closing_ids {
            let eased = crate::motion::ListChange::FADE.progress_after(
                crate::motion::Direction::Exit,
                crate::tokens::EffectiveMotion::default(),
                crate::tokens::MotionSource::Pointer,
                crate::time::now().saturating_duration_since(started),
            );
            let alpha = f32_of_f64(1.0 - eased);
            let scale = 1.0 + (-0.05) * eased; // 1.0 → 0.95
            let y_offset = -20.0 * eased;
            if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == id) {
                let rect = h.get_rect();
                // Matches the tile-spawn clip so shadows aren't chopped.
                let clip = Rect::new(
                    rect.x0 - 160.0,
                    rect.y0 - 160.0,
                    rect.x1 + 160.0,
                    rect.y1 + 160.0,
                );
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha.max(0.0),
                    transform,
                    &clip,
                );
                let centre = rect.center();
                // Affine composition is floating-point matrix multiply —
                // it cannot overflow or panic the way integer arithmetic
                // can.
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
                )]
                let close_transform = transform
                    * Affine::translate((0.0, y_offset))
                    * Affine::translate((centre.x, centre.y))
                    * Affine::scale(scale)
                    * Affine::translate((-centre.x, -centre.y));
                h.draw(scene, close_transform, tr, theme, presence);
                scene.pop_layer();
            }
        }
    }

    /// The two-bar resize grip on the edge of this window a press would
    /// resize, or is resizing ([`Self::edge_highlight`]): it marks which
    /// window will change before anything is pressed. Nothing at rest.
    #[allow(
        clippy::similar_names,
        reason = "cx/cy is the standard center-x/center-y coordinate naming convention; \
                  distinguishing them further would reduce clarity"
    )]
    fn draw_resize_grip(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(edge) = self.edge_highlight() else {
            return;
        };
        // Neutral ink, never the accent: secondary at rest, primary while
        // the edge is being dragged.
        let grip_col = if self.edge_drag.is_some() {
            theme.tokens.colors.text_primary
        } else {
            theme.tokens.colors.text_dim
        };
        let total_h = self.rect.height();
        let grip_h = (total_h * GRIP_HEIGHT_SHARE).clamp(
            f64::from(theme.tokens.spacing.s6),
            f64::from(theme.tokens.spacing.s16) + f64::from(theme.tokens.spacing.s4),
        );
        // Two bars, each `GRIP_BAR_PX` wide, that together fill the
        // `RESIZE_HANDLE_PX` band exactly: centred on the band's own centre,
        // one on either side of it, so the painted grip and the pointer
        // zone are the same thing.
        let grip_cx = match edge {
            WindowEdge::Leading => self.rect.x0 + RESIZE_HANDLE_PX / 2.0,
            WindowEdge::Trailing => self.rect.x1 - RESIZE_HANDLE_PX / 2.0,
        };
        let grip_cy = self.rect.center().y;
        let bar = |dx: f64| {
            vello::kurbo::RoundedRect::from_rect(
                Rect::new(
                    grip_cx + dx - GRIP_BAR_PX / 2.0,
                    grip_cy - grip_h / 2.0,
                    grip_cx + dx + GRIP_BAR_PX / 2.0,
                    grip_cy + grip_h / 2.0,
                ),
                GRIP_BAR_PX / 2.0,
            )
        };
        let offset = RESIZE_HANDLE_PX / 2.0 - GRIP_BAR_PX / 2.0;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            grip_col,
            None,
            &bar(-offset),
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            grip_col,
            None,
            &bar(offset),
        );
    }

    /// Paint the app being carried as one lifted object: real app pixels tucked
    /// under the translucent tab handle. This runs in the floating-surface pass
    /// so the preview can cross group and strip clips without changing hit-test
    /// geometry or client configure sizes.
    fn draw_tab_drag_visual(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(visual) = self.bar.drag_visual(theme) else {
            return;
        };
        let Some((_, host)) = self.hosts.iter_mut().find(|(id, _)| *id == visual.tab_id) else {
            self.bar
                .draw_drag_ghost(scene, transform, tr, theme, presence);
            return;
        };
        let source = host.get_rect();
        if source.width() <= 0.0 || source.height() <= 0.0 {
            // Inactive hosts intentionally have no layout until selected.
            // Keep the carried handle visible without waking/rendering them.
            self.bar
                .draw_drag_ghost(scene, transform, tr, theme, presence);
            return;
        }

        let scale = 1.0 - (1.0 - DRAG_CONTENT_SCALE_END) * visual.lift_progress.clamp(0.0, 1.0);
        let destination_top_center = Point::new(
            visual.lifted_handle.center().x,
            visual.lifted_handle.y1 + f64::from(theme.tokens.spacing.s1),
        );
        let source_top_center = Point::new(source.center().x, source.y0);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine multiplication composes finite floating-point transforms and cannot integer-overflow"
        )]
        let preview_transform = transform
            * Affine::translate((destination_top_center.x, destination_top_center.y))
            * Affine::scale(scale)
            * Affine::translate((-source_top_center.x, -source_top_center.y));

        // Dragging is elevation, not selection: use the Object role's e4 lift
        // before the deliberately translucent content layer.
        crate::surface::shadow(
            scene,
            preview_transform,
            source,
            f64::from(theme.tokens.radii.tile),
            &theme.tokens.elevation.floating,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            DRAG_CONTENT_ALPHA,
            preview_transform,
            &source.inflate(160.0, 160.0),
        );
        let was_focused = host.focused;
        host.set_focused(false);
        host.draw(scene, preview_transform, tr, theme, presence);
        host.set_focused(was_focused);
        scene.pop_layer();

        // The handle stays full-size while the content settles beneath it.
        self.bar
            .draw_drag_ghost(scene, transform, tr, theme, presence);
    }
}

impl FloatingSurfaceProvider for WindowGroup {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_focus_changed(host, id, focused);
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_ime_source_changed(host, id);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_event_source(host, id);
        }
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        self.hosts
            .iter()
            .find_map(|(_, host)| FloatingSurfaceProvider::floating_surface_accessibility(host, id))
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        self.hosts.iter_mut().any(|(_, host)| {
            FloatingSurfaceProvider::perform_floating_surface_accessibility_action(
                host, id, target, action, state, path,
            )
        })
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, Rect)> {
        self.hosts
            .iter()
            .find_map(|(_, host)| FloatingSurfaceProvider::floating_surface_ime(host, id))
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        // A detached presentation remains interactive even when its source tab
        // is not selected. Drain every retained owner, not only the tiled one.
        self.hosts
            .iter_mut()
            .find_map(|(_, host)| FloatingSurfaceProvider::take_floating_surface_request(host))
    }

    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        self.active_host_mut()
            .and_then(FloatingSurfaceProvider::take_capture_studio_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.hosts
            .iter_mut()
            .flat_map(|(_, host)| {
                FloatingSurfaceProvider::take_floating_surface_source_updates(host)
            })
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
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::draw_floating_surfaces(
                host,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        self.draw_tab_drag_visual(scene, transform, text_renderer, theme, presence);
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
        self.hosts.iter_mut().any(|(_, host)| {
            FloatingSurfaceProvider::draw_floating_surface(
                host,
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
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::set_floating_surface_bounds(host, bounds);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        self.hosts
            .iter()
            .flat_map(|(_, host)| FloatingSurfaceProvider::floating_surface_presentations(host))
            .collect()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.hosts
            .iter()
            .flat_map(|(_, host)| FloatingSurfaceProvider::active_floating_surface_ids(host))
            .collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_host_message(host, message);
        }
    }
}

impl Widget for WindowGroup {
    crate::provides!(
        dyn crate::widget::FloatingSurfaceProvider,
        dyn crate::collaboration::participants::CollaborationTopology,
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn crate::widget::controls::ActionControls,
        dyn crate::component_lifecycle::ComponentLifecycle
    );

    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        (!self.is_closing()).then(|| self.active_tab_id())
    }

    fn primary_vertical_scroll(&self) -> Option<crate::widget::PrimaryScrollRegion> {
        self.active_host()?.primary_vertical_scroll()
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: crate::widget::PrimaryScrollAction,
    ) -> crate::widget::PrimaryScrollResult {
        self.active_host_mut()
            .map_or(crate::widget::PrimaryScrollResult::Unsupported, |host| {
                host.apply_primary_vertical_scroll(action)
            })
    }

    // Presentation traversal follows the active tab, while lifecycle and
    // History ownership must cover every retained tab. Closing a group or
    // switching tabs must not hide an inactive document from durability or
    // retire its framework History route.
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners_mut(
        &mut self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        for (_, host) in &mut self.hosts {
            request.visit(host)?;
        }
        Ok(())
    }

    // Modal capture and presentation polling follow visible content, never inactive tabs.
    fn modal_capture(&self) -> Option<Rect> {
        self.children().into_iter().find_map(Widget::modal_capture)
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        // The bar's dwell or hold runs out with no further input.
        let reveal = self
            .maximized_chrome_live()
            .then(|| self.top_edge.deadline())
            .flatten();
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(reveal)
            .min()
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.bar.set_platform_conventions(conventions);
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| host as &mut dyn Widget)
        {
            child.set_platform_conventions(conventions);
        }
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.bar.set_interaction_environment(environment);
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| host as &mut dyn Widget)
        {
            child.set_interaction_environment(environment);
        }
    }

    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        self.bar.mount_native_views(host);
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| host as &mut dyn Widget)
        {
            child.mount_native_views(host);
        }
    }

    fn unmount_native_views(&mut self) {
        self.bar.unmount_native_views();
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| host as &mut dyn Widget)
        {
            child.unmount_native_views();
        }
    }

    fn native_view_frame_start(&mut self) {
        self.bar.native_view_frame_start();
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| host as &mut dyn Widget)
        {
            child.native_view_frame_start();
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        // Maximized or not, the bar and its search are the window's
        // children: laid out, drawn and reached (accessibility, tooltips,
        // redraw) the same way.
        if self.navigation_bar_visible() {
            children.push(&self.bar);
            children.push(&self.search);
        }
        let active = self.bar.active_id();
        if let Some((id, host)) = self.hosts.iter().find(|(id, _)| *id == active) {
            // A host swapped in through `host_for_tab_mut` is named here.
            host.set_panic_scope(*id);
            children.push(host);
        }
        children
    }
    /// The window's top layer: its app's, then its bar's menus over it,
    /// maximized or not.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Children run bar, search, app: reversed, the app's layer goes first.
        for child in self.children_mut().into_iter().rev() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let active = self.bar.active_id();
        let mut children: Vec<&mut dyn Widget> = Vec::new();
        if self.navigation_bar_visible() {
            children.push(&mut self.bar);
            children.push(&mut self.search);
        }
        if let Some((id, host)) = self.hosts.iter_mut().find(|(id, _)| *id == active) {
            host.set_panic_scope(*id);
            children.push(host);
        }
        children
    }

    // Read-only resource discovery follows retained owners, including inactive tabs.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for (id, host) in &self.hosts {
            host.set_panic_scope(*id);
            visitor(host);
        }
    }

    /// The bar and its search as well as every tab's host.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.bar);
        visitor(&self.search);
        self.for_each_child(visitor);
        true
    }

    #[allow(private_interfaces, reason = "toolkit-only checked custody traversal")]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        for child in self.hosts.iter_mut().map(|(_, host)| host) {
            child.visit_components(request)?;
        }
        Ok(())
    }

    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        if let Some(h) = self.active_host_mut() {
            h.sync(s, p);
        }
    }
    fn update(&mut self, s: &AppModel, p: &ModelPath) -> bool {
        // A chip pressed from assistive technology arrives outside
        // `handle_event`: run it here, before the frame lays out.
        let pressed = self.drain_chip_presses();
        // The search follows the active tab, and the content under it: a
        // match count that follows edits, a list that follows its rows.
        self.follow_search_tab();
        let searched = self.service_search();
        self.drive_search();
        // The bar's hover dwell or aim hold that came due (its wake
        // deadline) opens its pill here, before the frame lays out.
        let bar = self.bar.update(s, p);
        self.active_host_mut().is_some_and(|h| h.update(s, p)) || pressed || searched || bar
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.close_duration = window_close_time();
        self.set_reduce_motion(theme.reduce_motion);
        // This is the layout a maximized frame that came to rest is owed.
        self.rest_layout_owed = false;
        self.chrome_motion = theme.motion();
        self.fullscreen_bar.laid_out = self.bar_over_content();
        let bar_node = self
            .navigation_bar_visible()
            .then(|| self.bar.build_layout(taffy, theme, node_map));
        self.lay_bar_over_content(taffy, bar_node);
        let active_id = self.bar.active_id();
        self.offer_history_controls();
        for (id, host) in &mut self.hosts {
            if *id != active_id {
                host.discard_layout_nodes();
            }
        }
        // See `built_active`'s field doc: forces a rebuild when the
        // active tab switches.
        self.built_active = Some(active_id);
        self.offer_window_bar(active_id, theme);
        let tile_node = if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.set_flush_edges(self.external_navigation || self.fullscreen_presentation);
            h.build_layout(taffy, theme, node_map)
        } else {
            taffy.leaf_node(Style::default())
        };
        // `flex_grow: 0` keeps a group at its
        // declared `WindowWidth` regardless of sibling count or slack —
        // placing a new app never resizes siblings, the strip extends
        // past the viewport instead. Maximized overrides this below.
        let mut style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::length(f32_of_f64(self.layout_width_px())),
                height: Dimension::percent(1.0),
            },
            flex_grow: 0.0,
            gap: taffy::Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        };
        if self.maximized {
            style.size.width = Dimension::auto();
            style.flex_grow = 1.0e6;
            style.flex_basis = Dimension::length(0.0);
        }
        // Moved down, not shrunk, while the desktop's bar shows over the
        // windows' top edge (a maximized window, or a bar hidden by choice):
        // the window's own bar is never under the desktop's.
        let slide = self.top_slide_now();
        if slide > 0.0 {
            style.inset.top = taffy::LengthPercentageAuto::length(f32_of_f64(slide));
        }
        let nodes = bar_node.map_or_else(|| vec![tile_node], |bar| vec![bar, tile_node]);
        if let Some(laid) = self.content_width_in_flight() {
            for &node in &nodes {
                if let Ok(child) = taffy.style(node) {
                    let mut child = child.clone();
                    child.size.width = Dimension::length(f32_of_f64(laid));
                    child.min_size.width = Dimension::length(f32_of_f64(laid));
                    child.max_size.width = Dimension::length(f32_of_f64(laid));
                    let _ = taffy.set_style(node, child);
                }
            }
        }
        taffy.widget_node(style, &nodes, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let previous = (self.rect, self.active_host().map(Widget::get_rect));
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.retarget_rect_spring();
        let children = taffy.children(node).unwrap_or_default();
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if self.navigation_bar_visible() {
            if let Some(&bar_child) = children.first() {
                self.bar.apply_layout(taffy, bar_child, new_origin);
            }
        }
        self.place_search();
        let active_id = self.bar.active_id();
        let tile_index = usize::from(self.navigation_bar_visible());
        if let (Some((_, h)), Some(&tile_child)) = (
            self.hosts.iter_mut().find(|(i, _)| *i == active_id),
            children.get(tile_index),
        ) {
            h.apply_layout(taffy, tile_child, new_origin);
        }
        self.bar_slot = self
            .bar_offered_to
            .filter(|id| *id == active_id)
            .and_then(|id| self.hosts.iter().find(|(i, _)| *i == id))
            .and_then(|(_, h)| h.window_bar_slot());
        if previous != (self.rect, self.active_host().map(Widget::get_rect)) {
            self.clear_maximized_controls();
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
        self.reduce_motion = theme.reduce_motion;
        // The slide is a layout offset: lay out again on every frame of it.
        if self.top_slide_in_flight() {
            self.style_dirty = true;
        }
        self.step_rect_spring();
        self.offer_history_controls();
        // An indicator that flashes in and out before an app has time to
        // draw would just be a flicker, so nothing paints until either
        // something has drawn or the wait is long enough to say so.
        if !Widget::presented(self) && !Widget::opening(self) {
            if self.is_closing() {
                // A dying client must not blank the card: keep the last
                // frame we painted while the slot collapses.
            } else {
                self.held_back = true;
                return;
            }
        }
        // The first frame after holding the place back. The slot has been
        // opening since the window was asked for and goes on from where it
        // is; the content shows whole when it came inside the grace (a
        // local app's first frame normally does), and cross-fades in over
        // the ground its place held when it came later.
        self.take_first_paint();
        // Close checked first (mutually exclusive with an arrival): the
        // window fades out as one (α 1→0, scale 1→0.95) while its slot
        // closes; a late arrival cross-fades in. The whole paint composes
        // under one alpha+translate+scale layer so hit tests stay aligned
        // with what's on screen mid-fade.
        let (fade_alpha, fade_x, fade_y, fade_scale) = self.fade_transform_params();
        let spawn_active = fade_alpha < 1.0;
        if spawn_active {
            // 16px clip margin so outer strokes (glow rings, focus
            // outlines) aren't clipped mid-fade.
            let clip = Rect::new(
                self.rect.x0 - 16.0,
                self.rect.y0 - 16.0,
                self.rect.x1 + 16.0,
                self.rect.y1 + 16.0,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                fade_alpha,
                transform,
                &clip,
            );
        }
        let transform = self.fade_affine(transform, (fade_x, fade_y, fade_scale));
        // A frame in flight clips what it does not yet (or no longer) show,
        // and where it is larger than what is laid out in it (a window
        // growing past the last frame its app drew, or restoring from
        // maximized) its ground fills the rest: its body is never empty.
        let frame_clip = self.frame_clip();
        if frame_clip.is_none() {
            for (_, host) in &mut self.hosts {
                host.set_inside_growing_frame(false);
            }
        }
        if let Some(frame) = frame_clip {
            let shape =
                vello::kurbo::RoundedRect::from_rect(frame, f64::from(theme.tokens.radii.tile));
            scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &shape);
            let laid = self.content_width_in_flight().unwrap_or(self.rect.width());
            let grows_past =
                frame.width() > laid + 0.5 || frame.height() > self.rect.height() + 0.5;
            for (_, host) in &mut self.hosts {
                host.set_inside_growing_frame(grows_past);
            }
            if grows_past {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.content,
                    None,
                    &shape,
                );
            }
        }
        // The bar's hit rects stay live either way — only the paint is
        // gated (maximized swaps in the floating pill further down).
        let bar_over = self.bar_over_content();
        if self.navigation_bar_visible() && !bar_over {
            self.draw_bar(scene, transform, tr, theme, presence);
        }
        let active_id = self.bar.active_id();
        // A tile holding a place for an app that has not arrived takes
        // its name from the pill above it, so the two never disagree.
        let opening_name = self
            .bar
            .tabs()
            .iter()
            .find(|t| t.id == active_id)
            .map(|t| t.title.clone());
        // Scoping search *is* aiming the keyboard at this group, so it
        // lights the same edge/shadow signal as keyboard focus.
        let focused = self.focused || self.search_active;
        let dragged_tab = self.bar.drag_visual(theme).map(|visual| visual.tab_id);
        let bar_visible = self.navigation_bar_visible();
        self.advance_tab_handover(theme);
        let now = crate::motion::frame_now();
        let outgoing = self
            .tab_fade
            .as_ref()
            .and_then(|fade| fade.outgoing(now).map(|(id, _)| *id));
        let incoming = outgoing
            .and(self.tab_fade.as_ref())
            .map(|fade| fade.incoming(now));
        // The tab shown before stays whole under the new one while that
        // fades in over it: two windows' grounds never thin out together.
        if let Some(out_id) = outgoing
            && let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == out_id)
        {
            h.draw(scene, transform, tr, theme, presence);
        }
        if let Some(standing_in) = self.tab_standing_in() {
            if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == standing_in) {
                h.draw(scene, transform, tr, theme, presence);
            }
        } else if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            if let Some(name) = &opening_name {
                h.set_opening_name(name);
            }
            h.set_focused(focused);
            // A maximized window filling its display draws edge to edge;
            // under a docked bar, or mid-fade, it is still a card.
            h.set_square_bottom(self.maximized);
            h.set_full_bleed(self.maximized && self.fills_display && !spawn_active && !bar_visible);
            let source_is_lifted = dragged_tab == Some(active_id);
            if source_is_lifted {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    DRAG_SOURCE_CONTENT_ALPHA,
                    transform,
                    &h.get_rect().inflate(160.0, 160.0),
                );
            }
            if let Some(alpha) = incoming {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    transform,
                    &h.get_rect(),
                );
            }
            h.draw(scene, transform, tr, theme, presence);
            if incoming.is_some() {
                scene.pop_layer();
            }
            if source_is_lifted {
                scene.pop_layer();
            }
        }
        self.draw_closing_hosts(scene, transform, tr, theme, presence);
        if bar_over {
            self.draw_bar_over_content(scene, transform, tr, theme, presence);
        }
        if self.navigation_bar_visible() {
            let band = self.bar.get_rect().inflate(0.0, 6.0);
            self.bar.draw_holder_chip(scene, transform, tr, theme, band);
        }
        // Only under the pointer or while dragging: at rest the grip sits on
        // the window's own trailing edge, over its content, where it reads
        // as that app's scrollbar (and stays put when the app's panels go).
        // The east-west cursor already marks the band before it shows.
        if !self.maximized && !self.fullscreen_presentation {
            self.draw_resize_grip(scene, transform, theme);
        }

        // No outline rings around `self.rect` for drag/search/maximize:
        // per `crates/sdk/ui-core/HIG.md` §9.4 the accent means *live*, not *chosen*.
        self.clear_maximized_controls();

        if frame_clip.is_some() {
            scene.pop_layer();
        }
        // Matches the `spawn_active` push above.
        if spawn_active {
            scene.pop_layer();
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.is_closing() {
            return;
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let bar_child = self
            .navigation_bar_visible()
            .then(|| children.first().copied())
            .flatten();
        let tile_child = children
            .get(usize::from(self.navigation_bar_visible()))
            .copied();
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            // The active tab may have changed since the window was focused.
            self.sync_host_keyboard();
        }
        self.handle_maximized_controls(event, ctx);
        if *ctx.handled {
            return;
        }
        if self.handle_edge_input(event, ctx.scale_factor, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }
        // The window's search: Find opens it, and while open it takes what
        // is typed into its field before the bar or the app sees it.
        let found = self.takes_find(event, *ctx.modifiers);
        let routed = !found
            && self.navigation_bar_visible()
            && self
                .search
                .route(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
        if found || routed {
            self.service_search();
            self.drive_search();
            *ctx.handled = true;
            return;
        }
        if let Some(bar_child) = bar_child {
            self.bar
                .handle_event(event, &mut ctx.reborrow(ctx.path, bar_child));
        }

        self.sweep_expired_closing_hosts();
        if self.drain_bar_signals() {
            *ctx.handled = true;
        }

        if *ctx.handled {
            return;
        }
        let active_id = self.bar.active_id();
        if let (Some((_, h)), Some(tile_child)) = (
            self.hosts.iter_mut().find(|(i, _)| *i == active_id),
            tile_child,
        ) {
            h.handle_event(event, &mut ctx.reborrow(ctx.path, tile_child));
        }
    }
    fn name(&self) -> &'static str {
        "WindowGroup"
    }
    /// **Any**, not all: a group with one live window and a second still
    /// starting must not blink out because a sibling tab hasn't drawn.
    fn presented(&self) -> bool {
        if self.hosts.is_empty() {
            return true;
        }
        // `fold`, not `any`: asking `h.presented()` is what records a
        // tab's window as drawn, so short-circuiting on the first `true`
        // would strand every tab behind it un-asked, each going on
        // painting its placeholder over a window on screen for minutes.
        self.hosts
            .iter()
            .fold(false, |seen, (_, h)| h.presented() || seen)
    }
    /// Only true while *nothing* has presented, so a group with one live
    /// window never turns into a placeholder because a sibling is starting.
    fn opening(&self) -> bool {
        !Widget::presented(self) && self.hosts.iter().any(|(_, h)| h.opening())
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Mirrors the inter-group [`crate::widget::ResizeHandle`] so both
    /// affordances give the same feedback.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.edge_drag.is_some() || self.resize_grab_hit(point) {
            Some(winit::window::CursorIcon::EwResize)
        } else {
            None
        }
    }
    fn wants_pointer_capture(&self) -> bool {
        self.maximized_controls.wants_pointer_capture()
            || self.edge_drag.is_some()
            || self
                .active_host()
                .is_some_and(super::Widget::wants_pointer_capture)
    }
    fn take_structure_dirty(&mut self) -> bool {
        // Every child's flag is drained, but only the active host's counts:
        // inactive hosts have no layout nodes (see `is_layout_dirty`), and
        // selecting one rebuilds through `active_switched` anyway.
        // A bar dwell that matured since the last frame shows the bar in
        // this frame's layout (an app header in the bar joins the tree).
        if self.maximized_chrome_live() && self.top_edge.settle(crate::time::now()) {
            self.top_edge_changed();
        }
        if self.fullscreen_bar.laid_out != self.bar_over_content() {
            self.style_dirty = true;
        }
        let bar = self.bar.take_structure_dirty();
        let active = self.bar.active_id();
        let mut active_host = false;
        for (id, host) in &mut self.hosts {
            let dirty = host.take_structure_dirty();
            if *id == active {
                active_host = dirty;
            }
        }
        let style = std::mem::take(&mut self.style_dirty);
        let active_switched = self.built_active != Some(active);
        let closing = self.is_closing()
            && !self.close_complete()
            && !std::mem::replace(&mut self.close_rebuild_taken, true);
        bar || active_host || style || active_switched || closing
    }
    fn needs_redraw(&self) -> bool {
        // Redraw follows the presentation tree. Retained inactive hosts still
        // participate in resources, lifecycle, components and ownership, but
        // they are not drawn or marked drawn until selected.
        // A matured bar dwell or hold is settled by the next draw.
        let reveal_due = self.maximized_chrome_live()
            && self
                .top_edge
                .deadline()
                .is_some_and(|due| crate::time::now() >= due);
        self.grip_dirty || reveal_due || self.children().into_iter().any(Widget::needs_redraw)
    }
    fn mark_drawn(&mut self) {
        self.grip_dirty = false;
        self.next_frame();
        for child in self.children_mut() {
            child.mark_drawn();
        }
    }
    /// Layout follows the built tree: the tab bar (whenever it was built) and
    /// the active host. Inactive hosts have no taffy nodes until they are
    /// selected, which is a structural change, so their dirtiness must not
    /// keep the group re-measuring; the default would walk `for_each_child`
    /// and include them.
    fn is_layout_dirty(&self) -> bool {
        (self.navigation_bar_visible() && self.bar.is_layout_dirty())
            || self.active_host().is_some_and(Widget::is_layout_dirty)
    }
    fn layout_needs_measure(&self) -> bool {
        (self.navigation_bar_visible() && self.bar.layout_needs_measure())
            || self.active_host().is_some_and(Widget::layout_needs_measure)
    }
    fn has_pending_visual_work(&self) -> bool {
        // Inactive retained owners do not paint. Their record writes must not
        // prevent a still of the active presentation from reaching rest.
        self.children()
            .into_iter()
            .any(Widget::has_pending_visual_work)
    }
    fn animation_detail(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.arrival_fade_at.is_some() {
            parts.push("arrival".to_string());
        }
        if self.closing_at.is_some() {
            parts.push("closing".to_string());
        }
        if !self.closing_hosts.is_empty() {
            parts.push(format!("closing-hosts={}", self.closing_hosts.len()));
        }
        if self.rect_spring.is_some() {
            parts.push("rect-spring".to_string());
        }
        if let Some(spring) = &self.width_spring {
            parts.push(format!(
                "width={:.1}->{:.1} settled={} stepped={} dirty={}",
                spring.value(),
                spring.target(),
                spring.is_settled(),
                self.width_stepped,
                self.style_dirty
            ));
        }
        if self.navigation_bar_visible() && self.bar.needs_animation_frame() {
            parts.push("bar".to_string());
        }
        let active_id = self.bar.active_id();
        if self
            .hosts
            .iter()
            .any(|(i, h)| *i == active_id && h.needs_animation_frame())
        {
            parts.push("host".to_string());
        }
        Some(parts.join(" "))
    }

    fn needs_animation_frame(&self) -> bool {
        if self.close_complete() {
            return false;
        }
        // Include group and inactive closing-host animations beyond the active presentation.
        let spawn_in_flight = self.arrival_fade_at.is_some_and(|t| {
            arrival_progress(crate::time::now().saturating_duration_since(t)) < 1.0
        });
        let close_window = group_close_fade();
        let close_in_flight = !self.reduce_motion
            && self
                .closing_hosts
                .values()
                .any(|t| crate::time::now().saturating_duration_since(*t) < close_window);
        let group_close_in_flight = self.closing_at.is_some_and(|t| {
            !self.reduce_motion
                && crate::time::now().saturating_duration_since(t) < self.close_duration
        });
        let width_in_flight = !self.reduce_motion
            && !self.close_complete()
            && self.width_spring.as_ref().is_some_and(|s| !s.is_settled());
        // Only the *active* tab is laid out and drawn, so only its signal
        // counts — otherwise a background terminal's PTY-poll would pin
        // the whole shell at 60fps/100% CPU forever.
        let active_id = self.bar.active_id();
        let host = self
            .hosts
            .iter()
            .find(|(i, _)| *i == active_id)
            .is_some_and(|(_, h)| h.needs_animation_frame());
        let rect_in_flight =
            (!self.reduce_motion && self.rect_spring.is_some()) || self.rest_layout_owed;
        let chrome_fade = self.maximized_chrome_live()
            && self.top_edge.fading(crate::time::now(), self.chrome_motion);
        let top_slide = self.top_slide_in_flight();
        let tab_fade = self
            .tab_fade
            .as_ref()
            .is_some_and(|fade| fade.is_moving(crate::motion::frame_now()));
        let fullscreen_bar_fade =
            self.fullscreen_presentation && self.fullscreen_bar.fade.is_moving(crate::time::now());
        spawn_in_flight
            || tab_fade
            || fullscreen_bar_fade
            || top_slide
            || chrome_fade
            || close_in_flight
            || group_close_in_flight
            || rect_in_flight
            || width_in_flight
            || (self.navigation_bar_visible() && self.bar.needs_animation_frame())
            || host
    }
}

impl crate::component_lifecycle::ComponentLifecycle for WindowGroup {
    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        self.hosts
            .iter()
            .flat_map(|(_, host)| host.unsaved_documents(doc))
            .collect()
    }
    fn checkpoint_components(&mut self) -> Vec<String> {
        self.hosts
            .iter_mut()
            .flat_map(|(_, host)| host.checkpoint_components())
            .collect()
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.hosts
            .iter_mut()
            .flat_map(|(_, host)| host.finish_admitted_close())
            .collect()
    }
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        for child in self.hosts.iter_mut().map(|(_, host)| host) {
            self.close_reports.extend(child.close_components());
        }
        self.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.close_reports);
        for child in self.hosts.iter_mut().map(|(_, host)| host) {
            reports.extend(child.take_component_close_reports());
        }
        reports
    }
}

impl crate::widget::controls::ActionControls for WindowGroup {
    fn has_pending_control_invocation(&self) -> bool {
        self.hosts
            .iter()
            .any(|(_, host)| host.has_pending_control_invocation())
    }
}

impl crate::history_mode::capability::FrameworkHistoryTopology for WindowGroup {
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        for (_, host) in &self.hosts {
            host.visit_framework_history(visitor);
        }
    }
    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        for (_, host) in &mut self.hosts {
            host.visit_framework_history_mut(visitor);
        }
    }
}

impl crate::collaboration::participants::CollaborationTopology for WindowGroup {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.hosts
            .iter_mut()
            .map(|(_, host)| host as &mut dyn Widget)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` on the toolkit's clock at `ms` after `start`: a frame of a
    /// test's own timeline, however long the machine takes.
    fn at<T>(start: Instant, ms: u64, f: impl FnOnce() -> T) -> T {
        crate::time::testing::with_monotonic_time(start + Duration::from_millis(ms), f)
    }
    use crate::testing::Dispatch;
    use crate::widget::{AppKind, TabDescriptor, TileContent};
    use winit::event::MouseButton;

    #[test]
    fn external_navigation_omits_bar_from_layout_and_semantics_but_retains_tabs() {
        let mut group = make_group();
        assert_eq!(group.children().len(), 3);
        group.set_external_navigation(true);
        let mut tree = TaffyTree::new();
        let node = group.build_layout(&mut tree, &Theme::default(), &mut Default::default());
        assert_eq!(tree.children(node).unwrap().len(), 1);
        assert_eq!(group.children().len(), 1);
        assert_eq!(group.tabs().len(), 2);
        assert!(!group.maximized_chrome_shown());
    }
    #[test]
    fn external_windows_paint_to_their_border_without_spilling_into_neighbors() {
        use crate::offscreen::OffscreenRenderer;
        use taffy::prelude::TaffyMaxContent;
        let theme = Theme::dark();
        let mut renderer = OffscreenRenderer::new(400, 300);
        for focused in [false, true] {
            let mut group = make_group();
            group.set_external_navigation(true);
            group.set_focused(focused);
            let mut tree = TaffyTree::new();
            let node = group.build_layout(&mut tree, &theme, &mut Default::default());
            let mut style = tree.style(node).unwrap().clone();
            style.size.width = Dimension::length(200.0);
            tree.set_style(node, style).unwrap();
            let root = tree.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(200.0),
                        height: Dimension::length(200.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
            group.apply_layout(&tree, node, Point::new(100.0, 50.0));
            assert_eq!(group.rect, Rect::new(100.0, 50.0, 300.0, 250.0));
            let mut scene = Scene::new();
            group.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            let rgba = renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 255));
            for x in [96, 97, 98, 99, 300, 301, 302, 303] {
                let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 400, x, 150);
                assert_eq!(
                    (r, g, b),
                    (0, 0, 255),
                    "window spilled into neighbor at x={x}, focused={focused}"
                );
            }
            for x in [100, 299] {
                let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 400, x, 150);
                assert_ne!(
                    (r, g, b),
                    (0, 0, 255),
                    "gap before window border at x={x}, focused={focused}"
                );
            }
        }
    }

    /// The trailing-edge grip lies over the window's own content, so at
    /// rest it is not painted: an app's panels came and went under a pair
    /// of bars that looked like their scrollbar. Hovering the band or
    /// dragging shows it.
    /// The docked strip is a body of its own under the tabs, and the tabs
    /// on it lift off its ground, inset from its ends as from its top.

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sample points are small positive pixel coordinates"
    )]
    fn the_resize_grip_shows_only_on_the_edge_that_will_change() {
        use crate::offscreen::OffscreenRenderer;
        use taffy::prelude::TaffyMaxContent;
        let theme = Theme::dark();
        let mut renderer = OffscreenRenderer::new(400, 300);
        let mut render = |group: &mut WindowGroup| {
            let mut tree = TaffyTree::new();
            let node = group.build_layout(&mut tree, &theme, &mut Default::default());
            let mut style = tree.style(node).unwrap().clone();
            style.size.width = Dimension::length(200.0);
            tree.set_style(node, style).unwrap();
            let root = tree.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(200.0),
                        height: Dimension::length(240.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
            group.apply_layout(&tree, node, Point::new(100.0, 30.0));
            let mut scene = Scene::new();
            group.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 255))
        };
        let mut group = make_group();
        let idle = render(&mut group);
        let rect = group.rect;
        let (x, y) = (rect.x1 - RESIZE_HANDLE_PX / 2.0 - 2.0, rect.center().y);
        let at = |rgba: &[u8], x: f64, y: f64| {
            let (r, g, b, _) = OffscreenRenderer::sample(rgba, 400, x as u32, y as u32);
            (r, g, b)
        };
        // Nothing on the edge at the grip's height that is not also there
        // well below it.
        let below = rect.y1 - 40.0;
        assert_eq!(
            at(&idle, x, y),
            at(&idle, x, below),
            "an idle group paints a grip over its window's edge"
        );

        group.set_edge_highlight(Some(WindowEdge::Trailing));
        let hovered = render(&mut group);
        assert_ne!(
            at(&hovered, x, y),
            at(&idle, x, y),
            "the grip shows on the edge a press would resize"
        );
        group.set_edge_highlight(Some(WindowEdge::Leading));
        let leading = render(&mut group);
        assert_eq!(at(&leading, x, y), at(&idle, x, y), "and only on that edge");
        assert_ne!(
            at(&leading, rect.x0 + RESIZE_HANDLE_PX / 2.0 + 2.0, y),
            at(&idle, rect.x0 + RESIZE_HANDLE_PX / 2.0 + 2.0, y),
            "the leading edge's grip shows when that edge is the one"
        );

        group.set_edge_highlight(None);
        group.begin_edge_drag(WindowEdge::Trailing);
        let dragging = render(&mut group);
        assert_ne!(
            at(&dragging, x, y),
            at(&idle, x, y),
            "the grip shows while resizing"
        );
    }

    #[test]
    fn deferred_close_retains_host_pill_order_and_focus() {
        let mut group = make_group();
        assert!(group.request_close_tab(1));
        assert!(group.has_closing_host(1));
        assert!(group.cancel_tab_close(1));
        group.sweep_expired_closing_hosts();
        assert_eq!(
            group
                .bar
                .tabs()
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(group.active_tab_id(), 1);
        assert!(group.has_host(1));
        assert!(!group.has_closing_host(1));
        assert!(!group.bar.is_closing(1));
        assert!(
            !group.cancel_tab_close(1),
            "restoring twice must not duplicate the pill"
        );
        assert_eq!(group.tab_count(), 2);
    }

    pub(super) fn make_group() -> WindowGroup {
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "A"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "a".into(),
                },
            ),
            (
                TabDescriptor::new(2, "B"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "b".into(),
                },
            ),
        ];
        WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
    }

    #[derive(Default)]
    struct RetainedTraversalHits {
        unsaved: std::cell::Cell<u32>,
        checkpoint: std::cell::Cell<u32>,
        finish: std::cell::Cell<u32>,
        history: std::cell::Cell<u32>,
        history_mut: std::cell::Cell<u32>,
        resources_mut: std::cell::Cell<u32>,
    }

    struct RetainedTraversalProbe {
        hits: std::rc::Rc<RetainedTraversalHits>,
        pending_control: bool,
    }

    impl Widget for RetainedTraversalProbe {
        crate::provides!(
            dyn crate::history_mode::capability::FrameworkHistoryTopology,
            dyn crate::widget::controls::ActionControls,
            dyn crate::component_lifecycle::ComponentLifecycle
        );

        crate::testing::inert_widget_methods!(name = "RetainedTraversalProbe");
        fn visit_resource_owners_mut(
            &mut self,
            _: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
        ) -> Result<(), tilo_actions::DoorError> {
            self.hits
                .resources_mut
                .set(self.hits.resources_mut.get() + 1);
            Ok(())
        }
        fn has_pending_visual_work(&self) -> bool {
            self.pending_control
        }
        fn has_transient_feedback(&self) -> bool {
            self.pending_control
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            _: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(Style::default())
        }
    }

    impl crate::component_lifecycle::ComponentLifecycle for RetainedTraversalProbe {
        fn unsaved_documents(
            &self,
            _: &AutoCommit,
        ) -> Vec<crate::close_admission::UnsavedDocument> {
            self.hits.unsaved.set(self.hits.unsaved.get() + 1);
            vec![]
        }
        fn checkpoint_components(&mut self) -> Vec<String> {
            self.hits.checkpoint.set(self.hits.checkpoint.get() + 1);
            vec![]
        }
        fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            self.hits.finish.set(self.hits.finish.get() + 1);
            vec![]
        }
    }

    impl crate::widget::controls::ActionControls for RetainedTraversalProbe {
        fn has_pending_control_invocation(&self) -> bool {
            self.pending_control
        }
    }

    impl crate::history_mode::capability::FrameworkHistoryTopology for RetainedTraversalProbe {
        fn visit_framework_history(
            &self,
            _: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
        ) {
            self.hits.history.set(self.hits.history.get() + 1);
        }
        fn visit_framework_history_mut(
            &mut self,
            _: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
        ) {
            self.hits.history_mut.set(self.hits.history_mut.get() + 1);
        }
    }

    crate::testing::test_app_widget!(
        RetainedTraversalProbe,
        "Retained traversal probe",
        icon = crate::widget::app_widget::IconDescriptor::Glyph("R"),
        kind = AppKind::Settings
    );

    #[test]
    fn inactive_tabs_retain_session_collaboration_delivery() {
        let active = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "active-session",
            "fixture.active-session",
        );
        let inactive = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "inactive-session",
            "fixture.inactive-session",
        );
        let active_events = std::rc::Rc::clone(&active.collab_events);
        let inactive_events = std::rc::Rc::clone(&inactive.collab_events);
        let tabs = vec![
            (
                TabDescriptor::new(1, "Active"),
                TileContent::App(active.root),
            ),
            (
                TabDescriptor::new(2, "Inactive"),
                TileContent::App(inactive.root),
            ),
        ];
        let mut group = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

        crate::widget::handle_collab_event(
            &mut group,
            &crate::collaboration::CollabEvent::FollowChanged { peer_id: None },
        );
        assert_eq!(active_events.get(), 1);
        assert_eq!(inactive_events.get(), 1);
    }

    #[test]
    fn inactive_tabs_remain_in_lifecycle_history_and_resource_traversal() {
        let active = std::rc::Rc::new(RetainedTraversalHits::default());
        let inactive = std::rc::Rc::new(RetainedTraversalHits::default());
        let tabs = vec![
            (
                TabDescriptor::new(1, "Active"),
                TileContent::App(Box::new(RetainedTraversalProbe {
                    hits: active.clone(),
                    pending_control: false,
                })),
            ),
            (
                TabDescriptor::new(2, "Inactive"),
                TileContent::App(Box::new(RetainedTraversalProbe {
                    hits: inactive.clone(),
                    pending_control: true,
                })),
            ),
        ];
        let mut group = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
        let doc = AutoCommit::new();

        crate::component_lifecycle::unsaved_documents(&group, &doc);
        crate::component_lifecycle::checkpoint_components(&mut group);
        crate::component_lifecycle::finish_admitted_close(&mut group);
        crate::history_mode::capability::visit_framework_history(&group, &mut |_, _| {});
        crate::history_mode::capability::visit_framework_history_mut(&mut group, &mut |_, _| {});
        assert!(crate::widget::controls::has_pending_control_invocation(
            &group
        ));
        group
            .visit_resource_owners_mut(
                &mut crate::bound_dispatch::legacy_document::ResourceRequest::Discard,
            )
            .unwrap();

        assert!(
            !group.has_pending_visual_work(),
            "inactive work must not hold the active scene unsettled"
        );
        assert!(
            !group.has_transient_feedback(),
            "inactive feedback is not part of the presented scene"
        );
        group.bar.set_active(2);
        assert!(
            group.has_pending_visual_work(),
            "the same work counts when its tab becomes visible"
        );
        assert!(
            group.has_transient_feedback(),
            "the same feedback counts when its tab becomes visible"
        );

        for hits in [active, inactive] {
            assert_eq!(hits.unsaved.get(), 1);
            assert_eq!(hits.checkpoint.get(), 1);
            assert_eq!(hits.finish.get(), 1);
            assert_eq!(hits.history.get(), 1);
            assert_eq!(hits.history_mut.get(), 1);
            assert_eq!(hits.resources_mut.get(), 1);
        }
    }

    struct Starting {
        presented: bool,
        opening: bool,
    }

    impl Widget for Starting {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            _m: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn name(&self) -> &'static str {
            "Starting"
        }
        fn presented(&self) -> bool {
            self.presented
        }
        fn opening(&self) -> bool {
            self.opening
        }
    }

    impl crate::widget::app_widget::AppWidget for Starting {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Starting"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
    }

    /// An app that asks for a new layout tree every time it is asked.
    struct Restructuring {
        drained: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl Widget for Restructuring {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            _m: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn name(&self) -> &'static str {
            "Restructuring"
        }
        fn take_structure_dirty(&mut self) -> bool {
            self.drained.set(self.drained.get() + 1);
            true
        }
    }

    impl crate::widget::app_widget::AppWidget for Restructuring {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Restructuring"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("R")
        }
    }

    #[test]
    fn an_inactive_tab_does_not_rebuild_the_group_layout() {
        let drained = std::rc::Rc::new(std::cell::Cell::new(0));
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "Shown"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "shown".into(),
                },
            ),
            (
                TabDescriptor::new(2, "Behind"),
                TileContent::App(Box::new(Restructuring {
                    drained: drained.clone(),
                })),
            ),
        ];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        for frame in 0..3 {
            assert!(
                !g.take_structure_dirty(),
                "frame {frame}: a tab with no layout nodes rebuilt the group's layout"
            );
        }
        assert_eq!(drained.get(), 3, "the inactive tab's flag is still drained");

        assert!(g.set_active_tab(2));
        assert!(
            g.take_structure_dirty(),
            "selecting the tab builds its layout"
        );
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        assert!(
            g.take_structure_dirty(),
            "the active tab's own requests still reach the host"
        );
    }

    fn group_opening(title: &str, presented: bool, opening: bool) -> WindowGroup {
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
            TabDescriptor::new(1, title),
            TileContent::App(Box::new(Starting { presented, opening })),
        )];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
        g.rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        g
    }

    /// A full layout pass matters — a group with no tab rects paints
    /// nothing for unrelated reasons, which would make the "nothing on
    /// screen" case pass for free.
    fn painted_anything(g: &mut WindowGroup) -> bool {
        use taffy::prelude::TaffyMaxContent;
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        g.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );
        !scene.encoding().path_data.is_empty()
    }

    #[test]
    fn opening_card_holds_the_place_only_for_a_worthwhile_wait() {
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let scene = Scene::new();
            let _ = (&theme, &mut tr);
            assert!(
                scene.encoding().path_data.is_empty(),
                "an untouched scene must read as nothing painted, or the \
             held-back case passes for free",
            );
        }

        {
            let mut g = group_opening("Files", false, false);
            assert!(
                !painted_anything(&mut g),
                "a window frame went up for an app that has not drawn and has \
             not been waiting long enough to say so — which is a card \
             flashing on and off in front of an app that opened fast",
            );
        }

        {
            let mut g = group_opening("Files", false, true);
            assert!(
                Widget::opening(&g),
                "the group must report that it is holding a place",
            );
            assert!(
                painted_anything(&mut g),
                "the desktop stayed empty for an app the person asked for and \
             is still waiting on — the whole defect this exists to fix",
            );
        }

        // Case `the_card_takes_the_apps_name_from_its_tab`.
        // A card reading `so.tilo.file-explorer` is the failure this guards:
        // the person asked for Files, and a package id is not an answer.
        {
            let mut g = group_opening("Files", false, true);
            let _ = painted_anything(&mut g);
            let (_, host) = g.hosts.first().expect("the group has its one tab");
            assert_eq!(
                host.opening_name_for_test(),
                "Files",
                "the card must say what the tab says; anything else is two \
             names for one window",
            );
        }

        {
            let tabs: Vec<(TabDescriptor, TileContent)> = vec![
                (
                    TabDescriptor::new(1, "Files"),
                    TileContent::App(Box::new(Starting {
                        presented: true,
                        opening: false,
                    })),
                ),
                (
                    TabDescriptor::new(2, "Notes"),
                    TileContent::App(Box::new(Starting {
                        presented: false,
                        opening: true,
                    })),
                ),
            ];
            let g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
            assert!(
                Widget::presented(&g),
                "one of its tabs is a window on screen"
            );
            assert!(
                !Widget::opening(&g),
                "a group holding a window the person is using must never be \
             replaced by a card about the tab beside it",
            );
        }
    }

    /// Mirrors `ForeignSurfaceWidget`: being *asked* `presented()` is what
    /// records a window as drawn (`ForeignFrameStore::mark_presented`).
    struct Latching {
        drawn: bool,
        latched: std::rc::Rc<std::cell::Cell<bool>>,
        asked: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl Latching {
        fn new(
            drawn: bool,
        ) -> (
            Self,
            std::rc::Rc<std::cell::Cell<bool>>,
            std::rc::Rc<std::cell::Cell<u32>>,
        ) {
            let latched = std::rc::Rc::new(std::cell::Cell::new(false));
            let asked = std::rc::Rc::new(std::cell::Cell::new(0));
            (
                Self {
                    drawn,
                    latched: latched.clone(),
                    asked: asked.clone(),
                },
                latched,
                asked,
            )
        }
    }

    impl Widget for Latching {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            _m: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn name(&self) -> &'static str {
            "Latching"
        }
        fn presented(&self) -> bool {
            self.asked.set(self.asked.get().saturating_add(1));
            if self.drawn {
                self.latched.set(true);
            }
            self.latched.get()
        }
        fn opening(&self) -> bool {
            !self.latched.get()
        }
    }

    impl crate::widget::app_widget::AppWidget for Latching {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Latching"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("L")
        }
    }

    fn group_of_two(
        first_drawn: bool,
        second_drawn: bool,
    ) -> (
        WindowGroup,
        std::rc::Rc<std::cell::Cell<bool>>,
        std::rc::Rc<std::cell::Cell<u32>>,
    ) {
        let (a, _, _) = Latching::new(first_drawn);
        let (b, latched, asked) = Latching::new(second_drawn);
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "Notes"),
                TileContent::App(Box::new(a)),
            ),
            (
                TabDescriptor::new(2, "Files"),
                TileContent::App(Box::new(b)),
            ),
        ];
        (
            WindowGroup::new(10, tabs, 2, WindowWidth::Normal),
            latched,
            asked,
        )
    }

    #[test]
    fn opening_card_retires_when_any_tab_window_has_drawn() {
        {
            let (g, latched, asked) = group_of_two(true, true);

            assert!(Widget::presented(&g), "both of its tabs have drawn");
            assert!(
                asked.get() > 0,
                "the second tab was never asked whether its window had drawn, so \
             nothing recorded that it had",
            );
            assert!(
                latched.get(),
                "a window that has drawn must end up marked as having drawn",
            );
            assert!(
                !Widget::opening(&g),
                "and must not still be holding its own place",
            );
        }

        {
            let (g, _latched, _asked) = group_of_two(true, true);
            let _ = Widget::presented(&g);
            let host = g
                .hosts
                .iter()
                .find(|(id, _)| *id == 2)
                .map(|(_, h)| h)
                .expect("the group holds the tab it was built with");
            assert!(
                !Widget::opening(host),
                "the card holding this window's place must be gone: the window is \
             drawn, populated, and the one on screen",
            );
        }

        {
            let (g, latched, _asked) = group_of_two(true, false);
            let _ = Widget::presented(&g);
            assert!(
                !latched.get(),
                "asking is not answering — a window that has not drawn must not be \
             recorded as having drawn",
            );
            let host = g
                .hosts
                .iter()
                .find(|(id, _)| *id == 2)
                .map(|(_, h)| h)
                .expect("the group holds the tab it was built with");
            assert!(Widget::opening(host), "its place is still being held");
        }
    }

    /// Send `event` to `g` laid out at `node`; whether it was claimed.
    fn dispatch_at(
        g: &mut WindowGroup,
        taffy: &TaffyTree<()>,
        node: NodeId,
        event: &WindowEvent,
    ) -> bool {
        Dispatch::new().layout(taffy, node).send(g, event).handled
    }

    /// Build a `WindowGroup`, lay it out, and hover-open a tab pill's
    /// maximize chip. Returns everything a maximize-click test needs plus
    /// `moved`/`press` events aimed at the now-visible chip. Split out of
    /// `maximize_click_on_tab_pill_toggles_window_group_maximized` to keep
    /// that under the line cap.
    #[allow(
        clippy::type_complexity,
        reason = "test fixture bundling everything one maximize-click test needs; a named struct would only move the same fields, not reduce them"
    )]
    fn open_maximize_chip() -> (WindowGroup, TaffyTree<()>, NodeId, WindowEvent, WindowEvent) {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::{ElementState, MouseButton};

        let mut g = make_group();
        assert!(!g.maximized, "fresh group must start non-maximized");

        // `max_rects` must be populated by `apply_layout` before the
        // click; reduce-motion so the pill's reveal lands in one pass.
        let theme = Theme::dark().with_reduce_motion(true);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        let click_pt = g.bar.window_controls().maximize.center();

        let moved = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(click_pt.x, click_pt.y),
        };
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        (g, taffy, group_node, moved, press)
    }

    #[test]
    fn maximize_toggles_rebuild_layout_and_drain_once() {
        {
            let (mut g, taffy, group_node, moved, press) = open_maximize_chip();

            // Seed last_cursor on the icon — the bar's press arm uses the
            // cached cursor for its fresh hit-test.
            let release = WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);
            let handled = dispatch_at(&mut g, &taffy, group_node, &press);
            assert!(handled, "press on the maximize control must be claimed");
            dispatch_at(&mut g, &taffy, group_node, &release);
            assert!(
                g.maximized,
                "a click on the window's maximize control maximizes it",
            );

            dispatch_at(&mut g, &taffy, group_node, &moved);
            dispatch_at(&mut g, &taffy, group_node, &press);
            dispatch_at(&mut g, &taffy, group_node, &release);
            assert!(!g.maximized, "a second click restores it");
        }

        {
            use taffy::prelude::TaffyMaxContent;

            let mut g = make_group();
            assert!(!g.take_structure_dirty(), "fresh group is not dirty");

            // Simulate the bar publishing a maximize request, same path the
            // per-tab click hits.
            g.bar.pending_max = Some(1);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
                std::collections::HashMap::new();
            let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);

            assert!(g.maximized, "maximize drain must flip the flag");
            assert!(
                g.take_structure_dirty(),
                "maximize toggle must report structure dirty so App re-runs build_layout",
            );
            // Drained — second poll within the same frame is clean.
            assert!(!g.take_structure_dirty(), "structure_dirty drains on poll");
        }

        {
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut g = make_group();
            assert!(!g.take_structure_dirty(), "fresh group is not dirty");

            // Real build so `built_active` is stamped for the active tab.
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);
            assert!(
                !g.take_structure_dirty(),
                "a built group with no tab switch stays clean",
            );

            // Same as the click path: mutates `active_id` only.
            assert!(g.set_active_tab(2), "the second tab is a live target");
            assert!(
                g.take_structure_dirty(),
                "active-tab switch must report structure dirty so the new tile gets a node",
            );

            let mut taffy2 = TaffyTree::<()>::new();
            let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = g.build_layout(&mut taffy2, &theme, &mut node_map2);
            assert!(
                !g.take_structure_dirty(),
                "structure_dirty drains once the host rebuilds for the new active tab",
            );
        }

        {
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut g = make_group();
            assert_eq!(
                g.take_pending_maximize_change(),
                None,
                "no pending change before any click",
            );

            // Prime `pending_max` and run a no-op cursor-move through
            // `handle_event` to drive the drain block.
            g.bar.pending_max = Some(1);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);

            assert!(g.maximized, "drain must flip maximized true");
            assert_eq!(
                g.take_pending_maximize_change(),
                Some(true),
                "first poll after the click reports the new value",
            );
            assert_eq!(
                g.take_pending_maximize_change(),
                None,
                "take-pattern: drained on first poll, second poll is None",
            );

            // A second toggle reports the flipped value — the take-pattern
            // is one-shot but re-armable, so the shell sees every flip.
            g.bar.pending_max = Some(1);
            dispatch_at(&mut g, &taffy, group_node, &moved);
            assert!(!g.maximized, "second drain must flip back to false");
            assert_eq!(
                g.take_pending_maximize_change(),
                Some(false),
                "the second click reports the new (false) value once",
            );
        }
    }

    struct EdgeResizeJourney {
        row: crate::widget::GroupRow,
        tree: TaffyTree<()>,
        node: NodeId,
        pointer: Point,
    }

    impl EdgeResizeJourney {
        fn new() -> Self {
            use taffy::prelude::TaffyMaxContent;

            let mut row = crate::widget::GroupRow::new(vec![make_group()]);
            let mut tree = TaffyTree::new();
            let node = row.build_layout(
                &mut tree,
                &Theme::dark().with_reduce_motion(true),
                &mut Default::default(),
            );
            let root = tree.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
            row.apply_layout(&tree, node, Point::ZERO);
            let rect = row.groups[0].get_rect();
            assert!(rect.height() > 0.0);
            Self {
                row,
                tree,
                node,
                pointer: Point::new(rect.x1 - 3.0, rect.center().y),
            }
        }

        fn send(&mut self, event: &WindowEvent) -> bool {
            Dispatch::new()
                .layout(&self.tree, self.node)
                .send(&mut self.row, event)
                .handled
        }

        fn move_pointer(&mut self, dx: f64) {
            self.send(&WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(self.pointer.x + dx, self.pointer.y),
            });
        }

        fn mouse(&mut self, state: ElementState) {
            self.send(&WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            });
        }

        fn begin(&mut self) {
            self.move_pointer(0.0);
            self.mouse(ElementState::Pressed);
            assert!(self.row.wants_pointer_capture());
            self.move_pointer(100.0);
            assert_ne!(
                self.row.groups[0].layout_width_px(),
                self.row.groups[0].resolved_width_px(),
                "the drag sets the width live"
            );
            assert!(self.row.groups[0].take_pending_width_change().is_none());
        }

        fn assert_cancelled_and_restart(&mut self) {
            assert!(
                !self.row.wants_pointer_capture(),
                "interruption must release capture"
            );
            assert_eq!(self.row.groups[0].width(), WindowWidth::Normal);
            assert_eq!(
                self.row.groups[0].layout_width_px(),
                self.row.groups[0].resolved_width_px(),
                "and the window is back at its width"
            );
            self.move_pointer(250.0);
            self.mouse(ElementState::Released);
            assert_eq!(self.row.groups[0].width(), WindowWidth::Normal);
            assert!(self.row.groups[0].take_pending_width_change().is_none());
            self.begin();
            self.mouse(ElementState::Released);
            assert!(!self.row.wants_pointer_capture());
            assert!(self.row.groups[0].take_pending_width_change().is_some());
            assert!(self.row.groups[0].take_pending_width_change().is_none());
        }
    }

    #[test]
    fn window_edge_resize_focus_loss_cancels_preview_and_allows_a_fresh_drag() {
        let mut journey = EdgeResizeJourney::new();
        journey.begin();
        journey.send(&WindowEvent::Focused(false));
        journey.assert_cancelled_and_restart();
    }

    #[test]
    fn window_edge_resize_escape_cancels_preview_before_a_late_release() {
        let mut journey = EdgeResizeJourney::new();
        journey.begin();
        let escape = WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: crate::synthetic_input::key_event(
                Key::Named(winit::keyboard::NamedKey::Escape),
                None,
                ElementState::Pressed,
            ),
            is_synthetic: true,
        };
        assert!(journey.send(&escape));
        journey.send(&escape);
        journey.assert_cancelled_and_restart();
    }

    #[test]
    fn an_edge_drag_sets_the_width_live_and_commits_it_once() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        // (Re)build the taffy tree from the group's current state and run a
        // full layout pass, returning the group's laid-out width.
        fn layout_width(g: &mut WindowGroup, theme: &Theme) -> f64 {
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, node, Point::ZERO);
            g.get_rect().width()
        }

        {
            let theme = Theme::dark();
            let mut g = make_group();
            // 1800 px strip: one per-mille is 1.8 px, so a share alone could
            // not put the edge where the pointer is.
            g.set_strip_usable_px(1800.0);
            let _ = g.take_structure_dirty();
            assert_eq!(layout_width(&mut g, &theme), 1201.0);
            g.begin_edge_drag(WindowEdge::Trailing);
            assert!(g.edge_resize_in_flight());
            assert_eq!(g.edge_drag_origin_px(), Some(1201.0));
            assert_eq!(g.edge_highlight(), Some(WindowEdge::Trailing));
            g.set_live_width(1700.4);
            assert!(
                g.take_structure_dirty(),
                "a live width relays the window out"
            );
            assert_eq!(layout_width(&mut g, &theme), 1700.0, "whole pixels");
            assert_eq!(
                g.take_pending_width_change(),
                None,
                "mid-drag nothing is handed to the shell to persist",
            );
            let settle = g.end_edge_drag(true);
            assert_eq!(
                g.width(),
                WindowWidth::Fraction(944),
                "the drag is stored as the share it landed on (1700/1800)",
            );
            assert_eq!(
                g.take_pending_width_change(),
                Some(WindowWidth::Fraction(944))
            );
            assert_eq!(g.take_pending_width_change(), None, "drained once");
            assert_eq!(
                settle,
                g.resolved_width_px() - 1700.0,
                "the commit reports what the share's own grid moves the width by",
            );
            assert!(!g.edge_resize_in_flight());
            assert_eq!(g.edge_highlight(), None);
        }

        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            g.begin_edge_drag(WindowEdge::Leading);
            g.set_live_width(300.0);
            assert_eq!(g.layout_width_px(), 300.0);
            let _ = g.end_edge_drag(false);
            assert_eq!(g.width(), WindowWidth::Normal);
            assert_eq!(g.layout_width_px(), 800.0);
            assert_eq!(g.take_pending_width_change(), None);
        }

        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let limits = g.width_limits();
            assert_eq!(limits.min, min_group_px(1200.0));
            assert_eq!(limits.max, max_group_px(1200.0));
        }

        {
            let theme = Theme::dark();
            let mut g = make_group();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, node, Point::ZERO);
            let rect = g.get_rect();
            let cy = rect.center().y;
            for x in [rect.x1 - 3.0, rect.x0 + 3.0] {
                assert_eq!(
                    Widget::cursor_shape(&g, Point::new(x, cy)),
                    Some(winit::window::CursorIcon::EwResize),
                    "an edge band requests an east-west resize cursor (x = {x})",
                );
            }
            assert_eq!(
                Widget::cursor_shape(&g, Point::new(rect.center().x, cy)),
                None,
                "pointer over the group body (not the edge band) defers",
            );
            // An in-flight drag holds the cursor even off the band.
            g.begin_edge_drag(WindowEdge::Trailing);
            assert_eq!(
                Widget::cursor_shape(&g, Point::new(rect.center().x, cy)),
                Some(winit::window::CursorIcon::EwResize),
            );
        }

        {
            // A width a person set is a share, kept on every display (the
            // default width stops growing past what reads well instead).
            let mut wide = make_group();
            wide.set_width(WindowWidth::Fraction(667));
            wide.set_strip_usable_px(3840.0);
            assert_eq!(
                wide.resolved_width_px(),
                2561.0,
                "667/1000 of a 3840 px strip — the same share a 1200 px strip \
             gives at 800 px, to the 1 px that per-mille quantisation costs \
             at this size",
            );
            let mut default = make_group();
            default.set_strip_usable_px(3840.0);
            assert_eq!(default.width(), g_normal_width());
            assert_eq!(default.resolved_width_px(), 1280.0);
        }
    }

    #[test]
    fn tab_bar_requests_reorder_search_and_pin_keep_hosts_aligned() {
        {
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let theme = Theme::dark();
            let mut g = make_group();
            assert_eq!(
                g.take_pending_tab_reorder(),
                None,
                "nothing pending initially"
            );

            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();

            // Publish a reorder the same way the bar does on a drag release, then
            // pump the group's drain block via a harmless event.
            g.bar_mut_for_tests().pending_reorder = Some((0, 1));
            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
            };
            dispatch_at(&mut g, &taffy, node, &moved);

            assert_eq!(
                g.take_pending_tab_reorder(),
                Some((0, 1)),
                "a harvested reorder publishes (from, to) for the shell exactly once",
            );
            assert_eq!(
                g.take_pending_tab_reorder(),
                None,
                "take-pattern: drained on the first poll",
            );
        }

        // Case `reorder_keeps_hosts_and_bar_tabs_aligned`.
        // Parallel-vec invariant: drift between `bar.tabs()` and `hosts`
        // would render tab pill `i`'s content from a different host.
        {
            use crate::widget::{AppKind, TabDescriptor, TileContent};
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            // Helper: build a fresh 4-tab WindowGroup so we have enough
            // pills to exercise non-trivial permutations.
            fn make_group_4() -> WindowGroup {
                let tabs: Vec<(TabDescriptor, TileContent)> = (1..=4u64)
                    .map(|i| {
                        (
                            TabDescriptor::new(i, format!("T{i}")),
                            TileContent::Stub {
                                kind: AppKind::Settings,
                                label: format!("s{i}"),
                            },
                        )
                    })
                    .collect();
                WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
            }

            // (from, to) cases:
            //  - forward small step (0 → 2)
            //  - backward step (3 → 1)
            //  - adjacent swap (1 → 2)
            //  - no-op (2 → 2)
            //  - extreme (0 → 3)
            let cases: &[(usize, usize)] = &[(0, 2), (3, 1), (1, 2), (2, 2), (0, 3)];

            for &(from, to) in cases {
                let mut g = make_group_4();

                // `handle_event` walks the cached taffy children, so a real
                // layout pass is needed before the drain block runs.
                let theme = Theme::dark();
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy.parent_node(Style::default(), &[group_node]);
                taffy
                    .compute_layout(root, taffy::Size::MAX_CONTENT)
                    .unwrap();
                g.apply_layout(&taffy, group_node, Point::ZERO);

                for (i, t) in g.bar.tabs().iter().enumerate() {
                    assert_eq!(
                        t.id, g.hosts[i].0,
                        "precondition: tab[{i}] id must equal hosts[{i}].0",
                    );
                }

                // Same as `TabBar::handle_event` on a drag release.
                g.bar.pending_reorder = Some((from, to));

                // Drive the WindowGroup drain block via a no-op cursor move.
                let moved = WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
                };
                dispatch_at(&mut g, &taffy, group_node, &moved);

                assert_eq!(
                    g.bar.tabs().len(),
                    g.hosts.len(),
                    "case ({from}, {to}): bar.tabs().len() must equal hosts.len() after reorder",
                );
                for (i, t) in g.bar.tabs().iter().enumerate() {
                    assert_eq!(
                        t.id, g.hosts[i].0,
                        "case ({from}, {to}): bar.tabs()[{i}].id must equal hosts[{i}].0 after reorder",
                    );
                }
            }
        }

        {
            // Click-to-toggle: seeding `pending_search` and re-draining flips
            // the flag; doing it again flips it back.
            let mut g = make_group();
            assert!(!g.search_active, "fresh group must not be searching");
            g.bar.pending_search = Some(1);
            if let Some(_id) = g.bar.take_search() {
                g.search_active = !g.search_active;
            }
            assert!(g.search_active, "drain should flip search_active on");
            g.bar.pending_search = Some(1);
            if let Some(_id) = g.bar.take_search() {
                g.search_active = !g.search_active;
            }
            assert!(
                !g.search_active,
                "second drain should flip search_active off"
            );
        }

        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;
            use winit::event::Modifiers;
            use winit::keyboard::{Key, ModifiersState, SmolStr};

            fn key_press(logical_key: Key) -> WindowEvent {
                crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
                    logical_key,
                    None,
                    ElementState::Pressed,
                ))
            }

            let mut g = make_group();
            let active = g.bar.active_id();
            assert!(
                !g.bar.is_pinned(active),
                "fresh group: active tab is unpinned"
            );

            let mods = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
            let event = key_press(Key::Character(SmolStr::new_inline("p")));

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            let mut ctrl_shift = Dispatch::new().layout(&taffy, group_node).modifiers(mods);
            let handled = ctrl_shift.send(&mut g, &event).handled;
            assert!(handled, "Ctrl+Shift+P press must be claimed");
            assert!(
                g.bar.is_pinned(active),
                "first Ctrl+Shift+P must pin the active tab",
            );

            let handled = ctrl_shift.send(&mut g, &event).handled;
            assert!(handled, "second Ctrl+Shift+P press is also claimed");
            assert!(
                !g.bar.is_pinned(active),
                "second Ctrl+Shift+P must unpin the active tab",
            );

            dispatch_at(&mut g, &taffy, group_node, &event);
            assert!(
                !g.bar.is_pinned(active),
                "plain `P` (no Ctrl+Shift) must not toggle pin state",
            );
        }
    }

    /// The tier `make_group()` seeds. Named so the cross-display assertion
    /// above reads against the fixture rather than restating a literal.
    fn g_normal_width() -> WindowWidth {
        WindowWidth::Normal
    }

    #[test]
    fn maximize_restore_springs_the_drawn_rect_from_the_tiled_slot() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        fn layout(g: &mut WindowGroup, theme: &Theme, w: f32, h: f32) -> Rect {
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, theme, &mut node_map);
            let root = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, node, Point::ZERO);
            g.get_rect()
        }

        {
            let theme = Theme::dark();
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let t0 = crate::time::now();
            let tiled = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
            assert!(
                tiled.width() > 100.0 && tiled.width() < 1600.0,
                "tiled slot must be narrower than the work area, got {}",
                tiled.width(),
            );
            at(t0, 0, || g.toggle_maximized());
            assert!(
                g.needs_animation_frame(),
                "a maximize spring must pump frames",
            );
            let dest = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
            assert!(
                dest.width() > tiled.width() + 50.0,
                "layout must jump to the work area, tiled {} dest {}",
                tiled.width(),
                dest.width(),
            );
            let drawn = g.drawn_rect();
            assert!(
                (drawn.width() - tiled.width()).abs() < 1.0,
                "the first animated frame must still be the tiled rect, got {} want {}",
                drawn.width(),
                tiled.width(),
            );
            at(t0, 0, || g.step_rect_spring_for_tests());
            assert!(
                (g.drawn_rect().width() - tiled.width()).abs() < 1.0,
                "the arming frame does not move, so there is no jump at the start",
            );
            at(t0, 16, || g.step_rect_spring_for_tests());
            let mid = g.drawn_rect().width();
            assert!(
                mid > tiled.width() + 1.0,
                "the drawn width must grow toward the work area, {mid} from tiled {}",
                tiled.width(),
            );
        }

        {
            let theme = Theme::dark();
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let t0 = crate::time::now();
            let _ = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
            at(t0, 0, || g.toggle_maximized());
            let _ = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
            at(t0, 0, || g.step_rect_spring_for_tests());
            at(t0, 48, || g.step_rect_spring_for_tests());
            let vel_before = g
                .rect_spring_width_velocity()
                .expect("maximize spring in flight");
            assert!(
                vel_before > 0.0,
                "opening maximize must grow, vel={vel_before}"
            );
            let mid = g.drawn_rect();
            at(t0, 48, || g.toggle_maximized());
            let _ = at(t0, 48, || layout(&mut g, &theme, 1600.0, 900.0));
            let vel_after = g
                .rect_spring_width_velocity()
                .expect("restore retargets the same spring");
            assert_eq!(
                vel_after, vel_before,
                "a new Super+M must keep velocity so the motion is interruptible",
            );
            assert!(
                (g.drawn_rect().width() - mid.width()).abs() < 0.5,
                "retargeting must not jump the drawn rect",
            );
        }

        {
            let theme = Theme::dark();
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let _ = layout(&mut g, &theme, 1600.0, 900.0);
            g.reduce_motion = true;
            g.toggle_maximized();
            let dest = layout(&mut g, &theme, 1600.0, 900.0);
            assert!(
                g.rect_spring.is_none(),
                "reduced motion must not leave a spring ticking",
            );
            assert_eq!(
                g.drawn_rect(),
                dest,
                "reduced motion paints at the work-area rect immediately",
            );
            assert!(
                !g.needs_animation_frame(),
                "a snapped maximize must not pump frames",
            );
        }
    }

    #[test]
    fn width_spring_opens_closes_and_resizes_from_the_current_value() {
        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let target = g.resolved_width_px();
            assert!(target > 100.0, "fixture must have a real slot to grow into");
            let t0 = crate::time::now();
            at(t0, 0, || g.start_spawn_animation());
            assert!(
                g.layout_width_px() < 1.0,
                "an opening group reserves no width until the spring moves, got {}",
                g.layout_width_px(),
            );
            assert!(
                g.needs_animation_frame(),
                "an in-flight width spring must pump frames",
            );
            let mut last = g.layout_width_px();
            for frame in 1..=8 {
                // Frames 16 ms apart on the spring's clock.
                at(t0, 16 * frame, || {
                    g.next_frame();
                    g.step_width_spring();
                });
                let now = g.layout_width_px();
                assert!(now >= last, "open must grow the slot, {last} → {now}");
                last = now;
            }
            assert!(
                last > 1.0 && last < target,
                "after a few frames the slot is open but not yet at the target ({last} of {target})",
            );
        }

        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let t0 = crate::time::now();
            at(t0, 0, || g.start_spawn_animation());
            for frame in 1..=10 {
                at(t0, 16 * frame, || {
                    g.next_frame();
                    g.step_width_spring();
                });
            }
            let mid = g.layout_width_px();
            let vel_before = g
                .width_spring
                .as_ref()
                .map(FrameSpring::velocity)
                .expect("open spring still in flight");
            assert!(
                vel_before > 0.0,
                "opening must still be moving outward, vel={vel_before}",
            );
            at(t0, 160, || g.set_width(WindowWidth::Narrow));
            let vel_after = g
                .width_spring
                .as_ref()
                .map(FrameSpring::velocity)
                .expect("resize retargets the same spring");
            assert_eq!(
                vel_after, vel_before,
                "set_target must keep the current velocity so a new input is interruptible",
            );
            assert!(
                (g.layout_width_px() - mid).abs() < 0.5,
                "retargeting must not jump the slot, was {mid} now {}",
                g.layout_width_px(),
            );
            assert!(
                g.width_spring.as_ref().is_some_and(|s| s.target() < mid),
                "Narrow on a 1200 px strip is smaller than the mid-open width",
            );
        }

        {
            // The row steps the slot spring only while it rebuilds the
            // layout, and rebuilds only when a group asks for it. Frames at
            // 60 and at 20 a second must both end the motion.
            for frame_ms in [16, 50] {
                let mut g = group_opening("Files", true, false);
                g.set_strip_usable_px(1200.0);
                let t0 = crate::time::now();
                at(t0, 0, || g.start_spawn_animation());
                let mut rebuilds = 0;
                while std::mem::take(&mut g.style_dirty) && rebuilds < 200 {
                    rebuilds += 1;
                    at(t0, frame_ms * rebuilds, || {
                        g.next_frame();
                        g.step_width_spring();
                    });
                }
                assert!(
                    g.width_spring.is_none(),
                    "{frame_ms} ms frames: the slot spring stalled at {:?} after {rebuilds} rebuilds",
                    g.width_spring
                        .as_ref()
                        .map(|s| (s.value(), s.target(), s.velocity())),
                );
                assert!(
                    (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
                    "{frame_ms} ms frames: the slot ends at its width",
                );
            }
        }

        {
            // The host's frame: rebuild when a widget asks (the row steps
            // the spring in its build, a frame may take two passes), then a
            // painted frame turns the token. A tick that rebuilds while the
            // presenter is still busy paints nothing; the step the next
            // rebuild skips must not leave the slot stopped part-way, asking
            // for frames forever.
            let mut g = group_opening("Notes", true, false);
            g.set_strip_usable_px(1200.0);
            let t0 = crate::time::now();
            at(t0, 0, || g.start_spawn_animation());
            let mut frames = 0;
            while g.width_spring.is_some() && frames < 400 {
                at(t0, 16 * (frames + 1), || {
                    if Widget::take_structure_dirty(&mut g) {
                        g.step_width_spring();
                    }
                    if frames % 7 != 3 {
                        g.frame_presented();
                    }
                });
                frames += 1;
            }
            assert!(
                g.width_spring.is_none(),
                "the slot spring froze at {:?} after {frames} frames",
                g.width_spring
                    .as_ref()
                    .map(|s| (s.value(), s.target(), s.velocity())),
            );
            assert!(
                (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
                "the slot ends at its width"
            );
        }

        {
            let mut g = group_opening("Files", false, false);
            g.set_strip_usable_px(1200.0);
            assert!(
                !painted_anything(&mut g),
                "precondition: an unpresented group with no opening card paints nothing",
            );
            let from = g.layout_width_px();
            let t0 = crate::time::now();
            at(t0, 0, || g.start_closing());
            assert!(
                (g.layout_width_px() - from).abs() < 0.5,
                "close must start from the last reserved width, not jump to zero",
            );
            assert!(
                painted_anything(&mut g),
                "a closing group is drawn from its last frame while the slot collapses",
            );
            let mut last = g.layout_width_px();
            for frame in 1..=8 {
                // Frames 16 ms apart on the spring's clock.
                at(t0, 16 * frame, || {
                    g.next_frame();
                    g.step_width_spring();
                });
                let now = g.layout_width_px();
                assert!(now <= last, "close must collapse the slot, {last} → {now}");
                last = now;
            }
            assert!(
                last < from,
                "the slot must have started to collapse ({last} from {from})",
            );
        }

        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            g.set_reduce_motion(true);
            g.start_spawn_animation();
            assert!(
                (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
                "reduced motion opens at the target width, no spring frames",
            );
            assert!(
                g.width_spring.is_none(),
                "reduced motion must not leave a spring ticking",
            );
            g.set_reduce_motion(false);
            g.rect = Rect::new(0.0, 0.0, g.resolved_width_px(), 400.0);
            g.set_width(WindowWidth::Narrow);
            assert!(
                g.width_spring.is_some(),
                "a live resize starts a spring when motion is allowed",
            );
            g.set_reduce_motion(true);
            g.next_frame();
            g.step_width_spring();
            assert!(
                (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
                "reduced motion snaps a running resize to the target",
            );
        }
    }

    /// A host that lays out at ~12 fps (a headless session rendering in
    /// software) steps the opening slot 50 ms at a time. The slot must
    /// still settle at its width; it used to grow until the client was
    /// configured `i32::MAX` wide.
    #[test]
    fn a_slow_host_opens_the_slot_to_its_width() {
        let mut g = make_group();
        g.set_strip_usable_px(996.0);
        let target = g.resolved_width_px();
        let t0 = crate::time::now();
        at(t0, 0, || g.start_spawn_animation());
        for frame in 0..40 {
            at(t0, 80 * (frame + 1), || {
                g.next_frame();
                g.step_width_spring();
            });
            let width = g.layout_width_px();
            assert!(
                width <= target * 1.05,
                "frame {frame}: slot at {width} past its {target} px width",
            );
        }
        assert!(
            g.width_spring.is_none() && (g.layout_width_px() - target).abs() < 0.5,
            "the slot settles at {target}, got {}",
            g.layout_width_px(),
        );
    }

    #[test]
    fn refused_final_tab_close_restores_live_group_input() {
        let mut group = WindowGroup::new(
            10,
            vec![(
                TabDescriptor::new(1, "solo"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "a".into(),
                },
            )],
            1,
            WindowWidth::Normal,
        );
        assert!(group.request_close_tab(1));
        assert!(group.is_closing());
        assert_eq!(group.keyboard_input_scope(), None);
        assert!(group.cancel_tab_close(1));
        assert!(!group.is_closing());
        assert!(!group.has_closing_host(1));
        assert_eq!(group.keyboard_input_scope(), Some(1));
        assert!(group.has_host(1));
    }

    #[test]
    fn closing_tab_hosts_fade_out_and_the_last_one_closes_the_group() {
        // One instant for the whole test: each case backdates its own
        // timestamps, so a descheduled runner cannot move them.
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            // Case `detach_tab_clears_closing_state_for_moved_tab`.
            // Its id sits in `closing_hosts` — a stale entry whose timer never
            // expires once the host is moved — so `needs_animation_frame` would
            // otherwise keep pumping redraws for a host that's gone.
            {
                let mut g = make_group();
                // Seed `closing_hosts` directly with tab id 1 to simulate the
                // leak: still present in `bar.tabs` and `hosts`, but the
                // side-table erroneously carries a fade timestamp.
                g.closing_hosts.insert(1, crate::time::now());
                assert!(
                    g.closing_hosts.contains_key(&1),
                    "precondition: closing_hosts carries the moved tab id",
                );

                let detached = g.detach_tab(1);
                assert!(detached.is_some(), "detach_tab returns the moved tab");

                assert!(
                    !g.closing_hosts.contains_key(&1),
                    "detach_tab must clear closing_hosts for the moved tab so \
                 needs_animation_frame stops pumping for a host that's gone",
                );
                assert!(
                    !g.bar.closing_tab_ids().contains(&1),
                    "detach_tab must call bar.finalize_close so any pill-side \
                 fade state for the moved id is also dropped",
                );
            }

            {
                use crate::theme::Theme;
                use std::collections::HashMap;
                use taffy::Style;
                use taffy::prelude::TaffyMaxContent;

                let mut g = make_group();
                // Layout pass so the bar has authoritative pill/close rects —
                // `start_closing` snapshots those into `closing_tabs`.
                let theme = Theme::dark();
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy.parent_node(Style::default(), &[group_node]);
                taffy
                    .compute_layout(root, taffy::Size::MAX_CONTENT)
                    .unwrap();
                g.apply_layout(&taffy, group_node, Point::ZERO);

                // Latch via the bar API directly rather than simulating a full
                // press/click sequence.
                g.bar_mut().start_closing(2);

                // CursorMoved off-rect so no hover handlers fire and the event
                // reaches the close-drain block at the bottom of `handle_event`.
                let moved = WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
                };
                dispatch_at(&mut g, &taffy, group_node, &moved);

                assert!(
                    g.has_closing_host(2),
                    "drain must latch the host-level close fade",
                );
                assert!(
                    g.needs_animation_frame(),
                    "in-flight close fade must pump redraws",
                );
                assert!(g.has_host(2), "host stays in `hosts` for the fade duration");
                let host_count_during = g.host_count();

                g.force_expire_closing(2);
                let moved2 = WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
                };
                dispatch_at(&mut g, &taffy, group_node, &moved2);
                assert!(
                    !g.has_host(2),
                    "sweep drops the host after the fade expires",
                );
                assert_eq!(
                    g.host_count(),
                    host_count_during - 1,
                    "exactly one host removed",
                );
                assert!(
                    !g.has_closing_host(2),
                    "sweep clears the closing_hosts entry",
                );
            }

            // Case `closing_last_tab_kicks_off_group_close`.
            // `GroupRow` relies on `is_closing()` → `close_complete()` to
            // sequence the prune, so if `closing_at` never flips the group
            // stays around as an empty shell forever.
            {
                use crate::theme::Theme;
                use std::collections::HashMap;
                use taffy::Style;
                use taffy::prelude::TaffyMaxContent;

                // Single-tab group so "close this tab" truly empties the group.
                let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
                    TabDescriptor::new(1, "solo"),
                    TileContent::Stub {
                        kind: AppKind::Settings,
                        label: "a".into(),
                    },
                )];
                let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

                // Full layout pass so the bar has authoritative pill rects.
                let theme = Theme::dark();
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy.parent_node(Style::default(), &[group_node]);
                taffy
                    .compute_layout(root, taffy::Size::MAX_CONTENT)
                    .unwrap();
                g.apply_layout(&taffy, group_node, Point::ZERO);

                g.bar_mut().start_closing(1);

                // First handle_event drains `pending_close` into `closing_hosts`;
                // the group starts collapsing in the same interval as its final host.
                let moved = WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
                };
                dispatch_at(&mut g, &taffy, group_node, &moved);
                assert!(
                    g.has_closing_host(1),
                    "precondition: host-level close latched",
                );
                assert!(
                    g.is_closing(),
                    "the last tab and group close together, before canonical reconciliation",
                );

                // A later event must not restart or prolong the already-owned group close.
                g.force_expire_closing(1);
                let moved2 = WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
                };
                dispatch_at(&mut g, &taffy, group_node, &moved2);
                assert!(
                    g.is_closing(),
                    "the group close remains latched without a second fade",
                );
            }
        });
    }

    #[test]
    fn group_close_fade_paints_pumps_frames_and_completes() {
        // One instant for the whole test: each case backdates its own
        // timestamps, so a descheduled runner cannot move them.
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            {
                let mut g = make_group();
                g.set_closing_at_for_tests(
                    crate::time::now()
                        .checked_sub(g.close_duration.saturating_add(Duration::from_millis(20)))
                        .expect("representable close deadline"),
                );
                assert!(
                    g.close_complete(),
                    "close_complete must flip true after the fade window elapses",
                );
                g.set_closing_at_for_tests(crate::time::now());
                assert!(
                    !g.close_complete(),
                    "a just-latched close must not report complete",
                );
            }

            // Case `group_still_paints_during_close_fade`.
            // Guards against a panic or a collapsed rect mid-fade.
            {
                use crate::theme::Theme;
                use std::collections::HashMap;
                use taffy::Style;
                use taffy::prelude::TaffyMaxContent;

                let mut g = make_group();
                let theme = Theme::dark();
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy.parent_node(Style::default(), &[group_node]);
                taffy
                    .compute_layout(root, taffy::Size::MAX_CONTENT)
                    .unwrap();
                g.apply_layout(&taffy, group_node, Point::ZERO);
                let pre_rect = g.get_rect();

                g.start_closing();

                // The fade scales about the centre via an Affine; the logical
                // `self.rect` used for hit-tests stays put so drop-targets and
                // focus bookkeeping don't shift under the user mid-animation.
                let mut scene = Scene::new();
                let presence = PresenceCtx::default();
                let mut tr = TextRenderer::new();
                g.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

                assert_eq!(
                    g.get_rect(),
                    pre_rect,
                    "close fade paints over the existing rect; hit-tests must stay stable",
                );
                assert!(
                    g.is_closing(),
                    "close flag stays latched through the draw pass",
                );
            }

            {
                let mut g = make_group();
                assert!(
                    !g.needs_animation_frame(),
                    "precondition: idle group is quiet"
                );
                g.start_closing();
                assert!(
                    g.needs_animation_frame(),
                    "group-level close fade must pump frames until close_complete",
                );
                // Later, once the slot has shut too.
                let later = crate::time::now() + Duration::from_secs(2);
                crate::time::testing::with_monotonic_time(later, || {
                    g.step_width_spring();
                    g.set_closing_at_for_tests(
                        crate::time::now()
                            .checked_sub(g.close_duration.saturating_add(Duration::from_millis(20)))
                            .expect("representable close deadline"),
                    );
                    assert!(
                        !g.needs_animation_frame(),
                        "after the fade window elapses, frames are no longer required",
                    );
                });
            }

            {
                let mut g = make_group();
                assert!(
                    !g.needs_animation_frame(),
                    "precondition: idle group doesn't demand frames",
                );
                // Seed `closing_hosts` directly to simulate what `handle_event`
                // does after draining a `pending_close`, bypassing the full event
                // drive.
                g.closing_hosts.insert(1, crate::time::now());
                assert!(
                    g.needs_animation_frame(),
                    "a fresh closing host must pump the run loop",
                );
                g.closing_hosts.insert(
                    1,
                    crate::time::now()
                        .checked_sub(group_close_fade() + Duration::from_millis(20))
                        .unwrap(),
                );
                assert!(
                    !g.needs_animation_frame(),
                    "after the fade window elapses the group goes idle again",
                );
            }
        });
    }
}

/// The maximized window's bar: hidden at rest over a full-height client, and
/// one bar with a Tilo app's header row.
#[path = "window_group_fullscreen_bar.rs"]
mod fullscreen_bar;

#[cfg(test)]
#[path = "window_group_window_bar_tests.rs"]
mod window_bar_tests;

#[cfg(test)]
#[path = "window_group_search_tests.rs"]
mod search_tests;

#[cfg(test)]
#[path = "window_group_motion_tests.rs"]
mod motion_tests;
