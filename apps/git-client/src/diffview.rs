//! The diff of the selected file: unified or side by side, syntax
//! highlighted, with line numbers, per-hunk actions and a file header.
//!
//! The diff is a document the app draws: its rows read every size from the
//! theme's `Code` line box and every colour from the theme (added and removed
//! lines stand on the toolkit's success and danger status washes). With the
//! wrap preference on (the default), a line longer than its pane wraps onto
//! further rows of the same line, so no code is cut at a pane edge; with it
//! off, long lines scroll sideways. The `diff --git` / `index` / `---` /
//! `+++` metadata is not shown as rows: the file header above the rows
//! names the file, its rename and its line counts, and carries the file's
//! actions. Each hunk opens with its `@@` line and the hunk's own actions as
//! Small buttons.

use std::ops::Range;

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::tokens::colors::{NON_TEXT_MIN_CONTRAST, SMALL_TEXT_MIN_CONTRAST};
use ui::widget::code_editor::{Span, SyntaxHighlighter};
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent, Tone,
};

use crate::i18n::{t, t_args};
use crate::model::{DiffLine, DiffLineKind, FileDiff};
use crate::settings::{DiffLayout, Settings};

/// What a hunk button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkAction {
    /// Stage this hunk (worktree side) or unstage it (staged side).
    Toggle,
    /// Discard this hunk's worktree change.
    Discard,
}

/// What a press in the diff asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffHit {
    /// Stage or unstage the whole file.
    StageFile,
    /// Discard the file's worktree changes (after confirmation).
    DiscardFile,
    /// One hunk's action.
    Hunk(usize, HunkAction),
}

/// What the diff shows this frame.
pub struct DiffInput<'a> {
    pub diff: &'a FileDiff,
    pub scroll: f64,
    /// How far the code is scrolled sideways (only without wrapping).
    pub hscroll: f64,
    pub settings: Settings,
    /// The staged side (HEAD → index): actions unstage.
    pub staged: bool,
    /// Whether the worktree side may be discarded here.
    pub can_discard: bool,
    pub pointer: Option<Point>,
}

/// What one paint produced.
pub struct DiffDraw {
    pub targets: Vec<(Rect, DiffHit)>,
    /// The scrolling rows' viewport, under the file header.
    pub viewport: Rect,
    /// Height of all rows, for clamping the vertical scroll.
    pub content_height: f64,
    /// How far the code may scroll sideways before its longest line ends
    /// (zero while lines wrap).
    pub max_hscroll: f64,
}

/// Row geometry measured in the theme's `Code` role.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// One code row: the `Code` line box, in whole pixels.
    pub row_h: f64,
    /// One hunk header row: a Small control with the file header's `s2`
    /// margin above and below it.
    pub hunk_h: f64,
    /// Width of one monospace cell.
    pub cell_w: f64,
    /// Width of the widest line number.
    pub number_w: f64,
    /// Inset of the gutter and of code from the pane edges.
    pub pad: f64,
    /// Gap between controls, and between the marker and the code.
    pub gap: f64,
    /// Gap between the two number columns, and before the marker.
    pub column_gap: f64,
    /// The file header band.
    pub header_h: f64,
}

impl Metrics {
    /// Measure for `diff` in `theme`.
    pub fn measure(text: &mut TextRenderer, theme: &Theme, diff: &FileDiff) -> Self {
        let code = theme.text(TextRole::Code);
        let numbers = number_style(theme);
        let line_h = f64::from(text.measure_style("Ag", None, &code).1);
        let cell_w = f64::from(text.measure_style("0", None, &code).0);
        let last_old = diff.lines.iter().rev().find_map(|l| l.old_lineno);
        let last_new = diff.lines.iter().rev().find_map(|l| l.new_lineno);
        let digits = last_old
            .max(last_new)
            .map_or(2, |n| n.to_string().len().max(2));
        let number_w = f64::from(text.measure_style(&"0".repeat(digits), None, &numbers).0);
        let s = &theme.tokens.spacing;
        let small = ButtonFace::height(ControlSize::Small, theme);
        Self {
            row_h: line_h.ceil(),
            hunk_h: (small + 2.0 * f64::from(s.s2)).ceil(),
            cell_w,
            number_w,
            pad: f64::from(s.s3),
            gap: f64::from(s.s2),
            column_gap: f64::from(s.s4),
            header_h: (small + 2.0 * f64::from(s.s2)).ceil(),
        }
    }

