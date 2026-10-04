//! `NavShell` — the one window shell.
//!
//! Every app with navigation is a `NavShell`: an optional sidebar of
//! destinations (HIG §1a), an optional overview list beside the content
//! (§1b), the content, and an optional inspector at the trailing edge. The
//! app hands it the columns and their headers; the shell owns everything
//! else, at the shell metrics (`theme.tokens.metrics`):
//!
//! - **One header row** across the top of the window holds each column's
//!   header (title and actions), so the row reads as one unified toolbar.
//!   Windows have no title bar; this row is the window's top edge.
//! - **The sidebar toggle** is a Plain icon button (`Icon::PanelLeft`) the
//!   shell draws at the top-leading corner of the sidebar; with the sidebar
//!   hidden it moves to the leading edge of the next column's header. One
//!   shortcut toggles it in every app (`HostShortcut::Sidebar`), and the
//!   button publishes the action ("Show sidebar" / "Hide sidebar") to the
//!   palette and to agents.
//! - **History** is the window's: where the app has History, the host puts
//!   its control (`Icon::History`, a Plain icon button named "History") at
//!   the trailing edge of the row, and it reads as on while History shows.
//!   The app adds no History button of its own.
//! - **Responsive:** below the width where the content would be squeezed
//!   the sidebar collapses into a floating drawer over the content (or, in
//!   [`NavMode::Push`], becomes the first screen of a push stack); the
//!   overview and the content become push screens with a Back button when
//!   they no longer fit side by side; the inspector becomes a drawer from
//!   the trailing edge. A person's show/hide choice is remembered per window
//!   size class.
//! - **Resizable:** dragging the sidebar's trailing edge resizes it;
//!   dragging it below its minimum width hides it.
//! - **Regions:** the sidebar stands on `Region::Sidebar`, the inspector on
//!   `Region::Inspector`, the overview and content on the window ground;
//!   each column's children draw with that region's theme.
//! - **Motion:** hide, show and push move with `motion.durations.base`;
//!   under reduced motion they cross-fade.
//!
//! There is no width, breakpoint, colour or padding to pass: the shell is
//! semantic. A filter field for the sidebar's list goes in
//! [`NavShell::with_filter`] (Ctrl+F focuses it).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::LocalState;
use crate::icons::Icon;
use crate::local_state::LocalBinding;
use crate::platform::{HostShortcut, PlatformConventions};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::TextRole;
use crate::widget::flap::{FlapState, SplitLayout};
use crate::widget::manual::{ManualContext, ManualInvalidation, ManualPolicy, MeasureConstraint};
use crate::widget::window_header::{HistoryControl, WindowHeader};
use crate::widget::{
    AppModel, Button, Element, ModelPath, TaffyTreeExt, Widget, WidgetLayoutError,
};

/// Caps the per-frame dt so a stale clock (first frame, or a parked event
/// loop) can't teleport a slide past its animation.
const MAX_FRAME_DT: f64 = 0.1;

/// What the sidebar becomes when the window is too narrow to dock it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavMode {
    /// A floating drawer over the content, summoned by the toggle — a
    /// sidebar of destinations (mail, files, music).
    #[default]
    Overlay,
    /// The first screen of a push stack: choosing a destination pushes the
    /// content with a Back button (settings, a list of categories).
    Push,
    /// Always docked: a compact icon rail (`NavList::rail`) that fits beside
    /// the content at every width.
    Docked,
}

/// A screen of the narrow push stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavPane {
    /// The sidebar, as the first screen in [`NavMode::Push`].
    Sidebar,
    /// The overview list.
    Overview,
    /// The content.
    #[default]
    Content,
}

/// Where the sidebar is this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarPlacement {
    /// No sidebar.
    #[default]
    None,
    /// Beside the content (possibly hidden by the person).
    Docked,
    /// A floating drawer over the content.
    Drawer,
    /// The first screen of the push stack.
    Screen,
}

/// Where the inspector is this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectorPlacement {
    #[default]
    None,
    Docked,
    Drawer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Column {
    Sidebar,
    Overview,
    Content,
    Inspector,
}

impl Column {
    const ALL: [Self; 4] = [
        Self::Sidebar,
        Self::Overview,
        Self::Content,
        Self::Inspector,
    ];

    fn index(self) -> usize {
        match self {
            Self::Sidebar => 0,
            Self::Overview => 1,
            Self::Content => 2,
            Self::Inspector => 3,
        }
    }
}

/// One retained child of the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    Body(Column),
    Actions(Column),
    Filter,
    Toggle,
    Back,
    InspectorToggle,
    /// The window's History control; `true` is the one shown while History
    /// is open.
    History(bool),
}

/// A column's header: a title the shell paints and actions the app owns.
#[derive(Default)]
struct Header {
    title: Option<String>,
    actions: Option<Element>,
}

/// The shell metrics resolved against the theme the tree was built with.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    header: f64,
    chip: f64,
    inset: f64,
    gap: f64,
    sidebar: f64,
    sidebar_min: f64,
    sidebar_max: f64,
    rail: f64,
    overview: f64,
    inspector: f64,
    drawer: f64,
    content_min: f64,
    detail_min: f64,
    filter: f64,
    resize_grip: f64,
    scale: f64,
}

impl Metrics {
    fn resolve(theme: &Theme) -> Self {
        let m = theme.tokens.metrics;
        let s = theme.tokens.spacing;
        let growth = f64::from(theme.text_growth());
        let chip = crate::widget::text_control_height(
            f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            theme,
        );
        let inset = f64::from(s.s3);
        Self {
            header: f64::from(m.header_height).max(chip + 2.0 * inset),
            chip,
            inset,
            gap: f64::from(s.s2),
            sidebar: f64::from(m.sidebar_width) * growth,
            sidebar_min: f64::from(m.sidebar_min_width) * growth,
            sidebar_max: f64::from(m.sidebar_max_width) * growth,
            rail: f64::from(m.rail_width),
            overview: f64::from(m.overview_width) * growth,
            inspector: f64::from(m.inspector_width) * growth,
            drawer: f64::from(m.drawer_width) * growth,
            content_min: f64::from(m.content_min_width) * growth,
            detail_min: f64::from(m.detail_min_width) * growth,
            filter: crate::widget::text_control_height(
                f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
                theme,
            ),
            resize_grip: f64::from(s.s1),
            scale: theme.responsive_scale(),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::resolve(&Theme::dark())
    }
}

/// Everything the geometry depends on. Shared with the layout policy the
/// host runs, so the policy and the shell compute the same rects.
#[derive(Clone, Debug)]
struct Inputs {
    mode: NavMode,
    metrics: Metrics,
    has_sidebar: bool,
    has_overview: bool,
    has_inspector: bool,
    has_filter: bool,
    /// The window's History control, where the host gave the shell one.
    history: Option<HistoryControl>,
    titled: [bool; 4],
    with_actions: [bool; 4],
    /// The docked sidebar width a person dragged to, authored at 1×.
    user_sidebar: Option<f64>,
    sidebar: FlapState,
    inspector: FlapState,
    pane: NavPane,
    /// Eased position in the push stack: the front screen's index while
    /// settled, fractional while sliding.
    push_pos: Option<f64>,
    push_from: Option<f64>,
    push_progress: f64,
    /// The parts in the order they were built (their placement index).
    parts: Vec<Part>,
}

impl Default for Inputs {
    fn default() -> Self {
        Self {
            mode: NavMode::Overlay,
            metrics: Metrics::default(),
            has_sidebar: false,
            has_overview: false,
            has_inspector: false,
            has_filter: false,
            history: None,
            titled: [false; 4],
            with_actions: [false; 4],
            user_sidebar: None,
            sidebar: FlapState::open_when_docked(),
            inspector: FlapState::open_when_docked(),
            pane: NavPane::Content,
            push_pos: None,
            push_from: None,
            push_progress: 1.0,
            parts: Vec::new(),
        }
    }
}

impl Inputs {
    fn sidebar_width(&self) -> f64 {
        let m = &self.metrics;
        if self.mode == NavMode::Docked {
            return m.rail;
        }
        self.user_sidebar
            .map_or(m.sidebar, |authored| authored * m.scale)
            .clamp(m.sidebar_min, m.sidebar_max)
    }
}

/// How a part is placed by the layout policy.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// Exactly this rect.
    Exact(Rect),
    /// Its intrinsic size, at the trailing end of this band, centred.
    Trailing(Rect),
}

/// The shell's geometry for one size, in coordinates local to the shell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavGeometry {
    pub sidebar: SidebarPlacement,
    pub inspector: InspectorPlacement,
    /// The overview and content stand side by side.
    pub side_by_side: bool,
    /// The push stack's screens, first to last, when it has more than one.
    pub stack: Vec<NavPane>,
    pub header_height: f64,
    /// Width at or above which the sidebar docks.
    pub breakpoint: f64,
    /// Each column's visible area (header and body); a drawer's panel.
    pub sidebar_column: Rect,
    pub overview_column: Rect,
    pub content_column: Rect,
    pub inspector_column: Rect,
    bodies: [Rect; 4],
    headers: [Rect; 4],
    pub filter: Rect,
    pub toggle: Rect,
    pub back: Rect,
    pub inspector_toggle: Rect,
    /// The window's History control, at the trailing edge of the row.
    pub history: Rect,
    /// The column whose header holds the sidebar toggle and the Back button.
    toggle_column: Option<Column>,
    back_column: Option<Column>,
    inspector_toggle_column: Option<Column>,
    history_column: Option<Column>,
    pub scrim: Rect,
    pub scrim_strength: f32,
    pub resize_handle: Rect,
}

