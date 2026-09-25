//! Heap-probe calibration of `apply_incoming_checked` for ordinary typing
//! history: a document snapshot of many small text changes applied into a
//! fresh document. The minimal allowance that admits the snapshot must stay
//! within a small multiple of the measured peak of the whole call, and a
//! large (200k character) typed history must fit one 64 MiB work turn.
//!
//! Run with `--nocapture` to print the ratio table.
#![allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "heap-probe calibration harness: counting allocator and ratio arithmetic"
)]

use automerge::resource_limits::{
    NativeAllowance, NativeDependencyPolicy, NativeIncomingOutcome, NativeLimits,
};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct PeakCounting;

std::thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn add(bytes: usize) {
    let delta = isize::try_from(bytes).unwrap_or(isize::MAX);
    let _ = LIVE.try_with(|l| {
        let now = l.get().wrapping_add(delta);
        l.set(now);
        let _ = PEAK.try_with(|p| {
            if now > p.get() {
                p.set(now);
            }
        });
    });
}

fn sub(bytes: usize) {
    let delta = isize::try_from(bytes).unwrap_or(isize::MAX);
    let _ = LIVE.try_with(|l| l.set(l.get().wrapping_sub(delta)));
}

// SAFETY: every call forwards to the system allocator with the caller's
// arguments; the counters only record sizes.
unsafe impl GlobalAlloc for PeakCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        sub(layout.size());
        // SAFETY: `ptr` came from `System` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size);
        sub(layout.size());
        // SAFETY: as for `dealloc`, and the caller upholds the size contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: PeakCounting = PeakCounting;

fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|p| p.set(base));
    let value = f();
    let peak = PEAK.with(Cell::get) - base;
    (value, usize::try_from(peak).unwrap_or(0))
}

fn limits() -> NativeLimits {
    NativeLimits {
        max_normalized_bytes: 48 * 1024 * 1024,
        max_operations: 4 * 1024 * 1024,
        max_operation_counter: u64::from(u32::MAX),
        max_changes: 1024 * 1024,
        max_edges: 16 * 1024 * 1024,
        max_actors: 65_536,
        max_heads: 65_536,
        max_columns: 128,
        max_chunks: 65_536,
        max_value_bytes: 48 * 1024 * 1024,
    }
}

/// `splices` splices of `word`, committing every `per_commit` splices.
fn typed(splices: usize, word: &str, per_commit: usize) -> (AutoCommit, automerge::ObjId) {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    for index in 0..splices {
        let at = doc.length(&text);
        doc.splice_text(&text, at, 0, word).unwrap();
        if index % per_commit == per_commit - 1 {
            doc.commit();
        }
    }
    doc.commit();
    (doc, text)
}

fn apply(input: &[u8], bytes: usize) -> Result<NativeIncomingOutcome, String> {
    let allowance = NativeAllowance::new(bytes).map_err(|e| e.to_string())?;
    let mut fresh = AutoCommit::new();
    fresh
        .apply_incoming_checked(
            input,
            &limits(),
            &allowance,
            NativeDependencyPolicy::RequireComplete,
        )
        .map_err(|e| e.to_string())
}

/// Smallest allowance (to 1%) that admits `input` into a fresh document.
fn minimal_allowance(input: &[u8]) -> usize {
    let (mut low, mut high) = (64usize, 64 * 1024 * 1024);
    assert!(apply(input, high).is_ok(), "refused at 64 MiB");
    while high - low > high / 100 {
        let mid = low + (high - low) / 2;
        if apply(input, mid).is_ok() {
            high = mid;
        } else {
            low = mid;
        }
    }
    high
}

struct Case {
    name: &'static str,
    splices: usize,
    word: &'static str,
    per_commit: usize,
}

fn measure(case: &Case) -> (usize, usize, usize) {
    let (mut doc, text) = typed(case.splices, case.word, case.per_commit);
    let snapshot = doc.save();
    let expected = doc.text(&text).unwrap();
    let mut fresh = AutoCommit::new();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (outcome, peak) = peak_of(|| {
        fresh.apply_incoming_checked(
            &snapshot,
            &limits(),
            &allowance,
            NativeDependencyPolicy::RequireComplete,
        )
    });
    assert_eq!(
        outcome.unwrap(),
        NativeIncomingOutcome::Applied { changed: true },
        "{}",
        case.name
    );
    assert_eq!(fresh.text(&text).unwrap(), expected, "{}", case.name);
    let needed = minimal_allowance(&snapshot);
    (snapshot.len(), peak, needed)
}

