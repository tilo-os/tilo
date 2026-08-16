//! The collapsible bottom panel: a tabbed dock for Problems, Search and
//! Terminal. Problems lists diagnostics — the local linter merged with whatever
//! the language server has published (see `app.rs`). Search lists the hits a
//! project-wide search streamed back from its worker thread. Terminal hosts a
//! live embedded shell, spawned on first open, and shows an honest empty-state
//! note until then (never a fabricated build log).
//!
//! The panel renders the data the app feeds it and reports what the user
//! clicked; it owns no filesystem or language-server state of its own.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{EmptyState, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

pub const PANEL_HEIGHT: f64 = 160.0;
/// Search results get a taller dock: a list of file-and-line hits is only
/// useful if you can see more than a handful at once.
pub const PANEL_HEIGHT_SEARCH: f64 = 300.0;
const TAB_H: f64 = 30.0;
const ROW_H: f64 = 20.0;
/// Search rows are taller than problem rows — each carries a path, a line
/// number and a code preview.
const HIT_ROW_H: f64 = 26.0;

/// Which dock tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelTab {
    Problems,
    Search,
    Terminal,
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
    /// True when the walk stopped at the hit cap.
    pub truncated: bool,
    pub files_scanned: usize,
}

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A hard error (LSP severity 1). Rendered in the danger colour.
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

pub struct BottomPanel {
    rect: Rect,
    tab: PanelTab,
    problems: Vec<Diagnostic>,
    tab_rects: Vec<(f64, f64, PanelTab)>,
    last_cursor: Point,
    /// Set true when the user clicks a tab — read+cleared by the app.
    tab_changed: bool,
    /// When true, the Terminal tab's body is owned by an embedded live
    /// `TerminalWidget` the app draws over this panel's content rect, so the
    /// panel leaves that region blank instead of rendering the static fallback.
    has_live_terminal: bool,
    /// Note shown in the Terminal body when no live terminal is present (the
    /// pre-spawn invitation, or "shell unavailable" on a spawn failure).
    terminal_note: Option<String>,
    /// The Search tab's contents, pushed in by the app as the worker streams.
    search: SearchState,
    /// First visible search row, advanced by the wheel.
    search_scroll: usize,
    /// Row index the user clicked, drained by the app.
    activated_hit: Option<usize>,
    /// Search hit rows recorded during `draw`, for hit-testing clicks.
    hit_rects: Vec<Rect>,
    dirty: bool,
}

impl BottomPanel {
    pub fn new() -> Self {
        BottomPanel {
            rect: Rect::ZERO,
            tab: PanelTab::Problems,
            problems: Vec::new(),
            tab_rects: Vec::new(),
            last_cursor: Point::ZERO,
            tab_changed: false,
            has_live_terminal: false,
            terminal_note: None,
            search: SearchState::default(),
            search_scroll: 0,
            activated_hit: None,
            hit_rects: Vec::new(),
            dirty: true,
        }
    }

    /// Replace the Search tab's contents.
    pub fn set_search(&mut self, search: SearchState) {
        if self.search != search {
            // A different query is a different result list; keep the scroll
            // position only while the same search is still streaming in.
            if self.search.query != search.query {
                self.search_scroll = 0;
            }
            self.search = search;
            self.dirty = true;
        }
    }

    /// The Search tab's current state.
    pub fn search(&self) -> &SearchState {
        &self.search
    }

    /// The search row the user clicked, if any. Cleared on read.
    pub fn take_activated_hit(&mut self) -> Option<usize> {
        self.activated_hit.take()
    }

    /// The height this panel wants, which depends on the tab: a search result
    /// list needs more room than a status line.
    pub fn height(&self) -> f64 {
        match self.tab {
            PanelTab::Search => PANEL_HEIGHT_SEARCH,
            _ => PANEL_HEIGHT,
        }
    }

    /// How many search rows fit in the body at the current height.
    fn visible_hit_rows(&self) -> usize {
        let body = (self.rect.height() - TAB_H - 24.0).max(0.0);
        (body / HIT_ROW_H).floor().max(0.0) as usize
    }

    /// The one-line summary above the results: how many hits, across how many
    /// files, and whether the walk is still going or stopped at the cap.
    fn search_summary(&self) -> String {
        if self.search.query.is_empty() {
            return String::new();
        }
        let files = {
            let mut seen: Vec<&str> = self.search.rows.iter().map(|r| r.rel.as_str()).collect();
            seen.sort_unstable();
            seen.dedup();
            seen.len()
        };
        let n = self.search.rows.len();
        let head = match (n, files) {
            (0, _) if self.search.running => "Searching…".to_string(),
            (0, _) => format!("No results for \u{201C}{}\u{201D}", self.search.query),
            (1, _) => "1 result in 1 file".to_string(),
            (n, 1) => format!("{n} results in 1 file"),
            (n, f) => format!("{n} results in {f} files"),
        };
        if n > 0 && self.search.running {
            format!("{head} \u{2014} still searching…")
        } else if self.search.truncated {
            format!("{head} \u{2014} stopped at the first {n}; narrow the search")
        } else {
            head
        }
    }