impl NavGeometry {
    fn column(&self, column: Column) -> Rect {
        match column {
            Column::Sidebar => self.sidebar_column,
            Column::Overview => self.overview_column,
            Column::Content => self.content_column,
            Column::Inspector => self.inspector_column,
        }
    }

    fn body(&self, column: Column) -> Rect {
        self.bodies
            .get(column.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn header(&self, column: Column) -> Rect {
        self.headers
            .get(column.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn translated(&self, by: Vec2) -> Self {
        let t = |r: Rect| if r == Rect::ZERO { r } else { r + by };
        Self {
            sidebar_column: t(self.sidebar_column),
            overview_column: t(self.overview_column),
            content_column: t(self.content_column),
            inspector_column: t(self.inspector_column),
            bodies: self.bodies.map(t),
            headers: self.headers.map(t),
            filter: t(self.filter),
            toggle: t(self.toggle),
            back: t(self.back),
            inspector_toggle: t(self.inspector_toggle),
            history: t(self.history),
            scrim: t(self.scrim),
            resize_handle: t(self.resize_handle),
            ..self.clone()
        }
    }

    /// The parts this geometry shows, in build order (back to front).
    fn visible_parts(&self, inputs: &Inputs) -> Vec<Part> {
        let mut parts = Vec::new();
        let shows = |r: Rect| r.width() > 0.0 && r.height() > 0.0;
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            let present = match column {
                Column::Sidebar => inputs.has_sidebar,
                Column::Overview => inputs.has_overview,
                Column::Content => true,
                Column::Inspector => inputs.has_inspector,
            };
            if !present || !shows(self.column(column)) {
                continue;
            }
            if self.modal() && !self.in_drawer(column) {
                continue;
            }
            parts.push(Part::Body(column));
            if inputs
                .with_actions
                .get(column.index())
                .copied()
                .unwrap_or(false)
                && shows(self.header(column))
            {
                parts.push(Part::Actions(column));
            }
            if inputs.has_filter && self.filter_column(inputs) == Some(column) && shows(self.filter)
            {
                parts.push(Part::Filter);
            }
        }
        if shows(self.back) {
            parts.push(Part::Back);
        }
        if shows(self.inspector_toggle) && (!self.modal() || self.inspector_modal()) {
            parts.push(Part::InspectorToggle);
        }
        if let Some(control) = inputs.history
            && shows(self.history)
            && !self.modal()
        {
            parts.push(Part::History(control.is_open()));
        }
        if shows(self.toggle) && (!self.modal() || self.sidebar_modal()) {
            parts.push(Part::Toggle);
        }
        parts
    }

    fn sidebar_modal(&self) -> bool {
        self.sidebar == SidebarPlacement::Drawer && self.sidebar_column.width() > 0.0
    }

    fn inspector_modal(&self) -> bool {
        self.inspector == InspectorPlacement::Drawer && self.inspector_column.width() > 0.0
    }

    /// An open drawer is modal: only it takes input and is exposed.
    fn modal(&self) -> bool {
        self.sidebar_modal() || self.inspector_modal()
    }

    fn in_drawer(&self, column: Column) -> bool {
        match column {
            Column::Sidebar => self.sidebar_modal(),
            Column::Inspector => self.inspector_modal(),
            Column::Overview | Column::Content => false,
        }
    }

    fn filter_column(&self, inputs: &Inputs) -> Option<Column> {
        if inputs.has_sidebar {
            Some(Column::Sidebar)
        } else if inputs.has_overview {
            Some(Column::Overview)
        } else {
            Some(Column::Content)
        }
    }

    fn placement(&self, part: Part) -> Placement {
        match part {
            Part::Body(column) => Placement::Exact(self.body(column)),
            Part::Actions(column) => Placement::Trailing(self.actions_band(column)),
            Part::Filter => Placement::Exact(self.filter),
            Part::Toggle => Placement::Exact(self.toggle),
            Part::Back => Placement::Exact(self.back),
            Part::InspectorToggle => Placement::Exact(self.inspector_toggle),
            Part::History(_) => Placement::Exact(self.history),
        }
    }

    /// The run of a header the app's actions may take: from after the title
    /// lead to before the trailing shell buttons.
    fn actions_band(&self, column: Column) -> Rect {
        let header = self.header(column);
        if header == Rect::ZERO {
            return Rect::ZERO;
        }
        let mut x1 = header.x1;
        for (button, home) in [
            (self.inspector_toggle, self.inspector_toggle_column),
            (self.history, self.history_column),
        ] {
            if home == Some(column) && button.width() > 0.0 {
                x1 = x1.min(button.x0);
            }
        }
        Rect::new(header.x0, header.y0, x1, header.y1)
    }
}

/// Compute the geometry for a shell of size `w × h`.
#[allow(
    clippy::too_many_lines,
    reason = "the responsive plan is one ordered decision: sidebar, inspector, split, stack, then rects"
)]
fn plan(inputs: &Inputs, w: f64, h: f64) -> NavGeometry {
    let m = inputs.metrics;
    let mut g = NavGeometry::default();
    if w <= 0.0 || h <= 0.0 {
        return g;
    }
    let sw = inputs.sidebar_width().min(w);
    let ow = if inputs.has_overview { m.overview } else { 0.0 };
    let iw = if inputs.has_inspector {
        m.inspector
    } else {
        0.0
    };
    let content_need = if inputs.has_overview {
        ow + m.detail_min
    } else {
        m.content_min
    };

    // 1. The sidebar docks while the content beside it keeps its minimum.
    g.breakpoint = if inputs.has_sidebar && inputs.mode != NavMode::Docked {
        sw + content_need
    } else {
        0.0
    };
    g.sidebar = if !inputs.has_sidebar {
        SidebarPlacement::None
    } else if inputs.mode == NavMode::Docked || w >= g.breakpoint {
        SidebarPlacement::Docked
    } else if inputs.mode == NavMode::Push {
        SidebarPlacement::Screen
    } else {
        SidebarPlacement::Drawer
    };
    let sidebar_layout = layout_of(g.sidebar);
    let sidebar_open = inputs.sidebar.is_open(sidebar_layout);
    let sidebar_reveal = inputs.sidebar.reveal_in(sidebar_layout);
    let docked_target = if g.sidebar == SidebarPlacement::Docked && sidebar_open {
        sw
    } else {
        0.0
    };
    let docked_now = if g.sidebar == SidebarPlacement::Docked {
        sw * sidebar_reveal
    } else {
        0.0
    };

    // 2. The inspector docks while the content keeps its minimum beside it.
    g.inspector = if !inputs.has_inspector {
        InspectorPlacement::None
    } else if w - docked_target >= content_need + iw {
        InspectorPlacement::Docked
    } else {
        InspectorPlacement::Drawer
    };
    let inspector_layout = match g.inspector {
        InspectorPlacement::Drawer => SplitLayout::Overlay,
        InspectorPlacement::Docked | InspectorPlacement::None => SplitLayout::Docked,
    };
    let inspector_open = inputs.inspector.is_open(inspector_layout);
    let inspector_reveal = inputs.inspector.reveal_in(inspector_layout);
    let inspector_target = if g.inspector == InspectorPlacement::Docked && inspector_open {
        iw
    } else {
        0.0
    };
    let inspector_now = if g.inspector == InspectorPlacement::Docked {
        iw * inspector_reveal
    } else {
        0.0
    };

    // 3. Overview and content side by side while both fit.
    let region_target = w - docked_target - inspector_target;
    g.side_by_side = !inputs.has_overview || region_target >= ow + m.detail_min;

    // 4. The push stack.
    let mut stack = Vec::new();
    if g.sidebar == SidebarPlacement::Screen {
        stack.push(NavPane::Sidebar);
    }
    if inputs.has_overview && !g.side_by_side {
        stack.push(NavPane::Overview);
    }
    stack.push(NavPane::Content);
    let front = stack_index(&stack, inputs.pane);
    let pos = inputs.push_pos.map_or(index_f64(front), |pos| {
        pos.clamp(0.0, index_f64(stack.len().saturating_sub(1)))
    });
    g.stack = if stack.len() > 1 {
        stack.clone()
    } else {
        Vec::new()
    };

    // 5. The header row.
    let needs_header = inputs.has_sidebar
        || inputs.has_inspector
        || inputs.history.is_some()
        || inputs.titled.iter().any(|t| *t)
        || inputs.with_actions.iter().any(|a| *a)
        || stack.len() > 1;
    g.header_height = if needs_header { m.header.min(h) } else { 0.0 };
    let hh = g.header_height;
    let header_of = |col: Rect| {
        if hh > 0.0 && col.width() > 0.0 {
            Rect::new(col.x0, col.y0, col.x1, col.y0 + hh)
        } else {
            Rect::ZERO
        }
    };

    // 6. Columns.
    let region = Rect::new(docked_now, 0.0, (w - inspector_now).max(docked_now), h);
    let region_w = region.width();
    let screen_x = |pane: NavPane| -> f64 {
        let i = stack_index(&stack, pane);
        (index_f64(i) - pos) * w
    };
    // Sidebar.
    let mut sidebar_body = Rect::ZERO;
    match g.sidebar {
        SidebarPlacement::None => {}
        SidebarPlacement::Docked => {
            if docked_now > 0.0 {
                g.sidebar_column = Rect::new(0.0, 0.0, docked_now, h);
                // The body keeps its width and slides under the edge, so its
                // rows never reflow while it collapses.
                sidebar_body = Rect::new(docked_now - sw, hh, docked_now, h);
            }
        }
        SidebarPlacement::Drawer => {
            if sidebar_reveal > 0.001 {
                let dw = m.drawer.min((w - 2.0 * m.gap).max(0.0));
                let x0 = m.gap - (dw + m.gap) * (1.0 - sidebar_reveal);
                g.sidebar_column = Rect::new(x0, m.gap, x0 + dw, (h - m.gap).max(m.gap));
                sidebar_body = Rect::new(
                    g.sidebar_column.x0,
                    hh.max(g.sidebar_column.y0),
                    g.sidebar_column.x1,
                    g.sidebar_column.y1,
                );
                g.scrim = Rect::new(0.0, 0.0, w, h);
                g.scrim_strength =
                    crate::widget::sidebar::DRAWER_SCRIM_STRENGTH * unit_f32(sidebar_reveal);
            }
        }
        SidebarPlacement::Screen => {
            let x = screen_x(NavPane::Sidebar);
            if x > -w && x < w {
                g.sidebar_column = Rect::new(x, 0.0, x + w, h);
                sidebar_body = Rect::new(x, hh, x + w, h);
            }
        }
    }
    // Overview and content.
    if g.side_by_side {
        let x = region.x0 + screen_x(NavPane::Content);
        let ow_now = if inputs.has_overview {
            ow.min((region_w - m.detail_min).max(0.0))
        } else {
            0.0
        };
        if inputs.has_overview {
            g.overview_column = Rect::new(x, 0.0, x + ow_now, h);
        }
        g.content_column = Rect::new(x + ow_now, 0.0, x + region_w, h);
    } else {
        let ox = region.x0 + screen_x(NavPane::Overview);
        let cx = region.x0 + screen_x(NavPane::Content);
        if ox > -w && ox < w {
            g.overview_column = Rect::new(ox, 0.0, ox + region_w, h);
        }
        if cx > -w && cx < w {
            g.content_column = Rect::new(cx, 0.0, cx + region_w, h);
        }
    }
    // Inspector.
    let mut inspector_body = Rect::ZERO;
    match g.inspector {
        InspectorPlacement::None => {}
        InspectorPlacement::Docked => {
            if inspector_now > 0.0 {
                g.inspector_column = Rect::new(w - inspector_now, 0.0, w, h);
                inspector_body = Rect::new(w - inspector_now, hh, w - inspector_now + iw, h);
            }
        }
        InspectorPlacement::Drawer => {
            if inspector_reveal > 0.001 {
                let dw = iw.min((w - 2.0 * m.gap).max(0.0));
                let x1 = w - m.gap + (dw + m.gap) * (1.0 - inspector_reveal);
                g.inspector_column = Rect::new(x1 - dw, m.gap, x1, (h - m.gap).max(m.gap));
                inspector_body = Rect::new(
                    g.inspector_column.x0,
                    hh.max(g.inspector_column.y0),
                    g.inspector_column.x1,
                    g.inspector_column.y1,
                );
                if g.scrim == Rect::ZERO {
                    g.scrim = Rect::new(0.0, 0.0, w, h);
                    g.scrim_strength =
                        crate::widget::sidebar::DRAWER_SCRIM_STRENGTH * unit_f32(inspector_reveal);
                }
            }
        }
    }

    // 7. Headers and bodies.
    g.headers = [
        drawer_header(&g.sidebar_column, hh, g.sidebar == SidebarPlacement::Drawer),
        header_of(g.overview_column),
        header_of(g.content_column),
        drawer_header(
            &g.inspector_column,
            hh,
            g.inspector == InspectorPlacement::Drawer,
        ),
    ];
    let body_of = |col: Rect| {
        if col.width() > 0.0 {
            Rect::new(col.x0, col.y0 + hh, col.x1, col.y1)
        } else {
            Rect::ZERO
        }
    };
    g.bodies = [
        sidebar_body,
        body_of(g.overview_column),
        body_of(g.content_column),
        inspector_body,
    ];

    // 8. The filter field sits under its column's header.
    if inputs.has_filter
        && let Some(column) = g.filter_column(inputs)
    {
        let col = g.column(column);
        let body = g.body(column);
        if col.width() > 0.0 && body.height() > 0.0 {
            let top = body.y0;
            g.filter = Rect::new(
                body.x0 + m.inset,
                top,
                (body.x1 - m.inset).max(body.x0 + m.inset),
                top + m.filter,
            );
            if let Some(slot) = g.bodies.get_mut(column.index()) {
                slot.y0 = (top + m.filter + m.gap).min(slot.y1);
            }
        }
    }

    // 9. The shell's own buttons.
    let chip_in = |col: Rect, header: Rect, leading: bool| {
        if header == Rect::ZERO {
            return Rect::ZERO;
        }
        let y = header.y0 + (header.height() - m.chip) / 2.0;
        let x = if leading {
            col.x0.max(header.x0) + m.inset
        } else {
            header.x1 - m.inset - m.chip
        };
        Rect::new(x, y, x + m.chip, y + m.chip)
    };
    let front_pane = stack.get(front).copied().unwrap_or(NavPane::Content);
    let front_column = match front_pane {
        NavPane::Sidebar => Column::Sidebar,
        NavPane::Overview => Column::Overview,
        NavPane::Content if inputs.has_overview && g.side_by_side => Column::Overview,
        NavPane::Content => Column::Content,
    };
    if stack.len() > 1 && front > 0 {
        g.back_column = Some(front_column);
        g.back = chip_in(g.column(front_column), g.header(front_column), true);
    } else if inputs.has_sidebar && g.sidebar == SidebarPlacement::Docked {
        // Pinned to the window's top-leading corner, on the sidebar while it
        // shows and on the next column while it is hidden, so it never
        // chases a sliding edge.
        g.toggle = chip_in(Rect::new(0.0, 0.0, w, h), Rect::new(0.0, 0.0, w, hh), true);
        g.toggle_column = Some(if g.sidebar_column.x1 >= g.toggle.x1 {
            Column::Sidebar
        } else {
            front_column
        });
    } else if inputs.has_sidebar && g.sidebar == SidebarPlacement::Drawer {
        let column = if sidebar_open {
            Column::Sidebar
        } else {
            front_column
        };
        g.toggle_column = Some(column);
        g.toggle = chip_in(g.column(column), g.header(column), true);
    }
    // History stands in the window's top-trailing corner: the trailing edge
    // of the column that reaches the window's trailing edge (never a drawer).
    if inputs.history.is_some()
        && let Some(column) = [
            Column::Inspector,
            Column::Content,
            Column::Overview,
            Column::Sidebar,
        ]
        .into_iter()
        .find(|column| {
            let header = g.header(*column);
            !g.in_drawer(*column)
                && g.column(*column).width() > 0.0
                && header.x0 < w - 0.5
                && header.x1 >= w - 0.5
        })
    {
        g.history_column = Some(column);
        g.history = chip_in(g.column(column), g.header(column), false);
    }
    if inputs.has_inspector {
        let column = match g.inspector {
            InspectorPlacement::Docked | InspectorPlacement::Drawer if inspector_open => {
                Column::Inspector
            }
            _ => Column::Content,
        };
        let header = g.header(column);
        if header != Rect::ZERO && g.column(column).width() > 0.0 {
            g.inspector_toggle_column = Some(column);
            g.inspector_toggle = chip_in(g.column(column), header, false);
            if g.history_column == Some(column) && !g.inspector_modal() {
                g.inspector_toggle = g.inspector_toggle - Vec2::new(m.chip + m.gap, 0.0);
            }
        }
    }
    // Content is fully hidden by a pushed screen: no buttons on it.
    if g.toggle_column.is_some_and(|c| g.column(c).width() <= 0.0) {
        g.toggle = Rect::ZERO;
        g.toggle_column = None;
    }

    // 10. Dragging the docked sidebar's trailing edge resizes it.
    if g.sidebar == SidebarPlacement::Docked
        && inputs.mode != NavMode::Docked
        && sidebar_open
        && g.sidebar_column.width() > 0.0
    {
        let x = g.sidebar_column.x1;
        g.resize_handle = Rect::new(x - m.resize_grip, hh, x + m.resize_grip, h);
    }
    g
}

fn drawer_header(col: &Rect, hh: f64, floating: bool) -> Rect {
    if hh <= 0.0 || col.width() <= 0.0 {
        return Rect::ZERO;
    }
    // A floating drawer's header ends where the window's header row ends, so
    // titles line up across the drawer and the content behind it.
    let y1 = if floating {
        hh.max(col.y0)
    } else {
        col.y0 + hh
    };
    Rect::new(col.x0, col.y0, col.x1, y1)
}

fn layout_of(sidebar: SidebarPlacement) -> SplitLayout {
    match sidebar {
        SidebarPlacement::Drawer | SidebarPlacement::Screen => SplitLayout::Overlay,
        SidebarPlacement::Docked | SidebarPlacement::None => SplitLayout::Docked,
    }
}

fn stack_index(stack: &[NavPane], pane: NavPane) -> usize {
    stack
        .iter()
        .position(|p| *p == pane)
        .or_else(|| {
            // An overview standing beside the content is the content screen.
            (pane == NavPane::Overview)
                .then(|| stack.iter().position(|p| *p == NavPane::Content))
                .flatten()
        })
        .unwrap_or(stack.len().saturating_sub(1))
}

fn index_f64(i: usize) -> f64 {
    f64::from(u32::try_from(i).unwrap_or(u32::MAX))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a reveal is clamped to 0..=1, always well within f32 range"
)]
fn unit_f32(v: f64) -> f32 {
    v.clamp(0.0, 1.0) as f32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout extents fit f32 exactly at every UI scale"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// The shell's buttons report presses here.
#[derive(Default)]
struct Requests {
    toggle: Cell<bool>,
    back: Cell<bool>,
    inspector: Cell<bool>,
}

/// A drag of the sidebar's trailing edge.
#[derive(Clone, Copy, Debug)]
struct Resize {
    start_x: f64,
    start_width: f64,
}

/// The one window shell. See the module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "structure, layout and paint dirtiness and the cross-fade are independent flags"
)]
pub struct NavShell {
    id: Option<String>,
    sidebar: Option<Element>,
    overview: Option<Element>,
    content: Element,
    inspector: Option<Element>,
    filter: Option<Element>,
    headers: [Header; 4],
    toggle: Button,
    back: Button,
    inspector_toggle: Button,
    /// The window's History control: the one that opens History and the
    /// one, on, that closes it.
    history_open: Button,
    history_close: Button,
    requests: Rc<Requests>,
    pane_binding: Option<LocalBinding<NavPane>>,
    /// The content pane the person last worked in; a collapse to narrow
    /// keeps it in front.
    active_hint: Option<NavPane>,

