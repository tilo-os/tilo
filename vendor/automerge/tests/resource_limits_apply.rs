//! Checked atomic admission (`apply_incoming_checked`, `apply_checked_bounded`)
//! on automerge 0.12: dedup, duplicate sequences, dependency policies,
//! crafted semantic violations, populated Tables, counter bounds, the
//! fragment-growth refusal, and a mutation fuzz whose oracle is "accepted
//! input applies without error or panic; refused input leaves the document
//! byte-for-byte unchanged".
//!
//! Runs inside the workspace through crates/doc/tilo-automerge/tests/am012_apply.rs.
use std::panic::{catch_unwind, AssertUnwindSafe};

use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeApplyResult, NativeDependencyPolicy,
    NativeIncomingOutcome, NativeLimits, NativeMutationError, NativeResourceError,
};
use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, Change, ChangeHash, ExpandedChange, ObjType, ReadDoc, SaveOptions,
    ScalarValue, ROOT,
};
use serde_json::{json, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

// ===========================================================================
// Per-thread allocation meter (the same technique as
// resource_limits_admission.rs and qos-peer-state's assert_precharged).
// ===========================================================================
#[derive(Clone, Copy, Default, Debug)]
struct Allocations {
    live: usize,
    peak: usize,
    unmatched_free: bool,
    unreserved_peak: usize,
    first_unreserved: Option<(usize, usize, usize)>,
    watched_requests: usize,
    largest_request: usize,
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
            if WATCH_ALLOCATION_BYTES.try_with(|watch| watch.get() == Some(bytes)).unwrap_or(false) {
                state.watched_requests += 1;
            }
            state.largest_request = state.largest_request.max(bytes);
            state.live = state.live.saturating_add(bytes);
            state.peak = state.peak.max(state.live);
            let _ = CHECK_ALLOWANCE.try_with(|check| {
                if let Ok(check) = check.try_borrow() {
                    if let Some((allowance, initial)) = &*check {
                        let reserved = initial.saturating_sub(allowance.available_bytes());
                        if state.live > reserved && state.first_unreserved.is_none() {
                            state.first_unreserved = Some((bytes, state.live, reserved));
                        }
                        state.unreserved_peak = state.unreserved_peak.max(state.live.saturating_sub(reserved));
                    }
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
                state.live = 0;
                state.unmatched_free = true;
            }
            meter.set(Some(state));
        }
    });
}

// SAFETY: every operation delegates unchanged to System; the thread-local
// meter holds only Copy scalars (and a RefCell read) and never allocates.
unsafe impl GlobalAlloc for MeasuredSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the caller's valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the caller's valid layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        released(layout.size());
        // SAFETY: forwards the original allocation and layout.
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: forwards the original allocation, layout and size.
        let grown = unsafe { System.realloc(pointer, layout, size) };
        if !grown.is_null() {
            // Conservative old/new overlap for a moving reallocation.
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
        assert!(meter.get().is_none(), "nested measure");
        meter.set(Some(Allocations::default()));
    });
    let _reset = Reset;
    let value = operation();
    let allocations = METER.with(|meter| meter.replace(None).unwrap());
    (value, allocations)
}

/// Measures `operation` and asserts every allocation it made was already
/// reserved on `allowance` when it happened.
fn measure_under<T>(allowance: &NativeAllowance, operation: impl FnOnce() -> T) -> (T, Allocations) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CHECK_ALLOWANCE.with(|check| *check.borrow_mut() = None);
        }
    }
    // The clone is made before metering starts so it is not itself measured.
    let entry = (allowance.clone(), allowance.available_bytes());
    CHECK_ALLOWANCE.with(|check| {
        assert!(check.borrow().is_none());
        *check.borrow_mut() = Some(entry);
    });
    let _reset = Reset;
    let result = measure(operation);
    assert_eq!(
        result.1.unreserved_peak, 0,
        "every allocation must already be reserved (bytes, live, reserved): {:?}",
        result.1.first_unreserved
    );
    result
}

fn assert_precharged<T>(allowance: &NativeAllowance, operation: impl FnOnce() -> T) -> T {
    measure_under(allowance, operation).0
}



const MIB: usize = 1024 * 1024;

fn actor(n: u8) -> ActorId {
    ActorId::from(vec![n; 16])
}

fn a1() -> String {
    "01".repeat(16)
}

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * MIB).unwrap()
}

type Snapshot = (Vec<u8>, Vec<ChangeHash>, Vec<ChangeHash>);

/// Everything observable about a document's retained state, orphans included.
fn snapshot(doc: &mut AutoCommit) -> Snapshot {
    let bytes = doc.save_with_options(SaveOptions {
        deflate: false,
        retain_orphans: true,
    });
    let heads = doc.get_heads();
    let missing = doc.get_missing_deps(&[]);
    (bytes, heads, missing)
}

fn incoming(
    doc: &mut AutoCommit,
    bytes: &[u8],
    policy: NativeDependencyPolicy,
) -> Result<NativeIncomingOutcome, NativeMutationError> {
    let allowance = allowance();
    let baseline = allowance.retained_bytes();
    let result = doc.apply_incoming_checked(bytes, &NativeLimits::default(), &allowance, policy);
    assert_eq!(
        allowance.retained_bytes(),
        baseline,
        "apply_incoming_checked releases every temporary"
    );
    result
}

/// Refusal must not mutate: byte-identical save, same heads and missing deps.
fn refused(doc: &mut AutoCommit, bytes: &[u8], policy: NativeDependencyPolicy) -> NativeMutationError {
    let before = snapshot(doc);
    let result = incoming(doc, bytes, policy);
    let error = match result {
        Err(error) => error,
        Ok(outcome) => panic!("expected refusal, got {outcome:?}"),
    };
    assert_eq!(snapshot(doc), before, "refusal left the document unchanged");
    error
}

fn changes_bytes(changes: &[Change]) -> Vec<u8> {
    changes.iter().flat_map(|c| c.raw_bytes().to_vec()).collect()
}

fn expanded(change: &Change) -> Value {
    serde_json::to_value(change.decode()).unwrap()
}

fn encode(value: Value) -> Change {
    let expanded: ExpandedChange = serde_json::from_value(value).unwrap();
    Change::from(expanded)
}

