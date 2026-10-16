//! Locking that outlives a panicking holder.
//!
//! A std lock is poisoned when a thread panics while holding it, and every
//! later `lock()` returns an error carrying the guard. A caller that keeps
//! using the state after such a panic takes the guard regardless; these traits
//! say so once instead of `.unwrap_or_else(PoisonError::into_inner)` at every
//! call.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// [`Mutex::lock`], recovering the guard of a poisoned mutex.
pub trait MutexRecover<T: ?Sized> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> MutexRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// [`RwLock::read`] and [`RwLock::write`], recovering the guard of a poisoned
/// lock.
pub trait RwLockRecover<T: ?Sized> {
    fn read_recover(&self) -> RwLockReadGuard<'_, T>;
    fn write_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> RwLockRecover<T> for RwLock<T> {
    fn read_recover(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_recover(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_lock_still_hands_out_its_value() {
        let mutex = Arc::new(Mutex::new(1));
        let rwlock = Arc::new(RwLock::new(1));
        let (m, r) = (Arc::clone(&mutex), Arc::clone(&rwlock));
        let _ = std::thread::spawn(move || {
            let _m = m.lock_recover();
            let _r = r.write_recover();
            panic!("poison both");
        })
        .join();
        assert!(mutex.is_poisoned() && rwlock.is_poisoned());
        *mutex.lock_recover() += 1;
        *rwlock.write_recover() += 1;
        assert_eq!((*mutex.lock_recover(), *rwlock.read_recover()), (2, 2));
    }
}
