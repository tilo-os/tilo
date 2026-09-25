use automerge::resource_limits::NativeAllowance;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
#[derive(Clone, Copy, Default, Debug)]
struct Trace {
    live: usize,
    peak: usize,
    largest: usize,
    unreserved: usize,
    unmatched: bool,
}
thread_local! {
    static TRACE: Cell<Option<Trace>> = const { Cell::new(None) };
    static CREDIT: RefCell<Option<(NativeAllowance, usize)>> = const { RefCell::new(None) };
}
struct Meter;
fn allocated(bytes: usize) {
    let _ = TRACE.try_with(|state| {
        if let Some(mut trace) = state.get() {
            trace.live += bytes;
            trace.peak = trace.peak.max(trace.live);
            trace.largest = trace.largest.max(bytes);
            let _ = CREDIT.try_with(|credit| {
                if let Some((allowance, initial)) = &*credit.borrow() {
                    trace.unreserved = trace.unreserved.max(
                        trace
                            .live
                            .saturating_sub(initial.saturating_sub(allowance.available_bytes())),
                    );
                }
            });
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
#[global_allocator]
static ALLOCATOR: Meter = Meter;
fn measure<T>(operation: impl FnOnce() -> T) -> (T, Trace) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TRACE.with(|state| state.set(None));
            CREDIT.with(|credit| *credit.borrow_mut() = None);
        }
    }
    TRACE.with(|state| {
        assert!(state.get().is_none());
        state.set(Some(Trace::default()));
    });
    let _reset = Reset;
    let value = operation();
    let trace = TRACE.with(|state| state.replace(None).unwrap());
    assert!(!trace.unmatched, "fixture freed an unmeasured allocation");
    (value, trace)
}
pub fn largest<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let (value, trace) = measure(operation);
    (value, trace.largest)
}
pub fn precharged<T>(
    allowance: &NativeAllowance,
    operation: impl FnOnce() -> T,
) -> (T, usize, usize) {
    let before = allowance.available_bytes();
    CREDIT.with(|credit| {
        assert!(credit.borrow().is_none());
        *credit.borrow_mut() = Some((allowance.clone(), before));
    });
    let (value, trace) = measure(operation);
    assert_eq!(
        trace.unreserved, 0,
        "allocation preceded current-turn credit: {trace:?}"
    );
    assert!(
        trace.live <= before - allowance.available_bytes(),
        "output backing lost credit: {trace:?}"
    );
    (value, trace.peak, trace.largest)
}
