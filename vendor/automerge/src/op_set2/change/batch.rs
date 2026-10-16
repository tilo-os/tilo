use crate::change_queue::ChangeBatch;
use crate::hydrate::Value;
use crate::iter::RichTextDiff;
use crate::op_set2::types::{Action, KeyRef, MarkData, PropRef, ScalarValue as OpScalarValue};
use crate::op_set2::SuccInsert;
use crate::types::{
    ActorId, ElemId, ObjId, ObjType, OpId, Prop, ScalarValue, SequenceType, SmallHashMap,
};
use crate::{Automerge, Change, ChangeHash, PatchLog, PatchLogMismatch};
use crate::{AutomergeError, TextEncoding};

use super::super::op::{ChangeOp, Op, OpBuilder};
use super::super::op_set::{ObjIdIter, ObjIndex, OpIter, OpSet};

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

type PredCache = SmallHashMap<OpId, Vec<(OpId, Option<i64>)>>;

#[derive(Debug, Clone, Default)]
struct BatchApply {
    ops: Vec<ChangeOp>,
    changes: Vec<Change>,
    pred: PredCache,
    obj_spans: Vec<ObjSpan>,
}

struct Untangler<'a> {
    // the tangle of change ops we need to navigate
    change_ops: &'a mut [ChangeOp],
    // these are entry points into the change_op tangle
    // when we see a doc_op.id equal to key, put this vec onto the stack
    // ops inserted after HEAD are put onto the stack immidately
    entry: SmallHashMap<OpId, Vec<usize>>,
    // same concept as entry but internal to the change_ops array
    gosub: SmallHashMap<usize, Vec<usize>>,
    // stack of change ops ready to be processed
    stack: Vec<usize>,
    // these are change ops updating a pre-existing doc op elemid's
    // and are handled differently than inserts
    updates: SmallHashMap<ElemId, Vec<usize>>,
    updates_stack: Vec<usize>,
    pred: &'a mut PredCache,
    // Top and Conflicts keep track of changes that need to
    // be made to the index.top and index.visible columns
    top: Top,
    conflicts: &'a mut Vec<Adjust>,
    value: ValueState<'a>,
    seq_type: SequenceType,
    text_encoding: TextEncoding,
    count: usize,
    index: usize,
    max: usize,
    width: usize,
}

impl<'a> Untangler<'a> {
    fn flush(&mut self, log: &mut PatchLog) {
        self.value.list_flush(self.index, log);
        self.top.reset(self.conflicts);
        self.index += self.width;
        self.width = 0;
    }

    fn handle_doc_op(&mut self, doc_op: &Op<'a>, succ: &mut Vec<SuccInsert>, log: &mut PatchLog) {
        let mut deleted = false;
        if let Some(mut successors) = self.pred.remove(&doc_op.id) {
            normalize_increment_successors(doc_op.is_counter(), &mut successors);
            for (id, inc) in successors {
                deleted |= inc.is_none();
                succ.push(doc_op.add_succ(id, inc));
            }
        }

        if doc_op.insert {
            self.flush(log);
            self.value.key = Some(PropRef::Seq(self.index));
        }

        if doc_op.visible() && !deleted {
            self.width = doc_op.width(self.seq_type, self.text_encoding);
        }
        self.value.process_doc_op(doc_op, deleted);
        self.top.process_doc_op(self.change_ops, doc_op, deleted);
    }

    fn element_update(&mut self, doc_op: &Op<'_>) {
        while let Some(last) = self.updates_stack.last().copied() {
            if doc_op.insert || doc_op.id > self.change_ops[last].id() {
                self.updates_stack.pop();
                self.change_ops[last].pos = Some(doc_op.pos);
                self.change_ops[last].subsort = self.count;
                if self.change_ops[last].visible() {
                    self.width = self.change_ops[last].width(self.seq_type, self.text_encoding);
                }
                self.value.process_change_op(&self.change_ops[last]);
                self.count += 1;
                self.top
                    .process_change_op(self.conflicts, self.change_ops, last);
            } else {
                break;
            }
        }
    }

    fn finish_updates(&mut self) {
        for i in self.updates_stack.iter().rev() {
            self.change_ops[*i].pos = Some(self.max);
            self.change_ops[*i].subsort = self.count;
            self.width = 0;
            if self.change_ops[*i].visible() {
                self.width = self.change_ops[*i].width(self.seq_type, self.text_encoding);
            }
            self.value.process_change_op(&self.change_ops[*i]);

            self.top
                .process_change_op(self.conflicts, self.change_ops, *i);

            self.count += 1;
        }
    }

    fn finish_inserts(&mut self, log: &mut PatchLog) {
        while !self.stack.is_empty() {
            self.untangle_inner(self.max, log);
        }
    }

    fn finish(mut self, log: &mut PatchLog) {
        self.finish_updates();

        self.flush(log);

        assert!(self.entry.is_empty());

        self.finish_inserts(log);

        self.flush(log);

        assert_eq!(self.top, Top::Nothing);

        self.change_ops.sort_by(|a, b| {
            a.pos
                .unwrap()
                .cmp(&b.pos.unwrap())
                .then_with(|| a.subsort.cmp(&b.subsort))
        });
    }

    fn untangle_inserts(&mut self, id: OpId, insert_pos: usize, log: &mut PatchLog) {
        self.flush(log);

        if let Err(n) = self
            .stack
            .binary_search_by(|n| self.change_ops[*n].id().cmp(&id))
        {
            while self.stack.len() > n {
                self.untangle_inner(insert_pos, log);
            }
        }
        if let Some(v) = self.entry.remove(&id) {
            self.stack.extend(v);
        }
        if let Some(u) = self.updates.get(&ElemId(id)) {
            self.updates_stack.extend(u.iter().rev());
        }
    }

    fn untangle_inner(&mut self, insert_pos: usize, log: &mut PatchLog) -> Option<()> {
        let mut pos = self.stack.pop()?;
        let op = self.change_ops.get_mut(pos)?;

        let mut conflict = false;
        let mut vis = None;

        let key = KeyRef::Seq(ElemId(op.id()));

        assert!(op.pos.is_none());
        op.pos = Some(insert_pos);
        op.subsort = self.count;
        self.count += 1;

        if op.is_set_or_make() && !op.has_succ() {
            vis = Some(pos);
        } else if op.action() == Action::Mark {
            self.value.process_mark(op.id(), op.mark_data());
        }

        if let Some(v) = self.gosub.get(&pos) {
            self.stack.extend(v);
        }

        for i in (pos + 1)..self.change_ops.len() {
            let next_vis = {
                let next_op = &mut self.change_ops[i];

                pos += 1;

                if next_op.insert() || next_op.key() != &key {
                    break;
                }

                next_op.pos = Some(insert_pos);
                next_op.subsort = self.count;
                self.count += 1;

                next_op.is_set_or_make() && !next_op.has_succ()
            };

            if next_vis {
                // borrow checker stuff
                if let Some(conflict_pos) = vis {
                    self.change_ops[conflict_pos].conflicted = true;
                    conflict = true;
                }
                vis = Some(pos);
            }
        }

        if let Some(p) = vis {
            let op = &mut self.change_ops[p];
            if self.seq_type == SequenceType::List {
                let value = op.hydrate_value_and_fix_counters(self.text_encoding);
                log.insert(op.bld.obj, self.index, value, op.id(), conflict);
                self.index += 1;
            } else {
                let marks = self.value.marks.after.current().cloned();
                match op.bld.action {
                    Action::MakeMap => {
                        // Block markers
                        log.insert(
                            op.bld.obj,
                            self.index,
                            Value::map(),
                            op.bld.id,
                            op.conflicted,
                        );
                    }
                    _ => {
                        log.splice(op.bld.obj, self.index, op.bld.as_str(), marks);
                    }
                }
                self.index += op.width(self.seq_type, self.text_encoding);
            }
        }

        Some(())
    }

    fn new(
        obj: ObjId,
        encoding: SequenceType,
        text_encoding: TextEncoding,
        conflicts: &'a mut Vec<Adjust>,
        change_ops: &'a mut [ChangeOp],
        pred: &'a mut PredCache,
        max: usize,
    ) -> Self {
        let mut e_to_i = SmallHashMap::default();
        let mut gosub: SmallHashMap<usize, Vec<usize>> = HashMap::default();
        let mut entry: SmallHashMap<OpId, Vec<usize>> = HashMap::default();
        let mut stack: Vec<usize> = Vec::with_capacity(change_ops.len());
        let mut updates: SmallHashMap<ElemId, Vec<usize>> = HashMap::default();
        let mut last_e = None;
        let value = ValueState::new(obj, encoding, text_encoding);
        for (i, op) in change_ops.iter_mut().enumerate() {
            if let Some(mut successors) = pred.remove(&op.id()) {
                let is_counter = matches!(op.bld.value, OpScalarValue::Counter(_));
                normalize_increment_successors(is_counter, &mut successors);
                op.succ = successors;
            }
            if let KeyRef::Seq(e) = op.key() {
                if op.insert() {
                    if let Some(j) = e_to_i.get(e) {
                        gosub.entry(*j).or_default().push(i);
                    } else if e.is_head() {
                        stack.push(i);
                    } else {
                        entry.entry(e.0).or_default().push(i);
                    }
                } else if last_e != Some(*e) {
                    updates.entry(*e).or_default().push(i);
                }
                if op.insert() {
                    let this_e = ElemId(op.id());
                    e_to_i.insert(this_e, i);
                    last_e = Some(this_e);
                }
            }
        }
        let updates_stack = Vec::with_capacity(change_ops.len());
        Self {
            gosub,
            entry,
            stack,
            pred,
            change_ops,
            updates,
            seq_type: encoding,
            text_encoding,
            conflicts,
            updates_stack,
            top: Top::Nothing,
            count: 0,
            index: 0,
            width: 0,
            value,
            max,
        }
    }
}

// TILO(seq-apply-skip): a list or text walk visits only the elements a batch
// can affect and seeks over the rest (vendor/automerge/TILO-PATCH.md).
//
// An element (an insert op and the ops after it that update it) is inert if
// none of its ops is in the pred cache, no change op inserts after it or
// updates it, and the walk reaches it with no change insert waiting for its
// place (`stack`), no change update waiting for its place in an element
// (`updates_stack`) and, in a text, the same marks before and after the
// batch. Walking an inert element adds its visible width to `index`, feeds
// its mark ops (if any) to the mark state, and flushes neither a patch nor a
// conflict: its values are neither deleted nor exposed, `Top` ends where it
// started, and a text logs a mark patch for it only if the marks before and
// after the batch differ. Nothing it does makes any of those conditions
// false. So a run of inert elements is seeked over: `index` gains the width
// the op set's `top`/`text` index columns hold for the run, and its mark ops
// are replayed, leaving the walk with the same state, the same patches and
// the same insert position for every change op as stepping over it op by op.
struct SeqSkip<'a> {
    op_set: &'a OpSet,
    /// End of the object's ops.
    end: usize,
    /// Sorted starts of the elements the walk must visit.
    stops: Vec<usize>,
}

