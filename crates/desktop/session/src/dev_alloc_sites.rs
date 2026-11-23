//! The process's heap by allocation site, for the compositor's memory report.
//!
//! A session built with `dev-overrides` (the dev image's) records the call stack of every
//! allocation of at least [`THRESHOLD`] while `TILO_ALLOC_SITES` is set, and answers
//! `tilo-control raw '{"op":"process_memory"}'` with the live bytes of each site, named from the
//! executable's symbol table (`tilo_liveness::symtab`). The vm-acceptance memory checks then say
//! what holds an over-budget compositor's heap, not only how much it holds.
//!
//! Off unless asked: with no `TILO_ALLOC_SITES` the allocator is the system's, one atomic load
//! per allocation, and nothing is recorded. The probe's own bookkeeping is never recorded (a
//! thread-local guard spans it), though what it holds is in the heap the report also prints.
//!
//! The stack of each allocation is walked with `backtrace(3)`; names are resolved once per
//! report, from the binary's symbol table, and a frame in another object is named by that object
//! and its offset (`addr2line` resolves those from the report alone).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

/// Allocations below this are not recorded: the heap is held by layers, buffers and documents,
/// and a session has orders of magnitude more small allocations than it can afford to walk.
pub const THRESHOLD: usize = 8 << 10;
/// Frames kept of an allocation's stack: the allocator's own frames take the first few, and the
/// caller names the site.
const DEPTH: usize = 14;
/// Sites a report tells, largest live first.
const REPORTED: usize = 12;
/// The smallest live total a site is reported at.
const REPORT_MIN_BYTES: u64 = 256 << 10;
/// Call sites recorded before recording stops: a session's distinct stacks, with headroom.
const SITES_LIMIT: usize = 16 << 10;

/// Whether a session asked to record ([`install`]).
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The smallest recorded allocation, raised by tests and by `TILO_ALLOC_SITES_BYTES`.
static THRESHOLD_BYTES: AtomicUsize = AtomicUsize::new(THRESHOLD);

thread_local! {
    /// Set while the probe itself allocates or walks the stack, so its own bookkeeping is never
    /// recorded.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// One call site and what it holds.
struct Site {
    /// The frames after the allocator's own, innermost first; a zero frame ends them.
    frames: [usize; DEPTH],
    frames_len: usize,
    live_bytes: u64,
    live_count: u64,
}

/// An allocation still live: the site it came from and its size.
type Live = HashMap<usize, (usize, usize)>;

#[derive(Default)]
struct Table {
    sites: Vec<Site>,
    /// A stack's frames → its site.
    by_frames: HashMap<[usize; DEPTH], usize>,
    live: Live,
    /// Whether a stack came past [`SITES_LIMIT`], so its allocations were not recorded.
    limited: bool,
}

fn table() -> MutexGuard<'static, Option<Box<Table>>> {
    static TABLE: Mutex<Option<Box<Table>>> = Mutex::new(None);
    TABLE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn inside() -> bool {
    INSIDE.with(Cell::get)
}

fn set_inside(inside: bool) {
    INSIDE.with(|cell| cell.set(inside));
}

unsafe extern "C" {
    /// glibc's `backtrace(3)`: the calling frames into `buffer`, its length.
    fn backtrace(buffer: *mut *mut libc::c_void, size: libc::c_int) -> libc::c_int;
}

fn capture(frames: &mut [usize; DEPTH]) -> usize {
    // SAFETY: writes at most `DEPTH` frame addresses into the array.
    let len = unsafe {
        backtrace(
            frames.as_mut_ptr().cast::<*mut libc::c_void>(),
            i32::try_from(DEPTH).unwrap_or(i32::MAX),
        )
    };
    usize::try_from(len).unwrap_or(0).min(DEPTH)
}

fn record(size: usize, ptr: *mut u8) {
    if !ENABLED.load(Ordering::Relaxed)
        || size < THRESHOLD_BYTES.load(Ordering::Relaxed)
        || inside()
    {
        return;
    }
    set_inside(true);
    let mut frames = [0usize; DEPTH];
    let frames_len = capture(&mut frames);
    // `backtrace` and `record` are the probe's own frames.
    let skip = frames_len.min(2);
    let mut site_frames = [0usize; DEPTH];
    site_frames[..frames_len - skip].copy_from_slice(&frames[skip..frames_len]);
    let mut held = table();
    let table = held.get_or_insert_with(|| {
        Box::new(Table {
            sites: Vec::with_capacity(256),
            by_frames: HashMap::with_capacity(256),
            live: Live::new(),
            limited: false,
        })
    });
    let site = if let Some(&site) = table.by_frames.get(&site_frames) {
        site
    } else if table.sites.len() >= SITES_LIMIT {
        table.limited = true;
        drop(held);
        set_inside(false);
        return;
    } else {
        let site = table.sites.len();
        table.sites.push(Site {
            frames: site_frames,
            frames_len: frames_len.saturating_sub(skip),
            live_bytes: 0,
            live_count: 0,
        });
        table.by_frames.insert(site_frames, site);
        site
    };
    if let Some(entry) = table.sites.get_mut(site) {
        entry.live_bytes = entry.live_bytes.saturating_add(size as u64);
        entry.live_count = entry.live_count.saturating_add(1);
    }
    table.live.insert(ptr as usize, (site, size));
    drop(held);
    set_inside(false);
}

fn forget(ptr: *mut u8) {
    if !ENABLED.load(Ordering::Relaxed) || inside() {
        return;
    }
    set_inside(true);
    let mut held = table();
    if let Some(table) = held.as_deref_mut()
        && let Some((site, size)) = table.live.remove(&(ptr as usize))
        && let Some(entry) = table.sites.get_mut(site)
    {
        entry.live_bytes = entry.live_bytes.saturating_sub(size as u64);
        entry.live_count = entry.live_count.saturating_sub(1);
    }
    drop(held);
    set_inside(false);
}

struct Probe;

// SAFETY: every operation delegates to `System` with the caller's own layout and pointers; the
// accounting only reads and writes its own tables behind a mutex.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record(layout.size(), ptr);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        forget(ptr);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            forget(ptr);
            record(new_size, new);
        }
        new
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record(layout.size(), ptr);
        }
        ptr
    }
}

