use super::*;

#[test]
fn flow_credit_literal_is_only_exact_durable_prefix() {
    let mut literal = vec![1];
    literal.extend([1; 16]);
    literal.extend([2; 32]);
    literal.extend([0x80, 0x80, 0x80, 0x20, 0x80, 0x80, 0x10]);
    let credit = FlowCredit::new([1; 16], [2; 32], 64 * 1024 * 1024, 256 * 1024).unwrap();
    assert_eq!(credit.encode().unwrap(), literal);
    let decoded = FlowCredit::decode(&literal).unwrap();
    assert_eq!(
        (decoded.canonical_size(), decoded.next_offset()),
        (64 * 1024 * 1024, 256 * 1024)
    );
    assert!(FlowCredit::new([0; 16], [2; 32], 1, 0).is_err());
    assert!(FlowCredit::new([1; 16], [0; 32], 1, 0).is_err());
    assert!(FlowCredit::new([1; 16], [2; 32], 0, 0).is_err());
    assert!(FlowCredit::new([1; 16], [2; 32], 64 * 1024 * 1024 + 1, 0).is_err());
    assert!(FlowCredit::new([1; 16], [2; 32], 1, 2).is_err());
    for end in 0..literal.len() {
        assert!(FlowCredit::decode(&literal[..end]).is_err());
    }
    literal.push(0);
    assert!(FlowCredit::decode(&literal).is_err());
}

#[test]
fn snapshot_chunk_borrows_exact_bytes_and_rejects_noncanonical_ranges() {
    let chunk = SnapshotChunk::new([1; 16], [2; 32], 3, b"abc").unwrap();
    let mut literal = vec![1];
    literal.extend([1; 16]);
    literal.extend([2; 32]);
    literal.extend([3, 3, b'a', b'b', b'c']);
    assert_eq!(chunk.encode().unwrap(), literal);
    let decoded = SnapshotChunk::decode(&literal).unwrap();
    assert_eq!(decoded.bytes(), b"abc");
    assert_eq!(
        decoded.bytes().as_ptr(),
        literal[literal.len() - 3..].as_ptr()
    );
    assert!(SnapshotChunk::new([1; 16], [2; 32], 0, &vec![0; 240 * 1024]).is_ok());
    assert!(SnapshotChunk::new([1; 16], [2; 32], 0, &vec![0; 240 * 1024 + 1]).is_err());
    assert!(SnapshotChunk::new([1; 16], [2; 32], 1105, b"a").is_err());
    assert!(SnapshotChunk::new([1; 16], [2; 32], 0, b"").is_err());
    let mut noncanonical = literal.clone();
    noncanonical.splice(49..50, [0x83, 0]);
    assert!(SnapshotChunk::decode(&noncanonical).is_err());
    for end in 0..literal.len() {
        assert!(SnapshotChunk::decode(&literal[..end]).is_err());
    }
}

#[test]
fn record_header_has_fixed_offsets_and_bounded_canonical_item() {
    let header =
        TransferRecordHeader::new(257, TransferKind::State, 64 * 1024 * 1024, [9; 32]).unwrap();
    let mut literal = [0; 45];
    literal[..4].copy_from_slice(&[1, 1, 0, 0]);
    literal[4] = 1;
    literal[5..13].copy_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0]);
    literal[13..].fill(9);
    assert_eq!(header.encode(), literal);
    assert_eq!(TransferRecordHeader::decode(&literal).unwrap(), header);
    literal[4] = 2;
    assert!(TransferRecordHeader::decode(&literal).is_err());
    assert!(
        TransferRecordHeader::new(0, TransferKind::Governance, 64 * 1024 * 1024 + 1, [9; 32])
            .is_err()
    );
    assert!(TransferRecordHeader::new(0, TransferKind::State, 0, [9; 32]).is_err());
}