impl<'a> SeqSkip<'a> {
    fn new(
        op_set: &'a OpSet,
        obj: Range<usize>,
        ut: &Untangler<'_>,
        preds: &[usize],
    ) -> Option<Self> {
        if ut
            .change_ops
            .len()
            .saturating_mul(MapSkip::MIN_OPS_PER_CHANGE_OP)
            > obj.len()
        {
            return None;
        }
        let first = preds.partition_point(|&p| p < obj.start);
        let last = preds.partition_point(|&p| p < obj.end);
        let mut stops = Vec::with_capacity(last - first + ut.entry.len() + ut.updates.len());
        for &pos in &preds[first..last] {
            stops.push(op_set.element_start(pos, obj.clone())?);
        }
        // Change ops insert after, or update, elements by their insert op's
        // id. One that names anything else walks op by op, as it always did.
        let elements = ut.entry.keys().copied();
        for id in elements.chain(ut.updates.keys().map(|e| e.0)) {
            let pos = op_set.op_id_pos(id)?;
            if op_set.element_start(pos, obj.clone())? != pos {
                return None;
            }
            stops.push(pos);
        }
        stops.sort_unstable();
        stops.dedup();
        Some(Self {
            op_set,
            end: obj.end,
            stops,
        })
    }

    /// The first element the walk must visit at or after `from`, else the
    /// object's end.
    fn next_stop(&self, from: usize) -> usize {
        let next = self.stops.partition_point(|&s| s < from);
        self.stops.get(next).copied().unwrap_or(self.end)
    }
}

impl<'a> Untangler<'a> {
    // TILO(seq-apply-skip): the walk is at `from`, the start of an element it
    // has not taken yet; seek over the inert elements from there, if any.
    fn skip_inert(
        &mut self,
        skip: &SeqSkip<'a>,
        from: usize,
        log: &mut PatchLog,
    ) -> Option<OpIter<'a>> {
        if !self.stack.is_empty() || !self.updates_stack.is_empty() {
            return None;
        }
        // Marks are read by text patches only.
        let text = self.seq_type == SequenceType::Text;
        if text && self.value.marks.before != self.value.marks.after {
            return None;
        }
        let to = skip.next_stop(from);
        if to <= from {
            return None;
        }
        // What `untangle_inserts` does first at `from`.
        self.flush(log);
        if text {
            let mut at = from;
            while let Some(pos) = skip.op_set.next_mark(at..to) {
                if let Some(op) = skip.op_set.iter_range(&(pos..pos + 1)).next() {
                    self.value.process_doc_op(&op, false);
                }
                at = pos + 1;
            }
        }
        self.index += skip.op_set.visible_width(from..to, self.seq_type);
        Some(skip.op_set.iter_range(&(to..skip.end)))
    }
}

fn walk_list<'a>(
    mut ut: Untangler<'a>,
    mut doc_ops: OpIter<'a>,
    succ: &mut Vec<SuccInsert>,
    log: &mut PatchLog,
    skip: Option<SeqSkip<'a>>,
) {
    while let Some(op) = doc_ops.next() {
        // TILO(seq-apply-skip)
        if let (true, Some(skip)) = (op.insert, &skip) {
            if let Some(rest) = ut.skip_inert(skip, op.pos, log) {
                doc_ops = rest;
                continue;
            }
        }
        ut.element_update(&op);

        if op.insert {
            ut.untangle_inserts(op.id, op.pos, log);
        }

        ut.handle_doc_op(&op, succ, log);
    }

    ut.finish(log);
}

struct MapWalker<'a, 'b> {
    ops: OpIter<'a>,
    log: &'b mut PatchLog,
    pred: &'b mut PredCache,
    succ: &'b mut Vec<SuccInsert>,
    value: ValueState<'a>,
    pos: usize,
    doc_op: Option<Op<'a>>,
    conflicts: &'b mut Vec<Adjust>,
    top: Top,
    // TILO(map-apply-skip)
    skip: Option<MapSkip<'a>>,
}

// TILO(map-apply-skip): a map walk visits only the doc ops a batch can affect
// and seeks over the rest (vendor/automerge/TILO-PATCH.md).
//
// Walking a doc op does something only if the batch overwrites it (it is in
// the pred cache) or if it is visible in a key the batch writes: `process_pred`
// needs the first, `ValueState::process_doc_op` and `Top::process_doc_op` act
// on visible ops only (the mark state it also keeps is read by sequences, not
// maps), and a key whose ops are all untouched flushes no patch and leaves
// `Top` as it found it. Every other op is inert. Seeking over inert
// ops leaves the walk with the same state, the same patches and the same
// insert position for every change op as stepping over them one by one: the
// flush of the previous key that the first inert op of a new key would have
// done happens at the next op the walk does visit, which is in a later key.
struct MapSkip<'a> {
    op_set: &'a OpSet,
    /// End of the object's ops.
    end: usize,
    /// Sorted ranges of the keys a change op writes or a pred target is in.
    hot: Vec<Range<usize>>,
    /// Sorted positions of the object's doc ops in the pred cache.
    preds: Vec<usize>,
}

impl<'a> MapSkip<'a> {
    /// Below this many doc ops per change op, stepping over all of them
    /// costs no more than seeking.
    const MIN_OPS_PER_CHANGE_OP: usize = 32;

    fn new(
        op_set: &'a OpSet,
        obj: Range<usize>,
        change_ops: &[ChangeOp],
        preds: &[usize],
    ) -> Option<Self> {
        if change_ops.len().saturating_mul(Self::MIN_OPS_PER_CHANGE_OP) > obj.len() {
            return None;
        }
        let first = preds.partition_point(|&p| p < obj.start);
        let last = preds.partition_point(|&p| p < obj.end);
        let preds = preds[first..last].to_vec();
        let mut hot = Vec::new();
        let mut last_key = None;
        for op in change_ops {
            let KeyRef::Map(key) = op.key() else {
                return None;
            };
            if last_key != Some(key) {
                hot.push(op_set.key_range(key, obj.clone()));
                last_key = Some(key);
            }
        }
        for &pos in &preds {
            let (key, _) = op_set.key_and_id_at(pos)?;
            hot.push(op_set.key_range(&key?, obj.clone()));
        }
        hot.retain(|range| !range.is_empty());
        hot.sort_by_key(|range| (range.start, range.end));
        hot.dedup();
        Some(Self {
            op_set,
            end: obj.end,
            hot,
            preds,
        })
    }

    /// The first op in `from..bound` the walk must visit, else `bound`.
    fn next_stop(&self, from: usize, bound: usize) -> usize {
        let mut stop = bound;
        if let Some(&pos) = self.preds.get(self.preds.partition_point(|&p| p < from)) {
            stop = stop.min(pos);
        }
        let first = self.hot.partition_point(|range| range.end <= from);
        for range in &self.hot[first..] {
            if range.start >= stop {
                break;
            }
            let scan = range.start.max(from)..range.end.min(stop);
            if let Some(pos) = self.op_set.next_visible(scan) {
                stop = pos;
            }
        }
        stop
    }

    /// Where the full walk stops for change op `(key, id)` starting at
    /// `from`: the first op that sorts after it.
    fn insertion_point(&self, from: usize, key: &str, id: OpId) -> Option<usize> {
        let keyed = self.op_set.key_range(key, from..self.end);
        let id_at = |pos| self.op_set.key_and_id_at(pos).map(|(_, id)| id);
        let (mut lo, mut hi) = (keyed.start, keyed.end);
        // Usually the change op is the newest of its key.
        if lo < hi && id_at(hi - 1)? <= id {
            lo = hi;
        }
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if id_at(mid)? <= id {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        self.checked(from, lo, |k, i| k < key || (k == key && i <= id))
    }

    /// Where `finish`'s walk of the key `key` starting at `from` stops.
    fn key_end(&self, from: usize, key: &str) -> Option<usize> {
        let keyed = self.op_set.key_range(key, from..self.end);
        let end = if keyed.is_empty() { from } else { keyed.end };
        self.checked(from, end, |k, _| k == key)
    }

    /// `bound` if the op before it is still one the walk takes and the op
    /// at it is not; `None` (walk op by op) if the columns say otherwise.
    fn checked(
        &self,
        from: usize,
        bound: usize,
        takes: impl Fn(&str, OpId) -> bool,
    ) -> Option<usize> {
        let takes_at = |pos| match self.op_set.key_and_id_at(pos) {
            Some((Some(key), id)) => Some(takes(&key, id)),
            _ => None,
        };
        if bound > from && !takes_at(bound - 1)? {
            return None;
        }
        if bound < self.end && takes_at(bound)? {
            return None;
        }
        Some(bound)
    }
}

#[derive(Debug, Clone)]
enum Adjust {
    Conflict(usize),
    Expose(usize),
}

#[derive(PartialEq, Debug)]
enum Top {
    Nothing,
    ChangeIndex(usize),
    Doc(usize),
    Expose(usize),
}

impl Top {
    fn reset(&mut self, conflicts: &mut Vec<Adjust>) {
        if let Top::Expose(i) = self {
            conflicts.push(Adjust::Expose(*i));
        }
        *self = Top::Nothing;
    }

    fn process_doc_op(&mut self, ops: &mut [ChangeOp], d: &Op<'_>, deleted: bool) {
        if d.visible() {
            if deleted {
                if let Top::Doc(i) = self {
                    *self = Top::Expose(*i)
                }
            } else {
                if let Top::ChangeIndex(i) = self {
                    ops[*i].conflicted = true
                }
                *self = Top::Doc(d.pos);
            }
        }
    }

    fn process_change_op(&mut self, conflicts: &mut Vec<Adjust>, ops: &mut [ChangeOp], pos: usize) {
        if ops[pos].visible() {
            match self {
                Top::ChangeIndex(i) => ops[*i].conflicted = true,
                Top::Doc(i) => conflicts.push(Adjust::Conflict(*i)),
                _ => {}
            }
            *self = Top::ChangeIndex(pos);
        }
    }
}

impl<'a, 'b> MapWalker<'a, 'b> {
    fn new(
        obj: ObjId,
        mut ops: OpIter<'a>,
        text_encoding: TextEncoding,
        pred: &'b mut PredCache,
        succ: &'b mut Vec<SuccInsert>,
        log: &'b mut PatchLog,
        conflicts: &'b mut Vec<Adjust>,
        skip: Option<MapSkip<'a>>,
    ) -> Self {
        let pos = ops.pos();
        let doc_op = ops.next();
        let value = ValueState::new(obj, SequenceType::List, text_encoding);
        let top = Top::Nothing;
        MapWalker {
            ops,
            log,
            pos,
            doc_op,
            pred,
            succ,
            value,
            conflicts,
            top,
            skip,
        }
    }

    fn next_doc_op(&mut self) {
        self.pos = self.ops.pos();
        self.doc_op = self.ops.next();
    }

    // TILO(map-apply-skip): the full walk stops at `bound`; seek to the
    // first op before it the walk must visit.
    fn skip_inert(&mut self, bound: Option<usize>) {
        let (Some(skip), Some(bound), Some(_)) = (&self.skip, bound, &self.doc_op) else {
            return;
        };
        let stop = skip.next_stop(self.pos, bound);
        if stop > self.pos {
            self.ops = skip.op_set.iter_range(&(stop..skip.end));
            self.next_doc_op();
        }
    }

