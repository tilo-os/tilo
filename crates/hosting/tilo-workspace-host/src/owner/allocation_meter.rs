//! Test-only thread-local requested-allocation observer (count, live, peak/net bytes).
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub allocations: usize,
    pub live: usize,
    pub unmatched_free: bool,
    /// Signed net requested bytes (frees of older allocations go negative).
    pub net: isize,
    /// Highest `net` observed during the measurement: the operation's peak.
    pub peak: isize,
}
thread_local! {
    static STATS: Cell<Option<Stats>> = const { Cell::new(None) };
}

fn observe(bytes: usize, allocation: bool) {
    let _ = STATS.try_with(|slot| {
        if let Some(mut state) = slot.get() {
            if allocation {
                state.allocations = state.allocations.saturating_add(1);
                state.live = state.live.saturating_add(bytes);
                state.net = state.net.saturating_add_unsigned(bytes);
                state.peak = state.peak.max(state.net);
            } else {
                state.net = state.net.saturating_sub_unsigned(bytes);
            }
            if !allocation {
                state.unmatched_free |= bytes > state.live;
                state.live = state.live.saturating_sub(bytes);
            }
            slot.set(Some(state));
        }
    });
}

struct Meter;
// SAFETY: all allocation/deallocation operations delegate unchanged to System.
// Observation uses only nonallocating thread-local scalars.
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
// This is tilo-workspace-host's first test-binary allocator. Dependency cfg(test)
// meters are absent; when these TLS slots are empty every call passes through.
#[global_allocator]
static ALLOCATOR: Meter = Meter;

pub(in crate::owner) fn snapshot() -> Stats {
    STATS.with(|slot| slot.get().expect("active allocation observation"))
}

pub(crate) fn allocations_only<T>(operation: impl FnOnce() -> T) -> (T, Stats) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            STATS.with(|slot| slot.set(None));
        }
    }
    STATS.with(|slot| {
        assert!(slot.get().is_none());
        slot.set(Some(Stats::default()));
    });
    let _reset = Reset;
    let result = operation();
    (result, STATS.with(|slot| slot.take().unwrap()))
}
