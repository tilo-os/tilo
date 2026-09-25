//! Adversarial admission tests for `automerge::resource_limits` on 0.12.
//!
//! Threat: a remote peer controls every byte of automerge storage handed to
//! `NativeWorkTurn::normalize_input` / `AutoCommit::inspect_bounded`,
//! `AutoCommit::apply_incoming_checked`, `NativeOwned::apply_checked_bounded`
//! and `NormalizedStorage::reconstruct`.
//!
//! Oracle for every case: the expected closed error, no panic
//! (`catch_unwind`), the resident document's `save()` unchanged on refusal,
//! and, where measurable, allocation during the call covered by reservations
//! made beforehand (a per-thread counting `#[global_allocator]`).
//!
//! Sections:
//! - `vendor08`: the Cloud alignment branch's vendor-boundary expectations
//!   (qos-peer-state native_resource_vendor_tests.rs), hex blobs verbatim.
//! - `wire`: crafted chunk framing, LEB128, column, chunk and cardinality attacks,
//!   compression bombs with an allocation-peak oracle.
//! - `semantic`: forged change chunks (missing objects, preds, key kinds,
//!   actor indexes), duplicate sequences, missing dependencies.
//! - `budget`: allowance sweeps (WorkBytes and unchanged document at every
//!   stage), reserve-before-allocate over the whole bounded API (ported from
//!   native_footprint_vendor_tests.rs, adjusted to owned 0.12 slabs).
//! - `shapes`: deep nesting, long runs, many actors.
//!
//! Slow shapes are gated behind `AM012_SLOW=1`.
#![allow(clippy::type_complexity)]

use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeApplyResult, NativeDependencyPolicy, NativeIncomingOutcome,
    NativeLimits, NativeMutationError, NativeOwned, NativeReadSteps, NativeResourceError, NativeSelection,
    NativeValue,
};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, Change, ObjId, ObjType, ReadDoc, SaveOptions, ScalarValue, ROOT};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};

// ===========================================================================
// Per-thread allocation meter (ported from qos-peer-state
// native_footprint_vendor_tests.rs, with saturating frees so in-place
// mutation of pre-existing documents can be measured too).
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

// ===========================================================================
// Helpers
// ===========================================================================

const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

fn slow() -> bool {
    std::env::var_os("AM012_SLOW").is_some()
}

fn leb(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 128);
    }
}
fn sleb(mut n: i64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        let done = (n == 0 && b & 64 == 0) || (n == -1 && b & 64 != 0);
        if done {
            out.push(b);
            break;
        }
        out.push(b | 128);
    }
}

fn checksum(kind: u8, body: &[u8]) -> [u8; 4] {
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    hash.update([kind]);
    let mut len = Vec::new();
    leb(body.len() as u64, &mut len);
    hash.update(&len);
    hash.update(body);
    let digest = hash.finalize();
    [digest[0], digest[1], digest[2], digest[3]]
}

/// Frame `body` as a chunk with a genuine checksum (for kind 2 the checksum
/// covers the decompressed change body, as automerge computes it).
fn chunk(kind: u8, body: &[u8]) -> Vec<u8> {
    chunk_with_checksum(kind, body, checksum(kind, body))
}
fn chunk_with_checksum(kind: u8, body: &[u8], sum: [u8; 4]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend(sum);
    out.push(kind);
    leb(body.len() as u64, &mut out);
    out.extend(body);
    out
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// A raw DEFLATE stream that inflates to `n` zero bytes, built without
/// materialising the output (fixed-Huffman block of length-258 copies).
fn zero_bomb(n: usize) -> Vec<u8> {
    // Compress in bounded pieces to keep the generator itself small.
    use std::io::Write;
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    let block = vec![0u8; 1 << 20];
    let mut left = n;
    while left > 0 {
        let take = left.min(block.len());
        encoder.write_all(&block[..take]).unwrap();
        left -= take;
    }
    encoder.finish().unwrap()
}

/// Parsed change chunk header; `rest` (op column metadata, data, extra
/// bytes) is kept opaque so forged variants keep valid op columns.
#[derive(Clone, Debug)]
struct ChangeParts {
    deps: Vec<[u8; 32]>,
    actor: Vec<u8>,
    seq: u64,
    start_op: u64,
    time: i64,
    message: Vec<u8>,
    others: Vec<Vec<u8>>,
    rest: Vec<u8>,
}

fn read_uint(bytes: &mut &[u8]) -> u64 {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let b = bytes[0];
        *bytes = &bytes[1..];
        result |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return result;
        }
        shift += 7;
    }
}
fn read_sint(bytes: &mut &[u8]) -> i64 {
    let mut result = 0i64;
    let mut shift = 0;
    loop {
        let b = bytes[0];
        *bytes = &bytes[1..];
        result |= i64::from(b & 127) << shift;
        shift += 7;
        if b & 128 == 0 {
            if shift < 64 && b & 64 != 0 {
                result |= -1 << shift;
            }
            return result;
        }
    }
}
fn read_blob(bytes: &mut &[u8]) -> Vec<u8> {
    let n = read_uint(bytes) as usize;
    let out = bytes[..n].to_vec();
    *bytes = &bytes[n..];
    out
}

impl ChangeParts {
    fn parse(raw: &[u8]) -> Self {
        assert_eq!(&raw[..4], &MAGIC);
        assert_eq!(raw[8], 1, "uncompressed change chunk expected");
        let mut b = &raw[9..];
        let len = read_uint(&mut b) as usize;
        assert_eq!(b.len(), len);
        let ndeps = read_uint(&mut b) as usize;
        let deps = (0..ndeps)
            .map(|_| {
                let d: [u8; 32] = b[..32].try_into().unwrap();
                b = &b[32..];
                d
            })
            .collect();
        let actor = read_blob(&mut b);
        let seq = read_uint(&mut b);
        let start_op = read_uint(&mut b);
        let time = read_sint(&mut b);
        let message = read_blob(&mut b);
        let nothers = read_uint(&mut b) as usize;
        let others = (0..nothers).map(|_| read_blob(&mut b)).collect();
        Self { deps, actor, seq, start_op, time, message, others, rest: b.to_vec() }
    }
    fn body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        leb(self.deps.len() as u64, &mut out);
        for d in &self.deps {
            out.extend(d);
        }
        leb(self.actor.len() as u64, &mut out);
        out.extend(&self.actor);
        leb(self.seq, &mut out);
        leb(self.start_op, &mut out);
        sleb(self.time, &mut out);
        leb(self.message.len() as u64, &mut out);
        out.extend(&self.message);
        leb(self.others.len() as u64, &mut out);
        for a in &self.others {
            leb(a.len() as u64, &mut out);
            out.extend(a);
        }
        out.extend(&self.rest);
        out
    }
    fn encode(&self) -> Vec<u8> {
        chunk(1, &self.body())
    }
}

fn hash_bytes(change: &Change) -> [u8; 32] {
    change.hash().0
}

/// Full-state snapshot for "unchanged on refusal": saved bytes including
/// queued orphans, plus heads and missing dependencies.
fn snapshot(doc: &mut AutoCommit) -> (Vec<u8>, Vec<automerge::ChangeHash>, Vec<automerge::ChangeHash>) {
    (
        doc.save_with_options(SaveOptions { deflate: false, retain_orphans: true }),
        doc.get_heads(),
        doc.get_missing_deps(&[]),
    )
}

/// Runs apply_incoming_checked without panicking; on refusal the document
/// must be unchanged. Returns the result.
fn incoming(
    doc: &mut AutoCommit,
    input: &[u8],
    limits: &NativeLimits,
    credit: &NativeAllowance,
    policy: NativeDependencyPolicy,
) -> Result<NativeIncomingOutcome, NativeMutationError> {
    let before = snapshot(doc);
    let result = catch_unwind(AssertUnwindSafe(|| doc.apply_incoming_checked(input, limits, credit, policy)))
        .unwrap_or_else(|_| panic!("apply_incoming_checked panicked on {} input bytes", input.len()));
    match &result {
        Err(_) | Ok(NativeIncomingOutcome::MissingDependencies) => {
            assert!(snapshot(doc) == before, "refused input mutated the document: {result:?}");
        }
        Ok(_) => {}
    }
    result
}

fn inspect(input: &[u8], limits: &NativeLimits) -> Result<(), NativeResourceError> {
    catch_unwind(AssertUnwindSafe(|| AutoCommit::inspect_bounded(input, limits, &mut allowance()).map(drop)))
        .expect("inspect_bounded panicked")
}

/// Whole checked pipeline on a fresh owner: normalize -> reconstruct.
fn reconstruct(input: &[u8], limits: &NativeLimits) -> Result<NativeOwned<AutoCommit>, NativeResourceError> {
    catch_unwind(AssertUnwindSafe(|| {
        let credit = allowance();
        let mut registry = NativeAllocationSet::new(credit.clone())?;
        let turn = registry.work_turn(credit)?;
        turn.normalize_input(input, limits)?.reconstruct(&mut registry, &turn)
    }))
    .expect("normalize_input/reconstruct panicked")
}

/// Every refusal path of the whole admission surface must agree on refusing
/// an input that inspection refuses, without panicking or mutating.
fn assert_refused_everywhere(input: &[u8], limits: &NativeLimits, expected: NativeResourceError) {
    let inspected = inspect(input, limits);
    assert!(inspected.as_ref().err() == Some(&expected), "inspect_bounded: {inspected:?}, expected {expected:?}");
    let credit = allowance();
    let registry = NativeAllocationSet::new(credit.clone()).unwrap();
    let turn = registry.work_turn(credit).unwrap();
    let normalized = catch_unwind(AssertUnwindSafe(|| turn.normalize_input(input, limits).map(drop)))
        .expect("normalize_input panicked");
    assert!(normalized.as_ref().err() == Some(&expected), "normalize_input: {normalized:?}, expected {expected:?}");
    let mut doc = seeded_doc();
    for policy in [NativeDependencyPolicy::RequireComplete, NativeDependencyPolicy::RetainOrphans] {
        let result = incoming(&mut doc, input, limits, &allowance(), policy);
        assert!(
            matches!(&result, Err(NativeMutationError::Resource(e)) if *e == expected),
            "apply_incoming_checked: {result:?}, expected {expected:?}"
        );
    }
}

fn seeded_doc() -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0x51; 16]));
    doc.put(ROOT, "seed", "resident").unwrap();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, 1_i64).unwrap();
    doc.commit_with(CommitOptions::default().with_time(1));
    doc
}

/// A small valid change chunk from a fresh actor.
fn simple_change(actor: u8, key: &str) -> Change {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![actor; 16]));
    doc.put(ROOT, key, 1_i64).unwrap();
    doc.commit_with(CommitOptions::default().with_time(2));
    doc.get_last_local_change().unwrap()
}

// ===========================================================================
// vendor08: verbatim qos-peer-state native_resource_vendor_tests.rs cases
// ===========================================================================
mod vendor08 {
    use super::*;

