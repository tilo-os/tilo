//! `SidebarDrawer` — the headless sidebar controller for painted apps.
//!
//! A widget app uses [`crate::widget::NavShell`]; an app that paints its
//! whole window itself holds a `SidebarDrawer` instead. The drawer owns the
//! same decisions the shell makes, from the same metrics: docked beside the
//! content while it fits beside the content's minimum (shrinking toward its
//! own minimum first), the eased reveal, and the person's show/hide choice
//! per size class. Too narrow to dock, a sidebar whose app said what its
//! content is ([`SidebarDrawer::content_kind`]) takes turns with the content
//! as `NavShell`'s push layout does: open, the list fills the window
//! ([`SidebarDrawer::is_push`]) and the toggle in the header row brings the
//! content back, which is also where a choice in the list lands. A
//! content-primary app ([`crate::widget::ContentKind::Document`]), an
//! inspector and an [`SidebarDrawer::overlay_only`] panel float as a drawer
//! over the content instead. A drawer is not modal and dims nothing: a
//! press on the content beside it ([`SidebarDrawer::scrim_rect`]) puts it
//! away. It hands back the resolved
//! rects; the host paints the panel with [`SidebarDrawer::paint_panel`] and
//! the toggle with [`SidebarDrawer::draw_toggle`], and its rows with
//! [`crate::chrome::paint_nav_row`].
//!
//! Lifecycle per frame: [`SidebarDrawer::tick`] once from the draw path,
//! then [`SidebarDrawer::resolve`] with the outer rect. On the event path
//! call `resolve` alone so hit-tests match the last paint.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Metrics, Spacing};
use crate::widget::flap::{FlapState, SplitLayout};
use crate::widget::{AuthoredExtent, SplitHandle, SplitOrientation, SplitRatio};

/// The docked sidebar width every shell shares (`Metrics::sidebar_width`).
pub const DEFAULT_DRAWER_DOCKED_WIDTH: f64 = Metrics::BASE.sidebar_width as f64;
/// The overlay drawer width (`Metrics::drawer_width`).
pub const DEFAULT_DRAWER_WIDTH: f64 = Metrics::BASE.drawer_width as f64;
/// At or above this width the drawer docks (`Metrics::compact_breakpoint`).
pub const DEFAULT_DRAWER_BREAKPOINT: f64 = Metrics::BASE.compact_breakpoint as f64;

/// Which edge the drawer is anchored to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSide {
    /// Anchored to the leading edge: a navigation sidebar.
    Left,
    /// Anchored to the trailing edge: an inspector (calculator history).
    Right,
}

/// A collapsible sidebar or inspector for a painted app. See the module
/// docs.
#[derive(Clone, Debug)]
pub struct SidebarDrawer {
    side: SidebarSide,
    /// Authored at 1× (HIG §9.6, [`AuthoredExtent`]); resolved through the
    /// factors [`Self::scale_for`] caches.
    breakpoint: AuthoredExtent,
    content_min_width: Option<AuthoredExtent>,
    docked_width: AuthoredExtent,
    overlay_width: AuthoredExtent,
    /// Width left behind when collapsed while docked. `0.0` ⇒ fully hidden.
    /// An icon rail: follows `ui_scale`, never the text.
    rail_width: AuthoredExtent,
    /// `Theme::responsive_scale` and `Theme::ui_scale` of the theme last
    /// handed to [`Self::scale_for`]; `1.0` until then.
    responsive_scale: f64,
    ui_scale: f64,
    /// The `s2` gap an overlay drawer floats inside, from the same theme.
    float_inset: f64,
    flap: FlapState,

    // --- resolved geometry (last `resolve`) ---
    outer: Rect,
    layout: SplitLayout,
    sidebar_rect: Rect,
    content_rect: Rect,
    scrim_rect: Rect,

    // --- self-tick animation ---
    animating: bool,

    /// Never docks: a drawer over the content at every width.
    overlay_only: bool,
    /// Too narrow to dock, the sidebar and the content take turns filling
    /// the window ([`Self::content_kind`]) instead of the sidebar floating.
    push: bool,
    /// An icon rail stands beside the text list inside the drawer.
    with_rail: bool,
    /// `metrics.rail_width` of the theme last handed to [`Self::scale_for`],
    /// or the width its destinations' words need ([`Self::rail_labels`]).
    rail_column: f64,
    /// The rail's destination labels, when it shows captions: the rail then
    /// grows with the text to fit the longest word, as every shared rail does
    /// ([`crate::chrome::rail_width_fitting`]).
    rail_labels: Vec<String>,
    /// A snap asked for before the first [`Self::resolve`], kept until the
    /// layout is known.
    pending_snap: Option<bool>,
    resolved_once: bool,
    /// The bottom of the header row [`Self::header`] laid out: a floating
    /// drawer starts under it.
    header_bottom: Option<f64>,
    toggle: DrawerToggle,
    resize: SplitHandle,
    resize_min_width: f64,
    resize_max_width: f64,
    resize_redraw: bool,
}

/// The drawer's show/hide button in a painted header row: its rect, hover
/// and press, and the platform the shortcut is spelled for.
#[derive(Clone, Debug, Default)]
struct DrawerToggle {
    rect: Rect,
    hovered: bool,
    pressed: bool,
    pointer: Option<Point>,
    conventions: crate::platform::PlatformConventions,
}

impl Default for SidebarDrawer {
    fn default() -> Self {
        Self::new()
    }
}

impl SidebarDrawer {
    /// A leading navigation drawer, open when docked, at the shell metrics.
    pub fn new() -> Self {
        Self {
            side: SidebarSide::Left,
            breakpoint: AuthoredExtent::text(DEFAULT_DRAWER_BREAKPOINT),
            content_min_width: None,
            docked_width: AuthoredExtent::text(DEFAULT_DRAWER_DOCKED_WIDTH),
            overlay_width: AuthoredExtent::text(DEFAULT_DRAWER_WIDTH),
            rail_width: AuthoredExtent::icon(0.0),
            responsive_scale: 1.0,
            ui_scale: 1.0,
            float_inset: f64::from(Spacing::BASE.s2),
            flap: FlapState::open_when_docked(),
            outer: Rect::ZERO,
            layout: SplitLayout::Docked,
            sidebar_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            animating: false,
            overlay_only: false,
            push: false,
            with_rail: false,
            rail_column: f64::from(Metrics::BASE.rail_width),
            rail_labels: Vec::new(),
            pending_snap: None,
            resolved_once: false,
            header_bottom: None,
            toggle: DrawerToggle::default(),
            resize: SplitHandle::new(SplitOrientation::Horizontal, SplitRatio::default()),
            resize_min_width: f64::from(Metrics::BASE.sidebar_min_width),
            resize_max_width: f64::from(Metrics::BASE.sidebar_max_width),
            resize_redraw: false,
        }
    }

    // --- builders -----------------------------------------------------------

