//! The heap bytes the whole process holds, for tests that bound what a long-running component
//! keeps while it works on many threads (a session's tasks, its transport's).
//!
//! [`Held`] forwards every allocation to [`System`] and keeps one process-wide count of the bytes
//! allocated and not freed. A test binary opts in by installing it itself, so linking this crate
//! never changes the allocator:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: tilo_test_env::held::Held = tilo_test_env::held::Held;
//! ```
//!
//! The count is the whole process's: read it in a test that has its process to itself, as each
//! test under nextest does.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

static HELD: AtomicIsize = AtomicIsize::new(0);
/// Whether [`Held`] counted anything: it is the binary's allocator.
static COUNTING: AtomicBool = AtomicBool::new(false);

fn delta(bytes: usize) -> isize {
    isize::try_from(bytes).unwrap_or(isize::MAX)
}

/// The counting allocator; install it as the test binary's `#[global_allocator]`.
pub struct Held;

// SAFETY: every call forwards to the system allocator with the caller's arguments; the counter
// only records sizes.
unsafe impl GlobalAlloc for Held {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            HELD.fetch_add(delta(layout.size()), Ordering::Relaxed);
            if !COUNTING.load(Ordering::Relaxed) {
                COUNTING.store(true, Ordering::Relaxed);
            }
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            HELD.fetch_add(delta(layout.size()), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        HELD.fetch_sub(delta(layout.size()), Ordering::Relaxed);
        // SAFETY: `pointer` was allocated by `System` with `layout`, as the caller guarantees.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        // SAFETY: as for `dealloc`, and `bytes` is the caller's valid new size.
        let new = unsafe { System.realloc(pointer, layout, bytes) };
        if !new.is_null() {
            HELD.fetch_add(
                delta(bytes).wrapping_sub(delta(layout.size())),
                Ordering::Relaxed,
            );
        }
        new
    }
}

/// The bytes the process holds allocated now.
///
/// # Panics
/// [`Held`] is not the binary's global allocator: a count that never moves would pass any bound.
#[must_use]
pub fn bytes() -> isize {
    drop(std::hint::black_box(Box::new(0_u8)));
    assert!(
        COUNTING.load(Ordering::Relaxed),
        "install tilo_test_env::held::Held as the #[global_allocator]"
    );
    HELD.load(Ordering::Relaxed)
}
