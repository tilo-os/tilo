//! The collapsible bottom panel: a tabbed dock for Problems and Terminal.
//! Problems lists diagnostics — the local linter merged with whatever the
//! language server has published (see `app.rs`). Terminal hosts a live
//! embedded shell, spawned on first open, and shows an honest empty-state
//! note until then (never a fabricated build log).
//!
//! The tabs are the toolkit's [`TabBar`], the same strip as the document
//! tabs above it, and the diagnostics are retained [`ListRow`]s in a
//! [`Section`] inside the toolkit's scroll view, so hover, activation, focus
//! and scrolling are the toolkit's. The panel renders the data the app feeds
//! it and reports what the user chose; it owns no filesystem or
//! language-server state of its own.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::Rhythm;
use ui::widget::TaffyTreeExt;
use ui::widget::{
    Container, Element, EmptyState, EmptyStateContent, ListRow, Scrollable, Section, TabBar,
    TabDescriptor, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

/// Rows of diagnostics the panel shows before it scrolls.
const VISIBLE_ROWS: usize = 4;

/// Which dock tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelTab {
    Problems,
    Terminal,
    Search,
}

impl PanelTab {
    const fn id(self) -> u64 {
        match self {
            Self::Problems => 1,
            Self::Terminal => 2,
            Self::Search => 3,
        }
    }

    fn from_id(id: u64) -> Option<Self> {
        match id {
            1 => Some(Self::Problems),
            2 => Some(Self::Terminal),
            3 => Some(Self::Search),
            _ => None,
        }
    }
}

/// One row in the Search tab: everything the panel needs to draw a hit and
/// nothing about where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitRow {
    pub rel: String,
    pub line: usize,
    pub preview: String,
    /// Character offset and length of the match *within `preview`*, so the
    /// highlight lands on the matched text after the preview was clipped.
    pub highlight: (usize, usize),
}

/// What the Search tab is currently showing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchState {
    pub query: String,
    pub rows: Vec<HitRow>,
    /// True while the worker is still walking the tree.
    pub running: bool,
    /// True when the bounded walk stopped before it covered the whole project.
    pub truncated: bool,
    pub files_scanned: usize,
}

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A hard error (LSP severity 1).
    Error,
    Warning,
    Info,
    Hint,
}

/// One diagnostic row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent flag (a tab choice, a close request, a live terminal, a redraw) on the panel's state, not a set of options that combine"
)]
pub struct BottomPanel {
    rect: Rect,
    tab: PanelTab,
    problems: Vec<Diagnostic>,
    search: SearchState,
    tabs: TabBar,
    tabs_node: Option<NodeId>,
    /// Set when the user picks a tab — read and cleared by the app.
    tab_changed: bool,
    /// Set when the user closes a tab, which closes the whole panel.
    close_requested: bool,
    /// When true, the Terminal tab's body is owned by an embedded live
    /// `TerminalWidget` the app draws over this panel's content rect, so the
    /// panel leaves that region blank instead of rendering the static fallback.
    has_live_terminal: bool,
    /// Note shown in the Terminal body when no live terminal is present (the
    /// pre-spawn invitation, or "shell unavailable" on a spawn failure).
    terminal_note: Option<String>,
    /// What the language server is doing, when it has not finished. While this
    /// is `Some`, an empty Problems list means "not analysed yet" rather than
    /// "analysed and clean", and the panel must say so — see
    /// [`BottomPanel::set_analysis`].
    analysis: Option<String>,
    /// The diagnostics as a scrolling list of rows; `None` while there are
    /// none.
    list: Option<Scrollable>,
    list_node: Option<NodeId>,
    /// The scroll position, kept when the list is rebuilt for new problems.
    list_offset: Rc<Cell<(f32, f32)>>,
    /// The index of the row the user activated, read and cleared by the app.
    activated: Rc<Cell<Option<usize>>>,
    /// The tab strip's height, cached from layout.
    strip_height: f64,
    /// The panel's own height, from the theme at layout time.
    height: f64,
    dirty: bool,
}

