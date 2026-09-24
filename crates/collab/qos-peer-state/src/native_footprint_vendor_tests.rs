//! Real AutoCommit allocation/custody checks through the workspace dependency.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance, NativeResourceError};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, SaveOptions};

#[derive(Clone, Copy, Default, Debug)]
struct Allocations {
    live: usize,
    peak: usize,
    unmatched_free: bool,
    unreserved_peak: usize,
    first_unreserved: Option<(usize, usize, usize)>,
    watched_requests: usize,
}

thread_local! {
    static METER: Cell<Option<Allocations>> = const { Cell::new(None) };
    static CHECK_ALLOWANCE: RefCell<Option<(NativeAllowance, usize)>> = const { RefCell::new(None) };
    static WATCH_ALLOCATION_BYTES: Cell<Option<usize>> = const { Cell::new(None) };
}

struct MeasuredSystem;

fn allocated(bytes: usize) {
    let _ = METER.try_with(|meter| {
        if let Some(mut state) = meter.get() {
            if WATCH_ALLOCATION_BYTES
                .try_with(|watch| watch.get() == Some(bytes))
                .unwrap_or(false)
            {
                state.watched_requests += 1;
            }
            state.live = state
                .live
                .checked_add(bytes)
                .expect("test allocation counter overflow");
            state.peak = state.peak.max(state.live);
            let _ = CHECK_ALLOWANCE.try_with(|check| {
                if let Some((allowance, initial)) = &*check.borrow() {
                    let reserved = initial.saturating_sub(allowance.available_bytes());
                    if state.live > reserved && state.first_unreserved.is_none() {
                        state.first_unreserved = Some((bytes, state.live, reserved));
                    }
                    state.unreserved_peak = state
                        .unreserved_peak
                        .max(state.live.saturating_sub(reserved));
                }
            });
            meter.set(Some(state));
        }
    });
}

fn released(bytes: usize) {
    let _ = METER.try_with(|meter| {
        if let Some(mut state) = meter.get() {
            if let Some(remaining) = state.live.checked_sub(bytes) {
                state.live = remaining;
            } else {
                state.unmatched_free = true;
            }
            meter.set(Some(state));
        }
    });
}

// SAFETY: every allocation operation delegates unchanged to System. The
// thread-local counter contains only Copy scalars and never allocates.
unsafe impl GlobalAlloc for MeasuredSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc forwards its caller's valid layout to System.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc forwards its caller's valid layout to System.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        released(layout.size());
        // SAFETY: GlobalAlloc forwards the original allocation and layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: GlobalAlloc forwards the original allocation, layout and size.
        let grown = unsafe { System.realloc(pointer, layout, size) };
        if !grown.is_null() {
            // Count the conservative old/new overlap for a moving reallocation.
            allocated(size);
            released(layout.size());
        }
        grown
    }
}

#[global_allocator]
static ALLOCATOR: MeasuredSystem = MeasuredSystem;

fn measure<T>(operation: impl FnOnce() -> T) -> (T, Allocations) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            METER.with(|meter| meter.set(None));
        }
    }
    METER.with(|meter| {
        assert!(meter.get().is_none());
        meter.set(Some(Allocations::default()));
    });
    let _reset = Reset;
    let value = operation();
    let allocations = METER.with(|meter| meter.replace(None).unwrap());
    assert!(
        !allocations.unmatched_free,
        "fixture freed an unmeasured allocation"
    );
    (value, allocations)
}

fn measure_under<T>(
    allowance: &NativeAllowance,
    operation: impl FnOnce() -> T,
) -> (T, Allocations) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CHECK_ALLOWANCE.with(|check| *check.borrow_mut() = None);
        }
    }
    CHECK_ALLOWANCE.with(|check| {
        assert!(check.borrow().is_none());
        *check.borrow_mut() = Some((allowance.clone(), allowance.available_bytes()));
    });
    let _reset = Reset;
    let result = measure(operation);
    assert_eq!(
        result.1.unreserved_peak, 0,
        "every allocation must already be reserved, including planning metadata and realloc overlap: {:?}",
        result.1.first_unreserved
    );
    result
}

pub(super) fn assert_precharged<T>(
    allowance: &NativeAllowance,
    operation: impl FnOnce() -> T,
) -> T {
    measure_under(allowance, operation).0
}