    /// The unified gutter: both number columns and the marker.
    fn unified_gutter(&self) -> f64 {
        self.pad + self.number_w + self.column_gap + self.number_w + self.column_gap + self.marker()
    }

    /// One half of the side-by-side layout: its number column and marker.
    fn half_gutter(&self) -> f64 {
        self.pad + self.number_w + self.column_gap + self.marker()
    }

    fn marker(&self) -> f64 {
        self.cell_w + self.gap
    }
}

/// Line numbers: `Code` with tabular figures.
fn number_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Code).numeric()
}

/// One row of the laid-out diff.
#[derive(Clone, Copy, Debug)]
enum Row {
    /// A hunk's `@@` line, at the line index.
    Hunk(usize),
    /// A unified code line, at the line index.
    Line(usize),
    /// A side-by-side pair: the old and the new line indices.
    Pair(Option<usize>, Option<usize>),
}

/// How many cells a wrapped line's continuation rows are indented: the
/// line's own indentation, so wrapped code stays inside its block (at most
/// half the pane).
fn hanging_indent(content: &str, cols: usize) -> usize {
    content
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count()
        .min(cols / 2)
}

/// Where each line of code breaks to fit `cols` monospace cells: the byte
/// range of every visual line, at least one. Continuation rows are
/// [`hanging_indent`] cells narrower. A break prefers the last space, or the
/// last place a word meets punctuation, in the second half of a visual line,
/// so words stay whole where they can.
fn wrap(content: &str, cols: usize) -> Vec<Range<usize>> {
    let first_cols = cols.max(1);
    let indent = hanging_indent(content, first_cols);
    let mut cols = first_cols;
    let mut segments = Vec::new();
    let mut start = 0;
    // Characters on the visual line so far, and the last break after a
    // space: its byte offset and how many characters precede it.
    let mut count = 0;
    let mut soft: Option<(usize, usize)> = None;
    let mut previous: Option<char> = None;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    for (i, c) in content.char_indices() {
        // A word ends where punctuation follows it.
        if previous.is_some_and(is_word) && !is_word(c) && !c.is_whitespace() {
            soft = Some((i, count));
        }
        previous = Some(c);
        if count == cols {
            let (end, used) = match soft {
                Some((end, used)) if used * 2 > cols => (end, used),
                _ => (i, count),
            };
            segments.push(start..end);
            start = end;
            count -= used;
            soft = None;
            cols = (first_cols - indent).max(1);
        }
        count += 1;
        if c.is_whitespace() {
            soft = Some((i + c.len_utf8(), count));
        }
    }
    segments.push(start..content.len());
    segments
}

/// Every line's visual lines in its pane, by line index (empty for lines
/// that are not code).
fn wrap_lines(diff: &FileDiff, cols: usize) -> Vec<Vec<Range<usize>>> {
    diff.lines
        .iter()
        .map(|line| match line.kind {
            DiffLineKind::Meta | DiffLineKind::Hunk => Vec::new(),
            _ => wrap(&line.content, cols),
        })
        .collect()
}

/// How many monospace cells of code fit beside the gutter in `width`.
fn code_columns(width: f64, layout: DiffLayout, m: &Metrics) -> usize {
    let room = match layout {
        DiffLayout::Unified => width - m.unified_gutter() - m.pad,
        DiffLayout::SideBySide => width / 2.0 - m.half_gutter() - m.pad,
    };
    (room / m.cell_w.max(1.0)).floor().max(1.0) as usize
}

/// Where each row sits: its top relative to the first row, and its height.
/// A code row is as tall as its longest wrapped line needs.
fn lay_out(
    diff: &FileDiff,
    layout: DiffLayout,
    m: &Metrics,
    wrapped: &[Vec<Range<usize>>],
) -> (Vec<(Row, f64, f64)>, f64) {
    let rows: Vec<Row> = match layout {
        DiffLayout::Unified => diff
            .lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| match line.kind {
                DiffLineKind::Meta => None,
                DiffLineKind::Hunk => Some(Row::Hunk(i)),
                _ => Some(Row::Line(i)),
            })
            .collect(),
        DiffLayout::SideBySide => pair_rows(&diff.lines),
    };
    let visual = |i: Option<usize>| i.and_then(|i| wrapped.get(i)).map_or(1, |w| w.len().max(1));
    let mut y = 0.0;
    let placed = rows
        .into_iter()
        .map(|row| {
            let lines = match row {
                Row::Hunk(_) => 0,
                Row::Line(i) => visual(Some(i)),
                Row::Pair(l, r) => visual(l).max(visual(r)),
            };
            let h = if lines == 0 {
                m.hunk_h
            } else {
                m.row_h * ui::num::f64_of_usize(lines)
            };
            let top = y;
            y += h;
            (row, top, h)
        })
        .collect();
    (placed, y)
}

