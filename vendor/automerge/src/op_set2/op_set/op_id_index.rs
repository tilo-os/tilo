//! TILO(op-id-index): where an op is, by its id, without searching the
//! document's id columns (vendor/automerge/TILO-PATCH.md).
//!
//! `OpSet::get_op_id_pos` searches the `id_ctr` column for the op's counter.
//! The column's B-tree skips the slabs whose counters cannot hold it, which
//! works while counters rise along the document; but a text edited at
//! scattered places holds old and new counters side by side in every slab,
//! so the search decodes nearly every slab: about 0.25 ms per lookup at
//! 80,000 ops, and taking in one remote keystroke made two of them.
//!
//! The index holds the position every op had when it was built (by actor,
//! sorted by counter), and follows the document from there. Ops spliced in
//! since sit in the gaps between those ops: gap `g` is just before built op
//! `g` (gap `len` at the end), and holds its ops in order. A Fenwick tree
//! over blocks of [`BLOCK`] gaps counts the ops spliced into each, so the
//! ops before a built op or a gap are O(log n) to count and a splice
//! O(log n) to record, and so is taking such an op out again (a rolled
//! back transaction), and a renumbering of the actors renames the ops it
//! holds (a new actor sorting before others). Any other op leaving the
//! document, or an actor whose ops it holds going, drops the index, as does a gap
//! growing past [`MAX_GAP`] or more ops spliced in than a quarter of those
//! built; the next apply rebuilds it. It costs about 8 bytes per built op.
//!
//! Only a document whose searches have been slow is indexed: once the
//! searches since the last decision would have paid for building the index
//! (each counted as the slabs it may decode, at [`SEARCH_COST`] ops of
//! building per slab), an apply builds it if the `id_ctr` column spans at
//! least [`MIN_SLABS`] slabs and at least half as many counters are lower
//! than the one before them. A document typed in order has next to none
//! and packs thousands of ops into a slab; an import searches little. Those
//! keep nothing more. A document found not worth it is looked at again once
//! it has grown by an eighth. TILO(op-id-index-load): a document loaded is
//! looked at once at load, searched or not: a replica that loaded it keeps
//! taking changes into it, and its first should not pay for the build.
//!
//! A position the index gives is only a candidate: the caller reads the id
//! there, and searches the column as before if it is not the op asked for.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::types::{OpId, SmallHashMap};

/// Below this many slabs in the `id_ctr` column the search is quick
/// however the counters lie, and nothing is indexed.
const MIN_SLABS: usize = 64;

/// Ops one gap may hold before the index is rebuilt: finding an op in its
/// gap is a scan of the gap.
const MAX_GAP: usize = 4096;

/// Gaps per block of the Fenwick tree.
const BLOCK: usize = 64;

/// Ops of building the index that decoding one slab in a search costs as
/// much time as (about 0.45 µs against 30 ns).
const SEARCH_COST: usize = 16;

#[derive(Debug, Default)]
pub(crate) struct OpIdIndex {
    state: State,
    /// Slabs the searches since the last decision may have decoded.
    searched: AtomicUsize,
}

impl Clone for OpIdIndex {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            searched: AtomicUsize::new(self.searched.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug, Clone, Default)]
enum State {
    /// Not built; an apply builds it once searches have been slow enough.
    #[default]
    Unknown,
    /// Not worth building until the document holds this many ops.
    Unneeded(usize),
    /// Was built and had to be dropped; the next apply builds it again.
    Dropped,
    Built(Box<Built>),
}

#[derive(Debug, Clone)]
struct Built {
    /// How many ops the document held when this was built.
    len: usize,
    /// Per actor, the counter of each op of that actor the document held
    /// when this was built, and the op's position then; sorted.
    by_actor: Vec<Vec<(u32, u32)>>,
    /// Fenwick tree (1-based) over the ops spliced into each block of gaps.
    blocks: Vec<u32>,
    /// The ops spliced into each gap that has any, in document order.
    gaps: BTreeMap<u32, Vec<OpId>>,
    /// The gap of each op spliced in since the build.
    recent: SmallHashMap<OpId, u32>,
}

impl OpIdIndex {
    /// A search for an op went through a column of `slabs` slabs.
    pub(crate) fn searched(&self, slabs: usize) {
        if !matches!(self.state, State::Built(_)) {
            self.searched.fetch_add(slabs, Ordering::Relaxed);
        }
    }

