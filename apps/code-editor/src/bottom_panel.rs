//! The collapsible bottom panel: a tabbed dock for Problems, Search and
//! Terminal. Problems lists diagnostics — the local linter merged with whatever
//! the language server has published (see `app.rs`). Search lists the hits a
//! project-wide search streamed back from its worker thread. Terminal hosts a
//! live embedded shell, spawned on first open, and shows an honest empty-state
//! note until then (never a fabricated build log).
//!
//! The panel renders the data the app feeds it and reports what the user
//! clicked; it owns no filesystem or language-server state of its own.

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{EmptyState, EmptyStateContent, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

pub const PANEL_HEIGHT: f64 = 160.0;
/// Search gets a little more room than diagnostics without taking over the
/// editor. The wheel and result count make deeper result sets discoverable.
pub const PANEL_HEIGHT_SEARCH: f64 = 216.0;
const _: () = assert!(
    PANEL_HEIGHT_SEARCH > PANEL_HEIGHT,
    "the search dock must be the taller of the two"
);
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
    /// True when the bounded walk stopped before it covered the whole project.
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
    /// What the language server is doing, when it has not finished. While this
    /// is `Some`, an empty Problems list means "not analysed yet" rather than
    /// "analysed and clean", and the panel must say so — see
    /// [`BottomPanel::set_analysis`].
    analysis: Option<String>,
    /// The Search tab's contents, pushed in by the app as the worker streams.
    search: SearchState,
    /// First visible search row, advanced by the wheel.
    search_scroll: usize,
    /// Row index the user clicked, drained by the app.
    activated_hit: Option<usize>,
    /// Search hit rows recorded during `draw`, for hit-testing clicks.
    hit_rects: Vec<Rect>,
    /// Current host text scale, captured during layout so panel height, rows,
    /// drawing, and hit testing grow as one accessible surface.
    text_scale: f64,
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
            analysis: None,
            search: SearchState::default(),
            search_scroll: 0,
            activated_hit: None,
            hit_rects: Vec::new(),
            text_scale: 1.0,
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

    /// The search row the user clicked, if any. Cleared on read.
    pub fn take_activated_hit(&mut self) -> Option<usize> {
        self.activated_hit.take()
    }

    /// The height this panel wants, which depends on the tab: a search result
    /// list needs more room than a status line.
    pub fn height(&self) -> f64 {
        match self.tab {
            PanelTab::Search => PANEL_HEIGHT_SEARCH * self.text_scale,
            _ => PANEL_HEIGHT * self.text_scale,
        }
    }

    fn tab_height(&self) -> f64 {
        TAB_H * self.text_scale
    }

    fn row_height(&self) -> f64 {
        ROW_H * self.text_scale
    }

    fn hit_row_height(&self) -> f64 {
        HIT_ROW_H * self.text_scale
    }

    /// How many search rows fit in the body at the current height.
    ///
    /// The reserved strip covers the summary line above the rows *and* the
    /// "more below" hint under them, so neither can be overprinted by a row.
    fn visible_hit_rows(&self) -> usize {
        let body = (self.rect.height()
            - self.tab_height()
            - 26.0 * self.text_scale
            - 20.0 * self.text_scale)
            .max(0.0);
        (body / self.hit_row_height()).floor().max(0.0) as usize
    }

    /// The one-line summary above the results: how many hits, across how many
    /// files, and whether the walk is still going or reached a safety bound.
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
            format!(
                "{head} \u{2014} search limit reached after {} files; narrow the search",
                self.search.files_scanned
            )
        } else if !self.search.running && self.search.files_scanned > 0 {
            format!(
                "{head} \u{2014} {} files scanned",
                self.search.files_scanned
            )
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
        // A fixed column for `path:line` so the previews line up into a
        // readable second column instead of starting at a different x on every
        // row. Paths longer than it are trimmed from the *front*: the file name
        // and line number are the part you need, and the leading directories
        // are the part you can infer.
        const LOC_COL: f64 = 300.0;

        let c = &theme.tokens.colors;
        self.hit_rects.clear();

        // Nothing has been asked yet: a real zero-state, not a blank panel.
        if self.search.query.is_empty() {
            let body = Rect::new(
                self.rect.x0,
                self.rect.y0 + self.tab_height(),
                self.rect.x1,
                self.rect.y1,
            );
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                body,
                &EmptyStateContent {
                    icon: Some(ui::icons::Icon::Search),
                    title: "Search this project",
                    message: Some("Ctrl+Shift+F searches every file under the project root."),
                    action_label: None,
                },
                Point::new(-1.0, -1.0),
            );
            return;
        }

        let summary = self.search_summary();
        text_renderer.draw_text(
            scene,
            &summary,
            12.0 * theme.text_scale,
            if self.search.rows.is_empty() && !self.search.running {
                c.semantic.warning
            } else {
                c.text_dim
            },
            transform
                * Affine::translate((
                    self.rect.x0 + 14.0 * self.text_scale,
                    self.rect.y0 + self.tab_height() + 6.0 * self.text_scale,
                )),
            Some((self.rect.width() - 28.0) as f32),
        );

        let visible = self.visible_hit_rows();
        self.draw_search_rows(scene, transform, text_renderer, theme, visible, LOC_COL);

        // A quiet "more below" hint rather than a scrollbar the panel has no
        // room for. `visible_hit_rows` already reserves this strip, so it never
        // overprints the last row.
        let shown = self.search_scroll + self.hit_rects.len();
        if shown < self.search.rows.len() {
            let more = format!(
                "{} more \u{2014} scroll the list",
                self.search.rows.len() - shown
            );
            text_renderer.draw_text(
                scene,
                &more,
                11.0 * theme.text_scale,
                c.text_dim,
                transform
                    * Affine::translate((
                        self.rect.x0 + 14.0 * self.text_scale,
                        self.rect.y1 - 17.0 * self.text_scale,
                    )),
                None,
            );
        }
    }

    /// Draw one row per visible search hit: the `path:line` location column,
    /// then the code preview with the matched span tinted. Records each row's
    /// rect in `hit_rects` for click hit-testing.
    fn draw_search_rows(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        visible: usize,
        loc_col: f64,
    ) {
        let c = &theme.tokens.colors;
        let first_row_y = self.rect.y0 + self.tab_height() + 26.0 * self.text_scale;
        for (i, row) in self
            .search
            .rows
            .iter()
            .skip(self.search_scroll)
            .take(visible)
            .enumerate()
        {
            let y = first_row_y + i as f64 * self.hit_row_height();
            let rect = Rect::new(
                self.rect.x0 + 8.0,
                y,
                self.rect.x1 - 8.0,
                y + self.hit_row_height(),
            );
            if rect.y1 > self.rect.y1 {
                break;
            }
            self.hit_rects.push(rect);
            if rect.contains(self.last_cursor) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.lift(0.35),
                    None,
                    &RoundedRect::from_rect(rect, 4.0),
                );
            }

            // `path:line`, then the code preview with the match tinted.
            let location = ellipsize_front(
                text_renderer,
                &format!("{}:{}", row.rel, row.line),
                12.0 * theme.text_scale,
                loc_col - 12.0 * self.text_scale,
            );
            // Drawn with no wrap width: a wrapped row would spill onto the row
            // below it and the two would overprint.
            text_renderer.draw_text(
                scene,
                &location,
                12.0 * theme.text_scale,
                c.accent[5],
                transform * Affine::translate((rect.x0 + 8.0, y + 5.0)),
                None,
            );

            let preview_x = rect.x0 + 8.0 + loc_col;
            let (start, len) = row.highlight;
            let chars: Vec<char> = row.preview.chars().collect();
            let start = start.min(chars.len());
            let end = (start + len).min(chars.len());
            let before: String = chars.get(..start).unwrap_or(&[]).iter().collect();
            let hit: String = chars.get(start..end).unwrap_or(&[]).iter().collect();
            let after: String = chars.get(end..).unwrap_or(&[]).iter().collect();

            let right_edge = rect.x1 - 8.0;
            let mut x = preview_x;
            for (text, color) in [
                (before.as_str(), c.text_dim),
                (hit.as_str(), c.text_primary),
                (after.as_str(), c.text_dim),
            ] {
                if text.is_empty() || x >= right_edge {
                    continue;
                }
                // Truncated to what fits rather than wrapped, for the same
                // reason as the path: one hit is one row.
                let fitted =
                    ellipsize_back(text_renderer, text, 12.0 * theme.text_scale, right_edge - x);
                let (w, _) = text_renderer.measure(&fitted, 12.0 * theme.text_scale, None);
                // The matched span is the one that must stay legible, so it is
                // drawn on the theme's primary text colour against the dimmed
                // surrounding line rather than on a coloured block.
                text_renderer.draw_text(
                    scene,
                    &fitted,
                    12.0 * theme.text_scale,
                    color,
                    transform * Affine::translate((x, y + 5.0)),
                    None,
                );
                x += f64::from(w);
            }
        }
    }

    pub fn set_problems(&mut self, problems: Vec<Diagnostic>) {
        if self.problems != problems {
            self.problems = problems;
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

    /// The content region below the tab strip — where the embedded terminal is
    /// placed and drawn.
    pub fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.tab_height(),
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
            self.dirty = true;
        }
    }

    pub fn take_tab_changed(&mut self) -> bool {
        std::mem::take(&mut self.tab_changed)
    }

    /// Draw the tab strip: one label per tab (Problems carries a count,
    /// Search a hit count once it has results), the active tab's underline,
    /// and the baseline separating it from the body. Records each tab's
    /// clickable rect in `tab_rects`.
    fn draw_tab_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        self.tab_rects.clear();
        let search_label = if self.search.rows.is_empty() {
            "Search".to_string()
        } else {
            format!("Search ({})", self.search.rows.len())
        };
        let labels = [
            (
                PanelTab::Problems,
                format!("Problems ({})", self.problems.len()),
            ),
            (PanelTab::Search, search_label),
            (PanelTab::Terminal, "Terminal".to_string()),
        ];
        let mut x = self.rect.x0 + 8.0;
        for (tab, label) in &labels {
            let active = self.tab == *tab;
            let weight = if active {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            };
            // Measure with the weight that is painted; centre on the measured
            // line height so every tab label shares the strip's centre line.
            let (w, h) = text_renderer.measure_weighted(label, 12.0, None, weight);
            let x1 = x + f64::from(w) + 24.0;
            self.tab_rects.push((x, x1, *tab));
            let color = if active { c.text_primary } else { c.text_dim };
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0 * theme.text_scale,
                color,
                transform
                    * Affine::translate((
                        x + 12.0,
                        self.rect.y0 + (self.tab_height() - f64::from(h)) / 2.0,
                    )),
                None,
                weight,
            );
            if active {
                let underline = Rect::new(
                    x,
                    self.rect.y0 + self.tab_height() - 2.0,
                    x1,
                    self.rect.y0 + self.tab_height(),
                );
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &underline);
            }
            x = x1;
        }
        // Tab strip baseline: derived against the raised panel it rules.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y0 + self.tab_height()),
                Point::new(self.rect.x1, self.rect.y0 + self.tab_height()),
            ),
        );
    }

    /// Draw the Problems tab body: the shared zero-state when there is
    /// nothing to show (distinguishing "not analysed yet" from "analysed and
    /// clean"), otherwise one row per diagnostic.
    #[allow(
        clippy::too_many_lines,
        reason = "the empty-state truthfulness branch and the compact diagnostic-row fallback must stay adjacent so each panel state has one complete, auditable paint path"
    )]
    fn draw_problems(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        content_y0: f64,
    ) {
        let c = &theme.tokens.colors;
        if self.problems.is_empty() {
            // An empty list means two different things, and the panel
            // must not state the wrong one. While a language server is
            // still working nothing has been analysed yet; only once it
            // has answered is "no problems" a finding. Both go through
            // the shared zero-state so this tab reads like the Search
            // tab beside it rather than a bare line of dim text.
            let body = Rect::new(
                self.rect.x0,
                self.rect.y0 + self.tab_height(),
                self.rect.x1,
                self.rect.y1,
            );
            let compact = self.rect.width() < 520.0;
            let (icon, title, message) = match &self.analysis {
                Some(note) => (
                    ui::icons::Icon::RotateCw,
                    note.as_str(),
                    if compact {
                        "Problems appear here as analysis completes."
                    } else {
                        "Nothing has been analysed yet. Problems appear here as \
                         the language server reports them."
                    },
                ),
                // Scoped to the open file, because that is what was
                // actually checked — the linter reads the active buffer,
                // not every file under the root.
                None => (
                    ui::icons::Icon::Check,
                    "No problems in this file",
                    if compact {
                        "Diagnostics and built-in checks appear here."
                    } else {
                        "Diagnostics from the language server and the built-in \
                         checks both land here."
                    },
                ),
            };
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                body,
                &EmptyStateContent {
                    icon: Some(icon),
                    title,
                    message: Some(message),
                    action_label: None,
                },
                Point::new(-1.0, -1.0),
            );
        } else {
            for (i, d) in self.problems.iter().enumerate() {
                let y = content_y0 + i as f64 * self.row_height();
                if y > self.rect.y1 - self.row_height() {
                    break;
                }
                let (glyph, sev_color) = match d.severity {
                    Severity::Error => ("\u{2716}", c.semantic.danger),
                    Severity::Warning => ("\u{26A0}", c.semantic.warning),
                    Severity::Info => ("\u{2139}", c.semantic.info),
                    Severity::Hint => ("\u{2022}", c.text_dim),
                };
                // Three sizes on one row: centre each on the row band from its
                // own measured line height (the old `y + 0.5` nudge aligned
                // tops, not centres).
                let centred = |tr: &mut TextRenderer, text: &str, size: f32| {
                    let (_, h) = tr.measure(text, size, None);
                    y + (self.row_height() - f64::from(h)) / 2.0
                };
                let glyph_size = 13.0 * theme.text_scale;
                let glyph_y = centred(text_renderer, glyph, glyph_size);
                text_renderer.draw_text(
                    scene,
                    glyph,
                    glyph_size,
                    sev_color,
                    transform * Affine::translate((self.rect.x0 + 14.0 * self.text_scale, glyph_y)),
                    None,
                );
                let loc = format!("[Ln {}, Col {}]", d.line, d.col);
                let compact = self.rect.width() < 420.0 * self.text_scale;
                if compact {
                    // At large type in a narrow pane, location + message do
                    // not fit beside each other. Keep the actionable finding
                    // in one non-overlapping row; selecting the source via the
                    // Problems list remains the route to its exact location.
                    let message = ellipsize_back(
                        text_renderer,
                        &d.message,
                        13.0 * theme.text_scale,
                        self.rect.width() - 62.0 * self.text_scale,
                    );
                    let message_size = 13.0 * theme.text_scale;
                    let message_y = centred(text_renderer, &message, message_size);
                    text_renderer.draw_text(
                        scene,
                        &message,
                        message_size,
                        c.neutral[8],
                        transform
                            * Affine::translate((self.rect.x0 + 48.0 * self.text_scale, message_y)),
                        None,
                    );
                } else {
                    let loc_size = 12.0 * theme.text_scale;
                    let loc_y = centred(text_renderer, &loc, loc_size);
                    text_renderer.draw_text(
                        scene,
                        &loc,
                        loc_size,
                        c.text_dim,
                        transform
                            * Affine::translate((self.rect.x0 + 36.0 * self.text_scale, loc_y)),
                        None,
                    );
                    let message_size = 13.0 * theme.text_scale;
                    let message_y = centred(text_renderer, &d.message, message_size);
                    text_renderer.draw_text(
                        scene,
                        &d.message,
                        message_size,
                        c.neutral[8],
                        transform
                            * Affine::translate((
                                self.rect.x0 + 130.0 * self.text_scale,
                                message_y,
                            )),
                        Some((self.rect.width() - 140.0 * self.text_scale) as f32),
                    );
                }
            }
        }
    }
}

