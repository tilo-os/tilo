//! Owned synchronous mount custody, with no registry loan across host callbacks.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Busy;

pub(crate) struct Lease<'a, T> {
    mounts: &'a RefCell<HashMap<u64, T>>,
    servicing: &'a RefCell<HashSet<u64>>,
    id: u64,
    mount: Option<T>,
}
impl<'a, T> Lease<'a, T> {
    pub(crate) fn take(
        mounts: &'a RefCell<HashMap<u64, T>>,
        servicing: &'a RefCell<HashSet<u64>>,
        id: u64,
    ) -> Result<Option<Self>, Busy> {
        reject_reentry(servicing)?;
        let mut registered = mounts.try_borrow_mut().map_err(|_| Busy)?;
        let Some(mount) = registered.remove(&id) else {
            return Ok(None);
        };
        servicing.borrow_mut().insert(id);
        drop(registered);
        Ok(Some(Self {
            mounts,
            servicing,
            id,
            mount: Some(mount),
        }))
    }
    pub(crate) fn mount(&mut self) -> Result<&mut T, Busy> {
        self.mount.as_mut().ok_or(Busy)
    }
    /// Terminal close owns the value without restoring it. The lease remains
    /// Busy until original close callbacks and durable draining have returned.
    pub(crate) fn take_mount(&mut self) -> Result<T, Busy> {
        self.mount.take().ok_or(Busy)
    }
}
impl<T> Drop for Lease<'_, T> {
    fn drop(&mut self) {
        // Every entry capable of closing/replacing this owner refuses before
        // its first effect while Busy. Restore the owner before releasing Busy.
        if let Some(mount) = self.mount.take() {
            let replaced = self.mounts.borrow_mut().insert(self.id, mount);
            assert!(replaced.is_none(), "leased mount was replaced");
        }
        self.servicing.borrow_mut().remove(&self.id);
    }
}
/// Construction has no registered mount yet; keep the same runtime Busy
/// boundary through its original factory, insertion and status callbacks.
pub(crate) struct Construction<'a> {
    servicing: &'a RefCell<HashSet<u64>>,
    id: u64,
}
impl<'a> Construction<'a> {
    pub(crate) fn enter(servicing: &'a RefCell<HashSet<u64>>, id: u64) -> Result<Self, Busy> {
        reject_reentry(servicing)?;
        servicing.try_borrow_mut().map_err(|_| Busy)?.insert(id);
        Ok(Self { servicing, id })
    }
}
impl Drop for Construction<'_> {
    fn drop(&mut self) {
        self.servicing.borrow_mut().remove(&self.id);
    }
}

struct ReplayBatch<'a, E> {
    active: &'a std::cell::Cell<bool>,
    deferred: &'a RefCell<Vec<E>>,
    remaining: std::collections::VecDeque<E>,
}
impl<E> Drop for ReplayBatch<'_, E> {
    fn drop(&mut self) {
        if !self.remaining.is_empty() {
            let mut pending = self.deferred.borrow_mut();
            let mut prefix: Vec<_> = self.remaining.drain(..).collect();
            prefix.append(&mut *pending);
            *pending = prefix;
        }
        self.active.set(false);
    }
}
/// Deliver one finite original snapshot. Nested handlers do not recursively
/// drain late events; failure/unwind preserves the old prefix before them.
pub(crate) fn replay_snapshot<E: Clone>(
    active: &std::cell::Cell<bool>,
    servicing: &RefCell<HashSet<u64>>,
    deferred: &RefCell<Vec<E>>,
    mut deliver: impl FnMut(E),
) -> Result<bool, Busy> {
    if active.get() {
        return Ok(false);
    }
    let events = take_deferred(servicing, deferred)?;
    active.set(true);
    let mut batch = ReplayBatch {
        active,
        deferred,
        remaining: events.into(),
    };
    while let Some(event) = batch.remaining.front().cloned() {
        deliver(event);
        batch.remaining.pop_front();
    }
    Ok(!deferred.borrow().is_empty())
}

