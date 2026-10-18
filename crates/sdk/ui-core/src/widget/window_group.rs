use crate::animation::Spring;
use crate::icons;
use crate::num::f32_of_f64;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::SpringPresets;
use crate::widget::TaffyTreeExt;
use crate::widget::edge_resize::{WidthLimits, WindowEdge};
use crate::widget::tabs::CLOSE_FADE_MS;
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

const SPAWN_ANIM_MS: u64 = 420;
/// Kept inside the row's visible margin during the spawn animation.
const SPAWN_ANIM_X_OFFSET: f64 = -16.0;
const SPAWN_ANIM_INITIAL_SCALE: f64 = 0.92;
/// Matches the tab-pill `CLOSE_FADE_MS` in `tabs.rs` so the group's
/// shrink chains onto the final pill fade.
const GROUP_CLOSE_FADE_MS: u64 = 180;
/// The lifted handle remains readable while exposing the drop target; the app
/// pixels beneath it recede further so the target never disappears.
const DRAG_CONTENT_ALPHA: f32 = 0.42;
const DRAG_SOURCE_CONTENT_ALPHA: f32 = 0.18;
/// End scale of the fast lift curve. The content tucks under the full-size tab
/// handle instead of turning into a second opaque window under the pointer.
const DRAG_CONTENT_SCALE_END: f64 = 0.96;

/// [`SPAWN_ANIM_MS`] in seconds, as an `f64` divisor for the eased-progress
/// calculation.
#[allow(
    clippy::cast_precision_loss,
    reason = "SPAWN_ANIM_MS is a small compile-time millisecond constant (420), far below f64's precision limit"
)]
fn spawn_anim_secs() -> f64 {
    SPAWN_ANIM_MS as f64 / 1000.0
}

/// Test-only helper: an [`Instant`] `ms` milliseconds in the past, for
/// backdating a timestamp instead of sleeping. Falls back to `crate::time::now`
/// on the practically-unreachable case that the subtraction would underflow
/// the clock's epoch.
#[cfg(test)]
fn instant_ago_ms(ms: u64) -> Instant {
    crate::time::now()
        .checked_sub(Duration::from_millis(ms))
        .unwrap_or_else(crate::time::now)
}

/// One critically-damped heavy spring per rect edge. A new target mid-flight
/// keeps value and velocity, so Super+M twice is one motion, not a queue.
struct RectSprings {
    x: Spring,
    y: Spring,
    w: Spring,
    h: Spring,
}

impl RectSprings {
    fn at(rect: Rect) -> Self {
        let params = SpringPresets::default().heavy;
        Self {
            x: Spring::new(f32_of_f64(rect.x0), params),
            y: Spring::new(f32_of_f64(rect.y0), params),
            w: Spring::new(f32_of_f64(rect.width()), params),
            h: Spring::new(f32_of_f64(rect.height()), params),
        }
    }

    fn set_target(&mut self, rect: Rect) {
        self.x.set_target(f32_of_f64(rect.x0));
        self.y.set_target(f32_of_f64(rect.y0));
        self.w.set_target(f32_of_f64(rect.width().max(0.0)));
        self.h.set_target(f32_of_f64(rect.height().max(0.0)));
    }

    fn snap(&mut self, rect: Rect) {
        self.x.snap(f32_of_f64(rect.x0));
        self.y.snap(f32_of_f64(rect.y0));
        self.w.snap(f32_of_f64(rect.width().max(0.0)));
        self.h.snap(f32_of_f64(rect.height().max(0.0)));
    }

    fn step(&mut self, dt: f32) {
        self.x.step(dt);
        self.y.step(dt);
        self.w.step(dt);
        self.h.step(dt);
    }

    fn is_settled(&self) -> bool {
        self.x.is_settled() && self.y.is_settled() && self.w.is_settled() && self.h.is_settled()
    }

    fn rect(&self) -> Rect {
        let x = f64::from(self.x.value());
        let y = f64::from(self.y.value());
        Rect::new(
            x,
            y,
            x + f64::from(self.w.value()).max(0.0),
            y + f64::from(self.h.value()).max(0.0),
        )
    }

    #[cfg(test)]
    fn velocity_w(&self) -> f32 {
        self.w.velocity()
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
        for (_, host) in &mut self.hosts {
            host.offset_layout(dx);
        }
    }

    /// Where the group draws this frame: its layout rect, or the spring's
    /// while it moves there.
    pub(crate) fn drawn_rect(&self) -> Rect {
        self.rect_spring
            .as_ref()
            .map_or(self.rect, RectSprings::rect)
    }

    #[cfg(test)]
    pub(crate) fn step_rect_spring_for_tests(&mut self) {
        self.step_rect_spring();
    }

    #[cfg(test)]
    pub(crate) fn rect_spring_width_velocity(&self) -> Option<f32> {
        self.rect_spring.as_ref().map(RectSprings::velocity_w)
    }
}

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
    close_reports: Vec<crate::application::ComponentCloseReport>,
    pub id: u64,
    bar: TabBar,
    hosts: Vec<(u64, TileHost)>,
    width: WindowWidth,
    /// Pushed down by the parent [`crate::widget::GroupRow`] before every
    /// `build_layout`; seeded to [`DEFAULT_STRIP_USABLE_PX`].
    strip_usable_px: f64,
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
    /// The bar's fade, latched from the theme at draw.
    chrome_fade: Duration,
    /// Height of what overlays the window's top edge now (the desktop's top
    /// bar shown over a window under an auto-hiding top bar): the window's
    /// bar stands below it.
    top_overlay_inset: f64,
    /// Nothing of the desktop's stands beside a maximized window on its
    /// display (the top bar is not docked above it): maximized, it fills the
    /// display and draws edge to edge ([`Widget::set_full_bleed`]).
    fills_display: bool,
    last_cursor: Point,
    spawned_at: Option<Instant>,
    /// Drawn rect in flight between the tiled slot and the work area.
    /// Layout jumps to the destination; paint interpolates (no Taffy).
    rect_spring: Option<RectSprings>,
    last_rect_step: Option<Instant>,
    /// [`Self::draw_docked_strip`] stepped the rect spring for the frame
    /// whose `draw` comes next.
    spring_stepped: bool,
    /// Slot width in flight. Layout reads this so an open grows from 0,
    /// a close collapses the slot, and a resize retargets without a jump.
    width_spring: Option<Spring>,
    last_width_step: Option<Instant>,
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
    pending_width_change: Option<u32>,
    pending_search_open: Option<u64>,
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
/// Narrowest the `[app title]` chip is drawn, so a one-letter title still
/// reads as a chip rather than a dot.
const MAXIMIZED_TITLE_CHIP_MIN_W: f64 = 80.0;
/// Narrowest the `[Restore]` chip is drawn; a longer label widens it.
const MAXIMIZED_RESTORE_CHIP_W: f64 = 76.0;
/// Side of the lucide glyph slot inside the maximized chips — the same
/// 14 px the tab pills' app glyphs and action chips use.
const MAXIMIZED_ICON_PX: f64 = 14.0;
/// The per-group right-edge resize grip: two bars, each this wide, filling
/// the `RESIZE_HANDLE_PX` band between them.
const GRIP_BAR_PX: f64 = 2.0;
/// The grip runs a quarter of the group's height, clamped to
/// `spacing.s6 ..= spacing.s16 + spacing.s4`.
const GRIP_HEIGHT_SHARE: f64 = 0.25;