    inputs: Rc<RefCell<Inputs>>,
    policy: Rc<crate::widget::manual::Policy>,
    invalidation: ManualInvalidation,
    baseline: Vec<(NodeId, Style)>,
    /// The parts in the host tree, in build order.
    host_parts: Vec<Part>,
    layout_theme: Theme,
    conventions: PlatformConventions,

    rect: Rect,
    geometry: NavGeometry,
    last_stack: Option<Vec<NavPane>>,
    /// The app or the person chose the front screen.
    pane_chosen: bool,
    resize: Option<Resize>,
    last_cursor: Point,
    last_tick: Option<Instant>,
    /// Reduced motion: the sidebar and drawers fade in instead of sliding.
    fade: f64,
    animating: bool,
    /// The host tree has been built once.
    built: bool,
    structure_dirty: bool,
    layout_dirty: bool,
    paint_dirty: bool,
}

impl Default for NavShell {
    fn default() -> Self {
        Self::new()
    }
}

impl NavShell {
    /// An empty shell; give it columns with the builders.
    pub fn new() -> Self {
        let requests = Rc::new(Requests::default());
        let inputs = Rc::new(RefCell::new(Inputs::default()));
        let policy_inputs = Rc::clone(&inputs);
        let policy: Rc<crate::widget::manual::Policy> =
            Rc::new(move |ctx: &mut ManualContext<'_>| place_parts(&policy_inputs.borrow(), ctx));
        let toggle = Self::shell_button("nav-shell-sidebar", Icon::PanelLeft, "Hide sidebar", {
            let requests = Rc::clone(&requests);
            move || requests.toggle.set(true)
        });
        let back = Self::shell_button("nav-shell-back", Icon::ChevronLeft, "Back", {
            let requests = Rc::clone(&requests);
            move || requests.back.set(true)
        });
        let inspector_toggle =
            Self::shell_button("nav-shell-inspector", Icon::PanelRight, "Hide inspector", {
                let requests = Rc::clone(&requests);
                move || requests.inspector.set(true)
            });
        Self {
            id: None,
            sidebar: None,
            overview: None,
            content: Box::new(crate::widget::Spacer::new()),
            inspector: None,
            filter: None,
            headers: Default::default(),
            toggle,
            back,
            inspector_toggle,
            history_open: crate::standalone_history_host::history_control_button(
                "nav-shell-history",
                HistoryControl::Closed,
            ),
            history_close: crate::standalone_history_host::history_control_button(
                "nav-shell-history",
                HistoryControl::Open,
            ),
            requests,
            pane_binding: None,
            active_hint: None,
            inputs,
            policy,
            invalidation: ManualInvalidation::default(),
            baseline: Vec::new(),
            host_parts: Vec::new(),
            layout_theme: Theme::dark(),
            conventions: PlatformConventions::default(),
            rect: Rect::ZERO,
            geometry: NavGeometry::default(),
            last_stack: None,
            pane_chosen: false,
            resize: None,
            last_cursor: Point::ZERO,
            last_tick: None,
            fade: 1.0,
            animating: false,
            built: false,
            structure_dirty: false,
            layout_dirty: true,
            paint_dirty: true,
        }
    }

    fn shell_button(id: &str, icon: Icon, name: &str, on_press: impl FnMut() + 'static) -> Button {
        Button::icon_only(id, icon, name).on_view(on_press)
    }

    // --- slots -------------------------------------------------------------

    /// The sidebar of destinations (usually a [`crate::widget::NavList`]).
    #[must_use]
    pub fn sidebar(mut self, sidebar: Element) -> Self {
        self.set_sidebar(Some(sidebar));
        self
    }

    /// The overview list beside the content (a message list, a city list):
    /// content the person reads, not navigation.
    #[must_use]
    pub fn overview(mut self, overview: Element) -> Self {
        self.set_overview(Some(overview));
        self
    }

    /// The content.
    #[must_use]
    pub fn content(mut self, content: Element) -> Self {
        self.set_content(content);
        self
    }

    /// The inspector at the trailing edge: properties of the selection.
    #[must_use]
    pub fn inspector(mut self, inspector: Element) -> Self {
        self.set_inspector(Some(inspector));
        self
    }

    /// The sidebar column's header: a sentence-case title and the app's
    /// actions (buttons). The toggle is the shell's; do not add one.
    #[must_use]
    pub fn sidebar_header(self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.with_header(Column::Sidebar, title.into(), actions)
    }

    /// The overview column's header (the list's title and its actions).
    #[must_use]
    pub fn overview_header(self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.with_header(Column::Overview, title.into(), actions)
    }

    /// The content column's header: the detail title and toolbar actions.
    #[must_use]
    pub fn content_header(self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.with_header(Column::Content, title.into(), actions)
    }

    /// The inspector column's header.
    #[must_use]
    pub fn inspector_header(self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.with_header(Column::Inspector, title.into(), actions)
    }

    fn with_header(mut self, column: Column, title: String, actions: Vec<Element>) -> Self {
        self.set_header(column, title, actions);
        self
    }

    fn set_header(&mut self, column: Column, title: String, actions: Vec<Element>) {
        let actions = (!actions.is_empty()).then(|| -> Element {
            let mut row = crate::widget::Row::new().no_wrap();
            for action in actions {
                row = row.push(action);
            }
            Box::new(row)
        });
        if let Some(header) = self.headers.get_mut(column.index()) {
            header.title = (!title.trim().is_empty()).then_some(title);
            header.actions = actions;
        }
        self.slots_changed();
    }

    /// The one filter field of the window, scoped to the sidebar's list (or
    /// the overview when there is no sidebar), at the shell's metrics.
    /// Ctrl+F (⌘F) focuses it.
    #[must_use]
    pub fn with_filter(mut self, filter: Element) -> Self {
        self.filter = Some(filter);
        self.slots_changed();
        self
    }

    /// What the sidebar becomes in a narrow window.
    #[must_use]
    pub fn mode(mut self, mode: NavMode) -> Self {
        self.inputs.borrow_mut().mode = mode;
        self.slots_changed();
        self
    }

    /// Give the shell a stable id for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Start with the sidebar hidden (where it docks).
    #[must_use]
    pub fn with_sidebar_hidden(self) -> Self {
        self.inputs.borrow_mut().sidebar.snap(false);
        self
    }

    /// Start with the inspector hidden (where it docks).
    #[must_use]
    pub fn with_inspector_hidden(self) -> Self {
        self.inputs.borrow_mut().inspector.snap(false);
        self
    }

    /// Keep the front screen of a narrow window in `state`: a row of the
    /// overview can push the content by setting [`NavPane::Content`], and
    /// the Back button writes the screen it returns to.
    #[must_use]
    pub fn with_pane_state(mut self, state: &LocalState<NavPane>) -> Self {
        let (pane, binding) = LocalBinding::new(state);
        self.inputs.borrow_mut().pane = pane;
        self.pane_binding = Some(binding);
        self.pane_chosen = true;
        self
    }

    // --- live slot swap ----------------------------------------------------
    //
    // A host that rebuilds its children keeps the shell and swaps them in
    // here: the shell's responsive state lives on the shell.

    pub fn set_sidebar(&mut self, sidebar: Option<Element>) {
        self.sidebar = sidebar;
        self.slots_changed();
    }

    pub fn set_overview(&mut self, overview: Option<Element>) {
        self.overview = overview;
        self.slots_changed();
    }

    pub fn set_content(&mut self, content: Element) {
        self.content = content;
        self.slots_changed();
    }

    pub fn set_inspector(&mut self, inspector: Option<Element>) {
        self.inspector = inspector;
        self.slots_changed();
    }

    /// Change a column's title in place (the selected message's subject).
    pub fn set_content_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Content, title.into());
    }

    /// Change the overview's title in place.
    pub fn set_overview_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Overview, title.into());
    }

    fn set_title(&mut self, column: Column, title: String) {
        let title = (!title.trim().is_empty()).then_some(title);
        if let Some(header) = self.headers.get_mut(column.index())
            && header.title != title
        {
            header.title = title;
            self.slots_changed();
        }
    }

    fn slots_changed(&mut self) {
        {
            let mut inputs = self.inputs.borrow_mut();
            inputs.has_sidebar = self.sidebar.is_some();
            inputs.has_overview = self.overview.is_some();
            inputs.has_inspector = self.inspector.is_some();
            inputs.has_filter = self.filter.is_some();
            for column in Column::ALL {
                let i = column.index();
                let header = self.headers.get(i);
                if let Some(slot) = inputs.titled.get_mut(i) {
                    *slot = header.is_some_and(|h| h.title.is_some());
                }
                if let Some(slot) = inputs.with_actions.get_mut(i) {
                    *slot = header.is_some_and(|h| h.actions.is_some());
                }
            }
        }
        self.structure_dirty = true;
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.invalidation.invalidate();
        self.layout_dirty = true;
        self.paint_dirty = true;
    }

    // --- state -------------------------------------------------------------

    /// The geometry of the last layout, in window coordinates.
    pub fn geometry(&self) -> &NavGeometry {
        &self.geometry
    }

    pub fn has_sidebar(&self) -> bool {
        self.sidebar.is_some()
    }

    pub fn has_overview(&self) -> bool {
        self.overview.is_some()
    }

    /// Whether the sidebar shows (docked and not hidden, or a drawer that
    /// is open, or the front push screen).
    pub fn is_sidebar_open(&self) -> bool {
        if !self.has_sidebar() {
            return false;
        }
        match self.geometry.sidebar {
            SidebarPlacement::Screen => self.front_pane() == NavPane::Sidebar,
            placement => self.inputs.borrow().sidebar.is_open(layout_of(placement)),
        }
    }

    /// Whether the inspector shows.
    pub fn is_inspector_open(&self) -> bool {
        let layout = match self.geometry.inspector {
            InspectorPlacement::None => return false,
            InspectorPlacement::Docked => SplitLayout::Docked,
            InspectorPlacement::Drawer => SplitLayout::Overlay,
        };
        self.inputs.borrow().inspector.is_open(layout)
    }

    /// The front screen of the push stack (meaningful when it has more than
    /// one screen).
    pub fn front_pane(&self) -> NavPane {
        self.inputs.borrow().pane
    }

    /// Show or hide the sidebar, remembering the choice for this window size
    /// class. In [`NavMode::Push`] narrow, the sidebar is a screen: showing
    /// it goes back to it.
    pub fn toggle_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        if self.geometry.sidebar == SidebarPlacement::Screen {
            self.show(NavPane::Sidebar);
            return;
        }
        let layout = layout_of(self.geometry.sidebar);
        self.inputs.borrow_mut().sidebar.toggle(layout);
        self.after_flap_change();
    }

    pub fn open_sidebar(&mut self) {
        if self.has_sidebar() && !self.is_sidebar_open() {
            self.toggle_sidebar();
        }
    }

    pub fn close_sidebar(&mut self) {
        if self.has_sidebar()
            && self.is_sidebar_open()
            && self.geometry.sidebar != SidebarPlacement::Screen
        {
            self.toggle_sidebar();
        }
    }

    /// Show or hide the inspector.
    pub fn toggle_inspector(&mut self) {
        let layout = match self.geometry.inspector {
            InspectorPlacement::None => return,
            InspectorPlacement::Docked => SplitLayout::Docked,
            InspectorPlacement::Drawer => SplitLayout::Overlay,
        };
        self.inputs.borrow_mut().inspector.toggle(layout);
        self.after_flap_change();
    }

    fn after_flap_change(&mut self) {
        if self.layout_theme.reduce_motion {
            self.fade = 0.0;
        }
        self.animating = true;
        self.structure_dirty = true;
        self.update_toggle_names();
        self.invalidate();
    }

    /// Bring `pane` to the front of a narrow window's push stack. In a wide
    /// window every pane is already showing; this records which one a later
    /// collapse keeps in front.
    pub fn show(&mut self, pane: NavPane) {
        let pane = match pane {
            NavPane::Sidebar if !self.has_sidebar() => NavPane::Content,
            NavPane::Overview if !self.has_overview() => NavPane::Content,
            pane => pane,
        };
        if pane != NavPane::Sidebar {
            self.active_hint = Some(pane);
        }
        self.pane_chosen = true;
        {
            let mut inputs = self.inputs.borrow_mut();
            if inputs.pane == pane {
                return;
            }
            let stack = &self.geometry.stack;
            let from = inputs
                .push_pos
                .unwrap_or_else(|| index_f64(stack_index(stack, inputs.pane)));
            inputs.pane = pane;
            inputs.push_from = Some(from);
            inputs.push_progress = 0.0;
            if self.layout_theme.reduce_motion || stack.is_empty() {
                inputs.push_pos = None;
                inputs.push_progress = 1.0;
            }
        }
        if let Some(binding) = self.pane_binding.as_mut() {
            binding.publish(&pane);
        }
        self.animating = true;
        self.structure_dirty = true;
        self.invalidate();
    }

    /// Go back one screen in a narrow window. Returns whether it did.
    pub fn back(&mut self) -> bool {
        let stack = self.geometry.stack.clone();
        let front = stack_index(&stack, self.front_pane());
        let Some(previous) = front.checked_sub(1).and_then(|i| stack.get(i).copied()) else {
            return false;
        };
        self.show(previous);
        true
    }

    /// Snap every animation to its end (deterministic shots and tests).
    pub fn snap(&mut self) {
        {
            let mut inputs = self.inputs.borrow_mut();
            let layout = layout_of(self.geometry.sidebar);
            let open = inputs.sidebar.is_open(layout);
            inputs.sidebar.snap(open);
            let layout = match self.geometry.inspector {
                InspectorPlacement::Drawer => SplitLayout::Overlay,
                _ => SplitLayout::Docked,
            };
            let open = inputs.inspector.is_open(layout);
            inputs.inspector.snap(open);
            inputs.push_pos = None;
            inputs.push_from = None;
            inputs.push_progress = 1.0;
        }
        self.fade = 1.0;
        self.animating = false;
        self.structure_dirty = true;
        self.invalidate();
    }

    fn update_toggle_names(&mut self) {
        let shortcut = HostShortcut::Sidebar
            .shortcut(self.conventions)
            .label(self.conventions);
        let sidebar_open = self.is_sidebar_open();
        let name = if sidebar_open {
            "Hide sidebar"
        } else {
            "Show sidebar"
        };
        self.toggle.set_accessible_name(name);
        self.toggle
            .set_tooltip(Some(format!("{name} ({shortcut})")));
        let name = if self.is_inspector_open() {
            "Hide inspector"
        } else {
            "Show inspector"
        };
        self.inspector_toggle.set_accessible_name(name);
        self.inspector_toggle.set_tooltip(Some(name.to_owned()));
    }

    // --- parts -------------------------------------------------------------

    fn part(&self, part: Part) -> Option<&dyn Widget> {
        match part {
            Part::Body(Column::Sidebar) => self.sidebar.as_deref(),
            Part::Body(Column::Overview) => self.overview.as_deref(),
            Part::Body(Column::Content) => Some(self.content.as_ref()),
            Part::Body(Column::Inspector) => self.inspector.as_deref(),
            Part::Actions(column) => self
                .headers
                .get(column.index())
                .and_then(|header| header.actions.as_deref()),
            Part::Filter => self.filter.as_deref(),
            Part::Toggle => Some(&self.toggle),
            Part::Back => Some(&self.back),
            Part::InspectorToggle => Some(&self.inspector_toggle),
            Part::History(false) => Some(&self.history_open),
            Part::History(true) => Some(&self.history_close),
        }
    }

    fn part_mut(&mut self, part: Part) -> Option<&mut (dyn Widget + 'static)> {
        match part {
            Part::Body(Column::Sidebar) => self.sidebar.as_deref_mut(),
            Part::Body(Column::Overview) => self.overview.as_deref_mut(),
            Part::Body(Column::Content) => Some(self.content.as_mut()),
            Part::Body(Column::Inspector) => self.inspector.as_deref_mut(),
            Part::Actions(column) => self
                .headers
                .get_mut(column.index())
                .and_then(|header| header.actions.as_deref_mut()),
            Part::Filter => self.filter.as_deref_mut(),
            Part::Toggle => Some(&mut self.toggle),
            Part::Back => Some(&mut self.back),
            Part::InspectorToggle => Some(&mut self.inspector_toggle),
            Part::History(false) => Some(&mut self.history_open),
            Part::History(true) => Some(&mut self.history_close),
        }
    }

    /// Every element the shell owns, shown or not: sync, update and wake
    /// reach parts a modal drawer hides from input.
    const ALL_PARTS: [Part; 14] = [
        Part::Body(Column::Overview),
        Part::Body(Column::Content),
        Part::Body(Column::Inspector),
        Part::Body(Column::Sidebar),
        Part::Actions(Column::Overview),
        Part::Actions(Column::Content),
        Part::Actions(Column::Inspector),
        Part::Actions(Column::Sidebar),
        Part::Filter,
        Part::Toggle,
        Part::Back,
        Part::InspectorToggle,
        Part::History(false),
        Part::History(true),
    ];

    fn for_each_owned(&self, mut visit: impl FnMut(&dyn Widget)) {
        for part in Self::ALL_PARTS {
            if let Some(widget) = self.part(part) {
                visit(widget);
            }
        }
    }

    fn for_each_owned_mut(&mut self, mut visit: impl FnMut(&mut (dyn Widget + 'static))) {
        for part in Self::ALL_PARTS {
            if let Some(widget) = self.part_mut(part) {
                visit(widget);
            }
        }
    }

    /// The parts input and semantics reach: those in the host tree once it
    /// is built (in its order), and every owned part before the first build,
    /// when there is no tree for their order to disagree with.
    fn exposed_parts(&self) -> Vec<Part> {
        if self.built {
            self.host_parts.clone()
        } else {
            Self::ALL_PARTS
                .into_iter()
                .filter(|part| self.part(*part).is_some())
                .filter(|part| !matches!(part, Part::Toggle) || self.has_sidebar())
                .filter(|part| !matches!(part, Part::InspectorToggle) || self.inspector.is_some())
                .filter(|part| !matches!(part, Part::Back))
                .filter(|part| match part {
                    Part::History(open) => {
                        self.inputs.borrow().history.map(HistoryControl::is_open) == Some(*open)
                    }
                    _ => true,
                })
                .collect()
        }
    }

    /// Which column's theme a part draws with.
    fn part_column(&self, part: Part) -> Option<Column> {
        match part {
            Part::Body(column) | Part::Actions(column) => Some(column),
            Part::Filter => self.geometry.filter_column(&self.inputs.borrow()),
            Part::Toggle => self.geometry.toggle_column,
            Part::Back => self.geometry.back_column,
            Part::InspectorToggle => self.geometry.inspector_toggle_column,
            Part::History(_) => self.geometry.history_column,
        }
    }

    fn column_theme(&self, theme: &Theme, column: Option<Column>) -> Theme {
        match column {
            Some(Column::Sidebar) => match self.geometry.sidebar {
                SidebarPlacement::Drawer => theme.on_ground(crate::chrome::panel_ground(theme)),
                _ => theme.in_region(crate::surface::Region::Sidebar),
            },
            Some(Column::Inspector) => match self.geometry.inspector {
                InspectorPlacement::Drawer => theme.on_ground(crate::chrome::panel_ground(theme)),
                _ => theme.in_region(crate::surface::Region::Inspector),
            },
            Some(Column::Overview | Column::Content) | None => theme.clone(),
        }
    }

    fn take_requests(&mut self) -> bool {
        let mut acted = false;
        if self.requests.toggle.replace(false) {
            self.toggle_sidebar();
            acted = true;
        }
        if self.requests.back.replace(false) {
            acted |= self.back();
        }
        if self.requests.inspector.replace(false) {
            self.toggle_inspector();
            acted = true;
        }
        acted
    }

    /// The person chose a destination in the sidebar or a row of the
    /// overview: a narrow window moves on to what they chose.
    fn follow_navigation(&mut self) {
        let from_sidebar = self
            .sidebar
            .as_deref_mut()
            .is_some_and(|sidebar| crate::widget::nav_list::take_navigation(sidebar));
        let from_overview = self
            .overview
            .as_deref_mut()
            .is_some_and(|overview| crate::widget::nav_list::take_navigation(overview));
        if from_sidebar {
            if self.geometry.sidebar == SidebarPlacement::Drawer && self.is_sidebar_open() {
                self.toggle_sidebar();
            }
            if self.geometry.sidebar == SidebarPlacement::Screen {
                let next = if self.has_overview() && !self.geometry.side_by_side {
                    NavPane::Overview
                } else {
                    NavPane::Content
                };
                self.show(next);
            }
        }
        if from_overview && !self.geometry.stack.is_empty() && !self.geometry.side_by_side {
            self.show(NavPane::Content);
        }
    }

    /// Advance the slides by the time since the last frame.
    fn tick(&mut self, theme: &Theme) {
        let now = crate::time::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);
        let base = theme.tokens.motion.durations.base();
        let reduce = theme.reduce_motion;
        let sidebar_layout = layout_of(self.geometry.sidebar);
        let inspector_layout = match self.geometry.inspector {
            InspectorPlacement::Drawer => SplitLayout::Overlay,
            _ => SplitLayout::Docked,
        };
        let stack = self.geometry.stack.clone();
        let mut moving = false;
        {
            let mut inputs = self.inputs.borrow_mut();
            if reduce {
                inputs.sidebar.settle(sidebar_layout);
                inputs.inspector.settle(inspector_layout);
            } else {
                moving |= inputs.sidebar.tick(sidebar_layout, dt, base);
                moving |= inputs.inspector.tick(inspector_layout, dt, base);
            }
            if stack.is_empty() || (inputs.push_progress >= 1.0 && inputs.push_pos.is_none()) {
                inputs.push_pos = None;
                inputs.push_progress = 1.0;
            } else {
                let target = index_f64(stack_index(&stack, inputs.pane));
                if reduce || base.as_secs_f64() <= 0.0 {
                    inputs.push_progress = 1.0;
                } else if inputs.push_progress < 1.0 {
                    inputs.push_progress =
                        (inputs.push_progress + dt / base.as_secs_f64()).min(1.0);
                }
                let eased = crate::ease_out(inputs.push_progress);
                let from = inputs.push_from.unwrap_or(target);
                if inputs.push_progress >= 1.0 {
                    // Settled: the front screen's index, exactly.
                    inputs.push_pos = None;
                    inputs.push_from = None;
                } else {
                    inputs.push_pos = Some(from + (target - from) * eased);
                    moving = true;
                }
            }
        }
        if reduce && self.fade < 1.0 {
            let fast = theme.tokens.motion.durations.fast().as_secs_f64();
            self.fade = if fast <= 0.0 {
                1.0
            } else {
                (self.fade + dt / fast).min(1.0)
            };
            self.paint_dirty = true;
            self.animating = self.fade < 1.0;
            return;
        }
        if moving {
            self.invalidate();
        } else if self.animating {
            // The last frame of a slide may hide a part.
            self.structure_dirty = true;
            self.invalidate();
        }
        self.animating = moving;
    }

    fn refresh_geometry(&mut self) {
        let local = plan(&self.inputs.borrow(), self.rect.width(), self.rect.height());
        self.geometry = local.translated(Vec2::new(self.rect.x0, self.rect.y0));
    }

    /// Note a change of size class: the sidebar's choice for the new class
    /// applies, and a collapse keeps the pane the person worked in.
    fn reconcile_size_class(&mut self) {
        let layout = layout_of(self.geometry.sidebar);
        let stack = self.geometry.stack.clone();
        let previous = self.last_stack.replace(stack.clone());
        {
            let mut inputs = self.inputs.borrow_mut();
            inputs.sidebar.reconcile(layout);
            let inspector_layout = match self.geometry.inspector {
                InspectorPlacement::Drawer => SplitLayout::Overlay,
                _ => SplitLayout::Docked,
            };
            inputs.inspector.reconcile(inspector_layout);
        }
        if previous.as_ref() != Some(&stack) {
            // The first layout opens on the stack's first screen unless the
            // app chose one; a later collapse keeps the pane the person
            // worked in.
            let keep = match (previous, self.pane_chosen) {
                (None, false) => stack.first().copied().unwrap_or(NavPane::Content),
                (None, true) => self.front_pane(),
                (Some(_), _) => self.active_hint.unwrap_or(NavPane::Content),
            };
            let mut inputs = self.inputs.borrow_mut();
            inputs.pane = if stack.is_empty() || stack.contains(&keep) {
                keep
            } else {
                NavPane::Content
            };
            inputs.push_pos = None;
            inputs.push_from = None;
            inputs.push_progress = 1.0;
            drop(inputs);
            self.structure_dirty = true;
            self.update_toggle_names();
            self.invalidate();
        }
    }

    // --- painting ----------------------------------------------------------

    fn paint(&mut self, f: &mut Frame<'_>) {
        let g = self.geometry.clone();
        let floating = |column: Column| match column {
            Column::Sidebar => g.sidebar == SidebarPlacement::Drawer,
            Column::Inspector => g.inspector == InspectorPlacement::Drawer,
            Column::Overview | Column::Content => false,
        };
        // Under reduced motion a sidebar or drawer that just appeared fades
        // in instead of sliding.
        let fade = unit_f32(self.fade);

        // Docked regions under their columns.
        if g.inspector == InspectorPlacement::Docked && g.inspector_column.width() > 0.0 {
            crate::surface::region(
                f.scene,
                f.transform,
                g.inspector_column,
                crate::surface::Region::Inspector,
                f.theme,
            );
        }
        let sidebar_region = matches!(
            g.sidebar,
            SidebarPlacement::Docked | SidebarPlacement::Screen
        ) && g.sidebar_column.width() > 0.0;

        // Columns standing in the window, back to front.
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            if floating(column) {
                continue;
            }
            let faded = column == Column::Sidebar && sidebar_region && fade < 1.0;
            if faded {
                f.scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    fade,
                    f.transform,
                    &g.sidebar_column,
                );
            }
            if column == Column::Sidebar && sidebar_region {
                crate::surface::region(
                    f.scene,
                    f.transform,
                    g.sidebar_column,
                    crate::surface::Region::Sidebar,
                    f.theme,
                );
            }
            self.paint_column(f, column);
            if faded {
                f.scene.pop_layer();
            }
        }
        self.paint_rules(f.scene, f.transform, f.theme, &g);
        let history = self
            .inputs
            .borrow()
            .history
            .map(|control| Part::History(control.is_open()));
        for part in [Part::Back, Part::InspectorToggle, Part::Toggle]
            .into_iter()
            .chain(history)
        {
            if !self.part_column(part).is_some_and(floating) {
                self.paint_part(f, part);
            }
        }

        // Drawers over everything, behind their scrim.
        let open_drawer = floating(Column::Sidebar) && g.sidebar_column.width() > 0.0
            || floating(Column::Inspector) && g.inspector_column.width() > 0.0;
        if !open_drawer {
            return;
        }
        let faded = fade < 1.0;
        if faded {
            f.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                fade,
                f.transform,
                &self.rect,
            );
        }
        if g.scrim.width() > 0.0 {
            crate::chrome::fill_scrim(f.scene, f.transform, g.scrim, g.scrim_strength, f.theme);
        }
        for column in [Column::Inspector, Column::Sidebar] {
            let rect = g.column(column);
            if !floating(column) || rect.width() <= 0.0 {
                continue;
            }
            crate::widget::sidebar::paint_floating_drawer(f.scene, f.transform, rect, f.theme);
            self.paint_column(f, column);
            for part in [Part::Toggle, Part::InspectorToggle] {
                if self.part_column(part) == Some(column) {
                    self.paint_part(f, part);
                }
            }
        }
        if faded {
            f.scene.pop_layer();
        }
    }

    fn paint_column(&mut self, f: &mut Frame<'_>, column: Column) {
        let rect = self.geometry.column(column);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let local = self.column_theme(f.theme, Some(column));
        let clip = rect.intersect(self.rect);
        f.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            f.transform,
            &clip,
        );
        self.paint_title(f.scene, f.transform, f.text, &local, column);
        let mut parts = vec![Part::Body(column), Part::Actions(column)];
        if self.geometry.filter_column(&self.inputs.borrow()) == Some(column) {
            parts.push(Part::Filter);
        }
        for part in parts {
            f.draw(self.part_mut(part), &local);
        }
        f.scene.pop_layer();
    }

    fn paint_part(&mut self, f: &mut Frame<'_>, part: Part) {
        let rect = match part {
            Part::Toggle => self.geometry.toggle,
            Part::Back => self.geometry.back,
            Part::InspectorToggle => self.geometry.inspector_toggle,
            Part::History(_) => self.geometry.history,
            _ => return,
        };
        if rect.width() <= 0.0 {
            return;
        }
        let local = self.column_theme(f.theme, self.part_column(part));
        f.draw(self.part_mut(part), &local);
    }

    /// A column's title: `Heading`, primary ink of its ground, after the
    /// shell's leading button and before the actions.
    fn paint_title(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        column: Column,
    ) {
        let Some(title) = self
            .headers
            .get(column.index())
            .and_then(|header| header.title.as_deref())
        else {
            return;
        };
        let g = &self.geometry;
        let header = g.header(column);
        if header == Rect::ZERO
            || self.inputs.borrow().mode == NavMode::Docked && column == Column::Sidebar
        {
            return;
        }
        let m = self.inputs.borrow().metrics;
        let mut x0 = header.x0 + m.inset;
        for (button, home) in [(g.toggle, g.toggle_column), (g.back, g.back_column)] {
            if home == Some(column) && button.width() > 0.0 && button.x1 > x0 - m.inset {
                x0 = button.x1 + m.gap;
            }
        }
        let mut x1 = g.actions_band(column).x1 - m.inset;
        if let Some(actions) = self.part(Part::Actions(column)) {
            let rect = actions.get_rect();
            if rect.width() > 0.0 {
                x1 = x1.min(rect.x0 - m.gap);
            }
        }
        if x1 <= x0 {
            return;
        }
        let style = theme.text(TextRole::Heading);
        let title = text.elide_to_width_style(title, to_f32(x1 - x0), &style);
        let (_, line_h) = text.measure_style(&title, None, &style);
        let y =
            crate::text_metrics::centered_text_top(header.y0, header.height(), f64::from(line_h));
        text.draw_style(
            scene,
            &title,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(x0, y)),
            None,
            &style,
        );
    }

    /// The hairline under the header row over the content columns, and the
    /// rule between the overview and the content. Regions are told apart by
    /// tone, not lines.
    fn paint_rules(&self, scene: &mut Scene, transform: Affine, theme: &Theme, g: &NavGeometry) {
        let hh = g.header_height;
        for column in [Column::Overview, Column::Content, Column::Inspector] {
            let header = g.header(column);
            if header == Rect::ZERO || g.in_drawer(column) {
                continue;
            }
            let ground = self
                .column_theme(theme, Some(column))
                .tokens
                .colors
                .surfaces
                .content;
            crate::surface::separator(
                scene,
                transform,
                header.x0,
                header.x1,
                header.y0 + hh,
                0.0,
                ground,
            );
        }
        if g.side_by_side && g.overview_column.width() > 0.0 && g.content_column.width() > 0.0 {
            let x = g.overview_column.x1;
            let rule = Rect::new(x - 1.0, g.overview_column.y0, x, g.overview_column.y1);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::surface::edge_on(theme.tokens.colors.surfaces.content),
                None,
                &rule,
            );
        }
    }
}

