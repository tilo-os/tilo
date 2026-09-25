//! Conservative allocation bounds from document dimensions.
//!
//! OWNER: lane L1. Every bound is a closed-form upper bound on *requested
//! allocation bytes* (not RSS) of unmodified automerge 0.12 / hexane 1.0
//! routines, derived from `size_of` of the element types with explicit
//! growth factors (Vec doubling, hashbrown bucket rounding + rehash peak,
//! BTree node bound), and calibrated against the `tilo-automerge`
//! heap probe (`crates/doc/tilo-automerge/tests/bounds_calibration.rs`).
//! Each coefficient names the field(s) it covers.
//!
//! Reading dimensions allocates nothing. `DocDims::of` is O(actors +
//! queued changes + clock-cache entries + fragments + column runs of the
//! string columns); every bound function is O(1).
//!
//! Two kinds of document dimension are used:
//! - std containers (`Vec`, `HashMap`, `HashSet`) are *measured* from their
//!   actual capacities (`exact_bytes`), which is exact for requested bytes;
//! - hexane columns do not expose slab capacities, so they are *bounded*
//!   from `slab_count()` and the hexane per-slab segment ceiling
//!   (`DEFAULT_MAX_SEG` = 64 segments, `splice_slab` asserts it), or, for the
//!   string/raw columns whose segments are unbounded, from content bytes.
use super::{NativeResourceError, NativeShape};
use crate::{Change, ChangeHash};
use std::mem::size_of;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

// ── checked accumulation ────────────────────────────────────────────────────

/// Checked sum of products. Any overflow poisons the sum and the bound
/// fails closed with `WorkBytes`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sum(Option<usize>);
impl Sum {
    pub(crate) fn new() -> Self {
        Self(Some(0))
    }
    pub(crate) fn add(&mut self, v: usize) -> &mut Self {
        self.0 = self.0.and_then(|s| s.checked_add(v));
        self
    }
    pub(crate) fn add_sum(&mut self, v: Result<usize>) -> &mut Self {
        match v {
            Ok(v) => self.add(v),
            Err(_) => {
                self.0 = None;
                self
            }
        }
    }
    pub(crate) fn mul(&mut self, a: usize, b: usize) -> &mut Self {
        let p = a.checked_mul(b);
        self.0 = self.0.and_then(|s| p.and_then(|p| s.checked_add(p)));
        self
    }
    pub(crate) fn mul3(&mut self, a: usize, b: usize, c: usize) -> &mut Self {
        let p = a.checked_mul(b).and_then(|p| p.checked_mul(c));
        self.0 = self.0.and_then(|s| p.and_then(|p| s.checked_add(p)));
        self
    }
    pub(crate) fn get(&self) -> Result<usize> {
        self.0.ok_or(NativeResourceError::WorkBytes)
    }
}

fn cadd(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(NativeResourceError::WorkBytes)
}

// ── std container layout (requested bytes) ──────────────────────────────────

const PTR: usize = size_of::<usize>();
/// hashbrown `Group::WIDTH` upper bound (SSE2 = 16, generic = 8).
const GROUP_WIDTH: usize = 16;

/// Requested bytes of a `Vec<T>` with this capacity.
pub(crate) fn vec_bytes(capacity: usize, elem: usize) -> usize {
    capacity.saturating_mul(elem)
}

/// hashbrown bucket count for a table whose `capacity()` reports `cap`
/// (inverse of `bucket_mask_to_capacity`: `<8` buckets hold `buckets-1`,
/// otherwise `buckets/8*7`).
fn buckets_from_capacity(cap: usize) -> usize {
    if cap == 0 {
        0
    } else if cap < 8 {
        cap + 1
    } else {
        (cap / 7).saturating_mul(8)
    }
}

/// hashbrown `capacity_to_buckets`: at least 4 (8 for tiny entries),
/// otherwise the next power of two of `n * 8 / 7`.
fn buckets_for_items(n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    if n < 15 {
        return Some(if n < 4 {
            4
        } else if n < 8 {
            8
        } else {
            16
        });
    }
    n.checked_mul(8).map(|x| x / 7)?.checked_next_power_of_two()
}

/// Requested bytes of a hashbrown table with `buckets` buckets of `entry`
/// bytes: data (padded to the control alignment) plus `buckets + WIDTH`
/// control bytes.
fn table_bytes(buckets: usize, entry: usize) -> Option<usize> {
    if buckets == 0 {
        return Some(0);
    }
    buckets
        .checked_mul(entry.checked_add(1)?)?
        .checked_add(2 * GROUP_WIDTH)
}

/// Requested bytes of an existing `HashMap`/`HashSet` from its `capacity()`.
pub(crate) fn hash_table_bytes(capacity: usize, entry: usize) -> usize {
    table_bytes(buckets_from_capacity(capacity), entry).unwrap_or(usize::MAX)
}

/// Requested bytes of a table grown by inserting items until it holds `n`.
fn hash_table_bytes_for(n: usize, entry: usize) -> Result<usize> {
    buckets_for_items(n)
        .and_then(|b| table_bytes(b, entry))
        .ok_or(NativeResourceError::WorkBytes)
}

/// Peak bytes of growing a fresh table to `n` items by repeated insertion:
/// the final table plus the previous one alive during the last rehash
/// (`resize` allocates the new table before freeing the old one); earlier
/// tables are freed. The previous table is at most half the final one.
fn hash_table_peak_for(n: usize, entry: usize) -> Result<usize> {
    let full = hash_table_bytes_for(n, entry)?;
    cadd(full, full / 2 + 2 * GROUP_WIDTH)
}

/// Additional bytes (new allocation, the old one stays covered by retained
/// credit until freed) when an existing table of `capacity` grows to hold
/// `n` items. hashbrown reserves `max(n, capacity + 1)` on growth.
fn hash_table_growth(len: usize, capacity: usize, added: usize, entry: usize) -> Result<usize> {
    let n = cadd(len, added)?;
    if n <= capacity {
        return Ok(0);
    }
    // Repeated single inserts can rehash several times; every intermediate
    // table except the last is freed before the next is allocated, and each
    // is smaller than the final table, so the final table plus one previous
    // table bounds the live new bytes.
    hash_table_peak_for(n.max(capacity + 1), entry)
}

/// Peak bytes of a `Vec<T>` grown by pushes to `n` elements: the final
/// buffer (capacity ≤ 2n by amortized doubling, min 4) plus the previous
/// buffer alive during the last `realloc` (≤ n).
fn vec_peak_for(n: usize, elem: usize) -> Result<usize> {
    let mut s = Sum::new();
    s.mul3(3, n.max(4), elem);
    s.get()
}

/// Additional live bytes when an existing `Vec` of `(len, cap)` grows to
/// `len + added`: the new buffer, capacity `max(2 cap, needed)`.
fn vec_growth(len: usize, cap: usize, added: usize, elem: usize) -> Result<usize> {
    let n = cadd(len, added)?;
    if n <= cap {
        return Ok(0);
    }
    let mut s = Sum::new();
    s.mul(n.max(cap.saturating_mul(2)).max(4), elem);
    s.get()
}

/// `BTreeSet<K>`/`BTreeMap<K, ()>` nodes for `n` keys. std B = 6: leaf
/// nodes are `parent ptr + parent_idx u16 + len u16 + 11 keys`, internal
/// nodes add 12 edge pointers; every non-root node holds at least 5 keys,
/// so there are at most `n/5 + 1` leaves and `n/25 + 1` internal nodes.
pub(crate) fn btree_bytes(n: usize, key: usize) -> Result<usize> {
    if n == 0 {
        return Ok(0);
    }
    let leaf = PTR + 4 + 11 * key + PTR; // + alignment padding
    let internal = leaf + 12 * PTR;
    let mut s = Sum::new();
    s.mul(n / 5 + 1, leaf).mul(n / 25 + 1, internal);
    s.get()
}

/// Heap bytes of an `ActorId` (`TinyVec<[u8; 16]>`): inline up to 16 bytes,
/// otherwise a spilled `Vec<u8>` whose capacity is at most twice its length.
pub(crate) fn actor_heap_bytes(len: usize) -> usize {
    if len > 16 {
        len.saturating_mul(2)
    } else {
        0
    }
}

/// Heap bytes of a `SmolStr`: inline up to 23 bytes, otherwise an
/// `Arc<str>` (two counts + the bytes).
pub(crate) fn smol_heap(s: &smol_str::SmolStr) -> usize {
    if s.len() > 23 {
        s.len().saturating_add(2 * PTR)
    } else {
        0
    }
}

/// Heap bytes owned by a public `ScalarValue` (bytes/unknown buffers, long
/// strings); counters and numbers are inline.
pub(crate) fn native_scalar_heap(v: &crate::ScalarValue) -> usize {
    match v {
        crate::ScalarValue::Bytes(b) => b.capacity(),
        crate::ScalarValue::Str(s) => smol_heap(s),
        crate::ScalarValue::Unknown { bytes, .. } => bytes.capacity(),
        _ => 0,
    }
}

