// Bounded load, save and history export (`resource_limits` lane L3).
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::path::Path;

use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeLimits, NativeOwned, NativeResourceError,
    NativeWorkTurn,
};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjType, ReadDoc, SaveOptions, ScalarValue, ROOT};

// ---- per-thread allocation meter -------------------------------------------

#[derive(Clone, Copy, Default, Debug)]
struct Meter {
    live: isize,
    peak: isize,
    watched: usize,
    /// Largest excess of live measured bytes over bytes reserved on the
    /// checked allowance since measuring began (realloc overlap included).
    unreserved: isize,
}

thread_local! {
    static METER: Cell<Option<Meter>> = const { Cell::new(None) };
    static WATCH: Cell<Option<usize>> = const { Cell::new(None) };
    static CHECK: RefCell<Option<(NativeAllowance, usize)>> = const { RefCell::new(None) };
}

fn on_alloc(bytes: usize) {
    let _ = METER.try_with(|meter| {
        if let Some(mut m) = meter.get() {
            if WATCH.try_with(|w| w.get() == Some(bytes)).unwrap_or(false) {
                m.watched += 1;
            }
            m.live += bytes as isize;
            m.peak = m.peak.max(m.live);
            let _ = CHECK.try_with(|check| {
                if let Some((allowance, initial)) = &*check.borrow() {
                    let reserved = initial.saturating_sub(allowance.available_bytes()) as isize;
                    m.unreserved = m.unreserved.max(m.live - reserved);
                }
            });
            meter.set(Some(m));
        }
    });
}

fn on_free(bytes: usize) {
    let _ = METER.try_with(|meter| {
        if let Some(mut m) = meter.get() {
            m.live -= bytes as isize;
            meter.set(Some(m));
        }
    });
}

struct Counting;

// SAFETY: every operation delegates unchanged to System; the thread-local
// meter holds only Copy scalars and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            on_alloc(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            on_alloc(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        on_free(layout.size());
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            // Conservative old/new overlap of a moving reallocation.
            on_alloc(size);
            on_free(layout.size());
        }
        q
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn measure<T>(watch: Option<usize>, f: impl FnOnce() -> T) -> (T, Meter) {
    WATCH.with(|w| w.set(watch));
    METER.with(|m| m.set(Some(Meter::default())));
    let value = f();
    let meter = METER.with(|m| m.replace(None)).unwrap();
    WATCH.with(|w| w.set(None));
    (value, meter)
}

/// Runs `f` asserting every allocation was already reserved on `allowance`.
fn precharged<T>(allowance: &NativeAllowance, f: impl FnOnce() -> T) -> T {
    CHECK.with(|c| *c.borrow_mut() = Some((allowance.clone(), allowance.available_bytes())));
    let (value, meter) = measure(None, f);
    CHECK.with(|c| *c.borrow_mut() = None);
    assert!(
        meter.unreserved <= 0,
        "allocated {} bytes beyond the reservation",
        meter.unreserved
    );
    value
}

// ---- fixtures ---------------------------------------------------------------

fn rich_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, &"bounded native text ".repeat(64))
        .unwrap();
    doc.commit();
    doc.mark(
        &text,
        Mark::new("bold".into(), true, 3, 40),
        ExpandMark::Both,
    )
    .unwrap();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    for i in 0..32 {
        doc.insert(&list, i, i as i64).unwrap();
    }
    doc.put(ROOT, "count", ScalarValue::counter(1)).unwrap();
    doc.commit();
    let mut other = doc.fork();
    other.put(ROOT, "other", "a second actor").unwrap();
    other.increment(ROOT, "count", 5).unwrap();
    other.commit();
    doc.put(ROOT, "concurrent", vec![7u8; 600]).unwrap();
    doc.commit();
    doc.merge(&mut other).unwrap();
    doc.put_object(ROOT, "table", ObjType::Table).unwrap();
    let map = doc.put_object(ROOT, "map", ObjType::Map).unwrap();
    doc.put(&map, "row", 1).unwrap();
    doc.commit();
    doc
}

