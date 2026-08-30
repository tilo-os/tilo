//! Recursive 2D box layout for the math model.
//!
//! Lays the [`MathExpr`] tree out into a flat list of positioned
//! [`Box2d`]es with exact glyph metrics from [`TextRenderer`], plus the gap
//! geometry the caret and hit-testing need. Layout is pure: it reads the
//! model + a renderer (for measuring) and produces positions; it never
//! mutates the model.
//!
//! Coordinates are local to the field's content origin `(0, 0)`; the caller
//! translates by the field's on-screen rect.

use ui::text_render::{TextRenderer, TextWeight};

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
    /// A parenthesis glyph stretched to `height`.
    Paren { open: bool, size: f32 },
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

fn measure_glyph(tr: &mut TextRenderer, ch: char, size: f32) -> f64 {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    let (w, _) = tr.measure_weighted(s, size, None, TextWeight::Medium);
    f64::from(w)
}

fn measure_name(tr: &mut TextRenderer, name: &str, size: f32) -> f64 {
    let (w, _) = tr.measure_weighted(name, size, None, TextWeight::Medium);
    f64::from(w)
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
            gaps.push(g);
        }
        pen += nf.width;
        // Trailing gap after node `i` (offset i+1). `i` indexes a live row,
        // so the successor offset always fits.
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

/// Dispatch one node to the helper that knows its shape. Each helper returns
/// a fragment positioned from its own top-left with its baseline at `ascent`.
fn layout_node(
    node: &Node,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    match node {
        Node::Char(ch) => layout_char(*ch, tr, size),
        Node::Func { name, arg } => layout_call(name, arg, idx, steps, tr, size),
        Node::Paren { body } => layout_group(body, idx, steps, tr, size),
        Node::Sup { exp } => layout_superscript(exp, idx, steps, tr, size),
        Node::Frac { num, den } => layout_fraction(num, den, idx, steps, tr, size),
        Node::Sqrt { index, radicand } => {
            layout_radical(index.as_ref(), radicand, idx, steps, tr, size)
        }
    }
}

/// A single glyph: one box the width of its advance.
fn layout_char(ch: char, tr: &mut TextRenderer, size: f32) -> NodeFrag {
    let w = measure_glyph(tr, ch, size);
    let (ascent, descent) = metrics(size);
    let b = Box2d {
        x: 0.0,
        y: 0.0,
        w,
        h: ascent + descent,
        baseline: ascent,
        kind: BoxKind::Glyph { ch, size },
    };
    NodeFrag {
        boxes: vec![b],
        gaps: vec![],
        width: w,
        ascent,
        descent,
    }
}

/// `name( arg )` — the upright name, then the argument between parens that
/// stretch to it. The name shares the argument's size and row baseline.
fn layout_call(
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
    let body_h = inner.height();
    let paren_size = size;
    let open = measure_glyph(tr, '(', paren_size);
    let close = measure_glyph(tr, ')', paren_size);
    let row_ascent = ascent.max(inner.ascent);
    let row_descent = descent.max(inner.descent);
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
            text: name.to_owned(),
            size,
        },
    }];
    let mut gaps = Vec::new();
    let mut pen = name_w;
    boxes.push(Box2d {
        x: pen,
        y: total_ascent - row_ascent,
        w: open,
        h: body_h.max(ascent + descent),
        baseline: row_ascent,
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
        y: total_ascent - row_ascent,
        w: close,
        h: body_h.max(ascent + descent),
        baseline: row_ascent,
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

/// A bare `( body )` group with parens stretched to the body's height.
fn layout_group(
    body: &Row,
    idx: usize,
    steps: &[(usize, Slot)],
    tr: &mut TextRenderer,
    size: f32,
) -> NodeFrag {
    let (gascent, gdescent) = metrics(size);
    let body_steps = child_steps(steps, idx, Slot::Body);
    let inner = layout_row(body, &body_steps, tr, size);
    let row_ascent = gascent.max(inner.ascent);
    let row_descent = gdescent.max(inner.descent);
    let body_h = (row_ascent + row_descent).max(line_h(size));
    let open = measure_glyph(tr, '(', size);
    let close = measure_glyph(tr, ')', size);
    let mut boxes = Vec::new();
    let mut gaps = Vec::new();
    let mut pen = 0.0;
    boxes.push(Box2d {
        x: pen,
        y: 0.0,
        w: open,
        h: body_h,
        baseline: row_ascent,
        kind: BoxKind::Paren { open: true, size },
    });
    pen += open;
    let dy = row_ascent - inner.ascent;
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
        h: body_h,
        baseline: row_ascent,
        kind: BoxKind::Paren { open: false, size },
    });
    pen += close;
    NodeFrag {
        boxes,
        gaps,
        width: pen,
        ascent: row_ascent,
        descent: row_descent,
    }
}

/// A superscript: the exponent row, smaller and raised above the baseline.
fn layout_superscript(
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

/// A fraction: numerator and denominator centered over/under the bar, which
/// sits on the row baseline.
fn layout_fraction(
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

/// A radical: an optional index in the crook, the `√` sign, then the radicand
/// under the overbar.
fn layout_radical(
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
    use super::*;
    use crate::mathfield::model::parse;

    fn box_size(kind: &BoxKind) -> Option<f32> {
        match kind {
            BoxKind::Name { size, .. }
            | BoxKind::Glyph { size, .. }
            | BoxKind::Paren { size, .. }
            | BoxKind::SqrtSign { size, .. } => Some(*size),
            BoxKind::FracBar => None,
        }
    }

    /// The 3D default surface `sin(√(x²+y²))` used to render its `sin` at the
    /// frag top — floating above the argument and reading as an oversized,
    /// misaligned glyph. The name and its parenthesised argument must be the
    /// same font size AND share the row baseline.
    #[test]
    fn func_name_and_argument_share_size_and_baseline() {
        let mut tr = TextRenderer::new();
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
}
