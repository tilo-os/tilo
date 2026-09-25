//! Exercises the real vendored native boundary through the workspace dependency.
use automerge::resource_limits::{NativeAllowance, NativeLimits, NativeResourceError};
use automerge::{AutoCommit, ROOT, transaction::Transactable};

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

// Catches an unchecked document-column inflater before native reconstruction.
#[test]
fn compressed_document_expansion_is_refused() {
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![7; 65_536]),
    )
    .unwrap();
    let encoded = doc.save();
    assert!(encoded.len() < 1_024);
    assert!(AutoCommit::load(&encoded).is_ok());
    let limits = NativeLimits {
        max_normalized_bytes: 1_024,
        ..NativeLimits::default()
    };
    assert!(matches!(
        AutoCommit::inspect_bounded(&encoded, &limits, &mut allowance()),
        Err(NativeResourceError::NormalizedBytes)
    ));
}

// Catches guarding document columns but leaving compressed change chunks open.
#[test]
fn compressed_change_expansion_is_refused() {
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![9; 65_536]),
    )
    .unwrap();
    doc.commit();
    let encoded = doc.get_last_local_change().unwrap().bytes().into_owned();
    assert_eq!(encoded[8], 2, "fixture must be a compressed change chunk");
    assert!(encoded.len() < 1_024);
    assert!(AutoCommit::load(&encoded).is_ok());
    let limits = NativeLimits {
        max_normalized_bytes: 1_024,
        ..NativeLimits::default()
    };
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
    let limits = NativeLimits {
        max_operations: 64,
        ..NativeLimits::default()
    };
    assert!(matches!(
        AutoCommit::inspect_bounded(&encoded, &limits, &mut allowance()),
        Err(NativeResourceError::Operations)
    ));
}

// Catches silently disabling compressed input instead of bounding it.
#[test]
fn bounded_compressed_document_inspection_keeps_original_identity() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "bytes", automerge::ScalarValue::Bytes(vec![5; 4_096]))
        .unwrap();
    let encoded = doc.save();
    assert!(encoded.len() < 1_024);
    let checked =
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
    assert_eq!(checked.original_bytes(), encoded);
    assert_eq!(checked.shape().operations(), 1);
}

// Catches omitting the larger normalized column/header length encodings.
#[test]
fn normalized_size_includes_rewritten_framing() {
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![5; 65_536]),
    )
    .unwrap();
    let encoded = doc.save();
    let normalized = doc.save_nocompress();
    let checked =
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
    assert_eq!(checked.shape().normalized_bytes(), normalized.len());
    let exact = NativeLimits {
        max_normalized_bytes: normalized.len(),
        ..NativeLimits::default()
    };
    assert!(AutoCommit::inspect_bounded(&encoded, &exact, &mut allowance()).is_ok());
    let short = NativeLimits {
        max_normalized_bytes: normalized.len() - 1,
        ..exact
    };
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
    let limits = NativeLimits {
        max_operations: 1,
        ..NativeLimits::default()
    };
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
    assert!(matches!(
        child.reserve(2_000),
        Err(NativeResourceError::WorkBytes)
    ));
    drop(allowance);
    assert!(matches!(
        child.reserve(2_000),
        Err(NativeResourceError::WorkBytes)
    ));
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
        assert!(matches!(
            AutoCommit::inspect_bounded(
                &hex::decode(encoded).unwrap(),
                &NativeLimits::default(),
                &mut allowance()
            ),
            Err(NativeResourceError::Collection)
        ));
    }
    let encoded = hex::decode("856f4a831bfcd801000d00000001420780808080802001").unwrap();
    assert!(matches!(
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()),
        Err(NativeResourceError::Operations)
    ));
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
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![9; 65_536]),
    )
    .unwrap();
    doc.commit();
    let encoded = doc.get_last_local_change().unwrap().bytes().into_owned();
    assert_eq!(encoded[8], 2);
    assert!(
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).is_ok()
    );
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
        assert!(matches!(
            AutoCommit::inspect_bounded(bytes, &NativeLimits::default(), &mut allowance()),
            Err(NativeResourceError::Invalid)
        ));
    }
}