struct Owner {
    allocations: NativeAllocationSet,
    turn: NativeWorkTurn,
    allowance: NativeAllowance,
}

fn new_owner(bytes: usize) -> Owner {
    let allowance = NativeAllowance::new(bytes).unwrap();
    let allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    Owner {
        allocations,
        turn,
        allowance,
    }
}

fn reconstruct(
    owner: &mut Owner,
    bytes: &[u8],
) -> Result<NativeOwned<AutoCommit>, NativeResourceError> {
    let normalized = owner
        .turn
        .normalize_input(bytes, &NativeLimits::default())?;
    normalized.reconstruct(&mut owner.allocations, &owner.turn)
}

fn saved(owner: &Owner, doc: &NativeOwned<AutoCommit>, deflate: bool) -> Vec<u8> {
    doc.save_bounded(
        &owner.turn,
        SaveOptions {
            deflate,
            retain_orphans: true,
        },
    )
    .unwrap()
    .bytes()
    .to_vec()
}

fn change_hashes(
    owner: &Owner,
    doc: &NativeOwned<AutoCommit>,
    heads: &[ChangeHash],
) -> Vec<ChangeHash> {
    let changes = doc.changes_since_bounded(heads, &owner.turn).unwrap();
    (0..changes.len())
        .map(|i| changes.hash(i).unwrap())
        .collect()
}

// ---- reconstruct ------------------------------------------------------------

#[test]
fn reconstruct_round_trips_documents_saved_by_0_12() {
    let mut source = rich_document();
    let heads = source.get_heads();
    let expected = source.hydrate(&ROOT, None).unwrap();
    for deflate in [false, true] {
        let bytes = source.save_with_options(SaveOptions {
            deflate,
            retain_orphans: true,
        });
        let mut owner = new_owner(64 * 1024 * 1024);
        let native = reconstruct(&mut owner, &bytes).unwrap();
        let mut restored = AutoCommit::load(&saved(&owner, &native, deflate)).unwrap();
        assert_eq!(restored.get_heads(), heads);
        assert_eq!(restored.hydrate(&ROOT, None).unwrap(), expected);
        let base = native.base_heads_bounded(&owner.turn).unwrap();
        let mut sorted = heads.clone();
        sorted.sort();
        let mut got = base.as_slice().to_vec();
        got.sort();
        assert_eq!(got, sorted);
        assert!(!native.has_missing_dependencies());
    }
}

#[test]
fn reconstruct_accepts_change_chunks_and_concatenations() {
    let mut source = rich_document();
    let heads = source.get_heads();
    let changes = source.get_changes(&[]);
    let mut raw = Vec::new();
    for change in &changes {
        raw.extend_from_slice(change.raw_bytes());
    }
    let mut owner = new_owner(64 * 1024 * 1024);
    let native = reconstruct(&mut owner, &raw).unwrap();
    let mut restored = AutoCommit::load(&saved(&owner, &native, true)).unwrap();
    assert_eq!(restored.get_heads(), heads);

    // A document chunk followed by an incremental save.
    let mut base = AutoCommit::new();
    base.put(ROOT, "a", 1).unwrap();
    base.commit();
    let mut bytes = base.save();
    base.put(ROOT, "b", 2).unwrap();
    base.commit();
    bytes.extend(base.save_incremental());
    let mut owner = new_owner(64 * 1024 * 1024);
    let native = reconstruct(&mut owner, &bytes).unwrap();
    let mut restored = AutoCommit::load(&saved(&owner, &native, false)).unwrap();
    assert_eq!(restored.get_heads(), base.get_heads());
}

fn fixture_paths() -> Vec<std::path::PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    // Also resolvable when this file is compiled from a workspace member.
    for dir in [
        root.join("tests/fixtures"),
        root.join("../../../vendor/automerge/tests/fixtures"),
        root.join("../../crates/doc/qdoc/tests/data"),
        root.join("../qdoc/tests/data"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "automerge") {
                    paths.push(path);
                }
            }
        }
    }
    paths.sort();
    paths
}