/// What one paint pass draws into.
struct Frame<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    presence: &'a PresenceCtx,
    permissions: Option<&'a crate::permissions::PermissionContext>,
}

impl Frame<'_> {
    /// Draw one of the shell's children with the theme of the ground it
    /// stands on.
    fn draw(&mut self, widget: Option<&mut (dyn Widget + 'static)>, theme: &Theme) {
        let Some(widget) = widget else {
            return;
        };
        match self.permissions {
            Some(p) => {
                widget.draw_with_presence(
                    self.scene,
                    self.transform,
                    self.text,
                    theme,
                    self.presence,
                    p,
                );
            }
            None => widget.draw(self.scene, self.transform, self.text, theme, self.presence),
        }
    }
}

/// The layout policy the host runs: place every built part at the rect the
/// plan gives it.
fn place_parts(
    inputs: &Inputs,
    ctx: &mut ManualContext<'_>,
) -> Result<Size<f32>, WidgetLayoutError> {
    let size = ctx.available();
    let g = plan(inputs, f64::from(size.width), f64::from(size.height));
    for (index, part) in inputs.parts.iter().enumerate() {
        let rect = match g.placement(*part) {
            Placement::Exact(rect) => rect,
            Placement::Trailing(band) => {
                if band.width() <= 0.0 || band.height() <= 0.0 {
                    Rect::ZERO
                } else {
                    let proposal = Size {
                        width: MeasureConstraint::AtMost(to_f32(band.width())),
                        height: MeasureConstraint::AtMost(to_f32(band.height())),
                    };
                    let measured = ManualContext::measure(ctx, index, proposal)?.size;
                    let (w, h) = (f64::from(measured.width), f64::from(measured.height));
                    let x1 = band.x1 - inputs.metrics.inset;
                    let y0 = band.center().y - h / 2.0;
                    Rect::new((x1 - w).max(band.x0), y0, x1, y0 + h)
                }
            }
        };
        let rect = if rect.width() < 0.0 || rect.height() < 0.0 {
            Rect::ZERO
        } else {
            rect
        };
        ctx.place(index, rect)?;
    }
    Ok(size)
}

