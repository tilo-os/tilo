//! `NavSplit` — a reusable responsive sidebar / split view. Behaves like the
//! Adwaita `OverlaySplitView` "flap" plus a single-pane master-detail `Push`
//! mode:
//!
//! * **Adaptive** (default): above the breakpoint the sidebar is a
//!   persistent pane; below it, it collapses into an overlay flap (closed by
//!   default) that slides over the content behind a [`Scrim`], dismissed by
//!   the scrim, Esc, or a toggle.
//! * **Push** (master-detail): single-pane mode for narrow layouts, showing
//!   the sidebar OR the content, never both — driven via
//!   [`NavSplit::show_detail`] / [`NavSplit::show_master`].
//!
//! The split owns the breakpoint, eased reveal, scrim, and dismissal; apps
//! supply only the two child widgets.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::time::Instant;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{Element, Widget};

/// Default sidebar width (logical px) when docked or fully revealed.
pub const DEFAULT_SIDEBAR_WIDTH: f64 = 300.0;
/// Default width breakpoint (logical px). At or above this the sidebar is
/// docked; below it the sidebar becomes an overlay flap.
pub const DEFAULT_BREAKPOINT: f64 = 900.0;
/// Exponential approach rate for the reveal animation — `~0.2s` Adwaita-like
/// slide.
const REVEAL_RATE: f64 = 14.0;
/// Upper bound (seconds) on the per-frame delta used to advance the slide.
/// A large gap (first frame, or a parked event loop) would otherwise
/// teleport the flap straight to its endpoint instead of sliding.
const MAX_FRAME_DT: f64 = 0.1;

/// How the split resolves at the current width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitLayout {
    Docked,
    Overlay,
}

impl SplitLayout {
    /// Boundary is inclusive at the breakpoint → `Docked`.
    pub fn from_width(total_w: f64, breakpoint: f64) -> Self {
        if total_w >= breakpoint {
            SplitLayout::Docked
        } else {
            SplitLayout::Overlay
        }
    }
}

/// Which pane the [`SplitMode::Push`] master-detail flow is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushPane {
    Master,
    Detail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitMode {
    /// Docked-or-overlay flap, chosen per-frame from the width vs breakpoint.
    Adaptive,
    AlwaysDocked,
    Push,
}

/// The eased reveal kernel shared by every flap. Tracks a binary
/// open/closed target and animates a `reveal` value in `0..=1` toward it.
#[derive(Clone, Debug)]
pub struct FlapState {
    /// `None` ⇒ follow the layout default; `Some` once the user has toggled.
    user_open: Option<bool>,
    /// Eased `0..=1`. Drives both the slide offset and the scrim opacity.
    reveal: f64,
    /// Last resolved layout, to detect breakpoint crossings.
    last_layout: Option<SplitLayout>,
    /// Whether the sidebar is open by default when docked. Overlay always
    /// defaults closed.
    docked_default_open: bool,
}

impl FlapState {
    /// A flap that is open when docked (the common navigation sidebar).
    pub fn open_when_docked() -> Self {
        Self {
            user_open: None,
            reveal: 1.0,
            last_layout: None,
            docked_default_open: true,
        }
    }

    /// A flap that is closed even when docked — summoned on demand (e.g. a
    /// members / inspector panel).
    pub fn closed_by_default() -> Self {
        Self {
            user_open: None,
            reveal: 0.0,
            last_layout: None,
            docked_default_open: false,
        }
    }

    /// Whether the flap is currently open, resolving the user preference
    /// against the layout default.
    pub fn is_open(&self, layout: SplitLayout) -> bool {
        self.user_open.unwrap_or(match layout {
            SplitLayout::Docked => self.docked_default_open,
            SplitLayout::Overlay => false,
        })
    }

    /// Flip the flap relative to its currently-resolved openness.
    pub fn toggle(&mut self, layout: SplitLayout) {
        let now = self.is_open(layout);
        self.user_open = Some(!now);
    }

    /// Force the flap open (structural action, e.g. opening a detail).
    pub fn open(&mut self) {
        self.user_open = Some(true);
    }

    /// Force the flap closed (scrim / Esc / selection-dismiss).
    pub fn close(&mut self) {
        self.user_open = Some(false);
    }

    /// Set open state with NO animation — snaps `reveal` to the endpoint.
    /// Used by `--shots` / headless so a frame is deterministic.
    pub fn snap(&mut self, open: bool) {
        self.user_open = Some(open);
        self.reveal = if open { 1.0 } else { 0.0 };
    }

