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

use crate::time::Instant;
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
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
use crate::widget::{AuthoredExtent, Element, Widget};

/// Default sidebar width (logical px) when docked or fully revealed.
/// Predates `Metrics::sidebar_width` (280); `api_tester` and `slides` build
/// their own breakpoints on this pair, so it is recorded, not moved.
pub const DEFAULT_SIDEBAR_WIDTH: f64 = 300.0;
/// Default width breakpoint (logical px). At or above this the sidebar is
/// docked; below it the sidebar becomes an overlay flap. 180 px over
/// `Metrics::compact_breakpoint` (720).
pub const DEFAULT_BREAKPOINT: f64 = 900.0;
/// Exponential approach rate for the reveal animation — `~0.2s` Adwaita-like
/// slide.
const REVEAL_RATE: f64 = 14.0;
/// Upper bound (seconds) on the per-frame delta used to advance the slide.
/// A large gap (first frame, or a parked event loop) would otherwise
/// teleport the flap straight to its endpoint instead of sliding.
const MAX_FRAME_DT: f64 = 0.1;

/// The scrim's alpha for the current reveal — `reveal` is clamped to
/// `0.0..=1.0`, so this is always in `0.0..=0.5`, well inside `f32` range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "reveal is clamped to 0.0..=1.0, so alpha is always 0.0..=0.5"
)]
fn scrim_alpha(reveal: f64) -> f32 {
    (reveal * 0.5) as f32
}

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

/// The master↔detail push transition of a single-pane narrow layout: which
/// pane is the target and the eased slide toward it, on the shared
/// [`FlapState`] kernel (`slide() == 1` ⇒ detail fully shown).
///
/// [`NavSplit`] in [`SplitMode::Push`] runs it for its two child widgets;
/// hand-painted master-detail apps hold one directly and position their own
/// two panes from [`PushNav::slide`]. Which widths count as narrow is the
/// app's decision (its breakpoint), not this state's.
#[derive(Clone, Debug)]
pub struct PushNav {
    pane: PushPane,
    flap: FlapState,
    /// Wall clock of the last [`PushNav::tick`]; the kernel itself advances
    /// by an explicit `dt`.
    last_tick: Instant,
}

impl Default for PushNav {
    /// Starts on the master pane with the slide settled at `0`, so a narrow
    /// first frame (and `--shots`) opens to the list.
    fn default() -> Self {
        Self {
            pane: PushPane::Master,
            flap: FlapState::closed_by_default(),
            last_tick: crate::time::now(),
        }
    }
}

impl PushNav {
    /// Which pane the push flow currently targets.
    pub fn pane(&self) -> PushPane {
        self.pane
    }

    /// Push the detail pane in (call from the list's selection handler).
    pub fn show_detail(&mut self) {
        self.pane = PushPane::Detail;
        self.flap.open();
    }

    /// Slide back to the master pane (the Back affordance / Esc).
    pub fn show_master(&mut self) {
        self.pane = PushPane::Master;
        self.flap.close();
    }

    /// The eased slide in `0.0..=1.0`: `0` = master shown, `1` = detail shown.
    pub fn slide(&self) -> f64 {
        self.flap.slide()
    }

    /// Advance the slide by the wall-clock time since the previous tick
    /// (clamped to `MAX_FRAME_DT`). With `reduce_motion` the slide lands on
    /// its target at once. Returns `true` while still animating, so the host
    /// can request another frame.
    pub fn tick(&mut self, reduce_motion: bool) -> bool {
        let now = crate::time::now();
        let dt = now
            .saturating_duration_since(self.last_tick)
            .as_secs_f64()
            .min(MAX_FRAME_DT);
        self.last_tick = now;
        if reduce_motion {
            self.flap.snap(self.pane == PushPane::Detail);
            return false;
        }
        self.tick_by(dt)
    }

    /// Advance the slide by an explicit `dt` seconds. Returns `true` while
    /// still animating.
    pub fn tick_by(&mut self, dt: f64) -> bool {
        // The target is driven explicitly by `show_*`, so the layout the
        // kernel would otherwise default from does not matter here.
        self.flap.tick(SplitLayout::Overlay, dt)
    }

    /// `true` while the slide hasn't settled.
    pub fn is_animating(&self) -> bool {
        self.flap.is_animating(SplitLayout::Overlay)
    }