// Catches charging only logical length and deduping by contents or wrapper
// address. A retained handle also prevents allocator address reuse in a ledger.
#[test]
fn shared_backing_accounts_capacity_and_retains_identity() {
    use automerge::resource_limits::SharedBytes;
    use std::sync::Arc;
    let mut bytes = Vec::with_capacity(4_096);
    bytes.push(7);
    let capacity = bytes.capacity();
    let bytes = Arc::new(bytes);
    let weak = Arc::downgrade(&bytes);
    let handle = SharedBytes::from_arc(Arc::clone(&bytes));
    let clone = handle.clone();
    assert_eq!(handle.identity(), clone.identity());
    assert!(handle.allocation_bytes() >= capacity + std::mem::size_of::<Vec<u8>>());
    let separate = SharedBytes::from_arc(Arc::new(vec![7]));
    assert_ne!(handle.identity(), separate.identity());
    drop(bytes);
    drop(handle);
    assert!(weak.upgrade().is_some());
    drop(clone);
    assert!(weak.upgrade().is_none());
}

// Catches leaving compressed columns for a later unchecked native inflater,
// and releasing the normalized buffer's reservation before its owner dies.
#[test]
fn normalization_matches_native_storage_and_retains_its_credit() {
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![9; 65_536]),
    )
    .unwrap();
    let encoded = doc.save();
    let expected = doc.save_nocompress();
    let mut allowance = allowance();
    let checked =
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance).unwrap();
    let before = allowance.available_bytes();
    let normalized = checked.normalize(&allowance).unwrap();
    assert_eq!(normalized.original_bytes(), encoded);
    assert_eq!(normalized.bytes().len(), expected.len());
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
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![9; 65_536]),
    )
    .unwrap();
    let encoded = doc.save();
    let checked =
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
    let small = NativeAllowance::new(1_024).unwrap();
    let before = small.available_bytes();
    assert!(matches!(
        checked.normalize(&small),
        Err(NativeResourceError::WorkBytes)
    ));
    assert_eq!(small.available_bytes(), before);
}

// Catches rewriting change bytes in a way that changes their cryptographic ID.
#[test]
fn normalization_preserves_compressed_change_identity() {
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        "bytes",
        automerge::ScalarValue::Bytes(vec![9; 65_536]),
    )
    .unwrap();
    doc.commit();
    let mut change = doc.get_last_local_change().unwrap();
    let expected = change.raw_bytes().to_vec();
    let encoded = change.bytes().into_owned();
    assert_eq!(encoded[8], 2);
    let checked =
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
    let normalized = checked.normalize(&allowance()).unwrap();
    assert_eq!(normalized.bytes().len(), expected.len());
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
    let hashes = doc
        .get_changes(&[])
        .iter()
        .map(|change| change.hash())
        .collect::<Vec<_>>();
    let bundle = doc.bundle(hashes).unwrap();
    assert_eq!(bundle.bytes()[8], 3);
    assert!(AutoCommit::load(bundle.bytes()).is_ok());
    let checked =
        AutoCommit::inspect_bounded(bundle.bytes(), &NativeLimits::default(), &mut allowance())
            .unwrap();
    assert_eq!(checked.shape().operations(), 2);
    let normalized = checked.normalize(&allowance()).unwrap();
    assert_eq!(normalized.bytes(), bundle.bytes());
}

