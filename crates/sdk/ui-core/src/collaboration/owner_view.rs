//! Session-local native owner-view control. Documents retain their normal CRDT
//! routes; these messages carry presentation only and grant no focus or input.
use super::ContinuationMetadata;

pub use collab_types::handoff::{OwnerProjection, OwnerView};

/// Device-local lifetime; never persisted as document state.
#[derive(Default)]
pub(crate) struct OwnerViewRuntime {
    pub(crate) last_generation: u64,
    pub(crate) last_claim_at: Option<crate::time::Instant>,
    pub(crate) last_stamp: Option<(u64, String)>,
    pub(crate) pending: Option<ContinuationMetadata>,
    pub(crate) following: bool,
    pub(crate) awaiting_claim: bool,
}
