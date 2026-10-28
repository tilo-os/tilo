//! `SearchResultsView` — the shared results overlay for Tilo global search.
//!
//! Renders the ranked, kind-grouped [`SearchResult`]s
//! that a [`SearchRegistry`](crate::search::SearchRegistry) produces: one
//! section per kind, then keyboard- and pointer-navigable rows. Activating a
//! row latches its opaque `action_id`, which the host drains with
//! [`SearchResultsView::take_activated`] and dispatches on — the framework
//! never interprets it.
//!
//! The view is **self-laid-out** (like the command palette): the host gives
//! it a rect — via the layout pass, or directly with
//! [`SearchResultsView::set_rect`] for a free-floating overlay — and it lays
//! its own headers and rows top-down from the current groups.

use crate::widget::ModelPath;
use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::search::{
    PREVIEW_BODY_LINES, PREVIEW_LINES, SearchGroup, SearchPreview, SearchResult, SearchRow,
};
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{text_band, type_line_height};

const PAD: f64 = crate::tokens::Spacing::BASE.s2 as f64; // card inner padding
const ROW_INSET: f64 = 8.0; // rows inset from the card edge
const HEADER_H: f64 = 26.0;
const ROW_H_ONE_LINE: f64 = 38.0;
const ROW_H_TWO_LINE: f64 = 50.0;
const ROW_TEXT_X: f64 = 12.0; // text/icon inset within a row
const ICON_D: f64 = 18.0;
const ICON_COL: f64 = 30.0; // icon box + gap when a row has an icon
const EMPTY_H: f64 = 92.0;
/// A lifted row (selection, hover) is inset this much top and bottom, so a
/// selected row and its hovered neighbour keep one `s1` between them instead
/// of fusing into one block (HIG §7 "nothing touches").
const ROW_LIFT_INSET: f64 = crate::tokens::Spacing::BASE.s1 as f64 / 2.0;

/// Composes two affine transforms. `Affine`'s `Mul` impl is pure `f64` matrix
/// multiply-add with no overflow trap, so it cannot panic regardless of
/// operand magnitude — centralised here rather than repeating the exemption
/// at every text transform composed below.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn compose(a: Affine, b: Affine) -> Affine {
    a * b
}

/// Used by both `draw` and hit-testing so they never disagree.
struct RowGeom {
    rect: Rect,
    flat: usize,
    gi: usize,
    ri: usize,
}

struct LayoutPlan {
    headers: Vec<(Rect, String, usize)>, // (header rect, kind, result count)
    rows: Vec<RowGeom>,
}

/// Row and header bands. The constants are the 1× defaults; `build_layout`
/// resolves them from the theme's type so a 2× title still has its 1×
/// padding instead of overrunning the next row. A free-floating overlay
/// that never goes through layout keeps the defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowBands {
    header: f64,
    one_line: f64,
    two_line: f64,
    /// Sender, subject and snippet.
    three_line: f64,
    /// A thumbnail beside two lines.
    media: f64,
    /// The preview band's heading line.
    preview_title: f64,
    /// One of the preview band's caption lines.
    preview_line: f64,
    /// The step between the preview band's parts, and its insets.
    preview_gap: f64,
}

impl RowBands {
    const DEFAULT: Self = Self {
        header: HEADER_H,
        one_line: ROW_H_ONE_LINE,
        two_line: ROW_H_TWO_LINE,
        three_line: ROW_H_TWO_LINE + ROW_H_ONE_LINE / 2.0,
        media: ROW_H_TWO_LINE,
        preview_title: 22.0,
        preview_line: 18.0,
        preview_gap: 4.0,
    };

    /// Title (`base`) and subtitle (`sm`) lines plus one sibling step of
    /// padding; the caption band holds its `xs` line plus its two `s1`
    /// insets. Every value floors at the pinned 1× constant.
    fn resolve(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let title = type_line_height(&tokens.typography, TextRole::Body);
        let subtitle = type_line_height(&tokens.typography, TextRole::Caption);
        let caption = type_line_height(&tokens.typography, TextRole::Caption);
        let heading = type_line_height(&tokens.typography, TextRole::Subheading);
        let pad = f64::from(tokens.spacing.s3);
        let gap = f64::from(tokens.spacing.s1);
        Self {
            header: text_band(HEADER_H, heading, 2.0 * gap),
            one_line: text_band(ROW_H_ONE_LINE, title, pad),
            two_line: text_band(ROW_H_TWO_LINE, title + gap + subtitle, pad),
            three_line: text_band(
                Self::DEFAULT.three_line,
                subtitle + gap + title + gap + caption,
                pad,
            ),
            media: text_band(
                Self::DEFAULT.media,
                (title + gap + subtitle).max(thumbnail_size(theme).1),
                pad,
            ),
            preview_title: heading.max(Self::DEFAULT.preview_title),
            preview_line: caption.max(Self::DEFAULT.preview_line),
            preview_gap: gap.max(Self::DEFAULT.preview_gap),
        }
    }

    /// The preview band `preview` takes: its parts one under another, with
    /// a step between them and an `s2` inset above and below.
    fn preview(&self, preview: &SearchPreview) -> f64 {
        let parts = [
            preview.title.as_ref().map(|_| self.preview_title),
            (!preview.facts.is_empty()).then_some(self.preview_line),
            (!preview.lines.is_empty())
                .then(|| self.preview_line * line_count(preview.lines.len().min(PREVIEW_LINES))),
            preview
                .body
                .as_ref()
                .map(|_| self.preview_line * line_count(PREVIEW_BODY_LINES)),
        ];
        let shown: Vec<f64> = parts.into_iter().flatten().collect();
        let gaps = self.preview_gap * line_count(shown.len().saturating_sub(1));
        shown.iter().sum::<f64>() + gaps + 2.0 * PAD
    }
}

/// `n` lines as a multiplier of a line's height.
fn line_count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The results overlay. Populate it with [`set_groups`](Self::set_groups) each
/// time the query changes, drive selection with the keyboard or pointer, and
/// drain activations with [`take_activated`](Self::take_activated).
pub struct SearchResultsView {
    groups: Vec<SearchGroup>,
    /// flat index → (group idx, result idx) for the selectable rows.
    flat: Vec<(usize, usize)>,
    cursor: usize,
    hover: Option<usize>,
    rect: Rect,
    last_cursor: Point,
    activated: Option<String>,
    empty_text: String,
    test_id: Option<String>,
    bands: RowBands,
    /// More results are on their way: a "Searching…" row ends the list.
    loading: bool,
    /// Room kept under the empty-state line for controls its owner places
    /// inside the card (a confirmation's buttons).
    empty_footer: f64,
    /// The most the overlay may stand: rows past it scroll, under a preview
    /// band that stays put.
    max_height: Option<f64>,
    /// How far the rows are scrolled up, in logical px.
    scroll: f64,
    /// The cursor moved: bring its row into view at the next layout.
    reveal: bool,
}

