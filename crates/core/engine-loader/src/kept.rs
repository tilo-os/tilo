//! What a window hands an engine of the same process without it passing through a record: a
//! request's context (the site's cookies, the page it came from) kept here under a one-use
//! handle.
//!
//! A call's arguments and a view's state are records, and a site's cookies must never be
//! written into one. So the window keeps the context here and the app carries only the handle;
//! the engine redeems it once, through its [`Device`](crate::Device), when its job starts. A
//! handle is only good in the process that kept it, only once, and only while the window that
//! kept it is open: [`forget`] drops what a closing window never handed over.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// One kept context: the window that kept it, and what it holds.
struct Kept {
    owner: u64,
    context: String,
}

static KEPT: Mutex<Option<HashMap<String, Kept>>> = Mutex::new(None);

/// A number for one window that keeps contexts here, distinct from every other in the process.
#[must_use]
pub fn owner() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Keep `context` for `owner`, and answer the handle that redeems it once.
#[must_use]
pub fn keep(owner: u64, context: String) -> String {
    static COUNT: AtomicU64 = AtomicU64::new(1);
    let mut unguessable = RandomState::new().build_hasher();
    unguessable.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    let handle = format!(
        "{:016x}{:016x}",
        unguessable.finish(),
        RandomState::new().build_hasher().finish()
    );
    KEPT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(handle.clone(), Kept { owner, context });
    handle
}

/// The context `handle` names, which it no longer names after this: `None` when it was redeemed
/// already, its window closed, or this process never kept it.
#[must_use]
pub fn redeem(handle: &str) -> Option<String> {
    KEPT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()?
        .remove(handle)
        .map(|kept| kept.context)
}

/// Drop every context `owner` kept and nothing redeemed: its window closed.
pub fn forget(owner: u64) {
    if let Some(kept) = KEPT.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        kept.retain(|_, kept| kept.owner != owner);
    }
}

#[cfg(test)]
mod tests {
    use super::{forget, keep, owner, redeem};

    #[test]
    fn a_handle_redeems_once_and_not_after_its_window_closed() {
        let window = owner();
        let first = keep(window, "cookies=1".to_owned());
        let second = keep(window, "cookies=2".to_owned());
        assert_ne!(first, second);
        assert_eq!(redeem(&first).as_deref(), Some("cookies=1"));
        assert_eq!(redeem(&first), None, "a handle is good once");
        forget(window);
        assert_eq!(redeem(&second), None, "a closed window's context is gone");
        assert_eq!(redeem("0123"), None);
    }
}
