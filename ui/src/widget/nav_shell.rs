//! `NavShell` — the HIG §1 navigation primitive.
//!
//! A **sidebar** (§1a) docks/collapses via a hamburger when wide and becomes
//! an overlay drawer over content — never a full-screen page — when narrow.
//! A **content master/detail** (§1b) shows both panes side by side when wide
//! and pushes detail over overview (Back button) when narrow. Compose both
//! (`[ sidebar | overview | detail ]`) or use [`NavShell::new`] alone; the
//! shell owns the breakpoint, eased collapse/push slides, the scrim, and
//! button/scrim/Esc self-routing.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::time::Instant;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Cap, Join, Line, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{Icon, draw_icon};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::nav_split::{FlapState, SplitLayout};
use crate::widget::{Element, Widget};

pub const DEFAULT_NAVSHELL_SIDEBAR_WIDTH: f64 = 280.0;
pub const DEFAULT_NAVSHELL_OVERVIEW_WIDTH: f64 = 340.0;
pub const DEFAULT_NAVSHELL_BREAKPOINT: f64 = 700.0;
/// Detail pane's minimum width in the side-by-side layout; overview gives up
/// space first.
const MIN_DETAIL_WIDTH: f64 = 260.0;
/// Caps the per-frame dt so a stale `Instant` (first frame, or a parked
/// event loop) can't teleport a slide past its animation.
const MAX_FRAME_DT: f64 = 0.1;
const CHIP_SIZE: f64 = 36.0;
const CHIP_INSET: f64 = 12.0;
const BACK_CHIP_WIDTH: f64 = 104.0;

/// Narrows a screen-space `f64` (rect extent, alpha) to the `f32` taffy/
/// vello layout math expects.
#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout coordinates and [0,1] alpha never approach f32::MAX; this narrowing is exact for every practical UI extent"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellMode {
    /// Docked sidebar + side-by-side content (`>=` breakpoint).
    Wide,
    /// Overlay-drawer sidebar + push content (`<` breakpoint).
    Narrow,
}

impl ShellMode {
    pub fn from_width(total_w: f64, breakpoint: f64) -> Self {
        if total_w >= breakpoint {
            ShellMode::Wide
        } else {
            ShellMode::Narrow
        }
    }
}

/// How the **content** master/detail region resolves at the current width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentLayout {
    SideBySide,
    /// One pane at a time; detail pushes in from the right (narrow).
    Push,
    /// The only content pane; fills the region at every width. The shape of
    /// a sidebar-plus-content app (§1a with no §1b split) — a file manager,
    /// a settings pane.
    Single,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavPane {
    Overview,
    Detail,
}

/// What the single top-left affordance is doing this frame; exactly one
/// ever occupies the corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affordance {
    None,
    /// Hamburger closed, chevron open — never an `X`.
    SidebarToggle,
    Back,
}

pub struct NavShell {
    sidebar: Option<Element>,
    overview: Element,
    /// `None` for a sidebar-plus-content app: no overview/detail split to
    /// push between, so the single content pane must stay visible at every
    /// width.
    detail: Option<Element>,

    breakpoint: f64,
    sidebar_width: f64,
    overview_width: f64,

    /// Reveal easing for the sidebar (collapse when docked, slide when
    /// overlay); open-when-docked by default.
    sidebar_flap: FlapState,
    pane: NavPane,
    /// Push easing: `0` = overview front, `1` = detail front. Closed
    /// (overview) by default.
    push: FlapState,
    /// The content pane the user most recently engaged; drives which pane
    /// survives a collapse to narrow (never the sidebar). Falls back to the
    /// right-most pane (detail) when `None`.
    active_pane_hint: Option<NavPane>,
    /// Previous layout's mode, so `apply_layout` can spot a wide→narrow
    /// collapse and seed the visible pane from the last-interacted one.
    last_mode: Option<ShellMode>,

    // --- resolved geometry (recomputed each layout / animation frame) ---
    rect: Rect,
    sidebar_rect: Rect,
    overview_rect: Rect,
    detail_rect: Rect,
    scrim_rect: Rect,
    /// Zero when `affordance == Affordance::None`.
    affordance_rect: Rect,
    affordance: Affordance,
    /// Suppress the collapse chip while docked (wide); the hamburger still
    /// appears narrow, to summon the drawer.
    hide_docked_toggle: bool,

    last_cursor: Point,

    // --- inner taffy for the children ---
    inner_taffy: Option<TaffyTree<()>>,
    sidebar_node: Option<NodeId>,
    overview_node: Option<NodeId>,
    detail_node: Option<NodeId>,

    animating: bool,
    dirty: bool,
    last_tick: Option<Instant>,
}

impl NavShell {
    /// Content-only shell (no sidebar): overview + detail, e.g. weather.
    pub fn new(overview: Element, detail: Element) -> Self {
        Self {
            sidebar: None,
            overview,
            detail: Some(detail),
            breakpoint: DEFAULT_NAVSHELL_BREAKPOINT,
            sidebar_width: DEFAULT_NAVSHELL_SIDEBAR_WIDTH,
            overview_width: DEFAULT_NAVSHELL_OVERVIEW_WIDTH,
            sidebar_flap: FlapState::open_when_docked(),
            pane: NavPane::Overview,
            push: FlapState::closed_by_default(),
            active_pane_hint: None,
            last_mode: None,
            rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            overview_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            affordance_rect: Rect::ZERO,
            affordance: Affordance::None,
            hide_docked_toggle: false,
            last_cursor: Point::ZERO,
            inner_taffy: None,
            sidebar_node: None,
            overview_node: None,
            detail_node: None,
            animating: false,
            dirty: false,
            last_tick: None,
        }
    }

    /// Sidebar-plus-content shell (file-manager / settings shape).
    ///
    /// Use this instead of [`NavShell::new`] when the left pane is a list of
    /// *destinations*: passing it as `new`'s `overview` makes it §1b
    /// content, which narrow then promotes to a full-screen page, pushing
    /// the real content off the window.
    pub fn sidebar_over_content(sidebar: Element, content: Element) -> Self {
        let mut shell = Self::new(content, Box::new(crate::widget::Spacer::new()));
        shell.sidebar = Some(sidebar);
        shell.detail = None;
        shell
    }

    /// Attach a genuine sidebar: `[ sidebar | overview | detail ]`.
    #[must_use]
    pub fn with_sidebar(mut self, sidebar: Element) -> Self {
        self.sidebar = Some(sidebar);
        self
    }