impl Widget for NavShell {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn WindowHeader
    );

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.conventions = conventions;
        self.update_toggle_names();
        self.for_each_owned_mut(|child| child.set_platform_conventions(conventions));
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.for_each_owned_mut(|child| child.set_interaction_environment(environment));
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let next = self.pane_binding.as_mut().and_then(LocalBinding::poll);
        if let Some(pane) = next {
            self.show(pane);
        }
        self.take_requests();
        self.follow_navigation();
        self.for_each_owned_mut(|child| child.sync(state, path));
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let mut changed = self.take_requests();
        self.for_each_owned_mut(|child| changed |= child.update(state, path));
        changed || self.animating
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        let size = if self.rect.width() > 0.0 {
            (self.rect.width(), self.rect.height())
        } else {
            // Before the first layout, assume a regular window.
            (
                f64::from(theme.tokens.metrics.compact_breakpoint) * 4.0,
                f64::from(theme.tokens.metrics.compact_breakpoint),
            )
        };
        let parts = {
            let mut inputs = self.inputs.borrow_mut();
            inputs.metrics = Metrics::resolve(theme);
            let g = plan(&inputs, size.0, size.1);
            let parts = g.visible_parts(&inputs);
            inputs.parts.clone_from(&parts);
            parts
        };
        self.update_toggle_names();
        let mut nodes = Vec::with_capacity(parts.len());
        for part in &parts {
            if let Some(widget) = self.part_mut(*part) {
                nodes.push(widget.build_layout(taffy, theme, node_map));
            }
        }
        self.baseline = nodes
            .iter()
            .map(|&node| (node, taffy.style(node).cloned().unwrap_or_default()))
            .collect();
        self.host_parts = parts;
        self.built = true;
        self.structure_dirty = false;
        self.invalidation.invalidate();
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        Some(ManualPolicy::for_children(
            Rc::clone(&self.policy),
            self.baseline.clone(),
            self.invalidation.clone(),
        ))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.refresh_geometry();
        self.reconcile_size_class();
        self.refresh_geometry();
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        let child_nodes = taffy.children(node).unwrap_or_default();
        let parts = self.host_parts.clone();
        for (part, child) in parts.iter().zip(child_nodes) {
            if let Some(widget) = self.part_mut(*part) {
                widget.apply_layout(taffy, child, child_origin);
            }
        }
        self.invalidation.acknowledge_applied();
        self.layout_dirty = false;
        let visible = self.geometry.visible_parts(&self.inputs.borrow());
        if visible != self.host_parts {
            self.structure_dirty = true;
        }
    }

    fn layout_needs_measure(&self) -> bool {
        let mut dirty = self.invalidation.is_pending();
        self.for_each_owned(|child| dirty |= child.layout_needs_measure());
        dirty
    }

    fn is_layout_dirty(&self) -> bool {
        let mut dirty = self.layout_dirty || self.invalidation.is_pending();
        self.for_each_owned(|child| dirty |= child.is_layout_dirty());
        dirty
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint(&mut Frame {
            scene,
            transform,
            text: text_renderer,
            theme,
            presence,
            permissions: None,
        });
        self.tick(theme);
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
        if !presence.should_cull(self.rect) {
            self.paint(&mut Frame {
                scene,
                transform,
                text: text_renderer,
                theme,
                presence,
                permissions: Some(permissions),
            });
        }
        self.tick(theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.handle_own_event(event, ctx) {
            *ctx.handled = true;
            return;
        }
        self.route_event_to_children(event, ctx);
        self.take_requests();
        self.follow_navigation();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.exposed_parts()
            .iter()
            .filter_map(|part| self.part(*part))
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for part in self.exposed_parts() {
            if let Some(widget) = self.part(part) {
                visitor(widget);
            }
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let parts = self.exposed_parts();
        let NavShell {
            sidebar,
            overview,
            content,
            inspector,
            filter,
            headers,
            toggle,
            back,
            inspector_toggle,
            history_open,
            history_close,
            ..
        } = self;
        let [h_sidebar, h_overview, h_content, h_inspector] = headers;
        let mut sidebar = sidebar.as_deref_mut();
        let mut overview = overview.as_deref_mut();
        let mut content = Some(content.as_mut());
        let mut inspector = inspector.as_deref_mut();
        let mut filter = filter.as_deref_mut();
        let mut actions = [
            h_sidebar.actions.as_deref_mut(),
            h_overview.actions.as_deref_mut(),
            h_content.actions.as_deref_mut(),
            h_inspector.actions.as_deref_mut(),
        ];
        let mut toggle = Some(toggle as &mut dyn Widget);
        let mut back = Some(back as &mut dyn Widget);
        let mut inspector_toggle = Some(inspector_toggle as &mut dyn Widget);
        let mut history_open = Some(history_open as &mut dyn Widget);
        let mut history_close = Some(history_close as &mut dyn Widget);
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(parts.len());
        for part in parts {
            let next = match part {
                Part::Body(Column::Sidebar) => sidebar.take(),
                Part::Body(Column::Overview) => overview.take(),
                Part::Body(Column::Content) => content.take(),
                Part::Body(Column::Inspector) => inspector.take(),
                Part::Actions(column) => actions.get_mut(column.index()).and_then(Option::take),
                Part::Filter => filter.take(),
                Part::Toggle => toggle.take(),
                Part::Back => back.take(),
                Part::InspectorToggle => inspector_toggle.take(),
                Part::History(false) => history_open.take(),
                Part::History(true) => history_close.take(),
            };
            if let Some(widget) = next {
                out.push(widget);
            }
        }
        out
    }

    fn name(&self) -> &'static str {
        "NavShell"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.resize.is_some() || self.geometry.resize_handle.contains(point))
            .then_some(winit::window::CursorIcon::ColResize)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The column titles are headings of the window.
        let headings: Vec<AccessNode> = Column::ALL
            .iter()
            .filter_map(|column| {
                let title = self.headers.get(column.index())?.title.clone()?;
                let header = self.geometry.header(*column);
                (header.width() > 0.0).then(|| {
                    AccessNode::new(Role::Heading { level: 2 })
                        .with_name(title)
                        .with_bounds(header)
                })
            })
            .collect();
        (!headings.is_empty()).then(|| AccessNode::new(Role::Group).with_children(headings))
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        self.for_each_owned(|child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old| old.min(deadline)));
            }
        });
        earliest
    }

    fn needs_animation_frame(&self) -> bool {
        let mut needed = self.animating || self.fade < 1.0;
        self.for_each_owned(|child| needed |= child.needs_animation_frame());
        needed
    }

    fn needs_idle_sync(&self) -> bool {
        let mut needed = self.animating;
        self.for_each_owned(|child| needed |= child.needs_idle_sync());
        needed
    }

    fn needs_redraw(&self) -> bool {
        let mut dirty = self.paint_dirty || self.animating || self.fade < 1.0;
        self.for_each_owned(|child| dirty |= child.needs_redraw());
        dirty
    }

    fn mark_drawn(&mut self) {
        self.paint_dirty = false;
        self.for_each_owned_mut(|child| child.mark_drawn());
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_requests();
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        self.for_each_owned_mut(|child| dirty |= child.take_structure_dirty());
        dirty
    }
}