    /// Show `pane` with the slide already settled — no animation — so a
    /// headless frame captures the pane rather than one mid-slide.
    pub fn snap(&mut self, pane: PushPane) {
        self.pane = pane;
        self.flap.snap(pane == PushPane::Detail);
    }
}

/// The reusable responsive sidebar/split widget. See the module docs.
pub struct NavSplit {
    sidebar: Element,
    content: Element,
    mode: SplitMode,
    /// The breakpoint and sidebar width as the app authored them, at 1×
    /// (HIG §9.6).
    authored_breakpoint: AuthoredExtent,
    authored_sidebar_width: AuthoredExtent,
    /// The authored inputs resolved against the theme the outer tree was
    /// last built with ([`AuthoredExtent::resolve`]). Equal to the authored
    /// values until the first `build_layout`, and on a stock theme.
    breakpoint: f64,
    sidebar_width: f64,
    flap: FlapState,
    /// Push-mode master↔detail transition.
    push: PushNav,
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
    /// The `(width, height)` of each pane the inner tree was last *solved*
    /// at. A full re-solve (`place_children` — rebuild + `compute_layout` +
    /// text re-measure) is only needed when the pane extents actually change
    /// (first layout, resize, flap slide). A pure **scroll inside the content
    /// pane** — or a move of the whole split — repositions but doesn't
    /// resize, so `apply_layout` re-applies the children on the cached tree
    /// instead, letting the inner `Scrollable` push its new offset down with
    /// no rebuild and no re-measure.
    solved_extents: Option<[(f64, f64); 2]>,
    /// The theme the outer tree was last built with. The panes are built
    /// against it — never a throwaway `Theme::dark()` — so a `Text` under
    /// the split measures at the host's text scale, not at 1×.
    layout_theme: Theme,
    /// Set by `adjust_layout_with_measure` once the host solver has solved
    /// the panes for this pass, consumed by the `apply_layout` that follows:
    /// a freshly measured child still reports `layout_needs_measure()` until
    /// it is drawn, and without this the apply would re-solve the very same
    /// geometry with a throwaway renderer.
    host_solved: bool,
    /// `true` while the reveal/push animation is settling — read by
    /// `needs_animation_frame`.
    animating: bool,
    /// Whether the user toggled this frame (so the host re-syncs).
    dirty: bool,
    /// Wall-clock anchor of the last `draw`, so the slide self-ticks off a
    /// `dt` from inside the draw path — `tilo_ui::App` never calls
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
            authored_breakpoint: AuthoredExtent::text(DEFAULT_BREAKPOINT),
            authored_sidebar_width: AuthoredExtent::text(DEFAULT_SIDEBAR_WIDTH),
            breakpoint: DEFAULT_BREAKPOINT,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            flap: FlapState::open_when_docked(),
            push: PushNav::default(),
            rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            inner_taffy: None,
            sidebar_node: None,
            content_node: None,
            solved_extents: None,
            layout_theme: Theme::dark(),
            host_solved: false,
            animating: false,
            dirty: false,
            last_tick: None,
        }
    }

    /// Override the width breakpoint, in logical px **authored at 1×**. The
    /// split multiplies it by [`Theme::responsive_scale`] when the tree is
    /// built, so at 200 % text the panes stop fitting side by side at twice
    /// the window width — the app never restates that rule.
    #[must_use]
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.authored_breakpoint = AuthoredExtent::text(px);
        self.breakpoint = px;
        self
    }

    /// Override the docked / fully-revealed sidebar width, in logical px
    /// authored at 1× for a **text-sized** sidebar (a destination list);
    /// scaled like [`Self::with_breakpoint`].
    #[must_use]
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.authored_sidebar_width = AuthoredExtent::text(px);
        self.sidebar_width = px;
        self
    }

    /// Override the sidebar width for an **icon rail** — a column of glyphs
    /// that is not sized by the text scale. Follows `ui_scale` only.
    #[must_use]
    pub fn with_icon_sidebar_width(mut self, px: f64) -> Self {
        self.authored_sidebar_width = AuthoredExtent::icon(px);
        self.sidebar_width = px;
        self
    }

    /// The breakpoint the split currently resolves against — the authored
    /// value scaled by the theme it was last built with. Compare a window
    /// width against this, not against the constant handed to
    /// [`Self::with_breakpoint`], so an app's own narrow/wide decisions agree
    /// with the shell's at every text scale.
    pub fn breakpoint(&self) -> f64 {
        self.breakpoint
    }

    /// The docked sidebar width the split currently lays out, resolved like
    /// [`Self::breakpoint`].
    pub fn sidebar_width(&self) -> f64 {
        self.sidebar_width
    }

    /// Resolve the authored inputs against `theme` (HIG §9.6,
    /// [`AuthoredExtent::resolve`]). Identity on a stock theme.
    fn resolve_inputs(&mut self, theme: &Theme) {
        self.breakpoint = self.authored_breakpoint.resolve(theme);
        self.sidebar_width = self.authored_sidebar_width.resolve(theme);
    }

    /// Choose the structural mode (Adaptive / `AlwaysDocked` / Push).
    #[must_use]
    pub fn with_mode(mut self, mode: SplitMode) -> Self {
        self.mode = mode;
        self
    }

    /// Start the sidebar collapsed even when docked (summon-on-demand panel).
    #[must_use]
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
        self.push.show_detail();
        self.dirty = true;
    }

    /// Push mode: return to the master/list pane (the Back affordance).
    pub fn show_master(&mut self) {
        self.push.show_master();
        self.dirty = true;
    }

    /// Which pane Push mode is showing.
    pub fn push_pane(&self) -> PushPane {
        self.push.pane()
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
            #[allow(
                clippy::cast_possible_truncation,
                reason = "narrowing screen-space layout dimensions for taffy; never near f32::MAX"
            )]
            let (w, h) = (rect.width() as f32, rect.height() as f32);
            let _ = taffy.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
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
        self.solved_extents = Some(self.pane_extents());
    }

    /// `(width, height)` of the sidebar and content panes — what the inner
    /// solve depends on. Origins are not part of it: a move re-applies.
    fn pane_extents(&self) -> [(f64, f64); 2] {
        [self.sidebar_rect, self.content_rect].map(|r| (r.width(), r.height()))
    }

    /// Whether the panes need a full solve (rebuild + `compute_layout` +
    /// text measure) rather than a re-apply of the cached tree: no tree yet,
    /// the pane extents moved, or a child that is actually laid out asks to
    /// be re-measured. A pane with no extent cannot be solved, so a child
    /// parked in a collapsed pane never forces a solve every frame.
    fn needs_resolve(&self) -> bool {
        if self.inner_taffy.is_none() || split_cache_disabled() {
            return true;
        }
        let extents = self.pane_extents();
        if self.solved_extents != Some(extents) {
            return true;
        }
        let laid_out = |(w, h): (f64, f64)| w > 0.0 && h > 0.0;
        (laid_out(extents[0]) && self.sidebar.layout_needs_measure())
            || (laid_out(extents[1]) && self.content.layout_needs_measure())
    }

    /// Whether the cached inner tree was solved at the current pane extents.
    fn cached_extents_match(&self) -> bool {
        self.inner_taffy.is_some() && self.solved_extents == Some(self.pane_extents())
    }

    /// Solve the panes with the theme the outer tree was built with and a
    /// renderer at its text scale. This is the path for hosts that solve the
    /// outer tree with plain taffy and call `apply_layout` directly; the
    /// shared host solver reaches [`Widget::adjust_layout_with_measure`]
    /// first and solves with its own renderer, so it never lands here.
    fn resolve_children_fallback(&mut self) {
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
            self.place_children(&theme, tr)
        });
    }

    /// Re-apply the children onto the **cached** inner tree at the current
    /// sub-rects, without rebuilding or re-solving it — the pane geometry is
    /// unchanged, so re-running `apply_layout` just lets a descendant
    /// `Scrollable` push its new offset down. Falls back to a full
    /// `place_children` if no cached tree exists yet.
    fn reapply_children(&mut self) {
        // Take the tree out so the children (other fields) can be borrowed
        // mutably alongside it, then put it straight back.
        let (Some(taffy), Some(s_node), Some(c_node)) = (
            self.inner_taffy.take(),
            self.sidebar_node,
            self.content_node,
        ) else {
            // Shouldn't happen on a reposition, but be safe: fall back to a
            // full solve, paying for the `TextRenderer` only here.
            self.resolve_children_fallback();
            return;
        };
        let s_origin = Point::new(self.sidebar_rect.x0, self.sidebar_rect.y0);
        let c_origin = Point::new(self.content_rect.x0, self.content_rect.y0);
        self.sidebar.apply_layout(&taffy, s_node, s_origin);
        self.content.apply_layout(&taffy, c_node, c_origin);
        self.inner_taffy = Some(taffy);
    }

    /// Advance the reveal + push animations off a wall-clock delta,
    /// recomputing the child rects when something moved. Driven from `draw`
    /// (NOT `update`, which `tilo_ui::App` never calls) so the slide
    /// plays. The first draw sees `dt == 0` (no prior `Instant`), so the
    /// flap never teleports; later deltas are clamped to [`MAX_FRAME_DT`] so
    /// a parked event loop can't skip the slide.
    fn self_tick(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let now = crate::time::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);

        let layout = self.layout_mode();
        let a = self.flap.tick(layout, dt);
        let b = if self.mode == SplitMode::Push {
            self.push.tick_by(dt)
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
            (_, SplitLayout::Docked | SplitLayout::Overlay) => self.flap.reveal() > 0.001,
        }
    }
}

