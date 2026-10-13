//! Recursive 2D box layout for the math model.
//!
//! Lays the [`MathExpr`](crate::math::model::MathExpr) tree out into a flat list of positioned
//! [`Box2d`]es with exact glyph metrics from [`TextRenderer`], plus the gap
//! geometry the caret and hit-testing need. Layout is pure: it reads the
//! model + a renderer (for measuring) and produces positions; it never
//! mutates the model.
//!
//! Coordinates are local to the field's content origin `(0, 0)`; the caller
//! translates by the field's on-screen rect.
//!
//! Every size here is content: glyphs are measured and painted at exactly
//! the size the geometry (bars, radicals, delimiters, script offsets) is
//! built from, never grown again by the reader's text scale, so a caller
//! that scales an expression scales all of it together. [`layout_wrapped`]
//! breaks a long expression before its top-level operators.

use crate::text_render::{FontChoice, TextRenderer, TextStyle, UI_MATH_FAMILY};

use super::model::{Node, Row, Slot};

/// What a laid-out box paints.
#[derive(Clone, Debug)]
pub enum BoxKind {
    /// A glyph (digit / letter / operator) drawn at `size`.
    Glyph { ch: char, size: f32 },
    /// An upright function name (sin, cos, …).
    Name { text: String, size: f32 },
    /// The horizontal bar of a fraction.
    FracBar,
    /// The `√` sign + its overbar; `over_w` is the radicand span the bar
    /// must cover, starting at the box's right edge.
    SqrtSign { over_w: f64, size: f32 },
    /// A vector parenthesis constructed to exactly span this box. It is not
    /// a vertically-scaled font glyph: scaling a normal `(` distorts its
    /// stroke and, more importantly, makes its optical centre depend on the
    /// font glyph's baseline rather than on the enclosed expression.
    Paren { open: bool, size: f32 },
    /// A scalable TeX delimiter (`[]`, `{}`, `|`, `‖`, `⌊⌋`, `⌈⌉`, `⟨⟩`).
    Delimiter { ch: char, size: f32 },
    /// Close-fitting accent path spanning `w` (`¯`, `ˆ`, `˜`, arrows, dots).
    Accent { mark: char, size: f32 },
}

/// One positioned, paintable box.
#[derive(Clone, Debug)]
pub struct Box2d {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Baseline offset from `y` (top) to the text baseline.
    pub baseline: f64,
    pub kind: BoxKind,
}

/// A caret-insertable gap inside one laid-out row. `index` is the offset in
/// that row (0..=len). `x`/`top`/`bottom` give the caret bar geometry.
#[derive(Clone, Debug)]
pub struct Gap {
    /// Path steps identifying which row this gap belongs to.
    pub steps: Vec<(usize, Slot)>,
    pub index: usize,
    pub x: f64,
    pub top: f64,
    pub bottom: f64,
}

/// The result of laying out a whole field.
#[derive(Clone, Debug)]
pub struct Layout {
    pub boxes: Vec<Box2d>,
    pub gaps: Vec<Gap>,
    pub width: f64,
    pub height: f64,
    /// Baseline of the root row from the top of the content box.
    pub baseline: f64,
}

/// Internal size of a laid-out fragment before it is placed.
struct Frag {
    boxes: Vec<Box2d>,
    gaps: Vec<Gap>,
    width: f64,
    /// Height above the baseline.
    ascent: f64,
    /// Height below the baseline.
    descent: f64,
}

impl Frag {
    fn height(&self) -> f64 {
        self.ascent + self.descent
    }
}

pub const BASE_SIZE: f32 = 22.0;
const SCRIPT_RATIO: f32 = 0.72;
const MIN_SIZE: f32 = 11.0;

fn line_h(size: f32) -> f64 {
    f64::from(size * 1.25)
}

/// Approximate ascent/descent split of a glyph cell. cosmic-text centers the
/// glyph in a `size * 1.25` cell; the baseline sits near 80% down.
fn metrics(size: f32) -> (f64, f64) {
    let lh = line_h(size);
    let ascent = lh * 0.78;
    let descent = lh - ascent;
    (ascent, descent)
}

fn delimiter_width(size: f32) -> f64 {
    (f64::from(size) * 0.34).max(6.0)
}

fn delimiter_width_for(ch: char, size: f32) -> f64 {
    let em = f64::from(size);
    match ch {
        '|' | '‖' => (em * 0.20).max(4.0),
        '[' | ']' | '⌊' | '⌋' | '⌈' | '⌉' => (em * 0.28).max(5.0),
        '{' | '}' => (em * 0.42).max(8.0),
        '⟨' | '⟩' | '<' | '>' => (em * 0.38).max(7.0),
        _ => delimiter_width(size),
    }
}

fn delimiter_padding(size: f32) -> f64 {
    (f64::from(size) * 0.10).max(1.5)
}

/// The math face at exactly `size`, as content: measured and painted at the
/// size the layout's geometry uses.
pub(crate) fn math_style(size: f32) -> TextStyle {
    TextStyle::new(size)
        .with_family(FontChoice::Named(UI_MATH_FAMILY.into()))
        .with_content_size(size)
}

fn measure_glyph(tr: &mut TextRenderer, ch: char, size: f32) -> f64 {
    let mut buf = [0u8; 4];
    let display = math_display_char(ch);
    let s = display.encode_utf8(&mut buf);
    let (w, _) = tr.measure_style(s, None, &math_style(size));
    f64::from(w)
}

fn measure_name(tr: &mut TextRenderer, name: &str, size: f32) -> f64 {
    let (w, _) = tr.measure_style(name, None, &math_style(size));
    f64::from(w)
}