    // TILO(map-apply-skip): `bound` as `find` computes it from the walk's
    // position; without one the walk goes op by op from here on.
    fn skip_bound(
        &mut self,
        find: impl FnOnce(&MapSkip<'a>, usize) -> Option<usize>,
    ) -> Option<usize> {
        let bound = match (&self.skip, &self.doc_op) {
            (Some(skip), Some(_)) => find(skip, self.pos),
            _ => None,
        };
        if bound.is_none() {
            self.skip = None;
        }
        bound
    }

    fn change_op(&mut self, ops: &mut [ChangeOp], pos: usize) {
        if let Some(mut successors) = self.pred.remove(&ops[pos].id()) {
            let is_counter = matches!(ops[pos].bld.value, OpScalarValue::Counter(_));
            normalize_increment_successors(is_counter, &mut successors);
            ops[pos].succ = successors
        }

        self.advance_doc_op(pos, ops);

        if ops[pos].prop() != self.value.key {
            self.value.map_flush(self.log);
            self.value.key = ops[pos].prop_static();
            self.top.reset(self.conflicts);
        }
        self.value.process_change_op(&ops[pos]);

        ops[pos].pos = Some(self.pos);

        self.top.process_change_op(self.conflicts, ops, pos);
    }

    fn advance_doc_op(&mut self, pos: usize, ops: &mut [ChangeOp]) {
        // TILO(map-apply-skip)
        let bound = self.skip_bound(|skip, from| match ops[pos].key() {
            KeyRef::Map(key) => skip.insertion_point(from, key, ops[pos].id()),
            KeyRef::Seq(_) => None,
        });
        self.skip_inert(bound);
        while let Some(d) = self.doc_op.as_ref() {
            // TODO - sometimes we can fast forward to the next property
            match d.key.partial_cmp(ops[pos].key()) {
                Some(Ordering::Greater) => break,
                Some(Ordering::Equal) if d.id > ops[pos].id() => break,
                _ => {
                    let deleted = process_pred(self.doc_op.as_ref(), self.pred, self.succ);
                    if d.prop() != self.value.key {
                        self.value.map_flush(self.log);
                        self.value.key = d.prop();
                        self.top.reset(self.conflicts);
                    }
                    self.value.process_doc_op(d, deleted);
                    self.top.process_doc_op(ops, d, deleted);
                }
            }
            self.next_doc_op();
            self.skip_inert(bound); // TILO(map-apply-skip)
        }
    }

    fn finish(&mut self, ops: &mut [ChangeOp]) {
        // TILO(map-apply-skip)
        let key = match &self.value.key {
            Some(PropRef::Map(key)) => Some(key.clone()),
            _ => None,
        };
        let bound = self.skip_bound(|skip, from| skip.key_end(from, key.as_deref()?));
        self.skip_inert(bound);
        while let Some(d) = self.doc_op.as_ref() {
            let deleted = process_pred(self.doc_op.as_ref(), self.pred, self.succ);
            if d.prop() == self.value.key {
                self.top.process_doc_op(ops, d, deleted);
                self.value.process_doc_op(d, deleted);
                self.next_doc_op();
                self.skip_inert(bound); // TILO(map-apply-skip)
            } else {
                break;
            }
        }
        self.value.map_flush(self.log);
        self.top.reset(self.conflicts);
    }
}

fn normalize_increment_successors(is_counter: bool, successors: &mut [(OpId, Option<i64>)]) {
    if !is_counter {
        for (_, increment) in successors {
            // Increment operations preserve and update counter predecessors,
            // but act as ordinary overwrites for non-counter predecessors.
            if increment.is_some() {
                *increment = None;
            }
        }
    }
}

fn process_pred(doc_op: Option<&Op<'_>>, pred: &mut PredCache, succ: &mut Vec<SuccInsert>) -> bool {
    if let Some(d) = doc_op {
        let mut deleted = false;
        if let Some(mut successors) = pred.remove(&d.id) {
            normalize_increment_successors(d.is_counter(), &mut successors);
            for (id, inc) in successors {
                deleted |= inc.is_none();
                succ.push(d.add_succ(id, inc));
            }
        }
        deleted
    } else {
        false
    }
}

#[derive(Debug, Clone)]
struct ValueState<'a> {
    obj: ObjId,
    seq_type: SequenceType,
    text_encoding: TextEncoding,
    key: Option<PropRef<'a>>,
    doc: OpValueOption,
    change: OpValueOption,
    marks: RichTextDiff<'a>,
}

#[derive(Debug, Clone)]
struct OpValue {
    id: OpId,
    value: Value,
    deleted: bool,
    conflict: bool,
    expose: bool,
    replaced: Option<Value>,
}

#[derive(Debug, Default, Clone)]
struct OpValueOption(Option<OpValue>);

impl OpValueOption {
    fn id(&self) -> Option<OpId> {
        self.value().map(|o| o.id)
    }

    fn increment(&mut self, n: i64) {
        if let Self(Some(ov)) = self {
            if let Value::Scalar(ScalarValue::Counter(c)) = &mut ov.value {
                c.increment(n);
            }
        }
    }

    fn expose(&mut self, replaced: Value) {
        if let Self(Some(ov)) = self {
            ov.expose = true;
            ov.replaced = Some(replaced);
        }
    }

    fn set(&mut self, value: Value, id: OpId, deleted: bool) {
        if deleted && self.is_visible() {
            self.expose(value);
        } else {
            let conflict = self.is_visible();
            let expose = !deleted && self.is_deleted();
            // TILO(peer-change-check): ops come in id order, so a value that
            // follows only deleted ones was the element's value before the
            // batch as well; a text patch replaces it with itself, as `put_seq`
            // does in a list, where upstream left nothing to replace
            // (vendor/automerge/TILO-PATCH.md).
            let replaced = expose.then(|| value.clone());
            *self = Self(Some(OpValue {
                value,
                id,
                conflict,
                deleted,
                expose,
                replaced,
            }));
        }
    }

    fn is_none(&self) -> bool {
        self.value().is_none()
    }

    fn value(&self) -> Option<&OpValue> {
        self.0.as_ref()
    }

    fn is_visible(&self) -> bool {
        self.value().map(|o| !o.deleted).unwrap_or(false)
    }

    fn is_deleted(&self) -> bool {
        self.value().map(|o| o.deleted).unwrap_or(false)
    }

    fn take(&mut self) -> Self {
        Self(self.0.take())
    }

    fn into_value(self) -> Option<OpValue> {
        self.0
    }
}

impl<'a> ValueState<'a> {
    fn new(obj: ObjId, encoding: SequenceType, text_encoding: TextEncoding) -> Self {
        Self {
            obj,
            seq_type: encoding,
            text_encoding,
            key: None,
            doc: OpValueOption(None),
            change: OpValueOption(None),
            marks: RichTextDiff::default(),
        }
    }

    fn process_doc_op(&mut self, doc_op: &Op<'a>, deleted: bool) {
        match doc_op.action {
            Action::Increment => {}
            Action::Mark => {
                self.marks.before.process(doc_op.id, doc_op.action());
                self.marks.after.process(doc_op.id, doc_op.action());
            }
            _ => {
                if doc_op.visible() {
                    self.doc
                        .set(doc_op.hydrate_value(self.text_encoding), doc_op.id, deleted);
                }
            }
        }
    }

    fn do_increment(&mut self, op: &ChangeOp) {
        if self.change.is_none() {
            if let Some(id) = self.doc.id() {
                if op.pred().contains(&id) && !self.doc.is_deleted() {
                    self.change = self.doc.clone();
                }
            }
        }
        if let Some(id) = self.change.id() {
            if op.pred().contains(&id) {
                self.change.increment(op.value().as_i64());
            }
        }
    }

    fn process_mark(&mut self, id: OpId, data: Option<MarkData<'static>>) {
        if let Some(data) = data {
            self.marks.after.mark_begin(id, data);
        } else {
            self.marks.after.mark_end(id);
        }
    }

    fn process_change_op(&mut self, op: &ChangeOp) {
        match op.action() {
            Action::Delete => {}
            Action::Increment => self.do_increment(op),
            Action::Mark => self.process_mark(op.id(), op.mark_data()),
            _ => {
                if op.visible() {
                    self.change
                        .set(op.hydrate_value(self.text_encoding), op.id(), false);
                }
            }
        }
    }

    fn map_flush(&mut self, log: &mut PatchLog) {
        let obj = self.obj;
        let change = self.change.take();
        let doc = self.doc.take();
        if let Some(PropRef::Map(key)) = self.key.take() {
            Self::map_process(obj, &key, doc, change, log);
        }
    }

