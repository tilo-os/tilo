//! Bounded Damerau/Levenshtein edit distance.

/// Compute the Damerau–Levenshtein distance between `a` and `b`, but stop early
/// and return `None` as soon as the distance is known to exceed `max`.
///
/// This is the *optimal string alignment* variant: it counts insertions,
/// deletions, substitutions, and transpositions of two adjacent characters,
/// with the restriction that no substring is edited more than once. That
/// restriction is irrelevant for the short single-word inputs a spell checker
/// compares, and it keeps the algorithm to two rolling rows of state plus the
/// one extra previous row needed for the transposition rule.
///
/// Returning early on the `max` bound is what makes a full dictionary scan
/// affordable: most candidate words diverge in length or prefix immediately, so
/// the inner loop bails after a couple of rows.
///
/// Comparison is done over `char`s, so the cost is in Unicode scalar values, not
/// bytes — `"café"` vs `"cafe"` is distance 1, not 2.
pub fn damerau_levenshtein_within(a: &str, b: &str, max: usize) -> Option<usize> {
    // Cheap length-difference lower bound: if the words differ in length by more
    // than `max`, no sequence of <= max edits can align them.
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (la, lb) = (a_chars.len(), b_chars.len());

    if la.abs_diff(lb) > max {
        return None;
    }
    if la == 0 {
        return (lb <= max).then_some(lb);
    }
    if lb == 0 {
        return (la <= max).then_some(la);
    }

    // Three rolling rows: prev_prev (i-2) is needed for the transposition check.
    let width = lb.saturating_add(1);
    let mut prev_prev = vec![0usize; width];
    let mut prev = vec![0usize; width];
    let mut curr = vec![0usize; width];

    for (j, cell) in prev.iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=la {
        // `i` ranges over 1..=la, so `i - 1` always lands inside `a_chars`; the
        // `.get()` exists to satisfy the indexing lint, not because this can
        // actually miss — `?` and the surrounding `Option<usize>` return type
        // give the same "return None" behaviour as the old `let...else`.
        let a_char = a_chars.get(i.saturating_sub(1)).copied()?;

        if let Some(cell) = curr.get_mut(0) {
            *cell = i;
        }
        let mut row_min = i;

        for j in 1..=lb {
            let b_char = b_chars.get(j.saturating_sub(1)).copied()?;
            let cost = usize::from(a_char != b_char);

            let deletion = prev.get(j).copied().unwrap_or(usize::MAX).saturating_add(1);
            let insertion = curr
                .get(j.saturating_sub(1))
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            let substitution = prev
                .get(j.saturating_sub(1))
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(cost);
            let mut best = deletion.min(insertion).min(substitution);

            // Transposition of two adjacent characters.
            if i > 1
                && j > 1
                && let (Some(&b_prev), Some(&a_prev)) = (
                    b_chars.get(j.saturating_sub(2)),
                    a_chars.get(i.saturating_sub(2)),
                )
                && a_char == b_prev
                && a_prev == b_char
            {
                let transpose = prev_prev
                    .get(j.saturating_sub(2))
                    .copied()
                    .unwrap_or(usize::MAX)
                    .saturating_add(1);
                best = best.min(transpose);
            }

            if let Some(cell) = curr.get_mut(j) {
                *cell = best;
            }
            if best < row_min {
                row_min = best;
            }
        }

        // Early exit: if every cell in this row already exceeds `max`, the final
        // distance (which only grows as i increases for a fixed best alignment)
        // cannot come back under the bound.
        if row_min > max {
            return None;
        }

        // Rotate rows: prev_prev <- prev <- curr, and reuse the old prev_prev as
        // the next curr buffer to avoid reallocation.
        std::mem::swap(&mut prev_prev, &mut prev);
        std::mem::swap(&mut prev, &mut curr);
    }

    let dist = prev.get(lb).copied().unwrap_or(usize::MAX);
    (dist <= max).then_some(dist)
}