/// Shared layout numbers for the maximized-chrome pill row, computed once in
/// [`WindowGroup::draw_maximized_chrome`] and threaded through its two child
/// painters — keeps both well under clippy's argument-count limit.
#[derive(Clone, Copy)]
struct ChromePillLayout {
    pad: f64,
    pill_h: f64,
    y0: f64,
}

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
            Self::paint_strip_body(&self.bar, scene, (transform, Affine::IDENTITY), theme);
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
        bar.set_on_strip(true);
        Self {
            id,
            close_reports: vec![],
            bar,
            hosts,
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            rect: Rect::ZERO,
            external_navigation: false,
            fullscreen_presentation: false,
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
            chrome_fade: Duration::ZERO,
            top_overlay_inset: 0.0,
            fills_display: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            rect_spring: None,
            last_rect_step: None,
            spring_stepped: false,
            width_spring: None,
            last_width_step: None,
            held_back: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            close_requested: false,
            width_stepped: false,
            width_step_skipped: false,
            close_rebuild_taken: false,
            close_duration: crate::tokens::motion::Durations::default().base(),
            reduce_motion: false,
            motion_source: crate::tokens::MotionSource::default(),
            style_dirty: false,
            built_active: Some(active_id),
            pending_maximize_change: None,
            pending_width_change: None,
            pending_tab_reorder: None,
            pending_search_open: None,
            pending_new_tab: None,
            pending_close_commit: Vec::new(),
        }
    }
    pub fn width(&self) -> WindowWidth {
        self.width
    }

    fn navigation_bar_visible(&self) -> bool {
        !self.external_navigation && !self.fullscreen_presentation
    }

    pub(crate) fn set_fullscreen_presentation(&mut self, fullscreen: bool) {
        if self.fullscreen_presentation != fullscreen {
            self.fullscreen_presentation = fullscreen;
            self.style_dirty = true;
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
        true
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(crate::time::now());
        self.arm_width_from_zero();
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
                self.last_rect_step = None;
                self.style_dirty = true;
            }
            if !self.is_closing() {
                self.spawned_at = None;
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
            return f64::from(spring.value()).max(0.0);
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
            return f64::from(spring.target()).max(0.0);
        }
        self.layout_width_px()
    }

    /// True when the spring is still aimed at a previous usable basis.
    pub(crate) fn width_target_is_stale(&self) -> bool {
        self.width_spring.is_some() && (self.dest_width_px() - self.resolved_width_px()).abs() > 1.0
    }

    fn width_params() -> crate::tokens::SpringParams {
        SpringPresets::default().heavy
    }

    fn arm_width_from_zero(&mut self) {
        let target = f32_of_f64(self.resolved_width_px());
        if self.lands_at_once() {
            self.width_spring = None;
            return;
        }
        let mut spring = Spring::new(0.0, Self::width_params());
        spring.set_target(target);
        self.width_spring = Some(spring);
        self.last_width_step = Some(crate::time::now());
        self.style_dirty = true;
    }

    fn retarget_width(&mut self, from: f64, to: f64) {
        let to = f32_of_f64(to.max(0.0));
        if self.lands_at_once() {
            self.width_spring = None;
            return;
        }
        if let Some(spring) = self.width_spring.as_mut() {
            spring.set_target(to);
            return;
        }
        let from = f32_of_f64(from.max(0.0));
        if (from - to).abs() < 0.5 {
            return;
        }
        let mut spring = Spring::new(from, Self::width_params());
        spring.set_target(to);
        self.width_spring = Some(spring);
        // The spring's clock starts with the motion, so its first step
        // covers the time since the resize was asked for.
        self.last_width_step = Some(crate::time::now());
        self.style_dirty = true;
    }

    /// Advance the slot-width spring. Called from the row's layout pass
    /// so neighbours see the new width in the same frame.
    pub(crate) fn step_width_spring(&mut self) {
        if self.close_complete() {
            self.width_spring = None;
            self.last_width_step = None;
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
        // Unclamped: `Spring::step` sub-steps a long frame, so after a stall
        // the width lands where a wall-clock tween started with it would,
        // instead of crawling in slow motion.
        let now = crate::time::now();
        let dt = self
            .last_width_step
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        let before = spring.value();
        spring.step(dt);
        self.last_width_step = Some(now);
        if (spring.value() - before).abs() <= 0.05 && (spring.value() - spring.target()).abs() < 0.5
        {
            // The spring is only stepped while the layout is rebuilt, and a
            // step this small asks for no rebuild: the rest of the motion,
            // well under a pixel, would never be taken and the group would
            // ask for animation frames forever. Finish it now. A small step
            // far from the target is only a short frame (the first one after
            // the motion began), not the end of the motion.
            spring.snap(spring.target());
        }
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
        if self.width_step_skipped
            && !self.reduce_motion
            && self.width_spring.as_ref().is_some_and(|s| !s.is_settled())
        {
            self.style_dirty = true;
        }
        self.next_frame();
    }

    /// A new frame may step the width spring and rebuild a closing group
    /// once more. Called when the group is drawn, and by a row for groups
    /// it culled from a frame, so an off-screen transition still ends.
    pub fn next_frame(&mut self) {
        self.width_step_skipped = false;
        self.width_stepped = false;
        self.close_rebuild_taken = false;
    }

    pub fn is_closing(&self) -> bool {
        self.closing_at.is_some()
    }

    pub fn close_complete(&self) -> bool {
        self.closing_at
            .is_some_and(|t| self.reduce_motion || t.elapsed() >= self.close_duration)
    }

    /// Whether a maximized window's bar shows: the person reached for it
    /// (`widget::top_edge_reveal`). It hides at rest.
    pub fn maximized_chrome_shown(&self) -> bool {
        self.maximized_chrome_live() && self.top_edge.shown()
    }

    fn maximized_chrome_live(&self) -> bool {
        self.maximized && self.navigation_bar_visible()
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

    /// Show the bar for keyboard use (`pinned`) or let the pointer decide
    /// again. Returns whether the bar showed or hid.
    pub fn pin_maximized_chrome(&mut self, pinned: bool) -> bool {
        if !self.maximized_chrome_live() {
            return false;
        }
        let changed = if pinned {
            self.top_edge.pin(crate::time::now())
        } else {
            self.top_edge.unpin();
            false
        };
        if changed {
            self.top_edge_changed();
        }
        changed
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
            self.top_overlay_inset = inset;
            self.grip_dirty = true;
            if self.bar_offered_to.is_some() {
                self.style_dirty = true;
            }
            self.clear_maximized_controls();
        }
    }

    /// Test and shot fixture: as if the person had reached for the bar
    /// (`true`) or left it (`false`).
    #[cfg(any(test, debug_assertions))]
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
        let tab_row = !self.maximized && self.navigation_bar_visible();
        for (id, host) in &mut self.hosts {
            host.set_window_presents_history(tab_row);
            self.bar.set_history_control(*id, host.history_control());
        }
    }

    /// Run a press on a pill's search or History chip: search is the
    /// host's to open ([`Self::take_pending_search_open`]); History runs on
    /// that tab's History door, as the History shortcut and the palette do.
    /// Returns whether one ran.
    fn drain_chip_presses(&mut self) -> bool {
        let mut pressed = false;
        if let Some(id) = self.bar.take_search() {
            self.search_active = !self.search_active;
            self.pending_search_open = Some(id);
            pressed = true;
        }
        if let Some((id, control)) = self.bar.take_history()
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

    pub fn resolved_width_px(&self) -> f64 {
        self.width.resolve(self.strip_usable_px)
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
    /// maximum any of its tabs' apps states ([`AppWidget::min_window_width`]),
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
        self.last_width_step = None;
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
            self.pending_width_change = Some(self.width.permille());
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
            if let TileContent::App(app) = host.content_mut() {
                app.set_live_resize_anchor(fixed);
            }
        }
    }

    /// The drag is over: an app may stop drawing against the fixed edge once
    /// its content has caught up with the window's size.
    fn release_live_resize_anchor(&mut self) {
        for (_, host) in &mut self.hosts {
            if let TileContent::App(app) = host.content_mut() {
                app.release_live_resize_anchor();
            }
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
            .is_some_and(|pending| pending != self.width.permille())
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
        self.last_width_step = None;
        self.style_dirty = true;
    }

    pub fn stamp_pending_width_change(&mut self) {
        self.pending_width_change = Some(self.width.permille());
    }
    /// A width change the host has not yet persisted: until it does, its
    /// saved width is the old one and must not be reapplied over it.
    pub fn has_pending_width_change(&self) -> bool {
        self.pending_width_change.is_some()
    }
    pub fn take_pending_width_change(&mut self) -> Option<u32> {
        self.pending_width_change.take()
    }

    pub fn take_pending_tab_reorder(&mut self) -> Option<(usize, usize)> {
        self.pending_tab_reorder.take()
    }

    pub fn take_pending_search_open(&mut self) -> Option<u64> {
        self.pending_search_open.take()
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
        Self::paint_strip_body(&self.bar, scene, (transform, Affine::IDENTITY), theme);
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
            self.last_rect_step = None;
            return;
        }
        if self.rect_spring.is_some() {
            // Already in flight: keep value and velocity. The next
            // apply_layout retargets to the new destination.
            return;
        }
        self.rect_spring = Some(RectSprings::at(self.rect));
        self.last_rect_step = None;
    }

    fn retarget_rect_spring(&mut self) {
        let dest = self.rect;
        let Some(spring) = self.rect_spring.as_mut() else {
            return;
        };
        if self.reduce_motion {
            spring.snap(dest);
            self.rect_spring = None;
            self.last_rect_step = None;
            return;
        }
        spring.set_target(dest);
    }

    /// Advance the maximize/restore draw spring. The first call after arm
    /// does not step, so the first animated frame equals the last tiled one.
    fn step_rect_spring(&mut self) {
        let Some(spring) = self.rect_spring.as_mut() else {
            return;
        };
        if self.reduce_motion {
            spring.snap(self.rect);
            self.rect_spring = None;
            self.last_rect_step = None;
            return;
        }
        let now = crate::time::now();
        let Some(prev) = self.last_rect_step else {
            self.last_rect_step = Some(now);
            return;
        };
        let dt = now
            .saturating_duration_since(prev)
            .as_secs_f32()
            .clamp(1.0 / 240.0, 1.0 / 20.0);
        spring.step(dt);
        self.last_rect_step = Some(now);
        if spring.is_settled() {
            self.rect_spring = None;
        }
    }

    fn rect_draw_affine(&self, base: Affine) -> Affine {
        let Some(spring) = &self.rect_spring else {
            return base;
        };
        let src = self.rect;
        let dst = spring.rect();
        if src.width() < 1.0 || src.height() < 1.0 {
            return base;
        }
        let sx = dst.width() / src.width();
        let sy = dst.height() / src.height();
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices"
        )]
        {
            base * Affine::translate((dst.x0, dst.y0))
                * Affine::scale_non_uniform(sx, sy)
                * Affine::translate((-src.x0, -src.y0))
        }
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
    pub fn insert_detached_tab(&mut self, idx: usize, desc: TabDescriptor, mut host: TileHost) {
        let id = desc.id;
        self.bar.insert_tab(idx, desc);
        let clamped = idx.min(self.hosts.len());
        host.start_spawn_animation();
        host.set_panic_scope(id);
        self.hosts.insert(clamped, (id, host));
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
        bar.set_on_strip(true);
        Self {
            id,
            close_reports: vec![],
            bar,
            hosts: vec![(tab_id, host)],
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            rect: Rect::ZERO,
            external_navigation: false,
            fullscreen_presentation: false,
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
            chrome_fade: Duration::ZERO,
            top_overlay_inset: 0.0,
            fills_display: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            rect_spring: None,
            last_rect_step: None,
            spring_stepped: false,
            width_spring: None,
            last_width_step: None,
            held_back: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            close_requested: false,
            width_stepped: false,
            width_step_skipped: false,
            close_rebuild_taken: false,
            close_duration: crate::tokens::motion::Durations::default().base(),
            reduce_motion: false,
            motion_source: crate::tokens::MotionSource::default(),
            style_dirty: false,
            built_active: Some(active_id),
            pending_maximize_change: None,
            pending_width_change: None,
            pending_tab_reorder: None,
            pending_search_open: None,
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
        !(plus.area() > 0.0 && plus.contains(p))
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
    pub fn tabs(&self) -> &[TabDescriptor] {
        self.bar.tabs()
    }

    /// Mark tab `id` as held by someone else ([`crate::widget::TabHolder`]),
    /// or clear the mark: such a tab offers no close and resists a drag.
    /// `true` when that changed what the window offers.
    pub fn set_tab_holder(&mut self, id: u64, holder: Option<crate::widget::TabHolder>) -> bool {
        self.bar.set_holder(id, holder)
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
        WindowGroupBounds {
            group: self.rect,
            client: self.active_host().map(Widget::get_rect),
            tab_bar: self.bar.get_rect(),
            maximize: (!self.maximized && self.navigation_bar_visible())
                .then(|| self.active_maximize_rect())
                .flatten()
                .and_then(painted),
            restore: painted(self.restore_rect),
            close: painted(self.close_rect),
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
            self.closing_hosts
                .entry(close_id)
                .or_insert_with(crate::time::now);
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
            self.close_requested = true;
            self.width_spring = None;
            self.last_width_step = None;
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
    pub fn restore_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::Minimize2
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
            self.closing_hosts
                .insert(id, instant_ago_ms(GROUP_CLOSE_FADE_MS.saturating_add(50)));
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

    #[cfg(test)]
    pub(crate) fn set_close_hover_for_tests(&mut self, hover: bool) {
        self.close_hover = hover;
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
                self.closing_hosts
                    .entry(active)
                    .or_insert_with(crate::time::now);
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
        // GROUP_CLOSE_FADE_MS mirrors CLOSE_FADE_MS (tabs.rs) as a whole
        // millisecond u64, avoiding an f32->u64 cast at every use site.
        let close_window = Duration::from_millis(GROUP_CLOSE_FADE_MS);
        let expired: Vec<u64> = self
            .closing_hosts
            .iter()
            .filter(|(_, started)| started.elapsed() >= close_window)
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
            self.closing_hosts
                .entry(close_id)
                .or_insert_with(crate::time::now);
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
        if let Some(_id) = self.bar.take_max() {
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
            let raw = f32_of_f64(
                (started.elapsed().as_secs_f64() / self.close_duration.as_secs_f64())
                    .clamp(0.0, 1.0),
            );
            let eased = 1.0 - (1.0 - raw).powi(3);
            let alpha = (1.0 - eased).max(0.0);
            let scale = f64::from(1.0 - 0.05 * eased);
            (alpha, 0.0, 0.0, scale)
        } else if let Some(started) = self.spawned_at {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(SPAWN_ANIM_MS) {
                (1.0, 0.0, 0.0, 1.0)
            } else {
                let raw = (elapsed.as_secs_f64() / spawn_anim_secs()).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - raw).powi(3);
                let scale = SPAWN_ANIM_INITIAL_SCALE + (1.0 - SPAWN_ANIM_INITIAL_SCALE) * eased;
                let x_off = SPAWN_ANIM_X_OFFSET * (1.0 - eased);
                (f32_of_f64(eased), x_off, 0.0, scale)
            }
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

    /// Paint the body of this group's docked tab strip
    /// ([`crate::chrome::SelectorRole::Strip`]) under where [`Widget::draw`]
    /// puts its tabs, through the same fade and spring.
    ///
    /// Whoever draws the group calls this first, into the scene the group
    /// is composed into rather than into a drawing of the group it keeps:
    /// a frosted strip shows the desktop behind where it is, and
    /// `desktop` (the desktop's coordinates in the scene's, moved by the
    /// pan of a strip drawn in canvas coordinates) belongs to the row, not
    /// to the group's drawing.
    pub fn draw_docked_strip(
        &mut self,
        scene: &mut Scene,
        (transform, desktop): (Affine, Affine),
        theme: &Theme,
    ) {
        if self.maximized || !self.navigation_bar_visible() {
            return;
        }
        // As `draw`: nothing paints until the group has something to show.
        if !Widget::presented(self) && !Widget::opening(self) && !self.is_closing() {
            return;
        }
        self.reduce_motion = theme.reduce_motion;
        self.step_rect_spring();
        self.spring_stepped = true;
        let (fade_alpha, fade_x, fade_y, fade_scale) = self.fade_transform_params();
        if fade_alpha <= 0.0 {
            return;
        }
        let bar = self.bar.bar_rect();
        if bar.width() <= 0.0 || bar.height() <= 0.0 {
            return;
        }
        let fading = fade_alpha < 1.0;
        if fading {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                fade_alpha,
                transform,
                &bar.inflate(16.0, 16.0),
            );
        }
        let transform = self.fade_affine(transform, (fade_x, fade_y, fade_scale));
        Self::paint_strip_body(&self.bar, scene, (transform, desktop), theme);
        if fading {
            scene.pop_layer();
        }
    }

    /// The docked strip's body at the bar's rect, its capsule concentric
    /// with the tabs inset in it.
    fn paint_strip_body(
        bar: &TabBar,
        scene: &mut Scene,
        (transform, desktop): (Affine, Affine),
        theme: &Theme,
    ) {
        let strip = crate::chrome::strip_shape(
            bar.bar_rect(),
            f64::from(theme.tokens.radii.control),
            crate::widget::tabs::TAB_PILL_INSET_Y,
        );
        crate::chrome::paint_selector(
            scene,
            transform,
            &strip,
            crate::chrome::SelectorRole::Strip(desktop),
            theme,
        );
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
        // GROUP_CLOSE_FADE_MS mirrors CLOSE_FADE_MS (tabs.rs) as a whole
        // millisecond u64, avoiding an f32->u64 cast at every use site.
        let close_window = Duration::from_millis(GROUP_CLOSE_FADE_MS);
        let closing_ids: Vec<(u64, Instant)> = self
            .closing_hosts
            .iter()
            .filter(|(_, t)| t.elapsed() < close_window)
            .map(|(id, t)| (*id, *t))
            .collect();
        for (id, started) in closing_ids {
            let elapsed = started.elapsed().as_secs_f64();
            let raw = (elapsed / (f64::from(CLOSE_FADE_MS) / 1000.0)).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - raw).powi(3);
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
        let grip_col = theme.tokens.colors.border_focus;
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

    /// A maximized window's bar, over the top edge of its content while the
    /// person reaches for it (`top_edge_reveal`): the floating
    /// `[app title]  …  [Full screen][Restore][×]` chips over a foreign
    /// client, or three icon buttons in the trailing slot of a Tilo app's
    /// header row that took the bar. It fades in and out in place; its
    /// controls take the pointer only while it shows.
    fn draw_maximized_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.maximized_chrome_live() {
            self.top_edge.reset();
            self.clear_maximized_controls();
            return;
        }
        let now = crate::time::now();
        if self.top_edge.settle(now) {
            self.top_edge_changed();
        }
        let fade = if self.reduce_motion {
            Duration::ZERO
        } else {
            theme.tokens.motion.durations.fast()
        };
        self.chrome_fade = fade;
        let alpha = self.top_edge.opacity(now, fade);
        if alpha <= 0.0 {
            self.clear_maximized_controls();
            return;
        }
        let layered = alpha < 1.0;
        if layered {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::num::f32_of_f64(alpha),
                transform,
                &self.rect,
            );
        }
        if let Some(slot) = self.bar_slot {
            self.draw_merged_controls(scene, transform, tr, theme, slot);
        } else {
            // `[app title]  …  [Restore][×]`, matching the concept layout.
            // The row floats `TAB_PILL_INSET_Y` in from the card's top and
            // sides — the same inset the tab pills it stands in for keep.
            let pad = crate::widget::tabs::TAB_PILL_INSET_Y;
            let layout = ChromePillLayout {
                pad,
                pill_h: maximized_chip_height(theme),
                y0: self.rect.y0 + self.top_overlay_inset + pad,
            };
            self.draw_maximized_title_chip(scene, transform, tr, theme, layout);
            self.draw_maximized_buttons(scene, transform, tr, theme, layout);
        }
        if layered {
            scene.pop_layer();
        }
        if !self.top_edge.shown() {
            // Fading out: painted, but no longer the pointer's.
            self.clear_maximized_controls();
        }
    }

    /// Register the bar's three controls with their painted targets.
    fn register_maximized_controls(&mut self, restore: &str, fullscreen: &str) {
        let geometry = (self.restore_rect, self.close_rect, self.fullscreen_rect);
        if self.registered_controls != Some(geometry) {
            self.maximized_controls.begin_frame();
            self.maximized_controls
                .target(RESTORE_TARGET, self.restore_rect, restore);
            self.maximized_controls
                .target(FULLSCREEN_TARGET, self.fullscreen_rect, fullscreen);
            self.maximized_controls.target(
                CLOSE_TARGET,
                self.close_rect,
                crate::i18n::widget_format("host-window-close", &[]),
            );
            self.registered_controls = Some(geometry);
        }
    }

    /// The window's controls in the trailing slot of the app's header row:
    /// the row's own icon buttons, so the app's header and the window's
    /// controls read as one bar.
    fn draw_merged_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        slot: crate::widget::WindowBarSlot,
    ) {
        let m = crate::chrome::HeaderRowMetrics::resolve(theme);
        let y0 = slot.row.center().y - m.chip / 2.0;
        let at = |x1: f64| Rect::new(x1 - m.chip, y0, x1, y0 + m.chip);
        self.title_rect = Rect::ZERO;
        self.close_rect = at(slot.controls.x1);
        self.restore_rect = at(self.close_rect.x0 - m.gap);
        self.fullscreen_rect = at(self.restore_rect.x0 - m.gap);
        let restore = crate::i18n::widget_format("widget-maximized-restore", &[]);
        let fullscreen = crate::i18n::widget_format("widget-maximized-fullscreen", &[]);
        let close = crate::i18n::widget_format("host-window-close", &[]);
        self.register_maximized_controls(&restore, &fullscreen);
        for (rect, icon, name, hovered, target) in [
            (
                self.fullscreen_rect,
                icons::Icon::Maximize2,
                &fullscreen,
                self.fullscreen_hover,
                FULLSCREEN_TARGET,
            ),
            (
                self.restore_rect,
                icons::Icon::Minimize2,
                &restore,
                self.restore_hover,
                RESTORE_TARGET,
            ),
            (
                self.close_rect,
                icons::Icon::X,
                &close,
                self.close_hover,
                CLOSE_TARGET,
            ),
        ] {
            // Someone else's window offers no close.
            if target == CLOSE_TARGET && self.bar.is_held(self.bar.active_id()) {
                continue;
            }
            let interaction = self.maximized_controls.interaction(target);
            let face = crate::widget::ButtonFace::icon_only(rect, icon, name)
                .hovered(hovered || interaction.hovered)
                .pressed(interaction.pressed);
            crate::widget::Button::paint(scene, transform, tr, theme, &face);
        }
    }

    /// The `[app title]` chip at the left of the maximized-chrome pill row.
    fn draw_maximized_title_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        layout: ChromePillLayout,
    ) {
        let ChromePillLayout { pad, pill_h, y0 } = layout;
        let active_title = self
            .bar
            .tabs()
            .iter()
            .find(|t| t.id == self.bar.active_id())
            .map(|t| t.title.clone())
            .unwrap_or_default();
        let title_style = theme.text(TextRole::Caption);
        let (tw, th) = tr.measure_style(&active_title, None, &title_style);
        // The label sits `spacing.s3` in from either end of its chip.
        let chip_pad = f64::from(theme.tokens.spacing.s3);
        let title_pill_w = (f64::from(tw) + 2.0 * chip_pad).max(MAXIMIZED_TITLE_CHIP_MIN_W);
        let title_pill = Rect::new(
            self.rect.x0 + pad,
            y0,
            self.rect.x0 + pad + title_pill_w,
            y0 + pill_h,
        );
        self.title_rect = title_pill;
        crate::surface::shadow(
            scene,
            transform,
            title_pill,
            title_pill.height() / 2.0,
            &theme.tokens.elevation.floating,
        );
        crate::widget::chip::paint_pill_body(
            scene,
            transform,
            title_pill,
            crate::widget::ChipStyle::Neutral,
            false,
            theme,
        );
        // Affine composition is floating-point matrix multiply — it
        // cannot overflow or panic the way integer arithmetic can.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let title_text_transform = transform
            * vello::kurbo::Affine::translate((
                title_pill.center().x - f64::from(tw) / 2.0,
                title_pill.center().y - f64::from(th) / 2.0,
            ));
        tr.draw_style(
            scene,
            &active_title,
            theme.tokens.colors.text_primary,
            title_text_transform,
            None,
            &title_style,
        );
    }

    /// The `[Restore][×]` button pair at the right of the maximized-chrome
    /// pill row. Also updates `restore_rect`/`close_rect` for hit-testing,
    /// which keeps running regardless of the cursor band — only the paint
    /// is gated by the caller.
    fn draw_maximized_buttons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        layout: ChromePillLayout,
    ) {
        let ChromePillLayout { pad, pill_h, y0 } = layout;
        let title_style = theme.text(TextRole::Caption);
        // The close chip is a square of the row's height, so its × sits on
        // the same centre the title chip's text does.
        let close_w = pill_h;
        let close_x0 = self.rect.x1 - close_w - pad;
        self.close_rect = Rect::new(close_x0, y0, close_x0 + close_w, y0 + pill_h);
        let label = crate::i18n::widget_format("widget-maximized-restore", &[]);
        let (lw, lh) = tr.measure_style(&label, None, &title_style);
        let icon_inset = f64::from(theme.tokens.spacing.s2);
        // Icon slot, gap, label, end pad: a longer translation widens the chip.
        let restore_w = MAXIMIZED_RESTORE_CHIP_W
            .max(icon_inset + MAXIMIZED_ICON_PX + icon_inset + f64::from(lw) + icon_inset);
        let restore_x0 = close_x0 - restore_w - f64::from(theme.tokens.spacing.s1);
        self.restore_rect = Rect::new(restore_x0, y0, restore_x0 + restore_w, y0 + pill_h);
        let full_label = crate::i18n::widget_format("widget-maximized-fullscreen", &[]);
        let (flw, flh) = tr.measure_style(&full_label, None, &title_style);
        let full_w = MAXIMIZED_RESTORE_CHIP_W
            .max(icon_inset + MAXIMIZED_ICON_PX + icon_inset + f64::from(flw) + icon_inset);
        let full_x0 = restore_x0 - full_w - f64::from(theme.tokens.spacing.s1);
        self.fullscreen_rect = Rect::new(full_x0, y0, full_x0 + full_w, y0 + pill_h);
        crate::surface::shadow(
            scene,
            transform,
            self.fullscreen_rect,
            self.fullscreen_rect.height() / 2.0,
            &theme.tokens.elevation.floating,
        );
        crate::widget::chip::paint_pill_body(
            scene,
            transform,
            self.fullscreen_rect,
            crate::widget::ChipStyle::Neutral,
            self.fullscreen_hover,
            theme,
        );
        let full_icon = Rect::new(
            self.fullscreen_rect.x0 + icon_inset,
            self.fullscreen_rect.center().y - MAXIMIZED_ICON_PX / 2.0,
            self.fullscreen_rect.x0 + icon_inset + MAXIMIZED_ICON_PX,
            self.fullscreen_rect.center().y + MAXIMIZED_ICON_PX / 2.0,
        );
        icons::draw_icon(
            scene,
            transform,
            full_icon,
            icons::Icon::Maximize2,
            theme.tokens.colors.text_primary,
            icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let full_label_transform = transform
            * vello::kurbo::Affine::translate((
                full_icon.x1 + f64::from(theme.tokens.spacing.s2),
                self.fullscreen_rect.center().y - f64::from(flh) / 2.0,
            ));
        tr.draw_style(
            scene,
            &full_label,
            theme.tokens.colors.text_primary,
            full_label_transform,
            None,
            &title_style,
        );
        self.register_maximized_controls(&label, &full_label);

        crate::surface::shadow(
            scene,
            transform,
            self.restore_rect,
            self.restore_rect.height() / 2.0,
            &theme.tokens.elevation.floating,
        );
        let ink = crate::widget::chip::paint_pill_action_body(
            scene,
            transform,
            self.restore_rect,
            crate::widget::activation::Interaction {
                hovered: self.restore_hover,
                ..self.maximized_controls.interaction(RESTORE_TARGET)
            },
            theme,
        );
        // Text label kept (for screen readers/i18n), after a square icon
        // slot for the lucide `Minimize2` glyph: `spacing.s2` in from the
        // chip's edge, `MAXIMIZED_ICON_PX` a side and centred on the chip's
        // centre line, with the label one `spacing.s2` after it — the same
        // icon → gap → label rhythm a tab pill uses.
        let icon_x0 = self.restore_rect.x0 + icon_inset;
        let icon_slot = Rect::new(
            icon_x0,
            self.restore_rect.center().y - MAXIMIZED_ICON_PX / 2.0,
            icon_x0 + MAXIMIZED_ICON_PX,
            self.restore_rect.center().y + MAXIMIZED_ICON_PX / 2.0,
        );
        icons::draw_icon(
            scene,
            transform,
            icon_slot,
            icons::Icon::Minimize2,
            ink,
            icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let lx = icon_slot.x1 + f64::from(theme.tokens.spacing.s2);
        let ly = self.restore_rect.center().y - f64::from(lh) / 2.0;
        // Affine composition is floating-point matrix multiply — it
        // cannot overflow or panic the way integer arithmetic can.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let label_transform = transform * vello::kurbo::Affine::translate((lx, ly));
        tr.draw_style(scene, &label, ink, label_transform, None, &title_style);

        self.draw_maximized_close_button(scene, transform, theme);
    }

    /// The `[×]` close button at the far right of the maximized-chrome
    /// pill row, painted into `self.close_rect` (already computed by
    /// [`Self::draw_maximized_buttons`]).
    fn draw_maximized_close_button(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.bar.is_held(self.bar.active_id()) {
            return;
        }
        crate::surface::shadow(
            scene,
            transform,
            self.close_rect,
            self.close_rect.height() / 2.0,
            &theme.tokens.elevation.floating,
        );
        // The quiet chip, lifted under the pointer. Closing a window is not
        // an error, so it is never danger red.
        let glyph_color = crate::widget::chip::paint_pill_action_body(
            scene,
            transform,
            self.close_rect,
            crate::widget::activation::Interaction {
                hovered: self.close_hover,
                ..self.maximized_controls.interaction(CLOSE_TARGET)
            },
            theme,
        );
        icons::draw_icon(
            scene,
            transform,
            self.close_rect,
            icons::Icon::X,
            glyph_color,
            1.5,
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
        if !self.maximized && self.navigation_bar_visible() {
            children.push(&self.bar);
        }
        let active = self.bar.active_id();
        if let Some((id, host)) = self.hosts.iter().find(|(id, _)| *id == active) {
            // A host swapped in through `host_for_tab_mut` is named here.
            host.set_panic_scope(*id);
            children.push(host);
        }
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let active = self.bar.active_id();
        let mut children: Vec<&mut dyn Widget> = Vec::new();
        if !self.maximized && self.navigation_bar_visible() {
            children.push(&mut self.bar);
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
        // The bar's hover dwell or aim hold that came due (its wake
        // deadline) opens its pill here, before the frame lays out.
        let bar = self.bar.update(s, p);
        self.active_host_mut().is_some_and(|h| h.update(s, p)) || pressed || bar
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.close_duration = theme.tokens.motion.durations.base();
        self.set_reduce_motion(theme.reduce_motion);
        let bar_node = self
            .navigation_bar_visible()
            .then(|| self.bar.build_layout(taffy, theme, node_map));
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
            // The client fills the whole window. Its bar (title, Full screen,
            // Restore, close) hides at rest and appears over the window's top
            // edge when the person reaches for it (`top_edge_reveal`); a
            // Tilo app's header row takes it in as one bar
            // (`WindowHeader::set_window_bar`).
            if let Some(bar_node) = bar_node {
                let mut bar_style = taffy.style(bar_node).cloned().unwrap_or_default();
                bar_style.display = taffy::Display::None;
                let _ = taffy.set_style(bar_node, bar_style);
            }
        }
        let nodes = bar_node.map_or_else(|| vec![tile_node], |bar| vec![bar, tile_node]);
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
        // The strip under the tabs was drawn first and already moved the
        // spring for this frame; stepping it twice would run it fast.
        if !std::mem::take(&mut self.spring_stepped) {
            self.step_rect_spring();
        }
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
        // The shell latches the spawn fade when it splices the group in,
        // before the app has drawn — without restarting it here the fade
        // would already have run its course against an invisible group.
        if std::mem::take(&mut self.held_back) && !self.is_closing() {
            self.spawned_at = (!self.reduce_motion).then(crate::time::now);
            if !self.reduce_motion {
                self.arm_width_from_zero();
            }
        }
        // Close checked first (mutually exclusive with spawn in practice):
        // `opacity 0→1, scale 0.92→1, x -16→0` over `SPAWN_ANIM_MS`
        // cubic-out; close mirrors it (α 1→0, scale 1→0.95). The whole
        // paint composes under one alpha+translate+scale layer so hit
        // tests stay aligned with what's on screen mid-fade.
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
        // The bar's hit rects stay live either way — only the paint is
        // gated (maximized swaps in the floating pill further down).
        if !self.maximized && self.navigation_bar_visible() {
            self.bar
                .set_group_focused(self.focused || self.search_active);
            self.bar.draw(scene, transform, tr, theme, presence);
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
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            if let Some(name) = &opening_name {
                h.set_opening_name(name);
            }
            h.set_focused(focused);
            // A maximized window filling its display draws edge to edge;
            // under a docked bar, or mid-fade, it is still a card.
            h.set_full_bleed(self.maximized && self.fills_display && !spawn_active);
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
            h.draw(scene, transform, tr, theme, presence);
            if source_is_lifted {
                scene.pop_layer();
            }
        }
        self.draw_closing_hosts(scene, transform, tr, theme, presence);
        if !self.maximized && self.navigation_bar_visible() {
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
        if self.maximized {
            self.draw_maximized_chrome(scene, transform, tr, theme);
        } else {
            self.clear_maximized_controls();
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
        self.handle_maximized_controls(event, ctx);
        if *ctx.handled {
            return;
        }
        if self.handle_edge_input(event, ctx.scale_factor, *ctx.modifiers) {
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
        if self
            .spawned_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(SPAWN_ANIM_MS))
        {
            parts.push("spawn".to_string());
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
        let spawn_in_flight = !self.reduce_motion
            && self
                .spawned_at
                .is_some_and(|t| t.elapsed() < Duration::from_millis(SPAWN_ANIM_MS));
        // GROUP_CLOSE_FADE_MS mirrors CLOSE_FADE_MS (tabs.rs) as a whole
        // millisecond u64, avoiding an f32->u64 cast at every use site.
        let close_window = Duration::from_millis(GROUP_CLOSE_FADE_MS);
        let close_in_flight = !self.reduce_motion
            && self
                .closing_hosts
                .values()
                .any(|t| t.elapsed() < close_window);
        let group_close_in_flight = self
            .closing_at
            .is_some_and(|t| !self.reduce_motion && t.elapsed() < self.close_duration);
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
        let rect_in_flight = !self.reduce_motion && self.rect_spring.is_some();
        let chrome_fade = self.maximized_chrome_live()
            && self.top_edge.fading(crate::time::now(), self.chrome_fade);
        spawn_in_flight
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
    use winit::event::MouseButton;
    use crate::testing::Dispatch;
    use crate::widget::{AppKind, TabDescriptor, TileContent};

    #[test]
    fn external_navigation_omits_bar_from_layout_and_semantics_but_retains_tabs() {
        let mut group = make_group();
        assert_eq!(group.children().len(), 2);
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
    fn tabs_stand_on_the_docked_strip_and_lift_off_its_ground() {
        use crate::color_compat::ColorCompat;
        use crate::offscreen::OffscreenRenderer;
        use taffy::prelude::TaffyMaxContent;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut group = make_group();
            let mut tree = TaffyTree::new();
            let node = group.build_layout(&mut tree, &theme, &mut Default::default());
            let mut style = tree.style(node).unwrap().clone();
            style.size.width = Dimension::length(360.0);
            tree.set_style(node, style).unwrap();
            let root = tree.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(360.0),
                        height: Dimension::length(240.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
            group.apply_layout(&tree, node, Point::new(20.0, 20.0));
            let bar = group.bar_rect();
            let first = group.tab_rects()[0];
            assert!(
                (first.x0 - bar.x0 - crate::widget::tabs::TAB_PILL_INSET_Y).abs() < 1e-6,
                "{}: the first tab sits the strip's inset in from its end: {first:?} in {bar:?}",
                theme.name
            );
            let mut scene = Scene::new();
            group.draw_docked_strip(&mut scene, (Affine::IDENTITY, Affine::IDENTITY), &theme);
            group.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
            let mut renderer = OffscreenRenderer::new(400, 300);
            let rgba = renderer.render(&scene, theme.tokens.colors.surfaces.base);
            let at = |x: f64, y: f64| {
                let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 400, x as u32, y as u32);
                (r, g, b)
            };
            let rgb = |c: vello::peniko::Color| (c.r(), c.g(), c.b());
            let ground = crate::chrome::strip_ground(&theme);
            let close = |a: (u8, u8, u8), b: (u8, u8, u8)| {
                a.0.abs_diff(b.0) <= 2 && a.1.abs_diff(b.1) <= 2 && a.2.abs_diff(b.2) <= 2
            };
            // Outside a backdrop recording the strip is its opaque ground.
            let strip = at(bar.x0 + 3.0, bar.center().y);
            assert!(
                close(strip, rgb(ground)),
                "{}: the strip's body {strip:?} is not its ground {:?}",
                theme.name,
                rgb(ground)
            );
            let active = crate::chrome::selector_tier(
                &theme,
                crate::chrome::SelectionState::active(true),
                ground,
            );
            let pill = at(first.x0 + 5.0, first.center().y);
            assert!(
                close(pill, rgb(active.fill)),
                "{}: the active tab {pill:?} is not the lift of the strip's ground {:?}",
                theme.name,
                rgb(active.fill)
            );
        }
    }

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
        assert_eq!(
            at(&leading, x, y),
            at(&idle, x, y),
            "and only on that edge"
        );
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

    fn make_group() -> WindowGroup {
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
        // Case `the_paint_probe_reports_an_untouched_scene_as_empty`.
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

        // Case `a_group_paints_nothing_while_the_wait_is_still_too_short_to_be_one`.
        {
            let mut g = group_opening("Files", false, false);
            assert!(
                !painted_anything(&mut g),
                "a window frame went up for an app that has not drawn and has \
             not been waiting long enough to say so — which is a card \
             flashing on and off in front of an app that opened fast",
            );
        }

        // Case `a_group_holds_the_window_place_once_the_wait_is_worth_naming`.
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

        // Case `a_group_with_a_live_window_is_never_holding_a_place`.
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
        // Case `every_tab_in_a_group_is_asked_whether_its_window_has_drawn`.
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

        // Case `a_drawn_window_behind_another_tab_retires_its_opening_card`.
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

        // Case `a_window_that_has_not_drawn_still_holds_its_place`.
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

        // A pill only carries a maximize chip once hovered open.
        let pill = g.bar.tab_rects()[0];
        dispatch_at(
            &mut g,
            &taffy,
            group_node,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(pill.center().x, pill.center().y),
            },
        );
        assert!(
            g.bar.elapse_hover_dwell(),
            "resting on the pill must open it once the hover dwell has run",
        );
        g.apply_layout(&taffy, group_node, Point::ZERO);

        let max_rect = g.bar.max_rects()[0];
        assert!(
            max_rect.area() > 0.0,
            "an opened pill must carry a maximize chip to click",
        );
        let click_pt = max_rect.center();

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

    /// The expanded chip row carries a `[Full screen]` entry beside
    /// Restore; pressing it asks for real fullscreen once.
    #[test]
    fn the_expanded_chrome_offers_full_screen_beside_restore() {
        let (mut g, taffy, group_node, _moved, press) = open_maximize_chip();
        let theme = Theme::dark();
        g.maximized = true;
        g.top_edge.force_shown(true);
        g.draw_maximized_chrome(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
        );
        let chip = g.fullscreen_rect();
        assert!(chip.width() > 0.0, "the chip is drawn");
        assert_eq!(chip.y0, g.restore_rect.y0, "one row with Restore");
        assert!(chip.x1 <= g.restore_rect.x0, "left of Restore");
        assert!(!g.take_pending_fullscreen_request());
        // Maximized controls act on a completed click: press, then release
        // on the same control.
        let over_chip = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(chip.center().x, chip.center().y),
        };
        let release = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        g.last_cursor = chip.center();
        let _ = dispatch_at(&mut g, &taffy, group_node, &over_chip);
        assert!(dispatch_at(&mut g, &taffy, group_node, &press));
        assert!(
            !g.take_pending_fullscreen_request(),
            "a press awaits its release"
        );
        let _ = dispatch_at(&mut g, &taffy, group_node, &release);
        assert!(g.take_pending_fullscreen_request());
        assert!(!g.take_pending_fullscreen_request(), "drained once");
    }

    #[test]
    fn maximize_toggles_rebuild_layout_and_drain_once() {
        // Case `maximize_click_on_tab_pill_toggles_window_group_maximized`.
        {
            let (mut g, taffy, group_node, moved, press) = open_maximize_chip();

            // Seed last_cursor on the icon — the bar's press arm uses the
            // cached cursor for its fresh hit-test.
            dispatch_at(&mut g, &taffy, group_node, &moved);
            let handled = dispatch_at(&mut g, &taffy, group_node, &press);

            assert!(handled, "press on the maximize icon must be claimed");
            assert!(
                g.maximized,
                "maximize click on a tab pill must drive WindowGroup::maximized true",
            );

            dispatch_at(&mut g, &taffy, group_node, &moved);
            dispatch_at(&mut g, &taffy, group_node, &press);
            assert!(
                !g.maximized,
                "second maximize click must restore (toggle off)",
            );
        }

        // Case `maximize_toggle_marks_structure_dirty_for_layout_rebuild`.
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

        // Case `active_tab_switch_marks_structure_dirty_for_layout_rebuild`.
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

            // Once the host rebuilds for the new active tab, it drains clean.
            let mut taffy2 = TaffyTree::<()>::new();
            let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = g.build_layout(&mut taffy2, &theme, &mut node_map2);
            assert!(
                !g.take_structure_dirty(),
                "structure_dirty drains once the host rebuilds for the new active tab",
            );
        }

        // Case `take_pending_maximize_change_drains_on_tab_bar_toggle`.
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

        // Case `maximized_group_keeps_client_header_below_system_controls`.
        {
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            // Lay a group out and report (group_height, tile_height).
            fn measure(g: &mut WindowGroup, theme: &Theme) -> (f64, f64) {
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = g.build_layout(&mut taffy, theme, &mut node_map);
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
                let tile = taffy.children(node).unwrap()[1];
                let tile_h = f64::from(taffy.layout(tile).copied().unwrap_or_default().size.height);
                (g.get_rect().height(), tile_h)
            }

            let theme = Theme::dark();
            let mut g = make_group();
            let (group_h, tile_h) = measure(&mut g, &theme);
            assert!(
                tile_h < group_h,
                "while tiled, the tab strip takes height off the app: \
             group={group_h} tile={tile_h}",
            );

            for theme in [
                Theme::light(),
                Theme::dark(),
                Theme::high_contrast().with_text_scale(2.0),
            ] {
                g.maximized = true;
                let (group_h, tile_h) = measure(&mut g, &theme);
                assert!(
                    (group_h - tile_h).abs() < 0.0001,
                    "the client fills the maximized window: group={group_h}, client={tile_h}",
                );
                g.top_edge.force_shown(true);
                g.draw_maximized_chrome(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &theme,
                );
                let client = g.active_host().unwrap().get_rect();
                assert!(
                    g.restore_rect.y0 >= client.y0 + crate::widget::top_edge_reveal::REVEAL_BAND_PX,
                    "Restore stands over the client's top edge, below the reveal band"
                );
                assert!(
                    g.maximized_chrome_captures(g.restore_rect.center()),
                    "the revealed scaled Restore control must stay reachable"
                );
                assert!(tile_h > 0.0);
            }
        }

        // Case `maximized_restore_rebuilds_the_tiled_layout`.
        {
            use crate::synthetic_input::PointerAction;
            let mut fixture = MaximizedControlFixture::new(Theme::dark());
            let _ = fixture.group.take_structure_dirty();
            let restore = fixture.group.presentation_bounds().restore.unwrap();
            fixture.pointer(PointerAction::MoveTo(restore.center()));
            assert!(fixture.pointer(PointerAction::Press(MouseButton::Left)));
            assert!(fixture.group.maximized, "press must wait for release");
            assert!(fixture.pointer(PointerAction::Release(MouseButton::Left)));
            assert!(!fixture.group.maximized);
            assert_eq!(fixture.group.take_pending_maximize_change(), Some(false));
            assert!(
                fixture.group.take_structure_dirty(),
                "restore must invalidate the cached maximized geometry"
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

        // Case `the_live_width_is_laid_out_in_whole_pixels_off_the_share_grid`.
        {
            let theme = Theme::dark();
            let mut g = make_group();
            // 2400 px strip: one per-mille is 2.4 px, so a share alone could
            // not put the edge where the pointer is.
            g.set_strip_usable_px(2400.0);
            let _ = g.take_structure_dirty();
            assert_eq!(layout_width(&mut g, &theme), 1601.0);
            g.begin_edge_drag(WindowEdge::Trailing);
            assert!(g.edge_resize_in_flight());
            assert_eq!(g.edge_drag_origin_px(), Some(1601.0));
            assert_eq!(g.edge_highlight(), Some(WindowEdge::Trailing));
            g.set_live_width(1700.4);
            assert!(g.take_structure_dirty(), "a live width relays the window out");
            assert_eq!(layout_width(&mut g, &theme), 1700.0, "whole pixels");
            assert_eq!(
                g.take_pending_width_change(),
                None,
                "mid-drag nothing is handed to the shell to persist",
            );
            let settle = g.end_edge_drag(true);
            assert_eq!(
                g.width(),
                WindowWidth::Fraction(708),
                "the drag is stored as the share it landed on (1700/2400)",
            );
            assert_eq!(g.take_pending_width_change(), Some(708));
            assert_eq!(g.take_pending_width_change(), None, "drained once");
            assert_eq!(
                settle,
                g.resolved_width_px() - 1700.0,
                "the commit reports what the share's own grid moves the width by",
            );
            assert!(!g.edge_resize_in_flight());
            assert_eq!(g.edge_highlight(), None);
        }

        // Case `a_cancelled_drag_puts_the_width_back`.
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

        // Case `width_limits_are_the_floor_without_an_app_minimum`.
        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let limits = g.width_limits();
            assert_eq!(limits.min, min_group_px(1200.0));
            assert_eq!(limits.max, max_group_px(1200.0));
        }

        // Case `cursor_shape_is_ew_resize_over_either_edge_band`.
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

        // Case `the_share_persists_across_displays`.
        {
            let mut wide = make_group();
            wide.set_strip_usable_px(3840.0);
            assert_eq!(wide.width(), g_normal_width());
            assert_eq!(
                wide.resolved_width_px(),
                2561.0,
                "667/1000 of a 3840 px strip — the same share a 1200 px strip \
             gives at 800 px, to the 1 px that per-mille quantisation costs \
             at this size",
            );
        }
    }

    #[test]
    fn tab_bar_requests_reorder_search_and_pin_keep_hosts_aligned() {
        // Case `take_pending_tab_reorder_drains_after_a_reorder`.
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

        // Case `search_active_toggles_when_bar_publishes_pending_search`.
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

        // Case `ctrl_shift_p_toggles_pin_on_active_tab`.
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

            // Second press toggles back off.
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
    fn spawn_animation_eases_within_its_band_and_respects_reduced_motion() {
        // Case `start_spawn_animation_records_timestamp_and_pumps_frames`.
        {
            let mut g = make_group();
            assert!(g.spawned_at.is_none(), "fresh group has no spawn timestamp");
            g.start_spawn_animation();
            assert!(
                g.spawned_at.is_some(),
                "start_spawn_animation records the start"
            );
            assert!(
                g.needs_animation_frame(),
                "in-flight spawn fade must pump the run loop",
            );
        }

        // Case `reduced_motion_spawn_snaps_without_pumping_frames`.
        {
            let mut g = make_group();
            g.reduce_motion = true;
            g.start_spawn_animation();

            assert_eq!(
                g.fade_transform_params(),
                (1.0, 0.0, 0.0, 1.0),
                "reduced motion must paint a newly opened group at its settled transform",
            );
            assert!(
                !g.needs_animation_frame(),
                "the settled spawn must not keep a software-rendered compositor awake",
            );
        }

        // Case `spawn_fade_window_clears_after_duration`.
        {
            let mut g = make_group();
            g.spawned_at = Some(instant_ago_ms(SPAWN_ANIM_MS.saturating_add(16)));
            assert!(
                !g.needs_animation_frame(),
                "after the spawn window elapses, the fade no longer demands frames",
            );
        }

        // Case `spawn_animation_advances_eased_progress_over_time`.
        {
            let mut g = make_group();
            // No spawn in flight — the drawn alpha pins to 1.0.
            assert_eq!(
                g.fade_transform_params().0,
                1.0,
                "idle group reports finished progress"
            );

            // First frame: elapsed ~0, so eased progress is near zero.
            g.start_spawn_animation();
            let t_start = g.fade_transform_params().0;
            assert!(
                t_start < 0.05,
                "first-frame spawn progress must be near zero, got {t_start}",
            );

            // Roughly midway: cubic-out at raw=0.5 yields 1 - 0.5^3 = 0.875,
            // so we expect well past 0.5. Backdate the timestamp instead of
            // sleeping so the test doesn't waste real time.
            g.spawned_at = Some(instant_ago_ms(SPAWN_ANIM_MS / 2));
            let t_mid = g.fade_transform_params().0;
            assert!(
                t_mid > 0.5 && t_mid < 1.0,
                "mid-spawn progress must be past half but not yet complete, got {t_mid}",
            );

            // Past the duration: pins to 1.0.
            g.spawned_at = Some(instant_ago_ms(SPAWN_ANIM_MS.saturating_add(16)));
            assert_eq!(
                g.fade_transform_params().0,
                1.0,
                "after the spawn window elapses, progress pins to 1.0",
            );
        }

        // Case `spawn_animation_duration_in_visible_band`.
        // Locked to a 350-450ms band so a future pass can't silently shorten
        // it back to an imperceptible tuning.
        {
            assert!(
                (350..=450).contains(&SPAWN_ANIM_MS),
                "spawn duration must stay in the 350-450 ms readability band, got {SPAWN_ANIM_MS}",
            );
        }

        // Case `insert_detached_tab_plays_tile_spawn_animation`.
        {
            let mut group = WindowGroup::new(0, vec![], 0, WindowWidth::Normal);
            let host = TileHost::new(TileContent::Stub {
                kind: AppKind::Settings,
                label: "stub".to_string(),
            });
            let desc = TabDescriptor::new(42, "new");
            group.insert_detached_tab(0, desc, host);
            let inserted = group
                .hosts
                .iter()
                .find(|(i, _)| *i == 42)
                .expect("inserted host present");
            assert!(
                inserted.1.spawn_in_flight(),
                "inserting a detached tab must fire the tile-level spawn fade",
            );
        }
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

        // Case `maximize_layout_jumps_and_the_draw_rect_starts_at_the_tile`.
        {
            let theme = Theme::dark();
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let tiled = layout(&mut g, &theme, 1600.0, 900.0);
            assert!(
                tiled.width() > 100.0 && tiled.width() < 1600.0,
                "tiled slot must be narrower than the work area, got {}",
                tiled.width(),
            );
            g.toggle_maximized();
            assert!(
                g.needs_animation_frame(),
                "a maximize spring must pump frames",
            );
            let dest = layout(&mut g, &theme, 1600.0, 900.0);
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
            g.step_rect_spring_for_tests();
            assert!(
                (g.drawn_rect().width() - tiled.width()).abs() < 1.0,
                "the arming frame does not step, so there is no jump at the start",
            );
            g.last_rect_step = Some(instant_ago_ms(16));
            g.step_rect_spring_for_tests();
            let mid = g.drawn_rect().width();
            assert!(
                mid > tiled.width() + 1.0,
                "the drawn width must grow toward the work area, {mid} from tiled {}",
                tiled.width(),
            );
        }

        // Case `a_second_maximize_keeps_velocity`.
        {
            let theme = Theme::dark();
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let _ = layout(&mut g, &theme, 1600.0, 900.0);
            g.toggle_maximized();
            let _ = layout(&mut g, &theme, 1600.0, 900.0);
            g.step_rect_spring_for_tests();
            g.last_rect_step = Some(instant_ago_ms(48));
            g.step_rect_spring_for_tests();
            let vel_before = g
                .rect_spring_width_velocity()
                .expect("maximize spring in flight");
            assert!(
                vel_before > 0.0,
                "opening maximize must grow, vel={vel_before}"
            );
            let mid = g.drawn_rect();
            g.toggle_maximized();
            let _ = layout(&mut g, &theme, 1600.0, 900.0);
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

        // Case `reduced_motion_maximize_snaps`.
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
        // Case `opening_grows_the_slot_from_zero`.
        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            let target = g.resolved_width_px();
            assert!(target > 100.0, "fixture must have a real slot to grow into");
            g.start_spawn_animation();
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
            for _ in 0..8 {
                // Frames 16 ms apart on the spring's clock.
                g.last_width_step = g.last_width_step.map(|_| instant_ago_ms(16));
                g.next_frame();
                g.step_width_spring();
                let now = g.layout_width_px();
                assert!(now >= last, "open must grow the slot, {last} → {now}");
                last = now;
            }
            assert!(
                last > 1.0 && last < target,
                "after a few frames the slot is open but not yet at the target ({last} of {target})",
            );
        }

        // Case `a_new_resize_keeps_velocity`.
        {
            let mut g = make_group();
            g.set_strip_usable_px(1200.0);
            g.start_spawn_animation();
            for _ in 0..10 {
                g.last_width_step = g.last_width_step.map(|_| instant_ago_ms(16));
                g.next_frame();
                g.step_width_spring();
            }
            let mid = g.layout_width_px();
            let vel_before = g
                .width_spring
                .as_ref()
                .map(Spring::velocity)
                .expect("open spring still in flight");
            assert!(
                vel_before > 0.0,
                "opening must still be moving outward, vel={vel_before}",
            );
            g.set_width(WindowWidth::Narrow);
            let vel_after = g
                .width_spring
                .as_ref()
                .map(Spring::velocity)
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
                g.width_spring
                    .as_ref()
                    .is_some_and(|s| s.target() < f32_of_f64(mid)),
                "Narrow on a 1200 px strip is smaller than the mid-open width",
            );
        }

        // Case `an_opening_slot_settles_when_only_its_own_rebuilds_step_it`.
        {
            // The row steps the slot spring only while it rebuilds the
            // layout, and rebuilds only when a group asks for it. Frames at
            // 60 and at 20 a second must both end the motion.
            for frame_ms in [16, 50] {
                let mut g = group_opening("Files", true, false);
                g.set_strip_usable_px(1200.0);
                g.start_spawn_animation();
                let mut rebuilds = 0;
                while std::mem::take(&mut g.style_dirty) && rebuilds < 200 {
                    g.last_width_step = g.last_width_step.map(|_| instant_ago_ms(frame_ms));
                    g.next_frame();
                    g.step_width_spring();
                    rebuilds += 1;
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

        // Case `a_tick_that_rebuilt_but_did_not_paint_does_not_freeze_the_slot`.
        {
            // The host's frame: rebuild when a widget asks (the row steps
            // the spring in its build, a frame may take two passes), then a
            // painted frame turns the token. A tick that rebuilds while the
            // presenter is still busy paints nothing; the step the next
            // rebuild skips must not leave the slot stopped part-way, asking
            // for frames forever.
            let mut g = group_opening("Notes", true, false);
            g.set_strip_usable_px(1200.0);
            g.start_spawn_animation();
            let mut frames = 0;
            while g.width_spring.is_some() && frames < 400 {
                g.last_width_step = g.last_width_step.map(|_| instant_ago_ms(16));
                if Widget::take_structure_dirty(&mut g) {
                    g.step_width_spring();
                }
                if frames % 7 != 3 {
                    g.frame_presented();
                }
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

        // Case `closing_collapses_the_slot_and_keeps_drawing`.
        {
            let mut g = group_opening("Files", false, false);
            g.set_strip_usable_px(1200.0);
            assert!(
                !painted_anything(&mut g),
                "precondition: an unpresented group with no opening card paints nothing",
            );
            let from = g.layout_width_px();
            g.start_closing();
            assert!(
                (g.layout_width_px() - from).abs() < 0.5,
                "close must start from the last reserved width, not jump to zero",
            );
            assert!(
                painted_anything(&mut g),
                "a closing group is drawn from its last frame while the slot collapses",
            );
            let mut last = g.layout_width_px();
            for _ in 0..8 {
                // Frames 16 ms apart on the spring's clock.
                g.last_width_step = g.last_width_step.map(|_| instant_ago_ms(16));
                g.next_frame();
                g.step_width_spring();
                let now = g.layout_width_px();
                assert!(now <= last, "close must collapse the slot, {last} → {now}");
                last = now;
            }
            assert!(
                last < from,
                "the slot must have started to collapse ({last} from {from})",
            );
        }

        // Case `reduced_motion_cuts_to_the_target`.
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
        g.start_spawn_animation();
        for frame in 0..40 {
            g.last_width_step = g.width_spring.as_ref().map(|_| instant_ago_ms(80));
            g.next_frame();
            g.step_width_spring();
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

    struct MaximizedControlFixture {
        group: WindowGroup,
        theme: Theme,
        taffy: TaffyTree<()>,
        node: NodeId,
        doc: AutoCommit,
        focus: Option<NodeId>,
        input_scale: f64,
    }

    impl MaximizedControlFixture {
        fn new(theme: Theme) -> Self {
            Self::with_group(make_group(), theme)
        }

        fn with_group(mut group: WindowGroup, theme: Theme) -> Self {
            group.maximized = true;
            group.last_cursor = Point::new(600.0, 49.0);
            // As if the person had reached for the bar.
            group.top_edge.force_shown(true);
            let mut fixture = Self {
                group,
                theme: theme.with_reduce_motion(true),
                taffy: TaffyTree::new(),
                node: NodeId::from(0u64),
                doc: AutoCommit::new(),
                focus: None,
                input_scale: 1.0,
            };
            fixture.layout();
            fixture.paint();
            fixture
        }

        fn layout(&mut self) {
            use taffy::prelude::TaffyMaxContent;
            let mut map = HashMap::new();
            self.taffy = TaffyTree::new();
            self.node = self
                .group
                .build_layout(&mut self.taffy, &self.theme, &mut map);
            let root = self.taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(1280.0),
                        height: Dimension::length(752.0),
                    },
                    ..Default::default()
                },
                &[self.node],
            );
            self.taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            self.group
                .apply_layout(&self.taffy, self.node, Point::new(0.0, 48.0));
        }

        fn scene(&mut self) -> Scene {
            let mut scene = Scene::new();
            self.group.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &self.theme,
                &PresenceCtx::empty(),
            );
            scene
        }

        fn paint(&mut self) {
            self.scene();
        }

        fn capture(&mut self, name: &str) {
            if let Some(path) = ux1x_out(&self.theme, name) {
                let scene = self.scene();
                let pixels = crate::offscreen::OffscreenRenderer::new(1280, 800)
                    .render(&scene, self.theme.tokens.colors.surfaces.base);
                write_png(&pixels, 1280, 800, &path);
            }
        }

        fn pointer(&mut self, action: crate::synthetic_input::PointerAction) -> bool {
            self.drive(|group, ctx| action.dispatch(group, ctx))
        }

        fn drive(
            &mut self,
            send: impl FnOnce(&mut WindowGroup, &mut crate::widget::EventCtx<'_>),
        ) -> bool {
            let mut handled = false;
            send(
                &mut self.group,
                &mut crate::widget::EventCtx {
                    scale_factor: self.input_scale,
                    modifiers: &winit::event::Modifiers::default(),
                    state: crate::widget::Edits::lend(&mut self.doc),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut self.focus,
                    node_id: self.node,
                    taffy: &self.taffy,
                },
            );
            handled
        }
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
    fn maximized_control_bounds_capture_visible_chrome_across_themes() {
        for (theme, size) in [
            (Theme::dark(), "default"),
            (Theme::light(), "default"),
            (Theme::dark().with_text_scale(2.0), "text-200"),
            (Theme::light().with_text_scale(2.0), "text-200"),
        ] {
            let mut fixture = MaximizedControlFixture::new(theme);
            let bounds = fixture.group.presentation_bounds();
            assert_eq!(bounds.group, Rect::new(0.0, 48.0, 1280.0, 800.0));
            let client = bounds.client.expect("active client laid out");
            assert!(client.height() > 0.0);
            for control in [bounds.restore, bounds.close] {
                let control = control.expect("visible painted control");
                assert_eq!(control.intersect(bounds.group), control);
                assert!(
                    client.contains(control.center())
                        && control.y0 >= client.y0 + crate::widget::top_edge_reveal::REVEAL_BAND_PX,
                    "the bar stands over the client's top edge, below the reveal band"
                );
                assert!(control.area() > 0.0);
            }
            assert!(bounds.maximize.is_none());
            assert_eq!(bounds.tab_bar.height(), 0.0);
            for _ in 0..3 {
                assert_eq!(fixture.group.presentation_bounds(), bounds);
            }
            assert_eq!(fixture.group.take_pending_maximize_change(), None);
            assert!(fixture.group.take_pending_close_commits().is_empty());
            fixture.capture(&format!("maximized-controls-{size}.png"));
        }
    }

    #[test]
    fn maximized_controls_require_a_completed_pointer_gesture() {
        use crate::synthetic_input::PointerAction;
        for close in [false, true] {
            let mut fixture = MaximizedControlFixture::new(Theme::dark());
            let bounds = fixture.group.presentation_bounds();
            let target = if close { bounds.close } else { bounds.restore }.unwrap();
            fixture.pointer(PointerAction::MoveTo(target.center()));
            assert!(fixture.pointer(PointerAction::Press(MouseButton::Left)));
            fixture.capture(if close {
                "maximized-close-held.png"
            } else {
                "maximized-restore-held.png"
            });
            assert!(
                fixture.group.maximized,
                "a press must await its matching release"
            );
            assert!(Widget::wants_pointer_capture(&fixture.group));
            assert!(fixture.group.take_pending_close_commits().is_empty());
            assert!(fixture.pointer(PointerAction::Release(MouseButton::Left)));
            assert!(!fixture.group.maximized);
            assert_eq!(fixture.group.take_pending_maximize_change(), Some(false));
            assert_eq!(
                fixture.group.take_pending_close_commits().len(),
                usize::from(close)
            );
            assert!(!Widget::wants_pointer_capture(&fixture.group));
        }
    }

    #[test]
    fn maximized_controls_cancel_interrupted_or_moved_gestures() {
        use crate::synthetic_input::PointerAction;
        for close in [false, true] {
            for interruption in 0..6 {
                let mut fixture = MaximizedControlFixture::new(Theme::dark());
                let bounds = fixture.group.presentation_bounds();
                let target = if close { bounds.close } else { bounds.restore }.unwrap();
                fixture.pointer(PointerAction::MoveTo(target.center()));
                fixture.pointer(PointerAction::Press(MouseButton::Left));
                match interruption {
                    0 => {
                        fixture.pointer(PointerAction::Leave);
                    }
                    1 => {
                        fixture.pointer(PointerAction::LoseFocus);
                    }
                    2 => {
                        fixture.pointer(PointerAction::MoveTo(bounds.client.unwrap().center()));
                        fixture.paint();
                        let hidden = fixture.group.presentation_bounds();
                        assert!(
                            hidden.restore.is_none() && hidden.close.is_none(),
                            "unpainted controls cannot remain pointer targets"
                        );
                    }
                    3 => {
                        fixture.theme =
                            Theme::light().with_text_scale(2.0).with_reduce_motion(true);
                        fixture.layout();
                        fixture.paint();
                        let moved = fixture.group.presentation_bounds();
                        let next = if close { moved.close } else { moved.restore }.unwrap();
                        assert_ne!(next, target);
                        fixture.pointer(PointerAction::MoveTo(next.center()));
                    }
                    4 => {
                        fixture.input_scale = 1.5;
                    }
                    5 => {
                        let escape =
                            crate::synthetic_input::named_key(winit::keyboard::NamedKey::Escape);
                        fixture.drive(|group, ctx| group.handle_event(&escape, ctx));
                    }
                    _ => unreachable!(),
                }
                assert!(
                    fixture.pointer(PointerAction::Release(MouseButton::Left)),
                    "the cancelled release stays owned by chrome"
                );
                assert!(fixture.group.maximized);
                assert_eq!(fixture.group.take_pending_maximize_change(), None);
                assert!(fixture.group.take_pending_close_commits().is_empty());
                assert!(!Widget::wants_pointer_capture(&fixture.group));
                if !close && matches!(interruption, 2 | 3 | 5) {
                    fixture.capture(&format!("maximized-restore-cancel-{interruption}.png"));
                }
                fixture.pointer(PointerAction::MoveTo(Point::new(600.0, 49.0)));
                // Reached for again.
                fixture.group.set_maximized_chrome_shown_for_tests(true);
                fixture.paint();
                let fresh = fixture.group.presentation_bounds();
                let fresh = if close { fresh.close } else { fresh.restore }.unwrap();
                fixture.pointer(PointerAction::MoveTo(fresh.center()));
                fixture.pointer(PointerAction::Press(MouseButton::Left));
                assert!(fixture.group.maximized);
                fixture.pointer(PointerAction::Release(MouseButton::Left));
                assert!(
                    !fixture.group.maximized,
                    "a fresh complete gesture still works"
                );
                assert_eq!(
                    fixture.group.take_pending_close_commits().len(),
                    usize::from(close)
                );
            }
        }
    }

    #[test]
    fn maximized_chrome_shows_near_the_top_with_tokenised_chips() {
        // Case `window_group_close_glyph_is_lucide_x`.
        {
            assert_eq!(WindowGroup::close_glyph_icon_for_tests(), icons::Icon::X,);
        }

        // Case `window_group_restore_pill_uses_minimize2`.
        {
            assert_eq!(
                WindowGroup::restore_glyph_icon_for_tests(),
                icons::Icon::Minimize2,
            );
        }

        // Case `maximized_close_chip_uses_danger_token_on_hover`.
        // Sampled from the **production paint path** on both presets.
        // At rest the × chip is the quiet chip face (the raised
        // surface under a rest edge that registers against it), not a literal
        // white tint; on hover it turns to the danger token and the × glyph
        // stays legible on that face, where the old opaque white only happened
        // to on the dark preset's red.
        {
            use crate::a11y::{contrast_ratio, relative_luminance};
            const EDGE_MIN: f64 = 0.008;
            let (w, h) = (620u32, 200u32);
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                let mut g = make_group();
                g.maximized = true;
                // The chrome paints while the cursor is in the top band.
                g.last_cursor = Point::new(600.0, 10.0);
                g.top_edge.force_shown(true);

                g.set_close_hover_for_tests(false);
                let idle = production_frame(&mut g, &theme, w, h);
                if let Some(png) = ux1x_out(&theme, "window_group_close_chip_idle.png") {
                    write_png(&idle, w, h, &png);
                }
                let chip = g.close_rect;
                assert!(
                    chip.width() > 0.0
                        && chip.x0 >= 0.0
                        && chip.y0 >= 0.0
                        && chip.x1 <= f64::from(w)
                        && chip.y1 <= f64::from(h),
                    "{}: the maximized × chip was laid out on the canvas: {chip:?} in {:?}",
                    theme.name,
                    g.rect
                );
                // Interior, on the chip's centre column just under its top edge —
                // inside the capsule and above the × glyph's box.
                let (cx, top) = (chip.center().x, chip.y0);
                let face = pixel(&idle, w, cx, top + 3.0);
                let quiet = crate::widget::ChipStyle::Neutral.colors(
                    &theme,
                    theme.tokens.colors.surfaces.content,
                    false,
                    false,
                );
                let (quiet_fill, quiet_edge) = (quiet.fill, quiet.edge);
                assert!(
                    contrast_ratio(face, quiet_fill) < 1.02,
                    "{}: idle × chip {chip:?} (group {:?}) paints the quiet chip face {quiet_fill:?}, got {face:?}",
                    theme.name,
                    g.rect
                );
                assert!(
                    quiet_edge.is_some(),
                    "{}: the quiet chip declares a rest edge",
                    theme.name
                );
                let edge = pixel(&idle, w, cx, top);
                let moved = (relative_luminance(edge) - relative_luminance(face)).abs();
                assert!(
                    moved >= EDGE_MIN,
                    "{}: the × chip's rest edge {edge:?} moved only {moved:.4} against its face {face:?}",
                    theme.name
                );

                g.set_close_hover_for_tests(true);
                let hover = production_frame(&mut g, &theme, w, h);
                if let Some(png) = ux1x_out(&theme, "window_group_close_chip_hover.png") {
                    write_png(&hover, w, h, &png);
                }
                let hot = pixel(&hover, w, cx, top + 3.0);
                // Hover lifts the chip, as every quiet control: closing a
                // window is not an error, so it is never danger red.
                assert!(
                    crate::chrome::lightness_step(hot, face) >= 0.04,
                    "{}: hover must visibly lift the × chip ({face:?} -> {hot:?})",
                    theme.name
                );
                let danger = theme.tokens.colors.semantic.danger;
                assert!(
                    contrast_ratio(hot, danger) > 1.2,
                    "{}: the hovered × chip {hot:?} is not danger red",
                    theme.name
                );
                // The × glyph stays readable on the hot face: the darkest or
                // lightest pixel in the glyph's central 5×5 block clears the
                // non-text minimum against the face beside it.
                let c = chip.center();
                let mut best = 1.0_f64;
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        let px = pixel(&hover, w, c.x + f64::from(dx), c.y + f64::from(dy));
                        best = best.max(contrast_ratio(px, hot));
                    }
                }
                assert!(
                    best >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                    "{}: the × glyph reads {best:.2}:1 on the hovered face {hot:?}",
                    theme.name
                );
            }
        }

        // Case `maximized_chrome_hides_at_rest_and_shows_on_a_rest_at_the_top`.
        {
            let mut g = make_group();
            g.maximized = true;
            g.rect = Rect::new(0.0, 48.0, 1200.0, 800.0);
            assert!(!g.maximized_chrome_shown(), "hidden before any pointer");
            assert!(!g.note_maximized_pointer(Point::new(600.0, 60.0)));
            assert!(!g.maximized_chrome_shown(), "hidden while in the app");
            assert!(
                !g.elapse_maximized_chrome_intent(),
                "a rest on the app's top row is the app's"
            );
            // Into the band, then settling there (the first sample may read
            // as a fast pass; the rest counts once the pointer settles).
            g.note_maximized_pointer(Point::new(600.0, 50.0));
            g.note_maximized_pointer(Point::new(600.0, 51.0));
            assert!(g.next_wake_deadline().is_some(), "the dwell wakes the loop");
            assert!(g.elapse_maximized_chrome_intent(), "a rest in the band");
            assert!(g.maximized_chrome_shown());
        }

        // Case `maximized_chrome_always_hidden_when_not_maximized`.
        {
            let mut g = make_group();
            assert!(!g.maximized, "fresh group is not maximized");
            g.rect = Rect::new(0.0, 48.0, 1200.0, 800.0);
            assert!(!g.note_maximized_pointer(Point::new(600.0, 50.0)));
            assert!(!g.elapse_maximized_chrome_intent());
            assert!(!g.maximized_chrome_shown());
        }

        // Case `maximized_chrome_chips_share_one_row_and_a_token_gap`.
        // The floating `[Restore][×]` row: both chips one row tall, the close
        // chip square, the pair one `spacing.s1` apart, and the row inset from
        // the card by the same `TAB_PILL_INSET_Y` the tab pills keep.
        {
            let mut g = make_group();
            g.maximized = true;
            g.rect = Rect::new(0.0, 0.0, 1200.0, 700.0);
            g.last_cursor = Point::new(600.0, 10.0);
            g.top_edge.force_shown(true);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            g.draw_maximized_chrome(&mut scene, Affine::IDENTITY, &mut tr, &theme);

            let inset = crate::widget::tabs::TAB_PILL_INSET_Y;
            assert_eq!(g.close_rect.height(), MAXIMIZED_CHIP_H);
            assert_eq!(g.restore_rect.height(), MAXIMIZED_CHIP_H);
            assert_eq!(
                g.close_rect.width(),
                g.close_rect.height(),
                "the × chip is square"
            );
            assert_eq!(g.close_rect.y0, g.rect.y0 + inset);
            assert_eq!(g.restore_rect.y0, g.close_rect.y0, "one row");
            assert!((g.rect.x1 - g.close_rect.x1 - inset).abs() < 1e-9);
            assert!(
                (g.close_rect.x0 - g.restore_rect.x1 - f64::from(theme.tokens.spacing.s1)).abs()
                    < 1e-9,
                "restore and close sit one s1 apart"
            );
            assert!(
                g.restore_rect.width() >= MAXIMIZED_RESTORE_CHIP_W,
                "the restore chip never undercuts its authored width"
            );
        }
        // Case `the_restore_chip_reads_in_the_desktops_language_and_fits_it`.
        {
            use crate::i18n::I18nManager;
            let restore_width_in = |locale: &str| {
                I18nManager::init(locale);
                let mut g = make_group();
                g.maximized = true;
                g.rect = Rect::new(0.0, 0.0, 1200.0, 700.0);
                g.last_cursor = Point::new(600.0, 10.0);
                g.top_edge.force_shown(true);
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                g.draw_maximized_chrome(&mut scene, Affine::IDENTITY, &mut tr, &theme);
                let label = crate::i18n::widget_format("widget-maximized-restore", &[]);
                let style = theme.text(TextRole::Caption);
                let (label_w, _) = tr.measure_style(&label, None, &style);
                let label_x0 = g.restore_rect.x0
                    + f64::from(theme.tokens.spacing.s2) * 2.0
                    + MAXIMIZED_ICON_PX;
                assert!(
                    label_x0 + f64::from(label_w) <= g.restore_rect.x1,
                    "{locale}: `{label}` must fit its chip {:?}",
                    g.restore_rect
                );
                (label, g.restore_rect.width())
            };
            let (en, en_w) = restore_width_in("en-US");
            let (de, de_w) = restore_width_in("de-DE");
            assert_eq!(en, "Restore");
            assert_eq!(de, "Wiederherstellen", "the chip reads German under de-DE");
            assert!(
                de_w > en_w,
                "the longer label widens the chip: {de_w} vs {en_w}"
            );
            I18nManager::init("en-US");
        }
    }

    #[test]
    fn closing_tab_hosts_fade_out_and_the_last_one_closes_the_group() {
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

        // Case `closing_host_stays_until_fade_complete`.
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

            // Simulates pressing the pill's × on the sole tab.
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
    }

    #[test]
    fn group_close_fade_paints_pumps_frames_and_completes() {
        // Case `group_close_complete_true_after_fade_duration`.
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

        // Case `group_needs_animation_frame_while_closing`.
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
            g.set_closing_at_for_tests(
                crate::time::now()
                    .checked_sub(g.close_duration.saturating_add(Duration::from_millis(20)))
                    .expect("representable close deadline"),
            );
            assert!(
                !g.needs_animation_frame(),
                "after the fade window elapses, frames are no longer required",
            );
        }

        // Case `window_group_needs_animation_frame_during_close`.
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
            g.closing_hosts
                .insert(1, instant_ago_ms(GROUP_CLOSE_FADE_MS.saturating_add(20)));
            assert!(
                !g.needs_animation_frame(),
                "after the fade window elapses the group goes idle again",
            );
        }

        // Case `maximized_close_uses_deferred_flow`.
        {
            use crate::synthetic_input::PointerAction;
            let tabs = vec![(
                TabDescriptor::new(1, "solo"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "a".into(),
                },
            )];
            let group = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
            let mut fixture = MaximizedControlFixture::with_group(group, Theme::dark());
            let close = fixture.group.presentation_bounds().close.unwrap();
            fixture.pointer(PointerAction::MoveTo(close.center()));
            assert!(fixture.pointer(PointerAction::Press(MouseButton::Left)));
            assert!(fixture.group.maximized);
            assert!(fixture.pointer(PointerAction::Release(MouseButton::Left)));
            let g = &mut fixture.group;
            assert!(!g.maximized, "completed close leaves maximize");
            assert!(
                g.take_structure_dirty(),
                "leaving maximize must rebuild the cached tiled layout",
            );
            assert!(
                g.has_host(1),
                "host stays in `hosts` for the fade duration — no synchronous removal",
            );
            assert!(
                g.bar.closing_tab_ids().contains(&1),
                "bar.start_closing must have latched the tab pill fade",
            );
            assert!(
                g.has_closing_host(1),
                "closing_hosts must carry the active id so the host fade runs",
            );
            assert_eq!(
                g.take_pending_close_commits(),
                vec![1],
                "the maximized close must also reach the shell's durable lifecycle path",
            );
            assert!(
                !g.is_closing(),
                "group close must NOT fire while a host-level close is still in flight",
            );

            g.force_expire_closing(1);
            fixture.pointer(PointerAction::MoveTo(Point::new(9000.0, 9000.0)));
            let g = &fixture.group;

            assert!(!g.has_host(1), "sweep drops the host once the fade expires");
            assert!(
                !g.bar.closing_tab_ids().contains(&1),
                "bar.finalize_close clears the closing pill entry",
            );
            assert!(g.bar.tabs().is_empty(), "bar has no live tabs left");
            assert!(
                g.is_closing(),
                "group-level close fade must latch once the last host has swept out",
            );
        }
    }

    /// Lays `g` out at `w`×`h`, draws it through the production
    /// [`Widget::draw`] and composites the scene on the theme's app ground —
    /// the frame a person sees, not a helper that restates the paint code.
    #[allow(
        clippy::cast_precision_loss,
        reason = "small fixed test-canvas dimensions, exactly representable in f32"
    )]
    fn production_frame(g: &mut WindowGroup, theme: &Theme, w: u32, h: u32) -> Vec<u8> {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        // A maximized group is `width: auto, flex_grow: 1e6`: it fills the
        // strip it sits in. Give it one — a sized flex parent, as the strip
        // is — or as a taffy root it collapses to 0 px and its chrome lands
        // off-canvas.
        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Style::default()
            },
            &[node],
        );
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
            )
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        let mut scene = Scene::new();
        g.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            &PresenceCtx::empty(),
        );
        crate::offscreen::OffscreenRenderer::new(w, h)
            .render(&scene, theme.tokens.colors.surfaces.base)
    }

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).expect("create requested screenshot evidence directory");
        Some(dir.join(name))
    }

    /// Writes an RGBA frame as a PNG — the by-eye half of the evidence.
    fn write_png(rgba: &[u8], w: u32, h: u32, path: &std::path::Path) {
        crate::png::write_rgba8(path, w, h, rgba).expect("write evidence png");
    }

    /// The composited colour at (`x`, `y`) of a `w`-wide RGBA frame.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside the test canvas"
    )]
    fn pixel(rgba: &[u8], w: u32, x: f64, y: f64) -> vello::peniko::Color {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        vello::peniko::Color::from_rgb8(r, g, b)
    }
}

