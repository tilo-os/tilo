//! The shell's geometry for one size: which columns stand where, how the push
//! stack lays its screens, and the header row's bands.

use vello::kurbo::{Point, Rect, Vec2};

use super::inspector::keep_clear;
use super::{
    Column, ContentKind, Inputs, InspectorPlacement, Metrics, NavMode, NavPane, OverviewPlacement,
    Part, SidebarPlacement, inspector_layout_of, reaches,
};
use crate::num::f64_of_usize;
use crate::tokens::Breakpoint;
use crate::widget::flap::SplitLayout;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavGeometry {
    pub sidebar: SidebarPlacement,
    pub inspector: InspectorPlacement,
    pub overview: OverviewPlacement,
    pub side_by_side: bool,
    pub stack: Vec<NavPane>,
    pub header_height: f64,
    pub breakpoint: f64,
    /// The class the rect this geometry was solved in asks for (`Breakpoint`, hysteresis held
    /// across passes); read it instead of comparing a width to a number.
    pub size_class: Breakpoint,
    /// The size of the rect this plan was solved for. Two passes that differ here are a resize, and
    /// a plan that switches between them lands at once.
    pub size: Vec2,
    /// Each column's visible area (header and body); a drawer's panel.
    pub sidebar_column: Rect,
    pub overview_column: Rect,
    pub content_column: Rect,
    pub inspector_column: Rect,
    pub rail: Rect,
    pub(super) bodies: [Rect; 4],
    pub(super) headers: [Rect; 4],
    pub filter: Rect,
    pub controls: Rect,
    pub toggle: Rect,
    pub back: Rect,
    pub inspector_toggle: Rect,
    pub history: Rect,
    /// Where the window's controls stand at the trailing end of the row,
    /// while the host merged the window's bar into it; else zero.
    pub window_controls: Rect,
    pub window_row: Rect,
    pub band: Rect,
    pub(super) toggle_column: Option<Column>,
    pub(super) back_column: Option<Column>,
    pub(super) inspector_toggle_column: Option<Column>,
    pub(super) history_column: Option<Column>,
    pub resize_handle: Rect,
    pub sheet: crate::widget::SheetFrame,
    pub sheet_covered: f64,
    /// The content edges of a centred page column (its leading and trailing
    /// text edge), where the content is one ([`NavShell::with_page_column`]).
    pub(super) page_edges: Option<(f64, f64)>,
    pub(super) sidebar_rail: bool,
}

impl NavGeometry {
    pub(super) fn column(&self, column: Column) -> Rect {
        match column {
            Column::Sidebar => self.sidebar_column,
            Column::Overview => self.overview_column,
            Column::Content => self.content_column,
            Column::Inspector => self.inspector_column,
        }
    }

