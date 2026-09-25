//! qOS bounded admission (lane L1): read-only op-set dimensions.
//! Allocates nothing; O(actors + mark-cache entries + string-column runs).
use super::{ObjInfo, OpSet};
use crate::resource_limits::dims::{actor_heap_bytes, hash_table_bytes, vec_bytes, DocDims, Slabs};
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
        let (marks, mark_cap, mark_table, mark_owned, mark_slabs) = c.index.mark.native_dims();
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
        d.op_slabs = Slabs {
            // 16 op columns + text, top, visible, inc and mark index columns
            // (the raw value column is bounded separately).
            columns: 20,
            u32s: sum(&[
                c.id_actor.slab_count(),
                c.obj_actor.slab_count(),
                c.obj_ctr.slab_count(),
                c.key_actor.slab_count(),
                c.succ_count.slab_count(),
                c.succ_actor.slab_count(),
                c.index.text.slab_count(),
            ]),
            u64s: sum(&[
                c.id_ctr.slab_count(),
                c.key_ctr.slab_count(),
                c.succ_ctr.slab_count(),
                c.action.slab_count(),
                c.value_meta.slab_count(),
                c.index.inc.slab_count(),
            ]),
            bools: sum(&[
                c.insert.slab_count(),
                c.expand.slab_count(),
                c.index.top.slab_count(),
                c.index.visible.slab_count(),
            ]),
            marks: mark_slabs,
            strs: sum(&[c.key_str.slab_count(), c.mark_name.slab_count()]),
            str_bytes: strs,
        };
    }
}
