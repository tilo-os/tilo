//! A set of disjoint, sorted half-open byte ranges.

use std::ops::Range;

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct RangeSet {
    ranges: Vec<Range<u64>>,
}

impl RangeSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn full(len: u64) -> Self {
        let mut s = Self::new();
        s.insert(0..len);
        s
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Range<u64>> {
        self.ranges.iter()
    }

    pub fn total(&self) -> u64 {
        self.ranges
            .iter()
            .map(|r| r.end.saturating_sub(r.start))
            .fold(0u64, u64::saturating_add)
    }

    pub fn insert(&mut self, r: Range<u64>) {
        if r.start >= r.end {
            return;
        }
        let mut start = r.start;
        let mut end = r.end;
        // First range that ends at or after `start` (touching ranges merge).
        let lo = self.ranges.partition_point(|x| x.end < start);
        // First range that starts after `end`.
        let hi = self.ranges.partition_point(|x| x.start <= end);
        if let Some(first) = self.ranges.get(lo).filter(|_| lo < hi) {
            start = start.min(first.start);
        }
        if let Some(last) = hi
            .checked_sub(1)
            .and_then(|i| self.ranges.get(i))
            .filter(|_| lo < hi)
        {
            end = end.max(last.end);
        }
        self.ranges
            .splice(lo..hi.max(lo), std::iter::once(start..end));
    }

    pub fn remove(&mut self, r: Range<u64>) {
        if r.start >= r.end {
            return;
        }
        let mut out = Vec::with_capacity(self.ranges.len().saturating_add(1));
        for x in self.ranges.drain(..) {
            if x.end <= r.start || x.start >= r.end {
                out.push(x);
                continue;
            }
            if x.start < r.start {
                out.push(x.start..r.start);
            }
            if x.end > r.end {
                out.push(r.end..x.end);
            }
        }
        self.ranges = out;
    }

    /// The parts of `r` that are in the set.
    pub fn intersect(&self, r: &Range<u64>) -> Vec<Range<u64>> {
        let lo = self.ranges.partition_point(|x| x.end <= r.start);
        self.ranges
            .get(lo..)
            .unwrap_or_default()
            .iter()
            .take_while(|x| x.start < r.end)
            .map(|x| x.start.max(r.start)..x.end.min(r.end))
            .filter(|x| x.start < x.end)
            .collect()
    }

    /// The parts of `r` that are not in the set.
    pub fn missing(&self, r: &Range<u64>) -> Vec<Range<u64>> {
        let mut out = Vec::new();
        let mut at = r.start;
        for present in self.intersect(r) {
            if present.start > at {
                out.push(at..present.start);
            }
            at = present.end;
        }
        if at < r.end {
            out.push(at..r.end);
        }
        out
    }

    /// Whether every byte of `r` is in the set.
    pub fn contains(&self, r: &Range<u64>) -> bool {
        r.start >= r.end || self.missing(r).is_empty()
    }

    pub fn truncate(&mut self, len: u64) {
        self.remove(len..u64::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_sets_merge_split_and_clip() {
        // Phase `insert_merges_overlapping_and_touching_ranges`.
        {
            let mut s = RangeSet::new();
            s.insert(10..20);
            s.insert(30..40);
            s.insert(20..25);
            assert_eq!(s.iter().cloned().collect::<Vec<_>>(), vec![10..25, 30..40]);
            s.insert(0..100);
            assert_eq!(s.iter().cloned().collect::<Vec<_>>(), vec![0..100]);
            s.insert(200..300);
            s.insert(150..160);
            assert_eq!(
                s.iter().cloned().collect::<Vec<_>>(),
                vec![0..100, 150..160, 200..300]
            );
        }
        // Phase `remove_splits_and_intersect_clips`.
        {
            let mut s = RangeSet::full(100);
            s.remove(10..20);
            s.remove(50..60);
            assert_eq!(
                s.iter().cloned().collect::<Vec<_>>(),
                vec![0..10, 20..50, 60..100]
            );
            assert_eq!(s.intersect(&(5..55)), vec![5..10, 20..50]);
            assert_eq!(s.missing(&(5..55)), vec![10..20, 50..55]);
            assert!(s.contains(&(20..50)));
            assert!(!s.contains(&(15..25)));
            assert!(s.contains(&(7..7)));
            assert_eq!(s.total(), 80);
            s.truncate(30);
            assert_eq!(s.iter().cloned().collect::<Vec<_>>(), vec![0..10, 20..30]);
        }
    }
}