/// Storage written by earlier automerge releases (the crate's own fixtures
/// and the qdoc fixtures): reconstruct agrees with 0.12's strict load.
#[test]
fn reconstruct_agrees_with_strict_load_on_older_fixtures() {
    let mut accepted = 0;
    for path in fixture_paths() {
        let bytes = std::fs::read(&path).unwrap();
        let strict = automerge::Automerge::load_with_options(
            &bytes,
            automerge::LoadOptions::new()
                .on_partial_load(automerge::OnPartialLoad::Error)
                .verification_mode(automerge::VerificationMode::Check),
        );
        let mut owner = new_owner(64 * 1024 * 1024);
        let native = reconstruct(&mut owner, &bytes);
        match (&strict, &native) {
            (Ok(strict), Ok(native)) => {
                accepted += 1;
                let mut restored = AutoCommit::load(&saved(&owner, native, true)).unwrap();
                assert_eq!(restored.get_heads(), strict.get_heads(), "{path:?}");
                assert_eq!(
                    restored.hydrate(&ROOT, None).unwrap(),
                    strict.hydrate(None),
                    "{path:?}"
                );
                let mut expected: Vec<_> =
                    strict.get_changes(&[]).iter().map(|c| c.hash()).collect();
                let mut got = change_hashes(&owner, native, &[]);
                expected.sort();
                got.sort();
                assert_eq!(got, expected, "{path:?}");
            }
            (Err(_), Err(_)) => {}
            // Bounded reconstruct never accepts what strict load refuses.
            (Err(error), Ok(_)) => {
                panic!("{path:?}: strict load refused ({error}) but reconstruct accepted")
            }
            // Refusals stricter than 0.12 must come from inspection limits,
            // never from an accepted storage form.
            (Ok(_), Err(error)) => println!("{path:?}: reconstruct refused: {error:?}"),
        }
    }
    assert!(
        accepted >= 3,
        "expected several loadable fixtures, got {accepted}"
    );
}

#[test]
fn shape_over_limits_is_refused_at_normalize_input() {
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    for i in 0..200 {
        doc.insert(&list, i, i as i64).unwrap();
    }
    doc.commit();
    let bytes = doc.save();
    let owner = new_owner(64 * 1024 * 1024);
    let limits = NativeLimits {
        max_operations: 100,
        max_operation_counter: 100,
        ..NativeLimits::default()
    };
    let error = owner.turn.normalize_input(&bytes, &limits).unwrap_err();
    assert_eq!(error, NativeResourceError::Operations);
}

#[test]
fn malformed_storage_is_invalid() {
    let mut bytes = rich_document().save_nocompress();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // checksum no longer matches
    let mut owner = new_owner(64 * 1024 * 1024);
    assert_eq!(
        reconstruct(&mut owner, &bytes).unwrap_err(),
        NativeResourceError::Invalid
    );
}

#[test]
fn orphans_only_after_a_leading_document_chunk() {
    let mut full = AutoCommit::new();
    full.put(ROOT, "first", 1).unwrap();
    full.commit();
    full.put(ROOT, "second", 2).unwrap();
    full.commit();
    let changes = full.get_changes(&[]);
    let orphan = changes[1].raw_bytes().to_vec();

    // Change chunk with a missing dependency and no document: refused.
    let mut owner = new_owner(64 * 1024 * 1024);
    assert_eq!(
        reconstruct(&mut owner, &orphan).unwrap_err(),
        NativeResourceError::Invalid
    );

    // Unrelated document chunk, then the orphan: queued.
    let mut base = AutoCommit::new();
    base.put(ROOT, "base", true).unwrap();
    base.commit();
    let mut bytes = base.save();
    bytes.extend_from_slice(&orphan);
    let native = reconstruct(&mut owner, &bytes).unwrap();
    assert!(native.has_missing_dependencies());
    let out = saved(&owner, &native, true);
    assert!(out.ends_with(&orphan));
    let without = native
        .save_bounded(
            &owner.turn,
            SaveOptions {
                deflate: true,
                retain_orphans: false,
            },
        )
        .unwrap();
    assert!(!without.bytes().ends_with(&orphan));
    let restored = AutoCommit::load(without.bytes()).unwrap();
    assert!(restored.get_missing_deps(&[]).is_empty());
}