/// Heap bytes of an exported object id (`ExId::Id` holds an `ActorId`).
pub(crate) fn exid_heap(id: &crate::ObjId) -> usize {
    match id {
        crate::ObjId::Root => 0,
        crate::ObjId::Id(_, actor, _) => actor_heap_bytes(actor.to_bytes().len()),
    }
}

/// Heap bytes of a `Prop` (map keys own a `String`).
pub(crate) fn prop_heap(p: &crate::Prop) -> usize {
    match p {
        crate::Prop::Map(k) => k.capacity(),
        crate::Prop::Seq(_) => 0,
    }
}

/// Heap bytes of a `SequenceTree<T>` of `len` elements (B = 16): every
/// non-root node holds at least B-1 = 15 elements, so there are at most
/// `len/15 + 1` nodes; each node's `elements` Vec has capacity ≤ 32 (grown
/// by push to at most 2B-1 = 31) and an internal node's `children` Vec
/// ≤ 32 nodes of 56 bytes; at most one node in 16 is internal.
pub(crate) fn sequence_tree_heap(len: usize, elem: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let nodes = len / 15 + 1;
    nodes
        .saturating_mul(32usize.saturating_mul(elem))
        .saturating_add((nodes / 16 + 1).saturating_mul(32 * 56))
}

// ── hexane columns ──────────────────────────────────────────────────────────

/// hexane `DEFAULT_MAX_SEG`: RLE/bool/delta slabs never hold more than this
/// many segments after a splice (`splice_slab` asserts it on entry; load
/// cuts slabs at half of it).
const HEX_MAX_SEGS: usize = 64;
/// Non-data heap per slab of a hexane column: the `Slab<Tail>` element
/// (data Vec 24 + len 8 + segments 8 + tail ≤ 8 = 48, ×2 for `slabs`
/// doubling), its `SlabBTree` leaf aggregate (≤ 32 bytes, ×2 doubling),
/// the node/`ChildSlot` share (B = 64: ≤ 1 node per 32 slabs) and the
/// 8-byte minimum capacity of a non-empty `Vec<u8>`.
const HEX_SLAB_META: usize = 192;

// ── dimensions ──────────────────────────────────────────────────────────────

/// Dimensions of a resident document. Read without allocating.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DocDims {
    pub(crate) ops: usize,
    /// Measured op-column heap (hexane `heap_bytes`, from capacities).
    pub(crate) op_col_bytes: usize,
    /// `RawColumn` value bytes.
    pub(crate) value_bytes: usize,
    pub(crate) objects: usize,
    /// Mark index cache entries.
    pub(crate) marks: usize,
    pub(crate) changes: usize,
    pub(crate) edges: usize,
    pub(crate) actors: usize,
    /// Measured change-graph hexane column heap (hexane `heap_bytes`).
    pub(crate) graph_col_bytes: usize,
    pub(crate) clock_cache_entries: usize,
    pub(crate) queue_changes: usize,
    /// Raw bytes of queued changes.
    pub(crate) queue_bytes: usize,
    pub(crate) patch_log_events: usize,
    pub(crate) heads: usize,
    pub(crate) pending_ops: usize,

    // ── additions (L1) ──
    /// Successor entries (`succ_actor` rows).
    pub(crate) succ: usize,
    /// UTF-8 bytes of the op string columns' runs (`key_str`,
    /// `mark_name`), plus 10 per run.
    pub(crate) op_str_bytes: usize,
    /// Exactly measured requested bytes of every std container reachable
    /// from the document (capacities of Vecs, hash tables, clocks,
    /// fragments, actor ids, mark cache values, queued changes).
    pub(crate) exact_bytes: usize,
    /// Bytes the `AutoCommit` wrapper holds beyond the document
    /// (transaction, patch logs, cursors), measured from capacities.
    pub(crate) aux_bytes: usize,
    /// The wrapper holds state that is not measured (a cached diff, or a
    /// patch log with completed patches or marks): retained bounds fail
    /// closed with `Unaccounted`.
    pub(crate) aux_unmeasured: bool,
    /// Level ≥ 1 fragments and their total dependency entries.
    pub(crate) fragments: usize,
    pub(crate) fragment_deps: usize,
    /// Queued changes whose hash has `fragment_level() >= 1`.
    pub(crate) queue_fragments: usize,
    /// Ops, dependency hashes and (bound on) op predecessor references of
    /// the queued changes.
    pub(crate) queue_ops: usize,
    pub(crate) queue_edges: usize,
    pub(crate) queue_preds: usize,
    /// Queued changes with `seq == 1` (may add their actor on integration).
    pub(crate) queue_new_actors: usize,
    /// Largest op counter in the graph (`ChangeGraph::max_op`).
    pub(crate) max_op: u64,
    /// Largest SeqClock capacity (entries) among cached clocks.
    pub(crate) clock_width: usize,
    /// Capacities for growth prediction (see `apply_peak_bound`).
    pub(crate) cap: Caps,
    /// Longest actor id in bytes (for per-actor clones during apply).
    pub(crate) max_actor_len: usize,
}

/// Capacities of the growable std containers, for growth prediction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Caps {
    /// `ChangeGraph::{hashes, actors, parents, seq, max_ops}` capacities.
    pub(crate) graph_vecs: [usize; 5],
    pub(crate) edges: usize,
    pub(crate) nodes_by_hash: usize,
    pub(crate) clock_cache: usize,
    pub(crate) seq_index: usize,
    pub(crate) seq_index_inner: usize,
    pub(crate) fragments: usize,
    pub(crate) extra_raw: (usize, usize),
    pub(crate) obj_index: usize,
    pub(crate) mark_cache: usize,
    pub(crate) mark_owned_bytes: usize,
    pub(crate) op_actors: usize,
    pub(crate) deps: (usize, usize),
    pub(crate) queue_changes: usize,
    pub(crate) queue_hashes: usize,
    pub(crate) queue_actor_seqs: usize,
    pub(crate) authors: usize,
}

/// Element sizes of `ChangeGraph::{hashes, actors, parents, seq, max_ops}`.
pub(crate) const GRAPH_VEC_ELEMS: [usize; 5] = [size_of::<ChangeHash>(), 4, 4, 4, 4];

impl DocDims {
    pub(crate) fn of(doc: &crate::AutoCommit) -> Self {
        let mut d = DocDims::default();
        doc.doc.native_dims(&mut d);
        d.pending_ops = crate::transaction::Transactable::pending_ops(doc);
        match doc.native_aux_heap_bytes() {
            Some(bytes) => d.aux_bytes = bytes,
            None => d.aux_unmeasured = true,
        }
        d
    }
}

/// Bound on the heap of one parsed `Change` (storage::Change): the owned
/// chunk bytes (`Cow::Owned`, ≤ 2× for a caller-grown Vec), a lazily
/// compressed copy (DEFLATE output ≤ raw + raw/1000 + 64), `dependencies`
/// (32 bytes each, ×2 collect slack), `other_actors` (`ActorId` each ×2)
/// plus spilled actor-id heap, and the owned `message` String.
pub(crate) fn change_heap_bound(change: &Change) -> usize {
    let raw = change.raw_bytes().len();
    let mut s = Sum::new();
    s.mul(raw, 3)
        .add(64)
        .mul3(change.deps().len(), size_of::<ChangeHash>(), 2)
        .mul3(
            change.other_actor_ids().len(),
            size_of::<crate::ActorId>(),
            2,
        )
        .add(actor_heap_bytes(change.actor_id().to_bytes().len()))
        .mul(change.message().map_or(0, str::len), 2);
    for a in change.other_actor_ids() {
        s.add(actor_heap_bytes(a.to_bytes().len()));
    }
    s.get().unwrap_or(usize::MAX)
}

