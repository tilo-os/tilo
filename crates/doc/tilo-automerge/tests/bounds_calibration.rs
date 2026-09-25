//! Heap-probe calibration of the automerge 0.12 bounded-admission bounds
//! (`automerge::resource_limits`, lane L1).
//!
//! For adversarial document shapes this measures, with a per-thread
//! counting allocator that also tracks the peak of live bytes (a `realloc`
//! counts old + new while both exist):
//! - retained: bytes a document holds (built, then measured by moving a
//!   clone into existence and dropping it: `clone` allocates exactly what
//!   the copy retains) against `native_footprint`;
//! - clone peak against the reservation `clone_bounded` makes;
//! - usefulness: `bound / measured` for non-trivial documents.
//!
//! Run with `--nocapture` to print the ratio table.

use automerge::resource_limits::{NativeAllocationSet, NativeAllowance, NativeWorkTurn};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

// ── peak-tracking per-thread counting allocator ─────────────────────────────

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
        // Count the overlap: the new block may exist before the old is freed.
        add(new_size);
        sub(layout.size());
        // SAFETY: as for `dealloc`, and the caller upholds the size contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: PeakCounting = PeakCounting;

fn live() -> isize {
    LIVE.with(Cell::get)
}

/// Runs `f`, returning its result and the peak of live bytes above the
/// starting level while it ran.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = live();
    PEAK.with(|p| p.set(start));
    let value = f();
    let peak = PEAK.with(Cell::get) - start;
    (value, usize::try_from(peak).unwrap_or(0))
}

/// Bytes retained by `value` as produced by `f` (measured across `f`).
fn retained_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = live();
    let value = f();
    let held = live() - start;
    (value, usize::try_from(held).unwrap_or(0))
}

// ── document shapes ─────────────────────────────────────────────────────────

fn actor(i: u32) -> ActorId {
    ActorId::from(i.to_be_bytes().repeat(4))
}

fn one_actor_many_changes(n: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    for i in 0..n {
        doc.put(ROOT, "k", i as i64).unwrap();
        doc.commit();
    }
    doc
}

fn many_actors_few_changes(actors: u32, per: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(0));
    let list = doc.put_object(ROOT, "l", ObjType::List).unwrap();
    doc.commit();
    for a in 1..=actors {
        let mut fork = doc.fork().with_actor(actor(a));
        for j in 0..per {
            fork.insert(&list, 0, (a as i64) * 1000 + j as i64).unwrap();
            fork.commit();
        }
        doc.merge(&mut fork).unwrap();
    }
    doc
}

fn deep_nesting(depth: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let mut obj = ROOT;
    for i in 0..depth {
        obj = doc.put_object(&obj, format!("d{i}"), ObjType::Map).unwrap();
        if i % 16 == 0 {
            doc.commit();
        }
    }
    doc.commit();
    doc
}

fn text_with_marks(chars: usize, marks: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let text = doc.put_object(ROOT, "t", ObjType::Text).unwrap();
    let chunk: String = "abcdefghij".repeat(chars / 10 + 1)[..chars].to_string();
    for (i, c) in chunk.chars().enumerate() {
        doc.splice_text(&text, i, 0, &c.to_string()).unwrap();
        if i % 64 == 63 {
            doc.commit();
        }
    }
    doc.commit();
    for m in 0..marks {
        let start = (m * 7) % chars.max(1);
        let end = (start + 5).min(chars);
        let mark = automerge::marks::Mark::new(
            format!("m{}", m % 5),
            ScalarValue::Str(format!("value-{m}").into()),
            start,
            end,
        );
        doc.mark(&text, mark, automerge::marks::ExpandMark::After)
            .unwrap();
        if m % 16 == 15 {
            doc.commit();
        }
    }
    doc.commit();
    doc
}

fn many_map_keys(n: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let map = doc.put_object(ROOT, "m", ObjType::Map).unwrap();
    for i in 0..n {
        doc.put(&map, format!("key-{i:08}-some-longer-suffix"), i as i64)
            .unwrap();
        if i % 100 == 99 {
            doc.commit();
        }
    }
    doc.commit();
    doc
}