/// Controls in a column the shell is not showing (a hidden sidebar, a
/// closed drawer, a screen pushed away) still belong to the window: custody,
/// invocations and availability reach every part the shell owns.
impl WindowHeader for NavShell {
    fn set_history_control(&mut self, control: Option<HistoryControl>) {
        if self.inputs.borrow().history == control {
            return;
        }
        self.inputs.borrow_mut().history = control;
        self.structure_dirty = true;
        self.invalidate();
    }
}

impl crate::widget::controls::ActionControls for NavShell {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.for_each_owned_mut(|child| {
            crate::widget::controls::install_control_custody(child, domain, attachment);
        });
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let mut taken = None;
        self.for_each_owned_mut(|child| {
            if taken.is_none() {
                taken = crate::widget::controls::take_control_invocation(child);
            }
        });
        taken
    }

    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        self.for_each_owned_mut(|child| {
            if !child.has_separate_document_owner() {
                crate::widget::controls::on_control_invocation_result(child, address, result);
            }
        });
    }

    fn has_action_control_bindings(&self) -> bool {
        let mut found = false;
        self.for_each_child(&mut |child| {
            if !found {
                found = crate::widget::controls::has_action_control_bindings(child);
            }
        });
        found
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.for_each_owned_mut(|child| {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        });
    }

    fn has_pending_control_invocation(&self) -> bool {
        let mut pending = false;
        self.for_each_owned(|child| {
            pending |= crate::widget::controls::has_pending_control_invocation(child);
        });
        pending
    }
}

