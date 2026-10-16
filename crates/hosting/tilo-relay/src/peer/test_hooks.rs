//! Test hooks: the named test points the session paths pass through, the
//! `test-support` checkpoint-prefix fault marker and the reader-backlog
//! observers the private-transport tests watch.

#[cfg(test)]
use super::{Arc, Envelope, EnvelopeV2, HashMap, SharedWorkspace};

/// A named store test barrier in a peer path, in one greppable form.
///
/// Outside `cfg(test)` every form expands to nothing and none of its
/// arguments is evaluated, so a point costs production nothing. A root is a
/// `&Path` expression, or `room WS` for the store root of the
/// `SharedWorkspace` `WS`, read under the room's lock and released before
/// the barrier is reached.
///
/// - `pause(ROOT, "name")` waits while a test holds the barrier.
/// - `signal(ROOT, "name")` reports the point reached, without waiting.
/// - `check(ROOT, "name")?` pauses, then returns the failure a test injected.
/// - `pause(ROOT, "name") on WORKERS` pauses on a blocking worker of the
///   `WORKERS` pool, so the room's async owner is not parked.
/// - `if COND => POINT` reaches `POINT` only when `COND` holds.
/// - `if COND => check(ROOT, "name") else VALUE = FAULT` rebinds `VALUE` to
///   `FAULT` when `COND` holds and the check fails; `ROOT` is read only when
///   `COND` holds.
macro_rules! test_point {
    (if $cond:expr => $op:ident(room $workspace:expr, $name:literal) $($rest:tt)*) => {
        crate::peer::test_hooks::test_point!(
            if $cond => $op(
                &crate::peer::test_hooks::store_root(&$workspace).await, $name
            ) $($rest)*
        )
    };
    ($op:ident(room $workspace:expr, $name:literal) $($rest:tt)*) => {
        crate::peer::test_hooks::test_point!(
            $op(&crate::peer::test_hooks::store_root(&$workspace).await, $name) $($rest)*
        )
    };
    (if $cond:expr => check($root:expr, $name:literal) else $value:ident = $fault:expr) => {
        #[cfg(test)]
        let $value = crate::peer::test_hooks::inject_fault(
            $value,
            $cond && crate::store::test_barrier::check($root, $name).is_err(),
            || $fault,
        );
    };
    (if $cond:expr => $($point:tt)+) => {
        #[cfg(test)]
        if $cond {
            crate::peer::test_hooks::test_point!($($point)+);
        }
    };
    (pause($root:expr, $name:literal)) => {
        #[cfg(test)]
        crate::store::test_barrier::pause($root, $name);
    };
    (signal($root:expr, $name:literal)) => {
        #[cfg(test)]
        crate::store::test_barrier::signal($root, $name);
    };
    (check($root:expr, $name:literal)?) => {
        #[cfg(test)]
        crate::store::test_barrier::check($root, $name)?;
    };
    (pause($root:expr, $name:literal) on $workers:expr) => {
        #[cfg(test)]
        {
            let root = std::path::Path::to_path_buf($root);
            crate::persistence::blocking($workers, move || {
                crate::store::test_barrier::pause(&root, $name);
            })
            .await?;
        }
    };
}
pub(super) use test_point;

/// `value`, or `fault()` when a test injected a failure.
#[cfg(test)]
pub(super) fn inject_fault<T>(value: T, failed: bool, fault: impl FnOnce() -> T) -> T {
    if failed { fault() } else { value }
}

/// The store root of `workspace`, read under its lock and released at once.
#[cfg(test)]
pub(super) async fn store_root(workspace: &SharedWorkspace) -> std::path::PathBuf {
    workspace.lock().await.store.root().to_path_buf()
}

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