#[test]
fn streaming_order_preserves_old_digest_for_sparse_ordinals_without_old_count_cap() {
    let events = [(0, [1; 32]), (129, [2; 32]), (4096, [3; 32])];
    let mut order = VisibleOrder::new(3).unwrap();
    for (ordinal, digest) in events {
        order.push(ordinal, digest).unwrap();
    }
    assert_eq!(
        order.finish().unwrap(),
        super::super::visible_order_digest(&events).unwrap()
    );
    let mut golden = blake3::Hasher::new();
    golden.update(b"tilo-delivery-v3-visible-order-v1\0");
    let mut literal = vec![3, 0];
    literal.extend([1; 32]);
    literal.extend([0x81, 1]);
    literal.extend([2; 32]);
    literal.extend([0x80, 0x20]);
    literal.extend([3; 32]);
    golden.update(&literal);
    let mut order = VisibleOrder::new(3).unwrap();
    for (ordinal, digest) in events {
        order.push(ordinal, digest).unwrap();
    }
    assert_eq!(order.finish().unwrap(), *golden.finalize().as_bytes());
    let mut large = VisibleOrder::new(65536).unwrap();
    for i in 0..65536 {
        large.push(i, [1; 32]).unwrap();
    }
    assert!(large.finish().is_ok());
    assert!(VisibleOrder::new(65537).is_err());
    let mut bad = VisibleOrder::new(2).unwrap();
    bad.push(9, [1; 32]).unwrap();
    assert!(bad.push(9, [2; 32]).is_err());
    assert!(bad.finish().is_err());
    assert!(VisibleOrder::new(1).unwrap().finish().is_err());
    let mut full = VisibleOrder::new(0).unwrap();
    assert!(full.push(0, [1; 32]).is_err());
}