/// Pair deletions with the additions that follow them for the side-by-side
/// layout. Context lines span both sides; hunk lines span the whole row;
/// metadata lines are not rows.
fn pair_rows(lines: &[DiffLine]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut i = 0;
    while let Some(cur) = lines.get(i) {
        match cur.kind {
            DiffLineKind::Meta => i += 1,
            DiffLineKind::Hunk => {
                rows.push(Row::Hunk(i));
                i += 1;
            }
            DiffLineKind::Context => {
                rows.push(Row::Pair(Some(i), Some(i)));
                i += 1;
            }
            DiffLineKind::Deletion => {
                let del_start = i;
                while lines
                    .get(i)
                    .is_some_and(|l| l.kind == DiffLineKind::Deletion)
                {
                    i += 1;
                }
                let add_start = i;
                while lines
                    .get(i)
                    .is_some_and(|l| l.kind == DiffLineKind::Addition)
                {
                    i += 1;
                }
                let dels = add_start - del_start;
                let adds = i - add_start;
                for k in 0..dels.max(adds) {
                    rows.push(Row::Pair(
                        (k < dels).then_some(del_start + k),
                        (k < adds).then_some(add_start + k),
                    ));
                }
            }
            DiffLineKind::Addition => {
                rows.push(Row::Pair(None, Some(i)));
                i += 1;
            }
        }
    }
    rows
}

/// Paint `input.diff` into `pane`.
pub fn paint(ctx: &mut PaintCtx<'_>, pane: Rect, input: &DiffInput<'_>) -> DiffDraw {
    let m = Metrics::measure(ctx.text, ctx.theme, input.diff);
    let mut targets = Vec::new();
    let header = Rect::new(
        pane.x0,
        pane.y0,
        pane.x1,
        (pane.y0 + m.header_h).min(pane.y1),
    );
    paint_file_header(ctx, header, input, &m, &mut targets);
    ui::surface::separator(
        ctx.scene,
        ctx.transform,
        header.x0,
        header.x1,
        header.y1,
        0.0,
        ctx.theme.tokens.colors.surfaces.content,
    );
    let viewport = Rect::new(pane.x0, header.y1, pane.x1, pane.y1.max(header.y1));

    if paint_empty(ctx, viewport, input.diff) {
        return DiffDraw {
            targets,
            viewport,
            content_height: 0.0,
            max_hscroll: 0.0,
        };
    }

    let layout = input.settings.diff_layout;
    let cols = code_columns(viewport.width(), layout, &m);
    let wrapping = input.settings.wrap_lines;
    let wrapped = wrap_lines(input.diff, if wrapping { cols } else { usize::MAX });
    let (rows, content_height) = lay_out(input.diff, layout, &m, &wrapped);
    let widest = input
        .diff
        .lines
        .iter()
        .filter(|l| !matches!(l.kind, DiffLineKind::Meta | DiffLineKind::Hunk))
        .map(|l| l.content.chars().count())
        .max()
        .unwrap_or(0);
    let max_hscroll = if wrapping {
        0.0
    } else {
        (ui::num::f64_of_usize(widest.saturating_sub(cols)) * m.cell_w).max(0.0)
    };
    let pass = RowPass {
        input,
        m,
        viewport,
        top: viewport.y0 - input.scroll,
        wrapped: &wrapped,
        cols,
        hscroll: input.hscroll.clamp(0.0, max_hscroll),
    };
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        ctx.transform,
        &viewport,
    );
    let mut first_hunk = true;
    for (row, top, h) in &rows {
        let rect = Rect::new(viewport.x0, pass.top + top, viewport.x1, pass.top + top + h);
        if rect.y1 < viewport.y0 || rect.y0 > viewport.y1 {
            if matches!(row, Row::Hunk(_)) {
                first_hunk = false;
            }
            continue;
        }
        match *row {
            Row::Hunk(i) => {
                pass.hunk_row(ctx, rect, i, !first_hunk, &mut targets);
                first_hunk = false;
            }
            Row::Line(i) => pass.unified_row(ctx, rect, i),
            Row::Pair(left, right) => pass.split_row(ctx, rect, left, right),
        }
    }
    ctx.scene.pop_layer();

    DiffDraw {
        targets,
        viewport,
        content_height,
        max_hscroll,
    }
}