    /// Draw the Search tab body: a summary line, then one row per hit.
    fn draw_search(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        self.hit_rects.clear();

        // Nothing has been asked yet: a real zero-state, not a blank panel.
        if self.search.query.is_empty() {
            let body = Rect::new(
                self.rect.x0,
                self.rect.y0 + TAB_H,
                self.rect.x1,
                self.rect.y1,
            );
            EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                body,
                Some(ui::icons::Icon::Search),
                "Search this project",
                Some("Ctrl+Shift+F searches every file under the project root."),
                None,
                Point::new(-1.0, -1.0),
            );
            return;
        }

        let summary = self.search_summary();
        text_renderer.draw_text(
            scene,
            &summary,
            12.0,
            if self.search.rows.is_empty() && !self.search.running {
                c.semantic.warning
            } else {
                c.text_dim
            },
            transform * Affine::translate((self.rect.x0 + 14.0, self.rect.y0 + TAB_H + 6.0)),
            Some((self.rect.width() - 28.0) as f32),
        );

        let first_row_y = self.rect.y0 + TAB_H + 26.0;
        let visible = self.visible_hit_rows();
        for (i, row) in self
            .search
            .rows
            .iter()
            .skip(self.search_scroll)
            .take(visible)
            .enumerate()
        {
            let y = first_row_y + i as f64 * HIT_ROW_H;
            let rect = Rect::new(self.rect.x0 + 8.0, y, self.rect.x1 - 8.0, y + HIT_ROW_H);
            if rect.y1 > self.rect.y1 {
                break;
            }
            self.hit_rects.push(rect);
            if rect.contains(self.last_cursor) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.surface_white_5,
                    None,
                    &RoundedRect::from_rect(rect, 4.0),
                );
            }

            // `path:line`, then the code preview with the match tinted.
            let location = format!("{}:{}", row.rel, row.line);
            let (loc_w, _) = text_renderer.measure(&location, 12.0, None);
            text_renderer.draw_text(
                scene,
                &location,
                12.0,
                c.accent[5],
                transform * Affine::translate((rect.x0 + 8.0, y + 5.0)),
                Some(340.0),
            );

            let preview_x = rect.x0 + 16.0 + (loc_w as f64).min(340.0);
            let (start, len) = row.highlight;
            let chars: Vec<char> = row.preview.chars().collect();
            let before: String = chars[..start.min(chars.len())].iter().collect();
            let hit: String = chars[start.min(chars.len())..(start + len).min(chars.len())]
                .iter()
                .collect();
            let after: String = chars[(start + len).min(chars.len())..].iter().collect();

            let mut x = preview_x;
            let avail = (rect.x1 - 8.0 - preview_x).max(0.0);
            for (text, color) in [
                (before.as_str(), c.text_dim),
                (hit.as_str(), c.text_primary),
                (after.as_str(), c.text_dim),
            ] {
                if text.is_empty() || x >= rect.x1 - 8.0 {
                    continue;
                }
                let (w, _) = text_renderer.measure(text, 12.0, None);
                // The matched span is the one that must stay legible, so it is
                // drawn on the theme's primary text colour against the dimmed
                // surrounding line rather than on a coloured block.
                text_renderer.draw_text(
                    scene,
                    text,
                    12.0,
                    color,
                    transform * Affine::translate((x, y + 5.0)),
                    Some(avail as f32),
                );
                x += w as f64;
            }
        }

        // A quiet "more below" hint rather than a scrollbar the panel has no
        // room for.
        let shown = self.search_scroll + self.hit_rects.len();
        if shown < self.search.rows.len() {
            let more = format!("{} more below", self.search.rows.len() - shown);
            text_renderer.draw_text(
                scene,
                &more,
                11.0,
                c.text_dim,
                transform * Affine::translate((self.rect.x0 + 14.0, self.rect.y1 - 16.0)),
                None,
            );
        }
    }

    pub fn set_problems(&mut self, problems: Vec<Diagnostic>) {
        if self.problems != problems {
            self.problems = problems;
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

    /// The content region below the tab strip — where the embedded terminal is
    /// placed and drawn.
    pub fn content_rect(&self) -> Rect {
        Rect::new(self.rect.x0, self.rect.y0 + TAB_H, self.rect.x1, self.rect.y1)
    }

    pub fn problem_count(&self) -> usize {
        self.problems.len()
    }

    pub fn set_tab(&mut self, tab: PanelTab) {
        if self.tab != tab {
            self.tab = tab;
            self.dirty = true;
        }
    }

    pub fn take_tab_changed(&mut self) -> bool {
        std::mem::take(&mut self.tab_changed)
    }
}

impl Widget for BottomPanel {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(self.height() as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &self.rect);
        // Top divider.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y0),
                Point::new(self.rect.x1, self.rect.y0),
            ),
        );

        // Tab strip.
        self.tab_rects.clear();
        let search_label = if self.search.rows.is_empty() {
            "Search".to_string()
        } else {
            format!("Search ({})", self.search.rows.len())
        };
        let labels = [
            (PanelTab::Problems, format!("Problems ({})", self.problems.len())),
            (PanelTab::Search, search_label),
            (PanelTab::Terminal, "Terminal".to_string()),
        ];
        let mut x = self.rect.x0 + 8.0;
        for (tab, label) in labels.iter() {
            let (w, _) = text_renderer.measure(label, 12.0, None);
            let x1 = x + w as f64 + 24.0;
            self.tab_rects.push((x, x1, *tab));
            let active = self.tab == *tab;
            let color = if active { c.text_primary } else { c.text_dim };
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                color,
                transform * Affine::translate((x + 6.0, self.rect.y0 + 9.0)),
                None,
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
            if active {
                let underline = Rect::new(x, self.rect.y0 + TAB_H - 2.0, x1, self.rect.y0 + TAB_H);
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &underline);
            }
            x = x1;
        }
        // Tab strip baseline.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y0 + TAB_H),
                Point::new(self.rect.x1, self.rect.y0 + TAB_H),
            ),
        );

        let content_y0 = self.rect.y0 + TAB_H + 6.0;
        match self.tab {
            PanelTab::Problems => {
                if self.problems.is_empty() {
                    text_renderer.draw_text(
                        scene,
                        "No problems detected in the workspace.",
                        13.0,
                        c.text_dim,
                        transform * Affine::translate((self.rect.x0 + 14.0, content_y0)),
                        None,
                    );
                } else {
                    for (i, d) in self.problems.iter().enumerate() {
                        let y = content_y0 + i as f64 * ROW_H;
                        if y > self.rect.y1 - ROW_H {
                            break;
                        }
                        let (glyph, sev_color) = match d.severity {
                            Severity::Error => ("\u{2716}", c.semantic.danger),
                            Severity::Warning => ("\u{26A0}", c.semantic.warning),
                            Severity::Info => ("\u{2139}", c.semantic.info),
                            Severity::Hint => ("\u{2022}", c.text_dim),
                        };
                        text_renderer.draw_text(
                            scene,
                            glyph,
                            13.0,
                            sev_color,
                            transform * Affine::translate((self.rect.x0 + 14.0, y)),
                            None,
                        );
                        let loc = format!("[Ln {}, Col {}]", d.line, d.col);
                        text_renderer.draw_text(
                            scene,
                            &loc,
                            12.0,
                            c.text_dim,
                            transform * Affine::translate((self.rect.x0 + 36.0, y + 0.5)),
                            None,
                        );
                        text_renderer.draw_text(
                            scene,
                            &d.message,
                            13.0,
                            c.neutral[8],
                            transform * Affine::translate((self.rect.x0 + 130.0, y)),
                            Some((self.rect.width() - 140.0) as f32),
                        );
                    }
                }
            }
            PanelTab::Search => self.draw_search(scene, transform, text_renderer, theme),
            PanelTab::Terminal => {
                if self.has_live_terminal {
                    // The embedded `TerminalWidget` renders the body over
                    // `content_rect()`; leave it to the app so we don't paint
                    // stale text underneath the live grid.
                } else if let Some(note) = &self.terminal_note {
                    text_renderer.draw_text(
                        scene,
                        note,
                        13.0,
                        c.text_dim,
                        transform * Affine::translate((self.rect.x0 + 14.0, content_y0)),
                        Some((self.rect.width() - 28.0) as f32),
                    );
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.tab != PanelTab::Search || !self.rect.contains(self.last_cursor) {
                    return;
                }
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 20.0,
                };
                let step = (-lines).round() as i64;
                let visible = self.visible_hit_rows();
                let max = self.search.rows.len().saturating_sub(visible);
                let next = (self.search_scroll as i64 + step).clamp(0, max as i64) as usize;
                if next != self.search_scroll {
                    self.search_scroll = next;
                    self.dirty = true;
                }
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.last_cursor.y >= self.rect.y0
                    && self.last_cursor.y <= self.rect.y0 + TAB_H
                {
                    for (x0, x1, tab) in &self.tab_rects {
                        if self.last_cursor.x >= *x0 && self.last_cursor.x < *x1 {
                            if self.tab != *tab {
                                self.tab = *tab;
                                self.tab_changed = true;
                                self.dirty = true;
                            }
                            *handled = true;
                            break;
                        }
                    }
                    return;
                }
                // A click on a search hit asks the app to open that file at
                // that line.
                if self.tab == PanelTab::Search {
                    for (i, rect) in self.hit_rects.iter().enumerate() {
                        if rect.contains(self.last_cursor) {
                            self.activated_hit = Some(self.search_scroll + i);
                            self.dirty = true;
                            *handled = true;
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "BottomPanel"
    }

    fn test_id(&self) -> Option<&str> {
        Some("bottom_panel")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}
