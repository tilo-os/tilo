//! `NavShell` — the HIG §1 navigation primitive.
//!
//! A **sidebar** (§1a) docks/collapses via a hamburger when wide and becomes
//! an overlay drawer over content — never a full-screen page — when narrow.
//! A **content master/detail** (§1b) shows both panes side by side when wide
//! and pushes detail over overview (Back button) when narrow. Compose both
//! (`[ sidebar | overview | detail ]`) or use [`NavShell::new`] alone; the
//! shell owns the breakpoint, eased collapse/push slides, the scrim, and
//! button/scrim/Esc self-routing.

use crate::time::Instant;
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Cap, Join, Line, Point, Rect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{Icon, draw_icon};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Metrics, Spacing};
use crate::widget::TaffyTreeExt;
use crate::widget::nav_split::{FlapState, SplitLayout};
use crate::widget::{AuthoredExtent, Element, Widget};

/// The OS-wide genuine-sidebar width (`Metrics::sidebar_width`).
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_NAVSHELL_SIDEBAR_WIDTH: f64 = Metrics::BASE.sidebar_width as f64;
/// The OS-wide overview column width (`Metrics::overview_width`).
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
pub const DEFAULT_NAVSHELL_OVERVIEW_WIDTH: f64 = Metrics::BASE.overview_width as f64;
/// 20 px under `Metrics::compact_breakpoint` (720); pinned by tests and by
/// the apps that chose this shell, so recorded rather than moved.
pub const DEFAULT_NAVSHELL_BREAKPOINT: f64 = 700.0;
/// Detail pane's minimum width in the side-by-side layout; overview gives up
/// space first.
const MIN_DETAIL_WIDTH: f64 = 260.0;
/// Caps the per-frame dt so a stale `Instant` (first frame, or a parked
/// event loop) can't teleport a slide past its animation.
const MAX_FRAME_DT: f64 = 0.1;
/// Menu / Back chip square at 1×; grown with the body line by
/// `text_control_height` when the tree is built. `AppShell`'s Back button
/// is 34; neither is a `Metrics` height yet (see the F1 layout review, which
/// proposes `min_pointer_target` + `s3` inset for both).
const CHIP_SIZE: f64 = 36.0;
/// `spacing.s3` at 1×; the live value is read from the theme at build.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const CHIP_INSET: f64 = Spacing::BASE.s3 as f64;
/// Back chip width at 1×: the glyph square plus the "Back" label run.
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "toggle policy, layout cache validity, animation activity and dirtiness are independent flags"
)]
pub struct NavShell {
    sidebar: Option<Element>,
    overview: Element,
    /// `None` for a sidebar-plus-content app: no overview/detail split to
    /// push between, so the single content pane must stay visible at every
    /// width.
    detail: Option<Element>,

    /// The app's inputs as authored at 1× (HIG §9.6,
    /// [`AuthoredExtent`]); `None` means the token default
    /// (`theme.tokens.metrics`). Resolved against the theme in
    /// `build_layout` into the `breakpoint` / `*_width` fields below.
    authored_breakpoint: AuthoredExtent,
    authored_sidebar_width: Option<AuthoredExtent>,
    authored_narrow_sidebar_width: Option<AuthoredExtent>,
    authored_overview_width: Option<AuthoredExtent>,

    breakpoint: f64,
    sidebar_width: f64,
    /// Optional wider width for the narrow overlay drawer. This lets a
    /// destination list remain comfortably usable on a phone without making
    /// its persistent desktop sidebar unnecessarily wide.
    narrow_sidebar_width: Option<f64>,
    overview_width: f64,
    /// Detail pane's minimum width in the side-by-side layout, resolved
    /// from [`MIN_DETAIL_WIDTH`].
    min_detail_width: f64,
    /// The affordance chip's geometry resolved against the theme: the
    /// `s3` inset, the square grown with the body line
    /// ([`crate::widget::text_control_height`]) so a 2× "Back" label still
    /// fits, and the Back chip's width.
    chip_inset: f64,
    chip_size: f64,
    back_chip_width: f64,

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
    /// Pane constraints used by the retained child tree (origins may translate).
    inner_extents: Option<[(f64, f64); 3]>,
    /// Preserve the outer layout typography for the isolated child tree.
    layout_theme: Theme,
    /// Set by `adjust_layout_with_measure` once the host solver has solved
    /// the panes for this pass, consumed by the `apply_layout` that follows:
    /// a freshly measured child still reports `layout_needs_measure()` until
    /// it is drawn, and without this the apply would re-solve the very same
    /// geometry with a throwaway renderer.
    host_solved: bool,
    sidebar_node: Option<NodeId>,
    overview_node: Option<NodeId>,
    detail_node: Option<NodeId>,

    animating: bool,
    dirty: bool,
    host_structure_dirty: bool,
    host_interactive_panels: [bool; 3],
    last_tick: Option<Instant>,
}

impl NavShell {
    /// Content-only shell (no sidebar): overview + detail, e.g. weather.
    pub fn new(overview: Element, detail: Element) -> Self {
        Self {
            sidebar: None,
            overview,
            detail: Some(detail),
            authored_breakpoint: AuthoredExtent::text(DEFAULT_NAVSHELL_BREAKPOINT),
            authored_sidebar_width: None,
            authored_narrow_sidebar_width: None,
            authored_overview_width: None,
            breakpoint: DEFAULT_NAVSHELL_BREAKPOINT,
            sidebar_width: DEFAULT_NAVSHELL_SIDEBAR_WIDTH,
            narrow_sidebar_width: None,
            overview_width: DEFAULT_NAVSHELL_OVERVIEW_WIDTH,
            min_detail_width: MIN_DETAIL_WIDTH,
            chip_inset: CHIP_INSET,
            chip_size: CHIP_SIZE,
            back_chip_width: BACK_CHIP_WIDTH,
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
            inner_extents: None,
            layout_theme: Theme::dark(),
            host_solved: false,
            sidebar_node: None,
            overview_node: None,
            detail_node: None,
            animating: false,
            dirty: false,
            host_structure_dirty: false,
            host_interactive_panels: [false; 3],
            last_tick: None,
        }
    }

    /// Sidebar-plus-content shell (file-manager / settings shape).
    ///
    /// Use this instead of [`NavShell::new`] when the left pane is a list of
    /// *destinations*: passing it as `new`'s `overview` makes it §1b
    /// content, which narrow then promotes to a full-screen page, pushing
    /// the content off the window.
    pub fn sidebar_over_content(sidebar: Element, content: Element) -> Self {
        let mut shell = Self::new(content, Box::new(crate::widget::Spacer::new()));
        shell.sidebar = Some(sidebar);
        shell.detail = None;
        shell
    }

    /// Attach a genuine sidebar: `[ sidebar | overview | detail ]`.
    #[must_use]
    pub fn with_sidebar(mut self, sidebar: Element) -> Self {
        self.inner_taffy = None;
        self.sidebar = Some(sidebar);
        self
    }