impl NavShell {
    /// What the shell itself answers: the sidebar shortcut, Escape, Ctrl+F,
    /// a scrim press, and the resize drag. Returns whether it consumed
    /// `event`.
    fn handle_own_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(resize) = self.resize {
                    self.drag_sidebar_to(resize, self.last_cursor.x);
                    return true;
                }
                false
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key, ctx),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.resize.take().is_some() {
                    self.structure_dirty = true;
                    self.invalidate();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_key(&mut self, key: &Key, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        let modifiers = ctx.modifiers.state();
        let command = self.conventions.command_modifier_down(modifiers);
        if command && let Key::Character(c) = key {
            if HostShortcut::resolve(self.conventions, c, modifiers.shift_key())
                == Some(HostShortcut::Sidebar)
                && self.has_sidebar()
            {
                self.toggle_sidebar();
                return true;
            }
            if c.eq_ignore_ascii_case("f") && !modifiers.shift_key() {
                let index = self
                    .host_parts
                    .iter()
                    .position(|part| *part == Part::Filter);
                if let Some(node) = index.and_then(|i| {
                    ctx.taffy
                        .children(ctx.node_id)
                        .ok()
                        .and_then(|children| children.get(i).copied())
                }) {
                    if self.geometry.sidebar == SidebarPlacement::Drawer && !self.is_sidebar_open()
                    {
                        self.toggle_sidebar();
                    }
                    *ctx.focus_target = Some(node);
                    return true;
                }
            }
        }
        if *key == Key::Named(NamedKey::Escape) {
            if self.geometry.sidebar == SidebarPlacement::Drawer && self.is_sidebar_open() {
                self.toggle_sidebar();
                return true;
            }
            if self.geometry.inspector == InspectorPlacement::Drawer && self.is_inspector_open() {
                self.toggle_inspector();
                return true;
            }
            if self.back() {
                return true;
            }
        }
        false
    }

    fn handle_press(&mut self) -> bool {
        let p = self.last_cursor;
        let g = &self.geometry;
        if g.resize_handle.contains(p) {
            self.resize = Some(Resize {
                start_x: p.x,
                start_width: g.sidebar_column.width(),
            });
            return true;
        }
        if g.scrim.width() > 0.0 && g.scrim.contains(p) {
            let in_sidebar = g.sidebar_modal() && g.sidebar_column.contains(p);
            let in_inspector = g.inspector_modal() && g.inspector_column.contains(p);
            let on_button = [g.toggle, g.inspector_toggle, g.back, g.history]
                .iter()
                .any(|r| r.contains(p));
            if !in_sidebar && !in_inspector && !on_button {
                if g.sidebar_modal() {
                    self.toggle_sidebar();
                } else if g.inspector_modal() {
                    self.toggle_inspector();
                }
                return true;
            }
        }
        // Remember which content pane the person works in.
        if g.content_column.contains(p) {
            self.active_hint = Some(NavPane::Content);
        } else if g.overview_column.contains(p) {
            self.active_hint = Some(NavPane::Overview);
        }
        false
    }

    fn drag_sidebar_to(&mut self, resize: Resize, x: f64) {
        let width = resize.start_width + (x - resize.start_x);
        let (min, scale) = {
            let inputs = self.inputs.borrow();
            (inputs.metrics.sidebar_min, inputs.metrics.scale)
        };
        if width < min * 0.75 {
            // Dragged past its minimum: the sidebar hides.
            if self.is_sidebar_open() {
                let mut inputs = self.inputs.borrow_mut();
                inputs.sidebar.snap(false);
            }
        } else {
            let mut inputs = self.inputs.borrow_mut();
            if !inputs.sidebar.is_open(SplitLayout::Docked) {
                inputs.sidebar.snap(true);
            }
            inputs.user_sidebar = Some(width.max(min) / scale.max(f64::EPSILON));
        }
        self.update_toggle_names();
        self.structure_dirty = true;
        self.invalidate();
    }

    /// Route `event` to the shown parts, front to back. Pointer events go
    /// first to the part under the pointer, so a release or a wheel lands
    /// where it happened and never on a column beside it.
    fn route_event_to_children(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        let pointer = matches!(
            event,
            WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::TouchpadPressure { .. }
                | WindowEvent::Touch(_)
        );
        let mut order: Vec<(Part, NodeId)> = self
            .host_parts
            .iter()
            .copied()
            .zip(child_nodes)
            .rev()
            .collect();
        if pointer {
            let p = self.last_cursor;
            let under = |part: &Part| -> bool {
                let rect = match part {
                    Part::Toggle => self.geometry.toggle,
                    Part::Back => self.geometry.back,
                    Part::InspectorToggle => self.geometry.inspector_toggle,
                    Part::History(_) => self.geometry.history,
                    Part::Filter => self.geometry.filter,
                    Part::Actions(column) => self.geometry.header(*column),
                    Part::Body(column) => self.geometry.column(*column),
                };
                rect.contains(p)
            };
            order.sort_by_key(|(part, _)| !under(part));
        }
        for (part, node) in order {
            if *ctx.handled {
                break;
            }
            let path = ctx.path;
            if let Some(widget) = self.part_mut(part) {
                widget.handle_event(event, &mut ctx.reborrow(path, node));
            }
        }
    }
}

#[cfg(test)]
mod tests;