    /// Snap `reveal` to whatever the current (resolved) openness is, WITHOUT
    /// recording a user preference — unlike [`Self::snap`], the flap keeps
    /// following the layout default; this only removes the eased transient.
    /// Used on the first frame (or a breakpoint cross) so a flap that starts
    /// `open_when_docked` doesn't briefly paint open at a width whose
    /// resolved default is closed.
    pub fn settle(&mut self, layout: SplitLayout) {
        self.reveal = if self.is_open(layout) { 1.0 } else { 0.0 };
    }

    /// Drop a stale user preference when the layout crosses the breakpoint,
    /// so a window resize returns to the auto behaviour. Call once per frame
    /// before reading [`Self::reveal`]. The first reconcile does NOT clear
    /// the preference — instead it [`Self::settle`]s: the constructor picks
    /// a `reveal` before any width is known, so without this a flap whose
    /// first resolved layout is [`SplitLayout::Overlay`] would paint fully
    /// open and ease shut over the following frames, staying permanently
    /// open in a single-frame render like `--shots`. A later breakpoint
    /// crossing still animates — that transition is worth showing.
    pub fn reconcile(&mut self, layout: SplitLayout) {
        if self.last_layout != Some(layout) {
            let first = self.last_layout.is_none();
            if !first {
                self.user_open = None;
            }
            self.last_layout = Some(layout);
            if first {
                self.settle(layout);
            }
        }
    }

    /// Advance the animation by `dt` seconds toward the binary target.
    /// Returns `true` while still animating. Frame-rate independent.
    pub fn tick(&mut self, layout: SplitLayout, dt: f64) -> bool {
        let target = if self.is_open(layout) { 1.0 } else { 0.0 };
        let alpha = 1.0 - (-REVEAL_RATE * dt.min(0.1)).exp();
        self.reveal += (target - self.reveal) * alpha;
        if (self.reveal - target).abs() < 0.001 {
            self.reveal = target;
            false
        } else {
            true
        }
    }

    /// The raw eased reveal in `0..=1` — used directly for scrim alpha and
    /// docked content inset.
    pub fn reveal(&self) -> f64 {
        self.reveal.clamp(0.0, 1.0)
    }

    /// Ease-out-cubic of [`Self::reveal`] — used for the overlay slide so
    /// the flap decelerates into place.
    pub fn slide(&self) -> f64 {
        let r = self.reveal();
        crate::ease_out(r)
    }

    /// `true` while the animation hasn't settled — drives the host's
    /// "request another frame" decision.
    pub fn is_animating(&self, layout: SplitLayout) -> bool {
        let target = if self.is_open(layout) { 1.0 } else { 0.0 };
        (self.reveal - target).abs() >= 0.001
    }
}

/// The reusable responsive sidebar/split widget. See the module docs.
pub struct NavSplit {
    sidebar: Element,
    content: Element,
    mode: SplitMode,
    breakpoint: f64,
    sidebar_width: f64,
    flap: FlapState,
    /// Push-mode current pane.
    push_pane: PushPane,
    /// Eased push transition (master↔detail), reusing the flap kernel where
    /// `reveal == 1` means "detail fully shown".
    push: FlapState,
    /// Resolved outer rect from the last layout pass.
    rect: Rect,
    /// Cached sub-rects computed each layout for hit-testing + child layout.
    sidebar_rect: Rect,
    content_rect: Rect,
    scrim_rect: Rect,
    /// Last cursor position in logical coords (`MouseInput` carries none).
    last_cursor: Point,
    /// Inner taffy tree for the children so they receive layout + events.
    inner_taffy: Option<TaffyTree<()>>,
    sidebar_node: Option<NodeId>,
    content_node: Option<NodeId>,
    /// The `(sidebar_rect, content_rect)` the inner tree was last *solved* at.
    /// A full re-solve (`place_children` — rebuild + `compute_layout` + text
    /// re-measure) is only needed when the pane geometry actually changes
    /// (first layout, resize, flap slide). A pure **scroll inside the content
    /// pane** repositions but doesn't resize, so `apply_layout` re-applies the
    /// children on the cached tree instead, letting the inner `Scrollable`
    /// push its new offset down with no rebuild and no re-measure.
    solved_rects: Option<(Rect, Rect)>,
    /// `true` while the reveal/push animation is settling — read by
    /// `needs_animation_frame`.
    animating: bool,
    /// Whether the user toggled this frame (so the host re-syncs).
    dirty: bool,
    /// Wall-clock anchor of the last `draw`, so the slide self-ticks off a
    /// real `dt` from inside the draw path — `ui::App` never calls
    /// `Widget::update()`, so the flap must advance here. `None` until the
    /// first draw, which therefore advances by `0` and never jumps.
    last_tick: Option<Instant>,
}