/// Dimensions of incoming storage, from inspection (`NativeShape`) and/or
/// parsed changes. Upper bounds are fine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IncomingDims {
    pub(crate) changes: usize,
    pub(crate) ops: usize,
    pub(crate) max_op_counter: u64,
    pub(crate) value_bytes: usize,
    pub(crate) actors: usize,
    pub(crate) edges: usize,
    pub(crate) heads: usize,
    pub(crate) normalized_bytes: usize,

    // ── additions (L1) ──
    /// Changes whose hash has `fragment_level() >= 1`. From a shape (hashes
    /// unknown before reconstruction) this is `changes`, the sound maximum.
    pub(crate) fragments: usize,
    /// Bound on the retained heap of the parsed `Vec<Change>` (from
    /// `from_changes` exactly, from a shape by formula).
    pub(crate) parsed_bytes: usize,
    /// Longest actor id among the incoming changes, in bytes.
    pub(crate) max_actor_len: usize,
    /// Changes with `seq == 1` (the only way an actor becomes new); from a
    /// shape, every actor entry.
    pub(crate) new_actors: usize,
    /// Total raw bytes of the changes (sum of chunk lengths).
    pub(crate) raw_bytes: usize,
    /// Declared dense collector op slots (`NativeShape::dense_operations`);
    /// 0 for parsed changes.
    pub(crate) dense_operations: usize,
    /// The leading document chunk (first chunk, if it is a document chunk):
    /// its ops, changes and normalized bytes; 0 otherwise.
    pub(crate) leading_ops: usize,
    pub(crate) leading_changes: usize,
    pub(crate) leading_bytes: usize,
    /// Exact make ops (makeMap/List/Text/Table) and mark ops (begins and
    /// ends) in the inspected op columns; for parsed changes, every op.
    pub(crate) make_ops: usize,
    pub(crate) mark_ops: usize,
    /// Largest actor table of any single chunk (`NativeShape`); for parsed
    /// changes, every actor entry.
    pub(crate) chunk_actors: usize,
    /// Leading document chunk: bound on ops its load collector can queue
    /// out of order (`structure::queued_ops`), and its stored raw value
    /// column length.
    pub(crate) leading_queued: usize,
    pub(crate) leading_raw_values: usize,
    /// Largest declared op count of a single change.
    pub(crate) max_change_ops: usize,
    /// Leading document chunk: ops the collector may encode out of row
    /// order, and a bound on the encoded change-column bytes of all other
    /// ops (`NativeShape::leading_document_ordered_encoding`).
    pub(crate) leading_literal_ops: usize,
    pub(crate) leading_ordered_encoding: usize,
}
impl IncomingDims {
    pub(crate) fn from_shape(shape: &NativeShape) -> Self {
        let mut dims = Self {
            changes: shape.changes,
            ops: shape.operations.max(shape.dense_operations),
            max_op_counter: shape.maximum_operation_counter,
            value_bytes: shape.value_bytes,
            actors: shape.actors,
            edges: shape.edges,
            heads: shape.heads,
            normalized_bytes: shape.normalized_bytes,
            fragments: shape.changes,
            parsed_bytes: 0,
            // Longest single actor id in any chunk prefix (inspection).
            max_actor_len: shape.max_actor_bytes,
            raw_bytes: shape.normalized_bytes,
            dense_operations: shape.dense_operations,
            new_actors: shape.actors,
            leading_ops: shape.leading_document_operations,
            leading_changes: shape.leading_document_changes,
            leading_bytes: shape.leading_document_bytes,
            make_ops: shape.make_operations,
            mark_ops: shape.mark_operations,
            chunk_actors: shape.max_chunk_actors,
            leading_queued: shape.leading_document_queued_ops,
            leading_raw_values: shape.leading_document_raw_values,
            max_change_ops: shape.max_change_ops,
            leading_literal_ops: shape.leading_document_literal_ops,
            leading_ordered_encoding: shape.leading_document_ordered_encoding,
        };
        dims.parsed_bytes = shape_parsed_bound(&dims).unwrap_or(usize::MAX);
        dims
    }
    /// Parsed changes together with the inspected shape of the storage they
    /// came from: the larger of the exact parsed figures and the inspected
    /// ones (for document or bundle chunks the reconstructed changes can
    /// differ from the stored representation).
    pub(crate) fn from_parsed(changes: &[Change], shape: &NativeShape) -> Result<Self> {
        let mut d = Self::from_changes(changes)?;
        d.edges = d.edges.max(shape.edges);
        d.ops = d.ops.max(shape.operations);
        d.actors = d.actors.max(shape.actors);
        d.value_bytes = d.value_bytes.max(shape.value_bytes);
        d.max_op_counter = d.max_op_counter.max(shape.maximum_operation_counter);
        d.make_ops = shape.make_operations.min(d.ops);
        d.mark_ops = shape.mark_operations.min(d.ops);
        Ok(d)
    }
    /// Exact figures of parsed changes (`edges` = dependencies + op
    /// predecessor references, from the pred-count column).
    pub(crate) fn from_changes(changes: &[Change]) -> Result<Self> {
        let mut d = Self {
            changes: changes.len(),
            ..Self::default()
        };
        let mut parsed = Sum::new();
        // The Vec<Change> itself (callers collect with doubling slack).
        parsed.mul3(changes.len(), size_of::<Change>(), 2);
        let mut ops = Sum::new();
        let mut edges = Sum::new();
        let mut raw = Sum::new();
        let mut actors = Sum::new();
        for c in changes {
            parsed.add(change_heap_bound(c));
            ops.add(c.len());
            // Change dependencies plus op predecessor references.
            edges.add(c.deps().len()).add(c.native_pred_count());
            raw.add(c.raw_bytes().len());
            actors.add(1 + c.other_actor_ids().len());
            d.max_op_counter = d.max_op_counter.max(c.max_op());
            if c.hash().fragment_level() >= 1 {
                d.fragments += 1;
            }
            if c.seq() == 1 {
                d.new_actors += 1;
            }
            let len = c.actor_id().to_bytes().len();
            d.max_actor_len = d.max_actor_len.max(len);
            for a in c.other_actor_ids() {
                d.max_actor_len = d.max_actor_len.max(a.to_bytes().len());
            }
        }
        d.ops = ops.get()?;
        d.edges = edges.get()?;
        d.raw_bytes = raw.get()?;
        d.normalized_bytes = d.raw_bytes;
        // Values (and op keys/mark names) live inside the raw chunk bytes.
        d.value_bytes = d.raw_bytes;
        d.actors = actors.get()?;
        d.heads = d.edges;
        d.make_ops = d.ops;
        d.mark_ops = d.ops;
        d.chunk_actors = d.actors;
        d.max_change_ops = changes.iter().map(Change::len).max().unwrap_or(0);
        d.parsed_bytes = parsed.get()?;
        Ok(d)
    }
}

/// Bytes of one op in a rebuilt change's op columns when every value is a
/// literal (`write_change_ops`/`ProgressiveEncoder::save_to`): per column
/// the LEB128 value width plus ≤ 2 bytes of run/literal headers — obj actor
/// and ctr, key actor and ctr (delta), key string length/null, insert,
/// action, value meta, pred count, expand, mark name — with widths from the
/// inspected actor count, largest counter, value bytes and edges. String and
/// value contents are the inspected expanded `value_bytes`.
fn op_literal_bytes(d: &IncomingDims) -> usize {
    let la = leb(d.actors.max(1) as u64);
    let lc = leb(d.max_op_counter);
    let ls = leb(d.max_op_counter.saturating_mul(2).saturating_add(1));
    let lm = leb((d.value_bytes as u64).saturating_mul(16).saturating_add(15));
    let lp = leb(d.edges as u64 + 1);
    // Per column and op, a literal costs its value plus an amortized
    // literal-group header (≤ 1: a group of one has a 1-byte header), a
    // run of ≥ 2 costs ≤ (header + value) / 2, a null run of one 2 bytes,
    // a boolean run of one 1 byte: so ≤ max(V + 1, 2) per column.
    let col = |v: usize| (v + 1).max(2);
    col(la) // obj actor
        + col(lc) // obj ctr
        + col(la) // key actor
        + col(ls) // key ctr (delta)
        + 2 // key string length / null run (contents in value_bytes)
        + 1 // insert
        + col(1) // action
        + col(lm) // value meta
        + col(lp) // pred count
        + 1 // expand
        + 2 // mark name length / null run
}

/// Encoded op-column bytes of the rebuilt changes (without value and
/// string contents). With the leading document's collector-order figures:
/// ops encoded in row order cost at most the document's own column bytes
/// plus one cut per change switch (`leading_ordered_encoding`), other ops
/// their literal width; actor indexes are remapped per change (≤ one extra
/// LEB byte per op per actor column once there are ≥ 128 actors); the pred
/// columns (absent from the document) are charged separately: per op a
/// pred count (runs of zero broken ≤ twice per pred, plus one run per
/// change) and per pred its literals. Without those figures (parsed
/// changes, later chunks): every op at literal width.
fn encoded_ops_bytes(d: &IncomingDims) -> usize {
    let literal = d
        .ops
        .saturating_mul(op_literal_bytes(d))
        .saturating_add(d.edges.saturating_mul(pred_literal_bytes(d)));
    if d.leading_ops == 0 || d.leading_ops != d.ops {
        return literal;
    }
    let lp = leb(d.edges as u64 + 1);
    let la = leb(d.actors.max(1) as u64);
    let ordered = d
        .leading_ordered_encoding
        .saturating_add(
            d.leading_literal_ops
                .min(d.ops)
                .saturating_mul(op_literal_bytes(d)),
        )
        .saturating_add(d.ops.saturating_mul(2 * (la - 1)))
        .saturating_add(
            d.edges
                .saturating_mul(2)
                .saturating_add(d.changes)
                .saturating_mul(lp + 3),
        )
        .saturating_add(d.edges.saturating_mul(pred_literal_bytes(d)));
    ordered.min(literal)
}

/// Per predecessor: actor and delta counter literals.
fn pred_literal_bytes(d: &IncomingDims) -> usize {
    (leb(d.actors.max(1) as u64) + 1).max(2)
        + (leb(d.max_op_counter.saturating_mul(2).saturating_add(1)) + 1).max(2)
}

