//! The collapsible bottom panel: a tabbed dock for Problems and Terminal.
//! Problems lists diagnostics — the local linter merged with whatever the
//! language server has published (see `app.rs`). Terminal hosts a live
//! embedded shell, spawned on first open, and shows an honest empty-state
//! note until then (never a fabricated build log).
//!
//! The tabs are the toolkit's [`TabBar`], the same strip as the document
//! tabs above it, and each diagnostic is a [`ListRowFace`]. The panel renders
//! the data the app feeds it and reports what the user chose; it owns no
//! filesystem or language-server state of its own.

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    EmptyState, EmptyStateContent, Interaction, ListRowFace, RowLeading, RowStatus, TabBar,
    TabDescriptor, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{MouseScrollDelta, WindowEvent};

/// Rows of diagnostics the panel shows before it scrolls.
const VISIBLE_ROWS: usize = 4;

/// Which dock tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelTab {
    Problems,
    Terminal,
}

impl PanelTab {
    const fn id(self) -> u64 {
        match self {
            Self::Problems => 1,
            Self::Terminal => 2,
        }
    }

    fn from_id(id: u64) -> Option<Self> {
        match id {
            1 => Some(Self::Problems),
            2 => Some(Self::Terminal),
            _ => None,
        }
    }
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
    tabs: TabBar,
    tabs_node: Option<NodeId>,
    last_cursor: Point,
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
    /// First visible diagnostic row, advanced by the wheel.
    scroll: usize,
    /// Row rects recorded during `draw`, for hover and wheel hit-testing.
    row_rects: Vec<Rect>,
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
            tabs: Self::fresh_tabs(PanelTab::Problems, 0),
            tabs_node: None,
            last_cursor: Point::ZERO,
            tab_changed: false,
            close_requested: false,
            has_live_terminal: false,
            terminal_note: None,
            analysis: None,
            scroll: 0,
            row_rects: Vec::new(),
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
            ],
            active.id(),
        );
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
            self.scroll = self.scroll.min(self.problems.len().saturating_sub(1));
            self.refresh_tab_titles();
            self.dirty = true;
        }
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
    ) {
        self.row_rects.clear();
        let body = self.content_rect();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        if self.problems.is_empty() {
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
            return;
        }
        let inset = f64::from(theme.tokens.spacing.s2);
        let mut y = body.y0;
        for d in self.problems.iter().skip(self.scroll) {
            let location = format!("Ln {}, Col {}", d.line, d.col);
            let (icon, status) = match d.severity {
                Severity::Error => (ui::icons::Icon::TriangleAlert, Some(RowStatus::Error)),
                Severity::Warning => (ui::icons::Icon::TriangleAlert, Some(RowStatus::Warning)),
                Severity::Info | Severity::Hint => (ui::icons::Icon::Info, None),
            };
            let mut face = ListRowFace::new(&d.message)
                .leading(RowLeading::Icon(icon))
                .meta(&location);
            if let Some(status) = status {
                face = face.status(status);
            }
            let height = face.height(ctx.text, ctx.theme);
            let rect = Rect::new(body.x0 + inset, y, body.x1 - inset, y + height);
            if rect.y1 > body.y1 + 0.5 {
                break;
            }
            let face = face.interaction(Interaction {
                hovered: rect.contains(self.last_cursor),
                ..Interaction::default()
            });
            face.paint(&mut ctx, rect);
            self.row_rects.push(rect);
            y += height;
        }
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
        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a node over a freshly created child cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(self.height as f32),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &[tabs_node],
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        #[allow(
            clippy::unwrap_used,
            reason = "node was just created by this widget's own build_layout in the same tree; a lookup failure means the taffy tree is already corrupted and there is no meaningful layout to fall back to"
        )]
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(n) = self.tabs_node {
            self.tabs
                .apply_layout(taffy, n, Point::new(self.rect.x0, self.rect.y0));
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
            PanelTab::Problems => self.draw_problems(scene, transform, text_renderer, theme),
            PanelTab::Terminal => self.draw_terminal_note(scene, transform, text_renderer, theme),
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            let next = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            if self.row_rects.iter().any(|r| r.contains(next))
                != self.row_rects.iter().any(|r| r.contains(self.last_cursor))
                || self.row_rects.iter().any(|r| r.contains(next))
            {
                self.dirty = true;
            }
            self.last_cursor = next;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event {
            if self.tab != PanelTab::Problems || !self.rect.contains(self.last_cursor) {
                return;
            }
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                MouseScrollDelta::PixelDelta(p) => p.y / 20.0,
            };
            let step = (-lines).round() as i64;
            let visible = self.row_rects.len().max(1);
            let max = self.problems.len().saturating_sub(visible);
            let next = (self.scroll as i64 + step).clamp(0, max as i64) as usize;
            if next != self.scroll {
                self.scroll = next;
                self.dirty = true;
            }
            *ctx.handled = true;
            return;
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
        if self.tabs.take_close().is_some() {
            self.close_requested = true;
            self.dirty = true;
        }
        // The strip's "+" opens a terminal, the one thing the dock can add.
        if self.tabs.take_new_tab().is_some() {
            self.tab = PanelTab::Terminal;
            self.tabs.set_active(PanelTab::Terminal.id());
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
        vec![&self.tabs]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.tabs]
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.tabs.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.tabs.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.tabs.mark_drawn();
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
        assert_eq!(titles(&panel), ["Problems", "Terminal"]);
        panel.set_problems(vec![diagnostic(Severity::Error, 3)]);
        assert_eq!(titles(&panel), ["Problems (1)", "Terminal"]);
        panel.set_problems(Vec::new());
        assert_eq!(titles(&panel), ["Problems", "Terminal"]);
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