/// The session's allocator: the probe's while it records, the system's otherwise.
pub struct Allocator;

// SAFETY: as `Probe`; with recording off it is `System` plus one atomic load.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { Probe.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { Probe.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { Probe.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { Probe.alloc_zeroed(layout) }
    }
}

/// Start recording, when `TILO_ALLOC_SITES` asks, and answer the compositor's `process_memory`
/// with what each site holds.
pub fn install() {
    let Some(value) = std::env::var_os("TILO_ALLOC_SITES") else {
        return;
    };
    if value.is_empty() || value == "0" {
        return;
    }
    if let Ok(asked) = std::env::var("TILO_ALLOC_SITES_BYTES") {
        match asked.parse() {
            Ok(bytes) => THRESHOLD_BYTES.store(bytes, Ordering::Relaxed),
            Err(error) => tracing::warn!(
                %error,
                asked,
                "TILO_ALLOC_SITES_BYTES is not a byte count; recording from 8 KiB"
            ),
        }
    }
    ENABLED.store(true, Ordering::Relaxed);
    tilo_compositor::memory_report::provide_alloc_sites(report);
}

/// Turn recording on from a test or a harness, without the environment's say-so.
#[cfg(test)]
fn enable(threshold: usize) {
    THRESHOLD_BYTES.store(threshold, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
}

/// The largest live allocations by call site: `{"recording": true, "threshold_kib": 8,
/// "live_kib_total": 476, "sites": [{"live_kib": 54, "live_count": 15, "stack": ["…"]}]}`,
/// innermost frame first. Empty while nothing is recorded.
#[must_use]
pub fn report() -> Value {
    let mut rows: Vec<([usize; DEPTH], usize, u64, u64)> = Vec::new();
    let mut total = 0u64;
    let mut limited = false;
    {
        let held = table();
        if let Some(table) = held.as_deref() {
            limited = table.limited;
            for site in &table.sites {
                if site.live_bytes == 0 {
                    continue;
                }
                total = total.saturating_add(site.live_bytes);
                if site.live_bytes >= REPORT_MIN_BYTES {
                    rows.push((
                        site.frames,
                        site.frames_len,
                        site.live_bytes,
                        site.live_count,
                    ));
                }
            }
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.2));
    rows.truncate(REPORTED);
    let stacks: Vec<&[usize]> = rows
        .iter()
        .map(|(frames, len, _, _)| frames.get(..*len).unwrap_or_default())
        .collect();
    // Nothing over the floor: no symbol table to read.
    let names = if stacks.is_empty() {
        Vec::new()
    } else {
        tilo_liveness::symtab::names_of(&stacks)
    };
    let sites: Vec<Value> = rows
        .iter()
        .zip(names)
        .map(|((_, _, bytes, count), mut frames)| {
            // The allocator's own frames (`GlobalAlloc` implementations and the allocation
            // shims) are the same for every site: the caller is what names it.
            while frames.first().is_some_and(|frame| {
                frame.contains("dev_alloc_sites") || frame.contains("__rust_alloc")
            }) {
                frames.remove(0);
            }
            frames.truncate(10);
            while frames
                .last()
                .is_some_and(|frame| frame.starts_with("<unknown>"))
            {
                frames.pop();
            }
            json!({
                "live_kib": bytes / 1024,
                "live_count": count,
                "stack": frames,
            })
        })
        .collect();
    json!({
        "recording": ENABLED.load(Ordering::Relaxed),
        "threshold_kib": THRESHOLD_BYTES.load(Ordering::Relaxed) / 1024,
        "live_kib_total": total / 1024,
        "limited": limited,
        "sites": sites,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An allocation the report can name: a block this function holds.
    #[inline(never)]
    fn hold_a_known_block() -> Vec<u8> {
        vec![1u8; 512 << 10]
    }

    #[test]
    fn the_report_names_a_site_that_holds_memory() {
        enable(THRESHOLD);
        let held = hold_a_known_block();
        assert!(std::hint::black_box(&held).iter().all(|byte| *byte == 1));
        let report = report();
        let sites = report["sites"].as_array().expect("sites");
        let named = sites.iter().any(|site| {
            site["stack"].as_array().is_some_and(|stack| {
                stack.iter().any(|frame| {
                    frame
                        .as_str()
                        .is_some_and(|name| name.contains("hold_a_known_block"))
                })
            })
        });
        assert!(
            named,
            "a 512 KiB block names the function holding it: {report}"
        );
        drop(held);
    }
}