/// Raw chunk bytes of the changes `ChangeCollector::collect` rebuilds from
/// document chunks: per change a header (chunk header 14, seq/start
/// op/time/message length 30, column metadata ≤ 14 × 8, author id) and the
/// full id of each other actor it references (≤ min(changes × (table - 1),
/// 3 per op + 1 per pred)), 33 per dependency, `op_literal_bytes` per op,
/// `pred_literal_bytes` per pred (deps and preds ≤ edges), all expanded
/// strings and values, and extra bytes (≤ normalized).
fn rebuilt_raw_bytes(d: &IncomingDims) -> Result<usize> {
    let actor = d.max_actor_len;
    let others = d
        .changes
        .saturating_mul(d.chunk_actors.saturating_sub(1))
        .min(d.ops.saturating_mul(3).saturating_add(d.edges));
    let mut s = Sum::new();
    s.mul(d.changes, 160 + actor)
        .mul(others, actor + 2)
        .mul(d.edges, 33)
        .add(encoded_ops_bytes(d))
        .add(d.value_bytes)
        // extra bytes and messages live outside the raw value column
        .add(d.normalized_bytes.saturating_sub(d.leading_raw_values));
    s.get()
}

/// Rebuilt raw bytes of the largest single change: header, other actors,
/// its declared ops at literal width, and at most all preds, values and
/// extra bytes.
fn rebuilt_raw_max(d: &IncomingDims) -> Result<usize> {
    let actor = d.max_actor_len;
    let mut s = Sum::new();
    s.add(160 + actor)
        .mul(d.chunk_actors, actor + 2)
        .mul(d.edges, 33 + pred_literal_bytes(d))
        .mul(d.max_change_ops, op_literal_bytes(d))
        .add(d.value_bytes)
        .add(d.normalized_bytes.saturating_sub(d.leading_raw_values));
    Ok(s.get()?.min(rebuilt_raw_bytes(d)?))
}

/// Retained `Vec<Change>` of rebuilt changes: the Vec (`collect` reserves it
/// exactly: one `Change` per change), each chunk's bytes (built with exact
/// capacity: `Vec::with_capacity(header + data)`), its dependency Vec
/// (32 per edge), `other_actors` (an `ActorId` plus spilled id each), and
/// its message (inside `value_bytes`, counted in the raw bytes).
fn rebuilt_changes_bound(d: &IncomingDims) -> Result<usize> {
    let actor = d.max_actor_len;
    let others = d
        .changes
        .saturating_mul(d.chunk_actors.saturating_sub(1))
        .min(d.ops.saturating_mul(3).saturating_add(d.edges));
    let mut s = Sum::new();
    s.mul(d.changes, size_of::<Change>())
        .add_sum(rebuilt_raw_bytes(d))
        .mul(d.edges, size_of::<ChangeHash>())
        .mul(
            others.saturating_add(d.changes),
            size_of::<crate::ActorId>() + actor_heap_bytes(actor),
        );
    s.get()
}

/// Retained `Vec<Change>` of change chunks parsed verbatim: the Vec (×2
/// push slack), each change's owned chunk copy (≤ its normalized bytes),
/// dependency hashes (≤ heads, ×2), `other_actors` (≤ actor entries,
/// `ActorId` ×2 plus spilled ids) and messages (≤ value bytes).
fn verbatim_changes_bound(d: &IncomingDims) -> Result<usize> {
    let actor = d.max_actor_len;
    let mut s = Sum::new();
    s.mul3(d.changes, size_of::<Change>(), 2)
        .mul(d.changes, 64)
        .add(d.raw_bytes)
        .mul(d.heads, 2 * size_of::<ChangeHash>())
        .mul(
            d.actors,
            2 * size_of::<crate::ActorId>() + actor_heap_bytes(actor),
        )
        .mul(d.value_bytes, 2);
    s.get()
}

/// `Vec<Change>` heap bound from inspection only: the leading document
/// chunk's changes are rebuilt (`rebuilt_changes_bound`); every other chunk
/// is either a change/bundle chunk parsed verbatim or a later document chunk
/// rebuilt, so the rest is charged both ways.
fn shape_parsed_bound(d: &IncomingDims) -> Result<usize> {
    let (leading, rest) = split_leading(d);
    let mut s = Sum::new();
    if let Some(leading) = leading {
        s.add_sum(rebuilt_changes_bound(&leading));
    }
    if rest.changes != 0 || rest.ops != 0 {
        s.add_sum(verbatim_changes_bound(&rest))
            .add_sum(rebuilt_changes_bound(&rest));
    }
    s.get()
}

/// The leading document chunk (if any) and the rest of the input.
fn split_leading(d: &IncomingDims) -> (Option<IncomingDims>, IncomingDims) {
    let l_ops = d.leading_ops.min(d.ops);
    let l_changes = d.leading_changes.min(d.changes);
    let l_bytes = d.leading_bytes.min(d.normalized_bytes);
    let rest = IncomingDims {
        changes: d.changes - l_changes,
        ops: d.ops - l_ops,
        normalized_bytes: d.normalized_bytes - l_bytes,
        raw_bytes: d.raw_bytes.saturating_sub(l_bytes),
        dense_operations: d.dense_operations,
        fragments: 0,
        leading_ops: 0,
        leading_changes: 0,
        leading_bytes: 0,
        leading_queued: 0,
        leading_raw_values: 0,
        leading_literal_ops: 0,
        leading_ordered_encoding: 0,
        ..*d
    };
    if l_changes == 0 && l_ops == 0 && l_bytes == 0 {
        return (None, rest);
    }
    let leading = IncomingDims {
        changes: l_changes,
        ops: l_ops,
        normalized_bytes: l_bytes,
        raw_bytes: l_bytes,
        fragments: 0,
        leading_ops: l_ops,
        leading_changes: l_changes,
        leading_bytes: l_bytes,
        make_ops: d.make_ops.min(l_ops),
        mark_ops: d.mark_ops.min(l_ops),
        ..*d
    };
    (Some(leading), rest)
}

// ── bounds ──────────────────────────────────────────────────────────────────

/// Resident footprint bound: measured std containers + bounded hexane
/// columns + the change-graph `heads` BTreeSet + the AutoCommit wrapper.
pub(crate) fn retained_bound(doc: &DocDims) -> Result<usize> {
    if doc.aux_unmeasured {
        return Err(NativeResourceError::Unaccounted);
    }
    let mut s = Sum::new();
    s.add(doc.exact_bytes)
        .add(doc.op_col_bytes)
        .add(doc.graph_col_bytes)
        .add_sum(btree_bytes(doc.heads, size_of::<ChangeHash>()))
        .add(doc.aux_bytes);
    s.get()
}

/// Peak of `AutoCommit::clone`: the copy is at most the retained bound of
/// the source (clones allocate `len`-sized Vecs and same-bucket tables),
/// and nothing transient is allocated beyond the copy itself.
pub(crate) fn clone_peak_bound(doc: &DocDims) -> Result<usize> {
    retained_bound(doc)
}

/// Retained growth of the document when `incoming` is integrated into it
/// (every queued change may be integrated in the same call).
fn growth_bound(doc: &DocDims, inc: &IncomingDims) -> Result<usize> {
    let c = &doc.cap;
    // Every queued change may become ready and integrate too.
    let dc = cadd(inc.changes, doc.queue_changes)?;
    let dops = cadd(inc.ops, doc.queue_ops)?;
    // Successor rows and graph edges: op predecessor references plus
    // change dependencies (`inc.edges` from the shape covers both).
    let dedges = cadd(inc.edges, doc.queue_preds.saturating_add(doc.queue_edges))?;
    let dactors = new_actors(doc, inc)?;

    let mut s = Sum::new();
    // ChangeGraph change-indexed Vecs.
    for (cap, elem) in c.graph_vecs.iter().zip(GRAPH_VEC_ELEMS) {
        s.add_sum(vec_growth(doc.changes, *cap, dc, elem));
    }
    // ChangeGraph::edges (Edge = NodeIdx u32 + Option<EdgeIdx> u32).
    s.add_sum(vec_growth(doc.edges, c.edges, dedges, 8));
    // ChangeGraph::nodes_by_hash: (ChangeHash, NodeIdx) = 36 bytes.
    s.add_sum(hash_table_growth(doc.changes, c.nodes_by_hash, dc, 36));
    // Super-linear graph terms: clock cache, actor insertion, seq_index,
    // fragments.
    s.add_sum(superlinear_apply_bound(doc, inc));
    if dactors > 0 {
        // Actor-column remap rebuilds (`Columns::remap_actors`,
        // `MarkIndexColumn::remap_values`) and the rebuilt `ObjIndex` and
        // mark cache: a second copy of the op columns and of the measured
        // std bytes is alive while the old one is dropped.
        s.add(doc.op_col_bytes).add(doc.exact_bytes);
    }
    // ChangeGraph::seq_index outer Vec (one inner Vec per actor).
    s.add_sum(vec_growth(doc.actors, c.seq_index, dactors, 24));
    // ChangeGraph::extra_bytes_raw: ≤ incoming raw bytes.
    s.add_sum(vec_growth(c.extra_raw.0, c.extra_raw.1, inc.raw_bytes, 1));
    // Change-graph hexane columns and op columns (see `columns_growth`).
    s.add_sum(columns_growth(doc, inc, dc, dops, dedges));
    // OpSet::actors Vec<ActorId> and spilled ids.
    s.add_sum(vec_growth(
        doc.actors,
        c.op_actors,
        dactors,
        size_of::<crate::ActorId>(),
    ))
    .mul(dactors, actor_heap_bytes(inc.max_actor_len));
    // ObjIndex: (OpId, ObjInfo) ≤ 32 bytes per entry; ≤ one object per op.
    s.add_sum(hash_table_growth(doc.objects, c.obj_index, dops, 32));
    // Mark index cache: (OpId, MarkData) ≤ 88 bytes; owned name/value ≤ raw.
    s.add_sum(hash_table_growth(doc.marks, c.mark_cache, dops, 88))
        .mul(inc.raw_bytes, 2);
    // Automerge::deps HashSet<ChangeHash>: heads ≤ changes.
    s.add_sum(hash_table_growth(
        c.deps.0,
        c.deps.1,
        dc,
        size_of::<ChangeHash>(),
    ));
    // heads BTreeSet: new nodes for up to `dc` more heads.
    s.add_sum(btree_bytes(dc, size_of::<ChangeHash>()));
    // Authors: `authors` Vec<Author> (24 bytes + owned bytes ≤ raw, ×2) and
    // `actor_to_author` (16 bytes per actor, and per change after a load,
    // which sizes it by the change count), ×2 for doubling.
    s.mul(dactors, 2 * 24)
        .mul(dactors.saturating_add(dc), 2 * 16)
        .mul(inc.raw_bytes, 2);
    s.get()
}