    /// Whether a document of `len` ops whose `id_ctr` column spans `slabs`
    /// slabs, not indexed, is to be looked at with [`Self::decide`] now.
    pub(crate) fn wants_counters(&self, len: usize, slabs: usize) -> bool {
        let searched = self.searched.load(Ordering::Relaxed);
        slabs >= MIN_SLABS
            && match self.state {
                State::Built(_) => false,
                State::Dropped => true,
                State::Unneeded(until) if len < until => false,
                _ => searched.saturating_mul(SEARCH_COST) >= len,
            }
    }

    /// Whether to build, from the counters of the document's `len` ops in
    /// order, which span `slabs` slabs; if not, when to look again.
    pub(crate) fn decide(
        &mut self,
        len: usize,
        slabs: usize,
        counters: impl Iterator<Item = u32>,
    ) -> bool {
        let mut prev = 0;
        let mut descents = 0usize;
        for counter in counters {
            descents += usize::from(counter < prev);
            prev = counter;
        }
        let build = descents.saturating_mul(2) >= slabs;
        *self.searched.get_mut() = 0;
        if !build {
            self.state = State::Unneeded(len.saturating_add(len / 8));
        }
        build
    }

    #[cfg(test)]
    fn is_built(&self) -> bool {
        matches!(self.state, State::Built(_))
    }

    /// Index `ids`, the ids of the document's `len` ops in order.
    pub(crate) fn build(&mut self, len: usize, ids: impl Iterator<Item = OpId>) {
        self.state = State::Unknown;
        if u32::try_from(len).is_err() {
            return;
        }
        let ids: Vec<OpId> = ids.collect();
        if ids.len() != len {
            return;
        }
        let mut counts: Vec<usize> = Vec::new();
        for id in &ids {
            if counts.len() <= id.actor() {
                counts.resize(id.actor() + 1, 0);
            }
            if let Some(count) = counts.get_mut(id.actor()) {
                *count += 1;
            }
        }
        let mut by_actor: Vec<Vec<(u32, u32)>> =
            counts.into_iter().map(Vec::with_capacity).collect();
        for (pos, id) in ids.into_iter().enumerate() {
            let (Ok(counter), Ok(pos)) = (u32::try_from(id.counter()), u32::try_from(pos)) else {
                return;
            };
            if let Some(ops) = by_actor.get_mut(id.actor()) {
                ops.push((counter, pos));
            }
        }
        for ops in &mut by_actor {
            ops.sort_unstable();
        }
        self.state = State::Built(Box::new(Built {
            len,
            by_actor,
            blocks: vec![0; (len + 1).div_ceil(BLOCK) + 1],
            gaps: BTreeMap::new(),
            recent: SmallHashMap::default(),
        }));
    }

    /// TILO(op-id-index-actors): an actor was inserted at `idx`, so every
    /// actor from `idx` on moved up by one. The ops stay where they are; the
    /// index follows the renumbering rather than being built again.
    pub(crate) fn actor_inserted(&mut self, idx: usize) {
        let State::Built(built) = &mut self.state else {
            return;
        };
        if idx > built.by_actor.len() {
            built.by_actor.resize_with(idx, Vec::new);
        }
        built.by_actor.insert(idx, Vec::new());
        built.renumber(|id| Some(id.with_new_actor(idx)));
    }

    /// TILO(op-id-index-actors): the actor at `idx` was removed, so every
    /// actor after it moved down by one. Only an actor no indexed op names
    /// can go (as a transaction's actor that wrote nothing); otherwise the
    /// index is dropped.
    pub(crate) fn actor_removed(&mut self, idx: usize) {
        let State::Built(built) = &mut self.state else {
            return;
        };
        let names_ops = built.by_actor.get(idx).is_some_and(|ops| !ops.is_empty())
            || built.recent.keys().any(|id| id.actor() == idx);
        if names_ops {
            self.state = State::Dropped;
            return;
        }
        if idx < built.by_actor.len() {
            built.by_actor.remove(idx);
        }
        if built.renumber(|id| id.without_actor(idx)).is_none() {
            self.state = State::Dropped;
        }
    }