impl SearchResultsView {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            flat: Vec::new(),
            cursor: 0,
            hover: None,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            activated: None,
            empty_text: "No results".to_string(),
            test_id: None,
            bands: RowBands::DEFAULT,
            loading: false,
            empty_footer: 0.0,
            max_height: None,
            scroll: 0.0,
            reveal: false,
        }
    }

    /// Keep `height` free under the empty-state line, for controls the
    /// owner draws inside the card. The line stays centred above them.
    pub fn set_empty_footer(&mut self, height: f64) {
        self.empty_footer = height.max(0.0);
    }

    /// Show that more results are still coming: a loading row after the
    /// last result. With no results the empty text says so instead.
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Override the centered empty-state line shown when there are no results.
    #[must_use]
    pub fn with_empty_text(mut self, text: impl Into<String>) -> Self {
        self.empty_text = text.into();
        self
    }

    /// Update the empty-state copy without rebuilding the shared result view.
    pub fn set_empty_text(&mut self, text: impl Into<String>) {
        self.empty_text = text.into();
    }

    /// Give the overlay a stable test id so scenarios can resolve it.
    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Rebuilds the flat selection index and clamps the cursor so it always
    /// points at a real row (or `0` when empty).
    pub fn set_groups(&mut self, groups: Vec<SearchGroup>) {
        self.flat.clear();
        for (gi, g) in groups.iter().enumerate() {
            for ri in 0..g.results.len() {
                self.flat.push((gi, ri));
            }
        }
        self.groups = groups;
        if self.cursor >= self.flat.len() {
            self.cursor = self.flat.len().saturating_sub(1);
        }
        // New results read from their top.
        self.scroll = 0.0;
        self.reveal = true;
        // Hover no longer maps to a known row until the pointer moves again.
        self.hover = None;
    }

    /// Buckets by kind in first-seen order, for callers not going through the registry's grouping.
    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        let mut groups: Vec<SearchGroup> = Vec::new();
        for r in results {
            if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
                g.results.push(r);
            } else {
                groups.push(SearchGroup {
                    kind: r.kind.clone(),
                    results: vec![r],
                });
            }
        }
        self.set_groups(groups);
    }

    pub fn groups(&self) -> &[SearchGroup] {
        &self.groups
    }

    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    pub fn len(&self) -> usize {
        self.flat.len()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Clamped to the row range.
    pub fn set_cursor(&mut self, index: usize) {
        if !self.flat.is_empty() {
            self.cursor = index.min(self.flat.len().saturating_sub(1));
            self.reveal = true;
        }
    }

    /// Preserve one exact control across an asynchronous result replacement.
    /// A missing identity leaves no actionable selection; it never falls back
    /// to the row that inherited the old index.
    pub(crate) fn restore_action_selection(&mut self, id: &str) -> bool {
        let index = self
            .flat
            .iter()
            .position(|&(group, row)| self.groups[group].results[row].action_id == id);
        self.cursor = index.unwrap_or(usize::MAX);
        self.reveal = true;
        index.is_some()
    }

    pub fn selected(&self) -> Option<&SearchResult> {
        let (gi, ri) = *self.flat.get(self.cursor)?;
        self.groups.get(gi)?.results.get(ri)
    }

    fn preview_result(&self) -> Option<&SearchResult> {
        let at = |flat: usize| {
            let (gi, ri) = *self.flat.get(flat)?;
            self.groups
                .get(gi)?
                .results
                .get(ri)
                .filter(|result| result.preview.is_some())
        };
        self.hover.and_then(at).or_else(|| at(self.cursor))
    }

    /// The preview band under the list: as tall as the tallest preview any
    /// listed row has, so moving between rows never moves the list; none
    /// when no row has one.
    fn preview_band(&self) -> f64 {
        self.groups
            .iter()
            .flat_map(|group| &group.results)
            .filter_map(|result| result.preview.as_ref())
            .map(|preview| self.bands.preview(preview))
            .fold(0.0, f64::max)
    }

    pub fn selected_action_id(&self) -> Option<&str> {
        self.selected().map(|r| r.action_id.as_str())
    }

    /// Move the highlight down one row, wrapping to the top past the end.
    pub fn move_down(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.reveal = true;
        if self.selected().is_none() {
            self.cursor = 0;
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(1)
            .checked_rem(self.flat.len())
            .unwrap_or(0);
    }

    /// Move the highlight up one row, wrapping to the bottom past the top.
    pub fn move_up(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.reveal = true;
        if self.selected().is_none() {
            self.cursor = self.flat.len() - 1;
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(self.flat.len())
            .saturating_sub(1)
            .checked_rem(self.flat.len())
            .unwrap_or(0);
    }

    /// No-op when there is no selection.
    pub fn activate_selected(&mut self) {
        if let Some(id) = self.selected().map(|r| r.action_id.clone()) {
            self.activated = Some(id);
        }
    }

    /// `None` until a row is activated; drains after one read.
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }

    /// For a free-floating overlay not driven by the layout pass.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        if std::mem::take(&mut self.reveal) {
            self.reveal_cursor();
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// The most the overlay may stand, or `None` for as tall as its rows:
    /// past it the rows scroll and the preview band stays at the bottom.
    pub fn set_max_height(&mut self, max: Option<f64>) {
        self.max_height = max;
    }

    /// Falls back to an empty-state height with no results.
    pub fn desired_height(&self) -> f64 {
        if self.groups.is_empty() {
            return EMPTY_H + self.empty_footer;
        }
        let full = self.rows_height() + self.preview_band();
        // However little room there is, a header and its first row show.
        let least = PAD * 2.0 + self.bands.header + self.bands.two_line + self.preview_band();
        self.max_height.map_or(full, |max| full.min(max.max(least)))
    }

    /// The rows' whole height, padding, headers and loading row included.
    fn rows_height(&self) -> f64 {
        let mut h = PAD * 2.0;
        for g in &self.groups {
            h += self.bands.header;
            for r in &g.results {
                h += self.row_height(r);
            }
        }
        if self.loading {
            h += self.bands.one_line;
        }
        h
    }

    /// Where the rows show: the card above its preview band.
    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (self.rect.y1 - self.preview_band()).max(self.rect.y0),
        )
    }

    /// How far the rows can scroll before their end shows.
    fn max_scroll(&self) -> f64 {
        (self.rows_height() - self.list_rect().height()).max(0.0)
    }

    /// Scroll so the cursor's row shows whole, with its group's header when
    /// it is the group's first row.
    fn reveal_cursor(&mut self) {
        let Some(row) = self
            .plan()
            .rows
            .into_iter()
            .find(|row| row.flat == self.cursor)
        else {
            return;
        };
        let list = self.list_rect();
        let top = if row.ri == 0 {
            row.rect.y0 - self.bands.header
        } else {
            row.rect.y0
        };
        if top < list.y0 + PAD {
            self.scroll -= list.y0 + PAD - top;
        } else if row.rect.y1 > list.y1 - PAD {
            self.scroll += row.rect.y1 - (list.y1 - PAD);
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// Scroll the rows by `dy` logical px (positive moves down the list);
    /// returns whether they moved.
    fn scroll_by(&mut self, dy: f64) -> bool {
        let before = self.scroll;
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll());
        (self.scroll - before).abs() > f64::EPSILON
    }

    fn row_height(&self, r: &SearchResult) -> f64 {
        match &r.row {
            Some(SearchRow::Message {
                snippet: Some(_), ..
            }) => self.bands.three_line,
            Some(SearchRow::Message { .. } | SearchRow::FileHit { .. }) => self.bands.two_line,
            Some(SearchRow::Media { .. }) => self.bands.media,
            None if r.subtitle.is_some() => self.bands.two_line,
            None => self.bands.one_line,
        }
    }

    /// The loading row's band, after the last result.
    fn loading_rect(&self) -> Option<Rect> {
        if !self.loading || self.flat.is_empty() {
            return None;
        }
        let last = self
            .plan()
            .rows
            .last()
            .map_or(self.rect.y0 + PAD, |row| row.rect.y1);
        Some(Rect::new(
            self.rect.x0 + ROW_INSET,
            last,
            self.rect.x1 - ROW_INSET,
            last + self.bands.one_line,
        ))
    }

    /// Single source of truth shared by `draw` and `row_at`, so a click
    /// always lands on the row that was drawn.
    fn plan(&self) -> LayoutPlan {
        let mut headers = Vec::new();
        let mut rows = Vec::new();
        let x0 = self.rect.x0 + ROW_INSET;
        let x1 = self.rect.x1 - ROW_INSET;
        let mut y = self.rect.y0 + PAD - self.scroll;
        let mut flat = 0usize;
        let header_h = self.bands.header;
        for (gi, g) in self.groups.iter().enumerate() {
            headers.push((
                Rect::new(x0, y, x1, y + header_h),
                g.kind.clone(),
                g.results.len(),
            ));
            y += header_h;
            for (ri, r) in g.results.iter().enumerate() {
                let rh = self.row_height(r);
                rows.push(RowGeom {
                    rect: Rect::new(x0, y, x1, y + rh),
                    flat,
                    gi,
                    ri,
                });
                y += rh;
                flat = flat.saturating_add(1);
            }
        }
        LayoutPlan { headers, rows }
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        if !self.list_rect().contains(p) {
            return None;
        }
        self.plan()
            .rows
            .into_iter()
            .find(|r| r.rect.contains(p))
            .map(|r| r.flat)
    }

    fn draw_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: &RowGeom,
    ) {
        let colors = &theme.tokens.colors;
        let Some(result) = self.groups.get(row.gi).and_then(|g| g.results.get(row.ri)) else {
            return;
        };
        let selected = row.flat == self.cursor;
        let hovered = self.hover == Some(row.flat);
        // Selection is the panel's strong lift, as the armed row of every
        // menu; hover is a barely-there veneer, distinct from it.
        let (armed_fill, selected_ink) = crate::widget::floating_surface::armed_row(theme);
        let lift = RoundedRect::from_rect(
            row.rect.inflate(0.0, -ROW_LIFT_INSET),
            f64::from(theme.tokens.radii.detail),
        );
        if selected {
            scene.fill(Fill::NonZero, transform, armed_fill, None, &lift);
        } else if hovered {
            // One step off the panel it sits on, polarity chosen the way
            // `hairline_on` chooses it — the white veneer this replaces was
            // invisible over a light panel (HIG §9.5).
            let hover = crate::tokens::colors::Colors::hairline_on(
                crate::chrome::shell::panel_ground(theme),
                crate::chrome::shell::HAIRLINE_ALPHA,
            );
            scene.fill(Fill::NonZero, transform, hover, None, &lift);
        }

        if let Some(template) = &result.row {
            let ink = RowInk {
                primary: if selected {
                    selected_ink
                } else {
                    colors.text_primary
                },
                secondary: if selected {
                    selected_ink
                } else {
                    colors.text_dim
                },
                tertiary: if selected {
                    selected_ink
                } else {
                    colors.text_tertiary
                },
            };
            draw_template(scene, transform, tr, theme, row.rect, template, ink);
            return;
        }

        let mut text_x = row.rect.x0 + ROW_TEXT_X;

        // Leading glyph (icon color follows selection so it reads on the fill).
        if let Some(icon) = result.icon {
            let icon_rect = Rect::from_center_size(
                Point::new(text_x + ICON_D / 2.0, row.rect.center().y),
                (ICON_D, ICON_D),
            );
            let icon_col = if selected {
                selected_ink
            } else {
                colors.text_dim
            };
            icon.draw_with_stroke(scene, transform, icon_rect, icon_col, 1.6);
            text_x += ICON_COL;
        }

        let tokens = &theme.tokens;
        let title_style = tokens.typography.style(TextRole::Body);
        let sub_style = tokens.typography.style(TextRole::Caption);
        // Clamped to >= 0.0 above; a row's text width is a small on-screen
        // pixel span, well inside `f32`'s exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "row text width is a small non-negative on-screen pixel span"
        )]
        let text_w = (row.rect.x1 - text_x - ROW_TEXT_X).max(0.0) as f32;

        // Title and subtitle each keep to one line, elided to the row's
        // width: the row's height has room for one line of each, and a
        // wrapped title ran into the subtitle beneath it. The block is
        // centred as one, so the icon (centred on the row) sits on the
        // block's centre line whether or not a subtitle is present.
        let title_style = title_style.with_weight(TextWeight::Medium);
        let title = tr.elide_to_width_style(&result.title, text_w, &title_style);
        let (_, title_h) = tr.measure_style(&title, None, &title_style);
        let gap = f64::from(tokens.spacing.s1);
        let subtitle = result
            .subtitle
            .as_deref()
            .map(|sub| tr.elide_to_width_style(sub, text_w, &sub_style));
        let sub_h = subtitle
            .as_deref()
            .map(|sub| f64::from(tr.measure_style(sub, None, &sub_style).1));
        let block_h = f64::from(title_h) + sub_h.map_or(0.0, |h| gap + h);
        let title_y = row.rect.y0 + ((row.rect.height() - block_h) / 2.0).max(0.0);
        tr.draw_style(
            scene,
            &title,
            if selected {
                selected_ink
            } else {
                colors.text_primary
            },
            compose(transform, Affine::translate((text_x, title_y))),
            None,
            &title_style,
        );

        // Subtitle (dim; on a selected row it stays readable on the fill).
        if let Some(sub) = &subtitle {
            let sy = title_y + f64::from(title_h) + gap;
            let sub_col = if selected {
                selected_ink
            } else {
                colors.text_dim
            };
            tr.draw_style(
                scene,
                sub,
                sub_col,
                compose(transform, Affine::translate((text_x, sy))),
                None,
                &sub_style,
            );
        }
    }

    /// Where the empty-state line is set: its left edge, the width it wraps
    /// at and its style. A longer line (why a confirmation asks) wraps inside
    /// the card, centred, rather than running past a narrow window's edges.
    fn empty_frame(&self, theme: &Theme) -> (f64, f32, crate::text_render::TextStyle) {
        let style = theme
            .text(TextRole::Caption)
            .with_align(crate::text_render::ParagraphAlign::Center);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "card widths are logical pixels well within f32 range"
        )]
        let width = (self.rect.width() - PAD * 2.0).max(1.0) as f32;
        (self.rect.x0 + PAD, width, style)
    }

    fn draw_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let (x, width, style) = self.empty_frame(theme);
        let (_, th) = tr.measure_style(&self.empty_text, Some(width), &style);
        let cy = (self.rect.y1 - self.empty_footer + self.rect.y0) / 2.0 - f64::from(th) / 2.0;
        tr.draw_style(
            scene,
            &self.empty_text,
            theme.tokens.colors.text_dim,
            compose(transform, Affine::translate((x, cy))),
            Some(width),
            &style,
        );
    }

    fn draw_preview(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let band = self.preview_band();
        if band <= 0.0 {
            return;
        }
        let Some(preview) = self
            .preview_result()
            .and_then(|result| result.preview.as_ref())
        else {
            return;
        };
        let rect = Rect::new(
            self.rect.x0 + ROW_INSET,
            self.rect.y1 - band,
            self.rect.x1 - ROW_INSET,
            self.rect.y1,
        );
        crate::surface::separator(
            scene,
            transform,
            rect.x0,
            rect.x1,
            rect.y0,
            0.0,
            theme.tokens.colors.surfaces.base,
        );
        let colors = &theme.tokens.colors;
        let heading = theme.text(TextRole::Subheading);
        let caption = theme.text(TextRole::Caption);
        let x = rect.x0 + ROW_TEXT_X;
        let width = (rect.x1 - ROW_TEXT_X - x).max(0.0);
        let (gap, title_h, line_h) = (
            self.bands.preview_gap,
            self.bands.preview_title,
            self.bands.preview_line,
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a preview's width is a small on-screen pixel span"
        )]
        let room = width as f32;
        let body = preview.body.as_ref().map(|body| {
            let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
            tr.clamp_to_lines_style(&normalized, room, PREVIEW_BODY_LINES, &caption)
        });
        let mut y = rect.y0 + PAD;
        if let Some(title) = &preview.title {
            line(
                scene,
                transform,
                tr,
                title,
                colors.text_primary,
                Point::new(x, y),
                width,
                &heading,
            );
            y += title_h + gap;
        }
        if !preview.facts.is_empty() {
            // Facts stand in columns on one line, apart by a column gap,
            // never chained with separators.
            let column_gap = f64::from(theme.tokens.spacing.s4);
            let mut fx = x;
            for fact in &preview.facts {
                let room = (x + width - fx).max(0.0);
                if room <= 0.0 {
                    break;
                }
                line(
                    scene,
                    transform,
                    tr,
                    fact,
                    colors.text_dim,
                    Point::new(fx, y),
                    room,
                    &caption,
                );
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a fact's width is a small on-screen pixel span"
                )]
                let shown = tr.elide_to_width_style(fact, room as f32, &caption);
                let (w, _) = tr.measure_style(&shown, None, &caption);
                fx += f64::from(w) + column_gap;
            }
            y += line_h + gap;
        }
        // The item lines and the body, each with the step after it.
        let mut parts: Vec<(Vec<String>, f64)> = Vec::new();
        if !preview.lines.is_empty() {
            let items = preview.lines.iter().take(PREVIEW_LINES).cloned().collect();
            parts.push((items, line_h));
        }
        if let Some(body) = body {
            parts.push((body, line_h));
        }
        for (lines, height) in parts {
            for text in lines {
                line(
                    scene,
                    transform,
                    tr,
                    &text,
                    colors.text_primary,
                    Point::new(x, y),
                    width,
                    &caption,
                );
                y += height;
            }
            y += gap;
        }
    }
}