    #[must_use]
    pub fn side(mut self, side: SidebarSide) -> Self {
        self.side = side;
        self
    }
    #[must_use]
    pub fn left(self) -> Self {
        self.side(SidebarSide::Left)
    }
    #[must_use]
    pub fn right(self) -> Self {
        self.side(SidebarSide::Right)
    }
    /// Authored at 1× (text-sized); see [`Self::scale_for`].
    #[must_use]
    pub fn breakpoint(mut self, px: f64) -> Self {
        self.breakpoint = AuthoredExtent::text(px);
        self.content_min_width = None;
        self
    }
    /// Dock only while the content column keeps this authored minimum;
    /// the docked panel shrinks toward its minimum width first. The actual
    /// sidebar and icon rail widths remain independently scaled.
    #[must_use]
    pub fn content_min_width(mut self, px: f64) -> Self {
        self.content_min_width = Some(AuthoredExtent::text(px));
        self
    }
    /// Dock while the content keeps the minimum its kind needs
    /// ([`crate::widget::ContentKind`]), as `NavShell` does. Narrower, a
    /// leading sidebar and the content take turns filling the window (the
    /// push layout, [`Self::is_push`]); only content-primary content
    /// ([`crate::widget::ContentKind::Document`]) keeps a floating drawer.
    #[must_use]
    pub fn content_kind(mut self, kind: crate::widget::ContentKind) -> Self {
        self.push = !kind.is_content_primary();
        self.content_min_width(f64::from(kind.min_width(&Metrics::BASE)))
    }
    /// Docked width of a text-sized drawer (a page or destination list),
    /// authored at 1×.
    #[must_use]
    pub fn docked_width(mut self, px: f64) -> Self {
        self.docked_width = AuthoredExtent::text(px);
        self
    }
    /// Docked width of an **icon rail**: follows `ui_scale`, never the text.
    #[must_use]
    pub fn icon_docked_width(mut self, px: f64) -> Self {
        self.docked_width = AuthoredExtent::icon(px);
        self
    }
    /// Overlay width of the text-sized drawer, authored at 1×.
    #[must_use]
    pub fn overlay_width(mut self, px: f64) -> Self {
        self.overlay_width = AuthoredExtent::text(px);
        self
    }
    /// `0.0` hides it entirely. An icon rail: `ui_scale` only.
    #[must_use]
    pub fn rail_width(mut self, px: f64) -> Self {
        self.rail_width = AuthoredExtent::icon(px);
        self
    }
    /// A summon-on-demand panel.
    #[must_use]
    pub fn closed(mut self) -> Self {
        self.flap = FlapState::closed_by_default();
        self
    }
    /// A drawer over the content at every width, never docked (controls
    /// over full-bleed media: a player's queue, a viewer's info).
    #[must_use]
    pub fn overlay_only(mut self) -> Self {
        self.overlay_only = true;
        self
    }
    /// An icon rail beside the text list in the one drawer (workspaces
    /// beside a workspace's channels). The rail is `metrics.rail_width`
    /// wide, following `ui_scale`; the drawer grows by it, and
    /// [`Self::rail_rect`] and [`Self::list_rect`] split the panel.
    #[must_use]
    pub fn with_rail(mut self) -> Self {
        self.with_rail = true;
        self
    }
    /// The rail's destination captions: the rail grows with the text, up to
    /// its cap, so its longest word shows whole rather than as a fragment
    /// ([`crate::chrome::rail_width_fitting`]). Takes effect at the next
    /// [`Self::scale_for`].
    pub fn rail_labels(&mut self, labels: impl IntoIterator<Item = impl Into<String>>) {
        self.rail_labels = labels.into_iter().map(Into::into).collect();
    }

    // --- dynamic setters (per-frame reconfig) -------------------------------
    //
    // Each keeps the sizing its builder chose (a rail stays a rail).

    pub fn set_docked_width(&mut self, px: f64) {
        self.docked_width = self.docked_width.with_px(px);
    }
    pub fn set_breakpoint(&mut self, px: f64) {
        self.breakpoint = self.breakpoint.with_px(px);
        self.content_min_width = None;
    }

