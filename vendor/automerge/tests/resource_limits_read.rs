//! Bounded native reads (`NativeReadSession`) against the public read API.
//!
//! Every visit is compared with `get`/`get_all`/`keys`/`list_range`/`text`/
//! `get_heads` and `ExId` encodings on randomized documents with conflicts,
//! counters, marks and Table objects, while a counting global allocator
//! asserts that each heap allocation made during a visit was reserved on the
//! current turn first. Also covers the Table and wrapping-counter parity
//! patches that ship with the 0.12 port (legacy reads wrap; bounded reads
//! refuse an unrepresentable counter sum as Invalid, the 0.8 contract).
use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeCloneCandidate, NativeId, NativeObject,
    NativeReadError, NativeReadSession, NativeReadSteps, NativeResourceError, NativeScalar,
    NativeSelection, NativeValue,
};
use automerge::transaction::Transactable;
use automerge::{
    ActorId, AutoCommit, ObjId, ObjType, PatchAction, ReadDoc, ScalarValue, ScalarValueRef, Value,
    ValueRef, ROOT,
};
use std::ops::ControlFlow;

// ── Allocation harness ─────────────────────────────────────────────────────
//
// Same technique as the qos-peer-state `assert_precharged` harness: while a
// check is active, every allocation on this thread must fit inside what the
// current turn has reserved since the check began (moving reallocations
// count the old/new overlap).
mod harness {
    use automerge::resource_limits::NativeAllowance;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::{Cell, RefCell};

    #[derive(Clone, Copy, Default, Debug)]
    pub struct Allocations {
        live: usize,
        pub peak: usize,
        unmatched_free: bool,
        unreserved_peak: usize,
        first_unreserved: Option<(usize, usize, usize)>,
    }

    thread_local! {
        static METER: Cell<Option<Allocations>> = const { Cell::new(None) };
        static CHECK: RefCell<Option<(NativeAllowance, usize)>> = const { RefCell::new(None) };
    }

    fn allocated(bytes: usize) {
        let _ = METER.try_with(|meter| {
            if let Some(mut state) = meter.get() {
                state.live += bytes;
                state.peak = state.peak.max(state.live);
                let _ = CHECK.try_with(|check| {
                    if let Ok(check) = check.try_borrow() {
                        if let Some((allowance, initial)) = &*check {
                            let reserved = initial.saturating_sub(allowance.available_bytes());
                            if state.live > reserved && state.first_unreserved.is_none() {
                                state.first_unreserved = Some((bytes, state.live, reserved));
                            }
                            state.unreserved_peak = state
                                .unreserved_peak
                                .max(state.live.saturating_sub(reserved));
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
                match state.live.checked_sub(bytes) {
                    Some(live) => state.live = live,
                    None => state.unmatched_free = true,
                }
                meter.set(Some(state));
            }
        });
    }

    struct Measured;
    // SAFETY: every operation delegates unchanged to System; the counters are
    // Copy thread-locals and never allocate.
    unsafe impl GlobalAlloc for Measured {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                allocated(layout.size());
            }
            pointer
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() {
                allocated(layout.size());
            }
            pointer
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            released(layout.size());
            unsafe { System.dealloc(pointer, layout) };
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let grown = unsafe { System.realloc(pointer, layout, size) };
            if !grown.is_null() {
                allocated(size);
                released(layout.size());
            }
            grown
        }
    }

    #[global_allocator]
    static ALLOCATOR: Measured = Measured;

    /// Runs `operation` and returns its value plus what it allocated.
    pub fn measure<T>(operation: impl FnOnce() -> T) -> (T, Allocations) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                METER.with(|meter| meter.set(None));
            }
        }
        METER.with(|meter| {
            assert!(meter.get().is_none(), "nested measurement");
            meter.set(Some(Allocations::default()));
        });
        let _reset = Reset;
        let value = operation();
        let allocations = METER.with(|meter| meter.replace(None).unwrap());
        assert!(
            !allocations.unmatched_free,
            "the operation freed an allocation made before it started"
        );
        (value, allocations)
    }

    /// Asserts every allocation of `operation` was reserved on `allowance`
    /// before it happened; returns the operation's value.
    pub fn assert_precharged<T>(allowance: &NativeAllowance, operation: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                CHECK.with(|check| *check.borrow_mut() = None);
            }
        }
        CHECK.with(|check| {
            assert!(check.borrow().is_none());
            *check.borrow_mut() = Some((allowance.clone(), allowance.available_bytes()));
        });
        let _reset = Reset;
        let (value, allocations) = measure(operation);
        assert_eq!(
            allocations.unreserved_peak, 0,
            "unreserved allocation (bytes, live, reserved): {:?}",
            allocations.first_unreserved
        );
        value
    }
}
use harness::assert_precharged;

// ── Fixtures ───────────────────────────────────────────────────────────────

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

fn candidate(
    doc: &AutoCommit,
    allocations: &mut NativeAllocationSet,
) -> NativeCloneCandidate<AutoCommit> {
    let turn = allocations.work_turn(allowance()).unwrap();
    doc.clone_bounded(allocations, &turn).unwrap()
}

/// Deterministic xorshift generator; the test needs no external RNG API.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn random_scalar(rng: &mut Rng) -> ScalarValue {
    match rng.below(11) {
        0 => ScalarValue::Bytes((0..rng.below(40)).map(|i| i as u8 ^ 0xa5).collect()),
        1 => ScalarValue::Str(["", "x", "héllo", "a\n\"\\☺", "🦀界"][rng.below(5)].into()),
        2 => ScalarValue::Int([0, -1, i64::MIN, i64::MAX, 42][rng.below(5)]),
        3 => ScalarValue::Uint([0, 7, u64::MAX][rng.below(3)]),
        4 => ScalarValue::F64([0.0, -0.0, 1.5, f64::INFINITY, f64::NAN][rng.below(5)]),
        // Representable sums only: the bounded reader refuses overflow (see
        // counter_overflow_wraps_in_legacy_reads_and_is_refused_by_bounded_reads).
        5 => ScalarValue::counter([0, 5, -3, 1 << 40, -(1 << 40)][rng.below(5)]),
        6 => ScalarValue::Timestamp(rng.next() as i64),
        7 => ScalarValue::Boolean(rng.chance(50)),
        8 => ScalarValue::Unknown {
            type_code: 13 + rng.below(3) as u8,
            bytes: vec![1, 2, rng.next() as u8],
        },
        9 => ScalarValue::Str("long-".repeat(rng.below(200)).into()),
        _ => ScalarValue::Null,
    }
}