    /// TILO(op-id-index-load): whether a document just loaded is to be
    /// indexed now, rather than at the first apply once searches were slow:
    /// a loaded document is one a replica keeps taking changes into, and
    /// its first remote change should not pay for the build.
    pub(crate) fn wants_counters_at_load(&self, slabs: usize) -> bool {
        slabs >= MIN_SLABS && matches!(self.state, State::Unknown)
    }

    /// `ids` (in order) were spliced in at `at`.
    pub(crate) fn inserted(&mut self, at: usize, ids: impl Iterator<Item = OpId>) {
        let State::Built(built) = &mut self.state else {
            return;
        };
        let ids: Vec<OpId> = ids.collect();
        if !ids.is_empty() && built.insert(at, &ids).is_none() {
            self.state = State::Dropped;
        }
    }

    /// The op `id` left the document. One spliced in since the build (as
    /// every op of a transaction being rolled back is) leaves its gap; any
    /// other drops the index.
    pub(crate) fn removed(&mut self, id: OpId) {
        let State::Built(built) = &mut self.state else {
            return;
        };
        if built.remove(id).is_none() {
            self.state = State::Dropped;
        }
    }

    /// Where the op `id` is: `None` if nothing is indexed, `Some(None)` if
    /// the document does not hold it, else a position to check.
    pub(crate) fn find(&self, id: OpId) -> Option<Option<usize>> {
        let State::Built(built) = &self.state else {
            return None;
        };
        if let Some(&gap) = built.recent.get(&id) {
            let offset = built.gaps.get(&gap)?.iter().rposition(|&o| o == id)?;
            return Some(Some(built.gap_start(gap) + offset));
        }
        let Some(ops) = built.by_actor.get(id.actor()) else {
            return Some(None);
        };
        let Ok(counter) = u32::try_from(id.counter()) else {
            return Some(None);
        };
        match ops.binary_search_by_key(&counter, |&(c, _)| c) {
            // A built op sits right after its own gap.
            Ok(i) => {
                let (_, built_pos) = *ops.get(i)?;
                Some(Some(built.gap_start(built_pos) + built.gap_len(built_pos)))
            }
            Err(_) => Some(None),
        }
    }
}

impl Built {
    fn gap_len(&self, gap: u32) -> usize {
        self.gaps.get(&gap).map_or(0, Vec::len)
    }

    /// The position of the first op of gap `gap` (or of the built op after
    /// it, if the gap is empty).
    fn gap_start(&self, gap: u32) -> usize {
        let block = gap as usize / BLOCK;
        let mut spliced = 0usize;
        let mut i = block;
        while i > 0 {
            spliced += self.blocks.get(i).copied().unwrap_or(0) as usize;
            i &= i - 1;
        }
        let block_start = u32::try_from(block * BLOCK).unwrap_or(u32::MAX);
        for held in self.gaps.range(block_start..gap).map(|(_, held)| held) {
            spliced += held.len();
        }
        gap as usize + spliced
    }

    /// The gap a splice at position `at` goes into, and where in it.
    fn gap_at(&self, at: usize) -> Option<(u32, usize)> {
        // The block: the Fenwick tree's weights are each block's spliced
        // ops and the built op after each of its gaps. Only the last block
        // has fewer gaps, and no splice lies past all of it.
        let blocks = self.blocks.len() - 1;
        let mut block = 0usize;
        let mut rest = at;
        let mut step = blocks.checked_next_power_of_two()?;
        while step > 0 {
            let next = block + step;
            if next < blocks {
                let weight = step * BLOCK + *self.blocks.get(next)? as usize;
                if weight <= rest {
                    block = next;
                    rest -= weight;
                }
            }
            step >>= 1;
        }
        // The gap within it.
        let mut gap = block * BLOCK;
        let end = (gap + BLOCK).min(self.len + 1);
        let range = u32::try_from(gap).ok()?..u32::try_from(end).ok()?;
        for (&held_gap, held) in self.gaps.range(range) {
            let empty = held_gap as usize - gap;
            if rest < empty {
                break;
            }
            rest -= empty;
            gap = held_gap as usize;
            if rest <= held.len() {
                return Some((held_gap, rest));
            }
            rest -= held.len() + 1;
            gap += 1;
        }
        // An empty gap: before its built op, or at the end.
        if gap + rest < end {
            return Some((u32::try_from(gap + rest).ok()?, 0));
        }
        None
    }

