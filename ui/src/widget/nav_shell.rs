//! `NavShell` — the HIG §1 navigation primitive.
//!
//! Where [`crate::widget::NavSplit`] is a *single* responsive split (one
//! sidebar + one content pane), `NavShell` bakes in the full HIG §1 model,
//! which has **two distinct kinds of panel that must not be conflated**:
//!
//! 1. A **genuine sidebar / menu** (pure navigation — a list of
//!    calendars, accounts, folders). §1a.
//!    * **Wide:** SOLID, **docked** beside the content, collapsible via a
//!      proper **collapse / hamburger button** (never an `X`). Reads as part
//!      of the frame. Smooth collapse animation.
//!    * **Narrow:** it **stays a sidebar** — an **overlay drawer** over the
//!      content behind a scrim, summoned/dismissed by the *same* button. It
//!      does **not** become a full-screen page.
//!
//! 2. A **content master / detail** region (overview + detail are *content*
//!    the user reads and acts on — a month grid + an event, a message list +
//!    a message, a city list + a forecast). §1b.
//!    * **Wide:** overview and detail shown **side by side** (alongside the
//!      sidebar, if any). Only the sidebar collapses.
//!    * **Narrow:** only ONE of {overview, detail} at a time. Overview is the
//!      default full-screen view; selecting an entry **slides the detail in
//!      from the right** over the overview, with a **back button top-left**.
//!      Back returns. **Push** navigation — never a scrim-overlay of content
//!      over content.
//!
//! A full app (calendar / email) composes BOTH: `[ sidebar | overview |
//! detail ]` — desktop all three abreast (only the sidebar collapses); mobile
//! the sidebar is an overlay drawer AND the overview↔detail use push/slide. A
//! simpler app (weather) uses only the content master/detail piece via
//! [`NavShell::new`] with no sidebar.
//!
//! ## Why this is a primitive, not per-app code
//! The HIG's rule of thumb: *if an app can express a bad-UX layout without
//! effort, that is a toolkit bug.* `NavShell` makes the correct behaviour the
//! default — the collapse affordance is a hamburger/chevron (the widget
//! **refuses** to render an `X`), content pushes instead of scrim-overlaying,
//! and the sidebar-drawer scrim is reserved for the *menu* only. Apps supply
//! the three child elements and react to selection; the shell owns the
//! breakpoint, the eased collapse + push slides, the scrim, and the
//! button/scrim/Esc self-routing.
//!
//! ## Easing
//! Reuses the frame-rate-independent [`FlapState`] kernel proven in
//! [`crate::widget::NavSplit`] (exponential approach, ease-out-cubic slide),
//! so a collapse and a push decelerate into place and settle to *no frames at
//! rest* (HIG §5 — no idle spin).

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::time::Instant;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Cap, Join, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::icons::{Icon, draw_icon};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::nav_split::{FlapState, SplitLayout};
use crate::widget::{Element, Widget};

/// Default docked / fully-revealed sidebar width (logical px).
pub const DEFAULT_NAVSHELL_SIDEBAR_WIDTH: f64 = 280.0;
/// Default overview (master) width in the side-by-side layout (logical px).
/// The detail pane takes the remaining width.
pub const DEFAULT_NAVSHELL_OVERVIEW_WIDTH: f64 = 340.0;
/// Default width breakpoint (logical px). At or above this the shell is
/// **wide** (docked sidebar + side-by-side content); below it the shell is
/// **narrow** (overlay-drawer sidebar + push content).
pub const DEFAULT_NAVSHELL_BREAKPOINT: f64 = 700.0;
/// The detail pane never shrinks below this in the side-by-side layout — the
/// overview gives up width first.
const MIN_DETAIL_WIDTH: f64 = 260.0;
/// Upper bound (seconds) on the per-frame delta used to advance a slide, so a
/// stale `Instant` (first frame, or a parked event loop) can't teleport a
/// slide past its animation.
const MAX_FRAME_DT: f64 = 0.1;
/// Logical size of the square collapse/hamburger chip.
const CHIP_SIZE: f64 = 36.0;
/// Inset of the top-left affordance chip from the shell's top-left corner.
const CHIP_INSET: f64 = 12.0;
/// Extra width the "Back" chip needs for its label beside the chevron.
const BACK_CHIP_WIDTH: f64 = 104.0;

/// The shell's responsive mode, resolved from its allocated width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellMode {
    /// Docked sidebar + side-by-side content (`>=` breakpoint).
    Wide,
    /// Overlay-drawer sidebar + push content (`<` breakpoint).
    Narrow,
}

impl ShellMode {
    /// Resolve from the allocated width. Boundary is inclusive at the
    /// breakpoint → [`ShellMode::Wide`].
    pub fn from_width(total_w: f64, breakpoint: f64) -> Self {
        if total_w >= breakpoint { ShellMode::Wide } else { ShellMode::Narrow }
    }
}

/// How the **content** master/detail region resolves at the current width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentLayout {
    /// Overview + detail shown abreast (wide).
    SideBySide,
    /// One pane at a time; detail pushes in from the right (narrow).
    Push,
    /// There is only one content pane, so it fills the content region at
    /// every width. The shape of a sidebar-plus-content app (§1a with no
    /// §1b split) — a file manager, a settings pane — where the sidebar is
    /// pure navigation and the content is the single thing being read.
    Single,
}

/// Which content pane the push flow is targeting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavPane {
    /// The overview (master list / grid).
    Overview,
    /// The detail, with a Back affordance in narrow mode.
    Detail,
}

/// What the single top-left affordance is doing this frame. Exactly one
/// affordance ever occupies the corner, so it is never ambiguous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affordance {
    /// No affordance (e.g. wide content-only app, or a narrow full-screen
    /// overview with no sidebar).
    None,
    /// The sidebar collapse / summon button (hamburger when closed,
    /// chevron when open) — **never** an `X`.
    SidebarToggle,
    /// The push "Back" button that pops the detail back to the overview.
    Back,
}