/// Noto Sans Math encodes mathematical italic letters as their semantic
/// Unicode math alphabet. Keep the source tree plain while selecting those
/// presentation glyphs for single-letter variables.
pub fn math_display_char(character: char) -> char {
    match character {
        'A'..='Z' => {
            let offset = (character as u32).saturating_sub('A' as u32);
            char::from_u32(0x1D434u32.saturating_add(offset)).unwrap_or(character)
        }
        'a'..='z' if character != 'h' => {
            let offset = (character as u32).saturating_sub('a' as u32);
            char::from_u32(0x1D44Eu32.saturating_add(offset)).unwrap_or(character)
        }
        // Mathematical italic small h occupies the historic Planck-constant
        // code point rather than the otherwise contiguous alphabet range.
        'h' => '\u{210E}',
        _ => character,
    }
}

fn operator_spacing(character: char, size: f32) -> (f64, f64) {
    let em = f64::from(size);
    if matches!(
        character,
        '=' | '<' | '>' | '≤' | '≥' | '≈' | '≠' | '∈' | '∉'
    ) {
        (em * 0.22, em * 0.22)
    } else if matches!(
        character,
        '+' | '-' | '−' | '*' | '·' | '×' | '÷' | '±' | '∓'
    ) {
        (em * 0.16, em * 0.16)
    } else if matches!(character, ',' | ';' | ':') {
        (0.0, em * 0.10)
    } else {
        (0.0, 0.0)
    }
}

/// Full horizontal advance of one mathematical atom, including its TeX-style
/// binary/relation spacing. Editors use this when mapping external geometry.
pub fn measure_atom(tr: &mut TextRenderer, character: char, size: f32) -> f64 {
    let (before, after) = operator_spacing(character, size);
    before + measure_glyph(tr, character, size) + after
}

/// Lay out a whole expression at the field origin. The fragment's baseline is
/// already at local `y = ascent`; boxes/gaps are emitted directly in content
/// coords (top of the row at `y = 0`).
pub fn layout(row: &Row, tr: &mut TextRenderer, base_size: f32) -> Layout {
    let frag = layout_row(row, &[], tr, base_size);
    let baseline = frag.ascent;
    let height = frag.height();
    let min_w = measure_glyph(tr, '0', base_size);
    Layout {
        width: frag.width.max(min_w),
        height,
        baseline,
        boxes: frag.boxes,
        gaps: frag.gaps,
    }
}

/// Whether a top-level row may break before this node: a relation or a
/// binary operator that is not a sign (it follows an operand).
fn breaks_before(row: &Row, index: usize) -> bool {
    let Some(Node::Char(ch)) = row.get(index) else {
        return false;
    };
    let relation = matches!(ch, '=' | '<' | '>' | '≤' | '≥' | '≈' | '≠' | '∈' | '∉');
    let binary = matches!(ch, '+' | '-' | '−' | '×' | '·' | '±' | '∓' | '÷');
    if !(relation || binary) || index == 0 {
        return false;
    }
    // A sign right after another operator stays with its operand.
    !matches!(
        row.get(index - 1),
        Some(Node::Char(
            '=' | '<'
                | '>'
                | '≤'
                | '≥'
                | '≈'
                | '≠'
                | '+'
                | '-'
                | '−'
                | '×'
                | '·'
                | '±'
                | '∓'
                | '÷'
        ))
    )
}

/// Lay out an expression no wider than `max_width` where it can be: a line
/// that would run over breaks before a top-level relation or binary
/// operator, and the continuation lines are indented by one em. Gaps keep
/// their row indices, so a caret still lands where it did.
pub fn layout_wrapped(row: &Row, tr: &mut TextRenderer, base_size: f32, max_width: f64) -> Layout {
    let whole = layout(row, tr, base_size);
    if whole.width <= max_width || row.len() < 2 {
        return whole;
    }
    // Where each top-level node starts, from the root row's gaps.
    let starts: Vec<f64> = (0..=row.len())
        .map(|index| {
            whole
                .gaps
                .iter()
                .find(|gap| gap.steps.is_empty() && gap.index == index)
                .map_or(0.0, |gap| gap.x)
        })
        .collect();
    let indent = f64::from(base_size);
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    while start < row.len() {
        let budget = if lines.is_empty() {
            max_width
        } else {
            (max_width - indent).max(1.0)
        };
        let origin = starts.get(start).copied().unwrap_or(0.0);
        let mut end = row.len();
        if starts.get(row.len()).copied().unwrap_or(0.0) - origin > budget {
            // The farthest break that still fits; at least one node a line.
            let fits = (start + 1..row.len())
                .filter(|&index| breaks_before(row, index))
                .take_while(|&index| starts.get(index).copied().unwrap_or(0.0) - origin <= budget)
                .last();
            let first_break = (start + 1..row.len()).find(|&index| breaks_before(row, index));
            if let Some(index) = fits.or(first_break) {
                end = index;
            }
        }
        lines.push((start, end));
        start = end;
    }
    if lines.len() < 2 {
        return whole;
    }
    let line_gap = line_h(base_size) * 0.25;
    let mut out = Layout {
        boxes: Vec::new(),
        gaps: Vec::new(),
        width: 0.0,
        height: 0.0,
        baseline: 0.0,
    };
    let mut top = 0.0;
    for (line, &(start, end)) in lines.iter().enumerate() {
        let segment: Row = row
            .get(start..end)
            .map(<[Node]>::to_vec)
            .unwrap_or_default();
        let frag = layout_row(&segment, &[], tr, base_size);
        let x = if line == 0 { 0.0 } else { indent };
        if line == 0 {
            out.baseline = frag.ascent;
        }
        let height = frag.height();
        for mut b in frag.boxes {
            b.x += x;
            b.y += top;
            out.boxes.push(b);
        }
        for mut g in frag.gaps {
            g.x += x;
            g.top += top;
            g.bottom += top;
            if g.steps.is_empty() {
                // The segment's leading gap is the previous line's trailing
                // one; keep a single caret stop per index.
                if line > 0 && g.index == 0 {
                    continue;
                }
                g.index += start;
            } else if let Some(first) = g.steps.first_mut() {
                first.0 += start;
            }
            out.gaps.push(g);
        }
        out.width = out.width.max(x + frag.width);
        top += height + line_gap;
    }
    out.height = (top - line_gap).max(0.0);
    out
}

