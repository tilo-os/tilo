//! Test hooks: the named test points the session paths pass through.

#[cfg(test)]
use super::SharedWorkspace;

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