impl NavSplit {
    /// Build an adaptive split with the default breakpoint + sidebar width.
    /// The sidebar is open when docked, closed (overlay) when narrow.
    pub fn new(sidebar: Element, content: Element) -> Self {
        Self {
            sidebar,
            content,
            mode: SplitMode::Adaptive,
            breakpoint: DEFAULT_BREAKPOINT,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            flap: FlapState::open_when_docked(),
            push_pane: PushPane::Master,
            push: FlapState::closed_by_default(),
            rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            inner_taffy: None,
            sidebar_node: None,
            content_node: None,
            solved_rects: None,
            animating: false,
            dirty: false,
            last_tick: None,
        }
    }

    /// Override the width breakpoint (logical px).
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = px;
        self
    }

    /// Override the docked / fully-revealed sidebar width (logical px).
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.sidebar_width = px;
        self
    }

    /// Choose the structural mode (Adaptive / `AlwaysDocked` / Push).
    pub fn with_mode(mut self, mode: SplitMode) -> Self {
        self.mode = mode;
        self
    }

    /// Start the sidebar collapsed even when docked (summon-on-demand panel).
    pub fn with_sidebar_closed(mut self) -> Self {
        self.flap = FlapState::closed_by_default();
        self
    }

    /// Resolve the current layout from the widget's own width + mode.
    pub fn layout_mode(&self) -> SplitLayout {
        match self.mode {
            SplitMode::AlwaysDocked => SplitLayout::Docked,
            SplitMode::Push => SplitLayout::Overlay,
            SplitMode::Adaptive => SplitLayout::from_width(self.rect.width(), self.breakpoint),
        }
    }

    /// Toggle the sidebar open/closed.
    pub fn toggle_sidebar(&mut self) {
        self.flap.toggle(self.layout_mode());
        self.dirty = true;
    }

    /// Open the sidebar.
    pub fn open_sidebar(&mut self) {
        self.flap.open();
        self.dirty = true;
    }

    /// Close the sidebar (also used by scrim / Esc / selection-dismiss).
    pub fn close_sidebar(&mut self) {
        self.flap.close();
        self.dirty = true;
    }

    /// `true` while the sidebar is (resolved) open.
    pub fn is_sidebar_open(&self) -> bool {
        self.flap.is_open(self.layout_mode())
    }

    /// Push mode: reveal the detail pane (call from the list's selection
    /// handler). No-op outside [`SplitMode::Push`].
    pub fn show_detail(&mut self) {
        self.push_pane = PushPane::Detail;
        self.push.open();
        self.dirty = true;
    }

    /// Push mode: return to the master/list pane (the Back affordance).
    pub fn show_master(&mut self) {
        self.push_pane = PushPane::Master;
        self.push.close();
        self.dirty = true;
    }

    /// Which pane Push mode is showing.
    pub fn push_pane(&self) -> PushPane {
        self.push_pane
    }

    /// Snap all animation to its endpoint (deterministic `--shots`).
    pub fn snap_open(&mut self, open: bool) {
        self.flap.snap(open);
    }

    /// The flap state (for hosts that want to read the eased reveal).
    pub fn flap(&self) -> &FlapState {
        &self.flap
    }

    /// Resolved sidebar rect (after layout). Useful for the host to place a
    /// toggle button.
    pub fn sidebar_rect(&self) -> Rect {
        self.sidebar_rect
    }
    /// Resolved content rect (after layout).
    pub fn content_panel_rect(&self) -> Rect {
        self.content_rect
    }

    /// Recompute the sidebar / content / scrim rects from the outer rect,
    /// the resolved layout, and the eased reveal/push values.
    fn recompute_rects(&mut self) {
        let layout = self.layout_mode();
        let w = self.rect.width();
        let sw = self.sidebar_width.min(w);

        match self.mode {
            SplitMode::Push => {
                // Single pane: master OR detail, sliding horizontally.
                let p = self.push.slide(); // 0 = master, 1 = detail
                // Master sits at x0, detail slides in from the right.
                self.sidebar_rect = Rect::new(
                    self.rect.x0 - w * p,
                    self.rect.y0,
                    self.rect.x1 - w * p,
                    self.rect.y1,
                );
                self.content_rect = Rect::new(
                    self.rect.x0 + w * (1.0 - p),
                    self.rect.y0,
                    self.rect.x1 + w * (1.0 - p),
                    self.rect.y1,
                );
                self.scrim_rect = Rect::ZERO;
            }
            _ => match layout {
                SplitLayout::Docked => {
                    // Docked: content inset by the eased sidebar width so the
                    // sidebar reveals/collapses smoothly without overlapping.
                    let inset = sw * self.flap.reveal();
                    self.sidebar_rect = Rect::new(
                        self.rect.x0,
                        self.rect.y0,
                        self.rect.x0 + inset,
                        self.rect.y1,
                    );
                    self.content_rect = Rect::new(
                        self.rect.x0 + inset,
                        self.rect.y0,
                        self.rect.x1,
                        self.rect.y1,
                    );
                    self.scrim_rect = Rect::ZERO;
                }
                SplitLayout::Overlay => {
                    // Overlay: content spans full width; sidebar slides in
                    // from the left over a scrim.
                    self.content_rect = self.rect;
                    let slide = self.flap.slide();
                    let x0 = self.rect.x0 - sw * (1.0 - slide);
                    self.sidebar_rect = Rect::new(x0, self.rect.y0, x0 + sw, self.rect.y1);
                    self.scrim_rect = if self.flap.reveal() > 0.001 {
                        self.rect
                    } else {
                        Rect::ZERO
                    };
                }
            },
        }
    }

    /// Build the inner taffy tree for the two children at their resolved
    /// rects so they lay out + receive events. Mirrors the
    /// `ModalDialog::place_content` pattern.
    fn place_children(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let s_node = self.sidebar.build_layout(&mut taffy, theme, &mut map);
        let c_node = self.content.build_layout(&mut taffy, theme, &mut map);

        // Lay each child out independently against its own resolved rect.
        for (node, rect) in [(s_node, self.sidebar_rect), (c_node, self.content_rect)] {
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let _ = taffy.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(rect.width() as f32),
                    height: taffy::AvailableSpace::Definite(rect.height() as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            );
        }
        self.sidebar.apply_layout(
            &taffy,
            s_node,
            Point::new(self.sidebar_rect.x0, self.sidebar_rect.y0),
        );
        self.content.apply_layout(
            &taffy,
            c_node,
            Point::new(self.content_rect.x0, self.content_rect.y0),
        );
        self.inner_taffy = Some(taffy);
        self.sidebar_node = Some(s_node);
        self.content_node = Some(c_node);
        self.solved_rects = Some((self.sidebar_rect, self.content_rect));
    }

    /// Re-apply the children onto the **cached** inner tree at the current
    /// sub-rects, without rebuilding or re-solving it — the pane geometry is
    /// unchanged, so re-running `apply_layout` just lets a descendant
    /// `Scrollable` push its new offset down. Falls back to a full
    /// `place_children` if no cached tree exists yet.
    fn reapply_children(&mut self) {
        if self.inner_taffy.is_none() || self.sidebar_node.is_none() || self.content_node.is_none()
        {
            // Shouldn't happen on a reposition, but be safe: fall back to a
            // full solve, paying for the `TextRenderer` only here.
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            self.place_children(&theme, &mut tr);
            return;
        }
        // Take the tree out so the children (other fields) can be borrowed
        // mutably alongside it, then put it straight back.
        let taffy = self.inner_taffy.take().unwrap();
        let s_node = self.sidebar_node.unwrap();
        let c_node = self.content_node.unwrap();
        let s_origin = Point::new(self.sidebar_rect.x0, self.sidebar_rect.y0);
        let c_origin = Point::new(self.content_rect.x0, self.content_rect.y0);
        self.sidebar.apply_layout(&taffy, s_node, s_origin);
        self.content.apply_layout(&taffy, c_node, c_origin);
        self.inner_taffy = Some(taffy);
    }

    /// Advance the reveal + push animations off a real wall-clock delta,
    /// recomputing the child rects when something moved. Driven from `draw`
    /// (NOT `update`, which `ui::App` never calls) so the slide actually
    /// plays. The first draw sees `dt == 0` (no prior `Instant`), so the
    /// flap never teleports; later deltas are clamped to [`MAX_FRAME_DT`] so
    /// a parked event loop can't skip the slide.
    fn self_tick(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let now = Instant::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);

        let layout = self.layout_mode();
        let a = self.flap.tick(layout, dt);
        let b = if self.mode == SplitMode::Push {
            self.push.tick(SplitLayout::Docked, dt)
        } else {
            false
        };
        self.animating = a || b;
        if self.animating {
            // Re-derive the sub-rects from the freshly-eased reveal and
            // re-lay the children at them, so the panes visually slide every
            // frame instead of just the scrim alpha fading.
            self.recompute_rects();
            self.place_children(theme, text_renderer);
        }
    }

    /// Whether the sidebar pane should be drawn this frame.
    fn sidebar_visible(&self) -> bool {
        match (self.mode, self.layout_mode()) {
            (SplitMode::Push, _) => true, // master pane always laid out
            (_, SplitLayout::Docked) => self.flap.reveal() > 0.001,
            (_, SplitLayout::Overlay) => self.flap.reveal() > 0.001,
        }
    }
}