/// Lay out a row, producing a fragment whose baseline is at local y = ascent.
/// `steps` is the path prefix identifying this row in the tree.
fn layout_row(row: &Row, steps: &[(usize, Slot)], tr: &mut TextRenderer, size: f32) -> Frag {
    let (mut ascent, mut descent) = metrics(size);
    // First pass: build each node's fragment and track the row's extents.
    let mut frags: Vec<(usize, NodeFrag)> = Vec::new();
    for (i, node) in row.iter().enumerate() {
        let nf = layout_node(node, i, steps, tr, size);
        ascent = ascent.max(nf.ascent);
        descent = descent.max(nf.descent);
        frags.push((i, nf));
    }
    let baseline = ascent;

    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let mut pen = 0.0f64;
    let caret_h = line_h(size);
    let caret_top = baseline - caret_h * 0.78;
    let caret_bot = baseline + caret_h * 0.22;

    // Leading gap (offset 0).
    gaps.push(Gap {
        steps: steps.to_vec(),
        index: 0,
        x: 0.0,
        top: caret_top,
        bottom: caret_bot,
    });

    for (i, nf) in frags {
        // Place this node's boxes, shifting so its baseline aligns to the
        // row baseline.
        let dy = baseline - nf.ascent;
        for mut b in nf.boxes {
            b.x += pen;
            b.y += dy;
            boxes.push(b);
        }
        for mut g in nf.gaps {
            g.x += pen;
            g.top += dy;
            g.bottom += dy;
            gaps.push(g);
        }
        pen += nf.width;
        // Trailing gap after node `i` (offset i+1).
        gaps.push(Gap {
            steps: steps.to_vec(),
            index: i.saturating_add(1),
            x: pen,
            top: caret_top,
            bottom: caret_bot,
        });
    }

    Frag {
        boxes,
        gaps,
        width: pen,
        ascent,
        descent,
    }
}

/// A node's fragment: its own boxes/gaps plus the row baseline relative to
/// the fragment top (`ascent`).
struct NodeFrag {
    boxes: Vec<Box2d>,
    gaps: Vec<Gap>,
    width: f64,
    ascent: f64,
    descent: f64,
}

fn layout_node(
    node: &Node,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    match node {
        Node::Char(ch) => layout_char(*ch, tr, size),
        Node::Text(text) => layout_text(text, tr, size),
        Node::Accent { base, mark, over } => {
            layout_accent(base, *mark, *over, idx, steps, tr, size)
        }
        Node::UnderOver { base, under, over } => {
            layout_under_over(base, under.as_ref(), over.as_ref(), idx, steps, tr, size)
        }
        Node::Table { rows } => layout_table(rows, idx, steps, tr, size),
        Node::Func { name, arg } => layout_func(name, arg, idx, steps, tr, size),
        Node::Paren { body } => {
            layout_delimited(body, Some('('), Some(')'), idx, steps, tr, size, true)
        }
        Node::Delimited { open, close, body } => {
            layout_delimited(body, *open, *close, idx, steps, tr, size, false)
        }
        Node::Sup { exp } => layout_sup(exp, idx, steps, tr, size),
        Node::Sub { sub } => layout_sub(sub, idx, steps, tr, size),
        Node::SubSup { sub, sup } => layout_subsup(sub, sup, idx, steps, tr, size),
        Node::Frac { num, den } => layout_frac(num, den, idx, steps, tr, size),
        Node::Sqrt { index, radicand } => {
            layout_sqrt(index.as_ref(), radicand, idx, steps, tr, size)
        }
    }
}

fn layout_char(ch: char, tr: &mut TextRenderer, size: f32) -> NodeFrag {
    let w = measure_glyph(tr, ch, size);
    let (space_before, space_after) = operator_spacing(ch, size);
    let (ascent, descent) = metrics(size);
    let b = Box2d {
        x: space_before,
        y: 0.0,
        w,
        h: ascent + descent,
        baseline: ascent,
        kind: BoxKind::Glyph {
            ch: math_display_char(ch),
            size,
        },
    };
    NodeFrag {
        boxes: vec![b],
        gaps: vec![],
        width: space_before + w + space_after,
        ascent,
        descent,
    }
}

fn layout_text(text: &str, tr: &mut TextRenderer, size: f32) -> NodeFrag {
    let width = measure_name(tr, text, size);
    let (ascent, descent) = metrics(size);
    NodeFrag {
        boxes: vec![Box2d {
            x: 0.0,
            y: 0.0,
            w: width,
            h: ascent + descent,
            baseline: ascent,
            kind: BoxKind::Name {
                text: text.to_string(),
                size,
            },
        }],
        gaps: vec![],
        width,
        ascent,
        descent,
    }
}

fn layout_accent(
    base: &Row,
    mark: char,
    over: bool,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let base_steps = child_steps(steps, idx, Slot::Base);
    let inner = layout_row(base, &base_steps, tr, size);
    let accent_height = match mark {
        '˙' | '¨' => f64::from(size) * 0.18,
        _ => f64::from(size) * 0.24,
    }
    .max(3.0);
    let gap = (f64::from(size) * 0.045).max(1.0);
    let width = inner.width.max(f64::from(size) * 0.45);
    let inner_x = (width - inner.width) * 0.5;
    let (ascent, descent, inner_y, accent_y) = if over {
        (
            inner.ascent + accent_height + gap,
            inner.descent,
            accent_height + gap,
            0.0,
        )
    } else {
        (
            inner.ascent,
            inner.descent + accent_height + gap,
            0.0,
            inner.height() + gap,
        )
    };
    let mut boxes = vec![Box2d {
        x: 0.0,
        y: accent_y,
        w: width,
        h: accent_height,
        baseline: accent_height,
        kind: BoxKind::Accent { mark, size },
    }];
    let mut gaps = Vec::new();
    for mut math_box in inner.boxes {
        math_box.x += inner_x;
        math_box.y += inner_y;
        boxes.push(math_box);
    }
    for mut insertion in inner.gaps {
        insertion.x += inner_x;
        insertion.top += inner_y;
        insertion.bottom += inner_y;
        gaps.push(insertion);
    }
    NodeFrag {
        boxes,
        gaps,
        width,
        ascent,
        descent,
    }
}