    /// Rename the ops spliced in since the build after a renumbering of the
    /// actors; `None` if one no longer has an actor.
    fn renumber(&mut self, rename: impl Fn(OpId) -> Option<OpId>) -> Option<()> {
        let mut recent = SmallHashMap::default();
        for (&id, &gap) in self.recent.iter() {
            recent.insert(rename(id)?, gap);
        }
        self.recent = recent;
        for held in self.gaps.values_mut() {
            for id in held.iter_mut() {
                *id = rename(*id)?;
            }
        }
        Some(())
    }

    fn remove(&mut self, id: OpId) -> Option<()> {
        let gap = self.recent.remove(&id)?;
        let held = self.gaps.get_mut(&gap)?;
        let offset = held.iter().rposition(|&o| o == id)?;
        held.remove(offset);
        if held.is_empty() {
            self.gaps.remove(&gap);
        }
        let mut i = gap as usize / BLOCK + 1;
        while i < self.blocks.len() {
            let node = self.blocks.get_mut(i)?;
            *node = node.checked_sub(1)?;
            i += i & i.wrapping_neg();
        }
        Some(())
    }

    fn insert(&mut self, at: usize, ids: &[OpId]) -> Option<()> {
        let (gap, offset) = self.gap_at(at)?;
        let held = self.gaps.entry(gap).or_default();
        if offset > held.len() || held.len() + ids.len() > MAX_GAP {
            return None;
        }
        held.splice(offset..offset, ids.iter().copied());
        for &id in ids {
            self.recent.insert(id, gap);
        }
        let added = u32::try_from(ids.len()).ok()?;
        let mut i = gap as usize / BLOCK + 1;
        while i < self.blocks.len() {
            let node = self.blocks.get_mut(i)?;
            *node = node.checked_add(added)?;
            i += i & i.wrapping_neg();
        }
        // Each op spliced in takes more room than a built one: past a
        // quarter as many, build anew.
        (self.recent.len() <= (self.len / 4).max(MAX_GAP)).then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound.max(1) as u64) as usize
        }
    }

    fn check(index: &OpIdIndex, doc: &[OpId], next: u64) {
        for (pos, &id) in doc.iter().enumerate() {
            assert_eq!(index.find(id), Some(Some(pos)), "{id:?}");
        }
        for actor in 0..4 {
            assert_eq!(index.find(OpId::new(next, actor)), Some(None));
        }
    }

    #[test]
    fn follows_splices_anywhere() {
        for seed in 1..40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let next = std::cell::Cell::new(1u64);
            let fresh = |rng: &mut Rng| {
                next.set(next.get() + 1 + rng.below(2) as u64);
                OpId::new(next.get(), rng.below(3))
            };
            let len = 256 + rng.below(300);
            let mut doc: Vec<OpId> = (0..len).map(|_| fresh(&mut rng)).collect();
            let mut index = OpIdIndex::default();
            index.build(doc.len(), doc.iter().copied());
            assert!(index.is_built());
            let mut last = 0;
            for step in 0..400 {
                // Mostly one op, sometimes a run, often right after the last
                // splice, as typing does, and now and then at either end.
                let len = if rng.below(8) == 0 { rng.below(40) } else { 1 };
                let at = match rng.below(8) {
                    0 => 0,
                    1 => doc.len(),
                    2..=4 => last.min(doc.len()),
                    _ => rng.below(doc.len() + 1),
                };
                let ids: Vec<OpId> = (0..len).map(|_| fresh(&mut rng)).collect();
                doc.splice(at..at, ids.iter().copied());
                index.inserted(at, ids.into_iter());
                last = at + len;
                if step % 50 == 0 {
                    check(&index, &doc, next.get() + 1);
                }
            }
            assert!(index.is_built());
            check(&index, &doc, next.get() + 1);
        }
    }

    #[test]
    fn a_new_actor_renumbers_the_index_rather_than_dropping_it() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut next = 1u64;
        let mut doc: Vec<OpId> = (0..400)
            .map(|_| {
                next += 1;
                OpId::new(next, rng.below(3))
            })
            .collect();
        let mut index = OpIdIndex::default();
        index.build(doc.len(), doc.iter().copied());
        // Ops spliced in since the build, of every actor.
        for _ in 0..20 {
            next += 1;
            let id = OpId::new(next, rng.below(3));
            let at = rng.below(doc.len() + 1);
            doc.insert(at, id);
            index.inserted(at, std::iter::once(id));
        }
        // An actor sorting between the first and the second arrives.
        index.actor_inserted(1);
        for id in &mut doc {
            *id = id.with_new_actor(1);
        }
        assert!(index.is_built());
        check(&index, &doc, next + 1);
        // Its ops are found like any other's.
        next += 1;
        let theirs = OpId::new(next, 1);
        doc.insert(7, theirs);
        index.inserted(7, std::iter::once(theirs));
        check(&index, &doc, next + 1);
        // An actor without ops leaves the index as it was.
        index.actor_inserted(4);
        index.actor_removed(4);
        assert!(index.is_built());
        check(&index, &doc, next + 1);
        // One whose ops the index holds drops it.
        index.actor_removed(1);
        assert!(!index.is_built());
    }

    #[test]
    fn only_slow_searches_through_scattered_counters_are_indexed() {
        let index = OpIdIndex::default();
        // Searches worth a build, but too few slabs to be slow.
        index.searched(1_000);
        assert!(!index.wants_counters(1_000, MIN_SLABS - 1));
        // Enough slabs, but too few searches yet.
        let mut index = OpIdIndex::default();
        index.searched(MIN_SLABS);
        assert!(!index.wants_counters(MIN_SLABS * SEARCH_COST * 2, MIN_SLABS));
        index.searched(MIN_SLABS);
        assert!(index.wants_counters(MIN_SLABS * SEARCH_COST * 2, MIN_SLABS));
        // Typed in order: look again once grown by an eighth and searched
        // as much again.
        assert!(!index.decide(1_000, MIN_SLABS, 1..1_001));
        assert!(!index.wants_counters(1_125, MIN_SLABS));
        index.searched(1_000);
        assert!(!index.wants_counters(1_124, MIN_SLABS));
        assert!(index.wants_counters(1_125, MIN_SLABS));
        assert_eq!(index.find(OpId::new(1, 0)), None);
        // A low counter after a high one in every other op.
        let scattered = (0..1_000u32).map(|i| if i % 2 == 0 { 5_000 - i } else { i });
        assert!(index.decide(1_000, MIN_SLABS, scattered));
        index.build(1_000, (1..=1_000).map(|c| OpId::new(c, 0)));
        assert!(index.is_built());
        // Dropped (an actor whose ops it holds went), it is built again without waiting for
        // searches.
        index.actor_removed(0);
        assert!(!index.is_built());
        assert!(index.wants_counters(1_000, MIN_SLABS));
    }

    #[test]
    fn taking_back_spliced_ops_keeps_the_index() {
        for seed in 1..40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let mut next = 1u64;
            let mut doc: Vec<OpId> = (0..300)
                .map(|_| {
                    next += 1;
                    OpId::new(next, rng.below(3))
                })
                .collect();
            let mut index = OpIdIndex::default();
            index.build(doc.len(), doc.iter().copied());
            let first_built = doc[0];
            for _ in 0..60 {
                // A transaction: a few ops spliced in anywhere, then taken
                // back newest first, as `Transaction::rollback` does.
                let mut spliced = Vec::new();
                for _ in 0..1 + rng.below(6) {
                    next += 1;
                    let id = OpId::new(next, 3);
                    let at = rng.below(doc.len() + 1);
                    doc.insert(at, id);
                    index.inserted(at, std::iter::once(id));
                    spliced.push(id);
                }
                if rng.below(3) == 0 {
                    // Committed: they stay.
                    continue;
                }
                for id in spliced.into_iter().rev() {
                    let at = doc.iter().position(|&o| o == id).unwrap();
                    doc.remove(at);
                    index.removed(id);
                }
                assert!(index.is_built());
                check(&index, &doc, next + 1);
            }
            // A built op leaving drops the index.
            index.removed(first_built);
            assert!(!index.is_built());
        }
    }

    #[test]
    fn a_gap_too_full_drops_the_index() {
        let doc: Vec<OpId> = (1..=256).map(|c| OpId::new(c, 0)).collect();
        let mut index = OpIdIndex::default();
        index.build(doc.len(), doc.iter().copied());
        let paste = (0..=MAX_GAP as u64).map(|c| OpId::new(1_000 + c, 1));
        index.inserted(3, paste);
        assert!(!index.is_built());
    }
}