    #[must_use]
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = px;
        self
    }

    #[must_use]
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.sidebar_width = px;
        self
    }

    #[must_use]
    pub fn with_overview_width(mut self, px: f64) -> Self {
        self.overview_width = px;
        self
    }

    #[must_use]
    pub fn hide_toggle_when_docked(mut self) -> Self {
        self.hide_docked_toggle = true;
        self
    }

    #[must_use]
    pub fn with_sidebar_collapsed(mut self) -> Self {
        self.sidebar_flap = FlapState::closed_by_default();
        self
    }

    // --- live child swap -------------------------------------------------
    //
    // Hosts that rebuild their child subtree on a structure change keep the
    // `NavShell` itself and swap panes in here instead of recreating it —
    // the shell's responsive state lives on the shell, not the children, so
    // swapping preserves the sidebar/push position across the rebuild.

    pub fn set_overview(&mut self, overview: Element) {
        self.overview = overview;
    }

    pub fn set_detail(&mut self, detail: Element) {
        self.detail = Some(detail);
    }

    /// Replace the single content pane of a [`NavShell::sidebar_over_content`]
    /// shell. Writes the same slot as [`Self::set_overview`]; named so a
    /// single-pane app doesn't have to call its content an "overview".
    pub fn set_content(&mut self, content: Element) {
        self.overview = content;
    }

    pub fn has_detail(&self) -> bool {
        self.detail.is_some()
    }

    pub fn set_sidebar(&mut self, sidebar: Option<Element>) {
        self.sidebar = sidebar;
    }

    // --- layout-mode queries ---------------------------------------------

    pub fn has_sidebar(&self) -> bool {
        self.sidebar.is_some()
    }

    pub fn mode(&self) -> ShellMode {
        ShellMode::from_width(self.rect.width(), self.breakpoint)
    }

    pub fn content_layout(&self) -> ContentLayout {
        if self.detail.is_none() {
            return ContentLayout::Single;
        }
        match self.mode() {
            ShellMode::Wide => ContentLayout::SideBySide,
            ShellMode::Narrow => ContentLayout::Push,
        }
    }

    /// Docked (wide) or Overlay (narrow); `None` when there is no sidebar.
    pub fn sidebar_layout(&self) -> Option<SplitLayout> {
        self.sidebar.as_ref().map(|_| self.sidebar_split_layout())
    }

    fn sidebar_split_layout(&self) -> SplitLayout {
        match self.mode() {
            ShellMode::Wide => SplitLayout::Docked,
            ShellMode::Narrow => SplitLayout::Overlay,
        }
    }

    pub fn pane(&self) -> NavPane {
        self.pane
    }

    pub fn is_detail_shown(&self) -> bool {
        self.detail.is_some() && self.pane == NavPane::Detail
    }

    pub fn is_sidebar_open(&self) -> bool {
        self.has_sidebar() && self.sidebar_flap.is_open(self.sidebar_split_layout())
    }

    pub fn top_left_affordance(&self) -> Affordance {
        if self.content_layout() == ContentLayout::Push && self.pane == NavPane::Detail {
            Affordance::Back
        } else if self.has_sidebar() {
            // Docked sidebar is permanent at this width, so no collapse chip
            // when the host opts out; the narrow overlay still needs the
            // hamburger to summon the drawer.
            if self.hide_docked_toggle && self.sidebar_split_layout() == SplitLayout::Docked {
                Affordance::None
            } else {
                Affordance::SidebarToggle
            }
        } else {
            Affordance::None
        }
    }

    // --- runtime control -------------------------------------------------

    pub fn toggle_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.toggle(self.sidebar_split_layout());
        self.dirty = true;
    }

    pub fn open_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.open();
        self.dirty = true;
    }

    pub fn close_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.close();
        self.dirty = true;
    }

    /// Push the detail pane in. In the side-by-side (wide) layout both panes
    /// are already visible, so this only sets the push target for narrow.
    pub fn show_detail(&mut self) {
        self.pane = NavPane::Detail;
        self.active_pane_hint = Some(NavPane::Detail);
        self.push.open();
        self.dirty = true;
    }

    pub fn show_overview(&mut self) {
        self.pane = NavPane::Overview;
        self.active_pane_hint = Some(NavPane::Overview);
        self.push.close();
        self.dirty = true;
    }

    /// Optional: the shell already infers the active pane from pointer
    /// presses and from `show_detail`/`show_overview`; a host with its own
    /// selection model can state it directly. Only affects a later collapse
    /// to narrow.
    pub fn hint_active_pane(&mut self, pane: NavPane) {
        self.active_pane_hint = Some(pane);
    }

    /// A collapse never lands on the sidebar: keeps the last-interacted
    /// pane, falling back to the right-most (detail) when none is known.
    fn collapse_content_pane(&self) -> NavPane {
        self.active_pane_hint.unwrap_or(NavPane::Detail)
    }

    /// Alias for [`Self::show_overview`] — master/detail vocabulary.
    pub fn show_master(&mut self) {
        self.show_overview();
    }

    /// Snap **all** animation straight to its endpoint (deterministic
    /// `--shots` / headless). Call after setting the desired state.
    pub fn snap(&mut self) {
        let open = self.sidebar_flap.is_open(self.sidebar_split_layout());
        self.sidebar_flap.snap(open);
        self.push.snap(self.pane == NavPane::Detail);
        self.recompute_rects();
    }

    pub fn snap_sidebar(&mut self, open: bool) {
        self.sidebar_flap.snap(open);
        self.recompute_rects();
    }

    pub fn snap_pane(&mut self, pane: NavPane) {
        self.pane = pane;
        self.push.snap(pane == NavPane::Detail);
        self.recompute_rects();
    }

    // --- resolved rects (for hit-testing) --------------------------------

    pub fn sidebar_rect(&self) -> Rect {
        self.sidebar_rect
    }
    pub fn overview_rect(&self) -> Rect {
        self.overview_rect
    }
    pub fn detail_rect(&self) -> Rect {
        self.detail_rect
    }
    pub fn scrim_rect(&self) -> Rect {
        self.scrim_rect
    }
    pub fn collapse_button_rect(&self) -> Rect {
        if self.affordance == Affordance::SidebarToggle {
            self.affordance_rect
        } else {
            Rect::ZERO
        }
    }
    pub fn back_button_rect(&self) -> Rect {
        if self.affordance == Affordance::Back {
            self.affordance_rect
        } else {
            Rect::ZERO
        }
    }

    // --- geometry --------------------------------------------------------

    fn recompute_rects(&mut self) {
        let r = self.rect;
        let w = r.width();
        let has_sidebar = self.has_sidebar();
        let sw = self.sidebar_width.min(w);

        // Sidebar-plus-content: the one content pane fills the region at
        // every width; narrow turns the sidebar into a drawer *over* it
        // (HIG §1a) rather than promoting it to a full-screen page.
        if self.detail.is_none() {
            self.detail_rect = Rect::ZERO;
            match self.mode() {
                ShellMode::Wide => {
                    let inset = if has_sidebar {
                        sw * self.sidebar_flap.reveal()
                    } else {
                        0.0
                    };
                    self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + inset, r.y1);
                    self.overview_rect = Rect::new(r.x0 + inset, r.y0, r.x1, r.y1);
                    self.scrim_rect = Rect::ZERO;
                }
                ShellMode::Narrow => {
                    self.overview_rect = r;
                    if has_sidebar {
                        let slide = self.sidebar_flap.slide();
                        let x0 = r.x0 - sw * (1.0 - slide);
                        self.sidebar_rect = Rect::new(x0, r.y0, x0 + sw, r.y1);
                        self.scrim_rect = if self.sidebar_flap.reveal() > 0.001 {
                            r
                        } else {
                            Rect::ZERO
                        };
                    } else {
                        self.sidebar_rect = Rect::ZERO;
                        self.scrim_rect = Rect::ZERO;
                    }
                }
            }
            self.recompute_affordance();
            return;
        }

        match self.mode() {
            ShellMode::Wide => {
                // Content is inset by the eased sidebar width so the collapse
                // plays smoothly without overlap.
                let inset = if has_sidebar {
                    sw * self.sidebar_flap.reveal()
                } else {
                    0.0
                };
                self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + inset, r.y1);

                let content_x0 = r.x0 + inset;
                let content_w = (r.x1 - content_x0).max(0.0);
                let ow = self
                    .overview_width
                    .min((content_w - MIN_DETAIL_WIDTH).max(0.0))
                    .max(0.0);
                self.overview_rect = Rect::new(content_x0, r.y0, content_x0 + ow, r.y1);
                self.detail_rect = Rect::new(content_x0 + ow, r.y0, r.x1, r.y1);
                self.scrim_rect = Rect::ZERO;
            }
            ShellMode::Narrow => {
                // Overview slides off-screen left as detail slides in right.
                let p = self.push.slide(); // 0 = overview, 1 = detail
                self.overview_rect = Rect::new(r.x0 - w * p, r.y0, r.x1 - w * p, r.y1);
                self.detail_rect =
                    Rect::new(r.x0 + w * (1.0 - p), r.y0, r.x1 + w * (1.0 - p), r.y1);

                if has_sidebar {
                    let slide = self.sidebar_flap.slide();
                    let x0 = r.x0 - sw * (1.0 - slide);
                    self.sidebar_rect = Rect::new(x0, r.y0, x0 + sw, r.y1);
                    self.scrim_rect = if self.sidebar_flap.reveal() > 0.001 {
                        r
                    } else {
                        Rect::ZERO
                    };
                } else {
                    self.sidebar_rect = Rect::ZERO;
                    self.scrim_rect = Rect::ZERO;
                }
            }
        }

        self.recompute_affordance();
    }

    fn recompute_affordance(&mut self) {
        let r = self.rect;
        self.affordance = self.top_left_affordance();
        self.affordance_rect = match self.affordance {
            Affordance::None => Rect::ZERO,
            Affordance::SidebarToggle => Rect::new(
                r.x0 + CHIP_INSET,
                r.y0 + CHIP_INSET,
                r.x0 + CHIP_INSET + CHIP_SIZE,
                r.y0 + CHIP_INSET + CHIP_SIZE,
            ),
            Affordance::Back => Rect::new(
                r.x0 + CHIP_INSET,
                r.y0 + CHIP_INSET,
                r.x0 + CHIP_INSET + BACK_CHIP_WIDTH,
                r.y0 + CHIP_INSET + CHIP_SIZE,
            ),
        };
    }

    fn sidebar_visible(&self) -> bool {
        self.has_sidebar() && self.sidebar_flap.reveal() > 0.001
    }

    /// [`Self::sidebar_rect`] with the affordance chip's band reserved so
    /// the chip doesn't clip the sidebar's first row. The panel fill still
    /// uses the full rect, so the band reads as sidebar-coloured, not a notch.
    pub fn sidebar_content_rect(&self) -> Rect {
        let r = self.sidebar_rect;
        if self.affordance != Affordance::SidebarToggle || r.height() <= 0.0 {
            return r;
        }
        let band = CHIP_INSET * 2.0 + CHIP_SIZE;
        Rect::new(r.x0, (r.y0 + band).min(r.y1), r.x1, r.y1)
    }

    /// `r` with the affordance chip's band reserved when the chip floats
    /// over that pane. Resolved here, in the same pass that positions the
    /// chip, rather than by the host: a host re-deriving it from last
    /// frame's rect would see the content shift by a frame once the shell is
    /// actually measured, and a moving target is one the pointer can't hit.
    fn content_child_rect(&self, r: Rect) -> Rect {
        if self.affordance == Affordance::None || r.width() <= 0.0 || r.height() <= 0.0 {
            return r;
        }
        let chip = self.affordance_rect;
        if chip.x1 <= r.x0 || chip.x0 >= r.x1 {
            return r;
        }
        let band = CHIP_INSET * 2.0 + CHIP_SIZE;
        Rect::new(r.x0, (r.y0 + band).min(r.y1), r.x1, r.y1)
    }

    pub fn overview_content_rect(&self) -> Rect {
        self.content_child_rect(self.overview_rect)
    }

    pub fn detail_content_rect(&self) -> Rect {
        self.content_child_rect(self.detail_rect)
    }

    /// The drawer slides *over* live content, so without this fill the
    /// content reads straight through it. Drawn under the child so a sidebar
    /// that paints its own background simply covers this.
    fn fill_panel(scene: &mut Scene, transform: Affine, theme: &Theme, r: Rect) {
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &r);
    }

    fn place_children(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        let o_node = self.overview.build_layout(&mut taffy, theme, &mut map);
        let d_node = self
            .detail
            .as_mut()
            .map(|d| d.build_layout(&mut taffy, theme, &mut map));
        let s_node = self
            .sidebar
            .as_mut()
            .map(|s| s.build_layout(&mut taffy, theme, &mut map));

        let sidebar_child_rect = self.sidebar_content_rect();
        let overview_child_rect = self.content_child_rect(self.overview_rect);
        let detail_child_rect = self.content_child_rect(self.detail_rect);
        let mut jobs: Vec<(NodeId, Rect)> = vec![(o_node, overview_child_rect)];
        if let Some(dn) = d_node {
            jobs.push((dn, detail_child_rect));
        }
        if let Some(sn) = s_node {
            jobs.push((sn, sidebar_child_rect));
        }
        for (node, rect) in jobs {
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let _ = taffy.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(to_f32(rect.width())),
                    height: taffy::AvailableSpace::Definite(to_f32(rect.height())),
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

        self.overview.apply_layout(
            &taffy,
            o_node,
            Point::new(overview_child_rect.x0, overview_child_rect.y0),
        );
        if let (Some(d), Some(dn)) = (self.detail.as_mut(), d_node) {
            d.apply_layout(
                &taffy,
                dn,
                Point::new(detail_child_rect.x0, detail_child_rect.y0),
            );
        }
        if let (Some(sb), Some(sn)) = (self.sidebar.as_mut(), s_node) {
            sb.apply_layout(
                &taffy,
                sn,
                Point::new(sidebar_child_rect.x0, sidebar_child_rect.y0),
            );
        }

        self.inner_taffy = Some(taffy);
        self.overview_node = Some(o_node);
        self.detail_node = d_node;
        self.sidebar_node = s_node;
    }

    /// Driven from the draw path, never `update` (which `ui::App` never
    /// calls), so the slides actually play and settle to no frames at rest
    /// (HIG §5).
    fn self_tick(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        if theme.reduce_motion {
            let sidebar_before = self.sidebar_flap.reveal();
            let push_before = self.push.reveal();
            self.sidebar_flap.settle(self.sidebar_split_layout());
            self.push.settle(SplitLayout::Docked);
            self.animating = false;
            self.last_tick = Some(Instant::now());
            if self.sidebar_flap.reveal().to_bits() != sidebar_before.to_bits()
                || self.push.reveal().to_bits() != push_before.to_bits()
            {
                self.recompute_rects();
                self.place_children(theme, text_renderer);
            }
            return;
        }
        let now = Instant::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);

        let a = self.sidebar_flap.tick(self.sidebar_split_layout(), dt);
        let b = self.push.tick(SplitLayout::Docked, dt);
        self.animating = a || b;
        if self.animating {
            self.recompute_rects();
            self.place_children(theme, text_renderer);
        }
    }

    // --- chrome painting -------------------------------------------------

    /// The glyph is always a hamburger, a chevron, or a back-arrow —
    /// structurally never an `X`.
    fn draw_affordance(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.affordance == Affordance::None {
            return;
        }
        let rect = self.affordance_rect;

        let bg = RoundedRect::from_rect(rect, 8.0);
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &bg);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &bg,
        );

        let glyph_box = Rect::new(rect.x0, rect.y0, rect.x0 + CHIP_SIZE, rect.y1);
        match self.affordance {
            Affordance::None => {}
            Affordance::Back => {
                draw_icon(
                    scene,
                    transform,
                    glyph_box.inflate(-9.0, -9.0),
                    Icon::ChevronLeft,
                    theme.text_primary,
                    1.75,
                );
                text_renderer.draw_text(
                    scene,
                    "Back",
                    14.0,
                    theme.text_primary,
                    transform.pre_translate(Vec2::new(
                        glyph_box.x1 - 2.0,
                        rect.y0 + CHIP_SIZE / 2.0 - 9.0,
                    )),
                    None,
                );
            }
            Affordance::SidebarToggle => {
                if self.is_sidebar_open() {
                    draw_icon(
                        scene,
                        transform,
                        glyph_box.inflate(-9.0, -9.0),
                        Icon::ChevronLeft,
                        theme.text_primary,
                        1.75,
                    );
                } else {
                    Self::draw_hamburger(scene, transform, glyph_box, theme.text_primary);
                }
            }
        }
    }

    fn draw_hamburger(scene: &mut Scene, transform: Affine, box_rect: Rect, color: Color) {
        let cx = box_rect.center().x;
        let cy = box_rect.center().y;
        let half_w = 9.0;
        let gap = 5.5;
        let stroke = Stroke::new(2.0)
            .with_caps(Cap::Round)
            .with_join(Join::Round);
        for dy in [-gap, 0.0, gap] {
            let line = Line::new(
                Point::new(cx - half_w, cy + dy),
                Point::new(cx + half_w, cy + dy),
            );
            scene.stroke(&stroke, transform, color, None, &line);
        }
    }
}

