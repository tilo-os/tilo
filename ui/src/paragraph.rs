//! Paragraph-wide line composition.
//!
//! Unlike greedy wrapping, this module considers every legal line ending in a
//! paragraph together. The cost model follows Knuth--Plass: spaces carry
//! stretch and shrink, visually incompatible adjacent lines are discouraged,
//! and flagged breaks (normally hyphens) acquire an additional penalty.

/// One shaped box in logical order. A box is usually a word or an unbreakable
/// inline fragment; callers retain the actual glyph/style data separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParagraphBox {
    /// Natural shaped advance of this fragment.
    pub width: f32,
    /// Natural glue before this fragment. It is ignored when the fragment is
    /// first on a line.
    pub glue_before: f32,
    /// Additional stretch capacity at this boundary. Zero derives capacity
    /// from natural glue; a positive value supports script-specific expansion
    /// such as spacing between CJK ideographs.
    pub stretch_before: f32,
    /// Additional shrink capacity at this boundary. Zero derives capacity
    /// from natural glue.
    pub shrink_before: f32,
    /// Whether a line may end immediately before this fragment.
    pub break_before: bool,
    /// Extra demerit for breaking before this fragment. Positive values make
    /// the break less desirable; a negative value encourages it.
    pub break_penalty: i32,
    /// Whether this is a flagged break such as a discretionary hyphen.
    pub flagged_break: bool,
    /// Width added only when the line ends at this boundary (for example the
    /// visible hyphen exercised by a discretionary break).
    pub break_width: f32,
    /// Portion of this box allowed to protrude beyond the right margin when
    /// it ends a line. Typically a fraction of a closing punctuation glyph.
    pub end_protrusion: f32,
}

impl ParagraphBox {
    pub const fn word(width: f32, glue_before: f32) -> Self {
        Self {
            width,
            glue_before,
            stretch_before: 0.0,
            shrink_before: 0.0,
            break_before: true,
            break_penalty: 0,
            flagged_break: false,
            break_width: 0.0,
            end_protrusion: 0.0,
        }
    }
}

/// Tunable paragraph-composition policy. Defaults are deliberately close to
/// TeX's ordinary prose behavior while remaining expressed in font-relative
/// space capacities supplied by the caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParagraphOptions {
    pub line_width: f32,
    /// Maximum normalized stretch accepted for an ordinary line.
    pub tolerance: f32,
    /// Fraction of each natural glue that may stretch.
    pub space_stretch: f32,
    /// Fraction of each natural glue that may shrink.
    pub space_shrink: f32,
    pub line_penalty: f64,
    pub fitness_demerit: f64,
    pub flagged_demerit: f64,
    /// Maximum horizontal glyph expansion/contraction used to improve colour
    /// without making individual spaces conspicuously loose or tight.
    pub max_glyph_expansion: f32,
    /// Cost of using the full configured expansion. Larger values prefer
    /// inter-word glue; smaller values prefer micro-expansion.
    pub expansion_demerit: f64,
    /// Justify non-final lines. When false, the same global break search is
    /// used but output glue remains natural.
    pub justify: bool,
}

impl ParagraphOptions {
    pub fn new(line_width: f32) -> Self {
        Self {
            line_width,
            tolerance: 3.0,
            space_stretch: 0.5,
            space_shrink: 0.333,
            line_penalty: 10.0,
            fitness_demerit: 3_000.0,
            flagged_demerit: 10_000.0,
            max_glyph_expansion: 0.02,
            expansion_demerit: 40.0,
            justify: true,
        }
    }
}

/// One chosen line, indexing a half-open range of the input boxes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComposedLine {
    pub start: usize,
    pub end: usize,
    /// Multiplier applied to every natural glue on this line.
    pub glue_scale: f32,
    /// Horizontal scale applied to glyph outlines and advances on this line.
    /// Values stay very close to 1.0 and are never used on the final line.
    pub glyph_scale: f32,
    pub natural_width: f32,
    /// Normalized stretch (>0) or shrink (<0) used by the cost model.
    pub adjustment_ratio: f32,
}

#[derive(Clone, Copy, Debug)]
struct State {
    demerits: f64,
    previous: usize,
    previous_slot: usize,
    fitness: u8,
    flagged: bool,
    ratio: f32,
    natural_width: f32,
    glyph_scale: f32,
}

#[derive(Clone, Copy, Debug)]
struct LineMetrics {
    natural: f32,
    ratio: f32,
    fitness: u8,
    glyph_scale: f32,
    badness: f64,
}

const FITNESS_CLASSES: usize = 4;
const FLAG_STATES: usize = 2;
const STATE_SLOTS: usize = FITNESS_CLASSES * FLAG_STATES;

