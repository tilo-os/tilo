//! Requested-allocation metering for tests that bound what one operation
//! allocates: how many allocations it made, the bytes it asked for
//! (temporaries included) and what it left live.
//!
//! A test crate opts in by installing [`Meter`] itself, so linking this crate
//! never changes the allocator:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: tilo_test_env::requested_allocations::Meter =
//!     tilo_test_env::requested_allocations::Meter;
//! ```
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test support: failing the calling test is the point"
)]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

/// What a measured operation allocated.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Successful allocations and reallocations.
    pub allocations: usize,
    /// Bytes allocated and not yet freed.
    pub live: usize,
    /// Successful allocation/reallocation request bytes, including temporaries.
    pub requested: usize,
}
thread_local! {
    static STATS: Cell<Option<Stats>> = const { Cell::new(None) };
}
/// The metering allocator; install it as the test binary's `#[global_allocator]`.
pub struct Meter;
fn observe(bytes: usize, allocation: bool) {
    let _ = STATS.try_with(|slot| {
        if let Some(mut state) = slot.get() {
            if allocation {
                state.allocations = state.allocations.saturating_add(1);
                state.requested = state.requested.saturating_add(bytes);
                state.live = state.live.saturating_add(bytes);
            } else {
                state.live = state.live.saturating_sub(bytes);
            }
            slot.set(Some(state));
        }
    });
}
// SAFETY: identical System operations; instrumentation uses nonallocating TLS scalars.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            observe(layout.size(), true);
        }
        result
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() {
            observe(layout.size(), true);
        }
        result
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        observe(layout.size(), false);
        unsafe { System.dealloc(ptr, layout) };
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(ptr, layout, size) };
        if !result.is_null() {
            observe(size, true);
            observe(layout.size(), false);
        }
        result
    }
}
/// Run `operation`, metering what it allocates on this thread.
///
/// # Panics
/// When a measurement is already running on this thread.
pub fn allocations_only<T>(operation: impl FnOnce() -> T) -> (T, Stats) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            STATS.with(|v| v.set(None));
        }
    }
    STATS.with(|v| {
        assert!(v.get().is_none());
        v.set(Some(Stats::default()));
    });
    let _reset = Reset;
    let output = operation();
    (output, STATS.with(|v| v.take().unwrap()))
}