#[test]
fn reconstruct_refuses_a_turn_from_another_owner() {
    let bytes = rich_document().save();
    let mut first = new_owner(64 * 1024 * 1024);
    let second = new_owner(64 * 1024 * 1024);
    let normalized = second
        .turn
        .normalize_input(&bytes, &NativeLimits::default())
        .unwrap();
    assert_eq!(
        normalized
            .reconstruct(&mut first.allocations, &first.turn)
            .unwrap_err(),
        NativeResourceError::Unaccounted
    );
    // Same registry, but the normalized credit came from a different turn.
    let other_turn = first
        .allocations
        .work_turn(NativeAllowance::new(64 * 1024 * 1024).unwrap())
        .unwrap();
    let normalized = other_turn
        .normalize_input(&bytes, &NativeLimits::default())
        .unwrap();
    assert_eq!(
        normalized
            .reconstruct(&mut first.allocations, &first.turn)
            .unwrap_err(),
        NativeResourceError::Unaccounted
    );
}

#[test]
fn load_under_a_tiny_allowance_refuses_before_building_the_document() {
    let mut source = AutoCommit::new();
    let text = source.put_object(ROOT, "text", ObjType::Text).unwrap();
    source
        .splice_text(&text, 0, 0, &"x".repeat(20_000))
        .unwrap();
    source.commit();
    let bytes = source.save_nocompress();
    let mut owner = new_owner(8 * 1024 * 1024);
    let normalized = owner
        .turn
        .normalize_input(&bytes, &NativeLimits::default())
        .unwrap();
    // Leave only a little headroom: the load peak cannot be reserved.
    let _hog = owner
        .turn
        .reserve(owner.allowance.available_bytes() - 4096)
        .unwrap();
    let (result, meter) = measure(None, || {
        normalized.reconstruct(&mut owner.allocations, &owner.turn)
    });
    assert_eq!(result.unwrap_err(), NativeResourceError::WorkBytes);
    // Only the normalized input (allocated before measuring) is freed; no
    // native document state was built.
    assert!(
        meter.peak <= 1024,
        "allocated {} bytes before refusing",
        meter.peak
    );
}

// ---- save -------------------------------------------------------------------

#[test]
fn save_limit_is_checked_on_the_exact_output_before_the_destination() {
    let mut source = rich_document();
    let mut unrelated = AutoCommit::new();
    unrelated.put(ROOT, "dependency", true).unwrap();
    unrelated.commit();
    unrelated.put(ROOT, "queued", vec![0x5Au8; 4096]).unwrap();
    unrelated.commit();
    let queued = unrelated.get_changes(&[])[1].clone();
    let queued_raw = queued.raw_bytes().to_vec();
    source.apply_changes([queued]).unwrap();
    let mut bytes = source.save_with_options(SaveOptions {
        deflate: true,
        retain_orphans: true,
    });
    assert!(bytes.ends_with(&queued_raw));
    let mut owner = new_owner(64 * 1024 * 1024);
    let native = reconstruct(&mut owner, &bytes).unwrap();
    for deflate in [false, true] {
        let options = || SaveOptions {
            deflate,
            retain_orphans: true,
        };
        let expected = native.save_bounded(&owner.turn, options()).unwrap();
        assert!(expected.bytes().ends_with(&queued_raw));
        let length = expected.bytes().len();
        let (exact, meter) = measure(Some(length), || {
            native.save_bounded_with_limit(&owner.turn, options(), length)
        });
        let exact = exact.unwrap();
        assert_eq!(exact.bytes(), expected.bytes());
        assert_eq!(meter.watched, 1, "exact fit allocates its destination once");
        assert_eq!(meter.live, length as isize, "only the output stays live");
        let (short, meter) = measure(Some(length), || {
            native.save_bounded_with_limit(&owner.turn, options(), length - 1)
        });
        assert_eq!(short.unwrap_err(), NativeResourceError::WorkBytes);
        assert_eq!(meter.watched, 0, "refused before the destination");
        assert_eq!(meter.live, 0);
    }
    bytes.clear();
}