/// A finer-grained alignment cost used only for *ranking* suggestions, never for
/// the accept/reject bound.
///
/// It returns the same integer edit count as [`damerau_levenshtein_within`] in
/// the high bits, but breaks ties between equally-distant candidates by charging
/// a transposition slightly less (`0.9`) than a substitution or indel (`1.0`).
/// Adjacent-character transpositions ("teh" -> "the") are the single most common
/// human typo, so a candidate reachable by a transposition should outrank one
/// reachable only by a substitution at the same integer distance.
///
/// Returns `f64::INFINITY` when the integer distance exceeds `max` (so callers
/// can still bound the scan). Only called on candidates that already passed the
/// integer bound, so the bound check here is a cheap safety net.
#[allow(
    clippy::cast_precision_loss,
    reason = "word lengths a spell checker compares fit comfortably inside f64's 53-bit exact integer range"
)]
pub(crate) fn weighted_edit_cost(a: &str, b: &str, max: usize) -> f64 {
    const TRANSPOSE_COST: f64 = 0.9;

    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (la, lb) = (a_chars.len(), b_chars.len());

    if la.abs_diff(lb) > max {
        return f64::INFINITY;
    }
    if la == 0 {
        return lb as f64;
    }
    if lb == 0 {
        return la as f64;
    }

    let width = lb.saturating_add(1);
    let mut prev_prev = vec![0.0f64; width];
    let mut prev = vec![0.0f64; width];
    let mut curr = vec![0.0f64; width];

    for (j, cell) in prev.iter_mut().enumerate() {
        *cell = j as f64;
    }

    for i in 1..=la {
        // See the matching comment in `damerau_levenshtein_within`: these
        // `.get()` calls cannot actually miss given the loop bounds.
        let Some(a_char) = a_chars.get(i.saturating_sub(1)).copied() else {
            return f64::INFINITY;
        };

        if let Some(cell) = curr.get_mut(0) {
            *cell = i as f64;
        }

        for j in 1..=lb {
            let Some(b_char) = b_chars.get(j.saturating_sub(1)).copied() else {
                return f64::INFINITY;
            };
            let cost = if a_char == b_char { 0.0 } else { 1.0 };

            let deletion = prev.get(j).copied().unwrap_or(f64::INFINITY) + 1.0;
            let insertion = curr
                .get(j.saturating_sub(1))
                .copied()
                .unwrap_or(f64::INFINITY)
                + 1.0;
            let substitution = prev
                .get(j.saturating_sub(1))
                .copied()
                .unwrap_or(f64::INFINITY)
                + cost;
            let mut best = deletion.min(insertion).min(substitution);

            if i > 1
                && j > 1
                && let (Some(&b_prev), Some(&a_prev)) = (
                    b_chars.get(j.saturating_sub(2)),
                    a_chars.get(i.saturating_sub(2)),
                )
                && a_char == b_prev
                && a_prev == b_char
            {
                let transpose = prev_prev
                    .get(j.saturating_sub(2))
                    .copied()
                    .unwrap_or(f64::INFINITY)
                    + TRANSPOSE_COST;
                best = best.min(transpose);
            }

            if let Some(cell) = curr.get_mut(j) {
                *cell = best;
            }
        }

        std::mem::swap(&mut prev_prev, &mut prev);
        std::mem::swap(&mut prev, &mut curr);
    }

    prev.get(lb).copied().unwrap_or(f64::INFINITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damerau_levenshtein_distance_is_exact_within_an_inclusive_bound() {
        for (case, a, b, bound, want) in [
            ("identical_is_zero", "kitten", "kitten", 10, Some(0)),
            ("classic_levenshtein", "kitten", "sitting", 10, Some(3)),
            ("classic_levenshtein", "flaw", "lawn", 10, Some(2)),
            // Without the Damerau rule this would be 2 (two substitutions).
            ("transposition_is_distance_one", "teh", "the", 10, Some(1)),
            ("transposition_is_distance_one", "ab", "ba", 10, Some(1)),
            (
                "transposition_is_distance_one",
                "recieve",
                "receive",
                10,
                Some(1),
            ),
            ("insertion_and_deletion", "cat", "cats", 10, Some(1)),
            ("insertion_and_deletion", "cats", "cat", 10, Some(1)),
            (
                "unicode_counts_in_chars_not_bytes",
                "café",
                "cafe",
                10,
                Some(1),
            ),
            // Distance is 7; bound of 2 must reject.
            (
                "bound_returns_none_when_exceeded",
                "kitten",
                "monstrous",
                2,
                None,
            ),
            ("length_difference_short_circuit", "a", "abcdef", 2, None),
            ("empty_strings", "", "", 2, Some(0)),
            ("empty_strings", "", "ab", 2, Some(2)),
            ("empty_strings", "ab", "", 2, Some(2)),
            ("empty_strings", "", "abc", 2, None),
            ("bound_inclusive", "abc", "abd", 1, Some(1)),
            ("bound_inclusive", "abc", "xyz", 3, Some(3)),
            ("bound_inclusive", "abc", "xyz", 2, None),
        ] {
            assert_eq!(
                damerau_levenshtein_within(a, b, bound),
                want,
                "case {case}: {a:?} vs {b:?} within {bound}"
            );
        }
    }
}
