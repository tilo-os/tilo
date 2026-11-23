use crate::icons;
use crate::num::f32_of_f64;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::DrawChild;
use crate::widget::TaffyTreeExt;
use crate::widget::edge_resize::{WidthLimits, WindowEdge};
use crate::widget::tabs::close_fade;
use crate::widget::window_controls::WindowControl;
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

/// Immutable presentation inspection in root logical coordinates.
/// Reading this does not lay out, paint, reveal controls or drain input edges.
/// An edge drag a [`WindowGroup`]'s row is carrying out on it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GroupEdgeDrag {
    moving: WindowEdge,
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

/// Group widths are the shell model's: a fraction of the strip, never
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

/// The lifted handle remains readable while exposing the drop target; the app
/// pixels beneath it recede further so the target never disappears.
const DRAG_CONTENT_ALPHA: f32 = 0.42;
const DRAG_SOURCE_CONTENT_ALPHA: f32 = 0.18;
/// End scale of the fast lift curve. The content tucks under the full-size tab
/// handle instead of turning into a second opaque window under the pointer.
const DRAG_CONTENT_SCALE_END: f64 = 0.96;

fn window_progress(direction: crate::motion::Direction, elapsed: Duration) -> f64 {
    crate::motion::WindowMotion::OPEN.progress_after(
        direction,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
        elapsed,
    )
}

fn window_close_time() -> Duration {
    crate::motion::WindowMotion::OPEN.settle_time(
        crate::motion::Direction::Exit,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
    )
}

fn group_close_fade() -> Duration {
    close_fade()
}

#[path = "window_group_frame_spring.rs"]
mod frame_spring;
#[path = "window_group_held_bar.rs"]
mod held_bar;
#[path = "window_group_held_controls.rs"]
mod held_controls;
#[path = "window_group_opened_by.rs"]
mod opened_by;
use frame_spring::FrameSpring;
use held_controls::HeldControl;
use opened_by::OpenedByStop;

impl WindowGroup {
    #[cfg(test)]
    pub(crate) fn has_width_spring_for_tests(&self) -> bool {
        self.width_spring.is_some()
    }

    pub(crate) fn offset_layout(&mut self, dx: f64) {
        if dx == 0.0 {
            return;
        }
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.rect = self.rect + shift;
        self.bar.offset_layout(dx);
        self.search.offset_layout(dx);
        self.offset_held_controls(dx);
        for (_, host) in &mut self.hosts {
            host.offset_layout(dx);
        }
    }

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

const SEARCH_CONTROL_WIDTH: f64 =
    super::window_controls::CONTROL_PX + super::window_controls::CONTROL_GAP;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent UI flags on a widely-shared pub struct; not a state bitset"
)]
#[crate::component::not_a_component(shell, "a group of windows")]
pub struct WindowGroup {
    external_navigation: bool,
    /// Transient presentation from the parent row; never a persisted maximize.
    fullscreen_presentation: bool,
    /// While the person reaches for the shell's bar over a fullscreen window: how far below the
    /// window's top edge its own bar stands, under the shell's bar ([`Self::set_fullscreen_bar`]).
    pub(crate) fullscreen_bar: Option<f64>,
    /// The window's own exit-full-screen control was pressed, for the shell to carry out.
    pending_exit_fullscreen: bool,
    close_reports: Vec<crate::application::ComponentCloseReport>,
    pub id: u64,
    bar: TabBar,
    held_bar: Option<crate::widget::WindowBarPart>,
    held: crate::widget::window_header::RowFurniture,
    held_controls: [HeldControl; crate::widget::window_header::ROW_CONTROLS.len()],
    opened_by: OpenedByStop,
    held_hover: Option<WindowControl>,
    held_pressed: Option<WindowControl>,
    held_new_tab_hover: bool,
    held_new_tab_pressed: bool,
    held_dirty: bool,
    /// Each tab's host, boxed so it keeps its address while it stays in the
    /// window: the frame's layout pointer map names the shown host by
    /// address, and opening, closing or reordering another tab must not
    /// move it out from under that map.
    hosts: Vec<(u64, Box<TileHost>)>,
    width: WindowWidth,
    strip_usable_px: f64,
    alone: bool,
    width_before_share: Option<WindowWidth>,
    rect: Rect,
    edge_drag: Option<GroupEdgeDrag>,
    edge_highlight: Option<(WindowEdge, Instant)>,
    edge_lit_drawn: bool,
    size_label_fade: Option<(Instant, WindowEdge, (f64, f64))>,
    grip_dirty: bool,
    pub maximized: bool,
    floating: bool,
    collapsed: bool,
    opens_tabs: bool,
    pending_snap: bool,
    pub search_active: bool,
    pub focused: bool,
    top_overlay_inset: f64,
    /// A maximized window moves down while the shell's bar shows over its
    /// top edge, so its own bar is never under the shell's: where the
    /// slide started, and when. It moves, it is not resized, so the app is
    /// not laid out again for a bar that comes and goes.
    top_slide_from: f64,
    top_slide_since: Option<Instant>,
    /// Nothing of the shell's stands beside a maximized window on its
    /// display (the top bar is not docked above it): maximized, it fills the
    /// display and draws edge to edge ([`Widget::set_full_bleed`]).
    fills_display: bool,
    last_cursor: Point,
    /// A tab just opened in this window that has not drawn yet, and the tab
    /// shown before it: the window keeps showing that one meanwhile, so its
    /// body is never blank.
    tab_waiting: Option<(u64, u64)>,
    tab_fade: Option<crate::motion::CrossFade<u64>>,
    rect_spring: Option<FrameSpring<Rect>>,
    width_spring: Option<FrameSpring<f64>>,
    /// The width the window's bar and content are laid out at while its part width is in flight:
    /// the wide end of the motion. A widening window reflows to the width it is going to as the
    /// motion starts, its app's new frame arriving under the growing part's clip; a narrowing one
    /// keeps the width it had, clipped by the part, and reflows once it rests. Never on every
    /// frame; read only while `width_spring` is stepping.
    content_px: f64,
    /// The width the window had when it began to restore from maximized:
    /// what it stays laid out at, clipped by its shrinking frame, until the
    /// frame rests. Read only while `rect_spring` is set or
    /// `rest_layout_owed`, and the window is not maximized.
    rect_from_width: f64,
    rest_layout_owed: bool,
    closing_hosts: HashMap<u64, Instant>,
    style_dirty: bool,
    bar_offer_dirty: bool,
    built_active: Option<u64>,
    node: Option<NodeId>,
    laid_shape: Option<(bool, bool, u64)>,
    child_styles: Vec<(NodeId, Style)>,
    pending_maximize_change: Option<bool>,
    /// Per-mille, not px — a peer's output may be a different size.
    pending_width_change: Option<WindowWidth>,
    pending_search_open: Option<super::SearchHandoff>,
    search: super::WindowSearch,
    search_tab: Option<u64>,
    window_offers: Vec<(u64, crate::window_toolbar::WindowOffer)>,
    window_events: Vec<(u64, crate::window_toolbar::ToolbarEvent)>,
    pending_new_tab: Option<u64>,
    pending_tab_reorder: Option<(usize, usize)>,
    pending_close_commit: Vec<u64>,
    /// The resource the window's bar carries when a person picks it up, by its published
    /// address: what a press that travels publishes on the drag bus, so the pointer carries the
    /// window to the device beside this one as it rearranges the strip here. `None` for a window
    /// whose bar carries nothing (a foreign window's).
    carries: Option<String>,
    closing_at: Option<Instant>,
    close_duration: Duration,
    /// The close was a shortcut's or an action's: it lands, so the group is
    /// gone on the next frame instead of fading out.
    close_lands: bool,
    width_stepped: bool,
    close_rebuild_taken: bool,
    /// Last motion policy observed from the active theme. Widget scheduling
    /// has no `Theme` parameter, so draw latches it here and the next
    /// `needs_animation_frame` query can avoid waking for transitions that
    /// must snap to their endpoint.
    reduce_motion: bool,
    motion_source: crate::tokens::MotionSource,
}