impl BottomPanel {
    pub fn new() -> Self {
        let mut panel = BottomPanel {
            rect: Rect::ZERO,
            tab: PanelTab::Problems,
            problems: Vec::new(),
            search: SearchState::default(),
            tabs: Self::fresh_tabs(PanelTab::Problems, 0),
            tabs_node: None,
            tab_changed: false,
            close_requested: false,
            has_live_terminal: false,
            terminal_note: None,
            analysis: None,
            list: None,
            list_node: None,
            list_offset: Rc::new(Cell::new((0.0, 0.0))),
            activated: Rc::new(Cell::new(None)),
            strip_height: 0.0,
            height: 0.0,
            dirty: true,
        };
        panel.refresh_tab_titles();
        panel
    }

    /// A tab strip over the two dock tabs.
    fn fresh_tabs(active: PanelTab, problems: usize) -> TabBar {
        let mut tabs = TabBar::new(
            "code-panel",
            vec![
                TabDescriptor::new(PanelTab::Problems.id(), Self::problems_title(problems)),
                TabDescriptor::new(PanelTab::Terminal.id(), "Terminal"),
                TabDescriptor::new(PanelTab::Search.id(), "Search results"),
            ],
            active.id(),
        )
        // Two fixed views: nothing to add, and the dock closes as a whole.
        .closable(false)
        .without_new_tab();
        tabs.set_show_maximize(false);
        tabs
    }

    fn problems_title(count: usize) -> String {
        if count == 0 {
            "Problems".to_string()
        } else {
            format!("Problems ({count})")
        }
    }

    fn refresh_tab_titles(&mut self) {
        let title = Self::problems_title(self.problems.len());
        if let Some(tab) = self
            .tabs
            .tabs_mut()
            .iter_mut()
            .find(|tab| tab.id == PanelTab::Problems.id())
            && tab.title != title
        {
            tab.title = title;
        }
    }

    /// The height this panel wants: its tab strip plus a few rows.
    fn wanted_height(theme: &Theme) -> f64 {
        let spacing = &theme.tokens.spacing;
        let row = f64::from(theme.tokens.metrics.min_pointer_target + spacing.s2);
        ui::widget::tabs::TAB_BAR_HEIGHT + VISIBLE_ROWS as f64 * row
    }

    /// Replace the Problems list.
    pub fn set_problems(&mut self, problems: Vec<Diagnostic>) {
        if self.problems != problems {
            self.problems = problems;
            self.rebuild_list();
            self.refresh_tab_titles();
            self.dirty = true;
        }
    }

    /// One retained row per diagnostic, in a scroll view that keeps its
    /// position across rebuilds.
    fn rebuild_list(&mut self) {
        self.list_node = None;
        if self.tab == PanelTab::Search {
            let rows: Vec<Element> = self
                .search
                .rows
                .iter()
                .enumerate()
                .map(|(index, hit)| {
                    let activated = Rc::clone(&self.activated);
                    Box::new(
                        ListRow::new(format!("search-{index}"), &hit.preview)
                            .meta(format!("{}:{}", hit.rel, hit.line))
                            .on_view(move || activated.set(Some(index))),
                    ) as Element
                })
                .collect();
            self.list = Some(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_persisted_offset(Rc::clone(&self.list_offset))
                    .with_child(Box::new(
                        Container::new()
                            .with_inset(Rhythm::Section)
                            .with_child(Box::new(Section::titled(self.search_summary(), rows))),
                    )),
            );
            return;
        }
        if self.problems.is_empty() {
            self.list = None;
            self.list_offset.set((0.0, 0.0));
            return;
        }
        let rows: Vec<Element> = self
            .problems
            .iter()
            .enumerate()
            .map(|(index, d)| {
                let activated = Rc::clone(&self.activated);
                let row = ListRow::new(format!("problem-{index}"), d.message.clone())
                    .meta(format!("Ln {}, Col {}", d.line, d.col))
                    .on_view(move || activated.set(Some(index)));
                let row = match d.severity {
                    Severity::Error => row.icon(ui::icons::Icon::Ban).error(),
                    Severity::Warning => row.icon(ui::icons::Icon::TriangleAlert).warning(),
                    Severity::Info | Severity::Hint => row.icon(ui::icons::Icon::Info),
                };
                Box::new(row) as Element
            })
            .collect();
        self.list = Some(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_persisted_offset(Rc::clone(&self.list_offset))
                // The panel body's padding, so rows stand off the panel's
                // edges like every list in a page.
                .with_child(Box::new(
                    Container::new()
                        .with_inset(Rhythm::Section)
                        .with_child(Box::new(Section::new(rows))),
                )),
        );
    }