    fn list_flush(&mut self, index: usize, log: &mut PatchLog) {
        if self.key.take().is_none() {
            return;
        }
        let obj = self.obj;
        let encoding = self.seq_type;
        if encoding == SequenceType::List {
            match (self.doc.0.take(), self.change.0.take()) {
                (None, Some(c)) => log.insert(obj, index, c.value, c.id, c.conflict),
                (Some(d), Some(c)) if d.id == c.id => {
                    let n = c.value.as_i64() - d.value.as_i64();
                    if n != 0 {
                        log.increment_seq(obj, index, n, c.id);
                    }
                }
                (Some(d), Some(c)) if c.id < d.id => {
                    log.flag_conflict(obj, &Prop::from(index));
                }
                (Some(d), Some(c)) => {
                    let conflict = !d.deleted || c.conflict;
                    log.put_seq(obj, index, c.value, c.id, conflict, false)
                }
                (Some(d), None) => {
                    if d.expose {
                        log.put_seq(obj, index, d.value, d.id, d.conflict, true);
                    } else if d.deleted {
                        log.delete_seq(obj, index, 1);
                    }
                }
                _ => {}
            }
        } else {
            match (self.doc.0.take(), self.change.0.take()) {
                (None, Some(c)) => {
                    match c.value {
                        Value::Scalar(_) => {
                            // I don't think this branch can ever actually happen in practice. If we
                            // reach here it's because there is a non-inserting operation (i.e. an
                            // update) to the operation at `index`, but we only allow insertions
                            // into text objects. Regardless, we handle this is a splice just in
                            // case
                            log.splice(obj, index, c.value.as_str(), self.marks.current().export());
                        }
                        _ => log.insert(obj, index, c.value, c.id, c.conflict),
                    }
                }
                (Some(d), Some(c)) if d.deleted => {
                    // A text update replaces one Automerge sequence element, but that element can
                    // render as more than one unit in the configured text encoding. Express the
                    // update as a deletion and insertion so materialized text removes the full
                    // width of the old value.
                    log.replace_seq(
                        obj,
                        index,
                        &d.value,
                        c.value,
                        c.id,
                        c.conflict,
                        false,
                        self.seq_type,
                        self.text_encoding,
                        self.marks.current().export(),
                    );
                }
                (Some(d), Some(c)) if d.id == c.id => {
                    // Counter increments do not change the rendered text.
                }
                (Some(d), Some(c)) if c.id > d.id => {
                    let conflict = !d.deleted || c.conflict;
                    log.replace_seq(
                        obj,
                        index,
                        &d.value,
                        c.value,
                        c.id,
                        conflict,
                        false,
                        self.seq_type,
                        self.text_encoding,
                        self.marks.current().export(),
                    );
                }
                (Some(d), Some(_)) if !d.conflict => {
                    log.flag_conflict(obj, &Prop::from(index));
                }
                (Some(d), None) if d.expose => {
                    let replaced = d
                        .replaced
                        .clone()
                        .expect("exposed value must record the value it replaces");
                    log.replace_seq(
                        obj,
                        index,
                        &replaced,
                        d.value,
                        d.id,
                        d.conflict,
                        true,
                        self.seq_type,
                        self.text_encoding,
                        self.marks.current().export(),
                    );
                }
                (Some(d), None) if d.deleted => {
                    let w = d.value.width(self.seq_type, self.text_encoding);
                    log.delete_seq(obj, index, w);
                }
                (Some(d), None) => {
                    if let Some(m) = self.marks.current().export() {
                        log.mark(
                            obj,
                            index,
                            d.value.width(self.seq_type, self.text_encoding),
                            &m,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn map_process(
        obj: ObjId,
        key: &str,
        doc: OpValueOption,
        change: OpValueOption,
        log: &mut PatchLog,
    ) {
        match (doc.into_value(), change.into_value()) {
            (None, Some(c)) => {
                log.put_map(obj, key, c.value, c.id, c.conflict, false);
            }
            (Some(d), None) => {
                if d.expose {
                    log.put_map(obj, key, d.value, d.id, d.conflict, true);
                } else if d.deleted {
                    log.delete_map(obj, key);
                }
            }
            (Some(d), Some(c)) if c.id > d.id => {
                let conflict = (c.conflict && !d.conflict) || !d.deleted;
                log.put_map(obj, key, c.value, c.id, conflict, false);
            }
            (Some(d), Some(c)) if c.id < d.id => {
                if !d.conflict {
                    log.flag_conflict(obj, &Prop::from(key));
                }
            }
            (Some(d), Some(c)) if d.id == c.id => {
                let n = c.value.as_i64() - d.value.as_i64();
                if n != 0 {
                    log.increment_map(obj, key, n, c.id);
                }
            }
            _ => {}
        }
    }
}

fn walk_map(mw: &mut MapWalker<'_, '_>, change_ops: &mut [ChangeOp]) {
    for pos in 0..change_ops.len() {
        mw.change_op(change_ops, pos);
    }
    mw.finish(change_ops);
}

impl BatchApply {
    fn new(changes: Vec<Change>) -> Self {
        Self {
            ops: Vec::default(),
            changes,
            pred: PredCache::default(),
            obj_spans: Vec::default(),
        }
    }

    /// The actors this inserts, which the document did not have.
    fn insert_new_actors(&mut self, doc: &mut Automerge) -> Vec<ActorId> {
        let mut new = Vec::new();
        for c in self.changes.iter().filter(|c| c.seq() == 1) {
            if doc.ops().lookup_actor(c.actor_id()).is_none() {
                new.push(c.actor_id().clone());
            }
            doc.put_actor_ref(c.actor_id());
        }
        new
    }

    // TILO(peer-change-check): the ops are built and checked before anything
    // is written; the history only once the batch has passed.
    fn import_history(&mut self, doc: &mut Automerge) {
        for c in &self.changes {
            doc.update_history(c);
        }
        doc.remove_unused_actors(true);
    }

    pub(crate) fn apply(&mut self, doc: &mut Automerge, log: &mut PatchLog) -> Result<(), Refusal> {
        let new_actors = self.insert_new_actors(doc);
        // TILO(op-id-index): the check and the walks look ops up by id. (A
        // load applies its trailing changes, often none, through here.)
        if !self.changes.is_empty() {
            doc.ops_mut().index_op_ids();
        }

        // TILO(map-apply-skip): the objects this batch makes, looked up before
        // the document's own, rather than a copy of every object's info.
        let mut obj_info = ObjIndex::default();

        // TILO(peer-change-check): a batch that fails the check leaves the
        // document as it found it.
        if let Err(refused) = self.import_and_check(doc, &mut obj_info) {
            for actor in &new_actors {
                if let Some(index) = doc.ops().lookup_actor(actor) {
                    doc.remove_actor(index);
                }
            }
            return Err(Refusal::Refused(refused));
        }

        log.migrate_actors(&doc.ops().actors)?;

        self.import_history(doc);

        let mut succ = vec![];

        let mut walker = ObjWalker::new(doc.ops());

        let mut conflicts = vec![];

        // TILO(map-apply-skip): where the doc ops this batch overwrites are,
        // found once if any map walk can skip.
        let may_skip = self
            .ops
            .len()
            .saturating_mul(MapSkip::MIN_OPS_PER_CHANGE_OP)
            <= doc.ops().len();
        let mut pred_pos: Option<Vec<usize>> = None;
        let pred_positions = |pred: &PredCache| {
            let mut pos = (pred.keys())
                .filter_map(|id| doc.ops().op_id_pos(*id))
                .collect::<Vec<_>>();
            pos.sort_unstable();
            pos
        };

        for os in &self.obj_spans {
            let obj_range = walker.seek_to_obj(os.obj);
            let doc_ops = doc.ops().iter_range(&obj_range);
            let obj_type = obj_info
                .object_type(&os.obj)
                .or_else(|| doc.ops().obj_info.object_type(&os.obj));
            match obj_type {
                Some(ObjType::Map) => {
                    let skip = if may_skip {
                        let preds = pred_pos.get_or_insert_with(|| pred_positions(&self.pred));
                        MapSkip::new(
                            doc.ops(),
                            obj_range.clone(),
                            &self.ops[os.span.clone()],
                            preds,
                        )
                    } else {
                        None
                    };
                    let mut walker = MapWalker::new(
                        os.obj,
                        doc_ops,
                        doc.text_encoding(),
                        &mut self.pred,
                        &mut succ,
                        log,
                        &mut conflicts,
                        skip,
                    );
                    let change_ops = &mut self.ops[os.span.clone()];
                    walk_map(&mut walker, change_ops);
                }
                Some(otype) if otype.is_sequence() => {
                    let sequence_type = match otype {
                        ObjType::Text => SequenceType::Text,
                        ObjType::List => SequenceType::List,
                        _ => unreachable!(),
                    };
                    // TILO(seq-apply-skip)
                    if may_skip {
                        pred_pos.get_or_insert_with(|| pred_positions(&self.pred));
                    }
                    let ut = Untangler::new(
                        os.obj,
                        sequence_type,
                        doc.text_encoding(),
                        &mut conflicts,
                        &mut self.ops[os.span.clone()],
                        &mut self.pred,
                        doc_ops.end_pos(),
                    );
                    // TILO(seq-apply-skip)
                    let skip = (pred_pos.as_deref())
                        .and_then(|preds| SeqSkip::new(doc.ops(), obj_range.clone(), &ut, preds));
                    walk_list(ut, doc_ops, &mut succ, log, skip);
                }
                _ => panic!("Obj {:?} Missing from Index", os.obj),
            }
        }

        #[cfg(feature = "slow_path_assertions")]
        {
            // should always be ordered correctly - just double checking
            let mut tmp_succ = succ.clone();
            tmp_succ.sort_by(|a, b| {
                a.pos
                    .cmp(&b.pos)
                    .then_with(|| a.sub_pos.cmp(&b.sub_pos))
                    .then_with(|| a.id.counter().cmp(&b.id.counter()))
                    .then_with(|| a.id.actor().cmp(&b.id.actor()))
            });
            debug_assert_eq!(succ, tmp_succ);
        }

        for a in conflicts {
            match a {
                Adjust::Conflict(index) => doc.ops.conflict(index),
                Adjust::Expose(index) => doc.ops.expose(index),
            }
        }

        doc.ops.add_succ(&succ);

        self.insert_runs_of_ops(doc);

        debug_assert!(doc.ops.validate_op_order());
        Ok(())
    }

    fn insert_runs_of_ops(&mut self, doc: &mut Automerge) {
        let mut last_pos = None;
        let mut start = 0;
        let mut shift = 0;
        for (i, op) in self.ops.iter().enumerate() {
            if op.pos != last_pos {
                if let Some(pos) = last_pos {
                    let end = i;
                    shift += self.insert_ops(doc, pos + shift, start..end);
                    start = end;
                }
                last_pos = op.pos;
            }
        }
        if let Some(pos) = last_pos {
            self.insert_ops(doc, pos + shift, start..self.ops.len());
        }
    }

    pub(crate) fn insert_ops(&self, doc: &mut Automerge, pos: usize, range: Range<usize>) -> usize {
        let batch = &self.ops[range];
        let start = doc.ops().len();
        doc.ops_mut().splice(pos, batch);
        doc.ops().len() - start
    }

    pub(crate) fn order_ops_for_doc(&mut self) {
        self.ops.sort_by(|a, b| {
            a.bld.obj.cmp(&b.bld.obj).then_with(|| {
                match a.elemid_or_key().partial_cmp(&b.elemid_or_key()) {
                    Some(Ordering::Equal) | None => a.bld.id.cmp(&b.bld.id),
                    Some(order) => order,
                }
            })
        });
        let mut start = 0;
        let mut last_obj = None;
        for (i, o) in self.ops.iter().enumerate() {
            for p in o.pred().iter() {
                self.pred
                    .entry(*p)
                    .or_default()
                    .push((o.id(), o.get_increment_value()));
            }
            if Some(o.bld.obj) != last_obj {
                if let Some(obj) = last_obj {
                    self.obj_spans.push(ObjSpan {
                        obj,
                        span: start..i,
                    });
                }
                start = i;
                last_obj = Some(o.bld.obj);
            }
        }
        if let Some(obj) = last_obj {
            let span = start..self.ops.len();
            self.obj_spans.push(ObjSpan { obj, span });
        }
    }
}

// TILO(peer-change-check): a change is applied only if the apply can place
// every op it carries and the document it leaves saves and loads again
// (vendor/automerge/TILO-PATCH.md). Automerge itself only ever makes such
// changes; a peer can send any other. Upstream panics on some (an insert after
// an element the sequence never held, an op in an unknown object, a reference
// to an unknown actor) and silently drops ops of, or mis-records, others (an
// update of an unknown element, a pred in another key, ids below the
// change's dependencies), leaving a document whose next save no longer loads.
// The check runs on the batch's ops before anything is written, so a refused
// batch leaves the document untouched, and it holds only for what the walks
// and the save rely on, so a change Automerge makes passes it unchanged.

/// Why a batch was not applied.
pub(crate) enum Refusal {
    Log(PatchLogMismatch),
    /// `change` (an index into the batch) fails the check.
    Refused(Refused),
}

impl From<PatchLogMismatch> for Refusal {
    fn from(e: PatchLogMismatch) -> Self {
        Self::Log(e)
    }
}

pub(crate) struct Refused {
    change: usize,
    error: AutomergeError,
}

impl BatchApply {
    fn refuse(&self, change: usize, reason: &'static str) -> Refused {
        let hash = self.changes.get(change).map(Change::hash);
        Refused {
            change,
            error: AutomergeError::InvalidChange {
                hash: hash.unwrap_or(ChangeHash([0; 32])),
                reason,
            },
        }
    }

    /// The change in the batch that carries the op `id`.
    fn change_of(&self, doc: &Automerge, id: OpId) -> usize {
        let carries = |c: &Change| {
            doc.ops().lookup_actor(c.actor_id()) == Some(id.actor())
                && c.start_op().get() <= id.counter()
                && id.counter() <= c.max_op()
        };
        self.changes.iter().position(carries).unwrap_or(0)
    }

    /// Build the batch's ops into `self.ops`, its objects into `obj_info`, and
    /// order them for the walks, refusing the first change that cannot be
    /// applied.
    fn import_and_check(
        &mut self,
        doc: &mut Automerge,
        obj_info: &mut ObjIndex,
    ) -> Result<(), Refused> {
        self.check_changes(doc)?;
        let mut spans = Vec::with_capacity(self.changes.len());
        for (i, c) in self.changes.iter().enumerate() {
            let start = self.ops.len();
            doc.import_ops_to(c, &mut self.ops)
                .map_err(|error| Refused { change: i, error })?;
            spans.push(start..self.ops.len());
        }
        for o in &self.ops {
            if let Some(info) = o.obj_info() {
                obj_info.insert(o.id(), info)
            }
        }
        let mut batch_ops: Option<HashMap<OpId, usize>> = None;
        for (i, span) in spans.into_iter().enumerate() {
            for op in &self.ops[span] {
                if let Err(reason) = self.check_op(doc.ops(), obj_info, op, &mut batch_ops) {
                    return Err(self.refuse(i, reason));
                }
            }
        }
        self.order_ops_for_doc();
        for os in &self.obj_spans {
            let sequence = obj_info
                .object_type(&os.obj)
                .or_else(|| doc.ops().obj_info.object_type(&os.obj))
                .is_some_and(|t| t.is_sequence());
            let ops = &self.ops[os.span.clone()];
            if let (true, Err(id)) = (sequence, check_sequence_span(doc.ops(), os.obj, ops)) {
                let change = self.change_of(doc, id);
                return Err(self.refuse(change, "names an element its object does not hold"));
            }
        }
        Ok(())
    }

    /// Each change follows its actor's previous change, which is among its
    /// ancestors (every change Automerge makes depends on it directly), and
    /// numbers its ops after every op of its dependencies, where a saved
    /// document, which stores no start op, takes them to begin.
    fn check_changes(&self, doc: &Automerge) -> Result<(), Refused> {
        let mut last: HashMap<&ActorId, (u64, ChangeHash)> = HashMap::new();
        // The batch's changes so far: where each is, and its highest op.
        let mut earlier: HashMap<ChangeHash, (usize, u64)> = HashMap::new();
        for (i, c) in self.changes.iter().enumerate() {
            let previous = last.get(c.actor_id()).copied().or_else(|| {
                let actor = doc.ops().lookup_actor(c.actor_id())?;
                let seq = doc.change_graph.seq_for_actor(actor);
                if seq == 0 {
                    return None;
                }
                let hash = doc.change_graph.get_hash_for_actor_seq(actor, seq).ok()?;
                Some((seq, hash))
            });
            let follows = match previous {
                Some((seq, hash)) => {
                    c.seq() == seq.saturating_add(1) && self.descends(doc, &earlier, i, (hash, seq))
                }
                None => c.seq() == 1,
            };
            if !follows {
                return Err(self.refuse(i, "does not follow its actor's previous change"));
            }
            let mut deps_max = 0;
            for dep in c.deps() {
                let dep_max = (earlier.get(dep).map(|&(_, max_op)| max_op))
                    .or_else(|| doc.change_graph.max_op_for_hash(dep));
                deps_max = deps_max.max(dep_max.unwrap_or(u64::MAX));
            }
            if c.start_op().get() <= deps_max {
                return Err(self.refuse(i, "numbers its ops below ops it follows"));
            }
            // A change without ops has none to begin anywhere else.
            if c.is_empty() && c.max_op() != deps_max {
                return Err(self.refuse(i, "numbers no ops apart from those it follows"));
            }
            last.insert(c.actor_id(), (c.seq(), c.hash()));
            earlier.insert(c.hash(), (i, c.max_op()));
        }
        Ok(())
    }

    /// Change `i` of the batch has `ancestor`, its actor's change `seq`,
    /// among its ancestors; `earlier` holds the batch's changes before it.
    fn descends(
        &self,
        doc: &Automerge,
        earlier: &HashMap<ChangeHash, (usize, u64)>,
        i: usize,
        (ancestor, seq): (ChangeHash, u64),
    ) -> bool {
        let deps = self.changes[i].deps();
        if deps.contains(&ancestor) {
            return true;
        }
        let mut seen = HashSet::new();
        let mut stack = deps.to_vec();
        let mut in_doc = Vec::new();
        while let Some(hash) = stack.pop() {
            if hash == ancestor {
                return true;
            }
            if !seen.insert(hash) {
                continue;
            }
            match earlier.get(&hash) {
                Some(&(j, _)) => stack.extend_from_slice(self.changes[j].deps()),
                None => in_doc.push(hash),
            }
        }
        // An ancestor in the document is one the clock of the document's
        // changes among them covers.
        let actor = self.changes[i].actor_id();
        let Some(actor) = doc.ops().lookup_actor(actor) else {
            return false;
        };
        doc.change_graph
            .seq_clock_for_heads(&in_doc)
            .get_for_actor(&actor)
            .is_some_and(|covered| u64::from(covered.get()) >= seq)
    }

    /// The op is in an object the document or the batch has, keyed as that
    /// object is keyed; it names only ops older than itself; it overwrites
    /// only ops in its own object and key or element; and a delete overwrites
    /// something.
    fn check_op(
        &self,
        doc: &OpSet,
        obj_info: &ObjIndex,
        op: &ChangeOp,
        batch_ops: &mut Option<HashMap<OpId, usize>>,
    ) -> Result<(), &'static str> {
        let id = op.id();
        let obj = op.bld.obj;
        let older = |other: OpId| other.counter() < id.counter();
        let obj_type = obj_info
            .object_type(&obj)
            .or_else(|| doc.obj_info.object_type(&obj))
            .ok_or("is in an object the document does not hold")?;
        if !obj.is_root() && !older(obj.0) {
            return Err("is in an object made after it");
        }
        if op.insert() && !obj_type.is_sequence() {
            return Err("inserts into a map");
        }
        match op.key() {
            KeyRef::Map(_) if obj_type.is_sequence() => return Err("has a map key in a sequence"),
            KeyRef::Seq(_) if !obj_type.is_sequence() => {
                return Err("has a sequence element in a map")
            }
            KeyRef::Seq(e) if !e.is_head() && !older(e.0) => {
                return Err("names an element inserted after it")
            }
            _ => {}
        }
        if op.action() == Action::Delete && (op.insert() || op.pred().is_empty()) {
            return Err("deletes nothing");
        }
        for &pred in op.pred() {
            if !older(pred) {
                return Err("overwrites an op made after it");
            }
            let same_place = match doc.op_place(pred) {
                Some((pred_obj, key, _)) => pred_obj == obj && key == op.elemid_or_key(),
                None => {
                    let index = batch_ops.get_or_insert_with(|| {
                        self.ops
                            .iter()
                            .enumerate()
                            .map(|(i, o)| (o.id(), i))
                            .collect()
                    });
                    index
                        .get(&pred)
                        .and_then(|&i| self.ops.get(i))
                        .is_some_and(|p| {
                            p.bld.obj == obj && p.elemid_or_key() == op.elemid_or_key()
                        })
                }
            };
            if !same_place {
                return Err("overwrites an op outside its key or element");
            }
        }
        Ok(())
    }
}

/// TILO(peer-change-check): `Untangler::new` places an op on an element it
/// neither inserts nor updates right after its insert in the batch only when
/// the walk meets that element's insert op in the document; the op that
/// names any other element is never placed. `Err` is the first such op.
fn check_sequence_span(doc: &OpSet, obj: ObjId, ops: &[ChangeOp]) -> Result<(), OpId> {
    let mut inserted: HashSet<ElemId> = HashSet::new();
    let mut last_e = None;
    for op in ops {
        let KeyRef::Seq(e) = op.key() else {
            return Err(op.id());
        };
        let placed_in_batch = if op.insert() {
            e.is_head() || inserted.contains(e)
        } else {
            last_e == Some(*e)
        };
        if !placed_in_batch && !is_element(doc, obj, *e) {
            return Err(op.id());
        }
        if op.insert() {
            inserted.insert(ElemId(op.id()));
            last_e = Some(ElemId(op.id()));
        }
    }
    Ok(())
}

/// The document holds `e` as an element of `obj`.
fn is_element(doc: &OpSet, obj: ObjId, e: ElemId) -> bool {
    !e.is_head()
        && doc
            .op_place(e.0)
            .is_some_and(|(o, _, insert)| insert && o == obj)
}

#[derive(Debug, Clone)]
struct ObjWalker<'a> {
    iter: ObjIdIter<'a>,
}

impl<'a> ObjWalker<'a> {
    fn new(ops: &'a OpSet) -> Self {
        let iter = ops.obj_id_iter();
        Self { iter }
    }

    fn seek_to_obj(&mut self, obj: ObjId) -> Range<usize> {
        self.iter.seek_to_value(obj)
    }
}

#[derive(Debug, Clone, Default)]
struct ObjSpan {
    obj: ObjId,
    span: Range<usize>,
}

impl Automerge {
    pub fn apply_changes_batch(
        &mut self,
        changes: impl IntoIterator<Item = Change> + Clone,
    ) -> Result<(), AutomergeError> {
        self.apply_changes_batch_log_patches(changes, &mut PatchLog::inactive())
    }

    pub fn apply_changes_batch_log_patches<I: IntoIterator<Item = Change>>(
        &mut self,
        changes: I,
        log: &mut PatchLog,
    ) -> Result<(), AutomergeError> {
        // TILO(peer-change-check): the queue as it was, to restore.
        let queued: Vec<ChangeHash> = self.queue.iter().map(Change::hash).collect();
        let mut seen: HashSet<ChangeHash> = queued.iter().copied().collect();
        let mut actor_seqs: HashMap<ActorId, HashSet<u64>> = HashMap::new();
        let mut actor_author: HashSet<ActorId> = HashSet::new();

        for change in self.queue.iter() {
            actor_seqs
                .entry(change.actor_id().clone())
                .or_default()
                .insert(change.seq());
            if change.author().is_some() {
                actor_author.insert(change.actor_id().clone());
            }
        }

        let mut incoming = ChangeBatch::new();
        for change in changes {
            let hash = change.hash();
            if self.change_graph.has_change(&hash) || seen.contains(&hash) {
                continue;
            }
            if self.has_actor_seq(&change) {
                self.queue
                    .remove_actor_branch_from(change.actor_id(), change.seq().saturating_add(1));
                return Err(AutomergeError::duplicate_seq(&change));
            }
            if actor_seqs
                .get(change.actor_id())
                .is_some_and(|seqs| seqs.contains(&change.seq()))
            {
                return Err(AutomergeError::duplicate_seq(&change));
            }
            // TODO(finto): Refactor with BatchBuilder pattern
            if change.author().is_some()
                && (self.get_author_for_actor(change.actor_id()).is_some()
                    || actor_author.contains(change.actor_id()))
            {
                return Err(AutomergeError::duplicate_author(&change));
            }

            seen.insert(hash);
            actor_seqs
                .entry(change.actor_id().clone())
                .or_default()
                .insert(change.seq());
            if change.author().is_some() {
                actor_author.insert(change.actor_id().clone());
            }
            incoming.push(change)?;
        }

        self.queue.extend(incoming);
        let mut changes = self.queue.pop_topo_sorted_ready(&self.change_graph);
        // TILO(peer-change-check): a refused change is never applied. If it,
        // or a change that depends on it, came with this call, nothing is
        // applied and the queue is as it was; one that waited in the queue
        // from an earlier call is dropped with its dependents, so that it
        // cannot hold back the changes that arrive after it.
        loop {
            let mut batch = BatchApply::new(changes);
            let refused = match batch.apply(self, log) {
                Ok(()) => return Ok(()),
                Err(Refusal::Log(e)) => return Err(e.into()),
                Err(Refusal::Refused(refused)) => refused,
            };
            let mut ready = batch.changes;
            let mut dropped = HashSet::new();
            for (i, c) in ready.iter().enumerate() {
                if i == refused.change || c.deps().iter().any(|d| dropped.contains(d)) {
                    dropped.insert(c.hash());
                }
            }
            let waited: HashSet<&ChangeHash> = queued.iter().collect();
            if dropped.iter().any(|hash| !waited.contains(hash)) {
                self.queue.restore(&queued, ready);
                return Err(refused.error);
            }
            ready.retain(|c| !dropped.contains(&c.hash()));
            changes = ready;
        }
    }

    fn import_ops_to(
        &mut self,
        change: &Change,
        ops: &mut Vec<ChangeOp>,
    ) -> Result<(), AutomergeError> {
        let new_ops = self.import_ops(change)?;
        ops.extend(new_ops);
        Ok(())
    }

    fn import_ops(&mut self, change: &Change) -> Result<Vec<ChangeOp>, AutomergeError> {
        // TILO(peer-change-check): a change may name an actor the document
        // does not have.
        let actors: Vec<_> = change
            .actors()
            .map(|a| {
                self.ops
                    .lookup_actor(a)
                    .ok_or(AutomergeError::InvalidChange {
                        hash: change.hash(),
                        reason: "names an actor the document does not have",
                    })
            })
            .collect::<Result<_, _>>()?;

        change
            .iter_ops()
            .enumerate()
            .map(|(i, c)| {
                let id = OpId::new(change.start_op().get() + i as u64, 0).map(&actors)?;
                let key = c.key.map(&actors)?;
                let obj = c.obj.map(&actors)?;
                let pred = c
                    .pred
                    .into_iter()
                    .map(|id| id.map(&actors))
                    .collect::<Result<Vec<_>, _>>()?;
                let bld = OpBuilder {
                    id,
                    obj,
                    key,
                    action: c.action.try_into().map_err(AutomergeError::encoding)?,
                    value: c.val.into_ref(),
                    mark_name: c.mark_name.map(String::from).map(Cow::Owned),
                    expand: c.expand,
                    insert: c.insert,
                    pred,
                };
                let change = ChangeOp {
                    pos: None,
                    subsort: 0,
                    conflicted: false,
                    succ: vec![],
                    bld,
                };
                Ok(change)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::{ExpandMark, Mark};
    use crate::read::ReadDoc;
    use crate::transaction::Transactable;
    use crate::types;
    use crate::{make_rng, ActorId, AutoCommit, ROOT};
    use rand::prelude::*;

    /// InsertQuery::resolve had a bug where an increment op before a trailing
    /// insert made the insert resolve to the wrong position. That corrupted the
    /// op grouping so that reconstructing the change via get_changes wrote two
    /// ops into the same column slot.
    #[test]
    fn increment_plus_trailing_insert_keeps_group_order() {
        let mut doc = AutoCommit::new().with_actor("aa".try_into().unwrap());
        let list = doc.put_object(&ROOT, "list", crate::ObjType::List).unwrap();
        doc.insert(&list, 0, crate::ScalarValue::counter(5))
            .unwrap();
        let heads = doc.get_heads();

        let mut f = doc.fork().with_actor("bb".try_into().unwrap());
        f.increment(&list, 0, 1).unwrap();
        f.insert(&list, 1, "x").unwrap();

        doc.merge(&mut f).unwrap();

        assert_eq!(doc.length(&list), 2);
        assert_eq!(
            doc.get(&list, 0).unwrap().map(|(v, _)| v),
            Some(crate::Value::counter(6))
        );
        assert_eq!(doc.get(&list, 1).unwrap().map(|(v, _)| v), Some("x".into()));

        let changes = doc.doc.get_changes(&heads);
        let mut replayed = doc.fork_at(&heads).unwrap();
        replayed.apply_changes(changes).unwrap();
        replayed.doc.debug_cmp(&doc.doc);
    }

    impl AutoCommit {
        fn apply_changes_iter(
            &mut self,
            changes: impl IntoIterator<Item = Change> + Clone,
        ) -> Result<(), AutomergeError> {
            let changes = changes.into_iter().collect::<Vec<_>>();
            for c in changes {
                self.apply_changes([c])?;
            }
            self.validate_top_index();
            Ok(())
        }
    }

    #[test]
    fn map_batch_apply() {
        let actor3 = ActorId::try_from("aaaaaa").unwrap();
        let actor2 = ActorId::try_from("bbbbbb").unwrap();
        let actor1 = ActorId::try_from("cccccc").unwrap();

        let mut doc1 = AutoCommit::new().with_actor(actor1);
        let map1 = doc1.put_object(&ROOT, "map", ObjType::Map).unwrap();
        doc1.put(&map1, "key1", "val1").unwrap();
        doc1.put(&map1, "key2", "val2").unwrap();

        let heads1 = doc1.get_heads();

        let mut doc2 = doc1.fork().with_actor(actor2);
        doc2.put(&map1, "key1", "val3a").unwrap();
        doc2.put(&map1, "key1", "val3a.1").unwrap();
        doc2.put(&map1, "key1", "val3a.2").unwrap();
        doc2.delete(&map1, "key2").unwrap();
        doc2.put(&map1, "key3", "val4a").unwrap();
        let map2 = doc2.put_object(&map1, "map2", ObjType::Map).unwrap();
        doc2.put(&map2, "key1", "val5a").unwrap();

        let map3 = doc1.put_object(&map1, "map3", ObjType::Map).unwrap();
        doc1.put(&map1, "key1", "val6a").unwrap();
        doc1.put(&map3, "key1", "val7a").unwrap();

        let mut doc3 = doc1.fork().with_actor(actor3);
        doc3.put(&map1, "key1", "val3b").unwrap();
        doc3.put(&map1, "key3", "val4b").unwrap();

        let mut doc1_test = doc1.fork();
        let mut changes2 = doc2.get_changes(&heads1);

        let changes3 = doc3.get_changes(&heads1);
        changes2.extend(changes3);

        doc1.apply_changes_iter(changes2.clone()).unwrap();
        doc1_test.doc.apply_changes_batch(changes2.clone()).unwrap();
        doc1_test.validate_top_index();

        doc1.dump();
        doc1_test.dump();
        doc1.doc.debug_cmp(&doc1_test.doc);
    }

    #[test]
    fn list_batch_apply() {
        let actor3 = ActorId::try_from("aaaaaa").unwrap();
        let actor2 = ActorId::try_from("bbbbbb").unwrap();
        let actor1 = ActorId::try_from("cccccc").unwrap();

        let mut doc1 = AutoCommit::new().with_actor(actor1);
        let list = doc1.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc1.insert(&list, 0, "val1").unwrap();
        doc1.insert(&list, 1, "val2").unwrap();
        doc1.insert(&list, 2, "val3").unwrap();

        let heads1 = doc1.get_heads();

        let mut doc2 = doc1.fork().with_actor(actor2);
        doc2.insert(&list, 1, "val4a").unwrap();
        doc2.insert(&list, 1, "val4b").unwrap();
        doc2.insert(&list, 2, "val4c").unwrap();
        doc2.insert(&list, 0, "val4d").unwrap();
        doc2.insert(&list, 0, "val4e").unwrap();
        doc2.insert(&list, 0, "val4f").unwrap();

        let mut doc3 = doc1.fork().with_actor(actor3);
        doc3.insert(&list, 1, "val5a").unwrap();
        doc3.insert(&list, 1, "val5b").unwrap();
        doc3.insert(&list, 2, "val5c").unwrap();
        doc3.insert(&list, 3, "val5d").unwrap();
        doc3.insert(&list, 1, "val5e").unwrap();
        doc3.insert(&list, 1, "val5f").unwrap();
        doc3.insert(&list, 0, "val5g").unwrap();
        doc3.insert(&list, 0, "val5h").unwrap();

        let mut doc1_test = doc1.fork();
        let mut changes2 = doc2.get_changes(&heads1);
        let changes3 = doc3.get_changes(&heads1);
        changes2.extend(changes3);

        doc1.apply_changes_iter(changes2.clone()).unwrap();
        doc1_test.doc.apply_changes_batch(changes2.clone()).unwrap();
        doc1_test.validate_top_index();

        doc1.dump();
        doc1_test.dump();

        doc1.doc.debug_cmp(&doc1_test.doc);
    }

    #[test]
    fn text_batch_apply() {
        let actor3 = ActorId::try_from("aaaaaa").unwrap();
        let actor2 = ActorId::try_from("bbbbbb").unwrap();
        let actor1 = ActorId::try_from("cccccc").unwrap();

        let mut doc1 = AutoCommit::new().with_actor(actor1);
        let text = doc1.put_object(&ROOT, "text", ObjType::Text).unwrap();
        doc1.splice_text(&text, 0, 0, "the quick fox jumped over the lazy dog")
            .unwrap();

        let heads1 = doc1.get_heads();

        let mut doc2 = doc1.fork().with_actor(actor2);
        doc2.splice_text(&text, 0, 0, "abc").unwrap();

        let mut doc3 = doc1.fork().with_actor(actor3);
        doc3.splice_text(&text, 3, 1, "aalks").unwrap();

        let mut doc1_test = doc1.fork();
        let mut changes2 = doc2.get_changes(&heads1);
        let changes3 = doc3.get_changes(&heads1);
        changes2.extend(changes3);

        doc1.apply_changes_iter(changes2.clone()).unwrap();
        doc1_test.apply_changes_batch(changes2.clone()).unwrap();
        doc1_test.validate_top_index();

        doc1.dump();
        doc1_test.dump();

        doc1.doc.debug_cmp(&doc1_test.doc);
        //assert_eq!(doc1.save(), doc1_test.save());
    }

    #[test]
    fn multi_put_batch_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let list = doc1.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc1.insert(&list, 0, "a").unwrap();
        doc1.insert(&list, 1, "b").unwrap();
        doc1.insert(&list, 2, "c").unwrap();
        let heads = doc1.get_heads();

        let mut doc2 = doc1.fork().with_actor(rng.random());
        for i in 0..10 {
            let mut tmp = doc1.fork().with_actor(rng.random());
            tmp.put(&list, 0, i).unwrap();
            doc2.merge(&mut tmp).unwrap();
        }
        let changes = doc2.get_changes(&heads);
        doc1.apply_changes_batch(changes).unwrap();
        doc1.validate_top_index();
        assert_eq!(doc1.save(), doc2.save());
    }

    #[test]
    fn multi_insert_batch_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let list = doc1.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc1.insert(&list, 0, "a").unwrap();
        doc1.insert(&list, 1, "b").unwrap();
        doc1.insert(&list, 2, "c").unwrap();
        let heads = doc1.get_heads();

        let mut doc2 = doc1.fork().with_actor(rng.random());

        for i in 0..10 {
            let mut tmp = doc1.fork().with_actor(rng.random());
            tmp.insert(&list, 1, i).unwrap();
            //let change = tmp.get_last_local_change().unwrap();
            doc2.merge(&mut tmp).unwrap();
        }

        let changes = doc2.get_changes(&heads);
        doc1.apply_changes_batch(changes).unwrap();
        doc1.validate_top_index();
        assert_eq!(doc1.save(), doc2.save());
    }

    #[test]
    fn multi_update_batch_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let list = doc1.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc1.insert(&list, 0, "a").unwrap();
        doc1.insert(&list, 1, "b").unwrap();
        doc1.insert(&list, 2, "c").unwrap();
        let heads = doc1.get_heads();

        let mut doc2 = doc1.fork().with_actor(rng.random());

        for i in 0..3 {
            let mut tmp = doc1.fork().with_actor(rng.random());
            tmp.put(&list, 2, i).unwrap();
            doc2.merge(&mut tmp).unwrap();
        }

        let changes = doc2.get_changes(&heads);
        doc1.apply_changes_batch(changes).unwrap();
        doc1.validate_top_index();
        assert_eq!(doc1.save(), doc2.save());
    }

    #[test]
    fn fuzz_batch_list_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let list = doc1.put_object(&ROOT, "list", ObjType::List).unwrap();
        doc1.insert(&list, 0, "a").unwrap();
        doc1.insert(&list, 1, "b").unwrap();
        doc1.insert(&list, 2, "c").unwrap();
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let heads = doc1.get_heads();

        let mut doc1_tmp = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());

        for _ in 0..3 {
            for _ in 0..30 {
                let mut tmp = doc1_tmp.fork().with_actor(rng.random());
                let num_inserts = rng.random::<u32>() % 10 + 1;
                let num_updates = rng.random::<u32>() % 10 + 1;
                let num_deletes = rng.random::<u32>() % 2;
                for _ in 0..num_inserts {
                    let len = tmp.length(&list) as u32;
                    let pos = rng.random::<u32>() % len;
                    tmp.insert(&list, pos as usize, val()).unwrap();
                }
                for _ in 0..num_updates {
                    let len = tmp.length(&list) as u32;
                    let pos = rng.random::<u32>() % len;
                    tmp.put(&list, pos as usize, val()).unwrap();
                }
                for _ in 0..num_deletes {
                    let len = tmp.length(&list) as u32;
                    let pos = rng.random::<u32>() % len;
                    tmp.delete(&list, pos as usize).unwrap();
                }
                doc2.merge(&mut tmp).unwrap();
            }
            doc1_tmp.merge(&mut doc2).unwrap();
        }

        let changes = doc2.get_changes(&heads);
        doc1.apply_changes_batch(changes).unwrap();
        doc1.validate_top_index();
        assert_eq!(doc1.save(), doc2.save());
    }

    #[test]
    fn fuzz_batch_map1_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let map1 = doc1.put_object(&ROOT, "map1", ObjType::Map).unwrap();
        let map2 = doc1.put_object(&map1, "map2", ObjType::Map).unwrap();
        let map3 = doc1.put_object(&map2, "map3", ObjType::Map).unwrap();
        let maps = [map1, map2, map3];
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let heads = doc1.get_heads();

        let mut doc1_tmp = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());

        for _ in 0..3 {
            for _ in 0..30 {
                let mut tmp = doc1_tmp.fork().with_actor(rng.random());
                let num_updates = rng.random::<u32>() % 10 + 1;
                let num_deletes = rng.random::<u32>() % 2;
                for _ in 0..num_updates {
                    let key = format!("key{}", rng.random::<u32>() % 20);
                    let map = rng.random::<u32>() % (maps.len() as u32);
                    tmp.put(&maps[map as usize], key, val()).unwrap();
                }
                for _ in 0..num_deletes {
                    let key = format!("key{}", rng.random::<u32>() % 20);
                    let map = rng.random::<u32>() % (maps.len() as u32);
                    tmp.delete(&maps[map as usize], key).unwrap();
                }
                doc2.merge(&mut tmp).unwrap();
            }
            doc1_tmp.merge(&mut doc2).unwrap();
        }

        let changes = doc2.get_changes(&heads);
        doc1.apply_changes_batch(changes).unwrap();
        doc1.validate_top_index();
        assert_eq!(doc1.save(), doc2.save());
    }