fn manifest_issue() -> ManifestIssue {
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    ManifestIssue {
        scope: crate::delivery_v3::cursor::CursorIssue {
            room: "00000000-0000-0000-0000-000000000001".into(),
            workspace: 7,
            workspace_root: [1; 32],
            host_device: key.verifying_key().to_bytes(),
            host_root: [2; 32],
            consumer_device: [3; 32],
            consumer_root: [4; 32],
            consumer: [5; 16],
            store_generation: [6; 16],
            authorization_view: [8; 32],
            committed: super::super::CommitRef {
                sequence: 9,
                digest: [10; 32],
            },
        },
        pin: [11; 16],
        kind: SnapshotKind::Baseline,
        predecessor: None,
        baseline_generation: 8,
        expires_at_ns: 200,
        item_count: 1,
        stream_bytes: 48,
        chunk_size: 240 * 1024,
        chunk_digests: vec![[12; 32]],
    }
}
fn unsigned_literal(issue: &ManifestIssue) -> Vec<u8> {
    fn varint(mut n: u64, out: &mut Vec<u8>) {
        loop {
            let b = (n & 127) as u8;
            n >>= 7;
            out.push(b | if n != 0 { 128 } else { 0 });
            if n == 0 {
                break;
            }
        }
    }
    let s = &issue.scope;
    let mut out = vec![1, 3, 36];
    out.extend(s.room.as_bytes());
    varint(s.workspace, &mut out);
    for field in [
        &s.workspace_root,
        &s.host_device,
        &s.host_root,
        &s.consumer_device,
        &s.consumer_root,
    ] {
        out.extend(field);
    }
    out.extend(s.consumer);
    out.extend(s.store_generation);
    out.extend(s.authorization_view);
    varint(s.committed.sequence, &mut out);
    out.extend(s.committed.digest);
    out.extend(issue.pin);
    out.push(match issue.kind {
        SnapshotKind::Baseline => 0,
        SnapshotKind::Replay => 1,
    });
    if let Some(p) = &issue.predecessor {
        out.push(1);
        varint(p.sequence, &mut out);
        out.extend(p.digest);
    } else {
        out.push(0);
    }
    varint(issue.baseline_generation, &mut out);
    varint(issue.expires_at_ns, &mut out);
    varint(u64::from(issue.item_count), &mut out);
    varint(issue.stream_bytes, &mut out);
    varint(u64::from(issue.chunk_size), &mut out);
    varint(issue.chunk_digests.len() as u64, &mut out);
    for digest in &issue.chunk_digests {
        out.extend(digest);
    }
    out
}
fn sign_literal(mut claims: Vec<u8>, domain: &[u8]) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(&claims);
    claims.extend(
        ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .sign(h.finalize().as_bytes())
            .to_bytes(),
    );
    claims
}
#[test]
fn snapshot_requests_preserve_direction_canonical_cursor_and_renewal() {
    assert_eq!(
        SnapshotRequest::new(SnapshotOperation::Baseline)
            .unwrap()
            .encode()
            .unwrap(),
        [1, 0]
    );
    let raw = [1, 1, 3, 1, 2, 3];
    let request = SnapshotRequest::decode(&raw).unwrap();
    assert!(
        matches!(request.operation(),SnapshotOperation::ReplayAfter{cursor} if cursor.as_bytes()==[1,2,3])
    );
    assert_eq!(request.encode().unwrap(), raw);
    let mut renew = vec![1, 2];
    renew.extend([1; 16]);
    renew.extend([2; 32]);
    assert!(
        matches!(SnapshotRequest::decode(&renew).unwrap().operation(),SnapshotOperation::Renew{pin,..} if *pin==[1;16])
    );
    for raw in [&[1, 3][..], &[2, 0], &[1, 0, 0], &[1, 0x80, 0], &[1, 1, 0]] {
        assert!(SnapshotRequest::decode(raw).is_err());
    }
    assert!(SnapshotRequest::decode(&vec![0; 8193]).is_err());
}
#[test]
fn signed_manifest_matches_literal_domain_and_checks_tampering_and_expiry() {
    let issue = manifest_issue();
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let literal = sign_literal(
        unsigned_literal(&issue),
        b"tilo-delivery-v3-snapshot-manifest-v1\0",
    );
    let manifest = SnapshotManifest::issue(issue.clone(), &key, 100).unwrap();
    assert_eq!(manifest.as_bytes(), literal);
    let signed = SnapshotManifest::from_bytes(&literal)
        .unwrap()
        .authenticate(&key.verifying_key(), 100)
        .unwrap();
    assert_eq!(signed.issue(), &issue);
    assert_eq!(manifest.digest(), *blake3::hash(&literal).as_bytes());
    let mut changed = literal.clone();
    changed[50] ^= 1;
    assert!(
        SnapshotManifest::from_bytes(&changed)
            .unwrap()
            .authenticate(&key.verifying_key(), 100)
            .is_err()
    );
    let wrong = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    assert!(manifest.authenticate(&wrong.verifying_key(), 100).is_err());
    let wrong_domain = sign_literal(unsigned_literal(&issue), b"tilo-delivery-v3-cursor-v1\0");
    assert!(
        SnapshotManifest::from_bytes(&wrong_domain)
            .unwrap()
            .authenticate(&key.verifying_key(), 100)
            .is_err()
    );
    assert!(manifest.authenticate(&key.verifying_key(), 200).is_err());
    assert!(SnapshotRequest::decode(&literal).is_err());
    for end in [0, 2, 64, literal.len() - 1] {
        assert!(
            SnapshotManifest::from_bytes(&literal[..end])
                .and_then(|m| m.authenticate(&key.verifying_key(), 100))
                .is_err()
        );
    }
}
#[test]
fn manifest_enforces_stream_counts_scope_and_five_minute_limit() {
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let base = manifest_issue();
    let mut bad = base.clone();
    bad.chunk_digests.clear();
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut bad = base.clone();
    bad.item_count = 65537;
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut bad = base.clone();
    bad.stream_bytes = 45;
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut bad = base.clone();
    bad.expires_at_ns = 100 + 300_000_000_001;
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut bad = base.clone();
    bad.scope.room = "00000000-0000-0000-0000-00000000000A".into();
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut bad = base.clone();
    bad.scope.host_device = [1; 32];
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut bad = base.clone();
    bad.kind = SnapshotKind::Replay;
    assert!(SnapshotManifest::issue(bad, &key, 100).is_err());
    let mut replay = base.clone();
    replay.kind = SnapshotKind::Replay;
    replay.predecessor = Some(super::super::CommitRef {
        sequence: 8,
        digest: [13; 32],
    });
    assert!(SnapshotManifest::issue(replay, &key, 100).is_ok());
    let mut signed_bad = base;
    signed_bad.chunk_digests = vec![[1; 32]; 1106];
    let bytes = sign_literal(
        unsigned_literal(&signed_bad),
        b"tilo-delivery-v3-snapshot-manifest-v1\0",
    );
    assert!(
        SnapshotManifest::from_bytes(&bytes)
            .unwrap()
            .authenticate(&key.verifying_key(), 100)
            .is_err()
    );
}

