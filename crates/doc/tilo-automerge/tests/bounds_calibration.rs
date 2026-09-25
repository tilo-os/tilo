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
//! Run with `--nocapture` to print the ratio tables.
#![allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "heap-probe calibration harness: counting allocator and ratio arithmetic"
)]

use automerge::resource_limits::NativeLimits;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance, NativeWorkTurn};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, Change, ChangeHash, ObjType, ROOT, ReadDoc, ScalarValue};
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

/// Usefulness ceiling for `bound / measured` on non-trivial documents.
const USEFUL_RATIO: f64 = 8.0;
/// Below this many measured bytes a ratio is not asserted (fixed terms
/// such as the DEFLATE state or per-column bases dominate).
const USEFUL_FLOOR: usize = 256 * 1024;

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

/// Bounds from `AutoCommit::native_bound_probe`.
struct NativeBoundProbe {
    clone_peak: usize,
    apply_peak: usize,
    save_output: usize,
    save_peak: usize,
    history_peak: usize,
    incoming_changes: usize,
}

fn probe(doc: &AutoCommit, incoming: &[Change]) -> NativeBoundProbe {
    let [
        _retained,
        clone_peak,
        apply_peak,
        _superlinear,
        save_output,
        save_peak,
        history_peak,
        incoming_changes,
    ] = doc.native_bound_probe(incoming).unwrap();
    NativeBoundProbe {
        clone_peak,
        apply_peak,
        save_output,
        save_peak,
        history_peak,
        incoming_changes,
    }
}

fn fragments(changes: &[Change]) -> usize {
    changes.iter().filter(|c| c.hash().as_ref()[0] == 0).count()
}

/// One measured quantity against its bound.
struct Check {
    shape: String,
    what: &'static str,
    measured: usize,
    bound: usize,
    /// Whether the usefulness ratio is asserted for this quantity.
    useful: bool,
}