fn layout_under_over(
    base: &Row,
    under: Option<&Row>,
    over: Option<&Row>,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let base_steps = child_steps(steps, idx, Slot::Base);
    let base_fragment = layout_row(base, &base_steps, tr, size);
    let script_size = (size * SCRIPT_RATIO).max(MIN_SIZE);
    let upper =
        over.map(|row| layout_row(row, &child_steps(steps, idx, Slot::Over), tr, script_size));
    let lower =
        under.map(|row| layout_row(row, &child_steps(steps, idx, Slot::Under), tr, script_size));
    let gap = f64::from(size) * 0.12;
    let width = base_fragment
        .width
        .max(upper.as_ref().map_or(0.0, |fragment| fragment.width))
        .max(lower.as_ref().map_or(0.0, |fragment| fragment.width));
    let upper_height = upper.as_ref().map_or(0.0, Frag::height);
    let base_top = if upper.is_some() {
        upper_height + gap
    } else {
        0.0
    };
    let ascent = base_top + base_fragment.ascent;
    let base_descent = base_fragment.descent;
    let descent = base_descent
        + lower
            .as_ref()
            .map_or(0.0, |fragment| gap + fragment.height());
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let mut place = |fragment: Frag, top: f64| {
        let x = (width - fragment.width) * 0.5;
        for mut math_box in fragment.boxes {
            math_box.x += x;
            math_box.y += top;
            boxes.push(math_box);
        }
        for mut insertion in fragment.gaps {
            insertion.x += x;
            insertion.top += top;
            insertion.bottom += top;
            gaps.push(insertion);
        }
    };
    if let Some(upper) = upper {
        place(upper, 0.0);
    }
    place(base_fragment, base_top);
    if let Some(lower) = lower {
        place(lower, ascent + base_descent + gap);
    }
    NodeFrag {
        boxes,
        gaps,
        width,
        ascent,
        descent,
    }
}

fn layout_table(
    rows: &[Vec<Row>],
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() || column_count == 0 {
        let (ascent, descent) = metrics(size);
        return NodeFrag {
            boxes: Vec::new(),
            gaps: Vec::new(),
            width: measure_glyph(tr, '0', size),
            ascent,
            descent,
        };
    }
    let measured = measure_table(rows, column_count, idx, steps, tr, size);
    place_table(measured)
}

/// Cell fragments plus the shared row/column geometry `layout_table` derives
/// from them, kept separate so measuring and placing the grid stay under the
/// function-length lint independently.
struct TableMeasure {
    fragments: Vec<Vec<Frag>>,
    column_widths: Vec<f64>,
    row_ascents: Vec<f64>,
    row_descents: Vec<f64>,
    column_gap: f64,
    row_gap: f64,
    width: f64,
    ascent: f64,
    descent: f64,
}

