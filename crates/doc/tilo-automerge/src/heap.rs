//! Heap bytes the current thread holds and allocates, for tests that pin what a document keeps
//! in memory or what a hot path costs.
//!
//! Install [`ThreadCounting`] as the test binary's `#[global_allocator]` and read [`held`]
//! around the code under test. Counted per thread, so tests running in parallel on other threads
//! don't disturb the figure; memory freed on another thread than the one that allocated it is
//! miscounted, and automerge does all its work on the caller's thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The system allocator, counting the bytes each thread holds.
pub struct ThreadCounting;

std::thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

fn add(bytes: usize) {
    let delta = isize::try_from(bytes).unwrap_or(isize::MAX);
    // A thread being torn down has no counter left; nothing is measured there.
    let _ = LIVE.try_with(|l| l.set(l.get().wrapping_add(delta)));
}

fn count(bytes: usize) {
    let _ = ALLOCATED.try_with(|a| a.set(a.get().saturating_add(bytes)));
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
        count(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        count(layout.size());
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
        // Only growth is newly allocated; the live figure moves either way.
        count(new_size.saturating_sub(layout.size()));
        // SAFETY: as for `dealloc`, and the caller upholds `realloc`'s size contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Bytes allocated and not yet freed by this thread since it started (only meaningful with
/// [`ThreadCounting`] installed).
#[must_use]
pub fn held() -> isize {
    LIVE.with(Cell::get)
}

/// Bytes `build` leaves allocated on this thread in the value it returns.
///
/// The value is dropped after measuring, so the figure is what it held, not what building it
/// cost at its peak.
pub fn held_by<T>(build: impl FnOnce() -> T) -> isize {
    let before = held();
    let value = build();
    let bytes = held().wrapping_sub(before);
    drop(value);
    bytes
}

/// Bytes allocated on this thread since it started, freed or not (only meaningful with
/// [`ThreadCounting`] installed). A reallocation counts only its growth.
#[must_use]
pub fn allocated() -> usize {
    ALLOCATED.with(Cell::get)
}

/// Bytes `run` allocates on this thread, including what it frees again before returning: the
/// cost of running it, where [`held_by`] is what its result keeps.
pub fn allocated_by<T>(run: impl FnOnce() -> T) -> usize {
    let before = allocated();
    let value = run();
    let bytes = allocated().saturating_sub(before);
    drop(value);
    bytes
}
