//! qOS bounded admission (lane L1): read-only change-graph dimensions.
//! Allocates nothing; O(actors + clock-cache entries + fragments + message
//! runs).
use super::{ChangeGraph, Edge, FragmentNode, NodeIdx};
use crate::clock::SeqClock;
use crate::resource_limits::dims::{hash_table_bytes, vec_bytes, DocDims, Slabs};
use crate::ChangeHash;
use std::mem::size_of;

impl ChangeGraph {
    pub(crate) fn native_dims(&self, d: &mut DocDims) {
        d.changes = self.hashes.len();
        d.edges = self.edges.len();
        d.heads = self.heads.len();
        d.clock_cache_entries = self.clock_cache.len();
        d.fragments = self.fragments.len();

        let mut exact = 0usize;
        let mut add = |v: usize| exact = exact.saturating_add(v);
        // Change-indexed Vecs.
        add(vec_bytes(self.edges.capacity(), size_of::<Edge>()));
        add(vec_bytes(self.hashes.capacity(), size_of::<ChangeHash>()));
        add(vec_bytes(self.actors.capacity(), 4));
        add(vec_bytes(self.parents.capacity(), 4));
        add(vec_bytes(self.seq.capacity(), 4));
        add(vec_bytes(self.max_ops.capacity(), 4));
        add(self.extra_bytes_raw.capacity());
        add(hash_table_bytes(
            self.nodes_by_hash.capacity(),
            size_of::<(ChangeHash, NodeIdx)>(),
        ));
        add(hash_table_bytes(
            self.clock_cache.capacity(),
            size_of::<(NodeIdx, SeqClock)>(),
        ));
        let mut width = 0usize;
        for clock in self.clock_cache.values() {
            add(vec_bytes(clock.0.capacity(), 4));
            width = width.max(clock.0.capacity());
        }
        d.clock_width = width;
        add(vec_bytes(
            self.seq_index.capacity(),
            size_of::<Vec<NodeIdx>>(),
        ));
        let mut inner = 0usize;
        for v in &self.seq_index {
            inner = inner.saturating_add(v.capacity());
        }
        add(vec_bytes(inner, 4));
        add(vec_bytes(self.fragment_top.0.capacity(), 4));
        add(vec_bytes(
            self.fragments.capacity(),
            size_of::<FragmentNode>(),
        ));
        let mut deps = 0usize;
        for f in &self.fragments {
            deps = deps.saturating_add(f.deps.len());
            add(vec_bytes(f.deps.capacity(), 4));
            add(vec_bytes(f.clock.0.capacity(), 4));
        }
        d.fragment_deps = deps;
        d.exact_bytes = d.exact_bytes.saturating_add(exact);

        d.cap.graph_vecs = [
            self.hashes.capacity(),
            self.actors.capacity(),
            self.parents.capacity(),
            self.seq.capacity(),
            self.max_ops.capacity(),
        ];
        d.cap.edges = self.edges.capacity();
        d.cap.nodes_by_hash = self.nodes_by_hash.capacity();
        d.cap.clock_cache = self.clock_cache.capacity();
        d.cap.seq_index = self.seq_index.capacity();
        d.cap.seq_index_inner = inner;
        d.cap.fragments = self.fragments.capacity();
        d.cap.extra_raw = (self.extra_bytes_raw.len(), self.extra_bytes_raw.capacity());

        // hexane columns: num_ops (u64), timestamps (delta i64), messages
        // (Option<String>), extra_bytes_meta (prefix ValueMeta u64).
        let mut msg = 0usize;
        for run in self.messages.iter().runs() {
            if let Some(m) = run.value {
                msg = msg.saturating_add(m.len()).saturating_add(10);
            }
        }
        d.graph_col_bytes = self
            .num_ops
            .heap_bytes()
            .saturating_add(self.timestamps.heap_bytes())
            .saturating_add(self.messages.heap_bytes())
            .saturating_add(self.extra_bytes_meta.heap_bytes());
        d.graph_slabs = Slabs {
            columns: 4,
            u64s: self
                .num_ops
                .slab_count()
                .saturating_add(self.timestamps.slab_count())
                .saturating_add(self.extra_bytes_meta.slab_count()),
            strs: self.messages.slab_count(),
            str_bytes: msg,
            ..Slabs::default()
        };
    }
}