impl Widget for NavShell {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.overview.sync(state, path);
        if let Some(d) = self.detail.as_mut() {
            d.sync(state, path);
        }
        if let Some(s) = self.sidebar.as_mut() {
            s.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Children live in an isolated inner tree: their rects escape normal
        // flow in overlay/push mode.
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

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // A resize crossing the breakpoint drops a stale sidebar preference
        // so it returns to auto behaviour.
        self.sidebar_flap.reconcile(self.sidebar_split_layout());

        let mode = self.mode();
        if self.last_mode == Some(ShellMode::Wide)
            && mode == ShellMode::Narrow
            && self.has_sidebar()
            && self.has_detail()
        {
            self.pane = self.collapse_content_pane();
            // A structural jump, not a user push — snap rather than animate.
            self.push.snap(self.pane == NavPane::Detail);
        }
        self.last_mode = Some(mode);

        self.recompute_rects();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        self.place_children(&theme, &mut tr);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        // Slides advance from the draw path (`self_tick`); `ui::App` never
        // calls `update`. Just forward and report whether frames are needed.
        let mut any = self.overview.update(state, path);
        if let Some(d) = self.detail.as_mut() {
            any |= d.update(state, path);
        }
        if let Some(s) = self.sidebar.as_mut() {
            any |= s.update(state, path);
        }
        any || self.sidebar_flap.is_animating(self.sidebar_split_layout())
            || self.push.is_animating(SplitLayout::Docked)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.self_tick(theme, text_renderer);

        // Content first, behind the overlay drawer; off-screen push panes
        // simply fall outside the framebuffer.
        self.overview
            .draw(scene, transform, text_renderer, theme, presence);
        if let Some(d) = self.detail.as_mut() {
            d.draw(scene, transform, text_renderer, theme, presence);
        }
        if self.sidebar_visible()
            && let Some(s) = self.sidebar.as_mut()
        {
            // Scrim must sit between content and the drawer.
            if self.scrim_rect.width() > 0.0 {
                let alpha = to_f32(self.sidebar_flap.reveal() * 0.5);
                let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(alpha);
                scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
            }
            Self::fill_panel(scene, transform, theme, self.sidebar_rect);
            s.draw(scene, transform, text_renderer, theme, presence);
        }
        // Scrim already drawn above; this only adds dividers + affordance.
        self.draw_chrome_no_scrim(scene, transform, text_renderer, theme);
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
        self.self_tick(theme, text_renderer);
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.overview.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        if let Some(d) = self.detail.as_mut() {
            d.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
        if self.sidebar_visible() {
            if self.scrim_rect.width() > 0.0 {
                let alpha = to_f32(self.sidebar_flap.reveal() * 0.5);
                let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(alpha);
                scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
            }
            Self::fill_panel(scene, transform, theme, self.sidebar_rect);
            if let Some(s) = self.sidebar.as_mut() {
                s.draw_with_presence(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    permissions,
                );
            }
        }
        self.draw_chrome_no_scrim(scene, transform, text_renderer, theme);
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
                // Dismiss the overlay drawer, else pop the pushed detail.
                if self.is_sidebar_open() && self.sidebar_split_layout() == SplitLayout::Overlay {
                    self.close_sidebar();
                    *handled = true;
                    return;
                }
                if self.content_layout() == ContentLayout::Push && self.pane == NavPane::Detail {
                    self.show_overview();
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // 1. The top-left affordance owns its chip.
                if self.affordance != Affordance::None
                    && self.affordance_rect.contains(self.last_cursor)
                {
                    match self.affordance {
                        Affordance::SidebarToggle => self.toggle_sidebar(),
                        Affordance::Back => self.show_overview(),
                        Affordance::None => {}
                    }
                    *handled = true;
                    return;
                }
                // 2. A scrim click dismisses the overlay drawer.
                if self.is_sidebar_open()
                    && self.sidebar_split_layout() == SplitLayout::Overlay
                    && self.scrim_rect.width() > 0.0
                    && self.scrim_rect.contains(self.last_cursor)
                    && !self.sidebar_rect.contains(self.last_cursor)
                {
                    self.close_sidebar();
                    *handled = true;
                    return;
                }
                // 3. Record which content pane the user engaged (a press over
                //    an open sidebar drawer is navigation, not content).
                if self.detail.is_some()
                    && !(self.sidebar_visible() && self.sidebar_rect.contains(self.last_cursor))
                {
                    if self.detail_rect.width() > 0.0 && self.detail_rect.contains(self.last_cursor)
                    {
                        self.active_pane_hint = Some(NavPane::Detail);
                    } else if self.overview_rect.width() > 0.0
                        && self.overview_rect.contains(self.last_cursor)
                    {
                        self.active_pane_hint = Some(NavPane::Overview);
                    }
                }
            }
            _ => {}
        }
        if *handled {
            return;
        }
        self.route_event_to_children(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
        );
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = vec![self.overview.as_ref()];
        if let Some(d) = self.detail.as_ref() {
            v.push(d.as_ref());
        }
        if let Some(s) = self.sidebar.as_ref() {
            v.push(s.as_ref());
        }
        v
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = vec![self.overview.as_mut()];
        if let Some(d) = self.detail.as_mut() {
            v.push(d.as_mut());
        }
        if let Some(s) = self.sidebar.as_mut() {
            v.push(s.as_mut());
        }
        v
    }