/// A media row's 16:9 thumbnail: as tall as a large control.
fn thumbnail_size(theme: &Theme) -> (f64, f64) {
    let height = crate::widget::ControlSize::Large.height(theme);
    (height * 16.0 / 9.0, height)
}

/// The unread dot before a message's sender: a detail-radius disc.
fn unread_dot(theme: &Theme) -> f64 {
    f64::from(theme.tokens.radii.detail) * 1.5
}

/// The inks a row's text takes: on a selected (inverted) row all three are
/// the inverted label colour.
#[derive(Clone, Copy)]
struct RowInk {
    primary: vello::peniko::Color,
    secondary: vello::peniko::Color,
    tertiary: vello::peniko::Color,
}

/// Draw one line of `text` at `(x, y)`, elided to `width`; returns its
/// height.
fn line(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    text: &str,
    ink: vello::peniko::Color,
    at: Point,
    width: f64,
    style: &crate::text_render::TextStyle,
) -> f64 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a row's text width is a small on-screen pixel span"
    )]
    let room = width.max(0.0) as f32;
    let shown = tr.elide_to_width_style(text, room, style);
    let (_, h) = tr.measure_style(&shown, None, style);
    tr.draw_style(
        scene,
        &shown,
        ink,
        compose(transform, Affine::translate((at.x, at.y))),
        None,
        style,
    );
    f64::from(h)
}

