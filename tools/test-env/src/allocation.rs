//! Thread-local allocation metering for tests that bound how much a single
//! operation may allocate.
//!
//! [`Meter`] forwards every allocation to [`System`] and records sizes only
//! while [`largest`] or [`peak`] runs on the current thread. A test crate opts
//! in by installing it itself, so linking this crate never changes the
//! allocator:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: tilo_test_env::allocation::Meter = tilo_test_env::allocation::Meter;
//! ```
//!
//! Build fixtures before [`largest`]; frees of allocations made outside the
//! measurement fail it.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
#[derive(Clone, Copy, Default, Debug)]
struct Trace {
    live: usize,
    peak: usize,
    largest: usize,
    unmatched: bool,
}
thread_local! {
    static TRACE: Cell<Option<Trace>> = const { Cell::new(None) };
}
/// The metering allocator; install it as the test binary's `#[global_allocator]`.
pub struct Meter;
fn allocated(bytes: usize) {
    let _ = TRACE.try_with(|state| {
        if let Some(mut trace) = state.get() {
            trace.live = trace.live.saturating_add(bytes);
            trace.peak = trace.peak.max(trace.live);
            trace.largest = trace.largest.max(bytes);
            state.set(Some(trace));
        }
    });
}
fn released(bytes: usize) {
    let _ = TRACE.try_with(|state| {
        if let Some(mut trace) = state.get() {
            if let Some(live) = trace.live.checked_sub(bytes) {
                trace.live = live;
            } else {
                trace.unmatched = true;
            }
            state.set(Some(trace));
        }
    });
}
// SAFETY: all allocation operations forward the unchanged caller contract to System.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        released(layout.size());
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        let new = unsafe { System.realloc(pointer, layout, bytes) };
        if !new.is_null() {
            allocated(bytes);
            released(layout.size());
        }
        new
    }
}
fn measure<T>(operation: impl FnOnce() -> T) -> (T, Trace) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TRACE.with(|state| state.set(None));
        }
    }
    TRACE.with(|state| {
        assert!(state.get().is_none(), "allocation measurements do not nest");
        state.set(Some(Trace::default()));
    });
    let _reset = Reset;
    // A binary without `Meter` installed would measure nothing and pass any bound.
    drop(std::hint::black_box(Box::new(0_u8)));
    TRACE.with(|state| {
        assert!(
            state.get().is_some_and(|trace| trace.largest != 0),
            "install tilo_test_env::allocation::Meter as the #[global_allocator]"
        );
        state.set(Some(Trace::default()));
    });
    let value = operation();
    // `Reset` only clears after this read, so the measurement is still open.
    let trace = TRACE.with(|state| state.replace(None)).unwrap_or_default();
    assert!(!trace.unmatched, "fixture freed an unmeasured allocation");
    (value, trace)
}
/// Run `operation` and return its value with the largest single allocation it
/// requested on this thread. Requires [`Meter`] as the global allocator.
pub fn largest<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let (value, trace) = measure(operation);
    (value, trace.largest)
}
/// Run `operation` and return its value with the most bytes it held allocated
/// at once on this thread. Requires [`Meter`] as the global allocator.
pub fn peak<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let (value, trace) = measure(operation);
    (value, trace.peak)
}
