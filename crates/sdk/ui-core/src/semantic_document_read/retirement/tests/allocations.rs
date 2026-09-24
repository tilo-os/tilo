//! Test-only requested-allocation meter. Checks credit before allocation AND free.
use automerge::resource_limits::NativeAllowance;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::{Cell, RefCell},
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub allocations: usize,
    pub live: usize,
    pub underfunded: bool,
}
thread_local! {
    static STATS: Cell<Option<Stats>> = const { Cell::new(None) };
    static CREDIT: RefCell<Option<(NativeAllowance, usize)>> = const { RefCell::new(None) };
}
struct Meter;
fn observe(bytes: usize, allocation: bool) {
    let _ = STATS.try_with(|slot| {
        if let Some(mut state) = slot.get() {
            if allocation {
                state.allocations += 1;
                state.live = state.live.saturating_add(bytes);
            }
            let _ = CREDIT.try_with(|credit| {
                if let Some((credit, baseline)) = credit.borrow().as_ref() {
                    state.underfunded |=
                        state.live > credit.retained_bytes().saturating_sub(*baseline);
                }
            });
            if !allocation {
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

pub(super) fn allocations_only<T>(operation: impl FnOnce() -> T) -> (T, Stats) {
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
pub(crate) fn measure<T>(credit: &NativeAllowance, operation: impl FnOnce() -> T) -> (T, Stats) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CREDIT.with(|v| *v.borrow_mut() = None);
        }
    }
    CREDIT.with(|v| {
        assert!(v.borrow().is_none());
        *v.borrow_mut() = Some((credit.clone(), credit.retained_bytes()));
    });
    let _reset = Reset;
    allocations_only(operation)
}