const KEYS: [&str; 6] = ["a", "b", "c", "dd", "é", "zz"];

fn random_op(peer: &mut AutoCommit, objects: &mut Vec<(ObjId, ObjType)>, rng: &mut Rng) {
    let (obj, kind) = objects[rng.below(objects.len())].clone();
    let increment = [1, -1, 7, 1 << 20, -(1 << 20)][rng.below(5)];
    match kind {
        ObjType::Map | ObjType::Table => {
            let key = KEYS[rng.below(KEYS.len())];
            match rng.below(10) {
                0..=4 => {
                    let _ = peer.put(&obj, key, random_scalar(rng));
                }
                5 if objects.len() < 24 => {
                    let kind =
                        [ObjType::Map, ObjType::Table, ObjType::List, ObjType::Text][rng.below(4)];
                    if let Ok(id) = peer.put_object(&obj, key, kind) {
                        objects.push((id, kind));
                    }
                }
                6 | 7 => {
                    let _ = peer.increment(&obj, key, increment);
                }
                _ => {
                    let _ = peer.delete(&obj, key);
                }
            }
        }
        ObjType::List => {
            let len = peer.length(&obj);
            match rng.below(10) {
                0..=3 => {
                    let _ = peer.insert(&obj, rng.below(len + 1), random_scalar(rng));
                }
                4 if len > 0 => {
                    let _ = peer.put(&obj, rng.below(len), random_scalar(rng));
                }
                5 if objects.len() < 24 => {
                    let kind = [ObjType::Map, ObjType::Table, ObjType::List][rng.below(3)];
                    if let Ok(id) = peer.insert_object(&obj, rng.below(len + 1), kind) {
                        objects.push((id, kind));
                    }
                }
                6 | 7 if len > 0 => {
                    let _ = peer.increment(&obj, rng.below(len), increment);
                }
                8 if len > 0 => {
                    let _ = peer.delete(&obj, rng.below(len));
                }
                _ => {
                    let _ = peer.insert(&obj, rng.below(len + 1), ScalarValue::counter(1));
                }
            }
        }
        ObjType::Text => {
            let len = peer.length(&obj);
            match rng.below(8) {
                0..=3 => {
                    let text = ["ab", "é", "🦀", "xyz", "界"][rng.below(5)];
                    let _ = peer.splice_text(&obj, rng.below(len + 1), 0, text);
                }
                4 if len > 0 => {
                    let _ = peer.splice_text(&obj, rng.below(len), 1, "");
                }
                5 if len > 1 => {
                    let start = rng.below(len - 1);
                    let end = start + 1 + rng.below(len - start - 1);
                    let value = match rng.below(3) {
                        0 => ScalarValue::Boolean(true),
                        1 => ScalarValue::Str("mark text".into()),
                        _ => ScalarValue::Null,
                    };
                    let name = ["bold", "link", "font"][rng.below(3)];
                    let expand =
                        [ExpandMark::Both, ExpandMark::None, ExpandMark::After][rng.below(3)];
                    let _ = peer.mark(&obj, Mark::new(name.into(), value, start, end), expand);
                }
                6 if len > 0 => {
                    let _ = peer.split_block(&obj, rng.below(len));
                }
                _ => {
                    let _ = peer.splice_text(&obj, len, 0, "tail");
                }
            }
        }
    }
}

fn random_doc(seed: u64) -> AutoCommit {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut base = AutoCommit::new().with_actor(ActorId::from(vec![0x10, seed as u8]));
    let mut objects = vec![(ROOT, ObjType::Map)];
    for (key, kind) in [
        ("map", ObjType::Map),
        ("table", ObjType::Table),
        ("list", ObjType::List),
        ("text", ObjType::Text),
    ] {
        let id = base.put_object(ROOT, key, kind).unwrap();
        objects.push((id, kind));
    }
    base.commit();
    let mut peers: Vec<AutoCommit> = (0..3u8)
        .map(|i| {
            base.fork()
                .with_actor(ActorId::from(vec![0x20 + i, seed as u8, 0xff]))
        })
        .collect();
    for _round in 0..6 {
        for peer in peers.iter_mut() {
            for _ in 0..8 {
                random_op(peer, &mut objects, &mut rng);
            }
            peer.commit();
        }
        let a = rng.below(peers.len());
        let b = rng.below(peers.len());
        if a != b {
            let mut other = peers[b].clone();
            peers[a].merge(&mut other).unwrap();
        }
    }
    for peer in peers.iter_mut() {
        base.merge(peer).unwrap();
    }
    base.commit();
    base
}

// ── Parity oracle ──────────────────────────────────────────────────────────

struct Checker<'r, 'doc, 'turn> {
    read: &'r NativeReadSession<'doc, 'turn>,
    doc: &'r AutoCommit,
    current: &'r NativeAllowance,
    maps: usize,
    lists: usize,
}

impl<'doc> Checker<'_, 'doc, '_> {
    /// Identity parity: textual, binary and semantic parts all agree with ExId.
    fn check_id(&self, id: NativeId<'doc>, expected: &ObjId, text: &str, bytes: &[u8]) {
        let read = self.read;
        assert_eq!(read.id_len(id).unwrap(), text.len());
        let mut buffer = [0u8; 1024];
        let len = read.write_id(id, &mut buffer).unwrap();
        assert_eq!(&buffer[..len], text.as_bytes());
        assert_eq!(read.id_bytes_len(id).unwrap(), bytes.len());
        let len = read.write_id_bytes(id, &mut buffer).unwrap();
        assert_eq!(&buffer[..len], bytes);
        read.visit_id_parts(id, |parts| match (parts, expected) {
            (None, ObjId::Root) => {}
            (Some((counter, actor)), ObjId::Id(c, a, _)) => {
                assert_eq!(counter, *c);
                assert_eq!(actor, a.to_bytes());
            }
            other => panic!("id parts mismatch {other:?}"),
        })
        .unwrap();
    }

    fn check_object(&mut self, object: NativeObject<'doc>, exid: &ObjId) {
        match self.doc.object_type(exid).unwrap() {
            ObjType::Map | ObjType::Table => self.check_map(object, exid),
            ObjType::List | ObjType::Text => self.check_list(object, exid),
        }
    }