    // Catches an unchecked document-column inflater before native reconstruction.
    #[test]
    fn compressed_document_expansion_is_refused() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![7; 65_536])).unwrap();
        let encoded = doc.save();
        assert!(encoded.len() < 1_024);
        assert!(AutoCommit::load(&encoded).is_ok());
        let limits = NativeLimits { max_normalized_bytes: 1_024, ..NativeLimits::default() };
        assert!(matches!(
            AutoCommit::inspect_bounded(&encoded, &limits, &mut allowance()),
            Err(NativeResourceError::NormalizedBytes)
        ));
    }

    // Catches guarding document columns but leaving compressed change chunks open.
    #[test]
    fn compressed_change_expansion_is_refused() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![9; 65_536])).unwrap();
        doc.commit();
        let encoded = doc.get_last_local_change().unwrap().bytes().into_owned();
        assert_eq!(encoded[8], 2, "fixture must be a compressed change chunk");
        assert!(encoded.len() < 1_024);
        assert!(AutoCommit::load(&encoded).is_ok());
        let limits = NativeLimits { max_normalized_bytes: 1_024, ..NativeLimits::default() };
        assert!(matches!(
            AutoCommit::inspect_bounded(&encoded, &limits, &mut allowance()),
            Err(NativeResourceError::NormalizedBytes)
        ));
    }

    // Catches accepting compact/RLE native columns solely because their bytes fit.
    #[test]
    fn operation_cardinality_is_checked_before_reconstruction() {
        let mut doc = AutoCommit::new();
        for i in 0..65 {
            doc.put(ROOT, format!("key{i}"), 0_u64).unwrap();
        }
        let encoded = doc.save();
        assert!(AutoCommit::load(&encoded).is_ok());
        let limits = NativeLimits { max_operations: 64, ..NativeLimits::default() };
        assert!(matches!(
            AutoCommit::inspect_bounded(&encoded, &limits, &mut allowance()),
            Err(NativeResourceError::Operations)
        ));
    }

    // Catches silently disabling compressed input instead of bounding it.
    #[test]
    fn bounded_compressed_document_inspection_keeps_original_identity() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![5; 4_096])).unwrap();
        let encoded = doc.save();
        assert!(encoded.len() < 1_024);
        let checked = AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
        assert_eq!(checked.original_bytes(), encoded);
        assert_eq!(checked.shape().operations(), 1);
    }

    // Catches omitting the larger normalized column/header length encodings.
    #[test]
    fn normalized_size_includes_rewritten_framing() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![5; 65_536])).unwrap();
        let encoded = doc.save();
        let normalized = doc.save_nocompress();
        let checked = AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
        assert_eq!(checked.shape().normalized_bytes(), normalized.len());
        let exact = NativeLimits { max_normalized_bytes: normalized.len(), ..NativeLimits::default() };
        assert!(AutoCommit::inspect_bounded(&encoded, &exact, &mut allowance()).is_ok());
        let short = NativeLimits { max_normalized_bytes: normalized.len() - 1, ..exact };
        assert!(matches!(
            AutoCommit::inspect_bounded(&encoded, &short, &mut allowance()),
            Err(NativeResourceError::NormalizedBytes)
        ));
    }

    // Catches resetting aggregate limits at each concatenated chunk.
    #[test]
    fn concatenated_changes_share_operation_ceiling() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "a", 1_u64).unwrap();
        doc.commit();
        let mut encoded = doc.get_last_local_change().unwrap().bytes().into_owned();
        doc.put(ROOT, "b", 2_u64).unwrap();
        doc.commit();
        encoded.extend(doc.get_last_local_change().unwrap().bytes().as_ref());
        assert!(AutoCommit::load(&encoded).is_ok());
        let limits = NativeLimits { max_operations: 1, ..NativeLimits::default() };
        assert!(matches!(
            AutoCommit::inspect_bounded(&encoded, &limits, &mut allowance()),
            Err(NativeResourceError::Operations)
        ));
    }

    // Catches reconstructing a valid prefix while ignoring trailing native bytes.
    #[test]
    fn complete_native_input_is_required() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "a", 1_u64).unwrap();
        let mut encoded = doc.save();
        encoded.push(0);
        assert!(matches!(
            AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()),
            Err(NativeResourceError::Invalid)
        ));
    }

    // Catches independent allowance clones spending the same host reservation twice.
    #[test]
    fn retained_reservations_share_one_allowance_and_release_on_drop() {
        let allowance = NativeAllowance::new(4_096).unwrap();
        let child = allowance.clone();
        let retained = allowance.reserve(3_000).unwrap();
        assert_eq!(retained.bytes(), 3_000);
        assert!(matches!(child.reserve(2_000), Err(NativeResourceError::WorkBytes)));
        drop(allowance);
        assert!(matches!(child.reserve(2_000), Err(NativeResourceError::WorkBytes)));
        drop(retained);
        assert!(child.reserve(2_000).is_ok());
    }

    // Literal checksummed storage headers keep prefix/RLE attacks tiny while their
    // claimed native cardinality is enormous. No old unchecked decoder is invoked.
    #[test]
    fn literal_cardinality_bombs_are_refused() {
        for encoded in [
            "856f4a83c214676a0006808080808020",                 // 2^40 actors
            "856f4a8360ed8610000700808080808020",               // 2^40 heads
            "856f4a8397fa740b000400008101",                     // 129 columns
            "856f4a8326de2f6b000e0000000180010701808080808020", // 2^40 edges
        ] {
            assert_refused_everywhere(
                &hex::decode(encoded).unwrap(),
                &NativeLimits::default(),
                NativeResourceError::Collection,
            );
        }
        let encoded = hex::decode("856f4a831bfcd801000d00000001420780808080802001").unwrap();
        assert_refused_everywhere(&encoded, &NativeLimits::default(), NativeResourceError::Operations);
    }

    fn compressed_change_with_body(original: &[u8], body: &[u8]) -> Vec<u8> {
        // Keep the genuine checksum, so a checksum failure cannot mask a decoder
        // that incorrectly accepts an incomplete stream or trailing compressed data.
        let mut output = original[..9].to_vec();
        let mut len = body.len();
        while len >= 128 {
            output.push((len as u8 & 127) | 128);
            len >>= 7;
        }
        output.push(len as u8);
        output.extend(body);
        output
    }

    // Catches treating decoder EOF as a complete stream or hiding a second stream.
    #[test]
    fn malformed_deflate_must_not_reach_native_reconstruction() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![9; 65_536])).unwrap();
        doc.commit();
        let encoded = doc.get_last_local_change().unwrap().bytes().into_owned();
        assert_eq!(encoded[8], 2);
        assert!(AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).is_ok());
        let mut body_start = 9;
        while encoded[body_start] >= 128 {
            body_start += 1;
        }
        body_start += 1;
        let compressed = &encoded[body_start..];
        let truncated = compressed_change_with_body(&encoded, &compressed[..compressed.len() - 1]);
        let mut trailing = compressed.to_vec();
        trailing.extend(compressed);
        let trailing = compressed_change_with_body(&encoded, &trailing);
        for bytes in [&truncated, &trailing] {
            assert_refused_everywhere(bytes, &NativeLimits::default(), NativeResourceError::Invalid);
        }
    }

    // Catches leaving compressed columns for a later unchecked native inflater,
    // and releasing the normalized buffer's reservation before its owner dies.
    #[test]
    fn normalization_matches_native_storage_and_retains_its_credit() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![9; 65_536])).unwrap();
        let encoded = doc.save();
        let expected = doc.save_nocompress();
        let mut allowance = allowance();
        let checked = AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance).unwrap();
        let before = allowance.available_bytes();
        let normalized = assert_precharged(&allowance, || checked.normalize(&allowance).unwrap());
        assert_eq!(normalized.original_bytes(), encoded);
        assert_eq!(normalized.bytes(), expected);
        assert_eq!(normalized.shape().normalized_bytes(), expected.len());
        assert_eq!(allowance.available_bytes(), before - expected.len());
        let mut loaded = AutoCommit::load(normalized.bytes()).unwrap();
        assert_eq!(loaded.get_heads(), doc.get_heads());
        drop(normalized);
        assert_eq!(allowance.available_bytes(), before);
    }

    // Catches allocating normalized bytes before obtaining their complete charge.
    #[test]
    fn normalization_requires_full_output_credit_before_generation() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![9; 65_536])).unwrap();
        let encoded = doc.save();
        let checked = AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
        let small = NativeAllowance::new(1_024).unwrap();
        let before = small.available_bytes();
        let result = assert_precharged(&small, || checked.normalize(&small).map(drop));
        assert_eq!(result, Err(NativeResourceError::WorkBytes));
        assert_eq!(small.available_bytes(), before);
    }

    // Catches rewriting change bytes in a way that changes their cryptographic ID.
    #[test]
    fn normalization_preserves_compressed_change_identity() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![9; 65_536])).unwrap();
        doc.commit();
        let mut change = doc.get_last_local_change().unwrap();
        let expected = change.raw_bytes().to_vec();
        let encoded = change.bytes().into_owned();
        assert_eq!(encoded[8], 2);
        let checked = AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
        let normalized = checked.normalize(&allowance()).unwrap();
        assert_eq!(normalized.bytes(), expected);
        let mut restored = AutoCommit::load(normalized.bytes()).unwrap();
        assert_eq!(restored.get_heads(), doc.get_heads());
    }

    // Catches either rejecting valid bundle framing or rewriting bundle identity.
    #[test]
    fn bounded_bundle_inspection_and_normalization_remain_compatible() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "a", 1_u64).unwrap();
        doc.commit();
        doc.put(ROOT, "b", 2_u64).unwrap();
        doc.commit();
        let hashes = doc.get_changes(&[]).iter().map(|change| change.hash()).collect::<Vec<_>>();
        let bundle = doc.bundle(hashes).unwrap();
        assert_eq!(bundle.bytes()[8], 3);
        assert!(AutoCommit::load(bundle.bytes()).is_ok());
        let checked = AutoCommit::inspect_bounded(bundle.bytes(), &NativeLimits::default(), &mut allowance()).unwrap();
        assert_eq!(checked.shape().operations(), 2);
        let normalized = checked.normalize(&allowance()).unwrap();
        assert_eq!(normalized.bytes(), bundle.bytes());
    }

    // Catches undercounting or losing a later chunk while normalizing a document
    // followed by its retained incremental changes.
    #[test]
    fn document_and_incremental_change_normalize_as_one_complete_input() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "bytes", ScalarValue::Bytes(vec![9; 4_096])).unwrap();
        let mut encoded = doc.save();
        doc.put(ROOT, "more", ScalarValue::Bytes(vec![8; 4_096])).unwrap();
        doc.commit();
        encoded.extend(doc.get_last_local_change().unwrap().bytes().as_ref());
        let checked = AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
        let normalized = checked.normalize(&allowance()).unwrap();
        let mut restored = AutoCommit::load(normalized.bytes()).unwrap();
        assert_eq!(restored.get_heads(), doc.get_heads());
    }

    // A single change row can describe a billion dense collector slots.
    #[test]
    fn sparse_document_counter_is_refused_before_collector_allocation() {
        let bytes = hex::decode("856f4a83b40552a9001a0101000004010203021306400200010001010180808080040100").unwrap();
        assert_refused_everywhere(&bytes, &NativeLimits::default(), NativeResourceError::Operations);
    }

    // Each of four changes claims 64 operations with no dependencies.
    #[test]
    fn dense_collector_slots_share_the_operation_ceiling() {
        let bytes = hex::decode("856f4a8399cdc86e001a0101000004010203021306400200040004017cc0000000000400").unwrap();
        let limits = NativeLimits { max_operations: 128, ..NativeLimits::default() };
        assert_refused_everywhere(&bytes, &limits, NativeResourceError::Operations);
    }

    // Dependency indexes must refer to preceding change rows.
    #[test]
    fn out_of_range_document_dependency_is_invalid_before_reconstruction() {
        let bytes = hex::decode("856f4a83e9e773df001a0101000005010203021302400243020001000101010101010102").unwrap();
        assert_refused_everywhere(&bytes, &NativeLimits::default(), NativeResourceError::Invalid);
    }

    // From native_footprint_vendor_tests: malformed graph rows must not panic
    // in plain load either, and must be refused by the checked path.
    #[test]
    fn malformed_graph_rows_are_refused_without_panicking() {
        for encoded in [
            "856f4a83e9e773df001a0101000005010203021302400243020001000101010101010102",
            "856f4a830501ac78001601010000040102030213024002000100010101010200",
        ] {
            let bytes = hex::decode(encoded).unwrap();
            let loaded = catch_unwind(|| AutoCommit::load(&bytes));
            assert!(loaded.is_ok(), "plain load panicked on {encoded}");
            assert!(loaded.unwrap().is_err());
            assert!(inspect(&bytes, &NativeLimits::default()).is_err());
            assert!(reconstruct(&bytes, &NativeLimits::default()).is_err());
        }
    }

    #[test]
    fn standalone_change_prefix_dependencies_obey_edge_quota() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "first", 1_u64).unwrap();
        doc.commit();
        doc.put(ROOT, "second", 2_u64).unwrap();
        doc.commit();
        let change = doc.get_last_local_change().unwrap();
        assert_eq!(change.deps().len(), 1);
        let encoded = change.raw_bytes();
        let exact = NativeLimits { max_edges: 1, ..NativeLimits::default() };
        assert!(AutoCommit::inspect_bounded(encoded, &exact, &mut allowance()).is_ok());
        let short = NativeLimits { max_edges: 0, ..exact };
        assert_refused_everywhere(encoded, &short, NativeResourceError::Collection);
    }

    #[test]
    fn standalone_change_prefix_messages_share_value_quota() {
        let mut doc = AutoCommit::new();
        doc.empty_change(CommitOptions::default().with_message("one"));
        let mut encoded = doc.get_last_local_change().unwrap().raw_bytes().to_vec();
        doc.empty_change(CommitOptions::default().with_message("two"));
        encoded.extend_from_slice(doc.get_last_local_change().unwrap().raw_bytes());
        let exact = NativeLimits { max_value_bytes: 6, ..NativeLimits::default() };
        assert!(AutoCommit::inspect_bounded(&encoded, &exact, &mut allowance()).is_ok());
        let short = NativeLimits { max_value_bytes: 5, ..exact };
        assert_refused_everywhere(&encoded, &short, NativeResourceError::ValueBytes);
    }

    #[test]
    fn bundle_prefix_dependencies_obey_edge_quota() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "first", 1_u64).unwrap();
        doc.commit();
        doc.put(ROOT, "second", 2_u64).unwrap();
        let second = doc.commit().unwrap();
        let bundle = doc.bundle(vec![second]).unwrap();
        assert_eq!(bundle.deps().len(), 1);
        let exact = NativeLimits { max_edges: 1, ..NativeLimits::default() };
        assert!(AutoCommit::inspect_bounded(bundle.bytes(), &exact, &mut allowance()).is_ok());
        let short = NativeLimits { max_edges: 0, ..exact };
        assert_refused_everywhere(bundle.bytes(), &short, NativeResourceError::Collection);
    }

    #[test]
    fn finished_turn_releases_reservations_without_reopening_work() {
        let allowance = NativeAllowance::new(4_096).unwrap();
        let metadata = allowance.retained_bytes();
        let retained = allowance.reserve(1_024).unwrap();
        allowance.finish_turn();
        assert!(matches!(allowance.reserve(1), Err(NativeResourceError::WorkBytes)));
        assert_eq!(allowance.retained_bytes(), metadata + 1_024);
        drop(retained);
        assert_eq!(allowance.retained_bytes(), metadata);
        assert!(matches!(allowance.reserve(1), Err(NativeResourceError::WorkBytes)));
    }
}

