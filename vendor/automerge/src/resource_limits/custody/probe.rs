//! Calibration probe (lane L1): exposes the dimension bounds to the
//! out-of-crate heap-probe calibration test
//! (`crates/doc/tilo-automerge/tests/bounds_calibration.rs`). Read-only and
//! hidden; not part of the supported surface.
use super::super::dims::{self, DocDims, IncomingDims};
use super::super::{NativeResourceError, NativeShape};
use crate::{AutoCommit, Change};

/// Bounds computed for one document (and optional incoming changes).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeBoundProbe {
    pub retained: usize,
    pub clone_peak: usize,
    pub apply_peak: usize,
    pub superlinear_apply: usize,
    pub save_output: usize,
    pub save_peak: usize,
    pub history_peak: usize,
    pub incoming_changes: usize,
}

impl AutoCommit {
    #[doc(hidden)]
    pub fn native_bound_probe(
        &self,
        incoming: &[Change],
    ) -> Result<NativeBoundProbe, NativeResourceError> {
        let doc = DocDims::of(self);
        let inc = IncomingDims::from_changes(incoming)?;
        Ok(NativeBoundProbe {
            retained: dims::retained_bound(&doc)?,
            clone_peak: dims::clone_peak_bound(&doc)?,
            apply_peak: dims::apply_peak_bound(&doc, &inc)?,
            superlinear_apply: dims::superlinear_apply_bound(&doc, &inc)?,
            save_output: dims::save_output_bound(&doc)?,
            save_peak: dims::save_peak_bound(&doc)?,
            history_peak: dims::history_peak_bound(&doc, doc.changes)?,
            incoming_changes: dims::incoming_changes_bound(&inc)?,
        })
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
