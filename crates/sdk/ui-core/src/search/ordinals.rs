//! Browse ordinals for a catalogue synchronised to an ordered snapshot.
//!
//! A [`SearchCatalog`](super::SearchCatalog) orders equally relevant hits,
//! and empty-query browsing, by each record's browse ordinal. A bulk
//! replacement numbers its records in snapshot order, [`GAP`] apart from
//! [`GAP`] on, leaving room before the first as after the last. When a
//! later snapshot changes a few records, [`plan`] keeps the ordinal of every
//! record it can and places the rest between their neighbours, so the order
//! is the snapshot's, as a bulk replacement would have made it, while only
//! the placed records are indexed again.

/// Ordinals a bulk replacement leaves between neighbours.
pub(super) const GAP: u64 = 1 << 20;

/// The ordinal each snapshot record takes, given the ordinal it holds now
/// (`None` for a record new to the catalogue). Ordinals strictly increase
/// in snapshot order. A record keeps its ordinal when that is possible
/// within the longest run of held ordinals already in order; the others
/// are spread between their neighbours, which give up theirs too when
/// there is no room left between them. `None` when the snapshot would need
/// ordinals beyond `u64`.
pub(super) fn plan(held: &[Option<u64>]) -> Option<Vec<u64>> {
    let mut keep = longest_increasing(held);
    loop {
        match place(held, &keep) {
            Placement::Done(ordinals) => return Some(ordinals),
            Placement::Crowded { before, after } => {
                // Free the run's neighbours and try the wider run.
                let mut widened = false;
                for at in [before, after].into_iter().flatten() {
                    if let Some(kept) = keep.get_mut(at) {
                        widened |= std::mem::take(kept);
                    }
                }
                if !widened {
                    return None;
                }
            }
        }
    }
}

/// Which records keep their held ordinal: a longest strictly increasing
/// subsequence of the held ordinals, in snapshot order.
fn longest_increasing(held: &[Option<u64>]) -> Vec<bool> {
    // `tails[len]`: the index ending the best increasing run of `len + 1`.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; held.len()];
    for (at, ordinal) in held.iter().enumerate() {
        let Some(ordinal) = ordinal else {
            continue;
        };
        let len = tails.partition_point(|&tail| held.get(tail).copied().flatten() < Some(*ordinal));
        if let Some(slot) = previous.get_mut(at) {
            *slot = len
                .checked_sub(1)
                .and_then(|before| tails.get(before).copied());
        }
        match tails.get_mut(len) {
            Some(tail) => *tail = at,
            None => tails.push(at),
        }
    }
    let mut keep = vec![false; held.len()];
    let mut at = tails.last().copied();
    while let Some(index) = at {
        if let Some(kept) = keep.get_mut(index) {
            *kept = true;
        }
        at = previous.get(index).copied().flatten();
    }
    keep
}

enum Placement {
    Done(Vec<u64>),
    /// The run between these kept records has too few ordinals between them.
    Crowded {
        before: Option<usize>,
        after: Option<usize>,
    },
}

fn place(held: &[Option<u64>], keep: &[bool]) -> Placement {
    let mut ordinals = Vec::with_capacity(held.len());
    let mut at = 0;
    while at < held.len() {
        if keep.get(at).copied().unwrap_or(false) {
            ordinals.push(held.get(at).copied().flatten().unwrap_or_default());
            at = at.saturating_add(1);
            continue;
        }
        let start = at;
        while at < held.len() && !keep.get(at).copied().unwrap_or(false) {
            at = at.saturating_add(1);
        }
        let before = start.checked_sub(1);
        let after = (at < held.len()).then_some(at);
        let low = before.and_then(|index| held.get(index).copied().flatten());
        let high = after.and_then(|index| held.get(index).copied().flatten());
        match spread(low, high, at.saturating_sub(start)) {
            Some(run) => ordinals.extend(run),
            None => return Placement::Crowded { before, after },
        }
    }
    Placement::Done(ordinals)
}