// ===========================================================================
// wire: crafted framing / LEB128 / columns / chunks / cardinality / bombs
// ===========================================================================
mod wire {
    use super::*;

    /// A change body with explicit op columns: (spec, data).
    fn change_body(deps: u64, actors: &[&[u8]], columns: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        leb(deps, &mut out);
        // no dep hashes written: callers use deps == 0 unless testing counts
        leb(16, &mut out);
        out.extend([0xab; 16]);
        leb(1, &mut out); // seq
        leb(1, &mut out); // start op
        sleb(0, &mut out); // time
        leb(0, &mut out); // message
        leb(actors.len() as u64, &mut out);
        for a in actors {
            leb(a.len() as u64, &mut out);
            out.extend(*a);
        }
        leb(columns.len() as u64, &mut out);
        for (spec, data) in columns {
            leb(u64::from(*spec), &mut out);
            leb(data.len() as u64, &mut out);
        }
        for (_, data) in columns {
            out.extend(data);
        }
        out
    }

    #[test]
    fn empty_and_garbage_inputs() {
        // Empty input is an empty (valid) storage sequence for inspection.
        let _ = inspect(&[], &NativeLimits::default());
        for input in [&b"\x00"[..], &MAGIC[..], &b"not automerge at all"[..], &[0xff; 64][..]] {
            assert_refused_everywhere(input, &NativeLimits::default(), NativeResourceError::Invalid);
        }
    }

    #[test]
    fn unknown_chunk_kind_is_invalid() {
        for kind in [4u8, 5, 0x7f, 0xff] {
            assert_refused_everywhere(&chunk(kind, &[0]), &NativeLimits::default(), NativeResourceError::Invalid);
        }
    }

    #[test]
    fn leb128_lengths_near_u64_max_are_refused() {
        let limits = NativeLimits::default();
        // Chunk length u64::MAX and u64::MAX - 1: more than the input.
        for n in [u64::MAX, u64::MAX - 1, 1 << 40] {
            let mut input = MAGIC.to_vec();
            input.extend([0; 4]);
            input.push(1);
            leb(n, &mut input);
            input.extend([0; 8]);
            assert_refused_everywhere(&input, &limits, NativeResourceError::Invalid);
        }
        // Dependency count near u64::MAX in a correctly checksummed change.
        let body = change_body(u64::MAX - 1, &[], &[]);
        assert_refused_everywhere(&chunk(1, &body), &limits, NativeResourceError::Collection);
        // Other-actor count near u64::MAX.
        let mut body = Vec::new();
        leb(0, &mut body);
        leb(1, &mut body);
        body.push(1);
        leb(1, &mut body);
        leb(1, &mut body);
        sleb(0, &mut body);
        leb(0, &mut body);
        leb(u64::MAX, &mut body);
        assert_refused_everywhere(&chunk(1, &body), &limits, NativeResourceError::Collection);
        // Column count near u64::MAX.
        let mut body = change_body(0, &[], &[]);
        body.pop();
        leb(u64::MAX, &mut body);
        assert_refused_everywhere(&chunk(1, &body), &limits, NativeResourceError::Collection);
        // A column length near u64::MAX (beyond the input).
        let mut body = change_body(0, &[], &[]);
        body.pop();
        leb(1, &mut body);
        leb(1 << 4 | 2, &mut body);
        leb(u64::MAX, &mut body);
        assert_refused_everywhere(&chunk(1, &body), &limits, NativeResourceError::Invalid);
    }

    #[test]
    fn overlong_leb128_is_refused() {
        let limits = NativeLimits::default();
        // 11-byte LEB128 (overflows u64) as the chunk length.
        let mut input = MAGIC.to_vec();
        input.extend([0; 4]);
        input.push(1);
        input.extend([0x80; 10]);
        input.push(0x01);
        assert_refused_everywhere(&input, &limits, NativeResourceError::Invalid);
        // Unterminated LEB128 at end of input.
        let mut input = MAGIC.to_vec();
        input.extend([0; 4]);
        input.push(1);
        input.extend([0x80; 3]);
        assert_refused_everywhere(&input, &limits, NativeResourceError::Invalid);
        // Overflowing LEB128 inside a checksummed change body (dep count).
        let mut body = vec![0xff; 10];
        body.push(0x7f);
        assert_refused_everywhere(&chunk(1, &body), &limits, NativeResourceError::Invalid);
    }

    /// Non-minimal (padded) LEB128 inside a genuine change: whatever the
    /// checked path decides, it must not panic, and it must agree with
    /// itself (inspection acceptance implies no later panic or mutation).
    #[test]
    fn padded_leb128_inside_a_change_is_handled_consistently() {
        let change = simple_change(0x21, "padded");
        let parts = ChangeParts::parse(change.raw_bytes());
        let mut body = parts.body();
        // Re-encode seq (value 1) as 0x81 0x00.
        let seq_at = 1 + 1 + parts.actor.len();
        assert_eq!(body[seq_at], 1);
        body.splice(seq_at..seq_at + 1, [0x81, 0x00]);
        let input = chunk(1, &body);
        let mut doc = seeded_doc();
        for policy in [NativeDependencyPolicy::RequireComplete, NativeDependencyPolicy::RetainOrphans] {
            let _ = incoming(&mut doc, &input, &NativeLimits::default(), &allowance(), policy);
        }
        let _ = reconstruct(&input, &NativeLimits::default());
    }

    #[test]
    fn column_data_beyond_input_is_invalid() {
        let body = change_body(0, &[], &[(1 << 4 | 2, vec![1, 0])]);
        let mut shortened = body.clone();
        shortened.pop(); // declared 2 bytes, 1 present
        assert_refused_everywhere(&chunk(1, &shortened), &NativeLimits::default(), NativeResourceError::Invalid);
    }

