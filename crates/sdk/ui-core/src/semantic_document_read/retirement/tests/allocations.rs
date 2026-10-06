//! Test-only requested-allocation meter.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub allocations: usize,
    pub live: usize,
    /// Successful allocation/reallocation request bytes, including temporaries.
    pub requested: usize,
}
thread_local! {
    static STATS: Cell<Option<Stats>> = const { Cell::new(None) };
}
struct Meter;
fn observe(bytes: usize, allocation: bool) {
    let _ = STATS.try_with(|slot| {
        if let Some(mut state) = slot.get() {
            if allocation {
                state.allocations += 1;
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
#[global_allocator]
static ALLOCATOR: Meter = Meter;

pub(crate) fn allocations_only<T>(operation: impl FnOnce() -> T) -> (T, Stats) {
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

#[test]
fn requested_bytes_include_successful_allocation_freed_in_measurement() {
    let layout = Layout::from_size_align(64, 8).unwrap();
    let ((), stats) = allocations_only(|| {
        // SAFETY: nonzero valid layout, null checked before use, same Meter and layout on release.
        let pointer = unsafe { ALLOCATOR.alloc(layout) };
        assert!(!pointer.is_null());
        unsafe {
            pointer.write(7);
            ALLOCATOR.dealloc(pointer, layout);
        }
    });
    assert_eq!(stats.allocations, 1);
    assert_eq!(stats.requested, 64);
    assert_eq!(stats.live, 0);
}

#[test]
fn requested_bytes_include_zeroed_allocation_while_net_live_is_zero() {
    let layout = Layout::from_size_align(24, 8).unwrap();
    let ((), stats) = allocations_only(|| {
        // SAFETY: valid layout, successful zeroed allocation read within bounds and released once.
        let pointer = unsafe { ALLOCATOR.alloc_zeroed(layout) };
        assert!(!pointer.is_null());
        assert_eq!(unsafe { pointer.read() }, 0);
        unsafe {
            ALLOCATOR.dealloc(pointer, layout);
        }
    });
    assert_eq!(stats.allocations, 1);
    assert_eq!(stats.requested, 24);
    assert_eq!(stats.live, 0);
}

#[test]
fn requested_bytes_charge_full_successful_realloc_sizes_growing_and_shrinking() {
    let first = Layout::from_size_align(64, 8).unwrap();
    let grown = Layout::from_size_align(128, 8).unwrap();
    let shrunk = Layout::from_size_align(32, 8).unwrap();
    let (pointer, stats) = allocations_only(|| {
        // SAFETY: each successful realloc receives the immediately preceding valid allocation
        // and layout, all sizes are nonzero, alignment stays eight, retained byte stays in bounds.
        let pointer = unsafe { ALLOCATOR.alloc(first) };
        assert!(!pointer.is_null());
        unsafe {
            pointer.write(19);
        }
        let pointer = unsafe { ALLOCATOR.realloc(pointer, first, grown.size()) };
        assert!(!pointer.is_null());
        assert_eq!(unsafe { pointer.read() }, 19);
        let pointer = unsafe { ALLOCATOR.realloc(pointer, grown, shrunk.size()) };
        assert!(!pointer.is_null());
        assert_eq!(unsafe { pointer.read() }, 19);
        pointer
    });
    // Release outside the measured window, before assertions: Stats is the real retained result.
    unsafe {
        ALLOCATOR.dealloc(pointer, shrunk);
    }
    assert_eq!(stats.allocations, 3);
    assert_eq!(stats.requested, 64 + 128 + 32);
    assert_eq!(stats.live, 32);
}