#[test]
fn authenticated_manifest_checks_exact_chunk_identity_length_and_digest() {
    let mut issue = manifest_issue();
    issue.chunk_digests[0] = *blake3::hash(&[7; 48]).as_bytes();
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let manifest = SnapshotManifest::issue(issue, &key, 100).unwrap();
    let signed = manifest.authenticate(&key.verifying_key(), 100).unwrap();
    let chunk = SnapshotChunk::new([11; 16], manifest.digest(), 0, &[7; 48]).unwrap();
    assert!(signed.validate_chunk(&chunk).is_ok());
    for wrong in [
        SnapshotChunk::new([12; 16], manifest.digest(), 0, &[7; 48]).unwrap(),
        SnapshotChunk::new([11; 16], [2; 32], 0, &[7; 48]).unwrap(),
        SnapshotChunk::new([11; 16], manifest.digest(), 1, &[7; 48]).unwrap(),
        SnapshotChunk::new([11; 16], manifest.digest(), 0, &[8; 48]).unwrap(),
        SnapshotChunk::new([11; 16], manifest.digest(), 0, &[7; 47]).unwrap(),
    ] {
        assert!(signed.validate_chunk(&wrong).is_err());
    }
}
#[test]
fn renewed_manifest_reuses_staging_only_for_identical_immutable_claims() {
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let first = SnapshotManifest::issue(manifest_issue(), &key, 100)
        .unwrap()
        .authenticate(&key.verifying_key(), 100)
        .unwrap();
    let mut issue = manifest_issue();
    issue.expires_at_ns = 300;
    let renewal = SnapshotManifest::issue(issue.clone(), &key, 100)
        .unwrap()
        .authenticate(&key.verifying_key(), 100)
        .unwrap();
    assert!(first.same_content(&renewal));
    issue.scope.consumer_root = [14; 32];
    let other = SnapshotManifest::issue(issue, &key, 100)
        .unwrap()
        .authenticate(&key.verifying_key(), 100)
        .unwrap();
    assert!(!first.same_content(&other));
    let mut issue = manifest_issue();
    issue.chunk_digests[0] = [14; 32];
    let other = SnapshotManifest::issue(issue, &key, 100)
        .unwrap()
        .authenticate(&key.verifying_key(), 100)
        .unwrap();
    assert!(!first.same_content(&other));
}

#[test]
fn retained_manifest_authenticity_is_timeless_while_current_wrapper_still_expires() {
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let manifest = SnapshotManifest::issue(manifest_issue(), &key, 100).unwrap();
    assert!(manifest.authenticate(&key.verifying_key(), 200).is_err());
    let retained = manifest
        .authenticate_retained(&key.verifying_key())
        .unwrap();
    assert_eq!(retained.issue(), &manifest_issue());
    let mut corrupted = manifest.as_bytes().to_vec();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(
        SnapshotManifest::from_bytes(&corrupted)
            .unwrap()
            .authenticate_retained(&key.verifying_key())
            .is_err()
    );
    assert!(
        manifest
            .authenticate_retained(&ed25519_dalek::SigningKey::from_bytes(&[8; 32]).verifying_key())
            .is_err()
    );
}
