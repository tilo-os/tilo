//! Heap bytes the current thread holds, for tests that compare what documents keep in memory.
//!
//! Counted per thread, so tests running in parallel on other threads don't disturb the figure.
//! Memory freed on another thread than the one that allocated it is miscounted; automerge and
//! qdoc do all their work on the caller's thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

pub struct ThreadCounting;

std::thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn add(bytes: usize) {
    let delta = isize::try_from(bytes).unwrap_or(isize::MAX);
    // A thread being torn down has no counter left; nothing is measured there.
    let _ = LIVE.try_with(|l| l.set(l.get().wrapping_add(delta)));
}

fn sub(bytes: usize) {
    let delta = isize::try_from(bytes).unwrap_or(isize::MAX);
    let _ = LIVE.try_with(|l| l.set(l.get().wrapping_sub(delta)));
}

// SAFETY: every call forwards to the system allocator with the caller's arguments; the counter
// only records sizes.
unsafe impl GlobalAlloc for ThreadCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        sub(layout.size());
        // SAFETY: `ptr` came from this allocator, i.e. from `System`, with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        sub(layout.size());
        add(new_size);
        // SAFETY: as for `dealloc`, and the caller upholds `realloc`'s size contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Bytes allocated and not yet freed by this thread since it started.
pub fn live() -> isize {
    LIVE.try_with(Cell::get).unwrap_or(0)
}
