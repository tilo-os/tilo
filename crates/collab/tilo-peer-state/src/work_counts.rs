//! Scoped production work accounting, enabled only for integration tests.
//!
//! A scope attributes synchronous work on its thread to one owner. Owners
//! enter it at their mutation/read boundary, not at the test's call site.
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Cumulative actual work; take differences after quiescing the owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkSnapshot {
    /// Missing dependencies fetched or inspected by the recovery-history walk.
    pub recovery_dependencies: u64,
    /// Changes reconstructed from the admitted document (not metadata lookups).
    pub recovery_changes: u64,
    /// Whole native authority record serializer invocations, including preflights.
    pub record_serializations: u64,
}

/// Shared by one owner's clones and all threads that serve that owner.
#[derive(Clone, Default)]
pub struct WorkCounts(Arc<Counters>);
#[derive(Default)]
struct Counters {
    dependencies: AtomicU64,
    changes: AtomicU64,
    serializations: AtomicU64,
}
thread_local! {
    static CURRENT: RefCell<Option<WorkCounts>> = const { RefCell::new(None) };
    static OBSERVERS: RefCell<Vec<(WorkCounts, WorkCounts)>> = const { RefCell::new(Vec::new()) };
}

struct Observation {
    depth: usize,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for Observation {
    fn drop(&mut self) {
        OBSERVERS.with(|observers| observers.borrow_mut().truncate(self.depth));
    }
}
/// Restores the preceding attribution, including on early return or unwind.
#[must_use]
pub struct WorkScope {
    previous: Option<WorkCounts>,
    // Dropping on another thread would restore the wrong attribution.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for WorkScope {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
    }
}
impl WorkCounts {
    #[must_use]
    pub fn snapshot(&self) -> WorkSnapshot {
        WorkSnapshot {
            recovery_dependencies: self.0.dependencies.load(Ordering::Relaxed),
            recovery_changes: self.0.changes.load(Ordering::Relaxed),
            record_serializations: self.0.serializations.load(Ordering::Relaxed),
        }
    }
    /// Measure only this owner's synchronous turns inside `operation` on this
    /// thread. This observes attribution; it does not attribute unowned work.
    /// Nested measurements also contribute to their enclosing measurement.
    pub fn measure<R>(&self, operation: impl FnOnce() -> R) -> (R, WorkSnapshot) {
        let measured = Self::default();
        let _observation = Observation {
            depth: OBSERVERS.with(|observers| {
                let mut observers = observers.borrow_mut();
                let depth = observers.len();
                observers.push((self.clone(), measured.clone()));
                depth
            }),
            _thread: std::marker::PhantomData,
        };
        let result = operation();
        (result, measured.snapshot())
    }

    /// Attribute work until the returned guard is dropped. Scopes may nest.
    pub fn enter(&self) -> WorkScope {
        WorkScope {
            previous: CURRENT.with(|current| current.replace(Some(self.clone()))),
            _thread: std::marker::PhantomData,
        }
    }
}
fn add(select: impl Fn(&Counters) -> &AtomicU64, count: usize) {
    CURRENT.with(|current| {
        if let Some(counts) = current.borrow().as_ref() {
            select(&counts.0).fetch_add(count as u64, Ordering::Relaxed);
            OBSERVERS.with(|observers| {
                for (owner, measured) in observers.borrow().iter() {
                    if Arc::ptr_eq(&owner.0, &counts.0) {
                        select(&measured.0).fetch_add(count as u64, Ordering::Relaxed);
                    }
                }
            });
        }
    });
}
pub(crate) fn recovery_dependencies(count: usize) {
    add(|counts| &counts.dependencies, count);
}
pub(crate) fn recovery_changes(count: usize) {
    add(|counts| &counts.changes, count);
}
/// Called by the record's serializer, so counting includes every encoding
/// path rather than just successful state-file publications.
pub fn record_serialization() {
    add(|counts| &counts.serializations, 1);
}