const fn state_slot(fitness: u8, flagged: bool) -> usize {
    fitness as usize * FLAG_STATES + flagged as usize
}

fn fitness_class(ratio: f32) -> u8 {
    if ratio < -0.5 {
        0
    } else if ratio <= 0.5 {
        1
    } else if ratio <= 1.0 {
        2
    } else {
        3
    }
}

fn line_metrics(
    boxes: &[ParagraphBox],
    start: usize,
    end: usize,
    options: ParagraphOptions,
    final_line: bool,
) -> Option<LineMetrics> {
    let mut natural = 0.0;
    let mut stretch = 0.0;
    let mut shrink = 0.0;
    let mut glyph_width = 0.0;
    for (offset, item) in boxes[start..end].iter().enumerate() {
        let width = item.width.max(0.0);
        natural += width;
        glyph_width += width;
        if offset != 0 {
            let amount = item.glue_before.max(0.0);
            natural += amount;
            stretch += if item.stretch_before > 0.0 {
                item.stretch_before
            } else {
                amount * options.space_stretch.max(0.0)
            };
            shrink += if item.shrink_before > 0.0 {
                item.shrink_before
            } else {
                amount * options.space_shrink.max(0.0)
            };
        }
    }
    if end < boxes.len() {
        let break_width = boxes[end].break_width.max(0.0);
        natural += break_width;
        glyph_width += break_width;
    }
    let protrusion = boxes[end - 1].end_protrusion.max(0.0).min(glyph_width);
    natural -= protrusion;
    glyph_width -= protrusion;

    // TeX's paragraph fill glue makes the final line effectively ragged.
    if final_line || !options.justify {
        if natural > options.line_width && end != start + 1 {
            return None;
        }
        let ragged_ratio = if options.line_width > 0.0 {
            ((options.line_width - natural) / options.line_width).max(0.0)
        } else {
            0.0
        };
        let ratio = ragged_ratio * 0.35;
        return Some(LineMetrics {
            natural,
            ratio,
            fitness: 1,
            glyph_scale: 1.0,
            badness: f64::from(ratio * 100.0),
        });
    }

    let delta = options.line_width - natural;
    let capacity = if delta >= 0.0 { stretch } else { shrink };
    if capacity <= f32::EPSILON {
        if end < boxes.len() && boxes[end].break_before && natural <= options.line_width {
            let ratio = ((options.line_width - natural) / options.line_width.max(1.0)) * 3.0;
            if ratio <= options.tolerance.max(0.0) {
                return Some(LineMetrics {
                    natural,
                    ratio,
                    fitness: fitness_class(ratio),
                    glyph_scale: 1.0,
                    badness: (100.0 * f64::from(ratio.abs()).powi(3)).min(10_000.0),
                });
            }
        }
        // A non-final line with no adjustable glue is only unavoidable when a
        // single unbreakable box itself fills or overruns the measure. Treating
        // a short one-word row as perfect would make every word its own line.
        return (end == start + 1 && natural >= options.line_width).then_some(LineMetrics {
            natural,
            ratio: 0.0,
            fitness: 1,
            glyph_scale: 1.0,
            badness: 10_000.0,
        });
    }
    let minimum_ratio = if options.tolerance >= 100.0 {
        -4.0
    } else {
        -1.0
    };
    let max_expansion = options.max_glyph_expansion.max(0.0);
    let mut best_metrics: Option<LineMetrics> = None;
    for step in -8..=8 {
        let expansion = max_expansion * step as f32 / 8.0;
        let adjusted_delta = delta - glyph_width * expansion;
        let adjusted_capacity = if adjusted_delta >= 0.0 {
            stretch
        } else {
            shrink
        };
        if adjusted_capacity <= f32::EPSILON {
            continue;
        }
        let ratio = adjusted_delta / adjusted_capacity;
        if ratio < minimum_ratio || ratio > options.tolerance.max(0.0) {
            continue;
        }
        let space_badness = (100.0 * f64::from(ratio.abs()).powi(3)).min(10_000.0);
        let expansion_fraction = if max_expansion <= f32::EPSILON {
            0.0
        } else {
            f64::from((expansion / max_expansion).abs())
        };
        let badness = space_badness + options.expansion_demerit * expansion_fraction.powi(2);
        let candidate = LineMetrics {
            natural,
            ratio,
            fitness: fitness_class(ratio),
            glyph_scale: 1.0 + expansion,
            badness,
        };
        if best_metrics.is_none_or(|current| candidate.badness < current.badness) {
            best_metrics = Some(candidate);
        }
    }
    best_metrics
}

