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
fn btree_bytes(n: usize, key: usize) -> Result<usize> {
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

// ── hexane columns ──────────────────────────────────────────────────────────

/// hexane `DEFAULT_MAX_SEG`: RLE/bool/delta slabs never hold more than this
/// many segments after a splice (`splice_slab` asserts it on entry; load
/// cuts slabs at half of it).
const HEX_MAX_SEGS: usize = 64;
/// Upper bound on the non-data heap per slab of a hexane column:
/// `Vec<Slab<Tail>>` element (data Vec 24 + len 8 + segments 8 + tail ≤ 16,
/// ×2 for `slabs` doubling) plus the `SlabBTree` leaf aggregate (≤ 48 bytes,
/// ×2 doubling), its share of internal `ChildSlot`s and `nodes` entries,
/// plus the 8-byte minimum capacity of a non-empty `Vec<u8>`.
const HEX_SLAB_META: usize = 384;
/// Per-column fixed heap: `SlabBTree::new` reserves 8 `Option<Node>` (≤ 40
/// bytes each) plus a first leaf aggregate Vec and the `free` list.
const HEX_COLUMN_BASE: usize = 512;

/// Segment byte ceiling for each value class (LEB128 value + run header
/// ≤ 10 + literal-group header ≤ 2).
pub(crate) const SEG_U32: usize = 5 + 12; // ActorIdx, u32, Option<u32>, Option<ActorIdx>
pub(crate) const SEG_U64: usize = 10 + 12; // u64/i64/delta, Action, ValueMeta, Option<i64>
pub(crate) const SEG_BOOL: usize = 10; // bool runs: one ULEB count per segment
pub(crate) const SEG_MARK: usize = 1 + 10 + 10 + 12; // Option<MarkIdx>: tag + actor + ctr
pub(crate) const SEG_STR: usize = 10 + 12; // String framing; content counted separately

/// Slab counts of a group of hexane columns, by segment class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Slabs {
    pub(crate) columns: usize,
    pub(crate) u32s: usize,
    pub(crate) u64s: usize,
    pub(crate) bools: usize,
    pub(crate) marks: usize,
    pub(crate) strs: usize,
    /// UTF-8 bytes of the string columns' distinct runs (plus a 10-byte
    /// length prefix per run); slab data holds at most this plus framing.
    pub(crate) str_bytes: usize,
}
impl Slabs {
    fn total(&self) -> usize {
        self.u32s
            .saturating_add(self.u64s)
            .saturating_add(self.bools)
            .saturating_add(self.marks)
            .saturating_add(self.strs)
    }
    /// Bound on the columns' heap. A slab's data `Vec` only grows by
    /// `resize` (amortized doubling) and shrinks by `truncate` without
    /// releasing capacity, so its capacity is at most twice the largest
    /// encoded length it ever held, which is at most `HEX_MAX_SEGS`
    /// segments. String segments carry unbounded content: their content
    /// is charged at 4× (2× doubling, 2× for capacity left behind in the
    /// original slab by an overflow split). See the module note on string
    /// column capacity; this is the one calibrated rather than proven term.
    fn bound(&self) -> Result<usize> {
        let mut s = Sum::new();
        s.mul(self.columns, HEX_COLUMN_BASE)
            .mul(self.total(), HEX_SLAB_META)
            .mul3(self.u32s, 2 * HEX_MAX_SEGS, SEG_U32)
            .mul3(self.u64s, 2 * HEX_MAX_SEGS, SEG_U64)
            .mul3(self.bools, 2 * HEX_MAX_SEGS, SEG_BOOL)
            .mul3(self.marks, 2 * HEX_MAX_SEGS, SEG_MARK)
            .mul3(self.strs, 2 * HEX_MAX_SEGS, SEG_STR)
            .mul(self.str_bytes, 4);
        s.get()
    }
    /// Slabs after inserting `items` rows into every column of this group:
    /// each inserted row adds at most 3 segments (it can split a run in
    /// two), a slab splits only above `HEX_MAX_SEGS` into halves of at
    /// least `HEX_MAX_SEGS/2 - 2` segments, so at most one new slab per 10
    /// inserted rows per column, plus one per splice boundary.
    fn grown(&self, items: usize, str_bytes: usize) -> Self {
        let per = |slabs: usize| {
            if slabs == 0 && items == 0 {
                0
            } else {
                let cols = self.columns.max(1);
                slabs.saturating_add(cols.saturating_mul(items / 10 + 2))
            }
        };
        Self {
            columns: self.columns,
            u32s: per(self.u32s),
            u64s: per(self.u64s),
            bools: per(self.bools),
            marks: per(self.marks),
            strs: per(self.strs),
            str_bytes: self.str_bytes.saturating_add(str_bytes),
        }
    }
}