    fn check_map(&mut self, object: NativeObject<'doc>, exid: &ObjId) {
        self.maps += 1;
        let read = self.read;
        let keys: Vec<String> = self.doc.keys(exid).collect();
        let mut seen = 0;
        let count = assert_precharged(self.current, || {
            read.visit_keys(object, |key| {
                assert_eq!(key, keys[seen]);
                seen += 1;
                Ok::<_, ()>(())
            })
        })
        .unwrap();
        assert_eq!((count, seen), (keys.len(), keys.len()));
        // Stepped variant: exactly the same keys; stopping after the first.
        if !keys.is_empty() {
            let steps = NativeReadSteps::new(1_000_000).unwrap();
            let count = assert_precharged(self.current, || {
                read.visit_keys_with_steps(object, &steps, |key| {
                    assert_eq!(key, keys[0]);
                    Ok::<_, ()>(ControlFlow::Break(()))
                })
            })
            .unwrap();
            assert_eq!(count, 1);
        }
        for key in &keys {
            let all = self.doc.get_all(exid, key.as_str()).unwrap();
            let expected: Vec<_> = all
                .iter()
                .map(|(value, id)| (value.clone(), id.clone(), id.to_string(), id.to_bytes()))
                .collect();
            let mut children = Vec::with_capacity(expected.len());
            let mut index = 0;
            let count = assert_precharged(self.current, || {
                read.visit_property(object, key, NativeSelection::All, |value, id| {
                    let (value_expected, exid, text, bytes) = &expected[index];
                    assert_value(value, value_expected);
                    self.check_id(id, exid, text, bytes);
                    if let NativeValue::Object { object, .. } = value {
                        assert_eq!(object.id(), id);
                        children.push((object, index));
                    }
                    index += 1;
                    Ok::<_, ()>(())
                })
            })
            .unwrap();
            assert_eq!(count, expected.len());
            let winner = self
                .doc
                .get(exid, key.as_str())
                .unwrap()
                .map(|(value, id)| (value, id.to_string(), id.to_bytes(), id));
            let steps = NativeReadSteps::new(1_000_000).unwrap();
            let mut called = 0;
            let count = assert_precharged(self.current, || {
                read.visit_property_with_steps(
                    object,
                    key,
                    NativeSelection::Winner,
                    &steps,
                    |value, id| {
                        let (value_expected, text, bytes, exid) = winner.as_ref().unwrap();
                        assert_value(value, value_expected);
                        self.check_id(id, exid, text, bytes);
                        called += 1;
                        Ok::<_, ()>(())
                    },
                )
            })
            .unwrap();
            assert_eq!((count, called), (winner.is_some() as usize, count));
            for (child, index) in children {
                self.check_object(child, &expected[index].1);
            }
        }
        // A key that is absent visits nothing.
        let count = read
            .visit_property(object, "absent-key", NativeSelection::All, |_, _| {
                Err::<(), _>("absent key visited")
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    fn check_list(&mut self, object: NativeObject<'doc>, exid: &ObjId) {
        self.lists += 1;
        let read = self.read;
        let expected: Vec<_> = self
            .doc
            .list_range(exid, ..)
            .map(|item| {
                let item = item.into_owned();
                let id = item.id();
                (
                    item.index,
                    item.is_mark(),
                    item.conflict,
                    id.to_string(),
                    id.to_bytes(),
                    id,
                    item.value,
                )
            })
            .collect();
        let mut children = Vec::with_capacity(expected.len());
        let is_text = self.doc.object_type(exid).unwrap() == ObjType::Text;
        let expected_text = if is_text {
            self.doc.text(exid).unwrap()
        } else {
            String::new()
        };
        let mut text = String::with_capacity(expected_text.len());
        let mut seen = 0;
        let steps = NativeReadSteps::new(1_000_000).unwrap();
        let count = assert_precharged(self.current, || {
            read.visit_list(object, &steps, |entry| {
                let (index, is_mark, conflict, text_id, bytes, exid, value) = &expected[seen];
                assert_eq!(
                    (entry.index, entry.is_mark, entry.conflict),
                    (*index, *is_mark, *conflict),
                    "list entry {seen}"
                );
                self.check_id(entry.id, exid, text_id, bytes);
                assert_value_ref(entry.value, value);
                if is_text && !entry.is_mark {
                    match entry.value {
                        NativeValue::Scalar(NativeScalar::Str(s)) => text.push_str(s),
                        _ => text.push('\u{fffc}'),
                    }
                }
                if let NativeValue::Object { object, .. } = entry.value {
                    children.push((object, seen));
                }
                seen += 1;
                Ok::<_, ()>(ControlFlow::Continue(()))
            })
        })
        .unwrap();
        assert_eq!((count, seen), (expected.len(), expected.len()));
        assert_eq!(text, expected_text);
        for (child, index) in children {
            self.check_object(child, &expected[index].5);
        }
    }
}

fn assert_scalar(actual: NativeScalar<'_>, expected: &ScalarValue) {
    match (actual, expected) {
        (NativeScalar::Bytes(a), ScalarValue::Bytes(e)) => assert_eq!(a, e.as_slice()),
        (NativeScalar::Str(a), ScalarValue::Str(e)) => assert_eq!(a, e.as_str()),
        (NativeScalar::Int(a), ScalarValue::Int(e)) => assert_eq!(a, *e),
        (NativeScalar::Uint(a), ScalarValue::Uint(e)) => assert_eq!(a, *e),
        (NativeScalar::F64(a), ScalarValue::F64(e)) => assert_eq!(a.to_bits(), e.to_bits()),
        (NativeScalar::Counter(a), ScalarValue::Counter(e)) => assert_eq!(a, i64::from(e)),
        (NativeScalar::Timestamp(a), ScalarValue::Timestamp(e)) => assert_eq!(a, *e),
        (NativeScalar::Boolean(a), ScalarValue::Boolean(e)) => assert_eq!(a, *e),
        (
            NativeScalar::Unknown { type_code, bytes },
            ScalarValue::Unknown {
                type_code: e_code,
                bytes: e_bytes,
            },
        ) => assert_eq!((type_code, bytes), (*e_code, e_bytes.as_slice())),
        (NativeScalar::Null, ScalarValue::Null) => {}
        other => panic!("scalar mismatch {other:?}"),
    }
}

fn assert_value(actual: NativeValue<'_, '_>, expected: &Value<'_>) {
    match (actual, expected) {
        (NativeValue::Object { kind, .. }, Value::Object(e)) => assert_eq!(kind, *e),
        (NativeValue::Scalar(a), Value::Scalar(e)) => assert_scalar(a, e),
        other => panic!("value mismatch {other:?}"),
    }
}

fn assert_value_ref(actual: NativeValue<'_, '_>, expected: &ValueRef<'_>) {
    match (actual, expected) {
        (NativeValue::Object { kind, .. }, ValueRef::Object(e)) => assert_eq!(kind, *e),
        (NativeValue::Scalar(a), ValueRef::Scalar(e)) => match (a, e) {
            (NativeScalar::Bytes(a), ScalarValueRef::Bytes(e)) => assert_eq!(a, e.as_ref()),
            (NativeScalar::Str(a), ScalarValueRef::Str(e)) => assert_eq!(a, e.as_ref()),
            (NativeScalar::Int(a), ScalarValueRef::Int(e)) => assert_eq!(a, *e),
            (NativeScalar::Uint(a), ScalarValueRef::Uint(e)) => assert_eq!(a, *e),
            (NativeScalar::F64(a), ScalarValueRef::F64(e)) => assert_eq!(a.to_bits(), e.to_bits()),
            (NativeScalar::Counter(a), ScalarValueRef::Counter(e)) => assert_eq!(a, *e),
            (NativeScalar::Timestamp(a), ScalarValueRef::Timestamp(e)) => assert_eq!(a, *e),
            (NativeScalar::Boolean(a), ScalarValueRef::Boolean(e)) => assert_eq!(a, *e),
            (
                NativeScalar::Unknown { type_code, bytes },
                ScalarValueRef::Unknown {
                    type_code: e_code,
                    bytes: e_bytes,
                },
            ) => assert_eq!((type_code, bytes), (*e_code, e_bytes.as_ref())),
            (NativeScalar::Null, ScalarValueRef::Null) => {}
            other => panic!("list scalar mismatch {other:?}"),
        },
        other => panic!("list value mismatch {other:?}"),
    }
}

/// `doc` must have no open transaction; `&mut` only because
/// `AutoCommit::get_heads` takes it.
fn check_parity(doc: &mut AutoCommit) -> (usize, usize) {
    assert_eq!(doc.pending_ops(), 0);
    let heads = doc.get_heads();
    let doc = &*doc;
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let source = candidate(doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let initial = current.available_bytes();
    let mut checker = Checker {
        read: &read,
        doc,
        current: &current,
        maps: 0,
        lists: 0,
    };
    checker.check_id(read.root().id(), &ROOT, "_root", &[0]);
    checker.check_object(read.root(), &ROOT);
    let (maps, lists) = (checker.maps, checker.lists);

    // Heads: complete, sorted like get_heads, capped before any callback.
    let mut got = Vec::with_capacity(heads.len());
    let count = assert_precharged(&current, || {
        read.visit_heads(heads.len(), |hash| got.push(hash))
    })
    .unwrap();
    assert_eq!((count, &got), (heads.len(), &heads));
    if !heads.is_empty() {
        assert_eq!(
            read.visit_heads(heads.len() - 1, |_| panic!("prefix escaped")),
            Err(NativeResourceError::Collection)
        );
    }
    assert_eq!(read.operation_count() as u64, doc.stats().num_ops);
    assert_eq!(read.pending_ops(), 0);
    // Every query released its workspace credit.
    assert_eq!(current.available_bytes(), initial);
    (maps, lists)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn randomized_documents_match_public_read_api() {
    let (mut maps, mut lists) = (0, 0);
    for seed in 1..=40 {
        let mut doc = random_doc(seed);
        let (m, l) = check_parity(&mut doc);
        maps += m;
        lists += l;
    }
    // The generator really exercised nested maps, tables, lists and text.
    assert!(maps > 100 && lists > 80, "maps={maps} lists={lists}");
}

#[test]
fn randomized_documents_survive_save_load_roundtrip_parity() {
    for seed in 100..110 {
        let mut doc = random_doc(seed);
        let mut loaded = AutoCommit::load(&doc.save()).unwrap();
        check_parity(&mut loaded);
        // And the loaded document still accepts further local edits.
        loaded.put(ROOT, "after-load", 1).unwrap();
        loaded.commit();
        check_parity(&mut loaded);
    }
}

#[test]
fn conflicts_winner_and_all_follow_native_order() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1]));
    doc.put(ROOT, "title", "base").unwrap();
    doc.put(ROOT, "removed", true).unwrap();
    doc.commit();
    let mut left = doc.fork().with_actor(ActorId::from(vec![2]));
    let mut right = doc.fork().with_actor(ActorId::from(vec![3]));
    left.put(ROOT, "title", "left").unwrap();
    right.put(ROOT, "title", "right").unwrap();
    left.delete(ROOT, "removed").unwrap();
    doc.merge(&mut left).unwrap();
    doc.merge(&mut right).unwrap();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let source = candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let mut order = Vec::new();
    let count = read
        .visit_property(read.root(), "title", NativeSelection::All, |value, _| {
            if let NativeValue::Scalar(NativeScalar::Str(text)) = value {
                order.push(text.to_owned());
            }
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(
        (count, order),
        (2, vec!["left".to_owned(), "right".to_owned()])
    );
    read.visit_property(read.root(), "title", NativeSelection::Winner, |value, _| {
        assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("right")));
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(
        read.visit_property(read.root(), "removed", NativeSelection::All, |_, _| {
            Err::<(), _>("deleted")
        }),
        Ok(0)
    );
    let mut calls = 0;
    assert_eq!(
        read.visit_property(read.root(), "title", NativeSelection::All, |_, _| {
            calls += 1;
            Err("stop")
        }),
        Err(NativeReadError::Visitor("stop"))
    );
    assert_eq!(calls, 1);
}

#[test]
fn step_exhaustion_is_an_operations_refusal_and_steps_never_refresh() {
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    for i in 0..100 {
        doc.insert(&list, i, i as u64).unwrap();
    }
    let map = doc.put_object(ROOT, "map", ObjType::Map).unwrap();
    for i in 0..50 {
        doc.put(&map, format!("k{i:02}"), i as u64).unwrap();
    }
    doc.put(ROOT, "counter", ScalarValue::counter(0)).unwrap();
    for i in 1..=64 {
        doc.increment(ROOT, "counter", i).unwrap();
    }
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let source = candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let before = current.available_bytes();
    let object = |key: &str| {
        let mut found = None;
        read.visit_property(read.root(), key, NativeSelection::Winner, |value, _| {
            if let NativeValue::Object { object, .. } = value {
                found = Some(object);
            }
            Ok::<_, ()>(())
        })
        .unwrap();
        found.unwrap()
    };
    let (list, map) = (object("list"), object("map"));

    // A stop consumes only a prefix of the budget.
    let steps = NativeReadSteps::new(10).unwrap();
    let count = read
        .visit_list(list, &steps, |entry| {
            assert_eq!(entry.index, 0);
            Ok::<_, ()>(ControlFlow::Break(()))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(steps.remaining() > 0 && steps.remaining() < 10);

    // A full traversal needs at least one step per op visited.
    for budget in [1, 2, 50, 100] {
        let steps = NativeReadSteps::new(budget).unwrap();
        assert!(matches!(
            read.visit_list(list, &steps, |_| Ok::<_, ()>(ControlFlow::Continue(()))),
            Err(NativeReadError::Resource(NativeResourceError::Operations))
        ));
        let steps = NativeReadSteps::new(budget.min(50)).unwrap();
        assert!(matches!(
            read.visit_keys_with_steps(map, &steps, |_| Ok::<_, ()>(ControlFlow::Continue(()))),
            Err(NativeReadError::Resource(NativeResourceError::Operations))
        ));
    }
    let steps = NativeReadSteps::new(102).unwrap();
    assert_eq!(
        read.visit_list(list, &steps, |_| Ok::<_, ()>(ControlFlow::Continue(())))
            .unwrap(),
        100
    );

    // Counter successor increments are charged: 1 query + 65 ops in the key
    // range (the counter and its 64 increment ops) + 64 visibility + 64 value
    // steps; anything short of that is refused.
    let steps = NativeReadSteps::new(193).unwrap();
    assert!(matches!(
        read.visit_property_with_steps(
            read.root(),
            "counter",
            NativeSelection::Winner,
            &steps,
            |_, _| Ok::<_, ()>(())
        ),
        Err(NativeReadError::Resource(NativeResourceError::Operations))
    ));
    let steps = NativeReadSteps::new(194).unwrap();
    read.visit_property_with_steps(
        read.root(),
        "counter",
        NativeSelection::Winner,
        &steps,
        |value, _| {
            assert_eq!(
                value,
                NativeValue::Scalar(NativeScalar::Counter(64 * 65 / 2))
            );
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    assert_eq!(steps.remaining(), 0);

    // Exhausted shared steps refuse nested and later queries before visiting.
    let steps = NativeReadSteps::new(5).unwrap();
    steps.charge(5).unwrap();
    assert!(matches!(
        read.visit_list(list, &steps, |_| -> Result<ControlFlow<()>, ()> {
            panic!("exhausted budget reached the visitor")
        }),
        Err(NativeReadError::Resource(NativeResourceError::Operations))
    ));
    // Visitor errors and unwinds still release workspace credit.
    let enough = NativeReadSteps::new(1000).unwrap();
    assert_eq!(
        read.visit_list(list, &enough, |_| Err::<ControlFlow<()>, _>(17)),
        Err(NativeReadError::Visitor(17))
    );
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read.visit_list(list, &enough, |_| -> Result<ControlFlow<()>, ()> {
            panic!("intentional visitor unwind")
        })
    }));
    assert!(unwind.is_err());
    assert_eq!(current.available_bytes(), before);

    assert_eq!(
        NativeReadSteps::new(0).unwrap_err(),
        NativeResourceError::Operations
    );
    assert_eq!(
        NativeReadSteps::new(1_000_001).unwrap_err(),
        NativeResourceError::Operations
    );
    assert!(NativeReadSteps::new(1_000_000).is_ok());
}

#[test]
fn cross_document_objects_and_ids_are_refused_before_sinks_or_callbacks() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![9; 128]));
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, "one").unwrap();
    doc.put_object(ROOT, "map", ObjType::Map).unwrap();
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let a = candidate(&doc, &mut allocations);
    let b = candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = a.read_unqualified(&turn).unwrap();
    let foreign = b.read_unqualified(&turn).unwrap();
    assert_ne!(read.root(), foreign.root());
    assert_ne!(read.root().id(), foreign.root().id());
    let mut foreign_list = None;
    foreign
        .visit_property(
            foreign.root(),
            "list",
            NativeSelection::Winner,
            |value, _| {
                if let NativeValue::Object { object, .. } = value {
                    foreign_list = Some(object);
                }
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    let foreign_list = foreign_list.unwrap();
    let invalid = NativeReadError::Resource(NativeResourceError::Invalid);
    let steps = NativeReadSteps::new(1000).unwrap();
    for object in [foreign.root(), foreign_list] {
        assert_precharged(&current, || {
            assert_eq!(
                read.visit_keys(object, |_| Err::<(), _>("callback")),
                Err(invalid.clone_like())
            );
            assert_eq!(
                read.visit_property(object, "map", NativeSelection::All, |_, _| Err::<(), _>(
                    "callback"
                )),
                Err(invalid.clone_like())
            );
            assert!(matches!(
                read.visit_list(object, &steps, |_| Err::<ControlFlow<()>, _>("callback")),
                Err(NativeReadError::Resource(NativeResourceError::Invalid))
            ));
            let id = object.id();
            assert_eq!(read.id_len(id), Err(NativeResourceError::Invalid));
            assert_eq!(read.id_bytes_len(id), Err(NativeResourceError::Invalid));
            let mut sink = [0x33; 600];
            assert_eq!(
                read.write_id(id, &mut sink),
                Err(NativeResourceError::Invalid)
            );
            assert_eq!(
                read.write_id_bytes(id, &mut sink),
                Err(NativeResourceError::Invalid)
            );
            assert!(sink.iter().all(|b| *b == 0x33));
            let mut called = false;
            assert_eq!(
                read.visit_id_parts(id, |_| called = true),
                Err(NativeResourceError::Invalid)
            );
            assert!(!called);
        });
    }
    // Wrong kinds: the root is not a sequence; a list is not a map.
    let mut own_list = None;
    read.visit_property(read.root(), "list", NativeSelection::Winner, |value, _| {
        if let NativeValue::Object { object, .. } = value {
            own_list = Some(object);
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    assert!(matches!(
        read.visit_list(read.root(), &steps, |_| Ok::<_, ()>(ControlFlow::Continue(
            ()
        ))),
        Err(NativeReadError::Resource(NativeResourceError::Invalid))
    ));
    assert_eq!(
        read.visit_keys(own_list.unwrap(), |_| Ok::<_, ()>(())),
        Err(NativeReadError::Resource(NativeResourceError::Invalid))
    );

    // Identity details for a 128-byte actor.
    read.visit_property(read.root(), "map", NativeSelection::Winner, |_, id| {
        assert_eq!(read.id_len(id).unwrap(), 258);
        let mut short = [0x55; 257];
        assert_eq!(
            read.write_id(id, &mut short),
            Err(NativeResourceError::WorkBytes)
        );
        assert_eq!(short, [0x55; 257]);
        let mut bytes = [0; 258];
        assert_eq!(read.write_id(id, &mut bytes).unwrap(), 258);
        assert!(bytes.starts_with(b"3@"));
        assert!(bytes[2..].chunks_exact(2).all(|pair| pair == b"09"));
        let len = read.id_bytes_len(id).unwrap();
        let mut short = vec![0x55; len - 1];
        assert_eq!(
            read.write_id_bytes(id, &mut short),
            Err(NativeResourceError::WorkBytes)
        );
        assert!(short.iter().all(|b| *b == 0x55));
        Ok::<_, ()>(())
    })
    .unwrap();
    let mut root = [0; 5];
    read.write_id(read.root().id(), &mut root).unwrap();
    assert_eq!(&root, b"_root");
}

trait CloneLike {
    fn clone_like(&self) -> Self;
}
impl<E> CloneLike for NativeReadError<E> {
    fn clone_like(&self) -> Self {
        match self {
            NativeReadError::Resource(NativeResourceError::Invalid) => {
                NativeReadError::Resource(NativeResourceError::Invalid)
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn sessions_refuse_isolation_foreign_domains_and_short_turns() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "nonempty").unwrap();
    let old = doc.get_heads();
    doc.commit();
    let heads = doc.get_heads();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let source = candidate(&doc, &mut allocations);

    // Foreign owner domain.
    let foreign = NativeAllocationSet::new(allowance()).unwrap();
    let foreign_turn = foreign.work_turn(allowance()).unwrap();
    assert!(matches!(
        source.read_unqualified(&foreign_turn),
        Err(NativeResourceError::Unaccounted)
    ));

    // Isolated documents.
    let turn = allocations.work_turn(allowance()).unwrap();
    let mut isolated = doc.clone();
    isolated.isolate(&heads);
    let isolated = candidate(&isolated, &mut allocations);
    assert!(matches!(
        isolated.read_unqualified(&turn),
        Err(NativeResourceError::Unaccounted)
    ));
    let _ = old;

    // A turn too small for the workspace refuses before any callback.
    let tiny = NativeAllowance::new(64).unwrap();
    let turn = allocations.work_turn(tiny.clone()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let mut calls = 0;
    assert_precharged(&tiny, || {
        assert_eq!(
            read.visit_property(read.root(), "value", NativeSelection::All, |_, _| {
                calls += 1;
                Ok::<_, ()>(())
            }),
            Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
        );
        assert_eq!(
            read.visit_keys(read.root(), |_| {
                calls += 1;
                Ok::<_, ()>(())
            }),
            Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
        );
    });
    assert_eq!(calls, 0);

    // Nested queries each hold their own workspace credit; exhausting the
    // turn refuses the nested one, and everything is released afterwards.
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let initial = current.available_bytes();
    read.visit_keys(read.root(), |key| {
        assert!(current.available_bytes() < initial);
        let rest = current.reserve(current.available_bytes()).unwrap();
        assert_eq!(
            read.visit_property(read.root(), key, NativeSelection::Winner, |_, _| {
                Ok::<_, ()>(())
            }),
            Err(NativeReadError::Resource(NativeResourceError::WorkBytes))
        );
        drop(rest);
        read.visit_property(read.root(), key, NativeSelection::Winner, |value, _| {
            assert_eq!(value, NativeValue::Scalar(NativeScalar::Str("nonempty")));
            Ok::<_, ()>(())
        })
        .unwrap();
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(current.available_bytes(), initial);

    // Output credit comes from the same current turn.
    let credit = read.reserve_output(1024).unwrap();
    assert_eq!(current.available_bytes(), initial - 1024);
    drop(credit);

    // Pending operations: heads are refused, nothing is committed.
    doc.put(ROOT, "pending", true).unwrap();
    let pending = candidate(&doc, &mut allocations);
    let turn = allocations.work_turn(allowance()).unwrap();
    let read = pending.read_unqualified(&turn).unwrap();
    assert!(read.pending_ops() > 0);
    assert_eq!(
        read.visit_heads(64, |_| panic!("pending heads emitted")),
        Err(NativeResourceError::Unaccounted)
    );
    assert!(read.pending_ops() > 0 && doc.pending_ops() > 0);
}

#[test]
fn large_values_and_counter_successor_walks_are_precharged() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1; 256]));
    for i in 0..256 {
        doc.put(
            ROOT,
            format!("key-{i:04}-{}", "long-key".repeat(16)),
            i as u64,
        )
        .unwrap();
    }
    doc.put(
        ROOT,
        "large-bytes",
        ScalarValue::Bytes(vec![0xa5; 128 * 1024]),
    )
    .unwrap();
    doc.put(ROOT, "large-string", "borrowed-".repeat(16384))
        .unwrap();
    doc.put(ROOT, "counter", ScalarValue::counter(0)).unwrap();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, ScalarValue::counter(0)).unwrap();
    for increment in 1..=256 {
        doc.increment(ROOT, "counter", increment).unwrap();
        doc.increment(&list, 0, increment).unwrap();
    }
    for i in 1..2000 {
        doc.insert(&list, i, i as i64 * 7919).unwrap();
    }
    doc.commit();
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let source = candidate(&doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let initial = current.available_bytes();
    let mut count = 0;
    assert_precharged(&current, || {
        read.visit_keys(read.root(), |key| {
            read.visit_property(read.root(), key, NativeSelection::All, |value, id| {
                match value {
                    NativeValue::Scalar(NativeScalar::Bytes(bytes)) => {
                        assert_eq!(bytes.len(), 128 * 1024)
                    }
                    NativeValue::Scalar(NativeScalar::Str(text)) => {
                        assert_eq!(text.len(), 9 * 16384)
                    }
                    NativeValue::Scalar(NativeScalar::Uint(value)) => assert!(value < 256),
                    NativeValue::Scalar(NativeScalar::Counter(value)) => {
                        assert_eq!(value, 32896)
                    }
                    NativeValue::Object { object, .. } => {
                        let steps = NativeReadSteps::new(1_000_000).unwrap();
                        let n = read
                            .visit_list(object, &steps, |entry| {
                                if entry.index == 0 {
                                    assert_eq!(
                                        entry.value,
                                        NativeValue::Scalar(NativeScalar::Counter(32896))
                                    );
                                }
                                Ok::<_, ()>(ControlFlow::Continue(()))
                            })
                            .unwrap();
                        assert_eq!(n, 2000);
                    }
                    value => panic!("unexpected {value:?}"),
                }
                let mut id_bytes = [0; 600];
                let len = read.write_id(id, &mut id_bytes).unwrap();
                assert_eq!(read.id_len(id).unwrap(), len);
                count += 1;
                Ok::<_, ()>(())
            })
            .unwrap();
            Ok::<_, ()>(())
        })
        .unwrap();
    });
    assert_eq!(count, 260);
    assert_eq!(current.available_bytes(), initial);
}

#[test]
fn marks_keep_true_indices_and_text_matches() {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "abcdefgh界🦀").unwrap();
    doc.split_block(&text, 3).unwrap();
    doc.mark(
        &text,
        Mark::new("bold".into(), true, 1, 6),
        ExpandMark::Both,
    )
    .unwrap();
    doc.mark(
        &text,
        Mark::new("font".into(), "NOT TEXT", 4, 9),
        ExpandMark::Both,
    )
    .unwrap();
    doc.commit();
    assert_eq!(doc.list_range(&text, ..).filter(|e| e.is_mark()).count(), 4);
    check_parity(&mut doc);
}

#[test]
fn legacy_scalar_debug_is_byte_exact_and_allocation_free() {
    let bytes = [0u8, 1, 127, 255];
    let cases = [
        (NativeScalar::Bytes(&bytes), "Bytes([0, 1, 127, 255])"),
        (NativeScalar::Str("a\n\"\\☺"), "Str(\"a\\n\\\"\\\\☺\")"),
        (NativeScalar::Int(i64::MIN), "Int(-9223372036854775808)"),
        (NativeScalar::Uint(u64::MAX), "Uint(18446744073709551615)"),
        (NativeScalar::F64(-0.0), "F64(-0.0)"),
        (NativeScalar::F64(f64::INFINITY), "F64(inf)"),
        (NativeScalar::F64(f64::NAN), "F64(NaN)"),
        (
            NativeScalar::Counter(12),
            "Counter(Counter { start: 12, current: 12 })",
        ),
        (NativeScalar::Timestamp(-42), "Timestamp(-42)"),
        (NativeScalar::Boolean(true), "Boolean(true)"),
        (
            NativeScalar::Unknown {
                type_code: 15,
                bytes: &bytes,
            },
            "Unknown { type_code: 15, bytes: [0, 1, 127, 255] }",
        ),
        (NativeScalar::Null, "Null"),
    ];
    // Compares streamed output with the literal without buffering it.
    struct Expect<'a>(&'a str, bool);
    impl std::fmt::Write for Expect<'_> {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            match self.0.strip_prefix(text) {
                Some(rest) => self.0 = rest,
                None => self.1 = false,
            }
            Ok(())
        }
    }
    let current = allowance();
    for (value, text) in cases {
        let mut sink = Expect(text, true);
        assert_precharged(&current, || value.write_legacy_debug(&mut sink)).unwrap();
        assert!(sink.1 && sink.0.is_empty(), "{text}");
    }
    struct Refuse;
    impl std::fmt::Write for Refuse {
        fn write_str(&mut self, _: &str) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
    }
    assert!(NativeScalar::Int(1)
        .write_legacy_debug(&mut Refuse)
        .is_err());

    // Against the legacy owned ScalarValue for every kind, read natively.
    let values = vec![
        ScalarValue::Bytes(vec![0x17; 1600]),
        ScalarValue::Str("a\n\"\\☺".repeat(200).into()),
        ScalarValue::Int(i64::MIN),
        ScalarValue::Uint(u64::MAX),
        ScalarValue::F64(-0.0),
        ScalarValue::F64(f64::INFINITY),
        ScalarValue::F64(f64::NAN),
        ScalarValue::counter(7),
        ScalarValue::Timestamp(-42),
        ScalarValue::Boolean(true),
        ScalarValue::Unknown {
            type_code: 15,
            bytes: vec![255; 1600],
        },
        ScalarValue::Null,
    ];
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    for value in values {
        let counter = matches!(value, ScalarValue::Counter(_));
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "field", value).unwrap();
        if counter {
            doc.increment(ROOT, "field", 5).unwrap();
        }
        doc.commit();
        let legacy = doc.get(ROOT, "field").unwrap().unwrap().0;
        let legacy = format!("{:?}", legacy.as_scalar().unwrap());
        if counter {
            assert_eq!(legacy, "Counter(Counter { start: 12, current: 12 })");
        }
        let source = candidate(&doc, &mut allocations);
        let turn = allocations.work_turn(current.clone()).unwrap();
        let read = source.read_unqualified(&turn).unwrap();
        let mut sink = Expect(&legacy, true);
        assert_precharged(&current, || {
            read.visit_property(read.root(), "field", NativeSelection::All, |value, _| {
                let NativeValue::Scalar(value) = value else {
                    panic!("scalar")
                };
                value.write_legacy_debug(&mut sink).unwrap();
                Ok::<_, ()>(())
            })
        })
        .unwrap();
        assert!(sink.1 && sink.0.is_empty(), "{legacy}");
    }
}

