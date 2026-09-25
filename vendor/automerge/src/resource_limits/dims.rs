//! Conservative allocation bounds from document dimensions.
//!
//! OWNER: lane L1. Every bound is a closed-form upper bound on *requested
//! allocation bytes* (not RSS) of unmodified automerge 0.12 / hexane 1.0
//! routines, derived from `size_of` of the element types with explicit
//! growth factors (Vec doubling, hashbrown bucket rounding + rehash peak,
//! BTree node bound), and calibrated against the `tilo-automerge`
//! heap probe. Each coefficient must name the field(s) it covers.
//! Reading dimensions allocates nothing.
use super::{NativeResourceError, NativeShape};

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

/// Dimensions of a resident document. Read in O(actors + columns).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DocDims {
    pub(crate) ops: usize,
    pub(crate) op_col_bytes: usize,
    pub(crate) value_bytes: usize,
    pub(crate) objects: usize,
    pub(crate) marks: usize,
    pub(crate) changes: usize,
    pub(crate) edges: usize,
    pub(crate) actors: usize,
    pub(crate) graph_col_bytes: usize,
    pub(crate) clock_cache_entries: usize,
    pub(crate) queue_changes: usize,
    pub(crate) queue_bytes: usize,
    pub(crate) patch_log_events: usize,
    pub(crate) heads: usize,
    pub(crate) pending_ops: usize,
}
impl DocDims {
    pub(crate) fn of(doc: &crate::AutoCommit) -> Self {
        let _ = doc;
        todo!("L1")
    }
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
}
impl IncomingDims {
    pub(crate) fn from_shape(shape: &NativeShape) -> Self {
        let _ = shape;
        todo!("L1")
    }
    pub(crate) fn from_changes(changes: &[crate::Change]) -> Result<Self> {
        let _ = changes;
        todo!("L1")
    }
}

/// Resident footprint bound.
pub(crate) fn retained_bound(doc: &DocDims) -> Result<usize> {
    let _ = doc;
    todo!("L1")
}
/// Peak of `AutoCommit::clone` (the copy is the retained bound of the source).
pub(crate) fn clone_peak_bound(doc: &DocDims) -> Result<usize> {
    let _ = doc;
    todo!("L1")
}
/// Transient peak plus retained growth of `apply_changes_batch` applying
/// `incoming` together with every queued change.
pub(crate) fn apply_peak_bound(doc: &DocDims, incoming: &IncomingDims) -> Result<usize> {
    let _ = (doc, incoming);
    todo!("L1")
}
/// Peak of loading normalized storage of this shape (includes the result).
pub(crate) fn load_peak_bound(incoming: &IncomingDims) -> Result<usize> {
    let _ = incoming;
    todo!("L1")
}
/// Upper bound on `save_with_options` output length.
pub(crate) fn save_output_bound(doc: &DocDims) -> Result<usize> {
    let _ = doc;
    todo!("L1")
}
/// Transient peak of `save_with_options`, including its output.
pub(crate) fn save_peak_bound(doc: &DocDims) -> Result<usize> {
    let _ = doc;
    todo!("L1")
}
/// Peak of exporting up to `changes` canonical changes (`get_changes`),
/// including the returned `Vec<Change>`.
pub(crate) fn history_peak_bound(doc: &DocDims, changes: usize) -> Result<usize> {
    let _ = (doc, changes);
    todo!("L1")
}
/// Retained bytes of `changes` parsed incoming changes (`Vec<Change>`).
pub(crate) fn incoming_changes_bound(incoming: &IncomingDims) -> Result<usize> {
    let _ = incoming;
    todo!("L1")
}