/// The HIG §1 navigation shell. See the module docs.
pub struct NavShell {
    /// The genuine menu sidebar (§1a). `None` for content-only apps.
    sidebar: Option<Element>,
    /// The content overview / master (§1b).
    overview: Element,
    /// The content detail (§1b). `None` for a sidebar-plus-content app,
    /// where there is no overview/detail split to push between and the
    /// single content pane must stay visible at every width.
    detail: Option<Element>,

    breakpoint: f64,
    sidebar_width: f64,
    overview_width: f64,

    /// Reveal easing for the sidebar (collapse when docked, slide when
    /// overlay). Open-when-docked by default.
    sidebar_flap: FlapState,
    /// Push target for the content region.
    pane: NavPane,
    /// Push easing: `0` = overview front, `1` = detail front. Closed
    /// (overview) by default.
    push: FlapState,
    /// The content pane the user most recently engaged (a press landed in it,
    /// or the host called [`NavShell::hint_active_pane`] / `show_detail` /
    /// `show_overview`). Drives which content pane survives a collapse to the
    /// narrow layout: the primary content stays put — never the sidebar. `None`
    /// until something is known, whereupon the collapse falls back to the
    /// right-most content pane (the detail).
    active_pane_hint: Option<NavPane>,
    /// The shell mode the previous layout resolved to, so `apply_layout` can
    /// spot a wide→narrow collapse and seed the visible content pane from the
    /// last-interacted one.
    last_mode: Option<ShellMode>,

    // --- resolved geometry (recomputed each layout / animation frame) ---
    rect: Rect,
    sidebar_rect: Rect,
    overview_rect: Rect,
    detail_rect: Rect,
    scrim_rect: Rect,
    /// The active top-left affordance chip's hit rect (zero when None).
    affordance_rect: Rect,
    affordance: Affordance,
    /// When set, the sidebar collapse chip is suppressed while the sidebar is
    /// *docked* (wide) — the sidebar is permanent there, so no toggle is needed.
    /// The hamburger still appears in the narrow overlay layout to summon the
    /// drawer. Off by default so existing shells keep their collapse control.
    hide_docked_toggle: bool,
    /// When set, a breakpoint crossing keeps the user's explicit sidebar
    /// open/collapsed choice instead of dropping it back to the width default.
    /// The host is expected to persist that choice as view/pref state (see
    /// [`NavShell::with_remembered_sidebar`]). Off by default so existing shells
    /// keep the auto-reset-on-resize behaviour.
    remember_sidebar: bool,

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
    /// Build a **content-only** shell (no genuine sidebar) — the weather
    /// case: an overview master + a detail. Side-by-side when wide, push when
    /// narrow.
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
            remember_sidebar: false,
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

    /// Build a **sidebar-plus-content** shell: a genuine menu sidebar (§1a)
    /// beside a *single* content pane. The file-manager / settings shape —
    /// the sidebar is pure navigation (places, sections) and the content is
    /// the one thing being read.
    ///
    /// Wide docks the sidebar beside the content. Narrow turns it into an
    /// overlay drawer *over* the content behind a scrim, summoned by the
    /// hamburger chip — the content stays visible throughout.
    ///
    /// Use this instead of [`NavShell::new`] whenever the left pane is a list
    /// of *destinations*. Passing a navigation list as `new`'s `overview`
    /// declares it to be §1b content, and narrow then promotes it to the
    /// full-screen page while pushing the real content off the window: a file
    /// manager that shows its places list and no files.
    pub fn sidebar_over_content(sidebar: Element, content: Element) -> Self {
        let mut shell = Self::new(content, Box::new(crate::widget::Spacer::new()));
        shell.sidebar = Some(sidebar);
        shell.detail = None;
        shell
    }

    /// Attach a genuine menu sidebar (§1a) — the calendar / email case:
    /// `[ sidebar | overview | detail ]`.
    pub fn with_sidebar(mut self, sidebar: Element) -> Self {
        self.sidebar = Some(sidebar);
        self
    }