    #[test]
    fn more_than_128_columns_is_refused() {
        let columns: Vec<(u32, Vec<u8>)> = (0..129u32).map(|i| ((i + 1) << 4 | 2, Vec::new())).collect();
        let body = change_body(0, &[], &columns);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Collection);
        // Tighter caller limit.
        let columns: Vec<(u32, Vec<u8>)> = (0..9u32).map(|i| ((i + 1) << 4 | 2, Vec::new())).collect();
        let body = change_body(0, &[], &columns);
        let limits = NativeLimits { max_columns: 8, ..NativeLimits::default() };
        assert_refused_everywhere(&chunk(1, &body), &limits, NativeResourceError::Collection);
        // A policy above the format maximum is itself refused.
        let limits = NativeLimits { max_columns: 129, ..NativeLimits::default() };
        assert_eq!(inspect(&simple_change(1, "k").raw_bytes().to_vec(), &limits), Err(NativeResourceError::Collection));
    }

    #[test]
    fn unsorted_or_duplicate_column_specs_are_invalid() {
        let body = change_body(0, &[], &[(2 << 4 | 2, vec![]), (1 << 4 | 2, vec![])]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Invalid);
        let body = change_body(0, &[], &[(1 << 4 | 2, vec![]), (1 << 4 | 2, vec![])]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Invalid);
    }

    #[test]
    fn compressed_columns_inside_change_chunks_are_invalid() {
        // Change chunks may not carry per-column DEFLATE (spec bit 3).
        let data = deflate(&[1, 0]);
        let body = change_body(0, &[], &[(1 << 4 | 8 | 2, data)]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Invalid);
    }

    #[test]
    fn more_chunks_than_max_chunks_is_refused() {
        let one = simple_change(0x31, "k").raw_bytes().to_vec();
        let limits = NativeLimits::default();
        let many: Vec<u8> = std::iter::repeat_n(one.as_slice(), limits.max_chunks + 1).flatten().copied().collect();
        assert_refused_everywhere(&many, &limits, NativeResourceError::Collection);
        let tight = NativeLimits { max_chunks: 2, ..NativeLimits::default() };
        let three: Vec<u8> = std::iter::repeat_n(one.as_slice(), 3).flatten().copied().collect();
        assert_refused_everywhere(&three, &tight, NativeResourceError::Collection);
        let two: Vec<u8> = std::iter::repeat_n(one.as_slice(), 2).flatten().copied().collect();
        assert_eq!(inspect(&two, &tight), Ok(()));
    }

    #[test]
    fn actor_and_head_counts_over_limits_are_refused() {
        // 1025 real one-byte actors in a change.
        let actors: Vec<Vec<u8>> = (0..1025u32).map(|i| vec![(i % 251) as u8]).collect();
        let refs: Vec<&[u8]> = actors.iter().map(Vec::as_slice).collect();
        let body = change_body(0, &refs, &[]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Collection);
        // Actors are counted across chunks, including repeats.
        let one = simple_change(0x41, "k").raw_bytes().to_vec();
        let limits = NativeLimits { max_actors: 3, ..NativeLimits::default() };
        let four: Vec<u8> = std::iter::repeat_n(one.as_slice(), 4).flatten().copied().collect();
        assert_refused_everywhere(&four, &limits, NativeResourceError::Collection);
        // Heads in a document chunk.
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "a", 1).unwrap();
        doc.commit();
        let mut other = doc.fork();
        other.put(ROOT, "b", 2).unwrap();
        other.commit();
        doc.put(ROOT, "c", 3).unwrap();
        doc.commit();
        doc.merge(&mut other).unwrap();
        assert_eq!(doc.get_heads().len(), 2);
        let saved = doc.save();
        let limits = NativeLimits { max_heads: 1, ..NativeLimits::default() };
        assert_refused_everywhere(&saved, &limits, NativeResourceError::Collection);
        assert_eq!(inspect(&saved, &NativeLimits { max_heads: 2, ..NativeLimits::default() }), Ok(()));
    }

    #[test]
    fn op_counts_over_limits_via_long_rle_runs() {
        // One RLE run of 2^20 ops in a handful of bytes (uint column).
        let mut run = Vec::new();
        sleb(1 << 20, &mut run);
        leb(0, &mut run);
        let body = change_body(0, &[], &[(1 << 4 | 2, run)]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Operations);
        // Null run (run 0, count 2^40).
        let mut run = Vec::new();
        sleb(0, &mut run);
        leb(1 << 40, &mut run);
        let body = change_body(0, &[], &[(1 << 4 | 2, run)]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Operations);
        // Boolean column (kind 4) run near usize::MAX.
        let mut run = Vec::new();
        leb(u64::MAX >> 1, &mut run);
        let body = change_body(0, &[], &[(1 << 4 | 4, run)]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::Operations);
        // A value run whose repeated string would exceed max_value_bytes.
        let mut run = Vec::new();
        sleb(60_000, &mut run);
        leb(200, &mut run);
        run.extend([b'x'; 200]);
        let body = change_body(0, &[], &[(1 << 4 | 5, run)]);
        assert_refused_everywhere(&chunk(1, &body), &NativeLimits::default(), NativeResourceError::ValueBytes);
        // Real ops: 65_537 puts in one change (valid encoding, too many ops).
        if slow() {
            let mut doc = AutoCommit::new();
            let list = doc.put_object(ROOT, "l", ObjType::List).unwrap();
            for i in 0..65_537usize {
                doc.insert(&list, i, 0_i64).unwrap();
            }
            doc.commit();
            let change = doc.get_last_local_change().unwrap();
            assert_refused_everywhere(change.raw_bytes(), &NativeLimits::default(), NativeResourceError::Operations);
        }
    }

    #[test]
    fn truncated_chunks_are_refused_at_every_length() {
        let mut doc = seeded_doc();
        doc.put(ROOT, "more", ScalarValue::Bytes(vec![3; 300])).unwrap();
        doc.commit();
        let change = doc.get_last_local_change().unwrap().raw_bytes().to_vec();
        let saved = doc.save();
        for full in [&change, &saved] {
            for cut in 1..full.len() {
                let prefix = &full[..cut];
                assert!(inspect(prefix, &NativeLimits::default()).is_err(), "prefix {cut} accepted");
                let mut resident = seeded_doc();
                let result = incoming(
                    &mut resident,
                    prefix,
                    &NativeLimits::default(),
                    &allowance(),
                    NativeDependencyPolicy::RetainOrphans,
                );
                assert!(result.is_err(), "prefix {cut} applied: {result:?}");
            }
        }
    }

    #[test]
    fn bad_checksums_are_refused() {
        let change = simple_change(0x61, "sum").raw_bytes().to_vec();
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "k", "v").unwrap();
        let saved = doc.save();
        for good in [&change, &saved] {
            for at in [4usize, 7] {
                let mut bad = good.clone();
                bad[at] ^= 0x01;
                assert_refused_everywhere(&bad, &NativeLimits::default(), NativeResourceError::Invalid);
            }
            // A body byte flipped under an unchanged checksum.
            let mut bad = good.clone();
            let last = bad.len() - 1;
            bad[last] ^= 0x40;
            assert_refused_everywhere(&bad, &NativeLimits::default(), NativeResourceError::Invalid);
        }
    }

    #[test]
    fn bad_magic_is_refused() {
        let mut change = simple_change(0x62, "m").raw_bytes().to_vec();
        change[0] = 0x84;
        assert_refused_everywhere(&change, &NativeLimits::default(), NativeResourceError::Invalid);
    }

    /// Document chunk whose deflated op column inflates to 16 MiB from a few
    /// KiB. Refused as NormalizedBytes, and the refusal allocates nothing
    /// proportional to the inflated size: peak stays under a small multiple
    /// of the input plus fixed inflate scratch, all of it pre-reserved.
    #[test]
    fn document_column_compression_bomb_is_refused_without_inflating() {
        const INFLATED: usize = 16 << 20;
        let bomb = zero_bomb(INFLATED);
        assert!(bomb.len() < 64 * 1024, "bomb is {} bytes", bomb.len());
        // Take a real document and replace its first op column with the bomb.
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "k", ScalarValue::Bytes(vec![1; 4096])).unwrap();
        let input = replace_first_doc_column_with(&doc.save_nocompress(), &bomb);
        bomb_oracle(&input, INFLATED);
    }

    /// Compressed change chunk (kind 2) inflating to 16 MiB.
    #[test]
    fn change_chunk_compression_bomb_is_refused_without_inflating() {
        const INFLATED: usize = 16 << 20;
        let bomb = zero_bomb(INFLATED);
        let input = chunk_with_checksum(2, &bomb, [0; 4]);
        bomb_oracle(&input, INFLATED);
    }

    fn bomb_oracle(input: &[u8], inflated: usize) {
        let limits = NativeLimits::default();
        let credit = allowance();
        let (result, allocations) = measure_under(&credit, || {
            AutoCommit::inspect_bounded(input, &limits, &mut credit.clone()).map(drop)
        });
        assert_eq!(result, Err(NativeResourceError::NormalizedBytes));
        let ceiling = 4 * input.len() + 256 * 1024;
        assert!(
            allocations.peak <= ceiling && allocations.peak < inflated / 16,
            "inspection peak {} (largest request {}) for {} input bytes inflating to {inflated}",
            allocations.peak,
            allocations.largest_request,
            input.len()
        );
        // The same through the owner turn and incoming application.
        let registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let (result, allocations) = measure_under(&credit, || turn.normalize_input(input, &limits).map(drop));
        assert_eq!(result, Err(NativeResourceError::NormalizedBytes));
        assert!(allocations.peak <= ceiling, "normalize_input peak {}", allocations.peak);
        let mut doc = seeded_doc();
        let before = snapshot(&mut doc);
        let (result, allocations) = measure_under(&credit, || {
            catch_unwind(AssertUnwindSafe(|| {
                doc.apply_incoming_checked(input, &limits, &credit, NativeDependencyPolicy::RetainOrphans)
            }))
        });
        let result = result.expect("apply_incoming_checked panicked on a bomb");
        assert!(matches!(result, Err(NativeMutationError::Resource(NativeResourceError::NormalizedBytes))), "{result:?}");
        assert!(allocations.peak <= ceiling, "apply_incoming_checked peak {}", allocations.peak);
        assert!(snapshot(&mut doc) == before);
    }

    /// Rewrites a document chunk so its first op column holds `data` with
    /// the DEFLATE bit set; the chunk checksum is recomputed.
    fn replace_first_doc_column_with(saved: &[u8], data: &[u8]) -> Vec<u8> {
        assert_eq!(saved[8], 0);
        let mut b = &saved[9..];
        let _len = read_uint(&mut b);
        let body = b;
        let mut r = body;
        let nactors = read_uint(&mut r);
        for _ in 0..nactors {
            read_blob(&mut r);
        }
        let nheads = read_uint(&mut r) as usize;
        r = &r[nheads * 32..];
        let prefix_len = body.len() - r.len();
        let read_meta = |r: &mut &[u8]| -> Vec<(u64, usize)> {
            let n = read_uint(r);
            (0..n).map(|_| (read_uint(r), read_uint(r) as usize)).collect()
        };
        let change_meta = read_meta(&mut r);
        let op_meta = read_meta(&mut r);
        let change_data_len: usize = change_meta.iter().map(|c| c.1).sum();
        let op_data_len: usize = op_meta.iter().map(|c| c.1).sum();
        let change_data = &r[..change_data_len];
        let op_data = &r[change_data_len..change_data_len + op_data_len];
        let suffix = &r[change_data_len + op_data_len..];
        let mut new_op_meta = op_meta.clone();
        new_op_meta[0] = (op_meta[0].0 | 8, data.len());
        let mut new_op_data = data.to_vec();
        new_op_data.extend(&op_data[op_meta[0].1..]);
        let mut out = body[..prefix_len].to_vec();
        for meta in [&change_meta, &new_op_meta] {
            leb(meta.len() as u64, &mut out);
            for (spec, len) in meta.iter() {
                leb(*spec, &mut out);
                leb(*len as u64, &mut out);
            }
        }
        out.extend(change_data);
        out.extend(new_op_data);
        out.extend(suffix);
        chunk(0, &out)
    }

    /// A small, genuinely compressed document column that stays within the
    /// ceiling is accepted, normalized to the uncompressed save exactly, and
    /// the whole normalize is pre-reserved.
    #[test]
    fn benign_compressed_column_normalizes_under_reservation() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "k", ScalarValue::Bytes(vec![1; 40_000])).unwrap();
        let saved = doc.save();
        let plain = doc.save_nocompress();
        let credit = allowance();
        let registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let (normalized, allocations) =
            measure_under(&credit, || turn.normalize_input(&saved, &NativeLimits::default()).unwrap());
        assert_eq!(normalized.bytes(), plain);
        assert!(allocations.peak <= plain.len() + saved.len() + 256 * 1024, "peak {}", allocations.peak);
    }
}

// ===========================================================================
// semantic: forged changes, duplicate sequences, missing dependencies
// ===========================================================================
mod semantic {
    use super::*;

    fn policies() -> [NativeDependencyPolicy; 2] {
        [NativeDependencyPolicy::RequireComplete, NativeDependencyPolicy::RetainOrphans]
    }

    fn refused_semantically(result: &Result<NativeIncomingOutcome, NativeMutationError>) -> bool {
        matches!(
            result,
            Err(NativeMutationError::InvalidOperation)
                | Err(NativeMutationError::InvalidTarget)
                | Err(NativeMutationError::Resource(NativeResourceError::Invalid))
        )
    }

    /// Applies through the owned path: reconstruct `resident`, clone it as a
    /// disposable candidate, apply, and check the original is unchanged.
    fn owned_apply(
        resident: &[u8],
        input: &[u8],
        policy: NativeDependencyPolicy,
    ) -> Result<NativeApplyResult, NativeMutationError> {
        catch_unwind(AssertUnwindSafe(|| {
            let credit = allowance();
            let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
            let turn = registry.work_turn(credit.clone()).unwrap();
            let owned = turn.normalize_input(resident, &NativeLimits::default()).unwrap().reconstruct(&mut registry, &turn).unwrap();
            let before = owned.save_bounded(&turn, SaveOptions::default()).unwrap().bytes().to_vec();
            let candidate = owned.clone_bounded(&mut registry, &turn).unwrap();
            let normalized = turn.normalize_input(input, &NativeLimits::default())?;
            let result = assert_precharged(&credit, || candidate.apply_checked_bounded(normalized, policy, &mut registry, &turn));
            let after = owned.save_bounded(&turn, SaveOptions::default()).unwrap().bytes().to_vec();
            assert!(before == after, "the original owned document changed");
            result
        }))
        .expect("apply_checked_bounded panicked")
    }

    /// Resident: seeded. Incoming: an op on an object this document has never
    /// seen (dependencies stripped, so the change is causally ready).
    #[test]
    fn op_on_missing_object_is_refused() {
        let mut remote = AutoCommit::new().with_actor(ActorId::from(vec![0x71; 16]));
        let map = remote.put_object(ROOT, "m", ObjType::Map).unwrap();
        remote.commit();
        remote.put(&map, "k", "v").unwrap();
        remote.commit();
        let mut parts = ChangeParts::parse(remote.get_last_local_change().unwrap().raw_bytes());
        parts.deps.clear();
        let forged = parts.encode();
        let mut doc = seeded_doc();
        let resident = doc.save();
        for policy in policies() {
            let result = incoming(&mut doc, &forged, &NativeLimits::default(), &allowance(), policy);
            assert!(refused_semantically(&result), "{policy:?}: {result:?}");
            let owned = owned_apply(&resident, &forged, policy);
            assert!(owned.is_err(), "{policy:?}: owned {owned:?}");
        }
    }