/// Actors the batch can add to the document. An actor is new only through
/// its first change (`seq == 1`): a later change needs that actor's earlier
/// changes, and ops can only reference actors of ops in their causal past.
/// `BatchApply::insert_new_actors` inserts exactly the `seq == 1` authors.
fn new_actors(doc: &DocDims, inc: &IncomingDims) -> Result<usize> {
    cadd(inc.new_actors, doc.queue_new_actors)
}

/// ULEB128 length of `x`.
fn leb(x: u64) -> usize {
    (64 - (x | 1).leading_zeros() as usize).div_ceil(7)
}

/// Retained growth and splice transients of the hexane columns when `dops`
/// op rows, `dedges` successor rows and `dc` change rows are inserted.
///
/// Per column, with `V` the LEB128 width of the values that column can
/// receive (from the incoming counters, actors and value sizes) and `n` the
/// row count after the batch, one segment is at most
/// `seg = V + leb(n) + 2` bytes (value, run-length header, literal-group
/// header). One inserted row adds at most three segments (it can split a
/// run in two), so at most `3·seg` new bytes, ×2 because slab buffers grow
/// by amortized doubling. A slab splits only above `HEX_MAX_SEGS` into
/// halves of at least 30 segments, so at most one split per 10 rows; each
/// split allocates the moved half (≤ `HEX_MAX_SEGS·seg`) and one slab's
/// metadata. Existing slabs a row lands in may reallocate to twice their
/// buffer: ≤ min(2 × measured column heap, rows × 2·`HEX_MAX_SEGS`·seg).
/// String content (keys, mark names, messages) and values arrive ≤ once
/// per raw byte and are charged ×4 (doubling, split copy).
/// Transient: one splice buffer and one realloc in flight per column,
/// ≤ 4 × `HEX_MAX_SEGS`·seg + the inserted run.
fn columns_growth(
    doc: &DocDims,
    inc: &IncomingDims,
    dc: usize,
    dops: usize,
    dedges: usize,
) -> Result<usize> {
    let rows = cadd(dops, dedges)?;
    let n = cadd(doc.ops.saturating_add(doc.succ), rows)? as u64;
    let actors = cadd(doc.actors, new_actors(doc, inc)?)? as u64;
    let counter = inc.max_op_counter.max(doc.max_op);
    let value_bytes = inc.value_bytes.max(inc.raw_bytes) as u64;
    let ln = leb(n);
    let la = leb(actors);
    let lcu = leb(counter);
    let lcs = leb(counter.saturating_mul(2).saturating_add(1));
    let lmeta = leb(value_bytes.saturating_mul(16).saturating_add(15));
    // (columns, value width) for the op columns (`Columns` + `Indexes`).
    let op_cols: [(usize, usize); 12] = [
        (4, la),                                 // id_actor, obj_actor, key_actor, succ_actor
        (1, lcu),                                // obj_ctr
        (3, lcs),                                // id_ctr, key_ctr, succ_ctr (delta)
        (1, leb(rows as u64 + doc.succ as u64)), // succ_count
        (1, 1),                                  // action
        (1, lmeta),                              // value_meta
        (1, 10),                                 // index.inc (i64 counter sums)
        (1, leb(value_bytes)),                   // index.text widths
        (4, 0),                                  // insert, expand, index.top, index.visible
        (1, 1 + la + lcu),                       // index.mark (tag, actor, counter)
        (2, 0),                                  // key_str, mark_name (content below)
        (0, 0),
    ];
    // Change-graph columns: num_ops, timestamps (delta i64),
    // extra_bytes_meta; messages (content below).
    let graph_cols: [(usize, usize); 3] = [(1, leb(inc.ops as u64)), (1, 10), (2, lmeta)];
    let mut s = Sum::new();
    let per_col = |cols: &[(usize, usize)], rows: usize, ln: usize, s: &mut Sum| {
        for &(k, v) in cols {
            if k == 0 {
                continue;
            }
            let seg = v + ln + 2;
            s.mul3(k, rows, 2 * 3 * seg) // new bytes, doubled
                .mul3(k, rows / 10 + 1, HEX_MAX_SEGS * seg + HEX_SLAB_META) // splits
                .mul3(k, 4, HEX_MAX_SEGS * seg); // splice buffer + realloc in flight
        }
    };
    per_col(&op_cols, rows, ln, &mut s);
    let gln = leb(doc.changes.saturating_add(dc) as u64);
    per_col(&graph_cols, dc, gln, &mut s);
    // Touched existing slabs reallocating to twice their buffers.
    let mut realloc = 0usize;
    for &(k, v) in &op_cols {
        realloc = realloc.saturating_add(k.saturating_mul(2 * HEX_MAX_SEGS * (v + ln + 2)));
    }
    s.add(
        doc.op_col_bytes
            .saturating_mul(2)
            .min(rows.saturating_mul(realloc)),
    );
    // String content and values (raw value column: whole values per slab,
    // ≤ one slab per value) ×4, plus RawColumn slab/BIT entries.
    s.mul(inc.raw_bytes.max(inc.value_bytes), 4 * 3)
        .mul(dops, 2 * 32);
    s.get()
}

/// Growth of `ChangeGraph::fragments` when `f_in` changes whose hash has
/// `fragment_level() >= 1` are added to a graph holding `f_old` fragments,
/// `actors` actors wide and `changes` changes long (after the addition).
///
/// `cache_fragment` pushes one `FragmentNode { head, deps, clock }` per such
/// change. Its `deps` collects every earlier fragment whose clock the new one
/// covers at an equal or higher level, so a chain of ground hashes makes
/// fragment `i` hold `i` deps: the total is quadratic in the number of
/// fragments, which is why callers must count `f_in` exactly (from parsed or
/// reconstructed hashes) rather than assume every change is one.
/// Per new fragment: `deps: Vec<NodeIdx>` grown by pushes to at most
/// `f_old + f_in` entries (×2 doubling, ×4 bytes), a `SeqClock` of `actors`
/// `Option<NonZeroU32>` from `calculate_clock` (exact `vec![None; n]`), and a
/// 56-byte `FragmentNode` slot (the `fragments` Vec doubles: its new buffer
/// is ≤ 2 × (f_old + f_in) slots, live together with the old one).
/// Transient per call: `calculate_clock` walks uncached ancestors with a
/// `BTreeSet<NodeIdx>` `to_visit` and `visited` (≤ `changes` keys each,
/// 4-byte keys: a 56-byte leaf per 5 keys, a 152-byte internal node per 25),
/// and `deps`/`supercede` scratch Vecs (≤ f_old + f_in usize/NodeIdx each).
pub(crate) fn fragment_growth_bound(
    f_old: usize,
    f_in: usize,
    actors: usize,
    changes: usize,
) -> Result<usize> {
    if f_in == 0 {
        return Ok(0);
    }
    let after = cadd(f_old, f_in)?;
    let mut s = Sum::new();
    s.mul3(f_in, after, 2 * 4)
        .mul3(f_in, actors.max(1), 4)
        .mul3(after, 2, 56)
        .mul(2, btree_bytes(changes, 4)?)
        .mul(after, 8 + 4);
    s.get()
}

