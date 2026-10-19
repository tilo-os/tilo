//! Sorting compiled once for every item type.
//!
//! `slice::sort_by` is generic over the item and the comparison, so every
//! crate that sorts a type of its own carries its own copy of the standard
//! library's sort for it: about 2.5 KB of code per item type and comparison,
//! in each app, for lists that are rarely longer than a screen. These sort
//! the items' positions instead, with the comparison behind a reference: the
//! sort itself is compiled once, here in the shared runtime, and what each
//! item type carries is the comparison and a loop of swaps.
//!
//! The order is the one `slice::sort_by` gives: stable, so items that compare
//! equal keep their order. The comparison may be called with the same item
//! on both sides.

use std::cmp::Ordering;

/// Sort `items` by `compare`, stably: as `items.sort_by(compare)`.
pub fn sort_by<T>(items: &mut [T], mut compare: impl FnMut(&T, &T) -> Ordering) {
    let swaps = {
        let held: &[T] = items;
        swaps_to_sort(held.len(), &mut |a, b| match (held.get(a), held.get(b)) {
            (Some(a), Some(b)) => compare(a, b),
            _ => Ordering::Equal,
        })
    };
    for (a, b) in swaps {
        items.swap(a, b);
    }
}

/// Sort `items` by the key `key` reads from each, stably: as
/// `items.sort_by_key(key)`.
pub fn sort_by_key<T, K: Ord>(items: &mut [T], mut key: impl FnMut(&T) -> K) {
    sort_by(items, |a, b| key(a).cmp(&key(b)));
}

/// The swaps that put `len` items in the stable order `compare` gives their
/// positions, in the order to make them.
#[inline(never)]
pub(crate) fn swaps_to_sort(
    len: usize,
    compare: &mut dyn FnMut(usize, usize) -> Ordering,
) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..len).collect();
    if order.is_sorted_by(|&a, &b| compare(a, b).is_le()) {
        return Vec::new();
    }
    order.sort_by(|&a, &b| compare(a, b));
    // `at[p]` is the item now at position `p`, `place[i]` where item `i` now
    // is; position `p` is filled in turn with item `order[p]`.
    let mut at: Vec<usize> = (0..len).collect();
    let mut place: Vec<usize> = (0..len).collect();
    let mut swaps = Vec::new();
    for (position, &item) in order.iter().enumerate() {
        let Some(&from) = place.get(item) else {
            continue;
        };
        if from == position {
            continue;
        }
        let Some(&displaced) = at.get(position) else {
            continue;
        };
        swaps.push((position, from));
        if let (Some(here), Some(there)) = (at.get_mut(position), place.get_mut(item)) {
            *here = item;
            *there = position;
        }
        if let (Some(here), Some(there)) = (at.get_mut(from), place.get_mut(displaced)) {
            *here = displaced;
            *there = from;
        }
    }
    swaps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the standard library's stable sort, ties kept in place.
    #[test]
    fn sorts_as_the_standard_stable_sort() {
        let mut seed = 0x2545_f491_u32;
        for len in 0..40 {
            let items: Vec<(u8, usize)> = (0..len)
                .map(|at| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    ((seed % 5) as u8, at)
                })
                .collect();
            let mut expected = items.clone();
            expected.sort_by_key(|item| item.0);
            let mut sorted = items.clone();
            sort_by_key(&mut sorted, |item| item.0);
            assert_eq!(sorted, expected, "{items:?}");
            let mut reversed = items.clone();
            sort_by(&mut reversed, |a, b| b.0.cmp(&a.0));
            expected.sort_by_key(|item| std::cmp::Reverse(item.0));
            assert_eq!(reversed, expected, "{items:?}");
        }
    }

    /// Items already in order are not touched.
    #[test]
    fn sorted_items_need_no_swaps() {
        let items = [1, 2, 2, 3];
        let swaps = swaps_to_sort(items.len(), &mut |a, b| items[a].cmp(&items[b]));
        assert!(swaps.is_empty());
    }
}