    /// Predecessor referencing an op that does not exist.
    #[test]
    fn pred_to_missing_op_is_refused() {
        let mut remote = AutoCommit::new().with_actor(ActorId::from(vec![0x72; 16]));
        remote.put(ROOT, "k", 1).unwrap();
        remote.commit();
        remote.put(ROOT, "k", 2).unwrap(); // pred = 1@72
        remote.commit();
        let mut parts = ChangeParts::parse(remote.get_last_local_change().unwrap().raw_bytes());
        parts.deps.clear();
        let forged = parts.encode();
        let mut doc = seeded_doc();
        let resident = doc.save();
        for policy in policies() {
            let result = incoming(&mut doc, &forged, &NativeLimits::default(), &allowance(), policy);
            assert!(refused_semantically(&result), "{policy:?}: {result:?}");
            assert!(owned_apply(&resident, &forged, policy).is_err());
        }
    }

    /// Object is a List in the resident document, incoming op addresses it
    /// with a map key (dependencies forged onto the list-creating change).
    #[test]
    fn map_key_on_list_object_is_refused() {
        let actor = ActorId::from(vec![0x73; 16]);
        let mut list_side = AutoCommit::new().with_actor(actor.clone());
        list_side.put_object(ROOT, "x", ObjType::List).unwrap();
        list_side.commit_with(CommitOptions::default().with_time(5));
        let list_change = list_side.get_last_local_change().unwrap();
        let mut map_side = AutoCommit::new().with_actor(actor);
        let map = map_side.put_object(ROOT, "x", ObjType::Map).unwrap();
        map_side.commit_with(CommitOptions::default().with_time(6));
        map_side.put(&map, "key", "on a list").unwrap();
        map_side.commit();
        let mut parts = ChangeParts::parse(map_side.get_last_local_change().unwrap().raw_bytes());
        parts.deps = vec![hash_bytes(&list_change)];
        let forged = parts.encode();
        let mut doc = AutoCommit::new();
        doc.apply_changes([list_change]).unwrap();
        let resident = doc.save();
        for policy in policies() {
            let result = incoming(&mut doc, &forged, &NativeLimits::default(), &allowance(), policy);
            assert!(refused_semantically(&result), "{policy:?}: {result:?}");
            assert!(owned_apply(&resident, &forged, policy).is_err());
        }
        // Plain 0.12 apply on the same input must at least not panic (report only).
        let mut plain = AutoCommit::load(&resident).unwrap();
        let plain_result = catch_unwind(AssertUnwindSafe(|| plain.load_incremental(&forged)));
        println!("plain 0.12 load_incremental of map key on list: panicked={}", plain_result.is_err());
    }

    /// Elemid-keyed insert into a Map object.
    #[test]
    fn elem_key_on_map_object_is_refused() {
        let actor = ActorId::from(vec![0x74; 16]);
        let mut map_side = AutoCommit::new().with_actor(actor.clone());
        map_side.put_object(ROOT, "x", ObjType::Map).unwrap();
        map_side.commit_with(CommitOptions::default().with_time(5));
        let map_change = map_side.get_last_local_change().unwrap();
        let mut list_side = AutoCommit::new().with_actor(actor);
        let list = list_side.put_object(ROOT, "x", ObjType::List).unwrap();
        list_side.commit_with(CommitOptions::default().with_time(6));
        list_side.insert(&list, 0, "into a map").unwrap();
        list_side.commit();
        let mut parts = ChangeParts::parse(list_side.get_last_local_change().unwrap().raw_bytes());
        parts.deps = vec![hash_bytes(&map_change)];
        let forged = parts.encode();
        let mut doc = AutoCommit::new();
        doc.apply_changes([map_change]).unwrap();
        for policy in policies() {
            let result = incoming(&mut doc, &forged, &NativeLimits::default(), &allowance(), policy);
            assert!(refused_semantically(&result), "{policy:?}: {result:?}");
        }
    }

    /// The change's op columns reference actor index 1, but the forged
    /// actor table no longer has a second actor.
    #[test]
    fn actor_index_out_of_range_is_refused() {
        let mut base = AutoCommit::new().with_actor(ActorId::from(vec![0x75; 16]));
        base.put(ROOT, "k", 1).unwrap();
        base.commit();
        let base_change = base.get_last_local_change().unwrap();
        let mut other = base.fork().with_actor(ActorId::from(vec![0x76; 16]));
        other.put(ROOT, "k", 2).unwrap(); // pred 1@75 -> actor index 1
        other.commit();
        let mut parts = ChangeParts::parse(other.get_last_local_change().unwrap().raw_bytes());
        assert_eq!(parts.others.len(), 1);
        parts.others.clear();
        let forged = parts.encode();
        let mut doc = AutoCommit::new();
        doc.apply_changes([base_change]).unwrap();
        for policy in policies() {
            let result = incoming(&mut doc, &forged, &NativeLimits::default(), &allowance(), policy);
            assert!(result.is_err(), "{policy:?}: {result:?}");
        }
    }