fn counter_increments(n: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    doc.put(ROOT, "c", ScalarValue::counter(0)).unwrap();
    doc.commit();
    for i in 0..n {
        doc.increment(ROOT, "c", 1).unwrap();
        if i % 10 == 9 {
            doc.commit();
        }
    }
    doc.commit();
    doc
}

fn large_values(n: usize, size: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let list = doc.put_object(ROOT, "v", ObjType::List).unwrap();
    for i in 0..n {
        if i % 2 == 0 {
            doc.insert(&list, i, ScalarValue::Bytes(vec![i as u8; size]))
                .unwrap();
        } else {
            doc.insert(&list, i, "s".repeat(size)).unwrap();
        }
        doc.commit();
    }
    doc
}

fn queued_orphans(n: usize) -> AutoCommit {
    // Changes whose first dependency never arrives stay queued.
    let mut source = AutoCommit::new().with_actor(actor(9));
    source.put(ROOT, "missing", 1).unwrap();
    source.commit();
    let skip = source.get_heads();
    for i in 0..n {
        source.put(ROOT, format!("q{i}"), i as i64).unwrap();
        source.commit();
    }
    let changes: Vec<_> = source
        .get_changes(&[])
        .into_iter()
        .filter(|c| !skip.contains(&c.hash()))
        .collect();
    let mut doc = AutoCommit::new().with_actor(actor(1));
    doc.put(ROOT, "x", 1).unwrap();
    doc.commit();
    doc.apply_changes(changes).unwrap();
    doc
}

fn loaded(doc: &mut AutoCommit) -> AutoCommit {
    let bytes = doc.save();
    AutoCommit::load(&bytes).unwrap()
}

// ── measurement ─────────────────────────────────────────────────────────────

/// Usefulness ceiling for `footprint / retained` on non-trivial documents.
const USEFUL_RATIO: f64 = 8.0;

struct Row {
    name: &'static str,
    retained: usize,
    footprint: usize,
    clone_peak: usize,
    clone_bound: usize,
}

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

fn measure(name: &'static str, doc: &AutoCommit) -> Row {
    let a = allowance();
    let footprint = doc.native_footprint(&a).unwrap().total_bytes();
    // Retained: a fresh clone allocates exactly what it retains.
    let (copy, retained) = retained_of(|| doc.clone());
    drop(copy);
    let (copy, clone_peak) = peak_of(|| doc.clone());
    drop(copy);
    // The clone reservation is the retained bound (clone_peak_bound).
    let mut set = NativeAllocationSet::new(allowance()).unwrap();
    let turn: NativeWorkTurn = set.work_turn(a.clone()).unwrap();
    let before = a.available_bytes();
    let candidate = doc.clone_bounded(&mut set, &turn).unwrap();
    let clone_bound = footprint;
    assert!(before > a.available_bytes());
    drop(candidate);
    Row {
        name,
        retained,
        footprint,
        clone_peak,
        clone_bound,
    }
}