fn populated_document() -> (AutoCommit, ObjId) {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, &"retained native text ".repeat(256))
        .unwrap();
    doc.commit();
    doc.update_diff_cursor();
    doc.mark(
        &text,
        Mark::new(
            "long-shared-mark-name-for-allocation".into(),
            "long-shared-mark-value-for-allocation",
            0,
            200,
        ),
        ExpandMark::Both,
    )
    .unwrap();
    doc.splice_text(&text, 8, 0, "marked insertion").unwrap();
    doc.commit();
    let heads = doc.get_heads();
    drop(doc.diff(&[], &heads));
    // Preserve populated patch/diff caches and an open transaction checkpoint.
    doc.put(
        ROOT,
        "pending",
        automerge::ScalarValue::Bytes(vec![7; 32_768]),
    )
    .unwrap();
    assert!(doc.pending_ops() > 0);
    (doc, text)
}

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

fn measure_destination<T>(
    credit: &NativeAllowance,
    bytes: usize,
    operation: impl FnOnce() -> T,
) -> (T, Allocations) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            WATCH_ALLOCATION_BYTES.with(|watch| watch.set(None));
        }
    }
    WATCH_ALLOCATION_BYTES.with(|watch| {
        assert!(watch.get().is_none());
        watch.set(Some(bytes));
    });
    let _reset = Reset;
    measure_under(credit, operation)
}

#[test]
fn native_save_output_limit_checks_exact_chosen_representation_before_destination() {
    let (mut source, _) = populated_document();
    source.commit();
    let mut unrelated = AutoCommit::new();
    unrelated.put(ROOT, "dependency", true).unwrap();
    unrelated.commit();
    unrelated.put(ROOT, "queued", vec![0x5A; 8192]).unwrap();
    unrelated.commit();
    let queued = unrelated.get_changes(&[])[1].clone();
    let queued_raw = queued.raw_bytes().to_vec();
    source.apply_changes([queued]).unwrap();
    let heads = source.get_heads();
    let credit = allowance();
    let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
    let turn = registry.work_turn(credit.clone()).unwrap();
    let candidate = source.clone_bounded(&mut registry, &turn).unwrap();
    for deflate in [false, true] {
        let options = || SaveOptions {
            deflate,
            retain_orphans: true,
        };
        let expected = candidate.save_bounded(&turn, options()).unwrap();
        assert!(expected.bytes().ends_with(&queued_raw));
        let length = expected.bytes().len();
        assert!(length > 1);
        let (exact, exact_allocations) = measure_destination(&credit, length, || {
            candidate.save_bounded_with_limit(&turn, options(), length)
        });
        let exact = exact.unwrap();
        assert_eq!(exact.bytes(), expected.bytes());
        assert_eq!(
            exact_allocations.watched_requests, 1,
            "exact-fit must allocate its destination once"
        );
        assert_eq!(exact_allocations.live, length);
        let mut restored = AutoCommit::load(exact.bytes()).unwrap();
        assert_eq!(restored.get_heads(), heads);
        let (short, short_allocations) = measure_destination(&credit, length, || {
            candidate.save_bounded_with_limit(&turn, options(), length - 1)
        });
        assert!(matches!(short, Err(NativeResourceError::WorkBytes)));
        assert_eq!(
            short_allocations.watched_requests, 0,
            "one-short limit must refuse before the destination allocation"
        );
        assert_eq!(short_allocations.live, 0);
    }
}

#[test]
fn native_bounded_save_preserves_heads_and_raw_change_identity() {
    let (mut source, _) = populated_document();
    source.commit();
    let heads = source.get_heads();
    let changes: Vec<_> = source
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let credit = allowance();
    let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
    let turn = registry.work_turn(credit.clone()).unwrap();
    let candidate = source.clone_bounded(&mut registry, &turn).unwrap();
    for deflate in [false, true] {
        let (saved, allocations) = measure_under(&credit, || {
            candidate.save_bounded(
                &turn,
                SaveOptions {
                    deflate,
                    retain_orphans: true,
                },
            )
        });
        let saved = saved.unwrap();
        assert_eq!(allocations.live, saved.bytes().len());
        println!(
            "save: deflate={deflate} output={} actual_peak={} unreserved_peak={}",
            saved.bytes().len(),
            allocations.peak,
            allocations.unreserved_peak
        );
        assert!(!saved.bytes().is_empty());
        let mut restored = AutoCommit::load(saved.bytes())
            .unwrap_or_else(|error| panic!("deflate={deflate}: {error}"));
        assert_eq!(restored.get_heads(), heads);
        let restored_changes: Vec<_> = restored
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        assert_eq!(restored_changes, changes);
    }
}