    /// Override the width breakpoint (logical px).
    pub fn with_breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = px;
        self
    }

    /// Override the docked / revealed sidebar width (logical px).
    pub fn with_sidebar_width(mut self, px: f64) -> Self {
        self.sidebar_width = px;
        self
    }

    /// Override the overview (master) width in the side-by-side layout.
    pub fn with_overview_width(mut self, px: f64) -> Self {
        self.overview_width = px;
        self
    }

    /// Suppress the sidebar collapse chip while the sidebar is docked (wide):
    /// the sidebar is permanent there, so the toggle is clutter. The hamburger
    /// still appears in the narrow overlay layout to summon the drawer.
    pub fn hide_toggle_when_docked(mut self) -> Self {
        self.hide_docked_toggle = true;
        self
    }

    /// Start with the sidebar collapsed even when docked.
    pub fn with_sidebar_collapsed(mut self) -> Self {
        self.sidebar_flap = FlapState::closed_by_default();
        self
    }

    /// Remember the user's explicit sidebar open/collapsed choice across window
    /// resizes: a breakpoint crossing no longer drops it back to the width
    /// default (the annoying "the sidebar keeps re-opening when I resize").
    /// Pair this with persisting [`Self::sidebar_user_pref`] as view/pref state
    /// and re-applying it via [`Self::set_sidebar_pref`] on load.
    pub fn with_remembered_sidebar(mut self) -> Self {
        self.remember_sidebar = true;
        self
    }

    /// The user's explicit sidebar preference (`Some(true)` open / `Some(false)`
    /// collapsed), or `None` while still following the width-driven default.
    /// Persist this as view/pref state so the choice survives a relaunch.
    pub fn sidebar_user_pref(&self) -> Option<bool> {
        self.sidebar_flap.user_pref()
    }

    /// Re-apply a persisted sidebar preference (or clear it back to the layout
    /// default with `None`). The eased reveal catches up on the next frame.
    pub fn set_sidebar_pref(&mut self, pref: Option<bool>) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.set_pref(pref);
        self.dirty = true;
    }

    // --- live child swap -------------------------------------------------
    //
    // Hosts that **rebuild** their child subtree on a structure change (the
    // file-explorer rebuilds its list/grid + places list every time the
    // directory, sort, view mode or search changes) keep the `NavShell` itself
    // persistent and swap in the freshly-built panes here. Because the shell's
    // responsive state (docked/push, the eased reveal, the current pane) lives
    // on the shell — not the children — swapping the panes preserves the
    // sidebar's open/closed and master/detail position across the rebuild. The
    // next `apply_layout` re-solves the inner tree from the new children
    // (`place_children` always rebuilds it), so no explicit invalidation is
    // needed.

    /// Replace the overview (master) pane in place, keeping the shell's
    /// responsive state (see the module note on live child swap).
    pub fn set_overview(&mut self, overview: Element) {
        self.overview = overview;
    }

    /// Replace the detail pane in place, keeping the shell's responsive state.
    pub fn set_detail(&mut self, detail: Element) {
        self.detail = Some(detail);
    }

    /// Replace the single content pane of a sidebar-plus-content shell —
    /// the [`NavShell::sidebar_over_content`] shape. Same slot
    /// [`Self::set_overview`] writes; named for what it is so a
    /// single-pane app doesn't have to call its content an "overview".
    pub fn set_content(&mut self, content: Element) {
        self.overview = content;
    }

    /// Whether this shell has a content detail pane. `false` for a
    /// sidebar-plus-content shell.
    pub fn has_detail(&self) -> bool {
        self.detail.is_some()
    }

    /// Replace (or clear) the genuine menu sidebar in place, keeping the
    /// shell's responsive state.
    pub fn set_sidebar(&mut self, sidebar: Option<Element>) {
        self.sidebar = sidebar;
    }

    // --- layout-mode queries ---------------------------------------------

    /// Whether this shell has a genuine sidebar.
    pub fn has_sidebar(&self) -> bool {
        self.sidebar.is_some()
    }

    /// The shell's responsive mode at the current width.
    pub fn mode(&self) -> ShellMode {
        ShellMode::from_width(self.rect.width(), self.breakpoint)
    }

    /// The content region's layout at the current width.
    pub fn content_layout(&self) -> ContentLayout {
        if self.detail.is_none() {
            return ContentLayout::Single;
        }
        match self.mode() {
            ShellMode::Wide => ContentLayout::SideBySide,
            ShellMode::Narrow => ContentLayout::Push,
        }
    }

    /// The sidebar's presentation at the current width — [`SplitLayout::Docked`]
    /// (solid, wide) or [`SplitLayout::Overlay`] (drawer, narrow). `None` when
    /// there is no sidebar.
    pub fn sidebar_layout(&self) -> Option<SplitLayout> {
        self.sidebar.as_ref().map(|_| self.sidebar_split_layout())
    }

    /// Internal: the sidebar's [`SplitLayout`] regardless of whether one is
    /// attached (drives the reveal flap).
    fn sidebar_split_layout(&self) -> SplitLayout {
        match self.mode() {
            ShellMode::Wide => SplitLayout::Docked,
            ShellMode::Narrow => SplitLayout::Overlay,
        }
    }

    /// The content pane the push flow is targeting.
    pub fn pane(&self) -> NavPane {
        self.pane
    }

    /// Whether the detail pane is the push target. Always `false` when
    /// there is no detail pane — a single content pane is never "pushed
    /// over" anything.
    pub fn is_detail_shown(&self) -> bool {
        self.detail.is_some() && self.pane == NavPane::Detail
    }

    /// Whether the sidebar is (resolved) open. `false` when there is none.
    pub fn is_sidebar_open(&self) -> bool {
        self.has_sidebar() && self.sidebar_flap.is_open(self.sidebar_split_layout())
    }

    /// Which affordance occupies the top-left corner this frame.
    pub fn top_left_affordance(&self) -> Affordance {
        // Back always wins while the detail is pushed in a narrow layout.
        if self.content_layout() == ContentLayout::Push && self.pane == NavPane::Detail {
            Affordance::Back
        } else if self.has_sidebar() {
            // A docked sidebar needs no collapse chip when the host opts out —
            // it is permanent at this width. The narrow overlay keeps the
            // hamburger so the drawer can still be summoned.
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

    /// Toggle the sidebar open/closed (the collapse / hamburger button).
    pub fn toggle_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.toggle(self.sidebar_split_layout());
        self.dirty = true;
    }

    /// Open the sidebar.
    pub fn open_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.open();
        self.dirty = true;
    }

    /// Close the sidebar (scrim / Esc / the collapse button).
    pub fn close_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        self.sidebar_flap.close();
        self.dirty = true;
    }

    /// Push the detail pane in (call from the overview's selection handler).
    /// In the side-by-side (wide) layout both panes are already visible, so
    /// this only sets the push target for when the layout is narrow.
    pub fn show_detail(&mut self) {
        self.pane = NavPane::Detail;
        self.active_pane_hint = Some(NavPane::Detail);
        self.push.open();
        self.dirty = true;
    }

    /// Pop back to the overview (the Back button / Esc).
    pub fn show_overview(&mut self) {
        self.pane = NavPane::Overview;
        self.active_pane_hint = Some(NavPane::Overview);
        self.push.close();
        self.dirty = true;
    }

    /// Hint which content pane the user is currently working in (the
    /// "active / last-interacted" pane). Optional — the shell already records
    /// this from pointer presses and from `show_detail`/`show_overview` — but a
    /// host that drives selection through its own model can state it directly.
    /// It only affects a collapse to the narrow layout: that pane is the one
    /// kept on screen, so the content the user was reading survives the resize
    /// instead of snapping back to a default. No effect while wide (both panes
    /// are visible).
    pub fn hint_active_pane(&mut self, pane: NavPane) {
        self.active_pane_hint = Some(pane);
    }

    /// Which content pane a collapse of a genuine three-pane shell
    /// (`sidebar | overview | detail`) keeps on screen. The rule the whole
    /// fleet follows: a collapse **never** lands on the sidebar — it keeps the
    /// *content* the user was at. Only called when a detail pane exists, so the
    /// right-most fallback is always valid.
    ///
    /// * The last-interacted pane, when known — a press landed in it, or the
    ///   host called [`Self::hint_active_pane`] / `show_detail` /
    ///   `show_overview`. This is the "the panel the user is at" case, and it
    ///   keeps a message the user had open open through the resize.
    /// * Otherwise the **right-most content pane** (the detail) — the sensible
    ///   default that needs no per-app responsive logic, matching the pane a
    ///   wide shell already showed on the right. A host that wants the master
    ///   up front can say so with [`Self::hint_active_pane`].
    fn collapse_content_pane(&self) -> NavPane {
        self.active_pane_hint.unwrap_or(NavPane::Detail)
    }

    /// Alias for [`Self::show_overview`] — master/detail vocabulary.
    pub fn show_master(&mut self) {
        self.show_overview();
    }

    /// Snap **all** animation straight to its endpoint — deterministic
    /// `--shots` / headless. Call after setting the desired open/pane state.
    pub fn snap(&mut self) {
        let open = self.sidebar_flap.is_open(self.sidebar_split_layout());
        self.sidebar_flap.snap(open);
        self.push.snap(self.pane == NavPane::Detail);
        self.recompute_rects();
    }

    /// Snap the sidebar open/closed with no animation (deterministic shots).
    pub fn snap_sidebar(&mut self, open: bool) {
        self.sidebar_flap.snap(open);
        self.recompute_rects();
    }

    /// Snap the content push to a pane with no animation (deterministic shots).
    pub fn snap_pane(&mut self, pane: NavPane) {
        self.pane = pane;
        self.push.snap(pane == NavPane::Detail);
        self.recompute_rects();
    }

    // --- resolved rects (for hit-testing) --------------------------------

    /// Resolved sidebar rect.
    pub fn sidebar_rect(&self) -> Rect {
        self.sidebar_rect
    }
    /// Resolved overview (master) rect.
    pub fn overview_rect(&self) -> Rect {
        self.overview_rect
    }
    /// Resolved detail rect.
    pub fn detail_rect(&self) -> Rect {
        self.detail_rect
    }
    /// Resolved scrim rect (zero unless a narrow sidebar drawer is open).
    pub fn scrim_rect(&self) -> Rect {
        self.scrim_rect
    }
    /// Resolved rect of the collapse / hamburger button (zero unless that
    /// affordance is active this frame).
    pub fn collapse_button_rect(&self) -> Rect {
        if self.affordance == Affordance::SidebarToggle {
            self.affordance_rect
        } else {
            Rect::ZERO
        }
    }
    /// Resolved rect of the Back button (zero unless active this frame).
    pub fn back_button_rect(&self) -> Rect {
        if self.affordance == Affordance::Back {
            self.affordance_rect
        } else {
            Rect::ZERO
        }
    }

    // --- geometry --------------------------------------------------------

    /// Recompute every sub-rect from the outer rect, the resolved mode, and
    /// the eased reveal/push values.
    fn recompute_rects(&mut self) {
        let r = self.rect;
        let w = r.width();
        let has_sidebar = self.has_sidebar();
        let sw = self.sidebar_width.min(w);

        // Sidebar-plus-content: there is nothing to push between, so the one
        // content pane fills the content region at every width. Narrow turns
        // the sidebar into a drawer *over* that content (HIG §1a) — it never
        // promotes the navigation list to a full-screen page, which is what
        // the overview/detail branch below would do and what left a file
        // manager showing its places list and no files.
        if self.detail.is_none() {
            self.detail_rect = Rect::ZERO;
            match self.mode() {
                ShellMode::Wide => {
                    let inset =
                        if has_sidebar { sw * self.sidebar_flap.reveal() } else { 0.0 };
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
                // Sidebar docked: content is inset by the eased sidebar width
                // so the collapse plays smoothly without overlap.
                let inset = if has_sidebar { sw * self.sidebar_flap.reveal() } else { 0.0 };
                self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + inset, r.y1);

                // Remaining width → overview (fixed) + detail (rest), detail
                // never below MIN_DETAIL_WIDTH.
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
                // Content pushes: overview slides left off-screen as the
                // detail slides in from the right.
                let p = self.push.slide(); // 0 = overview, 1 = detail
                self.overview_rect =
                    Rect::new(r.x0 - w * p, r.y0, r.x1 - w * p, r.y1);
                self.detail_rect = Rect::new(
                    r.x0 + w * (1.0 - p),
                    r.y0,
                    r.x1 + w * (1.0 - p),
                    r.y1,
                );

                // Sidebar overlay drawer over the content behind a scrim.
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

    /// Resolve which affordance owns the top-left corner and its hit rect.
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

    /// Whether the sidebar pane should be drawn this frame.
    fn sidebar_visible(&self) -> bool {
        self.has_sidebar() && self.sidebar_flap.reveal() > 0.001
    }

    /// The rect the sidebar's *child* is laid out into: [`Self::sidebar_rect`]
    /// with the top-left affordance chip's band reserved.
    ///
    /// The chip is painted in the shell's top-left corner, which is the corner
    /// the sidebar occupies whenever it is docked or its drawer is open — so
    /// without this the chip lands on top of the sidebar's first row and clips
    /// its section heading. The panel *fill* still uses the full rect, so the
    /// reserved band is sidebar-coloured rather than a notch.
    pub fn sidebar_content_rect(&self) -> Rect {
        let r = self.sidebar_rect;
        if self.affordance != Affordance::SidebarToggle || r.height() <= 0.0 {
            return r;
        }
        let band = CHIP_INSET * 2.0 + CHIP_SIZE;
        Rect::new(r.x0, (r.y0 + band).min(r.y1), r.x1, r.y1)
    }

    /// Paint the sidebar's own opaque panel. The drawer slides *over* live
    /// content, so without a fill the content reads straight through it — the
    /// file list showing through the places list. Drawn under the child so a
    /// sidebar that paints its own background simply covers this.
    fn fill_panel(scene: &mut Scene, transform: Affine, theme: &Theme, r: Rect) {
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &r);
    }

    /// Build the inner taffy tree for the present children at their resolved
    /// rects so they lay out + receive events.
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
        let mut jobs: Vec<(NodeId, Rect)> = vec![(o_node, self.overview_rect)];
        if let Some(dn) = d_node {
            jobs.push((dn, self.detail_rect));
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

        self.overview
            .apply_layout(&taffy, o_node, Point::new(self.overview_rect.x0, self.overview_rect.y0));
        if let (Some(d), Some(dn)) = (self.detail.as_mut(), d_node) {
            d.apply_layout(&taffy, dn, Point::new(self.detail_rect.x0, self.detail_rect.y0));
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

    /// Advance the reveal + push animations off a real wall-clock delta,
    /// recomputing + re-placing children while something moves. Driven from
    /// the draw path (never `update`, which `ui::App` never calls) so the
    /// slides actually play and settle to no frames at rest (HIG §5).
    fn self_tick(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let now = Instant::now();
        let dt = self
            .last_tick
            .map(|t| now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT))
            .unwrap_or(0.0);
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

    /// Paint the collapse/hamburger or Back chip. The widget structurally
    /// **cannot** draw an `X` here — the glyph is always a hamburger, a
    /// chevron, or a back-arrow.
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

        // Rounded chip background so the affordance reads as a control.
        let bg = RoundedRect::from_rect(rect, 8.0);
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &bg);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &bg,
        );

        // The glyph sits in a leading CHIP_SIZE square of the chip.
        let glyph_box = Rect::new(
            rect.x0,
            rect.y0,
            rect.x0 + CHIP_SIZE,
            rect.y1,
        );
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
                    transform
                        * Affine::translate((glyph_box.x1 - 2.0, rect.y0 + CHIP_SIZE / 2.0 - 9.0)),
                    None,
                );
            }
            Affordance::SidebarToggle => {
                if self.is_sidebar_open() {
                    // Open → offer to collapse: a chevron pointing back at
                    // the edge the sidebar tucks into. Never an `X`.
                    draw_icon(
                        scene,
                        transform,
                        glyph_box.inflate(-9.0, -9.0),
                        Icon::ChevronLeft,
                        theme.text_primary,
                        1.75,
                    );
                } else {
                    // Closed → offer to summon: the universal hamburger.
                    Self::draw_hamburger(scene, transform, glyph_box, theme.text_primary);
                }
            }
        }
    }

    /// Draw a three-line hamburger centred in `box_rect`.
    fn draw_hamburger(scene: &mut Scene, transform: Affine, box_rect: Rect, color: Color) {
        let cx = box_rect.center().x;
        let cy = box_rect.center().y;
        let half_w = 9.0;
        let gap = 5.5;
        let stroke = Stroke::new(2.0).with_caps(Cap::Round).with_join(Join::Round);
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
        // The shell fills its whole allocation; children live in an isolated
        // inner tree (their rects escape normal flow in overlay/push mode).
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        // A window resize that crosses the breakpoint drops a stale sidebar
        // preference so the sidebar returns to its auto behaviour — unless the
        // host opted to remember the choice, in which case an explicit
        // open/collapse survives the resize (persisted view/pref state).
        let split_layout = self.sidebar_split_layout();
        if self.remember_sidebar {
            self.sidebar_flap.reconcile_sticky(split_layout);
        } else {
            self.sidebar_flap.reconcile(split_layout);
        }

        // On a wide→narrow collapse of a genuine three-pane shell
        // (`sidebar | overview | detail`) the content region can only show one
        // pane. Keep the last-interacted content pane so the pane the user was
        // reading survives the resize (a message they had open stays open),
        // falling back to the right-most content pane (the detail) with no app
        // change; the sidebar becomes a closed overlay drawer either way, so a
        // collapse never lands on the navigation (see
        // [`Self::collapse_content_pane`]).
        let mode = self.mode();
        if self.last_mode == Some(ShellMode::Wide)
            && mode == ShellMode::Narrow
            && self.has_sidebar()
            && self.has_detail()
        {
            self.pane = self.collapse_content_pane();
            // A collapse is a structural jump, not a user push — settle the
            // slide to the resolved pane rather than animating across.
            self.push.snap(self.pane == NavPane::Detail);
        }
        self.last_mode = Some(mode);

        self.recompute_rects();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        self.place_children(&theme, &mut tr);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        // Slides advance from the draw path (`self_tick`), not here — `ui::App`
        // never calls `Widget::update`. Forward to children and report whether
        // anything still needs frames.
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

        // Content panes first (behind the overlay drawer). Off-screen panes
        // in push mode simply fall outside the framebuffer.
        self.overview.draw(scene, transform, text_renderer, theme, presence);
        if let Some(d) = self.detail.as_mut() {
            d.draw(scene, transform, text_renderer, theme, presence);
        }
        if self.sidebar_visible() {
            if let Some(s) = self.sidebar.as_mut() {
                // Scrim must sit between content and the drawer; draw it here.
                if self.scrim_rect.width() > 0.0 {
                    let alpha = (self.sidebar_flap.reveal() * 0.5) as f32;
                    let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(alpha);
                    scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
                }
                Self::fill_panel(scene, transform, theme, self.sidebar_rect);
                s.draw(scene, transform, text_renderer, theme, presence);
            }
        }
        // Dividers + affordance on top (skip the scrim — already drawn above).
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
        self.overview
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        if let Some(d) = self.detail.as_mut() {
            d.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
        if self.sidebar_visible() {
            if self.scrim_rect.width() > 0.0 {
                let alpha = (self.sidebar_flap.reveal() * 0.5) as f32;
                let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(alpha);
                scene.fill(Fill::NonZero, transform, scrim, None, &self.scrim_rect);
            }
            Self::fill_panel(scene, transform, theme, self.sidebar_rect);
            if let Some(s) = self.sidebar.as_mut() {
                s.draw_with_presence(
                    scene, transform, text_renderer, theme, presence, permissions,
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
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
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
                // Esc: dismiss the overlay drawer, else pop the pushed detail.
                if self.is_sidebar_open()
                    && self.sidebar_split_layout() == SplitLayout::Overlay
                {
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
                // 3. Record which content pane the user engaged, so a later
                //    collapse keeps the pane they were at. A press over the open
                //    sidebar drawer is navigation, not content — ignore it.
                if self.detail.is_some()
                    && !(self.sidebar_visible() && self.sidebar_rect.contains(self.last_cursor))
                {
                    if self.detail_rect.width() > 0.0
                        && self.detail_rect.contains(self.last_cursor)
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

        // Route to children via the inner tree. The overlay drawer (when
        // open) is topmost, so route it first; otherwise route the front
        // content pane first.
        // The detail node is absent for a sidebar-plus-content shell; that
        // must not stop the sidebar and content panes from being routed to.
        let (Some(taffy), Some(o_node)) = (self.inner_taffy.as_ref(), self.overview_node) else {
            return;
        };
        let d_node = self.detail_node;
        let sidebar_first = self.is_sidebar_open()
            && self.sidebar_split_layout() == SplitLayout::Overlay;

        // Build the routing order. Each entry is (which child, its node).
        enum Which {
            Sidebar,
            Overview,
            Detail,
        }
        let mut order: Vec<(Which, NodeId)> = Vec::new();
        if sidebar_first {
            if let Some(sn) = self.sidebar_node {
                order.push((Which::Sidebar, sn));
            }
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
        if !sidebar_first {
            if let Some(sn) = self.sidebar_node {
                order.push((Which::Sidebar, sn));
            }
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
        let mut v: Vec<&mut (dyn Widget + '_)> =
            vec![self.overview.as_mut()];
        if let Some(s) = self.sidebar.as_mut() {
            v.push(s.as_mut());
        }
        v
    }

    fn name(&self) -> &str {
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
            || self.detail.as_ref().is_some_and(|d| d.needs_animation_frame())
            || self
                .sidebar
                .as_ref()
                .map(|s| s.needs_animation_frame())
                .unwrap_or(false)
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
            || self
                .sidebar
                .as_ref()
                .map(|s| s.needs_idle_sync())
                .unwrap_or(false)
    }
}

impl NavShell {
    /// Chrome minus the scrim (the scrim is painted inline between content
    /// and the drawer in each draw path so its z-order is correct).
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
            NavShell::new(Box::new(Text::new("overview")), Box::new(Text::new("detail")))
                .with_sidebar(Box::new(Text::new("sidebar")))
        } else {
            NavShell::new(Box::new(Text::new("overview")), Box::new(Text::new("detail")))
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
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
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
        // Settle the reveal easing: the flap is open-when-docked, so a first
        // frame at a narrow width is mid-way through easing the drawer out.
        // Every assertion here is about the settled frame.
        ns.snap();
        ns
    }

    // --- sidebar + single content pane (HIG §1a, no §1b split) -----------

    /// The regression this shape exists for: a navigation sidebar must never
    /// become the full-screen page at a narrow width. The content pane keeps
    /// the whole window and the sidebar draws *over* it.
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
            ns.overview_rect().x0, 0.0,
            "and start at the window's left edge, not offset by a sidebar",
        );
        assert!(
            !ns.is_detail_shown(),
            "there is no detail pane to push in",
        );
        assert_eq!(
            ns.detail_rect(),
            Rect::ZERO,
            "no detail pane means no detail geometry",
        );
    }

    /// Narrow starts with the drawer closed, so the content is what you see;
    /// the hamburger summons the sidebar over it behind a scrim.
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

    /// Wide docks the sidebar and gives the content the rest — no second
    /// content pane appears out of nowhere.
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
            ns.overview_rect().x1, 1200.0,
            "and runs to the window's right edge",
        );
        assert_eq!(ns.detail_rect(), Rect::ZERO);
    }

    /// Resizing across the breakpoint reflows both ways without the content
    /// ever leaving the window.
    #[test]
    fn sidebar_over_content_reflows_across_the_breakpoint_both_ways() {
        let mut ns = sidebar_content_shell(1200.0);
        assert_eq!(ns.mode(), ShellMode::Wide);
        for w in [600.0, 1200.0, 480.0, 900.0] {
            lay_out(&mut ns, w, 640.0);
            ns.snap();
            assert_eq!(
                ns.overview_rect().x1, w,
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
        // Three abreast: sidebar, then overview, then detail, left→right.
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
        assert!(!ns.is_sidebar_open(), "overlay sidebar is closed by default");
        // Overview is full-screen by default (detail parked off to the right).
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
        // Wide content-only: overview flush left (no sidebar inset).
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
        // Detail now fills the pane; overview slid off to the left.
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
        assert!(ns.overview_rect().width() > 1.0, "overview still visible wide");
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
        // Overview front + closed sidebar → summon affordance present.
        assert_eq!(ns.top_left_affordance(), Affordance::SidebarToggle);
        assert!(!ns.is_sidebar_open(), "glyph should be the hamburger (closed)");
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
        assert_eq!(ns.scrim_rect(), Rect::ZERO, "docked collapse shows no scrim");
    }

    #[test]
    fn toggle_opens_narrow_drawer_with_scrim() {
        let mut ns = shell(420.0, true);
        ns.toggle_sidebar();
        ns.snap_sidebar(true);
        assert!(ns.is_sidebar_open());
        assert!(ns.scrim_rect().width() > 0.0, "open drawer dims the content");
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
        assert!(!ns.is_sidebar_open(), "clicking the collapse button collapses the sidebar");
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
        // Start wide + open, user collapses, then resize narrow → auto closed,
        // resize back wide → auto open again (stale preference dropped).
        let mut ns = shell(1200.0, true);
        ns.toggle_sidebar(); // user collapse
        assert!(!ns.is_sidebar_open());
        lay_out(&mut ns, 420.0, 640.0); // cross to narrow
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert!(!ns.is_sidebar_open(), "narrow default is closed");
        lay_out(&mut ns, 1200.0, 640.0); // cross back to wide
        assert!(
            ns.is_sidebar_open(),
            "crossing the breakpoint drops the stale collapse preference"
        );
    }

    #[test]
    fn remembered_sidebar_survives_a_resize_across_the_breakpoint() {
        // With `with_remembered_sidebar`, an explicit collapse is NOT dropped
        // when the window width crosses the breakpoint — the user override beats
        // the width-driven default, so widening does not force the sidebar open.
        let mut ns =
            NavShell::new(Box::new(Text::new("overview")), Box::new(Text::new("detail")))
                .with_sidebar(Box::new(Text::new("sidebar")))
                .with_remembered_sidebar();
        lay_out(&mut ns, 1200.0, 640.0);
        assert!(ns.is_sidebar_open(), "docked default is open");
        ns.toggle_sidebar(); // user collapses it
        assert!(!ns.is_sidebar_open());
        assert_eq!(ns.sidebar_user_pref(), Some(false), "the choice is recorded");

        lay_out(&mut ns, 420.0, 640.0); // cross to narrow
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert!(!ns.is_sidebar_open(), "narrow keeps it closed");

        lay_out(&mut ns, 1200.0, 640.0); // cross back to wide
        assert!(
            !ns.is_sidebar_open(),
            "the remembered collapse survives the resize — the sidebar does NOT re-open"
        );
        assert_eq!(ns.sidebar_user_pref(), Some(false), "preference persists");
    }

    #[test]
    fn set_sidebar_pref_rehydrates_a_persisted_choice() {
        // Re-applying a persisted collapse (e.g. from the doc on load) collapses
        // a shell whose width default would otherwise open the sidebar.
        let mut ns =
            NavShell::new(Box::new(Text::new("overview")), Box::new(Text::new("detail")))
                .with_sidebar(Box::new(Text::new("sidebar")))
                .with_remembered_sidebar();
        lay_out(&mut ns, 1200.0, 640.0);
        assert!(ns.is_sidebar_open(), "docked default is open before rehydrate");
        ns.set_sidebar_pref(Some(false));
        lay_out(&mut ns, 1200.0, 640.0);
        assert!(!ns.is_sidebar_open(), "a persisted collapse is honoured on load");
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
        ns.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &perms);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(fills >= 1, "a laid-out shell must paint something");
    }

    /// The push slide must *animate* across frames from the draw path, not
    /// teleport — and settle to no-frames at rest (HIG §5).
    #[test]
    fn push_slides_across_frames_then_settles() {
        let mut ns = shell(420.0, true);
        ns.snap_pane(NavPane::Overview); // deterministic closed start
        ns.show_detail(); // target flips to detail (push 0 → 1)

        draw_frame(&mut ns); // establishes the clock; dt == 0, no jump
        assert_eq!(ns.push.reveal(), 0.0, "first frame must not jump the slide");

        std::thread::sleep(std::time::Duration::from_millis(16));
        draw_frame(&mut ns);
        let r = ns.push.reveal();
        assert!(r > 0.0 && r < 1.0, "mid-slide after one frame (reveal {r})");
        assert!(ns.needs_animation_frame(), "mid-slide keeps requesting frames");

        for _ in 0..240 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            draw_frame(&mut ns);
            if !ns.push.is_animating(SplitLayout::Docked) {
                break;
            }
        }
        assert!((ns.push.reveal() - 1.0).abs() < 0.01, "slide settles fully in");
        assert!(!ns.needs_animation_frame(), "a settled shell requests no frames");
    }

    #[test]
    fn snap_stays_deterministic_through_draw() {
        let mut ns = shell(420.0, true);
        ns.show_detail();
        ns.snap();
        assert_eq!(ns.push.reveal(), 1.0, "snap jumps straight to detail");
        std::thread::sleep(std::time::Duration::from_millis(20));
        draw_frame(&mut ns);
        assert_eq!(ns.push.reveal(), 1.0, "a snapped shell must not drift on draw");
        assert!(!ns.needs_animation_frame(), "a settled shell requests no frames");
    }

    /// A host that rebuilds its panes on a structure change swaps them into the
    /// persistent shell via `set_overview` / `set_detail`; the swap must keep
    /// the shell's push position (the whole point of keeping the shell around
    /// instead of reconstructing it), and the next layout must pick up the new
    /// child.
    #[test]
    fn swapping_panes_preserves_push_state_and_relayouts_new_child() {
        let mut ns = shell(420.0, false); // narrow, content-only
        ns.snap_pane(NavPane::Detail); // push to detail (deterministic)
        assert!(ns.is_detail_shown(), "pushed to detail before the swap");

        // Rebuild both panes (a fresh directory listing would do this) and swap
        // them in. The pushed-detail position must survive the swap.
        ns.set_overview(Box::new(Text::new("new-overview")));
        ns.set_detail(Box::new(Text::new("new-detail")));
        assert!(ns.is_detail_shown(), "the swap kept the shell showing the detail");

        // Re-lay-out at the same size: the shell re-solves its inner tree from
        // the NEW children, so the freshly-swapped detail occupies the pane.
        lay_out(&mut ns, 420.0, 640.0);
        ns.snap_pane(NavPane::Detail);
        assert!(
            (ns.detail_rect().x0 - ns.get_rect().x0).abs() < 1.0,
            "the swapped-in detail is laid out in the pane"
        );
        // And a real draw of the new children paints something.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let perms = crate::permissions::PermissionContext::permissive();
        let mut scene = Scene::new();
        ns.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &perms);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(fills >= 1, "the swapped children paint after the swap");
    }

    // --- three-pane collapse keeps content, never the sidebar -----------

    /// A genuine three-pane shell (`sidebar | overview | detail`) collapsed to
    /// narrow keeps a *content* pane on screen and pushes the sidebar into a
    /// closed overlay drawer — the collapse never lands on the navigation.
    /// With no prior interaction the collapse falls back to the right-most
    /// content pane (the detail), the sensible default that needs no per-app
    /// responsive logic and matches what the wide shell showed on the right.
    #[test]
    fn three_pane_cold_collapse_keeps_the_rightmost_content_and_overlays_the_sidebar() {
        let mut ns = shell(1200.0, true); // wide, sidebar + overview + detail
        assert_eq!(ns.mode(), ShellMode::Wide);
        assert_eq!(ns.content_layout(), ContentLayout::SideBySide);

        // Collapse to a narrow width (a window resize).
        lay_out(&mut ns, 520.0, 640.0);
        // Settle the reveal easing: the docked sidebar was open, so the first
        // narrow frame is mid-way through easing the drawer shut. Assert the
        // settled frame (what `--shots` / headless render deterministically).
        ns.snap();
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(ns.content_layout(), ContentLayout::Push);

        // A content pane fills the window (never the sidebar). Cold, that is the
        // right-most content — the detail — with no app change.
        assert_eq!(ns.pane(), NavPane::Detail, "a cold collapse keeps the right-most content, not the sidebar");
        assert!(
            (ns.detail_rect().x0 - ns.get_rect().x0).abs() < 1.0
                && (ns.detail_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "the content pane spans the whole narrow window",
        );
        assert!(
            ns.overview_rect().x1 <= ns.get_rect().x0 + 1.0,
            "the overview is pushed off-screen to the left",
        );

        // The sidebar is a closed overlay drawer — reachable, not shown.
        assert_eq!(ns.sidebar_layout(), Some(SplitLayout::Overlay));
        assert!(!ns.is_sidebar_open(), "the sidebar collapses to a closed drawer, not the shown pane");
        assert_eq!(ns.scrim_rect(), Rect::ZERO, "a closed drawer lays no scrim");
        // …and the drawer control is still there to summon it. (`Back` wins the
        // top-left while a detail is pushed, so summon it from the toggle API.)
        ns.toggle_sidebar();
        ns.snap();
        assert!(ns.is_sidebar_open(), "the collapsed sidebar is still summonable as a drawer");
    }

    /// A host hint overrides the right-most default: hinting the master keeps
    /// the overview through the collapse — the "the panel the user is at" path,
    /// steering away from the cold detail-first fallback.
    #[test]
    fn three_pane_collapse_hint_overview_overrides_the_rightmost_default() {
        let mut ns = shell(1200.0, true);
        ns.hint_active_pane(NavPane::Overview);
        lay_out(&mut ns, 520.0, 640.0);
        assert_eq!(ns.mode(), ShellMode::Narrow);
        assert_eq!(ns.pane(), NavPane::Overview, "the hinted master content overrides the right-most default");
        assert!(
            (ns.overview_rect().x0 - ns.get_rect().x0).abs() < 1.0
                && (ns.overview_rect().width() - ns.get_rect().width()).abs() < 1.0,
            "the overview spans the whole narrow window",
        );
        assert!(!ns.is_sidebar_open(), "the sidebar is still a closed overlay drawer");
    }

    /// The last-interacted content pane wins the collapse: after the user works
    /// in the overview (a press lands there while wide, side-by-side), collapsing
    /// keeps the overview — overriding the cold right-most (detail) default.
    #[test]
    fn three_pane_collapse_keeps_the_last_interacted_pane() {
        let mut ns = shell(1200.0, true);
        // A real press in the overview (left) column records it as active.
        let ov = ns.overview_rect();
        assert!(ov.width() > 1.0, "wide side-by-side shows an overview column");
        let pt = Point::new((ov.x0 + ov.x1) / 2.0, (ov.y0 + ov.y1) / 2.0);
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

    /// A content-only master/detail shell (no genuine sidebar — weather / mail)
    /// keeps its overview-first collapse: the master list there *is* content, so
    /// the right-most default must NOT hijack it.
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
        ns.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &perms);
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
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
        let node = taffy.new_leaf(Style::default()).unwrap();
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