    /// The (line, column) of the diagnostic the user activated, once. Both are
    /// 1-based, as listed.
    pub fn take_jump(&self) -> Option<(usize, usize)> {
        if self.tab != PanelTab::Problems {
            return None;
        }
        let index = self.activated.take()?;
        self.problems.get(index).map(|d| (d.line, d.col))
    }

    /// Say what the language server is still doing, or `None` once it has
    /// answered.
    ///
    /// This exists because "No problems detected in the workspace." is a claim,
    /// and for the first seconds — or, on a cold crate graph this size, the
    /// first minutes — it is not a true one: nothing has been analysed. An
    /// editor that asserts a clean bill of health before it has looked is worse
    /// than one that looks broken, because the user believes it.
    pub fn set_analysis(&mut self, note: Option<String>) {
        if self.analysis != note {
            self.analysis = note;
            self.dirty = true;
        }
    }

    /// Mark whether a live embedded terminal owns the Terminal tab body.
    pub fn set_has_live_terminal(&mut self, live: bool) {
        if self.has_live_terminal != live {
            self.has_live_terminal = live;
            self.dirty = true;
        }
    }

    /// Set (or clear) the note shown when there is no live terminal.
    pub fn set_terminal_note(&mut self, note: Option<String>) {
        if self.terminal_note != note {
            self.terminal_note = note;
            self.dirty = true;
        }
    }

    /// The currently-visible dock tab.
    pub fn active_tab(&self) -> PanelTab {
        self.tab
    }

    /// The body below the tab strip — where the embedded terminal is placed
    /// and drawn.
    pub fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.strip_height,
            self.rect.x1,
            self.rect.y1,
        )
    }

    pub fn problem_count(&self) -> usize {
        self.problems.len()
    }

    /// What the panel is saying in place of "no problems", if anything.
    #[cfg(test)]
    pub fn analysis(&self) -> Option<&str> {
        self.analysis.as_deref()
    }

    /// The diagnostics currently listed, so callers can assert on what the user
    /// is shown rather than only on how many rows there are.
    #[cfg(test)]
    pub fn problems(&self) -> &[Diagnostic] {
        &self.problems
    }

    pub fn set_tab(&mut self, tab: PanelTab) {
        if self.tab != tab {
            self.tab = tab;
            self.tabs.set_active(tab.id());
            self.activated.set(None);
            self.rebuild_list();
            self.dirty = true;
        }
    }

    pub fn take_tab_changed(&mut self) -> bool {
        std::mem::take(&mut self.tab_changed)
    }

    /// Whether the user closed a tab (which closes the panel), once.
    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    /// Start the tab strip afresh. A closed panel keeps no half-faded tab.
    pub fn reset_tabs(&mut self) {
        self.tabs = Self::fresh_tabs(self.tab, self.problems.len());
        self.tabs_node = None;
    }

    /// The Problems body: the shared zero-state when there is nothing to
    /// show (distinguishing "not analysed yet" from "analysed and clean"),
    /// otherwise one row per diagnostic.
    fn draw_problems(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(list) = self.list.as_mut() {
            list.draw(scene, transform, text_renderer, theme, presence);
            return;
        }
        let body = self.content_rect();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        // An empty list means two different things, and the panel must
        // not state the wrong one. While a language server is still
        // working nothing has been analysed yet; only once it has
        // answered is "no problems" a finding. Scoped to the open file,
        // because that is what was actually checked — the linter reads
        // the active buffer, not every file under the root.
        let (icon, title) = match &self.analysis {
            Some(note) => (ui::icons::Icon::RotateCw, note.as_str()),
            None => (ui::icons::Icon::Check, "No problems in this file"),
        };
        EmptyState::paint(
            &mut ctx,
            body,
            &EmptyStateContent {
                icon: Some(icon),
                ..EmptyStateContent::new(title)
            },
            Point::new(-1.0, -1.0),
        );
    }

    fn draw_terminal_note(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.has_live_terminal {
            // The embedded `TerminalWidget` renders the body over
            // `content_rect()`; leave it to the app so we don't paint stale
            // text underneath the live grid.
            return;
        }
        if let Some(note) = &self.terminal_note {
            let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
            EmptyState::paint(
                &mut ctx,
                self.content_rect(),
                &EmptyStateContent {
                    icon: Some(ui::icons::Icon::Terminal),
                    ..EmptyStateContent::new(note)
                },
                Point::new(-1.0, -1.0),
            );
        }
    }
}