/// A diff with no text rows (a binary file, a mode change) says so in one
/// sentence instead; returns whether it did.
fn paint_empty(ctx: &mut PaintCtx<'_>, viewport: Rect, diff: &FileDiff) -> bool {
    let empty = if diff.binary {
        Some(("git-binary-file", "git-binary-file-detail"))
    } else if diff.lines.iter().all(|l| l.kind == DiffLineKind::Meta) {
        Some(("git-mode-only", "git-mode-only-detail"))
    } else {
        None
    };
    let Some((title, message)) = empty else {
        return false;
    };
    let (title, message) = (t(title), t(message));
    let content = EmptyStateContent {
        message: Some(&message),
        ..EmptyStateContent::new(&title)
    };
    EmptyState::paint(ctx, viewport, &content, Point::new(f64::MIN, f64::MIN));
    true
}

/// The file header: the path (and the name it was renamed from), its line
/// counts, and the file's actions.
fn paint_file_header(
    ctx: &mut PaintCtx<'_>,
    band: Rect,
    input: &DiffInput<'_>,
    m: &Metrics,
    targets: &mut Vec<(Rect, DiffHit)>,
) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let gap = f64::from(theme.tokens.spacing.s2);
    let small_h = ButtonFace::height(ControlSize::Small, theme);
    let button_y = band.center().y - small_h / 2.0;

    // Actions trail: the file's toggle last, discard before it.
    let mut right = band.x1 - m.pad;
    let mut buttons: Vec<(String, ButtonRole, DiffHit)> = vec![(
        t(if input.staged {
            "git-unstage-file"
        } else {
            "git-stage-file"
        }),
        ButtonRole::Secondary,
        DiffHit::StageFile,
    )];
    if input.can_discard && !input.staged {
        buttons.push((
            t("git-discard-file"),
            ButtonRole::Destructive,
            DiffHit::DiscardFile,
        ));
    }
    for (label, role, hit) in buttons {
        let probe = ButtonFace::new(Rect::ZERO, &label)
            .role(role)
            .control_size(ControlSize::Small);
        let w = probe.intrinsic_width(ctx.text, theme);
        let rect = Rect::new(right - w, button_y, right, button_y + small_h);
        if rect.x0 < band.x0 + m.pad {
            break;
        }
        let face = ButtonFace::new(rect, &label)
            .role(role)
            .control_size(ControlSize::Small);
        let hovered = input.pointer.is_some_and(|p| face.contains(theme, p));
        let face = face.hovered(hovered);
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        targets.push((face.hit_rect(theme), hit));
        right = rect.x0 - gap;
    }

    // Path, then the counts.
    let diff = input.diff;
    let x = band.x0 + m.pad;
    let label = theme.text(TextRole::Label).strong();
    let caption = theme.text(TextRole::Caption).numeric();
    let added = diff.additions();
    let removed = diff.deletions();
    let counts: Vec<(String, Color)> = [
        (added, format!("+{added}"), colors.semantic.success),
        (
            removed,
            format!("\u{2212}{removed}"),
            colors.semantic.danger,
        ),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(_, s, tone)| {
        (
            s,
            colors.ink_on(colors.surfaces.content, tone, SMALL_TEXT_MIN_CONTRAST),
        )
    })
    .collect();
    let counts_w: f64 = counts
        .iter()
        .map(|(s, _)| f64::from(ctx.text.measure_style(s, None, &caption).0) + gap)
        .sum();
    let room = (right - gap - x - counts_w).max(0.0);
    let path = ctx
        .text
        .elide_to_width_style(&diff.path, ui::num::f32_of_f64(room), &label);
    let (path_w, path_h) = ctx.text.measure_style(&path, None, &label);
    let top = ui::text_metrics::centered_text_top(band.y0, band.height(), f64::from(path_h));
    ctx.text(&path, &label, colors.text_primary, Point::new(x, top), None);
    let mut cx = x + f64::from(path_w) + gap;
    for (s, ink) in &counts {
        let w = f64::from(ctx.text.measure_style(s, None, &caption).0);
        let line = f64::from(ctx.text.measure_style(s, None, &caption).1);
        let y = ui::text_metrics::centered_text_top(band.y0, band.height(), line);
        ctx.text(s, &caption, *ink, Point::new(cx, y), None);
        cx += w + gap;
    }
    if let Some(old) = &diff.old_path {
        let note = t_args("git-renamed-from", &[("path", old.as_str().into())]);
        let room = (right - gap - cx).max(0.0);
        if room > m.cell_w * 8.0 {
            let style = theme.text(TextRole::Caption);
            let shown = ctx
                .text
                .elide_to_width_style(&note, ui::num::f32_of_f64(room), &style);
            ctx.text_role_in_band(&shown, TextRole::Caption, theme.text_dim, cx, band, None);
        }
    }
}