#[test]
fn native_bounded_save_refuses_open_transactions_and_small_turns() {
    let (mut source, _) = populated_document();
    let credit = allowance();
    let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
    let turn = registry.work_turn(credit.clone()).unwrap();
    let pending = source.clone_bounded(&mut registry, &turn).unwrap();
    assert!(matches!(
        pending.save_bounded(&turn, SaveOptions::default()),
        Err(NativeResourceError::Unaccounted)
    ));
    assert!(pending.pending_ops() > 0);
    source.commit();
    let closed = source.clone_bounded(&mut registry, &turn).unwrap();
    let tiny = NativeAllowance::new(1024).unwrap();
    let tiny_turn = registry.work_turn(tiny.clone()).unwrap();
    let result = assert_precharged(&tiny, || {
        closed.save_bounded(&tiny_turn, SaveOptions::default())
    });
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
}

#[test]
fn native_bounded_save_covers_empty_branches_counters_and_queued_changes() {
    let mut branches = AutoCommit::new();
    branches
        .put(ROOT, "count", automerge::ScalarValue::counter(0))
        .unwrap();
    branches.put(ROOT, "boolean", true).unwrap();
    branches
        .put(ROOT, "null", automerge::ScalarValue::Null)
        .unwrap();
    branches.commit();
    let mut other = branches.fork();
    for i in 1..=96 {
        branches.increment(ROOT, "count", i).unwrap();
    }
    branches.put(ROOT, "boolean", false).unwrap();
    branches.commit();
    other
        .put(ROOT, "concurrent", "a different actor's retained value")
        .unwrap();
    other.commit();
    branches.merge(&mut other).unwrap();

    let mut complete = AutoCommit::new();
    complete.put(ROOT, "first", 1).unwrap();
    complete.commit();
    complete.put(ROOT, "second", vec![0xA5; 8192]).unwrap();
    complete.commit();
    let changes = complete.get_changes(&[]);
    let orphan_raw = changes[1].raw_bytes().to_vec();
    let mut queued = AutoCommit::new();
    queued.apply_changes([changes[1].clone()]).unwrap();

    for (mut source, orphan) in [
        (AutoCommit::new(), None),
        (branches, None),
        (queued, Some(orphan_raw)),
    ] {
        let heads = source.get_heads();
        let missing = source.get_missing_deps(&[]);
        let mut original: Vec<_> = source
            .get_changes(&[])
            .iter()
            .map(|c| (c.hash(), c.raw_bytes().to_vec()))
            .collect();
        original.sort_by_key(|entry| entry.0);
        let credit = allowance();
        let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let candidate = source.clone_bounded(&mut registry, &turn).unwrap();
        for deflate in [false, true] {
            let result = assert_precharged(&credit, || {
                candidate.save_bounded(
                    &turn,
                    SaveOptions {
                        deflate,
                        retain_orphans: true,
                    },
                )
            });
            let saved = result.unwrap();
            if let Some(orphan) = &orphan {
                assert!(saved.bytes().ends_with(orphan));
            }
            let mut restored = AutoCommit::load(saved.bytes()).unwrap();
            assert_eq!(restored.get_heads(), heads);
            assert_eq!(restored.get_missing_deps(&[]), missing);
            let mut restored_changes: Vec<_> = restored
                .get_changes(&[])
                .iter()
                .map(|c| (c.hash(), c.raw_bytes().to_vec()))
                .collect();
            restored_changes.sort_by_key(|entry| entry.0);
            assert_eq!(restored_changes, original);
        }
        if orphan.is_some() {
            let saved = assert_precharged(&credit, || {
                candidate.save_bounded(
                    &turn,
                    SaveOptions {
                        deflate: true,
                        retain_orphans: false,
                    },
                )
            })
            .unwrap();
            let mut restored = AutoCommit::load(saved.bytes()).unwrap();
            assert_eq!(restored.get_heads(), heads);
            assert!(restored.get_missing_deps(&[]).is_empty());
        }
    }
}

#[test]
fn full_native_footprint_covers_actual_nonempty_retained_allocations() {
    // Warm thread-local RNG/runtime state before measuring document ownership.
    drop(populated_document());
    let ((doc, _text), allocations) = measure(populated_document);
    let allowance = allowance();
    let footprint = doc.native_footprint(&allowance).unwrap();
    println!(
        "retained fixture: actual={} owned_bound={} shared={}",
        allocations.live,
        footprint.owned_bytes(),
        footprint.shared_bytes()
    );
    assert!(allocations.live > 32_768);
    assert!(footprint.owned_bytes() > 0);
    assert!(footprint.shared_bytes() > 0);
    assert!(
        footprint.total_bytes() >= allocations.live,
        "declared {} below actual retained {}",
        footprint.total_bytes(),
        allocations.live
    );
    assert!(
        doc.pending_ops() > 0,
        "inspection must not close a transaction"
    );
}