    pub(super) fn body(&self, column: Column) -> Rect {
        self.bodies
            .get(column.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    pub(super) fn header(&self, column: Column) -> Rect {
        self.headers
            .get(column.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    #[must_use]
    pub fn sidebar_header(&self) -> Rect {
        self.header(Column::Sidebar)
    }

    #[must_use]
    pub fn content_header(&self) -> Rect {
        self.header(Column::Content)
    }

    #[must_use]
    pub fn content_body(&self) -> Rect {
        self.body(Column::Content)
    }

    pub(super) fn translated(&self, by: Vec2) -> Self {
        let t = |r: Rect| if r == Rect::ZERO { r } else { r + by };
        Self {
            sidebar_column: t(self.sidebar_column),
            overview_column: t(self.overview_column),
            content_column: t(self.content_column),
            inspector_column: t(self.inspector_column),
            rail: t(self.rail),
            bodies: self.bodies.map(t),
            headers: self.headers.map(t),
            filter: t(self.filter),
            controls: t(self.controls),
            toggle: t(self.toggle),
            back: t(self.back),
            inspector_toggle: t(self.inspector_toggle),
            history: t(self.history),
            window_controls: t(self.window_controls),
            window_row: t(self.window_row),
            band: t(self.band),
            resize_handle: t(self.resize_handle),
            sheet: crate::widget::SheetFrame {
                area: t(self.sheet.area),
                ..self.sheet
            },
            page_edges: self.page_edges.map(|(x0, x1)| (x0 + by.x, x1 + by.x)),
            ..self.clone()
        }
    }

    /// The parts this geometry shows, in build order (back to front). Behind
    /// an open drawer the other columns stay: laid out and painted, inert.
    pub(super) fn visible_parts(&self, inputs: &Inputs) -> Vec<Part> {
        let mut parts = Vec::new();
        let shows = |r: Rect| r.width() > 0.0 && r.height() > 0.0;
        if inputs.has_rail && shows(self.rail) {
            parts.push(Part::Rail);
        }
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            let present = match column {
                Column::Sidebar => inputs.has_sidebar,
                Column::Overview => inputs.has_overview,
                Column::Content => inputs.has_content,
                Column::Inspector => inputs.has_inspector,
            };
            if !present || !shows(self.column(column)) {
                continue;
            }
            parts.push(Part::Body(column));
            let i = column.index();
            if shows(self.header(column)) {
                let rail = column == Column::Sidebar
                    && inputs.sidebar_reads_as_rail(self.body(column).width());
                if inputs.titled.get(i).copied().unwrap_or(false) && !rail {
                    parts.push(Part::Title(column));
                }
                if inputs.leading.get(i).copied().unwrap_or(false) {
                    parts.push(Part::Leading(column));
                }
                let actions = inputs.actions.get(i).copied().unwrap_or(0);
                for action in 0..actions {
                    parts.push(Part::Action(column, action));
                }
                if actions > 0 {
                    parts.push(Part::More(column));
                }
            }
            if inputs.has_filter && self.filter_column(inputs) == Some(column) && shows(self.filter)
            {
                parts.push(Part::Filter);
            }
            if inputs.has_controls && column == Column::Overview && shows(self.controls) {
                parts.push(Part::Controls);
            }
        }
        if shows(self.back) {
            parts.push(Part::Back);
        }
        if shows(self.inspector_toggle) {
            parts.push(Part::InspectorToggle);
        }
        if let Some(control) = inputs.history
            && shows(self.history)
        {
            parts.push(Part::History(control.is_open()));
        }
        if inputs.status_action && shows(self.header(self.status_column())) {
            parts.push(Part::StatusAction);
        }
        if shows(self.toggle) {
            parts.push(Part::Toggle);
        }
        if inputs.band.has {
            parts.push(Part::Band);
        }
        parts
    }

    /// The column whose header row carries the window's sync status: the one
    /// History stands in, reaching the window's trailing edge.
    pub(super) fn status_column(&self) -> Column {
        self.history_column.unwrap_or(Column::Content)
    }

    /// Whether `p` is on the header row, which stays live beside a drawer.
    pub(super) fn window_row_contains(&self, p: Point) -> bool {
        self.header_height > 0.0
            && Column::ALL
                .iter()
                .any(|column| self.header(*column).contains(p))
            || self.toggle.contains(p)
            || self.back.contains(p)
            || self.inspector_toggle.contains(p)
            || self.history.contains(p)
    }

    /// An open drawer from either edge. A drawer is never modal: the content beside it stays
    /// visible and live, and a press on it puts the drawer away.
    pub(super) fn open_drawer(&self) -> Option<Column> {
        [Column::Sidebar, Column::Inspector]
            .into_iter()
            .find(|column| self.floating(*column) && self.column(*column).width() > 0.0)
    }

    pub(super) fn floating(&self, column: Column) -> bool {
        match column {
            Column::Sidebar => self.sidebar == SidebarPlacement::Drawer,
            Column::Inspector => self.inspector == InspectorPlacement::Sheet,
            Column::Overview | Column::Content => false,
        }
    }

    pub(super) fn filter_column(&self, inputs: &Inputs) -> Option<Column> {
        if inputs.has_sidebar {
            Some(Column::Sidebar)
        } else if inputs.has_overview {
            Some(Column::Overview)
        } else if inputs.has_content {
            Some(Column::Content)
        } else {
            None
        }
    }

    /// Where the title (or, without one, the leading fill) of `column`'s header starts: after the
    /// shell's leading buttons in it, else at the content gutter — the same edge the column's
    /// rows and any tree's chevron column stand on, so a title lines up with what it names in a
    /// sidebar as in a page.
    pub(super) fn lead_end(&self, column: Column, m: &Metrics) -> f64 {
        let header = self.header(column);
        let mut x = header.x0 + m.row.gutter;
        for (button, home) in [
            (self.toggle, self.toggle_column),
            (self.back, self.back_column),
        ] {
            if home == Some(column) && button.width() > 0.0 {
                x = x.max(button.x1 + m.gap);
            }
        }
        x
    }

    /// The trailing end of `column`'s actions: before the shell's trailing buttons, or a content
    /// gutter in where the row reaches the window's edge.
    pub(super) fn trail_start(&self, column: Column, m: &Metrics) -> f64 {
        let band = self.actions_band(column);
        let edge = if column == Column::Sidebar || band.x1 < self.header(column).x1 - 0.5 {
            m.inset
        } else {
            m.row.gutter
        };
        band.x1 - edge
    }

    /// The run of a header the app's actions may take: from the header's
    /// start to before the trailing shell buttons.
    pub(super) fn actions_band(&self, column: Column) -> Rect {
        let header = self.header(column);
        if header == Rect::ZERO {
            return Rect::ZERO;
        }
        let mut x1 = header.x1;
        if self.window_controls.width() > 0.0 && header.x1 >= self.window_controls.x1 {
            x1 = x1.min(self.window_controls.x0);
        }
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

/// The content edges of a page column `lines` lines of body text wide, centred in `column`: the
/// scroller keeps its bar's gutter on the trailing edge and the page's padding applies to each
/// side.
pub(super) fn page_edges(column: Rect, lines: f64, m: &Metrics) -> Option<(f64, f64)> {
    if column.width() <= 0.0 {
        return None;
    }
    let room = (column.width() - m.scroll_gutter).max(0.0);
    let width = (m.body_line * lines).round().min(room);
    let x0 = column.x0 + (room - width) / 2.0 + m.page_pad;
    let x1 = (x0 + width - 2.0 * m.page_pad).max(x0);
    Some((x0, x1))
}

/// Compute the geometry for a shell of size `w × h`.
#[allow(
    clippy::too_many_lines,
    reason = "the responsive plan is one ordered decision: sidebar, inspector, split, stack, then rects"
)]
pub(super) fn plan_columns(inputs: &Inputs, w: f64, h: f64) -> NavGeometry {
    let m = inputs.metrics;
    let mut g = NavGeometry::default();
    if w <= 0.0 || h <= 0.0 {
        return g;
    }
    let ow = if inputs.has_overview { m.overview } else { 0.0 };
    let iw = if inputs.has_inspector {
        m.inspector
    } else {
        0.0
    };
    let held = inputs.held;
    let band = m.hysteresis;
    let content_min = inputs.content_min();
    let content_need = ow + content_min;
    let beside_need = ow + inputs.content_beside_sidebar_min();
    let rail_sidebar = inputs.mode == NavMode::Docked;

    g.size = Vec2::new(w, h);

    let preferred = inputs.sidebar_width().min(w);
    let narrowest = if rail_sidebar {
        preferred
    } else {
        m.sidebar_min.min(preferred)
    };
    g.breakpoint = if inputs.has_sidebar && !rail_sidebar {
        narrowest + beside_need
    } else {
        0.0
    };
    let docks = rail_sidebar || reaches(w, g.breakpoint, held.sidebar_docked, band);
    g.sidebar = if !inputs.has_sidebar || (inputs.floats && !docks) {
        SidebarPlacement::None
    } else if docks {
        SidebarPlacement::Docked
    } else if inputs.kind.is_content_primary() {
        SidebarPlacement::Drawer
    } else {
        SidebarPlacement::Screen
    };
    let sw = if g.sidebar == SidebarPlacement::Docked && !rail_sidebar {
        preferred.min(w - beside_need).max(narrowest)
    } else {
        preferred
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

    // 2. The inspector docks while the pane it leaves keeps the content's width; narrower, it
    // is a sheet up from the window's foot.
    let between = if g.sidebar == SidebarPlacement::Docked {
        beside_need
    } else {
        content_need
    };
    g.inspector = if !inputs.has_inspector {
        InspectorPlacement::None
    } else if reaches(w - docked_target, between + iw, held.inspector_docked, band) {
        InspectorPlacement::Docked
    } else {
        InspectorPlacement::Sheet
    };
    let inspector_layout = inspector_layout_of(g.inspector);
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

    let region_target = w - docked_target - inspector_target;
    let beside_sidebar = g.sidebar == SidebarPlacement::Docked && !rail_sidebar;
    let list_fits = if beside_sidebar {
        region_target >= content_need - 1e-6
    } else {
        reaches(region_target, content_need, held.list_fits, band)
    };
    let rail_fits =
        inputs.has_rail && reaches(region_target, m.rail + content_min, held.rail_fits, band);
    g.overview = if !inputs.has_overview {
        OverviewPlacement::None
    } else if inputs.has_content && !list_fits && rail_fits {
        OverviewPlacement::Rail
    } else {
        OverviewPlacement::List
    };
    g.side_by_side = !inputs.has_overview || list_fits || rail_fits;

    // 4. The push stack.
    let mut stack = Vec::new();
    if g.sidebar == SidebarPlacement::Screen {
        stack.push(NavPane::Sidebar);
    }
    if inputs.has_overview && !g.side_by_side {
        stack.push(NavPane::Overview);
    }
    if inputs.has_content {
        stack.push(NavPane::Content);
    } else if inputs.has_overview && g.side_by_side {
        stack.push(NavPane::Overview);
    }
    let front = stack_index(&stack, inputs.pane);
    let pos = inputs.push_pos.map_or(f64_of_usize(front), |pos| {
        pos.clamp(0.0, f64_of_usize(stack.len().saturating_sub(1)))
    });
    g.stack = if stack.len() > 1 {
        stack.clone()
    } else {
        Vec::new()
    };
    // A shell that declares its own buttons as the bar's slots has no toggle in a row of its
    // own, and the places its toggle reaches are no screen the push stack drills through.
    let first = usize::from(inputs.toggle_in_bar && stack.first() == Some(&NavPane::Sidebar));
    let drills = stack.len().saturating_sub(first) > 1;

    let in_row = |column: Column| match column {
        Column::Sidebar => g.sidebar != SidebarPlacement::Drawer,
        Column::Inspector => g.inspector != InspectorPlacement::Sheet,
        Column::Overview | Column::Content => true,
    };
    let shell_row = (inputs.has_sidebar && !inputs.toggle_in_bar)
        || inputs.has_inspector
        || inputs.history.is_some()
        || inputs.window_controls.is_some()
        || (!inputs.toggle_in_bar && (inputs.content_back || drills));
    let needs_header = !inputs.toolbar_in_bar
        && (shell_row
            || Column::ALL
                .iter()
                .any(|c| in_row(*c) && inputs.header_wanted(*c)));
    // Each column's header is the app's own — its title and the buttons that act on what it
    // shows — never the window's: it stands at the top of its column at every time, and the
    // window's controls come into the room the row keeps for them.
    let sidebar_reads_as_rail = match g.sidebar {
        SidebarPlacement::Docked => inputs.sidebar_reads_as_rail(sw),
        SidebarPlacement::Screen => inputs.sidebar_reads_as_rail(w),
        SidebarPlacement::Drawer => inputs.sidebar_reads_as_rail(m.drawer.min(w)),
        SidebarPlacement::None => false,
    };
    let sidebar_row = inputs.has_sidebar
        && !sidebar_reads_as_rail
        && (inputs.header_wanted(Column::Sidebar) || (shell_row && !inputs.toolbar_in_bar));
    let sidebar_hh = if sidebar_row { m.header.min(h) } else { 0.0 };
    let rows = |column: Column| match column {
        // The shell's own buttons stand in the sidebar's row, which the bar never takes.
        Column::Sidebar => sidebar_row,
        _ => shell_row || inputs.header_wanted(column),
    };
    g.sidebar_rail = inputs.has_sidebar && sidebar_reads_as_rail;
    let drawer_hh = m.header.min(h);
    let row_h = if needs_header { m.header.min(h) } else { 0.0 };
    g.header_height = row_h;
    let hh = row_h;
    let row_of = |column: Column| match column {
        _ if !rows(column) => 0.0,
        Column::Sidebar => sidebar_hh,
        _ => hh,
    };
    let header_of = |col: Rect, column: Column| {
        let row = row_of(column);
        if row > 0.0 && col.width() > 0.0 {
            Rect::new(col.x0, col.y0, col.x1, col.y0 + row)
        } else {
            Rect::ZERO
        }
    };
    let drawer_top = if hh > 0.0 { hh + m.gap } else { m.gap };
    let drawer_bottom = (h - m.gap).max(drawer_top);

    let sidebar_moving = g.sidebar == SidebarPlacement::Docked
        && !rail_sidebar
        && inputs.sidebar.is_animating(sidebar_layout);
    let inspector_moving = g.inspector == InspectorPlacement::Docked
        && inputs.inspector.is_animating(inspector_layout);
    let settled_w = (w - inspector_now - docked_now).max(0.0);
    let widest_w = w
        - if sidebar_moving { 0.0 } else { docked_now }
        - if inspector_moving { 0.0 } else { inspector_now };
    let region_w = crate::motion::PanelSlide::content_width(
        settled_w,
        widest_w.max(0.0),
        sidebar_moving || inspector_moving,
    );
    let region = Rect::new(docked_now, 0.0, docked_now + region_w, h);
    let region_w = region.width();
    let screen_x = |pane: NavPane| -> f64 {
        let i = stack_index(&stack, pane);
        (f64_of_usize(i) - pos) * w
    };
    let mut sidebar_body = Rect::ZERO;
    let mut sidebar_header = Rect::ZERO;
    match g.sidebar {
        SidebarPlacement::None => {}
        SidebarPlacement::Docked => {
            if docked_now > 0.0 {
                g.sidebar_column = Rect::new(0.0, 0.0, docked_now, h);
                // The body keeps its width and slides under the edge, so its
                // rows never reflow while it collapses.
                sidebar_body = Rect::new(docked_now - sw, row_of(Column::Sidebar), docked_now, h);
                sidebar_header = header_of(g.sidebar_column, Column::Sidebar);
            }
        }
        SidebarPlacement::Drawer => {
            if sidebar_reveal > 0.001 {
                let dw = m.drawer.min((w - m.gap - m.drawer_dismiss_band).max(0.0));
                let x0 = m.gap - (dw + m.gap) * (1.0 - sidebar_reveal);
                g.sidebar_column = Rect::new(x0, drawer_top, x0 + dw, drawer_bottom);
                (sidebar_header, sidebar_body) = drawer_split(
                    g.sidebar_column,
                    sidebar_hh,
                    inputs.header_wanted(Column::Sidebar),
                );
            }
        }
        SidebarPlacement::Screen => {
            let x = screen_x(NavPane::Sidebar);
            if x > -w && x < w {
                g.sidebar_column = Rect::new(x, 0.0, x + w, h);
                sidebar_body = Rect::new(x, row_of(Column::Sidebar), x + w, h);
                sidebar_header = header_of(g.sidebar_column, Column::Sidebar);
            }
        }
    }
    let reading = m.reading_measure + m.scroll_gutter;
    let roomy = ow + (region_w - reading - ow).max(0.0);
    let widest_list = roomy.min(m.overview_max);
    if g.side_by_side {
        let x = region.x0 + screen_x(NavPane::Content);
        let lead = match g.overview {
            OverviewPlacement::List if !inputs.has_content => region_w,
            OverviewPlacement::List => widest_list.min((region_w - content_min).max(0.0)),
            OverviewPlacement::Rail => m.rail.min(region_w),
            OverviewPlacement::None => 0.0,
        };
        match g.overview {
            OverviewPlacement::List => g.overview_column = Rect::new(x, 0.0, x + lead, h),
            OverviewPlacement::Rail => g.rail = Rect::new(x, 0.0, x + lead, h),
            OverviewPlacement::None => {}
        }
        if inputs.has_content {
            g.content_column = Rect::new(x + lead, 0.0, x + region_w, h);
        }
    } else {
        let ox = region.x0 + screen_x(NavPane::Overview);
        let cx = region.x0 + screen_x(NavPane::Content);
        if ox > -w && ox < w {
            g.overview_column = Rect::new(ox, 0.0, ox + region_w, h);
        }
        if inputs.has_content && cx > -w && cx < w {
            g.content_column = Rect::new(cx, 0.0, cx + region_w, h);
        }
    }
    let mut inspector_body = Rect::ZERO;
    let mut inspector_header = Rect::ZERO;
    match g.inspector {
        InspectorPlacement::None => {}
        InspectorPlacement::Docked => {
            if inspector_now > 0.0 {
                g.inspector_column = Rect::new(w - inspector_now, 0.0, w, h);
                inspector_body = Rect::new(
                    w - inspector_now,
                    row_of(Column::Inspector),
                    w - inspector_now + iw,
                    h,
                );
                inspector_header = header_of(g.inspector_column, Column::Inspector);
            }
        }
        InspectorPlacement::Sheet => {
            let header = inputs.header_wanted(Column::Inspector);
            g.sheet = crate::widget::SheetFrame {
                area: Rect::new(0.0, drawer_top, w, h),
                peek: if header {
                    drawer_hh.max(m.header)
                } else {
                    m.header
                },
                grip: m.pointer,
                least_half: m.sheet_least_half,
            };
            let sheet = inputs
                .sheet
                .geometry(g.sheet, inspector_reveal, inspector_open);
            g.inspector_column = sheet.panel;
            g.sheet_covered = sheet.covered;
            (inspector_header, inspector_body) = drawer_split(sheet.content, drawer_hh, header);
        }
    }

    // 7. Headers and bodies.
    g.headers = [
        sidebar_header,
        header_of(g.overview_column, Column::Overview),
        header_of(g.content_column, Column::Content),
        inspector_header,
    ];
    let body_of = |col: Rect, column: Column| {
        if col.width() > 0.0 {
            Rect::new(col.x0, col.y0 + row_of(column), col.x1, col.y1)
        } else {
            Rect::ZERO
        }
    };
    g.bodies = [
        sidebar_body,
        body_of(g.overview_column, Column::Overview),
        body_of(g.content_column, Column::Content),
        inspector_body,
    ];
    if g.rail.width() > 0.0 {
        g.rail = Rect::new(g.rail.x0, hh, g.rail.x1, g.rail.y1);
    }
    keep_clear(&mut g, h);
    g.page_edges = inputs
        .page_lines
        .and_then(|lines| page_edges(g.content_column, lines, &m));

    let take_band = |g: &mut NavGeometry, column: Column| -> Rect {
        let body = g.body(column);
        if g.column(column).width() <= 0.0 || body.height() <= 0.0 {
            return Rect::ZERO;
        }
        let top = (body.y0 + m.gap).min(body.y1);
        let (x0, x1) = match (column, g.page_edges) {
            (Column::Content, Some(edges)) => edges,
            _ => (body.x0 + m.inset, body.x1 - m.inset),
        };
        let band = Rect::new(x0, top, x1.max(x0), top + m.filter);
        if let Some(slot) = g.bodies.get_mut(column.index()) {
            slot.y0 = (top + m.filter + m.gap).min(slot.y1);
        }
        band
    };
    if inputs.has_filter
        && let Some(column) = g.filter_column(inputs)
    {
        g.filter = take_band(&mut g, column);
    }
    if inputs.has_controls && inputs.has_overview {
        g.controls = take_band(&mut g, Column::Overview);
    }

    // 9. The shell's own buttons.

    let front_pane = stack.get(front).copied().unwrap_or(NavPane::Content);
    let front_column = match front_pane {
        NavPane::Sidebar => Column::Sidebar,
        NavPane::Overview => Column::Overview,
        NavPane::Content if inputs.has_overview && g.overview_column.width() > 0.0 => {
            Column::Overview
        }
        NavPane::Content => Column::Content,
    };
    // Back has a step where the front screen is not the one the window opens on: one screen back
    // from a screen deeper than it, and forward to it from the sidebar's screen, which stands
    // before it and would otherwise be a dead end.
    let opening = back_home(inputs, &stack);
    let push_back =
        front > opening || (front < opening && stack.first() == Some(&NavPane::Sidebar));
    let window_row = Rect::new(0.0, 0.0, w, hh);
    let back_column = if push_back {
        Some(front_column)
    } else if inputs.content_back && g.content_column.width() > 0.0 {
        Some(Column::Content)
    } else {
        None
    };
    let back_leads = back_column.is_some() && !sidebar_open;
    let toggle_leads = match g.sidebar {
        SidebarPlacement::Docked | SidebarPlacement::Drawer => true,
        SidebarPlacement::Screen => front_pane != NavPane::Sidebar,
        SidebarPlacement::None => false,
    };
    if hh > 0.0
        && !push_back
        && !back_leads
        && inputs.has_sidebar
        && !inputs.toggle_in_bar
        && toggle_leads
    {
        // Pinned to the window's top-leading corner, on the sidebar while it shows docked and on
        // the next column otherwise, so it never chases a sliding edge.
        g.toggle = m.row.leading_chip(window_row, 0);
        g.toggle_column = Some(
            if g.sidebar == SidebarPlacement::Docked && g.sidebar_column.x1 >= g.toggle.x1 {
                Column::Sidebar
            } else {
                front_column
            },
        );
    }
    if let Some(column) = back_column
        && inputs.toolbar_in_bar
    {
        g.back_column = Some(column);
    } else if let Some(column) = back_column
        && (push_back || g.header(column) != Rect::ZERO)
    {
        let slot = usize::from(g.toggle_column == Some(column) && g.toggle.width() > 0.0);
        // The page moves; navigation belongs to the window. Back must stay
        // operable even before the incoming screen enters the viewport.
        let row = if push_back {
            window_row
        } else {
            g.header(column)
        };
        let drawer_open = g.sidebar == SidebarPlacement::Drawer && sidebar_open;
        g.back = if drawer_open {
            Rect::ZERO
        } else if slot == 0 {
            m.row.leading_chip(row, 0)
        } else {
            let x = g.toggle.x1 + m.gap;
            Rect::new(x, g.toggle.y0, x + m.chip, g.toggle.y1)
        };
        g.back_column = Some(column);
    }
    if let Some(width) = inputs.window_controls
        && row_h > 0.0
    {
        g.window_row = Rect::new(0.0, 0.0, w, row_h);
        let x1 = (w - m.inset).max(0.0);
        g.window_controls = Rect::new((x1 - width).max(0.0), 0.0, x1, row_h);
    }
    let trailing_row = |g: &NavGeometry, header: Rect| -> Rect {
        if g.window_controls.width() > 0.0 && header.x1 >= g.window_controls.x1 {
            Rect::new(
                header.x0,
                header.y0,
                (g.window_controls.x0 - m.gap + m.inset).max(header.x0),
                header.y1,
            )
        } else {
            header
        }
    };
    let trailing_edge = |column: Column, row: Rect| -> f64 {
        if column == Column::Sidebar || row.x1 < w - 0.5 {
            m.inset
        } else {
            m.row.gutter
        }
    };
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
            !g.floating(*column)
                && g.column(*column).width() > 0.0
                && header.x0 < w - 0.5
                && header.x1 >= w - 0.5
        })
    {
        g.history_column = Some(column);
        let row = trailing_row(&g, g.header(column));
        g.history = m.row.trailing_chip_from(row, 0, trailing_edge(column, row));
    }
    if inputs.has_inspector && hh > 0.0 {
        // Docked and open, the toggle heads the inspector; otherwise it
        // stands in the content's header (a drawer opens under it).
        let column = if g.inspector == InspectorPlacement::Docked && inspector_open {
            Column::Inspector
        } else if g.content_column.width() > 0.0 {
            Column::Content
        } else {
            front_column
        };
        let row = g.header(column);
        if row != Rect::ZERO && g.column(column).width() > 0.0 {
            g.inspector_toggle_column = Some(column);
            let slot = usize::from(g.history_column == Some(column));
            let chip_row = trailing_row(&g, row);
            g.inspector_toggle =
                m.row
                    .trailing_chip_from(chip_row, slot, trailing_edge(column, chip_row));
        }
    }
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
        g.resize_handle = Rect::new(
            x - m.resize_grip,
            row_of(Column::Sidebar),
            x + m.resize_grip,
            h,
        );
    }
    g
}

pub(super) fn drawer_split(col: Rect, hh: f64, header: bool) -> (Rect, Rect) {
    if col.width() <= 0.0 {
        return (Rect::ZERO, Rect::ZERO);
    }
    if header && hh > 0.0 {
        let y1 = (col.y0 + hh).min(col.y1);
        (
            Rect::new(col.x0, col.y0, col.x1, y1),
            Rect::new(col.x0, y1, col.x1, col.y1),
        )
    } else {
        (Rect::ZERO, col)
    }
}

pub(super) fn layout_of(sidebar: SidebarPlacement) -> SplitLayout {
    match sidebar {
        SidebarPlacement::Drawer | SidebarPlacement::Screen => SplitLayout::Overlay,
        SidebarPlacement::Docked | SidebarPlacement::None => SplitLayout::Docked,
    }
}

/// The first list of items the stack shows: the overview where there is one, the content where
/// the content is itself a list or a whole-window empty state stands in it.
fn list_pane(inputs: &Inputs, stack: &[NavPane]) -> Option<usize> {
    if let Some(overview) = stack.iter().position(|pane| *pane == NavPane::Overview) {
        Some(overview)
    } else if inputs.kind == ContentKind::List || inputs.nothing_listed {
        Some(stack_index(stack, NavPane::Content))
    } else {
        None
    }
}

/// The screen a new window opens on: its first list of items, else the first screen past the
/// places where the bar's toggle reaches them (a conversation beside its channels: the places give
/// way to the content they chose), else the first screen of the stack (Settings' list, a library
/// of shortcuts).
pub(super) fn opening_pane(inputs: &Inputs, stack: &[NavPane]) -> usize {
    back_home(inputs, stack)
}

/// The screen a narrow window's Back walks toward: the window's first list of items — the
/// screen it opens on — else the first screen of the stack once the sidebar is no longer one of
/// them, because a window whose sidebar the bar's toggle reaches opens on the screen after the
/// places.
pub(super) fn back_home(inputs: &Inputs, stack: &[NavPane]) -> usize {
    list_pane(inputs, stack).unwrap_or_else(|| {
        let past_places =
            usize::from(inputs.toggle_in_bar && stack.first() == Some(&NavPane::Sidebar));
        past_places.min(stack.len().saturating_sub(1))
    })
}

pub(super) fn stack_index(stack: &[NavPane], pane: NavPane) -> usize {
    stack
        .iter()
        .position(|p| *p == pane)
        .or_else(|| {
            (pane == NavPane::Overview)
                .then(|| stack.iter().position(|p| *p == NavPane::Content))
                .flatten()
        })
        .unwrap_or(stack.len().saturating_sub(1))
}
