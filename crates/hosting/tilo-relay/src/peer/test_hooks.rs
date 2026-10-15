//! Test hooks: the `test-support` checkpoint-prefix fault marker and the
//! reader-backlog observers the private-transport tests watch.

#[cfg(test)]
use super::{Arc, Envelope, EnvelopeV2, HashMap};

#[cfg(feature = "test-support")]
const CHECKPOINT_PREFIX_FAULT_ARMED: &str = ".checkpoint-prefix-post-first-promotion.armed";
#[cfg(feature = "test-support")]
const CHECKPOINT_PREFIX_FAULT_FIRED: &str = ".checkpoint-prefix-post-first-promotion.fired";

/// State of the test-only store marker used by the native-owner
/// reconnect regression. This API is absent from production builds.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointPrefixPromotionFaultState {
    /// No marker: the fault is not armed.
    Absent,
    /// Armed: the next first promotion trips the fault.
    Armed,
    /// The fault fired after the first promotion.
    Fired,
}

#[cfg(feature = "test-support")]
pub(super) fn checkpoint_prefix_post_first_promotion_fault(
    root: &std::path::Path,
) -> Result<(), crate::store::StoreError> {
    let armed = root.join(CHECKPOINT_PREFIX_FAULT_ARMED);
    let fired = root.join(CHECKPOINT_PREFIX_FAULT_FIRED);
    match std::fs::rename(armed, fired) {
        Ok(()) => Err(crate::persistence::error(
            "injected checkpoint prefix failure after first promotion",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(feature = "test-support"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the fallible test-support fault hook it stands in for"
)]
pub(super) fn checkpoint_prefix_post_first_promotion_fault(
    _root: &std::path::Path,
) -> Result<(), crate::store::StoreError> {
    Ok(())
}

#[cfg(test)]
static READER_BACKLOG_OBSERVERS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<u64, Arc<std::sync::atomic::AtomicUsize>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(super) fn track_reader_backlog(workspace_id: u64) -> Arc<std::sync::atomic::AtomicUsize> {
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    READER_BACKLOG_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(workspace_id, observed.clone());
    observed
}

#[cfg(test)]
pub(super) fn observe_reader_backlog(frame: &EnvelopeV2, len: usize) {
    let EnvelopeV2::State(Envelope::Delta { workspace_id, .. }) = frame else {
        return;
    };
    if let Some(observed) = READER_BACKLOG_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(workspace_id)
    {
        observed.store(len, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
pub(super) fn stop_tracking_reader_backlog(workspace_id: u64) {
    READER_BACKLOG_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&workspace_id);
}