/// `count` increasing ordinals strictly between `low` and `high`, evenly
/// spaced. Past the last neighbour they follow [`GAP`] apart, and with no
/// neighbour at all they start at [`GAP`], as a bulk replacement numbers;
/// before the first they start from zero. `None` when they do not fit.
fn spread(low: Option<u64>, high: Option<u64>, count: usize) -> Option<Vec<u64>> {
    let count = u64::try_from(count).ok()?;
    let slots = 1..=count;
    match (low, high) {
        (None, None) => slots.map(|slot| slot.checked_mul(GAP)).collect(),
        (Some(low), None) => slots
            .map(|slot| {
                slot.checked_mul(GAP)
                    .and_then(|offset| low.checked_add(offset))
            })
            .collect(),
        (Some(low), Some(high)) => {
            let step = high.checked_sub(low)?.checked_div(count.checked_add(1)?)?;
            (step > 0).then_some(())?;
            slots
                .map(|slot| {
                    step.checked_mul(slot)
                        .and_then(|offset| low.checked_add(offset))
                })
                .collect()
        }
        // As if `low` were one below zero.
        (None, Some(high)) => {
            let step = high.checked_add(1)?.checked_div(count.checked_add(1)?)?;
            (step > 0).then_some(())?;
            slots
                .map(|slot| {
                    step.checked_mul(slot)
                        .and_then(|value| value.checked_sub(1))
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(times: u64) -> u64 {
        times.checked_mul(GAP).unwrap()
    }

    fn increasing(ordinals: &[u64]) -> bool {
        ordinals.windows(2).all(|pair| pair[0] < pair[1])
    }

    #[test]
    fn a_fresh_snapshot_is_numbered_a_gap_apart() {
        assert_eq!(plan(&[None, None, None]), Some(vec![GAP, gap(2), gap(3)]));
    }

    #[test]
    fn records_in_order_keep_their_ordinals_and_new_ones_go_between() {
        let held = [Some(GAP), None, Some(gap(2)), Some(gap(3)), None];
        let planned = plan(&held).unwrap();
        assert!(increasing(&planned));
        assert_eq!(planned[0], GAP);
        assert_eq!(planned[2], gap(2));
        assert_eq!(planned[3], gap(3));
        assert!(planned[1] > GAP && planned[1] < gap(2));
        assert!(planned[4] > gap(3));
    }

    #[test]
    fn a_moved_record_takes_a_new_ordinal_and_the_rest_keep_theirs() {
        // The last record moved to the front.
        let held = [Some(gap(4)), Some(GAP), Some(gap(2)), Some(gap(3))];
        let planned = plan(&held).unwrap();
        assert!(increasing(&planned));
        assert_eq!(&planned[1..], &[GAP, gap(2), gap(3)]);
    }

    #[test]
    fn a_crowded_gap_frees_its_neighbours() {
        // Nothing fits between 4 and 5: the neighbours move apart.
        let held = [Some(0), Some(4), None, None, Some(5), Some(GAP)];
        let planned = plan(&held).unwrap();
        assert!(increasing(&planned));
        assert_eq!(planned[0], 0);
        assert_eq!(planned[5], GAP);
    }

    #[test]
    fn inserting_before_the_first_record_fits_below_it() {
        let held = [None, Some(GAP), Some(gap(2))];
        assert_eq!(
            plan(&held).unwrap(),
            [(GAP >> 1).checked_sub(1).unwrap(), GAP, gap(2)]
        );
        // Below ordinal zero nothing fits: the first record moves up.
        let held = [None, None, Some(0), Some(GAP)];
        let planned = plan(&held).unwrap();
        assert!(increasing(&planned));
        assert_eq!(planned[3], GAP);
    }

    #[test]
    fn a_record_at_the_end_of_the_ordinal_space_is_renumbered() {
        assert_eq!(plan(&[Some(u64::MAX), None]), Some(vec![GAP, gap(2)]));
    }
}