/// Cached `QUICK_UI_NO_SPLIT_CACHE=1` check: when set, `apply_layout` always
/// re-solves the content pane, as a safety escape and a perf baseline. Read
/// once and cached so the hot layout path never touches an env var.
fn split_cache_disabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("QUICK_UI_NO_SPLIT_CACHE").is_ok_and(|s| !s.is_empty()))
}

impl Widget for NavSplit {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.sidebar.sync(state, path);
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The split occupies its whole allocation; children are laid out in
        // an isolated inner tree (their rects escape normal flow in overlay
        // mode). Same approach as ModalDialog / PopoverWidget.
        let node = _taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
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
        // Reconcile + recompute geometry, then lay out children. A full
        // (re)solve of the inner tree is only needed when the pane geometry
        // changed; a pure reposition just re-applies the children on the
        // cached tree, skipping the expensive rebuild + `compute_layout` +
        // text re-measure that otherwise ran every scroll frame.
        self.flap.reconcile(self.layout_mode());
        self.recompute_rects();
        // A pure scroll reports no measure need (`Scrollable` overrides
        // `layout_needs_measure` to `false` for a reposition), so it takes
        // the cheap re-apply path, which does NOT build a `TextRenderer` (a
        // cosmic-text `FontSystem`, expensive to construct).
        let geometry_changed = self.solved_rects != Some((self.sidebar_rect, self.content_rect));
        let needs_resolve = geometry_changed
            || self.content.layout_needs_measure()
            || self.sidebar.layout_needs_measure()
            || split_cache_disabled();
        if needs_resolve {
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            self.place_children(&theme, &mut tr);
        } else {
            self.reapply_children();
        }
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        // The reveal/push slide is driven from the draw path (`self_tick`),
        // NOT here — `ui::App` never calls `Widget::update`, so advancing the
        // easing on this tick would silently snap the flap instead of
        // sliding it. This only forwards to children and reports whether
        // anything still needs frames; `needs_animation_frame` is
        // authoritative.
        let sc = self.sidebar.update(state, path);
        let cc = self.content.update(state, path);
        self.flap.is_animating(self.layout_mode()) || sc || cc
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Advance the slide off a real per-frame delta (the App pumps frames
        // via `needs_animation_frame`, never `update`).
        self.self_tick(theme, text_renderer);

        // Content first (it's behind the overlay flap).
        self.content
            .draw(scene, transform, text_renderer, theme, presence);

        // Scrim (overlay mode only), then the sidebar on top.
        if self.scrim_rect.width() > 0.0 {
            let alpha = (self.flap.reveal() * 0.5) as f32;
            let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(alpha);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
        }
        if self.sidebar_visible() {
            self.sidebar
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        // Advance the slide before the cull check so the animation keeps
        // ticking (and the host keeps pumping frames) even while the split is
        // momentarily off-viewport.
        self.self_tick(theme, text_renderer);
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.content.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        if self.scrim_rect.width() > 0.0 {
            let alpha = (self.flap.reveal() * 0.5) as f32;
            let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(alpha);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
        }
        if self.sidebar_visible() {
            self.sidebar.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                // Esc dismisses the overlay flap (or returns Push to master).
                if self.mode == SplitMode::Push && self.push_pane == PushPane::Detail {
                    self.show_master();
                    *handled = true;
                    return;
                }
                if self.layout_mode() == SplitLayout::Overlay && self.is_sidebar_open() {
                    self.close_sidebar();
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Scrim click dismisses the overlay flap before anything else.
                let overlay_open =
                    self.layout_mode() == SplitLayout::Overlay && self.is_sidebar_open();
                if overlay_open
                    && self.scrim_rect.width() > 0.0
                    && self.scrim_rect.contains(self.last_cursor)
                    && !self.sidebar_rect.contains(self.last_cursor)
                {
                    self.close_sidebar();
                    *handled = true;
                    return;
                }
            }
            _ => {}
        }
        if *handled {
            return;
        }
        // Route to children via the inner tree. Sidebar first when it's an
        // overlay on top, else content first.
        let route_sidebar_first = self.layout_mode() == SplitLayout::Overlay
            && self.is_sidebar_open()
            || self.mode == SplitMode::Push && self.push_pane == PushPane::Master;
        let (Some(taffy), Some(s_node), Some(c_node)) = (
            self.inner_taffy.as_ref(),
            self.sidebar_node,
            self.content_node,
        ) else {
            return;
        };
        let order = if route_sidebar_first {
            [(true, s_node), (false, c_node)]
        } else {
            [(false, c_node), (true, s_node)]
        };
        for (is_sidebar, child_node) in order {
            if *handled {
                break;
            }
            let target: &mut dyn Widget = if is_sidebar {
                self.sidebar.as_mut()
            } else {
                self.content.as_mut()
            };
            target.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.sidebar.as_ref(), self.content.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.sidebar.as_mut(), self.content.as_mut()]
    }

    fn name(&self) -> &'static str {
        "NavSplit"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        self.animating
            || self.flap.is_animating(self.layout_mode())
            || self.sidebar.needs_animation_frame()
            || self.content.needs_animation_frame()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let s = self.sidebar.take_structure_dirty();
        let c = self.content.take_structure_dirty();
        s || c
    }