/// One paint pass over the visible rows.
struct RowPass<'a, 'b> {
    input: &'a DiffInput<'b>,
    m: Metrics,
    viewport: Rect,
    /// Where the first row's top is, after scrolling.
    top: f64,
    /// Each line's visual lines, by line index.
    wrapped: &'a [Vec<Range<usize>>],
    /// How many cells of code a row holds.
    cols: usize,
    /// How far unwrapped code is scrolled sideways.
    hscroll: f64,
}

impl RowPass<'_, '_> {
    /// A hunk's `@@` line and its actions. `rule` draws the separator that
    /// parts it from the hunk above.
    fn hunk_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        index: usize,
        rule: bool,
        targets: &mut Vec<(Rect, DiffHit)>,
    ) {
        let theme = ctx.theme;
        let m = &self.m;
        let Some(line) = self.input.diff.lines.get(index) else {
            return;
        };
        if rule {
            ui::surface::separator(
                ctx.scene,
                ctx.transform,
                row.x0,
                row.x1,
                row.y0,
                0.0,
                theme.tokens.colors.surfaces.content,
            );
        }
        let gap = m.gap;
        let small_h = ButtonFace::height(ControlSize::Small, theme);
        let y = row.center().y - small_h / 2.0;
        let mut right = row.x1 - m.pad;
        let mut actions = vec![(
            t(if self.input.staged {
                "git-unstage-hunk"
            } else {
                "git-stage-hunk"
            }),
            ButtonRole::Secondary,
            HunkAction::Toggle,
        )];
        if self.input.can_discard && !self.input.staged {
            actions.push((
                t("git-discard-hunk"),
                ButtonRole::Destructive,
                HunkAction::Discard,
            ));
        }
        for (label, role, action) in actions {
            let w = ButtonFace::new(Rect::ZERO, &label)
                .role(role)
                .control_size(ControlSize::Small)
                .intrinsic_width(ctx.text, theme);
            let rect = Rect::new(right - w, y, right, y + small_h);
            if rect.x0 < row.x0 + m.pad {
                break;
            }
            let face = ButtonFace::new(rect, &label)
                .role(role)
                .control_size(ControlSize::Small);
            let hovered = self.input.pointer.is_some_and(|p| face.contains(theme, p));
            let face = face.hovered(hovered);
            Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
            if rect.y0 >= self.viewport.y0 && rect.y1 <= self.viewport.y1 {
                targets.push((face.hit_rect(theme), DiffHit::Hunk(line.hunk, action)));
            }
            right = rect.x0 - gap;
        }
        let code = theme.text(TextRole::Code);
        let x = row.x0 + m.pad;
        let header = ctx.text.elide_to_width_style(
            &line.content,
            ui::num::f32_of_f64((right - gap - x).max(0.0)),
            &code,
        );
        ctx.text_role_in_band(&header, TextRole::Code, theme.text_tertiary, x, row, None);
    }

    /// One unified code row.
    fn unified_row(&self, ctx: &mut PaintCtx<'_>, row: Rect, index: usize) {
        let Some(line) = self.input.diff.lines.get(index) else {
            return;
        };
        let m = &self.m;
        let ground = Self::paint_wash(ctx, row, line.kind);
        // Numbers and the marker sit on the line's first visual line.
        let first = Rect::new(row.x0, row.y0, row.x1, row.y0 + m.row_h);
        let numbers = number_style(ctx.theme);
        let old_right = row.x0 + m.pad + m.number_w;
        let new_right = old_right + m.column_gap + m.number_w;
        Self::number(ctx, line.old_lineno, old_right, first, &numbers);
        Self::number(ctx, line.new_lineno, new_right, first, &numbers);
        let marker_x = new_right + m.column_gap;
        Self::marker(ctx, line.kind, marker_x, first, ground);
        let code = Rect::new(marker_x + m.marker(), row.y0, row.x1, row.y1);
        self.code(ctx, index, line, code);
    }

    /// One side-by-side row: the old line on the left, the new on the right.
    fn split_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        left: Option<usize>,
        right: Option<usize>,
    ) {
        let mid = row.center().x;
        let halves = [
            (Rect::new(row.x0, row.y0, mid, row.y1), left, true),
            (Rect::new(mid, row.y0, row.x1, row.y1), right, false),
        ];
        let numbers = number_style(ctx.theme);
        let m = &self.m;
        // The two sides meet along one hairline; hunk rows run across it.
        let edge = ui::surface::edge_on(ctx.theme.tokens.colors.surfaces.content);
        let x = mid.round();
        ctx.fill_rect(Rect::new(x - 0.5, row.y0, x + 0.5, row.y1), edge);
        for (half, index, old) in halves {
            let Some((index, line)) =
                index.and_then(|i| self.input.diff.lines.get(i).map(|line| (i, line)))
            else {
                continue;
            };
            let ground = Self::paint_wash(ctx, half, line.kind);
            let first = Rect::new(half.x0, half.y0, half.x1, half.y0 + m.row_h);
            let number_right = half.x0 + m.pad + m.number_w;
            let number = if old {
                line.old_lineno
            } else {
                line.new_lineno
            };
            Self::number(ctx, number, number_right, first, &numbers);
            let marker_x = number_right + m.column_gap;
            Self::marker(ctx, line.kind, marker_x, first, ground);
            let code = Rect::new(marker_x + m.marker(), half.y0, half.x1, half.y1);
            self.code(ctx, index, line, code);
        }
    }

    /// Wash an added or removed row in the toolkit's status wash; returns
    /// the ground the row's text stands on.
    fn paint_wash(ctx: &mut PaintCtx<'_>, row: Rect, kind: DiffLineKind) -> Color {
        let theme = ctx.theme;
        let ground = theme.tokens.colors.surfaces.content;
        let tone = match kind {
            DiffLineKind::Addition => Tone::Success,
            DiffLineKind::Deletion => Tone::Danger,
            _ => return ground,
        };
        let washed = theme
            .tokens
            .colors
            .wash_on(ground, tone, ui::tokens::WashStrength::Rest)
            .fill;
        ctx.fill_rect(row, washed);
        washed
    }

    /// A right-aligned line number ending at `right`.
    fn number(
        ctx: &mut PaintCtx<'_>,
        number: Option<u32>,
        right: f64,
        row: Rect,
        style: &TextStyle,
    ) {
        let Some(number) = number else {
            return;
        };
        let s = number.to_string();
        let (w, h) = ctx.text.measure_style(&s, None, style);
        let y = ui::text_metrics::centered_text_top(row.y0, row.height(), f64::from(h));
        ctx.text(
            &s,
            style,
            ctx.theme.text_tertiary,
            Point::new(right - f64::from(w), y),
            None,
        );
    }

    /// The `+` / `−` marker of an added or removed line.
    fn marker(ctx: &mut PaintCtx<'_>, kind: DiffLineKind, x: f64, row: Rect, ground: Color) {
        let colors = &ctx.theme.tokens.colors;
        let (glyph, tone) = match kind {
            DiffLineKind::Addition => ("+", colors.semantic.success),
            DiffLineKind::Deletion => ("\u{2212}", colors.semantic.danger),
            _ => return,
        };
        let ink = colors.ink_on(ground, tone, NON_TEXT_MIN_CONTRAST);
        ctx.text_role_in_band(glyph, TextRole::Code, ink, x, row, None);
    }

    /// The line's code in `area`, one visual line per row height, clipped
    /// to the pane's code column.
    fn code(&self, ctx: &mut PaintCtx<'_>, index: usize, line: &DiffLine, area: Rect) {
        let clip = Rect::new(area.x0, area.y0, area.x1 - self.m.pad, area.y1);
        if clip.width() <= 0.0 {
            return;
        }
        let style = ctx.theme.text(TextRole::Code);
        let line_h = f64::from(ctx.text.measure_style("Ag", None, &style).1);
        let whole = 0..line.content.len();
        let segments = self
            .wrapped
            .get(index)
            .filter(|w| !w.is_empty())
            .map_or_else(|| std::slice::from_ref(&whole), Vec::as_slice);
        let spans = if self.input.settings.syntax_highlight {
            SyntaxHighlighter::for_token_or_plain(&self.input.diff.language)
                .start()
                .highlight_line(&line.content)
        } else {
            Vec::new()
        };
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        let mut band_top = area.y0;
        let indent = if segments.len() > 1 {
            ui::num::f64_of_usize(hanging_indent(&line.content, self.cols)) * self.m.cell_w
        } else {
            0.0
        };
        for (row, segment) in segments.iter().enumerate() {
            let y = ui::text_metrics::centered_text_top(band_top, self.m.row_h, line_h);
            let x = area.x0 - self.hscroll + if row == 0 { 0.0 } else { indent };
            draw_code(
                ctx,
                &line.content,
                segment.clone(),
                &spans,
                Point::new(x, y),
                &style,
            );
            band_top += self.m.row_h;
        }
        ctx.scene.pop_layer();
    }
}