/// Draw trailing `text` right-aligned to `right` on the line at `y`; returns
/// the x where it starts.
fn trailing(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    text: &str,
    ink: vello::peniko::Color,
    right: f64,
    y: f64,
    style: &crate::text_render::TextStyle,
) -> f64 {
    let (w, _) = tr.measure_style(text, None, style);
    let x = right - f64::from(w);
    tr.draw_style(
        scene,
        text,
        ink,
        compose(transform, Affine::translate((x, y))),
        None,
        style,
    );
    x
}

/// A search hit drawn as what it is. Every template shares the list row's
/// insets and centres its block of lines in the row.
fn draw_template(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    template: &SearchRow,
    ink: RowInk,
) {
    let gap = f64::from(theme.tokens.spacing.s1);
    let x0 = rect.x0 + ROW_TEXT_X;
    let x1 = rect.x1 - ROW_TEXT_X;
    let label = theme.text(TextRole::Label);
    let body = theme.text(TextRole::Body);
    let caption = theme.text(TextRole::Caption);
    let meta = theme.text(TextRole::Caption).numeric();
    let height = |style: &crate::text_render::TextStyle, tr: &mut TextRenderer| {
        f64::from(tr.measure_style("Ag", None, style).1)
    };
    match template {
        SearchRow::Message {
            sender,
            subject,
            snippet,
            date,
            unread,
        } => {
            let sender_style = if *unread {
                label.clone().strong()
            } else {
                label.clone()
            };
            let lines = height(&sender_style, tr)
                + gap
                + height(&body, tr)
                + snippet.as_ref().map_or(0.0, |_| gap + height(&caption, tr));
            let mut y = rect.y0 + ((rect.height() - lines) / 2.0).max(0.0);
            let mut x = x0;
            if *unread {
                // The unread dot: status, in primary ink, centred on the
                // sender's line.
                let dot = unread_dot(theme);
                let dot_cy = y + height(&sender_style, tr) / 2.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    ink.primary,
                    None,
                    &vello::kurbo::Circle::new((x + dot / 2.0, dot_cy), dot / 2.0),
                );
                x += dot + gap * 2.0;
            }
            let right = date.as_deref().map_or(x1, |date| {
                trailing(scene, transform, tr, date, ink.tertiary, x1, y, &meta) - gap * 2.0
            });
            y += line(
                scene,
                transform,
                tr,
                sender,
                ink.primary,
                Point::new(x, y),
                right - x,
                &sender_style,
            ) + gap;
            y += line(
                scene,
                transform,
                tr,
                subject,
                ink.primary,
                Point::new(x, y),
                x1 - x,
                &body,
            ) + gap;
            if let Some(snippet) = snippet {
                let flat = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
                line(
                    scene,
                    transform,
                    tr,
                    &flat,
                    ink.secondary,
                    Point::new(x, y),
                    x1 - x,
                    &caption,
                );
            }
        }
        SearchRow::Media {
            thumbnail,
            title,
            byline,
            duration,
        } => {
            let (thumb_w, thumb_h) = thumbnail_size(theme);
            let thumb = Rect::new(
                x0,
                rect.center().y - thumb_h / 2.0,
                x0 + thumb_w,
                rect.center().y + thumb_h / 2.0,
            );
            let corner = f64::from(theme.tokens.radii.detail);
            let drawn = thumbnail
                .as_ref()
                .is_some_and(|image| crate::icons::draw_app_image(scene, transform, thumb, image));
            if !drawn {
                // No artwork yet: a quiet well with the kind's glyph, one
                // hairline step off the panel it sits on.
                let well = crate::tokens::colors::Colors::hairline_on(
                    crate::chrome::shell::panel_ground(theme),
                    crate::chrome::shell::HAIRLINE_ALPHA,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    well,
                    None,
                    &RoundedRect::from_rect(thumb, corner),
                );
                let glyph = f64::from(theme.tokens.metrics.icon_md);
                crate::icons::Icon::Film.draw_with_stroke(
                    scene,
                    transform,
                    Rect::from_center_size(thumb.center(), (glyph, glyph)),
                    ink.tertiary,
                    1.5,
                );
            }
            let x = thumb.x1 + gap * 3.0;
            let lines =
                height(&body, tr) + byline.as_ref().map_or(0.0, |_| gap + height(&caption, tr));
            let mut y = rect.y0 + ((rect.height() - lines) / 2.0).max(0.0);
            let right = duration.as_deref().map_or(x1, |duration| {
                trailing(scene, transform, tr, duration, ink.tertiary, x1, y, &meta) - gap * 2.0
            });
            y += line(
                scene,
                transform,
                tr,
                title,
                ink.primary,
                Point::new(x, y),
                right - x,
                &body.clone().strong(),
            ) + gap;
            if let Some(byline) = byline {
                line(
                    scene,
                    transform,
                    tr,
                    byline,
                    ink.secondary,
                    Point::new(x, y),
                    x1 - x,
                    &caption,
                );
            }
        }
        SearchRow::FileHit {
            path,
            line: number,
            snippet,
            matched,
        } => {
            let code = theme.text(TextRole::Code);
            let location = match number {
                Some(number) => format!("{path}:{number}"),
                None => path.clone(),
            };
            let lines = height(&caption, tr) + gap + height(&code, tr);
            let mut y = rect.y0 + ((rect.height() - lines) / 2.0).max(0.0);
            y += line(
                scene,
                transform,
                tr,
                &location,
                ink.secondary,
                Point::new(x0, y),
                x1 - x0,
                &caption,
            ) + gap;
            // The matched line, leading whitespace dropped, with the matched
            // span marked by the searcher's own selection wash.
            let trimmed = snippet.trim_start();
            let cut = snippet.len().saturating_sub(trimmed.len());
            if let Some(range) = matched.clone()
                && range.start >= cut
                && let (Some(before), Some(hit)) = (
                    trimmed.get(..range.start.saturating_sub(cut)),
                    trimmed.get(range.start.saturating_sub(cut)..range.end.saturating_sub(cut)),
                )
            {
                let (bw, _) = tr.measure_style(before, None, &code);
                let (hw, hh) = tr.measure_style(hit, None, &code);
                let mark = Rect::new(
                    x0 + f64::from(bw),
                    y,
                    (x0 + f64::from(bw) + f64::from(hw)).min(x1),
                    y + f64::from(hh),
                );
                if mark.width() > 0.0 {
                    crate::widget::paint_find_match(scene, transform, mark, true, theme);
                }
            }
            line(
                scene,
                transform,
                tr,
                trimmed,
                ink.primary,
                Point::new(x0, y),
                x1 - x0,
                &code,
            );
        }
    }
}

