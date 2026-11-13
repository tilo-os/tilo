//! `SearchResultsView` — the shared results overlay for Tilo global search.

use crate::widget::ModelPath;
use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat as _;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::search::{
    ActivationModifier, CommandRow, PREVIEW_BODY_LINES, PREVIEW_LINES, SearchGroup, SearchPreview,
    SearchResult, SearchRow,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{text_band, type_line_height};
use crate::widget::floating_list::{FloatingList, FloatingMetrics, FloatingRow, RowState};

#[path = "search_results/command_row.rs"]
mod command_row;

const PAD: f64 = crate::tokens::Spacing::BASE.s2 as f64; // card inner padding
const ROW_INSET: f64 = 8.0; // rows inset from the card edge
const HEADER_H: f64 = 26.0;
const ROW_H_ONE_LINE: f64 = 38.0;
const ROW_H_TWO_LINE: f64 = 50.0;
const ROW_TEXT_X: f64 = 12.0; // text/icon inset within a row
const EMPTY_H: f64 = 92.0;
/// A lifted row (selection, hover) is inset this much top and bottom, so a
/// selected row and its hovered neighbour keep one `s1` between them instead
/// of fusing into one block (HIG §7 "nothing touches").
const ROW_LIFT_INSET: f64 = crate::tokens::Spacing::BASE.s1 as f64 / 2.0;

/// Composes two affine transforms.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn compose(a: Affine, b: Affine) -> Affine {
    a * b
}

/// One row of the list: the air over a group, its headings, a result, or the line that says
/// more are coming.
#[derive(Clone, PartialEq)]
enum Row {
    Gap(f64),
    Header {
        label: String,
        count: Option<usize>,
        level: HeaderLevel,
        height: f64,
    },
    Result {
        result: SearchResult,
        flat: usize,
        headed: bool,
        height: f64,
        highlight: String,
        held: Option<ActivationModifier>,
    },
    Loading(f64),
}

impl Row {
    fn band(&self) -> f64 {
        match self {
            Self::Gap(height)
            | Self::Loading(height)
            | Self::Header { height, .. }
            | Self::Result { height, .. } => *height,
        }
    }

    fn flat(&self) -> Option<usize> {
        match self {
            Self::Result { flat, .. } => Some(*flat),
            _ => None,
        }
    }
}

impl FloatingRow for Row {
    type Columns = ();

    fn height(&self, _: &FloatingMetrics) -> f64 {
        self.band()
    }

    fn same(&self, other: &Self) -> bool {
        self == other
    }

    fn pickable(&self) -> bool {
        matches!(self, Self::Result { .. })
    }

    fn reveals_lead(&self) -> bool {
        matches!(self, Self::Result { .. })
    }