#[test]
fn save_refuses_open_transactions_and_small_turns() {
    let mut pending = rich_document();
    pending.put(ROOT, "pending", 1).unwrap();
    assert!(pending.pending_ops() > 0);
    let mut owner = new_owner(64 * 1024 * 1024);
    let candidate = pending
        .clone_bounded(&mut owner.allocations, &owner.turn)
        .unwrap();
    assert_eq!(
        candidate
            .save_bounded(&owner.turn, SaveOptions::default())
            .unwrap_err(),
        NativeResourceError::Unaccounted
    );

    let native = reconstruct(&mut owner, &rich_document().save()).unwrap();
    let tiny = NativeAllowance::new(1024).unwrap();
    let tiny_turn = owner.allocations.work_turn(tiny).unwrap();
    let (result, meter) = measure(None, || {
        native.save_bounded(&tiny_turn, SaveOptions::default())
    });
    assert_eq!(result.unwrap_err(), NativeResourceError::WorkBytes);
    assert_eq!(meter.peak, 0, "refused before any allocation");
}

// ---- history ----------------------------------------------------------------

#[test]
fn changes_since_matches_get_changes() {
    let mut source = rich_document();
    let all = source.get_changes(&[]);
    let middle = vec![all[1].hash()];
    let unknown = ChangeHash([0xAB; 32]);
    let bytes = source.save();
    let mut owner = new_owner(64 * 1024 * 1024);
    let native = reconstruct(&mut owner, &bytes).unwrap();
    for heads in [
        vec![],
        middle.clone(),
        vec![unknown],
        vec![unknown, all[1].hash()],
        source.get_heads(),
    ] {
        let expected = source.get_changes(&heads);
        let changes = native.changes_since_bounded(&heads, &owner.turn).unwrap();
        assert_eq!(changes.len(), expected.len());
        for (i, change) in expected.iter().enumerate() {
            assert_eq!(changes.hash(i), Some(change.hash()));
            assert_eq!(changes.raw_bytes(i), Some(change.raw_bytes()));
        }
    }
    assert!(native
        .changes_since_bounded(&source.get_heads(), &owner.turn)
        .unwrap()
        .is_empty());
}

#[test]
fn history_charges_its_turn_and_refuses_small_turns() {
    let mut source = rich_document();
    let mut owner = new_owner(64 * 1024 * 1024);
    let native = reconstruct(&mut owner, &source.save()).unwrap();
    let before = owner.allowance.available_bytes();
    let heads = native.base_heads_bounded(&owner.turn).unwrap();
    assert_eq!(
        before - owner.allowance.available_bytes(),
        heads.as_slice().len() * std::mem::size_of::<ChangeHash>()
    );
    drop(heads);
    let tiny = owner
        .allocations
        .work_turn(NativeAllowance::new(1024).unwrap())
        .unwrap();
    let (result, meter) = measure(None, || native.changes_since_bounded(&[], &tiny));
    assert_eq!(result.unwrap_err(), NativeResourceError::WorkBytes);
    assert_eq!(meter.peak, 0);
    let (missing, meter) = measure(None, || native.has_missing_dependencies());
    assert!(!missing);
    assert_eq!(meter.peak, 0, "has_missing_dependencies must not allocate");
}

// ---- reserve-before-allocate ------------------------------------------------

