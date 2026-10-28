//! Stable slice sorting compiled once for every call site.
//!
//! std's `sort_by` and its relatives are generic over the comparator, so
//! every call site compiles its own copy of the whole sort (driftsort and
//! its small-sort and merge helpers, several kilobytes each in a release
//! build). The toolkit and the models under it sort small lists in a few
//! hundred places; in the web desktop's start module those copies came to
//! about half a megabyte.
//!
//! [`CompactSort`] sorts with one shared, non-generic merge sort over
//! indices: each call site adds only its comparator and a small in-place
//! permutation for its element type. It is stable, `O(n log n)`, and
//! allocates two index vectors, which suits the short lists of interface
//! and model code; keep std's sorts for large, hot data.
//!
//! ```
//! use tilo_sort::CompactSort as _;
//!
//! let mut words = vec!["pear", "fig", "apple", "kiwi"];
//! words.compact_sort_by_key(|word| word.len());
//! assert_eq!(words, ["fig", "pear", "kiwi", "apple"]);
//! ```

use std::cmp::Ordering;

/// Stable sorting of a slice through one shared merge sort; see the crate
/// documentation.
pub trait CompactSort<T> {
    /// Sort with a comparator, like `[T]::sort_by`. Stable.
    fn compact_sort_by(&mut self, compare: impl FnMut(&T, &T) -> Ordering);

    /// Sort by a key, like `[T]::sort_by_key`. Stable; the key is computed
    /// for every comparison.
    fn compact_sort_by_key<K: Ord>(&mut self, mut key: impl FnMut(&T) -> K) {
        self.compact_sort_by(|a, b| key(a).cmp(&key(b)));
    }

    /// Sort by `Ord`, like `[T]::sort`. Stable.
    fn compact_sort(&mut self)
    where
        T: Ord,
    {
        self.compact_sort_by(T::cmp);
    }
}

impl<T> CompactSort<T> for [T] {
    fn compact_sort_by(&mut self, mut compare: impl FnMut(&T, &T) -> Ordering) {
        if self.len() < 2 {
            return;
        }
        let order = {
            let items = &*self;
            stable_order(
                items.len(),
                &mut |a, b| match (items.get(a), items.get(b)) {
                    (Some(a), Some(b)) => compare(a, b) == Ordering::Greater,
                    _ => false,
                },
            )
        };
        permute(self, order);
    }
}

/// Runs this long are sorted by insertion before merging.
const RUN: usize = 8;

/// The sorted order of `len` items as their indices: `order[k]` is the index
/// of the item that belongs at position `k`. `after(a, b)` is whether item
/// `a` sorts strictly after item `b`; equal items keep their order.
#[inline(never)]
fn stable_order(len: usize, after: &mut dyn FnMut(usize, usize) -> bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    for run in order.chunks_mut(RUN) {
        for end in 1..run.len() {
            let (sorted, rest) = run.split_at(end);
            let Some(&item) = rest.first() else { break };
            // After the last item it does not sort before, so equal items
            // keep their order.
            let place = sorted
                .iter()
                .rposition(|&other| !after(other, item))
                .map_or(0, |found| found.saturating_add(1));
            if let Some(moved) = run.get_mut(place..=end) {
                moved.rotate_right(1);
            }
        }
    }
    let mut merged = vec![0; len];
    let mut width = RUN;
    while width < len {
        let pair = width.saturating_mul(2);
        for (out, input) in merged.chunks_mut(pair).zip(order.chunks(pair)) {
            let (left, right) = input.split_at(width.min(input.len()));
            let mut left = left.iter().copied().peekable();
            let mut right = right.iter().copied().peekable();
            for slot in out {
                let from_right = match (left.peek(), right.peek()) {
                    (Some(&a), Some(&b)) => after(a, b),
                    (None, _) => true,
                    (Some(_), None) => false,
                };
                let next = if from_right {
                    right.next()
                } else {
                    left.next()
                };
                *slot = next.unwrap_or_default();
            }
        }
        std::mem::swap(&mut order, &mut merged);
        width = pair;
    }
    order
}

/// Move every item to its place in `order` (from [`stable_order`]), one
/// cycle of the permutation at a time.
fn permute<T>(items: &mut [T], mut order: Vec<usize>) {
    for start in 0..items.len() {
        let mut position = start;
        while let Some(slot) = order.get_mut(position) {
            let source = *slot;
            if source == position {
                break;
            }
            *slot = position;
            if source == start {
                break;
            }
            items.swap(position, source);
            position = source;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompactSort as _;

    /// A small deterministic generator, so the tests need no dependency.
    fn numbers(seed: u64, len: usize, range: u64) -> Vec<u64> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.checked_rem(range).unwrap_or(0)
            })
            .collect()
    }

    #[test]
    fn sorts_like_std_for_every_length() {
        for len in 0..200 {
            for range in [2_u64, 10, 1000] {
                let input = numbers(range.wrapping_add(len as u64), len, range);
                let mut ours = input.clone();
                ours.compact_sort();
                let mut std = input;
                std.sort_unstable();
                assert_eq!(ours, std, "len {len}, range {range}");
            }
        }
    }

    #[test]
    fn keeps_equal_items_in_order() {
        let keys = numbers(7, 500, 5);
        let mut items: Vec<(u64, usize)> = keys.into_iter().zip(0..).collect();
        let mut expected = items.clone();
        expected.sort_by_key(|item| item.0);
        items.compact_sort_by_key(|item| item.0);
        assert_eq!(items, expected);
    }

    #[test]
    fn sorts_by_comparator_and_on_vectors_of_owned_values() {
        let mut words: Vec<String> = ["b", "a", "c", "a"].map(String::from).to_vec();
        words.compact_sort_by(|a, b| b.cmp(a));
        assert_eq!(words, ["c", "b", "a", "a"]);
        let mut empty: Vec<u8> = Vec::new();
        empty.compact_sort();
        assert!(empty.is_empty());
    }
}