/// Growth of `ChangeGraph::clock_cache` and the clocks it owns when
/// `dc` changes are added (`changes_after` total, `actors_after` wide).
/// `cache_clock` runs for every 16th node, and its frontier recursion
/// caches any uncached ancestor it stops at, so in the worst case every
/// node ends up cached exactly once: new entries ≤ `changes_after - entries`.
/// Each is a `(NodeIdx, SeqClock)` table slot (32 bytes; the table may
/// rehash, `hash_table_growth`) and a `SeqClock` of `actors_after` × 4 bytes,
/// ×2 because `cache_clock` keeps the returned clone alive while inserting.
/// Actor insertion (`insert_actor` → `rewrite_with_new_actor`) inserts one
/// slot in every cached and fragment clock and `fragment_top`; a full clock
/// Vec reallocates to twice its width.
fn clock_cache_growth(doc: &DocDims, dc: usize, dactors: usize) -> Result<usize> {
    let changes_after = cadd(doc.changes, dc)?;
    let actors_after = cadd(doc.actors, dactors)?;
    let new_entries = changes_after.saturating_sub(doc.clock_cache_entries);
    let mut s = Sum::new();
    s.add_sum(hash_table_growth(
        doc.clock_cache_entries,
        doc.cap.clock_cache,
        new_entries,
        32,
    ))
    .mul3(new_entries, actors_after.max(1), 4 * 2);
    if dactors > 0 {
        s.mul3(
            doc.clock_cache_entries
                .saturating_add(doc.fragments)
                .saturating_add(1),
            actors_after,
            2 * 4,
        );
    }
    s.get()
}

/// The super-linear part of `apply_changes_batch` growth: every term that
/// is not linear in the incoming size. `apply_incoming_checked` reserves
/// exactly this; `apply_peak_bound` includes it.
/// - clock cache and actor-insertion clock rewrites (`clock_cache_growth`);
/// - fragments (`fragment_growth_bound`) with `incoming.fragments` counted
///   exactly from parsed hashes; queued changes that may integrate in the
///   same call are counted as fragments when the caller did not count them
///   (queued hashes are known: they are counted in `DocDims::of`);
/// - `ChangeGraph::seq_index` inner Vecs: a push into an actor's full Vec
///   reallocates it to twice its capacity (≤ 2 × Σ capacities × 4 bytes,
///   live together with the old buffers) plus the pushed entries.
pub(crate) fn superlinear_apply_bound(doc: &DocDims, incoming: &IncomingDims) -> Result<usize> {
    let dc = cadd(incoming.changes, doc.queue_changes)?;
    let dactors = new_actors(doc, incoming)?;
    let actors_after = cadd(doc.actors, dactors)?;
    let changes_after = cadd(doc.changes, dc)?;
    let f_in = cadd(incoming.fragments, doc.queue_fragments)?;
    let mut s = Sum::new();
    s.add_sum(clock_cache_growth(doc, dc, dactors))
        .add_sum(fragment_growth_bound(
            doc.fragments,
            f_in,
            actors_after,
            changes_after,
        ))
        .mul(doc.cap.seq_index_inner, 2 * 4)
        .mul3(dc, 2, 4);
    s.get()
}

/// Transient scratch of `apply_changes_batch_log_patches` with `q` changes
/// in the queue plus the batch, excluding the op application itself.
fn queue_scratch(doc: &DocDims, inc: &IncomingDims) -> Result<usize> {
    let q = cadd(doc.queue_changes, inc.changes)?;
    let e = cadd(inc.edges, doc.queue_edges)?;
    let change = size_of::<Change>();
    let actor_seq = size_of::<(crate::ActorId, u64)>();
    let actor_clone = actor_heap_bytes(inc.max_actor_len.max(doc.max_actor_len));
    let mut s = Sum::new();
    // `seen: HashSet<ChangeHash>` over queue + incoming.
    s.add_sum(hash_table_peak_for(q, size_of::<ChangeHash>()));
    // `actor_seqs: HashMap<ActorId, HashSet<u64>>`: ≤ q actors, each with a
    // u64 set; `actor_author: HashSet<ActorId>`. Actor ids are cloned.
    s.add_sum(hash_table_peak_for(q, size_of::<crate::ActorId>() + 48))
        .add_sum(hash_table_peak_for(q, 8))
        .mul(q, hash_table_bytes_for(1, 8)?)
        .add_sum(hash_table_peak_for(q, size_of::<crate::ActorId>()))
        .mul3(q, actor_clone, 3);
    // `ChangeBatch`: Vec<Change>, hashes set, (ActorId, u64) set.
    s.add_sum(vec_peak_for(inc.changes, change))
        .add_sum(hash_table_peak_for(inc.changes, size_of::<ChangeHash>()))
        .add_sum(hash_table_peak_for(inc.changes, actor_seq))
        .mul(inc.changes, actor_clone);
    // `ChangeQueue::extend`: queue Vec / sets grow to q.
    s.add_sum(vec_growth(
        doc.queue_changes,
        doc.cap.queue_changes,
        inc.changes,
        change,
    ))
    .add_sum(hash_table_growth(
        doc.queue_changes,
        doc.cap.queue_hashes,
        inc.changes,
        32,
    ))
    .add_sum(hash_table_growth(
        doc.queue_changes,
        doc.cap.queue_actor_seqs,
        inc.changes,
        actor_seq,
    ))
    .mul(inc.changes, actor_clone);
    // `BatchApply` admissibility pre-check (L2): `introduced:
    // HashSet<&ActorId>` and `next: HashMap<&ActorId, u64>` over the ready
    // batch, `queued: HashSet<ChangeHash>` over the queue.
    s.add_sum(hash_table_peak_for(q, PTR))
        .add_sum(hash_table_peak_for(q, PTR + 8))
        .add_sum(hash_table_peak_for(q, size_of::<ChangeHash>()));
    // `pop_topo_sorted_ready`: unsatisfied (u32), waiting_on
    // (ChangeHash → Vec<usize>, ≤ e entries, each Vec ≤ 2× its pushes),
    // ready VecDeque<usize>, topo_order Vec<usize>, slots Vec<Option<Change>>,
    // topo Vec<Change>, rebuilt queue Vec<Change>.
    s.mul(q, 4)
        .add_sum(hash_table_peak_for(e, size_of::<ChangeHash>() + 24))
        .mul3(e.max(1), 8, 2)
        .add_sum(vec_peak_for(q, 8))
        .add_sum(vec_peak_for(q, 8))
        .mul(q, change + 8)
        .add_sum(vec_peak_for(q, change))
        .mul(q, change);
    s.get()
}

/// Transient of `BatchApply::apply` (op_set2/change/batch.rs) over the
/// incoming plus queued changes, beyond column growth (`columns_growth`):
/// - `import_ops`: per change a `Vec<ChangeOp>` (exact) extended into
///   `self.ops` (doubling: ≤ 3 × ops live), each op's decoded storage op
///   (pred Vec + value) mapped into an `OpBuilder` (a second pred Vec and
///   an owned value/mark name ≤ raw bytes), per change an actor map;
/// - `succ` pushes into `ChangeOp::succ` (≤ one per pred, 24 bytes, ×2);
/// - `obj_info.clone()` of the whole `ObjIndex` plus its growth;
/// - `sort_by` (stable merge sort: a buffer of ops/2);
/// - `PredCache` (≤ one entry per pred, each a Vec of `(OpId, inc)`);
/// - `obj_spans`, `succ: Vec<SuccInsert>`, `conflicts: Vec<Adjust>`
///   (doubling ×3);
/// - one Map/List walker at a time (entry/gosub/update maps and stacks
///   ≤ ops entries: ≤ 128 bytes per op);
/// - `Columns::splice` value iterators per inserted run (≤ 32 bytes per op
///   per column, 21 columns).
fn batch_apply_scratch(doc: &DocDims, inc: &IncomingDims) -> Result<usize> {
    use crate::op_set2::{ChangeOp, SuccInsert};
    let changes = cadd(inc.changes, doc.queue_changes)?;
    let ops = cadd(inc.ops, doc.queue_ops)?;
    let preds = cadd(inc.edges, doc.queue_preds)?;
    let op = size_of::<ChangeOp>();
    let mut s = Sum::new();
    s.mul3(ops, 3, op)
        .mul(ops, op) // per-change collected Vec before extend
        .mul(preds, 2 * 8 * 2) // storage pred Vec + OpBuilder pred Vec
        .mul(inc.raw_bytes.max(inc.value_bytes), 3)
        .mul(changes, 64)
        .mul3(inc.actors, 4, 2)
        .mul(preds, 2 * 24)
        .add(hash_table_bytes(doc.cap.obj_index, 32))
        .add_sum(hash_table_growth(doc.objects, doc.cap.obj_index, ops, 32))
        .mul(ops / 2 + 1, op)
        .add_sum(hash_table_peak_for(preds, 8 + 24))
        .mul(preds, 2 * 24)
        .mul3(ops, 3, 24)
        .mul3(preds, 3, size_of::<SuccInsert>())
        .mul3(ops, 3, 16)
        .mul(ops, 128)
        .mul3(ops, 21, 32)
        .add(64 * 1024);
    s.get()
}