#[derive(Default)]
struct Table(Vec<Check>);
impl Table {
    fn add(
        &mut self,
        shape: &str,
        what: &'static str,
        measured: usize,
        bound: usize,
        useful: bool,
    ) {
        self.0.push(Check {
            shape: shape.to_string(),
            what,
            measured,
            bound,
            useful,
        });
    }
    fn finish(&self) {
        println!(
            "{:<36} {:<16} {:>12} {:>12} {:>8}",
            "shape", "quantity", "measured", "bound", "ratio"
        );
        let mut failures = Vec::new();
        for c in &self.0 {
            let ratio = c.bound as f64 / c.measured.max(1) as f64;
            println!(
                "{:<36} {:<16} {:>12} {:>12} {:>8.2}",
                c.shape, c.what, c.measured, c.bound, ratio
            );
            if c.measured > c.bound {
                failures.push(format!(
                    "{} {}: measured {} > bound {}",
                    c.shape, c.what, c.measured, c.bound
                ));
            }
            if c.useful && c.measured > USEFUL_FLOOR && ratio > USEFUL_RATIO {
                failures.push(format!(
                    "{} {}: bound/measured {:.2} > {USEFUL_RATIO}",
                    c.shape, c.what, ratio
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}

/// Retained, clone, save, history and load measurements for one document.
fn measure_doc(t: &mut Table, name: &str, doc: &AutoCommit) {
    let a = allowance();
    let footprint = doc.native_footprint(&a).unwrap().total_bytes();
    let (copy, retained) = retained_of(|| doc.clone());
    drop(copy);
    t.add(name, "retained", retained, footprint, true);
    let (copy, clone_peak) = peak_of(|| doc.clone());
    drop(copy);
    let p = probe(doc, &[]);
    t.add(name, "clone peak", clone_peak, p.clone_peak, true);

    // clone_bounded reserves before copying and keeps the retained bound.
    let mut set = NativeAllocationSet::new(allowance()).unwrap();
    let turn: NativeWorkTurn = set.work_turn(a.clone()).unwrap();
    let before = a.available_bytes();
    let candidate = doc.clone_bounded(&mut set, &turn).unwrap();
    assert!(before > a.available_bytes());
    drop(candidate);

    // save (deflate) and save_nocompress
    let mut work = doc.clone();
    let (bytes, save_peak) = peak_of(|| work.save());
    t.add(name, "save peak", save_peak, p.save_peak, false);
    t.add(name, "save len", bytes.len(), p.save_output, false);
    let mut work = doc.clone();
    let (raw, raw_peak) = peak_of(|| work.save_nocompress());
    t.add(name, "save_nc peak", raw_peak, p.save_peak, false);
    t.add(name, "save_nc len", raw.len(), p.save_output, false);
    drop(raw);

    // history export
    let mut work = doc.clone();
    let (changes, history_peak) = peak_of(|| work.get_changes(&[]));
    t.add(name, "history peak", history_peak, p.history_peak, false);
    drop(work);
    let (held, history_retained) = retained_of(|| changes.clone());
    drop(held);
    let inc = probe(&AutoCommit::new(), &changes).incoming_changes;
    t.add(name, "changes held", history_retained, inc, true);

    // load (the loader adds the fragment term once hashes are known)
    let mut limits = NativeLimits::default();
    limits.max_normalized_bytes = 64 * 1024 * 1024;
    limits.max_value_bytes = 64 * 1024 * 1024;
    let mut scratch = allowance();
    if let Ok(checked) = AutoCommit::inspect_bounded(&bytes, &limits, &mut scratch) {
        let actors = doc.get_actors_len();
        let bound = checked
            .shape()
            .native_load_peak_probe(fragments(&changes), actors)
            .unwrap();
        let (loaded, load_peak) = peak_of(|| AutoCommit::load(&bytes).unwrap());
        drop(loaded);
        t.add(name, "load peak", load_peak, bound, false);
    }
}

/// Apply `incoming` to a clone of `base`, measuring the peak against
/// `apply_peak_bound` (existing retained bytes are held by the document's
/// own credit, so only the rise above the start is measured).
fn measure_apply(t: &mut Table, name: &str, base: &AutoCommit, incoming: Vec<Change>) {
    let p = probe(base, &incoming);
    let mut doc = base.clone();
    let (result, peak) = peak_of(move || {
        let r = doc.apply_changes_batch(incoming);
        (r, doc)
    });
    let (r, doc) = result;
    r.unwrap();
    drop(doc);
    t.add(name, "apply peak", peak, p.apply_peak, false);
}

trait ActorsLen {
    fn get_actors_len(&self) -> usize;
}
impl ActorsLen for AutoCommit {
    fn get_actors_len(&self) -> usize {
        let mut c = self.clone();
        let mut actors = std::collections::BTreeSet::new();
        for ch in c.get_changes(&[]) {
            actors.insert(ch.actor_id().clone());
        }
        actors.len().max(1)
    }
}

fn shapes() -> Vec<(&'static str, AutoCommit)> {
    vec![
        ("empty", AutoCommit::new()),
        ("1 actor x 4000 changes", one_actor_many_changes(4000)),
        ("300 actors x 3 changes", many_actors_few_changes(300, 3)),
        ("deep nesting 2000", deep_nesting(2000)),
        ("text 20k + 500 marks", text_with_marks(20_000, 500)),
        ("map 10k keys", many_map_keys(10_000)),
        ("counter 5k increments", counter_increments(5_000)),
        ("large values 40 x 64KiB", large_values(40, 64 * 1024)),
        ("queued orphans 500", queued_orphans(500)),
    ]
}

#[test]
fn bounds_cover_adversarial_shapes() {
    let mut t = Table::default();
    for (name, mut doc) in shapes() {
        measure_doc(&mut t, name, &doc);
        let l = loaded(&mut doc);
        measure_doc(&mut t, &format!("{name} (loaded)"), &l);
    }
    t.finish();
}

fn edits(doc: &mut AutoCommit, commits: usize) {
    let list = doc.put_object(ROOT, "edits", ObjType::List).unwrap();
    for c in 0..commits {
        for i in 0..10 {
            doc.put(ROOT, format!("e{c}-{i}"), (c * 10 + i) as i64)
                .unwrap();
            doc.insert(&list, 0, i as i64).unwrap();
        }
        doc.commit();
    }
}

fn since(doc: &mut AutoCommit, heads: &[ChangeHash]) -> Vec<Change> {
    doc.get_changes(heads)
}

#[test]
fn apply_peak_covers_adversarial_batches() {
    let mut t = Table::default();
    for (name, mut base) in shapes() {
        let heads = base.get_heads();
        // A concurrent editor (actor sorting last) adds 40 commits.
        let mut fork = base.fork().with_actor(ActorId::from([0xffu8; 16]));
        edits(&mut fork, 40);
        let incoming = since(&mut fork, &heads);
        measure_apply(&mut t, &format!("{name} +fork"), &base, incoming);
        // A new actor that sorts before every existing one (actor remap).
        let mut first = base.fork().with_actor(ActorId::from([0u8; 16]));
        edits(&mut first, 5);
        let incoming = since(&mut first, &heads);
        measure_apply(&mut t, &format!("{name} +first actor"), &base, incoming);
        // Bulk: every change into an empty document.
        let all = base.get_changes(&[]);
        measure_apply(&mut t, &format!("{name} bulk"), &AutoCommit::new(), all);
    }
    // Orphans resolved: the missing dependency arrives.
    let mut source = AutoCommit::new().with_actor(actor(9));
    source.put(ROOT, "missing", 1).unwrap();
    source.commit();
    let skip = source.get_heads();
    for i in 0..500 {
        source.put(ROOT, format!("q{i}"), i as i64).unwrap();
        source.commit();
    }
    let all = source.get_changes(&[]);
    let (first, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|c| skip.contains(&c.hash()));
    let mut doc = AutoCommit::new().with_actor(actor(1));
    doc.apply_changes_batch(rest).unwrap();
    measure_apply(&mut t, "orphans resolved", &doc, first);
    t.finish();
}

#[test]
fn load_of_document_plus_many_change_chunks_fits_64_mib() {
    // L3's restored-state shape: a document chunk of 600 changes followed by
    // 600 separate change chunks.
    let mut doc = one_actor_many_changes(600);
    let mut bytes = doc.save();
    let heads = doc.get_heads();
    for i in 0..600 {
        doc.put(ROOT, format!("later{i}"), i as i64).unwrap();
        doc.commit();
    }
    for mut c in doc.get_changes(&heads) {
        bytes.extend_from_slice(&c.bytes());
    }
    let mut scratch = allowance();
    let checked =
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut scratch).unwrap();
    let changes = doc.get_changes(&[]);
    let bound = checked
        .shape()
        .native_load_peak_probe(fragments(&changes), 1)
        .unwrap();
    let (loaded, peak) = peak_of(|| AutoCommit::load(&bytes).unwrap());
    drop(loaded);
    println!("600 + 600 chunks: load peak {peak} bound {bound}");
    assert!(peak <= bound, "{peak} > {bound}");
    assert!(bound <= 64 * 1024 * 1024, "{bound} > 64 MiB");
}

#[test]
fn diff_cursor_patch_log_and_diff_cache_are_measured() {
    let mut doc = text_with_marks(2_000, 50);
    doc.update_diff_cursor();
    let text = doc.put_object(ROOT, "t2", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "hello world, a longer inserted string")
        .unwrap();
    let mark = automerge::marks::Mark::new(
        "bold".into(),
        ScalarValue::Str("a-long-mark-value-exceeding-inline".into()),
        0,
        5,
    );
    doc.mark(&text, mark, automerge::marks::ExpandMark::After)
        .unwrap();
    doc.put(ROOT, "k", "v".repeat(100)).unwrap();
    doc.commit();
    let _ = doc.diff_incremental();
    doc.put(ROOT, "k2", 1).unwrap();
    doc.commit();
    let a = allowance();
    let footprint = doc.native_footprint(&a).unwrap().total_bytes();
    let (copy, retained) = retained_of(|| doc.clone());
    drop(copy);
    println!("diff/patch caches: retained {retained} footprint {footprint}");
    assert!(retained <= footprint, "{retained} > {footprint}");
    let mut set = NativeAllocationSet::new(allowance()).unwrap();
    let turn = set.work_turn(a.clone()).unwrap();
    doc.clone_bounded(&mut set, &turn).unwrap();
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

/// A chain of `n` changes by one new actor on top of `base`; with `ground`
/// every hash is ground to a leading zero byte (fragment level 1), which
/// makes `ChangeGraph::fragments` quadratic.
fn fragment_chain(base: &mut AutoCommit, n: usize, ground: bool) -> Vec<Change> {
    use automerge::legacy::{OpId, OpType, SortedVec};
    let heads = base.get_heads();
    let mut peer = base.fork().with_actor(actor(11));
    peer.put(ROOT, "g", 0).unwrap();
    peer.commit();
    let mut t = peer.get_changes(&heads).pop().unwrap().decode();
    let start = t.start_op.get();
    let who = t.actor_id.clone();
    let mut deps = t.deps.clone();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        t.seq = i as u64 + 1;
        t.start_op = std::num::NonZeroU64::new(start + i as u64).unwrap();
        t.deps = deps.clone();
        t.operations[0].action = OpType::Put(ScalarValue::Int(i as i64));
        t.operations[0].pred = if i == 0 {
            SortedVec::from(Vec::new())
        } else {
            SortedVec::from(vec![OpId(start + i as u64 - 1, who.clone())])
        };
        t.hash = None;
        let mut time = 0i64;
        let change = loop {
            t.time = time;
            let c = Change::from(t.clone());
            if !ground || c.hash().as_ref()[0] == 0 {
                break c;
            }
            time += 1;
        };
        deps = vec![change.hash()];
        out.push(change);
    }
    out
}

#[test]
fn ground_fragment_chain_is_charged_quadratically() {
    let mut t = Table::default();
    let mut base = one_actor_many_changes(50);
    for (name, ground) in [("600 honest chain", false), ("600 ground chain", true)] {
        let chain = fragment_chain(&mut base, 600, ground);
        let [_, _, _, superlinear, ..] = base.native_bound_probe(&chain).unwrap();
        println!(
            "{name}: fragments {} superlinear bound {superlinear}",
            fragments(&chain)
        );
        let mut applied = base.clone();
        applied.apply_changes_batch(chain.clone()).unwrap();
        let (copy, held) = retained_of(|| applied.clone());
        drop(copy);
        println!("{name}: retained after apply {held}");
        measure_apply(&mut t, name, &base, chain);
    }
    t.finish();
}