#[test]
fn bounded_native_clone_preserves_real_pending_state_and_covers_allocations() {
    let (doc, _text) = populated_document();
    let allowance = allowance();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let before = allowance.available_bytes();
    let (cloned, measured) = measure_under(&allowance, || {
        doc.clone_bounded(&mut allocations, &turn).unwrap()
    });
    println!(
        "clone: actual_retained={} actual_peak={} retained_charge={} unreserved_peak={}",
        measured.live,
        measured.peak,
        before - allowance.available_bytes(),
        measured.unreserved_peak
    );
    assert_eq!(cloned.pending_ops(), doc.pending_ops());
    assert!(measured.live > 32_768);
    assert!(before - allowance.available_bytes() >= measured.live);
    assert!(measured.peak <= before);
}

#[test]
fn native_clone_refuses_before_copying_past_the_allowance() {
    let (doc, _) = populated_document();
    let allowance = NativeAllowance::new(1_024).unwrap();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let before = allowance.available_bytes();
    let (result, measured) =
        measure_under(&allowance, || doc.clone_bounded(&mut allocations, &turn));
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    assert!(
        measured.peak <= before,
        "refusal must precede the native clone allocation"
    );
    assert_eq!(allowance.available_bytes(), before);
    assert!(doc.pending_ops() > 0);
}

#[test]
fn bounded_native_clone_accounts_fresh_object_patch_events() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "initial", true).unwrap();
    doc.commit();
    doc.update_diff_cursor();
    let map = doc.put_object(ROOT, "new_map", ObjType::Map).unwrap();
    doc.put(&map, "value", "retained patch scalar").unwrap();
    doc.put_object(ROOT, "new_list", ObjType::List).unwrap();
    let allowance = allowance();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance).unwrap();
    let cloned = doc.clone_bounded(&mut allocations, &turn).unwrap();
    assert_eq!(cloned.pending_ops(), doc.pending_ops());
}

#[test]
fn native_shared_custody_deduplicates_and_outlives_the_registry() {
    let (doc, _) = populated_document();
    let allowance = allowance();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let initial = allowance.available_bytes();
    let first = doc.clone_bounded(&mut allocations, &turn).unwrap();
    let first_charge = initial - allowance.available_bytes();
    let after_first = allowance.available_bytes();
    let second = first.clone_bounded(&mut allocations, &turn).unwrap();
    let second_charge = after_first - allowance.available_bytes();
    assert!(
        second_charge < first_charge,
        "second document must share already charged backing"
    );
    drop(first);
    allocations.reconcile();
    assert!(
        allowance.available_bytes() < initial,
        "second document still retains credit"
    );
    drop(allocations);
    assert!(
        allowance.available_bytes() < initial,
        "document leases outlive the registry"
    );
    assert!(second.pending_ops() > 0);
    drop(second);
    // Registry metadata credit is now gone; the still-live turn keeps its own
    // small domain lease until it too is dropped.
    assert!(allowance.available_bytes() >= initial);
}

#[test]
fn native_reconciliation_releases_dropped_documents_without_a_cycle() {
    let (doc, _) = populated_document();
    let allowance = allowance();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let initial = allowance.available_bytes();
    for _ in 0..3 {
        let cloned = doc.clone_bounded(&mut allocations, &turn).unwrap();
        drop(cloned);
        assert!(allowance.available_bytes() < initial);
        allocations.reconcile();
        assert_eq!(allowance.available_bytes(), initial);
    }
}

#[test]
fn native_footprint_accounts_queued_compressed_change_storage() {
    let mut source = AutoCommit::new();
    source.set_actor(automerge::ActorId::from(vec![3; 80]));
    source.put(ROOT, "dependency", true).unwrap();
    source.commit();
    source
        .put(
            ROOT,
            "queued",
            automerge::ScalarValue::Bytes(vec![9; 65_536]),
        )
        .unwrap();
    source.commit();
    let mut change = source.get_last_local_change().unwrap();
    let _ = change.bytes(); // retain both original and compressed change storage
    let build = || {
        let mut doc = AutoCommit::new();
        doc.apply_changes([change.clone()]).unwrap();
        assert_eq!(doc.get_missing_deps(&[]).len(), 1);
        doc
    };
    drop(build());
    let (doc, measured) = measure(build);
    let allowance = allowance();
    let footprint = doc.native_footprint(&allowance).unwrap();
    assert!(measured.live > 65_536);
    assert!(footprint.total_bytes() >= measured.live);
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let (owned, _) = measure_under(&allowance, || {
        doc.clone_bounded(&mut allocations, &turn).unwrap()
    });
    assert_eq!(owned.pending_ops(), 0);
}

