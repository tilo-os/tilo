//! Calibration probe (lane L1): exposes the dimension bounds to the
//! out-of-crate heap-probe calibration test
//! (`crates/doc/tilo-automerge/tests/bounds_calibration.rs`). Read-only and
//! hidden; not part of the supported surface.
use super::super::dims::{self, DocDims, IncomingDims};
use super::super::{NativeResourceError, NativeShape};
use crate::{AutoCommit, Change};

impl AutoCommit {
    /// Bounds for this document and `incoming`, in order: retained, clone
    /// peak, apply peak, super-linear apply, save output, save peak,
    /// history peak (all changes), parsed `incoming` changes.
    /// `op_preds` is the number of op predecessor references in `incoming`
    /// (callers of the real paths get it from the inspected shape).
    #[doc(hidden)]
    pub fn native_bound_probe(
        &self,
        incoming: &[Change],
        op_preds: usize,
    ) -> Result<[usize; 8], NativeResourceError> {
        let doc = DocDims::of(self);
        let mut inc = IncomingDims::from_changes(incoming)?;
        inc.edges = inc.edges.saturating_add(op_preds);
        Ok([
            dims::retained_bound(&doc)?,
            dims::clone_peak_bound(&doc)?,
            dims::apply_peak_bound(&doc, &inc)?,
            dims::superlinear_apply_bound(&doc, &inc)?,
            dims::save_output_bound(&doc)?,
            dims::save_peak_bound(&doc)?,
            dims::history_peak_bound(&doc, doc.changes)?,
            dims::incoming_changes_bound(&inc)?,
        ])
    }
}

impl NativeShape {
    /// `load_peak_bound` of this shape plus the fragment term a loader adds
    /// once it has counted `fragments` fragment-level hashes among the
    /// reconstructed changes of a document `actors` wide.
    #[doc(hidden)]
    pub fn native_load_peak_probe(
        &self,
        fragments: usize,
        actors: usize,
    ) -> Result<usize, NativeResourceError> {
        let inc = IncomingDims::from_shape(self);
        let base = dims::load_peak_bound(&inc)?;
        let frag = dims::fragment_growth_bound(0, fragments, actors, inc.changes)?;
        base.checked_add(frag).ok_or(NativeResourceError::WorkBytes)
    }
}