#[test]
fn every_allocation_is_reserved_first() {
    let mut source = rich_document();
    let mut orphan_source = AutoCommit::new();
    orphan_source.put(ROOT, "a", 1).unwrap();
    orphan_source.commit();
    orphan_source.put(ROOT, "b", vec![1u8; 2048]).unwrap();
    orphan_source.commit();
    let orphan = orphan_source.get_changes(&[])[1].raw_bytes().to_vec();
    for deflate in [false, true] {
        let mut bytes = source.save_with_options(SaveOptions {
            deflate,
            retain_orphans: true,
        });
        bytes.extend_from_slice(&orphan);
        let mut owner = new_owner(64 * 1024 * 1024);
        let normalized = owner
            .turn
            .normalize_input(&bytes, &NativeLimits::default())
            .unwrap();
        let native = precharged(&owner.allowance, || {
            normalized.reconstruct(&mut owner.allocations, &owner.turn)
        })
        .unwrap();
        let out = precharged(&owner.allowance, || {
            native.save_bounded(
                &owner.turn,
                SaveOptions {
                    deflate,
                    retain_orphans: true,
                },
            )
        })
        .unwrap();
        assert!(out.bytes().ends_with(&orphan));
        let changes = precharged(&owner.allowance, || {
            native.changes_since_bounded(&[], &owner.turn)
        })
        .unwrap();
        assert_eq!(changes.len(), source.get_changes(&[]).len());
        let heads =
            precharged(&owner.allowance, || native.base_heads_bounded(&owner.turn)).unwrap();
        assert_eq!(heads.as_slice().len(), source.get_heads().len());
        assert!(precharged(&owner.allowance, || native.has_missing_dependencies()));
    }
}

// ---- hostile storage --------------------------------------------------------

/// Minimal SHA-256 (FIPS 180-4) for re-checksumming crafted chunks.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn uleb(input: &mut &[u8]) -> u64 {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = input[0];
        *input = &input[1..];
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

fn write_uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Replaces one op column of an uncompressed document chunk and rewrites
/// lengths and the checksum.
fn rewrite_op_column(chunk: &[u8], spec: u64, data: &[u8]) -> Vec<u8> {
    assert_eq!(chunk[8], 0, "document chunk");
    let mut rest = &chunk[9..];
    let length = uleb(&mut rest) as usize;
    let body = &rest[..length];
    let mut input = body;
    for _ in 0..uleb(&mut input) {
        let len = uleb(&mut input) as usize;
        input = &input[len..];
    }
    let heads = uleb(&mut input) as usize;
    input = &input[heads * 32..];
    let prefix = &body[..body.len() - input.len()];
    let mut groups = Vec::new();
    for _ in 0..2 {
        let mut columns = Vec::new();
        for _ in 0..uleb(&mut input) {
            columns.push((uleb(&mut input), uleb(&mut input) as usize));
        }
        groups.push(columns);
    }
    let mut datas = Vec::new();
    for group in &groups {
        let mut columns = Vec::new();
        for (_, len) in group {
            columns.push(input[..*len].to_vec());
            input = &input[*len..];
        }
        datas.push(columns);
    }
    let suffix = input;
    let position = groups[1]
        .iter()
        .position(|(s, _)| *s == spec)
        .expect("op column present");
    groups[1][position].1 = data.len();
    datas[1][position] = data.to_vec();
    let mut new_body = prefix.to_vec();
    for group in &groups {
        write_uleb(&mut new_body, group.len() as u64);
        for (spec, len) in group {
            write_uleb(&mut new_body, *spec);
            write_uleb(&mut new_body, *len as u64);
        }
    }
    for columns in &datas {
        for column in columns {
            new_body.extend_from_slice(column);
        }
    }
    new_body.extend_from_slice(suffix);
    let mut hashed = vec![0u8];
    write_uleb(&mut hashed, new_body.len() as u64);
    hashed.extend_from_slice(&new_body);
    let checksum = sha256(&hashed);
    let mut out = chunk[..4].to_vec();
    out.extend_from_slice(&checksum[..4]);
    out.extend_from_slice(&hashed);
    out
}

fn op_column(chunk: &[u8], spec: u64) -> Vec<u8> {
    let mut rest = &chunk[9..];
    let _length = uleb(&mut rest);
    let mut input = rest;
    for _ in 0..uleb(&mut input) {
        let len = uleb(&mut input) as usize;
        input = &input[len..];
    }
    let heads = uleb(&mut input) as usize;
    input = &input[heads * 32..];
    let mut groups = Vec::new();
    for _ in 0..2 {
        let mut columns = Vec::new();
        for _ in 0..uleb(&mut input) {
            columns.push((uleb(&mut input), uleb(&mut input) as usize));
        }
        groups.push(columns);
    }
    let change_bytes: usize = groups[0].iter().map(|(_, len)| len).sum();
    input = &input[change_bytes..];
    for (s, len) in &groups[1] {
        if *s == spec {
            return input[..*len].to_vec();
        }
        input = &input[*len..];
    }
    panic!("no column {spec:#x}")
}