/// hexane `RawColumn` (op values): one slab per ≤ 4096-byte run of whole
/// values; a slab never splits a value, so there are at most
/// `min(values, bytes)` slabs. Data capacity is charged at 4× content (see
/// `Slabs::bound`); each slab carries a `RawSlab` (24 bytes) and a BIT
/// entry (8 bytes), both ×2 for doubling.
fn raw_column_bound(bytes: usize, values: usize) -> Result<usize> {
    let mut s = Sum::new();
    s.add(HEX_COLUMN_BASE)
        .mul(bytes, 4)
        .mul(values.min(bytes).saturating_add(1), 64);
    s.get()
}

// ── dimensions ──────────────────────────────────────────────────────────────

/// Dimensions of a resident document. Read without allocating.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DocDims {
    pub(crate) ops: usize,
    /// Bound on op-column heap (computed from `op_slabs`, `value_bytes`).
    pub(crate) op_col_bytes: usize,
    /// `RawColumn` value bytes.
    pub(crate) value_bytes: usize,
    pub(crate) objects: usize,
    /// Mark index cache entries.
    pub(crate) marks: usize,
    pub(crate) changes: usize,
    pub(crate) edges: usize,
    pub(crate) actors: usize,
    /// Bound on change-graph hexane column heap (from `graph_slabs`).
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
    pub(crate) op_slabs: Slabs,
    pub(crate) graph_slabs: Slabs,
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
        d.op_col_bytes = d
            .op_slabs
            .bound()
            .and_then(|b| cadd(b, raw_column_bound(d.value_bytes, d.ops)?))
            .unwrap_or(usize::MAX);
        d.graph_col_bytes = d.graph_slabs.bound().unwrap_or(usize::MAX);
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
    /// Total raw bytes of the changes (sum of chunk lengths).
    pub(crate) raw_bytes: usize,
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
            // Actor ids are stored verbatim in the normalized bytes.
            max_actor_len: shape.normalized_bytes,
            raw_bytes: shape.normalized_bytes,
        };
        dims.parsed_bytes = shape_parsed_bound(&dims).unwrap_or(usize::MAX);
        dims
    }
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
            edges.add(c.deps().len());
            raw.add(c.raw_bytes().len());
            actors.add(1 + c.other_actor_ids().len());
            d.max_op_counter = d.max_op_counter.max(c.max_op());
            if c.hash().fragment_level() >= 1 {
                d.fragments += 1;
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
        d.parsed_bytes = parsed.get()?;
        Ok(d)
    }
}