fn measure_table(
    rows: &[Vec<Row>],
    column_count: usize,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> TableMeasure {
    let mut fragments: Vec<Vec<Frag>> = Vec::with_capacity(rows.len());
    let mut column_widths = vec![0.0f64; column_count];
    let mut row_ascents = Vec::with_capacity(rows.len());
    let mut row_descents = Vec::with_capacity(rows.len());
    for (row_index, cells) in rows.iter().enumerate() {
        let mut laid_cells = Vec::with_capacity(cells.len());
        let mut row_ascent: f64 = 0.0;
        let mut row_descent: f64 = 0.0;
        for (column, cell) in cells.iter().enumerate() {
            let fragment = layout_row(
                cell,
                &child_steps(
                    steps,
                    idx,
                    Slot::Cell(
                        u16::try_from(row_index).unwrap_or(u16::MAX),
                        u16::try_from(column).unwrap_or(u16::MAX),
                    ),
                ),
                tr,
                size,
            );
            if let Some(slot) = column_widths.get_mut(column) {
                *slot = slot.max(fragment.width);
            }
            row_ascent = row_ascent.max(fragment.ascent);
            row_descent = row_descent.max(fragment.descent);
            laid_cells.push(fragment);
        }
        fragments.push(laid_cells);
        row_ascents.push(row_ascent);
        row_descents.push(row_descent);
    }
    let column_gap = f64::from(size) * 0.65;
    let row_gap = f64::from(size) * 0.28;
    let column_gaps = f64::from(u32::try_from(column_count.saturating_sub(1)).unwrap_or(u32::MAX));
    let width = column_widths.iter().sum::<f64>() + column_gap * column_gaps;
    let row_gaps = f64::from(u32::try_from(rows.len().saturating_sub(1)).unwrap_or(u32::MAX));
    let height = row_ascents
        .iter()
        .zip(&row_descents)
        .map(|(ascent, descent)| ascent + descent)
        .sum::<f64>()
        + row_gap * row_gaps;
    let ascent = height * 0.5;
    let descent = height - ascent;
    TableMeasure {
        fragments,
        column_widths,
        row_ascents,
        row_descents,
        column_gap,
        row_gap,
        width,
        ascent,
        descent,
    }
}

fn place_table(measured: TableMeasure) -> NodeFrag {
    let TableMeasure {
        fragments,
        column_widths,
        row_ascents,
        row_descents,
        column_gap,
        row_gap,
        width,
        ascent,
        descent,
    } = measured;
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let mut row_top = 0.0;
    for (row_index, laid_cells) in fragments.into_iter().enumerate() {
        let mut column_x = 0.0;
        let row_ascent = row_ascents.get(row_index).copied().unwrap_or(0.0);
        let row_descent = row_descents.get(row_index).copied().unwrap_or(0.0);
        for (column, fragment) in laid_cells.into_iter().enumerate() {
            let column_width = column_widths.get(column).copied().unwrap_or(0.0);
            let x = column_x + (column_width - fragment.width) * 0.5;
            let y = row_top + row_ascent - fragment.ascent;
            for mut math_box in fragment.boxes {
                math_box.x += x;
                math_box.y += y;
                boxes.push(math_box);
            }
            for mut insertion in fragment.gaps {
                insertion.x += x;
                insertion.top += y;
                insertion.bottom += y;
                gaps.push(insertion);
            }
            column_x += column_width + column_gap;
        }
        row_top += row_ascent + row_descent + row_gap;
    }
    NodeFrag {
        boxes,
        gaps,
        width,
        ascent,
        descent,
    }
}

fn layout_func(
    name: &str,
    arg: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let name_w = measure_name(tr, name, size);
    let (ascent, descent) = metrics(size);
    let arg_steps = child_steps(steps, idx, Slot::Arg);
    let inner = layout_row(arg, &arg_steps, tr, size);
    let paren_size = size;
    let open = delimiter_width(paren_size);
    let close = open;
    let delimiter_pad = delimiter_padding(paren_size);
    let row_ascent = ascent.max(inner.ascent + delimiter_pad);
    let row_descent = descent.max(inner.descent + delimiter_pad);
    let total_ascent = row_ascent;
    // The function name is the SAME font size as its argument; sit its
    // box so its baseline lands on the row baseline (`total_ascent`),
    // not the frag top — otherwise a tall argument (a √ with
    // superscripts, e.g. `sin(√(x²+y²))`) would leave the name floating
    // above and reading as an oversized, misaligned glyph.
    let mut boxes = vec![Box2d {
        x: 0.0,
        y: total_ascent - ascent,
        w: name_w,
        h: ascent + descent,
        baseline: ascent,
        kind: BoxKind::Name {
            text: name.to_string(),
            size,
        },
    }];
    let mut gaps = Vec::new();
    let mut pen = name_w;
    boxes.push(Box2d {
        x: pen,
        y: 0.0,
        w: open,
        h: row_ascent + row_descent,
        baseline: total_ascent,
        kind: BoxKind::Paren {
            open: true,
            size: paren_size,
        },
    });
    pen += open;
    let dy = total_ascent - inner.ascent;
    for mut b in inner.boxes {
        b.x += pen;
        b.y += dy;
        boxes.push(b);
    }
    for mut g in inner.gaps {
        g.x += pen;
        g.top += dy;
        g.bottom += dy;
        gaps.push(g);
    }
    pen += inner.width;
    boxes.push(Box2d {
        x: pen,
        y: 0.0,
        w: close,
        h: row_ascent + row_descent,
        baseline: total_ascent,
        kind: BoxKind::Paren {
            open: false,
            size: paren_size,
        },
    });
    pen += close;
    NodeFrag {
        boxes,
        gaps,
        width: pen,
        ascent: total_ascent,
        descent: row_descent,
    }
}

fn layout_sup(
    exp: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    // Exponent is raised and smaller, sitting above the row baseline.
    let exp_size = (size * SCRIPT_RATIO).max(MIN_SIZE);
    let exp_steps = child_steps(steps, idx, Slot::Exp);
    let inner = layout_row(exp, &exp_steps, tr, exp_size);
    let (base_ascent, base_descent) = metrics(size);
    // Raise the exponent so its bottom sits ~ at 55% of base ascent.
    let raise = base_ascent * 0.55;
    let ascent = (inner.height() + raise).max(base_ascent);
    let descent = base_descent;
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let dy = ascent - inner.height();
    for mut b in inner.boxes {
        b.y += dy;
        boxes.push(b);
    }
    for mut g in inner.gaps {
        g.top += dy;
        g.bottom += dy;
        gaps.push(g);
    }
    let width = inner.width.max(measure_glyph(tr, '2', exp_size));
    NodeFrag {
        boxes,
        gaps,
        width,
        ascent,
        descent,
    }
}

fn layout_sub(
    sub: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let sub_size = (size * SCRIPT_RATIO).max(MIN_SIZE);
    let sub_steps = child_steps(steps, idx, Slot::Sub);
    let inner = layout_row(sub, &sub_steps, tr, sub_size);
    let (ascent, base_descent) = metrics(size);
    let shift = f64::from(size) * 0.28;
    let top = ascent + shift - inner.ascent;
    let descent = base_descent.max(top + inner.height() - ascent);
    let mut boxes = inner.boxes;
    let mut gaps = inner.gaps;
    for math_box in &mut boxes {
        math_box.y += top;
    }
    for gap in &mut gaps {
        gap.top += top;
        gap.bottom += top;
    }
    NodeFrag {
        width: inner.width.max(measure_glyph(tr, '0', sub_size)),
        boxes,
        gaps,
        ascent,
        descent,
    }
}

fn layout_subsup(
    sub: &Row,
    sup: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let script_size = (size * SCRIPT_RATIO).max(MIN_SIZE);
    let sub_steps = child_steps(steps, idx, Slot::Sub);
    let superscript_steps = child_steps(steps, idx, Slot::Sup);
    let lower = layout_row(sub, &sub_steps, tr, script_size);
    let upper = layout_row(sup, &superscript_steps, tr, script_size);
    let (base_ascent, base_descent) = metrics(size);
    let raise = base_ascent * 0.55;
    let ascent = (upper.height() + raise).max(base_ascent);
    let lower_top = ascent + f64::from(size) * 0.28 - lower.ascent;
    let descent = base_descent.max(lower_top + lower.height() - ascent);
    let width = lower
        .width
        .max(upper.width)
        .max(measure_glyph(tr, '0', script_size));
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let upper_x = (width - upper.width) * 0.5;
    let upper_y = ascent - upper.height();
    for mut math_box in upper.boxes {
        math_box.x += upper_x;
        math_box.y += upper_y;
        boxes.push(math_box);
    }
    for mut gap in upper.gaps {
        gap.x += upper_x;
        gap.top += upper_y;
        gap.bottom += upper_y;
        gaps.push(gap);
    }
    let lower_x = (width - lower.width) * 0.5;
    for mut math_box in lower.boxes {
        math_box.x += lower_x;
        math_box.y += lower_top;
        boxes.push(math_box);
    }
    for mut gap in lower.gaps {
        gap.x += lower_x;
        gap.top += lower_top;
        gap.bottom += lower_top;
        gaps.push(gap);
    }
    NodeFrag {
        boxes,
        gaps,
        width,
        ascent,
        descent,
    }
}

fn layout_frac(
    num: &Row,
    den: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let num_steps = child_steps(steps, idx, Slot::Num);
    let den_steps = child_steps(steps, idx, Slot::Den);
    let num_f = layout_row(num, &num_steps, tr, size);
    let den_f = layout_row(den, &den_steps, tr, size);
    let pad = 6.0;
    let bar_pad = 4.0;
    let width = num_f
        .width
        .max(den_f.width)
        .max(measure_glyph(tr, '0', size))
        + pad * 2.0;
    let bar_thick = 1.6;
    let gap_above = 3.0;
    let gap_below = 3.0;
    // Numerator sits above the bar; the bar is at the row baseline.
    let num_h = num_f.height();
    let den_h = den_f.height();
    let bar_y = num_h + gap_above;
    let ascent = bar_y + bar_thick * 0.5;
    let descent = bar_thick * 0.5 + gap_below + den_h;

    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    // numerator centered
    let num_x = (width - num_f.width) / 2.0;
    for mut b in num_f.boxes {
        b.x += num_x;
        b.y += 0.0;
        boxes.push(b);
    }
    for mut g in num_f.gaps {
        g.x += num_x;
        gaps.push(g);
    }
    // bar
    boxes.push(Box2d {
        x: bar_pad,
        y: bar_y - bar_thick * 0.5,
        w: width - bar_pad * 2.0,
        h: bar_thick,
        baseline: bar_thick,
        kind: BoxKind::FracBar,
    });
    // denominator centered, below the bar
    let den_x = (width - den_f.width) / 2.0;
    let den_dy = bar_y + bar_thick * 0.5 + gap_below;
    for mut b in den_f.boxes {
        b.x += den_x;
        b.y += den_dy;
        boxes.push(b);
    }
    for mut g in den_f.gaps {
        g.x += den_x;
        g.top += den_dy;
        g.bottom += den_dy;
        gaps.push(g);
    }
    NodeFrag {
        boxes,
        gaps,
        width,
        ascent,
        descent,
    }
}

fn layout_sqrt(
    index: Option<&Row>,
    radicand: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let rad_steps = child_steps(steps, idx, Slot::Radicand);
    let inner = layout_row(radicand, &rad_steps, tr, size);
    // The radical is drawn as a vector path (see render::SqrtSign), so
    // its width is a fixed fraction of the font size rather than the
    // font's `√` advance — keeps the nib proportions consistent.
    let sign_w = (f64::from(size) * 0.62).max(9.0);
    let over_pad = 4.0;
    let top_gap = 3.0;
    let ascent = inner.ascent + top_gap + 2.0;
    let descent = inner.descent;
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let mut pen = 0.0;

    // Optional index in the crook (small, raised).
    if let Some(idx_row) = index {
        let idx_size = (size * 0.6).max(MIN_SIZE);
        let idx_steps = child_steps(steps, idx, Slot::Index);
        let idxf = layout_row(idx_row, &idx_steps, tr, idx_size);
        for mut b in idxf.boxes {
            b.x += pen;
            b.y += 0.0;
            boxes.push(b);
        }
        for mut g in idxf.gaps {
            g.x += pen;
            gaps.push(g);
        }
        pen += idxf.width.max(f64::from(idx_size) * 0.5);
    }

    let radicand_w = inner.width + over_pad;
    boxes.push(Box2d {
        x: pen,
        y: 0.0,
        w: sign_w,
        h: ascent + descent,
        baseline: ascent,
        kind: BoxKind::SqrtSign {
            over_w: radicand_w,
            size,
        },
    });
    pen += sign_w;
    let dy = ascent - inner.ascent;
    for mut b in inner.boxes {
        b.x += pen;
        b.y += dy;
        boxes.push(b);
    }
    for mut g in inner.gaps {
        g.x += pen;
        g.top += dy;
        g.bottom += dy;
        gaps.push(g);
    }
    pen += inner.width + over_pad;
    NodeFrag {
        boxes,
        gaps,
        width: pen,
        ascent,
        descent,
    }
}

fn layout_delimited(
    body: &Row,
    open: Option<char>,
    close: Option<char>,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
    legacy_parens: bool,
) -> NodeFrag {
    let (gascent, gdescent) = metrics(size);
    let body_steps = child_steps(steps, idx, Slot::Body);
    let inner = layout_row(body, &body_steps, tr, size);
    let padding = delimiter_padding(size);
    let row_ascent = gascent.max(inner.ascent + padding);
    let row_descent = gdescent.max(inner.descent + padding);
    let body_h = row_ascent + row_descent;
    let open_width = open.map_or(0.0, |ch| delimiter_width_for(ch, size));
    let close_width = close.map_or(0.0, |ch| delimiter_width_for(ch, size));
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let mut pen = 0.0;
    if let Some(ch) = open {
        boxes.push(Box2d {
            x: pen,
            y: 0.0,
            w: open_width,
            h: body_h,
            baseline: row_ascent,
            kind: if legacy_parens {
                BoxKind::Paren { open: true, size }
            } else {
                BoxKind::Delimiter { ch, size }
            },
        });
        pen += open_width;
    }
    let dy = row_ascent - inner.ascent;
    for mut math_box in inner.boxes {
        math_box.x += pen;
        math_box.y += dy;
        boxes.push(math_box);
    }
    for mut gap in inner.gaps {
        gap.x += pen;
        gap.top += dy;
        gap.bottom += dy;
        gaps.push(gap);
    }
    pen += inner.width;
    if let Some(ch) = close {
        boxes.push(Box2d {
            x: pen,
            y: 0.0,
            w: close_width,
            h: body_h,
            baseline: row_ascent,
            kind: if legacy_parens {
                BoxKind::Paren { open: false, size }
            } else {
                BoxKind::Delimiter { ch, size }
            },
        });
        pen += close_width;
    }
    NodeFrag {
        boxes,
        gaps,
        width: pen,
        ascent: row_ascent,
        descent: row_descent,
    }
}

fn child_steps(steps: &[(usize, Slot)], idx: usize, slot: Slot) -> Vec<(usize, Slot)> {
    let mut s = steps.to_vec();
    s.push((idx, slot));
    s
}

/// Find the gap nearest a click point `(px, py)` (content-local). Picks the
/// row whose vertical band contains `py` if any, else the globally nearest
/// gap by Euclidean distance to the gap's vertical extent.
pub fn nearest_gap(gaps: &[Gap], px: f64, py: f64) -> Option<&Gap> {
    let mut best: Option<(&Gap, f64)> = None;
    for g in gaps {
        let dy = if py < g.top {
            g.top - py
        } else if py > g.bottom {
            py - g.bottom
        } else {
            0.0
        };
        let dx = (g.x - px).abs();
        // Strongly prefer gaps whose band contains py, then nearest x.
        let cost = dy * 4.0 + dx;
        match best {
            Some((_, c)) if c <= cost => {}
            _ => best = Some((g, cost)),
        }
    }
    best.map(|(g, _)| g)
}

#[cfg(test)]
mod layout_tests {
    use super::super::{model::parse, tex::parse_tex};
    use super::*;

    fn box_size(kind: &BoxKind) -> Option<f32> {
        match kind {
            BoxKind::Name { size, .. }
            | BoxKind::Glyph { size, .. }
            | BoxKind::Paren { size, .. }
            | BoxKind::Delimiter { size, .. }
            | BoxKind::Accent { size, .. }
            | BoxKind::SqrtSign { size, .. } => Some(*size),
            BoxKind::FracBar => None,
        }
    }

    #[test]
    fn functions_operators_scripts_and_delimiters_get_math_spacing_and_baselines() {
        // phase: func_name_and_argument_share_size_and_baseline
        // The 3D default surface `sin(√(x²+y²))` used to render its `sin` at the
        // frag top — floating above the argument and reading as an oversized,
        // misaligned glyph. The name and its parenthesised argument must be the
        // same font size AND share the row baseline.
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let expr = parse("sin(sqrt(x^2+y^2))");
            let lay = layout(&expr.root, &mut tr, 20.0);

            let name = lay
                .boxes
                .iter()
                .find(|b| matches!(b.kind, BoxKind::Name { .. }))
                .expect("a function-name box");
            let paren = lay
                .boxes
                .iter()
                .find(|b| matches!(b.kind, BoxKind::Paren { .. }))
                .expect("the call's opening paren box");

            assert_eq!(
                box_size(&name.kind),
                box_size(&paren.kind),
                "the function name must be the SAME type size as its argument's parens"
            );
            let name_baseline = name.y + name.baseline;
            let paren_baseline = paren.y + paren.baseline;
            assert!(
                (name_baseline - paren_baseline).abs() < 0.6,
                "the function name must sit on the row baseline, not float above it \
             (name {name_baseline:.2} vs paren {paren_baseline:.2})"
            );
        }
        // phase: binary_and_relation_operators_receive_math_spacing
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let compact = parse("xy");
            let binary = parse("x+y");
            let relation = parse("x=y");
            let compact_layout = layout(&compact.root, &mut tr, 20.0);
            let binary_layout = layout(&binary.root, &mut tr, 20.0);
            let relation_layout = layout(&relation.root, &mut tr, 20.0);
            let plus_glyph = measure_glyph(&mut tr, '+', 20.0);
            let equals_glyph = measure_glyph(&mut tr, '=', 20.0);
            assert!(binary_layout.width > compact_layout.width + plus_glyph);
            assert!(relation_layout.width > compact_layout.width + equals_glyph);
            assert!(
                relation_layout.width - compact_layout.width - equals_glyph
                    > binary_layout.width - compact_layout.width - plus_glyph,
                "relations receive wider TeX-style spacing than binary operators"
            );
        }
        // phase: paired_scripts_share_a_column_above_and_below_the_baseline
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let row = vec![
                Node::Char('∑'),
                Node::SubSup {
                    sub: vec![Node::Char('i')],
                    sup: vec![Node::Char('n')],
                },
            ];
            let laid = layout(&row, &mut tr, 24.0);
            assert!(laid.height > 24.0 * 1.25);
            let script_gaps: Vec<_> = laid
                .gaps
                .iter()
                .filter(|gap| !gap.steps.is_empty())
                .collect();
            assert!(script_gaps.iter().any(|gap| gap.top < laid.baseline));
            assert!(script_gaps.iter().any(|gap| gap.bottom > laid.baseline));
        }
        // phase: tex_delimiters_enclose_tall_content_and_accents_fit_their_base
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let parsed = parse_tex(r"\left[\frac{\hat{x}}{y}\right]");
            let laid = layout(&parsed.expr.root, &mut tr, 22.0);
            let delimiters: Vec<_> = laid
                .boxes
                .iter()
                .filter(|math_box| matches!(math_box.kind, BoxKind::Delimiter { .. }))
                .collect();
            assert_eq!(delimiters.len(), 2);
            assert!((delimiters[0].y - delimiters[1].y).abs() < 0.01);
            assert!((delimiters[0].h - delimiters[1].h).abs() < 0.01);

            let accent = laid
                .boxes
                .iter()
                .find(|math_box| matches!(math_box.kind, BoxKind::Accent { .. }))
                .expect("hat geometry");
            let base = laid
                .boxes
                .iter()
                .filter(|math_box| matches!(math_box.kind, BoxKind::Glyph { .. }))
                .filter(|math_box| {
                    math_box.y >= accent.y + accent.h
                        && math_box.x < accent.x + accent.w
                        && math_box.x + math_box.w > accent.x
                })
                .min_by(|left, right| left.y.total_cmp(&right.y))
                .expect("accented base glyph");
            assert!(
                accent.y + accent.h <= base.y,
                "the hat stays above its base"
            );
            assert!(accent.x <= base.x + base.w * 0.25);
            assert!(accent.x + accent.w >= base.x + base.w * 0.75);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn matrix_cells_align_to_shared_rows_columns_and_baselines() {
        // phase: matrix_cells_align_to_shared_rows_and_columns
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let row = vec![Node::Paren {
                body: vec![Node::Table {
                    rows: vec![
                        vec![vec![Node::Char('1')], vec![Node::Char('x')]],
                        vec![
                            vec![Node::Char('y')],
                            vec![Node::Frac {
                                num: vec![Node::Char('1')],
                                den: vec![Node::Char('2')],
                            }],
                        ],
                    ],
                }],
            }];
            let laid = layout(&row, &mut tr, 22.0);
            assert!(laid.height > 22.0 * 2.0);
            assert!(laid.width > 22.0 * 3.0);
            assert!(
                laid.boxes
                    .iter()
                    .any(|math_box| matches!(math_box.kind, BoxKind::FracBar))
            );
            let cells = laid
                .gaps
                .iter()
                .filter(|gap| {
                    gap.steps
                        .iter()
                        .any(|(_, slot)| matches!(slot, Slot::Cell(_, _)))
                })
                .count();
            assert!(cells >= 8, "each matrix cell exposes insertion geometry");

            let delimiters: Vec<_> = laid
                .boxes
                .iter()
                .filter(|math_box| matches!(math_box.kind, BoxKind::Paren { .. }))
                .collect();
            assert_eq!(delimiters.len(), 2);
            assert!((delimiters[0].y - delimiters[1].y).abs() < 0.01);
            assert!((delimiters[0].h - delimiters[1].h).abs() < 0.01);

            let content_top = laid
                .boxes
                .iter()
                .filter(|math_box| !matches!(math_box.kind, BoxKind::Paren { .. }))
                .map(|math_box| math_box.y)
                .fold(f64::INFINITY, f64::min);
            let content_bottom = laid
                .boxes
                .iter()
                .filter(|math_box| !matches!(math_box.kind, BoxKind::Paren { .. }))
                .map(|math_box| math_box.y + math_box.h)
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(delimiters[0].y < content_top);
            assert!(delimiters[0].y + delimiters[0].h > content_bottom);
        }
        // phase: matrix_rows_share_baselines_and_remain_in_source_order
        {
            let mut tr = crate::font_catalog::fixture_renderer();
            let row = vec![Node::Paren {
                body: vec![Node::Table {
                    rows: vec![
                        vec![vec![Node::Char('a')], vec![Node::Char('b')]],
                        vec![vec![Node::Char('c')], vec![Node::Char('d')]],
                        vec![vec![Node::Char('e')], vec![Node::Char('f')]],
                    ],
                }],
            }];
            let laid = layout(&row, &mut tr, 22.0);
            let baselines: Vec<_> = laid
                .boxes
                .iter()
                .filter_map(|math_box| match math_box.kind {
                    BoxKind::Glyph { ch, .. } if ch.is_alphabetic() => {
                        Some(math_box.y + math_box.baseline)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(baselines.len(), 6);
            for pair in baselines.chunks_exact(2) {
                assert!((pair[0] - pair[1]).abs() < 0.01);
            }
            assert!(baselines[0] < baselines[2]);
            assert!(baselines[2] < baselines[4]);

            let parens: Vec<_> = laid
                .boxes
                .iter()
                .filter(|math_box| matches!(math_box.kind, BoxKind::Paren { .. }))
                .collect();
            assert_eq!(parens.len(), 2);
            let delimiter_center = parens[0].y + parens[0].h * 0.5;
            let matrix_top = laid
                .boxes
                .iter()
                .filter(|math_box| !matches!(math_box.kind, BoxKind::Paren { .. }))
                .map(|math_box| math_box.y)
                .fold(f64::INFINITY, f64::min);
            let matrix_bottom = laid
                .boxes
                .iter()
                .filter(|math_box| !matches!(math_box.kind, BoxKind::Paren { .. }))
                .map(|math_box| math_box.y + math_box.h)
                .fold(f64::NEG_INFINITY, f64::max);
            let matrix_center = (matrix_top + matrix_bottom) * 0.5;
            assert!(
                (delimiter_center - matrix_center).abs() < 22.0 * 0.2,
                "delimiter and matrix must share an optical centre"
            );
        }
    }

    #[test]
    fn a_long_expression_wraps_before_its_top_level_operators() {
        let mut tr = TextRenderer::new();
        let row: Row = "a+b+c+d+e+f+g+h=x".chars().map(Node::Char).collect();
        let whole = layout(&row, &mut tr, 20.0);
        let wrapped = layout_wrapped(&row, &mut tr, 20.0, whole.width / 2.0);
        assert!(wrapped.height > whole.height * 1.5, "it takes more lines");
        assert!(wrapped.width <= whole.width / 2.0 + 20.0);
        // Every top-level caret stop survives exactly once.
        for index in 0..=row.len() {
            assert_eq!(
                wrapped
                    .gaps
                    .iter()
                    .filter(|g| g.steps.is_empty() && g.index == index)
                    .count(),
                1,
                "gap {index}"
            );
        }
        // A sign is never split from its operand.
        let signed: Row = "a=-b".chars().map(Node::Char).collect();
        assert!(!breaks_before(&signed, 2));
        assert!(breaks_before(&signed, 1));
    }

    #[test]
    fn glyphs_are_measured_at_the_content_size_whatever_the_text_scale() {
        let mut plain = TextRenderer::new();
        let mut scaled = TextRenderer::new();
        scaled.set_text_scale(2.0);
        assert!(
            (measure_glyph(&mut plain, 'x', 20.0) - measure_glyph(&mut scaled, 'x', 20.0)).abs()
                < 0.01,
            "the renderer's text scale does not grow math glyphs past their geometry"
        );
    }
}