#[test]
fn table_objects_round_trip_through_writes_reads_and_storage() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![7]));
    let table = doc.put_object(ROOT, "table", ObjType::Table).unwrap();
    assert_eq!(doc.object_type(&table).unwrap(), ObjType::Table);
    doc.put(&table, "row1", "one").unwrap();
    doc.put(&table, "row2", ScalarValue::counter(1)).unwrap();
    doc.increment(&table, "row2", 4).unwrap();
    let nested = doc.put_object(&table, "row3", ObjType::Table).unwrap();
    doc.put(&nested, "cell", 3.5).unwrap();
    let list = doc.put_object(&table, "row4", ObjType::List).unwrap();
    doc.insert(&list, 0, "item").unwrap();
    let in_list = doc.insert_object(&list, 1, ObjType::Table).unwrap();
    doc.put(&in_list, "x", true).unwrap();
    // Sequence props are still refused on a Table.
    assert!(doc.put(&table, 0, "bad").is_err());
    assert!(doc.put_object(&table, 0, ObjType::Map).is_err());
    doc.commit();
    let check = |doc: &mut AutoCommit| {
        let table = match doc.get(ROOT, "table").unwrap().unwrap() {
            (Value::Object(ObjType::Table), id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            doc.keys(&table).collect::<Vec<_>>(),
            ["row1", "row2", "row3", "row4"]
        );
        assert_eq!(
            doc.get(&table, "row1").unwrap().unwrap().0,
            Value::from(ScalarValue::Str("one".into()))
        );
        assert_eq!(
            doc.get(&table, "row2").unwrap().unwrap().0,
            Value::from(ScalarValue::counter(5))
        );
        let (value, nested) = doc.get(&table, "row3").unwrap().unwrap();
        assert_eq!(value, Value::Object(ObjType::Table));
        assert_eq!(
            doc.get(&nested, "cell").unwrap().unwrap().0,
            Value::from(ScalarValue::F64(3.5))
        );
        let hydrated = doc.hydrate(&ROOT, None).unwrap();
        let text = format!("{hydrated:?}");
        assert!(text.contains("row1") && text.contains("cell"), "{text}");
        check_parity(doc);
    };
    check(&mut doc);
    let mut loaded = AutoCommit::load(&doc.save()).unwrap();
    check(&mut loaded);
    // Incremental changes on a Table apply on another replica.
    let mut replica = AutoCommit::load(&doc.save()).unwrap();
    doc.put(&table, "row5", 5).unwrap();
    doc.delete(&table, "row1").unwrap();
    doc.commit();
    replica.merge(&mut doc).unwrap();
    assert_eq!(
        replica.keys(&table).collect::<Vec<_>>(),
        ["row2", "row3", "row4", "row5"]
    );
    check_parity(&mut replica);
    // Concurrent writes to one Table key conflict like a Map.
    let mut left = replica.fork().with_actor(ActorId::from(vec![8]));
    let mut right = replica.fork().with_actor(ActorId::from(vec![9]));
    left.put(&table, "row5", "left").unwrap();
    right.put(&table, "row5", "right").unwrap();
    left.merge(&mut right).unwrap();
    assert_eq!(left.get_all(&table, "row5").unwrap().len(), 2);
    check_parity(&mut left);
}