#[test]
fn typed_history_allowance_tracks_measured_peak() {
    let cases = [
        Case { name: "1k splices x12, commit/3", splices: 1_000, word: "typed words ", per_commit: 3 },
        Case { name: "4k splices x12, commit/3", splices: 4_000, word: "typed words ", per_commit: 3 },
        Case { name: "10k keystrokes, commit/1", splices: 10_000, word: "a", per_commit: 1 },
        Case { name: "4k splices x12, 2 changes", splices: 4_000, word: "typed words ", per_commit: 2_000 },
    ];
    eprintln!("{:<28} {:>9} {:>12} {:>12} {:>6}", "shape", "snapshot", "peak", "allowance", "ratio");
    for case in &cases {
        let (len, peak, needed) = measure(case);
        let ratio = needed as f64 / peak.max(1) as f64;
        eprintln!("{:<28} {:>9} {:>12} {:>12} {:>6.2}", case.name, len, peak, needed, ratio);
        assert!(ratio <= 5.0, "{}: allowance {needed} vs peak {peak}", case.name);
    }
}

#[test]
fn large_typed_history_fits_one_64_mib_turn_with_margin() {
    // About 100k characters of ordinary typing (one change per ~36 chars).
    // (200k characters cannot fit any sound 64 MiB bound: upstream's own
    // verified load of that snapshot peaks at about 34 MB.)
    let (mut doc, text) = typed(8_400, "typed words ", 3);
    let snapshot = doc.save();
    let needed = minimal_allowance(&snapshot);
    eprintln!("100k typed chars: snapshot {} B, minimal allowance {needed} B", snapshot.len());
    assert!(needed <= 56 * 1024 * 1024, "needs {needed} bytes");
    let mut fresh = AutoCommit::new();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    fresh
        .apply_incoming_checked(&snapshot, &limits(), &allowance, NativeDependencyPolicy::RequireComplete)
        .unwrap();
    assert_eq!(fresh.text(&text).unwrap(), doc.text(&text).unwrap());
    assert_eq!(allowance.retained_bytes(), NativeAllowance::new(64 * 1024 * 1024).unwrap().retained_bytes());
}



/// A snapshot merged into an existing replica that already holds the first
/// half of the history (the `merge_committed` shape): not blank, so the
/// changes path runs.
#[test]
fn snapshot_into_existing_replica_tracks_measured_peak() {
    let (mut doc, text) = typed(4_000, "typed words ", 3);
    let snapshot = doc.save();
    let all = doc.get_changes(&[]);
    let half: Vec<_> = all[..all.len() / 2].to_vec();
    let base = {
        let mut base = AutoCommit::new();
        base.apply_changes(half).unwrap();
        AutoCommit::load(&base.save()).unwrap()
    };
    let apply = |bytes: usize| {
        let allowance = NativeAllowance::new(bytes).unwrap();
        let mut target = base.clone();
        target
            .apply_incoming_checked(&snapshot, &limits(), &allowance, NativeDependencyPolicy::RequireComplete)
            .map(|outcome| (outcome, target))
    };
    let mut target = base.clone();
    let allowance = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let (outcome, peak) = peak_of(|| {
        target.apply_incoming_checked(&snapshot, &limits(), &allowance, NativeDependencyPolicy::RequireComplete)
    });
    assert_eq!(outcome.unwrap(), NativeIncomingOutcome::Applied { changed: true });
    assert_eq!(target.text(&text).unwrap(), doc.text(&text).unwrap());
    let (mut low, mut high) = (64usize, 64 * 1024 * 1024);
    while high - low > high / 100 {
        let mid = low + (high - low) / 2;
        if apply(mid).is_ok() { high = mid } else { low = mid }
    }
    eprintln!("merge 4k splices into half replica: peak {peak} allowance {high} ratio {:.2}", high as f64 / peak as f64);
    assert!(high as f64 <= 4.0 * peak as f64, "needs {high} for a peak of {peak}");
    assert!(high <= 48 * 1024 * 1024, "needs {high}");
}