impl Default for SearchResultsView {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchResultsView {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.bands = RowBands::resolve(theme);
        // A results overlay's desired height is a small on-screen pixel value
        // (a handful of rows), well inside `f32`'s exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "desired overlay height is a small on-screen pixel value"
        )]
        let height_px = self.desired_height() as f32;
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(height_px),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        // Panel role (HIG §9.1): a results overlay floats over content, so it
        // takes the panel ground and the panel edge — the declared
        // `border_standard` this replaces vanished on the dark preset's
        // raised surface.
        let radius = f64::from(theme.tokens.radii.control);
        let card = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::shell::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        if self.flat.is_empty() {
            self.draw_empty(scene, transform, text_renderer, theme);
            return;
        }

        let plan = self.plan();
        // Rows scrolled past the card's edge or under its preview band are
        // cut off there.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.list_rect(),
        );
        // Section headers: the kind in sentence case as a list section
        // header, with a trailing count.
        let header_style = theme.text(TextRole::Subheading);
        let count_style = theme.text(TextRole::Caption).numeric();
        let header_inset = f64::from(theme.tokens.spacing.s1);
        for (hrect, kind, count) in &plan.headers {
            // The label sits on the band's bottom edge, one small step above
            // its rows, by its measured line box rather than its font size.
            let (_, label_h) = text_renderer.measure_style(kind, None, &header_style);
            let hy = hrect.y1 - f64::from(label_h) - header_inset;
            text_renderer.draw_style(
                scene,
                kind,
                theme.tokens.colors.text_dim,
                compose(transform, Affine::translate((hrect.x0 + header_inset, hy))),
                None,
                &header_style,
            );
            let count_str = count.to_string();
            let (cw, ch) = text_renderer.measure_style(&count_str, None, &count_style);
            text_renderer.draw_style(
                scene,
                &count_str,
                theme.tokens.colors.text_tertiary,
                compose(
                    transform,
                    Affine::translate((
                        hrect.x1 - f64::from(cw) - header_inset,
                        hy + f64::from(label_h - ch),
                    )),
                ),
                None,
                &count_style,
            );
        }
        for row in &plan.rows {
            self.draw_row(scene, transform, text_renderer, theme, row);
        }
        if let Some(band) = self.loading_rect() {
            let style = theme.text(TextRole::Caption);
            let (_, h) = text_renderer.measure_style("Searching…", None, &style);
            let y = crate::widget::text::centered_text_top(band.y0, band.height(), f64::from(h));
            text_renderer.draw_style(
                scene,
                "Searching…",
                theme.tokens.colors.text_tertiary,
                compose(transform, Affine::translate((band.x0 + ROW_TEXT_X, y))),
                None,
                &style,
            );
        }
        scene.pop_layer();
        self.draw_preview(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.hover = self.row_at(self.last_cursor);
            }
            // A wheel over the rows scrolls them.
            WindowEvent::MouseWheel { delta, .. }
                if self.list_rect().contains(self.last_cursor) =>
            {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => {
                        f64::from(*y) * -self.bands.two_line
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => -p.y / ctx.scale_factor,
                };
                if self.scroll_by(dy) {
                    self.hover = self.row_at(self.last_cursor);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(flat) = self.row_at(self.last_cursor) {
                    self.cursor = flat;
                    self.activate_selected();
                    *ctx.handled = true;
                }
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
                if self.flat.is_empty() {
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_down();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_up();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.activate_selected();
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SearchResultsView"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _: &mut crate::widget::Edits<'_>,
        _: &ModelPath,
    ) -> bool {
        if !matches!(
            action,
            crate::a11y::Action::Focus | crate::a11y::Action::Activate
        ) {
            return false;
        }
        let Some(index) = self
            .flat
            .iter()
            .position(|&(group, row)| self.groups[group].results[row].action_id == target)
        else {
            return false;
        };
        self.cursor = index;
        if matches!(action, crate::a11y::Action::Activate) {
            self.activate_selected();
        }
        true
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let total = self.flat.len();
        let mut children = if self.flat.is_empty() {
            vec![
                AccessNode::new(Role::StaticText)
                    .with_name(&self.empty_text)
                    .with_live(crate::a11y::Politeness::Polite),
            ]
        } else {
            self.plan()
                .rows
                .into_iter()
                .filter_map(|row| {
                    let result = self.groups.get(row.gi)?.results.get(row.ri)?;
                    let description = result
                        .row
                        .as_ref()
                        .map(SearchRow::spoken)
                        .into_iter()
                        .chain(result.subtitle.iter().cloned())
                        .chain(result.preview.as_ref().map(SearchPreview::spoken))
                        .collect::<Vec<_>>()
                        .join(". ");
                    let mut node = AccessNode::new(Role::ListItem)
                        .with_name(result.title.clone())
                        .with_action_target(&result.action_id)
                        .with_bounds(row.rect)
                        .with_position(row.flat.saturating_add(1), total)
                        .with_state(AccessState::new().selected(row.flat == self.cursor));
                    if !description.is_empty() {
                        node = node.with_description(description);
                    }
                    Some(node)
                })
                .collect()
        };
        if self.loading && !self.flat.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Searching…")
                    .with_live(crate::a11y::Politeness::Polite),
            );
        }
        Some(
            AccessNode::new(Role::List)
                .with_name("Search results")
                .with_children(children),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchRegistry;
    use crate::testing::dispatch;

    /// Relative luminance of the composited pixel at (`x`, `y`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside the test surface"
    )]
    fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        crate::a11y::relative_luminance(vello::peniko::Color::from_rgb8(r, g, b))
    }

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(name))
    }

    /// The results panel keeps a **visible derived edge** at 1× on both
    /// presets (HIG §9.1 Panel role, §9.5): before, it was a raised card under
    /// `border_standard`, three shades from the dark preset's raised surface.
    /// The hovered row's veneer was literal white at 5 %, invisible on light.
    ///
    /// Writes `$UX1X_F4_OUT/<theme>/search_results.png` plus the harness
    /// inspection when that variable is set; asserts regardless.
    #[test]
    fn ux1x_panel_edge_and_hover_register_on_both_presets() {
        use crate::offscreen::OffscreenRenderer;
        const EDGE_MIN: f64 = 0.008;
        // Ground of margin around the panel so its edge is against the app.
        let margin = 16.0;
        // Evidence first, for both presets, so a failing assertion on one
        // preset still leaves the other's PNG and inspection on disk.
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            let mut mounted = view_from("");
            let last = mounted.len() - 1;
            let (w, h) = (
                margin * 2.0 + 360.0,
                margin * 2.0 + mounted.desired_height(),
            );
            if let Some(png) = ux1x_out(&theme, "search_results.png") {
                mounted.set_cursor(0);
                mounted.hover = Some(last);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test viewport"
                )]
                let harness = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(mounted),
                    (w as u32, h as u32),
                    theme.clone(),
                );
                harness.render_png(&png);
                if let Ok(json) = harness.inspection_json() {
                    std::fs::write(png.with_extension("inspection.json"), json)
                        .expect("write inspection");
                }
            }
        }
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            let mut v = view_from("");
            v.set_cursor(0);
            let panel = Rect::new(margin, margin, margin + 360.0, margin + v.desired_height());
            v.set_rect(panel);
            // Hover the last row so the veneer is under test too.
            let last = v.len() - 1;
            v.hover = Some(last);
            let (w, h) = (panel.x1 + margin, panel.y1 + margin);
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            v.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small test surface"
            )]
            let rgba = OffscreenRenderer::new(w as u32, h as u32)
                .render(&scene, theme.tokens.colors.surfaces.base);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small test surface"
            )]
            let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

            // Panel edge against the panel ground just inside it, on the left
            // side below the rounded corner (at the top padding row the edge
            // column is still on the corner's arc, outside the panel), where
            // nothing but the ground paints at the left inset.
            let y = panel.y0 + f64::from(theme.tokens.radii.control) + 1.0;
            let edge = lum(panel.x0, y);
            let ground = lum(panel.x0 + 3.0, y);
            assert!(
                (edge - ground).abs() >= EDGE_MIN,
                "{}: panel edge {edge:.4} vs ground {ground:.4} did not register",
                theme.name
            );

            let row = v
                .plan()
                .rows
                .into_iter()
                .find(|r| r.flat == last)
                .expect("the hovered row is laid out")
                .rect;
            let hover = lum(row.x1 - 3.0, row.center().y);
            let beside = lum(panel.x1 - ROW_INSET / 2.0, row.center().y);
            assert!(
                (hover - beside).abs() >= EDGE_MIN,
                "{}: hover veneer {hover:.4} vs ground {beside:.4} did not register",
                theme.name
            );
        }
    }

    fn view_from(query: &str) -> SearchResultsView {
        let mut reg = SearchRegistry::new();
        reg.register_fn("Contacts", |_q| {
            vec![
                SearchResult::new("Contacts", "Alice Ng", "c:alice")
                    .with_subtitle("alice@example.com"),
                SearchResult::new("Contacts", "Alan Turing", "c:alan"),
            ]
        });
        reg.register_fn("Apps", |_q| {
            vec![SearchResult::new("Apps", "Calculator", "app:calc")]
        });
        let mut v = SearchResultsView::new();
        v.set_groups(reg.query_grouped(query));
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        v
    }

    /// More rows than the window has room for scroll inside the card: it
    /// stops at its bound, the cursor's row is brought into view, a click
    /// lands only on rows that show, and the preview band stays put.
    #[test]
    fn rows_past_the_cards_bound_scroll_under_a_fixed_preview() {
        let mut v = SearchResultsView::new();
        v.set_results(
            (0..30)
                .map(|n| {
                    SearchResult::new("Feeds", format!("Feed {n}"), format!("f:{n}"))
                        .with_preview(SearchPreview::titled(format!("Feed {n}")).fact("1 unread"))
                })
                .collect(),
        );
        let full = v.desired_height();
        v.set_max_height(Some(300.0));
        assert_eq!(v.desired_height(), 300.0, "the card stops at its bound");
        assert!(full > 300.0);
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        let band = v.preview_band();
        assert!(band > 0.0);
        let list = v.list_rect();
        assert_eq!(list.y1, 300.0 - band, "the band keeps the card's foot");
        assert_eq!(v.row_at(Point::new(100.0, list.y1 + 1.0)), None);

        // Up from the top wraps to the last row, which then shows whole.
        v.move_up();
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        let plan = v.plan();
        let last = plan.rows.last().unwrap();
        assert_eq!(last.flat, 29);
        assert!(last.rect.y1 <= v.list_rect().y1 && last.rect.y0 >= v.list_rect().y0);
        assert_eq!(v.row_at(last.rect.center()), Some(29));
        assert_eq!(v.list_rect().y1, list.y1, "the band never moves");

        // A wheel back up shows the first row again, and stops at the top.
        assert!(v.scroll_by(-10_000.0));
        assert!(!v.scroll_by(-10.0));
        assert_eq!(
            v.row_at(v.plan().rows.first().unwrap().rect.center()),
            Some(0)
        );
    }

    /// Why a confirmation asks is a sentence, and a window may be narrow:
    /// the line wraps inside the card and still fits its empty band.
    #[test]
    fn a_long_empty_line_wraps_inside_a_narrow_card() {
        let theme = Theme::dark();
        let mut view = SearchResultsView::new().with_empty_text(
            "This workspace asks before \u{201c}Save\u{201d} reaches outside it, and you decide: confirm to go ahead.",
        );
        view.set_rect(Rect::new(10.0, 0.0, 250.0, EMPTY_H));
        let (x, width, style) = view.empty_frame(&theme);
        assert!(x >= view.rect.x0 && x + f64::from(width) <= view.rect.x1);
        let mut text = TextRenderer::new();
        let (wrapped_width, wrapped_height) =
            text.measure_style(&view.empty_text, Some(width), &style);
        let (_, line) = text.measure_style("Ag", None, &style);
        assert!(wrapped_width <= width + 0.5, "{wrapped_width} > {width}");
        assert!(wrapped_height > line * 1.5, "the sentence wraps");
        assert!(f64::from(wrapped_height) <= EMPTY_H, "{wrapped_height}");
    }

    #[test]
    fn result_groups_index_preview_and_size_the_panel() {
        {
            let mut v = view_from("");
            assert_eq!(v.len(), 3, "two contacts + one app");
            v.set_cursor(99);
            assert_eq!(v.cursor(), v.len() - 1, "cursor clamps into range");
            v.set_groups(vec![SearchGroup {
                kind: "Apps".into(),
                results: vec![SearchResult::new("Apps", "Calculator", "app:calc")],
            }]);
            assert_eq!(v.cursor(), 0);
        }

        {
            let mut view = SearchResultsView::new();
            view.set_results(vec![
                SearchResult::new("Notes", "Design notes", "open")
                    .with_subtitle("Edited today")
                    .with_preview("A bounded note excerpt"),
                SearchResult::new("Notes", "Plain note", "plain"),
            ]);
            let with_preview = view.desired_height();
            view.set_cursor(1);
            assert_eq!(
                view.desired_height(),
                with_preview,
                "selection never changes result geometry"
            );
            let access = Widget::accessibility(&view).expect("result list semantics");
            assert!(
                access.children[0]
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("bounded note excerpt"))
            );
        }

        // A preview with a heading, facts and items takes the room they need,
        // the tallest one sets the band, and a reader hears all of it.
        {
            let mut view = SearchResultsView::new();
            view.set_results(vec![
                SearchResult::new("Notes", "Design notes", "open").with_preview("An excerpt"),
            ]);
            let body_only = view.desired_height();
            view.set_results(vec![
                SearchResult::new("Notes", "Design notes", "open").with_preview("An excerpt"),
                SearchResult::new("Feeds", "Rust Blog", "feed").with_preview(
                    crate::search::SearchPreview::titled("Rust Blog")
                        .fact("blog.rust-lang.org")
                        .fact("1 unread")
                        .line("Announcing Rust 1.96.1")
                        .line("A new era for the borrow checker"),
                ),
            ]);
            let structured = view.desired_height();
            assert!(
                structured > body_only + view.bands.one_line,
                "a heading, facts and items take more room than an excerpt: \
                 {structured} vs {body_only}"
            );
            view.set_cursor(1);
            assert_eq!(view.desired_height(), structured, "the band never moves");
            let access = Widget::accessibility(&view).expect("result list semantics");
            assert!(access.children[1].description.as_deref().is_some_and(|d| {
                d.contains("blog.rust-lang.org. 1 unread") && d.contains("Announcing Rust")
            }));
        }

        {
            let mut v = SearchResultsView::new().with_empty_text("Nothing here");
            v.set_groups(vec![]);
            v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
            assert!(v.is_empty());
            assert_eq!(v.selected(), None);
            let access = Widget::accessibility(&v).expect("empty result list semantics");
            assert_eq!(access.children.len(), 1);
            assert_eq!(access.children[0].role, crate::a11y::Role::StaticText);
            assert_eq!(access.children[0].name.as_deref(), Some("Nothing here"));
            assert_eq!(
                access.children[0].live,
                Some(crate::a11y::Politeness::Polite),
                "empty results remain a polite live announcement"
            );
            v.activate_selected();
            assert_eq!(v.take_activated(), None, "no row ⇒ nothing to activate");
        }

        {
            let empty = SearchResultsView::new().desired_height();
            let full = view_from("").desired_height();
            assert!(
                full > empty,
                "populated overlay is taller than the empty state"
            );
        }
    }

    #[test]
    fn results_activate_from_keyboard_and_pointer() {
        {
            let mut v = view_from("");

            let key = |k: NamedKey| {
                crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
                    Key::Named(k),
                    None,
                    ElementState::Pressed,
                ))
            };

            assert_eq!(v.cursor(), 0);
            let handled = dispatch(&mut v, &key(NamedKey::ArrowDown));
            assert!(handled, "arrow-down consumed");
            assert_eq!(v.cursor(), 1);
            // Wrap: up from the top lands on the last row.
            v.set_cursor(0);
            dispatch(&mut v, &key(NamedKey::ArrowUp));
            assert_eq!(v.cursor(), v.len() - 1, "up wraps to the bottom");

            v.set_cursor(0);
            dispatch(&mut v, &key(NamedKey::Enter));
            assert_eq!(
                v.take_activated().as_deref(),
                Some("c:alice"),
                "enter fires the selected action-id"
            );
            assert_eq!(v.take_activated(), None, "activation drains after one read");
        }

        {
            let mut v = view_from("");
            // The plan is the same geometry draw uses; target the third row (the App).
            let plan = v.plan();
            assert_eq!(plan.rows.len(), 3);
            let target = plan.rows[2].rect.center();

            let mut handled = dispatch(
                &mut v,
                &crate::synthetic_input::cursor_moved(target.x, target.y),
            );
            handled |= dispatch(&mut v, &crate::synthetic_input::mouse_press());
            assert!(handled, "row click consumed");
            assert_eq!(
                v.cursor(),
                2,
                "click moves the highlight to the clicked row"
            );
            assert_eq!(
                v.take_activated().as_deref(),
                Some("app:calc"),
                "click fires that row's action-id"
            );
        }
    }

    #[test]
    fn hits_draw_as_what_they_are_and_a_loading_row_ends_the_list() {
        let theme = Theme::dark();
        let mut view = SearchResultsView::new();
        view.bands = RowBands::resolve(&theme);
        view.set_results(vec![
            SearchResult::new("Mail", "Invoice", "m:1").with_row(
                SearchRow::message("Ada Lovelace", "Invoice")
                    .snippet("Attached is the March invoice")
                    .date("Tue")
                    .unread(true),
            ),
            SearchResult::new("Videos", "Talk", "v:1").with_row(
                SearchRow::media("Talk")
                    .byline("Rust Conf")
                    .duration("41:12"),
            ),
            SearchResult::new("Code", "parse", "f:1").with_row(
                SearchRow::file_hit("src/lib.rs", "    fn parse(input: &str)")
                    .line(12)
                    .matched(7..12),
            ),
            SearchResult::new("Apps", "Calculator", "app:calc"),
        ]);
        let heights: Vec<f64> = view
            .groups()
            .iter()
            .flat_map(|group| &group.results)
            .map(|hit| view.row_height(hit))
            .collect();
        assert!(heights[0] > heights[2], "a message has three lines");
        assert!(
            heights[1] >= thumbnail_size(&theme).1,
            "a video row holds its thumbnail"
        );
        assert!(heights[2] > heights[3], "a file hit has two lines");

        let before = view.desired_height();
        view.set_loading(true);
        assert!(
            view.desired_height() > before,
            "the loading row takes a row"
        );
        view.set_rect(Rect::new(0.0, 0.0, 480.0, view.desired_height()));
        assert!(view.loading_rect().is_some());

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::default(),
        );
        assert!(!scene.encoding().is_empty());

        let node = view.accessibility().unwrap();
        let first = &node.children[0];
        assert!(
            first
                .description
                .as_deref()
                .is_some_and(|text| text.starts_with("Unread, Ada Lovelace, Invoice")),
            "a reader hears the row in its own order: {:?}",
            first.description
        );
        assert_eq!(
            node.children.last().and_then(|n| n.name.as_deref()),
            Some("Searching…")
        );
    }
}