impl Default for BottomPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for BottomPanel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.height = Self::wanted_height(theme);
        self.strip_height = ui::widget::tabs::TAB_BAR_HEIGHT;
        let tabs_node = self.tabs.build_layout(taffy, theme, node_map);
        self.tabs_node = Some(tabs_node);
        // The list sits under the strip in every tab so the panel's height is
        // stable; it is drawn and reached only on the Problems tab.
        self.list_node = self
            .list
            .as_mut()
            .map(|list| list.build_layout(taffy, theme, node_map));
        let mut kids = vec![tabs_node];
        kids.extend(self.list_node);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height as f32),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_node(style, &kids, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(n) = self.tabs_node {
            self.tabs.apply_layout(taffy, n, origin);
        }
        if let (Some(list), Some(n)) = (self.list.as_mut(), self.list_node) {
            list.apply_layout(taffy, n, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The panel stands on the content ground the framework painted; one
        // hairline parts it from the editor above.
        ui::surface::separator(
            scene,
            transform,
            self.rect.x0,
            self.rect.x1,
            self.rect.y0,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        self.tabs
            .draw(scene, transform, text_renderer, theme, presence);
        match self.tab {
            PanelTab::Problems | PanelTab::Search => {
                self.draw_problems(scene, transform, text_renderer, theme, presence);
            }
            PanelTab::Terminal => self.draw_terminal_note(scene, transform, text_renderer, theme),
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(self.tab, PanelTab::Problems | PanelTab::Search)
            && let (Some(list), Some(n)) = (self.list.as_mut(), self.list_node)
        {
            list.handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                self.dirty = true;
                return;
            }
        }
        if let Some(n) = self.tabs_node {
            self.tabs
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        if let Some(id) = self.tabs.take_activation()
            && let Some(tab) = PanelTab::from_id(id)
            && self.tab != tab
        {
            self.tab = tab;
            self.tab_changed = true;
            self.dirty = true;
        }
    }

    fn name(&self) -> &'static str {
        "BottomPanel"
    }

    fn test_id(&self) -> Option<&str> {
        Some("bottom_panel")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.tabs];
        if matches!(self.tab, PanelTab::Problems | PanelTab::Search)
            && let Some(list) = &self.list
        {
            out.push(list);
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.tabs];
        if matches!(self.tab, PanelTab::Problems | PanelTab::Search)
            && let Some(list) = self.list.as_mut()
        {
            out.push(list);
        }
        out
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.tabs.needs_redraw()
            || self.list.as_ref().is_some_and(Widget::needs_redraw)
    }

    fn needs_animation_frame(&self) -> bool {
        self.tabs.needs_animation_frame()
            || self
                .list
                .as_ref()
                .is_some_and(Widget::needs_animation_frame)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.tabs.mark_drawn();
        if let Some(list) = self.list.as_mut() {
            list.mark_drawn();
        }
    }
}

impl BottomPanel {
    /// What the search found and whether it is still going or stopped at its
    /// limit: "3 results in 2 files — 410 files scanned".
    fn search_summary(&self) -> String {
        let search = &self.search;
        if search.query.is_empty() {
            return String::new();
        }
        let files = {
            let mut seen: Vec<&str> = search.rows.iter().map(|r| r.rel.as_str()).collect();
            seen.sort_unstable();
            seen.dedup();
            seen.len()
        };
        let head = match (search.rows.len(), files) {
            (0, _) if search.running => return "Searching…".to_string(),
            (0, _) => format!("No results for \u{201C}{}\u{201D}", search.query),
            (1, _) => "1 result in 1 file".to_string(),
            (n, 1) => format!("{n} results in 1 file"),
            (n, f) => format!("{n} results in {f} files"),
        };
        if search.running {
            format!("{head} \u{2014} still searching…")
        } else if search.truncated {
            format!(
                "{head} \u{2014} search limit reached after {} files; narrow the search",
                search.files_scanned
            )
        } else if search.files_scanned > 0 {
            format!("{head} \u{2014} {} files scanned", search.files_scanned)
        } else {
            head
        }
    }

    pub fn set_search(&mut self, search: SearchState) {
        if self.search != search {
            self.search = search;
            if self.tab == PanelTab::Search {
                self.rebuild_list();
            }
            self.dirty = true;
        }
    }
    pub fn take_search_jump(&self) -> Option<usize> {
        if self.tab == PanelTab::Search {
            self.activated.take()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(severity: Severity, line: usize) -> Diagnostic {
        Diagnostic {
            severity,
            line,
            col: 1,
            message: format!("problem on line {line}"),
        }
    }

    #[test]
    fn the_search_heading_says_how_far_the_search_got() {
        let hit = |rel: &str| HitRow {
            rel: rel.into(),
            line: 1,
            preview: "fn main()".into(),
            highlight: (0, 2),
        };
        let mut panel = BottomPanel::new();
        let mut search = SearchState {
            query: "main".into(),
            running: true,
            ..SearchState::default()
        };
        panel.set_search(search.clone());
        assert_eq!(panel.search_summary(), "Searching…");

        search.rows = vec![hit("a.rs"), hit("a.rs"), hit("b.rs")];
        search.running = false;
        search.files_scanned = 40;
        panel.set_search(search.clone());
        assert_eq!(
            panel.search_summary(),
            "3 results in 2 files \u{2014} 40 files scanned"
        );

        search.truncated = true;
        panel.set_search(search);
        assert!(
            panel
                .search_summary()
                .contains("search limit reached after 40 files")
        );
    }

    #[test]
    fn the_problems_tab_counts_what_it_lists() {
        let mut panel = BottomPanel::new();
        let titles = |panel: &BottomPanel| {
            panel
                .tabs
                .tabs()
                .iter()
                .map(|tab| tab.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(&panel), ["Problems", "Terminal", "Search results"]);
        panel.set_problems(vec![diagnostic(Severity::Error, 3)]);
        assert_eq!(
            titles(&panel),
            ["Problems (1)", "Terminal", "Search results"]
        );
        panel.set_problems(Vec::new());
        assert_eq!(titles(&panel), ["Problems", "Terminal", "Search results"]);
    }

    #[test]
    fn each_problem_is_a_row_and_activating_one_reports_where_it_was_found() {
        let mut panel = BottomPanel::new();
        assert!(panel.list.is_none(), "no problems, no list");
        panel.set_problems(vec![
            diagnostic(Severity::Error, 3),
            diagnostic(Severity::Warning, 9),
        ]);
        assert!(panel.list.is_some());
        assert_eq!(panel.take_jump(), None);
        // The row's activation is the one thing that writes this.
        panel.activated.set(Some(1));
        assert_eq!(panel.take_jump(), Some((9, 1)));
        assert_eq!(panel.take_jump(), None, "cleared on read");
        panel.set_problems(Vec::new());
        assert!(panel.list.is_none());
    }

    #[test]
    fn a_retained_problem_row_activates_through_its_native_view_door() {
        let mut panel = BottomPanel::new();
        panel.set_problems(vec![
            diagnostic(Severity::Error, 3),
            diagnostic(Severity::Warning, 9),
        ]);
        let activated = Rc::clone(&panel.activated);
        let mut harness =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(panel), (640, 300));
        assert_eq!(activated.get(), None);
        harness.click_named("problem-1").unwrap();
        assert_eq!(activated.take(), Some(1));
        assert_eq!(
            activated.take(),
            None,
            "the native activation is consumed once"
        );
    }

    #[test]
    fn choosing_a_tab_is_reported_once_and_closing_asks_to_hide_the_panel() {
        let mut panel = BottomPanel::new();
        assert_eq!(panel.active_tab(), PanelTab::Problems);
        panel.set_tab(PanelTab::Terminal);
        assert_eq!(panel.active_tab(), PanelTab::Terminal);
        assert!(!panel.take_tab_changed(), "a program's choice is no edge");
        assert!(!panel.take_close_request());
        panel.close_requested = true;
        assert!(panel.take_close_request());
        assert!(!panel.take_close_request(), "cleared on read");
    }

    #[test]
    fn the_panel_is_the_tab_strip_plus_a_few_rows_tall() {
        let theme = Theme::dark();
        let wanted = BottomPanel::wanted_height(&theme);
        let row = f64::from(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s2);
        assert!(
            (wanted - (ui::widget::tabs::TAB_BAR_HEIGHT + VISIBLE_ROWS as f64 * row)).abs() < 1e-9
        );
    }
}
