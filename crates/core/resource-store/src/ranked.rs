//! An ordered set that also answers where an entry stands: the entry at a position and the
//! position of an entry, without walking the entries before it. A window showing rows 40,000 to
//! 40,050 of a list reads those rows, and a list scrolled to a resource finds its row the same way.
//!
//! The entries are kept in sorted blocks of a bounded size. Finding the block takes a binary
//! search over the blocks' last entries. A position adds up the sizes of the blocks before it,
//! which is one addition per block, a few hundred at a million entries.

use std::ops::Bound;

/// The most entries a block holds before it splits in two.
const BLOCK: usize = 512;

#[derive(Clone, Debug)]
pub(crate) struct Ranked<T> {
    blocks: Vec<Vec<T>>,
    len: usize,
}

impl<T> Default for Ranked<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Ranked<T> {
    pub(crate) const fn new() -> Self {
        Self {
            blocks: Vec::new(),
            len: 0,
        }
    }
}

impl<T: Ord> Ranked<T> {
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// The block `item` belongs in: the first whose last entry is not below it, else the last.
    fn block_for(&self, item: &T) -> usize {
        self.blocks
            .partition_point(|block| block.last().is_some_and(|last| last < item))
            .min(self.blocks.len().saturating_sub(1))
    }

    /// Adds `item`; whether it was not held yet.
    pub(crate) fn insert(&mut self, item: T) -> bool {
        let at = self.block_for(&item);
        let Some(block) = self.blocks.get_mut(at) else {
            self.blocks.push(vec![item]);
            self.len = 1;
            return true;
        };
        let Err(place) = block.binary_search(&item) else {
            return false;
        };
        block.insert(place, item);
        self.len = self.len.saturating_add(1);
        if block.len() > BLOCK {
            let tail = block.split_off(BLOCK / 2);
            self.blocks.insert(at.saturating_add(1), tail);
        }
        true
    }

    /// Takes `item` out; whether it was held.
    pub(crate) fn remove(&mut self, item: &T) -> bool {
        let at = self.block_for(item);
        let Some(block) = self.blocks.get_mut(at) else {
            return false;
        };
        let Ok(place) = block.binary_search(item) else {
            return false;
        };
        block.remove(place);
        self.len = self.len.saturating_sub(1);
        if block.is_empty() {
            self.blocks.remove(at);
        }
        true
    }

    /// How many entries `before` holds of, for a `before` that holds of every entry up to some
    /// point and of none after it.
    pub(crate) fn partition_point(&self, before: impl Fn(&T) -> bool) -> usize {
        let at = self
            .blocks
            .partition_point(|block| block.last().is_some_and(&before));
        let prior: usize = self
            .blocks
            .iter()
            .take(at)
            .map(Vec::len)
            .fold(0, usize::saturating_add);
        prior.saturating_add(
            self.blocks
                .get(at)
                .map_or(0, |block| block.partition_point(&before)),
        )
    }

    /// How many entries sort before `item`: its position when it is held.
    pub(crate) fn rank(&self, item: &T) -> usize {
        self.partition_point(|entry| entry < item)
    }

    /// The entry at `position`.
    pub(crate) fn get(&self, mut position: usize) -> Option<&T> {
        for block in &self.blocks {
            match position.checked_sub(block.len()) {
                Some(rest) => position = rest,
                None => return block.get(position),
            }
        }
        None
    }

    /// The entries at the positions `from` up to `to`, in order; walked from either end.
    pub(crate) fn between(&self, from: usize, to: usize) -> impl DoubleEndedIterator<Item = &T> {
        let mut start = 0_usize;
        let spans: Vec<&[T]> = self
            .blocks
            .iter()
            .filter_map(|block| {
                let first = start;
                start = start.saturating_add(block.len());
                if start <= from || first >= to {
                    return None;
                }
                let lower = from.saturating_sub(first);
                let upper = to.saturating_sub(first).min(block.len());
                block.get(lower..upper)
            })
            .collect();
        spans.into_iter().flatten()
    }

    /// The entries between the bounds, in order; walked from either end.
    pub(crate) fn range<'a>(
        &'a self,
        lower: Bound<&T>,
        upper: Bound<&T>,
    ) -> impl DoubleEndedIterator<Item = &'a T> + use<'a, T> {
        let from = match lower {
            Bound::Included(item) => self.rank(item),
            Bound::Excluded(item) => self.partition_point(|entry| entry <= item),
            Bound::Unbounded => 0,
        };
        let to = match upper {
            Bound::Included(item) => self.partition_point(|entry| entry <= item),
            Bound::Excluded(item) => self.rank(item),
            Bound::Unbounded => self.len,
        };
        self.between(from, to.max(from))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::ops::Bound;

    use super::Ranked;

    #[test]
    fn it_answers_as_a_sorted_set_does() {
        let mut ranked = Ranked::default();
        let mut set = BTreeSet::new();
        // A spread of inserts and removals that splits and empties blocks.
        let mut n: u64 = 7;
        for step in 0..20_000_u64 {
            n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let item = (n >> 33) % 5_000;
            if step % 3 == 0 {
                assert_eq!(ranked.remove(&item), set.remove(&item));
            } else {
                assert_eq!(ranked.insert(item), set.insert(item));
            }
        }
        assert_eq!(ranked.len(), set.len());
        let sorted: Vec<u64> = set.iter().copied().collect();
        for (position, item) in sorted.iter().enumerate() {
            assert_eq!(ranked.get(position), Some(item));
            assert_eq!(ranked.rank(item), position);
        }
        assert_eq!(ranked.get(sorted.len()), None);
        let from = 1_000;
        let to = 3_000;
        let expected: Vec<u64> = set.range(from..to).copied().collect();
        let got: Vec<u64> = ranked
            .range(Bound::Included(&from), Bound::Excluded(&to))
            .copied()
            .collect();
        assert_eq!(got, expected);
        let backwards: Vec<u64> = ranked
            .range(Bound::Excluded(&from), Bound::Included(&to))
            .rev()
            .copied()
            .collect();
        let expected: Vec<u64> = set
            .range((Bound::Excluded(from), Bound::Included(to)))
            .rev()
            .copied()
            .collect();
        assert_eq!(backwards, expected);
    }
}