    /// The width, in logical px **authored at 1×**, at or above which the
    /// shell is wide. Resolved against the theme when the tree is built
    /// (`× ui_scale × text_scale.max(1.0)`, [`Theme::responsive_scale`]), so
    /// at 200 % text the panes stop fitting side by side at twice the
    /// window width without the app multiplying anything. Read the resolved
    /// value back with [`Self::breakpoint`].
    #[must_use]
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.authored_breakpoint = AuthoredExtent::text(px);
        self.breakpoint = px;
        self
    }

    /// Update the responsive threshold when an accessibility text scale makes
    /// the same logical chrome require more horizontal room.  This is local
    /// presentation policy, never collaborative navigation state.
    pub fn set_breakpoint(&mut self, px: f64) {
        self.breakpoint = px.max(1.0);
        self.dirty = true;
    }

    /// Docked width of a **text-sized** sidebar (a destination list),
    /// authored at 1× and scaled like [`Self::with_breakpoint`]. Default:
    /// `theme.tokens.metrics.sidebar_width`.
    #[must_use]
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.authored_sidebar_width = Some(AuthoredExtent::text(px));
        self.sidebar_width = px;
        self
    }

    /// Docked width of an **icon rail** sidebar — glyphs, not text — which
    /// follows `ui_scale` only.
    #[must_use]
    pub fn with_icon_sidebar_width(mut self, px: f64) -> Self {
        self.authored_sidebar_width = Some(AuthoredExtent::icon(px));
        self.sidebar_width = px;
        self
    }

    /// Override the sidebar width only while it is an overlay drawer below
    /// the shell breakpoint. The normal docked width remains unchanged.
    /// Authored at 1× for a text-sized drawer; scaled like
    /// [`Self::with_breakpoint`].
    #[must_use]
    pub fn with_narrow_sidebar_width(mut self, px: f64) -> Self {
        self.authored_narrow_sidebar_width = Some(AuthoredExtent::text(px));
        self.narrow_sidebar_width = Some(px);
        self
    }

    /// Width of a **text-sized** overview column, authored at 1× and scaled
    /// like [`Self::with_breakpoint`]. Default:
    /// `theme.tokens.metrics.overview_width`.
    #[must_use]
    pub fn with_overview_width(mut self, px: f64) -> Self {
        self.authored_overview_width = Some(AuthoredExtent::text(px));
        self.overview_width = px;
        self
    }

    /// Width of an overview collapsed to an **icon rail**; follows
    /// `ui_scale` only.
    #[must_use]
    pub fn with_icon_overview_width(mut self, px: f64) -> Self {
        self.authored_overview_width = Some(AuthoredExtent::icon(px));
        self.overview_width = px;
        self
    }

    /// The breakpoint the shell currently resolves against — the authored
    /// value scaled by the theme it was last built with. Compare a window
    /// width against this, never against the constant handed to
    /// [`Self::with_breakpoint`].
    pub fn breakpoint(&self) -> f64 {
        self.breakpoint
    }

    /// The docked sidebar width currently laid out (resolved like
    /// [`Self::breakpoint`]).
    pub fn sidebar_width(&self) -> f64 {
        self.sidebar_width
    }

    /// The overview column width currently laid out (resolved like
    /// [`Self::breakpoint`]).
    pub fn overview_width(&self) -> f64 {
        self.overview_width
    }

    /// Resolve the authored inputs and the chip geometry against `theme`
    /// (HIG §9.6, [`AuthoredExtent::resolve`]). A token default is already
    /// `ui_scale`d and grows with `text_growth`. Identity on a stock theme.
    fn resolve_inputs(&mut self, theme: &Theme) {
        let growth = f64::from(theme.text_growth());
        let metrics = theme.tokens.metrics;
        self.breakpoint = self.authored_breakpoint.resolve(theme);
        self.sidebar_width = self.authored_sidebar_width.map_or_else(
            || f64::from(metrics.sidebar_width) * growth,
            |extent| extent.resolve(theme),
        );
        self.narrow_sidebar_width = self
            .authored_narrow_sidebar_width
            .map(|extent| extent.resolve(theme));
        self.overview_width = self.authored_overview_width.map_or_else(
            || f64::from(metrics.overview_width) * growth,
            |extent| extent.resolve(theme),
        );
        self.min_detail_width = AuthoredExtent::text(MIN_DETAIL_WIDTH).resolve(theme);
        self.chip_inset = f64::from(theme.tokens.spacing.s3);
        self.chip_size = crate::widget::text_control_height(CHIP_SIZE, theme);
        // Glyph square plus the label run, which grows with the text.
        self.back_chip_width =
            self.chip_size + AuthoredExtent::text(BACK_CHIP_WIDTH - CHIP_SIZE).resolve(theme);
    }

    /// Height of the band the top-left chip occupies: inset, chip, inset.
    fn chip_band(&self) -> f64 {
        self.chip_inset * 2.0 + self.chip_size
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
        self.inner_taffy = None;
        self.overview = overview;
        self.host_structure_dirty = true;
    }

    pub fn set_detail(&mut self, detail: Element) {
        self.inner_taffy = None;
        self.detail = Some(detail);
        self.host_structure_dirty = true;
    }

    /// Replace the single content pane of a [`NavShell::sidebar_over_content`]
    /// shell. Writes the same slot as [`Self::set_overview`]; named so a
    /// single-pane app doesn't have to call its content an "overview".
    pub fn set_content(&mut self, content: Element) {
        self.set_overview(content);
    }

    pub fn has_detail(&self) -> bool {
        self.detail.is_some()
    }

    pub fn set_sidebar(&mut self, sidebar: Option<Element>) {
        self.inner_taffy = None;
        self.sidebar = sidebar;
        self.host_structure_dirty = true;
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

    fn interactive_panels(&self) -> [bool; 3] {
        let modal = self.overlay_sidebar_is_modal();
        let pushed_detail = self.content_layout() == ContentLayout::Push && self.is_detail_shown();
        [
            !modal && !pushed_detail,
            !modal
                && self.detail.is_some()
                && (self.content_layout() == ContentLayout::SideBySide || pushed_detail),
            self.is_sidebar_open(),
        ]
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
        let configured_sidebar_width = if self.mode() == ShellMode::Narrow {
            self.narrow_sidebar_width.unwrap_or(self.sidebar_width)
        } else {
            self.sidebar_width
        };
        let sw = configured_sidebar_width.min(w);

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
                    .min((content_w - self.min_detail_width).max(0.0))
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
        let (inset, chip) = (self.chip_inset, self.chip_size);
        self.affordance_rect = match self.affordance {
            Affordance::None => Rect::ZERO,
            Affordance::SidebarToggle => Rect::new(
                r.x0 + inset,
                r.y0 + inset,
                r.x0 + inset + chip,
                r.y0 + inset + chip,
            ),
            Affordance::Back => Rect::new(
                r.x0 + inset,
                r.y0 + inset,
                r.x0 + inset + self.back_chip_width,
                r.y0 + inset + chip,
            ),
        };
    }

    fn sidebar_visible(&self) -> bool {
        self.has_sidebar() && self.sidebar_flap.reveal() > 0.001
    }

    /// An overlay drawer is modal: the painted detail pane is retained only as
    /// backdrop continuity, never as an input or accessibility target.
    fn overlay_sidebar_is_modal(&self) -> bool {
        self.is_sidebar_open() && self.sidebar_split_layout() == SplitLayout::Overlay
    }

    /// [`Self::sidebar_rect`] less the affordance chip's band while the
    /// sidebar is an **overlay drawer**: it slides in under the chip, so its
    /// first row must not start beneath it. The panel fill still uses the
    /// full rect, so the band reads as sidebar-coloured, not a notch.
    ///
    /// **Docked**, the chip sits in the sidebar's own header row, which
    /// clears it horizontally (the `nav_shell_shots` fixture indents its
    /// title; mail's sidebar reserves `CHIP_CLEARANCE`), and the sidebar
    /// keeps its full height — so its header lines up with the overview
    /// and detail headers beside it instead of starting one chip band
    /// (60 px at 1×) lower (F1 round one, V-01).
    pub fn sidebar_content_rect(&self) -> Rect {
        let r = self.sidebar_rect;
        if self.affordance != Affordance::SidebarToggle
            || r.height() <= 0.0
            || self.sidebar_split_layout() == SplitLayout::Docked
        {
            return r;
        }
        let band = self.chip_band();
        Rect::new(r.x0, (r.y0 + band).min(r.y1), r.x1, r.y1)
    }

    /// `r` with the affordance chip's band reserved when the chip floats
    /// over that pane. Resolved here, in the same pass that positions the
    /// chip, rather than by the host: a host re-deriving it from last
    /// frame's rect would see the content shift by a frame once the shell is
    /// measured, and a moving target is one the pointer can't hit.
    fn content_child_rect(&self, r: Rect) -> Rect {
        if self.affordance == Affordance::None || r.width() <= 0.0 || r.height() <= 0.0 {
            return r;
        }
        let chip = self.affordance_rect;
        if chip.x1 <= r.x0 || chip.x0 >= r.x1 {
            return r;
        }
        let band = self.chip_band();
        Rect::new(r.x0, (r.y0 + band).min(r.y1), r.x1, r.y1)
    }

    pub fn overview_content_rect(&self) -> Rect {
        self.content_child_rect(self.overview_rect)
    }

    pub fn detail_content_rect(&self) -> Rect {
        self.content_child_rect(self.detail_rect)
    }

    /// The sidebar region's ground, docked or as the drawer sliding over
    /// live content, and the theme its child draws with: ink and selection
    /// measured against that ground (`Theme::in_region`).
    fn fill_panel(scene: &mut Scene, transform: Affine, theme: &Theme, r: Rect) -> Theme {
        crate::surface::region(scene, transform, r, crate::surface::Region::Sidebar, theme);
        theme.in_region(crate::surface::Region::Sidebar)
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

        self.apply_child_layout(&taffy, o_node, d_node, s_node);
        self.inner_taffy = Some(taffy);
        self.inner_extents = Some(self.child_extents());
        self.overview_node = Some(o_node);
        self.detail_node = d_node;
        self.sidebar_node = s_node;
    }

    fn child_extents(&self) -> [(f64, f64); 3] {
        [
            self.content_child_rect(self.overview_rect),
            self.content_child_rect(self.detail_rect),
            self.sidebar_content_rect(),
        ]
        .map(|rect| (rect.width(), rect.height()))
    }

    /// Whether the panes need a full solve rather than a re-apply of the
    /// cached tree: no tree yet, a pane's extent moved, or a pane that is
    /// actually laid out asks to be re-measured. A pane with no extent
    /// cannot be solved, so a child parked in a collapsed pane never forces
    /// a solve — and a font-system load — every frame.
    fn panes_need_resolve(&self) -> bool {
        if self.inner_taffy.is_none() || self.overview_node.is_none() {
            return true;
        }
        let extents = self.child_extents();
        if self.inner_extents != Some(extents) {
            return true;
        }
        let laid_out = |(w, h): (f64, f64)| w > 0.0 && h > 0.0;
        (laid_out(extents[0]) && self.overview.layout_needs_measure())
            || (laid_out(extents[1])
                && self
                    .detail
                    .as_ref()
                    .is_some_and(|pane| pane.layout_needs_measure()))
            || (laid_out(extents[2])
                && self
                    .sidebar
                    .as_ref()
                    .is_some_and(|pane| pane.layout_needs_measure()))
    }

    /// Solve the panes with the theme the outer tree was built with and a
    /// renderer at its text scale — the path for hosts that solve with plain
    /// taffy and call `apply_layout` directly. The shared host solver
    /// reaches `adjust_layout_with_measure` first and never lands here.
    fn resolve_panes_fallback(&mut self) {
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
            self.place_children(&theme, tr)
        });
    }

    fn apply_child_layout(
        &mut self,
        taffy: &TaffyTree<()>,
        o_node: NodeId,
        d_node: Option<NodeId>,
        s_node: Option<NodeId>,
    ) {
        let sidebar_child_rect = self.sidebar_content_rect();
        let overview_child_rect = self.content_child_rect(self.overview_rect);
        let detail_child_rect = self.content_child_rect(self.detail_rect);
        self.overview.apply_layout(
            taffy,
            o_node,
            Point::new(overview_child_rect.x0, overview_child_rect.y0),
        );
        if let (Some(d), Some(dn)) = (self.detail.as_mut(), d_node) {
            d.apply_layout(
                taffy,
                dn,
                Point::new(detail_child_rect.x0, detail_child_rect.y0),
            );
        }
        if let (Some(sb), Some(sn)) = (self.sidebar.as_mut(), s_node) {
            sb.apply_layout(
                taffy,
                sn,
                Point::new(sidebar_child_rect.x0, sidebar_child_rect.y0),
            );
        }
    }

    /// Driven from the draw path, never `update` (which `tilo_ui::App` never
    /// calls), so the slides play and settle to no frames at rest
    /// (HIG §5).
    fn self_tick(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        if theme.reduce_motion {
            let sidebar_before = self.sidebar_flap.reveal();
            let push_before = self.push.reveal();
            self.sidebar_flap.settle(self.sidebar_split_layout());
            self.push.settle(SplitLayout::Docked);
            self.animating = false;
            self.last_tick = Some(crate::time::now());
            if self.sidebar_flap.reveal().to_bits() != sidebar_before.to_bits()
                || self.push.reveal().to_bits() != push_before.to_bits()
            {
                self.recompute_rects();
                self.place_children(theme, text_renderer);
            }
            return;
        }
        let now = crate::time::now();
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

        // Neutral control face (HIG §9.5): its fill with a crisp derived edge.
        crate::surface::card(
            scene,
            transform,
            rect,
            theme.radius_standard,
            theme.tokens.colors.surfaces.raised,
        );

        // The glyph is icon-sized: the 1× chip square grown with `ui_scale`
        // (never the text), centred on the chip's leading edge however tall
        // the text has made the chip; the glyph itself fills half that side.
        let glyph_side = CHIP_SIZE * f64::from(theme.ui_scale);
        let glyph_box = Rect::from_center_size(
            (rect.x0 + glyph_side / 2.0, rect.center().y),
            (glyph_side, glyph_side),
        );
        let glyph_inset = glyph_side / 4.0;
        match self.affordance {
            Affordance::None => {}
            Affordance::Back => {
                draw_icon(
                    scene,
                    transform,
                    glyph_box.inflate(-glyph_inset, -glyph_inset),
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
                        rect.y0 + self.chip_size / 2.0 - 9.0,
                    )),
                    None,
                );
            }
            Affordance::SidebarToggle => {
                if self.is_sidebar_open() {
                    draw_icon(
                        scene,
                        transform,
                        glyph_box.inflate(-glyph_inset, -glyph_inset),
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

    /// Three bars filling half the glyph box's width (9 px half-width and a
    /// 5.5 px gap in the 1× 36 px box), so they follow `ui_scale` with it.
    fn draw_hamburger(scene: &mut Scene, transform: Affine, box_rect: Rect, color: Color) {
        let cx = box_rect.center().x;
        let cy = box_rect.center().y;
        let half_w = box_rect.width() / 4.0;
        let gap = box_rect.width() * (5.5 / CHIP_SIZE);
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
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
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
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        self.resolve_inputs(theme);
        self.inner_taffy = None;
        // Register the real panels in the host tree so accessibility actions
        // and keyboard focus have dispatch destinations. As with ModalDialog,
        // place_children still resolves their final overlay/push geometry.
        self.host_structure_dirty = false;
        self.host_interactive_panels = self.interactive_panels();
        let mut children = Vec::new();
        if self.host_interactive_panels[0] {
            children.push(self.overview.build_layout(taffy, theme, node_map));
        }
        if self.host_interactive_panels[1]
            && let Some(detail) = self.detail.as_mut()
        {
            children.push(detail.build_layout(taffy, theme, node_map));
        }
        if self.host_interactive_panels[2]
            && let Some(sidebar) = self.sidebar.as_mut()
        {
            children.push(sidebar.build_layout(taffy, theme, node_map));
        }
        for child in &children {
            if let Ok(style) = taffy.style(*child) {
                let mut style = style.clone();
                style.position = taffy::Position::Absolute;
                let _ = taffy.set_style(*child, style);
            }
        }
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &children,
        );
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
        // Scroll-only frames reuse solved sizes throughout the framework.
        // Apply them again so descendant scroll offsets and translated origins
        // reach hit testing, without rebuilding trees or loading system fonts.
        // A resize, rebuilt outer tree, swapped pane, or measure-dirty child
        // takes the full measurement path below — unless the shared host
        // solver already ran it with its own renderer in
        // `adjust_layout_with_measure`, in which case this is a re-apply too.
        // Modal traversal intentionally exposes only the sidebar for input
        // and accessibility, but all retained backdrop panes still sync/draw.
        let host_solved = std::mem::take(&mut self.host_solved);
        let cached_extents_match =
            self.inner_taffy.is_some() && self.inner_extents == Some(self.child_extents());
        if ((host_solved && cached_extents_match) || !self.panes_need_resolve())
            && let Some(o_node) = self.overview_node
            && let Some(taffy) = self.inner_taffy.take()
        {
            self.apply_child_layout(&taffy, o_node, self.detail_node, self.sidebar_node);
            self.inner_taffy = Some(taffy);
            return;
        }
        self.resolve_panes_fallback();
    }

    /// Solve the panes with the **host's** renderer while the host solver
    /// still has it in hand: the allocation is known here (only the origin
    /// is not, and the inner solve depends on extents alone), so the panes
    /// are measured with the same fonts and text scale as everything else
    /// on screen and `apply_layout` only re-applies the cached tree. The
    /// host tree is untouched and no style changes, so this returns `false`.
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
        self.sidebar_flap.reconcile(self.sidebar_split_layout());
        self.recompute_rects();
        // The settle loop calls this up to eight times a pass; once the
        // panes are solved at these extents the later iterations are no-ops
        // even though the just-measured children still read as dirty.
        let already_solved_this_pass = self.host_solved
            && self.inner_taffy.is_some()
            && self.inner_extents == Some(self.child_extents());
        if !already_solved_this_pass && self.panes_need_resolve() {
            let theme = self.layout_theme.clone();
            self.place_children(&theme, text);
        }
        self.host_solved = true;
        false
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        // Slides advance from the draw path (`self_tick`). The admitted
        // primary-frame update forwards model/momentum updates to every pane.
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
            let sidebar_theme = Self::fill_panel(scene, transform, theme, self.sidebar_rect);
            s.draw(scene, transform, text_renderer, &sidebar_theme, presence);
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
            let sidebar_theme = Self::fill_panel(scene, transform, theme, self.sidebar_rect);
            if let Some(s) = self.sidebar.as_mut() {
                s.draw_with_presence(
                    scene,
                    transform,
                    text_renderer,
                    &sidebar_theme,
                    presence,
                    permissions,
                );
            }
        }
        self.draw_chrome_no_scrim(scene, transform, text_renderer, theme);
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
                // Dismiss the overlay drawer, else pop the pushed detail.
                if self.is_sidebar_open() && self.sidebar_split_layout() == SplitLayout::Overlay {
                    self.close_sidebar();
                    *ctx.handled = true;
                    return;
                }
                if self.content_layout() == ContentLayout::Push && self.pane == NavPane::Detail {
                    self.show_overview();
                    *ctx.handled = true;
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
                    *ctx.handled = true;
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
                    *ctx.handled = true;
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
        if *ctx.handled {
            return;
        }
        self.route_event_to_children(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.overlay_sidebar_is_modal() {
            return self
                .sidebar
                .as_deref()
                .into_iter()
                .collect::<Vec<&dyn Widget>>();
        }
        let mut v: Vec<&dyn Widget> = vec![self.overview.as_ref()];
        if let Some(d) = self.detail.as_ref() {
            v.push(d.as_ref());
        }
        if let Some(s) = self.sidebar.as_ref() {
            v.push(s.as_ref());
        }
        v
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.overlay_sidebar_is_modal() {
            if let Some(sidebar) = &self.sidebar {
                visitor(sidebar.as_ref());
            }
            return;
        }
        visitor(self.overview.as_ref());
        if let Some(detail) = &self.detail {
            visitor(detail.as_ref());
        }
        if let Some(sidebar) = &self.sidebar {
            visitor(sidebar.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.overlay_sidebar_is_modal() {
            let mut modal_children: Vec<&mut (dyn Widget + '_)> = Vec::new();
            if let Some(sidebar) = self.sidebar.as_mut() {
                modal_children.push(sidebar.as_mut());
            }
            return modal_children;
        }
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

    fn next_wake_deadline(&self) -> Option<Instant> {
        // Modal traversal hides backdrop panes from interaction/accessibility,
        // but those retained panes still draw and must wake for timed changes.
        self.overview
            .next_wake_deadline()
            .into_iter()
            .chain(
                self.detail
                    .as_ref()
                    .and_then(|pane| pane.next_wake_deadline()),
            )
            .chain(
                self.sidebar
                    .as_ref()
                    .and_then(|pane| pane.next_wake_deadline()),
            )
            .min()
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
        let mut d = std::mem::take(&mut self.host_structure_dirty)
            || self.host_interactive_panels != self.interactive_panels();
        d |= self.overview.take_structure_dirty();
        if let Some(det) = self.detail.as_mut() {
            d |= det.take_structure_dirty();
        }
        if let Some(s) = self.sidebar.as_mut() {
            d |= s.take_structure_dirty();
        }
        if d {
            self.inner_taffy = None;
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

impl crate::widget::controls::ActionControls for NavShell {
    fn has_action_control_bindings(&self) -> bool {
        let mut found = false;
        self.for_each_child(&mut |child| {
            if !found {
                found = crate::widget::controls::has_action_control_bindings(child);
            }
        });
        found
    }
}

impl NavShell {
    /// Routes `event` to child panes in front-to-back order: an open overlay
    /// drawer is topmost so it routes first; otherwise the front content
    /// pane (for narrow push) routes before the back one. `d_node` is absent
    /// for a sidebar-plus-content shell but must not stop the sidebar/
    /// overview panes from being routed to. Split out of `handle_event` to
    /// keep that under the line cap.
    fn route_event_to_children(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
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
        let sidebar_first = self.overlay_sidebar_is_modal();

        let mut order: Vec<(Which, NodeId)> = Vec::new();
        if sidebar_first && let Some(sn) = self.sidebar_node {
            order.push((Which::Sidebar, sn));
        }
        if !sidebar_first {
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
            if let Some(sn) = self.sidebar_node {
                order.push((Which::Sidebar, sn));
            }
        }

        for (which, child_node) in order {
            if *ctx.handled {
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
                &mut crate::widget::EventCtx {
                    taffy,
                    ..ctx.reborrow(ctx.path, child_node)
                },
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
            // Pane dividers derive from the surface they are painted on (HIG
            // §9.5), centred on the last pixel column of the pane they close:
            // the sidebar's on the sidebar region, the overview's on the
            // window ground the framework paints under every content pane.
            let stroke = Stroke::new(1.0);
            if self.sidebar_visible() {
                let x = self.sidebar_rect.x1 - 0.5;
                scene.stroke(
                    &stroke,
                    transform,
                    crate::surface::edge_on(theme.region(crate::surface::Region::Sidebar)),
                    None,
                    &Line::new(Point::new(x, self.rect.y0), Point::new(x, self.rect.y1)),
                );
            }
            let x = self.overview_rect.x1 - 0.5;
            scene.stroke(
                &stroke,
                transform,
                crate::surface::edge_on(theme.tokens.colors.surfaces.content),
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
    use automerge::AutoCommit;
    use vello_encoding::DrawTag;

    /// A leaf pane that proves which retained pane received an input event.
    /// The shell's overlay drawer is modal, so it is important this is a
    /// routing assertion rather than only a geometry assertion.
    struct EventProbe {
        id: &'static str,
        deadline: Option<Instant>,
        events: std::rc::Rc<std::cell::Cell<usize>>,
        rect: Rect,
    }

    impl EventProbe {
        fn new(id: &'static str, events: std::rc::Rc<std::cell::Cell<usize>>) -> Self {
            Self {
                id,
                deadline: None,
                events,
                rect: Rect::ZERO,
            }
        }
    }

    impl Widget for EventProbe {
        fn next_wake_deadline(&self) -> Option<Instant> {
            self.deadline
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
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
            let layout = taffy.layout(node).copied().unwrap_or_default();
            self.rect = Rect::new(
                origin.x,
                origin.y,
                origin.x + f64::from(layout.size.width),
                origin.y + f64::from(layout.size.height),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
            self.events.set(self.events.get().saturating_add(1));
        }

        fn name(&self) -> &'static str {
            "EventProbe"
        }

        fn test_id(&self) -> Option<&str> {
            Some(self.id)
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

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

    #[test]
    fn host_registration_and_presence_track_modal_drawers() {
        // Case `host_registration_tracks_modal_drawers_and_replaced_panels`.
        {
            let mut ns = shell(420.0, true);
            ns.close_sidebar();
            ns.show_overview();
            ns.snap();
            let theme = Theme::dark();
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            let node = ns.build_layout(&mut taffy, &theme, &mut map);
            let children = taffy.children(node).unwrap();
            assert_eq!(
                children.len(),
                1,
                "hidden panels must not enter host focus order"
            );
            assert!(std::ptr::addr_eq(
                map[&children[0]],
                ns.overview.as_ref() as *const dyn Widget
            ));
            assert!(!ns.take_structure_dirty());

            ns.show_detail();
            assert!(
                ns.take_structure_dirty(),
                "push navigation changes host focus targets"
            );
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            let node = ns.build_layout(&mut taffy, &theme, &mut map);
            let children = taffy.children(node).unwrap();
            assert_eq!(children.len(), 1);
            assert!(std::ptr::addr_eq(
                map[&children[0]],
                ns.detail.as_deref().unwrap() as *const dyn Widget
            ));
            assert!(!ns.take_structure_dirty());

            ns.open_sidebar();
            ns.snap();
            assert!(ns.take_structure_dirty());
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            let node = ns.build_layout(&mut taffy, &theme, &mut map);
            let children = taffy.children(node).unwrap();
            assert_eq!(children.len(), 1);
            let sidebar = ns.sidebar.as_deref().unwrap();
            assert!(std::ptr::addr_eq(
                map[&children[0]],
                sidebar as *const dyn Widget
            ));
            assert!(!ns.take_structure_dirty());

            ns.set_overview(Box::new(Text::new("replacement")));
            assert!(ns.take_structure_dirty());
            ns.close_sidebar();
            assert!(ns.take_structure_dirty());
        }

        // Case `opaque_hold_deadline_reaches_host_through_modal_nav_shell`.
        {
            let deadline = crate::time::now() + crate::time::Duration::from_millis(900);
            let mut content = EventProbe::new("content", Default::default());
            content.deadline = Some(deadline);
            let mut ns =
                NavShell::sidebar_over_content(Box::new(Text::new("places")), Box::new(content));
            lay_out(&mut ns, 420.0, 500.0);
            ns.snap_sidebar(true);
            assert!(ns.overlay_sidebar_is_modal());
            assert_eq!(
                ns.next_wake_deadline(),
                Some(deadline),
                "a live backdrop must still wake for its fade"
            );
        }

        // Case `control_presence_modal_exposure_matches_full_inventory`.
        {
            let resource = tilo_actions::Resource::new("test-pane", "test", "Test").with_action(
                tilo_actions::ActionSpec::operation(
                    "go",
                    "Go",
                    tilo_actions::OperationKind::View,
                    "test.go",
                    &(),
                )
                .unwrap(),
            );
            let button = crate::widget::Button::new("go", "Go", None)
                .on_action(tilo_actions::ActionBinding::unit_for(&resource, "go").unwrap());
            let mut ns =
                NavShell::sidebar_over_content(Box::new(Text::new("places")), Box::new(button));
            lay_out(&mut ns, 420.0, 500.0);
            for open in [false, true, false] {
                ns.snap_sidebar(open);
                let inventory = crate::widget::controls::control_action_bindings(&ns)
                    .iter()
                    .any(|b| matches!(b.kind, crate::widget::ControlBindingKind::Action(_)));
                assert_eq!(
                    crate::widget::controls::has_action_control_bindings(&ns),
                    inventory
                );
                assert_eq!(inventory, !open, "modal sidebar hides overview emitter");
            }
        }
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

    #[test]
    fn measured_panes_are_reused_until_scale_theme_size_or_children_change() {
        // Case `translated_scroll_layout_reuses_measured_child_tree_without_new_fonts`.
        {
            use crate::widget::Scrollable;
            let content = Scrollable::new_vertical().with_child(Box::new(Text::new(
                &"file row with a measured label\n".repeat(100),
            )));
            let mut ns = NavShell::sidebar_over_content(
                Box::new(Text::new("Places\nHome\nDownloads")),
                Box::new(content),
            );
            let theme = Theme::dark();
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            let node = ns.build_layout(&mut taffy, &theme, &mut map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1000.0),
                        height: taffy::AvailableSpace::Definite(500.0),
                    },
                )
                .unwrap();
            ns.apply_layout(&taffy, node, Point::ZERO);
            ns.mark_drawn();
            let old_child = ns.overview.children()[0].get_rect();
            let old_node = ns.overview_node;
            crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
            for _ in 1..=4 {
                ns.overview
                    .as_any_mut()
                    .unwrap()
                    .downcast_mut::<Scrollable>()
                    .unwrap()
                    .scroll_by(0.0, 20.0);
                assert!(ns.is_layout_dirty(), "scroll must reposition children");
                assert!(
                    !ns.layout_needs_measure(),
                    "scroll leaves intrinsic sizes intact"
                );
                ns.apply_layout(&taffy, node, Point::new(12.0, 7.0));
                ns.mark_drawn();
            }
            let child = ns.overview.children()[0].get_rect();
            assert_eq!(child.x0, old_child.x0 + 12.0);
            assert_eq!(child.y0, old_child.y0 + 7.0 - 80.0);
            assert_eq!(ns.overview_node, old_node);
            assert_eq!(
                crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(std::cell::Cell::get),
                0,
                "translation-only layout must not reload system fonts"
            );
        }

        // Case `modal_drawer_remeasures_changed_backdrop_panes`.
        {
            use automerge::transaction::Transactable;
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "label", "first line").unwrap();
            let mut ns = NavShell::sidebar_over_content(
                Box::new(Text::new("places")),
                Box::new(Text::from_document_string("label")),
            );
            ns.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut ns, 420.0, 500.0);
            ns.snap_sidebar(true);
            let theme = Theme::dark();
            let mut taffy = TaffyTree::new();
            let node = ns.build_layout(&mut taffy, &theme, &mut HashMap::new());
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(420.0),
                        height: taffy::AvailableSpace::Definite(500.0),
                    },
                )
                .unwrap();
            ns.apply_layout(&taffy, node, Point::ZERO);
            ns.overview.mark_drawn();
            ns.sidebar.as_mut().unwrap().mark_drawn();
            let before = ns.overview.get_rect();
            doc.put(
                automerge::ROOT,
                "label",
                "first line\nsecond line\nthird line",
            )
            .unwrap();
            ns.sync(&mut doc, &automerge::ROOT);
            assert!(ns.overlay_sidebar_is_modal());
            assert!(ns.overview.layout_needs_measure());
            assert!(
                !ns.layout_needs_measure(),
                "modal traversal exposes only the clean sidebar"
            );
            ns.apply_layout(&taffy, node, Point::ZERO);
            let reused = ns.overview.get_rect();
            assert!(
                reused.height() > before.height(),
                "live backdrop must receive changed text geometry"
            );
            // Force the authoritative full path and compare its complete rectangle.
            ns.inner_taffy = None;
            ns.apply_layout(&taffy, node, Point::ZERO);
            assert_eq!(reused, ns.overview.get_rect());
        }

        // Case `measured_shell_rebuilds_after_theme_resize_and_child_replacement`.
        {
            let mut ns = NavShell::new(
                Box::new(Text::new("overview")),
                Box::new(Text::new("detail")),
            );
            lay_out(&mut ns, 1000.0, 500.0);
            ns.mark_drawn();
            let original = ns.overview.get_rect();
            let theme = Theme::dark().with_text_scale(1.5);
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            let node = ns.build_layout(&mut taffy, &theme, &mut map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1200.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
            ns.apply_layout(&taffy, node, Point::ZERO);
            assert!(ns.overview.get_rect().height() > original.height());
            let mut fresh = NavShell::new(
                Box::new(Text::new("overview")),
                Box::new(Text::new("detail")),
            );
            let mut fresh_taffy = TaffyTree::new();
            let fresh_node = fresh.build_layout(&mut fresh_taffy, &theme, &mut HashMap::new());
            fresh_taffy
                .compute_layout(
                    fresh_node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1200.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
            fresh.apply_layout(&fresh_taffy, fresh_node, Point::ZERO);
            assert_eq!(ns.overview.get_rect(), fresh.overview.get_rect());
            ns.mark_drawn();
            // Swapping a previously laid-out, clean pane must invalidate the
            // retained tree even though the incoming widget needs no measurement.
            let mut replacement = NavShell::new(
                Box::new(Text::new("replacement\nsecond line")),
                Box::new(Text::new("detail")),
            );
            lay_out(&mut replacement, 1200.0, 600.0);
            replacement.mark_drawn();
            assert!(!replacement.overview.layout_needs_measure());
            ns.set_overview(replacement.overview);
            assert!(!ns.layout_needs_measure());
            ns.apply_layout(&taffy, node, Point::ZERO);
            assert!(ns.overview.get_rect().height() > fresh.overview.get_rect().height());
        }

        // Case `panes_measure_at_the_host_text_scale_without_loading_fonts`.
        // The panes are solved with the theme the outer tree was built with and
        // the host's renderer, so a `Text` in a pane is twice as tall at 2× and
        // the shared-host path loads no font system of its own.
        {
            let height_at = |scale: f32| {
                let theme = Theme::dark().with_text_scale(scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(scale);
                let mut ns = NavShell::new(
                    Box::new(Text::new("overview")),
                    Box::new(Text::new("detail")),
                );
                let mut taffy = TaffyTree::<()>::new();
                let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let root = ns.build_layout(&mut taffy, &theme, &mut nodes);
                let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
                crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
                // SAFETY: the tree and maps come from this exact live build and
                // `ns` is exclusively owned for the whole solve.
                unsafe {
                    bindings.rebuild(&ns, &taffy, root, &nodes);
                    crate::widget::compute_widget_layout(
                        &mut taffy,
                        root,
                        Size {
                            width: taffy::AvailableSpace::Definite(1200.0),
                            height: taffy::AvailableSpace::Definite(600.0),
                        },
                        &nodes,
                        &bindings,
                        &mut renderer,
                    )
                    .unwrap();
                }
                ns.apply_layout(&taffy, root, Point::ZERO);
                assert_eq!(
                    crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(std::cell::Cell::get),
                    0,
                    "the host path must measure with the host's renderer, not load fonts of its own"
                );
                ns.overview.get_rect().height()
            };
            let one = height_at(1.0);
            let two = height_at(2.0);
            assert!(one > 0.0);
            assert!(
                (two / one - 2.0).abs() < 0.05,
                "an overview Text line is {two} px at 2× against {one} px at 1×; it must double"
            );
        }

        // Case `mutable_children_match_immutable_children_in_logical_order`.
        {
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

    // --- the chip's band over the content --------------------------------

    #[test]
    fn content_rects_clear_the_floating_chip_and_follow_the_responsive_scale() {
        // Case `narrow_content_starts_below_the_chip_that_floats_over_it`.
        {
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

        // Case `the_public_content_rects_are_what_the_layout_uses`.
        {
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

        // Case `wide_content_keeps_its_full_height_because_the_sidebar_holds_the_corner`.
        {
            let ns = sidebar_content_shell(1200.0);
            assert_eq!(ns.mode(), ShellMode::Wide);
            let pane = ns.overview_rect();
            assert_eq!(
                ns.content_child_rect(pane),
                pane,
                "a docked sidebar owns the top-left corner; the content is untouched",
            );
        }

        // Case `docked_sidebar_keeps_its_full_height_so_its_header_aligns_with_the_content_panes`.
        // F1 round one V-01: docked, the sidebar's header started one chip band
        // (60 px) below the overview and detail headers because the band was
        // reserved above the sidebar even though the chip sits in its header
        // row (which the fixture and mail clear horizontally). Docked keeps the
        // sidebar's full height; the overlay drawer still starts under the chip.
        {
            let wide = shell(1200.0, true);
            assert_eq!(wide.top_left_affordance(), Affordance::SidebarToggle);
            assert_eq!(
                wide.sidebar_content_rect(),
                wide.sidebar_rect(),
                "the docked sidebar's content starts at the pane top, level with the overview",
            );
            assert_eq!(
                wide.sidebar_content_rect().y0,
                wide.overview_content_rect().y0,
                "the three panes' headers share one top edge",
            );
            assert!(
                wide.collapse_button_rect().y1 <= wide.sidebar_rect().y0 + wide.chip_band(),
                "the chip still lives in the sidebar's header row",
            );

            let mut narrow = shell(600.0, true);
            narrow.open_sidebar();
            narrow.snap();
            assert_eq!(narrow.sidebar_layout(), Some(SplitLayout::Overlay));
            let drawer = narrow.sidebar_rect();
            assert_eq!(
                narrow.sidebar_content_rect().y0,
                drawer.y0 + narrow.chip_band(),
                "an overlay drawer slides in under the chip, so its first row starts below the band",
            );
        }

        // Case `authored_inputs_and_chip_follow_the_responsive_scale`.
        // HIG §9.6: breakpoint and text-sized pane widths are authored at 1×
        // and resolved by the shell (`× ui_scale × text_scale.max(1.0)`); the
        // token defaults follow the `ui_scale`d metrics; an icon rail follows
        // `ui_scale` only; the chip grows with the body line, not the scale.
        {
            let authored = || {
                NavShell::new(Box::new(Text::new("o")), Box::new(Text::new("d")))
                    .with_sidebar(Box::new(Text::new("s")))
                    .with_breakpoint(860.0)
                    .with_sidebar_width(260.0)
                    .with_overview_width(340.0)
            };
            let mut one = authored();
            lay_out_with(&mut one, &Theme::dark(), 1400.0, 700.0);
            assert_eq!(
                (one.breakpoint(), one.sidebar_width(), one.overview_width()),
                (860.0, 260.0, 340.0),
                "1× is the authored geometry",
            );
            assert_eq!(
                (one.chip_inset, one.chip_size, one.back_chip_width),
                (12.0, 36.0, 104.0)
            );
            assert_eq!(one.mode(), ShellMode::Wide);

            let two = Theme::dark().with_text_scale(2.0);
            let mut large = authored();
            lay_out_with(&mut large, &two, 1400.0, 700.0);
            assert_eq!(
                (
                    large.breakpoint(),
                    large.sidebar_width(),
                    large.overview_width()
                ),
                (1720.0, 520.0, 680.0),
            );
            assert_eq!(
                large.mode(),
                ShellMode::Narrow,
                "a 1400 px window at 2× has the room a 700 px window has at 1×",
            );
            let grown = crate::widget::text_control_height(CHIP_SIZE, &two);
            assert!(
                grown > CHIP_SIZE && grown < 2.0 * CHIP_SIZE,
                "chip grows by the line, not 2×"
            );
            assert_eq!(large.chip_size, grown);
            assert_eq!(large.chip_inset, 12.0, "the inset is spacing, not text");
            assert_eq!(
                large.back_chip_width,
                grown + (BACK_CHIP_WIDTH - CHIP_SIZE) * 2.0
            );
            assert_eq!(
                large.collapse_button_rect().height(),
                grown,
                "the hit rect is the grown chip"
            );

            let mut rail = NavShell::new(Box::new(Text::new("o")), Box::new(Text::new("d")))
                .with_icon_overview_width(68.0);
            lay_out_with(&mut rail, &two, 1400.0, 700.0);
            assert_eq!(
                rail.overview_width(),
                68.0,
                "an icon rail never grows with the text"
            );

            let mut defaults = NavShell::new(Box::new(Text::new("o")), Box::new(Text::new("d")))
                .with_sidebar(Box::new(Text::new("s")));
            lay_out_with(
                &mut defaults,
                &Theme::dark().with_ui_scale(1.5),
                2000.0,
                700.0,
            );
            let metrics = Theme::dark().with_ui_scale(1.5).tokens.metrics;
            assert!((defaults.sidebar_width() - f64::from(metrics.sidebar_width)).abs() < 1e-6);
            assert!((defaults.overview_width() - f64::from(metrics.overview_width)).abs() < 1e-6);
            assert!(
                (defaults.breakpoint() - 1050.0).abs() < 1e-6,
                "700 authored × ui 1.5"
            );
            assert!((defaults.chip_inset - 18.0).abs() < 1e-6, "s3 at ui 1.5");
        }
    }

    // --- sidebar + single content pane (HIG §1a, no §1b split) -----------

    #[test]
    fn sidebar_over_content_docks_wide_and_overlays_narrow() {
        // Case `narrow_sidebar_over_content_keeps_the_content_full_width`.
        {
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

        // Case `narrow_sidebar_starts_closed_and_opens_as_an_overlay_drawer`.
        {
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

        // Case `open_overlay_drawer_exposes_and_routes_only_its_sidebar`.
        {
            let sidebar_events = std::rc::Rc::new(std::cell::Cell::new(0));
            let content_events = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut ns = NavShell::sidebar_over_content(
                Box::new(EventProbe::new(
                    "sidebar",
                    std::rc::Rc::clone(&sidebar_events),
                )),
                Box::new(EventProbe::new(
                    "content",
                    std::rc::Rc::clone(&content_events),
                )),
            );
            lay_out(&mut ns, 600.0, 640.0);
            ns.open_sidebar();
            ns.snap();

            let visible_children: Vec<&str> = Widget::children(&ns)
                .into_iter()
                .filter_map(Widget::test_id)
                .collect();
            assert_eq!(visible_children, vec!["sidebar"]);

            let mut handled = false;
            let drawer_center = ns.sidebar_rect().center();
            dispatch(&mut ns, &cursor_at(drawer_center), &mut handled);
            assert!(sidebar_events.get() > 0, "drawer child receives its input");
            assert_eq!(
                content_events.get(),
                0,
                "the covered content must not receive pointer or assistive traversal input"
            );
        }

        // Case `narrow_drawer_width_can_exceed_the_docked_sidebar_without_shrinking_content`.
        {
            let mut ns = NavShell::sidebar_over_content(
                Box::new(Text::new("places")),
                Box::new(Text::new("files")),
            )
            .with_sidebar_width(200.0)
            .with_narrow_sidebar_width(320.0);
            lay_out(&mut ns, 390.0, 640.0);
            ns.snap();
            assert_eq!(
                ns.overview_rect().width(),
                390.0,
                "closed content stays full width"
            );
            ns.open_sidebar();
            ns.snap();
            assert_eq!(ns.sidebar_rect().width(), 320.0);
            assert_eq!(
                ns.overview_rect().width(),
                390.0,
                "the overlay drawer never displaces the live content pane"
            );
            lay_out(&mut ns, 1200.0, 640.0);
            ns.snap();
            assert_eq!(
                ns.sidebar_rect().width(),
                200.0,
                "desktop docking stays compact"
            );
        }

        // Case `wide_sidebar_over_content_docks_beside_a_full_height_content_pane`.
        {
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

        // Case `sidebar_over_content_reflows_across_the_breakpoint_both_ways`.
        {
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
    }

    // --- mode resolution -------------------------------------------------

    #[test]
    fn breakpoint_selects_side_by_side_or_push_mode() {
        // Case `breakpoint_boundary_is_inclusive_wide`.
        {
            assert_eq!(ShellMode::from_width(700.0, 700.0), ShellMode::Wide);
            assert_eq!(ShellMode::from_width(699.0, 700.0), ShellMode::Narrow);
        }

        // Case `wide_is_side_by_side_with_docked_sidebar`.
        {
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

        // Case `narrow_is_push_with_overlay_sidebar`.
        {
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

        // Case `content_only_shell_has_no_sidebar`.
        {
            let ns = shell(1200.0, false);
            assert!(!ns.has_sidebar());
            assert_eq!(ns.sidebar_layout(), None);
            assert!(!ns.is_sidebar_open());
            assert!((ns.overview_rect().x0 - ns.get_rect().x0).abs() < 0.5);
        }

        // Case `resize_across_breakpoint_returns_sidebar_to_auto`.
        {
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
    }

    // --- push / pop ------------------------------------------------------

    #[test]
    fn push_navigation_shows_detail_narrow_and_both_panes_wide() {
        // Case `narrow_push_shows_detail_then_pops_back`.
        {
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

        // Case `wide_shows_both_content_panes_regardless_of_push_target`.
        {
            let mut ns = shell(1200.0, true);
            ns.show_detail(); // sets target, but wide keeps both abreast
            ns.snap();
            assert!(
                ns.overview_rect().width() > 1.0,
                "overview still visible wide"
            );
            assert!(ns.detail_rect().width() > 1.0, "detail still visible wide");
        }

        // Case `swapping_panes_preserves_push_state_and_relayouts_new_child`.
        {
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
    }

    // --- affordance is a collapse control, NEVER an X --------------------

    #[test]
    fn navigation_affordance_matches_mode_and_push_state() {
        // Case `affordance_is_sidebar_toggle_in_wide_with_sidebar`.
        {
            let ns = shell(1200.0, true);
            assert_eq!(ns.top_left_affordance(), Affordance::SidebarToggle);
            assert!(ns.collapse_button_rect().width() > 0.0);
            assert_eq!(ns.back_button_rect(), Rect::ZERO);
        }

        // Case `affordance_is_hamburger_to_summon_narrow_drawer`.
        {
            let ns = shell(420.0, true);
            assert_eq!(ns.top_left_affordance(), Affordance::SidebarToggle);
            assert!(
                !ns.is_sidebar_open(),
                "glyph should be the hamburger (closed)"
            );
        }

        // Case `affordance_becomes_back_when_detail_pushed_narrow`.
        {
            let mut ns = shell(420.0, true);
            ns.snap_pane(NavPane::Detail);
            assert_eq!(ns.top_left_affordance(), Affordance::Back);
            assert!(ns.back_button_rect().width() > 0.0);
            assert_eq!(ns.collapse_button_rect(), Rect::ZERO);
        }

        // Case `content_only_wide_has_no_affordance`.
        {
            let ns = shell(1200.0, false);
            assert_eq!(ns.top_left_affordance(), Affordance::None);
            assert_eq!(ns.collapse_button_rect(), Rect::ZERO);
            assert_eq!(ns.back_button_rect(), Rect::ZERO);
        }
    }

    // --- collapse -------------------------------------------------------

    #[test]
    fn toggles_escape_scrim_and_buttons_open_dismiss_and_pop() {
        // Case `toggle_collapses_docked_sidebar_and_reclaims_width`.
        {
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

        // Case `toggle_opens_narrow_drawer_with_scrim`.
        {
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

        // Case `esc_dismisses_open_narrow_drawer`.
        {
            let mut ns = shell(420.0, true);
            ns.open_sidebar();
            ns.snap_sidebar(true);
            assert!(ns.is_sidebar_open());
            let mut handled = false;
            dispatch(&mut ns, &esc_event(), &mut handled);
            assert!(handled, "Esc on an open drawer is consumed");
            assert!(!ns.is_sidebar_open(), "Esc closes the drawer");
        }

        // Case `esc_pops_pushed_detail`.
        {
            let mut ns = shell(420.0, true);
            ns.snap_pane(NavPane::Detail);
            let mut handled = false;
            dispatch(&mut ns, &esc_event(), &mut handled);
            assert!(handled, "Esc on a pushed detail is consumed");
            assert_eq!(ns.pane(), NavPane::Overview, "Esc pops back to overview");
        }

        // Case `scrim_click_dismisses_narrow_drawer`.
        {
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

        // Case `collapse_button_click_toggles_sidebar`.
        {
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

        // Case `back_button_click_pops_detail`.
        {
            let mut ns = shell(420.0, true);
            ns.snap_pane(NavPane::Detail);
            let c = ns.back_button_rect().center();
            let mut handled = false;
            dispatch(&mut ns, &cursor_at(c), &mut handled);
            dispatch(&mut ns, &left_press(), &mut handled);
            assert!(handled, "the back chip owns its click");
            assert_eq!(ns.pane(), NavPane::Overview, "Back pops to overview");
        }
    }

    // --- self-routing ----------------------------------------------------

    // --- breakpoint reconcile -------------------------------------------

    // --- rendering -------------------------------------------------------

    #[test]
    fn push_and_drawer_animate_or_snap_deterministically() {
        // Case `draw_emits_fills`.
        {
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

        // Case `push_slides_across_frames_then_settles`.
        {
            let mut ns = shell(420.0, true);
            ns.snap_pane(NavPane::Overview);
            ns.show_detail();

            draw_frame(&mut ns); // establishes the clock; dt == 0, no jump
            assert_eq!(ns.push.reveal(), 0.0, "first frame must not jump the slide");

            elapse(&mut ns, 16);
            draw_frame(&mut ns);
            let r = ns.push.reveal();
            assert!(r > 0.0 && r < 1.0, "mid-slide after one frame (reveal {r})");
            assert!(
                ns.needs_animation_frame(),
                "mid-slide keeps requesting frames"
            );

            for _ in 0..240 {
                elapse(&mut ns, 16);
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

        // Case `reduced_motion_snaps_push_and_drawer_to_their_targets`.
        {
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

        // Case `snap_stays_deterministic_through_draw`.
        {
            let mut ns = shell(420.0, true);
            ns.show_detail();
            ns.snap();
            assert_eq!(ns.push.reveal(), 1.0, "snap jumps straight to detail");
            elapse(&mut ns, 20);
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
    }

    // --- three-pane collapse keeps content, never the sidebar -----------

    #[test]
    fn collapse_keeps_the_hinted_or_last_interacted_pane() {
        // Case `three_pane_cold_collapse_keeps_the_rightmost_content_and_overlays_the_sidebar`.
        {
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

        // Case `three_pane_collapse_hint_overview_overrides_the_rightmost_default`.
        {
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

        // Case `three_pane_collapse_keeps_the_last_interacted_pane`.
        {
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

        // Case `content_only_collapse_keeps_the_overview_first_default`.
        {
            let mut ns = shell(1200.0, false); // no sidebar
            lay_out(&mut ns, 520.0, 640.0);
            assert_eq!(ns.mode(), ShellMode::Narrow);
            assert_eq!(
                ns.pane(),
                NavPane::Overview,
                "a sidebar-less master/detail collapses to the overview, unchanged",
            );
        }
    }

    // --- scaffolding -----------------------------------------------------

    /// Pretend `ms` of wall clock passed since the last drawn frame, so the
    /// next draw advances the animations by exactly that step.
    fn elapse(ns: &mut NavShell, ms: u64) {
        ns.last_tick = crate::time::now().checked_sub(crate::time::Duration::from_millis(ms));
    }

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

    // --- F1 round two: theme-resolved inputs, host-renderer measuring ------

    fn lay_out_with(ns: &mut NavShell, theme: &Theme, w: f64, h: f64) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = ns.build_layout(&mut taffy, theme, &mut node_map);
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
}