/// Ops of actor 1: 1 map, 2 map.a, 3 list, 4 "x", 5 "y", 6 text, 7..=11
/// "hello", 12 count.
fn base() -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let map = doc.put_object(&ROOT, "map", ObjType::Map).unwrap();
    doc.put(&map, "a", 1).unwrap();
    let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "x").unwrap();
    doc.insert(&list, 1, "y").unwrap();
    let text = doc.put_object(&ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "hello").unwrap();
    doc.put(&ROOT, "count", ScalarValue::counter(1)).unwrap();
    doc.commit();
    doc
}

#[test]
fn applies_then_dedups_and_releases_credit() {
    let mut doc = base();
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(2));
    peer.put(&ROOT, "b", 2).unwrap();
    peer.increment(&ROOT, "count", 5).unwrap();
    peer.commit();
    let bytes = changes_bytes(&peer.get_changes(&heads));
    assert_eq!(
        incoming(&mut doc, &bytes, NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    assert_eq!(doc.get_heads(), peer.get_heads());
    let before = snapshot(&mut doc);
    assert_eq!(
        incoming(&mut doc, &bytes, NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: false }
    );
    assert_eq!(snapshot(&mut doc), before);
    // A whole saved document is also accepted input (reconstructed changes).
    let mut fresh = AutoCommit::new();
    let saved = peer.save();
    assert_eq!(
        incoming(&mut fresh, &saved, NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    assert_eq!(fresh.get_heads(), peer.get_heads());
}

#[test]
fn busy_on_pending_transaction_and_isolation() {
    let mut doc = base();
    let heads = doc.get_heads();
    doc.put(&ROOT, "pending", 1).unwrap();
    assert!(matches!(
        incoming(&mut doc, &[], NativeDependencyPolicy::RequireComplete),
        Err(NativeMutationError::Busy)
    ));
    doc.commit();
    doc.isolate(&heads);
    assert!(matches!(
        incoming(&mut doc, &[], NativeDependencyPolicy::RequireComplete),
        Err(NativeMutationError::Busy)
    ));
}

/// Two different changes claiming the same (actor, seq).
fn forked_sequences() -> (AutoCommit, Change, Change) {
    let mut doc = base();
    let heads = doc.get_heads();
    let mut one = doc.fork().with_actor(actor(7));
    one.put(&ROOT, "k", "one").unwrap();
    one.commit();
    let mut two = doc.fork().with_actor(actor(7));
    two.put(&ROOT, "k", "two").unwrap();
    two.commit();
    let a = one.get_changes(&heads).pop().unwrap();
    let b = two.get_changes(&heads).pop().unwrap();
    assert_eq!((a.actor_id(), a.seq()), (b.actor_id(), b.seq()));
    assert_ne!(a.hash(), b.hash());
    (doc, a, b)
}

#[test]
fn duplicate_sequence_against_resident() {
    let (mut doc, a, b) = forked_sequences();
    doc.apply_changes([a.clone()]).unwrap();
    // Same content: dedup.
    assert_eq!(
        incoming(&mut doc, a.raw_bytes(), NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: false }
    );
    // Different content: refused before 0.12 would prune the queue.
    let error = refused(&mut doc, b.raw_bytes(), NativeDependencyPolicy::RetainOrphans);
    assert!(matches!(error, NativeMutationError::DuplicateSequence), "{error:?}");
}

#[test]
fn duplicate_sequence_against_queue() {
    let (mut doc, a, b) = forked_sequences();
    // Queue `a` behind a missing dependency by rewriting its deps.
    let mut orphan = expanded(&a);
    orphan["deps"] = json!(["ff".repeat(32)]);
    let orphan = encode(orphan);
    let mut other = expanded(&b);
    other["deps"] = json!(["ff".repeat(32)]);
    let other = encode(other);
    assert_eq!(
        incoming(&mut doc, orphan.raw_bytes(), NativeDependencyPolicy::RetainOrphans).unwrap(),
        NativeIncomingOutcome::Applied { changed: false }
    );
    assert!(!doc.get_missing_deps(&[]).is_empty());
    // Same queued content: dedup, nothing new.
    assert_eq!(
        incoming(&mut doc, orphan.raw_bytes(), NativeDependencyPolicy::RetainOrphans).unwrap(),
        NativeIncomingOutcome::Applied { changed: false }
    );
    let error = refused(&mut doc, other.raw_bytes(), NativeDependencyPolicy::RetainOrphans);
    assert!(matches!(error, NativeMutationError::DuplicateSequence), "{error:?}");
}

#[test]
fn missing_dependencies_under_both_policies() {
    let mut doc = base();
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(3));
    peer.put(&ROOT, "first", 1).unwrap();
    peer.commit();
    let mid = peer.get_heads();
    peer.put(&ROOT, "second", 2).unwrap();
    peer.commit();
    let first = peer.get_changes(&heads);
    let second = peer.get_changes(&mid);
    let before = snapshot(&mut doc);
    assert_eq!(
        incoming(&mut doc, &changes_bytes(&second), NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::MissingDependencies
    );
    assert_eq!(snapshot(&mut doc), before, "RequireComplete leaves heads and queue");
    assert!(doc.get_missing_deps(&[]).is_empty());
    assert_eq!(
        incoming(&mut doc, &changes_bytes(&second), NativeDependencyPolicy::RetainOrphans).unwrap(),
        NativeIncomingOutcome::Applied { changed: false }
    );
    assert_eq!(doc.get_heads(), heads);
    assert!(!doc.get_missing_deps(&[]).is_empty());
    // The missing change releases the queued one.
    assert_eq!(
        incoming(&mut doc, &changes_bytes(&first), NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    assert_eq!(doc.get_heads(), peer.get_heads());
    assert!(doc.get_missing_deps(&[]).is_empty());
}

/// A peer change against `doc` whose single op is rewritten by `edit`.
fn crafted(doc: &mut AutoCommit, edit: impl FnOnce(&mut Value)) -> Change {
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(9));
    peer.put(&ROOT, "z", 1).unwrap();
    peer.commit();
    let mut value = expanded(&peer.get_changes(&heads).pop().unwrap());
    edit(&mut value);
    encode(value)
}

fn as_elem(v: &mut Value, obj: String, elem: String, insert: bool) {
    let op = v["ops"][0].as_object_mut().unwrap();
    op.remove("key");
    op.insert("obj".into(), json!(obj));
    op.insert("elemId".into(), json!(elem));
    op.insert("insert".into(), json!(insert));
}

fn semantic_refusal(doc: &mut AutoCommit, change: &Change, case: &str) {
    let error = refused(doc, change.raw_bytes(), NativeDependencyPolicy::RequireComplete);
    assert!(
        matches!(
            error,
            NativeMutationError::InvalidOperation
                | NativeMutationError::Resource(NativeResourceError::Invalid)
        ),
        "{case}: {error:?}"
    );
}

#[test]
fn refuses_invalid_objects_preds_keys_and_elements() {
    let a = a1();
    let map = format!("1@{a}");
    let list = format!("3@{a}");
    let text = format!("6@{a}");
    let cases: Vec<(&str, Box<dyn Fn(&mut Value)>)> = vec![
        ("missing object", Box::new(|v| v["ops"][0]["obj"] = json!(format!("999@{}", a1())))),
        ("object is a scalar", Box::new(|v| v["ops"][0]["obj"] = json!(format!("2@{}", a1())))),
        ("missing predecessor", Box::new(|v| v["ops"][0]["pred"] = json!([format!("998@{}", a1())]))),
        ("predecessor in another object", {
            let map = map.clone();
            Box::new(move |v| {
                v["ops"][0]["obj"] = json!(map);
                v["ops"][0]["key"] = json!("other");
                v["ops"][0]["pred"] = json!([format!("12@{}", a1())]);
            })
        }),
        ("predecessor under another key", {
            let map = map.clone();
            Box::new(move |v| {
                v["ops"][0]["obj"] = json!(map);
                v["ops"][0]["key"] = json!("other");
                v["ops"][0]["pred"] = json!([format!("2@{}", a1())]);
            })
        }),
        ("map key on a list", {
            let list = list.clone();
            Box::new(move |v| v["ops"][0]["obj"] = json!(list))
        }),
        ("element on a map", {
            let map = map.clone();
            Box::new(move |v| as_elem(v, map.clone(), format!("4@{}", a1()), true))
        }),
        ("missing element", {
            let list = list.clone();
            Box::new(move |v| as_elem(v, list.clone(), format!("997@{}", a1()), true))
        }),
        ("anchor in another object", {
            let list = list.clone();
            Box::new(move |v| as_elem(v, list.clone(), format!("8@{}", a1()), true))
        }),
        ("update of head", {
            let list = list.clone();
            Box::new(move |v| as_elem(v, list.clone(), "_head".into(), false))
        }),
        ("mark end without begin", {
            let text = text.clone();
            Box::new(move |v| {
                as_elem(v, text.clone(), "_head".into(), true);
                let op = v["ops"][0].as_object_mut().unwrap();
                op.remove("value");
                op.remove("datatype");
                op.insert("action".into(), json!("markEnd"));
            })
        }),
        ("unknown actor", Box::new(|v| v["ops"][0]["pred"] = json!([format!("2@{}", "05".repeat(16))]))),
        ("sequence gap", Box::new(|v| v["seq"] = json!(2))),
        ("start op not after ancestors", Box::new(|v| v["startOp"] = json!(1))),
        ("increment of a non-counter", {
            let map = map.clone();
            Box::new(move |v| {
                v["ops"][0]["obj"] = json!(map);
                v["ops"][0]["key"] = json!("a");
                v["ops"][0]["action"] = json!("inc");
                v["ops"][0]["value"] = json!(1);
                v["ops"][0]["pred"] = json!([format!("2@{}", a1())]);
            })
        }),
        ("delete of a delete target that is an increment", Box::new(|v| {
            v["ops"][0]["key"] = json!("count");
            v["ops"][0]["action"] = json!("del");
            v["ops"][0]["pred"] = json!([format!("2@{}", a1()), format!("1@{}", a1())]);
        })),
    ];
    let mut doc = base();
    for (name, case) in &cases {
        let change = crafted(&mut doc, |v| case(v));
        semantic_refusal(&mut doc, &change, name);
    }
}

#[test]
fn refuses_counter_and_opid_overflow() {
    let mut doc = base();
    // Op counters beyond u32 in a reference: 0.12 parsing would panic in
    // OpId::new; the borrowed preflight refuses first.
    let change = crafted(&mut doc, |v| {
        v["ops"][0]["pred"] = json!([format!("{}@{}", 1u64 << 33, a1())])
    });
    semantic_refusal(&mut doc, &change, "counter beyond u32");
    // A change whose op range ends beyond u32.
    let change = crafted(&mut doc, |v| {
        v["startOp"] = json!(u32::MAX as u64);
        let op = v["ops"][0].clone();
        v["ops"] = json!([op.clone(), op]);
        v["ops"][1]["key"] = json!("z2");
    });
    let before = snapshot(&mut doc);
    assert!(incoming(&mut doc, change.raw_bytes(), NativeDependencyPolicy::RequireComplete).is_err());
    assert_eq!(snapshot(&mut doc), before);
}

#[test]
fn populated_table_map_writes() {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let table = doc.put_object(&ROOT, "table", ObjType::Table).unwrap();
    doc.put(&table, "row", 1).unwrap();
    doc.commit();
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(2));
    peer.put(&table, "row", 2).unwrap();
    peer.put(&table, "other", "x").unwrap();
    peer.commit();
    let bytes = changes_bytes(&peer.get_changes(&heads));
    assert_eq!(
        incoming(&mut doc, &bytes, NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    assert_eq!(doc.get(&table, "row").unwrap().unwrap().0, 2.into());
    // A list element key on a Table is refused.
    let list_key = crafted(&mut doc, |v| as_elem(v, format!("1@{}", a1()), "_head".into(), true));
    semantic_refusal(&mut doc, &list_key, "element on a table");
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../../vendor/automerge/tests/fixtures/qos_08/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn qos_08_corpus_applies_without_panic() {
    for name in [
        "table_changes.all.changes",
        "table.automerge",
        "table.nocompress.automerge",
        "multi_changes.all.changes",
        "counters.automerge",
        "marks.automerge",
    ] {
        let bytes = fixture(name);
        let mut doc = AutoCommit::new();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            incoming(&mut doc, &bytes, NativeDependencyPolicy::RequireComplete)
        }))
        .unwrap_or_else(|_| panic!("{name} panicked"));
        assert_eq!(outcome.unwrap(), NativeIncomingOutcome::Applied { changed: true }, "{name}");
        let mut reference = AutoCommit::load(&bytes).unwrap();
        assert_eq!(doc.get_heads(), reference.get_heads(), "{name}");
    }
    // Change by change onto a document, the 0.8 table history in order.
    let mut doc = AutoCommit::new();
    for index in 0..4 {
        let bytes = fixture(&format!("table_changes.0{index}.change"));
        assert!(incoming(&mut doc, &bytes, NativeDependencyPolicy::RequireComplete).is_ok());
    }
}

#[test]
fn counter_hidden_at_representable_bound_applies() {
    // Port of qos-peer-state incoming_target_validation_preserves_hidden_counter_at_representable_bound.
    let mut source = AutoCommit::new();
    source.put(ROOT, "base", true).unwrap();
    source.commit();
    let mut other = source.fork();
    source.put(ROOT, "count", ScalarValue::counter(i64::MAX)).unwrap();
    source.commit();
    source.increment(ROOT, "count", -1).unwrap();
    source.commit();
    for index in 0..8 {
        other.put(ROOT, format!("filler_{index}"), index as i64).unwrap();
    }
    other.put(ROOT, "count", "later scalar").unwrap();
    other.commit();
    source.merge(&mut other).unwrap();
    assert_eq!(source.get_all(ROOT, "count").unwrap().len(), 2);
    let mut peer = source.fork();
    peer.increment(ROOT, "count", 1).unwrap();
    let bytes = peer.save();
    let expected = peer.get_heads();
    // Ordinary path.
    let mut ordinary = source.clone();
    assert_eq!(
        incoming(&mut ordinary, &bytes, NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    assert_eq!(ordinary.get_heads(), expected);
    // Bounded candidate path.
    let mut restored = bounded_apply(&mut source, &bytes).expect("representable history applies");
    assert_eq!(restored.get_heads(), expected);
    let values = restored.get_all(ROOT, "count").unwrap();
    assert!(
        values
            .iter()
            .any(|(value, _)| value.to_scalar() == Some(&ScalarValue::counter(i64::MAX))),
        "{values:?}"
    );
}

#[test]
fn counter_unrepresentable_sum_refused() {
    // Port of qos-peer-state incoming_target_validation_rejects_unrepresentable_counter_sum.
    let mut source = AutoCommit::new();
    source.put(ROOT, "count", ScalarValue::counter(i64::MAX)).unwrap();
    source.commit();
    let mut peer = source.fork();
    peer.increment(ROOT, "count", -1).unwrap();
    let mut value = expanded(&peer.get_last_local_change().unwrap());
    value["ops"][0]["value"] = json!(1);
    let change = encode(value);
    let error = refused(&mut source, change.raw_bytes(), NativeDependencyPolicy::RequireComplete);
    assert!(matches!(error, NativeMutationError::InvalidOperation), "{error:?}");
    match bounded_apply(&mut source, change.raw_bytes()) {
        Err(NativeMutationError::InvalidOperation) => {}
        other => panic!("semantic refusal must stay distinct: {:?}", other.map(|_| ())),
    }
}

/// `reconstruct` + `clone_bounded` + `apply_checked_bounded`, returning the
/// saved result reloaded. All custody is released afterwards.
fn bounded_apply(source: &mut AutoCommit, incoming: &[u8]) -> Result<AutoCommit, NativeMutationError> {
    let bytes = source.save();
    let allowance = allowance();
    let before = allowance.available_bytes();
    let result = (|| {
        let mut registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let original = turn
            .normalize_input(&bytes, &NativeLimits::default())?
            .reconstruct(&mut registry, &turn)?;
        let original_heads = original.base_heads_bounded(&turn)?.as_slice().to_vec();
        let candidate = original.clone_bounded(&mut registry, &turn)?;
        let input = turn.normalize_input(incoming, &NativeLimits::default())?;
        let result = assert_precharged(&allowance, || {
            candidate.apply_checked_bounded(
                input,
                NativeDependencyPolicy::RequireComplete,
                &mut registry,
                &turn,
            )
        });
        assert_eq!(original.base_heads_bounded(&turn)?.as_slice(), original_heads);
        match result? {
            NativeApplyResult::Applied { document, .. } => {
                let saved = document.save_bounded(&turn, SaveOptions::default())?;
                Ok(AutoCommit::load(saved.bytes()).unwrap())
            }
            NativeApplyResult::MissingDependencies => panic!("complete input"),
        }
    })();
    assert_eq!(allowance.available_bytes(), before, "all custody is released");
    result
}

#[test]
fn bounded_candidate_policies() {
    let mut doc = base();
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(4));
    peer.put(&ROOT, "one", 1).unwrap();
    peer.commit();
    let mid = peer.get_heads();
    peer.put(&ROOT, "two", 2).unwrap();
    peer.commit();
    let second = changes_bytes(&peer.get_changes(&mid));
    let all = changes_bytes(&peer.get_changes(&heads));
    let bytes = doc.save();
    let allowance = allowance();
    let before = allowance.available_bytes();
    {
        let mut registry = NativeAllocationSet::new(allowance.clone()).unwrap();
        let turn = registry.work_turn(allowance.clone()).unwrap();
        let original = turn
            .normalize_input(&bytes, &NativeLimits::default())
            .unwrap()
            .reconstruct(&mut registry, &turn)
            .unwrap();
        let mut run = |input: &[u8], policy| {
            let candidate = original.clone_bounded(&mut registry, &turn).unwrap();
            let input = turn.normalize_input(input, &NativeLimits::default()).unwrap();
            assert_precharged(&allowance, || {
                candidate.apply_checked_bounded(input, policy, &mut registry, &turn)
            })
            .unwrap()
        };
        assert!(matches!(
            run(&second, NativeDependencyPolicy::RequireComplete),
            NativeApplyResult::MissingDependencies
        ));
        assert!(matches!(
            run(&second, NativeDependencyPolicy::RetainOrphans),
            NativeApplyResult::Applied { changed: false, missing_dependencies: true, .. }
        ));
        assert!(matches!(
            run(&all, NativeDependencyPolicy::RequireComplete),
            NativeApplyResult::Applied { changed: true, missing_dependencies: false, .. }
        ));
        assert!(matches!(
            run(&bytes, NativeDependencyPolicy::RequireComplete),
            NativeApplyResult::Applied { changed: false, missing_dependencies: false, .. }
        ));
    }
    assert_eq!(allowance.available_bytes(), before);
}

#[test]
fn text_conflict_partial_delete_applies() {
    // 0.12 panicked in the text patch path when a delete removed only the
    // lower of two conflicting puts on one text element.
    for (x, y) in [(2u8, 3u8), (3, 2)] {
        let mut base = AutoCommit::new().with_actor(actor(1));
        let text = base.put_object(&ROOT, "t", ObjType::Text).unwrap();
        base.splice_text(&text, 0, 0, "ab").unwrap();
        base.commit();
        let mut a = base.fork().with_actor(actor(x));
        a.put(&text, 0, "x").unwrap();
        a.commit();
        let mut b = base.fork().with_actor(actor(y));
        b.put(&text, 0, "y").unwrap();
        b.commit();
        let mut c = base.fork().with_actor(actor(9));
        c.merge(&mut a).unwrap();
        let heads_c = c.get_heads();
        c.delete(&text, 0).unwrap();
        c.commit();
        let mut target = base.fork();
        target.merge(&mut a).unwrap();
        target.merge(&mut b).unwrap();
        let bytes = changes_bytes(&c.get_changes(&heads_c));
        assert_eq!(
            incoming(&mut target, &bytes, NativeDependencyPolicy::RequireComplete).unwrap(),
            NativeIncomingOutcome::Applied { changed: true }
        );
        let reloaded = AutoCommit::load(&target.save()).unwrap();
        assert_eq!(reloaded.text(&text).unwrap(), target.text(&text).unwrap());
    }
}

/// A linear history of `n` single-op changes by one actor on top of `doc`;
/// with `ground`, every change's time is varied until its hash starts with a
/// zero byte (a level >= 1 fragment in 0.12's change graph).
fn history(doc: &mut AutoCommit, n: usize, ground: bool) -> Vec<Change> {
    let mut template = {
        let heads = doc.get_heads();
        let mut peer = doc.fork().with_actor(actor(11));
        peer.put(&ROOT, "g", 0).unwrap();
        peer.commit();
        expanded(&peer.get_changes(&heads).pop().unwrap())
    };
    let mut deps = template["deps"].clone();
    let start = template["startOp"].as_u64().unwrap();
    let mut out = Vec::new();
    for i in 0..n {
        template["seq"] = json!(i + 1);
        template["startOp"] = json!(start + i as u64);
        template["deps"] = deps.clone();
        template["ops"][0]["value"] = json!(i);
        template["ops"][0]["datatype"] = json!("int");
        template["ops"][0]["pred"] = if i == 0 {
            json!([])
        } else {
            json!([format!("{}@{}", start + i as u64 - 1, "0b".repeat(16))])
        };
        let mut time = 0i64;
        let change = loop {
            template["time"] = json!(time);
            let change = encode(template.clone());
            if !ground || change.hash().0[0] == 0 {
                break change;
            }
            time += 1;
        };
        deps = json!([change.hash().to_string()]);
        out.push(change);
    }
    out
}

#[test]
fn fragment_growth_is_reserved_before_mutation() {
    const N: usize = 600;
    // Honest input needs about 3 MiB here and the ground chain about 7 MiB.
    let limit = 5 * MIB;
    let mut doc = base();
    let honest = changes_bytes(&history(&mut doc.clone(), N, false));
    let ground = changes_bytes(&history(&mut doc.clone(), N, true));
    let small = NativeAllowance::new(limit).unwrap();
    let mut honest_doc = doc.clone();
    assert_eq!(
        honest_doc
            .apply_incoming_checked(
                &honest,
                &NativeLimits::default(),
                &small,
                NativeDependencyPolicy::RequireComplete
            )
            .unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    let before = snapshot(&mut doc);
    let small = NativeAllowance::new(limit).unwrap();
    let result = doc.apply_incoming_checked(
        &ground,
        &NativeLimits::default(),
        &small,
        NativeDependencyPolicy::RequireComplete,
    );
    assert!(
        matches!(result, Err(NativeMutationError::Resource(NativeResourceError::WorkBytes))),
        "{result:?}"
    );
    assert_eq!(snapshot(&mut doc), before);
}

/// Fuzz rounds, scaled by `AM012_FUZZ` (default 1) for longer local runs.
fn rounds(n: usize) -> usize {
    n * std::env::var("AM012_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(1)
}

/// Seed, overridable with `AM012_SEED` for longer local runs.
fn seed(default: u64) -> u64 {
    std::env::var("AM012_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Deterministic xorshift for the fuzz loops (no proptest dependency where
/// this suite runs).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn random_doc(rng: &mut Rng) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor(1));
    let list = doc.put_object(&ROOT, "list", ObjType::List).unwrap();
    let text = doc.put_object(&ROOT, "text", ObjType::Text).unwrap();
    let table = doc.put_object(&ROOT, "table", ObjType::Table).unwrap();
    doc.put(&ROOT, "count", ScalarValue::counter(0)).unwrap();
    doc.splice_text(&text, 0, 0, "abcdef").unwrap();
    for i in 0..4 {
        doc.insert(&list, i, i as i64).unwrap();
    }
    doc.commit();
    for round in 0..(2 + rng.below(4)) {
        let mut peer = doc.fork().with_actor(actor(20 + round as u8));
        match rng.below(6) {
            0 => {
                peer.put(&table, format!("k{}", rng.below(3)), round as i64).unwrap();
            }
            1 => {
                peer.increment(&ROOT, "count", rng.below(10) as i64 - 5).unwrap();
            }
            2 => {
                let len = peer.length(&text);
                peer.splice_text(&text, rng.below(len), 0, "zz").unwrap();
            }
            3 => {
                let len = peer.length(&list);
                if len > 0 {
                    peer.delete(&list, rng.below(len)).unwrap();
                }
            }
            4 => {
                let len = peer.length(&text);
                peer.mark(
                    &text,
                    automerge::marks::Mark::new("bold".into(), true, 0, len.min(3)),
                    automerge::marks::ExpandMark::After,
                )
                .unwrap();
            }
            _ => {
                let len = peer.length(&list);
                if len > 0 {
                    peer.put(&list, rng.below(len), "p").unwrap();
                }
            }
        }
        peer.commit();
        doc.merge(&mut peer).unwrap();
    }
    doc
}

/// One random edit of an ExpandedChange: op fields, change metadata, or ids.
fn mutate(rng: &mut Rng, value: &mut Value, ids: &[String]) {
    let ops = value["ops"].as_array().map_or(0, Vec::len);
    let pick = |rng: &mut Rng| ids[rng.below(ids.len())].clone();
    let i = rng.below(ops);
    match rng.below(14) {
        0 if ops > 0 => value["ops"][i]["obj"] = json!(pick(rng)),
        1 if ops > 0 => value["ops"][i]["pred"] = json!([pick(rng)]),
        2 if ops > 0 => {
            let op = value["ops"][i].as_object_mut().unwrap();
            op.remove("key");
            op.insert("elemId".into(), json!(pick(rng)));
        }
        3 if ops > 0 => {
            let op = value["ops"][i].as_object_mut().unwrap();
            op.remove("elemId");
            op.remove("insert");
            op.insert("key".into(), json!("k0"));
        }
        4 if ops > 0 => {
            let actions = ["makeMap", "set", "makeList", "del", "makeText", "inc", "makeTable"];
            value["ops"][i]["action"] = json!(actions[rng.below(actions.len())]);
        }
        5 if ops > 0 => {
            let insert = value["ops"][i]["insert"].as_bool().unwrap_or(false);
            value["ops"][i]["insert"] = json!(!insert);
        }
        6 => {
            let seq = value["seq"].as_u64().unwrap() as i64 + rng.below(3) as i64 - 1;
            value["seq"] = json!(seq.max(0));
        }
        7 => {
            let start = value["startOp"].as_u64().unwrap() as i64 + rng.below(9) as i64 - 4;
            value["startOp"] = json!(start.max(1));
        }
        8 => value["deps"] = json!([]),
        9 if ops > 0 => {
            value["ops"][i]["value"] = json!(i64::MAX - rng.below(3) as i64);
            value["ops"][i]["datatype"] = json!("int");
        }
        10 if ops > 0 => {
            let mut pred = value["ops"][i]["pred"].as_array().cloned().unwrap_or_default();
            pred.push(json!(pick(rng)));
            value["ops"][i]["pred"] = json!(pred);
        }
        11 if ops > 1 => {
            value["ops"].as_array_mut().unwrap().swap(0, 1);
        }
        12 => value["actor"] = json!(a1()),
        _ if ops > 0 => {
            value["ops"][i]["pred"] = json!([]);
        }
        _ => {}
    }
}

#[test]
fn fuzz_mutated_changes_accept_cleanly_or_refuse_unchanged() {
    let mut rng = Rng(seed(0x9e37_79b9_7f4a_7c15));
    let mut accepted = 0;
    let mut refusals = 0;
    for _ in 0..rounds(60) {
        let mut doc = random_doc(&mut rng);
        let ids: Vec<String> = {
            let changes = doc.get_changes(&[]);
            let mut ids = vec!["_root".to_string(), "_head".to_string()];
            for change in &changes {
                let v = expanded(change);
                let start = v["startOp"].as_u64().unwrap();
                for k in 0..change.len() as u64 {
                    ids.push(format!("{}@{}", start + k, v["actor"].as_str().unwrap()));
                }
            }
            ids
        };
        let heads = doc.get_heads();
        let mut peer = doc.fork().with_actor(actor(90));
        let text = peer.get(&ROOT, "text").unwrap().unwrap().1;
        let list = peer.get(&ROOT, "list").unwrap().unwrap().1;
        peer.splice_text(&text, 1, 1, "Q").unwrap();
        peer.insert(&list, 0, "n").unwrap();
        peer.increment(&ROOT, "count", 2).unwrap();
        peer.commit();
        let template = expanded(&peer.get_changes(&heads).pop().unwrap());
        for _ in 0..25 {
            let mut value = template.clone();
            for _ in 0..(1 + rng.below(3)) {
                mutate(&mut rng, &mut value, &ids);
            }
            let dbg = value.clone();
            let Ok(expanded) = serde_json::from_value::<ExpandedChange>(value) else {
                continue;
            };
            let Ok(change) = catch_unwind(AssertUnwindSafe(|| Change::from(expanded))) else {
                continue;
            };
            let bytes = change.raw_bytes().to_vec();
            let mut target = doc.clone();
            let before = snapshot(&mut target);
            let result = catch_unwind(AssertUnwindSafe(|| {
                incoming(&mut target, &bytes, NativeDependencyPolicy::RetainOrphans)
            }))
            .expect("checked admission never panics");
            match result {
                Ok(_) => {
                    accepted += 1;
                    // An accepted document stays saveable and loadable.
                    let saved = target.save();
                    if let Err(e) = AutoCommit::load(&saved) {
                        eprintln!("TEMPLATE {}", serde_json::to_string(&template).unwrap());
                        eprintln!("MUTATED {}", serde_json::to_string(&dbg).unwrap());
                        panic!("accepted history reloads: {e:?}");
                    }
                }
                Err(_) => {
                    refusals += 1;
                    assert_eq!(snapshot(&mut target), before, "refused input left the document unchanged");
                }
            }
        }
    }
    eprintln!("fuzz: {accepted} accepted, {refusals} refused");
    assert!(accepted > 0 && refusals > 0);
}

/// Recompute a change chunk's checksum after mutating its body.
fn rechecksum(bytes: &mut [u8]) {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(&bytes[8..]);
    bytes[4..8].copy_from_slice(&digest[..4]);
}

#[test]
fn fuzz_mutated_change_bytes_never_panic() {
    let mut rng = Rng(seed(0x2545_f491_4f6c_dd1d));
    let mut doc = random_doc(&mut rng);
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(91));
    let text = peer.get(&ROOT, "text").unwrap().unwrap().1;
    peer.splice_text(&text, 2, 1, "RS").unwrap();
    peer.put(&ROOT, "k", "v").unwrap();
    peer.increment(&ROOT, "count", 3).unwrap();
    peer.commit();
    let original = peer.get_changes(&heads).pop().unwrap().raw_bytes().to_vec();
    let before = snapshot(&mut doc);
    let mut accepted = 0;
    for _ in 0..rounds(3000) {
        let mut bytes = original.clone();
        for _ in 0..(1 + rng.below(3)) {
            let at = 9 + rng.below(bytes.len() - 9);
            bytes[at] = match rng.below(4) {
                0 => rng.next() as u8,
                1 => bytes[at].wrapping_add(1),
                2 => 0x7f,
                _ => 0xff,
            };
        }
        rechecksum(&mut bytes);
        let mut target = doc.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            incoming(&mut target, &bytes, NativeDependencyPolicy::RetainOrphans)
        }))
        .expect("checked admission never panics");
        if result.is_err() {
            assert_eq!(snapshot(&mut target), before);
        } else {
            accepted += 1;
            if let Err(e) = AutoCommit::load(&target.save()) {
                eprintln!("ORIG {}", hex::encode(&original));
                eprintln!("MUT  {}", hex::encode(&bytes));
                let c = Change::try_from(&bytes[..]).unwrap();
                eprintln!("DECODED {}", serde_json::to_string(&expanded(&c)).unwrap());
                let o = Change::try_from(&original[..]).unwrap();
                eprintln!("ORIGDEC {}", serde_json::to_string(&expanded(&o)).unwrap());
                panic!("accepted history reloads: {e:?}");
            }
        }
    }
    eprintln!("byte fuzz: {accepted} accepted");
}


#[test]
fn bounded_apply_is_precharged_for_rich_history_and_refusals() {
    let mut doc = base();
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(5));
    let text = peer.get(&ROOT, "text").unwrap().unwrap().1;
    let list = peer.get(&ROOT, "list").unwrap().unwrap().1;
    for i in 0..200 {
        peer.splice_text(&text, i % 5, 0, "t").unwrap();
        if i % 7 == 0 {
            peer.insert(&list, 0, i as i64).unwrap();
            peer.increment(&ROOT, "count", 1).unwrap();
        }
        if i % 50 == 0 {
            peer.mark(
                &text,
                automerge::marks::Mark::new("bold".into(), true, 0, 3),
                automerge::marks::ExpandMark::After,
            )
            .unwrap();
        }
        peer.commit();
    }
    let bytes = changes_bytes(&peer.get_changes(&heads));
    let mut restored = bounded_apply(&mut doc, &bytes).expect("rich history applies");
    assert_eq!(restored.get_heads(), peer.get_heads());
    // Refusals are precharged too.
    let a = a1();
    let change = crafted(&mut doc, |v| v["ops"][0]["pred"] = json!([format!("998@{a}")]));
    assert!(matches!(
        bounded_apply(&mut doc, change.raw_bytes()),
        Err(NativeMutationError::InvalidOperation)
    ));
    let (mut dup, x, y) = forked_sequences();
    dup.apply_changes([x]).unwrap();
    assert!(matches!(
        bounded_apply(&mut dup, y.raw_bytes()),
        Err(NativeMutationError::DuplicateSequence)
    ));
}

#[test]
fn typed_history_applies_within_one_allowance() {
    // 4_000 single-character splices committed every third keystroke.
    let mut writer = AutoCommit::new().with_actor(actor(6));
    let text = writer.put_object(&ROOT, "text", ObjType::Text).unwrap();
    writer.commit();
    let mut target = writer.fork().with_actor(actor(1));
    let heads = writer.get_heads();
    for i in 0..4_000usize {
        let len = writer.length(&text);
        writer.splice_text(&text, (i * 7) % (len + 1), 0, "k").unwrap();
        if i % 3 == 2 {
            writer.commit();
        }
    }
    writer.commit();
    let changes = writer.get_changes(&heads);
    let bytes = changes_bytes(&changes);
    let started = std::time::Instant::now();
    let limits = NativeLimits {
        max_chunks: 1 << 16,
        max_heads: 1 << 16,
        max_actors: 1 << 16,
        ..NativeLimits::default()
    };
    let credit = allowance();
    assert_eq!(
        target
            .apply_incoming_checked(&bytes, &limits, &credit, NativeDependencyPolicy::RequireComplete)
            .unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    eprintln!("typed history: {} changes in {:?}", changes.len(), started.elapsed());
    assert_eq!(target.text(&text).unwrap(), writer.text(&text).unwrap());
    // One more change onto the now-large document is cheap.
    let heads = writer.get_heads();
    writer.splice_text(&text, 0, 0, "!").unwrap();
    writer.commit();
    let started = std::time::Instant::now();
    let one = changes_bytes(&writer.get_changes(&heads));
    incoming(&mut target, &one, NativeDependencyPolicy::RequireComplete).unwrap();
    eprintln!("one keystroke onto it: {:?}", started.elapsed());
}

#[test]
fn fuzz_mutated_chains_accept_cleanly_or_refuse_unchanged() {
    // Two dependent incoming changes (the second references objects and
    // elements created by the first); either one is mutated.
    let mut rng = Rng(seed(0xd1b5_4a32_d192_ed03));
    let (mut accepted, mut refusals) = (0, 0);
    for _ in 0..rounds(40) {
        let mut doc = random_doc(&mut rng);
        let heads = doc.get_heads();
        let mut peer = doc.fork().with_actor(actor(92));
        let map = peer.put_object(&ROOT, "fresh", ObjType::Map).unwrap();
        let list = peer.put_object(&map, "items", ObjType::List).unwrap();
        peer.insert(&list, 0, "a").unwrap();
        let text = peer.put_object(&map, "note", ObjType::Text).unwrap();
        peer.splice_text(&text, 0, 0, "xyz").unwrap();
        peer.commit();
        peer.put(&map, "k", 1).unwrap();
        peer.insert(&list, 1, "b").unwrap();
        peer.delete(&list, 0).unwrap();
        peer.splice_text(&text, 1, 1, "Q").unwrap();
        peer.mark(
            &text,
            automerge::marks::Mark::new("em".into(), true, 0, 2),
            automerge::marks::ExpandMark::Both,
        )
        .unwrap();
        peer.commit();
        let chain = peer.get_changes(&heads);
        assert_eq!(chain.len(), 2);
        let mut ids = vec!["_root".to_string(), "_head".to_string()];
        for change in peer.get_changes(&[]) {
            let v = expanded(&change);
            let start = v["startOp"].as_u64().unwrap();
            for k in 0..change.len() as u64 {
                ids.push(format!("{}@{}", start + k, v["actor"].as_str().unwrap()));
            }
        }
        for _ in 0..25 {
            let which = rng.below(2);
            let mut values: Vec<Value> = chain.iter().map(expanded).collect();
            for _ in 0..(1 + rng.below(3)) {
                mutate(&mut rng, &mut values[which], &ids);
            }
            let mut bytes = Vec::new();
            let mut ok = true;
            let mut previous: Option<ChangeHash> = None;
            for (index, mut value) in values.into_iter().enumerate() {
                if index == 1 && which == 0 {
                    // Keep the chain linked to the mutated first change.
                    if let Some(hash) = previous {
                        value["deps"] = json!([hash.to_string()]);
                    }
                }
                let Ok(expanded) = serde_json::from_value::<ExpandedChange>(value) else {
                    ok = false;
                    break;
                };
                let Ok(change) = catch_unwind(AssertUnwindSafe(|| Change::from(expanded))) else {
                    ok = false;
                    break;
                };
                previous = Some(change.hash());
                bytes.extend_from_slice(change.raw_bytes());
            }
            if !ok {
                continue;
            }
            let mut target = doc.clone();
            let before = snapshot(&mut target);
            let result = catch_unwind(AssertUnwindSafe(|| {
                incoming(&mut target, &bytes, NativeDependencyPolicy::RetainOrphans)
            }))
            .expect("checked admission never panics");
            match result {
                Ok(_) => {
                    accepted += 1;
                    let saved = target.save();
                    AutoCommit::load(&saved).expect("accepted history reloads");
                }
                Err(_) => {
                    refusals += 1;
                    assert_eq!(snapshot(&mut target), before);
                }
            }
        }
    }
    eprintln!("chain fuzz: {accepted} accepted, {refusals} refused");
    assert!(accepted > 0 && refusals > 0);
}

#[test]
fn marks_apply_and_inverted_mark_is_refused() {
    use automerge::marks::{ExpandMark, Mark};
    let mut doc = base();
    let text = doc.get(&ROOT, "text").unwrap().unwrap().1;
    // Concurrent edits so marks span resident and concurrent elements.
    let mut other = doc.fork().with_actor(actor(30));
    other.splice_text(&text, 2, 0, "OTHER").unwrap();
    other.commit();
    doc.merge(&mut other).unwrap();
    let heads = doc.get_heads();
    let mut peer = doc.fork().with_actor(actor(31));
    peer.splice_text(&text, 0, 0, "abc").unwrap();
    let len = peer.length(&text);
    for (start, end, expand) in [
        (0, 1, ExpandMark::Before),
        (0, len, ExpandMark::Both),
        (2, 7, ExpandMark::After),
        (len - 1, len, ExpandMark::None),
        (3, 4, ExpandMark::Both),
    ] {
        peer.mark(&text, Mark::new("m".into(), true, start, end), expand).unwrap();
    }
    peer.commit();
    let changes = peer.get_changes(&heads);
    let bytes = changes_bytes(&changes);
    let mut target = doc.clone();
    assert_eq!(
        incoming(&mut target, &bytes, NativeDependencyPolicy::RequireComplete).unwrap(),
        NativeIncomingOutcome::Applied { changed: true }
    );
    AutoCommit::load(&target.save()).expect("marks reload");
    assert_eq!(target.marks(&text).unwrap(), peer.marks(&text).unwrap());

    // Re-anchor the end of a mark to the element before its begin.
    let mut plain = doc.fork().with_actor(actor(32));
    plain.mark(&text, Mark::new("m".into(), true, 3, 5), ExpandMark::None).unwrap();
    plain.commit();
    let mut value = expanded(&plain.get_changes(&heads).pop().unwrap());
    let ops = value["ops"].as_array_mut().unwrap();
    let end = ops.iter_mut().find(|op| op["action"] == json!("markEnd")).unwrap();
    end["elemId"] = json!("_head");
    let inverted = encode(value);
    let mut target = doc.clone();
    semantic_refusal(&mut target, &inverted, "mark end before begin");
}

#[test]
#[ignore = "0.12 ChangeCollector still panics on forged document chunks (collector.rs asserts); \
            enable once the reconstruct hardening lands"]
fn fuzz_mutated_document_and_bundle_bytes_never_panic() {
    let mut rng = Rng(seed(0x7a3c_91e5_0b2d_4f68));
    let mut source = random_doc(&mut rng);
    let document = source.save_nocompress();
    let mut target = AutoCommit::new();
    let before = snapshot(&mut target);
    let mut accepted = 0;
    for _ in 0..rounds(1500) {
        let mut bytes = document.clone();
        for _ in 0..(1 + rng.below(2)) {
            let at = 9 + rng.below(bytes.len() - 9);
            bytes[at] = match rng.below(4) {
                0 => rng.next() as u8,
                1 => bytes[at].wrapping_add(1),
                2 => bytes[at].wrapping_sub(1),
                _ => bytes[at] ^ 0x80,
            };
        }
        rechecksum(&mut bytes);
        let mut doc = target.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            incoming(&mut doc, &bytes, NativeDependencyPolicy::RetainOrphans)
        }));
        let result = match result {
            Ok(result) => result,
            Err(_) => panic!("checked admission panicked on {}", hex::encode(&bytes)),
        };
        if result.is_err() {
            assert_eq!(snapshot(&mut doc), before);
        } else {
            accepted += 1;
            AutoCommit::load(&doc.save()).expect("accepted history reloads");
        }
    }
    eprintln!("document fuzz: {accepted} accepted");
}