fn check(rows: &[Row]) {
    println!(
        "{:<34} {:>11} {:>11} {:>7} {:>11} {:>11} {:>7}",
        "shape", "retained", "footprint", "ratio", "clone_peak", "clone_bnd", "ratio"
    );
    let mut failures = Vec::new();
    for r in rows {
        let ratio = r.footprint as f64 / r.retained.max(1) as f64;
        let cratio = r.clone_bound as f64 / r.clone_peak.max(1) as f64;
        println!(
            "{:<34} {:>11} {:>11} {:>7.2} {:>11} {:>11} {:>7.2}",
            r.name, r.retained, r.footprint, ratio, r.clone_peak, r.clone_bound, cratio
        );
        if r.retained > r.footprint {
            failures.push(format!(
                "{}: retained {} > footprint {}",
                r.name, r.retained, r.footprint
            ));
        }
        if r.clone_peak > r.clone_bound {
            failures.push(format!(
                "{}: clone peak {} > bound {}",
                r.name, r.clone_peak, r.clone_bound
            ));
        }
        if r.retained > 64 * 1024 && ratio > USEFUL_RATIO {
            failures.push(format!(
                "{}: footprint/retained {:.2} > {USEFUL_RATIO}",
                r.name, ratio
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn retained_and_clone_bounds_cover_adversarial_shapes() {
    let mut shapes: Vec<(&'static str, AutoCommit)> = vec![
        ("empty", AutoCommit::new()),
        ("1 actor x 4000 changes", one_actor_many_changes(4000)),
        ("300 actors x 3 changes", many_actors_few_changes(300, 3)),
        ("deep nesting 2000", deep_nesting(2000)),
        ("text 20k + 500 marks", text_with_marks(20_000, 500)),
        ("map 10k keys", many_map_keys(10_000)),
        ("counter 5k increments", counter_increments(5_000)),
        ("large values 40 x 64KiB", large_values(40, 64 * 1024)),
        ("queued orphans 500", queued_orphans(500)),
    ];
    let mut loaded_shapes = Vec::new();
    for (name, doc) in &mut shapes {
        let l = loaded(doc);
        let leaked: &'static str = Box::leak(format!("{name} (loaded)").into_boxed_str());
        loaded_shapes.push((leaked, l));
    }
    shapes.extend(loaded_shapes);
    let rows: Vec<Row> = shapes.iter().map(|(n, d)| measure(n, d)).collect();
    check(&rows);
}

#[test]
fn open_transaction_is_covered_and_preserved_by_clone() {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    doc.put(ROOT, "a", "x".repeat(10_000)).unwrap();
    doc.put(ROOT, "b", 1).unwrap();
    assert_eq!(doc.pending_ops(), 2);
    let a = allowance();
    let mut set = NativeAllocationSet::new(allowance()).unwrap();
    let turn = set.work_turn(a.clone()).unwrap();
    let footprint = doc.native_footprint(&a).unwrap().total_bytes();
    let (copy, retained) = retained_of(|| doc.clone());
    drop(copy);
    assert!(retained <= footprint, "{retained} > {footprint}");
    let candidate = doc.clone_bounded(&mut set, &turn).unwrap();
    assert_eq!(candidate.pending_ops(), 2);
    assert_eq!(doc.pending_ops(), 2);
    assert!(doc.get(ROOT, "a").unwrap().is_some());
}

#[test]
fn clone_refuses_before_copying_when_credit_is_short() {
    let doc = one_actor_many_changes(200);
    let small = NativeAllowance::new(4096).unwrap();
    let mut set = NativeAllocationSet::new(allowance()).unwrap();
    let turn = set.work_turn(small.clone()).unwrap();
    let before = small.available_bytes();
    let start = live();
    PEAK.with(|p| p.set(start));
    let result = doc.clone_bounded(&mut set, &turn);
    assert!(result.is_err());
    let peak = PEAK.with(Cell::get) - start;
    assert_eq!(small.available_bytes(), before);
    // No document copy was attempted (only the error value may exist).
    assert!(peak < 1024, "peak {peak}");
}

#[test]
fn clone_credit_returns_on_drop() {
    let doc = many_map_keys(500);
    let a = allowance();
    let initial = a.available_bytes();
    let mut set = NativeAllocationSet::new(allowance()).unwrap();
    let turn = set.work_turn(a.clone()).unwrap();
    let after_turn = a.available_bytes();
    let c1 = doc.clone_bounded(&mut set, &turn).unwrap();
    let c2 = c1.clone_bounded(&mut set, &turn).unwrap();
    assert!(a.available_bytes() < after_turn);
    drop(c1);
    drop(c2);
    set.reconcile();
    assert_eq!(a.available_bytes(), after_turn);
    drop(turn);
    assert_eq!(a.available_bytes(), initial);
}