    /// Adopt the theme's scale so the authored breakpoint and the
    /// text-sized widths grow by [`Theme::responsive_scale`], an icon rail by
    /// `ui_scale` (HIG §9.6), and the overlay drawer floats inside the
    /// theme's `s2`. Call it from the host's `build_layout` with the theme it
    /// receives. Never called ⇒ authored geometry.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.responsive_scale = theme.responsive_scale();
        self.ui_scale = f64::from(theme.ui_scale);
        self.float_inset = f64::from(theme.tokens.spacing.s2);
        self.rail_column = if self.rail_labels.is_empty() {
            f64::from(theme.tokens.metrics.rail_width)
        } else {
            crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                crate::chrome::rail_width_fitting(
                    theme,
                    text,
                    self.rail_labels.iter().map(String::as_str),
                )
            })
        };
        let growth = f64::from(theme.text_growth());
        self.resize_min_width = f64::from(theme.tokens.metrics.sidebar_min_width) * growth;
        self.resize_max_width = f64::from(theme.tokens.metrics.sidebar_max_width) * growth;
        self.resize.set_rect(self.outer, theme);
    }

    /// The icon rail's width inside the drawer ([`Self::with_rail`]), or 0.
    fn rail_column_width(&self) -> f64 {
        if self.with_rail {
            // Before `scale_for` the base metric, scaled by the cached UI
            // scale; after it, the theme's metric (already scaled).
            self.rail_column
        } else {
            0.0
        }
    }

    fn resolve_extent(&self, extent: AuthoredExtent) -> f64 {
        extent.resolve_with(self.responsive_scale, self.ui_scale)
    }

    /// The breakpoint as laid out (text-sized): with a content minimum,
    /// the panel at its narrowest beside that minimum.
    pub fn resolved_breakpoint(&self) -> f64 {
        self.content_min_width.map_or_else(
            || self.resolve_extent(self.breakpoint),
            |minimum| self.narrowest_docked_width() + self.resolve_extent(minimum),
        )
    }
    /// The narrowest the docked panel shrinks to before it stops docking:
    /// its minimum width (with the rail), never wider than it is.
    fn narrowest_docked_width(&self) -> f64 {
        let docked = self.resolved_docked_width();
        if self.docked_width.sizing() == crate::widget::layout::ExtentSizing::Icon {
            return docked;
        }
        (self.resize_min_width + self.rail_column_width()).min(docked)
    }
    /// The docked panel's width in a window `extent` wide: its width, shrunk
    /// toward [`Self::narrowest_docked_width`] while the content would get
    /// less than its minimum.
    fn docked_width_in(&self, extent: f64) -> f64 {
        let docked = self.resolved_docked_width();
        match self.content_min_width {
            Some(minimum) => docked
                .min(extent - self.resolve_extent(minimum))
                .max(self.narrowest_docked_width()),
            None => docked,
        }
    }
    /// The docked width as laid out (with the rail, where it has one).
    pub fn resolved_docked_width(&self) -> f64 {
        self.resolve_extent(self.docked_width) + self.rail_column_width()
    }
    /// The overlay width as laid out (text-sized, with the rail).
    pub fn resolved_overlay_width(&self) -> f64 {
        self.resolve_extent(self.overlay_width) + self.rail_column_width()
    }
    /// The collapsed rail width as laid out (icon-sized: `ui_scale` only).
    pub fn resolved_rail_width(&self) -> f64 {
        self.resolve_extent(self.rail_width)
    }

    // --- state --------------------------------------------------------------

    /// Docked at/above the (resolved) breakpoint; always an overlay for
    /// [`Self::overlay_only`].
    pub fn layout_for(&self, width: f64) -> SplitLayout {
        if self.overlay_only {
            return SplitLayout::Overlay;
        }
        SplitLayout::from_width(width, self.resolved_breakpoint())
    }
    pub fn layout(&self) -> SplitLayout {
        self.layout
    }
    pub fn is_overlay(&self) -> bool {
        self.layout == SplitLayout::Overlay
    }
    pub fn is_docked(&self) -> bool {
        self.layout == SplitLayout::Docked
    }
    /// Too narrow to dock, a leading sidebar of an app that said what its
    /// content is takes turns with the content: open, the panel fills the
    /// window under the header row as the sidebar region (no float, no
    /// shadow); closed, the content does. It is still the overlay size
    /// class ([`Self::is_overlay`]): a choice in the list closes it, and
    /// narrowing lands on the content.
    pub fn is_push(&self) -> bool {
        self.is_overlay() && self.pushes()
    }
    fn pushes(&self) -> bool {
        self.push && self.side == SidebarSide::Left && !self.overlay_only
    }
    pub fn is_open(&self) -> bool {
        self.flap.is_open(self.layout)
    }
    /// Flip open/closed and remember the person's choice for this size
    /// class.
    pub fn toggle(&mut self) {
        self.flap.toggle(self.layout);
    }
    /// Force open (remembering the choice).
    pub fn open(&mut self) {
        self.flap.open();
    }
    /// Force closed (scrim / Esc / a choice made in the drawer).
    pub fn close(&mut self) {
        self.flap.close();
    }
    /// Snap to `open` with NO animation — deterministic `--shots` / tests.
    /// Before the first [`Self::resolve`] the snap is kept until the layout
    /// is known, so it lands in the size class the drawer is shown in.
    pub fn snap(&mut self, open: bool) {
        if self.resolved_once {
            self.flap.snap(open);
        } else {
            self.pending_snap = Some(open);
        }
    }
    /// The eased reveal in `0..=1` (scrim strength, docked inset).
    pub fn reveal(&self) -> f64 {
        self.flap.reveal()
    }
    /// `true` while the slide hasn't settled.
    pub fn is_animating(&self) -> bool {
        self.animating || self.flap.is_animating(self.layout)
    }

    // --- geometry -----------------------------------------------------------

    /// The sidebar panel. An overlay drawer floats `s2` inside the outer
    /// rect; its contents lay out inside this rect.
    pub fn sidebar_rect(&self) -> Rect {
        self.sidebar_rect
    }
    /// The usable area beside/under the drawer.
    pub fn content_rect(&self) -> Rect {
        self.content_rect
    }
    /// The content beside an open overlay drawer, where a press dismisses
    /// it (nothing is painted there); `Rect::ZERO` when absent.
    pub fn scrim_rect(&self) -> Rect {
        self.scrim_rect
    }
    /// The icon rail inside the panel ([`Self::with_rail`]): its leading
    /// strip (trailing, for a right drawer), or `Rect::ZERO`.
    pub fn rail_rect(&self) -> Rect {
        let w = self.rail_column_width().min(self.sidebar_rect.width());
        if w <= 0.0 {
            return Rect::ZERO;
        }
        let r = self.sidebar_rect;
        match self.side {
            SidebarSide::Left => Rect::new(r.x0, r.y0, r.x0 + w, r.y1),
            SidebarSide::Right => Rect::new(r.x1 - w, r.y0, r.x1, r.y1),
        }
    }
    /// The text list inside the panel: the panel beside the rail.
    pub fn list_rect(&self) -> Rect {
        let rail = self.rail_rect();
        let r = self.sidebar_rect;
        if rail == Rect::ZERO {
            return r;
        }
        match self.side {
            SidebarSide::Left => Rect::new(rail.x1, r.y0, r.x1, r.y1),
            SidebarSide::Right => Rect::new(r.x0, r.y0, rail.x0, r.y1),
        }
    }

    // --- the header row and the toggle --------------------------------------

    /// The window's header row across the top of `area`, with this drawer's
    /// toggle in it: at the leading corner for a sidebar, the trailing one
    /// for an inspector. A floating drawer then opens under the row, which
    /// stays live. Paint the toggle with [`Self::paint_toggle`] and route
    /// events to it with [`Self::handle_event`].
    pub fn header(&mut self, area: Rect, theme: &Theme) -> crate::chrome::HeaderRowFace {
        self.header_with_history(area, theme, false)
    }

    /// [`Self::header`] with the window's History control in the row's
    /// trailing corner (`history`: the host offered it,
    /// [`crate::widget::HistoryOpener::is_shown`]).
    pub fn header_with_history(
        &mut self,
        area: Rect,
        theme: &Theme,
        history: bool,
    ) -> crate::chrome::HeaderRowFace {
        let face = crate::chrome::HeaderRowFace::new(area, theme);
        let face = match self.side {
            SidebarSide::Left => face.with_toggle().with_history_if(history),
            SidebarSide::Right => face.with_history_if(history).with_trailing(),
        };
        self.toggle.rect = match self.side {
            SidebarSide::Left => face.toggle,
            SidebarSide::Right => face.trailing,
        };
        self.header_bottom = Some(face.row.y1);
        face
    }

    /// The toggle's rect in the header row, or `Rect::ZERO` before
    /// [`Self::header`].
    pub fn toggle_rect(&self) -> Rect {
        self.toggle.rect
    }

    /// Spell the show/hide shortcut for `conventions`.
    pub fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.toggle.conventions = conventions;
    }

    /// What the toggle does now, for its name and tooltip.
    fn toggle_name(&self) -> &'static str {
        match (self.side, self.is_open()) {
            (SidebarSide::Left, true) => "Hide sidebar",
            (SidebarSide::Left, false) => "Show sidebar",
            (SidebarSide::Right, true) => "Hide inspector",
            (SidebarSide::Right, false) => "Show inspector",
        }
    }

    /// Route `event` to the drawer's own parts: the toggle (hover, a press
    /// and release on it), `HostShortcut::Sidebar` for a sidebar, Escape
    /// and a press on the scrim for an open floating drawer. Returns whether
    /// the drawer took the event; call [`Self::resolve`] first so the rects
    /// match the last paint.
    pub fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        if self.resize_available() {
            let hovered = self.resize.is_hovered();
            let moved = self.resize.handle_event(event, ctx);
            self.resize_redraw |= hovered != self.resize.is_hovered();
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            ) && self.resize.is_dragging()
            {
                self.resize.set_focused(true);
            }
            if moved {
                self.adopt_resize();
            }
            if *ctx.handled {
                return true;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let scale = if ctx.scale_factor > 0.0 {
                    ctx.scale_factor
                } else {
                    1.0
                };
                let p = Point::new(position.x / scale, position.y / scale);
                self.toggle.pointer = Some(p);
                self.toggle.hovered =
                    self.toggle.rect.width() > 0.0 && self.toggle.rect.contains(p);
                false
            }
            WindowEvent::CursorLeft { .. } => {
                self.toggle.pointer = None;
                self.toggle.hovered = false;
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(p) = self.toggle.pointer else {
                    return false;
                };
                if self.toggle.rect.width() > 0.0 && self.toggle.rect.contains(p) {
                    self.toggle.pressed = true;
                    return true;
                }
                self.press_dismisses(p)
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let was = std::mem::take(&mut self.toggle.pressed);
                if was
                    && self
                        .toggle
                        .pointer
                        .is_some_and(|p| self.toggle.rect.contains(p))
                {
                    self.toggle();
                }
                was
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let modifiers = ctx.modifiers.state();
                let conventions = self.toggle.conventions;
                if self.side == SidebarSide::Left
                    && conventions.command_modifier_down(modifiers)
                    && let Key::Character(c) = logical_key
                    && crate::platform::HostShortcut::resolve(conventions, c, modifiers.shift_key())
                        == Some(crate::platform::HostShortcut::Sidebar)
                {
                    self.toggle();
                    return true;
                }
                if *logical_key == Key::Named(NamedKey::Escape)
                    && self.is_overlay()
                    && self.is_open()
                {
                    self.close();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Paint the toggle at its place in the header row, with the theme of
    /// the ground it stands on.
    pub fn paint_toggle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.toggle.rect.width() <= 0.0 {
            return;
        }
        self.draw_toggle(
            scene,
            transform,
            text_renderer,
            theme,
            self.toggle.rect,
            self.toggle.hovered,
        );
    }

    /// The toggle's node in the window's semantic tree: a button named for
    /// what it does ("Show sidebar"), with its shortcut. Route an `Activate`
    /// on it back with [`Self::perform_toggle_action`].
    pub fn toggle_accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if self.toggle.rect.width() <= 0.0 {
            return None;
        }
        let mut node = AccessNode::new(Role::Button)
            .with_name(self.toggle_name())
            .with_action_target(self.toggle_target())
            .with_bounds(self.toggle.rect)
            .with_state(AccessState::new().focusable(true).expanded(self.is_open()));
        if self.side == SidebarSide::Left {
            let shortcut = crate::platform::HostShortcut::Sidebar
                .shortcut(self.toggle.conventions)
                .label(self.toggle.conventions);
            node = node.with_description(shortcut);
        }
        Some(node)
    }

    fn toggle_target(&self) -> &'static str {
        match self.side {
            SidebarSide::Left => "sidebar-toggle",
            SidebarSide::Right => "inspector-toggle",
        }
    }

    /// Toggle from the toggle's node (`key_path` from
    /// `Widget::perform_action`); returns whether it did.
    pub fn perform_toggle_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
    ) -> bool {
        let ours = key_path
            .last()
            .is_some_and(|key| *key == crate::a11y::hash_key(&self.toggle_target()));
        if ours && matches!(action, crate::a11y::Action::Activate) {
            self.toggle();
            return true;
        }
        false
    }

    /// Advance the reveal on the standard panel slide
    /// ([`FlapState::advance`]). Call once per draw. Under reduced motion the
    /// reveal lands at once. Returns whether it is still animating.
    pub fn tick(&mut self, theme: &Theme) -> bool {
        self.animating = self
            .flap
            .advance(self.layout, crate::time::now(), theme.motion());
        self.recompute_rects();
        self.animating
    }

    /// Resolve the docked-vs-overlay layout + all rects for `outer`. Cheap and
    /// deterministic (no clock); call from both the draw and event paths.
    /// Returns the content rect.
    pub fn resolve(&mut self, outer: Rect) -> Rect {
        self.outer = outer;
        if !self.resize.is_dragging() {
            let layout = self.layout_for(outer.width());
            // Hysteresis: back to docked only once the window is
            // `layout_hysteresis` past the breakpoint, so a window resized
            // near it does not flicker between the two.
            let band = f64::from(Metrics::BASE.layout_hysteresis) * self.ui_scale;
            let held_overlay = self.resolved_once
                && self.layout == SplitLayout::Overlay
                && layout == SplitLayout::Docked
                && !self.overlay_only
                && outer.width() < self.resolved_breakpoint() + band;
            self.layout = if held_overlay {
                SplitLayout::Overlay
            } else {
                layout
            };
        }
        self.flap.reconcile(self.layout);
        self.resolved_once = true;
        if let Some(open) = self.pending_snap.take() {
            self.flap.snap(open);
        }
        self.recompute_rects();
        self.content_rect
    }

    /// The top of the area a floating drawer and its scrim cover: under the
    /// header row [`Self::header`] laid out, when it lies in `outer`.
    fn overlay_top(&self, o: Rect) -> f64 {
        self.header_bottom
            .filter(|bottom| *bottom > o.y0 && *bottom < o.y1)
            .unwrap_or(o.y0)
    }

    fn recompute_rects(&mut self) {
        let o = self.outer;
        if o.width() <= 0.0 {
            self.sidebar_rect = Rect::ZERO;
            self.content_rect = o;
            self.scrim_rect = Rect::ZERO;
            return;
        }
        match self.layout {
            SplitLayout::Docked => {
                let r = self.flap.reveal();
                let full = self.docked_width_in(o.width()).min(o.width());
                let rail = self.resolved_rail_width().min(full);
                let inset = rail + (full - rail) * r;
                // The content reflows once, at the wide end of the slide
                // (`motion::PanelSlide`): while the sidebar moves it keeps
                // the width it has with the sidebar shut and slides with
                // the sidebar's edge, under the sidebar or the window edge.
                let content_w = crate::motion::PanelSlide::content_width(
                    o.width() - inset,
                    o.width() - rail,
                    self.flap.is_animating(self.layout),
                );
                match self.side {
                    SidebarSide::Left => {
                        self.sidebar_rect = Rect::new(o.x0, o.y0, o.x0 + inset, o.y1);
                        self.content_rect =
                            Rect::new(o.x0 + inset, o.y0, o.x0 + inset + content_w, o.y1);
                    }
                    SidebarSide::Right => {
                        self.sidebar_rect = Rect::new(o.x1 - inset, o.y0, o.x1, o.y1);
                        self.content_rect = Rect::new(o.x0, o.y0, o.x0 + content_w, o.y1);
                    }
                }
                self.scrim_rect = Rect::ZERO;
            }
            SplitLayout::Overlay if self.pushes() => {
                // One column at a time: the list slides in over the whole
                // window under the header row, and the content stands under
                // it at full width. Nothing beside the list to dismiss on.
                self.content_rect = o;
                let w = o.width();
                let top = self.overlay_top(o);
                let x0 = o.x0 - w * (1.0 - self.flap.reveal());
                self.sidebar_rect = Rect::new(x0, top, x0 + w, o.y1);
                self.scrim_rect = Rect::ZERO;
            }
            SplitLayout::Overlay => {
                // Content spans the full width; the drawer floats in over it.
                self.content_rect = o;
                let gap = self.float_inset;
                let slide = self.flap.reveal();
                let w = self
                    .resolved_overlay_width()
                    .min((o.width() - 2.0 * gap).max(0.0));
                let travel = w + gap;
                let top = self.overlay_top(o);
                let (y0, y1) = (top + gap, (o.y1 - gap).max(top + gap));
                self.sidebar_rect = match self.side {
                    SidebarSide::Left => {
                        let x0 = o.x0 + gap - travel * (1.0 - slide);
                        Rect::new(x0, y0, x0 + w, y1)
                    }
                    SidebarSide::Right => {
                        let x1 = o.x1 - gap + travel * (1.0 - slide);
                        Rect::new(x1 - w, y0, x1, y1)
                    }
                };
                self.scrim_rect = if self.flap.is_open(self.layout) || slide > 0.001 {
                    Rect::new(o.x0, top, o.x1, o.y1)
                } else {
                    Rect::ZERO
                };
            }
        }
        self.sync_resize();
    }

    /// Whether the shared divider owns the current pointer drag.
    pub fn is_resizing(&self) -> bool {
        self.resize.is_dragging()
    }

    /// Hover changes request a repaint without consuming pointer motion.
    pub fn take_resize_redraw(&mut self) -> bool {
        std::mem::take(&mut self.resize_redraw)
    }

    /// Follow the painted host's focus without retaining a stale keyboard owner.
    pub fn set_resize_focused(&mut self, focused: bool) {
        self.resize.set_focused(focused);
    }

    fn resize_available(&self) -> bool {
        self.resize.is_dragging()
            || (self.is_docked() && self.is_open() && !self.flap.is_animating(self.layout))
    }

    fn sync_resize(&mut self) {
        if !self.resize_available() || self.resize.is_dragging() || self.outer.width() <= 0.0 {
            return;
        }
        let extent = self.outer.width();
        let maximum = self.resize_max_width + self.rail_column_width();
        let maximum = self.content_min_width.map_or(maximum, |minimum| {
            maximum.min((extent - self.resolve_extent(minimum)).max(0.0))
        });
        let share = self.sidebar_rect.width() / extent;
        let (position, min, max) = match self.side {
            SidebarSide::Left => (share, 0.0, (maximum / extent).min(1.0)),
            SidebarSide::Right => (1.0 - share, (1.0 - maximum / extent).max(0.0), 1.0),
        };
        let mut ratio = SplitRatio::default()
            .with_bounds(crate::num::f32_of_f64(min), crate::num::f32_of_f64(max));
        ratio.set(crate::num::f32_of_f64(position));
        *self.resize.ratio_mut() = ratio;
        let top = self.header_bottom.unwrap_or(self.outer.y0);
        self.resize
            .set_bounds(Rect::new(self.outer.x0, top, self.outer.x1, self.outer.y1));
    }

    fn adopt_resize(&mut self) {
        let (first, second) = self.resize.pane_rects();
        let panel = match self.side {
            SidebarSide::Left => first.width(),
            SidebarSide::Right => second.width(),
        };
        let width = panel - self.rail_column_width();
        let minimum = self.resize_min_width;
        if width < minimum {
            self.flap.snap(false);
        } else {
            self.flap.snap(true);
            let extent_scale = self.resolve_extent(self.docked_width.with_px(1.0));
            self.docked_width = self.docked_width.with_px(
                width.clamp(minimum, self.resize_max_width) / extent_scale.max(f64::EPSILON),
            );
        }
        self.recompute_rects();
    }

    fn resize_target(&self) -> &'static str {
        match self.side {
            SidebarSide::Left => "sidebar-resize",
            SidebarSide::Right => "inspector-resize",
        }
    }

    /// The shared separator's semantic door, available for a docked panel.
    pub fn resize_accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.resize_available().then(|| {
            let key = match self.side {
                SidebarSide::Left => "widget-sidebar-resize",
                SidebarSide::Right => "widget-inspector-resize",
            };
            self.resize
                .accessibility()
                .with_name(crate::i18n::widget_format(key, &[]))
                .with_action_target(self.resize_target())
                .with_hashed_key(self.resize_target())
                .with_bounds(self.resize.hit_rect())
        })
    }

    /// Route the separator's ordinary accessibility/semantic action.
    pub fn perform_resize_action(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        if target != self.resize_target() || !self.resize_available() {
            return false;
        }
        if matches!(action, crate::a11y::Action::Focus) {
            self.resize.set_focused(true);
            return true;
        }
        if self.resize.perform_action(action) {
            self.adopt_resize();
            return true;
        }
        false
    }

    /// The shared resize cursor for a docked sidebar or captured drag.
    pub fn resize_cursor(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.resize_available()
            .then(|| self.resize.cursor_shape(point))
            .flatten()
    }

    /// Paint the existing shared divider using the sidebar's theme ground.
    pub fn paint_resize(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.resize_available() {
            self.resize.paint(
                scene,
                transform,
                theme.region(match self.side {
                    SidebarSide::Left => crate::surface::Region::Sidebar,
                    SidebarSide::Right => crate::surface::Region::Inspector,
                }),
                theme,
            );
        }
    }

    // --- hit-testing --------------------------------------------------------

    /// Whether `p` is over the drawer panel.
    pub fn sidebar_contains(&self, p: Point) -> bool {
        self.sidebar_rect.width() > 0.0 && self.sidebar_rect.contains(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open overlay drawer.
    pub fn scrim_contains(&self, p: Point) -> bool {
        self.is_overlay()
            && self.scrim_rect.width() > 0.0
            && self.scrim_rect.contains(p)
            && !self.sidebar_contains(p)
    }
    /// If `p` dismisses an open overlay drawer (a scrim tap), close it and
    /// report `true`.
    pub fn press_dismisses(&mut self, p: Point) -> bool {
        if self.is_overlay() && self.is_open() && self.scrim_contains(p) {
            self.close();
            true
        } else {
            false
        }
    }

    // --- drawing ------------------------------------------------------------

    /// Paint the panel the drawer's contents stand on and return the theme
    /// to draw them with. Docked, it is the window region
    /// (`Region::Sidebar`, or `Region::Inspector` for a trailing drawer)
    /// with no radius and no shadow, and so is a push screen
    /// ([`Self::is_push`]); as a floating drawer it is the floating
    /// material with the panel radius and the floating shadow.
    pub fn paint_panel(&self, scene: &mut Scene, transform: Affine, theme: &Theme) -> Theme {
        let rect = self.sidebar_rect;
        if self.is_overlay() && !self.is_push() {
            paint_floating_drawer(scene, transform, rect, theme)
        } else {
            let region = match self.side {
                SidebarSide::Left => crate::surface::Region::Sidebar,
                SidebarSide::Right => crate::surface::Region::Inspector,
            };
            crate::surface::region(scene, transform, rect, region, theme);
            theme.in_region(region)
        }
    }

    /// Paint the floating drawer panel at `rect` for a host that lays its
    /// panel out itself (a timeline's bin over the canvas): the floating
    /// material, `radii.panel` and the floating shadow, as an open overlay
    /// drawer. Returns the theme to draw the panel's contents with.
    pub fn paint_panel_in(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        theme: &Theme,
    ) -> Theme {
        paint_floating_drawer(scene, transform, rect, theme)
    }

    /// Paint the show/hide control inside `rect`: the same Plain icon
    /// button `NavShell` draws (`Icon::PanelLeft`, or `PanelRight` for a
    /// trailing drawer), transparent at rest and lifted under the pointer.
    /// Draw it with the theme of the ground it stands on.
    pub fn draw_toggle(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        hovered: bool,
    ) {
        let (icon, name) = match (self.side, self.is_open()) {
            (SidebarSide::Left, true) => (crate::icons::Icon::PanelLeft, "Hide sidebar"),
            (SidebarSide::Left, false) => (crate::icons::Icon::PanelLeft, "Show sidebar"),
            (SidebarSide::Right, true) => (crate::icons::Icon::PanelRight, "Hide inspector"),
            (SidebarSide::Right, false) => (crate::icons::Icon::PanelRight, "Show inspector"),
        };
        let face = crate::widget::ButtonFace::icon_only(rect, icon, name).hovered(hovered);
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &face);
    }
}