    /// start_op so large that start_op + ops overflows the counter ceiling.
    #[test]
    fn huge_start_op_is_refused() {
        let mut parts = ChangeParts::parse(simple_change(0x77, "k").raw_bytes());
        parts.start_op = u64::MAX - 1;
        let mut doc = seeded_doc();
        let result = incoming(&mut doc, &parts.encode(), &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        assert!(result.is_err(), "{result:?}");
        parts.start_op = 0;
        let result = incoming(&mut doc, &parts.encode(), &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        assert!(result.is_err(), "start_op 0: {result:?}");
    }

    /// seq 0 and a seq gap for a known actor.
    #[test]
    fn inconsistent_sequence_numbers_do_not_corrupt() {
        let mut doc = seeded_doc();
        let mut parts = ChangeParts::parse(simple_change(0x78, "k").raw_bytes());
        parts.seq = 0;
        let _ = incoming(&mut doc, &parts.encode(), &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        let mut parts = ChangeParts::parse(simple_change(0x79, "k").raw_bytes());
        parts.seq = 5; // no deps, seq gap
        let _ = incoming(&mut doc, &parts.encode(), &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        // Whatever was admitted, the document still saves and reloads.
        let saved = doc.save();
        AutoCommit::load(&saved).unwrap();
    }

    #[test]
    fn duplicate_sequence_same_content_is_deduplicated() {
        let mut doc = seeded_doc();
        let change = simple_change(0x81, "dup");
        let bytes = change.raw_bytes().to_vec();
        for policy in policies() {
            let first = incoming(&mut doc, &bytes, &NativeLimits::default(), &allowance(), policy).unwrap();
            let before = snapshot(&mut doc);
            let again = incoming(&mut doc, &bytes, &NativeLimits::default(), &allowance(), policy).unwrap();
            assert_eq!(again, NativeIncomingOutcome::Applied { changed: false }, "{policy:?} after {first:?}");
            assert!(snapshot(&mut doc) == before);
        }
        // The same change twice in one input.
        let mut fresh = seeded_doc();
        let twice = [bytes.clone(), bytes].concat();
        let outcome = incoming(&mut fresh, &twice, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RequireComplete).unwrap();
        assert_eq!(outcome, NativeIncomingOutcome::Applied { changed: true });
    }

    fn same_seq_different_content(actor: u8) -> (Change, Change) {
        let a = ActorId::from(vec![actor; 16]);
        let mut one = AutoCommit::new().with_actor(a.clone());
        one.put(ROOT, "k", "first").unwrap();
        one.commit_with(CommitOptions::default().with_time(1));
        let mut two = AutoCommit::new().with_actor(a);
        two.put(ROOT, "k", "second").unwrap();
        two.commit_with(CommitOptions::default().with_time(1));
        let (c1, c2) = (one.get_last_local_change().unwrap(), two.get_last_local_change().unwrap());
        assert_eq!((c1.seq(), c2.seq()), (1, 1));
        assert_ne!(c1.hash(), c2.hash());
        (c1, c2)
    }

    #[test]
    fn duplicate_sequence_different_content_resident_is_refused() {
        let (c1, c2) = same_seq_different_content(0x82);
        let mut doc = seeded_doc();
        doc.apply_changes([c1]).unwrap();
        let resident = doc.save();
        for policy in policies() {
            let result = incoming(&mut doc, c2.raw_bytes(), &NativeLimits::default(), &allowance(), policy);
            assert!(matches!(result, Err(NativeMutationError::DuplicateSequence)), "{policy:?}: {result:?}");
            let owned = owned_apply(&resident, c2.raw_bytes(), policy);
            assert!(matches!(owned, Err(NativeMutationError::DuplicateSequence)), "{policy:?}: owned {owned:?}");
        }
    }

    #[test]
    fn duplicate_sequence_different_content_within_one_input_is_refused() {
        let (c1, c2) = same_seq_different_content(0x83);
        let input = [c1.raw_bytes(), c2.raw_bytes()].concat();
        let mut doc = seeded_doc();
        for policy in policies() {
            let result = incoming(&mut doc, &input, &NativeLimits::default(), &allowance(), policy);
            assert!(matches!(result, Err(NativeMutationError::DuplicateSequence)), "{policy:?}: {result:?}");
        }
    }

    /// Queued duplicate: seq 2 of an actor is queued (its seq 1 is missing);
    /// a different seq 2 arrives. 0.12's native path removes the queued branch
    /// before failing (the quirk Layer D must pre-empt).
    #[test]
    fn duplicate_sequence_different_content_queued_is_refused_without_touching_the_queue() {
        let a = ActorId::from(vec![0x84; 16]);
        let mut src = AutoCommit::new().with_actor(a.clone());
        src.put(ROOT, "base", 1).unwrap();
        src.commit_with(CommitOptions::default().with_time(1));
        let base = src.get_last_local_change().unwrap();
        let mut left = src.fork().with_actor(a.clone());
        left.put(ROOT, "k", "left").unwrap();
        left.commit_with(CommitOptions::default().with_time(2));
        let mut right = src.fork().with_actor(a);
        right.put(ROOT, "k", "right").unwrap();
        right.commit_with(CommitOptions::default().with_time(3));
        let (l2, r2) = (left.get_last_local_change().unwrap(), right.get_last_local_change().unwrap());
        assert_eq!((l2.seq(), r2.seq()), (2, 2));
        let mut doc = seeded_doc();
        doc.apply_changes([l2.clone()]).unwrap(); // queued: base missing
        assert_eq!(doc.get_missing_deps(&[]), vec![base.hash()]);
        let result = incoming(&mut doc, r2.raw_bytes(), &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        assert!(matches!(result, Err(NativeMutationError::DuplicateSequence)), "{result:?}");
        // And the base plus the conflicting seq 2 together.
        let input = [base.raw_bytes(), r2.raw_bytes()].concat();
        let result = incoming(&mut doc, &input, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        assert!(matches!(result, Err(NativeMutationError::DuplicateSequence)), "{result:?}");
    }

    #[test]
    fn missing_dependencies_require_complete_vs_retain_orphans() {
        let mut src = AutoCommit::new().with_actor(ActorId::from(vec![0x85; 16]));
        src.put(ROOT, "one", 1).unwrap();
        src.commit();
        src.put(ROOT, "two", 2).unwrap();
        src.commit();
        let changes = src.get_changes(&[]);
        let orphan = changes[1].raw_bytes().to_vec();
        let mut doc = seeded_doc();
        let before = snapshot(&mut doc);
        let result = incoming(&mut doc, &orphan, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RequireComplete);
        assert_eq!(result.unwrap(), NativeIncomingOutcome::MissingDependencies);
        assert!(snapshot(&mut doc) == before);
        let result = incoming(&mut doc, &orphan, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        assert_eq!(result.unwrap(), NativeIncomingOutcome::Applied { changed: false });
        assert_eq!(doc.get_missing_deps(&[]), vec![changes[0].hash()]);
        assert_eq!(doc.get_heads(), before.1, "queued change must not move heads");
        // Supplying the dependency integrates both.
        let result = incoming(&mut doc, changes[0].raw_bytes(), &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RequireComplete);
        assert_eq!(result.unwrap(), NativeIncomingOutcome::Applied { changed: true });
        assert!(doc.get_missing_deps(&[]).is_empty());
        assert_eq!(doc.get(ROOT, "two").map(|v| v.is_some()).ok(), Some(true));

        // Mixed input: one ready change plus one orphan, RequireComplete refuses all.
        let mut doc = seeded_doc();
        let before = snapshot(&mut doc);
        let ready = simple_change(0x86, "ready").raw_bytes().to_vec();
        let mixed = [ready.clone(), orphan.clone()].concat();
        let result = incoming(&mut doc, &mixed, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RequireComplete);
        assert_eq!(result.unwrap(), NativeIncomingOutcome::MissingDependencies);
        assert!(snapshot(&mut doc) == before, "RequireComplete applied the ready part");
        let result = incoming(&mut doc, &mixed, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
        assert_eq!(result.unwrap(), NativeIncomingOutcome::Applied { changed: true });
        assert_eq!(doc.get_missing_deps(&[]).len(), 1);

        // Owned path.
        let resident = seeded_doc().save();
        let owned = owned_apply(&resident, &orphan, NativeDependencyPolicy::RequireComplete).unwrap();
        assert!(matches!(owned, NativeApplyResult::MissingDependencies), "{owned:?}");
        let owned = owned_apply(&resident, &orphan, NativeDependencyPolicy::RetainOrphans).unwrap();
        assert!(
            matches!(owned, NativeApplyResult::Applied { changed: false, missing_dependencies: true, .. }),
            "{owned:?}"
        );
    }

    /// A populated Table written through map keys (qOS 0.8 patch feature).
    /// On 0.12 without the patch plain apply panics in batch import; the
    /// checked path must never panic: either apply faithfully or refuse.
    #[test]
    fn table_parent_map_key_ops_never_panic_the_checked_path() {
        let dir = {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let own = root.join("tests/fixtures/qos_08");
            if own.is_dir() { own } else { root.join("../../../vendor/automerge/tests/fixtures/qos_08") }
        };
        let input = std::fs::read(dir.join("table_changes.all.changes")).unwrap();
        let mut doc = AutoCommit::new();
        for policy in policies() {
            let result = incoming(&mut doc, &input, &NativeLimits::default(), &allowance(), policy);
            println!("table changes via apply_incoming_checked ({policy:?}): {result:?}");
        }
        let result = reconstruct(&input, &NativeLimits::default()).map(drop);
        println!("table changes via reconstruct: {result:?}");
    }
}

// ===========================================================================
// budget: allowance sweeps and reserve-before-allocate over the bounded API
// ===========================================================================
mod budget {
    use super::*;

    fn medium_doc() -> AutoCommit {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0x91; 16]));
        let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, &"bounded admission text ".repeat(64)).unwrap();
        doc.mark(&text, Mark::new("bold".into(), true, 0, 100), ExpandMark::Both).unwrap();
        let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
        for i in 0..200 {
            doc.insert(&list, i, i as i64).unwrap();
        }
        doc.put(ROOT, "counter", ScalarValue::counter(1)).unwrap();
        doc.commit();
        doc.increment(ROOT, "counter", 5).unwrap();
        doc.commit();
        doc
    }

    /// Candidate allowance sizes from tiny to comfortably large.
    fn sizes() -> Vec<usize> {
        let mut v = vec![64, 128, 256, 512];
        let mut n = 1024usize;
        while n <= 32 << 20 {
            v.push(n);
            v.push(n + n / 2);
            n *= 2;
        }
        v
    }

    /// apply_incoming_checked: for every allowance size the result is either
    /// success or WorkBytes with the document unchanged, every allocation is
    /// pre-reserved, and success is monotone in the allowance.
    #[test]
    fn incoming_allowance_sweep() {
        let mut src = medium_doc();
        let input = src.save();
        let mut first_ok = None;
        for size in sizes() {
            let Ok(credit) = NativeAllowance::new(size) else { continue };
            let mut doc = seeded_doc();
            let before = snapshot(&mut doc);
            let result = assert_precharged(&credit, || {
                catch_unwind(AssertUnwindSafe(|| {
                    doc.apply_incoming_checked(&input, &NativeLimits::default(), &credit, NativeDependencyPolicy::RequireComplete)
                }))
            })
            .unwrap_or_else(|_| panic!("panicked at allowance {size}"));
            match result {
                Ok(NativeIncomingOutcome::Applied { changed: true }) => {
                    first_ok.get_or_insert(size);
                    assert_eq!(doc.get_heads(), src.get_heads());
                }
                Err(NativeMutationError::Resource(NativeResourceError::WorkBytes)) => {
                    assert!(first_ok.is_none(), "refused at {size} after succeeding at {first_ok:?}");
                    assert!(snapshot(&mut doc) == before, "refusal at {size} mutated the document");
                }
                other => panic!("allowance {size}: unexpected {other:?}"),
            }
            assert_eq!(credit.available_bytes() + credit.retained_bytes(), size, "leaked reservation at {size}");
        }
        println!("incoming: first success at allowance {first_ok:?} for {} input bytes", input.len());
        assert!(first_ok.is_some(), "never succeeded");
    }

    /// normalize_input -> reconstruct -> save_bounded / clone_bounded /
    /// changes_since_bounded / base_heads_bounded: every stage refuses with
    /// WorkBytes below its need and is pre-reserved at every size.
    #[test]
    fn owned_pipeline_allowance_sweep() {
        let mut src = medium_doc();
        let input = src.save();
        let expected_heads = src.get_heads();
        let mut first_ok = None;
        for size in sizes() {
            let Ok(credit) = NativeAllowance::new(size) else { continue };
            let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), NativeResourceError> {
                let mut registry = NativeAllocationSet::new(credit.clone())?;
                let turn = registry.work_turn(credit.clone())?;
                let normalized = assert_precharged(&credit, || turn.normalize_input(&input, &NativeLimits::default()))?;
                let owned = assert_precharged(&credit, || normalized.reconstruct(&mut registry, &turn))?;
                let heads = assert_precharged(&credit, || owned.base_heads_bounded(&turn))?;
                assert_eq!(heads.as_slice(), expected_heads.as_slice());
                let _missing = assert_precharged(&credit, || owned.has_missing_dependencies());
                let saved = assert_precharged(&credit, || owned.save_bounded(&turn, SaveOptions::default()))?;
                assert_eq!(AutoCommit::load(saved.bytes()).unwrap().get_heads(), expected_heads);
                drop(saved);
                let history = assert_precharged(&credit, || owned.changes_since_bounded(&[], &turn))?;
                assert_eq!(history.len(), src.get_changes(&[]).len());
                drop(history);
                let clone = assert_precharged(&credit, || owned.clone_bounded(&mut registry, &turn))?;
                drop(clone);
                Ok(())
            }))
            .unwrap_or_else(|_| panic!("panicked at allowance {size}"));
            match outcome {
                Ok(()) => {
                    first_ok.get_or_insert(size);
                }
                Err(NativeResourceError::WorkBytes) => {
                    assert!(first_ok.is_none(), "refused at {size} after succeeding at {first_ok:?}");
                }
                Err(other) => panic!("allowance {size}: {other:?}"),
            }
            assert_eq!(credit.available_bytes() + credit.retained_bytes(), size, "leaked reservation at {size}");
        }
        println!("owned pipeline: first success at allowance {first_ok:?}");
        assert!(first_ok.is_some());
    }

    /// apply_checked_bounded at every allowance size: success or WorkBytes,
    /// pre-reserved, original unchanged.
    #[test]
    fn owned_apply_allowance_sweep() {
        let resident = seeded_doc().save();
        let mut src = medium_doc();
        let input = src.save();
        let mut first_ok = None;
        for size in sizes() {
            let big = allowance();
            let mut registry = NativeAllocationSet::new(big.clone()).unwrap();
            let setup = registry.work_turn(big.clone()).unwrap();
            let owned = setup.normalize_input(&resident, &NativeLimits::default()).unwrap().reconstruct(&mut registry, &setup).unwrap();
            let before = owned.save_bounded(&setup, SaveOptions::default()).unwrap().bytes().to_vec();
            let candidate = owned.clone_bounded(&mut registry, &setup).unwrap();
            let normalized = setup.normalize_input(&input, &NativeLimits::default()).unwrap();
            let Ok(credit) = NativeAllowance::new(size) else { continue };
            let Ok(turn) = registry.work_turn(credit.clone()) else { continue };
            let result = catch_unwind(AssertUnwindSafe(|| {
                assert_precharged(&credit, || {
                    candidate.apply_checked_bounded(normalized, NativeDependencyPolicy::RequireComplete, &mut registry, &turn)
                })
            }))
            .unwrap_or_else(|_| panic!("panicked at allowance {size}"));
            match result {
                Ok(NativeApplyResult::Applied { document, changed: true, missing_dependencies: false }) => {
                    first_ok.get_or_insert(size);
                    let heads = document.base_heads_bounded(&setup).unwrap();
                    let mut expected = src.get_heads();
                    expected.extend(seeded_doc().get_heads());
                    expected.sort();
                    let mut got = heads.as_slice().to_vec();
                    got.sort();
                    assert_eq!(got, expected);
                }
                Err(NativeMutationError::Resource(NativeResourceError::WorkBytes)) => {
                    assert!(first_ok.is_none(), "refused at {size} after succeeding at {first_ok:?}");
                }
                other => panic!("allowance {size}: {other:?}"),
            }
            let after = owned.save_bounded(&setup, SaveOptions::default()).unwrap().bytes().to_vec();
            assert!(before == after, "original changed at {size}");
        }
        println!("owned apply: first success at allowance {first_ok:?}");
        assert!(first_ok.is_some());
    }

    // ---- ported from native_footprint_vendor_tests.rs (0.12 semantics) ----

    fn populated_document() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, &"retained native text ".repeat(256)).unwrap();
        doc.commit();
        doc.update_diff_cursor();
        doc.mark(
            &text,
            Mark::new("long-shared-mark-name-for-allocation".into(), "long-shared-mark-value-for-allocation", 0, 200),
            ExpandMark::Both,
        )
        .unwrap();
        doc.splice_text(&text, 8, 0, "marked insertion").unwrap();
        doc.commit();
        let heads = doc.get_heads();
        drop(doc.diff(&[], &heads));
        doc.put(ROOT, "pending", ScalarValue::Bytes(vec![7; 32_768])).unwrap();
        assert!(doc.pending_ops() > 0);
        (doc, text)
    }

    fn measure_destination<T>(credit: &NativeAllowance, bytes: usize, operation: impl FnOnce() -> T) -> (T, Allocations) {
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
    fn save_output_limit_checks_exact_chosen_representation_before_destination() {
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
            let options = || SaveOptions { deflate, retain_orphans: true };
            let expected = candidate.save_bounded(&turn, options()).unwrap();
            assert!(expected.bytes().ends_with(&queued_raw));
            let length = expected.bytes().len();
            let (exact, _) = measure_destination(&credit, length, || candidate.save_bounded_with_limit(&turn, options(), length));
            let exact = exact.unwrap();
            assert_eq!(exact.bytes(), expected.bytes());
            let mut restored = AutoCommit::load(exact.bytes()).unwrap();
            assert_eq!(restored.get_heads(), heads);
            let (short, short_allocations) =
                measure_destination(&credit, length, || candidate.save_bounded_with_limit(&turn, options(), length - 1));
            assert!(matches!(short, Err(NativeResourceError::WorkBytes)));
            // 0.12 port: save_with_options builds its output internally, so
            // the destination-size allocation may occur; it must be reserved
            // (measure_under) and nothing may be retained after refusal.
            assert_eq!(short_allocations.live, 0, "refused save retained {} bytes", short_allocations.live);
        }
    }

    #[test]
    fn bounded_save_preserves_heads_and_raw_change_identity() {
        let (mut source, _) = populated_document();
        source.commit();
        let heads = source.get_heads();
        let changes: Vec<_> = source.get_changes(&[]).iter().map(|c| c.raw_bytes().to_vec()).collect();
        let credit = allowance();
        let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let candidate = source.clone_bounded(&mut registry, &turn).unwrap();
        for deflate in [false, true] {
            let (saved, allocations) =
                measure_under(&credit, || candidate.save_bounded(&turn, SaveOptions { deflate, retain_orphans: true }));
            let saved = saved.unwrap();
            println!("save: deflate={deflate} output={} peak={}", saved.bytes().len(), allocations.peak);
            let mut restored = AutoCommit::load(saved.bytes()).unwrap();
            assert_eq!(restored.get_heads(), heads);
            let restored_changes: Vec<_> = restored.get_changes(&[]).iter().map(|c| c.raw_bytes().to_vec()).collect();
            assert_eq!(restored_changes, changes);
        }
    }

    #[test]
    fn bounded_save_refuses_open_transactions_and_small_turns() {
        let (mut source, _) = populated_document();
        let credit = allowance();
        let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let pending = source.clone_bounded(&mut registry, &turn).unwrap();
        assert!(matches!(pending.save_bounded(&turn, SaveOptions::default()), Err(NativeResourceError::Unaccounted)));
        assert!(pending.pending_ops() > 0);
        source.commit();
        let closed = source.clone_bounded(&mut registry, &turn).unwrap();
        let tiny = NativeAllowance::new(1024).unwrap();
        let tiny_turn = registry.work_turn(tiny.clone()).unwrap();
        let result = assert_precharged(&tiny, || closed.save_bounded(&tiny_turn, SaveOptions::default()));
        assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    }

    #[test]
    fn bounded_save_covers_empty_branches_counters_and_queued_changes() {
        let mut branches = AutoCommit::new();
        branches.put(ROOT, "count", ScalarValue::counter(0)).unwrap();
        branches.put(ROOT, "boolean", true).unwrap();
        branches.put(ROOT, "null", ScalarValue::Null).unwrap();
        branches.commit();
        let mut other = branches.fork();
        for i in 1..=96 {
            branches.increment(ROOT, "count", i).unwrap();
        }
        branches.put(ROOT, "boolean", false).unwrap();
        branches.commit();
        other.put(ROOT, "concurrent", "a different actor's retained value").unwrap();
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

        for (mut source, orphan) in [(AutoCommit::new(), None), (branches, None), (queued, Some(orphan_raw))] {
            let heads = source.get_heads();
            let missing = source.get_missing_deps(&[]);
            let mut original: Vec<_> = source.get_changes(&[]).iter().map(|c| (c.hash(), c.raw_bytes().to_vec())).collect();
            original.sort_by_key(|entry| entry.0);
            let credit = allowance();
            let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
            let turn = registry.work_turn(credit.clone()).unwrap();
            let candidate = source.clone_bounded(&mut registry, &turn).unwrap();
            for deflate in [false, true] {
                let saved = assert_precharged(&credit, || {
                    candidate.save_bounded(&turn, SaveOptions { deflate, retain_orphans: true })
                })
                .unwrap();
                if let Some(orphan) = &orphan {
                    assert!(saved.bytes().ends_with(orphan));
                }
                let mut restored = AutoCommit::load(saved.bytes()).unwrap();
                assert_eq!(restored.get_heads(), heads);
                assert_eq!(restored.get_missing_deps(&[]), missing);
                let mut restored_changes: Vec<_> =
                    restored.get_changes(&[]).iter().map(|c| (c.hash(), c.raw_bytes().to_vec())).collect();
                restored_changes.sort_by_key(|entry| entry.0);
                assert_eq!(restored_changes, original);
            }
            if orphan.is_some() {
                let saved = assert_precharged(&credit, || {
                    candidate.save_bounded(&turn, SaveOptions { deflate: true, retain_orphans: false })
                })
                .unwrap();
                let mut restored = AutoCommit::load(saved.bytes()).unwrap();
                assert_eq!(restored.get_heads(), heads);
                assert!(restored.get_missing_deps(&[]).is_empty());
            }
        }
    }

    #[test]
    fn native_footprint_covers_actual_retained_allocations() {
        drop(populated_document());
        let ((doc, _text), allocations) = measure(populated_document);
        let footprint = doc.native_footprint(&allowance()).unwrap();
        println!("retained fixture: actual={} bound={}", allocations.live, footprint.owned_bytes());
        assert!(allocations.live > 32_768);
        // 0.12: slabs are uniquely owned.
        assert_eq!(footprint.shared_bytes(), 0);
        assert_eq!(footprint.total_bytes(), footprint.owned_bytes());
        assert!(footprint.total_bytes() >= allocations.live, "declared {} below actual {}", footprint.total_bytes(), allocations.live);
        assert!(footprint.total_bytes() <= 8 * allocations.live.max(64 * 1024), "bound {} is not useful", footprint.total_bytes());
        assert!(doc.pending_ops() > 0, "inspection must not close a transaction");
    }

    #[test]
    fn bounded_clone_preserves_pending_state_and_covers_allocations() {
        let (doc, _text) = populated_document();
        let allowance = allowance();
        let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
        let turn = allocations.work_turn(allowance.clone()).unwrap();
        let before = allowance.available_bytes();
        let (cloned, measured) = measure_under(&allowance, || doc.clone_bounded(&mut allocations, &turn).unwrap());
        assert_eq!(cloned.pending_ops(), doc.pending_ops());
        assert!(measured.live > 32_768);
        assert!(before - allowance.available_bytes() >= measured.live, "clone retains more than it holds credit for");
        assert!(measured.peak <= before);
    }

    #[test]
    fn clone_refuses_before_copying_past_the_allowance() {
        let (doc, _) = populated_document();
        let allowance = NativeAllowance::new(1_024).unwrap();
        let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
        let turn = allocations.work_turn(allowance.clone()).unwrap();
        let before = allowance.available_bytes();
        let (result, measured) = measure_under(&allowance, || doc.clone_bounded(&mut allocations, &turn));
        assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
        assert!(measured.peak <= before, "refusal must precede the native clone allocation");
        assert_eq!(allowance.available_bytes(), before);
        assert!(doc.pending_ops() > 0);
    }

    /// 0.12: each clone deep-copies, so a second clone costs about as much
    /// as the first (no shared backing), and dropping a clone returns its
    /// credit directly (reconcile is a no-op).
    #[test]
    fn clones_cost_full_footprint_and_release_on_drop() {
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
        assert!(second_charge * 2 >= first_charge, "second clone ({second_charge}) cannot share backing with the first ({first_charge})");
        drop(first);
        assert_eq!(allowance.available_bytes(), initial - second_charge, "dropping a clone releases its credit");
        allocations.reconcile();
        drop(allocations);
        assert!(allowance.available_bytes() < initial, "the live clone still holds credit");
        assert!(second.pending_ops() > 0);
        drop(second);
        assert!(allowance.available_bytes() >= initial - 64, "all but turn metadata released");
    }

    #[test]
    fn work_turn_denies_cross_registry_and_uses_new_work_credit() {
        let (doc, _) = populated_document();
        let initial = allowance();
        let mut allocations = NativeAllocationSet::new(initial.clone()).unwrap();
        let first_turn = allocations.work_turn(initial.clone()).unwrap();
        let first = doc.clone_bounded(&mut allocations, &first_turn).unwrap();
        let old_available = initial.available_bytes();
        let current = allowance();
        let turn = allocations.work_turn(current.clone()).unwrap();
        let before = current.available_bytes();
        let second = measure_under(&current, || first.clone_bounded(&mut allocations, &turn).unwrap()).0;
        assert_eq!(initial.available_bytes(), old_available, "new work cannot debit an old resident ledger");
        assert!(current.available_bytes() < before);
        let wrong_set = NativeAllocationSet::new(allowance()).unwrap();
        let wrong_turn = wrong_set.work_turn(allowance()).unwrap();
        assert!(matches!(second.clone_bounded(&mut allocations, &wrong_turn), Err(NativeResourceError::Unaccounted)));
        assert!(matches!(second.read_unqualified(&wrong_turn), Err(NativeResourceError::Unaccounted)));
        assert!(matches!(second.save_bounded(&wrong_turn, SaveOptions::default()), Err(NativeResourceError::Unaccounted)));
    }

    /// Reconstructing under one registry and applying with another owner's
    /// turn is refused (budget laundering).
    #[test]
    fn apply_with_foreign_turn_is_refused() {
        let resident = seeded_doc().save();
        let credit = allowance();
        let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let owned = turn.normalize_input(&resident, &NativeLimits::default()).unwrap().reconstruct(&mut registry, &turn).unwrap();
        let mut foreign = NativeAllocationSet::new(allowance()).unwrap();
        let foreign_turn = foreign.work_turn(allowance()).unwrap();
        let input = simple_change(0x99, "k").raw_bytes().to_vec();
        let normalized = foreign_turn.normalize_input(&input, &NativeLimits::default()).unwrap();
        let result = owned.apply_checked_bounded(normalized, NativeDependencyPolicy::RetainOrphans, &mut foreign, &foreign_turn);
        assert!(matches!(result, Err(NativeMutationError::Resource(NativeResourceError::Unaccounted))), "{result:?}");
    }

    /// Reads are step-charged and pre-reserved.
    #[test]
    fn reads_are_step_bounded_and_precharged() {
        let mut src = medium_doc();
        let input = src.save();
        let credit = allowance();
        let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
        let turn = registry.work_turn(credit.clone()).unwrap();
        let owned = turn.normalize_input(&input, &NativeLimits::default()).unwrap().reconstruct(&mut registry, &turn).unwrap();
        let read = owned.read(&turn).unwrap();
        let root = read.root();
        let mut list = None;
        assert_precharged(&credit, || {
            read.visit_property(root, "list", NativeSelection::Winner, |value, _| {
                if let NativeValue::Object { object, .. } = value {
                    list = Some(object);
                }
                Ok::<_, ()>(())
            })
        })
        .unwrap();
        let list = list.expect("list object");
        let steps = NativeReadSteps::new(10).unwrap();
        let mut seen = 0;
        let result = assert_precharged(&credit, || {
            read.visit_list(list, &steps, |_| {
                seen += 1;
                Ok::<_, ()>(std::ops::ControlFlow::Continue(()))
            })
        });
        assert!(result.is_err(), "200-element list visited within 10 steps");
        assert!(seen <= 10);
        let steps = NativeReadSteps::new(10_000).unwrap();
        let visited = assert_precharged(&credit, || {
            read.visit_list(list, &steps, |_| Ok::<_, ()>(std::ops::ControlFlow::Continue(())))
        })
        .unwrap();
        assert_eq!(visited, 200);
        let keys = assert_precharged(&credit, || read.visit_keys(root, |_| Ok::<_, ()>(()))).unwrap();
        assert_eq!(keys, 3);
        let mut heads = Vec::new();
        assert_precharged(&credit, || read.visit_heads(8, |h| heads.push(h))).ok();
        let _ = src.get_heads();
    }
}