    #[test]
    fn fuzz_batch_map2_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let map1 = doc1.put_object(&ROOT, "map1", ObjType::Map).unwrap();
        let map2 = doc1.put_object(&map1, "map2", ObjType::Map).unwrap();
        let map3 = doc1.put_object(&map2, "map3", ObjType::Map).unwrap();
        let maps = [map1, map2, map3];
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let heads = doc1.get_heads();

        let mut doc1_tmp = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());

        for _ in 0..3 {
            for _ in 0..30 {
                let mut tmp = doc1_tmp.fork().with_actor(rng.random());
                let num_updates = rng.random::<u32>() % 10 + 1;
                let num_deletes = rng.random::<u32>() % 2;
                for _ in 0..num_updates {
                    let key = format!("key{}", rng.random::<u32>() % 1000);
                    let map = rng.random::<u32>() % (maps.len() as u32);
                    tmp.put(&maps[map as usize], key, val()).unwrap();
                }
                for _ in 0..num_deletes {
                    let key = format!("key{}", rng.random::<u32>() % 1000);
                    let map = rng.random::<u32>() % (maps.len() as u32);
                    tmp.delete(&maps[map as usize], key).unwrap();
                }
                doc2.merge(&mut tmp).unwrap();
            }
            doc1_tmp.merge(&mut doc2).unwrap();
        }