/// Cached `TILO_UI_NO_SPLIT_CACHE=1` check: when set, `apply_layout` always
/// re-solves the content pane, as a safety escape and a perf baseline. Read
/// once and cached so the hot layout path never touches an env var.
fn split_cache_disabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| crate::host::environment().diagnostics.disable_split_cache)
}

impl Widget for NavSplit {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The panes are built against this same theme (text scale, tokens),
        // and the authored breakpoint / width resolve against it once.
        self.layout_theme = theme.clone();
        self.resolve_inputs(theme);
        // A rebuilt outer tree carries new typography or child structure;
        // the cached inner tree is stale either way.
        self.inner_taffy = None;
        // The split occupies its whole allocation; children are laid out in
        // an isolated inner tree (their rects escape normal flow in overlay
        // mode). Same approach as ModalDialog / PopoverWidget.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// Solve the panes with the **host's** renderer while the host solver
    /// still has it in hand. The allocation is known here (only the origin
    /// is not, and the inner solve depends on extents alone), so the panes
    /// are measured with the same fonts and text scale as everything else
    /// on screen; `apply_layout` then only re-applies the cached tree at the
    /// final origin. The host tree is not touched — the split's own
    /// isolated child tree is what `apply_layout` would otherwise build
    /// with a throwaway renderer — and no style changes, so this returns
    /// `false` and never extends the settle loop.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Ok(l) = taffy.layout(node) else {
            return false;
        };
        self.rect = Rect::from_origin_size(
            (self.rect.x0, self.rect.y0),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        self.flap.reconcile(self.layout_mode());
        self.recompute_rects();
        // The settle loop calls this up to eight times a pass; once the
        // panes are solved at these extents the later iterations are no-ops
        // even though the just-measured children still read as dirty.
        let already_solved_this_pass = self.host_solved && self.cached_extents_match();
        if !already_solved_this_pass && self.needs_resolve() {
            let theme = self.layout_theme.clone();
            self.place_children(&theme, text);
        }
        self.host_solved = true;
        false
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
        // (re)solve of the inner tree is only needed when the pane extents
        // changed; a pure reposition just re-applies the children on the
        // cached tree, skipping the expensive rebuild + `compute_layout` +
        // text re-measure that otherwise ran every scroll frame.
        self.flap.reconcile(self.layout_mode());
        self.recompute_rects();
        // A pure scroll reports no measure need (`Scrollable` overrides
        // `layout_needs_measure` to `false` for a reposition), so it takes
        // the cheap re-apply path, which does NOT build a `TextRenderer` (a
        // cosmic-text `FontSystem`, expensive to construct). Under the shared
        // host solver the panes were already solved with the host's renderer
        // in `adjust_layout_with_measure` this pass, so this is a re-apply
        // too — even though the just-measured children still read as dirty
        // until they are drawn.
        let host_solved = std::mem::take(&mut self.host_solved);
        if (host_solved && self.cached_extents_match()) || !self.needs_resolve() {
            self.reapply_children();
        } else {
            self.resolve_children_fallback();
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        // The reveal/push slide is driven from the draw path (`self_tick`),
        // not here, because drawing owns its frame delta. This pass only
        // forwards to children and reports whether anything still needs
        // frames; `needs_animation_frame` is authoritative.
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
        // Advance the slide off a per-frame delta (the App pumps frames
        // via `needs_animation_frame`, never `update`).
        self.self_tick(theme, text_renderer);

        // Content first (it's behind the overlay flap).
        self.content
            .draw(scene, transform, text_renderer, theme, presence);

        // Scrim (overlay mode only), then the sidebar on top.
        if self.scrim_rect.width() > 0.0 {
            let alpha = scrim_alpha(self.flap.reveal());
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
            let alpha = scrim_alpha(self.flap.reveal());
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                if self.mode == SplitMode::Push && self.push.pane() == PushPane::Detail {
                    self.show_master();
                    *ctx.handled = true;
                    return;
                }
                if self.layout_mode() == SplitLayout::Overlay && self.is_sidebar_open() {
                    self.close_sidebar();
                    *ctx.handled = true;
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
                    *ctx.handled = true;
                    return;
                }
            }
            _ => {}
        }
        if *ctx.handled {
            return;
        }
        // Route to children via the inner tree. Sidebar first when it's an
        // overlay on top, else content first.
        let route_sidebar_first = self.layout_mode() == SplitLayout::Overlay
            && self.is_sidebar_open()
            || self.mode == SplitMode::Push && self.push.pane() == PushPane::Master;
        let (Some(taffy), Some(s_node), Some(c_node)) = (
            self.inner_taffy.as_ref(),
            self.sidebar_node,
            self.content_node,
        ) else {
            return;
        };
        if self.layout_mode() == SplitLayout::Overlay && self.is_sidebar_open() {
            // An open flap is a modal drawer, not merely a painted layer:
            // keeping the detail pane from receiving keyboard/pointer events
            // prevents a focused search field or underlying card from acting
            // through the scrim. Esc and scrim clicks were handled above;
            // remaining events belong exclusively to the drawer.
            self.sidebar.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    taffy,
                    ..ctx.reborrow(ctx.path, s_node)
                },
            );
            return;
        }
        let order = if route_sidebar_first {
            [(true, s_node), (false, c_node)]
        } else {
            [(false, c_node), (true, s_node)]
        };
        for (is_sidebar, child_node) in order {
            if *ctx.handled {
                break;
            }
            let target: &mut dyn Widget = if is_sidebar {
                self.sidebar.as_mut()
            } else {
                self.content.as_mut()
            };
            target.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    taffy,
                    ..ctx.reborrow(ctx.path, child_node)
                },
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.sidebar.as_ref(), self.content.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.sidebar.as_ref());
        visitor(self.content.as_ref());
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
    use automerge::AutoCommit;

    #[test]
    fn push_navigation_defaults_to_master_and_pushes_detail() {
        // Case `push_nav_defaults_to_the_master_pane`.
        {
            let nav = PushNav::default();
            assert_eq!(nav.pane(), PushPane::Master);
            assert!(
                nav.slide().abs() < f64::EPSILON,
                "a fresh narrow frame opens to the list"
            );
            assert!(!nav.is_animating());
        }

        // Case `push_nav_selecting_pushes_detail_and_back_returns`.
        {
            let mut nav = PushNav::default();
            nav.show_detail();
            assert_eq!(nav.pane(), PushPane::Detail);
            assert!(nav.is_animating(), "showing the detail starts the slide");
            assert!(nav.tick_by(0.016));
            assert!(nav.slide() > 0.0 && nav.slide() < 1.0);
            nav.snap(PushPane::Detail);
            assert!((nav.slide() - 1.0).abs() < f64::EPSILON);
            assert!(!nav.is_animating(), "snap leaves nothing to animate");
            nav.show_master();
            nav.snap(PushPane::Master);
            assert_eq!(nav.pane(), PushPane::Master);
            assert!(nav.slide().abs() < f64::EPSILON);
        }

        // Case `push_nav_reduced_motion_lands_without_requesting_frames`.
        {
            let mut nav = PushNav::default();
            nav.show_detail();
            assert!(!nav.tick(true));
            assert!((nav.slide() - 1.0).abs() < f64::EPSILON);
            assert!(!nav.is_animating());
            nav.show_master();
            assert!(!nav.tick(true));
            assert!(nav.slide().abs() < f64::EPSILON);
        }

        // Case `push_mode_shows_master_then_detail`.
        {
            let mut ns = split(600.0, SplitMode::Push);
            assert_eq!(ns.push_pane(), PushPane::Master);
            // Master sits at the left edge initially.
            ns.show_detail();
            ns.push.snap(PushPane::Detail);
            ns.recompute_rects();
            assert_eq!(ns.push_pane(), PushPane::Detail);
            // Detail is now flush with the pane; master has slid off-left.
            assert!(
                (ns.content_panel_rect().x0 - ns.get_rect().x0).abs() < 1.0,
                "detail occupies the pane after show_detail"
            );
            // Back returns to master.
            ns.show_master();
            ns.push.snap(PushPane::Master);
            ns.recompute_rects();
            assert_eq!(ns.push_pane(), PushPane::Master);
        }
    }

    use crate::widget::Text;
    use std::cell::Cell;
    use std::rc::Rc;
    use vello_encoding::DrawTag;

    /// Content spy: counts its `build_layout` calls (the expensive rebuild
    /// `place_children` performs) and reports `layout_needs_measure() ==
    /// false`, exactly like `Scrollable`. Used by
    /// [`pure_reposition_reuses_the_inner_layout_instead_of_re_solving`].
    struct Spy {
        builds: Rc<Cell<usize>>,
        events: Rc<Cell<usize>>,
        rect: Rect,
    }
    impl Widget for Spy {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.builds.set(self.builds.get().saturating_add(1));
            let n = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            });
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
            n
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).copied().unwrap_or_default();
            self.rect = Rect::new(
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
                origin.x + f64::from(l.location.x) + f64::from(l.size.width),
                origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        fn handle_event(&mut self, _e: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
            self.events.set(self.events.get().saturating_add(1));
        }
        fn is_layout_dirty(&self) -> bool {
            self.rect == Rect::ZERO
        }
        fn layout_needs_measure(&self) -> bool {
            false
        }
        fn name(&self) -> &'static str {
            "Spy"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

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
        lay_out_with(ns, &Theme::dark(), w, h);
    }

    /// Plain-taffy host: solve the outer node, then `apply_layout` — the
    /// path with no `adjust_layout_with_measure` and no host renderer.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test viewport dimensions are small literals, never near f32::MAX"
    )]
    fn lay_out_with(ns: &mut NavSplit, theme: &Theme, w: f64, h: f64) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = ns.build_layout(&mut taffy, theme, &mut node_map);
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
    fn panes_measure_at_the_host_text_scale_and_reuse_inner_layout() {
        // Case `private_child_layout_preserves_the_active_text_scale`.
        {
            let mut ns = NavSplit::new(
                Box::new(Text::new("Sidebar")),
                Box::new(Text::new("Scaled content").with_scaled_size(20.0)),
            )
            .with_mode(SplitMode::AlwaysDocked);
            lay_out_with(&mut ns, &Theme::dark().with_text_scale(2.0), 1000.0, 600.0);

            assert!(
                ns.content.get_rect().height() >= 49.0,
                "inner text must reserve its 200% line height, got {:?}",
                ns.content.get_rect()
            );
        }

        // Case `pure_reposition_reuses_the_inner_layout_instead_of_re_solving`.
        // Scroll-perf regression guard: a second `apply_layout` at the SAME
        // pane geometry (every scroll frame) must NOT rebuild + re-solve the
        // content pane — that full rebuild was the dominant per-frame cost that
        // made scrolling long lists crawl. A resize DOES re-solve.
        {
            let builds = Rc::new(Cell::new(0usize));
            let events = Rc::new(Cell::new(0usize));
            let mut ns = NavSplit::new(
                Box::new(Text::new("sidebar")),
                Box::new(Spy {
                    builds: builds.clone(),
                    events,
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

            // A resize changes the pane geometry → a full re-solve.
            solve(&mut taffy, 900.0);
            ns.apply_layout(&taffy, node, Point::ZERO);
            assert!(
                builds.get() > 1,
                "a resize re-solves the content pane (build count {})",
                builds.get()
            );
        }

        // Case `text_under_a_split_measures_at_the_host_text_scale`.
        // A8 (tube): every `Text` under a split kept its 1× line height under
        // 2× glyphs because the panes were built against `Theme::dark()` and
        // measured with a fresh `TextRenderer::new()`. Under the shared host
        // solver the panes are now solved with the theme the outer tree was
        // built with and the host's own renderer — so the content line is
        // twice as tall at 200 % text, and the host path loads no font system
        // of its own.
        {
            let height_at = |scale: f32| {
                let theme = Theme::dark().with_text_scale(scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(scale);
                let mut ns = NavSplit::new(
                    Box::new(Text::new("sidebar")),
                    Box::new(Text::new("content")),
                )
                .with_mode(SplitMode::AlwaysDocked);
                crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
                let content = host_layout(&mut ns, &theme, &mut renderer);
                assert_eq!(
                    crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(std::cell::Cell::get),
                    0,
                    "the host path must measure with the host's renderer, not load fonts of its own"
                );
                content.height()
            };
            let one = height_at(1.0);
            let two = height_at(2.0);
            assert!(one > 0.0, "the content Text is laid out");
            assert!(
                (two / one - 2.0).abs() < 0.05,
                "a Text line under the split is {two} px at 2× against {one} px at 1×; it must double"
            );
        }

        // Case `raw_layout_path_measures_panes_at_the_built_theme_text_scale`.
        // A host that solves the outer tree with plain taffy and calls
        // `apply_layout` directly gets the same answer: the panes are built
        // against the theme the outer tree was built with, at its text scale.
        {
            let height_at = |scale: f32| {
                let mut ns = NavSplit::new(
                    Box::new(Text::new("sidebar")),
                    Box::new(Text::new("content")),
                )
                .with_mode(SplitMode::AlwaysDocked);
                lay_out_with(
                    &mut ns,
                    &Theme::dark().with_text_scale(scale),
                    1200.0,
                    600.0,
                );
                ns.content.get_rect().height()
            };
            let one = height_at(1.0);
            let two = height_at(2.0);
            assert!(one > 0.0);
            assert!(
                (two / one - 2.0).abs() < 0.05,
                "a Text line under the split is {two} px at 2× against {one} px at 1×; it must double"
            );
        }

        // Case `authored_inputs_follow_the_responsive_scale_and_icon_rails_follow_ui_scale_only`.
        // HIG §9.6: a breakpoint and a text-sized sidebar width are authored at
        // 1× and resolved by the shell — `× ui_scale × text_scale.max(1.0)` —
        // so an app never restates the rule. An icon rail follows `ui_scale`
        // only, and text never shrinks a pane below its authored geometry.
        {
            let text_sized = || {
                NavSplit::new(Box::new(Text::new("s")), Box::new(Text::new("c")))
                    .with_breakpoint(760.0)
                    .with_sidebar_width(300.0)
            };
            let rail = || {
                NavSplit::new(Box::new(Text::new("s")), Box::new(Text::new("c")))
                    .with_breakpoint(760.0)
                    .with_icon_sidebar_width(86.0)
            };

            let (mut list, mut icons) = (text_sized(), rail());
            lay_out(&mut list, 1200.0, 600.0);
            lay_out(&mut icons, 1200.0, 600.0);
            assert_eq!((list.breakpoint(), list.sidebar_width()), (760.0, 300.0));
            assert_eq!((icons.breakpoint(), icons.sidebar_width()), (760.0, 86.0));
            assert_eq!(
                list.layout_mode(),
                SplitLayout::Docked,
                "1× is the authored geometry"
            );

            let two = Theme::dark().with_text_scale(2.0);
            let (mut list, mut icons) = (text_sized(), rail());
            lay_out_with(&mut list, &two, 1200.0, 600.0);
            lay_out_with(&mut icons, &two, 1200.0, 600.0);
            assert_eq!((list.breakpoint(), list.sidebar_width()), (1520.0, 600.0));
            assert_eq!(
                (icons.breakpoint(), icons.sidebar_width()),
                (1520.0, 86.0),
                "an icon rail never grows with the text scale"
            );
            assert_eq!(
                list.layout_mode(),
                SplitLayout::Overlay,
                "a 1200 px window at 2× has the room a 600 px window has at 1×, so it pushes"
            );

            let mut small = text_sized();
            lay_out_with(
                &mut small,
                &Theme::dark().with_text_scale(0.75),
                1200.0,
                600.0,
            );
            assert_eq!(
                (small.breakpoint(), small.sidebar_width()),
                (760.0, 300.0),
                "text never shrinks a pane below its authored geometry"
            );

            let mut dense = rail();
            lay_out_with(&mut dense, &Theme::dark().with_ui_scale(1.5), 1200.0, 600.0);
            assert!((dense.breakpoint() - 1140.0).abs() < 1e-9);
            assert!(
                (dense.sidebar_width() - 129.0).abs() < 1e-9,
                "ui_scale grows an icon rail with the rest of the density system"
            );
        }
    }

    #[test]
    fn breakpoint_docks_wide_and_overlays_narrow_by_default() {
        // Case `breakpoint_boundary_is_inclusive_docked`.
        {
            assert_eq!(SplitLayout::from_width(900.0, 900.0), SplitLayout::Docked);
            assert_eq!(SplitLayout::from_width(899.0, 900.0), SplitLayout::Overlay);
        }

        // Case `wide_is_docked_and_open_by_default`.
        {
            let ns = split(1200.0, SplitMode::Adaptive);
            assert_eq!(ns.layout_mode(), SplitLayout::Docked);
            assert!(ns.is_sidebar_open(), "docked sidebar opens by default");
            // Content is inset by the (full-reveal) sidebar width.
            assert!(
                ns.content_panel_rect().x0 > ns.get_rect().x0 + 1.0,
                "docked content must be inset by the sidebar"
            );
        }

        // Case `narrow_is_overlay_and_closed_by_default`.
        {
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
    }

    #[test]
    fn overlay_flap_opens_with_scrim_dismisses_and_blocks_obscured_content() {
        // Case `toggle_opens_overlay_flap_and_paints_scrim`.
        {
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

        // Case `esc_dismisses_open_overlay_flap`.
        {
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

        // Case `scrim_click_dismisses_overlay_flap`.
        {
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

        // Case `open_overlay_does_not_route_events_to_obscured_content`.
        {
            let builds = Rc::new(Cell::new(0));
            let events = Rc::new(Cell::new(0));
            let mut ns = NavSplit::new(
                Box::new(Text::new("sidebar")),
                Box::new(Spy {
                    builds,
                    events: events.clone(),
                    rect: Rect::ZERO,
                }),
            )
            .with_mode(SplitMode::Adaptive);
            lay_out(&mut ns, 600.0, 600.0);
            ns.open_sidebar();
            ns.snap_open(true);
            ns.recompute_rects();
            let mut handled = false;
            dispatch(&mut ns, &cursor_at(Point::new(580.0, 300.0)), &mut handled);
            assert_eq!(
                events.get(),
                0,
                "an open overlay is modal; obscured detail content receives no input"
            );
        }
    }

    #[test]
    fn reveal_reconciles_to_the_layout_default_across_breakpoints() {
        // Case `first_reconcile_settles_the_reveal_to_the_layout_default`.
        {
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

        // Case `a_later_breakpoint_cross_still_animates`.
        {
            // Only the *first* resolution snaps; a resize is a transition the
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

        // Case `reconcile_drops_stale_preference_on_breakpoint_cross`.
        {
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
    }

    #[test]
    fn overlay_flap_draws_slides_and_snaps_deterministically() {
        // Case `draw_emits_content`.
        {
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

        // Case `overlay_flap_slides_across_frames_from_draw_path`.
        // The overlay flap must *slide* — advance its eased reveal across
        // successive frames from the DRAW path — not snap straight to the
        // endpoint during the update pass. Opens the flap, draws two frames a
        // few ms apart, and asserts the reveal moved strictly between 0 and 1.
        {
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

            // A frame's worth of wall clock, then a second draw advances the
            // easing by that dt.
            elapse(&mut ns, 16);
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
                elapse(&mut ns, 16);
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

        // Case `snap_open_stays_deterministic_through_draw`.
        // `snap_open` must remain a hard, deterministic jump for `--shots`: a
        // draw afterward (which self-ticks) must not drift the reveal.
        {
            let mut ns = split(600.0, SplitMode::Adaptive);
            ns.open_sidebar();
            ns.snap_open(true);
            assert_eq!(ns.flap().reveal(), 1.0, "snap jumps straight to fully open");
            // Even with a time gap, a snapped (already-settled) flap stays
            // put because tick() is a no-op once reveal == target.
            elapse(&mut ns, 20);
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
    }

    /// Pretend `ms` of wall clock passed since the last drawn frame, so the
    /// next draw advances the animations by exactly that step.
    fn elapse(ns: &mut NavSplit, ms: u64) {
        ns.last_tick = crate::time::now().checked_sub(crate::time::Duration::from_millis(ms));
    }

    /// Helper: run a full presence draw through the production path.
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
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Shared-host path: build, admit, `compute_widget_layout` with the
    /// host's renderer, `apply_layout`. Returns the content pane's rect.
    fn host_layout(ns: &mut NavSplit, theme: &Theme, renderer: &mut TextRenderer) -> Rect {
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root = ns.build_layout(&mut taffy, theme, &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        // SAFETY: the tree and maps come from this exact live build, and
        // `ns` is exclusively owned for the whole solve.
        unsafe {
            bindings.rebuild(&*ns, &taffy, root, &nodes);
            crate::widget::compute_widget_layout(
                &mut taffy,
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(1200.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
                &nodes,
                &bindings,
                renderer,
            )
            .unwrap();
        }
        ns.apply_layout(&taffy, root, Point::ZERO);
        ns.content.get_rect()
    }
}