/// Transient peak plus retained growth of `apply_changes_batch` applying
/// `incoming` together with every queued change.
pub(crate) fn apply_peak_bound(doc: &DocDims, incoming: &IncomingDims) -> Result<usize> {
    let mut s = Sum::new();
    s.add_sum(growth_bound(doc, incoming))
        .add_sum(queue_scratch(doc, incoming))
        .add_sum(batch_apply_scratch(doc, incoming));
    s.get()
}

/// Entry bytes of an out-of-order op queued in a `ProgressiveEncoder`
/// (`queue: BTreeMap<usize, OpBuilder>`): a leaf holds ≤ 11 `(usize,
/// OpBuilder)` entries plus parent pointer and lengths, and every non-root
/// node holds ≥ 5, so ≤ leaf/5 per entry, plus the internal-node share
/// (one ≤ 12-edge internal node per ≥ 6 leaves).
fn queued_op_bytes() -> usize {
    use crate::op_set2::OpBuilder;
    let leaf = 2 * PTR + 11 * (PTR + size_of::<OpBuilder<'static>>());
    let internal = leaf + 12 * PTR;
    leaf / 5 + internal / 30 + 1
}

/// Peak of `Document::reconstruct` for the leading document chunk
/// (`storage/document.rs`), from the exact inspected figures. Held
/// throughout:
/// - `OpSet::load`: every column's encoded bytes copied into slabs cut at
///   `HEX_MAX_SEGS/2` segments: ≤ 10 × the chunk's bytes, 256 per column;
///   the actor table;
/// - `ChangeGraphCols::load`: per change actor/parent/seq/max-op (16), 8
///   per edge, graph columns (inside the 10 × bytes);
/// - `IndexBuilder` (op_set2/op_set/index.rs): its per-op Vecs are reserved
///   exactly (`with_capacity(op_set.len())`): succ u32, top bool, width
///   u64, `Option<MarkIndexBuilder>`; `incs` per successor row (16); the
///   `counters` map (≤ one entry per edge); `ObjIndex` (one entry per make
///   op) and `MarkOrderValidator::begins` (one per mark op), both grown by
///   insertion.
///
/// Then the largest of three phases:
/// - processing ops: the collector's per-change slots (native: the dense
///   `Vec<Option<OpBuilder>>` of the declared op count or one boxed
///   `ProgressiveEncoder`, whichever is smaller, so ≤ min(dense × slot,
///   changes × encoder); wasm: always dense), `ChangeBuilder` and metadata
///   per change, per-encoder minimum buffers and the actor-wide
///   `Vec<bool>`, the `preds` map, and per op either a queued entry
///   (`queued_op_bytes`, an out-of-order op in change order) or its encoded
///   literal bytes ×2 (buffers double), whichever is larger — an op is in
///   exactly one of the two;
/// - collecting: the same collector state, the encoded ops, the rebuilt
///   `Vec<Change>` and one change's assembly transient (≤ 3 × the rebuilt
///   raw bytes);
/// - finishing: `IndexBuilder::finish` (text `Option<u32>`, visible and top
///   copies per op, the index columns built from them: ≤ 20 bytes per op
///   ×2), the mark cache (one `(OpId, MarkData)` per mark op, owned values
///   ≤ value bytes), the rebuilt changes, and `ChangeGraphCols::finalize`
///   (hashes, `nodes_by_hash`, `seq_index`, the change-count-sized
///   `actor_to_author`, the clock cache).
fn leading_document_peak(d: &IncomingDims) -> Result<usize> {
    use crate::op_set2::change::collector::ProgressiveEncoder;
    use crate::op_set2::change::BuildChangeMetadata;
    use crate::op_set2::op_set::MarkIndexBuilder;
    use crate::op_set2::OpBuilder;
    let ops = d.ops;
    let changes = d.changes;
    let slot = size_of::<Option<OpBuilder<'static>>>();
    let encoder = size_of::<ProgressiveEncoder<'static>>() + PTR;
    let dense = d.dense_operations.saturating_mul(slot);
    let slots = if cfg!(target_family = "wasm") {
        dense
    } else {
        dense.min(changes.saturating_mul(encoder))
    };
    let raw_values = d.leading_raw_values.min(d.normalized_bytes);
    let mut held = Sum::new();
    // Re-segmented columns ≤ 10 × their bytes; the raw value column is
    // copied once into ≤ 4 KiB slabs (64 bytes of slab/BIT entries each).
    held.mul(d.normalized_bytes - raw_values, 10)
        .add(raw_values)
        .mul(raw_values / 4096 + 1, 64)
        .mul(23, 256)
        .mul(
            d.actors,
            size_of::<crate::ActorId>() + actor_heap_bytes(d.max_actor_len),
        )
        .mul(changes, 16)
        .mul(d.edges, 8)
        .mul(ops, 4 + 1 + 8 + size_of::<Option<MarkIndexBuilder>>())
        .mul(d.edges, 16)
        .add_sum(hash_table_peak_for(d.edges, 32))
        .mul(d.edges, 2 * 16)
        .add_sum(hash_table_peak_for(d.make_ops, 32))
        .add_sum(hash_table_peak_for(d.mark_ops, 16));
    let held = held.get()?;

    let mut collector = Sum::new();
    collector
        .add(slots)
        .mul(changes, 64 + size_of::<BuildChangeMetadata<'static>>())
        .mul(changes, 16 * 16 + d.actors)
        .add_sum(hash_table_peak_for(d.edges, 32))
        .mul(d.edges, 2 * 8);
    let collector = collector.get()?;
    // Only ops the inspector's collector simulation found out of order can
    // be queued; every other op is encoded as it arrives.
    let queued = d.leading_queued.min(ops);
    let queue_or_encoded = queued
        .saturating_mul(queued_op_bytes())
        .saturating_add(encoded_ops_bytes(d).saturating_mul(2))
        .saturating_add(d.edges.saturating_mul(2 * (8 + pred_literal_bytes(d))))
        .saturating_add(d.value_bytes.saturating_mul(2));
    let rebuilt = rebuilt_changes_bound(d)?;
    let rebuilt_raw = rebuilt_raw_bytes(d)?;

    let mut processing = Sum::new();
    processing.add(collector).add(queue_or_encoded);
    // Collecting: every other change is either still encoded (≤ 2 × its
    // rebuilt raw bytes) or already rebuilt (its raw bytes, plus the Change
    // and its Vecs, charged in full); the change being assembled holds its
    // encoder output moved into `data` (≤ 2 × raw, doubling) and then
    // `ChangeBuilder::build`'s `data` (reserved for the columns, regrown
    // by the header: ≤ 2 × raw while the old ≤ 1 × lives), so ≤ 5 × its raw
    // bytes: 2 × all + 3 × the largest.
    let mut collecting = Sum::new();
    collecting
        .add(collector)
        .add(rebuilt - rebuilt_raw.min(rebuilt))
        .mul(rebuilt_raw, 2)
        .mul(rebuilt_raw_max(d)?, 3);
    let empty = DocDims::default();
    let mut finishing = Sum::new();
    finishing
        .mul(ops, 8 + 1 + 1 + 2 * 20)
        .add_sum(hash_table_peak_for(d.mark_ops, 88))
        // the mark cache owns copies of mark names and values
        .add(if d.mark_ops == 0 { 0 } else { d.value_bytes })
        .add(rebuilt)
        .mul(changes, 32 + 2 * 4 + 2 * 16)
        .add_sum(hash_table_peak_for(changes, 36))
        .add_sum(clock_cache_growth(&empty, changes, d.actors));
    let phase = processing
        .get()?
        .max(collecting.get()?)
        .max(finishing.get()?);
    cadd(held, phase)
}

/// Peak of loading normalized storage of this shape (includes the result),
/// EXCLUDING the fragment term: change hashes are unknown until the load
/// path has reconstructed and verified the changes, so the loader counts
/// fragment-level hashes then and reserves `fragment_growth_bound` before
/// the graph caches fragments.
///
/// `load_strict` takes two paths:
/// 1. a leading document chunk is reconstructed directly
///    (`leading_document_peak`);
/// 2. every other chunk (change and bundle chunks, later document chunks)
///    is parsed or rebuilt into a `Vec<Change>` (`shape_parsed_bound` of
///    the rest) and batch-applied to the loaded document: the apply model
///    (`growth_bound`, `queue_scratch`, `batch_apply_scratch`) with the
///    leading document's peak as its base.
pub(crate) fn load_peak_bound(incoming: &IncomingDims) -> Result<usize> {
    let (leading, rest) = split_leading(incoming);
    let mut s = Sum::new();
    // The empty document every load starts from (`Automerge::new`: 21 op
    // and 4 graph columns with their initial B-tree nodes, the actor id):
    // 9 KiB observed, 16 KiB charged.
    s.add(16 * 1024);
    let mut base = DocDims::default();
    if let Some(leading) = leading {
        let peak = leading_document_peak(&leading)?;
        s.add(peak);
        // What the reconstructed document retains (the base the rest is
        // applied to): its columns (≤ 10 × bytes + index columns ≤ 40 per
        // op) and its std containers (graph ≤ 200 per change + 16 per edge,
        // ObjIndex and mark cache tables, owned mark values).
        let mut cols = Sum::new();
        cols.mul(leading.normalized_bytes, 10)
            .mul(23, 256)
            .mul(leading.ops, 40);
        let mut exact = Sum::new();
        exact
            .mul(leading.changes, 200)
            .mul(incoming.edges, 16)
            .add_sum(hash_table_peak_for(leading.make_ops, 32))
            .add_sum(hash_table_peak_for(leading.mark_ops, 88))
            .mul(incoming.value_bytes, 2)
            .add_sum(clock_cache_growth(
                &DocDims::default(),
                leading.changes,
                incoming.actors,
            ));
        base = DocDims {
            ops: leading.ops,
            changes: leading.changes,
            actors: incoming.actors,
            edges: incoming.edges,
            op_col_bytes: cols.get()?,
            exact_bytes: exact.get()?,
            max_op: incoming.max_op_counter,
            max_actor_len: incoming.max_actor_len,
            ..DocDims::default()
        };
    }
    if rest.changes != 0 || rest.ops != 0 {
        use crate::op_set2::change::collector::ProgressiveEncoder;
        use crate::op_set2::OpBuilder;
        s.add_sum(shape_parsed_bound(&rest))
            .add_sum(growth_bound(&base, &rest))
            .add_sum(queue_scratch(&base, &rest))
            .add_sum(batch_apply_scratch(&base, &rest))
            // later document chunks are rebuilt through the collector
            .mul(
                rest.dense_operations,
                size_of::<Option<OpBuilder<'static>>>(),
            )
            .mul(rest.changes, size_of::<ProgressiveEncoder<'static>>() + PTR)
            .mul(rest.ops, queued_op_bytes() + 4 + 1 + 8 + 64);
    }
    s.get()
}

/// Upper bound on the uncompressed document chunk data `Document::new`
/// assembles (`storage/document.rs`):
/// - `op_set.export()`: the op columns re-encoded; a canonical re-encoding
///   is never longer than the slab encodings it reads (slab boundaries only
///   break runs), so ≤ the measured op-column heap (`op_col_bytes`, which
///   also covers the unexported index columns);
/// - `change_graph.encode()`: per change actor/seq/max_op/time/deps-count
///   (≤ 5 + 5 + 5 + 10 + 5 LEB bytes, 32 per change), 6 per dependency
///   edge, the message/extra-bytes/num-ops/timestamp columns (≤ their
///   measured hexane heap `graph_col_bytes`) and the raw extra bytes;
/// - prefix/suffix: actor table (≤ 10 + id bytes each), heads (32 + 10 for
///   the head index each), column metadata (≤ 64 columns × 16 bytes) and
///   the chunk header (≤ 64).
fn document_data_bound(doc: &DocDims) -> Result<usize> {
    let mut s = Sum::new();
    s.add(doc.op_col_bytes)
        .mul(doc.changes, 32)
        .mul(doc.edges, 6)
        .add(doc.graph_col_bytes)
        .add(doc.cap.extra_raw.0)
        .mul(doc.actors, 10 + doc.max_actor_len.max(16))
        .mul(doc.heads, size_of::<ChangeHash>() + 10)
        .add(64 * 16 + 64);
    s.get()
}

/// DEFLATE encoder state that `flate2` (zlib-rs backend, default
/// windowBits 15 / memLevel 8) allocates per compressed column while
/// saving: sliding window 2 × 2^15, `prev` 2^15 × u16, `head` 2^16 × u16
/// (zlib-rs uses 16 hash bits), pending/symbol buffer 4 × 2^14, plus the
/// state struct and 64-byte alignment padding: 64 + 64 + 128 + 64 KiB +
/// ≤ 64 KiB = 384 KiB (observed 371 KiB). One encoder is live at a time.
pub(crate) const DEFLATE_ENCODER_STATE: usize = 384 * 1024;
/// Matching INFLATE state (window 2^15 + state + tables) for paths that
/// decompress: 64 KiB. Normalized storage is already inflated
/// (`normalize` counts inflation with fixed scratch), so the checked load
/// and apply paths never inflate; kept for callers that do.
#[allow(dead_code)]
pub(crate) const INFLATE_DECODER_STATE: usize = 64 * 1024;

/// Upper bound on `save_with_options` output length: the document chunk
/// (`document_data_bound`), DEFLATE expansion of compressed columns (stored
/// blocks: 5 bytes per 16 KiB + a 64-byte stream overhead per column), and
/// every queued change appended verbatim (`retain_orphans`).
pub(crate) fn save_output_bound(doc: &DocDims) -> Result<usize> {
    let data = document_data_bound(doc)?;
    let mut t = Sum::new();
    t.add(data)
        .add(data / 3000)
        .mul(64, 64)
        .add(doc.queue_bytes);
    t.get()
}

/// Transient peak of `save_with_options`, including its output. With `D`
/// the document data bound: the op export and change export Vecs (grown
/// by push, ≤ 2D together), `data` (reserved for the exports, then grown
/// by the prefix: a realloc to ≤ 2D while the old buffer lives, 3D), the
/// header+data copy `bytes` (D), the compressed copy (≤ 2D by doubling)
/// with per-column scratch (≤ 2D) and one live DEFLATE encoder, the cloned
/// actor table, head Vecs, and the final `extend` of queued changes
/// (realloc of the output to ≤ 2 (D + Q) while the old one lives).
pub(crate) fn save_peak_bound(doc: &DocDims) -> Result<usize> {
    let out = save_output_bound(doc)?;
    let mut s = Sum::new();
    s.mul(out, 12)
        .mul(doc.queue_bytes, 2)
        .mul(
            doc.actors,
            size_of::<crate::ActorId>() + doc.max_actor_len.max(16),
        )
        .mul(doc.heads, 2 * (size_of::<ChangeHash>() + 8))
        .add(DEFLATE_ENCODER_STATE)
        .add(64 * 1024);
    s.get()
}

/// Peak of exporting up to `changes` canonical changes (`get_changes`),
/// including the returned `Vec<Change>`.
///
/// `ChangeCollector::exclude_hashes` builds the change metadata (a Vec
/// cloned once), one `ChangeBuilder` per change with a boxed
/// `ProgressiveEncoder` (its `actors: Vec<bool>` is actor-wide, and each of
/// its ~16 column encoders owns a doubling buffer), the `preds` map (one
/// entry per successor edge), an out-of-order `queue` (≤ every op as an
/// `OpBuilder`), then encodes each change: op data (≤ 48 bytes of column
/// encoding per op plus values and key strings) passes through the encoder
/// buffers (×2 doubling), the assembled chunk and its parse (×2), so ×6 in
/// all; every rebuilt change repeats the full id of each actor it
/// references (≤ 3 per op + its author). With `debug_assertions` the same
/// changes are rebuilt a second time through `Bundle::for_hashes` while the
/// first result is alive, so the whole collector term counts twice.
pub(crate) fn history_peak_bound(doc: &DocDims, changes: usize) -> Result<usize> {
    use crate::op_set2::change::collector::ProgressiveEncoder;
    use crate::op_set2::change::BuildChangeMetadata;
    use crate::op_set2::OpBuilder;
    let n = changes.min(doc.changes);
    let actor = doc.max_actor_len.max(16);
    let refs = cadd(doc.ops.saturating_mul(3), n)?;
    let mut per_change = Sum::new();
    per_change
        .add(size_of::<ProgressiveEncoder<'static>>())
        .add(16 * 64)
        .mul(3, size_of::<BuildChangeMetadata<'static>>())
        .mul(2, size_of::<Change>())
        .add(512)
        .add(doc.actors);
    let per_change = per_change.get()?;
    let mut op_data = Sum::new();
    op_data
        .mul(doc.ops, 48)
        .add(doc.value_bytes)
        .add(doc.op_str_bytes);
    let op_data = op_data.get()?;
    let mut p = Sum::new();
    p.mul(n, per_change)
        .mul(op_data, 6)
        .mul(refs, 4 * actor + 2 * size_of::<crate::ActorId>())
        .mul(doc.edges, 4 * 34)
        .mul(doc.succ, 64)
        .mul(doc.ops, 2 * size_of::<OpBuilder<'static>>())
        .mul(doc.changes, 64);
    let collector = p.get()?;
    let mut s = Sum::new();
    s.mul(collector, 2).add(64 * 1024);
    s.get()
}

/// Retained bytes of `changes` parsed incoming changes (`Vec<Change>`).
pub(crate) fn incoming_changes_bound(incoming: &IncomingDims) -> Result<usize> {
    Ok(incoming.parsed_bytes)
}