#[test]
fn counter_overflow_wraps_in_legacy_reads_and_is_refused_by_bounded_reads() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1]));
    doc.put(ROOT, "max", ScalarValue::counter(i64::MAX))
        .unwrap();
    doc.put(ROOT, "min", ScalarValue::counter(i64::MIN))
        .unwrap();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, ScalarValue::counter(i64::MAX))
        .unwrap();
    doc.put(ROOT, "exact", ScalarValue::counter(i64::MAX - 5))
        .unwrap();
    doc.put(ROOT, "detour", ScalarValue::counter(i64::MAX))
        .unwrap();
    doc.commit();
    let before = doc.get_heads();
    doc.update_diff_cursor();
    // Local increments, including within one transaction.
    doc.increment(ROOT, "max", 1).unwrap();
    doc.increment(ROOT, "min", -1).unwrap();
    doc.increment(&list, 0, 2).unwrap();
    doc.increment(ROOT, "max", i64::MAX).unwrap();
    doc.increment(ROOT, "exact", 5).unwrap();
    // Final sum representable, but the first increment overflows.
    doc.increment(ROOT, "detour", 1).unwrap();
    doc.increment(ROOT, "detour", -1).unwrap();
    doc.commit();
    let expected_max = i64::MAX.wrapping_add(1).wrapping_add(i64::MAX);
    let get = |doc: &AutoCommit, key: &str| match doc.get(ROOT, key).unwrap().unwrap().0 {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Counter(c) => i64::from(c),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(get(&doc, "max"), expected_max);
    assert_eq!(get(&doc, "min"), i64::MAX);
    let list_value = doc.list_range(&list, ..).next().unwrap().into_owned().value;
    assert_eq!(
        list_value,
        ValueRef::Scalar(ScalarValueRef::Counter(i64::MAX.wrapping_add(2)))
    );
    // Patches, diffs and hydration wrap too.
    let patches = doc.diff_incremental();
    assert!(patches
        .iter()
        .any(|p| matches!(p.action, PatchAction::Increment { .. })));
    let after = doc.get_heads();
    let _ = doc.diff(&before, &after);
    let _ = doc.hydrate(&ROOT, None).unwrap();
    // Concurrent increments from two peers merged and reloaded.
    let mut a = doc.fork().with_actor(ActorId::from(vec![2]));
    let mut b = doc.fork().with_actor(ActorId::from(vec![3]));
    a.increment(ROOT, "max", i64::MAX).unwrap();
    b.increment(ROOT, "max", i64::MAX).unwrap();
    a.commit();
    b.commit();
    let mut observed = a.clone();
    observed.update_diff_cursor();
    observed.merge(&mut b).unwrap();
    let _ = observed.diff_incremental();
    let expected = expected_max.wrapping_add(i64::MAX).wrapping_add(i64::MAX);
    assert_eq!(get(&observed, "max"), expected);
    let loaded = AutoCommit::load(&observed.save()).unwrap();
    assert_eq!(get(&loaded, "max"), expected);
    let _ = loaded.hydrate(&ROOT, None).unwrap();
    // Historical reads scope increments to the clock with wrapping sums.
    let at = loaded.get_at(ROOT, "max", &before).unwrap().unwrap().0;
    assert_eq!(at, Value::from(ScalarValue::counter(i64::MAX)));
    // The bounded reader keeps the 0.8 contract: a checked sum, refused as
    // Invalid at the first unrepresentable step, never a wrapped value.
    assert_eq!(get(&loaded, "detour"), i64::MAX);
    for doc in [&doc, &loaded] {
        assert_bounded_counters(doc, &list);
    }
}