/// The floating drawer surface: the floating shadow, the floating material
/// with its edge, at the panel radius. Returns the theme for what stands on
/// it.
pub(crate) fn paint_floating_drawer(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
) -> Theme {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return theme.on_ground(crate::chrome::panel_ground(theme));
    }
    let radius = f64::from(theme.tokens.radii.panel);
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    crate::surface::panel(scene, transform, rect, radius, theme);
    theme.on_ground(crate::chrome::panel_ground(theme))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HANG_BOUND;
    use vello::kurbo::Affine;

    fn wide() -> Rect {
        Rect::new(0.0, 0.0, 1200.0, 800.0)
    }
    fn narrow() -> Rect {
        Rect::new(0.0, 0.0, 600.0, 800.0)
    }

    #[test]
    fn drawer_shares_the_shell_metrics() {
        let mut d = SidebarDrawer::new();
        d.scale_for(&Theme::dark());
        let metrics = Metrics::BASE;
        assert_eq!(d.resolved_docked_width(), f64::from(metrics.sidebar_width));
        assert_eq!(d.resolved_overlay_width(), f64::from(metrics.drawer_width));
        assert_eq!(
            d.resolved_breakpoint(),
            f64::from(metrics.compact_breakpoint)
        );
    }

    #[test]
    fn adaptive_sidebar_resize_keeps_the_content_minimum_and_handle_available() {
        for side in [SidebarSide::Left, SidebarSide::Right] {
            let mut drawer = SidebarDrawer::new()
                .side(side)
                .with_rail()
                .content_min_width(360.0);
            drawer.scale_for(&Theme::dark());
            let area = Rect::new(40.0, 20.0, 800.0, 740.0);
            drawer.resolve(area);
            drawer.snap(true);
            drawer.resolve(area);
            let value = match side {
                SidebarSide::Left => "90%",
                SidebarSide::Right => "10%",
            };
            assert!(drawer.perform_resize_action(
                drawer.resize_target(),
                &crate::a11y::Action::SetValue(value.into()),
            ));
            drawer.resolve(area);
            assert!(drawer.is_docked(), "resize must not turn into an overlay");
            assert_eq!(drawer.content_rect().width(), 360.0);
            assert_eq!(drawer.list_rect().width(), 324.0);
            assert!(drawer.resize_accessibility().is_some());
        }
    }

    #[test]
    fn sidebar_trailing_edge_drag_resizes_and_collapses_the_actual_panel() {
        let mut drawer = SidebarDrawer::new().content_min_width(560.0);
        let theme = Theme::dark();
        drawer.scale_for(&theme);
        let area = Rect::new(40.0, 20.0, 1240.0, 820.0);
        drawer.header(area, &theme);
        drawer.resolve(area);
        drawer.snap(true);
        drawer.resolve(area);
        let edge = drawer.sidebar_rect().x1;
        let y = 200.0;
        let motion = |x| crate::synthetic_input::cursor_moved(x, y);
        route(&mut drawer, &motion(edge));
        assert!(route(&mut drawer, &crate::synthetic_input::mouse_press()));
        route(&mut drawer, &motion(edge + 40.0));
        drawer.resolve(area);
        assert_eq!(drawer.list_rect().width(), 320.0);
        route(&mut drawer, &motion(edge + 400.0));
        drawer.resolve(area);
        assert_eq!(drawer.list_rect().width(), 420.0);
        route(&mut drawer, &motion(area.x0 + 100.0));
        assert!(!drawer.is_open());
        assert!(route(&mut drawer, &crate::synthetic_input::mouse_release()));
        drawer.open();
        drawer.snap(true);
        drawer.resolve(area);
        assert_eq!(drawer.list_rect().width(), 420.0);
    }

    #[test]
    fn sidebar_resize_hides_only_below_the_minimum_and_reopens_at_the_last_usable_width() {
        use winit::event::{ElementState, MouseButton};
        for side in [SidebarSide::Left, SidebarSide::Right] {
            let mut drawer = SidebarDrawer::new().side(side).content_min_width(560.0);
            drawer.scale_for(&Theme::dark());
            let area = wide();
            drawer.resolve(area);
            drawer.snap(true);
            drawer.resolve(area);
            let motion = |width| {
                crate::synthetic_input::cursor_moved(
                    match side {
                        SidebarSide::Left => area.x0 + width,
                        SidebarSide::Right => area.x1 - width,
                    },
                    200.0,
                )
            };
            let button = |state| crate::synthetic_input::mouse_input(state, MouseButton::Left);
            let initial_width = drawer.list_rect().width();
            route(&mut drawer, &motion(initial_width));
            assert!(route(&mut drawer, &button(ElementState::Pressed)));
            route(&mut drawer, &motion(200.0));
            drawer.resolve(area);
            assert!(drawer.is_open(), "the exact minimum stays visible");
            assert_eq!(drawer.list_rect().width(), 200.0);
            route(&mut drawer, &motion(199.0));
            assert!(!drawer.is_open(), "dragging below the minimum hides it");
            assert!(route(&mut drawer, &button(ElementState::Released)));
            drawer.open();
            drawer.snap(true);
            drawer.resolve(area);
            assert_eq!(drawer.list_rect().width(), 200.0);
        }
    }

    #[test]
    fn sidebar_resize_semantics_share_the_pointer_width_and_preserve_scale_and_rail() {
        use crate::a11y::Action;
        for side in [SidebarSide::Left, SidebarSide::Right] {
            let mut drawer = SidebarDrawer::new()
                .side(side)
                .with_rail()
                .content_min_width(560.0);
            let theme = Theme::dark().with_text_scale(2.0);
            drawer.scale_for(&theme);
            let area = Rect::new(0.0, 0.0, 2400.0, 800.0);
            drawer.resolve(area);
            drawer.snap(true);
            drawer.resolve(area);
            let node = drawer
                .resize_accessibility()
                .expect("actual docked separator");
            assert_eq!(node.role, crate::a11y::Role::Separator);
            let target = node.action_target.expect("stable semantic route");
            let value = match side {
                SidebarSide::Left => "30%",
                SidebarSide::Right => "70%",
            };
            assert!(drawer.perform_resize_action(&target, &Action::SetValue(value.into())));
            assert_eq!(drawer.sidebar_rect().width(), 720.0);
            assert_eq!(
                drawer.rail_rect().width(),
                f64::from(theme.tokens.metrics.rail_width)
            );
            let before = drawer.sidebar_rect();
            assert!(!drawer.perform_resize_action(&target, &Action::SetValue("NaN".into())));
            assert_eq!(drawer.sidebar_rect(), before);
            let increase = match side {
                SidebarSide::Left => Action::Increment,
                SidebarSide::Right => Action::Decrement,
            };
            assert!(drawer.perform_resize_action(&target, &increase));
            assert_eq!(drawer.sidebar_rect().width(), 840.0);
            drawer.resolve(Rect::new(0.0, 0.0, 500.0, 800.0));
            assert!(drawer.is_overlay());
            assert!(drawer.resize_accessibility().is_none());
            drawer.resolve(area);
            drawer.snap(true);
            drawer.resolve(area);
            assert_eq!(drawer.sidebar_rect().width(), 840.0);
        }
    }

    #[test]
    fn sidebar_resize_preserves_icon_extent_scaling_when_text_grows_independently() {
        for side in [SidebarSide::Left, SidebarSide::Right] {
            let mut drawer = SidebarDrawer::new()
                .side(side)
                .icon_docked_width(280.0)
                .with_rail();
            let theme = Theme::dark().with_text_scale(2.0);
            drawer.scale_for(&theme);
            let area = Rect::new(0.0, 0.0, 2400.0, 800.0);
            drawer.resolve(area);
            drawer.snap(true);
            drawer.resolve(area);
            let target = drawer
                .resize_accessibility()
                .expect("actual docked icon-sized separator")
                .action_target
                .expect("stable semantic route");
            let value = match side {
                SidebarSide::Left => "30%",
                SidebarSide::Right => "70%",
            };
            assert!(
                drawer.perform_resize_action(&target, &crate::a11y::Action::SetValue(value.into()))
            );
            assert_eq!(drawer.sidebar_rect().width(), 720.0);
            drawer.resolve(area);
            assert_eq!(drawer.sidebar_rect().width(), 720.0);
        }
    }

    // USER REQUIREMENT (2026-10-18): "it darkens out the actual content on
    // the right and i cant have content+sidebar at the same time"
    #[test]
    fn content_minimum_docks_by_the_narrowest_panel_and_shrinks_it_first() {
        let m = Metrics::BASE;
        let content_min = f64::from(m.content_min_width);
        let mut drawer = SidebarDrawer::new()
            .with_rail()
            .content_min_width(content_min);
        drawer.scale_for(&Theme::dark());
        let narrowest = f64::from(m.sidebar_min_width + m.rail_width);
        let breakpoint = narrowest + content_min;
        assert_eq!(drawer.resolved_breakpoint(), breakpoint);
        let at = |drawer: &mut SidebarDrawer, width: f64| {
            drawer.resolve(Rect::new(0.0, 0.0, width, 800.0));
        };
        // Wide: the panel at its width.
        let docked = drawer.resolved_docked_width();
        at(&mut drawer, docked + content_min + 100.0);
        assert!(drawer.is_docked());
        assert_eq!(drawer.sidebar_rect().width(), docked);
        // Narrower: the panel shrinks before the layout changes, and the
        // content keeps its minimum.
        at(&mut drawer, breakpoint + 10.0);
        assert!(drawer.is_docked());
        assert_eq!(drawer.content_rect().width(), content_min);
        assert_eq!(drawer.sidebar_rect().width(), narrowest + 10.0);
        at(&mut drawer, breakpoint - 1.0);
        assert!(drawer.is_overlay());
        assert_eq!(
            drawer.scrim_rect().width(),
            0.0,
            "a closed drawer has no dismiss area"
        );
        // Hysteresis: back to docked only past the band.
        let band = f64::from(m.layout_hysteresis);
        at(&mut drawer, breakpoint + band - 1.0);
        assert!(drawer.is_overlay(), "no flicker near the threshold");
        at(&mut drawer, breakpoint + band);
        assert!(drawer.is_docked());
        drawer.scale_for(&Theme::dark().with_text_scale(2.0));
        assert_eq!(
            drawer.resolved_breakpoint(),
            2.0 * f64::from(m.sidebar_min_width + m.content_min_width) + f64::from(m.rail_width)
        );
        drawer.set_breakpoint(700.0);
        assert_eq!(drawer.resolved_breakpoint(), 1400.0);
    }

    #[test]
    fn a_content_kind_docks_by_its_real_minimum() {
        let m = Metrics::BASE;
        let mut drawer = SidebarDrawer::new().content_kind(crate::widget::ContentKind::Grid);
        drawer.scale_for(&Theme::dark());
        assert_eq!(
            drawer.resolved_breakpoint(),
            f64::from(m.sidebar_min_width + m.grid_min_width)
        );
        drawer.resolve(Rect::new(0.0, 0.0, 600.0, 800.0));
        assert!(drawer.is_docked(), "a media grid keeps its sidebar at 600");
    }

    /// Too narrow to dock, a sidebar of an app that said what its content
    /// is takes turns with the content instead of floating over it: the
    /// list fills the window on the sidebar region, the content comes back
    /// with the toggle. A content-primary app keeps its floating drawer.
    #[test]
    fn too_narrow_to_dock_the_list_and_the_content_take_turns() {
        let theme = Theme::dark();
        let window = Rect::new(0.0, 0.0, 360.0, 800.0);
        let mut drawer = SidebarDrawer::new().content_kind(crate::widget::ContentKind::List);
        drawer.scale_for(&theme);
        drawer.resolve(window);
        assert!(drawer.is_push());
        assert!(!drawer.is_open(), "narrowing lands on the content");
        assert_eq!(drawer.content_rect(), window);
        drawer.snap(true);
        drawer.resolve(window);
        assert_eq!(drawer.sidebar_rect(), window, "the list fills the window");
        assert_eq!(drawer.scrim_rect(), Rect::ZERO, "nothing beside it");
        assert!(!drawer.press_dismisses(Point::new(300.0, 400.0)));
        let local = drawer.paint_panel(&mut Scene::new(), Affine::IDENTITY, &theme);
        assert_eq!(
            local.tokens.colors.surfaces.content,
            theme
                .in_region(crate::surface::Region::Sidebar)
                .tokens
                .colors
                .surfaces
                .content,
            "on the sidebar region, not a floating panel"
        );

        let mut document = SidebarDrawer::new().content_kind(crate::widget::ContentKind::Document);
        document.scale_for(&theme);
        document.snap(true);
        document.resolve(window);
        assert!(document.is_overlay() && !document.is_push());
        assert!(
            document.sidebar_rect().width() < window.width(),
            "a content-primary app's sidebar floats over the content"
        );
    }

    #[test]
    fn sidebar_docks_wide_overlays_narrow_and_mirrors_sides() {
        // Case `wide_docks_and_insets_content_right_side`.
        {
            let mut d = SidebarDrawer::new().right().docked_width(300.0);
            let content = d.resolve(wide());
            assert!(d.is_docked(), "wide width docks");
            assert!(d.is_open(), "docked default is open");
            assert!((content.x0 - wide().x0).abs() < 0.5);
            assert!((wide().x1 - content.x1 - 300.0).abs() < 0.5);
            assert_eq!(d.scrim_rect(), Rect::ZERO, "docked draws no scrim");
            assert!((d.sidebar_rect().x1 - wide().x1).abs() < 0.5);
            assert!((d.sidebar_rect().width() - 300.0).abs() < 0.5);
        }

        // Case `narrow_overlays_closed_by_default_full_width_content`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            let content = d.resolve(narrow());
            assert!(d.is_overlay(), "narrow width overlays");
            assert!(!d.is_open(), "overlay default is closed");
            assert!((content.width() - narrow().width()).abs() < 0.5);
            assert_eq!(d.scrim_rect(), Rect::ZERO);
            assert!(
                d.sidebar_rect().x0 >= narrow().x1 - 0.5,
                "closed right drawer is off-screen to the right"
            );
        }

        // Case `left_side_mirrors_right`.
        {
            let mut d = SidebarDrawer::new().left().docked_width(280.0);
            let content = d.resolve(wide());
            assert!(d.is_docked() && d.is_open());
            assert!((content.x0 - wide().x0 - 280.0).abs() < 0.5);
            assert!((d.sidebar_rect().x0 - wide().x0).abs() < 0.5);
        }

        // Case `rail_leaves_a_thin_strip_when_collapsed_docked`.
        {
            let mut d = SidebarDrawer::new()
                .left()
                .docked_width(280.0)
                .rail_width(56.0)
                .closed();
            let content = d.resolve(wide());
            assert!((content.x0 - wide().x0 - 56.0).abs() < 0.5);
            d.open();
            d.snap(true);
            d.resolve(wide());
            assert!((d.content_rect().x0 - wide().x0 - 280.0).abs() < 0.5);
        }

        // Case `a_hidden_sidebar_stays_hidden_across_a_narrow_window`.
        {
            let mut d = SidebarDrawer::new().right();
            d.resolve(wide());
            d.close();
            d.resolve(wide());
            assert!(!d.is_open(), "explicit collapse holds while docked");
            d.resolve(narrow());
            d.resolve(wide());
            assert!(
                !d.is_open(),
                "the choice is remembered for the wide size class"
            );
        }
    }

    #[test]
    fn overlay_drawer_floats_slides_with_scrim_and_dismisses_on_scrim_only() {
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.s2);
        // Case `toggle_open_overlay_paints_scrim_and_floats_in`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.scale_for(&theme);
            d.resolve(narrow());
            d.toggle();
            d.snap(true);
            d.resolve(narrow());
            assert!(d.is_open());
            assert!(d.scrim_rect().width() > 0.0, "open overlay lays a scrim");
            assert!(
                (d.sidebar_rect().x1 - (narrow().x1 - gap)).abs() < 0.5,
                "an open drawer floats s2 inside the trailing edge"
            );
            assert!((d.sidebar_rect().y0 - gap).abs() < 0.5);
        }

        // Case `scrim_tap_dismisses_overlay_but_a_tap_on_the_drawer_does_not`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.open();
            d.snap(true);
            d.resolve(narrow());
            assert!(!d.press_dismisses(d.sidebar_rect().center()));
            assert!(d.is_open());
            let scrim_pt = Point::new(narrow().x0 + 20.0, 400.0);
            assert!(d.scrim_contains(scrim_pt));
            assert!(d.press_dismisses(scrim_pt));
            assert!(!d.is_open());
        }

        // Case `overlay_flap_slides_across_frames_from_the_tick_path`.
        {
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.snap(false);
            assert_eq!(d.reveal(), 0.0);
            d.open();

            d.tick(&theme);
            assert!(
                d.reveal() < 0.01,
                "the first tick covers only the time since the toggle ({})",
                d.reveal()
            );

            std::thread::sleep(crate::time::Duration::from_millis(16));
            d.tick(&theme);
            let r = d.reveal();
            assert!(r > 0.0 && r < 1.0, "mid-slide after one frame ({r})");
            assert!(d.is_animating());

            let deadline = crate::time::now() + HANG_BOUND;
            while crate::time::now() < deadline {
                std::thread::sleep(crate::time::Duration::from_millis(1));
                d.tick(&theme);
                if !d.is_animating() {
                    break;
                }
            }
            assert!((d.reveal() - 1.0).abs() < 0.01);
        }

        // Case `reduce_motion_snaps_instead_of_sliding`.
        {
            let mut calm = Theme::dark();
            calm.reduce_motion = true;
            let mut d = SidebarDrawer::new().right().overlay_width(320.0);
            d.resolve(narrow());
            d.snap(false);
            d.open();
            d.tick(&calm);
            assert_eq!(d.reveal(), 1.0);
            assert!(!d.is_animating());
        }

        // Case `panel_and_toggle_paint_from_tokens`.
        {
            let mut d = SidebarDrawer::new().overlay_width(320.0);
            d.resolve(narrow());
            d.open();
            d.snap(true);
            d.resolve(narrow());
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let drawer_theme = d.paint_panel(&mut scene, Affine::IDENTITY, &theme);
            assert_eq!(
                drawer_theme.tokens.colors.surfaces.content,
                crate::chrome::panel_ground(&theme),
                "drawer contents stand on the floating material"
            );
            d.draw_toggle(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &drawer_theme,
                Rect::new(12.0, 12.0, 44.0, 44.0),
                true,
            );
            assert!(scene.encoding().draw_tags.len() >= 3);

            let mut docked = SidebarDrawer::new();
            docked.resolve(wide());
            let docked_theme = docked.paint_panel(&mut Scene::new(), Affine::IDENTITY, &theme);
            assert_eq!(
                docked_theme.tokens.colors.surfaces.content,
                theme.region(crate::surface::Region::Sidebar)
            );
        }
    }

    /// Route `event` to the drawer's own parts with a throwaway context.
    fn route(d: &mut SidebarDrawer, event: &winit::event::WindowEvent) -> bool {
        route_with(d, event, winit::event::Modifiers::default())
    }

    fn route_with(
        d: &mut SidebarDrawer,
        event: &winit::event::WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let mut taffy = taffy::TaffyTree::<()>::new();
        let node = taffy
            .new_leaf(taffy::Style::default())
            .expect("a leaf node");
        let mut doc = automerge::AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        d.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: crate::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        )
    }

    #[test]
    fn a_painted_header_row_owns_the_toggle_and_the_drawer_opens_under_it() {
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut d = SidebarDrawer::new();
        d.scale_for(&theme);
        d.set_platform_conventions(crate::platform::PlatformConventions::new(
            crate::platform::Platform::Linux,
        ));
        let face = d.header_with_history(narrow(), &theme, true);
        assert_eq!(d.toggle_rect(), face.toggle);
        assert!(face.toggle.x0 < face.history.x0);
        d.resolve(narrow());
        assert!(!d.is_open());
        let node = d.toggle_accessibility().expect("a node");
        assert_eq!(node.name.as_deref(), Some("Show sidebar"));

        // A click on the toggle opens the drawer, under the header row.
        let p = face.toggle.center();
        let device = DeviceId::dummy();
        route(
            &mut d,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
        );
        for state in [ElementState::Pressed, ElementState::Released] {
            assert!(route(
                &mut d,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state,
                    button: MouseButton::Left,
                },
            ));
        }
        assert!(d.is_open());
        d.snap(true);
        d.resolve(narrow());
        assert!((d.sidebar_rect().y0 - (face.row.y1 + gap)).abs() < 0.5);
        assert!(
            (d.scrim_rect().y0 - face.row.y1).abs() < 0.5,
            "the row stays live"
        );

        // The system shortcut closes it, as in every app.
        let key = crate::synthetic_input::key_input(
            &winit::keyboard::Key::Character("L".into()),
            ElementState::Pressed,
        );
        let modifiers = winit::event::Modifiers::from(
            winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT,
        );
        assert!(route_with(&mut d, &key, modifiers));
        assert!(!d.is_open());
        assert!(
            d.perform_toggle_action(&crate::a11y::Action::Activate, &[node.key.expect("keyed")])
        );
        assert!(d.is_open());
    }

    #[test]
    fn overlay_only_rail_and_an_early_snap() {
        let theme = Theme::dark();
        let mut d = SidebarDrawer::new().overlay_only();
        d.resolve(wide());
        assert!(d.is_overlay(), "never docks");

        // A snap before the first resolve lands in the layout shown.
        let mut d = SidebarDrawer::new();
        d.snap(true);
        d.resolve(narrow());
        assert!(d.is_open(), "the drawer opens over a narrow window");
        d.resolve(wide());
        assert!(d.is_open(), "the docked default was not overwritten");

        let mut d = SidebarDrawer::new().with_rail();
        d.scale_for(&theme);
        d.resolve(wide());
        let rail = d.rail_rect();
        assert!((rail.width() - f64::from(theme.tokens.metrics.rail_width)).abs() < 0.5);
        // From the coordinator (2026-10-19): the drawer's rail follows the
        // shared rail rule. At double text it grows to keep "Channels" whole,
        // up to its cap; at stock text it keeps its width.
        let big = Theme::high_contrast().with_text_scale(2.0);
        let mut grown = SidebarDrawer::new().with_rail();
        grown.rail_labels(["Chat", "Channels"]);
        grown.scale_for(&big);
        let width = grown.rail_column_width();
        assert!(width > f64::from(big.tokens.metrics.rail_width), "{width}");
        assert!(
            width <= f64::from(big.tokens.metrics.rail_max_width),
            "{width}"
        );
        let mut stock = SidebarDrawer::new().with_rail();
        stock.rail_labels(["Channels"]);
        stock.scale_for(&theme);
        assert!(
            (stock.rail_column_width() - f64::from(theme.tokens.metrics.rail_width)).abs() < 0.5
        );
        assert_eq!(rail.x0, d.sidebar_rect().x0);
        assert_eq!(d.list_rect().x0, rail.x1);
        assert!(
            (d.list_rect().width() - f64::from(theme.tokens.metrics.sidebar_width)).abs() < 0.5,
            "the list keeps the sidebar's width beside the rail"
        );

        let mut scene = Scene::new();
        let local = SidebarDrawer::paint_panel_in(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(10.0, 70.0, 330.0, 700.0),
            &theme,
        );
        assert_eq!(
            local.tokens.colors.surfaces.content,
            crate::chrome::panel_ground(&theme)
        );
        assert!(scene.encoding().draw_tags.len() >= 2);
    }

    #[test]
    fn scale_for_grows_text_sized_extents_and_leaves_the_icon_rail() {
        let authored = || {
            SidebarDrawer::new()
                .breakpoint(760.0)
                .docked_width(240.0)
                .overlay_width(320.0)
                .rail_width(56.0)
        };
        let mut plain = authored();
        plain.resolve(Rect::new(0.0, 0.0, 1000.0, 800.0));
        assert_eq!(plain.layout(), SplitLayout::Docked);
        assert_eq!(plain.sidebar_rect().width(), 240.0);
        assert_eq!(
            (
                plain.resolved_breakpoint(),
                plain.resolved_overlay_width(),
                plain.resolved_rail_width()
            ),
            (760.0, 320.0, 56.0)
        );

        let mut large = authored();
        large.scale_for(&Theme::dark().with_text_scale(2.0));
        assert_eq!(
            (
                large.resolved_breakpoint(),
                large.resolved_docked_width(),
                large.resolved_overlay_width()
            ),
            (1520.0, 480.0, 640.0)
        );
        assert_eq!(large.resolved_rail_width(), 56.0);
        large.resolve(Rect::new(0.0, 0.0, 1000.0, 800.0));
        assert_eq!(large.layout(), SplitLayout::Overlay);

        let mut smaller = authored();
        smaller.scale_for(&Theme::dark().with_text_scale(0.75));
        assert_eq!(smaller.resolved_docked_width(), 240.0);

        let mut dense = authored();
        dense.scale_for(&Theme::dark().with_ui_scale(1.5));
        assert!((dense.resolved_rail_width() - 84.0).abs() < 1e-9);
        assert!((dense.resolved_breakpoint() - 1140.0).abs() < 1e-9);

        let mut glyphs = SidebarDrawer::new()
            .icon_docked_width(56.0)
            .overlay_width(288.0);
        glyphs.scale_for(&Theme::dark().with_text_scale(2.0));
        assert_eq!(glyphs.resolved_docked_width(), 56.0);
        assert_eq!(glyphs.resolved_overlay_width(), 576.0);
        glyphs.set_docked_width(64.0);
        assert_eq!(glyphs.resolved_docked_width(), 64.0);
    }
}