// ===========================================================================
// shapes: deep nesting, many actors
// ===========================================================================
mod shapes {
    use super::*;

    /// 10k nested maps in one change: admitted within default limits (ops
    /// 10k), without stack overflow in validation/apply, and readable one
    /// level at a time with bounded steps.
    #[test]
    fn deep_nesting_is_admitted_without_recursion_blowup() {
        let depth = if slow() { 10_000 } else { 2_000 };
        let mut src = AutoCommit::new().with_actor(ActorId::from(vec![0xa1; 16]));
        let mut obj = ROOT;
        for _ in 0..depth {
            obj = src.put_object(&obj, "n", ObjType::Map).unwrap();
        }
        src.commit();
        let change = src.get_last_local_change().unwrap().raw_bytes().to_vec();
        // Run on a thread with a small stack: recursion proportional to depth
        // would overflow (abort) rather than fail politely.
        let handle = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut doc = AutoCommit::new();
                let result = doc.apply_incoming_checked(&change, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RequireComplete);
                assert_eq!(result.unwrap(), NativeIncomingOutcome::Applied { changed: true });
                let owned = reconstruct(&change, &NativeLimits::default()).unwrap();
                let credit = allowance();
                let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
                let _ = &mut registry;
                drop(owned);
            })
            .unwrap();
        handle.join().expect("deep nesting admission panicked");
    }

    /// Many actors with one change each: counted per chunk against max_actors.
    #[test]
    fn many_actor_changes_within_and_over_limits() {
        let n = 64;
        let input: Vec<u8> = (0..n).flat_map(|i| simple_change(i as u8, "k").raw_bytes().to_vec()).collect();
        let mut doc = seeded_doc();
        let tight = NativeLimits { max_actors: n - 1, ..NativeLimits::default() };
        let result = incoming(&mut doc, &input, &tight, &allowance(), NativeDependencyPolicy::RequireComplete);
        assert!(matches!(result, Err(NativeMutationError::Resource(NativeResourceError::Collection))), "{result:?}");
        let result = incoming(&mut doc, &input, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RequireComplete);
        assert_eq!(result.unwrap(), NativeIncomingOutcome::Applied { changed: true });
        assert_eq!(doc.get_all(ROOT, "k").unwrap().len(), n);
    }
}