fn assert_bounded_counters(doc: &AutoCommit, list: &ObjId) {
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let source = candidate(doc, &mut allocations);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = source.read_unqualified(&turn).unwrap();
    let invalid = || NativeReadError::Resource(NativeResourceError::Invalid);
    let steps = NativeReadSteps::new(1_000_000).unwrap();
    for key in ["max", "min", "detour"] {
        for selection in [NativeSelection::Winner, NativeSelection::All] {
            assert_eq!(
                assert_precharged(&current, || read.visit_property(
                    read.root(),
                    key,
                    selection,
                    |_, _| Err::<(), _>("overflowed counter reached the visitor")
                )),
                Err(invalid()),
                "{key}"
            );
            assert_eq!(
                read.visit_property_with_steps(read.root(), key, selection, &steps, |_, _| {
                    Err::<(), _>("overflowed counter reached the visitor")
                }),
                Err(invalid())
            );
        }
    }
    // A sum that ends exactly at the bound is still lent.
    let count = read
        .visit_property(read.root(), "exact", NativeSelection::All, |value, _| {
            assert_eq!(value, NativeValue::Scalar(NativeScalar::Counter(i64::MAX)));
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(count, 1);
    let mut list_object = None;
    read.visit_property(read.root(), "list", NativeSelection::Winner, |value, _| {
        if let NativeValue::Object { object, .. } = value {
            list_object = Some(object);
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(doc.length(list), 1);
    assert_eq!(
        read.visit_list(list_object.unwrap(), &steps, |_| {
            Err::<ControlFlow<()>, _>("overflowed counter reached the visitor")
        }),
        Err(invalid())
    );
    // Keys are still visitable; only the counter value is refused.
    assert_eq!(
        read.visit_keys(read.root(), |_| Ok::<_, ()>(())).unwrap(),
        5
    );
}
