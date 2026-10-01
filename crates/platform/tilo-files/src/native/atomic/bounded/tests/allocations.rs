//! Rust-requested heap observation only. Native syscall stubs are audited
//! separately; this observer does not claim to intercept C malloc or kernel use.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
#[derive(Clone, Copy)]
struct Usage {
    enabled: bool,
    live: usize,
    peak: usize,
}
std::thread_local! { static USAGE:Cell<Usage>=const { Cell::new(Usage {enabled:false,live:0,peak:0}) }; }
struct Meter;
#[global_allocator]
static ALLOCATOR: Meter = Meter;
fn allocated(n: usize) {
    let _ = USAGE.try_with(|c| {
        let mut s = c.get();
        if s.enabled {
            s.live = s.live.checked_add(n).unwrap();
            s.peak = s.peak.max(s.live);
            c.set(s);
        }
    });
}
fn released(n: usize) {
    let _ = USAGE.try_with(|c| {
        let mut s = c.get();
        if s.enabled {
            s.live = s.live.saturating_sub(n);
            c.set(s);
        }
    });
}
// SAFETY: forwards exact System allocation/deallocation contracts; bookkeeping
// uses fixed thread-local Cells and never changes pointers or allocation sizes.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            allocated(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            allocated(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        released(l.size());
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let next = unsafe { System.realloc(p, l, n) };
        if !next.is_null() {
            allocated(n);
            released(l.size());
        }
        next
    }
}
pub(super) fn measure<T>(maximum: usize, f: impl FnOnce() -> T) -> T {
    struct Restore(Usage);
    impl Drop for Restore {
        fn drop(&mut self) {
            USAGE.with(|c| c.set(self.0));
        }
    }
    let restore = Restore(USAGE.with(|c| {
        c.replace(Usage {
            enabled: true,
            live: 0,
            peak: 0,
        })
    }));
    let value = f();
    let peak = USAGE.with(|c| c.get().peak);
    drop(restore);
    assert!(
        peak <= maximum,
        "requested heap peak{peak} exceeds derived{maximum}"
    );
    value
}