    fn name(&self) -> &'static str {
        "NavShell"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        self.animating
            || self.sidebar_flap.is_animating(self.sidebar_split_layout())
            || self.push.is_animating(SplitLayout::Docked)
            || self.overview.needs_animation_frame()
            || self
                .detail
                .as_ref()
                .is_some_and(|d| d.needs_animation_frame())
            || self
                .sidebar
                .as_ref()
                .is_some_and(|s| s.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = self.overview.take_structure_dirty();
        if let Some(det) = self.detail.as_mut() {
            d |= det.take_structure_dirty();
        }
        if let Some(s) = self.sidebar.as_mut() {
            d |= s.take_structure_dirty();
        }
        d
    }

    fn needs_idle_sync(&self) -> bool {
        self.animating
            || self.overview.needs_idle_sync()
            || self.detail.as_ref().is_some_and(|d| d.needs_idle_sync())
            || self.sidebar.as_ref().is_some_and(|s| s.needs_idle_sync())
    }
}

impl NavShell {
    /// Routes `event` to child panes in front-to-back order: an open overlay
    /// drawer is topmost so it routes first; otherwise the front content
    /// pane (for narrow push) routes before the back one. `d_node` is absent
    /// for a sidebar-plus-content shell but must not stop the sidebar/
    /// overview panes from being routed to. Split out of `handle_event` to
    /// keep that under the line cap.
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards Widget::handle_event's own parameter list to route to the right \
                  child pane; bundling it would mean bundling the trait method too"
    )]
    fn route_event_to_children(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) {
        enum Which {
            Sidebar,
            Overview,
            Detail,
        }

        let (Some(taffy), Some(o_node)) = (self.inner_taffy.as_ref(), self.overview_node) else {
            return;
        };
        let d_node = self.detail_node;
        let sidebar_first =
            self.is_sidebar_open() && self.sidebar_split_layout() == SplitLayout::Overlay;

        let mut order: Vec<(Which, NodeId)> = Vec::new();
        if sidebar_first && let Some(sn) = self.sidebar_node {
            order.push((Which::Sidebar, sn));
        }
        // Front content pane first for narrow push; harmless order in wide.
        match (self.pane, d_node) {
            (NavPane::Detail, Some(dn)) => {
                order.push((Which::Detail, dn));
                order.push((Which::Overview, o_node));
            }
            (_, Some(dn)) => {
                order.push((Which::Overview, o_node));
                order.push((Which::Detail, dn));
            }
            (_, None) => order.push((Which::Overview, o_node)),
        }
        if !sidebar_first && let Some(sn) = self.sidebar_node {
            order.push((Which::Sidebar, sn));
        }

        for (which, child_node) in order {
            if *handled {
                break;
            }
            let target: &mut dyn Widget = match which {
                Which::Sidebar => match self.sidebar.as_mut() {
                    Some(s) => s.as_mut(),
                    None => continue,
                },
                Which::Overview => self.overview.as_mut(),
                Which::Detail => match self.detail.as_mut() {
                    Some(d) => d.as_mut(),
                    None => continue,
                },
            };
            target.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
    }

    /// Scrim is painted inline between content and the drawer in each draw
    /// path instead, so its z-order is correct.
    fn draw_chrome_no_scrim(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.mode() == ShellMode::Wide {
            let stroke = Stroke::new(1.0);
            if self.sidebar_visible() {
                let x = self.sidebar_rect.x1;
                scene.stroke(
                    &stroke,
                    transform,
                    theme.border_standard,
                    None,
                    &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
                );
            }
            let x = self.overview_rect.x1;
            scene.stroke(
                &stroke,
                transform,
                theme.border_standard,
                None,
                &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
            );
        }
        self.draw_affordance(scene, transform, text_renderer, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Text;
    use vello_encoding::DrawTag;

    fn shell(w: f64, with_sidebar: bool) -> NavShell {
        let mut ns = if with_sidebar {
            NavShell::new(
                Box::new(Text::new("overview")),
                Box::new(Text::new("detail")),
            )
            .with_sidebar(Box::new(Text::new("sidebar")))
        } else {
            NavShell::new(
                Box::new(Text::new("overview")),
                Box::new(Text::new("detail")),
            )
        };
        lay_out(&mut ns, w, 640.0);
        ns
    }

    fn lay_out(ns: &mut NavShell, w: f64, h: f64) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = ns.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(to_f32(w)),
                    height: taffy::AvailableSpace::Definite(to_f32(h)),
                },
            )
            .unwrap();
        ns.apply_layout(&taffy, node, Point::ZERO);
    }

    fn sidebar_content_shell(w: f64) -> NavShell {
        let mut ns = NavShell::sidebar_over_content(
            Box::new(Text::new("places")),
            Box::new(Text::new("files")),
        );
        lay_out(&mut ns, w, 640.0);
        // Flap is open-when-docked, so a first narrow frame is mid-easing;
        // snap to the settled frame every assertion here is about.
        ns.snap();
        ns
    }

    #[test]
    fn mutable_children_match_immutable_children_in_logical_order() {
        let mut ns = NavShell::new(
            Box::new(Text::new("overview").with_id("overview")),
            Box::new(Text::new("detail").with_id("detail")),
        )
        .with_sidebar(Box::new(Text::new("sidebar").with_id("sidebar")));

        let immutable: Vec<String> = Widget::children(&ns)
            .into_iter()
            .map(|child| child.test_id().unwrap_or_default().to_string())
            .collect();
        let mutable: Vec<String> = Widget::children_mut(&mut ns)
            .into_iter()
            .map(|child| child.test_id().unwrap_or_default().to_string())
            .collect();

        assert_eq!(immutable, vec!["overview", "detail", "sidebar"]);
        assert_eq!(mutable, immutable);
    }

    // --- the chip's band over the content --------------------------------

    #[test]
    fn narrow_content_starts_below_the_chip_that_floats_over_it() {
        let ns = sidebar_content_shell(600.0);
        assert_eq!(ns.top_left_affordance(), Affordance::SidebarToggle);
        let pane = ns.overview_rect();
        let child = ns.content_child_rect(pane);
        assert_eq!(
            child.y0,
            pane.y0 + CHIP_INSET * 2.0 + CHIP_SIZE,
            "the content has to start below the chip, not under it",
        );
        assert!(
            child.y0 >= ns.affordance_rect.y1,
            "the reserved band must clear the chip's own rect ({:?})",
            ns.affordance_rect,
        );
        assert_eq!(
            (child.x0, child.x1),
            (pane.x0, pane.x1),
            "the band is vertical only",
        );
    }

    #[test]
    fn the_public_content_rects_are_what_the_layout_uses() {
        for width in [600.0_f64, 1200.0] {
            let ns = sidebar_content_shell(width);
            assert_eq!(
                ns.overview_content_rect(),
                ns.content_child_rect(ns.overview_rect()),
                "overview accessor disagrees with the laid-out child at {width}",
            );
            assert_eq!(
                ns.detail_content_rect(),
                ns.content_child_rect(ns.detail_rect()),
                "detail accessor disagrees with the laid-out child at {width}",
            );
        }
    }

    #[test]
    fn wide_content_keeps_its_full_height_because_the_sidebar_holds_the_corner() {
        let ns = sidebar_content_shell(1200.0);
        assert_eq!(ns.mode(), ShellMode::Wide);
        let pane = ns.overview_rect();
        assert_eq!(
            ns.content_child_rect(pane),
            pane,
            "a docked sidebar owns the top-left corner; the content is untouched",
        );
    }

    // --- sidebar + single content pane (HIG §1a, no §1b split) -----------

    #[test]
    fn narrow_sidebar_over_content_keeps_the_content_full_width() {
        let ns = sidebar_content_shell(600.0);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(ns.content_layout(), ContentLayout::Single);
        assert_eq!(
            ns.overview_rect().width(),
            600.0,
            "the content pane must span the whole narrow window",
        );
        assert_eq!(
            ns.overview_rect().x0,
            0.0,
            "and start at the window's left edge, not offset by a sidebar",
        );
        assert!(!ns.is_detail_shown(), "there is no detail pane to push in");
        assert_eq!(
            ns.detail_rect(),
            Rect::ZERO,
            "no detail pane means no detail geometry",
        );
    }

    #[test]
    fn narrow_sidebar_starts_closed_and_opens_as_an_overlay_drawer() {
        let mut ns = sidebar_content_shell(600.0);
        assert!(!ns.is_sidebar_open(), "the drawer is closed by default");
        assert_eq!(
            ns.scrim_rect(),
            Rect::ZERO,
            "no scrim while the drawer is closed",
        );
        assert_eq!(
            ns.top_left_affordance(),
            Affordance::SidebarToggle,
            "the corner offers the hamburger, never a Back arrow",
        );

        ns.open_sidebar();
        ns.snap();
        assert!(ns.is_sidebar_open());
        assert!(ns.sidebar_rect().width() > 0.0, "the drawer is on screen");
        assert_eq!(
            ns.overview_rect().width(),
            600.0,
            "the drawer draws OVER the content — the content does not shrink \
             or slide away",
        );
        assert!(
            ns.scrim_rect().width() > 0.0,
            "an open drawer dims the content behind a scrim",
        );
    }

    #[test]
    fn wide_sidebar_over_content_docks_beside_a_full_height_content_pane() {
        let ns = sidebar_content_shell(1200.0);
        assert_eq!(ns.mode(), ShellMode::Wide);
        assert_eq!(ns.content_layout(), ContentLayout::Single);
        assert!(ns.sidebar_rect().width() > 0.0, "the sidebar is docked");
        assert_eq!(
            ns.overview_rect().x0,
            ns.sidebar_rect().x1,
            "the content starts where the sidebar ends",
        );
        assert_eq!(
            ns.overview_rect().x1,
            1200.0,
            "and runs to the window's right edge",
        );
        assert_eq!(ns.detail_rect(), Rect::ZERO);
    }

    #[test]
    fn sidebar_over_content_reflows_across_the_breakpoint_both_ways() {
        let mut ns = sidebar_content_shell(1200.0);
        assert_eq!(ns.mode(), ShellMode::Wide);
        for w in [600.0, 1200.0, 480.0, 900.0] {
            lay_out(&mut ns, w, 640.0);
            ns.snap();
            assert_eq!(
                ns.overview_rect().x1,
                w,
                "the content pane must reach the right edge at width {w}",
            );
            assert!(
                ns.overview_rect().width() > 0.0,
                "the content pane must never be laid out to zero width at {w}",
            );
        }
    }

    // --- mode resolution -------------------------------------------------

    #[test]
    fn breakpoint_boundary_is_inclusive_wide() {
        assert_eq!(ShellMode::from_width(700.0, 700.0), ShellMode::Wide);
        assert_eq!(ShellMode::from_width(699.0, 700.0), ShellMode::Narrow);
    }

    #[test]
    fn wide_is_side_by_side_with_docked_sidebar() {
        let ns = shell(1200.0, true);
        assert_eq!(ns.mode(), ShellMode::Wide);
        assert_eq!(ns.content_layout(), ContentLayout::SideBySide);
        assert_eq!(ns.sidebar_layout(), Some(SplitLayout::Docked));
        assert!(ns.is_sidebar_open(), "docked sidebar opens by default");
        assert!(ns.sidebar_rect().width() > 1.0, "docked sidebar has width");
        assert!(
            ns.overview_rect().x0 >= ns.sidebar_rect().x1 - 0.5,
            "overview sits right of the sidebar"
        );
        assert!(
            ns.detail_rect().x0 >= ns.overview_rect().x1 - 0.5,
            "detail sits right of the overview"
        );
        assert!(ns.detail_rect().width() >= MIN_DETAIL_WIDTH - 1.0);
        assert_eq!(ns.scrim_rect(), Rect::ZERO, "no scrim in wide mode");
    }

    #[test]
    fn narrow_is_push_with_overlay_sidebar() {
        let ns = shell(420.0, true);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(ns.content_layout(), ContentLayout::Push);
        assert_eq!(ns.sidebar_layout(), Some(SplitLayout::Overlay));
        assert!(
            !ns.is_sidebar_open(),
            "overlay sidebar is closed by default"
        );
        assert!(
            (ns.overview_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "overview is full-width by default"
        );
        assert!(
            ns.detail_rect().x0 >= ns.get_rect().x1 - 1.0,
            "detail is parked off-screen to the right until pushed"
        );
    }

    #[test]
    fn content_only_shell_has_no_sidebar() {
        let ns = shell(1200.0, false);
        assert!(!ns.has_sidebar());
        assert_eq!(ns.sidebar_layout(), None);
        assert!(!ns.is_sidebar_open());
        assert!((ns.overview_rect().x0 - ns.get_rect().x0).abs() < 0.5);
    }

    // --- push / pop ------------------------------------------------------

    #[test]
    fn narrow_push_shows_detail_then_pops_back() {
        let mut ns = shell(420.0, true);
        assert_eq!(ns.pane(), NavPane::Overview);
        ns.show_detail();
        ns.snap_pane(NavPane::Detail);
        assert_eq!(ns.pane(), NavPane::Detail);
        assert!(ns.is_detail_shown());
        assert!(
            (ns.detail_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "pushed detail occupies the pane"
        );
        assert!(
            ns.overview_rect().x1 <= ns.get_rect().x0 + 1.0,
            "overview slid off-screen left"
        );
        ns.show_overview();
        ns.snap_pane(NavPane::Overview);
        assert_eq!(ns.pane(), NavPane::Overview);
        assert!(
            (ns.overview_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "overview back in place"
        );
    }

    #[test]
    fn wide_shows_both_content_panes_regardless_of_push_target() {
        let mut ns = shell(1200.0, true);
        ns.show_detail(); // sets target, but wide keeps both abreast
        ns.snap();
        assert!(
            ns.overview_rect().width() > 1.0,
            "overview still visible wide"
        );
        assert!(ns.detail_rect().width() > 1.0, "detail still visible wide");
    }

    // --- affordance is a collapse control, NEVER an X --------------------

    #[test]
    fn affordance_is_sidebar_toggle_in_wide_with_sidebar() {
        let ns = shell(1200.0, true);
        assert_eq!(ns.top_left_affordance(), Affordance::SidebarToggle);
        assert!(ns.collapse_button_rect().width() > 0.0);
        assert_eq!(ns.back_button_rect(), Rect::ZERO);
    }

    #[test]
    fn affordance_is_hamburger_to_summon_narrow_drawer() {
        let ns = shell(420.0, true);
        assert_eq!(ns.top_left_affordance(), Affordance::SidebarToggle);
        assert!(
            !ns.is_sidebar_open(),
            "glyph should be the hamburger (closed)"
        );
    }

    #[test]
    fn affordance_becomes_back_when_detail_pushed_narrow() {
        let mut ns = shell(420.0, true);
        ns.snap_pane(NavPane::Detail);
        assert_eq!(ns.top_left_affordance(), Affordance::Back);
        assert!(ns.back_button_rect().width() > 0.0);
        assert_eq!(ns.collapse_button_rect(), Rect::ZERO);
    }

    #[test]
    fn content_only_wide_has_no_affordance() {
        let ns = shell(1200.0, false);
        assert_eq!(ns.top_left_affordance(), Affordance::None);
        assert_eq!(ns.collapse_button_rect(), Rect::ZERO);
        assert_eq!(ns.back_button_rect(), Rect::ZERO);
    }

    // --- collapse -------------------------------------------------------

    #[test]
    fn toggle_collapses_docked_sidebar_and_reclaims_width() {
        let mut ns = shell(1200.0, true);
        let overview_x_open = ns.overview_rect().x0;
        ns.toggle_sidebar();
        ns.snap_sidebar(false);
        assert!(!ns.is_sidebar_open());
        assert!(
            ns.overview_rect().x0 < overview_x_open - 1.0,
            "collapsing the docked sidebar reclaims its width for content"
        );
        assert_eq!(
            ns.scrim_rect(),
            Rect::ZERO,
            "docked collapse shows no scrim"
        );
    }

    #[test]
    fn toggle_opens_narrow_drawer_with_scrim() {
        let mut ns = shell(420.0, true);
        ns.toggle_sidebar();
        ns.snap_sidebar(true);
        assert!(ns.is_sidebar_open());
        assert!(
            ns.scrim_rect().width() > 0.0,
            "open drawer dims the content"
        );
        assert!(
            (ns.sidebar_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "revealed drawer sits flush at the left edge"
        );
    }

    // --- self-routing ----------------------------------------------------

    #[test]
    fn esc_dismisses_open_narrow_drawer() {
        let mut ns = shell(420.0, true);
        ns.open_sidebar();
        ns.snap_sidebar(true);
        assert!(ns.is_sidebar_open());
        let mut handled = false;
        dispatch(&mut ns, &esc_event(), &mut handled);
        assert!(handled, "Esc on an open drawer is consumed");
        assert!(!ns.is_sidebar_open(), "Esc closes the drawer");
    }

    #[test]
    fn esc_pops_pushed_detail() {
        let mut ns = shell(420.0, true);
        ns.snap_pane(NavPane::Detail);
        let mut handled = false;
        dispatch(&mut ns, &esc_event(), &mut handled);
        assert!(handled, "Esc on a pushed detail is consumed");
        assert_eq!(ns.pane(), NavPane::Overview, "Esc pops back to overview");
    }

    #[test]
    fn scrim_click_dismisses_narrow_drawer() {
        let mut ns = shell(420.0, true);
        ns.open_sidebar();
        ns.snap_sidebar(true);
        // Click to the right of the drawer, in the scrim.
        let pt = Point::new(ns.get_rect().x1 - 20.0, 320.0);
        let mut handled = false;
        dispatch(&mut ns, &cursor_at(pt), &mut handled);
        dispatch(&mut ns, &left_press(), &mut handled);
        assert!(handled, "scrim click is consumed");
        assert!(!ns.is_sidebar_open(), "scrim click closes the drawer");
    }

    #[test]
    fn collapse_button_click_toggles_sidebar() {
        let mut ns = shell(1200.0, true);
        assert!(ns.is_sidebar_open());
        let c = ns.collapse_button_rect().center();
        let mut handled = false;
        dispatch(&mut ns, &cursor_at(c), &mut handled);
        dispatch(&mut ns, &left_press(), &mut handled);
        assert!(handled, "the collapse chip owns its click");
        assert!(
            !ns.is_sidebar_open(),
            "clicking the collapse button collapses the sidebar"
        );
    }

    #[test]
    fn back_button_click_pops_detail() {
        let mut ns = shell(420.0, true);
        ns.snap_pane(NavPane::Detail);
        let c = ns.back_button_rect().center();
        let mut handled = false;
        dispatch(&mut ns, &cursor_at(c), &mut handled);
        dispatch(&mut ns, &left_press(), &mut handled);
        assert!(handled, "the back chip owns its click");
        assert_eq!(ns.pane(), NavPane::Overview, "Back pops to overview");
    }

    // --- breakpoint reconcile -------------------------------------------

    #[test]
    fn resize_across_breakpoint_returns_sidebar_to_auto() {
        // wide+open, user collapses, resize narrow (auto closed), resize
        // back wide (auto open again — stale preference dropped).
        let mut ns = shell(1200.0, true);
        ns.toggle_sidebar();
        assert!(!ns.is_sidebar_open());
        lay_out(&mut ns, 420.0, 640.0);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert!(!ns.is_sidebar_open(), "narrow default is closed");
        lay_out(&mut ns, 1200.0, 640.0);
        assert!(
            ns.is_sidebar_open(),
            "crossing the breakpoint drops the stale collapse preference"
        );
    }

    // --- rendering -------------------------------------------------------

    #[test]
    fn draw_emits_fills() {
        let mut ns = shell(1200.0, true);
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
        assert!(fills >= 1, "a laid-out shell must paint something");
    }

    #[test]
    fn push_slides_across_frames_then_settles() {
        let mut ns = shell(420.0, true);
        ns.snap_pane(NavPane::Overview);
        ns.show_detail();

        draw_frame(&mut ns); // establishes the clock; dt == 0, no jump
        assert_eq!(ns.push.reveal(), 0.0, "first frame must not jump the slide");

        std::thread::sleep(std::time::Duration::from_millis(16));
        draw_frame(&mut ns);
        let r = ns.push.reveal();
        assert!(r > 0.0 && r < 1.0, "mid-slide after one frame (reveal {r})");
        assert!(
            ns.needs_animation_frame(),
            "mid-slide keeps requesting frames"
        );

        for _ in 0..240 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            draw_frame(&mut ns);
            if !ns.push.is_animating(SplitLayout::Docked) {
                break;
            }
        }
        assert!(
            (ns.push.reveal() - 1.0).abs() < 0.01,
            "slide settles fully in"
        );
        assert!(
            !ns.needs_animation_frame(),
            "a settled shell requests no frames"
        );
    }

    #[test]
    fn reduced_motion_snaps_push_and_drawer_to_their_targets() {
        let mut ns = shell(420.0, true);
        ns.snap_pane(NavPane::Overview);
        ns.show_detail();
        ns.open_sidebar();

        let theme = Theme::dark().with_reduce_motion(true);
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let permissions = crate::permissions::PermissionContext::permissive();
        let mut scene = Scene::new();
        ns.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &permissions,
        );

        assert_eq!(ns.push.reveal(), 1.0);
        assert_eq!(ns.sidebar_flap.reveal(), 1.0);
        assert!(!ns.needs_animation_frame());
    }

    #[test]
    fn snap_stays_deterministic_through_draw() {
        let mut ns = shell(420.0, true);
        ns.show_detail();
        ns.snap();
        assert_eq!(ns.push.reveal(), 1.0, "snap jumps straight to detail");
        std::thread::sleep(std::time::Duration::from_millis(20));
        draw_frame(&mut ns);
        assert_eq!(
            ns.push.reveal(),
            1.0,
            "a snapped shell must not drift on draw"
        );
        assert!(
            !ns.needs_animation_frame(),
            "a settled shell requests no frames"
        );
    }

    #[test]
    fn swapping_panes_preserves_push_state_and_relayouts_new_child() {
        let mut ns = shell(420.0, false);
        ns.snap_pane(NavPane::Detail);
        assert!(ns.is_detail_shown(), "pushed to detail before the swap");

        ns.set_overview(Box::new(Text::new("new-overview")));
        ns.set_detail(Box::new(Text::new("new-detail")));
        assert!(
            ns.is_detail_shown(),
            "the swap kept the shell showing the detail"
        );

        lay_out(&mut ns, 420.0, 640.0);
        ns.snap_pane(NavPane::Detail);
        assert!(
            (ns.detail_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "the swapped-in detail is laid out in the pane"
        );
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
        assert!(fills >= 1, "the swapped children paint after the swap");
    }

    // --- three-pane collapse keeps content, never the sidebar -----------

    #[test]
    fn three_pane_cold_collapse_keeps_the_rightmost_content_and_overlays_the_sidebar() {
        let mut ns = shell(1200.0, true); // wide, sidebar + overview + detail
        assert_eq!(ns.mode(), ShellMode::Wide);
        assert_eq!(ns.content_layout(), ContentLayout::SideBySide);

        // Resize narrow, then settle the reveal easing to the frame
        // `--shots` / headless renders.
        lay_out(&mut ns, 520.0, 640.0);
        ns.snap();
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(ns.content_layout(), ContentLayout::Push);

        assert_eq!(
            ns.pane(),
            NavPane::Detail,
            "a cold collapse keeps the right-most content, not the sidebar"
        );
        assert!(
            (ns.detail_rect().x0 - ns.get_rect().x0).abs() < 1.0
                && (ns.detail_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "the content pane spans the whole narrow window",
        );
        assert!(
            ns.overview_rect().x1 <= ns.get_rect().x0 + 1.0,
            "the overview is pushed off-screen to the left",
        );

        assert_eq!(ns.sidebar_layout(), Some(SplitLayout::Overlay));
        assert!(
            !ns.is_sidebar_open(),
            "the sidebar collapses to a closed drawer, not the shown pane"
        );
        assert_eq!(ns.scrim_rect(), Rect::ZERO, "a closed drawer lays no scrim");
        // `Back` wins the top-left while detail is pushed, so summon by call.
        ns.toggle_sidebar();
        ns.snap();
        assert!(
            ns.is_sidebar_open(),
            "the collapsed sidebar is still summonable as a drawer"
        );
    }

    #[test]
    fn three_pane_collapse_hint_overview_overrides_the_rightmost_default() {
        let mut ns = shell(1200.0, true);
        ns.hint_active_pane(NavPane::Overview);
        lay_out(&mut ns, 520.0, 640.0);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(
            ns.pane(),
            NavPane::Overview,
            "the hinted master content overrides the right-most default"
        );
        assert!(
            (ns.overview_rect().x0 - ns.get_rect().x0).abs() < 1.0
                && (ns.overview_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "the overview spans the whole narrow window",
        );
        assert!(
            !ns.is_sidebar_open(),
            "the sidebar is still a closed overlay drawer"
        );
    }

    #[test]
    fn three_pane_collapse_keeps_the_last_interacted_pane() {
        let mut ns = shell(1200.0, true);
        let ov = ns.overview_rect();
        assert!(
            ov.width() > 1.0,
            "wide side-by-side shows an overview column"
        );
        let pt = Point::new(ov.x0.midpoint(ov.x1), ov.y0.midpoint(ov.y1));
        let mut handled = false;
        dispatch(&mut ns, &cursor_at(pt), &mut handled);
        dispatch(&mut ns, &left_press(), &mut handled);

        lay_out(&mut ns, 520.0, 640.0);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(
            ns.pane(),
            NavPane::Overview,
            "the pane the user last engaged survives the collapse, overriding the right-most default",
        );
        assert!(
            (ns.overview_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "the overview fills the narrow window",
        );
    }

    #[test]
    fn content_only_collapse_keeps_the_overview_first_default() {
        let mut ns = shell(1200.0, false); // no sidebar
        lay_out(&mut ns, 520.0, 640.0);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(
            ns.pane(),
            NavPane::Overview,
            "a sidebar-less master/detail collapses to the overview, unchanged",
        );
    }

    // --- scaffolding -----------------------------------------------------

    fn draw_frame(ns: &mut NavShell) {
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
    fn dispatch(ns: &mut NavShell, event: &WindowEvent, handled: &mut bool) {
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
}
