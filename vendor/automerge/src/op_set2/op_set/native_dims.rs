//! qOS bounded admission (lane L1): read-only op-set dimensions.
//! Allocates nothing; O(actors + mark-cache entries + string-column runs).
use super::{ObjInfo, OpSet};
use crate::resource_limits::dims::{actor_heap_bytes, hash_table_bytes, vec_bytes, DocDims};
use crate::types::{ActorId, OpId};
use std::mem::size_of;

impl OpSet {
    pub(crate) fn native_dims(&self, d: &mut DocDims) {
        let c = &self.cols;
        d.ops = self.len();
        d.succ = c.succ_actor.len();
        d.value_bytes = c.value.len();
        d.actors = self.actors.len();
        d.objects = self.obj_info.0.len();

        let mut exact = 0usize;
        let mut add = |v: usize| exact = exact.saturating_add(v);
        add(vec_bytes(self.actors.capacity(), size_of::<ActorId>()));
        let mut max_actor = 0usize;
        for a in &self.actors {
            let len = a.to_bytes().len();
            max_actor = max_actor.max(len);
            add(actor_heap_bytes(len));
        }
        d.max_actor_len = d.max_actor_len.max(max_actor);
        add(hash_table_bytes(
            self.obj_info.0.capacity(),
            size_of::<(OpId, ObjInfo)>(),
        ));
        let (marks, mark_cap, mark_table, mark_owned, mark_heap) = c.index.mark.native_dims();
        d.marks = marks;
        add(mark_table);
        add(mark_owned);
        d.exact_bytes = d.exact_bytes.saturating_add(exact);
        d.cap.op_actors = self.actors.capacity();
        d.cap.obj_index = self.obj_info.0.capacity();
        d.cap.mark_cache = mark_cap;
        d.cap.mark_owned_bytes = mark_owned;

        let mut strs = 0usize;
        for run in c.key_str.iter().runs() {
            if let Some(s) = run.value {
                strs = strs.saturating_add(s.len()).saturating_add(10);
            }
        }
        for run in c.mark_name.iter().runs() {
            if let Some(s) = run.value {
                strs = strs.saturating_add(s.len()).saturating_add(10);
            }
        }
        let sum = |xs: &[usize]| xs.iter().fold(0usize, |a, b| a.saturating_add(*b));
        // Measured heap of every op column (hexane `heap_bytes`: slab
        // tables, slab buffers and indexes, from capacities).
        d.op_col_bytes = sum(&[
            c.id_actor.heap_bytes(),
            c.id_ctr.heap_bytes(),
            c.obj_actor.heap_bytes(),
            c.obj_ctr.heap_bytes(),
            c.key_actor.heap_bytes(),
            c.key_ctr.heap_bytes(),
            c.key_str.heap_bytes(),
            c.succ_count.heap_bytes(),
            c.succ_actor.heap_bytes(),
            c.succ_ctr.heap_bytes(),
            c.insert.heap_bytes(),
            c.action.heap_bytes(),
            c.value_meta.heap_bytes(),
            c.value.heap_bytes(),
            c.mark_name.heap_bytes(),
            c.expand.heap_bytes(),
            c.index.text.heap_bytes(),
            c.index.top.heap_bytes(),
            c.index.visible.heap_bytes(),
            c.index.inc.heap_bytes(),
            mark_heap,
        ]);
        d.op_str_bytes = strs;
    }
}