/// The maximized window's bar: hidden at rest over a full-height client, and
/// one bar with a Tilo app's header row.
#[cfg(test)]
mod window_bar_tests {
    use super::*;
    use crate::widget::{AppKind, NavShell, TaffyTreeExt};

    /// A page of text that fills what it is given, so a bar over it shows.
    struct Page {
        rect: Rect,
    }

    impl Widget for Page {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.fill_leaf(node_map, self)
        }

        crate::rect_geometry!();

        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            tr: &mut TextRenderer,
            theme: &Theme,
            _presence: &PresenceCtx,
        ) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.content,
                None,
                &self.rect,
            );
            let style = theme.text(TextRole::Body);
            for line in 0..30u32 {
                let y = self.rect.y0 + 8.0 + f64::from(line) * 24.0;
                if y > self.rect.y1 - 24.0 {
                    break;
                }
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine `*` composes floating-point transform matrices"
                )]
                let at = transform * Affine::translate((self.rect.x0 + 24.0, y));
                tr.draw_style(
                    scene,
                    &format!("{line:02}  The quick brown fox jumps over the lazy dog."),
                    theme.tokens.colors.text_primary,
                    at,
                    None,
                    &style,
                );
            }
        }

        fn name(&self) -> &'static str {
            "Page"
        }
    }

    /// A Tilo app drawn in-process: a `NavShell` with a sidebar, a content
    /// column and their header rows.
    struct HeaderedApp {
        rect: Rect,
        shell: NavShell,
    }

    impl HeaderedApp {
        fn new(header_in_bar: bool) -> Self {
            let shell = NavShell::new()
                .sidebar(Box::new(Page { rect: Rect::ZERO }))
                .content(Box::new(Page { rect: Rect::ZERO }))
                .sidebar_header("Notes", Vec::new())
                .content_header(
                    "Groceries",
                    vec![Box::new(crate::widget::Button::icon_only(
                        "more",
                        crate::icons::Icon::MoreHorizontal,
                        "More",
                    ))],
                );
            let shell = if header_in_bar {
                shell.with_header_in_window_bar()
            } else {
                shell
            };
            Self {
                rect: Rect::ZERO,
                shell,
            }
        }
    }

    impl Widget for HeaderedApp {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let child = self.shell.build_layout(taffy, theme, node_map);
            let style = Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            };
            taffy.widget_node(style, &[child], node_map, self)
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.rect = crate::widget::layout_rect(taffy, node, origin);
            if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
                self.shell
                    .apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
            }
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            tr: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.shell.draw(scene, transform, tr, theme, presence);
        }

        fn children(&self) -> Vec<&dyn Widget> {
            vec![&self.shell]
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            vec![&mut self.shell]
        }

        fn take_structure_dirty(&mut self) -> bool {
            self.shell.take_structure_dirty()
        }

        fn name(&self) -> &'static str {
            "HeaderedApp"
        }
    }

    impl crate::widget::app_widget::AppWidget for HeaderedApp {
        fn uses_host_record(&self) -> bool {
            false
        }
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Notes"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("N")
        }
    }

    /// The desktop's top bar the maximized window sits under, in logical px.
    const TOP: f64 = 36.0;
    const W: f64 = 1280.0;
    const H: f64 = 800.0;

    fn group(content: TileContent) -> WindowGroup {
        let mut g = WindowGroup::new(
            10,
            vec![(TabDescriptor::new(1, "Notes"), content)],
            1,
            WindowWidth::Normal,
        );
        g.maximized = true;
        g
    }

    fn page_group() -> WindowGroup {
        group(TileContent::App(Box::new(PageApp(Page {
            rect: Rect::ZERO,
        }))))
    }

    /// A foreign-like app: no header row to merge with.
    struct PageApp(Page);

    impl Widget for PageApp {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.0.build_layout(taffy, theme, node_map)
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.0.apply_layout(taffy, node, origin);
        }
        fn get_rect(&self) -> Rect {
            self.0.get_rect()
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            tr: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.0.draw(scene, transform, tr, theme, presence);
        }
        fn name(&self) -> &'static str {
            "PageApp"
        }
    }

    impl crate::widget::app_widget::AppWidget for PageApp {
        fn uses_host_record(&self) -> bool {
            false
        }
        fn app_kind(&self) -> AppKind {
            AppKind::Terminal
        }
        fn app_title(&self) -> &'static str {
            "Terminal"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("T")
        }
    }

    /// Lay the group out as the work area under the desktop's top bar, at
    /// `scale` (the layout is logical; the scale only matters for paint).
    fn lay_out(g: &mut WindowGroup, theme: &Theme) {
        // Twice: a NavShell plans against the size it was last given.
        for _ in 0..2 {
            let _ = g.take_structure_dirty();
            let mut taffy = TaffyTree::<()>::new();
            let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, theme, &mut map);
            // The strip gives a maximized group the work area.
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.size = Size {
                width: Dimension::length(f32_of_f64(W)),
                height: Dimension::length(f32_of_f64(H - TOP)),
            };
            let _ = taffy.set_style(node, style);
            let available = Size {
                width: taffy::AvailableSpace::Definite(f32_of_f64(W)),
                height: taffy::AvailableSpace::Definite(f32_of_f64(H - TOP)),
            };
            let mut text = TextRenderer::new();
            let mut bindings = crate::bound_dispatch::BoundNodeMap::default();
            // SAFETY: `map` describes the tree just built from `g`, which this
            // function holds exclusively.
            unsafe {
                bindings.rebuild(g, &taffy, node, &map);
                crate::widget::layout::compute_widget_layout(
                    &mut taffy, node, available, &map, &bindings, &mut text,
                )
                .unwrap();
            }
            g.apply_layout(&taffy, node, Point::new(0.0, TOP));
        }
    }

    fn paint(g: &mut WindowGroup, theme: &Theme, scale: f64) -> Scene {
        let mut scene = Scene::new();
        let t = Affine::scale(scale);
        // The desktop's top bar above the window, so a gap would show.
        scene.fill(
            vello::peniko::Fill::NonZero,
            t,
            theme.tokens.colors.surfaces.base,
            None,
            &Rect::new(0.0, 0.0, W, TOP),
        );
        g.draw(
            &mut scene,
            t,
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }

    #[test]
    fn a_maximized_client_fills_the_work_area_at_1x_and_2x() {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_reduce_motion(true);
            let mut g = page_group();
            lay_out(&mut g, &theme);
            let _ = paint(&mut g, &theme, scale);
            let work_area = Rect::new(0.0, TOP, W, H);
            assert_eq!(
                g.get_rect(),
                work_area,
                "{scale}x: the window is the work area"
            );
            assert_eq!(
                g.active_host().unwrap().get_rect(),
                work_area,
                "{scale}x: the client fills it: no lane above, no gap below"
            );
            assert!(!g.maximized_chrome_shown(), "{scale}x: no bar at rest");
            assert_eq!(g.presentation_bounds().restore, None);
        }
    }

    #[test]
    fn the_bar_reveals_over_the_client_on_intent_and_holds_under_aim() {
        let theme = Theme::dark().with_reduce_motion(true);
        let mut g = page_group();
        lay_out(&mut g, &theme);
        let _ = paint(&mut g, &theme, 1.0);
        // A synthetic timeline: one pointer sample per 16 ms frame.
        let t0 = crate::time::now();
        let mut frame = 0u64;
        let mut at = |g: &mut WindowGroup, p: Point| {
            frame += 1;
            g.note_maximized_pointer_at(p, t0 + Duration::from_millis(16 * frame));
        };
        // Aiming at the app's top content: never.
        at(&mut g, Point::new(400.0, TOP + 30.0));
        at(&mut g, Point::new(401.0, TOP + 24.0));
        assert!(!g.elapse_maximized_chrome_intent());
        let _ = paint(&mut g, &theme, 1.0);
        assert!(!g.maximized_chrome_captures(Point::new(401.0, TOP + 17.0)));
        // A rest at the very top: the bar, over the client's top edge.
        at(&mut g, Point::new(400.0, TOP + 2.0));
        at(&mut g, Point::new(401.0, TOP + 2.0));
        assert!(g.elapse_maximized_chrome_intent());
        let _ = paint(&mut g, &theme, 1.0);
        let restore = g.presentation_bounds().restore.expect("Restore shows");
        let client = g.active_host().unwrap().get_rect();
        assert!(
            client.contains(restore.center()),
            "over the client, not above it"
        );
        assert!(restore.y0 >= TOP + crate::widget::top_edge_reveal::REVEAL_BAND_PX);
        // Across the app's top row to Restore: off every chip on the way,
        // but heading for it the whole time, so the bar holds.
        let title = g.title_rect;
        assert!(title.area() > 0.0, "the floating bar names the window");
        let from = Point::new(401.0, TOP + 2.0);
        let to = Point::new(restore.x0 - 30.0, restore.center().y);
        for step in 1..=10u32 {
            let p = from.lerp(to, f64::from(step) / 10.0);
            at(&mut g, p);
            assert!(g.maximized_chrome_shown(), "heading for Restore at {p:?}");
        }
        for dx in [-12.0, -4.0, 0.0] {
            at(&mut g, restore.center() + (dx, 0.0));
        }
        let _ = paint(&mut g, &theme, 1.0);
        assert!(g.maximized_chrome_captures(restore.center()));
        // Down into the app: gone.
        at(&mut g, Point::new(restore.center().x, 300.0));
        at(&mut g, Point::new(restore.center().x, 400.0));
        assert!(!g.maximized_chrome_shown());
    }

    #[test]
    fn the_bar_stands_below_a_desktop_bar_shown_over_the_window() {
        let theme = Theme::dark().with_reduce_motion(true);
        let mut g = page_group();
        lay_out(&mut g, &theme);
        g.set_maximized_chrome_shown_for_tests(true);
        let _ = paint(&mut g, &theme, 1.0);
        let flush = g.presentation_bounds().restore.expect("shown");
        g.set_top_overlay_inset(36.0);
        let _ = paint(&mut g, &theme, 1.0);
        let below = g.presentation_bounds().restore.expect("shown");
        assert!(
            (below.y0 - flush.y0 - 36.0).abs() < 1e-9,
            "{flush:?} -> {below:?}"
        );
    }

    #[test]
    fn a_tilo_app_shows_one_bar_with_its_header() {
        for header_in_bar in [false, true] {
            let theme = Theme::dark().with_reduce_motion(true);
            let mut g = group(TileContent::App(Box::new(HeaderedApp::new(header_in_bar))));
            lay_out(&mut g, &theme);
            let _ = paint(&mut g, &theme, 1.0);
            let slot = g
                .bar_slot
                .expect("the app's header row took the window's bar");
            assert_eq!(slot.hides_with_bar, header_in_bar);
            assert_eq!(slot.row.y0, TOP, "the bar is the window's top row");
            assert!(slot.controls.x1 <= W && slot.controls.y1 <= slot.row.y1);
            assert!(!g.maximized_chrome_shown());

            g.set_maximized_chrome_shown_for_tests(true);
            lay_out(&mut g, &theme);
            let _ = paint(&mut g, &theme, 1.0);
            let bounds = g.presentation_bounds();
            let restore = bounds.restore.expect("Restore in the header row");
            let close = bounds.close.expect("close in the header row");
            for control in [restore, close, g.fullscreen_rect] {
                assert!(
                    slot.controls.contains(control.center()) && slot.row.contains(control.center()),
                    "{header_in_bar}: {control:?} stands in the row's slot {slot:?}"
                );
            }
            assert_eq!(
                g.title_rect,
                Rect::ZERO,
                "{header_in_bar}: no floating title chip: the header names the window"
            );
            assert_eq!(
                g.active_host().unwrap().get_rect(),
                Rect::new(0.0, TOP, W, H),
                "the client still fills the window"
            );
        }
    }

    /// Review frames: a maximized foreign-like window at rest (1× and 2×),
    /// the bar revealed and its Restore hovered, and a Tilo app with its
    /// header in the bar at rest and revealed, plus one with a permanent
    /// header and the controls merged into its row.
    #[test]
    #[ignore = "a review tool: needs a GPU adapter; writes PNGs to TILO_UI_SCREENSHOT_DIR"]
    fn render_maximized_bar_frames() {
        let dir = std::path::PathBuf::from(
            std::env::var("TILO_UI_SCREENSHOT_DIR").expect("TILO_UI_SCREENSHOT_DIR"),
        );
        std::fs::create_dir_all(&dir).expect("shot dir");
        let theme = Theme::dark().with_reduce_motion(true);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "small fixed canvas"
        )]
        let shoot = |g: &mut WindowGroup, scale: f64, name: &str, pointer: Option<Point>| {
            let (pw, ph) = ((W * scale) as u32, (H * scale) as u32);
            let mut renderer =
                crate::offscreen::OffscreenRenderer::try_new(pw, ph).expect("a GPU adapter");
            let mut scene = paint(g, &theme, scale);
            if let Some(p) = pointer {
                let dot = vello::kurbo::Circle::new(p, 4.0);
                let t = Affine::scale(scale);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    t,
                    vello::peniko::Color::from_rgb8(255, 210, 0),
                    None,
                    &dot,
                );
            }
            let rgba = renderer.render(&scene, vello::peniko::Color::BLACK);
            crate::png::write_rgba8(dir.join(format!("{name}.png")), pw, ph, &rgba)
                .expect("write frame");
        };

        for scale in [1.0, 2.0] {
            let mut g = page_group();
            lay_out(&mut g, &theme);
            shoot(&mut g, scale, &format!("maximized-rest-{scale}x"), None);
        }
        // A sequence: pointer on the app's top row, resting in the band,
        // the bar shown, Restore hovered, then down into the app.
        let mut g = page_group();
        lay_out(&mut g, &theme);
        let _ = paint(&mut g, &theme, 1.0);
        let top_row = Point::new(700.0, TOP + 22.0);
        g.note_maximized_pointer(top_row);
        let _ = g.elapse_maximized_chrome_intent();
        shoot(&mut g, 2.0, "seq-1-aiming-at-app-top-row", Some(top_row));
        let band = Point::new(1100.0, TOP + 2.0);
        g.note_maximized_pointer(band);
        g.note_maximized_pointer(Point::new(1101.0, TOP + 2.0));
        shoot(&mut g, 2.0, "seq-2-resting-in-band", Some(band));
        let _ = g.elapse_maximized_chrome_intent();
        shoot(&mut g, 2.0, "seq-3-bar-shown", Some(band));
        let restore = g.presentation_bounds().restore.expect("shown");
        g.note_maximized_pointer(restore.center());
        g.restore_hover = true;
        shoot(&mut g, 2.0, "seq-4-restore-hovered", Some(restore.center()));
        g.restore_hover = false;
        let below = Point::new(restore.center().x, 400.0);
        g.note_maximized_pointer(Point::new(restore.center().x, 300.0));
        g.note_maximized_pointer(below);
        shoot(&mut g, 2.0, "seq-5-back-in-app", Some(below));

        for (header_in_bar, name) in [
            (true, "notes-header-in-bar"),
            (false, "files-permanent-header"),
        ] {
            let mut g = group(TileContent::App(Box::new(HeaderedApp::new(header_in_bar))));
            lay_out(&mut g, &theme);
            shoot(&mut g, 2.0, &format!("{name}-rest"), None);
            g.set_maximized_chrome_shown_for_tests(true);
            lay_out(&mut g, &theme);
            let _ = paint(&mut g, &theme, 2.0);
            shoot(
                &mut g,
                2.0,
                &format!("{name}-revealed"),
                Some(Point::new(900.0, TOP + 2.0)),
            );
        }
    }
}
