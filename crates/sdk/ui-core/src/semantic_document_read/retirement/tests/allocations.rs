//! The requested-allocation meter (`tilo_test_env::requested_allocations`),
//! installed for this test binary.
use std::alloc::{GlobalAlloc, Layout};
pub(crate) use tilo_test_env::requested_allocations::allocations_only;

#[global_allocator]
static ALLOCATOR: tilo_test_env::requested_allocations::Meter =
    tilo_test_env::requested_allocations::Meter;

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
