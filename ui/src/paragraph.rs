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
}

impl ParagraphBox {
    pub const fn word(width: f32, glue_before: f32) -> Self {
        Self {
            width,
            glue_before,
            break_before: true,
            break_penalty: 0,
            flagged_break: false,
            break_width: 0.0,
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
) -> Option<(f32, f32, u8)> {
    let mut natural = 0.0;
    let mut glue = 0.0;
    for (offset, item) in boxes[start..end].iter().enumerate() {
        natural += item.width.max(0.0);
        if offset != 0 {
            let amount = item.glue_before.max(0.0);
            natural += amount;
            glue += amount;
        }
    }
    if end < boxes.len() {
        natural += boxes[end].break_width.max(0.0);
    }

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
        return Some((natural, ragged_ratio * 0.35, 1));
    }

    let delta = options.line_width - natural;
    let capacity = if delta >= 0.0 {
        glue * options.space_stretch.max(0.0)
    } else {
        glue * options.space_shrink.max(0.0)
    };
    if capacity <= f32::EPSILON {
        if end < boxes.len() && boxes[end].flagged_break && natural <= options.line_width {
            let ratio = ((options.line_width - natural) / options.line_width.max(1.0)) * 3.0;
            if ratio <= options.tolerance.max(0.0) {
                return Some((natural, ratio, fitness_class(ratio)));
            }
        }
        // A non-final line with no adjustable glue is only unavoidable when a
        // single unbreakable box itself fills or overruns the measure. Treating
        // a short one-word row as perfect would make every word its own line.
        return (end == start + 1 && natural >= options.line_width).then_some((natural, 0.0, 1));
    }
    let ratio = delta / capacity;
    let minimum_ratio = if options.tolerance >= 100.0 {
        -4.0
    } else {
        -1.0
    };
    if ratio < minimum_ratio || ratio > options.tolerance.max(0.0) {
        return None;
    }
    Some((natural, ratio, fitness_class(ratio)))
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
                let Some((natural, ratio, fitness)) =
                    line_metrics(boxes, start, end, options, final_line)
                else {
                    continue;
                };
                let badness = if final_line {
                    // A modest ragged-last-line cost avoids absurdly short endings
                    // without forcing the last row to justify.
                    f64::from(ratio * 100.0)
                } else {
                    (100.0 * f64::from(ratio.abs()).powi(3)).min(10_000.0)
                };
                let boundary = if end < count { Some(boxes[end]) } else { None };
                let penalty = boundary.map_or(0.0, |item| f64::from(item.break_penalty));
                let flagged = boundary.is_some_and(|item| item.flagged_break);
                let mut demerits = previous.demerits
                    + (options.line_penalty + badness).powi(2)
                    + penalty.signum() * penalty.powi(2);
                if previous.fitness.abs_diff(fitness) > 1 {
                    demerits += options.fitness_demerit;
                }
                if previous.flagged && flagged {
                    demerits += options.flagged_demerit;
                }
                let slot = state_slot(fitness, flagged);
                if best[end][slot].is_none_or(|current| demerits < current.demerits) {
                    best[end][slot] = Some(State {
                        demerits,
                        previous: start,
                        previous_slot,
                        fitness,
                        flagged,
                        ratio,
                        natural_width: natural,
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
    fn fallback_keeps_a_forbidden_break_group_together() {
        let mut options = ParagraphOptions::new(1.0);
        options.space_shrink = 0.0;
        options.space_stretch = 0.0;
        let boxes = vec![
            ParagraphBox::word(10.0, 0.0),
            ParagraphBox {
                width: 5.0,
                glue_before: 0.0,
                break_before: false,
                break_penalty: 0,
                flagged_break: false,
                break_width: 0.0,
            },
            ParagraphBox::word(10.0, 2.0),
        ];
        let lines = compose_paragraph(&boxes, options);
        assert_eq!((lines[0].start, lines[0].end), (0, 2));
    }
}