#[test]
fn native_footprint_accounts_mark_tree_weights_and_long_actor_storage() {
    let build = || {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(vec![4; 96]));
        let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, &"mark tree storage ".repeat(512))
            .unwrap();
        doc.commit();
        doc.update_diff_cursor();
        for index in 0..96 {
            doc.mark(
                &text,
                Mark::new(
                    format!("long-mark-name-for-weight-{index}"),
                    format!("long-value-for-mark-{index}"),
                    index * 2,
                    400 + index * 2,
                ),
                ExpandMark::Both,
            )
            .unwrap();
        }
        doc.commit();
        // The last transaction retains a second complete tree checkpoint.
        doc.splice_text(&text, 350, 0, "inside many marks").unwrap();
        doc
    };
    drop(build());
    let (doc, measured) = measure(build);
    let allowance = allowance();
    let footprint = doc.native_footprint(&allowance).unwrap();
    println!(
        "mark tree: actual={} owned_bound={} shared={}",
        measured.live,
        footprint.owned_bytes(),
        footprint.shared_bytes()
    );
    assert!(footprint.total_bytes() >= measured.live);
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    let (cloned, _) = measure_under(&allowance, || {
        doc.clone_bounded(&mut allocations, &turn).unwrap()
    });
    assert_eq!(cloned.pending_ops(), doc.pending_ops());
}

#[test]
fn native_graph_load_rejects_dependency_indices_without_panicking() {
    let mut valid = AutoCommit::new();
    valid.put(ROOT, "first", 1_u64).unwrap();
    valid.commit();
    valid.put(ROOT, "dependent", 2_u64).unwrap();
    valid.commit();
    let expected_heads = valid.get_heads();
    for encoded in [valid.save(), valid.save_nocompress()] {
        let mut loaded = AutoCommit::load(&encoded).unwrap();
        assert_eq!(loaded.get_heads(), expected_heads);
    }
    for encoded in [
        // One change with a dependency index beyond the prior graph.
        "856f4a83e9e773df001a0101000005010203021302400243020001000101010101010102",
        // One max-op row, two dependency-count rows.
        "856f4a830501ac78001601010000040102030213024002000100010101010200",
    ] {
        let bytes: Vec<u8> = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => panic!("invalid fixture"),
                };
                digit(pair[0]) * 16 + digit(pair[1])
            })
            .collect();
        let loaded = std::panic::catch_unwind(|| AutoCommit::load(&bytes));
        assert!(
            loaded.is_ok(),
            "malformed graph must return an error before direct indexing"
        );
        assert!(loaded.unwrap().is_err());
    }
}

#[test]
fn native_work_turn_denies_cross_registry_and_uses_new_work_credit() {
    let (doc, _) = populated_document();
    let initial = allowance();
    let mut allocations = NativeAllocationSet::new(initial.clone()).unwrap();
    let first_turn = allocations.work_turn(initial.clone()).unwrap();
    let first = doc.clone_bounded(&mut allocations, &first_turn).unwrap();
    let old_available = initial.available_bytes();
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let before = current.available_bytes();
    let second = measure_under(&current, || {
        first.clone_bounded(&mut allocations, &turn).unwrap()
    })
    .0;
    assert_eq!(
        initial.available_bytes(),
        old_available,
        "new work cannot debit an old resident ledger"
    );
    assert!(current.available_bytes() < before);
    let wrong_set = NativeAllocationSet::new(allowance()).unwrap();
    let wrong_turn = wrong_set.work_turn(allowance()).unwrap();
    assert!(matches!(
        second.clone_bounded(&mut allocations, &wrong_turn),
        Err(NativeResourceError::Unaccounted)
    ));
}

#[test]
fn native_finished_turn_releases_reservations_without_reopening_work() {
    let allowance = NativeAllowance::new(4_096).unwrap();
    let metadata = allowance.retained_bytes();
    let retained = allowance.reserve(1_024).unwrap();
    allowance.finish_turn();
    assert!(matches!(
        allowance.reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    assert_eq!(allowance.retained_bytes(), metadata + 1_024);
    drop(retained);
    assert_eq!(allowance.retained_bytes(), metadata);
    assert!(matches!(
        allowance.reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
}