// ===========================================================================
// columnar: forged document/bundle column contents that reach 0.12's
// collectors (VecEncoder::add assert, ChangeCollector::unbundle unwrap), and
// a checksum-preserving single-byte mutation sweep over every chunk kind.
// ===========================================================================
mod columnar {
    use super::*;

    /// Column-level view of a document (kind 0) or bundle (kind 3) chunk.
    struct Columnar {
        kind: u8,
        prefix: Vec<u8>,
        changes: Vec<(u64, Vec<u8>)>,
        ops: Vec<(u64, Vec<u8>)>,
        suffix: Vec<u8>,
    }

    fn read_meta(r: &mut &[u8]) -> Vec<(u64, usize)> {
        let n = read_uint(r);
        (0..n).map(|_| (read_uint(r), read_uint(r) as usize)).collect()
    }
    fn take_cols(r: &mut &[u8], meta: &[(u64, usize)]) -> Vec<(u64, Vec<u8>)> {
        meta.iter()
            .map(|(spec, len)| {
                let data = r[..*len].to_vec();
                *r = &r[*len..];
                (*spec, data)
            })
            .collect()
    }
    fn write_meta(cols: &[(u64, Vec<u8>)], out: &mut Vec<u8>) {
        leb(cols.len() as u64, out);
        for (spec, data) in cols {
            leb(*spec, out);
            leb(data.len() as u64, out);
        }
    }

    impl Columnar {
        fn parse(raw: &[u8]) -> Self {
            let kind = raw[8];
            let mut b = &raw[9..];
            let _len = read_uint(&mut b);
            let body = b;
            let mut r = body;
            match kind {
                0 => {
                    let n = read_uint(&mut r);
                    for _ in 0..n {
                        read_blob(&mut r);
                    }
                    let heads = read_uint(&mut r) as usize;
                    r = &r[heads * 32..];
                    let prefix = body[..body.len() - r.len()].to_vec();
                    let change_meta = read_meta(&mut r);
                    let op_meta = read_meta(&mut r);
                    let changes = take_cols(&mut r, &change_meta);
                    let ops = take_cols(&mut r, &op_meta);
                    Self { kind, prefix, changes, ops, suffix: r.to_vec() }
                }
                3 => {
                    let n = read_uint(&mut r) as usize;
                    r = &r[n * 32..];
                    let a = read_uint(&mut r);
                    for _ in 0..a {
                        read_blob(&mut r);
                    }
                    let prefix = body[..body.len() - r.len()].to_vec();
                    let change_meta = read_meta(&mut r);
                    let changes = take_cols(&mut r, &change_meta);
                    let op_meta = read_meta(&mut r);
                    let ops = take_cols(&mut r, &op_meta);
                    assert!(r.is_empty());
                    Self { kind, prefix, changes, ops, suffix: Vec::new() }
                }
                k => panic!("not a columnar chunk: {k}"),
            }
        }
        fn encode(&self) -> Vec<u8> {
            let mut out = self.prefix.clone();
            match self.kind {
                0 => {
                    write_meta(&self.changes, &mut out);
                    write_meta(&self.ops, &mut out);
                    for (_, d) in self.changes.iter().chain(&self.ops) {
                        out.extend(d);
                    }
                    out.extend(&self.suffix);
                }
                _ => {
                    write_meta(&self.changes, &mut out);
                    for (_, d) in &self.changes {
                        out.extend(d);
                    }
                    write_meta(&self.ops, &mut out);
                    for (_, d) in &self.ops {
                        out.extend(d);
                    }
                }
            }
            chunk(self.kind, &out)
        }
        fn set(cols: &mut [(u64, Vec<u8>)], spec: u64, data: Vec<u8>) {
            let col = cols.iter_mut().find(|(s, _)| *s == spec).unwrap_or_else(|| panic!("no column {spec:#x}"));
            col.1 = data;
        }
    }

    /// Delta column holding exactly `values` as a literal run.
    fn delta_literal(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        sleb(-(values.len() as i64), &mut out);
        let mut last = 0;
        for v in values {
            sleb(v - last, &mut out);
            last = *v;
        }
        out
    }

    /// Runs the checked surface on `input`; asserts refusal without panic.
    fn assert_refused_without_panic(what: &str, input: &[u8]) {
        let owned = reconstruct(input, &NativeLimits::default()).map(drop);
        assert!(owned.is_err(), "{what}: reconstruct accepted forged storage");
        let mut doc = seeded_doc();
        for policy in [NativeDependencyPolicy::RequireComplete, NativeDependencyPolicy::RetainOrphans] {
            let result = incoming(&mut doc, input, &NativeLimits::default(), &allowance(), policy);
            assert!(result.is_err(), "{what}: apply_incoming_checked accepted forged storage: {result:?}");
        }
    }

    /// Two ops of one change given the same op id (1@A) in a document
    /// chunk. 0.12's ChangeCollector hits `assert!` in VecEncoder::add.
    #[test]
    fn document_with_duplicate_op_ids_is_refused_without_panic() {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0xc1; 16]));
        doc.put(ROOT, "a", 1).unwrap();
        doc.put(ROOT, "b", 2).unwrap();
        doc.commit();
        let mut parts = Columnar::parse(&doc.save_nocompress());
        Columnar::set(&mut parts.ops, 0x23, delta_literal(&[1, 1]));
        let forged = parts.encode();
        let plain = catch_unwind(|| AutoCommit::load(&forged).map(drop));
        println!("plain 0.12 load of duplicate op ids: panicked={}", plain.is_err());
        assert_refused_without_panic("duplicate op ids", &forged);
    }

    /// An op id beyond its change's op range (1@A, 5@A with max_op 2):
    /// indexes VecEncoder past its dense slots.
    #[test]
    fn document_with_op_id_outside_its_change_is_refused_without_panic() {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0xc2; 16]));
        doc.put(ROOT, "a", 1).unwrap();
        doc.put(ROOT, "b", 2).unwrap();
        doc.commit();
        let mut parts = Columnar::parse(&doc.save_nocompress());
        Columnar::set(&mut parts.ops, 0x23, delta_literal(&[1, 5]));
        assert_refused_without_panic("op id outside change", &parts.encode());
    }

    /// A bundle whose second change claims one more op than the bundle
    /// carries (max_op bumped). 0.12's ChangeCollector::unbundle unwraps the
    /// resulting MissingOps error.
    #[test]
    fn bundle_change_with_missing_ops_is_refused_without_panic() {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0xc3; 16]));
        doc.put(ROOT, "a", 1).unwrap();
        doc.commit();
        doc.put(ROOT, "b", 2).unwrap();
        doc.commit();
        let hashes: Vec<_> = doc.get_changes(&[]).iter().map(Change::hash).collect();
        let bundle = doc.bundle(hashes).unwrap();
        let mut parts = Columnar::parse(bundle.bytes());
        // Bundle change column group: MAX_OP is delta column id 2.
        Columnar::set(&mut parts.changes, 0x23, delta_literal(&[1, 3]));
        let forged = parts.encode();
        let plain = catch_unwind(|| AutoCommit::load(&forged).map(drop));
        println!("plain 0.12 load of bundle with missing ops: panicked={}", plain.is_err());
        assert_refused_without_panic("bundle missing ops", &forged);
    }

    /// Single-byte mutations of every body byte (checksum recomputed so the
    /// mutation reaches the decoders): the checked surface must never panic,
    /// and must never mutate the resident document on refusal. Full value
    /// set behind AM012_SLOW.
    #[test]
    fn checksum_preserving_mutation_sweep_never_panics() {
        let mut src = AutoCommit::new().with_actor(ActorId::from(vec![0xd1; 16]));
        let text = src.put_object(ROOT, "t", ObjType::Text).unwrap();
        src.splice_text(&text, 0, 0, "hello").unwrap();
        src.mark(&text, Mark::new("b".into(), true, 0, 3), ExpandMark::After).unwrap();
        let list = src.put_object(ROOT, "l", ObjType::List).unwrap();
        src.insert(&list, 0, ScalarValue::counter(1)).unwrap();
        src.commit();
        src.put(ROOT, "k", "v").unwrap();
        src.increment(&list, 0, 2).unwrap();
        src.delete(&text, 1).unwrap();
        src.commit();
        let changes = src.get_changes(&[]);
        let hashes: Vec<_> = changes.iter().map(Change::hash).collect();
        let seeds: Vec<(u8, Vec<u8>)> = vec![
            (0, src.save_nocompress()),
            (1, changes[1].raw_bytes().to_vec()),
            (3, src.bundle(hashes).unwrap().bytes().to_vec()),
        ];
        let values: &[fn(u8) -> u8] = if slow() {
            &[|b| b ^ 1, |b| b ^ 0x80, |_| 0, |_| 0xff, |b| b.wrapping_add(1), |b| b.wrapping_sub(1), |b| b ^ 0x40]
        } else {
            &[|b| b ^ 1, |b| b ^ 0x80]
        };
        let mut failures = Vec::new();
        let mut accepted = 0usize;
        // Mutant panics are collected below; silence the default hook meanwhile.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for (kind, raw) in &seeds {
            let mut b = &raw[9..];
            let _ = read_uint(&mut b);
            let body_start = raw.len() - b.len();
            let body = &raw[body_start..];
            for i in 0..body.len() {
                for f in values {
                    let mut m = body.to_vec();
                    m[i] = f(m[i]);
                    if m[i] == body[i] {
                        continue;
                    }
                    let input = chunk(*kind, &m);
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        if inspect(&input, &NativeLimits::default()).is_err() {
                            return false;
                        }
                        let _ = reconstruct(&input, &NativeLimits::default());
                        let mut doc = seeded_doc();
                        let _ = incoming(&mut doc, &input, &NativeLimits::default(), &allowance(), NativeDependencyPolicy::RetainOrphans);
                        true
                    }));
                    match outcome {
                        Ok(true) => accepted += 1,
                        Ok(false) => {}
                        Err(p) => {
                            let msg = p
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                                .unwrap_or_default();
                            failures.push(format!("kind {kind} byte {i}: {} -> {}: {}", body[i], m[i], msg.chars().take(160).collect::<String>()));
                        }
                    }
                }
            }
        }
        std::panic::set_hook(hook);
        println!("mutation sweep: {accepted} mutants passed inspection");
        assert!(failures.is_empty(), "{} panicking mutants:\n{}", failures.len(), failures.iter().take(40).cloned().collect::<Vec<_>>().join("\n"));
    }
}