fn compose_at_tolerance(
    boxes: &[ParagraphBox],
    mut options: ParagraphOptions,
    tolerance: f32,
) -> Option<Vec<ComposedLine>> {
    options.tolerance = tolerance;
    let count = boxes.len();
    // Keep a candidate for every fitness/flag combination at each boundary.
    // A locally more expensive route can be globally preferable when it avoids
    // an abrupt loose-to-tight transition or consecutive hyphenated lines.
    let mut best = vec![[None; STATE_SLOTS]; count + 1];
    let initial_slot = state_slot(1, false);
    best[0][initial_slot] = Some(State {
        demerits: 0.0,
        previous: 0,
        previous_slot: initial_slot,
        fitness: 1,
        flagged: false,
        ratio: 0.0,
        natural_width: 0.0,
        glyph_scale: 1.0,
    });

    for end in 1..=count {
        if end != count && !boxes[end].break_before {
            continue;
        }
        for start in 0..end {
            let previous_states = best[start];
            for (previous_slot, previous) in previous_states.into_iter().enumerate() {
                let Some(previous) = previous else {
                    continue;
                };
                let final_line = end == count;
                let Some(metrics) = line_metrics(boxes, start, end, options, final_line) else {
                    continue;
                };
                let badness = metrics.badness;
                let boundary = if end < count { Some(boxes[end]) } else { None };
                let penalty = boundary.map_or(0.0, |item| f64::from(item.break_penalty));
                let flagged = boundary.is_some_and(|item| item.flagged_break);
                let mut demerits = previous.demerits
                    + (options.line_penalty + badness).powi(2)
                    + penalty.signum() * penalty.powi(2);
                if previous.fitness.abs_diff(metrics.fitness) > 1 {
                    demerits += options.fitness_demerit;
                }
                if previous.flagged && flagged {
                    demerits += options.flagged_demerit;
                }
                let slot = state_slot(metrics.fitness, flagged);
                if best[end][slot].is_none_or(|current| demerits < current.demerits) {
                    best[end][slot] = Some(State {
                        demerits,
                        previous: start,
                        previous_slot,
                        fitness: metrics.fitness,
                        flagged,
                        ratio: metrics.ratio,
                        natural_width: metrics.natural,
                        glyph_scale: metrics.glyph_scale,
                    });
                }
            }
        }
    }

    let (mut slot, _) = best[count]
        .iter()
        .enumerate()
        .filter_map(|(slot, state)| state.map(|state| (slot, state)))
        .min_by(|(_, a), (_, b)| a.demerits.total_cmp(&b.demerits))?;
    let mut lines = Vec::new();
    let mut end = count;
    while end > 0 {
        let state = best[end][slot]?;
        let final_line = end == count;
        let glue_scale = if final_line || !options.justify {
            1.0
        } else if state.ratio >= 0.0 {
            1.0 + state.ratio * options.space_stretch
        } else {
            (1.0 + state.ratio * options.space_shrink).max(0.2)
        };
        lines.push(ComposedLine {
            start: state.previous,
            end,
            glue_scale,
            glyph_scale: if final_line || !options.justify {
                1.0
            } else {
                state.glyph_scale
            },
            natural_width: state.natural_width,
            adjustment_ratio: state.ratio,
        });
        end = state.previous;
        slot = state.previous_slot;
    }
    lines.reverse();
    Some(lines)
}