/// `Vec<Change>` heap bound from inspection only: per change the `Change`
/// value (×2 Vec slack) and the `change_heap_bound` terms summed over the
/// whole input: raw bytes ≤ normalized (×3), deps ≤ heads (×64), other
/// actors ≤ actors (`ActorId` ×2), spilled actor ids and messages ≤
/// normalized (×2 each).
fn shape_parsed_bound(d: &IncomingDims) -> Result<usize> {
    let mut s = Sum::new();
    s.mul3(d.changes, size_of::<Change>(), 2)
        .mul(d.changes, 64)
        .mul(d.raw_bytes, 3 + 2 + 2)
        .mul(d.heads, 2 * size_of::<ChangeHash>())
        .mul(d.actors, 2 * size_of::<crate::ActorId>());
    s.get()
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
    let dops = cadd(inc.ops, doc.queue_bytes)?; // ≤ 1 op per queued raw byte
    let dedges = cadd(inc.edges, doc.queue_bytes / size_of::<ChangeHash>())?;
    let dactors = inc.actors.min(cadd(doc.actors, inc.actors)?);
    let actors_after = cadd(doc.actors, dactors)?;
    let changes_after = cadd(doc.changes, dc)?;
    let frag_in = inc.fragments.min(dc).max(doc.queue_changes.min(dc));
    let fragments_after = cadd(doc.fragments, frag_in)?;

    let mut s = Sum::new();
    // ChangeGraph change-indexed Vecs.
    for (cap, elem) in c.graph_vecs.iter().zip(GRAPH_VEC_ELEMS) {
        s.add_sum(vec_growth(doc.changes, *cap, dc, elem));
    }
    // ChangeGraph::edges (Edge = NodeIdx u32 + Option<EdgeIdx> u32).
    s.add_sum(vec_growth(doc.edges, c.edges, dedges, 8));
    // ChangeGraph::nodes_by_hash: (ChangeHash, NodeIdx) = 36 bytes.
    s.add_sum(hash_table_growth(doc.changes, c.nodes_by_hash, dc, 36));
    // ChangeGraph::clock_cache: every node may be cached once (frontier
    // recursion in `cache_clock` caches uncached ancestors), each entry a
    // (NodeIdx, SeqClock) slot (4 + 24 padded to 32) plus a SeqClock of
    // width `actors_after` × 4 bytes, ×2 for the clone kept by
    // `cache_clock` while inserting.
    let new_entries = changes_after.saturating_sub(doc.clock_cache_entries);
    s.add_sum(hash_table_growth(
        doc.clock_cache_entries,
        c.clock_cache,
        new_entries,
        32,
    ))
    .mul3(new_entries, actors_after.max(1), 4 * 2);
    // Inserting actors (`insert_actor`) inserts one slot into every cached
    // clock and fragment clock; a full Vec doubles: ≤ 2 × width × 4 each.
    if dactors > 0 {
        s.mul3(
            doc.clock_cache_entries.saturating_add(doc.fragments),
            actors_after,
            2 * 4,
        );
        // Actor-column remap rebuilds (`Columns::remap_actors`,
        // `MarkIndexColumn::remap_values`) and the rebuilt `ObjIndex` and
        // mark cache: a second copy of the op columns and of the measured
        // std bytes is alive while the old one is dropped.
        s.add(doc.op_col_bytes).add(doc.exact_bytes);
    }
    // ChangeGraph::seq_index: outer Vec per actor, inner Vec per actor of
    // NodeIdx; an inner Vec reallocates to at most twice its capacity plus
    // the pushed entries.
    s.add_sum(vec_growth(doc.actors, c.seq_index, dactors, 24))
        .mul(c.seq_index_inner, 2 * 4)
        .mul3(dc, 2, 4);
    // ChangeGraph::fragments and fragment deps: a new level ≥ 1 fragment
    // can depend on every earlier fragment (quadratic, see report) and
    // carries a SeqClock of actor width.
    s.add_sum(vec_growth(doc.fragments, c.fragments, frag_in, 56))
        .mul3(frag_in, fragments_after, 2 * 4)
        .mul3(frag_in, actors_after, 2 * 4);
    // ChangeGraph::extra_bytes_raw: ≤ incoming raw bytes.
    s.add_sum(vec_growth(c.extra_raw.0, c.extra_raw.1, inc.raw_bytes, 1));
    // Change-graph hexane columns (num_ops, timestamps, messages,
    // extra_bytes_meta): regrown slab bound minus nothing (conservative).
    s.add_sum(doc.graph_slabs.grown(dc, inc.raw_bytes).bound());
    // Op columns: every incoming op inserts one row in each column (plus
    // succ rows for its preds); values/keys/mark names ≤ raw bytes.
    s.add_sum(
        doc.op_slabs
            .grown(dops.saturating_add(dedges), inc.raw_bytes)
            .bound(),
    )
    .add_sum(raw_column_bound(
        inc.value_bytes.min(inc.raw_bytes.max(inc.value_bytes)),
        dops,
    ));
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
    // Authors: `authors` Vec<Author> (24 + bytes ≤ raw), actor_to_author.
    s.mul(dactors, 2 * (24 + 16)).mul(inc.raw_bytes, 2);
    s.get()
}

