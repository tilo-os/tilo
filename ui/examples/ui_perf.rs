//! UI frame profiling and benchmark scenario runner.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

struct ProfilingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOCATION_BYTES: AtomicU64 = AtomicU64::new(0);
static REALLOCATION_CALLS: AtomicU64 = AtomicU64::new(0);
static REALLOCATION_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static GLOBAL_ALLOCATOR: ProfilingAllocator = ProfilingAllocator;

// SAFETY: every operation delegates to `System` with the exact layout/pointer
// supplied by the caller. Relaxed atomic accounting does not affect allocator
// semantics and executes only after a successful allocation.
unsafe impl GlobalAlloc for ProfilingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATION_BYTES.fetch_add(
                u64::try_from(layout.size()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            REALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
            REALLOCATION_BYTES.fetch_add(
                u64::try_from(new_size).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        new_ptr
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AllocationSnapshot {
    pub calls: u64,
    pub bytes: u64,
    pub realloc_calls: u64,
    pub realloc_bytes: u64,
}

pub(crate) fn begin_allocation_profile() {
    ALLOCATION_CALLS.store(0, Ordering::Relaxed);
    ALLOCATION_BYTES.store(0, Ordering::Relaxed);
    REALLOCATION_CALLS.store(0, Ordering::Relaxed);
    REALLOCATION_BYTES.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
}

pub(crate) fn allocation_profile_snapshot() -> AllocationSnapshot {
    AllocationSnapshot {
        calls: ALLOCATION_CALLS.load(Ordering::Relaxed),
        bytes: ALLOCATION_BYTES.load(Ordering::Relaxed),
        realloc_calls: REALLOCATION_CALLS.load(Ordering::Relaxed),
        realloc_bytes: REALLOCATION_BYTES.load(Ordering::Relaxed),
    }
}

pub(crate) fn end_allocation_profile() -> AllocationSnapshot {
    COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
    allocation_profile_snapshot()
}

#[path = "common/gallery_scene.rs"]
mod gallery_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Perf;
    dashboard_perf_bench, "dashboard_perf_bench.rs", "dashboard stress benchmark";
    perf_scenario, "perf_scenario.rs", "headless pass-level profiling";
    widget_perf_bench, "widget_perf_bench.rs", "widget construction and rendering benchmark";
}

fn main() {
    registry::dispatch(registry::Surface::Perf, SCENARIOS, Some("perf_scenario"));
}