const EDGE_HOVER_DWELL: Duration = Duration::from_millis(120);
const EDGE_LIT_PX: f64 = 2.0;

impl WindowGroup {
    /// Paint a whole group while its window handle is being reordered. The
    /// handle remains half-transparent and the app pixels recede further,
    /// matching the tab-drag visual without duplicating layout state.
    pub(crate) fn draw_group_drag_visual(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        crate::surface::shadow(
            scene,
            transform,
            self.rect,
            f64::from(theme.tokens.radii.tile),
            &theme.tokens.elevation.floating,
        );
        if !self.maximized && self.navigation_bar_visible() {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                TabBar::GHOST_PILL_ALPHA,
                transform,
                &self.bar_rect().inflate(32.0, 32.0),
            );
            self.bar.draw_child(scene, transform, tr, theme,&env);
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
            host.draw_child(scene, transform, tr, theme,&env);
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
                (d.id, Box::new(h))
            })
            .collect();
        let mut bar = TabBar::new(&format!("g{id}"), descs, active_id);
        bar.set_drag_overlay_owned_by_parent(true);
        bar.set_window_controls(true);
        bar.set_search_control(Some(SEARCH_CONTROL_WIDTH));
        Self {
            id,
            close_reports: vec![],
            bar,
            held_bar: None,
            held: crate::widget::window_header::RowFurniture::default(),
            held_controls: crate::widget::window_header::ROW_CONTROLS.map(HeldControl::new),
            opened_by: OpenedByStop::default(),
            held_hover: None,
            held_pressed: None,
            held_new_tab_hover: false,
            held_new_tab_pressed: false,
            held_dirty: false,
            hosts,
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            alone: false,
            width_before_share: None,
            rect: Rect::ZERO,
            external_navigation: false,
            fullscreen_presentation: false,
            fullscreen_bar: None,
            pending_exit_fullscreen: false,
            edge_drag: None,
            edge_highlight: None,
            edge_lit_drawn: false,
            size_label_fade: None,
            grip_dirty: false,
            maximized: false,
            floating: false,
            collapsed: false,
            opens_tabs: true,
            pending_snap: false,
            search_active: false,
            focused: false,
            top_overlay_inset: 0.0,
            top_slide_from: 0.0,
            top_slide_since: None,
            fills_display: false,
            last_cursor: Point::ZERO,
            tab_waiting: None,
            tab_fade: None,
            rect_spring: None,
            width_spring: None,
            content_px: 0.0,
            rect_from_width: 0.0,
            rest_layout_owed: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            width_stepped: false,
            close_rebuild_taken: false,
            close_duration: window_close_time(),
            close_lands: false,
            reduce_motion: false,
            motion_source: crate::tokens::MotionSource::default(),
            style_dirty: false,
            bar_offer_dirty: false,
            built_active: Some(active_id),
            node: None,
            laid_shape: None,
            child_styles: Vec::new(),
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
            carries: None,
        }
    }
    pub fn width(&self) -> WindowWidth {
        self.width
    }

    /// The window shows its bar: its own band above the content, or the row its app draws at its
    /// top ([`crate::widget::window_header::content_bar`]). One bar per window, never two.
    fn shows_its_bar(&self) -> bool {
        !self.external_navigation
            && (!self.fullscreen_presentation || self.fullscreen_bar.is_some())
    }

    fn navigation_bar_visible(&self) -> bool {
        self.held_bar.is_none() && self.shows_its_bar()
    }

    pub(crate) fn set_fullscreen_presentation(&mut self, fullscreen: bool) {
        if self.fullscreen_presentation != fullscreen {
            self.fullscreen_presentation = fullscreen;
            self.style_dirty = true;
            self.bar.set_resize(self.resize());
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
        if self.focused == focused {
            return false;
        }
        self.focused = focused;
        self.sync_host_keyboard();
        true
    }

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
        self.arm_width_from_zero();
    }

    /// Whether the window stands on screen: something in it has drawn, or
    /// its place is reserved for content still on its way. Only a first
    /// frame from this device, inside its frame budget, keeps it off.
    #[must_use]
    pub fn stands(&self) -> bool {
        Widget::presented(self) || self.holds_first_frame_until(crate::time::now()).is_none()
    }

    fn holds_first_frame_until(&self, now: Instant) -> Option<Instant> {
        if Widget::presented(self) || self.is_closing() {
            return None;
        }
        self.active_host()?.holds_first_frame_until(now)
    }

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
        }
    }

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

    /// Width the part is going to, in the same pixels as
    /// [`Self::layout_width_px`]. A later usable-basis change does not
    /// rewrite an in-flight target, so overflow can use this instead of
    /// the current fraction.
    pub fn dest_width_px(&self) -> f64 {
        if let Some(spring) = &self.width_spring {
            return spring.target().max(0.0);
        }
        self.layout_width_px()
    }

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
    /// window keeps the width it had until its frame rests, and a part in
    /// flight lays out at the wide end of its motion.
    fn content_width_in_flight(&self) -> Option<f64> {
        if self.edge_drag.is_some() {
            return None;
        }
        if self.rect_spring.is_some() || self.rest_layout_owed {
            return (!self.maximized && self.rect_from_width > 1.0).then_some(self.rect_from_width);
        }
        if self.maximized {
            return None;
        }
        let part = self.width_spring.as_ref()?.value().max(0.0);
        ((self.content_px - part).abs() > 0.5).then_some(self.content_px)
    }

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

    /// A frame was presented by a host that never marks this group drawn
    /// ([`super::GroupRow::frame_presented`]), so the next frame may step the width spring. If a
    /// rebuild skipped its step because the token was still spent, nothing has asked for the
    /// rebuild that would step it, and the spring would stay where it stopped while the group kept
    /// asking for animation frames.
    pub fn frame_presented(&mut self) {
        self.next_frame();
    }

    pub fn next_frame(&mut self) {
        if !self.reduce_motion && self.width_spring.as_ref().is_some_and(|s| !s.is_settled()) {
            self.style_dirty = true;
        }
        self.width_stepped = false;
        self.close_rebuild_taken = false;
    }

    pub fn is_closing(&self) -> bool {
        self.closing_at.is_some()
    }

    pub fn close_complete(&self) -> bool {
        self.closing_at.is_some_and(|t| {
            self.reduce_motion
                || self.close_lands
                || (crate::time::now().saturating_duration_since(t) >= self.close_duration
                    && self.part_at_rest())
        })
    }

    fn part_at_rest(&self) -> bool {
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

    pub fn set_top_overlay_inset(&mut self, inset: f64) {
        let inset = inset.max(0.0);
        if (self.top_overlay_inset - inset).abs() > f64::EPSILON {
            self.top_slide_from = self.top_slide_now();
            self.top_slide_since = Some(crate::time::now());
            self.top_overlay_inset = inset;
            self.style_dirty = true;
            self.grip_dirty = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn top_overlay_inset(&self) -> f64 {
        self.top_overlay_inset
    }

    fn draw_bar(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        self.bar.set_resize(self.resize());
        self.bar
            .set_group_focused(self.focused || self.search_active);
        self.bar.set_popup_area(self.rect);
        self.bar.draw_child(scene, transform, tr, theme,&env);
        self.search.set_ground(self.bar.ground_color(theme));
        self.search
            .draw_child(scene, transform, tr, theme,&env);
    }

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

    fn offer_history_controls(&mut self) {
        let tab_row = self.navigation_bar_visible();
        for (id, host) in &mut self.hosts {
            host.set_window_presents_history(tab_row);
            let control = match self.window_offers.iter().find(|(tab, _)| tab == id) {
                Some((_, offer)) => offer.history.map(super::HistoryControl::from),
                None => host.history_control(),
            };
            self.bar.set_history_control(*id, control);
        }
    }

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

    pub fn strip_usable_px(&self) -> f64 {
        self.strip_usable_px
    }

    pub fn set_strip_usable_px(&mut self, px: f64) {
        let px = px.max(1.0);
        if (self.strip_usable_px - px).abs() > 0.5 {
            self.strip_usable_px = px;
            self.style_dirty = true;
        }
    }

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

    /// Light up (or, with `None`, put out) the edge of this window a press
    /// would resize, once the pointer has rested there for
    /// `EDGE_HOVER_DWELL`. Set by the parent row from
    /// [`super::edge_resize::boundary_owner`]; the window itself never decides.
    pub fn set_edge_highlight(&mut self, edge: Option<WindowEdge>) {
        if self.edge_highlight.map(|(edge, _)| edge) != edge {
            self.edge_highlight = edge.map(|edge| (edge, crate::time::now()));
            self.edge_lit_drawn = false;
            self.grip_dirty = true;
        }
    }

    pub fn edge_highlight(&self) -> Option<WindowEdge> {
        self.edge_drag
            .map(|drag| drag.moving)
            .or(self.edge_highlight.map(|(edge, _)| edge))
    }

    fn edge_reveal_at(&self) -> Option<Instant> {
        let (_, since) = self.edge_highlight?;
        (self.edge_drag.is_none() && !self.edge_lit_drawn)
            .then(|| since.checked_add(EDGE_HOVER_DWELL))
            .flatten()
    }

    fn lit_edge(&self, now: Instant) -> Option<WindowEdge> {
        if let Some(drag) = self.edge_drag {
            return Some(drag.moving);
        }
        let (edge, since) = self.edge_highlight?;
        (now.saturating_duration_since(since) >= EDGE_HOVER_DWELL
            || crate::motion::testing::is_settled())
        .then_some(edge)
    }

    fn size_label_fading(&self, now: Instant) -> Option<f64> {
        let (released, ..) = self.size_label_fade?;
        let progress = crate::motion::CROSS_FADE.progress_after(
            crate::motion::Direction::Exit,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
            now.saturating_duration_since(released),
        );
        (!self.reduce_motion && progress < 1.0).then_some(progress)
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
        self.size_label_fade = None;
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

    pub fn edge_drag_origin_px(&self) -> Option<f64> {
        self.edge_drag.map(|drag| drag.live_px)
    }

    /// End an edge drag. Committed, the width is stored as the share of the strip it landed on and
    /// handed to the shell to persist; cancelled (an Escape, focus loss), the window goes back to
    /// the width it had. Returns the change in laid-out width the commit itself makes, since the
    /// stored share resolves to whole pixels on its own grid.
    pub fn end_edge_drag(&mut self, commit: bool) -> f64 {
        let Some(drag) = self.edge_drag.take() else {
            return 0.0;
        };
        self.size_label_fade = Some((
            crate::time::now(),
            drag.moving,
            (drag.live_px, self.rect.height().round()),
        ));
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

    fn release_live_resize_anchor(&mut self) {
        for (_, host) in &mut self.hosts {
            if let Some(app) = host.app_mut() {
                app.release_live_resize_anchor();
            }
        }
    }

    /// Narrow (or widen) this window to `w` to share the screen with a
    /// window opened beside it, remembering a width the person chose so that
    /// they get it back once the window is alone on the strip again. A
    /// default width needs no remembering: a [`WindowWidth::Share`] is
    /// already the shell's, and a lone one takes the strip
    /// ([`WindowWidth::resolve_in_strip`]), whether it is left alone by a
    /// neighbour closing beside it here or by being carried to a strip of
    /// its own, where a rebuilt window has no memory of its own at all.
    pub fn share_width(&mut self, w: WindowWidth) {
        let person_chose = |width: WindowWidth| {
            matches!(
                width,
                WindowWidth::Fraction(_) | WindowWidth::Narrow | WindowWidth::Wide
            )
        };
        let before = self
            .width_before_share
            .or_else(|| person_chose(self.width).then_some(self.width));
        self.set_width(w);
        self.width_before_share = before;
    }

    pub fn set_width(&mut self, w: WindowWidth) {
        let visual = self.layout_width_px();
        let before = self.width;
        match w {
            WindowWidth::Fraction(p) => {
                self.width = WindowWidth::Fraction(p.clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE));
            }
            WindowWidth::Share(p) => {
                self.width = WindowWidth::Share(p.clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE));
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

    /// Pin the part at its persisted width. Used after [`Self::set_width`]
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

    fn window_offer(&self, id: u64) -> Option<&crate::window_toolbar::WindowOffer> {
        self.window_offers
            .iter()
            .find(|(tab, _)| *tab == id)
            .map(|(_, offer)| offer)
    }

    pub fn set_tab_window_offer(
        &mut self,
        id: u64,
        offer: Option<crate::window_toolbar::WindowOffer>,
    ) -> bool {
        let at = self.window_offers.iter().position(|(tab, _)| *tab == id);
        let history =
            |offer: Option<&crate::window_toolbar::WindowOffer>| offer.map(|offer| offer.history);
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

    pub fn close_search(&mut self, source: crate::tokens::MotionSource) {
        self.search.close(source);
        self.search_active = false;
        self.drive_search();
        self.search_tab = None;
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
        if self.held_bar.is_some() {
            return self.held.new_tab;
        }
        self.bar.new_tab_rect()
    }

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
    /// Whether any tab still stands for content whose first frame is not
    /// shown, even when another tab of the window has drawn.
    pub fn any_tile_arriving(&self) -> bool {
        let _ = Widget::presented(self);
        self.hosts.iter().any(|(_, h)| h.is_arriving())
    }

    pub fn active_host_mut(&mut self) -> Option<&mut TileHost> {
        let id = self.bar.active_id();
        self.hosts
            .iter_mut()
            .find(|(i, _)| *i == id)
            .map(|(_, h)| &mut **h)
    }

    /// The window is drawn as a card whose outline the card paints: its tile
    /// keeps its own edge off, so no arc of it is cut by the card's clip.
    pub(crate) fn set_edge_by_card(&mut self, by_card: bool) {
        if let Some(host) = self.active_host_mut() {
            host.set_edge_by_card(by_card);
        }
    }

    pub fn active_host(&self) -> Option<&TileHost> {
        let id = self.bar.active_id();
        self.hosts.iter().find(|(i, _)| *i == id).map(|(_, h)| &**h)
    }

    fn card_hosts(&self) -> impl Iterator<Item = &TileHost> {
        let active = self.bar.active_id();
        self.hosts
            .iter()
            .filter(move |(id, _)| *id != active)
            .map(|(_, host)| &**host)
            .filter(|host| !FloatingSurfaceProvider::active_floating_surface_ids(*host).is_empty())
    }

    pub(crate) fn discard_layout_nodes(&mut self) {
        self.node = None;
        self.laid_shape = None;
        self.child_styles.clear();
        for (_, host) in &mut self.hosts {
            host.discard_layout_nodes();
        }
    }

    pub(crate) fn layout_node(&self) -> Option<NodeId> {
        self.node
    }

    fn layout_shape(&self) -> (bool, bool, u64) {
        (
            self.navigation_bar_visible(),
            self.bar_over_content(),
            self.bar.active_id(),
        )
    }

    /// The window's own node style. `flex_grow: 0` keeps a group at its
    /// declared `WindowWidth` regardless of sibling count or slack: placing
    /// a new app never resizes siblings, the strip extends past the viewport
    /// instead. A maximized window takes the width instead.
    pub(crate) fn own_style(&self) -> Style {
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
            // A maximized window takes the strip's width and no more: it is not
            // measured at its content's least width (`filling`).
            style = crate::widget::layout::filling(style, 1.0e6);
        }
        // While the shell's bar shows over the windows' top edge (a maximized
        // window, or a bar hidden by choice) the window is the space below it:
        // its top edge moves down by the bar and its bottom edge stays where
        // the row ends, so its own bar is never under the shell's and no edge
        // of it leaves the screen. The row stretches it to its height less the
        // margin.
        let slide = self.top_slide_now();
        if slide > 0.0 {
            style.size.height = Dimension::auto();
            style.margin.top = taffy::LengthPercentageAuto::length(f32_of_f64(slide));
        }
        style
    }

    fn style_children(&self, taffy: &mut TaffyTree<()>) {
        let laid = self.content_width_in_flight();
        for (node, own) in &self.child_styles {
            let mut style = own.clone();
            if let Some(laid) = laid {
                style.size.width = Dimension::length(f32_of_f64(laid));
                style.min_size.width = Dimension::length(f32_of_f64(laid));
                style.max_size.width = Dimension::length(f32_of_f64(laid));
            }
            if taffy.style(*node).ok() != Some(&style) {
                let _ = taffy.set_style(*node, style);
            }
        }
    }

    fn prepare_layout(&mut self, theme: &Theme) {
        self.close_duration = window_close_time();
        self.set_reduce_motion(theme.reduce_motion);
        self.rest_layout_owed = false;
    }

    /// Carry out the window's changes in its retained nodes, for the row
    /// that places it: the active tab's content remounts in place, and the
    /// bar and tile take their widths. The row sets the window's own node
    /// style (from [`Self::own_style`]). Returns `false` when the window
    /// must be built again.
    pub(crate) fn remount_in_row(&mut self, remount: &mut super::Remount<'_>) -> bool {
        if self.node.is_none() || self.laid_shape != Some(self.layout_shape()) {
            return false;
        }
        let theme = remount.theme();
        let active_id = self.bar.active_id();
        let Some(host) = self
            .hosts
            .iter_mut()
            .find(|(id, _)| *id == active_id)
            .map(|(_, host)| host)
        else {
            return false;
        };
        let relaid = host.relaid_layout_node();
        if !host.remount_children(remount) {
            return false;
        }
        if let Some(node) = relaid
            && let Ok(style) = remount.taffy().style(node).cloned()
            && let Some(entry) = self.child_styles.iter_mut().find(|(n, _)| *n == node)
        {
            entry.1 = style;
        }
        self.style_dirty = false;
        self.prepare_layout(theme);
        self.offer_history_controls();
        let flush = self.external_navigation || self.fullscreen_presentation;
        if let Some((_, host)) = self.hosts.iter_mut().find(|(id, _)| *id == active_id) {
            host.set_flush_edges(flush);
        }
        self.style_children(remount.taffy());
        true
    }

    pub fn cancel_tab_drag(&mut self) -> bool {
        self.bar.cancel_drag()
    }

    pub fn take_tab_detach(&mut self) -> Option<(u64, Point)> {
        self.bar.take_detach()
    }

    pub fn take_pending_maximize_change(&mut self) -> Option<bool> {
        self.pending_maximize_change.take()
    }

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

    fn frame_clip(&self) -> Option<Rect> {
        if let Some(spring) = &self.rect_spring {
            let dst = spring.value();
            return Some(Rect::from_origin_size(self.rect.origin(), dst.size()));
        }
        self.content_width_in_flight().map(|_| self.rect)
    }

    pub fn detach_tab(&mut self, tab_id: u64) -> Option<(TabDescriptor, TileHost)> {
        let host_idx = self.hosts.iter().position(|(i, _)| *i == tab_id)?;
        let (_, host) = self.hosts.remove(host_idx);
        let host = *host;
        let desc = self.bar.tabs().iter().find(|t| t.id == tab_id)?.clone();
        self.bar.remove_tab(tab_id);
        self.closing_hosts.remove(&tab_id);
        self.pending_close_commit.retain(|id| *id != tab_id);
        self.bar.finalize_close(tab_id);
        Some((desc, host))
    }

    /// The sibling path — splitting into a *brand-new* group — plays the group-level fade instead,
    /// in `GroupRow::split_tab_into_new_group`, so the two never double up. The new tab is shown at
    /// once; until it has drawn, the window keeps showing the tab it showed before, and then
    /// cross-fades to it.
    pub fn insert_detached_tab(&mut self, idx: usize, desc: TabDescriptor, host: TileHost) {
        let id = desc.id;
        let before = self.bar.active_id();
        let before_shown = self
            .hosts
            .iter()
            .any(|(i, h)| *i == before && Widget::presented(&**h))
            && !self.closing_hosts.contains_key(&before);
        self.bar.insert_tab(idx, desc);
        let clamped = idx.min(self.hosts.len());
        host.set_panic_scope(id);
        self.hosts.insert(clamped, (id, Box::new(host)));
        if before_shown && before != id && self.bar.active_id() == id {
            self.tab_waiting = Some((before, id));
        }
    }

    fn advance_tab_handover(&mut self, theme: &Theme) {
        let active = self.bar.active_id();
        if let Some((from, to)) = self.tab_waiting {
            let from_alive = self.hosts.iter().any(|(i, _)| *i == from);
            if to != active || !from_alive {
                self.tab_waiting = None;
            } else if self.hosts.iter().any(|(i, h)| {
                *i == to
                    && (Widget::presented(&**h)
                        || h.holds_first_frame_until(crate::time::now()).is_none())
            }) {
                let mut fade = crate::motion::CrossFade::new(from);
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

    fn stand_in_tab(&self) -> Option<u64> {
        self.tab_waiting
            .filter(|&(_, to)| to == self.bar.active_id())
            .map(|(from, _)| from)
    }

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
        Self::from_hosts(id, vec![(desc, host)], active_id, width)
    }

    pub fn tab_count(&self) -> usize {
        self.hosts.len()
    }

    #[cfg(test)]
    pub fn draws_its_bar_for_tests(&self) -> bool {
        self.navigation_bar_visible()
    }

    pub fn tab_rects(&self) -> &[Rect] {
        self.bar.tab_rects()
    }

    pub fn bar_grab_hit(&self, p: Point) -> bool {
        if !self.shows_its_bar() || !self.bar_rect().contains(p) {
            return false;
        }
        if self.resize_grab_hit(p) || self.held_furniture_hit(p) {
            return false;
        }
        if self.held_bar.is_some() {
            return true;
        }
        // A window the bar lifts from wherever it is not a control — one that stands on a canvas,
        // and one of a single tab, which its bar shows as its title. A bar of several tabs keeps
        // its presses for tab drags.
        if self.bar.lifts_the_window() {
            return self.carry_grab_hit(p);
        }
        if self.new_tab_hit(p)
            || self.bar.toolbar_hit(p)
            || self.search.hit(p)
            || self.window_control_hit(p)
        {
            return false;
        }
        !self.tab_rects().iter().any(|r| r.contains(p))
    }

    /// Where the window can be carried by its bar while it stands on a
    /// canvas: anywhere on the bar that is not a control, its title and
    /// location included. The resize edge and the search control are not.
    pub fn carry_grab_hit(&self, p: Point) -> bool {
        self.shows_its_bar()
            && self.bar_rect().contains(p)
            && !self.resize_grab_hit(p)
            && !self.held_furniture_hit(p)
            && !self.search.hit(p)
            && !self.window_control_hit(p)
            && !self.bar.control_hit(p)
    }

    /// Whether `p` is on a part of the bar's toolbar: its navigation, its location or its actions.
    pub fn toolbar_hit(&self, p: Point) -> bool {
        self.shows_its_bar() && self.bar.toolbar_hit(p)
    }

    pub fn set_carriable(&mut self, carriable: bool) {
        self.bar.set_carriable(carriable);
    }

    pub fn cancel_press_for_carry(&mut self) {
        self.bar.cancel_press_for_carry();
    }

    /// Whether `p` is on the window's own controls at the end of its bar
    /// (maximize or restore, and close) or on the glass pill they stand in,
    /// whose padding is the window's too — never the app's underneath.
    pub(crate) fn window_control_hit(&self, p: Point) -> bool {
        if !self.shows_its_bar() {
            return false;
        }
        let controls = if self.held_bar.is_some() {
            self.held.controls
        } else {
            *self.bar.window_controls()
        };
        controls.hit(p).is_some() || controls.on_pill(p)
    }

    pub fn touch_grab_hit(&self, p: Point) -> bool {
        if !self.shows_its_bar() || !self.bar_rect().contains(p) {
            return false;
        }
        if self.resize_grab_hit(p) || self.held_furniture_hit(p) {
            return false;
        }
        if self.held_bar.is_some() {
            return true;
        }
        !self.new_tab_hit(p) && !self.bar.toolbar_hit(p) && !self.search.hit(p)
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

    pub fn resize_grab_rects(&self) -> Vec<Rect> {
        [self.leading_resize_grab_rect(), self.resize_grab_rect()]
            .into_iter()
            .flatten()
            .collect()
    }

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

    pub fn update_tab_descriptor(&mut self, descriptor: TabDescriptor) -> bool {
        let Some(tab) = self
            .bar
            .tabs_mut()
            .iter_mut()
            .find(|tab| tab.id == descriptor.id)
        else {
            return false;
        };
        if tab.title == descriptor.title
            && tab.app_kind == descriptor.app_kind
            && tab.named == descriptor.named
            && tab.app_name == descriptor.app_name
        {
            return false;
        }
        *tab = descriptor;
        self.style_dirty = true;
        true
    }
    pub fn set_tab_toolbar(
        &mut self,
        id: u64,
        toolbar: Option<crate::window_toolbar::WindowToolbar>,
    ) -> bool {
        let active = self.bar.active_id();
        let declared_before = self.bar.toolbar(active).is_some();
        let changed = self.bar.set_toolbar(id, toolbar);
        if changed {
            self.style_dirty = true;
            self.grip_dirty = true;
            // Whether the window draws that bar or its app's own row does turns on the app
            // declaring nothing ([`Self::offer_bar_to_app`]), so an app that declares its parts
            // (or gives them up) while its window is up is asked again by a layout built whole:
            // a restyle keeps the row the window holds now, and the app's parts would never
            // reach the bar.
            if id == active && declared_before != self.bar.toolbar(active).is_some() {
                self.bar_offer_dirty = true;
            }
        }
        changed
    }

    #[must_use]
    pub fn tab_toolbar(&self, id: u64) -> Option<&crate::window_toolbar::WindowToolbar> {
        self.bar.toolbar(id)
    }

    pub fn take_toolbar_events(&mut self) -> Vec<(u64, crate::window_toolbar::ToolbarEvent)> {
        let mut events = self.bar.take_toolbar_events();
        events.append(&mut self.window_events);
        events
    }

    pub fn take_location_drafts(&mut self) -> Vec<(u64, crate::window_toolbar::ToolbarEvent)> {
        self.bar.take_location_drafts()
    }

    #[must_use]
    pub fn editing_location(&self) -> bool {
        self.navigation_bar_visible() && self.bar.editing_location()
    }

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

    #[must_use]
    pub fn bar_tooltip_at(&self, p: Point) -> Option<(Rect, String)> {
        if self.held_bar.is_some() {
            return self
                .new_tab_hit(p)
                .then(|| (self.held.new_tab, crate::widget::tabs::new_tab_name()));
        }
        if !self.navigation_bar_visible() {
            return None;
        }
        self.bar.tooltip_target_at(p)
    }

    pub fn draw_bar_popups(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.navigation_bar_visible() {
            self.bar
                .draw_overlay(&mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
        }
        if self.shows_its_bar() {
            self.search
                .draw_overlay(&mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
        }
    }

    /// Whether `p` is on the edited location or the suggestions under it:
    /// a press there keeps the edit.
    #[must_use]
    pub fn location_hit(&self, p: Point) -> bool {
        self.navigation_bar_visible() && self.bar.location_hit(p)
    }

    pub fn cancel_location_edit(&mut self) -> bool {
        let cancelled = self.bar.cancel_location_edit();
        if cancelled {
            self.grip_dirty = true;
        }
        cancelled
    }

    #[must_use]
    pub fn location_rect(&self) -> Rect {
        self.bar.location_rect()
    }

    pub fn edit_location(&mut self) -> bool {
        let started = self.navigation_bar_visible() && self.bar.edit_location();
        if started {
            self.grip_dirty = true;
        }
        started
    }

    pub fn begin_tab_rename(&mut self, id: u64, name: &str) -> bool {
        let started = self.navigation_bar_visible() && self.bar.begin_rename(id, name);
        if started {
            self.style_dirty = true;
            self.grip_dirty = true;
        }
        started
    }

    #[must_use]
    pub fn takes_typing(&self) -> bool {
        self.renaming_tab().is_some() || self.editing_location()
    }

    pub fn take_tab_rename_ask(&mut self) -> Option<u64> {
        self.bar.take_rename_ask()
    }

    #[must_use]
    pub fn renaming_tab(&self) -> Option<u64> {
        self.bar.renaming()
    }

    pub fn take_tab_rename(&mut self) -> Option<(u64, String)> {
        self.bar.take_rename()
    }

    pub fn tabs(&self) -> &[TabDescriptor] {
        self.bar.tabs()
    }

    pub fn set_tab_holder(&mut self, id: u64, holder: Option<crate::widget::TabHolder>) -> bool {
        self.bar.set_holder(id, holder)
    }

    pub fn set_new_tab(&mut self, on: bool) -> bool {
        self.opens_tabs = on;
        self.bar.set_new_tab(on && !self.floating)
    }

    pub fn is_held(&self) -> bool {
        self.bar.tabs().iter().any(|tab| self.bar.is_held(tab.id))
    }

    pub fn set_tab_attention(&mut self, id: u64, attention: bool) -> bool {
        self.bar.set_attention(id, attention)
    }

    pub fn set_tab_icon(&mut self, id: u64, icon: Option<icons::AppImage>) -> bool {
        self.bar.set_icon(id, icon)
    }

    pub fn active_tab_id(&self) -> u64 {
        self.bar.active_id()
    }

    /// The window's bar carries `carries` now; whether that is news.
    pub fn set_carries(&mut self, carries: Option<String>) -> bool {
        if self.carries == carries {
            return false;
        }
        self.carries = carries;
        true
    }

    #[must_use]
    pub fn carries(&self) -> Option<&str> {
        self.carries.as_deref()
    }

    /// What the window is called in a drag of it, as its bar shows it.
    #[must_use]
    pub fn name(&self) -> String {
        self.tabs()
            .iter()
            .find(|tab| tab.id == self.active_tab_id())
            .map(|tab| tab.title.clone())
            .unwrap_or_default()
    }

    /// Layout bounds and the control targets cached by production paint.
    /// A missing target means it is not currently painted.
    pub fn presentation_bounds(&self) -> WindowGroupBounds {
        let painted = |rect: Rect| (rect.area() > 0.0).then_some(rect);
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
            restore: if self.maximized { toggle } else { None },
            close: painted(controls.close),
        }
    }

    pub fn active_maximize_rect(&self) -> Option<Rect> {
        let active = self.bar.active_id();
        self.bar
            .tabs()
            .iter()
            .position(|tab| tab.id == active)
            .and_then(|index| self.bar.max_rects_internal().get(index).copied())
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

    pub fn cancel_tab_close(&mut self, tab_id: u64) -> bool {
        let restored = self.bar.cancel_close(tab_id);
        self.closing_hosts.remove(&tab_id);
        self.pending_close_commit.retain(|id| *id != tab_id);
        if restored && self.closing_at.is_some() && !self.bar.tabs().is_empty() {
            self.closing_at = None;
            self.close_lands = false;
            self.width_spring = None;
            self.style_dirty = true;
        }
        restored
    }

    pub fn drag_in_flight(&self) -> bool {
        self.bar.drag_in_flight()
    }

    /// Whether the press that lifted this window landed on a tab — a window of one tab shows it
    /// as the bar's title — rather than on the bar's empty area.
    #[must_use]
    pub fn pressed_a_tab(&self) -> bool {
        self.bar.pressed_a_tab()
    }

    pub fn dragged_tab(&self) -> Option<u64> {
        self.bar.dragged_tab()
    }

    pub fn edge_resize_in_flight(&self) -> bool {
        self.edge_drag.is_some()
    }

    pub fn set_external_drag_hover(&mut self, cursor: Option<Point>) {
        self.bar.set_external_drag_hover(cursor);
    }

    #[cfg(test)]
    pub(crate) fn bar_mut(&mut self) -> &mut crate::widget::TabBar {
        &mut self.bar
    }

    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    pub fn bar_mut_for_tests(&mut self) -> &mut crate::widget::TabBar {
        &mut self.bar
    }

    #[cfg(any(test, debug_assertions))]
    pub fn close_rect_for_tests(&self, tab_id: u64) -> Option<Rect> {
        self.bar.close_rect_for_tests(tab_id)
    }

    /// The window's bar, as it last laid out: its tabs, its toolbar and its controls.
    pub(crate) fn bar(&self) -> &crate::widget::TabBar {
        &self.bar
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

    pub fn host_for_tab_mut(&mut self, tab_id: u64) -> Option<&mut TileHost> {
        self.hosts
            .iter_mut()
            .find(|(id, _)| *id == tab_id)
            .map(|(_, h)| &mut **h)
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
                    .extend(crate::component_lifecycle::close_components(&mut *host));
            }
            self.bar.finalize_close(id);
        }

        if !self.is_closing()
            && self.bar.tabs().is_empty()
            && self.hosts.is_empty()
            && self.closing_hosts.is_empty()
        {
            self.start_closing();
        }
    }

    fn drain_bar_signals(&mut self) -> bool {
        let mut handled = false;

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
            self.press_resize();
            handled = true;
        }

        handled |= self.drain_chip_presses();

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
            let eased = window_progress(
                crate::motion::Direction::Exit,
                crate::time::now().saturating_duration_since(started),
            );
            let alpha = f32_of_f64((1.0 - eased).max(0.0));
            let scale = 1.0 - 0.05 * eased;
            (alpha, 0.0, 0.0, scale)
        } else {
            (1.0, 0.0, 0.0, 1.0)
        }
    }

    fn fade_affine(
        &self,
        transform: Affine,
        (fade_x, fade_y, fade_scale): (f64, f64, f64),
    ) -> Affine {
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

    fn draw_closing_hosts(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        if self.is_closing() {
            let ids: Vec<u64> = self.closing_hosts.keys().copied().collect();
            for id in ids {
                if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == id) {
                    h.draw_child(scene, transform, tr, theme,&env);
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
                h.draw_child(scene, close_transform, tr, theme,&env);
                scene.pop_layer();
            }
        }
    }

    /// The edge of this window a press would resize, or is resizing
    /// ([`Self::lit_edge`]), lit along its whole length: the window's own
    /// outline brightens on that side, so which window and which edge will
    /// move is plain before anything is pressed. Neutral ink, never the
    /// accent: secondary on hover, primary while dragging. Nothing at rest.
    fn draw_lit_edge(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let edge = self.lit_edge(crate::time::now());
        self.edge_lit_drawn = edge.is_some();
        let Some(edge) = edge else {
            return;
        };
        let ink = if self.edge_drag.is_some() {
            theme.tokens.colors.text_primary
        } else {
            theme.tokens.colors.text_dim
        };
        let card = self.card_rect();
        let radius = f64::from(theme.tokens.radii.tile);
        let outline = vello::kurbo::RoundedRect::from_rect(
            card.inset(-EDGE_LIT_PX / 2.0),
            (radius - EDGE_LIT_PX / 2.0).max(0.0),
        );
        let reach = radius.max(EDGE_LIT_PX);
        let side = match edge {
            WindowEdge::Leading => Rect::new(card.x0, card.y0, card.x0 + reach, card.y1),
            WindowEdge::Trailing => Rect::new(card.x1 - reach, card.y0, card.x1, card.y1),
        };
        scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &side);
        scene.stroke(
            &vello::kurbo::Stroke::new(EDGE_LIT_PX),
            transform,
            ink,
            None,
            &outline,
        );
        scene.pop_layer();
    }

    fn card_rect(&self) -> Rect {
        self.active_host()
            .map(Widget::get_rect)
            .filter(|card| card.height() > 0.0)
            .map_or(self.rect, |card| {
                Rect::new(self.rect.x0, card.y0, self.rect.x1, card.y1)
            })
    }

    /// The window's size while its edge is dragged, and fading out once let
    /// go: a small chip inside the window beside the moving edge, at its
    /// middle, where it never covers the pointer's path.
    fn draw_size_label(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = crate::time::now();
        let (edge, (width, height), alpha) = if let Some(drag) = self.edge_drag {
            (drag.moving, (drag.live_px, self.rect.height().round()), 1.0)
        } else if let (Some(faded), Some((_, edge, size))) =
            (self.size_label_fading(now), self.size_label_fade)
        {
            (edge, size, 1.0 - faded)
        } else {
            self.size_label_fade = None;
            return;
        };
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let label = format!("{width:.0} \u{d7} {height:.0}");
        let card = self.card_rect();
        let inset = f64::from(theme.tokens.spacing.rhythm.related);
        let x = match edge {
            WindowEdge::Leading => card.x0 + inset,
            WindowEdge::Trailing => card.x1 - inset,
        };
        let anchor = Rect::new(x, card.center().y, x, card.center().y);
        let within = card.inset(-inset);
        let chip = crate::widget::tooltip::hover_chip_rect_on(
            tr,
            theme,
            (anchor, crate::widget::tooltip::TooltipSide::Beside),
            &label,
            within,
        );
        let faded = alpha < 1.0;
        if faded {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                f32_of_f64(alpha),
                transform,
                &crate::widget::tooltip::chip_paint_bounds(chip, theme),
            );
        }
        crate::widget::tooltip::draw_chip(scene, transform, tr, theme, chip, &label);
        if faded {
            scene.pop_layer();
        }
    }

    /// Paint the app being carried as one lifted object: real app pixels tucked
    /// under the translucent tab handle. This runs in the floating-surface pass
    /// so the preview can cross group and strip clips without changing hit-test
    /// geometry or client configure sizes.
    fn draw_tab_drag_visual(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        let Some(visual) = self.bar.drag_visual(theme) else {
            return;
        };
        let Some((_, host)) = self.hosts.iter_mut().find(|(id, _)| *id == visual.tab_id) else {
            self.bar
                .draw_drag_ghost(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
            return;
        };
        let source = host.get_rect();
        if source.width() <= 0.0 || source.height() <= 0.0 {
            // Inactive hosts intentionally have no layout until selected.
            // Keep the carried handle visible without waking/rendering them.
            self.bar
                .draw_drag_ghost(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
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
        host.draw_child(scene, preview_transform, tr, theme,&env);
        host.set_focused(was_focused);
        scene.pop_layer();

        self.bar
            .draw_drag_ghost(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
    }
}

impl FloatingSurfaceProvider for WindowGroup {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_focus_changed(&mut **host, id, focused);
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_ime_source_changed(&mut **host, id);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_event_source(&mut **host, id);
        }
    }

    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        self.hosts.iter_mut().any(|(_, host)| {
            FloatingSurfaceProvider::floating_surface_event(&mut **host, id, event, ctx)
        })
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        self.hosts.iter().find_map(|(_, host)| {
            FloatingSurfaceProvider::floating_surface_accessibility(&**host, id)
        })
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
                &mut **host,
                id,
                target,
                action,
                state,
                path,
            )
        })
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, Rect)> {
        self.hosts
            .iter()
            .find_map(|(_, host)| FloatingSurfaceProvider::floating_surface_ime(&**host, id))
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        // A detached presentation remains interactive even when its source tab
        // is not selected. Drain every retained owner, not only the tiled one.
        self.hosts.iter_mut().find_map(|(_, host)| {
            FloatingSurfaceProvider::take_floating_surface_request(&mut **host)
        })
    }

    fn take_pip_area_request(&mut self) -> bool {
        self.active_host_mut()
            .is_some_and(FloatingSurfaceProvider::take_pip_area_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.hosts
            .iter_mut()
            .flat_map(|(_, host)| {
                FloatingSurfaceProvider::take_floating_surface_source_updates(&mut **host)
            })
            .collect()
    }

    fn draw_floating_surfaces(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::draw_floating_surfaces(
                &mut **host,
                &mut crate::draw::DrawCtx::new(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    env.clone(),
                ),
            );
        }
        self.draw_tab_drag_visual(&mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
    }

    fn draw_floating_surface(&mut self, id: &str, ctx: &mut crate::draw::DrawCtx<'_>) -> bool {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.hosts.iter_mut().any(|(_, host)| {
            FloatingSurfaceProvider::draw_floating_surface(
                &mut **host,
                id,
                &mut crate::draw::DrawCtx::new(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    env.clone(),
                ),
            )
        })
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::set_floating_surface_bounds(&mut **host, bounds);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        self.hosts
            .iter()
            .flat_map(|(_, host)| FloatingSurfaceProvider::floating_surface_presentations(&**host))
            .collect()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.hosts
            .iter()
            .flat_map(|(_, host)| FloatingSurfaceProvider::active_floating_surface_ids(&**host))
            .collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for (_, host) in &mut self.hosts {
            FloatingSurfaceProvider::on_floating_surface_host_message(&mut **host, message);
        }
    }
}

impl Widget for WindowGroup {
    crate::provides!(
        dyn crate::widget::FloatingSurfaceProvider,
        dyn crate::collaboration::presences::CollaborationTopology,
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn crate::widget::controls::ActionControls,
        dyn crate::component_lifecycle::ComponentLifecycle
    );

    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        (!self.is_closing()).then(|| self.active_tab_id())
    }

    /// The focused window is where Tab travels: its bar (the tab list, the agent that opened it, the
    /// app's parts, its controls) and the app's own controls, and never the other windows or the
    /// shell's chrome around it. Without that the bar's controls had no keyboard route at all.
    fn scopes_keyboard_focus(&self) -> bool {
        self.focused || self.search_active
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if let Some(performed) = self.perform_held_furniture_action(target, action) {
            return performed;
        }
        self.bar
            .perform_semantic_action(target, action, state, path)
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

    // Modal capture and presentation polling follow visible content, never inactive tabs.
    fn modal_capture(&self) -> Option<Rect> {
        self.children().into_iter().find_map(Widget::modal_capture)
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(self.card_hosts().filter_map(Widget::next_wake_deadline))
            .chain(self.edge_reveal_at())
            .min()
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.bar.set_platform_conventions(conventions);
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| &mut **host as &mut dyn Widget)
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
            .map(|(_, host)| &mut **host as &mut dyn Widget)
        {
            child.set_interaction_environment(environment);
        }
    }

    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        self.bar.mount_native_views(host);
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| &mut **host as &mut dyn Widget)
        {
            child.mount_native_views(host);
        }
    }

    fn unmount_native_views(&mut self) {
        self.bar.unmount_native_views();
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| &mut **host as &mut dyn Widget)
        {
            child.unmount_native_views();
        }
    }

    fn native_view_frame_start(&mut self) {
        self.bar.native_view_frame_start();
        for child in self
            .hosts
            .iter_mut()
            .map(|(_, host)| &mut **host as &mut dyn Widget)
        {
            child.native_view_frame_start();
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.navigation_bar_visible() {
            children.push(&self.bar);
            children.push(&self.opened_by);
        }
        let active = self.bar.active_id();
        if let Some((id, host)) = self.hosts.iter().find(|(id, _)| *id == active) {
            host.set_panic_scope(*id);
            children.push(&**host);
        }
        self.held_control_children(&mut children);
        if self.shows_its_bar() {
            children.push(&self.search);
        }
        children
    }
    fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        let active = self.bar.active_id();
        if let Some((_, host)) = self.hosts.iter_mut().find(|(id, _)| *id == active) {
            host.draw_overlay(&mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
        }
        if self.shows_its_bar() {
            self.search
                .draw_overlay(&mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
        }
        if self.navigation_bar_visible() {
            self.bar
                .draw_overlay(&mut crate::draw::DrawCtx::new(scene, transform, text_renderer, theme, env.clone()));
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let active = self.bar.active_id();
        let band = self.navigation_bar_visible();
        let held = self.held_bar.is_some();
        let shows = self.shows_its_bar();
        let mut children: Vec<&mut dyn Widget> = Vec::new();
        if band {
            children.push(&mut self.bar);
            children.push(&mut self.opened_by);
        }
        if let Some((id, host)) = self.hosts.iter_mut().find(|(id, _)| *id == active) {
            host.set_panic_scope(*id);
            children.push(&mut **host);
        }
        if held {
            for control in &mut self.held_controls {
                children.push(control);
            }
        }
        if shows {
            children.push(&mut self.search);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for (id, host) in &self.hosts {
            host.set_panic_scope(*id);
            visitor(&**host);
        }
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.bar);
        visitor(&self.search);
        self.for_each_child(visitor);
        true
    }

    #[allow(
        private_interfaces,
        reason = "toolkit-only checked ownership traversal"
    )]
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
        let pressed = self.drain_bar_signals();
        let furniture = self.drain_held_controls();
        let opened = self.opened_by.take_press() && self.bar.activate_opened_by();
        self.follow_search_tab();
        let searched = self.service_search();
        self.drive_search();
        let bar = self.bar.update(s, p);
        let active = self.bar.active_id();
        let focused = self.active_host_mut().is_some_and(|h| h.update(s, p));
        let mut detached = false;
        for (id, host) in &mut self.hosts {
            if *id == active {
                continue;
            }
            if !FloatingSurfaceProvider::active_floating_surface_ids(&**host).is_empty()
                && host.update(s, p)
            {
                detached = true;
            }
        }
        focused || detached || pressed || furniture || opened || searched || bar
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.prepare_layout(theme);
        // One bar per window: the app's own header row holds it where it cannot stand as the
        // bar's parts, and the window draws none above it then.
        self.held_bar = self.offer_bar_to_app();
        let active_id = self.bar.active_id();
        let in_place = match self.window_offer(active_id) {
            Some(offer) => offer.search.into(),
            None => self
                .active_host_mut()
                .map_or(crate::widget::InPlace::Nothing, TileHost::in_place_search),
        };
        self.bar.set_search_least(
            (in_place != crate::widget::InPlace::Nothing)
                .then(|| super::WindowSearch::least_span(theme)),
        );
        let bar_node = self
            .navigation_bar_visible()
            .then(|| self.bar.build_layout(taffy, theme, node_map));
        self.lay_bar_over_content(taffy, bar_node);
        self.offer_history_controls();
        for (id, host) in &mut self.hosts {
            if *id != active_id {
                host.discard_layout_nodes();
            }
        }
        self.built_active = Some(active_id);
        let tile_node = if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.set_flush_edges(self.external_navigation || self.fullscreen_presentation);
            h.build_layout(taffy, theme, node_map)
        } else {
            taffy.leaf_node(Style::default())
        };
        let mut nodes = bar_node.map_or_else(|| vec![tile_node], |bar| vec![bar, tile_node]);
        self.child_styles = nodes
            .iter()
            .map(|&node| (node, taffy.style(node).cloned().unwrap_or_default()))
            .collect();
        self.build_opened_by_stop(taffy, theme, node_map, &mut nodes, bar_node.is_some());
        self.build_held_controls(taffy, theme, node_map, &mut nodes);
        self.style_children(taffy);
        let style = self.own_style();
        let node = taffy.widget_node(style, &nodes, node_map, self);
        self.node = Some(node);
        self.laid_shape = Some(self.layout_shape());
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
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
        let tile_index = self.tile_child_index();
        if let (Some((_, h)), Some(&tile_child)) = (
            self.hosts.iter_mut().find(|(i, _)| *i == active_id),
            children.get(tile_index),
        ) {
            h.apply_layout(taffy, tile_child, new_origin);
        }
        let held = self.read_held_bar();
        if held.is_some() != self.held_bar.is_some() {
            self.style_dirty = true;
        }
        self.held_bar = held;
        self.held = held.map_or_else(
            crate::widget::window_header::RowFurniture::default,
            crate::widget::window_header::RowFurniture::of,
        );
        self.place_held_controls();
        self.place_search();
        self.sync_host_keyboard();
    }
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        self.reduce_motion = theme.reduce_motion;
        self.bar.set_opened_by_focused(self.opened_by.focused());
        if self.top_slide_in_flight() {
            self.style_dirty = true;
        }
        self.step_rect_spring();
        self.offer_history_controls();
        // A first frame from this device may hold the window's first frame
        // up to one frame budget, so a window whose app draws in time
        // appears complete. A host that honours the hold
        // (`Widget::holds_frame_until`) keeps its last frame meanwhile; one
        // that draws anyway shows the part without the window. Past the
        // budget the window stands, its place reserved, and its content
        // arrives inside its frame ([`TileHost`]).
        if self.holds_first_frame_until(crate::time::now()).is_some() {
            return;
        }
        let (fade_alpha, fade_x, fade_y, fade_scale) = self.fade_transform_params();
        let spawn_active = fade_alpha < 1.0;
        if spawn_active {
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
        let bar_over = self.bar_over_content();
        if self.navigation_bar_visible() && !bar_over {
            self.draw_bar(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
        }
        let active_id = self.bar.active_id();
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
            h.draw_child(scene, transform, tr, theme,&env);
        }
        if let Some(stand_in) = self.stand_in_tab() {
            if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == stand_in) {
                h.draw_child(scene, transform, tr, theme,&env);
            }
        } else if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.set_focused(focused);
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
            h.draw_child(scene, transform, tr, theme,&env);
            if incoming.is_some() {
                scene.pop_layer();
            }
            if source_is_lifted {
                scene.pop_layer();
            }
        }
        self.draw_held_furniture(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
        self.draw_closing_hosts(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
        if bar_over {
            self.draw_bar_over_content(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
        }
        if self.navigation_bar_visible() {
            self.draw_bar_tips(&mut crate::draw::DrawCtx::new(scene, transform, tr, theme, env.clone()));
        }
        if !self.maximized && !self.fullscreen_presentation {
            self.draw_lit_edge(scene, transform, theme);
            self.draw_size_label(scene, transform, tr, theme);
        }

        if frame_clip.is_some() {
            scene.pop_layer();
        }
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
        let tile_child = children.get(self.tile_child_index()).copied();
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            self.sync_host_keyboard();
        }
        if self.handle_edge_input(event, ctx.scale_factor, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }
        if self.held_control_takes_keys(event) || self.opened_by_takes_keys(event) {
            return;
        }
        if self.held_row_input(event, ctx.scale_factor) {
            *ctx.handled = true;
            return;
        }
        let found = self.takes_find(event, *ctx.modifiers);
        let routed = !found
            && self.shows_its_bar()
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
            let renaming = self.bar.renaming();
            self.bar
                .handle_event(event, &mut ctx.reborrow(ctx.path, bar_child));
            if self.bar.renaming() != renaming {
                self.style_dirty = true;
            }
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
    /// Only what the active tab shows holds the frame: a tab beside it,
    /// still starting, never does.
    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        if self.is_closing() {
            return None;
        }
        Widget::holds_frame_until(self.active_host()?, now)
    }
    /// **Any**, not all: a group with one live window and a second still
    /// starting must not blink out because a sibling tab hasn't drawn.
    fn presented(&self) -> bool {
        if self.hosts.is_empty() {
            return true;
        }
        self.hosts
            .iter()
            .fold(false, |seen, (_, h)| h.presented() || seen)
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.bar_tooltip_at(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.edge_drag.is_some() || self.resize_grab_hit(point) {
            Some(winit::window::CursorIcon::EwResize)
        } else {
            None
        }
    }
    fn wants_pointer_capture(&self) -> bool {
        self.edge_drag.is_some()
            || super::Widget::wants_pointer_capture(&self.bar)
            || self
                .active_host()
                .is_some_and(super::Widget::wants_pointer_capture)
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        use crate::widget::StructureChange;
        let bar = self.bar.take_structure_dirty();
        let active = self.bar.active_id();
        let mut active_host = StructureChange::None;
        for (id, host) in &mut self.hosts {
            let change = host.take_structure_change();
            if *id == active {
                active_host = change;
            }
        }
        let style = std::mem::take(&mut self.style_dirty);
        let offered = std::mem::take(&mut self.bar_offer_dirty);
        let active_switched = self.built_active != Some(active);
        let closing = self.is_closing()
            && !self.close_complete()
            && !std::mem::replace(&mut self.close_rebuild_taken, true);
        let reshaped = self.node.is_some() && self.laid_shape != Some(self.layout_shape());
        let own = if bar || offered || active_switched || closing || reshaped || self.node.is_none()
        {
            StructureChange::of(bar || offered || style || active_switched || closing || reshaped)
        } else if style {
            StructureChange::Children
        } else {
            StructureChange::None
        };
        own.join(active_host)
    }

    fn remount_children(&mut self, remount: &mut super::Remount<'_>) -> bool {
        if !self.remount_in_row(remount) {
            return false;
        }
        if let Some(node) = self.node {
            let style = self.own_style();
            if remount.taffy().style(node).ok() != Some(&style) {
                let _ = remount.taffy().set_style(node, style);
            }
        }
        true
    }
    fn needs_redraw(&self) -> bool {
        // Redraw follows the presentation tree. Retained inactive hosts still
        // participate in resources, lifecycle, components and ownership, but
        // they are not drawn or marked drawn until selected.
        let edge_due = self
            .edge_reveal_at()
            .is_some_and(|due| crate::time::now() >= due);
        self.grip_dirty
            || self.held_dirty
            || edge_due
            || self.held_control_needs_repaint()
            || self.opened_by.needs_repaint()
            || (self.navigation_bar_visible()
                && (self.bar.needs_redraw() || self.bar.needs_animation_frame()))
            || (self.shows_its_bar()
                && (self.search.needs_redraw() || self.search.needs_animation_frame()))
            || self.active_host().is_some_and(Widget::needs_redraw)
            || self.card_hosts().any(Widget::needs_redraw)
    }
    fn mark_drawn(&mut self) {
        self.grip_dirty = false;
        self.held_dirty = false;
        self.next_frame();
        self.mark_held_controls_painted();
        self.opened_by.mark_painted();
        if self.navigation_bar_visible() {
            self.bar.mark_drawn();
        }
        if self.shows_its_bar() {
            self.search.mark_drawn();
        }
        let active = self.bar.active_id();
        for (id, host) in &mut self.hosts {
            let draws_card = *id != active
                && !FloatingSurfaceProvider::active_floating_surface_ids(&**host).is_empty();
            if *id == active || draws_card {
                host.set_panic_scope(*id);
                host.mark_drawn();
            }
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
        let top_slide = self.top_slide_in_flight();
        let tab_fade = self
            .tab_fade
            .as_ref()
            .is_some_and(|fade| fade.is_moving(crate::motion::frame_now()));
        let size_label_fading = self.size_label_fade.is_some_and(|(released, ..)| {
            crate::time::now().saturating_duration_since(released)
                < crate::tokens::Durations::default().fast().saturating_mul(2)
        });
        tab_fade
            || size_label_fading
            || top_slide
            || close_in_flight
            || group_close_in_flight
            || rect_in_flight
            || width_in_flight
            || (self.navigation_bar_visible()
                && (self.bar.needs_animation_frame() || self.search.needs_animation_frame()))
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

impl crate::collaboration::presences::CollaborationTopology for WindowGroup {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.hosts
            .iter_mut()
            .map(|(_, host)| &mut **host as &mut dyn Widget)
            .collect()
    }
}

#[cfg(test)]
#[path = "window_group_tests.rs"]
mod tests;

#[path = "window_group_fullscreen_bar.rs"]
mod fullscreen_bar;
#[path = "window_group_search.rs"]
mod search;

pub(crate) use search::place_search;

#[cfg(test)]
#[path = "window_group_window_bar_tests.rs"]
mod window_bar_tests;

#[cfg(test)]
#[path = "window_group_edge_resize_tests.rs"]
mod edge_resize_tests;

#[cfg(test)]
#[path = "window_group_search_tests.rs"]
mod search_tests;

#[cfg(test)]
#[path = "window_group_motion_tests.rs"]
mod motion_tests;

#[cfg(test)]
#[path = "window_group_arrival_tests.rs"]
mod arrival_tests;