/// Transient scratch of `apply_changes_batch_log_patches` with `q` changes
/// in the queue plus the batch, excluding the op application itself.
fn queue_scratch(doc: &DocDims, inc: &IncomingDims) -> Result<usize> {
    let q = cadd(doc.queue_changes, inc.changes)?;
    let e = cadd(inc.edges, doc.queue_bytes / size_of::<ChangeHash>())?;
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

/// Transient of `BatchApply` over `ops` ops from `changes` changes
/// (provisional: per-op `ChangeOp` with builder, preds and succ Vecs, the
/// sorted op Vec and per-object spans; per-change graph insertion).
fn batch_apply_scratch(doc: &DocDims, inc: &IncomingDims) -> Result<usize> {
    let changes = cadd(inc.changes, doc.queue_changes)?;
    let ops = cadd(inc.ops, doc.queue_bytes)?;
    let edges = cadd(inc.edges, doc.queue_bytes)?;
    let mut s = Sum::new();
    s.mul(ops, 1024)
        .mul(edges, 64)
        .mul(changes, 1024)
        .mul(inc.raw_bytes, 4)
        // clock computations over the graph: SeqClocks and BTreeSets of
        // visited nodes bounded by the graph size.
        .mul(cadd(doc.changes, changes)?, 64)
        .mul3(cadd(doc.actors, inc.actors)?, 4, 8)
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

/// Peak of loading normalized storage of this shape (includes the result).
/// Modelled as integrating every change into an empty document (the load
/// path builds the same graph, columns and indexes), plus the load
/// collectors (every change reconstructed and verified: `parsed_bytes`),
/// plus a full copy of the normalized input held while decoding.
pub(crate) fn load_peak_bound(incoming: &IncomingDims) -> Result<usize> {
    let empty = DocDims::default();
    let mut s = Sum::new();
    s.add_sum(growth_bound(&empty, incoming))
        .add_sum(batch_apply_scratch(&empty, incoming))
        .add(incoming.parsed_bytes)
        .mul(incoming.normalized_bytes, 2);
    s.get()
}

/// Upper bound on `save_with_options` output length: the columnar document
/// chunk re-encodes each op column (≤ its slab bytes), the change columns
/// (≤ graph bound + 32 per hash + 8 per edge), actor ids, and headers; a
/// DEFLATE pass never expands by more than 5 bytes per 16 KiB block + 64.
pub(crate) fn save_output_bound(doc: &DocDims) -> Result<usize> {
    let mut s = Sum::new();
    s.add(doc.op_col_bytes)
        .add(doc.graph_col_bytes)
        .mul(doc.changes, size_of::<ChangeHash>() + 16)
        .mul(doc.edges, 10)
        .mul(doc.heads, size_of::<ChangeHash>())
        .mul(doc.actors, 16)
        .add(doc.exact_bytes / 4)
        .add(4096);
    let raw = s.get()?;
    let mut t = Sum::new();
    t.add(raw).add(raw / 3000).add(64);
    t.get()
}

/// Transient peak of `save_with_options`, including its output: column
/// encoders write each column into a scratch Vec (≤ 2× by doubling), the
/// uncompressed chunk (≤ output bound, ×2 doubling) and the compressed
/// copy, plus the change-graph export (hashes, clocks) proportional to
/// the graph.
pub(crate) fn save_peak_bound(doc: &DocDims) -> Result<usize> {
    let out = save_output_bound(doc)?;
    let mut s = Sum::new();
    s.mul(out, 5)
        .mul(doc.changes, 128)
        .mul3(doc.actors, doc.actors.min(doc.changes).max(1), 4)
        .add(64 * 1024);
    s.get()
}

/// Peak of exporting up to `changes` canonical changes (`get_changes`),
/// including the returned `Vec<Change>`: each change is rebuilt from the
/// columns (its ops ≤ all ops, bytes ≤ the save output), so the total of
/// the returned changes is bounded by the document's encoded size plus
/// per-change headers, ×2 for build scratch.
pub(crate) fn history_peak_bound(doc: &DocDims, changes: usize) -> Result<usize> {
    let n = changes.min(doc.changes);
    let out = save_output_bound(doc)?;
    // Actor references across rebuilt changes: each op names at most three
    // actors (id, obj, key/pred) and each change its own author actor.
    let actor_refs = cadd(doc.ops.saturating_mul(3).saturating_add(doc.succ), n)?;
    let mut s = Sum::new();
    // Rebuilt raw bytes: the op data is at most the encoded columns; actor
    // ids are repeated per change; 128 bytes of header/hash per change.
    // ×2 for build scratch (encoders write into doubling Vecs).
    s.mul(out, 4)
        .mul3(actor_refs, doc.max_actor_len.max(16), 2)
        .mul3(n, 128, 2)
        // the returned Vec<Change> (×2 doubling) and each parsed Change's
        // other_actors Vec / dependency list.
        .mul3(n, size_of::<Change>(), 2)
        .mul3(actor_refs, size_of::<crate::ActorId>(), 2)
        .mul3(doc.edges, size_of::<ChangeHash>(), 2)
        // graph traversal (visited sets, clocks) over all changes.
        .mul(doc.changes, 64)
        .add(64 * 1024);
    s.get()
}

/// Retained bytes of `changes` parsed incoming changes (`Vec<Change>`).
pub(crate) fn incoming_changes_bound(incoming: &IncomingDims) -> Result<usize> {
    Ok(incoming.parsed_bytes)
}