pub(crate) fn reject_reentry(servicing: &RefCell<HashSet<u64>>) -> Result<(), Busy> {
    if servicing.try_borrow().map_err(|_| Busy)?.is_empty() {
        Ok(())
    } else {
        Err(Busy)
    }
}
pub(crate) fn guarded_ingress<R>(
    servicing: &RefCell<HashSet<u64>>,
    run: impl FnOnce() -> R,
) -> Result<R, Busy> {
    reject_reentry(servicing)?;
    Ok(run())
}
pub(crate) fn admit_or_defer<E>(
    servicing: &RefCell<HashSet<u64>>,
    deferred: &RefCell<Vec<E>>,
    event: E,
) -> Option<E> {
    if reject_reentry(servicing).is_err() {
        deferred.borrow_mut().push(event);
        None
    } else {
        Some(event)
    }
}
pub(crate) fn take_deferred<E>(
    servicing: &RefCell<HashSet<u64>>,
    deferred: &RefCell<Vec<E>>,
) -> Result<Vec<E>, Busy> {
    guarded_ingress(servicing, || std::mem::take(&mut *deferred.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn destructive_sink_and_close_registries_are_untouched_during_lease() {
        let mounts = RefCell::new(HashMap::from([(7, 11)]));
        let servicing = RefCell::new(HashSet::new());
        let requests = RefCell::new(vec![3, 4]);
        let desktops = RefCell::new(HashMap::from([(7, 19)]));
        {
            let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
            assert!(mounts.try_borrow_mut().is_ok());
            *lease.mount().unwrap() = 17;
            let taken = guarded_ingress(&servicing, || std::mem::take(&mut *requests.borrow_mut()));
            assert_eq!(taken, Err(Busy));
            assert_eq!(*requests.borrow(), vec![3, 4]);
            let closed = guarded_ingress(&servicing, || {
                requests.borrow_mut().clear();
                desktops.borrow_mut().remove(&7);
                mounts.borrow_mut().remove(&7)
            });
            assert_eq!(closed, Err(Busy));
            assert_eq!(desktops.borrow()[&7], 19);
            assert_eq!(*requests.borrow(), vec![3, 4]);
        }
        assert_eq!(mounts.borrow()[&7], 17);
        assert_eq!(
            guarded_ingress(&servicing, || std::mem::take(&mut *requests.borrow_mut())).unwrap(),
            vec![3, 4]
        );
    }
    #[test]
    fn typed_events_including_shutdown_sentinel_retain_original_order() {
        let mounts = RefCell::new(HashMap::from([(7, 11)]));
        let servicing = RefCell::new(HashSet::new());
        let deferred = RefCell::new(Vec::new());
        {
            let _lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
            for event in [7, 0, 9] {
                assert!(admit_or_defer(&servicing, &deferred, event).is_none());
            }
            assert_eq!(take_deferred(&servicing, &deferred), Err(Busy));
        }
        assert_eq!(take_deferred(&servicing, &deferred).unwrap(), vec![7, 0, 9]);
        assert!(deferred.borrow().is_empty());
    }
    #[test]
    fn error_and_unwind_restore_owner_without_close_or_resurrection() {
        let mounts = RefCell::new(HashMap::from([(7, 11)]));
        let servicing = RefCell::new(HashSet::new());
        let callback = Cell::new(false);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
            *lease.mount().unwrap() = 23;
            assert_eq!(
                guarded_ingress(&servicing, || callback.set(true)),
                Err(Busy)
            );
            panic!("actual leased callback unwind");
        }));
        assert!(failed.is_err());
        assert!(!callback.get());
        assert_eq!(mounts.borrow()[&7], 23);
        assert!(servicing.borrow().is_empty());
        assert!(Lease::take(&mounts, &servicing, 99).unwrap().is_none());
        assert!(servicing.borrow().is_empty());
    }
    #[test]
    fn terminal_close_keeps_busy_through_original_effects_and_never_restores_owner() {
        let mounts = RefCell::new(HashMap::from([(7, 11)]));
        let servicing = RefCell::new(HashSet::new());
        let callbacks = Cell::new(0);
        {
            let mut lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
            let owner = lease.take_mount().unwrap();
            assert_eq!(owner, 11);
            assert!(mounts.borrow().is_empty());
            assert_eq!(
                guarded_ingress(&servicing, || callbacks.set(callbacks.get() + 1)),
                Err(Busy)
            );
            assert_eq!(Lease::take(&mounts, &servicing, 7).err(), Some(Busy));
        }
        assert_eq!(callbacks.get(), 0);
        assert!(mounts.borrow().is_empty());
        assert!(servicing.borrow().is_empty());
        assert!(Lease::take(&mounts, &servicing, 7).unwrap().is_none());
    }
    #[test]
    fn construction_callbacks_refuse_destructive_ingress_and_unwind_releases_busy() {
        let servicing = RefCell::new(HashSet::new());
        let requests = RefCell::new(vec![7]);
        let touched = Cell::new(false);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _construction = Construction::enter(&servicing, 7).unwrap();
            assert_eq!(
                guarded_ingress(&servicing, || {
                    touched.set(true);
                    requests.borrow_mut().clear();
                }),
                Err(Busy)
            );
            assert!(Construction::enter(&servicing, 9).is_err());
            panic!("original constructor/status callback unwind");
        }));
        assert!(failed.is_err());
        assert!(!touched.get());
        assert_eq!(*requests.borrow(), vec![7]);
        assert!(servicing.borrow().is_empty());
    }
    #[test]
    fn replay_keeps_original_unvisited_events_before_callback_late_events_without_recursion() {
        let servicing = RefCell::new(HashSet::new());
        let mounts = RefCell::new(HashMap::from([(7, 0_u8)]));
        let deferred = RefCell::new(vec![1, 2]);
        let active = Cell::new(false);
        let seen = RefCell::new(Vec::new());
        let late = replay_snapshot(&active, &servicing, &deferred, |event| {
            seen.borrow_mut().push(event);
            if event == 1 {
                let _lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
                assert!(admit_or_defer(&servicing, &deferred, 3).is_none());
            }
            assert!(
                !replay_snapshot(&active, &servicing, &deferred, |event| seen
                    .borrow_mut()
                    .push(event))
                .unwrap()
            );
        })
        .unwrap();
        assert!(late);
        assert_eq!(*seen.borrow(), vec![1, 2]);
        assert!(
            !replay_snapshot(&active, &servicing, &deferred, |event| seen
                .borrow_mut()
                .push(event))
            .unwrap()
        );
        assert_eq!(*seen.borrow(), vec![1, 2, 3]);
    }
    #[test]
    fn replay_unwind_restores_failed_and_unvisited_prefix_before_late_shutdown_sentinel() {
        let servicing = RefCell::new(HashSet::new());
        let mounts = RefCell::new(HashMap::from([(7, 0_u8)]));
        let deferred = RefCell::new(vec![1, 2]);
        let active = Cell::new(false);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            replay_snapshot(&active, &servicing, &deferred, |_| {
                let _lease = Lease::take(&mounts, &servicing, 7).unwrap().unwrap();
                assert!(admit_or_defer(&servicing, &deferred, 0).is_none());
                panic!("original typed handler unwind");
            })
            .unwrap();
        }));
        assert!(failed.is_err());
        assert!(!active.get());
        assert_eq!(*deferred.borrow(), vec![1, 2, 0]);
        assert!(mounts.borrow().contains_key(&7));
        assert!(servicing.borrow().is_empty());
    }
}
