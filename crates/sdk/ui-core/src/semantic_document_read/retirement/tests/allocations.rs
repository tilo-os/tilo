//! Test-only requested-allocation meter.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub allocations: usize,
    pub live: usize,
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