    /// The rows sit `ROW_INSET` in from the card's sides, `PAD` from its top
    /// and bottom: the card's own padding, not a floating panel's.
    fn rows_inset(_: &FloatingMetrics, _: Rect) -> Vec2 {
        Vec2::new(ROW_INSET, PAD)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, row: Rect, state: RowState<'_, ()>) -> Option<f64> {
        match self {
            Self::Gap(_) => {}
            Self::Header {
                label,
                count,
                level,
                ..
            } => draw_header(ctx, row, label, *count, *level),
            Self::Result {
                result,
                highlight,
                held,
                ..
            } => draw_result(
                ctx,
                row,
                result,
                state.armed,
                state.hovered,
                highlight,
                *held,
            ),
            Self::Loading(_) => {
                let style = ctx.theme.text(TextRole::Caption);
                let searching = crate::i18n::widget_format("widget-searching", &[]);
                let (_, h) = ctx.text.measure_style(&searching, None, &style);
                let y = crate::widget::text::centered_text_top(row.y0, row.height(), f64::from(h));
                ctx.text.draw_style(
                    ctx.scene,
                    &searching,
                    ctx.theme.tokens.colors.text_tertiary,
                    ctx.transform
                        .pre_translate(Vec2::new(row.x0 + ROW_TEXT_X, y)),
                    None,
                    &style,
                );
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderLevel {
    /// A group of results, with how many it holds.
    Group,
    /// A section ([`SearchGroup::section`]): what is going on, what to
    /// continue. No count: it is not a tally.
    Section,
    /// A group within a section.
    Subgroup,
}

/// Row and header bands. The constants are the 1× defaults; `build_layout` resolves them from
/// the theme's type so a 2× title still has its 1× padding instead of overrunning the next row.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowBands {
    header: f64,
    one_line: f64,
    two_line: f64,
    three_line: f64,
    media: f64,
    preview_title: f64,
    preview_line: f64,
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

/// The number a group's heading shows, when the number is worth reading
/// ([`super::section::heading_count`]): a group of one row saying "1" restates the row's own
/// presence; a group of several says how many, and a group the list cut short says how many it
/// left out.
fn heading_count(group: &SearchGroup) -> Option<usize> {
    super::section::heading_count(group.results.len(), group.total)
}

fn draw_header(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    label: &str,
    count: Option<usize>,
    level: HeaderLevel,
) {
    let theme = ctx.theme;
    let transform = ctx.transform;
    let header_style = theme.text(TextRole::Subheading);
    let count_style = theme.text(TextRole::Caption).numeric();
    let caption_style = theme.text(TextRole::Caption);
    let header_inset = f64::from(theme.tokens.spacing.s1);
    let (style, ink) = match level {
        HeaderLevel::Group => (&header_style, theme.tokens.colors.text_dim),
        HeaderLevel::Section => (&header_style, theme.tokens.colors.text_primary),
        HeaderLevel::Subgroup => (&caption_style, theme.tokens.colors.text_dim),
    };
    let (_, label_h) = ctx.text.measure_style(label, None, style);
    let hy = rect.y1 - f64::from(label_h) - header_inset;
    ctx.text.draw_style(
        ctx.scene,
        label,
        ink,
        compose(transform, Affine::translate((rect.x0 + header_inset, hy))),
        None,
        style,
    );
    let Some(count) = count else {
        return;
    };
    let count_str = count.to_string();
    let (cw, ch) = ctx.text.measure_style(&count_str, None, &count_style);
    ctx.text.draw_style(
        ctx.scene,
        &count_str,
        theme.tokens.colors.text_tertiary,
        compose(
            transform,
            Affine::translate((
                rect.x1 - f64::from(cw) - header_inset,
                hy + f64::from(label_h - ch),
            )),
        ),
        None,
        &count_style,
    );
}

fn draw_result(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    result: &SearchResult,
    selected: bool,
    hovered: bool,
    highlight: &str,
    held: Option<ActivationModifier>,
) {
    let theme = ctx.theme;
    let transform = ctx.transform;
    let colors = &theme.tokens.colors;
    let (armed_fill, selected_ink) = crate::widget::floating_surface::armed_row(theme);
    let lift = RoundedRect::from_rect(
        rect.inflate(0.0, -ROW_LIFT_INSET),
        f64::from(theme.tokens.radii.detail),
    );
    if selected {
        ctx.scene
            .fill(Fill::NonZero, transform, armed_fill, None, &lift);
    } else if hovered {
        // One step off the panel it sits on, polarity chosen the way
        // `hairline_on` chooses it — the white veneer this replaces was
        // invisible over a light panel (HIG §9.5).
        let hover = crate::tokens::colors::Colors::hairline_on(
            crate::chrome::shell::panel_ground(theme),
            crate::chrome::shell::HAIRLINE_ALPHA,
        );
        ctx.scene.fill(Fill::NonZero, transform, hover, None, &lift);
    }
    let state = command_row::RowState {
        selected,
        highlight,
        held,
    };
    match &result.row {
        Some(SearchRow::Command(command)) => {
            command_row::draw(ctx, rect, result, command, &state);
        }
        // An item is the reference its app draws: the generic row until the
        // row that app draws for it is here.
        Some(SearchRow::Item { .. }) | None => {
            command_row::draw(ctx, rect, result, &CommandRow::default(), &state);
        }
        Some(template) => {
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
            draw_template(ctx.scene, transform, ctx.text, theme, rect, template, ink);
        }
    }
}

/// The results overlay.
#[crate::component::not_a_component(system, "system search")]
pub struct SearchResultsView {
    groups: Vec<SearchGroup>,
    flat: Vec<(usize, usize)>,
    cursor: usize,
    hover: Option<usize>,
    rect: Rect,
    last_cursor: Point,
    activated: Option<String>,
    empty_text: String,
    test_id: Option<String>,
    bands: RowBands,
    loading: bool,
    /// Room kept under the empty-state line for controls its owner places
    /// inside the card (a confirmation's buttons).
    empty_footer: f64,
    max_height: Option<f64>,
    /// The rows themselves: headings, results and the loading line, laid out
    /// and scrolled by the shared list.
    list: FloatingList<Row>,
    slots: Vec<Option<usize>>,
    reveal: bool,
    group_gap: f64,
    highlight: String,
    held: Option<ActivationModifier>,
    /// Its owner holds the keyboard: the selected row is the focused one
    /// to assistive technology.
    focused: bool,
    /// What to try instead when nothing matches: the empty state then
    /// stands composed, a search glyph over its line and this hint.
    empty_hint: Option<String>,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
}

impl SearchResultsView {
    pub fn new() -> Self {
        Self {
            dirty: true,
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
            list: FloatingList::new(),
            slots: Vec::new(),
            reveal: false,
            group_gap: 0.0,
            highlight: String::new(),
            held: None,
            focused: false,
            empty_hint: None,
        }
    }

    /// Whether the owner of the list holds the keyboard, so assistive
    /// technology reads the selected row as focused.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.dirty = true;
        self.focused = focused;
    }

    pub fn set_group_gap(&mut self, gap: f64) {
        self.dirty = true;
        self.group_gap = gap.max(0.0);
        self.rebuild();
    }

    pub fn set_highlight(&mut self, query: &str) {
        self.dirty = true;
        if self.highlight != query {
            query.clone_into(&mut self.highlight);
            self.rebuild();
        }
    }

    pub fn set_held_modifier(&mut self, held: Option<ActivationModifier>) {
        self.dirty = true;
        if self.held != held {
            self.held = held;
            self.rebuild();
        }
    }

    /// The row and header bands from `theme`'s type, as layout resolves
    /// them, for an owner that lays the view out itself.
    pub(crate) fn resolve_metrics(&mut self, theme: &Theme) {
        self.bands = RowBands::resolve(theme);
        self.rebuild();
    }

    pub(crate) fn row_band(&self, two_line: bool) -> f64 {
        if two_line {
            self.bands.two_line
        } else {
            self.bands.one_line
        }
    }

    /// What the list spends around its rows: the padding at both ends, the band a row's preview
    /// keeps, `groups` headers and the gaps between them.
    fn rows_overhead(&self, groups: usize) -> f64 {
        let gaps = f64::from(u32::try_from(groups.saturating_sub(1)).unwrap_or(u32::MAX));
        PAD * 2.0
            + self.preview_band()
            + crate::num::f64_of_usize(groups) * self.bands.header
            + self.group_gap * gaps
    }

    /// The height `two_lines` rows take in the list under `groups` headers, as the plan lays
    /// them out: padding, headers and the gaps between groups included.
    pub(crate) fn height_for(&self, two_lines: &[bool], groups: usize) -> f64 {
        let rows: f64 = two_lines
            .iter()
            .map(|&two_line| self.row_band(two_line))
            .sum();
        self.rows_overhead(groups) + rows
    }

    /// How many rows of one band the list takes in `cap` under `groups`
    /// headers, never less than one: the room left once the padding,
    /// headers and gaps are paid for.
    pub(crate) fn row_capacity(&self, cap: f64, groups: usize, two_line: bool) -> usize {
        let band = self.row_band(two_line).max(1.0);
        let room = (cap - self.rows_overhead(groups)).max(band);
        crate::num::usize_of_f64((room / band).floor()).max(1)
    }

    /// Keep `height` free under the empty-state line, for controls the
    /// owner draws inside the card. The line stays centred above them.
    pub fn set_empty_footer(&mut self, height: f64) {
        self.dirty = true;
        self.empty_footer = height.max(0.0);
    }

    /// Show that more results are still coming: a loading row after the
    /// last result. With no results the empty text says so instead.
    pub fn set_loading(&mut self, loading: bool) {
        self.dirty = true;
        self.loading = loading;
        self.rebuild();
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    #[must_use]
    pub fn with_empty_text(mut self, text: impl Into<String>) -> Self {
        self.empty_text = text.into();
        self
    }

    /// Update the empty-state copy without rebuilding the shared result view.
    pub(crate) fn set_empty_hint(&mut self, hint: Option<String>) {
        self.dirty = true;
        self.empty_hint = hint;
    }

    pub fn set_empty_text(&mut self, text: impl Into<String>) {
        self.dirty = true;
        self.empty_text = text.into();
    }

    pub(crate) fn empty_text(&self) -> &str {
        &self.empty_text
    }

    pub(crate) fn empty_hint(&self) -> Option<&str> {
        self.empty_hint.as_deref()
    }

    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Rebuilds the flat selection index and clamps the cursor so it always
    /// points at a real row (or `0` when empty).
    pub fn set_groups(&mut self, groups: Vec<SearchGroup>) {
        self.dirty = true;
        self.replace_groups(groups);
        self.list.reset_scroll();
    }

    pub(crate) fn replace_groups(&mut self, groups: Vec<SearchGroup>) {
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
        self.reveal = true;
        // Hover no longer maps to a known row until the pointer moves again.
        self.set_hover(None);
        self.rebuild();
    }

    /// Buckets by kind in first-seen order, for callers not going through the registry's grouping.
    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        self.dirty = true;
        let mut groups: Vec<SearchGroup> = Vec::new();
        for r in results {
            if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
                g.results.push(r);
            } else {
                groups.push(SearchGroup::new(r.kind.clone(), vec![r]));
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
        self.dirty = true;
        if !self.flat.is_empty() {
            self.cursor = index.min(self.flat.len().saturating_sub(1));
            self.reveal = true;
            self.reveal_cursor();
        }
    }

    /// Select row `index` (clamped) where it stands, without scrolling it
    /// into view: the pointer picks the row it is over.
    pub(crate) fn set_cursor_in_place(&mut self, index: usize) {
        self.dirty = true;
        if !self.flat.is_empty() {
            self.cursor = index.min(self.flat.len().saturating_sub(1));
            self.reveal_cursor();
        }
    }

    /// The row the result `flat` is drawn on.
    fn slot(&self, flat: usize) -> Option<usize> {
        self.slots.get(flat).copied().flatten()
    }

    fn set_hover(&mut self, flat: Option<usize>) {
        self.hover = flat;
        self.list.set_hover(flat.and_then(|flat| self.slot(flat)));
    }

    fn reveal_cursor(&mut self) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let slot = self.slot(self.cursor);
        if std::mem::take(&mut self.reveal) {
            self.list.set_highlight(slot);
        } else {
            self.list.set_highlight_in_place(slot);
        }
    }

    /// Lay the rows out again from the groups: the gaps, headings, results
    /// and the line that more are coming, with the flat index to each row.
    fn rebuild(&mut self) {
        let offset = self.list.scroll_offset();
        let mut rows: Vec<Row> = Vec::new();
        let mut slots = vec![None; self.flat.len()];
        let mut flat = 0usize;
        let mut section: Option<&str> = None;
        for group in &self.groups {
            let opens = group.section.is_some() && group.section.as_deref() != section;
            section = group.section.as_deref();
            let mut heads: Vec<(String, Option<usize>, HeaderLevel)> = Vec::new();
            if let Some(name) = group.section.as_ref().filter(|_| opens) {
                heads.push((name.clone(), None, HeaderLevel::Section));
            }
            if !group.kind.is_empty() {
                heads.push(if group.section.is_some() {
                    (group.kind.clone(), None, HeaderLevel::Subgroup)
                } else {
                    (group.kind.clone(), heading_count(group), HeaderLevel::Group)
                });
            }
            let headed = !heads.is_empty();
            for (label, count, level) in heads {
                if level != HeaderLevel::Subgroup && !rows.is_empty() {
                    rows.push(Row::Gap(self.group_gap));
                }
                rows.push(Row::Header {
                    label,
                    count,
                    level,
                    height: self.bands.header,
                });
            }
            for (ri, result) in group.results.iter().enumerate() {
                slots[flat] = Some(rows.len());
                rows.push(Row::Result {
                    result: result.clone(),
                    flat,
                    headed: headed && ri == 0,
                    height: self.row_height(result),
                    highlight: self.highlight.clone(),
                    held: self.held,
                });
                flat = flat.saturating_add(1);
            }
        }
        if self.loading && !self.flat.is_empty() {
            rows.push(Row::Loading(self.bands.one_line));
        }
        self.list.set_rows(rows);
        self.list.scroll_px(offset - self.list.scroll_offset());
        self.slots = slots;
        self.set_hover(self.hover);
        self.reveal_cursor();
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
        self.reveal_cursor();
        index.is_some()
    }

    pub(crate) fn select_action(&mut self, id: &str) -> bool {
        let Some(index) = self
            .flat
            .iter()
            .position(|&(group, row)| self.groups[group].results[row].action_id == id)
        else {
            return false;
        };
        self.cursor = index;
        self.reveal = true;
        self.reveal_cursor();
        true
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

    pub fn move_down(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.reveal = true;
        if self.selected().is_none() {
            self.cursor = 0;
            self.reveal_cursor();
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(1)
            .checked_rem(self.flat.len())
            .unwrap_or(0);
        self.reveal_cursor();
    }

    pub fn move_up(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.reveal = true;
        if self.selected().is_none() {
            self.cursor = self.flat.len() - 1;
            self.reveal_cursor();
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(self.flat.len())
            .saturating_sub(1)
            .checked_rem(self.flat.len())
            .unwrap_or(0);
        self.reveal_cursor();
    }

    pub fn activate_selected(&mut self) {
        if let Some(id) = self.selected().map(|r| r.action_id.clone()) {
            self.activated = Some(id);
        }
    }

    /// `None` until a row is activated; drains after one read.
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.list.set_rect(self.list_rect());
        self.reveal_cursor();
    }

    /// The most the overlay may stand, or `None` for as tall as its rows:
    /// past it the rows scroll and the preview band stays at the bottom.
    pub fn set_max_height(&mut self, max: Option<f64>) {
        self.dirty = true;
        self.max_height = max;
    }

    pub fn desired_height(&self) -> f64 {
        if self.groups.is_empty() {
            return EMPTY_H + self.empty_footer;
        }
        let full = self.rows_height() + self.preview_band();
        let least = PAD * 2.0 + self.bands.header + self.bands.two_line + self.preview_band();
        self.max_height.map_or(full, |max| full.min(max.max(least)))
    }

    fn rows_height(&self) -> f64 {
        self.content_height() + PAD * 2.0
    }

    fn content_height(&self) -> f64 {
        self.list.rows().iter().map(Row::band).sum::<f64>()
    }

    /// The height the list takes when the most rows that fit in `cap` show, preview band
    /// included: an owner sizing the list must leave the band its room, or it would eat into
    /// the last row, and the card's own padding at the foot, or it would clip one.
    pub fn fitted_height(&self, cap: f64) -> f64 {
        let band = self.preview_band();
        let cap = (cap - band - PAD * 2.0).max(0.0);
        let mut end = 0.0;
        let mut fitted = 0.0;
        for row in self.list.rows() {
            if matches!(row, Row::Loading(_)) {
                break;
            }
            end += row.band();
            if end > cap + 0.01 {
                break;
            }
            if matches!(row, Row::Result { .. }) {
                fitted = end;
            }
        }
        let least = self
            .groups
            .iter()
            .find_map(|g| g.results.first())
            .map_or(self.bands.one_line, |r| self.row_height(r));
        f64::max(fitted, least) + PAD * 2.0 + band
    }

    fn shown_band(&self) -> (f64, f64) {
        let list = self.list_rect();
        (list.y0 + PAD, list.y1 - PAD)
    }

    /// The rows wholly in view, in order.
    pub fn shown_rows(&self) -> std::ops::Range<usize> {
        let (top, bottom) = self.shown_band();
        let shown: Vec<usize> = self
            .list
            .in_view()
            .filter_map(|slot| {
                let rect = self.list.row_bounds(slot)?;
                (rect.y0 >= top - 0.01 && rect.y1 <= bottom + 0.01)
                    .then(|| self.list.rows().get(slot)?.flat())
                    .flatten()
            })
            .collect();
        match (shown.first(), shown.last()) {
            (Some(&first), Some(&last)) => first..last.saturating_add(1),
            _ => 0..0,
        }
    }

    /// Moves through the results: arrows step and wrap, Page keys move a view's worth, Tab and
    /// Shift+Tab jump to the next and previous group, and Home and End reach the first and last
    /// result when the owner's caret has nowhere left to go in its field (`caret_at_ends`: at
    /// its start, at its end), always with Ctrl.
    pub fn handle_list_key(
        &mut self,
        key: &Key,
        shift: bool,
        ctrl: bool,
        caret_at_ends: (bool, bool),
    ) -> bool {
        let len = self.flat.len();
        let last = len.saturating_sub(1);
        let cursor = self.cursor.min(last);
        let target = match key {
            Key::Named(NamedKey::ArrowDown) => {
                if self.cursor >= last {
                    0
                } else {
                    cursor.saturating_add(1)
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor == 0 || self.cursor > last {
                    last
                } else {
                    cursor.saturating_sub(1)
                }
            }
            Key::Named(NamedKey::PageDown) => cursor.saturating_add(self.rows_per_page()).min(last),
            Key::Named(NamedKey::PageUp) => cursor.saturating_sub(self.rows_per_page()),
            Key::Named(NamedKey::Tab) => self.next_group_start(cursor, !shift),
            Key::Named(NamedKey::Home) if ctrl || (!shift && caret_at_ends.0) => 0,
            Key::Named(NamedKey::End) if ctrl || (!shift && caret_at_ends.1) => last,
            _ => return false,
        };
        self.set_cursor(target);
        true
    }

    /// The first result of the group after `cursor`'s (`forward`) or of
    /// its own group, else the one before it; wrapping at the ends. An
    /// ungrouped list has one group, so Tab stays put.
    fn next_group_start(&self, cursor: usize, forward: bool) -> usize {
        let starts = self.group_starts();
        if forward {
            starts
                .iter()
                .copied()
                .find(|&s| s > cursor)
                .or_else(|| starts.first().copied())
                .unwrap_or(cursor)
        } else {
            let own = starts.iter().copied().rfind(|&s| s <= cursor);
            match own {
                Some(s) if s < cursor => s,
                _ => starts
                    .iter()
                    .copied()
                    .rfind(|&s| s < cursor)
                    .or_else(|| starts.last().copied())
                    .unwrap_or(cursor),
            }
        }
    }

    pub fn rows_per_page(&self) -> usize {
        self.shown_rows().len().saturating_sub(1).max(1)
    }

    /// The first row under each header, in order: where Tab moves to.
    pub fn group_starts(&self) -> Vec<usize> {
        self.list
            .rows()
            .iter()
            .filter_map(|row| match row {
                Row::Result {
                    flat, headed: true, ..
                } => Some(*flat),
                _ => None,
            })
            .collect()
    }

    /// Where row `flat` is painted, `None` while it is scrolled out of view.
    pub fn row_rect(&self, flat: usize) -> Option<Rect> {
        let (top, bottom) = self.shown_band();
        self.list
            .row_bounds(self.slot(flat)?)
            .filter(|rect| rect.y1 > top && rect.y0 < bottom)
            .map(|rect| rect.intersect(self.list_rect()))
    }

    /// The label of the header painted under `p`.
    pub fn header_at(&self, p: Point) -> Option<String> {
        if !self.list_rect().contains(p) {
            return None;
        }
        self.list
            .rows()
            .iter()
            .enumerate()
            .find_map(|(slot, row)| match row {
                Row::Header { label, .. } => self
                    .list
                    .row_bounds(slot)?
                    .contains(p)
                    .then(|| label.clone()),
                _ => None,
            })
    }

    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (self.rect.y1 - self.preview_band()).max(self.rect.y0),
        )
    }

    /// Scroll the rows by `dy` logical px (positive moves down the list);
    /// returns whether they moved.
    pub(crate) fn scroll_by(&mut self, dy: f64) -> bool {
        let before = self.list.scroll_offset();
        self.list.scroll_px(dy);
        (self.list.scroll_offset() - before).abs() > f64::EPSILON
    }

    fn row_height(&self, r: &SearchResult) -> f64 {
        match &r.row {
            Some(SearchRow::Message {
                snippet: Some(_), ..
            }) => self.bands.three_line,
            Some(SearchRow::Message { .. } | SearchRow::FileHit { .. }) => self.bands.two_line,
            Some(SearchRow::Media { .. }) => self.bands.media,
            // An item is drawn as the generic row until the app's own row
            // for it is there to draw instead.
            Some(SearchRow::Command(_) | SearchRow::Item { .. }) | None if r.subtitle.is_some() => {
                self.bands.two_line
            }
            Some(SearchRow::Command(_) | SearchRow::Item { .. }) | None => self.bands.one_line,
        }
    }

    /// The result row under `p`, `None` off the rows.
    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.list
            .row_at_point(p)
            .and_then(|slot| self.list.rows().get(slot)?.flat())
    }

    /// The headers, rows and loading line, cut off at the list's edges: the
    /// list without its card, for an owner that paints the card itself.
    pub(crate) fn draw_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.list.set_theme(theme);
        self.list.set_rect(self.list_rect());
        self.list.paint_rows(scene, transform, text_renderer, theme);
    }

    /// Where the empty-state line is set: its left edge, the width it wraps at and its style.
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

    pub(crate) fn draw_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if let Some(hint) = &self.empty_hint {
            self.draw_composed_empty(scene, transform, tr, theme, hint);
            return;
        }
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

    /// A search glyph over the empty line, and the hint under it, centred
    /// so the card keeps its shape instead of collapsing to a bare field.
    fn draw_composed_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hint: &str,
    ) {
        let colors = &theme.tokens.colors;
        let (top, bottom) = (self.rect.y0 + PAD, self.rect.y1 - PAD);
        if bottom <= top {
            return;
        }
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = f64::midpoint(top, bottom);
        let icon_size = 24.0;
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                cx - icon_size / 2.0,
                cy - icon_size - 10.0,
                cx + icon_size / 2.0,
                cy - 10.0,
            ),
            crate::icons::Icon::Search,
            colors.text_dim.with_alpha_factor(0.55),
            1.5,
        );
        let style = theme.text(TextRole::Caption);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "card widths are logical pixels well within f32 range"
        )]
        let room = (self.rect.width() - 48.0).max(1.0) as f32;
        let title = tr.elide_to_width_style(&self.empty_text, room, &style);
        let (tw, th) = tr.measure_style(&title, None, &style);
        tr.draw_style(
            scene,
            &title,
            colors.text_primary,
            compose(transform, Affine::translate((cx - f64::from(tw) / 2.0, cy))),
            None,
            &style,
        );
        let (hw, _) = tr.measure_style(hint, None, &style);
        tr.draw_style(
            scene,
            hint,
            colors.text_dim,
            compose(
                transform,
                Affine::translate((cx - f64::from(hw) / 2.0, cy + f64::from(th) + 6.0)),
            ),
            None,
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

fn unread_dot(theme: &Theme) -> f64 {
    f64::from(theme.tokens.radii.detail) * 1.5
}

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
        // Drawn by `command_row`, with the row's state; an item's row is
        // the app's own, drawn from the reference, and the generic row
        // carries the line above until it is.
        SearchRow::Command(_) | SearchRow::Item { .. } => {}
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
        self.resolve_metrics(theme);
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

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.set_rect(crate::widget::layout_rect(taffy, node, origin));
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

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
        self.draw_list(scene, transform, text_renderer, theme);
        self.draw_preview(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                let hover = self.row_at(self.last_cursor);
                self.set_hover(hover);
            }
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
                    let hover = self.row_at(self.last_cursor);
                    self.set_hover(hover);
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

    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
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
        if !self.select_action(target) {
            return false;
        }
        if matches!(action, crate::a11y::Action::Activate) {
            self.activate_selected();
        }
        true
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let total = self.flat.len();
        let mut children = if self.flat.is_empty() {
            let mut empty = AccessNode::new(Role::StaticText)
                .with_name(&self.empty_text)
                .with_bounds(self.rect)
                .with_live(crate::a11y::Politeness::Polite);
            if let Some(hint) = &self.empty_hint {
                empty = empty.with_description(hint.clone());
            }
            vec![empty]
        } else {
            let mut rows: Vec<(Option<&str>, AccessNode)> = Vec::new();
            for (slot, row) in self.list.rows().iter().enumerate() {
                let Some(flat) = row.flat() else {
                    continue;
                };
                let Some(&(gi, ri)) = self.flat.get(flat) else {
                    continue;
                };
                let Some(group) = self.groups.get(gi) else {
                    continue;
                };
                let Some(result) = group.results.get(ri) else {
                    continue;
                };
                let description = result
                    .subtitle
                    .iter()
                    .cloned()
                    .chain(result.row.as_ref().map(SearchRow::spoken))
                    .chain(result.preview.as_ref().map(SearchPreview::spoken))
                    .filter(|part| !part.is_empty())
                    .fold(Vec::<String>::new(), |mut parts, part| {
                        if !parts.contains(&part) {
                            parts.push(part);
                        }
                        parts
                    })
                    .join(". ");
                let selected = flat == self.cursor;
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(result.title.clone())
                    .with_action_target(&result.action_id)
                    .with_bounds(self.list.row_bounds(slot).unwrap_or(Rect::ZERO))
                    .with_position(flat.saturating_add(1), total)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused && selected)
                            .selected(selected),
                    );
                if !description.is_empty() {
                    node = node.with_description(description);
                }
                rows.push((group.section.as_deref(), node));
            }
            if rows.iter().all(|(section, _)| section.is_none()) {
                rows.into_iter().map(|(_, node)| node).collect()
            } else {
                let mut sections: Vec<(Option<&str>, Vec<AccessNode>)> = Vec::new();
                for (section, node) in rows {
                    match sections.last_mut() {
                        Some((last, nodes)) if *last == section => nodes.push(node),
                        _ => sections.push((section, vec![node])),
                    }
                }
                sections
                    .into_iter()
                    .flat_map(|(section, nodes)| match section {
                        Some(name) => vec![
                            AccessNode::new(Role::Group)
                                .with_name(name)
                                .with_children(nodes),
                        ],
                        None => nodes,
                    })
                    .collect()
            }
        };
        if self.loading && !self.flat.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(crate::i18n::widget_format("widget-searching", &[]))
                    .with_live(crate::a11y::Politeness::Polite),
            );
        }
        Some(
            AccessNode::new(Role::List)
                .with_name(crate::i18n::widget_format("widget-search-results", &[]))
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

    /// Where result `flat` is laid out, whether or not it shows: the same
    /// geometry the list paints from.
    fn laid(view: &SearchResultsView, flat: usize) -> Rect {
        let slot = view.slot(flat).expect("the result has a row");
        view.list.row_bounds(slot).expect("the list is placed")
    }

    /// The results panel keeps a **visible derived edge** at 1× on both presets (HIG §9.1 Panel
    /// role, §9.5): before, it was a raised card under `border_standard`, three shades from the
    /// dark preset's raised surface.
    #[test]
    fn ux1x_panel_edge_and_hover_register_on_both_presets() {
        use crate::offscreen::OffscreenRenderer;
        const EDGE_MIN: f64 = 0.008;
        let margin = 16.0;
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
                mounted.set_hover(Some(last));
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
            let last = v.len() - 1;
            v.set_hover(Some(last));
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

            let y = panel.y0 + f64::from(theme.tokens.radii.control) + 1.0;
            let edge = lum(panel.x0, y);
            let ground = lum(panel.x0 + 3.0, y);
            assert!(
                (edge - ground).abs() >= EDGE_MIN,
                "{}: panel edge {edge:.4} vs ground {ground:.4} did not register",
                theme.name
            );

            let row = v.row_rect(last).expect("the hovered row is laid out");
            let hover = lum(row.x1 - 3.0, row.center().y);
            let beside = lum(panel.x1 - ROW_INSET / 2.0, row.center().y);
            assert!(
                (hover - beside).abs() >= EDGE_MIN,
                "{}: hover veneer {hover:.4} vs ground {beside:.4} did not register",
                theme.name
            );
        }
    }

    /// The counts the headings draw, in order.
    fn heading_counts(view: &SearchResultsView) -> Vec<Option<usize>> {
        view.list
            .rows()
            .iter()
            .filter_map(|row| match row {
                Row::Header { count, .. } => Some(*count),
                _ => None,
            })
            .collect()
    }

    /// A count on a heading is news: a group of one row saying "1" restates the row standing
    /// under it, so the heading names the group alone; a group of several says how many, and a
    /// group the card cut short says how many it has in all.
    #[test]
    fn a_group_heading_counts_only_where_the_number_says_something() {
        let mut view = SearchResultsView::new();
        view.set_results(vec![SearchResult::new("Web", "Tilo", "w:1")]);
        assert_eq!(heading_counts(&view), vec![None], "one row counts nothing");

        view.set_results(vec![
            SearchResult::new("Mail", "Invoice", "m:1"),
            SearchResult::new("Mail", "Receipt", "m:2"),
        ]);
        assert_eq!(heading_counts(&view), vec![Some(2)], "two rows say so");

        view.set_groups(vec![SearchGroup {
            total: Some(46),
            ..SearchGroup::new("Files", vec![SearchResult::new("Files", "plan.pdf", "f:1")])
        }]);
        assert_eq!(
            heading_counts(&view),
            vec![Some(46)],
            "a group the card cut short says how many it has"
        );

        view.set_groups(vec![]);
        assert!(heading_counts(&view).is_empty());
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

    /// A fitted list counts the preview band as part of itself: an owner
    /// sizing the list to the rows alone would put the band over the last
    /// row, which then never shows.
    #[test]
    fn a_fitted_list_keeps_the_preview_band_its_room() {
        let mut v = SearchResultsView::new();
        v.set_results(
            (0..10)
                .map(|n| {
                    SearchResult::new("Feeds", format!("Feed {n}"), format!("f:{n}"))
                        .with_preview(SearchPreview::titled("Feed").fact("1 unread"))
                })
                .collect(),
        );
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        let band = v.preview_band();
        assert!(band > 0.0);
        let third = laid(&v, 2).y1 - v.rect.y0;
        let cap = third + PAD + band;
        let fitted = v.fitted_height(cap);
        assert!(
            (fitted - cap).abs() < 0.01,
            "the fitted list keeps its padding and the band's room: {fitted} for {cap}"
        );
        v.set_rect(Rect::new(0.0, 0.0, 360.0, fitted));
        assert!(
            laid(&v, 2).y1 <= v.rect.y1 - band - PAD + 0.01,
            "the last fitted row is not clipped at the card's foot"
        );
        assert!(v.list_rect().y1 <= v.rect.y1, "the band stays in the card");
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

        v.move_up();
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        assert_eq!(v.list.rows().last().and_then(Row::flat), Some(29));
        let last = laid(&v, 29);
        assert!(last.y1 <= v.list_rect().y1 && last.y0 >= v.list_rect().y0);
        assert_eq!(v.row_at(last.center()), Some(29));
        assert_eq!(v.list_rect().y1, list.y1, "the band never moves");

        assert!(v.scroll_by(-10_000.0));
        assert!(!v.scroll_by(-10.0));
        assert_eq!(v.row_at(laid(&v, 0).center()), Some(0));
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
            v.set_groups(vec![SearchGroup::new(
                "Apps",
                vec![SearchResult::new("Apps", "Calculator", "app:calc")],
            )]);
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
            assert_eq!(v.len(), 3);
            let target = laid(&v, 2).center();

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
        assert!(
            view.list
                .rows()
                .iter()
                .any(|row| matches!(row, Row::Loading(_))),
            "the loading line ends the list"
        );

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