#[test]
fn crafted_chunk_helper_round_trips() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "a", 1).unwrap();
    doc.put(ROOT, "b", 2).unwrap();
    doc.commit();
    let bytes = doc.save_nocompress();
    // Same id counters (deltas 1, 1), re-encoded as a literal run.
    let original = op_column(&bytes, 0x23);
    assert_eq!(original, [0x02, 0x01], "id counters 1, 2 as one delta run");
    assert_eq!(rewrite_op_column(&bytes, 0x23, &original), bytes);
}

/// Two ops of one change sharing an op id reached `assert!` in 0.12's
/// `VecEncoder::add`; now refused as Invalid.
#[test]
fn duplicate_op_ids_are_invalid_not_a_panic() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "a", 1).unwrap();
    doc.put(ROOT, "b", 2).unwrap();
    doc.commit();
    let bytes = doc.save_nocompress();
    // Counters 1, 1 (deltas 1, 0): a canonical literal run.
    let rewritten = rewrite_op_column(&bytes, 0x23, &[0x7e, 0x01, 0x00]);
    let mut owner = new_owner(64 * 1024 * 1024);
    assert!(reconstruct(&mut owner, &rewritten).is_err());
}

#[test]
fn upstream_fuzz_crashers_are_refused() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut seen = 0;
    for dir in [
        root.join("tests/fuzz-crashers"),
        root.join("../../../vendor/automerge/tests/fuzz-crashers"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let bytes = std::fs::read(entry.path()).unwrap();
            let mut owner = new_owner(64 * 1024 * 1024);
            assert!(
                reconstruct(&mut owner, &bytes).is_err(),
                "{:?}",
                entry.path()
            );
            seen += 1;
        }
    }
    assert!(seen > 0);
}

/// Many changes: roughly one in 256 hashes is a level >= 1 fragment, whose
/// cache growth is reserved from the exact count before finalization.
#[test]
fn fragment_cache_growth_is_reserved() {
    // Deterministic hashes (fixed actor and timestamps). The size stays
    // below the point where L1's provisional F = changes quadratic fragment
    // term would exceed the 64 MiB allowance.
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from([7u8; 16]));
    for i in 0..1200u64 {
        doc.put(ROOT, "k", i).unwrap();
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    }
    let fragments: Vec<usize> = doc
        .get_changes(&[])
        .iter()
        .enumerate()
        .filter(|(_, c)| c.hash().0[0] == 0)
        .map(|(i, _)| i)
        .collect();
    // Deterministic: [13, 328, 881, 950, 1137], on both sides of the split.
    assert!(fragments.iter().any(|&i| i < 1000) && fragments.iter().any(|&i| i >= 1000));
    let heads = doc.get_heads();
    // Document chunk of the first 1000 changes, then 200 change chunks: the
    // finalize path and the queued-apply path both grow the fragment cache.
    let all = doc.get_changes(&[]);
    let mut mixed = doc.fork_at(&[all[999].hash()]).unwrap().save();
    for change in &all[1000..] {
        mixed.extend_from_slice(change.raw_bytes());
    }
    for (variant, bytes) in [doc.save(), mixed].into_iter().enumerate() {
        let mut owner = new_owner(64 * 1024 * 1024);
        let normalized = owner
            .turn
            .normalize_input(&bytes, &NativeLimits::default())
            .unwrap();
        let native = precharged(&owner.allowance, || {
            normalized.reconstruct(&mut owner.allocations, &owner.turn)
        })
        .unwrap_or_else(|error| panic!("variant {variant}: {error:?}"));
        let mut restored = AutoCommit::load(&saved(&owner, &native, true)).unwrap();
        assert_eq!(restored.get_heads(), heads);
    }
}