    fn needs_idle_sync(&self) -> bool {
        // While animating, keep the host pumping frames.
        self.animating || self.sidebar.needs_idle_sync() || self.content.needs_idle_sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Text;
    use vello_encoding::DrawTag;

    fn split(w: f64, mode: SplitMode) -> NavSplit {
        let mut ns = NavSplit::new(
            Box::new(Text::new("sidebar")),
            Box::new(Text::new("content")),
        )
        .with_mode(mode);
        lay_out(&mut ns, w, 600.0);
        ns
    }

    fn lay_out(ns: &mut NavSplit, w: f64, h: f64) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = ns.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
                },
            )
            .unwrap();
        ns.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn breakpoint_boundary_is_inclusive_docked() {
        assert_eq!(SplitLayout::from_width(900.0, 900.0), SplitLayout::Docked);
        assert_eq!(SplitLayout::from_width(899.0, 900.0), SplitLayout::Overlay);
    }

    #[test]
    fn wide_is_docked_and_open_by_default() {
        let ns = split(1200.0, SplitMode::Adaptive);
        assert_eq!(ns.layout_mode(), SplitLayout::Docked);
        assert!(ns.is_sidebar_open(), "docked sidebar opens by default");
        // Content is inset by the (full-reveal) sidebar width.
        assert!(
            ns.content_panel_rect().x0 > ns.get_rect().x0 + 1.0,
            "docked content must be inset by the sidebar"
        );
    }

    #[test]
    fn narrow_is_overlay_and_closed_by_default() {
        let ns = split(600.0, SplitMode::Adaptive);
        assert_eq!(ns.layout_mode(), SplitLayout::Overlay);
        assert!(
            !ns.is_sidebar_open(),
            "overlay sidebar is closed by default"
        );
        // Content spans full width when the flap is closed.
        assert!(
            (ns.content_panel_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "overlay content spans full width while the flap is closed"
        );
    }

    #[test]
    fn toggle_opens_overlay_flap_and_paints_scrim() {
        let mut ns = split(600.0, SplitMode::Adaptive);
        ns.toggle_sidebar();
        ns.snap_open(true); // settle the animation deterministically
        ns.recompute_rects();
        assert!(ns.is_sidebar_open());
        assert!(
            ns.scrim_rect.width() > 0.0,
            "open overlay flap shows a scrim"
        );
        // The sidebar slides fully on-screen at reveal 1.
        assert!(
            (ns.sidebar_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "fully-revealed overlay sidebar sits flush at the left edge"
        );
    }

    #[test]
    fn esc_dismisses_open_overlay_flap() {
        let mut ns = split(600.0, SplitMode::Adaptive);
        ns.open_sidebar();
        ns.snap_open(true);
        ns.recompute_rects();
        assert!(ns.is_sidebar_open());
        // Esc closes it.
        let ev = esc_event();
        let mut handled = false;
        dispatch(&mut ns, &ev, &mut handled);
        assert!(handled, "Esc on an open overlay flap is consumed");
        assert!(!ns.is_sidebar_open(), "Esc closes the overlay flap");
    }

    #[test]
    fn scrim_click_dismisses_overlay_flap() {
        let mut ns = split(600.0, SplitMode::Adaptive);
        ns.open_sidebar();
        ns.snap_open(true);
        ns.recompute_rects();
        // Click in the scrim region, to the right of the sidebar.
        let pt = Point::new(ns.get_rect().x1 - 20.0, 300.0);
        let mut handled = false;
        dispatch(&mut ns, &cursor_at(pt), &mut handled);
        dispatch(&mut ns, &left_press(), &mut handled);
        assert!(handled, "scrim click is consumed");
        assert!(!ns.is_sidebar_open(), "scrim click closes the overlay flap");
    }

    #[test]
    fn push_mode_shows_master_then_detail() {
        let mut ns = split(600.0, SplitMode::Push);
        assert_eq!(ns.push_pane(), PushPane::Master);
        // Master sits at the left edge initially.
        ns.show_detail();
        ns.push.snap(true);
        ns.recompute_rects();
        assert_eq!(ns.push_pane(), PushPane::Detail);
        // Detail is now flush with the pane; master has slid off-left.
        assert!(
            (ns.content_panel_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "detail occupies the pane after show_detail"
        );
        // Back returns to master.
        ns.show_master();
        ns.push.snap(false);
        ns.recompute_rects();
        assert_eq!(ns.push_pane(), PushPane::Master);
    }

    #[test]
    fn first_reconcile_settles_the_reveal_to_the_layout_default() {
        // `open_when_docked` guesses reveal = 1.0 before any width is known;
        // if the first resolved layout is an overlay the drawer must already
        // be shut, not ease shut over the next frames.
        let mut f = FlapState::open_when_docked();
        assert_eq!(f.reveal(), 1.0, "constructor guess");
        f.reconcile(SplitLayout::Overlay);
        assert_eq!(
            f.reveal(),
            0.0,
            "first resolution snaps to the overlay default (closed)",
        );
        assert!(
            !f.is_animating(SplitLayout::Overlay),
            "and requests no frames — there is nothing to animate",
        );

        // Docked first resolution keeps it open, no transient either way.
        let mut g = FlapState::open_when_docked();
        g.reconcile(SplitLayout::Docked);
        assert_eq!(g.reveal(), 1.0);
        assert!(!g.is_animating(SplitLayout::Docked));
    }

    #[test]
    fn a_later_breakpoint_cross_still_animates() {
        // Only the *first* resolution snaps; a real resize is a transition the
        // user should see.
        let mut f = FlapState::open_when_docked();
        f.reconcile(SplitLayout::Docked);
        f.reconcile(SplitLayout::Overlay);
        assert_eq!(f.reveal(), 1.0, "the reveal has not jumped");
        assert!(
            f.is_animating(SplitLayout::Overlay),
            "it eases shut over the following frames",
        );
    }

    #[test]
    fn reconcile_drops_stale_preference_on_breakpoint_cross() {
        let mut f = FlapState::open_when_docked();
        f.reconcile(SplitLayout::Docked); // first: no clear
        f.close(); // user closed it while docked
        assert!(!f.is_open(SplitLayout::Docked));
        f.reconcile(SplitLayout::Overlay); // crossed → clears preference
        // Overlay default is closed regardless, so flip to docked to observe.
        f.reconcile(SplitLayout::Docked);
        assert!(
            f.is_open(SplitLayout::Docked),
            "crossing the breakpoint drops the stale user preference"
        );
    }

    #[test]
    fn draw_emits_content() {
        let mut ns = split(1200.0, SplitMode::Adaptive);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let perms = crate::permissions::PermissionContext::permissive();
        let mut scene = Scene::new();
        ns.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &perms,
        );
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(fills >= 1, "a laid-out split must paint something");
    }

    /// Helper: run a full presence draw through the real production path.
    fn draw_frame(ns: &mut NavSplit) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let perms = crate::permissions::PermissionContext::permissive();
        let mut scene = Scene::new();
        ns.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &perms,
        );
    }

    /// The overlay flap must *slide* — advance its eased reveal across
    /// successive frames from the DRAW path — not snap straight to the
    /// endpoint (`Widget::update()`, which `ui::App` never calls, would
    /// teleport it). Opens the flap, draws two frames a few ms apart, and
    /// asserts the reveal moved strictly between 0 and 1.
    #[test]
    fn overlay_flap_slides_across_frames_from_draw_path() {
        let mut ns = split(600.0, SplitMode::Adaptive);
        // Pin a deterministic CLOSED start (reveal == 0), then open: the
        // target flips to 1 while the current reveal is 0, so a correct
        // slide must traverse the gap across frames.
        ns.snap_open(false);
        assert_eq!(
            ns.flap().reveal(),
            0.0,
            "snap gives a deterministic closed start"
        );
        ns.open_sidebar();
        assert!(ns.is_sidebar_open());

        // First draw establishes the clock (dt == 0) → reveal stays at 0,
        // proving the flap does NOT teleport on the very first frame.
        draw_frame(&mut ns);
        assert_eq!(
            ns.flap().reveal(),
            0.0,
            "first frame establishes the clock and must not jump the slide"
        );

        // A real wall-clock gap, then a second draw advances the easing by a
        // genuine dt.
        std::thread::sleep(std::time::Duration::from_millis(16));
        draw_frame(&mut ns);
        let r = ns.flap().reveal();
        assert!(
            r > 0.0 && r < 1.0,
            "after one ~16ms frame the flap is mid-slide (reveal {r}), not snapped"
        );
        assert!(
            ns.needs_animation_frame(),
            "a mid-slide flap must keep requesting frames so the host pumps the loop"
        );

        // Pump enough frames for the exponential approach to settle.
        for _ in 0..240 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            draw_frame(&mut ns);
            if !ns.flap().is_animating(ns.layout_mode()) {
                break;
            }
        }
        assert!(
            (ns.flap().reveal() - 1.0).abs() < 0.01,
            "the slide eventually settles fully open (reveal {})",
            ns.flap().reveal()
        );
    }

    /// `snap_open` must remain a hard, deterministic jump for `--shots`: a
    /// draw afterward (which self-ticks) must not drift the reveal.
    #[test]
    fn snap_open_stays_deterministic_through_draw() {
        let mut ns = split(600.0, SplitMode::Adaptive);
        ns.open_sidebar();
        ns.snap_open(true);
        assert_eq!(ns.flap().reveal(), 1.0, "snap jumps straight to fully open");
        // Even with a real time gap, a snapped (already-settled) flap stays
        // put because tick() is a no-op once reveal == target.
        std::thread::sleep(std::time::Duration::from_millis(20));
        draw_frame(&mut ns);
        assert_eq!(
            ns.flap().reveal(),
            1.0,
            "a snapped flap must not drift when the draw path self-ticks"
        );
        assert!(
            !ns.needs_animation_frame(),
            "a settled flap requests no further frames"
        );
    }

    // --- event scaffolding ---
    use winit::event::DeviceId;
    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn esc_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Escape);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn dispatch(ns: &mut NavSplit, event: &WindowEvent, handled: &mut bool) {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        ns.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            handled,
            &mut focus_target,
            node,
            &taffy,
        );
    }

    /// Scroll-perf regression guard: a second `apply_layout` at the SAME
    /// pane geometry (every scroll frame) must NOT rebuild + re-solve the
    /// content pane — that full rebuild was the dominant per-frame cost that
    /// made scrolling long lists crawl. A resize DOES re-solve.
    #[test]
    fn pure_reposition_reuses_the_inner_layout_instead_of_re_solving() {
        use std::cell::Cell;
        use std::rc::Rc;

        // Content spy: counts its `build_layout` calls (the expensive rebuild
        // `place_children` performs) and reports `layout_needs_measure() ==
        // false`, exactly like `Scrollable`.
        struct Spy {
            builds: Rc<Cell<usize>>,
            rect: Rect,
        }
        impl Widget for Spy {
            fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
            fn build_layout(
                &mut self,
                taffy: &mut TaffyTree<()>,
                _t: &Theme,
                node_map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                self.builds.set(self.builds.get() + 1);
                let n = taffy.leaf_node(Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                });
                node_map.insert(n, self as *mut dyn Widget);
                n
            }
            fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
                let l = taffy.layout(node).copied().unwrap_or_default();
                self.rect = Rect::new(
                    origin.x + l.location.x as f64,
                    origin.y + l.location.y as f64,
                    origin.x + l.location.x as f64 + l.size.width as f64,
                    origin.y + l.location.y as f64 + l.size.height as f64,
                );
            }
            fn draw(
                &mut self,
                _s: &mut Scene,
                _t: Affine,
                _tr: &mut TextRenderer,
                _th: &Theme,
                _p: &PresenceCtx,
            ) {
            }
            fn handle_event(
                &mut self,
                _e: &WindowEvent,
                _sf: f64,
                _m: &winit::event::Modifiers,
                _s: &mut AutoCommit,
                _p: &ObjId,
                _h: &mut bool,
                _ft: &mut Option<NodeId>,
                _n: NodeId,
                _t: &TaffyTree<()>,
            ) {
            }
            fn is_layout_dirty(&self) -> bool {
                self.rect == Rect::ZERO
            }
            fn layout_needs_measure(&self) -> bool {
                false
            }
            fn name(&self) -> &str {
                "Spy"
            }
            fn get_rect(&self) -> Rect {
                self.rect
            }
        }

        let builds = Rc::new(Cell::new(0usize));
        let mut ns = NavSplit::new(
            Box::new(Text::new("sidebar")),
            Box::new(Spy {
                builds: builds.clone(),
                rect: Rect::ZERO,
            }),
        )
        .with_mode(SplitMode::AlwaysDocked);

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = ns.build_layout(&mut taffy, &theme, &mut node_map);

        let solve = |taffy: &mut TaffyTree<()>, w: f32| {
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(w),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
        };

        // First layout: the content pane is built + solved once.
        solve(&mut taffy, 1200.0);
        ns.apply_layout(&taffy, node, Point::ZERO);
        assert_eq!(builds.get(), 1, "first layout builds the content pane once");

        // A scroll frame — apply_layout again at the identical geometry — must
        // re-apply on the cached inner tree, NOT rebuild it.
        ns.apply_layout(&taffy, node, Point::ZERO);
        assert_eq!(
            builds.get(),
            1,
            "a pure reposition must not rebuild/re-solve the content pane"
        );

        // A resize genuinely changes the pane geometry → a full re-solve.
        solve(&mut taffy, 900.0);
        ns.apply_layout(&taffy, node, Point::ZERO);
        assert!(
            builds.get() > 1,
            "a resize re-solves the content pane (build count {})",
            builds.get()
        );
    }
}