        let changes = doc2.get_changes(&heads);

        let mut doc_a = doc1;
        let mut doc_b = doc_a.clone();

        doc_a.update_diff_cursor();
        doc_a.apply_changes_batch(changes.clone()).unwrap();
        doc_a.validate_top_index();
        doc_b.apply_changes_iter(changes).unwrap();

        let final_heads = doc_a.get_heads();

        assert_eq!(doc_a.save(), doc_b.save());

        let pa = doc_a.diff_incremental();
        let pb = doc_b.diff(&heads, &final_heads);

        let len = std::cmp::max(pa.len(), pb.len());

        for i in 0..len {
            if pa.get(i) != pb.get(i) {
                log!(" i={} ", i);
                log!(" pa={:?}", pa.get(i));
                log!(" pb={:?}", pb.get(i));
            }
        }

        if pa != pb {
            panic!()
        }
    }

    #[test]
    fn fuzz_batch_map_counter_apply() {
        let mut rng = make_rng();
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let map1 = doc1.put_object(&ROOT, "map1", ObjType::Map).unwrap();
        doc1.put(&map1, "key1", ScalarValue::counter(10)).unwrap();
        doc1.increment(&map1, "key1", 15).unwrap();
        doc1.increment(&map1, "key1", 10).unwrap();
        let map2 = doc1.put_object(&map1, "map2", ObjType::Map).unwrap();
        doc1.put(&map2, "key1", ScalarValue::counter(100)).unwrap();
        doc1.increment(&map2, "key1", 20).unwrap();
        doc1.increment(&map2, "key1", 1).unwrap();
        doc1.delete(&map2, "key1").unwrap();
        doc1.put(&map2, "key1", ScalarValue::counter(101)).unwrap();
        doc1.increment(&map2, "key1", 1).unwrap();
        let map3 = doc1.put_object(&map2, "map3", ObjType::Map).unwrap();
        doc1.put(&map3, "key1", ScalarValue::counter(1000)).unwrap();
        doc1.increment(&map3, "key1", 30).unwrap();
        let maps = [map1, map2, map3];
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let heads = doc1.get_heads();

        let mut doc1_tmp = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());

        for _ in 0..4 {
            for _ in 0..30 {
                let mut tmp = doc1_tmp.fork().with_actor(rng.random());
                let num_updates = rng.random::<u32>() % 10 + 1;
                let num_deletes = rng.random::<u32>() % 2;
                for _ in 0..num_updates {
                    let key = format!("key{}", rng.random::<u32>() % 30);
                    let map = rng.random::<u32>() % (maps.len() as u32);
                    if let Ok(Some((
                        types::Value::Scalar(Cow::Owned(ScalarValue::Counter(_))),
                        _,
                    ))) = tmp.get(&maps[map as usize], &key)
                    {
                        tmp.increment(&maps[map as usize], key, val()).unwrap();
                    } else {
                        tmp.put(&maps[map as usize], key, ScalarValue::counter(val()))
                            .unwrap();
                    }
                }
                for _ in 0..num_deletes {
                    let key = format!("key{}", rng.random::<u32>() % 30);
                    let map = rng.random::<u32>() % (maps.len() as u32);
                    tmp.delete(&maps[map as usize], key).unwrap();
                }
                doc2.merge(&mut tmp).unwrap();
            }
            doc1_tmp.merge(&mut doc2).unwrap();
        }

        let changes = doc2.get_changes(&heads);

        let mut doc_a = doc1;
        let mut doc_b = doc_a.clone();

        doc_a.update_diff_cursor();
        doc_a.apply_changes_batch(changes.clone()).unwrap();
        doc_a.validate_top_index();

        doc_b.apply_changes_iter(changes).unwrap();

        let final_heads = doc_a.get_heads();

        assert_eq!(doc_a.save(), doc_b.save());

        let pa = doc_a.diff_incremental();
        let pb = doc_b.diff(&heads, &final_heads);

        let len = std::cmp::max(pa.len(), pb.len());

        for i in 0..len {
            if pa.get(i) != pb.get(i) {
                log!(" i={} ", i);
                log!(" pa={:?}", pa.get(i));
                log!(" pb={:?}", pb.get(i));
            }
        }

        if pa != pb {
            panic!()
        }
    }

    #[test]
    fn batch_counter_list_patch() {
        let mut rng = make_rng();
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let list1 = doc1.put_object(&ROOT, "list1", ObjType::List).unwrap();
        doc1.insert(&list1, 0, ScalarValue::counter(val())).unwrap();
        doc1.insert(&list1, 1, ScalarValue::counter(val())).unwrap();
        doc1.insert(&list1, 2, ScalarValue::counter(val())).unwrap();

        let mut doc1_copy = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());
        let mut doc2_copy = doc1.fork().with_actor(rng.random());

        let mut changes = vec![];
        //for _ in 0..3 {
        for _ in 0..2 {
            //for _ in 0..10 {
            for _ in 0..2 {
                let mut tmp = doc2.fork().with_actor(rng.random());
                //let num_updates = rng.gen::<usize>() % 10 + 1;
                let num_updates = 2;
                //let num_inserts = rng.gen::<usize>() % 10 + 1;
                let num_inserts = 2;
                //let num_deletes = rng.gen::<usize>() % 2;
                let num_deletes = 1;
                for _ in 0..num_updates {
                    let len = tmp.length(&list1);
                    let index = rng.random::<u32>() % (len as u32);
                    tmp.increment(&list1, index as usize, val()).unwrap();
                }
                for _ in 0..num_inserts {
                    let len = tmp.length(&list1);
                    let index = rng.random::<u32>() % (len as u32);
                    tmp.insert(&list1, index as usize, ScalarValue::counter(val()))
                        .unwrap();
                }
                for _ in 0..num_deletes {
                    let len = tmp.length(&list1);
                    let index = rng.random::<u32>() % (len as u32);
                    tmp.delete(&list1, index as usize).unwrap();
                }
                let change = tmp.get_last_local_change().unwrap();
                changes.push(change);
            }
            merge_and_diff(&mut doc2, &mut doc2_copy, &changes);
        }
        merge_and_diff(&mut doc1, &mut doc1_copy, &changes);
    }

    #[test]
    fn batch_list_patch() {
        let mut rng = make_rng();
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let list1 = doc1.put_object(&ROOT, "list1", ObjType::List).unwrap();
        doc1.insert(&list1, 0, val()).unwrap();
        doc1.insert(&list1, 1, val()).unwrap();
        doc1.insert(&list1, 2, val()).unwrap();

        let mut doc1_copy = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());
        let mut doc2_copy = doc1.fork().with_actor(rng.random());

        let mut changes = vec![];
        for _ in 0..3 {
            for _ in 0..30 {
                let mut tmp = doc2.fork().with_actor(rng.random());
                let num_updates = rng.random::<u32>() % 10 + 1;
                let num_inserts = rng.random::<u32>() % 10 + 1;
                let num_deletes = rng.random::<u32>() % 2;
                for _ in 0..num_updates {
                    let len = tmp.length(&list1);
                    let index = rng.random::<u32>() % (len as u32);
                    tmp.put(&list1, index as usize, val()).unwrap();
                }
                for _ in 0..num_inserts {
                    let len = tmp.length(&list1);
                    let index = rng.random::<u32>() % (len as u32);
                    tmp.insert(&list1, index as usize, val()).unwrap();
                }
                for _ in 0..num_deletes {
                    let len = tmp.length(&list1);
                    let index = rng.random::<u32>() % (len as u32);
                    tmp.delete(&list1, index as usize).unwrap();
                }
                let change = tmp.get_last_local_change().unwrap();
                changes.push(change);
            }
            merge_and_diff(&mut doc2, &mut doc2_copy, &changes);
        }
        merge_and_diff(&mut doc1, &mut doc1_copy, &changes);
    }

    #[test]
    fn batch_text_patch() {
        let mut rng = make_rng();
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let text1 = doc1.put_object(&ROOT, "text1", ObjType::Text).unwrap();
        doc1.splice_text(&text1, 0, 0, "--------").unwrap();

        let mut doc1_copy = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());
        let mut doc2_copy = doc1.fork().with_actor(rng.random());

        let mut changes = vec![];
        for _ in 0..10 {
            for _ in 0..5 {
                let mut tmp = doc2.fork().with_actor(rng.random());
                let num_splices = rng.random::<u32>() % 10 + 1;
                for _ in 0..num_splices {
                    let len = tmp.length(&text1) as u32;
                    let index = rng.random::<u32>() % len;
                    let del = std::cmp::min(rng.random::<u32>() % 2, len - index);
                    tmp.splice_text(
                        &text1,
                        index as usize,
                        del as isize,
                        &format!("[{}]", val()),
                    )
                    .unwrap();
                }
                let change = tmp.get_last_local_change().unwrap();
                changes.push(change);
            }
            merge_and_diff(&mut doc2, &mut doc2_copy, &changes);
        }
        merge_and_diff(&mut doc1, &mut doc1_copy, &changes);
    }

    #[test]
    fn batch_marks_patch() {
        let mut rng = make_rng();
        let mut value = 0;
        let mut val = move || {
            value += 1;
            value
        };
        let mut doc1 = AutoCommit::new().with_actor(rng.random());
        let text1 = doc1.put_object(&ROOT, "text1", ObjType::Text).unwrap();
        doc1.splice_text(&text1, 0, 0, "---------------------")
            .unwrap();

        let mut doc1_copy = doc1.fork().with_actor(rng.random());
        let mut doc2 = doc1.fork().with_actor(rng.random());
        let mut doc2_copy = doc1.fork().with_actor(rng.random());

        let mut changes = vec![];
        for _ in 0..5 {
            for _ in 0..10 {
                let mut tmp = doc2.fork().with_actor(rng.random());
                let num_splices = rng.random::<u32>() % 10 + 1;
                for _ in 0..num_splices {
                    let len = tmp.length(&text1) as u32;
                    let index = rng.random::<u32>() % len;
                    let del = std::cmp::min(rng.random::<u32>() % 2, len - index);
                    tmp.splice_text(
                        &text1,
                        index as usize,
                        del as isize,
                        &format!("[{}]", val()),
                    )
                    .unwrap();
                }
                let num_marks = rng.random::<u32>() % 3 + 1;
                for _ in 0..num_marks {
                    let len = tmp.length(&text1) as u32;
                    let a = rng.random::<u32>() % len;
                    let b = rng.random::<u32>() % len;
                    if a == b {
                        continue;
                    }
                    let start = std::cmp::min(a, b);
                    let end = std::cmp::max(a, b);
                    let name = "bold".into();
                    let value = ScalarValue::from(val());
                    let mark = Mark {
                        start: start as usize,
                        end: end as usize,
                        name,
                        value,
                    };
                    tmp.mark(&text1, mark, ExpandMark::After).unwrap();
                }
                let change = tmp.get_last_local_change().unwrap();
                changes.push(change);
            }
            merge_and_diff(&mut doc2, &mut doc2_copy, &changes);
        }
        merge_and_diff(&mut doc1, &mut doc1_copy, &changes);
    }

    fn merge_and_diff(a: &mut AutoCommit, a_copy: &mut AutoCommit, changes: &[Change]) {
        let heads = a.get_heads();

        a.update_diff_cursor();
        a.apply_changes_batch(changes.to_owned()).unwrap();
        a.validate_top_index();
        let pa = a.diff_incremental();
        let final_heads = a.get_heads();

        a_copy.apply_changes_iter(changes.to_owned()).unwrap();
        let pb = a_copy.diff(&heads, &final_heads);

        let len = std::cmp::max(pa.len(), pb.len());

        assert_eq!(a.get_heads(), a_copy.get_heads());

        for i in 0..len {
            if pa.get(i) != pb.get(i) {
                log!(" i={} ", i);
                log!(" pa={:?}", pa.get(i));
                log!(" pb={:?}", pb.get(i));
            }
        }

        if pa != pb {
            panic!()
        }
    }

    #[test]
    fn map_key_conflict() {
        let mut rng = make_rng();
        let mut doc = AutoCommit::new().with_actor(rng.random());

        doc.put(&ROOT, "key1", "value1").unwrap();

        const CYCLES: u32 = 10;
        const DOCS: u32 = 5;
        const KEYS: u32 = 4;

        let mut docs = vec![];

        for _ in 0..DOCS {
            docs.push(doc.fork().with_actor(rng.random()));
        }

        for _ in 0..CYCLES {
            for d in &mut docs {
                for _ in 0..10 {
                    let k = rng.random::<u32>() % KEYS;
                    let val = rng.random::<u32>();
                    d.put(&ROOT, format!("key{}", k), format!("value{}", val))
                        .unwrap();
                }
                let k = rng.random::<u32>() % KEYS;
                let _ = d.delete(&ROOT, format!("key{}", k));
            }

            let changes: Vec<_> = docs
                .iter_mut()
                .map(|d| d.get_last_local_change().unwrap())
                .collect();

            doc.apply_changes(changes).unwrap();

            doc.validate_top_index();
        }
    }

    #[test]
    fn list_element_conflict() {
        let mut rng = make_rng();
        let mut doc = AutoCommit::new().with_actor(rng.random());

        let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();

        const CYCLES: u32 = 5;
        const DOCS: u32 = 6;
        const KEYS: u32 = 3;

        for i in 0..KEYS {
            doc.insert(&list, i as usize, "_").unwrap();
        }

        let mut docs = vec![];

        for _ in 0..DOCS {
            docs.push(doc.fork().with_actor(rng.random()));
        }

        for _ in 0..CYCLES {
            for d in &mut docs {
                for _ in 0..3 {
                    let k = rng.random::<u32>() % KEYS;
                    let val = rng.random::<u32>();
                    d.put(&list, k as usize, format!("value{}", val)).unwrap();
                }
            }

            let changes: Vec<_> = docs
                .iter_mut()
                .map(|d| d.get_last_local_change().unwrap())
                .collect();

            doc.apply_changes(changes).unwrap();
            doc.validate_top_index();
        }
    }

    #[test]
    fn conflicts_with_isolate() {
        let mut rng = make_rng();
        let mut doc = AutoCommit::new().with_actor(rng.random());

        let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();
        let map = doc.put_object(&ROOT, "map", ObjType::Map).unwrap();
        doc.insert(&list, 0, "_").unwrap();
        doc.put(&map, "key", "_").unwrap();

        const CYCLES: u32 = 5;
        const DOCS: u32 = 6;

        let mut docs = vec![];
        let mut heads = vec![doc.get_heads()];

        for _ in 0..DOCS {
            docs.push(doc.fork().with_actor(rng.random()));
        }

        for _ in 0..CYCLES {
            for d in &mut docs {
                let head = rng.random::<u32>() % (heads.len() as u32);
                d.isolate(&heads[head as usize]);
                for _ in 0..3 {
                    let del = rng.random::<u32>() % 5;
                    let val = rng.random::<u32>();
                    let len = d.length(&list);
                    let val = format!("value{}", val);
                    if del == 0 {
                        if len > 0 {
                            d.delete(&list, 0).unwrap();
                        }
                        d.delete(&map, "key").unwrap();
                    } else {
                        if len > 0 {
                            d.put(&list, 0, &val).unwrap();
                        } else {
                            d.insert(&list, 0, &val).unwrap();
                        }
                        d.put(&map, "key", &val).unwrap();
                    }
                }
                d.integrate();
                d.validate_top_index();
            }

            let changes: Vec<_> = docs
                .iter_mut()
                .map(|d| d.get_last_local_change().unwrap())
                .collect();

            doc.apply_changes(changes).unwrap();

            heads.push(doc.get_heads());

            doc.validate_top_index();
        }
    }
}
