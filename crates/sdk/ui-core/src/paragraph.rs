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

/// Tolerance of the last composition pass, which must find some solution.
const EMERGENCY_TOLERANCE: f32 = 100.0;
const FITNESS_CLASSES: usize = 4;
const FLAG_STATES: usize = 2;
const STATE_SLOTS: usize = FITNESS_CLASSES * FLAG_STATES;

const fn state_slot(fitness: u8, flagged: bool) -> usize {
    // Bounded by construction: fitness is one of FITNESS_CLASSES (0..4) and
    // flagged contributes 0 or 1, so the product+sum never approaches
    // overflow; wrapping arithmetic keeps this a `const fn` without a panic
    // path that can never actually be taken.
    (fitness as usize)
        .wrapping_mul(FLAG_STATES)
        .wrapping_add(flagged as usize)
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

/// Accumulate natural width, stretch, shrink and glyph width for the boxes in
/// `start..end`, including the break-width contributed by the box that would
/// end the line and any protrusion of the last box's closing punctuation.
/// Returns `None` only if `start..end` is not a valid range into `boxes`,
/// which callers never construct — `compose_at_tolerance` always derives
/// `end` from `1..=boxes.len()` and `start` from `0..end`.
fn accumulate_line(
    boxes: &[ParagraphBox],
    start: usize,
    end: usize,
    options: ParagraphOptions,
) -> Option<(f32, f32, f32, f32)> {
    let mut natural = 0.0;
    let mut stretch = 0.0;
    let mut shrink = 0.0;
    let mut glyph_width = 0.0;
    let slice = boxes.get(start..end)?;
    for (offset, item) in slice.iter().enumerate() {
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
        let break_width = boxes.get(end)?.break_width.max(0.0);
        natural += break_width;
        glyph_width += break_width;
    }
    let last = end.checked_sub(1).and_then(|idx| boxes.get(idx))?;
    let protrusion = last.end_protrusion.max(0.0).min(glyph_width);
    natural -= protrusion;
    glyph_width -= protrusion;
    Some((natural, stretch, shrink, glyph_width))
}

/// Metrics for a final line (or any line when justification is off), which
/// TeX's paragraph fill glue makes effectively ragged: it is never stretched
/// or shrunk to the measure, only checked that it does not overrun it.
fn ragged_metrics(
    natural: f32,
    start: usize,
    end: usize,
    options: ParagraphOptions,
) -> Option<LineMetrics> {
    if natural > options.line_width && end != start.saturating_add(1) {
        return None;
    }
    let ragged_ratio = if options.line_width > 0.0 {
        ((options.line_width - natural) / options.line_width).max(0.0)
    } else {
        0.0
    };
    let ratio = ragged_ratio * 0.35;
    Some(LineMetrics {
        natural,
        ratio,
        fitness: 1,
        glyph_scale: 1.0,
        badness: f64::from(ratio * 100.0),
    })
}

/// Knuth--Plass badness `100·|r|³`. Unlike TeX it is not capped at 10 000:
/// once a paragraph needs a looser pass, a line stretched to ratio 9 must
/// still cost more than one stretched to ratio 5, or the composer trades a
/// barely-loose line for a gaping one to tighten its neighbour.
///
/// Shrinking past the glue's capacity (`r < -1`, emergency pass only) makes
/// the line wider than the measure, so it costs more than any line the
/// emergency pass could stretch instead.
fn stretch_badness(ratio: f32) -> f64 {
    let badness = 100.0 * f64::from(ratio.abs()).powi(3);
    if ratio < -1.0 {
        stretch_badness(EMERGENCY_TOLERANCE) + badness
    } else {
        badness
    }
}

/// Metrics when neither stretch nor shrink is available at this boundary.
/// A non-final line with no adjustable glue is only unavoidable when a
/// single unbreakable box itself fills or overruns the measure. Treating a
/// short one-word row as perfect would make every word its own line.
///
/// An underfull row is rated as if it could stretch like one ordinary
/// interword space (`nominal_stretch`), so a lone short word costs what an
/// equally loose two-word line does instead of undercutting every line that
/// has real glue. Only the final emergency pass accepts it beyond tolerance.
fn zero_capacity_metrics(
    boxes: &[ParagraphBox],
    start: usize,
    end: usize,
    natural: f32,
    options: ParagraphOptions,
    nominal_stretch: f32,
) -> Option<LineMetrics> {
    if end < boxes.len()
        && boxes.get(end).is_some_and(|item| item.break_before)
        && natural <= options.line_width
    {
        let slack = options.line_width - natural;
        let ratio = if nominal_stretch > f32::EPSILON {
            slack / nominal_stretch
        } else {
            (slack / options.line_width.max(1.0)) * 3.0
        };
        let tolerance = options.tolerance.max(0.0);
        if ratio <= tolerance || tolerance >= EMERGENCY_TOLERANCE {
            return Some(LineMetrics {
                natural,
                ratio: ratio.min(tolerance),
                fitness: fitness_class(ratio),
                glyph_scale: 1.0,
                badness: stretch_badness(ratio),
            });
        }
    }
    (end == start.saturating_add(1) && natural >= options.line_width).then_some(LineMetrics {
        natural,
        ratio: 0.0,
        fitness: 1,
        glyph_scale: 1.0,
        badness: 10_000.0,
    })
}

/// Search glyph micro-expansion steps for the least-bad stretch/shrink
/// solution at this boundary, trading inter-word glue against a small
/// horizontal scale on the glyphs themselves.
fn expansion_search(
    natural: f32,
    stretch: f32,
    shrink: f32,
    glyph_width: f32,
    delta: f32,
    options: ParagraphOptions,
) -> Option<LineMetrics> {
    let minimum_ratio = if options.tolerance >= EMERGENCY_TOLERANCE {
        -4.0
    } else {
        -1.0
    };
    let max_expansion = options.max_glyph_expansion.max(0.0);
    let mut best_metrics: Option<LineMetrics> = None;
    for step in -8i8..=8i8 {
        let expansion = max_expansion * f32::from(step) / 8.0;
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
        let space_badness = stretch_badness(ratio);
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

fn line_metrics(
    boxes: &[ParagraphBox],
    start: usize,
    end: usize,
    options: ParagraphOptions,
    final_line: bool,
    nominal_stretch: f32,
) -> Option<LineMetrics> {
    let (natural, stretch, shrink, glyph_width) = accumulate_line(boxes, start, end, options)?;

    // TeX's paragraph fill glue makes the final line effectively ragged.
    if final_line || !options.justify {
        let metrics = ragged_metrics(natural, start, end, options)?;
        // Ragged text has no measure for its last line to fall short of:
        // costing that line's slack balanced two-line paragraphs, ending
        // the first line far short of the margin with the next word in
        // reach. Justified text keeps the cost, which steers it away from a
        // stub of a last line.
        if final_line && !options.justify {
            return Some(LineMetrics {
                ratio: 0.0,
                badness: 0.0,
                ..metrics
            });
        }
        return Some(metrics);
    }

    let delta = options.line_width - natural;
    let capacity = if delta >= 0.0 { stretch } else { shrink };
    if capacity <= f32::EPSILON {
        return zero_capacity_metrics(boxes, start, end, natural, options, nominal_stretch);
    }
    expansion_search(natural, stretch, shrink, glyph_width, delta, options)
}

fn compose_at_tolerance(
    boxes: &[ParagraphBox],
    mut options: ParagraphOptions,
    tolerance: f32,
    nominal_stretch: f32,
) -> Option<Vec<ComposedLine>> {
    options.tolerance = tolerance;
    let count = boxes.len();
    // Keep a candidate for every fitness/flag combination at each boundary.
    // A locally more expensive route can be globally preferable when it avoids
    // an abrupt loose-to-tight transition or consecutive hyphenated lines.
    let mut best = vec![[None; STATE_SLOTS]; count.saturating_add(1)];
    let initial_slot = state_slot(1, false);
    if let Some(slot0) = best.get_mut(0)
        && let Some(cell) = slot0.get_mut(initial_slot)
    {
        *cell = Some(State {
            demerits: 0.0,
            previous: 0,
            previous_slot: initial_slot,
            fitness: 1,
            flagged: false,
            ratio: 0.0,
            natural_width: 0.0,
            glyph_scale: 1.0,
        });
    }

    for end in 1..=count {
        if end != count && boxes.get(end).is_some_and(|item| !item.break_before) {
            continue;
        }
        accumulate_states_for_end(boxes, options, nominal_stretch, &mut best, count, end);
    }

    backtrack_composed_lines(&best, count, options)
}

/// The `start` sweep of [`compose_at_tolerance`]'s dynamic-programming
/// step for one `end` boundary — split out to keep that function under
/// the line-count limit. Mutates `best[end]` in place; every other slot
/// it reads (`best[start]` for `start < end`) is already finalized.
fn accumulate_states_for_end(
    boxes: &[ParagraphBox],
    options: ParagraphOptions,
    nominal_stretch: f32,
    best: &mut [[Option<State>; STATE_SLOTS]],
    count: usize,
    end: usize,
) {
    for start in 0..end {
        let Some(previous_states) = best.get(start).copied() else {
            continue;
        };
        for (previous_slot, previous) in previous_states.into_iter().enumerate() {
            let Some(previous) = previous else {
                continue;
            };
            let final_line = end == count;
            let Some(metrics) =
                line_metrics(boxes, start, end, options, final_line, nominal_stretch)
            else {
                continue;
            };
            let badness = metrics.badness;
            let boundary = if end < count {
                boxes.get(end).copied()
            } else {
                None
            };
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
            let Some(slots) = best.get_mut(end) else {
                continue;
            };
            let Some(cell) = slots.get_mut(slot) else {
                continue;
            };
            if cell.is_none_or(|current| demerits < current.demerits) {
                *cell = Some(State {
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

/// Walks [`compose_at_tolerance`]'s finished DP table backwards from the
/// least-demerit final state to build the forward-ordered line list —
/// split out to keep that function under the line-count limit.
fn backtrack_composed_lines(
    best: &[[Option<State>; STATE_SLOTS]],
    count: usize,
    options: ParagraphOptions,
) -> Option<Vec<ComposedLine>> {
    let (mut slot, _) = best
        .get(count)?
        .iter()
        .enumerate()
        .filter_map(|(slot, state)| state.map(|state| (slot, state)))
        .min_by(|(_, a), (_, b)| a.demerits.total_cmp(&b.demerits))?;
    let mut lines = Vec::new();
    let mut end = count;
    while end > 0 {
        let state = best
            .get(end)
            .and_then(|slots| slots.get(slot))
            .copied()
            .flatten()?;
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
    let nominal_stretch = boxes
        .iter()
        .map(|item| item.glue_before.max(0.0))
        .fold(0.0, f32::max)
        * options.space_stretch.max(0.0);
    for tolerance in [options.tolerance, 6.0, 12.0, EMERGENCY_TOLERANCE] {
        if let Some(lines) = compose_at_tolerance(boxes, options, tolerance, nominal_stretch) {
            return lines;
        }
    }
    // Last-resort greedy packing still honors forbidden break boundaries. It
    // is preferable to an overfull unbreakable group beginning a line with
    // detached punctuation or an inline style fragment.
    let mut lines = Vec::new();
    let mut start = 0;
    while start < boxes.len() {
        let Some(start_box) = boxes.get(start) else {
            break;
        };
        let mut end = start.saturating_add(1);
        let mut natural = start_box.width.max(0.0);
        while end < boxes.len() {
            let Some(end_box) = boxes.get(end) else {
                break;
            };
            let candidate = natural + end_box.glue_before.max(0.0) + end_box.width.max(0.0);
            if end_box.break_before && candidate > options.line_width && end > start {
                break;
            }
            natural = candidate;
            end = end.saturating_add(1);
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

    /// Ragged text fills each line before its last as far as the measure
    /// allows: a short last line is no flaw there, so two lines are not
    /// balanced by ending the first with room for the next word.
    #[test]
    fn ragged_text_does_not_balance_away_a_short_last_line() {
        let boxes: Vec<_> = (0..8)
            .map(|index| ParagraphBox::word(20.0, if index == 0 { 0.0 } else { 10.0 }))
            .collect();
        let lines = compose_paragraph(
            &boxes,
            ParagraphOptions {
                justify: false,
                ..ParagraphOptions::new(150.0)
            },
        );
        let starts: Vec<_> = lines.iter().map(|line| (line.start, line.end)).collect();
        assert_eq!(starts, vec![(0, 5), (5, 8)], "{lines:?}");
    }

    #[allow(
        clippy::float_cmp,
        reason = "checks glue_scale actually moved away from the literal 1.0 sentinel; an epsilon comparison would let a barely-adjusted line slip through undetected"
    )]
    #[test]
    fn paragraph_composition_balances_lines_and_keeps_glue_valid() {
        {
            let widths = [46.0, 42.0, 45.0, 50.0, 36.0, 47.0, 41.0, 44.0];
            let boxes: Vec<_> = widths
                .into_iter()
                .enumerate()
                .map(|(index, width)| {
                    ParagraphBox::word(width, if index == 0 { 0.0 } else { 10.0 })
                })
                .collect();
            let lines = compose_paragraph(&boxes, ParagraphOptions::new(150.0));
            assert_eq!(lines.len(), 3);
            assert!(
                lines.last().unwrap().natural_width > 90.0,
                "the optimizer should rebalance earlier lines: {lines:?}"
            );
        }

        {
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

        {
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

        {
            let boxes = vec![
                ParagraphBox::word(95.0, 0.0),
                ParagraphBox::word(95.0, 10.0),
                ParagraphBox::word(95.0, 10.0),
            ];
            let lines = compose_paragraph(&boxes, ParagraphOptions::new(120.0));
            assert!(lines.iter().all(|line| line.glue_scale >= 0.2));
        }

        {
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

        {
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

        {
            let boxes = vec![
                ParagraphBox::word(100.0, 0.0),
                ParagraphBox::word(20.0, 10.0),
                ParagraphBox::word(100.0, 10.0),
            ];
            let lines = compose_paragraph(&boxes, ParagraphOptions::new(150.0));
            assert_eq!(
                lines
                    .iter()
                    .map(|line| (line.start, line.end))
                    .collect::<Vec<_>>(),
                vec![(0, 2), (2, 3)],
                "a half-empty unjustifiable line must not beat a loose justified one"
            );
        }

        {
            let boxes = vec![
                ParagraphBox::word(20.0, 0.0),
                ParagraphBox::word(145.0, 10.0),
                ParagraphBox::word(60.0, 10.0),
            ];
            let lines = compose_paragraph(&boxes, ParagraphOptions::new(150.0));
            assert_eq!(lines[0].end, 1, "only the emergency pass strands the word");
            assert!(lines.iter().all(|line| line.glue_scale >= 0.2));
        }

        {
            let mut options = ParagraphOptions::new(100.0);
            options.tolerance = EMERGENCY_TOLERANCE;
            options.max_glyph_expansion = 0.0;
            let overfull = expansion_search(110.0, 10.0, 5.0, 90.0, -10.0, options)
                .expect("the emergency pass may shrink past capacity");
            let gaping = expansion_search(50.0, 1.0, 1.0, 45.0, 50.0, options)
                .expect("and stretch to its tolerance");
            assert!(overfull.ratio < -1.0 && gaping.ratio >= 49.0);
            assert!(
                overfull.badness > gaping.badness,
                "a line wider than the measure loses to any loose line"
            );
        }

        {
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
}
