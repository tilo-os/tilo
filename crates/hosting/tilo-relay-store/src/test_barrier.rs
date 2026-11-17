//! Test-only, one-shot barriers scoped to an exact temporary store and operation.
//!
//! Built only for tests and the `test-support` feature: the store and its
//! owner pause or fail at named points so a test can interleave them
//! deterministically.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError},
};
use tilo_lock::MutexRecover as _;
type Key = (PathBuf, &'static str);
struct Hold {
    released: Mutex<bool>,
    release: Condvar,
    reached: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}
/// A panicking test must not wedge every later one on a poisoned registry.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock_recover()
}
fn holds() -> &'static Mutex<HashMap<Key, Arc<Hold>>> {
    static HOLDS: OnceLock<Mutex<HashMap<Key, Arc<Hold>>>> = OnceLock::new();
    HOLDS.get_or_init(Mutex::default)
}
/// Releases its barrier when dropped.
pub struct Release {
    key: Key,
    hold: Arc<Hold>,
}
impl Drop for Release {
    fn drop(&mut self) {
        *lock(&self.hold.released) = true;
        self.hold.release.notify_all();
        lock(holds()).remove(&self.key);
    }
}
/// Hold the next [`pause`] of `operation` on `root` until the returned
/// [`Release`] drops; the receiver fires when it is reached.
pub fn install(
    root: &Path,
    operation: &'static str,
) -> (Release, tokio::sync::oneshot::Receiver<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let key = (root.to_path_buf(), operation);
    let hold = Arc::new(Hold {
        released: Mutex::new(false),
        release: Condvar::new(),
        reached: Mutex::new(Some(tx)),
    });
    assert!(lock(holds()).insert(key.clone(), hold.clone()).is_none());
    (Release { key, hold }, rx)
}
/// Report that `operation` on `root` was reached, without holding it.
pub fn signal(root: &Path, operation: &'static str) {
    let hold = lock(holds()).get(&(root.to_path_buf(), operation)).cloned();
    if let Some(hold) = hold
        && let Some(reached) = lock(&hold.reached).take()
    {
        let _ = reached.send(());
    }
}
/// Wait at `operation` on `root` while a barrier for it is armed.
pub fn pause(root: &Path, operation: &'static str) {
    let hold = lock(holds()).remove(&(root.to_path_buf(), operation));
    if let Some(hold) = hold {
        let reached = lock(&hold.reached).take();
        if let Some(reached) = reached {
            let _ = reached.send(());
        }
        let mut released = lock(&hold.released);
        while !*released {
            released = hold
                .release
                .wait(released)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

// Deterministic I/O faults, scoped to a temporary path and removed on unwind.
fn faults() -> &'static Mutex<std::collections::HashSet<Key>> {
    static FAULTS: OnceLock<Mutex<std::collections::HashSet<Key>>> = OnceLock::new();
    FAULTS.get_or_init(Mutex::default)
}
/// Removes its injected fault when dropped.
pub struct Fault(Key);
impl Drop for Fault {
    fn drop(&mut self) {
        lock(faults()).remove(&self.0);
    }
}
/// Make every [`check`] of `operation` on `root` fail until the returned
/// [`Fault`] drops.
pub fn fail(root: &Path, operation: &'static str) -> Fault {
    let key = (root.to_path_buf(), operation);
    assert!(lock(faults()).insert(key.clone()));
    Fault(key)
}
/// [`pause`], then fail when a fault for `operation` on `root` is injected.
pub fn check(root: &Path, operation: &'static str) -> std::io::Result<()> {
    pause(root, operation);
    if lock(faults()).contains(&(root.to_path_buf(), operation)) {
        Err(std::io::Error::other(format!(
            "injected {operation} failure"
        )))
    } else {
        Ok(())
    }
}