/// The `range` of `content` with its top-left at `origin`, coloured by the
/// theme's syntax palette where `spans` cover it (plain ink elsewhere).
fn draw_code(
    ctx: &mut PaintCtx<'_>,
    content: &str,
    range: Range<usize>,
    spans: &[Span],
    origin: Point,
    style: &TextStyle,
) {
    let theme = ctx.theme;
    let plain = theme.text_primary;
    if spans.is_empty() {
        if let Some(text) = content.get(range) {
            ctx.text(text, style, plain, origin, None);
        }
        return;
    }
    let mut x = origin.x;
    let mut at = range.start;
    let mut paint = |ctx: &mut PaintCtx<'_>, from: usize, to: usize, color: Color| {
        if let Some(text) = content.get(from..to)
            && !text.is_empty()
        {
            ctx.text(text, style, color, Point::new(x, origin.y), None);
            x += f64::from(ctx.text.measure_style(text, None, style).0);
        }
    };
    for span in spans {
        let from = span.range.start.max(range.start);
        let to = span.range.end.min(range.end);
        if from >= to {
            continue;
        }
        if from > at {
            paint(ctx, at, from, plain);
        }
        paint(ctx, from, to, theme.tokens.syntax.color(span.kind));
        at = to;
    }
    if at < range.end {
        paint(ctx, at, range.end, plain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;
    use vello::Scene;
    use vello::kurbo::Affine;

    fn sample_diff() -> FileDiff {
        parse_unified(
            "src/scene.rs",
            "rust",
            "diff --git a/src/scene.rs b/src/scene.rs\nindex 1..2 100644\n--- a/src/scene.rs\n+++ b/src/scene.rs\n@@ -10,3 +10,4 @@ impl Scene for a very long context that overflows the pane\n a\n-b\n+c\n+d\n@@ -40,2 +41,2 @@\n e\n-f\n+g\n",
        )
    }

    fn draw(diff: &FileDiff, width: f64, layout: DiffLayout, staged: bool, wrap: bool) -> DiffDraw {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let settings = Settings {
            diff_layout: layout,
            wrap_lines: wrap,
            ..Settings::default()
        };
        paint(
            &mut ctx,
            Rect::new(0.0, 0.0, width, 600.0),
            &DiffInput {
                diff,
                scroll: 0.0,
                hscroll: 0.0,
                settings,
                staged,
                can_discard: true,
                pointer: None,
            },
        )
    }

    /// Metadata lines are not rows, side-by-side pairs a deletion with the
    /// addition after it, and the rows follow the measured `Code` line box.
    #[test]
    fn rows_skip_metadata_pair_changes_and_follow_the_code_line() {
        let diff = sample_diff();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let m = Metrics::measure(&mut text, &theme, &diff);
        let wrapped = wrap_lines(&diff, 200);
        let (unified, height) = lay_out(&diff, DiffLayout::Unified, &m, &wrapped);
        let code_lines = diff
            .lines
            .iter()
            .filter(|l| !matches!(l.kind, DiffLineKind::Meta))
            .count();
        assert_eq!(unified.len(), code_lines);
        assert!((height - (2.0 * m.hunk_h + 7.0 * m.row_h)).abs() < 0.01);
        let (split, _) = lay_out(&diff, DiffLayout::SideBySide, &m, &wrapped);
        assert!(split.iter().any(|(row, _, _)| matches!(
            row,
            Row::Pair(Some(l), Some(r))
                if diff.lines[*l].kind == DiffLineKind::Deletion
                    && diff.lines[*r].kind == DiffLineKind::Addition
        )));

        let two = Metrics::measure(&mut text, &Theme::dark().with_text_scale(2.0), &diff);
        assert!(two.row_h > m.row_h && two.hunk_h >= m.hunk_h);
    }

    /// Each hunk offers its own action, the worktree side offers discard
    /// beside it, and the file header carries the file's actions.
    #[test]
    fn hunks_and_the_file_offer_their_actions() {
        let diff = sample_diff();
        let worktree = draw(&diff, 900.0, DiffLayout::Unified, false, true);
        for hunk in 0..2 {
            for action in [HunkAction::Toggle, HunkAction::Discard] {
                assert!(
                    worktree
                        .targets
                        .iter()
                        .any(|(_, hit)| *hit == DiffHit::Hunk(hunk, action)),
                    "hunk {hunk} offers {action:?}"
                );
            }
        }
        assert!(
            worktree
                .targets
                .iter()
                .any(|(_, h)| *h == DiffHit::DiscardFile)
        );
        let staged = draw(&diff, 900.0, DiffLayout::Unified, true, true);
        assert!(
            !staged.targets.iter().any(|(_, h)| matches!(
                h,
                DiffHit::DiscardFile | DiffHit::Hunk(_, HunkAction::Discard)
            )),
            "the staged side has nothing to discard"
        );
        // Targets never overlap one another.
        for (i, (a, _)) in worktree.targets.iter().enumerate() {
            for (b, _) in worktree.targets.iter().skip(i + 1) {
                assert!(a.intersect(*b).area() <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    /// Without wrapping, a narrow pane scrolls its long lines sideways; with
    /// it, nothing scrolls sideways.
    #[test]
    fn unwrapped_lines_scroll_sideways() {
        let diff = parse_unified(
            "x.rs",
            "rust",
            "@@ -1,1 +1,1 @@\n-let value = compute_the_answer_to_everything(with_a_very_long_argument_list, and_more);\n+let value = compute_the_answer_to_everything(with_an_even_longer_argument_list, and_more);\n",
        );
        assert!(draw(&diff, 400.0, DiffLayout::Unified, false, false).max_hscroll > 0.0);
        assert!(draw(&diff, 2400.0, DiffLayout::Unified, false, false).max_hscroll <= 0.0);
        assert!(draw(&diff, 400.0, DiffLayout::Unified, false, true).max_hscroll <= 0.0);
    }

    /// A long line wraps onto more rows of its pane instead of being cut at
    /// the pane edge, breaking after a space or a word where one falls late enough,
    /// and the row grows with it; a wide pane keeps it on one row.
    #[test]
    fn long_lines_wrap_inside_their_pane() {
        let line = "let value = compute(with_a_long_argument, and_more);";
        let segments = wrap(line, 20);
        assert!(segments.len() > 1);
        let joined: String = segments.iter().map(|r| &line[r.clone()]).collect();
        assert_eq!(joined, line, "wrapping keeps every character once");
        assert!(
            segments
                .iter()
                .all(|r| line[r.clone()].chars().count() <= 20)
        );
        assert_eq!(&line[segments[0].clone()], "let value = compute");
        assert_eq!(wrap("short", 20), vec![0..5]);
        let nested = "        call(first_argument, second_argument);";
        let rows = wrap(nested, 24);
        assert!(rows.len() > 2);
        assert!(
            rows.iter()
                .skip(1)
                .all(|r| nested[r.clone()].chars().count() <= 24 - 8),
            "continuation rows leave room for the hanging indent"
        );
        assert_eq!(wrap("", 20), vec![0..0]);

        let diff = parse_unified(
            "x.rs",
            "rust",
            &format!("@@ -1,1 +1,1 @@\n-{line}\n+{line} // and a trailing comment\n"),
        );
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let m = Metrics::measure(&mut text, &theme, &diff);
        for layout in [DiffLayout::Unified, DiffLayout::SideBySide] {
            let narrow = wrap_lines(&diff, code_columns(400.0, layout, &m));
            let (rows, _) = lay_out(&diff, layout, &m, &narrow);
            assert!(
                rows.iter().any(|(_, _, h)| *h > m.row_h * 1.5),
                "{layout:?}: a long line takes more than one row"
            );
            let wide = wrap_lines(&diff, code_columns(2400.0, layout, &m));
            let (rows, _) = lay_out(&diff, layout, &m, &wide);
            assert!(
                rows.iter()
                    .all(|(row, _, h)| matches!(row, Row::Hunk(_)) || (*h - m.row_h).abs() < 0.01),
                "{layout:?}: a wide pane keeps every line on one row"
            );
        }
    }

    /// A word is not cut from the punctuation that follows it when the break
    /// can fall between them.
    #[test]
    fn a_break_falls_between_a_word_and_its_punctuation() {
        let line = "    let x = blur_params);";
        let cols = 24;
        let rows = wrap(line, cols);
        assert!(rows.len() > 1);
        assert_eq!(&line[rows[0].clone()], "    let x = blur_params");
    }
}