/// Trim `s` from the **front** until it fits `max_w`, prefixing an ellipsis.
///
/// For a path, the informative end is the right one — `…/code_editor/src/app.rs:1673`
/// still tells you which file and which line, where trimming the tail would
/// leave `apps/code_edi…`, which tells you nothing.
fn ellipsize_front(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if f64::from(tr.measure(s, size, None).0) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    // Binary search the longest suffix that fits, so a long list of rows does
    // not cost a measure per character per row.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = usize::midpoint(lo, hi);
        let candidate: String = std::iter::once('\u{2026}')
            .chain(chars.get(mid..).unwrap_or(&[]).iter().copied())
            .collect();
        if f64::from(tr.measure(&candidate, size, None).0) <= max_w {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    std::iter::once('\u{2026}')
        .chain(chars.get(lo..).unwrap_or(&[]).iter().copied())
        .collect()
}

/// Trim `s` from the **back** until it fits `max_w`, appending an ellipsis.
/// For a code preview the start of the line is the informative end.
fn ellipsize_back(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if f64::from(tr.measure(s, size, None).0) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars
            .get(..mid)
            .unwrap_or(&[])
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
            .collect();
        if f64::from(tr.measure(&candidate, size, None).0) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars
        .get(..lo)
        .unwrap_or(&[])
        .iter()
        .copied()
        .chain(std::iter::once('\u{2026}'))
        .collect()
}

impl Widget for BottomPanel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a single leaf with a fixed Style cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
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
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &self.rect,
        );
        // Top divider: the raised panel's own derived edge against the editor.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y0),
                Point::new(self.rect.x1, self.rect.y0),
            ),
        );

        self.draw_tab_strip(scene, transform, text_renderer, theme);

        let content_y0 = self.rect.y0 + self.tab_height() + 6.0;
        match self.tab {
            PanelTab::Problems => {
                self.draw_problems(scene, transform, text_renderer, theme, content_y0);
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
                        13.0 * theme.text_scale,
                        c.text_dim,
                        transform
                            * Affine::translate((
                                self.rect.x0 + 14.0 * self.text_scale,
                                content_y0,
                            )),
                        Some((self.rect.width() - 28.0 * self.text_scale) as f32),
                    );
                }
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.tab != PanelTab::Search || !self.rect.contains(self.last_cursor) {
                    return;
                }
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
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
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.last_cursor.y >= self.rect.y0
                    && self.last_cursor.y <= self.rect.y0 + self.tab_height()
                {
                    for (x0, x1, tab) in &self.tab_rects {
                        if self.last_cursor.x >= *x0 && self.last_cursor.x < *x1 {
                            if self.tab != *tab {
                                self.tab = *tab;
                                self.tab_changed = true;
                                self.dirty = true;
                            }
                            *ctx.handled = true;
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
                            *ctx.handled = true;
                            break;
                        }
                    }
                }
            }
            _ => {}
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

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    fn tr() -> TextRenderer {
        TextRenderer::new()
    }

    fn row(rel: &str, line: usize) -> HitRow {
        HitRow {
            rel: rel.to_string(),
            line,
            preview: "x".into(),
            highlight: (0, 1),
        }
    }

    #[test]
    fn ellipsizing_fits_the_width_and_keeps_the_actionable_end() {
        // Each phase uses a fresh text renderer.
        a_short_string_is_left_alone();
        a_long_path_keeps_its_file_and_line();
        a_long_preview_keeps_its_beginning();
        zero_width_yields_nothing_rather_than_an_ellipsis();
    }

    fn a_short_string_is_left_alone() {
        let mut t = tr();
        assert_eq!(
            ellipsize_front(&mut t, "src/a.rs:1", 12.0, 500.0),
            "src/a.rs:1"
        );
        assert_eq!(
            ellipsize_back(&mut t, "fn main() {}", 12.0, 500.0),
            "fn main() {}"
        );
    }

    fn a_long_path_keeps_its_file_and_line() {
        // Front trimming must preserve the actionable file and line suffix.
        let mut t = tr();
        let long = "workspace/components/editor/src/diagnostics/rendering.rs:682";
        let out = ellipsize_front(&mut t, long, 12.0, 120.0);
        assert!(out.starts_with('\u{2026}'), "trimmed from the front: {out}");
        assert!(out.ends_with(":682"), "the line number survives: {out}");
        assert!(
            f64::from(t.measure(&out, 12.0, None).0) <= 120.0,
            "and it actually fits: {out}"
        );
    }

    fn a_long_preview_keeps_its_beginning() {
        let mut t = tr();
        let long = "a".repeat(400);
        let out = ellipsize_back(&mut t, &long, 12.0, 100.0);
        assert!(out.ends_with('\u{2026}'), "trimmed from the back");
        assert!(out.starts_with('a'));
        assert!(f64::from(t.measure(&out, 12.0, None).0) <= 100.0);
    }

    fn zero_width_yields_nothing_rather_than_an_ellipsis() {
        let mut t = tr();
        assert_eq!(ellipsize_front(&mut t, "x", 12.0, 0.0), "");
        assert_eq!(ellipsize_back(&mut t, "x", 12.0, -5.0), "");
    }

    #[test]
    fn search_tab_summary_activation_and_scroll_follow_the_query() {
        // Each phase builds a fresh panel.
        the_search_summary_says_what_is_true();
        the_search_tab_reports_the_row_the_user_clicked();
        a_new_query_resets_the_scroll_but_streaming_does_not();
    }

    fn the_search_summary_says_what_is_true() {
        let mut p = BottomPanel::new();
        p.set_tab(PanelTab::Search);
        assert_eq!(p.search_summary(), "", "no query yet");

        p.set_search(SearchState {
            query: "needle".into(),
            rows: Vec::new(),
            running: true,
            truncated: false,
            files_scanned: 0,
        });
        assert_eq!(p.search_summary(), "Searching\u{2026}");

        p.set_search(SearchState {
            query: "needle".into(),
            rows: Vec::new(),
            running: false,
            truncated: false,
            files_scanned: 12,
        });
        assert_eq!(
            p.search_summary(),
            "No results for \u{201C}needle\u{201D} \u{2014} 12 files scanned"
        );

        let rows = vec![row("a.rs", 1), row("a.rs", 9), row("b.rs", 3)];
        p.set_search(SearchState {
            query: "needle".into(),
            rows: rows.clone(),
            running: false,
            truncated: false,
            files_scanned: 12,
        });
        assert_eq!(
            p.search_summary(),
            "3 results in 2 files \u{2014} 12 files scanned"
        );

        p.set_search(SearchState {
            query: "needle".into(),
            rows,
            running: false,
            truncated: true,
            files_scanned: 12,
        });
        assert!(
            p.search_summary().contains("narrow the search"),
            "a truncated walk must say so: {}",
            p.search_summary()
        );
    }

    fn the_search_tab_reports_the_row_the_user_clicked() {
        let mut p = BottomPanel::new();
        assert_eq!(p.take_activated_hit(), None);
        p.activated_hit = Some(4);
        assert_eq!(p.take_activated_hit(), Some(4));
        assert_eq!(p.take_activated_hit(), None, "cleared on read");
    }

    #[test]
    fn panel_layout_centres_tab_labels_and_budgets_search_rows() {
        // Each phase builds a fresh panel.
        tab_labels_share_one_centre_line_inside_the_strip();
        the_search_tab_is_taller_than_the_others();
        the_row_budget_reserves_room_for_the_summary_and_the_more_hint();
    }

    /// Every tab label is placed from its measured line height, so the three
    /// labels share one glyph top inside the strip and none escapes it.
    fn tab_labels_share_one_centre_line_inside_the_strip() {
        let mut panel = BottomPanel::new();
        panel.rect = Rect::new(0.0, 100.0, 900.0, 100.0 + PANEL_HEIGHT);
        let theme = Theme::dark();
        let mut tr = tr();
        let mut scene = Scene::new();
        panel.draw_tab_strip(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let runs = &scene.encoding().resources.glyph_runs;
        assert_eq!(runs.len(), 3, "one glyph run per tab label");
        let tops: Vec<f32> = runs
            .iter()
            .map(|run| run.transform.translation[1])
            .collect();
        for top in &tops {
            assert!(
                (top - tops[0]).abs() < 0.01,
                "tab labels are placed on different lines: {tops:?}"
            );
        }
        let (_, line_h) = tr.measure_weighted("Problems (0)", 12.0, None, TextWeight::Semibold);
        let expected = 100.0 + (TAB_H - f64::from(line_h)) / 2.0;
        assert!(
            (f64::from(tops[0]) - expected).abs() < 0.01,
            "label top {} is not centred in the {TAB_H}px strip (expected {expected})",
            tops[0]
        );
        assert!(f64::from(tops[0]) + f64::from(line_h) <= 100.0 + TAB_H);
    }

    fn the_search_tab_is_taller_than_the_others() {
        let mut p = BottomPanel::new();
        assert_eq!(p.height(), PANEL_HEIGHT);
        p.set_tab(PanelTab::Search);
        assert_eq!(p.height(), PANEL_HEIGHT_SEARCH);
    }

    fn a_new_query_resets_the_scroll_but_streaming_does_not() {
        let mut p = BottomPanel::new();
        p.set_search(SearchState {
            query: "a".into(),
            rows: (0..40).map(|i| row("f.rs", i)).collect(),
            running: true,
            truncated: false,
            files_scanned: 0,
        });
        p.search_scroll = 10;
        // More hits for the same query arrive: the user's scroll position holds.
        p.set_search(SearchState {
            query: "a".into(),
            rows: (0..80).map(|i| row("f.rs", i)).collect(),
            running: false,
            truncated: false,
            files_scanned: 0,
        });
        assert_eq!(p.search_scroll, 10);
        // A different query is a different list: back to the top.
        p.set_search(SearchState {
            query: "b".into(),
            rows: (0..5).map(|i| row("f.rs", i)).collect(),
            running: false,
            truncated: false,
            files_scanned: 0,
        });
        assert_eq!(p.search_scroll, 0);
    }

    fn the_row_budget_reserves_room_for_the_summary_and_the_more_hint() {
        // A row count that ignored those two strips would draw the last row on
        // top of the "N more" line.
        let mut p = BottomPanel::new();
        p.set_tab(PanelTab::Search);
        p.rect = Rect::new(0.0, 0.0, 900.0, PANEL_HEIGHT_SEARCH);
        let rows = p.visible_hit_rows();
        assert!(rows > 0, "a 300px panel shows some rows");
        let used = TAB_H + 26.0 + rows as f64 * HIT_ROW_H;
        assert!(
            used <= PANEL_HEIGHT_SEARCH - 20.0,
            "rows must stop short of the hint strip: used {used} of {PANEL_HEIGHT_SEARCH}"
        );
    }
}