// Catches undercounting or losing a later chunk while normalizing a document
// followed by its retained incremental changes.
#[test]
fn document_and_incremental_change_normalize_as_one_complete_input() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "bytes", automerge::ScalarValue::Bytes(vec![9; 4_096]))
        .unwrap();
    let mut encoded = doc.save();
    doc.put(ROOT, "more", automerge::ScalarValue::Bytes(vec![8; 4_096]))
        .unwrap();
    doc.commit();
    encoded.extend(doc.get_last_local_change().unwrap().bytes().as_ref());
    let checked =
        AutoCommit::inspect_bounded(&encoded, &NativeLimits::default(), &mut allowance()).unwrap();
    let normalized = checked.normalize(&allowance()).unwrap();
    let mut restored = AutoCommit::load(normalized.bytes()).unwrap();
    assert_eq!(restored.get_heads(), doc.get_heads());
}

// A single change row can describe a billion dense collector slots. Logical
// row count and encoded bytes alone must not authorize that native allocation.
#[test]
fn sparse_document_counter_is_refused_before_collector_allocation() {
    let bytes =
        hex::decode("856f4a83b40552a9001a0101000004010203021306400200010001010180808080040100")
            .unwrap();
    assert!(matches!(
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut allowance()),
        Err(NativeResourceError::Operations)
    ));
}

// Each of four changes claims 64 operations with no dependencies: its actual
// dense collector demand is 256 despite an empty stored operation table.
#[test]
fn dense_collector_slots_share_the_operation_ceiling() {
    let bytes =
        hex::decode("856f4a8399cdc86e001a0101000004010203021306400200040004017cc0000000000400")
            .unwrap();
    let limits = NativeLimits {
        max_operations: 128,
        ..NativeLimits::default()
    };
    assert!(matches!(
        AutoCommit::inspect_bounded(&bytes, &limits, &mut allowance()),
        Err(NativeResourceError::Operations)
    ));
}

// Dependency indexes must refer to preceding change rows before reconstruction
// uses them to index max-op or change-hash arrays.
#[test]
fn out_of_range_document_dependency_is_invalid_before_reconstruction() {
    let bytes =
        hex::decode("856f4a83e9e773df001a0101000005010203021302400243020001000101010101010102")
            .unwrap();
    assert!(matches!(
        AutoCommit::inspect_bounded(&bytes, &NativeLimits::default(), &mut allowance()),
        Err(NativeResourceError::Invalid)
    ));
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
    let exact = NativeLimits {
        max_edges: 1,
        ..NativeLimits::default()
    };
    assert!(AutoCommit::inspect_bounded(encoded, &exact, &mut allowance()).is_ok());
    let short = NativeLimits {
        max_edges: 0,
        ..exact
    };
    assert!(matches!(
        AutoCommit::inspect_bounded(encoded, &short, &mut allowance()),
        Err(NativeResourceError::Collection)
    ));
}

#[test]
fn standalone_change_prefix_messages_share_value_quota() {
    use automerge::transaction::CommitOptions;
    let mut doc = AutoCommit::new();
    doc.empty_change(CommitOptions::default().with_message("one"));
    let mut encoded = doc.get_last_local_change().unwrap().raw_bytes().to_vec();
    doc.empty_change(CommitOptions::default().with_message("two"));
    encoded.extend_from_slice(doc.get_last_local_change().unwrap().raw_bytes());
    let exact = NativeLimits {
        max_value_bytes: 6,
        ..NativeLimits::default()
    };
    assert!(AutoCommit::inspect_bounded(&encoded, &exact, &mut allowance()).is_ok());
    let short = NativeLimits {
        max_value_bytes: 5,
        ..exact
    };
    assert!(matches!(
        AutoCommit::inspect_bounded(&encoded, &short, &mut allowance()),
        Err(NativeResourceError::ValueBytes)
    ));
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
    let exact = NativeLimits {
        max_edges: 1,
        ..NativeLimits::default()
    };
    assert!(AutoCommit::inspect_bounded(bundle.bytes(), &exact, &mut allowance()).is_ok());
    let short = NativeLimits {
        max_edges: 0,
        ..exact
    };
    assert!(matches!(
        AutoCommit::inspect_bounded(bundle.bytes(), &short, &mut allowance()),
        Err(NativeResourceError::Collection)
    ));
}