/// Compose a paragraph globally. If the requested tolerance has no feasible
/// solution, progressively looser emergency passes are tried; the final
/// fallback preserves every box and always makes forward progress.
pub fn compose_paragraph(boxes: &[ParagraphBox], options: ParagraphOptions) -> Vec<ComposedLine> {
    if boxes.is_empty() {
        return Vec::new();
    }
    for tolerance in [options.tolerance, 6.0, 12.0, 100.0] {
        if let Some(lines) = compose_at_tolerance(boxes, options, tolerance) {
            return lines;
        }
    }
    // Last-resort greedy packing still honors forbidden break boundaries. It
    // is preferable to an overfull unbreakable group beginning a line with
    // detached punctuation or an inline style fragment.
    let mut lines = Vec::new();
    let mut start = 0;
    while start < boxes.len() {
        let mut end = start + 1;
        let mut natural = boxes[start].width.max(0.0);
        while end < boxes.len() {
            let candidate = natural + boxes[end].glue_before.max(0.0) + boxes[end].width.max(0.0);
            if boxes[end].break_before && candidate > options.line_width && end > start {
                break;
            }
            natural = candidate;
            end += 1;
        }
        lines.push(ComposedLine {
            start,
            end,
            glue_scale: 1.0,
            glyph_scale: 1.0,
            natural_width: natural,
            adjustment_ratio: 0.0,
        });
        start = end;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_composition_avoids_a_tiny_last_line() {
        let widths = [46.0, 42.0, 45.0, 50.0, 36.0, 47.0, 41.0, 44.0];
        let boxes: Vec<_> = widths
            .into_iter()
            .enumerate()
            .map(|(index, width)| ParagraphBox::word(width, if index == 0 { 0.0 } else { 10.0 }))
            .collect();
        let lines = compose_paragraph(&boxes, ParagraphOptions::new(150.0));
        assert_eq!(lines.len(), 3);
        assert!(
            lines.last().unwrap().natural_width > 90.0,
            "the optimizer should rebalance earlier lines: {lines:?}"
        );
    }

    #[test]
    fn non_breaking_inline_fragments_stay_with_the_previous_box() {
        let mut boxes = vec![
            ParagraphBox::word(70.0, 0.0),
            ParagraphBox::word(65.0, 10.0),
            ParagraphBox::word(12.0, 0.0),
            ParagraphBox::word(60.0, 10.0),
        ];
        boxes[2].break_before = false;
        let lines = compose_paragraph(&boxes, ParagraphOptions::new(150.0));
        assert!(
            lines.iter().all(|line| line.start != 2),
            "punctuation-like fragments cannot begin a line"
        );
    }

    #[test]
    fn justified_lines_fill_the_measure_but_the_last_line_stays_natural() {
        let boxes: Vec<_> = (0..8)
            .map(|index| ParagraphBox::word(40.0, if index == 0 { 0.0 } else { 10.0 }))
            .collect();
        let lines = compose_paragraph(&boxes, ParagraphOptions::new(150.0));
        assert!(lines.len() >= 3);
        assert!(
            lines[..lines.len() - 1]
                .iter()
                .all(|line| line.glue_scale != 1.0)
        );
        assert_eq!(lines.last().unwrap().glue_scale, 1.0);
    }

    #[test]
    fn emergency_composition_never_produces_negative_glue() {
        let boxes = vec![
            ParagraphBox::word(95.0, 0.0),
            ParagraphBox::word(95.0, 10.0),
            ParagraphBox::word(95.0, 10.0),
        ];
        let lines = compose_paragraph(&boxes, ParagraphOptions::new(120.0));
        assert!(lines.iter().all(|line| line.glue_scale >= 0.2));
    }

    #[test]
    fn micro_expansion_shares_adjustment_with_interword_glue() {
        let boxes: Vec<_> = (0..6)
            .map(|index| ParagraphBox::word(80.0, if index == 0 { 0.0 } else { 10.0 }))
            .collect();
        let lines = compose_paragraph(&boxes, ParagraphOptions::new(270.0));
        let first = lines.first().unwrap();
        assert!((0.98..=1.02).contains(&first.glyph_scale));
        assert_ne!(first.glyph_scale, 1.0, "micro-expansion should participate");
        let visual_width = 240.0 * first.glyph_scale + 20.0 * first.glue_scale;
        assert!((visual_width - 270.0).abs() < 0.1, "line fills exactly");
        assert_eq!(lines.last().unwrap().glyph_scale, 1.0);
    }

    #[test]
    fn fallback_keeps_a_forbidden_break_group_together() {
        let mut options = ParagraphOptions::new(1.0);
        options.space_shrink = 0.0;
        options.space_stretch = 0.0;
        let boxes = vec![
            ParagraphBox::word(10.0, 0.0),
            ParagraphBox {
                width: 5.0,
                glue_before: 0.0,
                stretch_before: 0.0,
                shrink_before: 0.0,
                break_before: false,
                break_penalty: 0,
                flagged_break: false,
                break_width: 0.0,
                end_protrusion: 0.0,
            },
            ParagraphBox::word(10.0, 2.0),
        ];
        let lines = compose_paragraph(&boxes, options);
        assert_eq!((lines[0].start, lines[0].end), (0, 2));
    }

    #[test]
    fn closing_punctuation_can_protrude_past_the_measure() {
        let mut boxes: Vec<_> = (0..6)
            .map(|index| ParagraphBox::word(80.0, if index == 0 { 0.0 } else { 10.0 }))
            .collect();
        boxes[2].end_protrusion = 10.0;
        let mut options = ParagraphOptions::new(260.0);
        options.max_glyph_expansion = 0.0;
        let lines = compose_paragraph(&boxes, options);
        assert_eq!(lines[0].end, 3);
        assert_eq!(lines[0].natural_width, 250.0);
        assert_eq!(lines[0].glue_scale, 1.5);
    }
}
